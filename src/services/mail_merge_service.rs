// SPDX-License-Identifier: GPL-3.0-or-later
//
// Mail Merge service — pure-Rust recipient list parsing (CSV, TSV, freeform),
// placeholder discovery (`{{name}}`, `{{first_name}}`, `{{email}}`, custom columns),
// and safe per-recipient subject/HTML/plain-text expansion.

use std::collections::BTreeMap;

/// One recipient row in a Mail Merge campaign.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MailMergeRecipient {
    pub name: String,
    pub email: String,
    /// Optional additional columns (e.g. `company`, `salutation`, `invoice_id`)
    /// imported from CSV/TSV or entered manually.
    pub custom_fields: BTreeMap<String, String>,
}

/// Summary returned when importing recipients from CSV/TSV/text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MailMergeImportResult {
    pub recipients: Vec<MailMergeRecipient>,
    pub custom_columns: Vec<String>,
    pub skipped_rows: usize,
}

impl MailMergeRecipient {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into().trim().to_string(),
            email: email.into().trim().to_string(),
            custom_fields: BTreeMap::new(),
        }
    }

    pub fn is_valid(&self) -> bool {
        let email = self.email.trim();
        !email.is_empty() && email.contains('@') && !email.chars().any(char::is_whitespace)
    }

    /// Parse a single RFC822-like address such as `"Ada Lovelace" <ada@example.org>` or `ada@example.org`.
    pub fn from_rfc822_address(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (name, email) = if let (Some(lt), Some(gt)) = (trimmed.rfind('<'), trimmed.rfind('>')) {
            if lt < gt {
                let n = trimmed[..lt].trim().trim_matches('"').trim().to_string();
                let addr = trimmed[lt + 1..gt].trim().to_string();
                (n, addr)
            } else {
                (String::new(), trimmed.to_string())
            }
        } else {
            (String::new(), trimmed.to_string())
        };
        let recipient = Self::new(name, email);
        recipient.is_valid().then_some(recipient)
    }

    /// Format as an RFC822 `To:` header value.
    pub fn formatted_to_header(&self) -> String {
        let email = self.email.trim();
        let name = self.name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case(email) {
            email.to_string()
        } else if name.contains([',', ';', '"', '<', '>']) {
            let escaped = name.replace('"', "\\\"");
            format!("\"{escaped}\" <{email}>")
        } else {
            format!("{name} <{email}>")
        }
    }

    /// Effective display name, deriving a friendly fallback from the email local part when blank.
    pub fn effective_name(&self) -> String {
        let trimmed = self.name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        let local = self.email.split('@').next().unwrap_or(&self.email);
        local
            .replace(['.', '_', '-'], " ")
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn first_name(&self) -> String {
        if let Some(v) = self.lookup_custom(&["first_name", "firstname", "vorname"]) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
        let effective = self.effective_name();
        effective
            .split_whitespace()
            .next()
            .unwrap_or(&effective)
            .to_string()
    }

    pub fn last_name(&self) -> String {
        if let Some(v) = self.lookup_custom(&["last_name", "lastname", "nachname", "surname"]) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
        let effective = self.effective_name();
        let mut parts = effective.split_whitespace();
        let _first = parts.next();
        let rest: Vec<&str> = parts.collect();
        rest.join(" ")
    }

    fn lookup_custom(&self, aliases: &[&str]) -> Option<&str> {
        for (k, v) in &self.custom_fields {
            let norm = normalize_key(k);
            if aliases.iter().any(|alias| norm == *alias) {
                return Some(v.trim());
            }
        }
        None
    }

    /// Resolve a placeholder key (case-insensitive) for this recipient.
    pub fn resolve_placeholder(&self, key: &str, subject_context: &str) -> Option<String> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return None;
        }
        let norm = normalize_key(trimmed);

        // Exact or normalized custom field wins if explicitly supplied by the user/CSV
        for (k, v) in &self.custom_fields {
            if k.eq_ignore_ascii_case(trimmed) || normalize_key(k) == norm {
                return Some(v.clone());
            }
        }

        match norm.as_str() {
            "email" | "e_mail" | "mail" | "to_email" | "email_address" => {
                Some(self.email.trim().to_string())
            }
            "name" | "full_name" | "fullname" | "to_name" | "recipient" => {
                Some(self.effective_name())
            }
            "first_name" | "firstname" | "vorname" | "given_name" => Some(self.first_name()),
            "last_name" | "lastname" | "nachname" | "surname" | "family_name" => {
                Some(self.last_name())
            }
            "date" | "datum" => Some(chrono::Local::now().format("%Y-%m-%d").to_string()),
            "time" | "uhrzeit" => Some(chrono::Local::now().format("%H:%M").to_string()),
            "subject" | "betreff" => Some(subject_context.to_string()),
            _ => None,
        }
    }
}

fn normalize_key(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

/// Extract all unique `{{placeholder}}` names across one or more texts in order of appearance.
pub fn extract_placeholders(texts: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let mut seen_norm = std::collections::HashSet::new();
    for text in texts {
        let mut cursor = *text;
        while let Some(open) = cursor.find("{{") {
            let after_open = &cursor[open + 2..];
            let Some(close) = after_open.find("}}") else {
                break;
            };
            let candidate = after_open[..close].trim();
            if !candidate.is_empty()
                && candidate.len() <= 64
                && !candidate.contains(['<', '>', '\n', '\r', '{', '}'])
            {
                let norm = normalize_key(candidate);
                if seen_norm.insert(norm) {
                    found.push(candidate.to_string());
                }
            }
            cursor = &after_open[close + 2..];
        }
    }
    found
}

/// Returns true if either subject or body contains at least one valid `{{placeholder}}`.
pub fn has_placeholders(subject: &str, body: &str) -> bool {
    !extract_placeholders(&[subject, body]).is_empty()
}

/// Render a plain-text (or subject) template for one recipient.
/// Unknown placeholders remain intact so nothing is silently erased.
pub fn render_template_for_recipient(
    template: &str,
    recipient: &MailMergeRecipient,
    subject_context: &str,
) -> String {
    render_template_internal(template, recipient, subject_context, false)
}

/// Render an HTML body template for one recipient, escaping substituted values
/// so special characters (`<`, `>`, `&`) in recipient data cannot break HTML markup.
pub fn render_html_template_for_recipient(
    html_template: &str,
    recipient: &MailMergeRecipient,
    subject_context: &str,
) -> String {
    render_template_internal(html_template, recipient, subject_context, true)
}

fn html_escape_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_template_internal(
    template: &str,
    recipient: &MailMergeRecipient,
    subject_context: &str,
    escape_html: bool,
) -> String {
    if !template.contains("{{") {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut cursor = template;
    while let Some(open) = cursor.find("{{") {
        out.push_str(&cursor[..open]);
        let after_open = &cursor[open + 2..];
        let Some(close) = after_open.find("}}") else {
            out.push_str(&cursor[open..]);
            return out;
        };
        let key = after_open[..close].trim();
        if let Some(value) = recipient.resolve_placeholder(key, subject_context) {
            if escape_html {
                out.push_str(&html_escape_value(&value));
            } else {
                out.push_str(&value);
            }
        } else {
            out.push_str(&cursor[open..open + 2 + close + 2]);
        }
        cursor = &after_open[close + 2..];
    }
    out.push_str(cursor);
    out
}

/// Parse a `To:` header string (comma- or semicolon-separated, respecting quotes)
/// into `MailMergeRecipient` entries.
pub fn parse_recipients_from_to_header(to_header: &str) -> Vec<MailMergeRecipient> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in to_header.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' | ';' | '\n' if !in_quotes => {
                let t = current.trim();
                if !t.is_empty() {
                    tokens.push(t.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        tokens.push(tail.to_string());
    }
    tokens
        .into_iter()
        .filter_map(|t| MailMergeRecipient::from_rfc822_address(&t))
        .collect()
}

/// Parse a CSV, TSV, or freeform recipient list (one address per line or tabular rows).
/// Automatically detects `,`, `;` (common in European spreadsheets), or `\t` delimiters,
/// handles quoted RFC 4180 fields, maps standard name/email columns, and preserves any
/// extra columns as custom `{{column}}` placeholders.
pub fn parse_recipients_csv(input: &str) -> Result<MailMergeImportResult, String> {
    let cleaned = input.trim_start_matches('\u{feff}');
    let non_empty_lines: Vec<&str> = cleaned
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if non_empty_lines.is_empty() {
        return Err("Recipient list is empty".to_string());
    }

    let delimiter = detect_delimiter(non_empty_lines[0]);
    let parsed_rows: Vec<Vec<String>> = non_empty_lines
        .iter()
        .map(|line| parse_delimited_line(line, delimiter))
        .collect();

    let first_row = &parsed_rows[0];
    let has_header = row_looks_like_header(first_row);

    let mut result = MailMergeImportResult::default();
    if has_header {
        let headers: Vec<String> = first_row
            .iter()
            .map(|h| h.trim().trim_matches('"').trim().to_string())
            .collect();
        let email_col = headers.iter().position(|h| {
            matches!(
                normalize_key(h).as_str(),
                "email" | "e_mail" | "mail" | "email_address" | "to_email"
            )
        });
        let name_col = headers.iter().position(|h| {
            matches!(
                normalize_key(h).as_str(),
                "name" | "full_name" | "fullname" | "to_name" | "recipient"
            )
        });
        let first_name_col = headers.iter().position(|h| {
            matches!(
                normalize_key(h).as_str(),
                "first_name" | "firstname" | "vorname" | "given_name"
            )
        });
        let last_name_col = headers.iter().position(|h| {
            matches!(
                normalize_key(h).as_str(),
                "last_name" | "lastname" | "nachname" | "surname" | "family_name"
            )
        });

        for (idx, h) in headers.iter().enumerate() {
            if !h.is_empty() && Some(idx) != email_col && Some(idx) != name_col {
                result.custom_columns.push(h.clone());
            }
        }

        for row in parsed_rows.iter().skip(1) {
            let email = email_col
                .and_then(|idx| row.get(idx))
                .map(|s| s.trim().to_string())
                .or_else(|| {
                    row.iter()
                        .find(|cell| cell.contains('@'))
                        .map(|s| s.trim().to_string())
                })
                .unwrap_or_default();

            let mut name = name_col
                .and_then(|idx| row.get(idx))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                let first = first_name_col
                    .and_then(|idx| row.get(idx))
                    .map(|s| s.trim())
                    .unwrap_or("");
                let last = last_name_col
                    .and_then(|idx| row.get(idx))
                    .map(|s| s.trim())
                    .unwrap_or("");
                name = [first, last]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
            }

            let mut recipient =
                if let Some(mut parsed) = MailMergeRecipient::from_rfc822_address(&email) {
                    if !name.is_empty() {
                        parsed.name = name;
                    }
                    parsed
                } else {
                    MailMergeRecipient::new(name, email)
                };

            if !recipient.is_valid() {
                result.skipped_rows += 1;
                continue;
            }

            for (idx, header) in headers.iter().enumerate() {
                if header.is_empty() || Some(idx) == email_col || Some(idx) == name_col {
                    continue;
                }
                let val = row
                    .get(idx)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                recipient.custom_fields.insert(header.clone(), val);
            }
            result.recipients.push(recipient);
        }
    } else {
        for row in &parsed_rows {
            if row.len() == 1 {
                if let Some(rcpt) = MailMergeRecipient::from_rfc822_address(&row[0]) {
                    result.recipients.push(rcpt);
                } else {
                    result.skipped_rows += 1;
                }
                continue;
            }
            // Headerless multi-column row: find the cell with '@' as email, join earlier/other cell as name.
            let Some(email_idx) = row.iter().position(|cell| cell.contains('@')) else {
                result.skipped_rows += 1;
                continue;
            };
            let email = row[email_idx].trim();
            let name = row
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != email_idx)
                .map(|(_, v)| v.trim())
                .find(|v| !v.is_empty())
                .unwrap_or("");
            let recipient = if let Some(mut parsed) = MailMergeRecipient::from_rfc822_address(email)
            {
                if !name.is_empty() {
                    parsed.name = name.to_string();
                }
                parsed
            } else {
                MailMergeRecipient::new(name, email)
            };
            if recipient.is_valid() {
                result.recipients.push(recipient);
            } else {
                result.skipped_rows += 1;
            }
        }
    }

    if result.recipients.is_empty() {
        return Err("No valid email addresses found in the recipient list".to_string());
    }
    Ok(result)
}

fn detect_delimiter(first_line: &str) -> char {
    let mut tabs = 0usize;
    let mut semis = 0usize;
    let mut commas = 0usize;
    let mut in_quotes = false;
    for ch in first_line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '\t' if !in_quotes => tabs += 1,
            ';' if !in_quotes => semis += 1,
            ',' if !in_quotes => commas += 1,
            _ => {}
        }
    }
    if tabs > 0 && tabs >= semis && tabs >= commas {
        '\t'
    } else if semis > 0 && semis >= commas {
        ';'
    } else {
        ','
    }
}

fn parse_delimited_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        if ch == '"' {
            if in_quotes && chars.peek() == Some(&'"') {
                chars.next();
                current.push('"');
            } else {
                in_quotes = !in_quotes;
            }
        } else if ch == delimiter && !in_quotes {
            fields.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn row_looks_like_header(row: &[String]) -> bool {
    if row.iter().any(|cell| cell.contains('@')) {
        return false;
    }
    row.iter().any(|cell| {
        matches!(
            normalize_key(cell).as_str(),
            "email"
                | "e_mail"
                | "mail"
                | "email_address"
                | "to_email"
                | "name"
                | "full_name"
                | "fullname"
                | "first_name"
                | "firstname"
                | "vorname"
                | "last_name"
                | "lastname"
                | "nachname"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_german_semicolon_csv_with_custom_columns() {
        let csv = "\u{feff}Vorname;Nachname;E-Mail;Firma\n\
                   Max;Mustermann;max@example.org;Acme GmbH\n\
                   Erika;Musterfrau;erika@example.org;\"Beta, AG\"\n\
                   Invalid;;not-an-email;Skip\n";
        let res = parse_recipients_csv(csv).expect("CSV should parse");
        assert_eq!(res.recipients.len(), 2);
        assert_eq!(res.skipped_rows, 1);
        assert_eq!(res.recipients[0].name, "Max Mustermann");
        assert_eq!(res.recipients[0].email, "max@example.org");
        assert_eq!(res.recipients[0].first_name(), "Max");
        assert_eq!(res.recipients[0].last_name(), "Mustermann");
        assert_eq!(
            res.recipients[0].resolve_placeholder("Firma", ""),
            Some("Acme GmbH".to_string())
        );
        assert_eq!(
            res.recipients[1].resolve_placeholder("firma", ""),
            Some("Beta, AG".to_string())
        );
    }

    #[test]
    fn parses_headerless_and_tsv_recipient_lists() {
        let tsv = "Ada Lovelace\tada@example.org\nGrace Hopper\tgrace@example.org\n";
        let res = parse_recipients_csv(tsv).expect("TSV should parse");
        assert_eq!(res.recipients.len(), 2);
        assert_eq!(res.recipients[0].name, "Ada Lovelace");
        assert_eq!(res.recipients[0].email, "ada@example.org");

        let lines = "Alice <alice@example.org>\nbob@example.org\n";
        let res2 = parse_recipients_csv(lines).expect("Plain lines should parse");
        assert_eq!(res2.recipients.len(), 2);
        assert_eq!(res2.recipients[0].name, "Alice");
        assert_eq!(res2.recipients[1].email, "bob@example.org");
    }

    #[test]
    fn extracts_and_renders_placeholders_with_html_escaping() {
        let subject = "Hello {{first_name}} from {{Company}}";
        let html = "<p>Dear {{name}},</p><p>Your email is {{email}} ({{Company}}).</p>";
        let keys = extract_placeholders(&[subject, html]);
        assert_eq!(keys, vec!["first_name", "Company", "name", "email"]);

        let mut rcpt = MailMergeRecipient::new("Ada Lovelace", "ada@example.org");
        rcpt.custom_fields
            .insert("Company".to_string(), "A & B <Labs>".to_string());

        let rendered_subject = render_template_for_recipient(subject, &rcpt, "");
        assert_eq!(rendered_subject, "Hello Ada from A & B <Labs>");

        let rendered_html = render_html_template_for_recipient(html, &rcpt, &rendered_subject);
        assert_eq!(
            rendered_html,
            "<p>Dear Ada Lovelace,</p><p>Your email is ada@example.org (A &amp; B &lt;Labs&gt;).</p>"
        );
    }
}
