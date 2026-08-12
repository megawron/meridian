// SPDX-License-Identifier: GPL-3.0-or-later
//
// Cross-platform RFC822/MIME construction for the composer.  Account
// credentials and transport selection remain in EDS/Camel; this service only
// turns the GTK composer state into a complete message that Camel can send.

use super::crypto_service::SmimeOptions;
use super::draft_store_service::DraftAttachment;

use chrono::Utc;
use std::fs;

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
    pub(crate) content: Vec<u8>,
}

pub(crate) fn build_rfc822(message: &OutgoingMessage) -> Result<Vec<u8>, String> {
    if message.source_uid.trim().is_empty() {
        return Err("Kein Mailkonto ausgewählt".to_string());
    }

    let recipients = [
        message.to.as_str(),
        message.cc.as_str(),
        message.bcc.as_str(),
    ];
    if recipients.iter().all(|value| value.trim().is_empty()) {
        return Err("Mindestens ein Empfänger ist erforderlich".to_string());
    }

    validate_header_value("Empfänger", &message.to)?;
    validate_header_value("Cc", &message.cc)?;
    validate_header_value("Bcc", &message.bcc)?;
    validate_header_value("Betreff", &message.subject)?;
    validate_header_value("Von", &message.from)?;
    validate_header_value("Reply-To", &message.reply_to)?;
    if let Some(method) = message.calendar_method.as_deref() {
        validate_header_value("iTIP-Methode", method)?;
    }
    if message.calendar_component.is_some() != message.calendar_method.is_some() {
        return Err(
            "iTIP-Methode und iCalendar-Komponente müssen gemeinsam gesetzt werden".to_string(),
        );
    }

    let attachments = message
        .attachments
        .iter()
        .map(|attachment| {
            let content = fs::read(&attachment.path).map_err(|error| {
                format!(
                    "Anhang konnte nicht gelesen werden ({}): {error}",
                    attachment.filename
                )
            })?;
            Ok(ExportAttachment {
                filename: attachment.filename.clone(),
                mime_type: attachment.mime_type.clone(),
                content,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let raw = build_rfc822_parts(
        &message.from,
        &message.reply_to,
        message.request_read_receipt,
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
        return Err("OpenPGP und S/MIME können nicht gleichzeitig aktiviert werden".to_string());
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
    validate_header_value("Von", &message.from)?;
    validate_header_value("Reply-To", &message.reply_to)?;
    validate_header_value("Empfänger", &message.to)?;
    validate_header_value("Cc", &message.cc)?;
    validate_header_value("Bcc", &message.bcc)?;
    validate_header_value("Betreff", &message.subject)?;
    validate_header_value("Datum", &message.date)?;
    if let Some(message_id) = &message.message_id {
        validate_header_value("Message-ID", message_id)?;
    }
    for value in message.in_reply_to.iter().chain(message.references.iter()) {
        validate_header_value("Thread-Header", value)?;
    }

    build_rfc822_parts(
        &message.from,
        &message.reply_to,
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
        &message.attachments,
        None,
        None,
    )
}

fn build_rfc822_parts(
    from: &str,
    reply_to: &str,
    request_read_receipt: bool,
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
    attachments: &[ExportAttachment],
    calendar_method: Option<&str>,
    calendar_component: Option<&str>,
) -> Result<Vec<u8>, String> {
    let date_value = if date.trim().is_empty() {
        Utc::now().to_rfc2822()
    } else {
        date.trim().to_string()
    };
    let message_id_value = message_id
        .filter(|value| !value.trim().is_empty())
        .map(format_message_id)
        .unwrap_or_else(|| format!("<{}@evolution-gtk4.local>", uuid::Uuid::new_v4().simple()));

    let mut raw = String::new();
    raw.push_str("Date: ");
    raw.push_str(&date_value);
    raw.push_str("\r\nMessage-ID: ");
    raw.push_str(&message_id_value);
    raw.push_str("\r\n");
    if !from.trim().is_empty() {
        raw.push_str("From: ");
        raw.push_str(from.trim());
        raw.push_str("\r\n");
    }
    if !reply_to.trim().is_empty() {
        raw.push_str("Reply-To: ");
        raw.push_str(reply_to.trim());
        raw.push_str("\r\n");
    }
    if request_read_receipt && !from.trim().is_empty() {
        raw.push_str("Disposition-Notification-To: ");
        raw.push_str(from.trim());
        raw.push_str("\r\n");
    }
    append_recipient_header(&mut raw, "To", to);
    append_recipient_header(&mut raw, "Cc", cc);
    append_recipient_header(&mut raw, "Bcc", bcc);
    if !in_reply_to.is_empty() {
        raw.push_str("In-Reply-To: ");
        raw.push_str(&format_message_id_list(in_reply_to));
        raw.push_str("\r\n");
    }
    if !references.is_empty() {
        raw.push_str("References: ");
        raw.push_str(&format_message_id_list(references));
        raw.push_str("\r\n");
    }
    raw.push_str("Subject: ");
    raw.push_str(&encode_header_value(subject));
    raw.push_str("\r\nMIME-Version: 1.0\r\n");

    let body = if plain_text {
        normalize_body(&html_to_plain_text(html_body))
    } else {
        normalize_body(html_body)
    };
    let body_content_type = if plain_text {
        "text/plain; charset=UTF-8"
    } else {
        "text/html; charset=UTF-8"
    };
    if attachments.is_empty() && calendar_component.is_none() {
        raw.push_str("Content-Type: ");
        raw.push_str(body_content_type);
        raw.push_str("\r\n");
        raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        raw.push_str(&body);
        ensure_crlf(&mut raw);
    } else {
        let boundary = format!("=_EvolutionGTK4_{}", uuid::Uuid::new_v4().simple());
        let multipart_kind = if calendar_component.is_some() && attachments.is_empty() {
            "alternative"
        } else {
            "mixed"
        };
        raw.push_str("Content-Type: multipart/");
        raw.push_str(multipart_kind);
        raw.push_str("; boundary=\"");
        raw.push_str(&boundary);
        raw.push_str("\"\r\n\r\n");
        raw.push_str("--");
        raw.push_str(&boundary);
        raw.push_str("\r\nContent-Type: ");
        raw.push_str(body_content_type);
        raw.push_str("\r\n");
        raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        raw.push_str(&body);
        ensure_crlf(&mut raw);

        if let (Some(method), Some(component)) = (calendar_method, calendar_component) {
            append_calendar_part(&mut raw, &boundary, method, component)?;
        }

        for attachment in attachments {
            let filename = quoted_parameter(&attachment.filename, "Dateiname")?;
            let mime_type = if attachment.mime_type.trim().is_empty() {
                "application/octet-stream"
            } else {
                attachment.mime_type.trim()
            };
            validate_header_value("MIME-Typ", mime_type)?;
            raw.push_str("--");
            raw.push_str(&boundary);
            raw.push_str("\r\nContent-Type: ");
            raw.push_str(mime_type);
            raw.push_str("; name=\"");
            raw.push_str(&filename);
            raw.push_str("\"\r\nContent-Transfer-Encoding: base64\r\n");
            raw.push_str("Content-Disposition: attachment; filename=\"");
            raw.push_str(&filename);
            raw.push_str("\"\r\n\r\n");
            raw.push_str(&base64_with_crlf(&attachment.content));
        }
        raw.push_str("--");
        raw.push_str(&boundary);
        raw.push_str("--\r\n");
    }
    Ok(raw.into_bytes())
}

fn append_calendar_part(
    raw: &mut String,
    boundary: &str,
    method: &str,
    component: &str,
) -> Result<(), String> {
    validate_header_value("iTIP-Methode", method)?;
    if method.trim().is_empty() || component.trim().is_empty() {
        return Err("Die iTIP-MIME-Komponente darf nicht leer sein".to_string());
    }
    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("\r\nContent-Type: text/calendar; method=");
    raw.push_str(method.trim());
    raw.push_str("; charset=UTF-8\r\nContent-Disposition: inline\r\n");
    raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    raw.push_str(component.trim_end());
    ensure_crlf(raw);
    Ok(())
}

fn append_recipient_header(raw: &mut String, name: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }

    raw.push_str(name);
    raw.push_str(": ");
    // Evolution accepts semicolon-separated recipient lists in its fields;
    // RFC822 uses commas, so normalize only the separator at this boundary.
    raw.push_str(&value.replace(';', ","));
    raw.push_str("\r\n");
}

fn format_message_id(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('<') && value.ends_with('>') {
        value.to_string()
    } else {
        format!("<{value}>")
    }
}

fn format_message_id_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format_message_id(value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_header_value(name: &str, value: &str) -> Result<(), String> {
    if value
        .chars()
        .any(|character| character == '\r' || character == '\n')
    {
        return Err(format!("{name} enthält ungültige Zeilenumbrüche"));
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\t')
    {
        return Err(format!("{name} enthält ungültige Steuerzeichen"));
    }
    Ok(())
}

fn encode_header_value(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }

    format!("=?UTF-8?B?{}?=", base64(value.as_bytes()))
}

fn quoted_parameter(value: &str, name: &str) -> Result<String, String> {
    validate_header_value(name, value)?;
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
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
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    let mut tag = String::new();

    for character in value.chars() {
        if in_tag {
            if character == '>' {
                let normalized = tag.trim().to_ascii_lowercase();
                if normalized == "br"
                    || normalized == "br/"
                    || normalized.starts_with("br ")
                    || normalized == "/p"
                    || normalized == "/div"
                    || normalized == "/li"
                    || normalized == "/blockquote"
                    || normalized == "/h1"
                    || normalized == "/h2"
                    || normalized == "/h3"
                    || normalized == "/h4"
                    || normalized == "/h5"
                    || normalized == "/h6"
                {
                    push_newline(&mut output);
                }
                in_tag = false;
                tag.clear();
            } else {
                tag.push(character);
            }
            continue;
        }

        if character == '<' {
            in_tag = true;
            tag.clear();
        } else {
            output.push(character);
        }
    }

    decode_basic_entities(&output)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn push_newline(output: &mut String) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

fn decode_basic_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
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

fn base64(bytes: &[u8]) -> String {
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
        assert!(text.contains("Content-Type: text/html; charset=UTF-8\r\n"));
        assert!(text.ends_with("<p>Hello</p>\r\n"));
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
            attachments: vec![DraftAttachment {
                path: path.to_string_lossy().into_owned(),
                filename: "hello.txt".to_string(),
                mime_type: "text/plain".to_string(),
                size: 5,
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
        assert!(text.contains("Content-Disposition: attachment; filename=\"hello.txt\""));
        assert!(text.contains("aGVsbG8=\r\n"));
        assert!(text.ends_with("--\r\n"));
        let _ = fs::remove_file(path);
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
                content: b"note".to_vec(),
            }],
        })
        .expect("export should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("From: Sender <sender@example.org>\r\n"));
        assert!(text.contains("Reply-To: Reply <reply@example.org>\r\n"));
        assert!(text.contains("Message-ID: <message@example.org>\r\n"));
        assert!(text.contains("In-Reply-To: <parent@example.org>\r\n"));
        assert!(text.contains("References: <root@example.org>\r\n"));
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
                .contains("Empfänger")
        );

        message.to = "person@example.org\r\nX-Injected: yes".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("header injection should be rejected")
                .contains("Zeilenumbrüche")
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
            html_body: "<p>Hello &amp; goodbye</p><p>Second line</p>".to_string(),
            plain_text: true,
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
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: true,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        })
        .expect_err("signing without a key must fail before invoking GnuPG");
        assert!(error.contains("kein passender Schlüssel"));
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
        assert!(error.contains("Zertifikat oder privater Schlüssel fehlt"));
    }

    fn temporary_file(label: &str, contents: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-{label}-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, contents).expect("temporary attachment should be written");
        path
    }
}
