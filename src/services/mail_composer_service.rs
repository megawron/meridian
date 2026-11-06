// SPDX-License-Identifier: GPL-3.0-or-later
//
// Cross-platform RFC822/MIME construction for the composer.  Account
// credentials and transport selection remain in EDS/Camel; this service only
// turns the GTK composer state into a complete message that Camel can send.

use super::crypto_service::SmimeOptions;
use super::draft_store_service::DraftAttachment;
use super::settings_service::AppSettings;

use chrono::Utc;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const OPERATION_ACTIVE: u8 = 0;
const OPERATION_CANCEL_REQUESTED: u8 = 1;
const OPERATION_COMMITTED: u8 = 2;

/// Cancellation shared by the UI and a worker thread.
///
/// A boolean is not sufficient here: a cancel request racing with the final
/// file rename or with transport startup must not be able to report a cancel
/// after the irreversible operation has already been committed.
#[derive(Debug, Default)]
pub(crate) struct CancellationToken {
    state: AtomicU8,
}

impl CancellationToken {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_cancel_requested(&self) -> bool {
        self.state.load(Ordering::Acquire) == OPERATION_CANCEL_REQUESTED
    }

    pub(crate) fn is_committed(&self) -> bool {
        self.state.load(Ordering::Acquire) == OPERATION_COMMITTED
    }

    pub(crate) fn request_cancel(&self) -> bool {
        self.state
            .compare_exchange(
                OPERATION_ACTIVE,
                OPERATION_CANCEL_REQUESTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn try_commit(&self) -> bool {
        self.state
            .compare_exchange(
                OPERATION_ACTIVE,
                OPERATION_COMMITTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtomicSaveResult {
    Saved,
    Cancelled,
}

#[derive(Debug, Clone)]
pub(crate) struct OutgoingMessage {
    pub(crate) source_uid: String,
    pub(crate) from: String,
    pub(crate) reply_to: String,
    pub(crate) request_read_receipt: bool,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) html_body: String,
    pub(crate) plain_text: bool,
    pub(crate) priority: bool,
    pub(crate) charset: String,
    pub(crate) attachments: Vec<DraftAttachment>,
    pub(crate) calendar_method: Option<String>,
    pub(crate) calendar_component: Option<String>,
    pub(crate) pgp_sign: bool,
    pub(crate) pgp_encrypt: bool,
    pub(crate) pgp_key: String,
    pub(crate) smime_options: Option<SmimeOptions>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportMessage {
    pub(crate) from: String,
    pub(crate) reply_to: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) date: String,
    pub(crate) message_id: Option<String>,
    pub(crate) in_reply_to: Vec<String>,
    pub(crate) references: Vec<String>,
    pub(crate) html_body: String,
    pub(crate) attachments: Vec<ExportAttachment>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportAttachment {
    pub(crate) filename: String,
    pub(crate) mime_type: String,
    pub(crate) content_id: Option<String>,
    pub(crate) content: Arc<[u8]>,
}

/// Write an EML file through a same-directory temporary file and atomically
/// replace the selected destination only after the complete payload has been
/// flushed.  Cancellation remains effective while rendering and writing; the
/// commit token closes the last race immediately before rename.
pub(crate) fn save_rfc822_atomically(
    path: &Path,
    contents: &[u8],
    cancellation: &CancellationToken,
) -> Result<AtomicSaveResult, String> {
    if cancellation.is_cancel_requested() {
        return Ok(AtomicSaveResult::Cancelled);
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("message.eml");
    let temporary_path = parent.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4()));

    let write_result = (|| -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        for chunk in contents.chunks(64 * 1024) {
            if cancellation.is_cancel_requested() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            file.write_all(chunk)?;
        }
        file.sync_all()?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        if error.kind() == io::ErrorKind::Interrupted || cancellation.is_cancel_requested() {
            return Ok(AtomicSaveResult::Cancelled);
        }
        return Err(format!(
            "Could not prepare EML file: {error}"
        ));
    }

    if !cancellation.try_commit() {
        let _ = fs::remove_file(&temporary_path);
        return Ok(AtomicSaveResult::Cancelled);
    }

    if let Err(error) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(format!("Could not replace EML file: {error}"));
    }

    // Directory fsync is best-effort here.  It makes the rename durable on
    // filesystems that support it without turning a successful atomic save
    // into a failure on platforms where opening directories is unsupported.
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(AtomicSaveResult::Saved)
}

pub(crate) fn build_rfc822(message: &OutgoingMessage) -> Result<Vec<u8>, String> {
    if message.source_uid.trim().is_empty() {
        return Err("No mail account selected".to_string());
    }

    let recipients = [
        message.to.as_str(),
        message.cc.as_str(),
        message.bcc.as_str(),
    ];
    if recipients.iter().all(|value| value.trim().is_empty()) {
        return Err("At least one recipient is required".to_string());
    }

    validate_header_value("Recipient", &message.to)?;
    validate_header_value("Cc", &message.cc)?;
    validate_header_value("Bcc", &message.bcc)?;
    validate_header_value("Subject", &message.subject)?;
    validate_header_value("From", &message.from)?;
    validate_header_value("Reply-To", &message.reply_to)?;
    if let Some(method) = message.calendar_method.as_deref() {
        validate_header_value("iTIP method", method)?;
    }
    if message.calendar_component.is_some() != message.calendar_method.is_some() {
        return Err(
            "iTIP method and iCalendar component must be set together".to_string(),
        );
    }
    if message.request_read_receipt && message.from.trim().is_empty() {
        return Err("A read receipt requires a configured sender address".to_string());
    }

    let attachments = load_attachments(&message.attachments)?;

    let charset = normalize_charset(&message.charset)?;
    let raw = build_rfc822_parts(
        &message.from,
        &message.reply_to,
        message.request_read_receipt,
        message.priority,
        &message.to,
        &message.cc,
        &message.bcc,
        &message.subject,
        "",
        None,
        &[],
        &[],
        &message.html_body,
        message.plain_text,
        &charset,
        &attachments,
        message.calendar_method.as_deref(),
        message.calendar_component.as_deref(),
    )?;
    if !message.pgp_sign && !message.pgp_encrypt {
        if let Some(options) = message.smime_options.as_ref()
            && (options.sign || options.encrypt)
        {
            return crate::services::crypto_service::protect_smime_rfc822(&raw, options);
        }
        return Ok(raw);
    }
    if message
        .smime_options
        .as_ref()
        .is_some_and(|options| options.sign || options.encrypt)
    {
        return Err("OpenPGP and S/MIME cannot be enabled at the same time".to_string());
    }
    let recipients = [message.to.clone(), message.cc.clone(), message.bcc.clone()];
    let signing_key = if message.pgp_sign && message.pgp_key.trim().is_empty() {
        crate::services::crypto_service::normalize_recipient_addresses(&[message.from.clone()])?
            .into_iter()
            .next()
            .unwrap_or_default()
    } else {
        message.pgp_key.clone()
    };
    crate::services::crypto_service::protect_rfc822(
        &raw,
        &crate::services::crypto_service::OpenPgpOptions {
            sign: message.pgp_sign,
            encrypt: message.pgp_encrypt,
            signing_key,
            recipients: recipients.to_vec(),
        },
    )
}

pub(crate) fn build_export_rfc822(message: &ExportMessage) -> Result<Vec<u8>, String> {
    validate_header_value("From", &message.from)?;
    validate_header_value("Reply-To", &message.reply_to)?;
    validate_header_value("Recipient", &message.to)?;
    validate_header_value("Cc", &message.cc)?;
    validate_header_value("Bcc", &message.bcc)?;
    validate_header_value("Subject", &message.subject)?;
    validate_header_value("Date", &message.date)?;
    if let Some(message_id) = &message.message_id {
        validate_header_value("Message-ID", message_id)?;
        if !message_id.trim().is_empty() {
            validate_message_id("Message-ID", message_id)?;
        }
    }
    for value in message.in_reply_to.iter().chain(message.references.iter()) {
        validate_header_value("Thread header", value)?;
        if !value.trim().is_empty() {
            validate_message_id("Thread header", value)?;
        }
    }

    build_rfc822_parts(
        &message.from,
        &message.reply_to,
        false,
        false,
        &message.to,
        &message.cc,
        &message.bcc,
        &message.subject,
        &message.date,
        message.message_id.as_deref(),
        &message.in_reply_to,
        &message.references,
        &message.html_body,
        false,
        "UTF-8",
        &message.attachments,
        None,
        None,
    )
}

fn build_rfc822_parts(
    from: &str,
    reply_to: &str,
    request_read_receipt: bool,
    priority: bool,
    to: &str,
    cc: &str,
    bcc: &str,
    subject: &str,
    date: &str,
    message_id: Option<&str>,
    in_reply_to: &[String],
    references: &[String],
    html_body: &str,
    plain_text: bool,
    charset: &str,
    attachments: &[ExportAttachment],
    calendar_method: Option<&str>,
    calendar_component: Option<&str>,
) -> Result<Vec<u8>, String> {
    validate_header_value("Date", date)?;
    validate_header_value("Message-ID", message_id.unwrap_or_default())?;
    validate_header_values("In-Reply-To", in_reply_to)?;
    validate_header_values("References", references)?;
    validate_header_value("From", from)?;
    validate_header_value("Reply-To", reply_to)?;
    validate_header_value("Recipient", to)?;
    validate_header_value("Cc", cc)?;
    validate_header_value("Bcc", bcc)?;
    validate_header_value("Subject", subject)?;
    let charset = normalize_charset(charset)?;
    if let Some(method) = calendar_method {
        validate_calendar_method(method)?;
    }
    if calendar_component.is_some() != calendar_method.is_some() {
        return Err(
            "iTIP method and iCalendar component must be set together".to_string(),
        );
    }
    if request_read_receipt && from.trim().is_empty() {
        return Err("A read receipt requires a configured sender address".to_string());
    }

    let date_value = if date.trim().is_empty() {
        Utc::now().to_rfc2822()
    } else {
        date.trim().to_string()
    };
    let message_id_value = message_id
        .filter(|value| !value.trim().is_empty())
        .map(|value| format_message_id("Message-ID", value))
        .transpose()?
        .unwrap_or_else(|| format!("<{}@meridian.local>", uuid::Uuid::new_v4().simple()));

    let mut raw = String::new();
    append_header_line(&mut raw, "Date", &date_value);
    append_header_line(&mut raw, "Message-ID", &message_id_value);
    if !from.trim().is_empty() {
        let from = encode_address_header(from)?;
        append_header_line(&mut raw, "From", &from);
    }
    if !reply_to.trim().is_empty() {
        let reply_to = encode_address_header(reply_to)?;
        append_header_line(&mut raw, "Reply-To", &reply_to);
    }
    if request_read_receipt && !from.trim().is_empty() {
        let from = encode_address_header(from)?;
        append_header_line(&mut raw, "Disposition-Notification-To", &from);
    }
    if priority {
        append_header_line(&mut raw, "X-Priority", "1");
    }
    append_recipient_header(&mut raw, "To", to)?;
    append_recipient_header(&mut raw, "Cc", cc)?;
    append_recipient_header(&mut raw, "Bcc", bcc)?;
    let in_reply_to = format_message_id_list("In-Reply-To", in_reply_to)?;
    if !in_reply_to.is_empty() {
        append_header_line(&mut raw, "In-Reply-To", &in_reply_to);
    }
    let references = format_message_id_list("References", references)?;
    if !references.is_empty() {
        append_header_line(&mut raw, "References", &references);
    }
    append_header_line(&mut raw, "Subject", &encode_header_value(subject));
    append_header_line(&mut raw, "MIME-Version", "1.0");

    for attachment in attachments {
        validate_export_attachment(attachment)?;
    }

    let (processed_html, extracted_inline) = if plain_text {
        (html_body.to_string(), Vec::new())
    } else {
        extract_inline_data_images(html_body)
    };

    let mut all_attachments = attachments.to_vec();
    all_attachments.extend(extracted_inline);

    let (inline_attachments, regular_attachments): (Vec<_>, Vec<_>) = all_attachments
        .into_iter()
        .partition(|att| att.content_id.is_some());

    let (font_family, font_size) = AppSettings::try_new()
        .map(|s| (s.mail_font_family(), s.mail_font_size()))
        .unwrap_or_else(|| {
            (
                "system-ui, -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif"
                    .to_string(),
                14,
            )
        });

    let plain_body = if plain_text {
        normalize_body(&processed_html)
    } else {
        normalize_body(&html_to_plain_text(&processed_html))
    };
    let html_body = (!plain_text).then(|| {
        let wrapped = wrap_html_with_typography(&processed_html, &font_family, font_size);
        normalize_body(&wrapped)
    });

    if inline_attachments.is_empty() {
        if regular_attachments.is_empty() {
            append_alternative_or_single_part(
                &mut raw,
                None,
                &plain_body,
                html_body.as_deref(),
                &charset,
                calendar_method,
                calendar_component,
            )?;
        } else {
            let mixed_boundary = new_boundary();
            append_multipart_header(&mut raw, "mixed", &mixed_boundary);
            append_alternative_or_single_part(
                &mut raw,
                Some(&mixed_boundary),
                &plain_body,
                html_body.as_deref(),
                &charset,
                calendar_method,
                calendar_component,
            )?;
            for attachment in &regular_attachments {
                append_attachment_part(&mut raw, &mixed_boundary, attachment)?;
            }
            close_multipart(&mut raw, &mixed_boundary);
        }
    } else if regular_attachments.is_empty() {
        let related_boundary = new_boundary();
        append_multipart_related_header(&mut raw, &related_boundary);
        append_alternative_or_single_part(
            &mut raw,
            Some(&related_boundary),
            &plain_body,
            html_body.as_deref(),
            &charset,
            calendar_method,
            calendar_component,
        )?;
        for attachment in &inline_attachments {
            append_attachment_part(&mut raw, &related_boundary, attachment)?;
        }
        close_multipart(&mut raw, &related_boundary);
    } else {
        let mixed_boundary = new_boundary();
        let related_boundary = new_boundary();
        append_multipart_header(&mut raw, "mixed", &mixed_boundary);
        append_multipart_related_part_header(&mut raw, &mixed_boundary, &related_boundary);
        append_alternative_or_single_part(
            &mut raw,
            Some(&related_boundary),
            &plain_body,
            html_body.as_deref(),
            &charset,
            calendar_method,
            calendar_component,
        )?;
        for attachment in &inline_attachments {
            append_attachment_part(&mut raw, &related_boundary, attachment)?;
        }
        close_multipart(&mut raw, &related_boundary);

        for attachment in &regular_attachments {
            append_attachment_part(&mut raw, &mixed_boundary, attachment)?;
        }
        close_multipart(&mut raw, &mixed_boundary);
    }
    Ok(raw.into_bytes())
}

fn load_attachments(attachments: &[DraftAttachment]) -> Result<Vec<ExportAttachment>, String> {
    attachments
        .iter()
        .map(|attachment| {
            validate_attachment_filename(&attachment.filename)?;
            validate_mime_type(&attachment.mime_type)?;

            let content = if !attachment.embedded_content_base64.is_empty() {
                let content = decode_embedded_attachment(&attachment.embedded_content_base64)?;
                if content.len() as u64 != attachment.size {
                    return Err(format!(
                        "Embedded attachment has an invalid size: {}",
                        attachment.filename
                    ));
                }
                content
            } else if attachment.path.trim().is_empty() {
                return Err(format!(
                    "Could not read attachment ({}): path is empty",
                    attachment.filename
                ));
            } else {
                let path = Path::new(&attachment.path);
                let metadata = fs::metadata(path).map_err(|error| {
                    format!(
                        "Could not read attachment ({}): {error}",
                        attachment.filename
                    )
                })?;
                if !metadata.is_file() {
                    return Err(format!(
                        "Attachment is not a regular file: {}",
                        attachment.filename
                    ));
                }
                fs::read(path).map_err(|error| {
                    format!(
                        "Could not read attachment ({}): {error}",
                        attachment.filename
                    )
                })?
            };
            Ok(ExportAttachment {
                filename: attachment.filename.clone(),
                mime_type: attachment.mime_type.clone(),
                content_id: attachment.content_id.clone(),
                content: content.into(),
            })
        })
        .collect()
}

pub(crate) fn embedded_draft_attachment(
    filename: &str,
    mime_type: &str,
    content: &[u8],
) -> Result<DraftAttachment, String> {
    validate_attachment_filename(filename)?;
    validate_mime_type(mime_type)?;
    if content.is_empty() {
        return Err("Embedded attachment is empty".to_string());
    }
    Ok(DraftAttachment {
        path: format!("embedded:{}", uuid::Uuid::new_v4()),
        filename: filename.to_string(),
        mime_type: mime_type.to_string(),
        size: content.len() as u64,
        content_id: None,
        embedded_content_base64: base64_with_crlf(content).replace(['\r', '\n'], ""),
    })
}

pub(crate) fn embedded_draft_attachment_with_content_id(
    filename: &str,
    mime_type: &str,
    content_id: Option<&str>,
    content: &[u8],
) -> Result<DraftAttachment, String> {
    let mut attachment = embedded_draft_attachment(filename, mime_type, content)?;
    if let Some(content_id) = content_id {
        validate_message_id("Content-ID", content_id)?;
        attachment.content_id = Some(content_id.trim().to_string());
    }
    Ok(attachment)
}

pub(crate) fn decode_embedded_attachment(value: &str) -> Result<Vec<u8>, String> {
    let mut sextets = Vec::with_capacity(value.len());
    let mut padding = 0usize;
    for byte in value.bytes() {
        let decoded = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                0
            }
            _ => return Err("Embedded attachment is not valid Base64".to_string()),
        };
        if padding > 0 && byte != b'=' {
            return Err("Embedded attachment contains invalid Base64 padding".to_string());
        }
        sextets.push(decoded);
    }
    if sextets.is_empty() {
        return Ok(Vec::new());
    }
    if sextets.len() % 4 != 0 || padding > 2 {
        return Err("Embedded attachment contains invalid Base64 padding".to_string());
    }
    let mut decoded = Vec::with_capacity(sextets.len() / 4 * 3);
    for chunk in sextets.chunks_exact(4) {
        decoded.push((chunk[0] << 2) | (chunk[1] >> 4));
        decoded.push((chunk[1] << 4) | (chunk[2] >> 2));
        decoded.push((chunk[2] << 6) | chunk[3]);
    }
    decoded.truncate(decoded.len().saturating_sub(padding));
    Ok(decoded)
}

fn validate_export_attachment(attachment: &ExportAttachment) -> Result<(), String> {
    validate_attachment_filename(&attachment.filename)?;
    validate_mime_type(&attachment.mime_type)?;
    Ok(())
}

fn validate_attachment_filename(value: &str) -> Result<(), String> {
    validate_header_value("Filename", value)?;
    let filename = value.trim();
    if filename.is_empty() {
        return Err("Attachment requires a filename".to_string());
    }
    if filename.chars().any(char::is_control) {
        return Err("Attachment filename contains control characters".to_string());
    }
    if filename == "."
        || filename == ".."
        || filename
            .chars()
            .any(|character| character == '/' || character == '\\')
    {
        return Err("Attachment filename contains an invalid path".to_string());
    }
    Ok(())
}

fn validate_mime_type(value: &str) -> Result<String, String> {
    let mime_type = value.trim();
    if mime_type.is_empty() {
        return Ok("application/octet-stream".to_string());
    }
    let mut components = mime_type.split('/');
    let type_name = components.next().unwrap_or_default();
    let subtype = components.next().unwrap_or_default();
    if components.next().is_some() || !is_mime_token(type_name) || !is_mime_token(subtype) {
        return Err(format!("Invalid attachment MIME type: {mime_type}"));
    }
    Ok(mime_type.to_ascii_lowercase())
}

fn is_mime_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// Normalize the small set of encodings exposed by the composer. Keeping the
/// list explicit prevents a per-message value from becoming a MIME header
/// injection vector and lets the portable builder report unsupported data
/// instead of silently corrupting it.
pub(crate) fn normalize_charset(value: &str) -> Result<String, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "utf-8" | "utf8" => Ok("UTF-8".to_string()),
        "iso-8859-1" | "latin1" | "latin-1" => Ok("ISO-8859-1".to_string()),
        "iso-8859-15" | "latin9" | "latin-9" => Ok("ISO-8859-15".to_string()),
        "windows-1252" | "cp1252" => Ok("Windows-1252".to_string()),
        "us-ascii" | "ascii" => Ok("US-ASCII".to_string()),
        other => Err(format!("Unsupported character encoding: {other}")),
    }
}

fn encode_text_body(body: &str, charset: &str) -> Result<Vec<u8>, String> {
    match charset {
        "UTF-8" => Ok(body.as_bytes().to_vec()),
        "US-ASCII" => body
            .chars()
            .map(|character| {
                if character.is_ascii() {
                    Ok(character as u8)
                } else {
                    Err(format!(
                        "Message body contains characters that cannot be represented in {charset}"
                    ))
                }
            })
            .collect(),
        "ISO-8859-1" => body
            .chars()
            .map(|character| {
                let codepoint = character as u32;
                (codepoint <= u8::MAX as u32)
                    .then_some(codepoint as u8)
                    .ok_or_else(|| {
                        format!(
                            "Message body contains characters that cannot be represented in {charset}"
                        )
                    })
            })
            .collect(),
        "ISO-8859-15" => body
            .chars()
            .map(|character| {
                let byte = match character {
                    '€' => Some(0xA4),
                    'Š' => Some(0xA6),
                    'š' => Some(0xA8),
                    'Ž' => Some(0xB4),
                    'ž' => Some(0xB8),
                    'Œ' => Some(0xBC),
                    'œ' => Some(0xBD),
                    'Ÿ' => Some(0xBE),
                    _ => (character as u32 <= u8::MAX as u32).then_some(character as u8),
                };
                byte.ok_or_else(|| {
                    format!(
                        "Message body contains characters that cannot be represented in {charset}"
                    )
                })
            })
            .collect(),
        "Windows-1252" => body
            .chars()
            .map(|character| {
                let byte = match character {
                    '€' => Some(0x80),
                    '‚' => Some(0x82),
                    'ƒ' => Some(0x83),
                    '„' => Some(0x84),
                    '…' => Some(0x85),
                    '†' => Some(0x86),
                    '‡' => Some(0x87),
                    'ˆ' => Some(0x88),
                    '‰' => Some(0x89),
                    'Š' => Some(0x8A),
                    '‹' => Some(0x8B),
                    'Œ' => Some(0x8C),
                    'Ž' => Some(0x8E),
                    '‘' => Some(0x91),
                    '’' => Some(0x92),
                    '“' => Some(0x93),
                    '”' => Some(0x94),
                    '•' => Some(0x95),
                    '–' => Some(0x96),
                    '—' => Some(0x97),
                    '˜' => Some(0x98),
                    '™' => Some(0x99),
                    'š' => Some(0x9A),
                    '›' => Some(0x9B),
                    'œ' => Some(0x9C),
                    'ž' => Some(0x9E),
                    'Ÿ' => Some(0x9F),
                    _ => (character as u32 <= u8::MAX as u32).then_some(character as u8),
                };
                byte.ok_or_else(|| {
                    format!(
                        "Message body contains characters that cannot be represented in {charset}"
                    )
                })
            })
            .collect(),
        _ => Err(format!("Unsupported character encoding: {charset}")),
    }
}

fn new_boundary() -> String {
    format!("=_Meridian_{}", uuid::Uuid::new_v4().simple())
}

fn append_multipart_header(raw: &mut String, kind: &str, boundary: &str) {
    raw.push_str("Content-Type: multipart/");
    raw.push_str(kind);
    raw.push_str("; boundary=\"");
    raw.push_str(boundary);
    raw.push_str("\"\r\n\r\n");
}

fn append_multipart_related_header(raw: &mut String, boundary: &str) {
    raw.push_str("Content-Type: multipart/related; type=\"text/html\"; boundary=\"");
    raw.push_str(boundary);
    raw.push_str("\"\r\n\r\n");
}

fn append_multipart_related_part_header(raw: &mut String, parent_boundary: &str, boundary: &str) {
    raw.push_str("--");
    raw.push_str(parent_boundary);
    raw.push_str("\r\nContent-Type: multipart/related; type=\"text/html\"; boundary=\"");
    raw.push_str(boundary);
    raw.push_str("\"\r\n\r\n");
}

fn append_multipart_alternative_part_header(raw: &mut String, parent_boundary: &str, boundary: &str) {
    raw.push_str("--");
    raw.push_str(parent_boundary);
    raw.push_str("\r\nContent-Type: multipart/alternative; boundary=\"");
    raw.push_str(boundary);
    raw.push_str("\"\r\n\r\n");
}

fn append_leaf_part(
    raw: &mut String,
    content_type: &str,
    body: &str,
    charset: &str,
) -> Result<(), String> {
    raw.push_str("Content-Type: ");
    raw.push_str(content_type);
    let encoded = encode_text_body(body, charset)?;
    if charset.eq_ignore_ascii_case("UTF-8") {
        raw.push_str("\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
        raw.push_str(body);
        ensure_crlf(raw);
    } else {
        raw.push_str("\r\nContent-Transfer-Encoding: base64\r\n\r\n");
        raw.push_str(&base64_with_crlf(&encoded));
    }
    Ok(())
}

fn append_text_part(
    raw: &mut String,
    boundary: &str,
    content_type: &str,
    body: &str,
    charset: &str,
) -> Result<(), String> {
    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("\r\n");
    append_leaf_part(raw, content_type, body, charset)
}

fn close_multipart(raw: &mut String, boundary: &str) {
    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("--\r\n");
}

fn append_alternative_or_single_part(
    raw: &mut String,
    parent_boundary: Option<&str>,
    plain_body: &str,
    html_body: Option<&str>,
    charset: &str,
    calendar_method: Option<&str>,
    calendar_component: Option<&str>,
) -> Result<(), String> {
    let has_alternative = html_body.is_some() || calendar_component.is_some();
    if let Some(parent) = parent_boundary {
        if has_alternative {
            let alt_boundary = new_boundary();
            append_multipart_alternative_part_header(raw, parent, &alt_boundary);
            append_text_part(
                raw,
                &alt_boundary,
                &format!("text/plain; charset={charset}"),
                plain_body,
                charset,
            )?;
            if let Some(html) = html_body {
                append_text_part(
                    raw,
                    &alt_boundary,
                    &format!("text/html; charset={charset}"),
                    html,
                    charset,
                )?;
            }
            if let (Some(method), Some(component)) = (calendar_method, calendar_component) {
                append_calendar_part(raw, &alt_boundary, method, component)?;
            }
            close_multipart(raw, &alt_boundary);
        } else {
            append_text_part(
                raw,
                parent,
                &format!("text/plain; charset={charset}"),
                plain_body,
                charset,
            )?;
        }
    } else if has_alternative {
        let alt_boundary = new_boundary();
        append_multipart_header(raw, "alternative", &alt_boundary);
        append_text_part(
            raw,
            &alt_boundary,
            &format!("text/plain; charset={charset}"),
            plain_body,
            charset,
        )?;
        if let Some(html) = html_body {
            append_text_part(
                raw,
                &alt_boundary,
                &format!("text/html; charset={charset}"),
                html,
                charset,
            )?;
        }
        if let (Some(method), Some(component)) = (calendar_method, calendar_component) {
            append_calendar_part(raw, &alt_boundary, method, component)?;
        }
        close_multipart(raw, &alt_boundary);
    } else {
        append_leaf_part(
            raw,
            &format!("text/plain; charset={charset}"),
            plain_body,
            charset,
        )?;
    }
    Ok(())
}

fn append_attachment_part(
    raw: &mut String,
    boundary: &str,
    attachment: &ExportAttachment,
) -> Result<(), String> {
    let mime_type = validate_mime_type(&attachment.mime_type)?;
    validate_attachment_filename(&attachment.filename)?;

    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("\r\nContent-Type: ");
    raw.push_str(&mime_type);
    append_mime_parameter(raw, "name", &attachment.filename)?;
    raw.push_str("\r\nContent-Transfer-Encoding: base64\r\n");
    if let Some(content_id) = attachment.content_id.as_deref() {
        raw.push_str("Content-ID: ");
        raw.push_str(&format_message_id("Content-ID", content_id)?);
        raw.push_str("\r\n");
    }
    raw.push_str(if attachment.content_id.is_some() {
        "Content-Disposition: inline"
    } else {
        "Content-Disposition: attachment"
    });
    append_mime_parameter(raw, "filename", &attachment.filename)?;
    raw.push_str("\r\n\r\n");
    raw.push_str(&base64_with_crlf(&attachment.content));
    ensure_crlf(raw);
    Ok(())
}

fn append_mime_parameter(raw: &mut String, name: &str, value: &str) -> Result<(), String> {
    validate_attachment_filename(value)?;
    let value = value.trim();
    raw.push_str("; ");
    raw.push_str(name);
    if value.is_ascii() {
        raw.push_str("=\"");
        raw.push_str(&value.replace('\\', "\\\\").replace('"', "\\\""));
        raw.push('"');
    } else {
        raw.push_str("*=utf-8''");
        raw.push_str(&percent_encode_parameter(value.as_bytes()));
    }
    Ok(())
}

fn percent_encode_parameter(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn append_calendar_part(
    raw: &mut String,
    boundary: &str,
    method: &str,
    component: &str,
) -> Result<(), String> {
    validate_calendar_method(method)?;
    if component.trim().is_empty() {
        return Err("The iTIP MIME component must not be empty".to_string());
    }
    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("\r\nContent-Type: text/calendar; method=");
    raw.push_str(method.trim());
    raw.push_str("; charset=UTF-8\r\nContent-Disposition: inline\r\n");
    raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    raw.push_str(normalize_body(component).trim_end_matches("\r\n"));
    ensure_crlf(raw);
    Ok(())
}

fn append_header_line(raw: &mut String, name: &str, value: &str) {
    raw.push_str(name);
    raw.push_str(": ");

    let initial_column = name.chars().count() + 2;
    let mut start = 0;
    let mut column = initial_column;
    while value[start..].chars().count() + column > 78 {
        let mut candidate = None;
        let mut quoted = false;
        let mut angle_depth = 0u8;
        let mut escaped = false;
        let mut exceeded = false;

        for (offset, character) in value[start..].char_indices() {
            let end = start + offset + character.len_utf8();
            if escaped {
                escaped = false;
            } else if quoted && character == '\\' {
                escaped = true;
            } else {
                match character {
                    '"' => quoted = !quoted,
                    '<' if !quoted => angle_depth = angle_depth.saturating_add(1),
                    '>' if !quoted => angle_depth = angle_depth.saturating_sub(1),
                    ' ' | '\t' if !quoted => candidate = Some(end),
                    ',' if !quoted && angle_depth == 0 => candidate = Some(end),
                    _ => {}
                }
            }

            if value[start..end].chars().count() + column > 78 {
                exceeded = true;
                if candidate.is_some() {
                    break;
                }
            }
        }

        let Some(mut break_at) = candidate else {
            break;
        };
        if !exceeded && value[start..].chars().count() + column <= 78 {
            break;
        }
        while break_at > start
            && value[..break_at]
                .chars()
                .last()
                .is_some_and(char::is_whitespace)
        {
            break_at -= value[..break_at]
                .chars()
                .last()
                .map(char::len_utf8)
                .unwrap_or(0);
        }
        if break_at == start {
            break;
        }
        raw.push_str(&value[start..break_at]);
        raw.push_str("\r\n ");
        start = break_at;
        while start < value.len()
            && value[start..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        {
            start += value[start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
        }
        column = 1;
    }
    raw.push_str(value[start..].trim_start());
    raw.push_str("\r\n");
}

fn append_recipient_header(raw: &mut String, name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Ok(());
    }

    // Evolution accepts semicolon-separated recipient lists in its fields;
    // RFC822 uses commas, so normalize only the separator at this boundary.
    let normalized = normalize_recipient_separators(value);
    let encoded = encode_address_header(&normalized)?;
    append_header_line(raw, name, &encoded);
    Ok(())
}

fn normalize_recipient_separators(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut quoted = false;
    let mut angle_depth = 0u8;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            escaped = false;
            normalized.push(character);
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
            normalized.push(character);
            continue;
        }
        match character {
            '"' => quoted = !quoted,
            '<' if !quoted => angle_depth = angle_depth.saturating_add(1),
            '>' if !quoted => angle_depth = angle_depth.saturating_sub(1),
            ';' if !quoted && angle_depth == 0 => {
                normalized.push(',');
                continue;
            }
            _ => {}
        }
        normalized.push(character);
    }
    normalized
}

fn format_message_id(name: &str, value: &str) -> Result<String, String> {
    validate_message_id(name, value)?;
    let value = value.trim();
    if value.starts_with('<') && value.ends_with('>') {
        Ok(value.to_string())
    } else {
        Ok(format!("<{value}>"))
    }
}

fn format_message_id_list(name: &str, values: &[String]) -> Result<String, String> {
    values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format_message_id(name, value))
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join(" "))
}

fn validate_header_value(name: &str, value: &str) -> Result<(), String> {
    if value
        .chars()
        .any(|character| character == '\r' || character == '\n')
    {
        return Err(format!("{name} contains invalid line breaks"));
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\t')
    {
        return Err(format!("{name} contains invalid control characters"));
    }
    Ok(())
}

fn validate_header_values(name: &str, values: &[String]) -> Result<(), String> {
    for value in values {
        if !value.trim().is_empty() {
            validate_message_id(name, value)?;
        }
    }
    Ok(())
}

fn validate_message_id(name: &str, value: &str) -> Result<(), String> {
    validate_header_value(name, value)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{name} must not be empty"));
    }

    let wrapped = value.starts_with('<') || value.ends_with('>');
    let inner = if value.starts_with('<') && value.ends_with('>') {
        &value[1..value.len() - 1]
    } else if wrapped {
        return Err(format!("{name} has invalid angle brackets"));
    } else {
        value
    };
    if inner.is_empty()
        || inner
            .chars()
            .any(|character| character.is_whitespace() || character == '<' || character == '>')
    {
        return Err(format!("{name} has an invalid format"));
    }
    Ok(())
}

fn validate_calendar_method(value: &str) -> Result<(), String> {
    validate_header_value("iTIP method", value)?;
    let method = value.trim();
    if !is_mime_token(method) {
        return Err("Invalid iTIP method".to_string());
    }
    Ok(())
}

fn encode_header_value(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }

    // RFC 2047 encoded-words are limited to 75 characters including their
    // delimiters.  Forty-two UTF-8 bytes produce at most fifty-six base64
    // characters, leaving room for the UTF-8/base64 markers and a conservative
    // 78-column header wrap.
    let mut chunks = Vec::<Vec<u8>>::new();
    for character in value.chars() {
        let bytes = character.to_string();
        let bytes = bytes.as_bytes();
        if chunks
            .last()
            .is_some_and(|chunk| chunk.len() + bytes.len() > 42)
            || chunks.is_empty()
        {
            chunks.push(Vec::with_capacity(42));
        }
        chunks
            .last_mut()
            .expect("a chunk was just created")
            .extend(bytes);
    }
    chunks
        .iter()
        .map(|chunk| format!("=?UTF-8?B?{}?=", base64(chunk)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn encode_address_header(value: &str) -> Result<String, String> {
    let mut output = String::with_capacity(value.len());
    let mut phrase = String::new();
    let mut address = String::new();
    let mut in_address = false;
    let mut saw_address = false;

    for character in value.trim().chars() {
        if in_address {
            if character == '>' {
                validate_addr_spec(&address)?;
                output.push_str(address.trim());
                output.push('>');
                address.clear();
                in_address = false;
                saw_address = true;
            } else if character == '<' {
                return Err("Address contains nested angle brackets".to_string());
            } else {
                address.push(character);
            }
        } else if character == '<' {
            output.push_str(&encode_address_phrase(&phrase));
            phrase.clear();
            output.push('<');
            in_address = true;
        } else if character == '>' {
            return Err("Address contains a closing angle bracket without an address".to_string());
        } else {
            phrase.push(character);
        }
    }

    if in_address {
        return Err("Address contains an unclosed angle bracket".to_string());
    } else if saw_address {
        output.push_str(&encode_address_phrase(&phrase));
    } else {
        validate_bare_address_list(&phrase)?;
        output.push_str(&phrase);
    }
    Ok(output)
}

fn validate_addr_spec(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty()
        || !value.is_ascii()
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err("Address does not contain an RFC-compliant ASCII address".to_string());
    }
    Ok(())
}

fn validate_bare_address_list(value: &str) -> Result<(), String> {
    let mut recipient = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in value.chars().chain(std::iter::once(',')) {
        if escaped {
            escaped = false;
            recipient.push(character);
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
            recipient.push(character);
            continue;
        }
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                validate_addr_spec(&recipient)?;
                recipient.clear();
                continue;
            }
            _ => recipient.push(character),
        }
    }
    if quoted {
        return Err("Address contains an unclosed quotation mark".to_string());
    }
    Ok(())
}

fn encode_address_phrase(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut token = String::new();
    let mut quoted = false;

    for character in value.chars() {
        if character == '"' {
            quoted = !quoted;
            token.push(character);
        } else if character.is_whitespace() && !quoted {
            append_encoded_phrase_token(&mut output, &token);
            token.clear();
            output.push(character);
        } else {
            token.push(character);
        }
    }
    append_encoded_phrase_token(&mut output, &token);
    output
}

fn append_encoded_phrase_token(output: &mut String, token: &str) {
    if token.is_empty() || token.is_ascii() {
        output.push_str(token);
    } else if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
        output.push_str(&encode_header_value(&token[1..token.len() - 1]));
    } else {
        output.push_str(&encode_header_value(token));
    }
}

fn normalize_body(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

/// Convert sanitized composer HTML into a readable plain-text MIME body.
/// The editor has a native plain-text mode, but drafts and service callers can
/// still contain HTML, so this remains the final MIME-layer invariant.
fn html_to_plain_text(value: &str) -> String {
    crate::services::html_sanitizer::html_to_plain_text(value)
}

pub(crate) fn wrap_html_with_typography(html: &str, font_family: &str, font_size: u32) -> String {
    let trimmed = html.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.starts_with("<html") {
        return html.to_string();
    }

    let safe_font_family = if font_family.trim().is_empty() {
        "system-ui, -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif"
    } else {
        font_family.trim()
    };
    let safe_size = if font_size < 8 || font_size > 72 {
        14
    } else {
        font_size
    };

    format!(
        "<!DOCTYPE html>\r\n<html>\r\n<head>\r\n<meta charset=\"utf-8\">\r\n<style type=\"text/css\">\r\nbody, p, div {{\r\n  font-family: {safe_font_family};\r\n  font-size: {safe_size}px;\r\n  line-height: 1.5;\r\n  color: #222222;\r\n}}\r\nimg {{\r\n  max-width: 100%;\r\n  height: auto;\r\n}}\r\n</style>\r\n</head>\r\n<body style=\"font-family: {safe_font_family}; font-size: {safe_size}px; line-height: 1.5; color: #222222;\">\r\n{trimmed}\r\n</body>\r\n</html>"
    )
}

pub(crate) fn extract_inline_data_images(html: &str) -> (String, Vec<ExportAttachment>) {
    let mut output = String::with_capacity(html.len());
    let mut attachments = Vec::new();
    let mut cursor = 0;
    let mut counter = 1usize;

    while cursor < html.len() {
        let remaining = &html[cursor..];
        let Some(tag_start) = remaining.find('<') else {
            output.push_str(remaining);
            break;
        };
        let tag_start_idx = cursor + tag_start;
        output.push_str(&html[cursor..tag_start_idx]);
        cursor = tag_start_idx;

        let Some(tag_end_rel) = html[cursor..].find('>') else {
            output.push_str(&html[cursor..]);
            break;
        };
        let tag_end_idx = cursor + tag_end_rel;
        let tag_str = &html[cursor..=tag_end_idx];

        let tag_lower = tag_str.to_ascii_lowercase();
        if !tag_lower.starts_with("<img") && !tag_lower.starts_with("<source") {
            output.push_str(tag_str);
            cursor = tag_end_idx + 1;
            continue;
        }

        if let Some(data_pos) = tag_str.find("data:image/") {
            let before = &tag_str[..data_pos];
            if let Some(quote_idx) = before.rfind(['"', '\'']) {
                let quote = tag_str.as_bytes()[quote_idx];
                let before_quote = before[..quote_idx].trim_end();
                if before_quote.ends_with('=') {
                    let before_eq = before_quote[..before_quote.len() - 1].trim_end();
                    if before_eq.to_ascii_lowercase().ends_with("src") {
                        if let Some(end_quote_rel) = tag_str[data_pos..].find(quote as char) {
                            let end_quote_idx = data_pos + end_quote_rel;
                            let data_uri = &tag_str[data_pos..end_quote_idx];
                            if let Some(rest) = data_uri.strip_prefix("data:image/") {
                                if let Some(b64_pos) = rest.find(";base64,") {
                                    let mime_sub = &rest[..b64_pos];
                                    let b64_payload = &rest[b64_pos + 8..];
                                    let cleaned: String = b64_payload.chars().filter(|c| !c.is_whitespace()).collect();
                                    if let Ok(bytes) = decode_embedded_attachment(&cleaned) {
                                        if !bytes.is_empty() {
                                            let mime_type = format!("image/{mime_sub}");
                                            let ext = match mime_sub.to_ascii_lowercase().as_str() {
                                                "png" => "png",
                                                "jpeg" | "jpg" => "jpg",
                                                "gif" => "gif",
                                                "webp" => "webp",
                                                "svg+xml" => "svg",
                                                _ => "png",
                                            };
                                            let cid_uuid = uuid::Uuid::new_v4().simple().to_string();
                                            let cid = format!("img-{cid_uuid}@meridian.local");
                                            let filename = format!("image-{counter}.{ext}");
                                            counter += 1;

                                            attachments.push(ExportAttachment {
                                                filename,
                                                mime_type,
                                                content_id: Some(format!("<{cid}>")),
                                                content: bytes.into(),
                                            });

                                            output.push_str(&tag_str[..data_pos]);
                                            output.push_str(&format!("cid:{cid}"));
                                            output.push_str(&tag_str[end_quote_idx..]);
                                            cursor = tag_end_idx + 1;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        output.push_str(tag_str);
        cursor = tag_end_idx + 1;
    }

    (output, attachments)
}

fn ensure_crlf(value: &mut String) {
    if !value.ends_with("\r\n") {
        value.push_str("\r\n");
    }
}

fn base64_with_crlf(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for chunk in bytes.chunks(57) {
        encoded.push_str(&base64(chunk));
        encoded.push_str("\r\n");
    }
    encoded
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(
            TABLE[(((first & 0x03) << 4) | second.map_or(0, |value| value >> 4)) as usize] as char,
        );
        encoded.push(second.map_or('=', |value| {
            TABLE[(((value & 0x0f) << 2) | third.map_or(0, |third| third >> 6)) as usize] as char
        }));
        encoded.push(third.map_or('=', |value| TABLE[(value & 0x3f) as usize] as char));
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn builds_html_rfc822_message() {
        let raw = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: "Person <person@example.org>".to_string(),
            reply_to: "reply@example.org".to_string(),
            request_read_receipt: true,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Grüße".to_string(),
            html_body: "<p>Hello</p>".to_string(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect("message should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("From: Person <person@example.org>\r\n"));
        assert!(text.contains("Reply-To: reply@example.org\r\n"));
        assert!(text.contains("Disposition-Notification-To: Person <person@example.org>\r\n"));
        assert!(text.contains("To: person@example.org\r\n"));
        assert!(text.contains("Subject: =?UTF-8?B?R3LDvMOfZQ==?=\r\n"));
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(text.contains("Content-Type: text/plain; charset=UTF-8\r\n"));
        assert!(text.contains("Content-Type: text/html; charset=UTF-8\r\n"));
        assert!(text.contains("<p>Hello</p>\r\n"));
        assert!(text.ends_with("--\r\n"));
    }

    #[test]
    fn emits_per_message_receipt_and_priority_headers() {
        let mut message = outgoing_with_attachments(Vec::new());
        message.from = "Sender <sender@example.org>".to_string();
        message.request_read_receipt = true;
        message.priority = true;

        let raw = build_rfc822(&message).expect("message options should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");

        assert!(text.contains("Disposition-Notification-To: Sender <sender@example.org>\r\n"));
        assert!(text.contains("X-Priority: 1\r\n"));
    }

    #[test]
    fn encodes_selected_legacy_charset_in_text_mime_part() {
        let mut message = outgoing_with_attachments(Vec::new());
        message.plain_text = true;
        message.html_body = "Grüße".to_string();
        message.charset = "latin1".to_string();

        let raw = build_rfc822(&message).expect("ISO-8859-1 message should build");
        let text = String::from_utf8(raw).expect("MIME envelope should remain ASCII");

        assert!(text.contains("Content-Type: text/plain; charset=ISO-8859-1\r\n"));
        assert!(text.contains("Content-Transfer-Encoding: base64\r\n"));
        let expected = base64_with_crlf(&[b'G', b'r', 0xfc, 0xdf, b'e']);
        assert!(text.contains(&expected));
    }

    #[test]
    fn rejects_unsupported_per_message_charset() {
        let mut message = outgoing_with_attachments(Vec::new());
        message.charset = "KOI8-R".to_string();

        let error = build_rfc822(&message).expect_err("unsupported charset should fail closed");
        assert!(error.contains("Unsupported character encoding"));
    }

    #[test]
    fn atomic_eml_save_can_be_cancelled_without_touching_the_destination() {
        let root = std::env::temp_dir().join(format!(
            "meridian-atomic-cancel-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("temporary directory should be created");
        let path = root.join("message.eml");
        fs::write(&path, b"old").expect("old destination should be written");
        let cancellation = CancellationToken::new();
        assert!(cancellation.request_cancel());

        assert_eq!(
            save_rfc822_atomically(&path, b"new", &cancellation)
                .expect("cancelled save should be reported"),
            AtomicSaveResult::Cancelled
        );
        assert_eq!(
            fs::read(&path).expect("destination should remain readable"),
            b"old"
        );
        assert_eq!(
            fs::read_dir(&root)
                .expect("directory should be readable")
                .count(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_eml_save_replaces_the_destination_only_after_commit() {
        let root = std::env::temp_dir().join(format!(
            "meridian-atomic-save-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("temporary directory should be created");
        let path = root.join("message.eml");
        fs::write(&path, b"old").expect("old destination should be written");
        let cancellation = CancellationToken::new();

        assert_eq!(
            save_rfc822_atomically(&path, b"new", &cancellation)
                .expect("atomic save should succeed"),
            AtomicSaveResult::Saved
        );
        assert_eq!(
            fs::read(&path).expect("destination should be readable"),
            b"new"
        );
        assert!(cancellation.is_committed());
        assert_eq!(
            fs::read_dir(&root)
                .expect("directory should be readable")
                .count(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn long_subjects_use_bounded_rfc2047_encoded_words_and_folding() {
        let mut message = outgoing_with_attachments(Vec::new());
        message.subject = "ä".repeat(120);
        let raw = build_rfc822(&message).expect("long subject should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        let headers = text
            .split_once("\r\n\r\n")
            .expect("headers should be present")
            .0;
        for line in headers.split("\r\n") {
            assert!(line.len() <= 998, "header line is too long: {line}");
            if line.starts_with("Subject:") || line.starts_with(' ') {
                assert!(line.len() <= 78, "folded subject line is too long: {line}");
            }
        }
        assert!(text.contains("Subject: =?UTF-8?B?"));
        assert!(text.contains("\r\n =?UTF-8?B?"));
    }

    #[test]
    fn rejects_non_ascii_bare_addresses_and_receipts_without_a_sender() {
        let mut message = outgoing_with_attachments(Vec::new());
        message.to = "jörg@example.org".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("SMTPUTF8 addresses need an explicit backend capability")
                .contains("RFC-compliant ASCII address")
        );

        message.to = "person@example.org".to_string();
        message.request_read_receipt = true;
        assert!(
            build_rfc822(&message)
                .expect_err("read receipt without a sender must fail closed")
                .contains("sender address")
        );
    }

    #[test]
    fn builds_multipart_attachment_and_normalizes_recipients() {
        let path = temporary_file("attachment", b"hello");
        let raw = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "one@example.org; two@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Report".to_string(),
            html_body: "<p>Body</p>\n".to_string(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: vec![DraftAttachment {
                path: path.to_string_lossy().into_owned(),
                filename: "hello.txt".to_string(),
                mime_type: "text/plain".to_string(),
                size: 5,
                content_id: None,
                embedded_content_base64: String::new(),
            }],
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect("multipart message should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("To: one@example.org, two@example.org\r\n"));
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\""));
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(text.contains("Content-Disposition: attachment; filename=\"hello.txt\""));
        assert!(text.contains("aGVsbG8=\r\n"));
        assert!(text.ends_with("--\r\n"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn embedded_rfc822_attachment_survives_draft_and_mime_build_without_a_path() {
        let original = b"From: sender@example.org\r\nSubject: Original\r\n\r\nBody\r\n";
        let attachment = embedded_draft_attachment("Original.eml", "message/rfc822", original)
            .expect("RFC822 payload should embed");
        assert!(attachment.path.starts_with("embedded:"));
        assert!(!attachment.embedded_content_base64.is_empty());

        let serialized = serde_json::to_string(&attachment).expect("attachment should serialize");
        let restored: DraftAttachment =
            serde_json::from_str(&serialized).expect("attachment should deserialize");
        assert_eq!(restored, attachment);

        let raw = build_rfc822(&outgoing_with_attachments(vec![restored]))
            .expect("embedded attachment should build without filesystem access");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("Content-Type: message/rfc822"));
        assert!(text.contains("RnJvbTogc2VuZGVyQGV4YW1wbGUub3Jn"));
    }

    #[test]
    fn embedded_inline_attachment_preserves_content_id_through_draft_and_mime() {
        let attachment = embedded_draft_attachment_with_content_id(
            "logo.png",
            "image/png",
            Some("<logo@example.org>"),
            b"png-data",
        )
        .expect("inline payload should embed");
        let serialized = serde_json::to_string(&attachment).expect("attachment should serialize");
        let restored: DraftAttachment =
            serde_json::from_str(&serialized).expect("attachment should deserialize");
        assert_eq!(restored.content_id.as_deref(), Some("<logo@example.org>"));

        let raw = build_rfc822(&outgoing_with_attachments(vec![restored]))
            .expect("inline attachment should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("Content-ID: <logo@example.org>\r\n"));
        assert!(text.contains("Content-Disposition: inline; filename=\"logo.png\""));
    }

    #[test]
    fn embedded_attachment_rejects_empty_corrupt_and_size_mismatched_payloads() {
        assert!(
            embedded_draft_attachment("Original.eml", "message/rfc822", b"")
                .expect_err("empty embedded payload must fail")
                .contains("empty")
        );
        let mut corrupt = embedded_draft_attachment("Original.eml", "message/rfc822", b"message")
            .expect("fixture should embed");
        corrupt.embedded_content_base64 = "%%%=".to_string();
        assert!(
            build_rfc822(&outgoing_with_attachments(vec![corrupt]))
                .expect_err("corrupt Base64 must fail")
                .contains("Base64")
        );
        let mut mismatched =
            embedded_draft_attachment("Original.eml", "message/rfc822", b"message")
                .expect("fixture should embed");
        mismatched.size += 1;
        assert!(
            build_rfc822(&outgoing_with_attachments(vec![mismatched]))
                .expect_err("size mismatch must fail")
                .contains("size")
        );
    }

    #[test]
    fn exports_message_headers_thread_and_attachment() {
        let raw = build_export_rfc822(&ExportMessage {
            from: "Sender <sender@example.org>".to_string(),
            reply_to: "Reply <reply@example.org>".to_string(),
            to: "receiver@example.org".to_string(),
            cc: "team@example.org".to_string(),
            bcc: String::new(),
            subject: "Export".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            message_id: Some("message@example.org".to_string()),
            in_reply_to: vec!["parent@example.org".to_string()],
            references: vec!["root@example.org".to_string()],
            html_body: "<p>Body</p>".to_string(),
            attachments: vec![ExportAttachment {
                filename: "note.txt".to_string(),
                mime_type: "text/plain".to_string(),
                content_id: None,
                content: b"note".to_vec().into(),
            }],
        })
        .expect("export should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("From: Sender <sender@example.org>\r\n"));
        assert!(text.contains("Reply-To: Reply <reply@example.org>\r\n"));
        assert!(text.contains("Message-ID: <message@example.org>\r\n"));
        assert!(text.contains("In-Reply-To: <parent@example.org>\r\n"));
        assert!(text.contains("References: <root@example.org>\r\n"));
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(text.contains("Content-Disposition: attachment; filename=\"note.txt\""));
        assert!(text.contains("bm90ZQ==\r\n"));
    }

    #[test]
    fn rejects_header_injection_and_missing_recipients() {
        let mut message = OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Subject".to_string(),
            html_body: String::new(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };
        assert!(
            build_rfc822(&message)
                .expect_err("recipients should be required")
                .contains("recipient")
        );

        message.to = "person@example.org\r\nX-Injected: yes".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("header injection should be rejected")
                .contains("line breaks")
        );
    }

    #[test]
    fn builds_plain_text_messages_without_html_mime_type() {
        let raw = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Plain".to_string(),
            html_body: "Hello & goodbye\r\nSecond line".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect("plain message should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("Content-Type: text/plain; charset=UTF-8\r\n"));
        assert!(!text.contains("Content-Type: text/html; charset=UTF-8\r\n"));
        assert!(text.ends_with("Hello & goodbye\r\nSecond line\r\n"));
    }

    #[test]
    fn rich_text_gets_a_readable_plaintext_fallback() {
        let raw = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Rich".to_string(),
            html_body:
                "<style>body { display: none }</style><p>Hello &amp; team</p><p>Second<br>line</p>"
                    .to_string(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect("rich message should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("Hello & team\r\nSecond\r\nline\r\n"));
        assert!(text.contains("Content-Type: text/html; charset=UTF-8\r\n"));
        let plain_part = text
            .split("Content-Type: text/plain; charset=UTF-8\r\n")
            .nth(1)
            .expect("plain alternative should be present")
            .split("\r\n--")
            .next()
            .expect("plain alternative should have a body");
        assert!(!plain_part.contains("display: none"));
    }

    #[test]
    fn builds_itip_reply_as_calendar_alternative_part() {
        let component = concat!(
            "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\n",
            "BEGIN:VEVENT\r\nUID:meeting-1\r\n",
            "ATTENDEE;PARTSTAT=ACCEPTED:mailto:person@example.org\r\n",
            "END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let raw = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "organizer@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Re: Meeting".to_string(),
            html_body: "<p>Ich nehme teil.</p>".to_string(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: Some("REPLY".to_string()),
            calendar_component: Some(component.to_string()),
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect("iTIP response should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(text.contains("Content-Type: text/calendar; method=REPLY; charset=UTF-8\r\n"));
        assert!(text.contains("ATTENDEE;PARTSTAT=ACCEPTED:mailto:person@example.org"));
        assert!(text.ends_with("--\r\n"));
    }

    #[test]
    fn plain_text_conversion_preserves_blocks_and_decodes_entities() {
        assert_eq!(
            html_to_plain_text("<blockquote>Hi<br>there</blockquote><ul><li>A</li><li>B</li></ul>"),
            "Hi\nthere\nA\nB"
        );
        assert_eq!(
            normalize_body("one\ntwo\rthree\r\nfour"),
            "one\r\ntwo\r\nthree\r\nfour"
        );
    }

    #[test]
    fn encodes_unicode_display_names_without_corrupting_addresses() {
        let raw = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: "Jürgen Müller <juergen@example.org>".to_string(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "Zoë <zoe@example.org>".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Grüße".to_string(),
            html_body: "Hallo".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect("unicode address message should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains(
            "From: =?UTF-8?B?SsO8cmdlbg==?= =?UTF-8?B?TcO8bGxlcg==?= <juergen@example.org>\r\n"
        ));
        assert!(text.contains("To: =?UTF-8?B?Wm/Dqw==?= <zoe@example.org>\r\n"));
        assert!(!text.contains("From: Jürgen Müller"));
    }

    #[test]
    fn rejects_invalid_message_id_and_calendar_parameter_values() {
        let export = ExportMessage {
            from: String::new(),
            reply_to: String::new(),
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Subject".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            message_id: Some("message@example.org\r\nX-Injected: yes".to_string()),
            in_reply_to: Vec::new(),
            references: Vec::new(),
            html_body: "Body".to_string(),
            attachments: Vec::new(),
        };
        assert!(
            build_export_rfc822(&export)
                .expect_err("message-id injection should be rejected")
                .contains("line breaks")
        );

        let outgoing = OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Subject".to_string(),
            html_body: "Body".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: Some("REPLY;X-Injected".to_string()),
            calendar_component: Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR".to_string()),
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };
        assert!(
            build_rfc822(&outgoing)
                .expect_err("invalid iTIP parameter should be rejected")
                .contains("iTIP")
        );
    }

    #[test]
    fn validates_attachment_names_and_mime_types() {
        let path = temporary_file("unicode-attachment", b"payload");
        let mut message = outgoing_with_attachments(vec![DraftAttachment {
            path: path.to_string_lossy().into_owned(),
            filename: "Résumé 2026.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 7,
            content_id: None,
            embedded_content_base64: String::new(),
        }]);
        let raw = build_rfc822(&message).expect("unicode attachment should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("name*=utf-8''R%C3%A9sum%C3%A9%202026.pdf"));
        assert!(text.contains("filename*=utf-8''R%C3%A9sum%C3%A9%202026.pdf"));

        message.attachments[0].filename = "../secret.txt".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("path-like attachment names should be rejected")
                .contains("invalid path")
        );

        message.attachments[0].filename = "valid.txt".to_string();
        message.attachments[0].mime_type = "text/plain; charset=UTF-8".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("MIME parameters must not be injected")
                .contains("MIME type")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_directories_as_attachments() {
        let path = std::env::temp_dir().join(format!(
            "meridian-attachment-directory-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).expect("temporary attachment directory should be created");
        let message = outgoing_with_attachments(vec![DraftAttachment {
            path: path.to_string_lossy().into_owned(),
            filename: "directory".to_string(),
            mime_type: "application/octet-stream".to_string(),
            size: 0,
            content_id: None,
            embedded_content_base64: String::new(),
        }]);
        assert!(
            build_rfc822(&message)
                .expect_err("directories must not be sent as attachments")
                .contains("not a regular file")
        );
        let _ = fs::remove_dir(path);
    }

    #[test]
    fn openpgp_signing_requires_an_explicit_key() {
        let error = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Signed".to_string(),
            html_body: "Hello".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: true,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect_err("signing without a key must fail before invoking GnuPG");
        assert!(error.to_ascii_lowercase().contains("key"));
    }

    #[test]
    fn smime_signing_requires_certificate_and_key() {
        let error = build_rfc822(&OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Signed".to_string(),
            html_body: "Hello".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: Some(SmimeOptions {
                sign: true,
                ..SmimeOptions::default()
            }),
        })
        .expect_err("S/MIME signing without a certificate must fail before OpenSSL");
        assert!(
            error.to_ascii_lowercase().contains("certificate")
                || error.to_ascii_lowercase().contains("key")
        );
    }

    fn temporary_file(label: &str, contents: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "meridian-{label}-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, contents).expect("temporary attachment should be written");
        path
    }

    fn outgoing_with_attachments(attachments: Vec<DraftAttachment>) -> OutgoingMessage {
        OutgoingMessage {
            source_uid: "account-1".to_string(),
            from: String::new(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Attachment".to_string(),
            html_body: "<p>Body</p>".to_string(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments,
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        }
    }

    #[test]
    fn extracts_inline_data_image_and_replaces_with_cid() {
        let sample_html = r#"<p>Hello</p><p><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==" alt="test"></p>"#;
        let (processed, inline_attachments) = extract_inline_data_images(sample_html);
        assert_eq!(inline_attachments.len(), 1);
        let att = &inline_attachments[0];
        assert_eq!(att.mime_type, "image/png");
        assert!(att.filename.ends_with(".png"));
        let cid = att.content_id.as_ref().expect("must have content_id");
        assert!(cid.starts_with("<img-"));
        assert!(cid.ends_with("@meridian.local>"));

        let inner_cid = cid.trim_matches(['<', '>']);
        assert!(processed.contains(&format!("src=\"cid:{inner_cid}\"")));
        assert!(!processed.contains("data:image/png"));
    }

    #[test]
    fn wrap_html_with_typography_preserves_full_documents_and_styles_fragments() {
        let fragment = "<p>Message content</p>";
        let wrapped = wrap_html_with_typography(fragment, "Cantarell", 16);
        assert!(wrapped.starts_with("<!DOCTYPE html>"));
        assert!(wrapped.contains("font-family: Cantarell;"));
        assert!(wrapped.contains("font-size: 16px;"));
        assert!(wrapped.contains("<p>Message content</p>"));

        let full_doc = "<!DOCTYPE html><html><body>Already complete</body></html>";
        assert_eq!(wrap_html_with_typography(full_doc, "Cantarell", 16), full_doc);
    }

    #[test]
    fn builds_rfc822_with_inline_image_creates_multipart_related() {
        let mut msg = outgoing_with_attachments(Vec::new());
        msg.from = "sender@example.org".to_string();
        msg.html_body = r#"<p>Hi</p><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==">"#.to_string();

        let raw = build_rfc822(&msg).expect("message with inline image should build");
        let text = String::from_utf8(raw).expect("UTF-8 string");
        assert!(text.contains("Content-Type: multipart/related; type=\"text/html\"; boundary=\""));
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(text.contains("Content-Type: image/png; name=\"image-1.png\""));
        assert!(text.contains("Content-Disposition: inline; filename=\"image-1.png\""));
        assert!(text.contains("Content-ID: <img-"));
        assert!(text.contains("cid:img-"));
        assert!(!text.contains("data:image/png;base64"));
    }

    #[test]
    fn builds_rfc822_with_inline_image_and_regular_attachment() {
        let temp_dir = std::env::temp_dir().join(format!("meridian-test-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&temp_dir);
        let pdf_path = temp_dir.join("document.pdf");
        fs::write(&pdf_path, b"%PDF-1.4").expect("write temp pdf");

        let draft_att = DraftAttachment {
            filename: "document.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 8,
            path: pdf_path.to_string_lossy().to_string(),
            content_id: None,
            embedded_content_base64: String::new(),
        };

        let mut msg = outgoing_with_attachments(vec![draft_att]);
        msg.from = "sender@example.org".to_string();
        msg.html_body = r#"<p>Check attachment and logo:</p><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==">"#.to_string();

        let raw = build_rfc822(&msg).expect("message should build");
        let text = String::from_utf8(raw).expect("UTF-8 string");

        // Root must be multipart/mixed
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\""));
        // Child must be multipart/related
        assert!(text.contains("Content-Type: multipart/related; type=\"text/html\"; boundary=\""));
        // Inner child must be multipart/alternative
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        // Inline image must have disposition inline and Content-ID
        assert!(text.contains("Content-Disposition: inline; filename=\"image-1.png\""));
        assert!(text.contains("Content-ID: <img-"));
        // Regular attachment must have disposition attachment
        assert!(text.contains("Content-Disposition: attachment; filename=\"document.pdf\""));

        let _ = fs::remove_file(&pdf_path);
        let _ = fs::remove_dir(&temp_dir);
    }
}
