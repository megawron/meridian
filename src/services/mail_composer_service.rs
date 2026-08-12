// SPDX-License-Identifier: GPL-3.0-or-later
//
// Cross-platform RFC822/MIME construction for the composer.  Account
// credentials and transport selection remain in EDS/Camel; this service only
// turns the GTK composer state into a complete message that Camel can send.

use super::crypto_service::SmimeOptions;
use super::draft_store_service::DraftAttachment;

use chrono::Utc;
use std::fs;
use std::path::Path;

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

    let attachments = load_attachments(&message.attachments)?;

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
        if !message_id.trim().is_empty() {
            validate_message_id("Message-ID", message_id)?;
        }
    }
    for value in message.in_reply_to.iter().chain(message.references.iter()) {
        validate_header_value("Thread-Header", value)?;
        if !value.trim().is_empty() {
            validate_message_id("Thread-Header", value)?;
        }
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
    validate_header_value("Datum", date)?;
    validate_header_value("Message-ID", message_id.unwrap_or_default())?;
    validate_header_values("In-Reply-To", in_reply_to)?;
    validate_header_values("References", references)?;
    validate_header_value("Von", from)?;
    validate_header_value("Reply-To", reply_to)?;
    validate_header_value("Empfänger", to)?;
    validate_header_value("Cc", cc)?;
    validate_header_value("Bcc", bcc)?;
    validate_header_value("Betreff", subject)?;
    if let Some(method) = calendar_method {
        validate_calendar_method(method)?;
    }
    if calendar_component.is_some() != calendar_method.is_some() {
        return Err(
            "iTIP-Methode und iCalendar-Komponente müssen gemeinsam gesetzt werden".to_string(),
        );
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
        .unwrap_or_else(|| format!("<{}@evolution-gtk4.local>", uuid::Uuid::new_v4().simple()));

    let mut raw = String::new();
    raw.push_str("Date: ");
    raw.push_str(&date_value);
    raw.push_str("\r\nMessage-ID: ");
    raw.push_str(&message_id_value);
    raw.push_str("\r\n");
    if !from.trim().is_empty() {
        raw.push_str("From: ");
        raw.push_str(&encode_address_header(from));
        raw.push_str("\r\n");
    }
    if !reply_to.trim().is_empty() {
        raw.push_str("Reply-To: ");
        raw.push_str(&encode_address_header(reply_to));
        raw.push_str("\r\n");
    }
    if request_read_receipt && !from.trim().is_empty() {
        raw.push_str("Disposition-Notification-To: ");
        raw.push_str(&encode_address_header(from));
        raw.push_str("\r\n");
    }
    append_recipient_header(&mut raw, "To", to);
    append_recipient_header(&mut raw, "Cc", cc);
    append_recipient_header(&mut raw, "Bcc", bcc);
    let in_reply_to = format_message_id_list("In-Reply-To", in_reply_to)?;
    if !in_reply_to.is_empty() {
        raw.push_str("In-Reply-To: ");
        raw.push_str(&in_reply_to);
        raw.push_str("\r\n");
    }
    let references = format_message_id_list("References", references)?;
    if !references.is_empty() {
        raw.push_str("References: ");
        raw.push_str(&references);
        raw.push_str("\r\n");
    }
    raw.push_str("Subject: ");
    raw.push_str(&encode_header_value(subject));
    raw.push_str("\r\nMIME-Version: 1.0\r\n");

    for attachment in attachments {
        validate_export_attachment(attachment)?;
    }

    let plain_body = if plain_text {
        normalize_body(html_body)
    } else {
        normalize_body(&html_to_plain_text(html_body))
    };
    let html_body = (!plain_text).then(|| normalize_body(html_body));
    let has_alternative = html_body.is_some() || calendar_component.is_some();

    if attachments.is_empty() {
        if has_alternative {
            let boundary = new_boundary();
            append_multipart_header(&mut raw, "alternative", &boundary);
            append_text_part(
                &mut raw,
                &boundary,
                "text/plain; charset=UTF-8",
                &plain_body,
            );
            if let Some(html_body) = html_body.as_deref() {
                append_text_part(&mut raw, &boundary, "text/html; charset=UTF-8", html_body);
            }
            if let (Some(method), Some(component)) = (calendar_method, calendar_component) {
                append_calendar_part(&mut raw, &boundary, method, component)?;
            }
            close_multipart(&mut raw, &boundary);
        } else {
            append_leaf_part(&mut raw, "text/plain; charset=UTF-8", &plain_body);
        }
    } else {
        let mixed_boundary = new_boundary();
        append_multipart_header(&mut raw, "mixed", &mixed_boundary);
        if has_alternative {
            let alternative_boundary = new_boundary();
            raw.push_str("--");
            raw.push_str(&mixed_boundary);
            raw.push_str("\r\nContent-Type: multipart/alternative; boundary=\"");
            raw.push_str(&alternative_boundary);
            raw.push_str("\"\r\n\r\n");
            append_text_part(
                &mut raw,
                &alternative_boundary,
                "text/plain; charset=UTF-8",
                &plain_body,
            );
            if let Some(html_body) = html_body.as_deref() {
                append_text_part(
                    &mut raw,
                    &alternative_boundary,
                    "text/html; charset=UTF-8",
                    html_body,
                );
            }
            if let (Some(method), Some(component)) = (calendar_method, calendar_component) {
                append_calendar_part(&mut raw, &alternative_boundary, method, component)?;
            }
            close_multipart(&mut raw, &alternative_boundary);
        } else {
            append_text_part(
                &mut raw,
                &mixed_boundary,
                "text/plain; charset=UTF-8",
                &plain_body,
            );
        }
        for attachment in attachments {
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

            if attachment.path.trim().is_empty() {
                return Err(format!(
                    "Anhang konnte nicht gelesen werden ({}): Pfad ist leer",
                    attachment.filename
                ));
            }
            let path = Path::new(&attachment.path);
            let metadata = fs::metadata(path).map_err(|error| {
                format!(
                    "Anhang konnte nicht gelesen werden ({}): {error}",
                    attachment.filename
                )
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "Anhang ist keine reguläre Datei: {}",
                    attachment.filename
                ));
            }
            let content = fs::read(path).map_err(|error| {
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
        .collect()
}

fn validate_export_attachment(attachment: &ExportAttachment) -> Result<(), String> {
    validate_attachment_filename(&attachment.filename)?;
    validate_mime_type(&attachment.mime_type)?;
    Ok(())
}

fn validate_attachment_filename(value: &str) -> Result<(), String> {
    validate_header_value("Dateiname", value)?;
    let filename = value.trim();
    if filename.is_empty() {
        return Err("Anhang benötigt einen Dateinamen".to_string());
    }
    if filename.chars().any(char::is_control) {
        return Err("Dateiname des Anhangs enthält Steuerzeichen".to_string());
    }
    if filename == "."
        || filename == ".."
        || filename
            .chars()
            .any(|character| character == '/' || character == '\\')
    {
        return Err("Dateiname des Anhangs enthält einen ungültigen Pfad".to_string());
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
        return Err(format!("Ungültiger MIME-Typ des Anhangs: {mime_type}"));
    }
    Ok(mime_type.to_ascii_lowercase())
}

fn is_mime_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn new_boundary() -> String {
    format!("=_EvolutionGTK4_{}", uuid::Uuid::new_v4().simple())
}

fn append_multipart_header(raw: &mut String, kind: &str, boundary: &str) {
    raw.push_str("Content-Type: multipart/");
    raw.push_str(kind);
    raw.push_str("; boundary=\"");
    raw.push_str(boundary);
    raw.push_str("\"\r\n\r\n");
}

fn append_leaf_part(raw: &mut String, content_type: &str, body: &str) {
    raw.push_str("Content-Type: ");
    raw.push_str(content_type);
    raw.push_str("\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
    raw.push_str(body);
    ensure_crlf(raw);
}

fn append_text_part(raw: &mut String, boundary: &str, content_type: &str, body: &str) {
    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("\r\n");
    append_leaf_part(raw, content_type, body);
}

fn close_multipart(raw: &mut String, boundary: &str) {
    raw.push_str("--");
    raw.push_str(boundary);
    raw.push_str("--\r\n");
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
    raw.push_str("Content-Disposition: attachment");
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
        return Err("Die iTIP-MIME-Komponente darf nicht leer sein".to_string());
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

fn append_recipient_header(raw: &mut String, name: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }

    raw.push_str(name);
    raw.push_str(": ");
    // Evolution accepts semicolon-separated recipient lists in its fields;
    // RFC822 uses commas, so normalize only the separator at this boundary.
    let normalized = value.replace(';', ",");
    raw.push_str(&encode_address_header(&normalized));
    raw.push_str("\r\n");
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
        return Err(format!("{name} darf nicht leer sein"));
    }

    let wrapped = value.starts_with('<') || value.ends_with('>');
    let inner = if value.starts_with('<') && value.ends_with('>') {
        &value[1..value.len() - 1]
    } else if wrapped {
        return Err(format!("{name} hat ungültige spitze Klammern"));
    } else {
        value
    };
    if inner.is_empty()
        || inner
            .chars()
            .any(|character| character.is_whitespace() || character == '<' || character == '>')
    {
        return Err(format!("{name} hat ein ungültiges Format"));
    }
    Ok(())
}

fn validate_calendar_method(value: &str) -> Result<(), String> {
    validate_header_value("iTIP-Methode", value)?;
    let method = value.trim();
    if !is_mime_token(method) {
        return Err("Ungültige iTIP-Methode".to_string());
    }
    Ok(())
}

fn encode_header_value(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }

    format!("=?UTF-8?B?{}?=", base64(value.as_bytes()))
}

fn encode_address_header(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut phrase = String::new();
    let mut address = String::new();
    let mut in_address = false;

    for character in value.trim().chars() {
        if in_address {
            if character == '>' {
                output.push_str(&address);
                output.push('>');
                address.clear();
                in_address = false;
            } else {
                address.push(character);
            }
        } else if character == '<' {
            output.push_str(&encode_address_phrase(&phrase));
            phrase.clear();
            output.push('<');
            in_address = true;
        } else {
            phrase.push(character);
        }
    }

    if in_address {
        output.push('<');
        output.push_str(&address);
    } else {
        output.push_str(&encode_address_phrase(&phrase));
    }
    output
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
    if token.is_empty() || token.is_ascii() || token.contains('@') {
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
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(text.contains("Content-Type: text/plain; charset=UTF-8\r\n"));
        assert!(text.contains("Content-Type: text/html; charset=UTF-8\r\n"));
        assert!(text.contains("<p>Hello</p>\r\n"));
        assert!(text.ends_with("--\r\n"));
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
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\""));
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
            html_body: "Hello & goodbye\r\nSecond line".to_string(),
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
                .contains("Zeilenumbrüche")
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
        }]);
        let raw = build_rfc822(&message).expect("unicode attachment should build");
        let text = String::from_utf8(raw).expect("message should be UTF-8");
        assert!(text.contains("name*=utf-8''R%C3%A9sum%C3%A9%202026.pdf"));
        assert!(text.contains("filename*=utf-8''R%C3%A9sum%C3%A9%202026.pdf"));

        message.attachments[0].filename = "../secret.txt".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("path-like attachment names should be rejected")
                .contains("ungültigen Pfad")
        );

        message.attachments[0].filename = "valid.txt".to_string();
        message.attachments[0].mime_type = "text/plain; charset=UTF-8".to_string();
        assert!(
            build_rfc822(&message)
                .expect_err("MIME parameters must not be injected")
                .contains("MIME-Typ")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_directories_as_attachments() {
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-attachment-directory-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).expect("temporary attachment directory should be created");
        let message = outgoing_with_attachments(vec![DraftAttachment {
            path: path.to_string_lossy().into_owned(),
            filename: "directory".to_string(),
            mime_type: "application/octet-stream".to_string(),
            size: 0,
        }]);
        assert!(
            build_rfc822(&message)
                .expect_err("directories must not be sent as attachments")
                .contains("keine reguläre Datei")
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
            attachments,
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        }
    }
}
