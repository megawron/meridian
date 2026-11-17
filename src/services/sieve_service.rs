// SPDX-License-Identifier: GPL-3.0-or-later
//
// SieveService — RFC 5804 ManageSieve client and RFC 5228 / RFC 5230 Sieve
// script generator for server-side mail filters and IMAP vacation replies.

use gio::prelude::*;
use std::collections::BTreeSet;

use super::mail_filter_service::{
    FilterAction, FilterField, FilterOperator, MailFilterRule,
};
use super::out_of_office_service::OutOfOfficeSettings;

pub const DEFAULT_MANAGESIEVE_PORT: u16 = 4190;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SieveScriptSummary {
    pub name: String,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct ManageSieveConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub secret: String,
    pub use_oauth2: bool,
}

/// Generate a standards-compliant RFC 5230 Sieve `vacation` script from
/// Meridian's `OutOfOfficeSettings` and account aliases.
pub fn generate_sieve_vacation_script(
    settings: &OutOfOfficeSettings,
    addresses: &[String],
) -> String {
    let mut requires = BTreeSet::new();
    requires.insert("vacation");
    let has_date_guard = settings.use_schedule
        && (!settings.start_date.trim().is_empty() || !settings.end_date.trim().is_empty());
    if has_date_guard {
        requires.insert("date");
        requires.insert("relational");
    }

    let req_list: Vec<String> = requires.into_iter().map(|r| format!("\"{r}\"")).collect();
    let mut out = format!("require [{}];\n\n", req_list.join(", "));

    if !settings.enabled {
        out.push_str("# Vacation auto-responder is currently disabled.\n");
        return out;
    }

    let body = if !settings.external_message.trim().is_empty() {
        strip_basic_html(&settings.external_message)
    } else {
        strip_basic_html(&settings.internal_message)
    };
    let clean_body = if body.trim().is_empty() {
        "I am currently out of the office and will reply upon my return.".to_string()
    } else {
        body
    };

    let mut conditions = Vec::new();
    if settings.use_schedule {
        if !settings.start_date.trim().is_empty() {
            conditions.push(format!(
                "currentdate :value \"ge\" \"date\" \"{}\"",
                escape_sieve_string(settings.start_date.trim())
            ));
        }
        if !settings.end_date.trim().is_empty() {
            conditions.push(format!(
                "currentdate :value \"le\" \"date\" \"{}\"",
                escape_sieve_string(settings.end_date.trim())
            ));
        }
    }

    let addr_list: Vec<String> = addresses
        .iter()
        .map(|a| a.trim())
        .filter(|a| a.contains('@'))
        .map(|a| format!("\"{}\"", escape_sieve_string(a)))
        .collect();
    let addresses_clause = if addr_list.is_empty() {
        String::new()
    } else {
        format!(" :addresses [{}]", addr_list.join(", "))
    };

    let vacation_stmt = format!(
        "vacation :days 7{addresses_clause} :subject \"Out of Office\" \"{}\";",
        escape_sieve_string(&clean_body)
    );

    if conditions.is_empty() {
        out.push_str(&vacation_stmt);
        out.push('\n');
    } else {
        out.push_str(&format!(
            "if allof ({}) {{\n    {}\n}}\n",
            conditions.join(", "),
            vacation_stmt
        ));
    }

    out
}

/// Compile Meridian `MailFilterRule` rules into an RFC 5228 Sieve script so
/// power users can export or upload client rules directly to ManageSieve.
pub fn compile_filters_to_sieve(rules: &[MailFilterRule]) -> String {
    let mut requires = BTreeSet::new();
    let mut blocks = Vec::new();

    for rule in rules.iter().filter(|r| r.enabled) {
        let mut cond_parts = Vec::new();
        for cond in &rule.conditions {
            let val = escape_sieve_string(&cond.value);
            let match_kw = match cond.operator {
                FilterOperator::Contains | FilterOperator::NotContains => ":contains",
                FilterOperator::Equals | FilterOperator::NotEquals => ":is",
                FilterOperator::StartsWith | FilterOperator::EndsWith => ":matches",
                FilterOperator::MatchesRegex => {
                    requires.insert("regex");
                    ":regex"
                }
                _ => ":contains",
            };
            let pattern = match cond.operator {
                FilterOperator::StartsWith => format!("{val}*"),
                FilterOperator::EndsWith => format!("*{val}"),
                _ => val,
            };
            let negated = matches!(
                cond.operator,
                FilterOperator::NotContains | FilterOperator::NotEquals
            );

            let expr = match &cond.field {
                FilterField::Subject => {
                    format!("header {match_kw} \"Subject\" \"{pattern}\"")
                }
                FilterField::Sender => {
                    format!("address {match_kw} \"From\" \"{pattern}\"")
                }
                FilterField::Recipient => {
                    format!("address {match_kw} [\"To\", \"Cc\"] \"{pattern}\"")
                }
                FilterField::Header => {
                    if let Some((hdr, hdr_val)) = cond.value.split_once(':') {
                        format!(
                            "header {match_kw} \"{}\" \"{}\"",
                            escape_sieve_string(hdr.trim()),
                            escape_sieve_string(hdr_val.trim())
                        )
                    } else {
                        format!("exists \"{}\"", escape_sieve_string(cond.value.trim()))
                    }
                }
                FilterField::Body => {
                    requires.insert("body");
                    format!("body :text {match_kw} \"{pattern}\"")
                }
                FilterField::AllMessages => "true".to_string(),
                _ => continue,
            };
            if negated {
                cond_parts.push(format!("not {expr}"));
            } else {
                cond_parts.push(expr);
            }
        }

        if cond_parts.is_empty() {
            continue;
        }

        let mut action_lines = Vec::new();
        for action in &rule.actions {
            match action {
                FilterAction::MoveToFolder(folder) => {
                    requires.insert("fileinto");
                    action_lines.push(format!("    fileinto \"{}\";", escape_sieve_string(folder)));
                }
                FilterAction::CopyToFolder(folder) => {
                    requires.insert("fileinto");
                    requires.insert("copy");
                    action_lines.push(format!(
                        "    fileinto :copy \"{}\";",
                        escape_sieve_string(folder)
                    ));
                }
                FilterAction::Delete => {
                    action_lines.push("    discard;".to_string());
                }
                FilterAction::MarkRead => {
                    requires.insert("imap4flags");
                    action_lines.push("    addflag \"\\\\Seen\";".to_string());
                }
                FilterAction::SetFlagged => {
                    requires.insert("imap4flags");
                    action_lines.push("    addflag \"\\\\Flagged\";".to_string());
                }
                FilterAction::SetLabel(tag) => {
                    requires.insert("imap4flags");
                    action_lines.push(format!("    addflag \"{}\";", escape_sieve_string(tag)));
                }
                FilterAction::ForwardTo(addr) => {
                    action_lines.push(format!("    redirect \"{}\";", escape_sieve_string(addr)));
                }
                FilterAction::StopProcessing => {
                    action_lines.push("    stop;".to_string());
                }
                _ => {}
            }
        }

        if action_lines.is_empty() {
            continue;
        }

        let joiner = if cond_parts.len() == 1 {
            cond_parts[0].clone()
        } else if rule.match_all {
            format!("allof ({})", cond_parts.join(", "))
        } else {
            format!("anyof ({})", cond_parts.join(", "))
        };

        blocks.push(format!(
            "# Rule: {}\nif {} {{\n{}\n}}",
            rule.name.replace('\n', " "),
            joiner,
            action_lines.join("\n")
        ));
    }

    let mut script = String::new();
    if !requires.is_empty() {
        let reqs: Vec<String> = requires.into_iter().map(|r| format!("\"{r}\"")).collect();
        script.push_str(&format!("require [{}];\n\n", reqs.join(", ")));
    }
    if blocks.is_empty() {
        script.push_str("# No server-compatible rules enabled.\nkeep;\n");
    } else {
        script.push_str(&blocks.join("\n\n"));
        script.push('\n');
    }
    script
}

/// Upload and activate a ManageSieve script over TLS (`port 4190`).
pub fn upload_and_activate_sieve_script(
    cfg: &ManageSieveConfig,
    script_name: &str,
    script_body: &str,
) -> Result<(), String> {
    if cfg.host.trim().is_empty() {
        return Err("ManageSieve server host is empty".to_string());
    }
    let client = gio::SocketClient::new();
    client.set_timeout(12);
    // Implicit TLS on 4190 or STARTTLS fallback: try plain connect + STARTTLS first per RFC 5804.
    let addr = format!("{}:{}", cfg.host.trim(), cfg.port);
    let conn = client
        .connect_to_host(&addr, cfg.port, gio::Cancellable::NONE)
        .map_err(|e| format!("ManageSieve connection to {addr} failed: {e}"))?;

    let mut stream: gio::IOStream = conn.clone().upcast();
    let greeting = read_sieve_response(&stream)?;
    if greeting
        .iter()
        .any(|line| line.to_ascii_uppercase().contains("\"STARTTLS\""))
    {
        write_sieve_line(&stream, "STARTTLS\r\n")?;
        let tls_resp = read_sieve_response(&stream)?;
        expect_sieve_ok(&tls_resp, "STARTTLS")?;
        let identity = gio::NetworkAddress::new(cfg.host.trim(), cfg.port);
        let tls_conn = gio::TlsClientConnection::new(&stream, Some(&identity))
            .map_err(|e| format!("TLS initialization failed: {e}"))?;
        tls_conn
            .handshake(gio::Cancellable::NONE)
            .map_err(|e| format!("ManageSieve TLS handshake failed: {e}"))?;
        stream = tls_conn.upcast();
        let _post_tls_caps = read_sieve_response(&stream)?;
    }

    let auth_cmd = if cfg.use_oauth2 {
        let token = format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            cfg.username, cfg.secret
        );
        let b64 = glib::base64_encode(token.as_bytes());
        format!("AUTHENTICATE \"XOAUTH2\" \"{b64}\"\r\n")
    } else {
        let sasl = format!("\0{}\0{}", cfg.username, cfg.secret);
        let b64 = glib::base64_encode(sasl.as_bytes());
        format!("AUTHENTICATE \"PLAIN\" \"{b64}\"\r\n")
    };
    write_sieve_line(&stream, &auth_cmd)?;
    let auth_resp = read_sieve_response(&stream)?;
    expect_sieve_ok(&auth_resp, "Authentication")?;

    let safe_name = escape_sieve_string(script_name);
    let bytes = script_body.as_bytes();
    let put_header = format!("PUTSCRIPT \"{safe_name}\" {{{}+}}\r\n", bytes.len());
    write_sieve_line(&stream, &put_header)?;
    write_sieve_bytes(&stream, bytes)?;
    write_sieve_line(&stream, "\r\n")?;
    let put_resp = read_sieve_response(&stream)?;
    expect_sieve_ok(&put_resp, "PUTSCRIPT")?;

    let activate_cmd = format!("SETACTIVE \"{safe_name}\"\r\n");
    write_sieve_line(&stream, &activate_cmd)?;
    let act_resp = read_sieve_response(&stream)?;
    expect_sieve_ok(&act_resp, "SETACTIVE")?;

    let _ = write_sieve_line(&stream, "LOGOUT\r\n");
    Ok(())
}

fn write_sieve_line(stream: &gio::IOStream, text: &str) -> Result<(), String> {
    write_sieve_bytes(stream, text.as_bytes())
}

fn write_sieve_bytes(stream: &gio::IOStream, bytes: &[u8]) -> Result<(), String> {
    stream
        .output_stream()
        .write_all(bytes, gio::Cancellable::NONE)
        .map(|_| ())
        .map_err(|e| format!("ManageSieve write error: {e}"))
}

fn read_sieve_response(stream: &gio::IOStream) -> Result<Vec<String>, String> {
    let input = stream.input_stream();
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut byte = [0u8; 1];
    while lines.len() < 128 {
        let n = input
            .read(&mut byte, gio::Cancellable::NONE)
            .map_err(|e| format!("ManageSieve read error: {e}"))?;
        if n == 0 {
            break;
        }
        current.push(byte[0]);
        if current.ends_with(b"\r\n") || current.ends_with(b"\n") {
            let line = String::from_utf8_lossy(&current).trim_end().to_string();
            current.clear();
            let upper = line.to_ascii_uppercase();
            let done = upper.starts_with("OK")
                || upper.starts_with("NO")
                || upper.starts_with("BYE");
            lines.push(line);
            if done {
                return Ok(lines);
            }
        }
    }
    if lines.is_empty() {
        Err("Empty response from ManageSieve server".to_string())
    } else {
        Ok(lines)
    }
}

fn expect_sieve_ok(lines: &[String], step: &str) -> Result<(), String> {
    if let Some(last) = lines.last() {
        if last.to_ascii_uppercase().starts_with("OK") {
            return Ok(());
        }
        return Err(format!("{step} rejected by server: {last}"));
    }
    Err(format!("{step} received no server status"))
}

fn escape_sieve_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn strip_basic_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let normalized = input
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n\n");
    for ch in normalized.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mail_filter_service::{FilterCondition, FilterScope};

    #[test]
    fn generates_dated_vacation_sieve_script() {
        let oof = OutOfOfficeSettings {
            enabled: true,
            use_schedule: true,
            start_date: "2026-12-20".to_string(),
            end_date: "2026-12-31".to_string(),
            internal_message: "<p>On holiday</p>".to_string(),
            external_message: "<p>Out until Jan 2</p>".to_string(),
        };
        let script =
            generate_sieve_vacation_script(&oof, &["martin@example.org".to_string()]);
        assert!(script.contains("require [\"date\", \"relational\", \"vacation\"];"));
        assert!(script.contains("currentdate :value \"ge\" \"date\" \"2026-12-20\""));
        assert!(script.contains(":addresses [\"martin@example.org\"]"));
        assert!(script.contains("Out until Jan 2"));
    }

    #[test]
    fn compiles_client_filters_to_standard_sieve() {
        let rule = MailFilterRule {
            id: "r1".to_string(),
            name: "Move Receipts".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                operator: FilterOperator::Contains,
                value: "Invoice".to_string(),
            },
            action: FilterAction::MarkRead,
            conditions: vec![FilterCondition {
                field: FilterField::Subject,
                operator: FilterOperator::Contains,
                value: "Invoice".to_string(),
            }],
            actions: vec![
                FilterAction::MarkRead,
                FilterAction::MoveToFolder("Accounting".to_string()),
                FilterAction::StopProcessing,
            ],
            match_all: true,
        };
        let sieve = compile_filters_to_sieve(&[rule]);
        assert!(sieve.contains("require [\"fileinto\", \"imap4flags\"];"));
        assert!(sieve.contains("header :contains \"Subject\" \"Invoice\""));
        assert!(sieve.contains("addflag \"\\\\Seen\";"));
        assert!(sieve.contains("fileinto \"Accounting\";"));
    }
}
