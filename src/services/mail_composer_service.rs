// SPDX-License-Identifier: GPL-3.0-or-later
//
// Cross-platform RFC822/MIME construction for the composer.  Account
// credentials and transport selection remain in EDS/Camel; this service only
// turns the GTK composer state into a complete message that Camel can send.

use super::draft_store_service::DraftAttachment;

use chrono::Utc;
use std::fs;

#[derive(Debug, Clone)]
pub(crate) struct OutgoingMessage {
    pub(crate) source_uid: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) html_body: String,
    pub(crate) attachments: Vec<DraftAttachment>,
}

pub(crate) fn build_rfc822(message: &OutgoingMessage) -> Result<Vec<u8>, String> {
    if message.source_uid.trim().is_empty() {
        return Err("Kein EDS-Mailkonto ausgewählt".to_string());
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

    let mut raw = String::new();
    raw.push_str("Date: ");
    raw.push_str(&Utc::now().to_rfc2822());
    raw.push_str("\r\nMessage-ID: <");
    raw.push_str(&uuid::Uuid::new_v4().simple().to_string());
    raw.push_str("@evolution-gtk4.local>\r\n");

    append_recipient_header(&mut raw, "To", &message.to);
    append_recipient_header(&mut raw, "Cc", &message.cc);
    append_recipient_header(&mut raw, "Bcc", &message.bcc);
    raw.push_str("Subject: ");
    raw.push_str(&encode_header_value(&message.subject));
    raw.push_str("\r\nMIME-Version: 1.0\r\n");

    let body = normalize_body(&message.html_body);
    if message.attachments.is_empty() {
        raw.push_str("Content-Type: text/html; charset=UTF-8\r\n");
        raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        raw.push_str(&body);
        ensure_crlf(&mut raw);
    } else {
        let boundary = format!("=_EvolutionGTK4_{}", uuid::Uuid::new_v4().simple());
        raw.push_str("Content-Type: multipart/mixed; boundary=\"");
        raw.push_str(&boundary);
        raw.push_str("\"\r\n\r\n");

        raw.push_str("--");
        raw.push_str(&boundary);
        raw.push_str("\r\nContent-Type: text/html; charset=UTF-8\r\n");
        raw.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        raw.push_str(&body);
        ensure_crlf(&mut raw);

        for attachment in &message.attachments {
            let bytes = fs::read(&attachment.path).map_err(|error| {
                format!(
                    "Anhang konnte nicht gelesen werden ({}): {error}",
                    attachment.filename
                )
            })?;
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
            raw.push_str(&base64_with_crlf(&bytes));
        }

        raw.push_str("--");
        raw.push_str(&boundary);
        raw.push_str("--\r\n");
    }

    Ok(raw.into_bytes())
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
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Grüße".to_string(),
            html_body: "<p>Hello</p>".to_string(),
            attachments: Vec::new(),
        })
        .expect("message should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
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
            to: "one@example.org; two@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Report".to_string(),
            html_body: "<p>Body</p>\n".to_string(),
            attachments: vec![DraftAttachment {
                path: path.to_string_lossy().into_owned(),
                filename: "hello.txt".to_string(),
                mime_type: "text/plain".to_string(),
                size: 5,
            }],
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
    fn rejects_header_injection_and_missing_recipients() {
        let mut message = OutgoingMessage {
            source_uid: "account-1".to_string(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Subject".to_string(),
            html_body: String::new(),
            attachments: Vec::new(),
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
