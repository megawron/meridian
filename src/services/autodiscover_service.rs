// SPDX-License-Identifier: GPL-3.0-or-later
//
// AutodiscoverService: 5-Tier Email Account Autodiscover Engine.
//
// Cascade:
// - Tier 1: Built-in Known Provider Profiles (Gmail, Outlook/O365, iCloud, Yahoo, Posteo, GMX, Web.de, Mailbox.org, Fastmail, T-Online)
// - Tier 2: Mozilla Thunderbird ISPDB HTTP Lookup (https://autoconfig.thunderbird.net/v1.1/{domain})
// - Tier 3: Domain Autoconfig HTTP Queries (autoconfig.{domain} & /.well-known/autoconfig/mail/config-v1.1.xml)
// - Tier 4: DNS MX Lookup via gio::Resolver & SaaS provider correlation
// - Tier 5: Common Host Heuristics (imap.{domain}:993, smtp.{domain}:587)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gio::prelude::*;
use gio::{Resolver, ResolverRecordType};
use serde::{Deserialize, Serialize};

use crate::services::account_config_service::AccountProtocol;

/// TLS connection security mode matching AccountProfile and AccountSetupDialog
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TlsMode {
    /// SSL/TLS on dedicated port (IMAP 993, SMTP 465, POP3 995)
    SslTls,
    /// STARTTLS on standard port (IMAP 143, SMTP 587, POP3 110)
    Starttls,
    /// Plain unencrypted connection (insecure)
    None,
}

impl TlsMode {
    pub const TLS: TlsMode = TlsMode::SslTls;
    pub const SSL: TlsMode = TlsMode::SslTls;
    pub const PLAIN: TlsMode = TlsMode::None;

    #[allow(non_upper_case_globals)]
    pub const Tls: TlsMode = TlsMode::SslTls;
    #[allow(non_upper_case_globals)]
    pub const Ssl: TlsMode = TlsMode::SslTls;
    #[allow(non_upper_case_globals)]
    pub const Plain: TlsMode = TlsMode::None;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SslTls => "tls",
            Self::Starttls => "starttls",
            Self::None => "none",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "starttls" => Self::Starttls,
            "none" | "plain" | "cleartext" => Self::None,
            _ => Self::SslTls,
        }
    }

    /// Maps to index in AccountSetupDialog's `row_security` DropDown:
    /// 0 -> SSL / TLS (Port 993 / 995)
    /// 1 -> STARTTLS (Port 143 / 110)
    /// 2 -> No encryption
    pub fn row_security_index(self) -> u32 {
        match self {
            Self::SslTls => 0,
            Self::Starttls => 1,
            Self::None => 2,
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::SslTls => "SSL/TLS",
            Self::Starttls => "STARTTLS",
            Self::None => "No Encryption",
        }
    }
}

pub type SocketSecurity = TlsMode;

/// The discovery tier that successfully produced the configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryTier {
    /// Tier 1: Local built-in provider profiles (0 ms, 100% offline reliability)
    BuiltinProfile,
    /// Tier 2: Mozilla Thunderbird ISPDB HTTP lookup
    MozillaIspdb,
    /// Tier 3: Domain autoconfig HTTP query (.well-known or autoconfig.{domain})
    DomainAutoconfig,
    /// Tier 4: DNS MX resolution & provider pattern matching
    DnsMx,
    /// Tier 5: Common host heuristics (imap.{domain}:993, smtp.{domain}:587)
    Heuristic,
}

impl DiscoveryTier {
    pub const BUILTIN: DiscoveryTier = DiscoveryTier::BuiltinProfile;
    pub const ISPDB: DiscoveryTier = DiscoveryTier::MozillaIspdb;

    #[allow(non_upper_case_globals)]
    pub const Builtin: DiscoveryTier = DiscoveryTier::BuiltinProfile;
    #[allow(non_upper_case_globals)]
    pub const Ispdb: DiscoveryTier = DiscoveryTier::MozillaIspdb;

    pub fn display_name(self) -> &'static str {
        match self {
            Self::BuiltinProfile => "Known Provider",
            Self::MozillaIspdb => "Mozilla ISPDB",
            Self::DomainAutoconfig => "Domain Autoconfig",
            Self::DnsMx => "DNS MX Detection",
            Self::Heuristic => "Standard Heuristics",
        }
    }

    pub fn is_authoritative(self) -> bool {
        matches!(
            self,
            Self::BuiltinProfile | Self::MozillaIspdb | Self::DomainAutoconfig | Self::DnsMx
        )
    }
}

/// Incoming or outgoing server connection parameters
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub hostname: String,
    pub port: u16,
    pub tls_mode: TlsMode,
    pub security: TlsMode,
    pub socket_type: String, // "SSL", "STARTTLS", "plain"
    pub auth_mechanism: Option<String>, // e.g. "OAuth2", "password-cleartext"
    pub username_template: String, // "%EMAILADDRESS%", "%EMAILLOCALPART%", etc.
}

impl ServerConfig {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        tls_mode: TlsMode,
        username_template: impl Into<String>,
        auth_mechanism: Option<&str>,
    ) -> Self {
        let host_str = host.into();
        let socket_type = match tls_mode {
            TlsMode::SslTls => "SSL",
            TlsMode::Starttls => "STARTTLS",
            TlsMode::None => "plain",
        }
        .to_string();

        Self {
            host: host_str.clone(),
            hostname: host_str,
            port,
            tls_mode,
            security: tls_mode,
            socket_type,
            auth_mechanism: auth_mechanism.map(|s| s.to_string()),
            username_template: username_template.into(),
        }
    }
}

/// Fully resolved account configuration returned by discovery
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredAccountConfig {
    pub provider_name: String,
    pub display_name: String,
    pub email: String,
    pub domain: String,
    pub incoming: ServerConfig,
    pub outgoing: ServerConfig,
    pub incoming_protocol: AccountProtocol,
    pub tier: DiscoveryTier,
    pub note: Option<String>,
}

impl DiscoveredAccountConfig {
    pub fn imap_host(&self) -> &str {
        &self.incoming.host
    }

    pub fn imap_port(&self) -> u16 {
        self.incoming.port
    }

    pub fn smtp_host(&self) -> &str {
        &self.outgoing.host
    }

    pub fn smtp_port(&self) -> u16 {
        self.outgoing.port
    }

    pub fn incoming_tls_mode(&self) -> TlsMode {
        self.incoming.tls_mode
    }

    pub fn outgoing_tls_mode(&self) -> TlsMode {
        self.outgoing.tls_mode
    }

    /// Resolves the username template (%EMAILADDRESS%, %EMAILLOCALPART%, %EMAILDOMAIN%)
    pub fn resolved_username(&self) -> String {
        let (local, domain) = split_email_parts(&self.email);
        expand_template(&self.incoming.username_template, &self.email, local, domain)
    }
}

pub struct AutodiscoverService;

impl AutodiscoverService {
    /// Execute the full 5-tier discovery cascade synchronously.
    /// Run off the main GTK thread.
    pub fn discover(email: &str) -> Result<DiscoveredAccountConfig, String> {
        let dummy_cancel = Arc::new(AtomicBool::new(false));
        Self::discover_cancelable(email, &dummy_cancel)
    }

    /// Execute the full 5-tier discovery cascade with cancellation token support.
    pub fn discover_cancelable(
        email: &str,
        cancellation: &Arc<AtomicBool>,
    ) -> Result<DiscoveredAccountConfig, String> {
        let email = email.trim();
        let (local, domain) = split_email_parts(email);
        if local.is_empty() || domain.is_empty() || !domain.contains('.') {
            return Err(format!("Invalid email address: '{email}'"));
        }

        if cancellation.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }

        // Tier 1: Built-in Known Provider Profiles (0 ms, 100% offline)
        if let Some(config) = Self::discover_tier1_builtin(email, domain) {
            return Ok(config);
        }

        if cancellation.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }

        // Tier 2: Mozilla Thunderbird ISPDB HTTP Lookup
        if let Some(config) = Self::discover_tier2_mozilla_ispdb(email, domain, Some(cancellation)) {
            return Ok(config);
        }

        if cancellation.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }

        // Tier 3: Domain Autoconfig HTTP Queries (.well-known and autoconfig.{domain})
        if let Some(config) = Self::discover_tier3_domain_autoconfig(email, domain, Some(cancellation)) {
            return Ok(config);
        }

        if cancellation.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }

        // Tier 4: DNS MX Resolution & SaaS Provider Pattern Matching
        if let Some(config) = Self::discover_tier4_dns_mx(email, domain, Some(cancellation)) {
            return Ok(config);
        }

        if cancellation.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }

        // Tier 5: Common Host Heuristic Fallback
        Ok(Self::discover_tier5_heuristic(email, domain))
    }

    /// Tier 1: Built-in Provider Profiles
    pub fn discover_tier1_builtin(email: &str, domain: &str) -> Option<DiscoveredAccountConfig> {
        let domain_lower = domain.to_ascii_lowercase();

        for profile in BUILTIN_PROVIDERS {
            if profile
                .domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&domain_lower))
            {
                return Some(DiscoveredAccountConfig {
                    provider_name: profile.name.to_string(),
                    display_name: profile.name.to_string(),
                    email: email.to_string(),
                    domain: domain.to_string(),
                    incoming: ServerConfig::new(
                        profile.imap_host,
                        profile.imap_port,
                        profile.imap_tls,
                        profile.username_template,
                        profile.auth_mechanism,
                    ),
                    outgoing: ServerConfig::new(
                        profile.smtp_host,
                        profile.smtp_port,
                        profile.smtp_tls,
                        profile.username_template,
                        profile.auth_mechanism,
                    ),
                    incoming_protocol: AccountProtocol::ImapSmtp,
                    tier: DiscoveryTier::BuiltinProfile,
                    note: profile.note.map(|s| s.to_string()),
                });
            }
        }
        None
    }

    /// Tier 2: Mozilla Thunderbird ISPDB HTTP Query
    pub fn discover_tier2_mozilla_ispdb(
        email: &str,
        domain: &str,
        cancellation: Option<&Arc<AtomicBool>>,
    ) -> Option<DiscoveredAccountConfig> {
        if let Some(c) = cancellation {
            if c.load(Ordering::Relaxed) {
                return None;
            }
        }

        let url = format!("https://autoconfig.thunderbird.net/v1.1/{domain}");
        if let Ok(xml) = http_fetch_get(&url, 4, cancellation) {
            if let Ok(config) = parse_thunderbird_autoconfig_xml(
                &xml,
                email,
                domain,
                DiscoveryTier::MozillaIspdb,
            ) {
                return Some(config);
            }
        }
        None
    }

    /// Tier 3: Domain Autoconfig HTTP Queries
    pub fn discover_tier3_domain_autoconfig(
        email: &str,
        domain: &str,
        cancellation: Option<&Arc<AtomicBool>>,
    ) -> Option<DiscoveredAccountConfig> {
        let urls = [
            format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
            format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
            format!("http://autoconfig.{domain}/mail/config-v1.1.xml"),
            format!("http://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
        ];

        for url in &urls {
            if let Some(c) = cancellation {
                if c.load(Ordering::Relaxed) {
                    return None;
                }
            }

            if let Ok(xml) = http_fetch_get(url, 3, cancellation) {
                if let Ok(config) = parse_thunderbird_autoconfig_xml(
                    &xml,
                    email,
                    domain,
                    DiscoveryTier::DomainAutoconfig,
                ) {
                    return Some(config);
                }
            }
        }
        None
    }

    /// Tier 4: DNS MX Resolution & Pattern Matching
    pub fn discover_tier4_dns_mx(
        email: &str,
        domain: &str,
        cancellation: Option<&Arc<AtomicBool>>,
    ) -> Option<DiscoveredAccountConfig> {
        if let Some(c) = cancellation {
            if c.load(Ordering::Relaxed) {
                return None;
            }
        }

        let mx_hosts = resolve_dns_mx(domain).ok()?;
        if mx_hosts.is_empty() {
            return None;
        }

        // 1. Detect known SaaS mail providers by MX hostname patterns
        for (_, mx_host) in &mx_hosts {
            let mx_lower = mx_host.to_ascii_lowercase();

            // Google Workspace
            if mx_lower.contains("google.com")
                || mx_lower.contains("googlemail.com")
                || mx_lower.contains("l.google.com")
            {
                return Some(DiscoveredAccountConfig {
                    provider_name: "Google Workspace (DNS MX)".to_string(),
                    display_name: "Google Workspace".to_string(),
                    email: email.to_string(),
                    domain: domain.to_string(),
                    incoming: ServerConfig::new(
                        "imap.gmail.com",
                        993,
                        TlsMode::SslTls,
                        "%EMAILADDRESS%",
                        Some("OAuth2"),
                    ),
                    outgoing: ServerConfig::new(
                        "smtp.gmail.com",
                        465,
                        TlsMode::SslTls,
                        "%EMAILADDRESS%",
                        Some("OAuth2"),
                    ),
                    incoming_protocol: AccountProtocol::ImapSmtp,
                    tier: DiscoveryTier::DnsMx,
                    note: Some("Google Workspace detected. With 2FA enabled, an app password (or OAuth2) is required.".to_string()),
                });
            }

            // Microsoft 365 / Exchange Online
            if mx_lower.contains("outlook.com")
                || mx_lower.contains("mail.protection.outlook.com")
                || mx_lower.contains("protection.outlook.com")
                || mx_lower.contains("office365.com")
            {
                return Some(DiscoveredAccountConfig {
                    provider_name: "Microsoft 365 (DNS MX)".to_string(),
                    display_name: "Microsoft 365".to_string(),
                    email: email.to_string(),
                    domain: domain.to_string(),
                    incoming: ServerConfig::new(
                        "outlook.office365.com",
                        993,
                        TlsMode::SslTls,
                        "%EMAILADDRESS%",
                        Some("OAuth2"),
                    ),
                    outgoing: ServerConfig::new(
                        "smtp-mail.outlook.com",
                        587,
                        TlsMode::Starttls,
                        "%EMAILADDRESS%",
                        Some("OAuth2"),
                    ),
                    incoming_protocol: AccountProtocol::ImapSmtp,
                    tier: DiscoveryTier::DnsMx,
                    note: Some("Microsoft 365 detected. Modern authentication (OAuth2) is recommended.".to_string()),
                });
            }

            // Apple iCloud Custom Domain
            if mx_lower.contains("mail.icloud.com") {
                return Some(DiscoveredAccountConfig {
                    provider_name: "Apple iCloud Custom Domain (DNS MX)".to_string(),
                    display_name: "Apple iCloud".to_string(),
                    email: email.to_string(),
                    domain: domain.to_string(),
                    incoming: ServerConfig::new(
                        "imap.mail.me.com",
                        993,
                        TlsMode::SslTls,
                        "%EMAILLOCALPART%",
                        None,
                    ),
                    outgoing: ServerConfig::new(
                        "smtp.mail.me.com",
                        587,
                        TlsMode::Starttls,
                        "%EMAILADDRESS%",
                        None,
                    ),
                    incoming_protocol: AccountProtocol::ImapSmtp,
                    tier: DiscoveryTier::DnsMx,
                    note: Some("iCloud detected. Requires an app-specific password.".to_string()),
                });
            }

            // Fastmail
            if mx_lower.contains("messagingengine.com") {
                return Some(DiscoveredAccountConfig {
                    provider_name: "Fastmail (DNS MX)".to_string(),
                    display_name: "Fastmail".to_string(),
                    email: email.to_string(),
                    domain: domain.to_string(),
                    incoming: ServerConfig::new(
                        "imap.fastmail.com",
                        993,
                        TlsMode::SslTls,
                        "%EMAILADDRESS%",
                        None,
                    ),
                    outgoing: ServerConfig::new(
                        "smtp.fastmail.com",
                        465,
                        TlsMode::SslTls,
                        "%EMAILADDRESS%",
                        None,
                    ),
                    incoming_protocol: AccountProtocol::ImapSmtp,
                    tier: DiscoveryTier::DnsMx,
                    note: None,
                });
            }

            // ProtonMail Custom Domain
            if mx_lower.contains("protonmail.ch") || mx_lower.contains("proton.me") {
                return Some(DiscoveredAccountConfig {
                    provider_name: "ProtonMail Bridge (DNS MX)".to_string(),
                    display_name: "ProtonMail".to_string(),
                    email: email.to_string(),
                    domain: domain.to_string(),
                    incoming: ServerConfig::new(
                        "127.0.0.1",
                        1143,
                        TlsMode::Starttls,
                        "%EMAILADDRESS%",
                        None,
                    ),
                    outgoing: ServerConfig::new(
                        "127.0.0.1",
                        1025,
                        TlsMode::Starttls,
                        "%EMAILADDRESS%",
                        None,
                    ),
                    incoming_protocol: AccountProtocol::ImapSmtp,
                    tier: DiscoveryTier::DnsMx,
                    note: Some("ProtonMail detected. Requires the local ProtonMail Bridge.".to_string()),
                });
            }
        }

        // 2. Upstream Evolution algorithm: Strip MX subdomains and query Mozilla ISPDB for provider domain
        if let Some((_, primary_mx)) = mx_hosts.first() {
            let parts: Vec<&str> = primary_mx.trim_end_matches('.').split('.').collect();
            if parts.len() >= 2 {
                let mx_parent_domain =
                    format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1]);
                if let Ok(xml) = http_fetch_get(
                    &format!("https://autoconfig.thunderbird.net/v1.1/{mx_parent_domain}"),
                    4,
                    cancellation,
                ) {
                    if let Ok(mut config) = parse_thunderbird_autoconfig_xml(
                        &xml,
                        email,
                        domain,
                        DiscoveryTier::DnsMx,
                    ) {
                        config.provider_name =
                            format!("{} (via MX {})", config.provider_name, mx_parent_domain);
                        return Some(config);
                    }
                }
            }
        }

        None
    }

    /// Tier 5: Common Host Heuristics
    pub fn discover_tier5_heuristic(email: &str, domain: &str) -> DiscoveredAccountConfig {
        DiscoveredAccountConfig {
            provider_name: format!("{domain} Mail"),
            display_name: format!("{domain} Mail"),
            email: email.to_string(),
            domain: domain.to_string(),
            incoming: ServerConfig::new(
                format!("imap.{domain}"),
                993,
                TlsMode::SslTls,
                "%EMAILADDRESS%",
                None,
            ),
            outgoing: ServerConfig::new(
                format!("smtp.{domain}"),
                587,
                TlsMode::Starttls,
                "%EMAILADDRESS%",
                None,
            ),
            incoming_protocol: AccountProtocol::ImapSmtp,
            tier: DiscoveryTier::Heuristic,
            note: Some("Settings are based on standard heuristics (imap/smtp). Please verify manually.".to_string()),
        }
    }
}

// Built-in Known Providers Database
struct BuiltinProviderDef {
    name: &'static str,
    domains: &'static [&'static str],
    imap_host: &'static str,
    imap_port: u16,
    imap_tls: TlsMode,
    smtp_host: &'static str,
    smtp_port: u16,
    smtp_tls: TlsMode,
    username_template: &'static str,
    auth_mechanism: Option<&'static str>,
    note: Option<&'static str>,
}

static BUILTIN_PROVIDERS: &[BuiltinProviderDef] = &[
    // 1. Google Gmail / Googlemail
    BuiltinProviderDef {
        name: "Google Gmail",
        domains: &["gmail.com", "googlemail.com"],
        imap_host: "imap.gmail.com",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp.gmail.com",
        smtp_port: 465,
        smtp_tls: TlsMode::SslTls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: Some("OAuth2"),
        note: Some("For Gmail with 2FA enabled, an app password (or OAuth2) is required."),
    },
    // 2. Microsoft Outlook / Hotmail / Live
    BuiltinProviderDef {
        name: "Outlook / Microsoft 365",
        domains: &[
            "outlook.com", "hotmail.com", "live.com", "msn.com",
            "outlook.de", "hotmail.de", "live.de", "office365.com",
        ],
        imap_host: "outlook.office365.com",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp-mail.outlook.com",
        smtp_port: 587,
        smtp_tls: TlsMode::Starttls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: Some("OAuth2"),
        note: Some("Modern authentication (OAuth2) is recommended for Microsoft accounts."),
    },
    // 3. Apple iCloud
    BuiltinProviderDef {
        name: "Apple iCloud",
        domains: &["icloud.com", "me.com", "mac.com"],
        imap_host: "imap.mail.me.com",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp.mail.me.com",
        smtp_port: 587,
        smtp_tls: TlsMode::Starttls,
        username_template: "%EMAILLOCALPART%",
        auth_mechanism: None,
        note: Some("iCloud requires an app-specific password (appleid.apple.com)."),
    },
    // 4. Yahoo Mail
    BuiltinProviderDef {
        name: "Yahoo! Mail",
        domains: &[
            "yahoo.com", "yahoo.de", "ymail.com", "rocketmail.com",
            "yahoo.co.uk", "yahoo.fr", "yahoo.it", "yahoo.es",
        ],
        imap_host: "imap.mail.yahoo.com",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp.mail.yahoo.com",
        smtp_port: 465,
        smtp_tls: TlsMode::SslTls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: Some("OAuth2"),
        note: Some("Yahoo requires an app-specific password or OAuth2."),
    },
    // 5. Posteo
    BuiltinProviderDef {
        name: "Posteo",
        domains: &["posteo.de", "posteo.net", "posteo.org", "posteo.at", "posteo.ch"],
        imap_host: "posteo.de",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "posteo.de",
        smtp_port: 465,
        smtp_tls: TlsMode::SslTls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: None,
        note: None,
    },
    // 6. GMX
    BuiltinProviderDef {
        name: "GMX Mail",
        domains: &["gmx.de", "gmx.net", "gmx.at", "gmx.ch", "gmx.com"],
        imap_host: "imap.gmx.net",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "mail.gmx.net",
        smtp_port: 587,
        smtp_tls: TlsMode::Starttls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: None,
        note: Some("Please ensure IMAP/POP3 access is enabled in your GMX settings."),
    },
    // 7. WEB.DE
    BuiltinProviderDef {
        name: "WEB.DE",
        domains: &["web.de"],
        imap_host: "imap.web.de",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp.web.de",
        smtp_port: 587,
        smtp_tls: TlsMode::Starttls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: None,
        note: Some("Please ensure IMAP/POP3 access is enabled in your WEB.DE settings."),
    },
    // 8. Mailbox.org
    BuiltinProviderDef {
        name: "Mailbox.org",
        domains: &["mailbox.org"],
        imap_host: "imap.mailbox.org",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp.mailbox.org",
        smtp_port: 465,
        smtp_tls: TlsMode::SslTls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: None,
        note: None,
    },
    // 9. Fastmail
    BuiltinProviderDef {
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm"],
        imap_host: "imap.fastmail.com",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "smtp.fastmail.com",
        smtp_port: 465,
        smtp_tls: TlsMode::SslTls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: None,
        note: None,
    },
    // 10. T-Online
    BuiltinProviderDef {
        name: "Telekom Mail (T-Online)",
        domains: &["t-online.de", "magenta.de"],
        imap_host: "secureimap.t-online.de",
        imap_port: 993,
        imap_tls: TlsMode::SslTls,
        smtp_host: "securesmtp.t-online.de",
        smtp_port: 465,
        smtp_tls: TlsMode::SslTls,
        username_template: "%EMAILADDRESS%",
        auth_mechanism: None,
        note: Some("Requires the separate 'Password for email programs'."),
    },
];

fn http_fetch_get(
    url: &str,
    timeout_secs: u64,
    cancellation: Option<&Arc<AtomicBool>>,
) -> Result<String, String> {
    if let Some(c) = cancellation {
        if c.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("Meridian/0.1.0 (Autoconfig)")
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(url)
        .header(
            reqwest::header::ACCEPT,
            "application/xml, text/xml;q=0.9, */*;q=0.8",
        )
        .send()
        .map_err(|e| e.to_string())?;

    if let Some(c) = cancellation {
        if c.load(Ordering::Relaxed) {
            return Err("Operation cancelled".to_string());
        }
    }

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    response.text().map_err(|e| e.to_string())
}

/// Resolves DNS MX records using GIO Resolver
pub fn resolve_dns_mx(domain: &str) -> Result<Vec<(u16, String)>, String> {
    let resolver = Resolver::default();
    let records = resolver
        .lookup_records(domain, ResolverRecordType::Mx, gio::Cancellable::NONE)
        .map_err(|e| format!("DNS MX lookup failed: {e}"))?;

    let mut result = Vec::new();
    for var in records {
        // Variant is (qs) -> (uint16 preference, string host)
        let pref = var.child_value(0).get::<u16>();
        let host = var.child_value(1).get::<String>();
        if let (Some(p), Some(h)) = (pref, host) {
            result.push((p, h));
        }
    }

    result.sort_by_key(|(pref, _)| *pref);
    Ok(result)
}

/// Pure helper to correlate MX hostname with known mail provider base domains
pub fn correlate_mx_provider(mx_host: &str) -> Option<&'static str> {
    let lower = mx_host.to_ascii_lowercase();
    if lower.contains("google.com")
        || lower.contains("googlemail.com")
        || lower.contains("l.google.com")
    {
        Some("gmail.com")
    } else if lower.contains("outlook.com")
        || lower.contains("mail.protection.outlook.com")
        || lower.contains("protection.outlook.com")
        || lower.contains("office365.com")
    {
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

/// Parses Thunderbird / Mozilla clientConfig XML format safely with zero external dependencies.
pub fn parse_thunderbird_autoconfig_xml(
    xml: &str,
    email: &str,
    fallback_domain: &str,
    tier: DiscoveryTier,
) -> Result<DiscoveredAccountConfig, String> {
    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return Err("XML content is empty".to_string());
    }

    // Safety defense against XXE: reject XML with custom DOCTYPE entity expansions
    if trimmed.contains("<!ENTITY") || trimmed.contains("<!DOCTYPE") {
        return Err("XML containing DOCTYPE/ENTITY definitions is rejected for security".to_string());
    }

    let (_local, domain) = split_email_parts(email);
    let effective_domain = if domain.is_empty() {
        fallback_domain
    } else {
        domain
    };

    // Extract provider name
    let provider_name = extract_tag_content(trimmed, "displayName")
        .or_else(|| extract_tag_content(trimmed, "displayShortName"))
        .unwrap_or_else(|| format!("{effective_domain} Mail"));

    // Extract incomingServer (prefer imap, fallback to pop3)
    let (incoming_block, protocol) = extract_server_block(trimmed, "incomingServer", "imap")
        .map(|b| (b, AccountProtocol::ImapSmtp))
        .or_else(|| {
            extract_server_block(trimmed, "incomingServer", "pop3")
                .map(|b| (b, AccountProtocol::Pop3))
        })
        .ok_or_else(|| "Missing <incomingServer> tag in ISPDB XML".to_string())?;

    // Extract outgoingServer (smtp)
    let outgoing_block = extract_server_block(trimmed, "outgoingServer", "smtp")
        .ok_or_else(|| "Missing <outgoingServer> tag in ISPDB XML".to_string())?;

    // Incoming parameters
    let in_host = extract_tag_content(&incoming_block, "hostname")
        .ok_or_else(|| "Missing hostname in incomingServer".to_string())?;

    // Validate hostname doesn't contain spaces
    if in_host.contains(' ') {
        return Err(format!("Invalid incoming hostname: '{in_host}'"));
    }

    let in_socket = extract_tag_content(&incoming_block, "socketType")
        .unwrap_or_else(|| "SSL".to_string());
    let in_tls = match in_socket.to_ascii_uppercase().as_str() {
        "STARTTLS" => TlsMode::Starttls,
        "NONE" | "PLAIN" => TlsMode::None,
        _ => TlsMode::SslTls,
    };

    let in_port_str = extract_tag_content(&incoming_block, "port");
    let in_port: u16 = if let Some(ref p_str) = in_port_str {
        p_str
            .parse::<u16>()
            .map_err(|_| format!("Invalid incoming port: '{p_str}'"))?
    } else {
        match (protocol, in_tls) {
            (AccountProtocol::Pop3, TlsMode::SslTls) => 995,
            (AccountProtocol::Pop3, _) => 110,
            (_, TlsMode::SslTls) => 993,
            _ => 143,
        }
    };

    if in_port == 0 {
        return Err("Incoming port cannot be zero".to_string());
    }

    let in_user_template = extract_tag_content(&incoming_block, "username")
        .unwrap_or_else(|| "%EMAILADDRESS%".to_string());
    let in_auth = extract_tag_content(&incoming_block, "authentication");

    // Outgoing parameters
    let out_host = extract_tag_content(&outgoing_block, "hostname")
        .ok_or_else(|| "Missing hostname in outgoingServer".to_string())?;

    if out_host.contains(' ') {
        return Err(format!("Invalid outgoing hostname: '{out_host}'"));
    }

    let out_socket = extract_tag_content(&outgoing_block, "socketType")
        .unwrap_or_else(|| "STARTTLS".to_string());
    let out_tls = match out_socket.to_ascii_uppercase().as_str() {
        "SSL" => TlsMode::SslTls,
        "NONE" | "PLAIN" => TlsMode::None,
        _ => TlsMode::Starttls,
    };

    let out_port_str = extract_tag_content(&outgoing_block, "port");
    let out_port: u16 = if let Some(ref p_str) = out_port_str {
        p_str
            .parse::<u16>()
            .map_err(|_| format!("Invalid outgoing port: '{p_str}'"))?
    } else {
        match out_tls {
            TlsMode::SslTls => 465,
            _ => 587,
        }
    };

    if out_port == 0 {
        return Err("Outgoing port cannot be zero".to_string());
    }

    let out_user_template = extract_tag_content(&outgoing_block, "username")
        .unwrap_or_else(|| "%EMAILADDRESS%".to_string());
    let out_auth = extract_tag_content(&outgoing_block, "authentication");

    Ok(DiscoveredAccountConfig {
        provider_name: provider_name.clone(),
        display_name: provider_name,
        email: email.to_string(),
        domain: effective_domain.to_string(),
        incoming: ServerConfig {
            host: in_host.clone(),
            hostname: in_host,
            port: in_port,
            tls_mode: in_tls,
            security: in_tls,
            socket_type: in_socket,
            auth_mechanism: in_auth,
            username_template: in_user_template,
        },
        outgoing: ServerConfig {
            host: out_host.clone(),
            hostname: out_host,
            port: out_port,
            tls_mode: out_tls,
            security: out_tls,
            socket_type: out_socket,
            auth_mechanism: out_auth,
            username_template: out_user_template,
        },
        incoming_protocol: protocol,
        tier,
        note: None,
    })
}

fn extract_server_block(xml: &str, tag_name: &str, server_type: &str) -> Option<String> {
    let target_type = server_type.to_ascii_lowercase();
    let mut search_idx = 0;
    while let Some(start_rel) = xml[search_idx..].find(&format!("<{tag_name}")) {
        let tag_start = search_idx + start_rel;
        let end_of_open = xml[tag_start..].find('>')? + tag_start;
        let open_tag = &xml[tag_start..=end_of_open];

        let open_tag_lower = open_tag.to_ascii_lowercase();
        let is_matching_type = open_tag_lower.contains(&format!("type=\"{target_type}\""))
            || open_tag_lower.contains(&format!("type='{target_type}'"));

        let close_tag = format!("</{tag_name}>");
        let block_end = xml[end_of_open..].find(&close_tag)? + end_of_open;

        if is_matching_type {
            return Some(xml[end_of_open + 1..block_end].to_string());
        }
        search_idx = block_end + close_tag.len();
    }
    None
}

fn extract_tag_content(xml: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{tag}");
    let close_tag = format!("</{tag}>");

    let start_pos = xml.find(&open_tag)?;
    let content_start = xml[start_pos..].find('>')? + start_pos + 1;
    let content_end = xml[content_start..].find(&close_tag)? + content_start;

    let content = xml[content_start..content_end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

pub fn split_email_parts(email: &str) -> (&str, &str) {
    if let Some((local, domain)) = email.split_once('@') {
        (local.trim(), domain.trim())
    } else {
        (email.trim(), "")
    }
}

pub fn expand_template(
    template: &str,
    email: &str,
    local_part: &str,
    domain_part: &str,
) -> String {
    template
        .replace("%EMAILADDRESS%", email)
        .replace("%EMAILLOCALPART%", local_part)
        .replace("%EMAILDOMAIN%", domain_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier1_builtin_gmail() {
        let config = AutodiscoverService::discover("user@gmail.com").expect("Gmail discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "imap.gmail.com");
        assert_eq!(config.imap_port(), 993);
        assert_eq!(config.incoming_tls_mode(), TlsMode::SslTls);
        assert_eq!(config.smtp_host(), "smtp.gmail.com");
        assert_eq!(config.smtp_port(), 465);
        assert_eq!(config.outgoing_tls_mode(), TlsMode::SslTls);
        assert_eq!(config.resolved_username(), "user@gmail.com");
    }

    #[test]
    fn test_tier1_builtin_googlemail() {
        let config = AutodiscoverService::discover("user@googlemail.com").expect("Googlemail discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "imap.gmail.com");
        assert_eq!(config.smtp_host(), "smtp.gmail.com");
    }

    #[test]
    fn test_tier1_builtin_outlook() {
        let config = AutodiscoverService::discover("user@outlook.de").expect("Outlook discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "outlook.office365.com");
        assert_eq!(config.imap_port(), 993);
        assert_eq!(config.incoming_tls_mode(), TlsMode::SslTls);
        assert_eq!(config.smtp_host(), "smtp-mail.outlook.com");
        assert_eq!(config.smtp_port(), 587);
        assert_eq!(config.outgoing_tls_mode(), TlsMode::Starttls);
    }

    #[test]
    fn test_tier1_builtin_icloud() {
        let config = AutodiscoverService::discover("test@icloud.com").expect("iCloud discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "imap.mail.me.com");
        assert_eq!(config.smtp_host(), "smtp.mail.me.com");
        assert_eq!(config.outgoing_tls_mode(), TlsMode::Starttls);
        assert_eq!(config.resolved_username(), "test");
    }

    #[test]
    fn test_tier1_builtin_posteo() {
        let config = AutodiscoverService::discover("person@posteo.de").expect("Posteo discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "posteo.de");
        assert_eq!(config.smtp_host(), "posteo.de");
        assert_eq!(config.smtp_port(), 465);
    }

    #[test]
    fn test_tier1_builtin_gmx() {
        let config = AutodiscoverService::discover("person@gmx.net").expect("GMX discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "imap.gmx.net");
        assert_eq!(config.smtp_host(), "mail.gmx.net");
        assert_eq!(config.smtp_port(), 587);
    }

    #[test]
    fn test_tier1_builtin_webde() {
        let config = AutodiscoverService::discover("person@web.de").expect("WEB.DE discover");
        assert_eq!(config.tier, DiscoveryTier::BuiltinProfile);
        assert_eq!(config.imap_host(), "imap.web.de");
        assert_eq!(config.smtp_host(), "smtp.web.de");
    }

    #[test]
    fn test_tier5_heuristic_unknown_domain() {
        let config =
            AutodiscoverService::discover_tier5_heuristic("alice@corp.internal", "corp.internal");
        assert_eq!(config.tier, DiscoveryTier::Heuristic);
        assert_eq!(config.imap_host(), "imap.corp.internal");
        assert_eq!(config.imap_port(), 993);
        assert_eq!(config.incoming_tls_mode(), TlsMode::SslTls);
        assert_eq!(config.smtp_host(), "smtp.corp.internal");
        assert_eq!(config.smtp_port(), 587);
        assert_eq!(config.outgoing_tls_mode(), TlsMode::Starttls);
        assert_eq!(config.resolved_username(), "alice@corp.internal");
    }

    #[test]
    fn test_parse_thunderbird_autoconfig_xml() {
        let sample_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example Corp Mail</displayName>
    <incomingServer type="imap">
      <hostname>mail.example.com</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

        let parsed = parse_thunderbird_autoconfig_xml(
            sample_xml,
            "bob@example.com",
            "example.com",
            DiscoveryTier::DomainAutoconfig,
        )
        .expect("Parse sample XML");

        assert_eq!(parsed.provider_name, "Example Corp Mail");
        assert_eq!(parsed.imap_host(), "mail.example.com");
        assert_eq!(parsed.imap_port(), 993);
        assert_eq!(parsed.incoming_tls_mode(), TlsMode::SslTls);
        assert_eq!(parsed.smtp_host(), "smtp.example.com");
        assert_eq!(parsed.smtp_port(), 587);
        assert_eq!(parsed.outgoing_tls_mode(), TlsMode::Starttls);
        assert_eq!(parsed.resolved_username(), "bob@example.com");
    }

    #[test]
    fn test_template_expansion_all_three() {
        let expanded = expand_template(
            "user: %EMAILLOCALPART% on %EMAILDOMAIN% (%EMAILADDRESS%)",
            "test@example.org",
            "test",
            "example.org",
        );
        assert_eq!(
            expanded,
            "user: test on example.org (test@example.org)"
        );
    }

    #[test]
    fn test_cancellation_token() {
        let cancel = Arc::new(AtomicBool::new(true));
        let res = AutodiscoverService::discover_cancelable("user@gmail.com", &cancel);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "Operation cancelled");
    }
}
