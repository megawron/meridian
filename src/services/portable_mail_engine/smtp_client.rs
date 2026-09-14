// SPDX-License-Identifier: GPL-3.0-or-later
//
// Standalone Portable SMTP Client
// Implements SMTP (RFC 5321), STARTTLS, SASL AUTH (PLAIN & LOGIN),
// recipient delivery, and RFC 5321 dot-stuffing.

use super::connection::MailConnection;
use std::sync::atomic::AtomicBool;

pub struct SmtpClient {
    conn: MailConnection,
}

impl SmtpClient {
    pub fn new(conn: MailConnection) -> Self {
        Self { conn }
    }

    pub fn connect(
        host: &str,
        port: u16,
        tls_mode: &str,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        let use_tls = tls_mode.eq_ignore_ascii_case("tls")
            || tls_mode.eq_ignore_ascii_case("ssl")
            || port == 465;

        let mut conn = MailConnection::connect(host, port, use_tls, cancelled)?;

        // Read 220 banner
        let banner = conn.read_line()?;
        if !banner.starts_with("220") {
            return Err(format!("Ungültiger SMTP-Banner von {host}:{port}: {banner}"));
        }

        Ok(Self::new(conn))
    }

    pub fn ehlo(&mut self, hostname: &str) -> Result<String, String> {
        self.conn.write_line(&format!("EHLO {hostname}"))?;

        let mut capabilities = String::new();
        loop {
            let line = self.conn.read_line()?;
            capabilities.push_str(&line);
            capabilities.push('\n');

            if line.starts_with("250 ") {
                break;
            } else if !line.starts_with("250-") {
                return Err(format!("EHLO abgewiesen: {line}"));
            }
        }

        Ok(capabilities)
    }

    pub fn starttls(&mut self, host: &str, port: u16) -> Result<(), String> {
        self.conn.write_line("STARTTLS")?;
        let resp = self.conn.read_line()?;
        if !resp.starts_with("220") {
            return Err(format!("STARTTLS abgelehnt: {resp}"));
        }
        self.conn.starttls(host, port)?;
        Ok(())
    }

    pub fn auth_plain(&mut self, username: &str, pass: &str) -> Result<(), String> {
        // PLAIN format: \0<username>\0<password>
        let mut raw = Vec::new();
        raw.push(0);
        raw.extend_from_slice(username.as_bytes());
        raw.push(0);
        raw.extend_from_slice(pass.as_bytes());

        let encoded = glib::base64_encode(&raw);
        self.conn.write_line(&format!("AUTH PLAIN {encoded}"))?;

        let resp = self.conn.read_line()?;
        if resp.starts_with("235") {
            Ok(())
        } else {
            Err(format!("SMTP-Authentifizierungsfehler (535): {resp}"))
        }
    }

    pub fn auth_login(&mut self, username: &str, pass: &str) -> Result<(), String> {
        self.conn.write_line("AUTH LOGIN")?;
        let challenge = self.conn.read_line()?;
        if !challenge.starts_with("334") {
            return Err(format!("AUTH LOGIN abgewiesen: {challenge}"));
        }

        let u_encoded = glib::base64_encode(username.as_bytes());
        self.conn.write_line(&u_encoded)?;

        let p_challenge = self.conn.read_line()?;
        if !p_challenge.starts_with("334") {
            return Err(format!("Passwort-Challenge fehlgeschlagen: {p_challenge}"));
        }

        let p_encoded = glib::base64_encode(pass.as_bytes());
        self.conn.write_line(&p_encoded)?;

        let resp = self.conn.read_line()?;
        if resp.starts_with("235") {
            Ok(())
        } else {
            Err(format!("SMTP-Authentifizierungsfehler (535): {resp}"))
        }
    }

    pub fn mail_from(&mut self, sender: &str) -> Result<(), String> {
        self.conn.write_line(&format!("MAIL FROM:<{sender}>"))?;
        let resp = self.conn.read_line()?;
        if resp.starts_with("250") {
            Ok(())
        } else {
            Err(format!("MAIL FROM abgewiesen ({sender}): {resp}"))
        }
    }

    pub fn rcpt_to(&mut self, recipient: &str) -> Result<(), String> {
        self.conn.write_line(&format!("RCPT TO:<{recipient}>"))?;
        let resp = self.conn.read_line()?;
        if resp.starts_with("250") || resp.starts_with("251") {
            Ok(())
        } else if resp.starts_with("550") {
            Err(format!("Empfänger zurückgewiesen (550): {resp}"))
        } else {
            Err(format!("RCPT TO abgewiesen ({recipient}): {resp}"))
        }
    }

    pub fn data(&mut self, raw_message: &[u8]) -> Result<(), String> {
        self.conn.write_line("DATA")?;
        let resp = self.conn.read_line()?;
        if !resp.starts_with("354") {
            return Err(format!("DATA abgewiesen: {resp}"));
        }

        // Apply RFC 5321 dot-stuffing:
        // Any line beginning with a dot must have an additional dot prepended.
        let mut stuffed = Vec::new();
        let mut at_line_start = true;

        for &byte in raw_message {
            if at_line_start && byte == b'.' {
                stuffed.push(b'.');
            }

            stuffed.push(byte);

            if byte == b'\n' {
                at_line_start = true;
            } else if byte != b'\r' {
                at_line_start = false;
            }
        }

        // Ensure trailing CRLF before terminating dot
        if !stuffed.ends_with(b"\r\n") {
            if stuffed.ends_with(b"\n") {
                stuffed.pop();
            }
            stuffed.extend_from_slice(b"\r\n");
        }

        stuffed.extend_from_slice(b".\r\n");

        self.conn.write_all(&stuffed)?;

        let final_resp = self.conn.read_line()?;
        if final_resp.starts_with("250") {
            Ok(())
        } else {
            Err(format!("Nachrichtenzustellung fehlgeschlagen: {final_resp}"))
        }
    }

    pub fn quit(&mut self) -> Result<(), String> {
        let _ = self.conn.write_line("QUIT");
        let _ = self.conn.read_line();
        Ok(())
    }
}

/// Unfold RFC 5322 header lines into logical header lines.
///
/// Per RFC 5322 Section 2.2.3, continuation lines begin with whitespace (space or horizontal tab).
/// Unfolding joins continuation lines to the preceding header line.
/// Scanning stops at the first empty line, which marks the end of the header section.
fn unfold_header_lines(raw_message: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw_message);
    let mut unfolded: Vec<String> = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            break; // Header section ended per RFC 5322 Section 2.1
        }

        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            let last = unfolded.last_mut().unwrap();
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if !last.ends_with(' ') && !last.ends_with('\t') {
                    last.push(' ');
                }
                last.push_str(trimmed);
            }
        } else {
            unfolded.push(line.to_string());
        }
    }

    unfolded
}

/// Splits a comma- or semicolon-separated list of addresses in a header field body,
/// respecting quoted strings (`"..."`), angle brackets (`<...>`), and comments (`(...)`).
///
/// Commas inside quoted display names (e.g. `"Public, John" <john@example.com>`)
/// are NOT treated as address separators.
fn split_header_addresses(val: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape = false;
    let mut paren_depth: usize = 0;
    let mut in_angle = false;

    for ch in val.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' if in_quotes => {
                escape = true;
                current.push(ch);
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '(' if !in_quotes => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_quotes && paren_depth > 0 => {
                paren_depth -= 1;
                current.push(ch);
            }
            '<' if !in_quotes => {
                in_angle = true;
                current.push(ch);
            }
            '>' if !in_quotes => {
                in_angle = false;
                current.push(ch);
            }
            ',' | ';' if !in_quotes && paren_depth == 0 && !in_angle => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }

    parts
}

/// Extract sender and recipient email addresses from raw RFC 822 / RFC 5322 message bytes.
///
/// Conforms to RFC 5322:
/// - Unfolds continuation lines (Section 2.2.3) so folded To:/Cc:/Bcc: headers are not truncated.
/// - Parses recipient lists with quote awareness (Section 3.4), preserving display names with commas.
/// - Validates that extracted addresses contain a valid `@` specification, discarding corrupt tokens.
pub fn extract_envelope_addresses(raw_message: &[u8]) -> (String, Vec<String>) {
    let mut sender = String::new();
    let mut recipients: Vec<String> = Vec::new();

    let header_lines = unfold_header_lines(raw_message);

    for header in &header_lines {
        if let Some((name, val)) = header.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            let val = val.trim();

            if key == "from" && sender.is_empty() {
                for part in split_header_addresses(val) {
                    let addr = clean_email_address(&part);
                    if !addr.is_empty() {
                        sender = addr;
                        break;
                    }
                }
            } else if key == "to" || key == "cc" || key == "bcc" {
                for part in split_header_addresses(val) {
                    let addr = clean_email_address(&part);
                    if !addr.is_empty() && !recipients.iter().any(|r| r.eq_ignore_ascii_case(&addr)) {
                        recipients.push(addr);
                    }
                }
            }
        }
    }

    (sender, recipients)
}

/// Extracts and sanitizes a valid email address (`local-part@domain`) from an address specification token.
///
/// Handles:
/// - Angle-bracketed addresses: `"Display Name" <user@domain.com>` -> `user@domain.com`
/// - Route addresses: `<@route1,@route2:user@domain.com>` -> `user@domain.com`
/// - Quoted display names containing angle brackets: `"User <Nick>" <real@domain.com>` -> `real@domain.com`
/// - Bare email addresses: `user@domain.com` -> `user@domain.com`
/// - Addresses with comments: `user@domain.com (John Doe)` -> `user@domain.com`
/// - Rejection of invalid tokens lacking `@` (e.g. `"Public"`, `undisclosed-recipients:;`) -> `""`
fn clean_email_address(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Check for angle brackets `<...>` that are outside double quotes
    let mut in_quotes = false;
    let mut escape = false;
    let mut angle_start = None;
    let mut angle_end = None;

    for (idx, ch) in trimmed.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                escape = true;
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            '<' if !in_quotes => {
                if angle_start.is_none() {
                    angle_start = Some(idx);
                }
            }
            '>' if !in_quotes => {
                if angle_start.is_some() {
                    angle_end = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }

    let candidate = if let (Some(start), Some(end)) = (angle_start, angle_end) {
        if start < end {
            let mut inner = trimmed[start + 1..end].trim();
            // Handle obsolete route address e.g. @route1,@route2:user@domain
            if let Some(colon) = inner.rfind(':') {
                inner = inner[colon + 1..].trim();
            }
            inner
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    // Strip comments `(...)` from candidate
    let mut stripped = String::new();
    let mut paren_depth = 0;
    for ch in candidate.chars() {
        match ch {
            '(' => paren_depth += 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ch if paren_depth == 0 => stripped.push(ch),
            _ => {}
        }
    }

    // Find the whitespace-separated token that contains '@'
    let token = stripped
        .split_whitespace()
        .find(|tok| tok.contains('@'))
        .unwrap_or(stripped.trim());

    // Clean surrounding punctuation, quotes, angle brackets, and delimiters
    let cleaned = token.trim_matches(|c: char| {
        c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>' || c == ',' || c == ';'
    });

    // Validate that it conforms to a plausible email: contains '@',
    // does not start or end with '@', and contains no internal whitespace
    if let Some(at_idx) = cleaned.find('@') {
        if at_idx > 0 && at_idx < cleaned.len() - 1 && !cleaned.contains(' ') {
            return cleaned.to_string();
        }
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtp_dot_stuffing_matrix() {
        let input = b"Header: test\r\n\r\n.\r\n..\r\n...\r\n.Starts with dot\r\nNormal line\r\nLine with . dot inside\r\n.";
        
        let conn = MailConnection::new_mock(vec![
            "354 Start mail input; end with <CRLF>.<CRLF>".into(),
            "250 OK: queued".into(),
        ]);
        let mut client = SmtpClient::new(conn);
        let res = client.data(input);
        assert!(res.is_ok(), "data should succeed: {:?}", res.err());

        let written = client.conn.mock_written();
        assert_eq!(written.len(), 2);
        assert_eq!(written[0], "DATA\r\n");
        let data_payload = &written[1];

        // Verify terminating sequence: \r\n.\r\n
        assert!(data_payload.ends_with("\r\n.\r\n"), "Must terminate with \\r\\n.\\r\\n");

        // Verify stuffed lines
        assert!(data_payload.contains("\r\n..\r\n"), "Single dot must become ..: {}", data_payload);
        assert!(data_payload.contains("\r\n...\r\n"), "Two dots must become ...: {}", data_payload);
        assert!(data_payload.contains("\r\n....\r\n"), "Three dots must become ....: {}", data_payload);
        assert!(data_payload.contains("\r\n..Starts with dot\r\n"), "Dotted line must have dot prepended");
        assert!(data_payload.contains("\r\nNormal line\r\n"));
        assert!(data_payload.contains("\r\nLine with . dot inside\r\n"));
    }

    #[test]
    fn test_smtp_dot_stuffing_large_body() {
        let mut body = Vec::new();
        body.extend_from_slice(b"Subject: Huge Body\r\n\r\n");
        for i in 0..5000 {
            if i % 2 == 0 {
                body.extend_from_slice(b".Dotted line in big payload\r\n");
            } else {
                body.extend_from_slice(b"Regular text line in big payload\r\n");
            }
        }

        let conn = MailConnection::new_mock(vec![
            "354 Go ahead".into(),
            "250 2.0.0 OK".into(),
        ]);
        let mut client = SmtpClient::new(conn);
        assert!(client.data(&body).is_ok());

        let written = client.conn.mock_written();
        let payload = &written[1];
        assert!(payload.ends_with("\r\n.\r\n"));
        assert!(!payload.contains("\r\n.Dotted line"));
        assert!(payload.contains("\r\n..Dotted line"));
    }

    #[test]
    fn test_smtp_sasl_plain_formatting() {
        let conn = MailConnection::new_mock(vec![
            "235 2.7.0 Authentication successful".into(),
        ]);
        let mut client = SmtpClient::new(conn);
        let res = client.auth_plain("testuser", "secretpass123");
        assert!(res.is_ok());

        let written = client.conn.mock_written();
        assert_eq!(written.len(), 1);

        // Expected SASL PLAIN: \0testuser\0secretpass123
        let expected_raw = b"\0testuser\0secretpass123";
        let expected_b64 = glib::base64_encode(expected_raw);
        let expected_cmd = format!("AUTH PLAIN {}\r\n", expected_b64);
        assert_eq!(written[0], expected_cmd);
    }

    #[test]
    fn test_extract_envelope_addresses_variants() {
        let raw = b"From: \"Alice Smith\" <alice@example.com>\r\n\
To: bob@example.com, \"Charlie Brown\" <charlie@example.com>\r\n\
Cc: dave@example.com\r\n\
Bcc: secret@example.com\r\n\
Subject: Multi-recipient Test\r\n\
\r\n\
Body text";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "alice@example.com");
        assert_eq!(
            recipients,
            vec![
                "bob@example.com".to_string(),
                "charlie@example.com".to_string(),
                "dave@example.com".to_string(),
                "secret@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn test_extract_envelope_addresses_folded_headers() {
        let raw = concat!(
            "From: alice@example.com\r\n",
            "To: first@example.com,\r\n",
            "    second@example.com,\r\n",
            "\tthird@example.com\r\n",
            "Subject: Folded Header Test\r\n",
            "\r\n",
            "Body"
        ).as_bytes();

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "alice@example.com");
        // All recipients across folded continuation lines must be extracted
        assert_eq!(
            recipients,
            vec![
                "first@example.com".to_string(),
                "second@example.com".to_string(),
                "third@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn test_extract_envelope_addresses_quoted_comma() {
        let raw = b"From: \"Smith, Alice\" <alice@example.com>\r\n\
To: \"Tester, Evolution\" <martin@example.com>\r\n\
Subject: Comma Test\r\n\
\r\n\
Body";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "alice@example.com");
        // Must NOT produce "Tester" as a recipient
        assert_eq!(recipients, vec!["martin@example.com".to_string()]);
    }

    #[test]
    fn test_extract_envelope_addresses_quoted_comma_multiple() {
        let raw = b"From: \"Public, John Q.\" <john@example.com>\r\n\
To: \"Doe, Jane, Jr.\" <jane@example.com>, \"Boss, Big\" <boss@example.com>, regular@example.com\r\n\
Cc: \"Consultant, Ext.\" <consultant@example.com>\r\n\
\r\n\
Body";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "john@example.com");
        assert_eq!(
            recipients,
            vec![
                "jane@example.com".to_string(),
                "boss@example.com".to_string(),
                "regular@example.com".to_string(),
                "consultant@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn test_extract_envelope_addresses_display_name_with_embedded_angles() {
        let raw = b"From: \"User <Nickname> Test\" <real_sender@example.com>\r\n\
To: \"Another <Alias> Person\" <real_recipient@example.com>\r\n\
\r\n\
Body";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "real_sender@example.com");
        assert_eq!(recipients, vec!["real_recipient@example.com".to_string()]);
    }

    #[test]
    fn test_extract_envelope_addresses_discards_bogus_tokens() {
        let raw = b"From: sender@example.com\r\n\
To: \"Just A Name\", undisclosed-recipients:;, valid@example.com, <>\r\n\
\r\n\
Body";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "sender@example.com");
        // "Just A Name", "undisclosed-recipients:;", and "<>" lack '@' and must be rejected
        assert_eq!(recipients, vec!["valid@example.com".to_string()]);
    }

    #[test]
    fn test_extract_envelope_addresses_comments_and_semicolons() {
        let raw = b"From: sender@example.com (Sender Name)\r\n\
To: recipient1@example.com (Comment, With, Comma); recipient2@example.com\r\n\
\r\n\
Body";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "sender@example.com");
        assert_eq!(
            recipients,
            vec![
                "recipient1@example.com".to_string(),
                "recipient2@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn test_extract_envelope_addresses_route_syntax() {
        let raw = b"From: <@route1.org,@route2.org:sender@example.com>\r\n\
To: <@mx.domain.com:recipient@example.com>\r\n\
\r\n\
Body";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "sender@example.com");
        assert_eq!(recipients, vec!["recipient@example.com".to_string()]);
    }

    #[test]
    fn test_extract_envelope_addresses_case_insensitive_dedup() {
        let raw = b"From: sender@example.com\r\n\
To: alice@example.com, Alice@example.com, ALICE@EXAMPLE.COM\r\n\
\r\n\
Body";

        let (_, recipients) = extract_envelope_addresses(raw);
        assert_eq!(recipients, vec!["alice@example.com".to_string()]);
    }

    #[test]
    fn test_extract_envelope_addresses_body_isolation() {
        let raw = b"From: sender@example.com\r\n\
To: real@example.com\r\n\
\r\n\
From: fake_body_sender@example.com\r\n\
To: fake_body_recipient@example.com\r\n";

        let (sender, recipients) = extract_envelope_addresses(raw);
        assert_eq!(sender, "sender@example.com");
        assert_eq!(recipients, vec!["real@example.com".to_string()]);
    }
}


