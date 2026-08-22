// SPDX-License-Identifier: GPL-3.0-or-later
//
// Backend-neutral mail print snapshot and document contract.
//
// The GTK layer captures already parsed message fields into a snapshot.  All
// sanitizing, CID resolution and HTML composition then happen in the caller's
// worker, so MIME work and untrusted HTML never run on the GTK main context.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::html_sanitizer::{
    block_remote_images, sanitize_html, sanitized_html_to_plain_text,
};
use crate::services::inline_content_service::{InlineImage, resolve_cid_images};

const PRINT_CSP: &str = "default-src 'none'; base-uri 'none'; form-action 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; media-src 'none'; frame-src 'none'; object-src 'none'; worker-src 'none'";

/// The two user-visible mail print actions from Evolution's reader action
/// table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailPrintAction {
    Print,
    Preview,
}

impl MailPrintAction {
    pub fn is_preview(self) -> bool {
        matches!(self, Self::Preview)
    }
}

/// State shared by the portable action contract and the GTK presentation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MailPrintJobState {
    #[default]
    Idle,
    Preparing,
    Printing,
    Previewing,
    Cancelled,
    Error,
}

impl MailPrintJobState {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Preparing | Self::Printing | Self::Previewing)
    }

    /// Preparation can be cancelled by the application.  Once WebKit owns a
    /// native print dialog, GTK has no portable handle for cancelling that
    /// dialog; its own Cancel button is the authoritative cancellation path.
    pub fn is_cancellable(self) -> bool {
        matches!(self, Self::Preparing)
    }

    pub fn action_enabled(self, has_snapshot: bool) -> bool {
        has_snapshot && !self.is_running()
    }
}

/// Small, owned attachment metadata used in the printable document.  The
/// actual inline image bytes are kept separately in `inline_images` so an
/// attachment list never accidentally causes non-inline payloads to enter the
/// generated HTML.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailPrintAttachment {
    pub filename: String,
    pub mime_type: String,
    pub size_formatted: String,
}

/// A complete selection snapshot.  It contains only data already available in
/// the visible mail record; it does not contain GTK/GObject values or a MIME
/// parser handle and is therefore safe to move to a worker thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailPrintSnapshot {
    pub message_key: String,
    pub sender: String,
    pub reply_to: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub date: String,
    pub html_body: String,
    pub fallback_text: String,
    pub inline_images: Vec<InlineImage>,
    pub attachments: Vec<MailPrintAttachment>,
}

/// Deterministic, fully self-contained print document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailPrintDocument {
    pub message_key: String,
    pub suggested_filename: String,
    pub html: String,
    pub plain_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MailPrintDocumentError {
    Cancelled,
    EmptyMessageKey,
    WorkerDisconnected,
}

impl fmt::Display for MailPrintDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Druckvorbereitung abgebrochen"),
            Self::EmptyMessageKey => formatter.write_str("Nachricht hat keine stabile Identität"),
            Self::WorkerDisconnected => {
                formatter.write_str("Druckvorbereitung konnte nicht abgeschlossen werden")
            }
        }
    }
}

impl std::error::Error for MailPrintDocumentError {}

/// Prepare one immutable print document.  The cancellation flag is checked at
/// every expensive boundary; no network, remote resource or script is ever
/// consulted while composing the document.
pub fn prepare_print_document(
    snapshot: &MailPrintSnapshot,
    cancellation: &AtomicBool,
) -> Result<MailPrintDocument, MailPrintDocumentError> {
    ensure_not_cancelled(cancellation)?;
    if snapshot.message_key.trim().is_empty() {
        return Err(MailPrintDocumentError::EmptyMessageKey);
    }

    let resolved_html = resolve_cid_images(&snapshot.html_body, &snapshot.inline_images);
    ensure_not_cancelled(cancellation)?;

    // The print path is stricter than the reader: remote images are removed
    // rather than merely hidden by a runtime preference.  The CSP below is a
    // second, backend-independent guard for every other resource type.
    let sanitized_body = strip_document_wrappers(&strip_remote_image_variants(
        &block_remote_images(&sanitize_html(&resolved_html)),
    ));
    ensure_not_cancelled(cancellation)?;

    let body_text = sanitized_html_to_plain_text(&sanitized_body);
    let body_fragment = if sanitized_body.trim().is_empty() {
        let fallback = if snapshot.fallback_text.trim().is_empty() {
            "Keine lesbare Textdarstellung verfügbar."
        } else {
            snapshot.fallback_text.trim()
        };
        format!(
            "<pre class=\"mail-print-fallback\">{}</pre>",
            escape_html(fallback)
        )
    } else {
        sanitized_body.clone()
    };

    let title = if snapshot.subject.trim().is_empty() {
        "(Ohne Betreff)"
    } else {
        snapshot.subject.trim()
    };
    let mut html = String::with_capacity(sanitized_body.len().saturating_add(2_048));
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    html.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"");
    html.push_str(PRINT_CSP);
    html.push_str("\"><title>");
    html.push_str(&escape_html(title));
    html.push_str("</title><style>");
    html.push_str(
        "@page{size:auto;margin:18mm 16mm}body{color:#202124;background:#fff;font:11pt system-ui,sans-serif;line-height:1.35;margin:0}h1{font-size:17pt;line-height:1.2;margin:0 0 10pt}dl{display:grid;grid-template-columns:max-content 1fr;column-gap:12pt;row-gap:3pt;margin:0 0 14pt}dt{font-weight:600;color:#555}dd{margin:0;overflow-wrap:anywhere}.mail-print-body{border-top:1px solid #bbb;padding-top:12pt}.mail-print-body img{max-width:100%;height:auto}.mail-print-body table{border-collapse:collapse;max-width:100%}.mail-print-attachments{border-top:1px solid #bbb;margin-top:16pt;padding-top:10pt}.mail-print-attachments h2{font-size:11pt;margin:0 0 4pt}.mail-print-attachments ul{margin:0;padding-left:18pt}.mail-print-fallback{white-space:pre-wrap;font:inherit}",
    );
    html.push_str("</style></head><body><header><h1>");
    html.push_str(&escape_html(title));
    html.push_str("</h1><dl>");
    push_html_field(&mut html, "Von", &snapshot.sender);
    push_html_field(&mut html, "Reply-To", &snapshot.reply_to);
    push_html_field(&mut html, "An", &snapshot.to);
    push_html_field(&mut html, "Cc", &snapshot.cc);
    push_html_field(&mut html, "Bcc", &snapshot.bcc);
    push_html_field(&mut html, "Datum", &snapshot.date);
    html.push_str("</dl></header><main class=\"mail-print-body\">");
    html.push_str(&body_fragment);
    html.push_str("</main>");
    if !snapshot.attachments.is_empty() {
        html.push_str("<section class=\"mail-print-attachments\"><h2>Anhänge</h2><ul>");
        for attachment in &snapshot.attachments {
            ensure_not_cancelled(cancellation)?;
            html.push_str("<li>");
            html.push_str(&escape_html(&attachment.filename));
            if !attachment.size_formatted.trim().is_empty() {
                html.push_str(" (");
                html.push_str(&escape_html(&attachment.size_formatted));
                html.push(')');
            }
            html.push_str("</li>");
        }
        html.push_str("</ul></section>");
    }
    html.push_str("</body></html>");

    let mut plain_text = String::with_capacity(body_text.len().saturating_add(512));
    plain_text.push_str("Betreff: ");
    plain_text.push_str(title);
    plain_text.push('\n');
    push_plain_field(&mut plain_text, "Von", &snapshot.sender);
    push_plain_field(&mut plain_text, "Reply-To", &snapshot.reply_to);
    push_plain_field(&mut plain_text, "An", &snapshot.to);
    push_plain_field(&mut plain_text, "Cc", &snapshot.cc);
    push_plain_field(&mut plain_text, "Bcc", &snapshot.bcc);
    push_plain_field(&mut plain_text, "Datum", &snapshot.date);
    plain_text.push('\n');
    if body_text.trim().is_empty() {
        plain_text.push_str(if snapshot.fallback_text.trim().is_empty() {
            "Keine lesbare Textdarstellung verfügbar."
        } else {
            snapshot.fallback_text.trim()
        });
    } else {
        plain_text.push_str(body_text.trim());
    }
    if !snapshot.attachments.is_empty() {
        plain_text.push_str("\n\nAnhänge:\n");
        for attachment in &snapshot.attachments {
            ensure_not_cancelled(cancellation)?;
            plain_text.push_str("- ");
            plain_text.push_str(attachment.filename.trim());
            if !attachment.size_formatted.trim().is_empty() {
                plain_text.push_str(" (");
                plain_text.push_str(attachment.size_formatted.trim());
                plain_text.push(')');
            }
            plain_text.push('\n');
        }
        plain_text.pop();
    }

    ensure_not_cancelled(cancellation)?;
    Ok(MailPrintDocument {
        message_key: snapshot.message_key.clone(),
        suggested_filename: suggested_filename(title),
        html,
        plain_text,
    })
}

fn ensure_not_cancelled(cancellation: &AtomicBool) -> Result<(), MailPrintDocumentError> {
    if cancellation.load(Ordering::Acquire) {
        Err(MailPrintDocumentError::Cancelled)
    } else {
        Ok(())
    }
}

/// A parsed mail body can legally contain a complete HTML document.  The
/// print shell owns the outer document, so retaining the mail's html/head/body
/// wrapper would create invalid nested documents and can make WebKit discard
/// the message body.  Sanitized style blocks are kept in place.
fn strip_document_wrappers(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while cursor < input.len() {
        let Some(relative_start) = input[cursor..].find('<') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&input[cursor..start]);

        let Some(end) = find_tag_end(input, start) else {
            output.push_str(&input[start..]);
            break;
        };
        let raw_tag = &input[start..=end];
        if !is_document_wrapper_tag(raw_tag) {
            output.push_str(raw_tag);
        }
        cursor = end + 1;
    }

    output
}

/// `block_remote_images` handles the normal `src` attribute.  Keep the same
/// privacy guarantee for legacy HTML that uses SVG-style `xlink:href`; the
/// print CSP remains a second independent deny-by-default guard.
fn strip_remote_image_variants(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while cursor < input.len() {
        let Some(relative_start) = input[cursor..].find('<') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&input[cursor..start]);

        let Some(end) = find_tag_end(input, start) else {
            output.push_str(&input[start..]);
            break;
        };
        let raw_tag = &input[start..=end];
        let remote_xlink = html_tag_name(raw_tag).is_some_and(|(closing, name)| {
            !closing
                && name == "img"
                && tag_attribute_value(raw_tag, "xlink:href")
                    .is_some_and(|value| is_remote_uri(&value))
        });
        if !remote_xlink {
            output.push_str(raw_tag);
        }
        cursor = end + 1;
    }

    output
}

fn find_tag_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote = None;
    for index in start + 1..bytes.len() {
        let character = bytes[index] as char;
        match quote {
            Some(expected) if character == expected => quote = None,
            Some(_) => {}
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character == '>' => return Some(index),
            None => {}
        }
    }
    None
}

fn html_tag_name(raw_tag: &str) -> Option<(bool, String)> {
    let mut content = raw_tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    let closing = content.starts_with('/');
    if closing {
        content = content[1..].trim_start();
    }
    let name_end = content
        .char_indices()
        .find(|(_, character)| !character.is_ascii_alphanumeric())
        .map(|(index, _)| index)
        .unwrap_or(content.len());
    (name_end > 0).then(|| (closing, content[..name_end].to_ascii_lowercase()))
}

fn is_document_wrapper_tag(raw_tag: &str) -> bool {
    html_tag_name(raw_tag)
        .is_some_and(|(_, name)| matches!(name.as_str(), "html" | "head" | "body"))
}

fn tag_attribute_value(raw_tag: &str, wanted: &str) -> Option<String> {
    // `sanitize_html` canonicalizes retained attributes to a quoted,
    // lowercase form, so this lookup deliberately runs after sanitizing.
    let needle = format!(" {wanted}=\"");
    let lower_tag = raw_tag.to_ascii_lowercase();
    let start = lower_tag.find(&needle)? + needle.len();
    let end = raw_tag[start..].find('"')? + start;
    Some(raw_tag[start..end].to_string())
}

fn is_remote_uri(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && !character.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    let decoded = percent_decode_uri(&normalized);
    [normalized.as_str(), decoded.as_str()].iter().any(|uri| {
        uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("//")
    })
}

fn percent_decode_uri(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%'
            && cursor + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[cursor + 1]), hex_value(bytes[cursor + 2]))
        {
            output.push((high << 4 | low) as char);
            cursor += 3;
            continue;
        }
        output.push(bytes[cursor] as char);
        cursor += 1;
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

fn push_html_field(output: &mut String, label: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    output.push_str("<dt>");
    output.push_str(label);
    output.push_str("</dt><dd>");
    output.push_str(&escape_html(value.trim()));
    output.push_str("</dd>");
}

fn push_plain_field(output: &mut String, label: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    output.push_str(label);
    output.push_str(": ");
    output.push_str(value.trim());
    output.push('\n');
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}

fn suggested_filename(subject: &str) -> String {
    let stem = subject
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect::<String>();
    let stem = stem.trim().trim_matches('.');
    let stem = if stem.is_empty() { "nachricht" } else { stem };
    format!("{}.pdf", stem.chars().take(80).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> MailPrintSnapshot {
        MailPrintSnapshot {
            message_key: "local:message-1".to_string(),
            sender: "Ada <ada@example.test>".to_string(),
            reply_to: "".to_string(),
            to: "Grace <grace@example.test>".to_string(),
            cc: "".to_string(),
            bcc: "".to_string(),
            subject: "Quarterly report".to_string(),
            date: "2026-08-22 10:00".to_string(),
            html_body: "<p>Hello <strong>team</strong>.</p><script>bad()</script><img src=\"https://remote.example/image.png\"><img src=\"cid:logo@example.test\">".to_string(),
            fallback_text: "Hello team.".to_string(),
            inline_images: vec![InlineImage::new(
                "logo@example.test",
                "image/png",
                b"PNG".to_vec(),
            )],
            attachments: vec![MailPrintAttachment {
                filename: "report.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                size_formatted: "12 KB".to_string(),
            }],
        }
    }

    #[test]
    fn actions_need_a_snapshot_and_are_disabled_while_running() {
        assert!(!MailPrintJobState::Idle.action_enabled(false));
        assert!(MailPrintJobState::Idle.action_enabled(true));
        assert!(!MailPrintJobState::Preparing.action_enabled(true));
        assert!(!MailPrintJobState::Printing.action_enabled(true));
        assert!(!MailPrintJobState::Previewing.action_enabled(true));
        assert!(MailPrintJobState::Cancelled.action_enabled(true));
        assert!(MailPrintJobState::Error.action_enabled(true));
        assert!(MailPrintJobState::Preparing.is_cancellable());
        assert!(!MailPrintJobState::Printing.is_cancellable());
        assert!(!MailPrintJobState::Previewing.is_cancellable());
    }

    #[test]
    fn document_is_deterministic_and_has_no_active_or_remote_content() {
        let cancellation = AtomicBool::new(false);
        let first = prepare_print_document(&snapshot(), &cancellation).expect("document");
        let second = prepare_print_document(&snapshot(), &cancellation).expect("document");

        assert_eq!(first, second);
        assert!(first.html.contains("Quarterly report"));
        assert!(first.html.contains("data:image/png;base64,UE5H"));
        assert!(!first.html.contains("<script"));
        assert!(!first.html.contains("remote.example"));
        assert!(first.html.contains("script-src 'none'"));
        assert!(first.html.contains("img-src data:"));
        assert!(first.plain_text.contains("Betreff: Quarterly report"));
        assert!(first.plain_text.contains("- report.pdf (12 KB)"));
        assert_eq!(first.suggested_filename, "Quarterly report.pdf");
    }

    #[test]
    fn complete_html_snapshots_are_flattened_into_the_print_shell() {
        let mut snapshot = snapshot();
        snapshot.html_body =
            "<html><head><style>p { color: red }</style></head><body><p>Full body</p></body></html>"
                .to_string();
        let document = prepare_print_document(&snapshot, &AtomicBool::new(false)).unwrap();

        assert_eq!(document.html.matches("<html").count(), 1);
        assert!(!document.html.contains("<body><html"));
        assert!(document.html.contains("<style>p { color: red; }</style>"));
        assert!(document.html.contains("Full body"));
        assert!(document.plain_text.contains("Full body"));
    }

    #[test]
    fn remote_xlink_images_are_removed_before_the_csp_is_applied() {
        let mut snapshot = snapshot();
        snapshot.html_body =
            r#"<img xlink:href="https://remote.example/image.png"><p>Text</p>"#.to_string();
        let document = prepare_print_document(&snapshot, &AtomicBool::new(false)).unwrap();

        assert!(!document.html.contains("remote.example"));
        assert!(document.html.contains("Text"));
    }

    #[test]
    fn header_order_and_escaping_are_stable() {
        let mut snapshot = snapshot();
        snapshot.subject = "<subject>".to_string();
        snapshot.sender = "Sender & Co.".to_string();
        snapshot.reply_to = "reply@example.test".to_string();
        snapshot.cc = "cc@example.test".to_string();
        let document = prepare_print_document(&snapshot, &AtomicBool::new(false)).unwrap();

        let subject = document.html.find("&lt;subject&gt;").unwrap();
        let sender = document.html.find("Sender &amp; Co.").unwrap();
        let reply_to = document.html.find("reply@example.test").unwrap();
        let recipient = document
            .html
            .find("Grace &lt;grace@example.test&gt;")
            .unwrap();
        let cc = document.html.find("cc@example.test").unwrap();
        let date = document.html.find("2026-08-22 10:00").unwrap();
        assert!(
            subject < sender
                && sender < reply_to
                && reply_to < recipient
                && recipient < cc
                && cc < date
        );
        assert!(document.plain_text.contains("Reply-To: reply@example.test"));
    }

    #[test]
    fn cancellation_is_reported_before_work_is_started() {
        let cancellation = AtomicBool::new(true);
        assert_eq!(
            prepare_print_document(&snapshot(), &cancellation),
            Err(MailPrintDocumentError::Cancelled)
        );
    }

    #[test]
    fn empty_message_identity_is_rejected() {
        let mut snapshot = snapshot();
        snapshot.message_key.clear();
        assert_eq!(
            prepare_print_document(&snapshot, &AtomicBool::new(false)),
            Err(MailPrintDocumentError::EmptyMessageKey)
        );
    }
}
