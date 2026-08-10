// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailStoreService — local mail ingestion with sidecar UI metadata.
//
// The first local backend deliberately does not create or modify Evolution
// mail data.  Read/flag state is kept in an application-owned sidecar file.
// It discovers mail that Evolution already owns and supports the three
// formats encountered in local Evolution installations:
//
//   * RFC 822 / EML files
//   * Maildir (cur/, new/ and tmp/)
//   * Unix mbox files used by Evolution's local provider
//
// Network synchronization, flags stored in Camel indexes and mutations such
// as move/delete are separate backend work and must not be faked here.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::models::mail_message::{AttachmentInfo, MailMessageObject};
use crate::services::mail_filter_service::{
    FilterAction, FilterScope, MailFilterRule, MailFilterStore,
};
use crate::services::mail_metadata_service::{MailMessageState, MailMetadataStore};
use crate::services::mail_thread_service::{
    ThreadIdentity, extract_message_ids, group_messages, normalize_message_id,
};

/// Thread-safe parser output.  GTK objects are created only after this value
/// returns to the main context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MailBackendLocation {
    pub(crate) source_uid: String,
    pub(crate) folder_name: String,
    pub(crate) message_uid: String,
}

#[derive(Debug, Clone)]
pub struct ParsedMail {
    pub id: String,
    pub folder_id: String,
    pub folder_name: String,
    pub backend_location: Option<MailBackendLocation>,
    pub sender_name: String,
    pub sender_email: String,
    pub recipient_to: String,
    pub subject: String,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub thread_id: String,
    pub thread_size: usize,
    pub thread_position: usize,
    pub date_str: String,
    pub preview_snippet: String,
    pub html_body: String,
    pub is_unread: bool,
    pub is_flagged: bool,
    pub attachments: Vec<AttachmentInfo>,
}

impl ParsedMail {
    pub fn into_object(self) -> MailMessageObject {
        let ParsedMail {
            id,
            folder_id,
            sender_name,
            sender_email,
            recipient_to,
            subject,
            message_id,
            in_reply_to,
            references,
            thread_id,
            thread_size,
            thread_position,
            date_str,
            preview_snippet,
            html_body,
            is_unread,
            is_flagged,
            backend_location,
            attachments,
            ..
        } = self;

        MailMessageObject::new(
            &id,
            &folder_id,
            &sender_name,
            &sender_email,
            &recipient_to,
            &subject,
            message_id,
            in_reply_to,
            references,
            &thread_id,
            thread_size,
            thread_position,
            &date_str,
            &preview_snippet,
            &html_body,
            is_unread,
            is_flagged,
            backend_location,
            attachments,
        )
    }
}

/* Keep the destructuring above explicit in the GTK conversion while preserving
 * backend location metadata for remote actions in the legacy object. */
impl ParsedMail {
    #[allow(dead_code)]
    pub(crate) fn backend_location(&self) -> Option<&MailBackendLocation> {
        self.backend_location.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct MailStoreService {
    /// Root directory used for local mail discovery.
    ///
    /// `mail_dir` remains public so callers and tests can point the service at
    /// an explicit mailbox without relying on environment variables.
    pub mail_dir: PathBuf,
    metadata: MailMetadataStore,
}

impl MailStoreService {
    /// Create a service pointing at the user's existing Evolution local store.
    ///
    /// No directory is created and no sample data is written.  The optional
    /// `EVOLUTION_GTK4_MAIL_DIR` override is useful for tests and for users
    /// importing a local mailbox from another location.
    pub fn new() -> Self {
        let mail_dir = std::env::var_os("EVOLUTION_GTK4_MAIL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution/mail/local"));
        let metadata_path = std::env::var_os("EVOLUTION_GTK4_MAIL_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4/mail-state.json"));

        Self {
            mail_dir,
            metadata: MailMetadataStore::new(metadata_path),
        }
    }

    /// Construct a service for an explicit mailbox root.
    #[allow(dead_code)]
    pub fn with_mail_dir<P: Into<PathBuf>>(mail_dir: P) -> Self {
        let mail_dir = mail_dir.into();
        let metadata_path = mail_dir.join(".evolution-gtk4-state.json");
        Self {
            mail_dir,
            metadata: MailMetadataStore::new(metadata_path),
        }
    }

    /// Construct a local store with an explicit metadata path, useful for
    /// imports and tests that must keep the mailbox and client state apart.
    #[allow(dead_code)]
    pub fn with_mail_dir_and_state_path(
        mail_dir: impl Into<PathBuf>,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            mail_dir: mail_dir.into(),
            metadata: MailMetadataStore::new(state_path),
        }
    }

    /// Read all discoverable local messages in a deterministic order.
    ///
    /// This method is synchronous by design so that it can also be used by
    /// importers and tests.  The GTK caller must move it off the main thread
    /// once large local stores are supported; the asynchronous UI pipeline is
    /// tracked separately in the roadmap.
    #[allow(dead_code)]
    pub fn load_local_messages(&self) -> Vec<MailMessageObject> {
        self.load_local_records()
            .into_iter()
            .map(ParsedMail::into_object)
            .collect()
    }

    /// Parse local mail without constructing GTK objects.
    ///
    /// This is the method intended for worker threads.  Its return type is
    /// made only of owned, thread-safe Rust data.
    pub fn load_local_records(&self) -> Vec<ParsedMail> {
        let cancelled = AtomicBool::new(false);
        self.load_local_records_cancelable(&cancelled)
    }

    /// Load local messages while allowing a caller to stop a worker that is
    /// no longer needed.  The parser returns records collected before the
    /// cancellation was observed and never mutates the source mailbox.
    pub fn load_local_records_cancelable(&self, cancelled: &AtomicBool) -> Vec<ParsedMail> {
        let metadata = self.metadata.load();
        let filter_rules = MailFilterStore::default().load();
        let mut paths = Vec::new();
        collect_mail_files(&self.mail_dir, &mut paths);
        paths.sort();

        let mut messages = Vec::new();
        for path in paths {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let Ok(raw) = fs::read(&path) else {
                tracing::warn!(path = %path.display(), "Could not read local mail file");
                continue;
            };

            let text = String::from_utf8_lossy(&raw);
            let unread = maildir_message_is_unread(&path);

            if looks_like_mbox(&text) {
                messages.extend(parse_mbox(&path, &text, unread));
            } else if looks_like_rfc822(&text, &path) {
                let id = message_id(&path, 0);
                let folder_id = folder_id_for_path(&path);
                if let Some(message) = parse_rfc822(&id, &folder_id, &text, unread) {
                    messages.push(message);
                }
            }
        }

        for message in &mut messages {
            apply_client_state(&metadata, &filter_rules, message);
        }

        assign_thread_metadata(&mut messages);

        messages
    }

    pub fn persist_message_state(
        &self,
        id: &str,
        is_unread: bool,
        is_flagged: bool,
    ) -> std::io::Result<()> {
        self.metadata.update(
            id,
            MailMessageState {
                is_unread,
                is_flagged,
            },
        )
    }

    /// Apply application-owned read/flag state and local filter rules to
    /// records supplied by another local/read-only backend.
    #[cfg(eds_camel_bridge)]
    #[allow(dead_code)]
    pub(crate) fn apply_client_state(&self, messages: &mut [ParsedMail]) {
        let metadata = self.metadata.load();
        let filter_rules = MailFilterStore::default().load();

        for message in messages {
            apply_client_state(&metadata, &filter_rules, message);
        }
    }

    /// Apply only local incoming filter actions to messages whose flags are
    /// already authoritative in Camel/EDS.  Remote messages must not be
    /// overwritten by a stale client sidecar after a successful Camel
    /// mutation.
    #[cfg(eds_camel_bridge)]
    pub(crate) fn apply_backend_filters(&self, messages: &mut [ParsedMail]) {
        let filter_rules = MailFilterStore::default().load();

        for message in messages {
            apply_local_filters(&filter_rules, message);
        }
    }

    #[allow(dead_code)]
    pub fn metadata_path(&self) -> &Path {
        self.metadata.path()
    }
}

fn apply_client_state(
    metadata: &HashMap<String, MailMessageState>,
    filter_rules: &[MailFilterRule],
    message: &mut ParsedMail,
) {
    if let Some(state) = metadata.get(&message.id) {
        message.is_unread = state.is_unread;
        message.is_flagged = state.is_flagged;
    }

    apply_local_filters(filter_rules, message);
}

fn apply_local_filters(rules: &[MailFilterRule], message: &mut ParsedMail) {
    let body = format!("{} {}", message.preview_snippet, message.html_body);
    let sender = format!("{} <{}>", message.sender_name, message.sender_email);

    for rule in rules {
        if rule.scope != FilterScope::Incoming
            || !rule.matches(&message.subject, &sender, &body, "")
        {
            continue;
        }

        match &rule.action {
            FilterAction::MarkRead => message.is_unread = false,
            FilterAction::SetFlagged => message.is_flagged = true,
            FilterAction::MoveToFolder(folder) | FilterAction::CopyToFolder(folder) => {
                tracing::debug!(
                    rule = %rule.name,
                    folder,
                    "Local filter matched folder action; Camel backend is required to apply it"
                );
            }
            FilterAction::Delete => {
                tracing::debug!(
                    rule = %rule.name,
                    "Local filter matched delete action; read-only backend left message untouched"
                );
            }
        }
    }
}

impl Default for MailStoreService {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_mail_files(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            // A symlinked directory can create a cycle.  Mail stores do not
            // need symlink traversal for the initial local backend.
            if !file_type.is_symlink() {
                collect_mail_files(&path, paths);
            }
        } else if file_type.is_file() && is_candidate_mail_file(&path) {
            paths.push(path);
        }
    }
}

fn is_candidate_mail_file(path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) == Some("eml") {
        return true;
    }

    // Maildir messages have no extension and live in cur/ or new/.  Files in
    // tmp/ are not delivered messages, so they are intentionally ignored.
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "cur" | "new"))
    {
        return true;
    }

    // Evolution's local provider stores mbox files without an extension.
    // Metadata/index files are filtered later by content inspection.
    path.extension().is_none()
}

fn looks_like_mbox(raw: &str) -> bool {
    let mut lines = raw.lines();
    let Some(first_line) = lines.next() else {
        return false;
    };

    first_line.starts_with("From ") && lines.any(|line| line.starts_with("From: "))
}

fn looks_like_rfc822(raw: &str, path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) == Some("eml") {
        return true;
    }

    // Maildir and mbox detection is intentionally conservative.  This keeps
    // Camel index/cache files out of the message list.
    raw.contains("\nFrom:")
        || raw.starts_with("From:")
        || raw.contains("\nSubject:")
        || raw.starts_with("Subject:")
}

fn maildir_message_is_unread(path: &Path) -> bool {
    let Some(parent) = path.parent().and_then(Path::file_name) else {
        return true;
    };

    if parent.to_str() != Some("cur") {
        return true;
    }

    // Maildir info is encoded after `:2,`; the `S` flag means Seen.
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name.split_once(":2,")
        .map(|(_, flags)| !flags.contains('S'))
        .unwrap_or(true)
}

fn parse_mbox(path: &Path, raw: &str, unread: bool) -> Vec<ParsedMail> {
    let mut starts = Vec::new();
    let mut offset = 0;

    for line in raw.split_inclusive('\n') {
        if line.starts_with("From ") {
            starts.push(offset);
        }
        offset += line.len();
    }

    if starts.is_empty() {
        return Vec::new();
    }

    starts
        .iter()
        .enumerate()
        .filter_map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(raw.len());
            let record = &raw[*start..end];
            let message_start = record.find('\n').map(|line_end| line_end + 1)?;
            let id = message_id(path, index);
            let folder_id = folder_id_for_path(path);
            parse_rfc822(&id, &folder_id, &record[message_start..], unread)
        })
        .collect()
}

fn message_id(path: &Path, index: usize) -> String {
    format!("local:{}#{index}", path.to_string_lossy())
}

fn folder_id_for_path(path: &Path) -> String {
    let candidate = match path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        Some("cur") | Some("new") => path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str()),
        Some(_) if path.extension().and_then(|extension| extension.to_str()) == Some("eml") => {
            Some("inbox")
        }
        _ => path.file_name().and_then(|name| name.to_str()),
    };

    let candidate = candidate.unwrap_or("inbox").trim_start_matches('.');
    match candidate.to_ascii_lowercase().as_str() {
        "inbox" => "inbox".to_string(),
        "sent" | "sent-mail" => "sent".to_string(),
        "draft" | "drafts" => "drafts".to_string(),
        "trash" | "deleted" => "trash".to_string(),
        "archive" | "all-mail" => "archive".to_string(),
        "junk" | "spam" => "junk".to_string(),
        _ => format!("custom:{candidate}"),
    }
}

pub(crate) fn parse_rfc822(
    id: &str,
    folder_id: &str,
    raw: &str,
    unread: bool,
) -> Option<ParsedMail> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let (header_text, body) = normalized
        .split_once("\n\n")
        .map(|(headers, body)| (headers, body))
        .unwrap_or((normalized.as_str(), ""));
    let headers = parse_headers(header_text);

    if headers.is_empty() && body.is_empty() {
        return None;
    }

    let (sender_name, sender_email) = headers
        .get("from")
        .map(|value| parse_address(value))
        .unwrap_or_else(|| ("Unbekannt".to_string(), String::new()));
    let recipient = headers
        .get("to")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let subject = headers
        .get("subject")
        .map(|value| decode_mime_header(value))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "(Kein Betreff)".to_string());
    let date = headers
        .get("date")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let message_id = headers
        .get("message-id")
        .and_then(|value| normalize_message_id(value));
    let in_reply_to = headers
        .get("in-reply-to")
        .map(|value| extract_message_ids(value))
        .unwrap_or_default();
    let references = headers
        .get("references")
        .map(|value| extract_message_ids(value))
        .unwrap_or_default();

    let mut html_body = String::new();
    let mut plain_body = String::new();
    let mut attachments = Vec::new();
    extract_mime_content(
        &headers,
        body,
        &mut html_body,
        &mut plain_body,
        &mut attachments,
    );

    let body_html = if !html_body.trim().is_empty() {
        html_body
    } else if !plain_body.trim().is_empty() {
        format!("<pre>{}</pre>", escape_html(plain_body.trim()))
    } else {
        "<p>(Keine Nachricht)</p>".to_string()
    };

    let preview = text_preview(if !plain_body.trim().is_empty() {
        plain_body.as_str()
    } else {
        body_html.as_str()
    });

    Some(ParsedMail {
        id: id.to_string(),
        folder_id: folder_id.to_string(),
        folder_name: folder_id.to_string(),
        backend_location: None,
        sender_name,
        sender_email,
        recipient_to: recipient,
        subject,
        message_id,
        in_reply_to,
        references,
        thread_id: String::new(),
        thread_size: 1,
        thread_position: 0,
        date_str: date,
        preview_snippet: preview,
        html_body: body_html,
        is_unread: unread,
        is_flagged: false,
        attachments,
    })
}

pub(crate) fn assign_thread_metadata(messages: &mut [ParsedMail]) {
    let inputs = messages
        .iter()
        .map(|message| ThreadIdentity {
            message_id: message.message_id.as_deref(),
            in_reply_to: &message.in_reply_to,
            references: &message.references,
            subject: &message.subject,
        })
        .collect::<Vec<_>>();
    let memberships = group_messages(&inputs);
    for (message, membership) in messages.iter_mut().zip(memberships) {
        message.thread_id = membership.thread_id;
        message.thread_size = membership.size;
        message.thread_position = membership.position;
    }
}

fn parse_headers(header_text: &str) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_value = String::new();

    for line in header_text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            if current_name.is_some() {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }

        let Some((name, value)) = line.split_once(':') else {
            continue;
        };

        flush_header(&mut headers, &mut current_name, &mut current_value);
        current_name = Some(name.trim().to_ascii_lowercase());
        current_value.push_str(value.trim());
    }

    flush_header(&mut headers, &mut current_name, &mut current_value);
    headers
}

fn flush_header(
    headers: &mut HashMap<String, String>,
    current_name: &mut Option<String>,
    current_value: &mut String,
) {
    if let Some(name) = current_name.take() {
        headers.insert(name, current_value.trim().to_string());
        current_value.clear();
    }
}

fn parse_address(value: &str) -> (String, String) {
    let decoded = decode_mime_header(value);
    if let Some(start) = decoded.rfind('<') {
        if let Some(end_offset) = decoded[start..].find('>') {
            let end = start + end_offset;
            let email = decoded[start + 1..end].trim().to_string();
            let name = decoded[..start].trim().trim_matches('"').trim().to_string();
            return (if name.is_empty() { email.clone() } else { name }, email);
        }
    }

    let email = decoded.trim().trim_matches('"').to_string();
    (email.clone(), email)
}

fn extract_mime_content(
    headers: &HashMap<String, String>,
    body: &str,
    html_body: &mut String,
    plain_body: &mut String,
    attachments: &mut Vec<AttachmentInfo>,
) {
    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("text/plain");
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();

    if media_type.starts_with("multipart/") {
        if let Some(boundary) = mime_parameter(content_type, "boundary") {
            let marker = format!("--{boundary}");
            for part in body.split(&marker).skip(1) {
                if part.trim_start().starts_with("--") {
                    break;
                }
                let part =
                    part.trim_start_matches(|character| character == '\n' || character == '\r');
                let part =
                    part.trim_end_matches(|character| character == '\n' || character == '\r');
                let normalized = part.replace("\r\n", "\n").replace('\r', "\n");
                let (part_headers, part_body) = normalized
                    .split_once("\n\n")
                    .map(|(headers, body)| (parse_headers(headers), body))
                    .unwrap_or_else(|| (HashMap::new(), ""));
                extract_mime_content(&part_headers, part_body, html_body, plain_body, attachments);
            }
        }
        return;
    }

    let decoded = decode_transfer_encoding(
        body,
        headers
            .get("content-transfer-encoding")
            .map(String::as_str)
            .unwrap_or(""),
    );
    let disposition = headers
        .get("content-disposition")
        .map(String::as_str)
        .unwrap_or("");
    let filename =
        mime_parameter(disposition, "filename").or_else(|| mime_parameter(content_type, "name"));

    if let Some(filename) = filename.filter(|_| {
        disposition.to_ascii_lowercase().contains("attachment") || !media_type.starts_with("text/")
    }) {
        attachments.push(AttachmentInfo {
            filename: decode_mime_header(&filename),
            size_formatted: format_size(decoded.len()),
            mime_type: media_type,
            content: decoded,
        });
        return;
    }

    match media_type.as_str() {
        "text/html" => {
            if html_body.trim().is_empty() {
                *html_body = String::from_utf8_lossy(&decoded).into_owned();
            }
        }
        "text/plain" | "" => {
            if plain_body.trim().is_empty() {
                *plain_body = String::from_utf8_lossy(&decoded).into_owned();
            }
        }
        _ => {
            // A non-text part without a filename is still useful to expose as
            // an attachment, but use a stable fallback name for the UI.
            attachments.push(AttachmentInfo {
                filename: "attachment".to_string(),
                size_formatted: format_size(decoded.len()),
                mime_type: media_type,
                content: decoded,
            });
        }
    }
}

fn mime_parameter(value: &str, parameter: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        if name.trim().eq_ignore_ascii_case(parameter) {
            Some(value.trim().trim_matches('"').to_string())
        } else {
            None
        }
    })
}

fn decode_transfer_encoding(body: &str, encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => decode_base64(body).unwrap_or_else(|| body.as_bytes().to_vec()),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.as_bytes().to_vec(),
    }
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    let mut saw_data = false;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        saw_data = true;
        buffer = (buffer << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }

    saw_data.then_some(output)
}

fn decode_quoted_printable(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'=' {
            if index + 2 < bytes.len() && bytes[index + 1] == b'\n' {
                index += 2;
                continue;
            }
            if index + 3 < bytes.len() && bytes[index + 1] == b'\r' && bytes[index + 2] == b'\n' {
                index += 3;
                continue;
            }
            if index + 2 < bytes.len() {
                if let (Some(high), Some(low)) =
                    (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
                {
                    output.push(high * 16 + low);
                    index += 3;
                    continue;
                }
            }
        }
        output.push(bytes[index]);
        index += 1;
    }

    output
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn decode_mime_header(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;

    while let Some(start) = rest.find("=?") {
        result.push_str(&rest[..start]);
        let encoded = &rest[start..];
        let Some(end) = encoded.find("?=") else {
            result.push_str(encoded);
            rest = "";
            break;
        };
        let word = &encoded[2..end];
        let mut fields = word.splitn(3, '?');
        let _charset = fields.next();
        let encoding = fields.next();
        let payload = fields.next();

        let decoded = match (encoding, payload) {
            (Some(encoding), Some(payload)) if encoding.eq_ignore_ascii_case("b") => {
                decode_base64(payload).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            }
            (Some(encoding), Some(payload)) if encoding.eq_ignore_ascii_case("q") => Some(
                String::from_utf8_lossy(&decode_quoted_printable(&payload.replace('_', " ")))
                    .into_owned(),
            ),
            _ => None,
        };

        if let Some(decoded) = decoded {
            result.push_str(&decoded);
        } else {
            result.push_str(&encoded[..end + 2]);
        }
        rest = &encoded[end + 2..];
    }

    result.push_str(rest);
    result.trim().to_string()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text_preview(value: &str) -> String {
    let mut text = String::new();
    let mut inside_tag = false;

    for character in value.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if inside_tag => {}
            character if character.is_whitespace() => {
                if !text.ends_with(' ') {
                    text.push(' ');
                }
            }
            character => text.push(character),
        }
    }

    let text = text.trim();
    let preview: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        format!("{preview}…")
    } else {
        preview
    }
}

fn format_size(size: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = KIB * 1024;

    if size >= MIB {
        format!("{:.1} MiB", size as f64 / MIB as f64)
    } else if size >= KIB {
        format!("{:.1} KiB", size as f64 / KIB as f64)
    } else {
        format!("{size} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mail_filter_service::{
        FilterAction, FilterCondition, FilterField, FilterScope,
    };

    #[test]
    fn parses_folded_headers_and_plain_text() {
        let raw = concat!(
            "From: =?UTF-8?Q?M=C3=BCller?= <mueller@example.org>\r\n",
            "Subject: =?UTF-8?B?U3RhdHVz?=\r\n",
            "  Update\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Hallo Welt\n"
        );

        let message = parse_rfc822("folded", "inbox", raw, true).expect("message should parse");
        assert_eq!(message.sender_name, "Müller");
        assert_eq!(message.sender_email, "mueller@example.org");
        assert_eq!(message.subject, "Status Update");
        assert_eq!(message.folder_id, "inbox");
        assert!(message.html_body.contains("Hallo Welt"));
        assert!(message.is_unread);
    }

    #[test]
    fn parses_message_thread_headers_and_assigns_transitive_membership() {
        let root = parse_rfc822(
            "root",
            "inbox",
            concat!(
                "Message-ID: <root@example.org>\n",
                "Subject: Project\n\n",
                "Root"
            ),
            false,
        )
        .expect("root message should parse");
        let reply = parse_rfc822(
            "reply",
            "inbox",
            concat!(
                "Message-ID: <reply@example.org>\n",
                "References: <root@example.org>\n",
                "Subject: Re: Project\n\n",
                "Reply"
            ),
            false,
        )
        .expect("reply message should parse");
        let nested_reply = parse_rfc822(
            "nested",
            "inbox",
            concat!(
                "Message-ID: <nested@example.org>\n",
                "In-Reply-To: <reply@example.org>\n",
                "Subject: Re: Project\n\n",
                "Nested reply"
            ),
            false,
        )
        .expect("nested reply should parse");

        let mut messages = vec![root, reply, nested_reply];
        assign_thread_metadata(&mut messages);

        assert_eq!(messages[0].message_id.as_deref(), Some("root@example.org"));
        assert_eq!(messages[0].thread_id, "root@example.org");
        assert_eq!(messages[1].thread_id, "root@example.org");
        assert_eq!(messages[2].thread_id, "root@example.org");
        assert_eq!(messages[0].thread_size, 3);
        assert_eq!(messages[2].thread_position, 2);
    }

    #[test]
    fn parses_multipart_html_and_attachment() {
        let raw = concat!(
            "From: Sender <sender@example.org>\n",
            "To: Receiver <receiver@example.org>\n",
            "Subject: Report\n",
            "Content-Type: multipart/mixed; boundary=boundary42\n",
            "\n",
            "--boundary42\n",
            "Content-Type: text/html; charset=utf-8\n",
            "\n",
            "<p>Report</p>\n",
            "--boundary42\n",
            "Content-Type: application/pdf\n",
            "Content-Disposition: attachment; filename=report.pdf\n",
            "Content-Transfer-Encoding: base64\n",
            "\n",
            "SGk=\n",
            "--boundary42--\n"
        );

        let message = parse_rfc822("multipart", "sent", raw, false).expect("message should parse");
        assert!(message.html_body.contains("<p>Report</p>"));
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].filename, "report.pdf");
        assert_eq!(message.attachments[0].content, b"Hi");
        assert_eq!(message.folder_id, "sent");
    }

    #[test]
    fn splits_mbox_into_individual_messages() {
        let raw = concat!(
            "From sender@example.org Mon Aug 10 12:00:00 2026\n",
            "From: Sender <sender@example.org>\n",
            "Subject: First\n\nOne\n",
            "From sender@example.org Mon Aug 10 12:01:00 2026\n",
            "From: Sender <sender@example.org>\n",
            "Subject: Second\n\nTwo\n"
        );

        let messages = parse_mbox(Path::new("Inbox"), raw, true);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].subject, "First");
        assert_eq!(messages[1].subject, "Second");
    }

    #[test]
    fn missing_mailbox_does_not_create_demo_data() {
        let service = MailStoreService::with_mail_dir(
            std::env::temp_dir().join("evolution-gtk4-nonexistent-mailbox"),
        );
        assert!(service.load_local_messages().is_empty());
    }

    #[test]
    fn incoming_filter_applies_only_supported_local_actions() {
        let mut message = parse_rfc822(
            "filter",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody",
            true,
        )
        .expect("message should parse");
        let rule = MailFilterRule {
            id: "rule-1".to_string(),
            name: "Mark invoices".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
            },
            action: FilterAction::MarkRead,
        };

        apply_local_filters(&[rule], &mut message);

        assert!(!message.is_unread);
    }

    #[test]
    fn cancelled_load_stops_before_reading_mail_files() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-cancelled-load-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("mail directory should be created");
        fs::write(
            root.join("message.eml"),
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");

        let cancelled = AtomicBool::new(true);
        let records =
            MailStoreService::with_mail_dir(root.clone()).load_local_records_cancelable(&cancelled);

        assert!(records.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
