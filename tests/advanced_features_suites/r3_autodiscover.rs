// SPDX-License-Identifier: GPL-3.0-or-later
//
// R3: Account Autodiscover & Wizard Modernization Tests
// Covers:
// - Built-in provider profiles (Gmail, Outlook, iCloud, Yahoo, Posteo, GMX, Web.de)
// - Mozilla ISPDB XML response parsing & variable substitutions
// - DNS MX record resolution & heuristic fallback
// - AccountSetupDialog auto-fill state machine & validation error handling

use serde::{Deserialize, Serialize};
use super::common::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SocketSecurity {
    Ssl,
    Starttls,
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredServerConfig {
    pub hostname: String,
    pub port: u16,
    pub security: SocketSecurity,
    pub username_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredAccountConfig {
    pub email: String,
    pub display_name: String,
    pub incoming: DiscoveredServerConfig,
    pub outgoing: DiscoveredServerConfig,
}

/// Core Autodiscover engine simulator
pub struct MockAutodiscoverService;

impl MockAutodiscoverService {
    /// Extract domain from email address with strict validation
    pub fn extract_domain(email: &str) -> Result<(&str, &str), String> {
        let trimmed = email.trim();
        if trimmed.is_empty() {
            return Err("Email address is empty".to_string());
        }

        let parts: Vec<&str> = trimmed.split('@').collect();
        if parts.len() != 2 {
            return Err(format!("Invalid email address format: '{}'", email));
        }

        let local_part = parts[0];
        let domain = parts[1];

        if local_part.is_empty() || domain.is_empty() {
            return Err("Local part or domain cannot be empty".to_string());
        }

        if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
            return Err(format!("Invalid domain structure: '{}'", domain));
        }

        let domain_parts: Vec<&str> = domain.split('.').collect();
        if domain_parts.len() < 2 {
            return Err(format!("Invalid domain (missing TLD): '{}'", domain));
        }

        if let Some(tld) = domain_parts.last() {
            if tld.len() < 2 {
                return Err(format!("Invalid TLD length: '{}'", tld));
            }
        }

        Ok((local_part, domain))
    }

    /// Substitute username templates `%EMAILADDRESS%`, `%EMAILLOCALPART%`, and `%EMAILDOMAIN%`
    pub fn substitute_template(template: &str, email: &str, local_part: &str) -> String {
        let domain = email.split_once('@').map(|(_, d)| d).unwrap_or("");
        template
            .replace("%EMAILADDRESS%", email)
            .replace("%EMAILLOCALPART%", local_part)
            .replace("%EMAILDOMAIN%", domain)
    }

    /// Substitute username templates using all three components explicitly
    pub fn substitute_full_template(
        template: &str,
        email: &str,
        local_part: &str,
        domain: &str,
    ) -> String {
        template
            .replace("%EMAILADDRESS%", email)
            .replace("%EMAILLOCALPART%", local_part)
            .replace("%EMAILDOMAIN%", domain)
    }

    /// Tier 1: Known Built-in Provider Profiles
    pub fn lookup_builtin_profile(email: &str) -> Option<DiscoveredAccountConfig> {
        let (_local_part, domain) = Self::extract_domain(email).ok()?;
        let d = domain.to_lowercase();

        if d == "gmail.com" || d == "googlemail.com" {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Google Mail".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.gmail.com".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp.gmail.com".to_string(),
                    port: 465,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "outlook.com"
            || d == "hotmail.com"
            || d == "live.com"
            || d == "msn.com"
            || d == "outlook.de"
            || d == "hotmail.de"
            || d == "live.de"
            || d == "office365.com"
        {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Outlook / Microsoft 365".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "outlook.office365.com".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp-mail.outlook.com".to_string(),
                    port: 587,
                    security: SocketSecurity::Starttls,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "icloud.com" || d == "me.com" || d == "mac.com" {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Apple iCloud".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.mail.me.com".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILLOCALPART%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp.mail.me.com".to_string(),
                    port: 587,
                    security: SocketSecurity::Starttls,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "yahoo.com"
            || d == "yahoo.de"
            || d == "ymail.com"
            || d == "rocketmail.com"
            || d == "yahoo.co.uk"
            || d == "yahoo.fr"
            || d == "yahoo.it"
            || d == "yahoo.es"
        {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Yahoo! Mail".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.mail.yahoo.com".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp.mail.yahoo.com".to_string(),
                    port: 465,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "posteo.de"
            || d == "posteo.net"
            || d == "posteo.org"
            || d == "posteo.at"
            || d == "posteo.ch"
        {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Posteo".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "posteo.de".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "posteo.de".to_string(),
                    port: 465,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "gmx.de"
            || d == "gmx.net"
            || d == "gmx.at"
            || d == "gmx.ch"
            || d == "gmx.com"
        {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "GMX Mail".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.gmx.net".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "mail.gmx.net".to_string(),
                    port: 587,
                    security: SocketSecurity::Starttls,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "web.de" {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "WEB.DE".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.web.de".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp.web.de".to_string(),
                    port: 587,
                    security: SocketSecurity::Starttls,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "fastmail.com" || d == "fastmail.fm" {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Fastmail".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.fastmail.com".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp.fastmail.com".to_string(),
                    port: 465,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "mailbox.org" {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Mailbox.org".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "imap.mailbox.org".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "smtp.mailbox.org".to_string(),
                    port: 465,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else if d == "t-online.de" || d == "magenta.de" {
            Some(DiscoveredAccountConfig {
                email: email.to_string(),
                display_name: "Telekom Mail (T-Online)".to_string(),
                incoming: DiscoveredServerConfig {
                    hostname: "secureimap.t-online.de".to_string(),
                    port: 993,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
                outgoing: DiscoveredServerConfig {
                    hostname: "securesmtp.t-online.de".to_string(),
                    port: 465,
                    security: SocketSecurity::Ssl,
                    username_template: "%EMAILADDRESS%".to_string(),
                },
            })
        } else {
            None
        }
    }

    /// Tier 2: Mozilla ISPDB XML Parser
    pub fn parse_ispdb_xml(xml: &str, email: &str) -> Result<DiscoveredAccountConfig, String> {
        let trimmed = xml.trim();
        if trimmed.is_empty() {
            return Err("XML content is empty".to_string());
        }

        // XXE Security defense: reject DOCTYPE / ENTITY
        if trimmed.contains("<!ENTITY") || trimmed.contains("<!DOCTYPE") {
            return Err("XML containing DOCTYPE/ENTITY definitions is rejected for security".to_string());
        }

        let (_local_part, domain) = Self::extract_domain(email)?;

        // Helper to extract tag value
        let extract_tag = |block: &str, tag: &str| -> Option<String> {
            let open = format!("<{}>", tag);
            let close = format!("</{}>", tag);
            let start = block.find(&open)? + open.len();
            let end = block[start..].find(&close)?;
            Some(block[start..start + end].trim().to_string())
        };

        // Extract incomingServer: prefer imap, fallback to pop3
        let (incoming_block, is_pop3) = if let Some(start) = xml.find(r#"<incomingServer type="imap">"#) {
            let close = "</incomingServer>";
            let end = xml[start..].find(close).ok_or_else(|| "Unclosed incomingServer tag".to_string())?;
            (&xml[start..start + end + close.len()], false)
        } else if let Some(start) = xml.find(r#"<incomingServer type="pop3">"#) {
            let close = "</incomingServer>";
            let end = xml[start..].find(close).ok_or_else(|| "Unclosed incomingServer tag".to_string())?;
            (&xml[start..start + end + close.len()], true)
        } else {
            return Err("Missing <incomingServer> tag in ISPDB XML".to_string());
        };

        let in_host = extract_tag(incoming_block, "hostname")
            .ok_or_else(|| "Missing hostname in incomingServer".to_string())?;
        if in_host.contains(' ') {
            return Err(format!("Invalid incoming hostname: '{in_host}'"));
        }

        let in_port = if let Some(p_str) = extract_tag(incoming_block, "port") {
            let p = p_str.parse::<u16>().map_err(|_| format!("Invalid port: '{p_str}'"))?;
            if p == 0 {
                return Err("Port cannot be zero".to_string());
            }
            p
        } else if is_pop3 {
            995
        } else {
            993
        };

        let in_socket = extract_tag(incoming_block, "socketType")
            .map(|s| match s.to_uppercase().as_str() {
                "SSL" => SocketSecurity::Ssl,
                "STARTTLS" => SocketSecurity::Starttls,
                _ => SocketSecurity::Plain,
            })
            .unwrap_or(SocketSecurity::Ssl);
        let in_user = extract_tag(incoming_block, "username")
            .unwrap_or_else(|| "%EMAILADDRESS%".to_string());

        // Extract outgoingServer
        let outgoing_block = xml
            .find(r#"<outgoingServer type="smtp">"#)
            .and_then(|start| {
                let close = "</outgoingServer>";
                let end = xml[start..].find(close)?;
                Some(&xml[start..start + end + close.len()])
            })
            .ok_or_else(|| "Missing <outgoingServer> tag in ISPDB XML".to_string())?;

        let out_host = extract_tag(outgoing_block, "hostname")
            .ok_or_else(|| "Missing hostname in outgoingServer".to_string())?;
        if out_host.contains(' ') {
            return Err(format!("Invalid outgoing hostname: '{out_host}'"));
        }

        let out_port = if let Some(p_str) = extract_tag(outgoing_block, "port") {
            let p = p_str.parse::<u16>().map_err(|_| format!("Invalid port: '{p_str}'"))?;
            if p == 0 {
                return Err("Port cannot be zero".to_string());
            }
            p
        } else {
            587
        };

        let out_socket = extract_tag(outgoing_block, "socketType")
            .map(|s| match s.to_uppercase().as_str() {
                "SSL" => SocketSecurity::Ssl,
                "STARTTLS" => SocketSecurity::Starttls,
                _ => SocketSecurity::Plain,
            })
            .unwrap_or(SocketSecurity::Starttls);
        let out_user = extract_tag(outgoing_block, "username")
            .unwrap_or_else(|| "%EMAILADDRESS%".to_string());

        let display_name = extract_tag(xml, "displayName").unwrap_or_else(|| format!("{} Mail", domain));

        Ok(DiscoveredAccountConfig {
            email: email.to_string(),
            display_name,
            incoming: DiscoveredServerConfig {
                hostname: in_host,
                port: in_port,
                security: in_socket,
                username_template: in_user,
            },
            outgoing: DiscoveredServerConfig {
                hostname: out_host,
                port: out_port,
                security: out_socket,
                username_template: out_user,
            },
        })
    }

    /// Tier 4: DNS MX Record Correlation
    pub fn correlate_mx_provider(mx_host: &str) -> Option<&'static str> {
        let lower = mx_host.to_lowercase();
        if lower.contains("google.com") || lower.contains("googlemail.com") || lower.contains("l.google.com") {
            Some("gmail.com")
        } else if lower.contains("outlook.com") || lower.contains("protection.outlook.com") {
            Some("outlook.com")
        } else if lower.contains("mail.icloud.com") {
            Some("icloud.com")
        } else if lower.contains("messagingengine.com") {
            Some("fastmail.com")
        } else if lower.contains("pphosted.com") || lower.contains("proofpoint.com") {
            Some("proofpoint")
        } else {
            None
        }
    }

    /// Tier 5: Standard Heuristic Fallback
    pub fn heuristic_fallback(email: &str) -> Result<DiscoveredAccountConfig, String> {
        let (_, domain) = Self::extract_domain(email)?;
        Ok(DiscoveredAccountConfig {
            email: email.to_string(),
            display_name: format!("{} Mail", domain),
            incoming: DiscoveredServerConfig {
                hostname: format!("imap.{}", domain),
                port: 993,
                security: SocketSecurity::Ssl,
                username_template: "%EMAILADDRESS%".to_string(),
            },
            outgoing: DiscoveredServerConfig {
                hostname: format!("smtp.{}", domain),
                port: 587,
                security: SocketSecurity::Starttls,
                username_template: "%EMAILADDRESS%".to_string(),
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryTier {
    Builtin,
    Ispdb,
    DomainAutoconfig,
    DnsMx,
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeResult {
    pub tier: DiscoveryTier,
    pub config: DiscoveredAccountConfig,
}

pub fn simulate_cascade(
    email: &str,
    mock_ispdb_available: bool,
    mock_ispdb_xml: Option<&str>,
) -> CascadeResult {
    if let Some(conf) = MockAutodiscoverService::lookup_builtin_profile(email) {
        return CascadeResult {
            tier: DiscoveryTier::Builtin,
            config: conf,
        };
    }
    if mock_ispdb_available {
        if let Some(xml) = mock_ispdb_xml {
            if let Ok(conf) = MockAutodiscoverService::parse_ispdb_xml(xml, email) {
                return CascadeResult {
                    tier: DiscoveryTier::Ispdb,
                    config: conf,
                };
            }
        }
    }
    CascadeResult {
        tier: DiscoveryTier::Heuristic,
        config: MockAutodiscoverService::heuristic_fallback(email).unwrap(),
    }
}

pub fn simulate_cascade_with_mx(email: &str, mx_host: &str) -> CascadeResult {
    if let Some(provider_domain) = MockAutodiscoverService::correlate_mx_provider(mx_host) {
        let mock_email = format!("user@{}", provider_domain);
        if let Some(mut conf) = MockAutodiscoverService::lookup_builtin_profile(&mock_email) {
            conf.email = email.to_string();
            return CascadeResult {
                tier: DiscoveryTier::DnsMx,
                config: conf,
            };
        }
    }
    CascadeResult {
        tier: DiscoveryTier::Heuristic,
        config: MockAutodiscoverService::heuristic_fallback(email).unwrap(),
    }
}

// ============================================================================
// Tests for R3: Account Autodiscover & Wizard Modernization
// ============================================================================

#[test]
fn r3_test_autodiscover_builtin_profile_gmail() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("user@gmail.com").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.gmail.com");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.incoming.security, SocketSecurity::Ssl);
    assert_eq!(conf.outgoing.hostname, "smtp.gmail.com");
    assert_eq!(conf.outgoing.port, 465);
    assert_eq!(conf.outgoing.security, SocketSecurity::Ssl);
}

#[test]
fn r3_test_autodiscover_builtin_profile_outlook_and_hotmail() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("alice@outlook.com").unwrap();
    assert_eq!(conf.incoming.hostname, "outlook.office365.com");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "smtp-mail.outlook.com");
    assert_eq!(conf.outgoing.port, 587);
    assert_eq!(conf.outgoing.security, SocketSecurity::Starttls);

    let conf_hot = MockAutodiscoverService::lookup_builtin_profile("bob@hotmail.com").unwrap();
    assert_eq!(conf_hot.incoming.hostname, "outlook.office365.com");
}

#[test]
fn r3_test_autodiscover_builtin_profile_icloud() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("claire@icloud.com").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.mail.me.com");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "smtp.mail.me.com");
    assert_eq!(conf.outgoing.port, 587);
    assert_eq!(conf.incoming.username_template, "%EMAILLOCALPART%");
}

#[test]
fn r3_test_autodiscover_builtin_profile_yahoo() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("user@yahoo.com").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.mail.yahoo.com");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "smtp.mail.yahoo.com");
    assert_eq!(conf.outgoing.port, 465);
}

#[test]
fn r3_test_autodiscover_builtin_profile_posteo() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("privacy@posteo.de").unwrap();
    assert_eq!(conf.incoming.hostname, "posteo.de");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "posteo.de");
    assert_eq!(conf.outgoing.port, 465);
}

#[test]
fn r3_test_autodiscover_builtin_profile_gmx() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("kunde@gmx.de").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.gmx.net");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "mail.gmx.net");
    assert_eq!(conf.outgoing.port, 587);
}

#[test]
fn r3_test_autodiscover_builtin_profile_webde() {
    let conf = MockAutodiscoverService::lookup_builtin_profile("kunde@web.de").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.web.de");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "smtp.web.de");
    assert_eq!(conf.outgoing.port, 587);
}

#[test]
fn r3_test_mozilla_ispdb_xml_parsing_standard() {
    let xml = build_ispdb_xml(
        "example.org",
        "mail.example.org",
        993,
        "SSL",
        "smtp.example.org",
        587,
        "STARTTLS",
    );

    let res = MockAutodiscoverService::parse_ispdb_xml(&xml, "test@example.org");
    assert!(res.is_ok());
    let conf = res.unwrap();
    assert_eq!(conf.incoming.hostname, "mail.example.org");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.incoming.security, SocketSecurity::Ssl);
    assert_eq!(conf.outgoing.hostname, "smtp.example.org");
    assert_eq!(conf.outgoing.port, 587);
    assert_eq!(conf.outgoing.security, SocketSecurity::Starttls);
}

#[test]
fn r3_test_mozilla_ispdb_xml_username_template_substitution() {
    let template = "%EMAILADDRESS%";
    let resolved = MockAutodiscoverService::substitute_template(template, "john.doe@company.com", "john.doe");
    assert_eq!(resolved, "john.doe@company.com");

    let local_template = "%EMAILLOCALPART%";
    let resolved_local = MockAutodiscoverService::substitute_template(local_template, "john.doe@company.com", "john.doe");
    assert_eq!(resolved_local, "john.doe");
}

#[test]
fn r3_test_dns_mx_record_provider_correlation() {
    let mx_google = "aspmx.l.google.com";
    assert_eq!(
        MockAutodiscoverService::correlate_mx_provider(mx_google),
        Some("gmail.com")
    );

    let mx_m365 = "company-com.mail.protection.outlook.com";
    assert_eq!(
        MockAutodiscoverService::correlate_mx_provider(mx_m365),
        Some("outlook.com")
    );

    let mx_unknown = "mail.mycustomserver.net";
    assert_eq!(
        MockAutodiscoverService::correlate_mx_provider(mx_unknown),
        None
    );
}

#[test]
fn r3_test_dns_mx_priority_sorting() {
    let mut mx_records = vec![
        (50, "backup.mail.org"),
        (10, "primary.mail.org"),
        (20, "secondary.mail.org"),
    ];

    // Standard DNS MX sort by lowest preference value
    mx_records.sort_by_key(|r| r.0);
    assert_eq!(mx_records[0].1, "primary.mail.org");
    assert_eq!(mx_records[1].1, "secondary.mail.org");
    assert_eq!(mx_records[2].1, "backup.mail.org");
}

#[test]
fn r3_test_autodiscover_heuristic_fallback() {
    let conf = MockAutodiscoverService::heuristic_fallback("dev@customdomain.io").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.customdomain.io");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.outgoing.hostname, "smtp.customdomain.io");
    assert_eq!(conf.outgoing.port, 587);
}

#[test]
fn r3_test_autodiscover_invalid_email_rejection() {
    let invalid_cases = [
        "",
        "   ",
        "invalid-email",
        "missing-at-domain.com",
        "@domain.com",
        "user@",
        "user@@domain.com",
        "user@nodot",
    ];

    for invalid in &invalid_cases {
        let res = MockAutodiscoverService::extract_domain(invalid);
        assert!(res.is_err(), "Expected error for invalid email: '{}'", invalid);
    }
}

#[test]
fn r3_test_account_setup_dialog_autofill_state_machine() {
    // Simulates user entering "alice@posteo.de" into row_email
    let email_input = "alice@posteo.de";
    let discovered = MockAutodiscoverService::lookup_builtin_profile(email_input)
        .or_else(|| MockAutodiscoverService::heuristic_fallback(email_input).ok())
        .expect("Autodiscover must produce config for posteo.de");

    // Check dialog auto-fill values
    assert_eq!(discovered.incoming.hostname, "posteo.de");
    assert_eq!(discovered.incoming.port, 993);
    assert_eq!(discovered.outgoing.hostname, "posteo.de");
    assert_eq!(discovered.outgoing.port, 465);
    assert_eq!(discovered.display_name, "Posteo");
}

#[test]
fn r3_test_ispdb_xml_missing_tags_error_handling() {
    let broken_xml = r#"<clientConfig><emailProvider></emailProvider></clientConfig>"#;
    let res = MockAutodiscoverService::parse_ispdb_xml(broken_xml, "test@example.org");
    assert!(res.is_err(), "Broken XML must return error instead of panicking");
}

#[test]
fn r3_test_builtin_provider_comprehensive_matrix() {
    let test_cases = [
        ("user@fastmail.com", "imap.fastmail.com", 993, SocketSecurity::Ssl, "smtp.fastmail.com", 465, SocketSecurity::Ssl, "Fastmail"),
        ("user@fastmail.fm", "imap.fastmail.com", 993, SocketSecurity::Ssl, "smtp.fastmail.com", 465, SocketSecurity::Ssl, "Fastmail"),
        ("user@mailbox.org", "imap.mailbox.org", 993, SocketSecurity::Ssl, "smtp.mailbox.org", 465, SocketSecurity::Ssl, "Mailbox.org"),
        ("user@t-online.de", "secureimap.t-online.de", 993, SocketSecurity::Ssl, "securesmtp.t-online.de", 465, SocketSecurity::Ssl, "Telekom Mail (T-Online)"),
        ("user@magenta.de", "secureimap.t-online.de", 993, SocketSecurity::Ssl, "securesmtp.t-online.de", 465, SocketSecurity::Ssl, "Telekom Mail (T-Online)"),
    ];

    for (email, in_host, in_port, in_sec, out_host, out_port, out_sec, disp) in test_cases {
        let conf = MockAutodiscoverService::lookup_builtin_profile(email)
            .unwrap_or_else(|| panic!("Built-in profile missing for {}", email));
        assert_eq!(conf.incoming.hostname, in_host, "incoming host mismatch for {}", email);
        assert_eq!(conf.incoming.port, in_port, "incoming port mismatch for {}", email);
        assert_eq!(conf.incoming.security, in_sec, "incoming sec mismatch for {}", email);
        assert_eq!(conf.outgoing.hostname, out_host, "outgoing host mismatch for {}", email);
        assert_eq!(conf.outgoing.port, out_port, "outgoing port mismatch for {}", email);
        assert_eq!(conf.outgoing.security, out_sec, "outgoing sec mismatch for {}", email);
        assert_eq!(conf.display_name, disp, "display name mismatch for {}", email);
    }
}

#[test]
fn r3_test_ispdb_xml_pop3_fallback_when_no_imap() {
    let xml = r#"<clientConfig>
      <emailProvider id="legacy.org">
        <domain>legacy.org</domain>
        <displayName>Legacy Mail</displayName>
        <incomingServer type="pop3">
          <hostname>pop.legacy.org</hostname>
          <port>995</port>
          <socketType>SSL</socketType>
          <username>%EMAILADDRESS%</username>
        </incomingServer>
        <outgoingServer type="smtp">
          <hostname>smtp.legacy.org</hostname>
          <port>465</port>
          <socketType>SSL</socketType>
          <username>%EMAILADDRESS%</username>
        </outgoingServer>
      </emailProvider>
    </clientConfig>"#;

    let res = MockAutodiscoverService::parse_ispdb_xml(xml, "user@legacy.org");
    assert!(res.is_ok(), "POP3 fallback should parse successfully");
    let conf = res.unwrap();
    assert_eq!(conf.incoming.hostname, "pop.legacy.org");
    assert_eq!(conf.incoming.port, 995);
    assert_eq!(conf.incoming.security, SocketSecurity::Ssl);
    assert_eq!(conf.display_name, "Legacy Mail");
}

#[test]
fn r3_test_ispdb_xml_imap_preferred_over_pop3() {
    let xml = r#"<clientConfig>
      <emailProvider id="multi.org">
        <domain>multi.org</domain>
        <displayName>Multi Protocol Mail</displayName>
        <incomingServer type="pop3">
          <hostname>pop.multi.org</hostname>
          <port>995</port>
          <socketType>SSL</socketType>
          <username>%EMAILADDRESS%</username>
        </incomingServer>
        <incomingServer type="imap">
          <hostname>imap.multi.org</hostname>
          <port>993</port>
          <socketType>SSL</socketType>
          <username>%EMAILADDRESS%</username>
        </incomingServer>
        <outgoingServer type="smtp">
          <hostname>smtp.multi.org</hostname>
          <port>587</port>
          <socketType>STARTTLS</socketType>
          <username>%EMAILADDRESS%</username>
        </outgoingServer>
      </emailProvider>
    </clientConfig>"#;

    let conf = MockAutodiscoverService::parse_ispdb_xml(xml, "user@multi.org").unwrap();
    assert_eq!(conf.incoming.hostname, "imap.multi.org");
    assert_eq!(conf.incoming.port, 993);
    assert_eq!(conf.incoming.security, SocketSecurity::Ssl);
}

#[test]
fn r3_test_email_validation_exhaustive_boundaries() {
    // Valid cases
    let valid_cases = [
        ("user@domain.com", "domain.com"),
        ("  user@domain.com  ", "domain.com"),
        ("USER@DOMAIN.COM", "domain.com"),
        ("first.last+tag@sub.example.co.uk", "sub.example.co.uk"),
        ("123456@numeric-domain.io", "numeric-domain.io"),
    ];
    for (email, expected_domain) in valid_cases {
        let domain_res = MockAutodiscoverService::extract_domain(email);
        assert_eq!(domain_res.map(|(_, d)| d.to_ascii_lowercase()).as_deref(), Ok(expected_domain), "Failed extraction for: {}", email);
    }

    // Invalid cases
    let invalid_cases = [
        "",
        "   ",
        "no-at-sign",
        "@domain.com",
        "user@",
        "user@@domain.com",
        "user@domain@com",
        "user@nodot",
        "user@.domain.com",
        "user@domain.com.",
        "user@domain..com",
    ];
    for invalid in invalid_cases {
        let res = MockAutodiscoverService::extract_domain(invalid);
        assert!(res.is_err(), "Expected error for invalid candidate: '{}'", invalid);
    }
}

#[test]
fn r3_test_email_internationalized_domain_names() {
    let idn_email = "tester@xn--mgbh0fb.org";
    let (_local, domain) = MockAutodiscoverService::extract_domain(idn_email).unwrap();
    assert_eq!(domain, "xn--mgbh0fb.org");

    let heuristic = MockAutodiscoverService::heuristic_fallback(idn_email).unwrap();
    assert_eq!(heuristic.incoming.hostname, "imap.xn--mgbh0fb.org");
    assert_eq!(heuristic.outgoing.hostname, "smtp.xn--mgbh0fb.org");
}

#[test]
fn r3_test_username_substitution_all_three_variables() {
    let email = "alex.smith@corporation.com";
    let (local, domain) = MockAutodiscoverService::extract_domain(email).unwrap();
    assert_eq!(domain, "corporation.com");

    assert_eq!(
        MockAutodiscoverService::substitute_template("%EMAILADDRESS%", email, local),
        "alex.smith@corporation.com"
    );
    assert_eq!(
        MockAutodiscoverService::substitute_template("%EMAILLOCALPART%", email, local),
        "alex.smith"
    );
    assert_eq!(
        MockAutodiscoverService::substitute_template("%EMAILDOMAIN%", email, local),
        "corporation.com"
    );
    assert_eq!(
        MockAutodiscoverService::substitute_template("%EMAILLOCALPART%@auth.%EMAILDOMAIN%", email, local),
        "alex.smith@auth.corporation.com"
    );
}

#[test]
fn r3_test_autodiscover_cascade_simulation_priority() {
    // 1. Builtin priority
    let res1 = simulate_cascade("alice@gmail.com", true, None);
    assert_eq!(res1.tier, DiscoveryTier::Builtin);
    assert_eq!(res1.config.incoming.hostname, "imap.gmail.com");

    // 2. ISPDB priority when not builtin
    let custom_xml = build_ispdb_xml(
        "customorg.net",
        "mail.customorg.net",
        993,
        "SSL",
        "smtp.customorg.net",
        587,
        "STARTTLS",
    );
    let res2 = simulate_cascade("bob@customorg.net", true, Some(&custom_xml));
    assert_eq!(res2.tier, DiscoveryTier::Ispdb);
    assert_eq!(res2.config.incoming.hostname, "mail.customorg.net");

    // 3. Heuristic fallback when ISPDB unavailable or fails
    let res3 = simulate_cascade("carol@unknown-biz.io", false, None);
    assert_eq!(res3.tier, DiscoveryTier::Heuristic);
    assert_eq!(res3.config.incoming.hostname, "imap.unknown-biz.io");

    // 4. DNS MX resolution fallback
    let res4 = simulate_cascade_with_mx("sales@customdomain.de", "aspmx.l.google.com");
    assert_eq!(res4.tier, DiscoveryTier::DnsMx);
    assert_eq!(res4.config.incoming.hostname, "imap.gmail.com");
    assert_eq!(res4.config.email, "sales@customdomain.de");
}

#[test]
fn r3_test_discovered_config_to_account_profile_and_json_roundtrip() {
    let conf = DiscoveredAccountConfig {
        email: "charlie@example.org".to_string(),
        display_name: "Example Org".to_string(),
        incoming: DiscoveredServerConfig {
            hostname: "imap.example.org".to_string(),
            port: 993,
            security: SocketSecurity::Ssl,
            username_template: "%EMAILADDRESS%".to_string(),
        },
        outgoing: DiscoveredServerConfig {
            hostname: "smtp.example.org".to_string(),
            port: 587,
            security: SocketSecurity::Starttls,
            username_template: "%EMAILADDRESS%".to_string(),
        },
    };

    let serialized = serde_json::to_string(&conf).expect("Must serialize DiscoveredAccountConfig");
    let deserialized: DiscoveredAccountConfig =
        serde_json::from_str(&serialized).expect("Must deserialize DiscoveredAccountConfig");

    assert_eq!(conf, deserialized);
    assert_eq!(deserialized.incoming.hostname, "imap.example.org");
    assert_eq!(deserialized.outgoing.port, 587);
}
