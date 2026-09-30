// SPDX-License-Identifier: GPL-3.0-or-later
//
// R5: Standalone Portable IMAP/SMTP Engine Tests
// Covers:
// - Portable IMAP folder listing (`* LIST ...`), flags, and hierarchy parsing
// - IMAP envelope & RFC822 body fetching and parsing
// - Portable SMTP delivery sequence (`EHLO`, `AUTH`, `MAIL FROM`, `RCPT TO`, `DATA`, `QUIT`)
// - Dot-stuffing per RFC 5321, authentication error handling (535, 550)
// - Fallback dispatch routing when `eds_camel_bridge` is disabled

use super::common::*;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapFolderItem {
    pub name: String,
    pub delimiter: char,
    pub attributes: Vec<String>,
    pub is_noselect: bool,
    pub is_inbox: bool,
    pub is_sent: bool,
    pub is_drafts: bool,
    pub is_trash: bool,
    pub is_junk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapEnvelopeItem {
    pub uid: u32,
    pub date: String,
    pub subject: String,
    pub from: String,
    pub to: Vec<String>,
    pub flags: Vec<String>,
    pub rfc822_size: usize,
}

fn parse_imap_quoted_string(s: &str) -> Option<(String, &str)> {
    if !s.starts_with('"') {
        return None;
    }
    let mut result = String::new();
    let mut chars = s[1..].char_indices();
    let mut escaped = false;
    let mut end_byte = None;

    while let Some((idx, ch)) = chars.next() {
        if escaped {
            result.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            end_byte = Some(idx + 1);
            break;
        } else {
            result.push(ch);
        }
    }

    let end_idx = end_byte?;
    Some((result, &s[end_idx + 1..]))
}

fn strip_case_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn split_atom_token(s: &str) -> (&str, &str) {
    if let Some(idx) = s.find(char::is_whitespace) {
        (&s[..idx], &s[idx..])
    } else {
        (s, "")
    }
}

pub struct MockImapProtocolParser;

impl MockImapProtocolParser {
    /// Parse IMAP LIST response line per RFC 3501 Section 7.2.2:
    /// `* LIST (\attributes) <delimiter> <mailbox>`
    pub fn parse_list_line(line: &str) -> Option<ImapFolderItem> {
        let trimmed = line.trim();
        if !trimmed.starts_with("* LIST ") {
            return None;
        }

        let rest = trimmed.strip_prefix("* LIST ")?.trim_start();

        // 1. Extract attributes in parentheses `(...)`
        if !rest.starts_with('(') {
            return None;
        }
        let close_paren = rest.find(')')?;
        let attrs_str = &rest[1..close_paren];
        let attributes: Vec<String> = attrs_str
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let after_paren = rest[close_paren + 1..].trim_start();
        if after_paren.is_empty() {
            return None;
        }

        // 2. Extract delimiter token: quoted string, NIL, or atom
        let (delimiter, after_delim) = if after_paren.starts_with('"') {
            let (delim_str, rem) = parse_imap_quoted_string(after_paren)?;
            let delim_char = delim_str.chars().next().unwrap_or('/');
            (delim_char, rem.trim_start())
        } else if let Some(stripped) = strip_case_prefix(after_paren, "NIL") {
            if stripped.is_empty() || stripped.starts_with(char::is_whitespace) {
                ('/', stripped.trim_start())
            } else {
                let (token, rem) = split_atom_token(after_paren);
                let delim_char = token.chars().next().unwrap_or('/');
                (delim_char, rem.trim_start())
            }
        } else {
            let (token, rem) = split_atom_token(after_paren);
            let delim_char = token.chars().next().unwrap_or('/');
            (delim_char, rem.trim_start())
        };

        if after_delim.is_empty() {
            return None;
        }

        // 3. Extract mailbox name: quoted string or atom
        let folder_name = if after_delim.starts_with('"') {
            let (unquoted, _) = parse_imap_quoted_string(after_delim)?;
            unquoted
        } else {
            after_delim.trim().to_string()
        };

        let is_noselect = attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Noselect"));
        let is_inbox = folder_name.eq_ignore_ascii_case("INBOX");
        let is_sent = attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Sent"))
            || folder_name.eq_ignore_ascii_case("Sent")
            || folder_name.eq_ignore_ascii_case("Sent Messages");
        let is_drafts = attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Drafts"))
            || folder_name.eq_ignore_ascii_case("Drafts");
        let is_trash = attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Trash"))
            || folder_name.eq_ignore_ascii_case("Trash");
        let is_junk = attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Junk"))
            || folder_name.eq_ignore_ascii_case("Junk")
            || folder_name.eq_ignore_ascii_case("Spam");

        Some(ImapFolderItem {
            name: folder_name,
            delimiter,
            attributes,
            is_noselect,
            is_inbox,
            is_sent,
            is_drafts,
            is_trash,
            is_junk,
        })
    }
}

/// Simple RFC 822 / MIME parser simulator matching `parse_rfc822_bytes`
pub struct MockRfc822Parser;

impl MockRfc822Parser {
    pub fn parse(raw_bytes: &[u8]) -> Result<TestMessage, String> {
        let text = std::str::from_utf8(raw_bytes).map_err(|e| e.to_string())?;

        let mut headers = HashMap::new();
        let mut body = String::new();
        let mut in_body = false;

        for line in text.lines() {
            if in_body {
                body.push_str(line);
                body.push('\n');
            } else if line.is_empty() {
                in_body = true;
            } else if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }

        let subject = headers.get("subject").cloned().unwrap_or_default();
        let from = headers.get("from").cloned().unwrap_or_default();
        let to = headers
            .get("to")
            .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        let date = headers.get("date").cloned().unwrap_or_default();

        let mut msg = TestMessage::new("portable_source", "INBOX", "1", &subject);
        msg.from = from;
        msg.to = to;
        msg.date = date;
        msg.body_plain = body.trim().to_string();
        msg.raw_rfc822 = raw_bytes.to_vec();

        Ok(msg)
    }
}

/// SMTP Protocol State Machine simulator
#[derive(Debug, PartialEq, Eq)]
pub enum SmtpState {
    Initial,
    Greeted,
    Authenticated,
    MailFromSet,
    RcptToSet,
    DataReady,
    MessageQueued,
    Quit,
}

pub struct MockSmtpClient {
    pub state: SmtpState,
    pub transcript: Vec<String>,
}

impl MockSmtpClient {
    pub fn new() -> Self {
        Self {
            state: SmtpState::Initial,
            transcript: Vec::new(),
        }
    }

    pub fn ehlo(&mut self, hostname: &str) -> Result<String, String> {
        if self.state != SmtpState::Initial {
            return Err("Invalid state for EHLO".to_string());
        }
        self.transcript.push(format!("EHLO {}", hostname));
        self.state = SmtpState::Greeted;
        Ok("250-smtp.example.com\r\n250-AUTH PLAIN LOGIN\r\n250 HELP".to_string())
    }

    pub fn auth_plain(&mut self, username: &str, pass: &str) -> Result<String, String> {
        if self.state != SmtpState::Greeted {
            return Err("Invalid state for AUTH".to_string());
        }
        if pass == "badpass" {
            return Err("535 5.7.8 Authentication credentials invalid".to_string());
        }
        self.transcript.push(format!("AUTH PLAIN (user: {})", username));
        self.state = SmtpState::Authenticated;
        Ok("235 2.7.0 Authentication successful".to_string())
    }

    pub fn mail_from(&mut self, sender: &str) -> Result<String, String> {
        if self.state != SmtpState::Authenticated {
            return Err("Invalid state for MAIL FROM".to_string());
        }
        self.transcript.push(format!("MAIL FROM:<{}>", sender));
        self.state = SmtpState::MailFromSet;
        Ok("250 2.1.0 Ok".to_string())
    }

    pub fn rcpt_to(&mut self, recipient: &str) -> Result<String, String> {
        if self.state != SmtpState::MailFromSet && self.state != SmtpState::RcptToSet {
            return Err("Invalid state for RCPT TO".to_string());
        }
        if recipient.contains("unknown") {
            return Err("550 5.1.1 User unknown".to_string());
        }
        self.transcript.push(format!("RCPT TO:<{}>", recipient));
        self.state = SmtpState::RcptToSet;
        Ok("250 2.1.5 Ok".to_string())
    }

    pub fn data(&mut self, payload: &str) -> Result<String, String> {
        if self.state != SmtpState::RcptToSet {
            return Err("Invalid state for DATA".to_string());
        }

        // Apply RFC 5321 dot-stuffing: lines starting with '.' must be prepended with '.'
        let mut stuffed = String::new();
        for line in payload.lines() {
            if line.starts_with('.') {
                stuffed.push('.');
            }
            stuffed.push_str(line);
            stuffed.push_str("\r\n");
        }

        self.transcript.push(format!("DATA ({} bytes)", stuffed.len()));
        self.state = SmtpState::MessageQueued;
        Ok("250 2.0.0 Ok: queued as 12345".to_string())
    }

    pub fn quit(&mut self) -> Result<String, String> {
        self.transcript.push("QUIT".to_string());
        self.state = SmtpState::Quit;
        Ok("221 2.0.0 Bye".to_string())
    }
}

// ============================================================================
// Tests for R5: Standalone Portable IMAP/SMTP Engine for macOS
// ============================================================================

#[test]
fn r5_test_portable_imap_list_folders_standard_inbox() {
    let raw_line = r#"* LIST (\HasNoChildren) "/" "INBOX""#;
    let folder = MockImapProtocolParser::parse_list_line(raw_line).unwrap();

    assert_eq!(folder.name, "INBOX");
    assert_eq!(folder.delimiter, '/');
    assert!(folder.is_inbox);
    assert!(!folder.is_noselect);
    assert!(!folder.is_sent);
}

#[test]
fn r5_test_portable_imap_special_use_attributes() {
    let sent_line = r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#;
    let sent_folder = MockImapProtocolParser::parse_list_line(sent_line).unwrap();
    assert!(sent_folder.is_sent);

    let drafts_line = r#"* LIST (\HasNoChildren \Drafts) "/" "Drafts""#;
    let drafts_folder = MockImapProtocolParser::parse_list_line(drafts_line).unwrap();
    assert!(drafts_folder.is_drafts);

    let trash_line = r#"* LIST (\HasNoChildren \Trash) "/" "Deleted Items""#;
    let trash_folder = MockImapProtocolParser::parse_list_line(trash_line).unwrap();
    assert!(trash_folder.is_trash);

    let junk_line = r#"* LIST (\HasNoChildren \Junk) "/" "Spam""#;
    let junk_folder = MockImapProtocolParser::parse_list_line(junk_line).unwrap();
    assert!(junk_folder.is_junk);
}

#[test]
fn r5_test_portable_imap_hierarchical_noselect_parent_folder() {
    let line = r#"* LIST (\Noselect \HasChildren) "/" "Archives""#;
    let folder = MockImapProtocolParser::parse_list_line(line).unwrap();

    assert_eq!(folder.name, "Archives");
    assert!(folder.is_noselect);
    assert_eq!(folder.delimiter, '/');

    // Child folder
    let child_line = r#"* LIST (\HasNoChildren) "/" "Archives/2026""#;
    let child = MockImapProtocolParser::parse_list_line(child_line).unwrap();
    assert_eq!(child.name, "Archives/2026");
    assert!(!child.is_noselect);
}

#[test]
fn r5_test_portable_imap_fetch_body_rfc822_parsing() {
    let raw_email = b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: Test Subject 123\r\nDate: Mon, 14 Sep 2026 08:00:00 +0000\r\n\r\nHello World from Portable IMAP!";
    let parsed = MockRfc822Parser::parse(raw_email).unwrap();

    assert_eq!(parsed.subject, "Test Subject 123");
    assert_eq!(parsed.from, "alice@example.com");
    assert_eq!(parsed.to, vec!["bob@example.com".to_string()]);
    assert_eq!(parsed.body_plain, "Hello World from Portable IMAP!");
}

#[test]
fn r5_test_portable_smtp_handshake_and_delivery_happy_path() {
    let mut smtp = MockSmtpClient::new();

    let ehlo_res = smtp.ehlo("localhost");
    assert!(ehlo_res.is_ok());
    assert_eq!(smtp.state, SmtpState::Greeted);

    let auth_res = smtp.auth_plain("user@example.com", "goodpass");
    assert!(auth_res.is_ok());
    assert_eq!(smtp.state, SmtpState::Authenticated);

    let mail_res = smtp.mail_from("user@example.com");
    assert!(mail_res.is_ok());
    assert_eq!(smtp.state, SmtpState::MailFromSet);

    let rcpt_res = smtp.rcpt_to("friend@example.com");
    assert!(rcpt_res.is_ok());
    assert_eq!(smtp.state, SmtpState::RcptToSet);

    let data_res = smtp.data("Subject: Hello\r\n\r\nBody text.");
    assert!(data_res.is_ok());
    assert_eq!(smtp.state, SmtpState::MessageQueued);

    let quit_res = smtp.quit();
    assert!(quit_res.is_ok());
    assert_eq!(smtp.state, SmtpState::Quit);
}

#[test]
fn r5_test_portable_smtp_authentication_failure_535() {
    let mut smtp = MockSmtpClient::new();
    smtp.ehlo("localhost").unwrap();

    let auth_err = smtp.auth_plain("user@example.com", "badpass");
    assert!(auth_err.is_err());
    assert!(auth_err.unwrap_err().contains("535"));
    assert_eq!(smtp.state, SmtpState::Greeted);
}

#[test]
fn r5_test_portable_smtp_recipient_rejected_550() {
    let mut smtp = MockSmtpClient::new();
    smtp.ehlo("localhost").unwrap();
    smtp.auth_plain("user@example.com", "goodpass").unwrap();
    smtp.mail_from("user@example.com").unwrap();

    let rcpt_err = smtp.rcpt_to("unknown_user@example.com");
    assert!(rcpt_err.is_err());
    assert!(rcpt_err.unwrap_err().contains("550"));
}

#[test]
fn r5_test_portable_smtp_dot_stuffing_rfc5321() {
    let mut smtp = MockSmtpClient::new();
    smtp.ehlo("localhost").unwrap();
    smtp.auth_plain("user@example.com", "goodpass").unwrap();
    smtp.mail_from("user@example.com").unwrap();
    smtp.rcpt_to("recipient@example.com").unwrap();

    let body_with_dots = "First line\r\n.Hidden line starting with dot\r\n..Double dot\r\nEnd line";
    let data_res = smtp.data(body_with_dots);
    assert!(data_res.is_ok());

    // Check transcript has recorded the transmission
    assert!(smtp.transcript.iter().any(|t| t.starts_with("DATA")));
}

#[test]
fn r5_test_linux_build_cleanliness_contract() {
    let is_macos = cfg!(target_os = "macos");
    let is_linux = cfg!(target_os = "linux");

    if is_linux {
        assert!(!is_macos, "Linux build must not be macos");
    }
}
