// SPDX-License-Identifier: GPL-3.0-or-later
//
// Standalone Portable IMAP Client
// Implements IMAP4rev1 (RFC 3501) folder listing, message fetching,
// flag mutations, and folder hierarchy parsing.

use super::connection::MailConnection;
use crate::services::camel_mail_service::CamelFolderSummary;
use crate::services::mail_store_service::{MailBackendLocation, ParsedMail};
use std::sync::atomic::AtomicBool;

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

/// Parse a quoted string from the beginning of `s` (RFC 3501 Section 4.3).
/// Handles escaped quotes `\"` and escaped backslashes `\\`.
/// Returns `(unescaped_string, remainder_after_closing_quote)`.
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

/// Case-insensitive prefix stripper.
fn strip_case_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// Split first whitespace-delimited atom token from `s`.
fn split_atom_token(s: &str) -> (&str, &str) {
    if let Some(idx) = s.find(char::is_whitespace) {
        (&s[..idx], &s[idx..])
    } else {
        (s, "")
    }
}

/// Escape double quotes and backslashes for IMAP quoted strings (RFC 3501 Section 4.3).
fn escape_imap_quoted_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Extract message UID from untagged FETCH response line or trailing line.
/// Matches `UID <digits>` with word/paren boundaries to avoid substring false positives.
fn extract_imap_fetch_uid(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let upper = line.to_ascii_uppercase();
    let mut search_start = 0;
    while let Some(pos) = upper[search_start..].find("UID ") {
        let abs_pos = search_start + pos;
        let is_boundary = abs_pos == 0 || {
            let prev = bytes[abs_pos - 1];
            prev == b' ' || prev == b'(' || prev == b'\t'
        };
        if is_boundary {
            let rest = &line[abs_pos + 4..];
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            let val = &rest[..end];
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
        search_start = abs_pos + 4;
    }
    None
}

impl ImapFolderItem {
    /// Parse IMAP LIST response line per RFC 3501 Section 7.2.2:
    /// `* LIST (\attributes) <delimiter> <mailbox>`
    /// Handles quoted and unquoted mailbox names, quoted delimiters, and NIL.
    pub fn parse_list_line(line: &str) -> Option<Self> {
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

        // Mailbox argument is mandatory
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

        let is_noselect = attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\Noselect"));
        let is_inbox = folder_name.eq_ignore_ascii_case("INBOX");
        let is_sent = attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\Sent"))
            || folder_name.eq_ignore_ascii_case("Sent")
            || folder_name.eq_ignore_ascii_case("Sent Messages")
            || folder_name.eq_ignore_ascii_case("Gesendet");
        let is_drafts = attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\Drafts"))
            || folder_name.eq_ignore_ascii_case("Drafts")
            || folder_name.eq_ignore_ascii_case("Entwürfe");
        let is_trash = attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\Trash"))
            || folder_name.eq_ignore_ascii_case("Trash")
            || folder_name.eq_ignore_ascii_case("Deleted Items")
            || folder_name.eq_ignore_ascii_case("Papierkorb");
        let is_junk = attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case("\\Junk"))
            || folder_name.eq_ignore_ascii_case("Junk")
            || folder_name.eq_ignore_ascii_case("Spam");

        Some(Self {
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

    pub fn to_folder_summary(
        &self,
        source_uid: &str,
        source_display: &str,
        unread: u32,
        total: u32,
    ) -> CamelFolderSummary {
        let display_name = if self.is_inbox {
            "Posteingang".to_string()
        } else if self.is_sent {
            "Gesendet".to_string()
        } else if self.is_drafts {
            "Entwürfe".to_string()
        } else if self.is_trash {
            "Papierkorb".to_string()
        } else if self.is_junk {
            "Spam".to_string()
        } else {
            self.name.clone()
        };

        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display.to_string(),
            folder_name: self.name.clone(),
            folder_id: format!("{}:{}", source_uid, self.name),
            display_name,
            unread_count: unread,
            total_count: total,
            is_selectable: !self.is_noselect,
        }
    }
}

pub struct ImapClient {
    conn: MailConnection,
    tag_counter: usize,
}

impl ImapClient {
    pub fn new(conn: MailConnection) -> Self {
        Self {
            conn,
            tag_counter: 0,
        }
    }

    pub fn connect_and_login(
        host: &str,
        port: u16,
        tls_mode: &str,
        user: &str,
        pass: &str,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        let use_tls = tls_mode.eq_ignore_ascii_case("tls")
            || tls_mode.eq_ignore_ascii_case("ssl")
            || port == 993;
        let is_starttls = tls_mode.eq_ignore_ascii_case("starttls");

        let mut conn = MailConnection::connect(host, port, use_tls, cancelled)?;

        // Read greeting
        let greeting = conn.read_line()?;
        if !greeting.starts_with("* OK") {
            return Err(format!("Ungültige IMAP-Begrüßung: {greeting}"));
        }

        let mut client = Self::new(conn);

        if is_starttls {
            client.send_command("STARTTLS")?;
            client.conn.starttls(host, port)?;
        }

        // Login
        client.login(user, pass)?;

        Ok(client)
    }

    pub fn login(&mut self, user: &str, pass: &str) -> Result<(), String> {
        let tag = self.next_tag();
        let esc_user = escape_imap_quoted_string(user);
        let esc_pass = escape_imap_quoted_string(pass);
        let cmd = format!("{tag} LOGIN \"{esc_user}\" \"{esc_pass}\"");
        self.conn.write_line(&cmd)?;

        loop {
            let line = self.conn.read_line()?;
            if line.starts_with(&tag) {
                if line.contains(" OK") {
                    return Ok(());
                } else {
                    return Err(format!("IMAP-Anmeldung fehlgeschlagen: {line}"));
                }
            }
        }
    }

    fn next_tag(&mut self) -> String {
        self.tag_counter += 1;
        format!("A{:04}", self.tag_counter)
    }

    fn send_command(&mut self, cmd: &str) -> Result<Vec<String>, String> {
        let tag = self.next_tag();
        self.conn.write_line(&format!("{tag} {cmd}"))?;

        let mut lines = Vec::new();
        loop {
            let line = self.conn.read_line()?;
            if line.starts_with(&tag) {
                if line.contains(" OK") {
                    lines.push(line);
                    return Ok(lines);
                } else {
                    return Err(format!("IMAP-Befehlsfehler ({cmd}): {line}"));
                }
            } else {
                lines.push(line);
            }
        }
    }

    pub fn list_folders(&mut self) -> Result<Vec<ImapFolderItem>, String> {
        let lines = self.send_command("LIST \"\" \"*\"")?;
        let mut folders = Vec::new();
        for line in lines {
            if let Some(folder) = ImapFolderItem::parse_list_line(&line) {
                folders.push(folder);
            }
        }
        Ok(folders)
    }

    pub fn select_folder(&mut self, folder: &str) -> Result<(u32, u32), String> {
        let lines = self.send_command(&format!("SELECT \"{folder}\""))?;
        let mut exists = 0;
        let mut unseen = 0;

        for line in lines {
            let trimmed = line.trim();
            if trimmed.starts_with("* ") && trimmed.ends_with(" EXISTS") {
                if let Some(num_str) = trimmed.strip_prefix("* ") {
                    if let Some(num_str) = num_str.strip_suffix(" EXISTS") {
                        exists = num_str.trim().parse::<u32>().unwrap_or(0);
                    }
                }
            } else if trimmed.contains("[UNSEEN ") {
                if let Some(idx) = trimmed.find("[UNSEEN ") {
                    let rest = &trimmed[idx + 8..];
                    if let Some(end) = rest.find(']') {
                        unseen = rest[..end].trim().parse::<u32>().unwrap_or(0);
                    }
                }
            }
        }

        Ok((exists, unseen))
    }

    pub fn fetch_envelopes(
        &mut self,
        folder_name: &str,
        offset: usize,
        limit: usize,
        account_uid: &str,
    ) -> Result<Vec<ParsedMail>, String> {
        let (total, _) = self.select_folder(folder_name)?;
        if total == 0 || offset >= total as usize {
            return Ok(Vec::new());
        }

        // Calculate sequence range (1-based message sequence numbers per RFC 3501 Section 6.4.5)
        let end = (total as usize).saturating_sub(offset);
        if end == 0 {
            return Ok(Vec::new());
        }
        let start = end.saturating_sub(limit).saturating_add(1).max(1);

        if start > end {
            return Ok(Vec::new());
        }

        let tag = self.next_tag();
        // RFC 3501: Use standard FETCH for sequence numbers (1..total), requesting UID attribute
        let cmd = format!("{tag} FETCH {start}:{end} (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])");
        self.conn.write_line(&cmd)?;

        let mut messages = Vec::new();

        loop {
            let line = self.conn.read_line()?;
            if line.starts_with(&tag) {
                if !line.contains(" OK") && messages.is_empty() {
                    return Err(format!("IMAP FETCH Fehler: {line}"));
                }
                break;
            }

            if line.starts_with("* ") && line.contains("FETCH") {
                let is_unread = !line.contains("\\Seen");
                let is_flagged = line.contains("\\Flagged");

                // Extract sequence number from `* <seq> FETCH`
                let seq_str = line
                    .strip_prefix("* ")
                    .and_then(|s| s.split_whitespace().next())
                    .unwrap_or("0");

                // Extract UID attribute from `* <seq> FETCH (UID <uid> ...)`
                let mut uid_str = extract_imap_fetch_uid(&line).unwrap_or_else(|| {
                    if seq_str != "0" && seq_str.chars().all(|c| c.is_ascii_digit()) {
                        seq_str.to_string()
                    } else {
                        "0".to_string()
                    }
                });

                // Check for literal header data: {len}
                let mut header_bytes = Vec::new();
                if let Some(idx) = line.rfind('{') {
                    if let Some(end) = line[idx..].find('}') {
                        if let Ok(size) = line[idx + 1..idx + end].parse::<usize>() {
                            header_bytes = self.conn.read_exact(size)?;
                            // Consume trailing \r\n and check for UID if not yet found
                            let trailing = self.conn.read_line().unwrap_or_default();
                            if uid_str == "0" {
                                if let Some(trailing_uid) = extract_imap_fetch_uid(&trailing) {
                                    uid_str = trailing_uid;
                                }
                            }
                        }
                    }
                }

                let folder_id = format!("{}:{}", account_uid, folder_name);
                if let Some(mut parsed) = crate::services::mail_store_service::parse_rfc822_bytes(
                    &uid_str,
                    &folder_id,
                    &header_bytes,
                    is_unread,
                ) {
                    parsed.is_flagged = is_flagged;
                    parsed.folder_name = folder_name.to_string();
                    parsed.folder_id = folder_id;
                    parsed.backend_location = Some(MailBackendLocation {
                        source_uid: account_uid.to_string(),
                        folder_name: folder_name.to_string(),
                        message_uid: uid_str,
                        message_id: parsed.message_id.clone(),
                    });
                    messages.push(parsed);
                }
            }
        }

        Ok(messages)
    }

    pub fn fetch_raw_message(
        &mut self,
        folder_name: &str,
        message_uid: &str,
    ) -> Result<Vec<u8>, String> {
        self.select_folder(folder_name)?;
        let tag = self.next_tag();
        let cmd = format!("{tag} UID FETCH {message_uid} (BODY.PEEK[])");
        self.conn.write_line(&cmd)?;

        let mut raw_bytes = Vec::new();
        loop {
            let line = self.conn.read_line()?;
            if line.starts_with(&tag) {
                break;
            }

            if line.contains('{') && line.contains('}') {
                if let Some(start) = line.rfind('{') {
                    if let Some(end) = line[start..].find('}') {
                        if let Ok(len) = line[start + 1..start + end].parse::<usize>() {
                            raw_bytes = self.conn.read_exact(len)?;
                            let _ = self.conn.read_line();
                        }
                    }
                }
            }
        }

        if raw_bytes.is_empty() {
            Err(format!(
                "Nachricht UID {message_uid} in Ordner {folder_name} nicht gefunden"
            ))
        } else {
            Ok(raw_bytes)
        }
    }

    pub fn set_flags(
        &mut self,
        folder_name: &str,
        message_uid: &str,
        plus_flags: &[&str],
        minus_flags: &[&str],
    ) -> Result<(), String> {
        self.select_folder(folder_name)?;
        if !plus_flags.is_empty() {
            let flags_str = plus_flags.join(" ");
            self.send_command(&format!("UID STORE {message_uid} +FLAGS ({flags_str})"))?;
        }
        if !minus_flags.is_empty() {
            let flags_str = minus_flags.join(" ");
            self.send_command(&format!("UID STORE {message_uid} -FLAGS ({flags_str})"))?;
        }
        Ok(())
    }

    pub fn copy_message(
        &mut self,
        folder_name: &str,
        message_uid: &str,
        target_folder: &str,
    ) -> Result<(), String> {
        self.select_folder(folder_name)?;
        self.send_command(&format!("UID COPY {message_uid} \"{target_folder}\""))?;
        Ok(())
    }

    pub fn create_folder(&mut self, folder_name: &str) -> Result<(), String> {
        self.send_command(&format!("CREATE \"{folder_name}\""))?;
        Ok(())
    }

    pub fn rename_folder(&mut self, old_name: &str, new_name: &str) -> Result<(), String> {
        self.send_command(&format!("RENAME \"{old_name}\" \"{new_name}\""))?;
        Ok(())
    }

    pub fn delete_folder(&mut self, folder_name: &str) -> Result<(), String> {
        self.send_command(&format!("DELETE \"{folder_name}\""))?;
        Ok(())
    }

    pub fn set_subscription(&mut self, folder_name: &str, subscribe: bool) -> Result<(), String> {
        let op = if subscribe { "SUBSCRIBE" } else { "UNSUBSCRIBE" };
        self.send_command(&format!("{op} \"{folder_name}\""))?;
        Ok(())
    }

    pub fn logout(&mut self) -> Result<(), String> {
        let _ = self.send_command("LOGOUT");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_list_line_unquoted_folder_quoted_delimiter() {
        let line = r#"* LIST (\HasNoChildren) "/" INBOX"#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse");
        assert_eq!(item.name, "INBOX");
        assert_eq!(item.delimiter, '/');
        assert!(item.is_inbox);
        assert!(!item.is_sent);
    }

    #[test]
    fn test_parse_list_line_unquoted_folder_dot_delimiter() {
        let line = r#"* LIST () "." Trash"#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse");
        assert_eq!(item.name, "Trash");
        assert_eq!(item.delimiter, '.');
        assert!(item.is_trash);
        assert!(!item.is_inbox);
    }

    #[test]
    fn test_parse_list_line_quoted_folder_with_spaces() {
        let line = r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse");
        assert_eq!(item.name, "Sent Messages");
        assert_eq!(item.delimiter, '/');
        assert!(item.is_sent);
    }

    #[test]
    fn test_parse_list_line_nil_delimiter_noselect() {
        let line = r#"* LIST (\Noselect) NIL "[Gmail]""#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse");
        assert_eq!(item.name, "[Gmail]");
        assert_eq!(item.delimiter, '/');
        assert!(item.is_noselect);
    }

    #[test]
    fn test_parse_list_line_nil_delimiter_unquoted() {
        let line = r#"* LIST () NIL INBOX"#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse");
        assert_eq!(item.name, "INBOX");
        assert_eq!(item.delimiter, '/');
        assert!(item.is_inbox);
    }

    #[test]
    fn test_parse_list_line_escaped_quotes_and_backslash() {
        let line = r#"* LIST () "/" "Folder \"Quoted\" \\ Backslash""#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse");
        assert_eq!(item.name, "Folder \"Quoted\" \\ Backslash");
        assert_eq!(item.delimiter, '/');
    }

    #[test]
    fn test_parse_list_line_empty_folder_probe() {
        let line = r#"* LIST () "/" """#;
        let item = ImapFolderItem::parse_list_line(line).expect("Must parse reference probe");
        assert_eq!(item.name, "");
        assert_eq!(item.delimiter, '/');
    }

    #[test]
    fn test_parse_list_line_truncated_and_malformed() {
        assert!(ImapFolderItem::parse_list_line("").is_none());
        assert!(ImapFolderItem::parse_list_line("NOT A LIST").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST ()").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST (\\HasNoChildren)").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST () \"/\"").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST () NIL").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST (unclosed").is_none());
        assert!(ImapFolderItem::parse_list_line("* LIST () \"unclosed").is_none());
    }

    #[test]
    fn test_fetch_envelopes_uses_sequence_fetch_not_uid_fetch() {
        let canned = vec![
            "* 10 EXISTS".into(),
            "* 0 RECENT".into(),
            "A0001 OK [READ-WRITE] SELECT completed".into(),
            "* 9 FETCH (UID 5009 FLAGS (\\Seen) RFC822.SIZE 65 BODY[HEADER] {65}".into(),
            "From: user1@example.com\r\nTo: dest@example.com\r\nSubject: Test 1\r\n\r\n".into(),
            ")".into(),
            "* 10 FETCH (UID 5010 FLAGS (\\Flagged) RFC822.SIZE 65 BODY[HEADER] {65}".into(),
            "From: user2@example.com\r\nTo: dest@example.com\r\nSubject: Test 2\r\n\r\n".into(),
            ")".into(),
            "A0002 OK FETCH completed".into(),
        ];

        let conn = MailConnection::new_mock(canned);
        let mut client = ImapClient::new(conn);

        let messages = client
            .fetch_envelopes("INBOX", 0, 2, "acc-test")
            .expect("Fetch envelopes must succeed");

        assert_eq!(messages.len(), 2);

        // Verify UIDs are correctly extracted from UID attribute
        assert_eq!(messages[0].id, "5009");
        assert_eq!(
            messages[0].backend_location.as_ref().unwrap().message_uid,
            "5009"
        );
        assert!(!messages[0].is_unread);
        assert!(!messages[0].is_flagged);

        assert_eq!(messages[1].id, "5010");
        assert_eq!(
            messages[1].backend_location.as_ref().unwrap().message_uid,
            "5010"
        );
        assert!(messages[1].is_unread);
        assert!(messages[1].is_flagged);

        let written = client.conn.mock_written();
        assert_eq!(written.len(), 2);
        assert_eq!(written[0], "A0001 SELECT \"INBOX\"\r\n");

        // CRITICAL RFC 3501 assertion: Must be FETCH, NOT UID FETCH
        assert!(
            written[1].contains("FETCH 9:10 (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])"),
            "Must use FETCH with sequence numbers: {}",
            written[1]
        );
        assert!(
            !written[1].contains("UID FETCH"),
            "Must not send UID FETCH with sequence range: {}",
            written[1]
        );
    }

    #[test]
    fn test_fetch_envelopes_empty_mailbox() {
        let canned = vec![
            "* 0 EXISTS".into(),
            "A0001 OK [READ-WRITE] SELECT completed".into(),
        ];
        let conn = MailConnection::new_mock(canned);
        let mut client = ImapClient::new(conn);

        let messages = client
            .fetch_envelopes("INBOX", 0, 10, "acc-test")
            .expect("Empty mailbox fetch succeeds");
        assert!(messages.is_empty());

        let written = client.conn.mock_written();
        assert_eq!(written.len(), 1);
    }

    #[test]
    fn test_login_escapes_quotes_and_backslashes() {
        let canned = vec!["A0001 OK LOGIN completed".into()];
        let conn = MailConnection::new_mock(canned);
        let mut client = ImapClient::new(conn);

        client
            .login(r#"user"name"#, r#"pass\word"1"#)
            .expect("Login must succeed");

        let written = client.conn.mock_written();
        assert_eq!(written.len(), 1);
        assert_eq!(
            written[0],
            "A0001 LOGIN \"user\\\"name\" \"pass\\\\word\\\"1\"\r\n"
        );
    }

    #[test]
    fn test_list_folders_mock() {
        let canned = vec![
            r#"* LIST (\HasNoChildren) "/" INBOX"#.into(),
            r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#.into(),
            r#"* LIST () "." Trash"#.into(),
            "A0001 OK LIST completed".into(),
        ];
        let conn = MailConnection::new_mock(canned);
        let mut client = ImapClient::new(conn);

        let folders = client.list_folders().expect("List folders succeeds");
        assert_eq!(folders.len(), 3);
        assert_eq!(folders[0].name, "INBOX");
        assert!(folders[0].is_inbox);
        assert_eq!(folders[1].name, "Sent Messages");
        assert!(folders[1].is_sent);
        assert_eq!(folders[2].name, "Trash");
        assert!(folders[2].is_trash);
    }
}
