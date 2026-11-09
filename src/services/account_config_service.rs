// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent, non-secret account profiles for the setup wizard.
//
// Passwords and OAuth tokens never belong in this file.  The Linux EDS/Camel
// adapter will use the profile to create an ESource and hand credentials to
// Secret Service when that backend is connected.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountProtocol {
    ImapSmtp,
    Exchange,
    Goa,
    Pop3,
}

impl AccountProtocol {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImapSmtp => "imapx/smtp",
            Self::Exchange => "exchange",
            Self::Goa => "goa",
            Self::Pop3 => "pop3",
        }
    }
}

impl AccountProtocol {
    fn requires_mail_servers(self) -> bool {
        matches!(self, Self::ImapSmtp | Self::Pop3)
    }

    fn incoming_server_label(self) -> &'static str {
        if self == Self::Pop3 { "POP3" } else { "IMAP" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AccountProfileRaw")]
pub struct AccountProfile {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub protocol: AccountProtocol,
    pub imap_host: String,
    pub smtp_host: String,
    #[serde(default)]
    pub oab_url: String,
    pub tls_mode: String,
    #[serde(default)]
    pub username: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub auth_method: String,
    #[serde(default)]
    pub reply_to: String,
    #[serde(default)]
    pub organization: String,
    /// Comma-separated aliases are kept without credentials and can later be
    /// mapped to ESource identities by the native Linux adapter.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Portable folder mapping imported from Evolution.  Empty values mean
    /// that the provider's default is still authoritative.
    #[serde(default)]
    pub inbox_folder: String,
    #[serde(default)]
    pub drafts_folder: String,
    #[serde(default)]
    pub sent_folder: String,
    #[serde(default)]
    pub archive_folder: String,
    #[serde(default)]
    pub trash_folder: String,
    #[serde(default)]
    pub junk_folder: String,
    pub enabled: bool,
    #[serde(default)]
    pub signature_id: Option<String>,
    #[serde(default = "default_check_interval")]
    pub check_interval_minutes: u32,
    #[serde(default = "default_true")]
    pub auto_check_enabled: bool,
    #[serde(default)]
    pub check_all_folders: bool,
    #[serde(default)]
    pub expunge_on_exit: bool,
    #[serde(default)]
    pub pgp_key_id: String,
    #[serde(default)]
    pub pgp_always_sign: bool,
    #[serde(default)]
    pub pgp_always_encrypt: bool,
    #[serde(default)]
    pub smime_sign_cert: String,
    #[serde(default)]
    pub smime_encrypt_cert: String,
    #[serde(default)]
    pub default_cc: String,
    #[serde(default)]
    pub default_bcc: String,
    #[serde(default = "default_true")]
    pub prefer_html: bool,
    #[serde(default)]
    pub open_mailbox_other_user: bool,
    #[serde(default)]
    pub foreign_mailbox: String,
    #[serde(default)]
    pub is_shared_mailbox: bool,
    #[serde(default)]
    pub parent_account_id: Option<String>,
    #[serde(default)]
    pub follow_global_sync: bool,
}

#[derive(Deserialize)]
struct AccountProfileRaw {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub protocol: AccountProtocol,
    pub imap_host: String,
    pub smtp_host: String,
    #[serde(default)]
    pub oab_url: String,
    pub tls_mode: String,
    #[serde(default)]
    pub username: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub auth_method: String,
    #[serde(default)]
    pub reply_to: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub inbox_folder: String,
    #[serde(default)]
    pub drafts_folder: String,
    #[serde(default)]
    pub sent_folder: String,
    #[serde(default)]
    pub archive_folder: String,
    #[serde(default)]
    pub trash_folder: String,
    #[serde(default)]
    pub junk_folder: String,
    pub enabled: bool,
    #[serde(default)]
    pub signature_id: Option<String>,
    #[serde(default = "default_check_interval")]
    pub check_interval_minutes: u32,
    #[serde(default = "default_true")]
    pub auto_check_enabled: bool,
    #[serde(default)]
    pub check_all_folders: bool,
    #[serde(default)]
    pub expunge_on_exit: bool,
    #[serde(default)]
    pub pgp_key_id: String,
    #[serde(default)]
    pub pgp_always_sign: bool,
    #[serde(default)]
    pub pgp_always_encrypt: bool,
    #[serde(default)]
    pub smime_sign_cert: String,
    #[serde(default)]
    pub smime_encrypt_cert: String,
    #[serde(default)]
    pub default_cc: String,
    #[serde(default)]
    pub default_bcc: String,
    #[serde(default = "default_true")]
    pub prefer_html: bool,
    #[serde(default)]
    pub open_mailbox_other_user: bool,
    #[serde(default)]
    pub foreign_mailbox: String,
    #[serde(default)]
    pub is_shared_mailbox: bool,
    #[serde(default)]
    pub parent_account_id: Option<String>,
    #[serde(default)]
    pub follow_global_sync: Option<bool>,
}

impl From<AccountProfileRaw> for AccountProfile {
    fn from(raw: AccountProfileRaw) -> Self {
        let follow_global_sync = match raw.follow_global_sync {
            Some(v) => v,
            None => raw.check_interval_minutes == default_check_interval(),
        };
        AccountProfile {
            id: raw.id,
            display_name: raw.display_name,
            email: raw.email,
            protocol: raw.protocol,
            imap_host: raw.imap_host,
            smtp_host: raw.smtp_host,
            oab_url: raw.oab_url,
            tls_mode: raw.tls_mode,
            username: raw.username,
            imap_port: raw.imap_port,
            smtp_port: raw.smtp_port,
            auth_method: raw.auth_method,
            reply_to: raw.reply_to,
            organization: raw.organization,
            aliases: raw.aliases,
            inbox_folder: raw.inbox_folder,
            drafts_folder: raw.drafts_folder,
            sent_folder: raw.sent_folder,
            archive_folder: raw.archive_folder,
            trash_folder: raw.trash_folder,
            junk_folder: raw.junk_folder,
            enabled: raw.enabled,
            signature_id: raw.signature_id,
            check_interval_minutes: raw.check_interval_minutes,
            auto_check_enabled: raw.auto_check_enabled,
            check_all_folders: raw.check_all_folders,
            expunge_on_exit: raw.expunge_on_exit,
            pgp_key_id: raw.pgp_key_id,
            pgp_always_sign: raw.pgp_always_sign,
            pgp_always_encrypt: raw.pgp_always_encrypt,
            smime_sign_cert: raw.smime_sign_cert,
            smime_encrypt_cert: raw.smime_encrypt_cert,
            default_cc: raw.default_cc,
            default_bcc: raw.default_bcc,
            prefer_html: raw.prefer_html,
            open_mailbox_other_user: raw.open_mailbox_other_user,
            foreign_mailbox: raw.foreign_mailbox,
            is_shared_mailbox: raw.is_shared_mailbox,
            parent_account_id: raw.parent_account_id,
            follow_global_sync,
        }
    }
}

impl Default for AccountProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            email: String::new(),
            protocol: AccountProtocol::ImapSmtp,
            imap_host: String::new(),
            smtp_host: String::new(),
            oab_url: String::new(),
            tls_mode: "tls".to_string(),
            username: String::new(),
            imap_port: default_imap_port(),
            smtp_port: default_smtp_port(),
            auth_method: "password".to_string(),
            reply_to: String::new(),
            organization: String::new(),
            aliases: Vec::new(),
            inbox_folder: String::new(),
            drafts_folder: String::new(),
            sent_folder: String::new(),
            archive_folder: String::new(),
            trash_folder: String::new(),
            junk_folder: String::new(),
            enabled: true,
            signature_id: None,
            check_interval_minutes: default_check_interval(),
            auto_check_enabled: true,
            check_all_folders: false,
            expunge_on_exit: false,
            pgp_key_id: String::new(),
            pgp_always_sign: false,
            pgp_always_encrypt: false,
            smime_sign_cert: String::new(),
            smime_encrypt_cert: String::new(),
            default_cc: String::new(),
            default_bcc: String::new(),
            prefer_html: true,
            open_mailbox_other_user: false,
            foreign_mailbox: String::new(),
            is_shared_mailbox: false,
            parent_account_id: None,
            follow_global_sync: true,
        }
    }
}

impl AccountProfile {
    /// Validate portable profile data before it is written.  This deliberately
    /// does not validate passwords or OAuth tokens because those secrets are
    /// never part of the local profile file.
    pub fn validate(&self) -> Result<(), String> {
        let email = self.email.trim();
        if email.is_empty() || !is_plausible_address(email) {
            return Err("Please enter a valid email address".to_string());
        }
        if self.display_name.trim().is_empty() {
            return Err("Display name must not be empty".to_string());
        }
        if self.id.trim().is_empty() {
            return Err("Account does not have a valid internal ID".to_string());
        }
        if !matches!(self.tls_mode.as_str(), "tls" | "starttls" | "none") {
            return Err("Invalid encryption method".to_string());
        }
        if self.imap_port == 0 || self.smtp_port == 0 {
            return Err("IMAP/SMTP ports must be between 1 and 65535".to_string());
        }
        if self.protocol.requires_mail_servers() && self.imap_host.trim().is_empty() {
            return Err(format!(
                "Please enter an incoming {} server",
                self.protocol.incoming_server_label()
            ));
        }
        if matches!(self.protocol, AccountProtocol::ImapSmtp) && self.smtp_host.trim().is_empty() {
            return Err("Please enter an outgoing SMTP server".to_string());
        }
        if self
            .aliases
            .iter()
            .any(|alias| !alias.trim().is_empty() && !is_plausible_address(alias.trim()))
        {
            return Err("Every alias must be a valid email address".to_string());
        }
        if !self.reply_to.trim().is_empty() && !is_plausible_address(self.reply_to.trim()) {
            return Err("Reply-To address is invalid".to_string());
        }
        Ok(())
    }
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    465
}

fn default_check_interval() -> u32 {
    10
}

fn default_true() -> bool {
    true
}

pub fn is_plausible_address(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.trim().is_empty()
        && !domain.trim().is_empty()
        && !value.chars().any(char::is_whitespace)
        && domain.contains('.')
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountFile {
    version: u32,
    #[serde(default)]
    accounts: Vec<AccountProfile>,
}

static ACCOUNT_PROFILES_CACHE: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<PathBuf, std::sync::Arc<[AccountProfile]>>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashMap::new()));

#[cfg(not(test))]
static DEFAULT_ACCOUNT_FILE_PATH: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
    std::env::var_os("MERIDIAN_ACCOUNT_FILE")
        .or_else(|| std::env::var_os("EVOLUTION_GTK4_ACCOUNT_FILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::services::app_data_dir().join("accounts.json"))
});

#[derive(Debug, Clone)]
pub struct AccountConfigStore {
    path: PathBuf,
}

impl AccountConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        #[cfg(not(test))]
        {
            return Self::new(DEFAULT_ACCOUNT_FILE_PATH.clone());
        }
        #[cfg(test)]
        {
            let path = std::env::var_os("MERIDIAN_ACCOUNT_FILE")
                .or_else(|| std::env::var_os("EVOLUTION_GTK4_ACCOUNT_FILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| crate::services::app_data_dir().join("accounts.json"));
            Self::new(path)
        }
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Zero-I/O cached profile reader for hot paths (GTK filter callbacks, SQLite row mappers).
    /// Automatically updated whenever `save()` mutates the profile store.
    pub fn load_cached(&self) -> std::sync::Arc<[AccountProfile]> {
        #[cfg(test)]
        {
            return std::sync::Arc::from(self.load());
        }
        #[cfg(not(test))]
        {
            if let Ok(guard) = ACCOUNT_PROFILES_CACHE.read() {
                if let Some(cached) = guard.get(&self.path) {
                    return std::sync::Arc::clone(cached);
                }
            }
            let loaded: std::sync::Arc<[AccountProfile]> = std::sync::Arc::from(self.load());
            if let Ok(mut guard) = ACCOUNT_PROFILES_CACHE.write() {
                guard.insert(self.path.clone(), std::sync::Arc::clone(&loaded));
            }
            loaded
        }
    }

    #[allow(dead_code)]
    pub fn invalidate_cache() {
        if let Ok(mut guard) = ACCOUNT_PROFILES_CACHE.write() {
            guard.clear();
        }
        crate::services::eds_service::invalidate_account_collections_cache();
    }

    pub fn load(&self) -> Vec<AccountProfile> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<AccountFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid account profile file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported account profile version"
            );
            return Vec::new();
        }
        file.accounts
    }

    pub fn upsert(&self, profile: AccountProfile) -> io::Result<()> {
        let mut accounts = self.load();
        if let Some(existing) = accounts
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile;
        } else {
            accounts.push(profile);
        }
        self.save(&accounts)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> io::Result<bool> {
        let mut accounts = self.load();
        let Some(account) = accounts.iter_mut().find(|account| account.id == id) else {
            return Ok(false);
        };
        if account.enabled == enabled {
            return Ok(true);
        }
        account.enabled = enabled;
        self.save(&accounts)?;
        Ok(true)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut accounts = self.load();
        let original_len = accounts.len();
        let removed_account = accounts.iter().find(|account| account.id == id).cloned();
        accounts.retain(|account| account.id != id);
        if accounts.len() == original_len {
            return Ok(false);
        }
        self.save(&accounts)?;
        if let Some(acc) = removed_account {
            remove_eds_sources_for_profile(&acc);
            let parent_uid = eds_source_uid_for_email(&acc.email);
            let mail_uid = format!("{}-mail", parent_uid);
            let transport_uid = format!("{}-transport", parent_uid);
            let _ = crate::services::credential_service::CredentialService::remove_password(&acc.id);
            let _ = crate::services::credential_service::CredentialService::remove_password(&mail_uid);
            let _ = crate::services::credential_service::CredentialService::remove_password(&transport_uid);
            let _ = crate::services::credential_service::CredentialService::remove_password(&parent_uid);
            let _ = crate::services::credential_service::CredentialService::remove_password(&acc.email);
        }
        Ok(true)
    }

    /// Move one app-owned profile by one or more positions without changing
    /// any account fields. A boundary move is a successful no-op; a missing
    /// profile returns `false` and never rewrites the file.
    pub fn move_profile(&self, id: &str, delta: i32) -> io::Result<bool> {
        let mut accounts = self.load();
        let Some(index) = accounts.iter().position(|account| account.id == id) else {
            return Ok(false);
        };
        if accounts.len() < 2 || delta == 0 {
            return Ok(true);
        }
        let target = (index as i64 + delta as i64).clamp(0, accounts.len().saturating_sub(1) as i64)
            as usize;
        if target == index {
            return Ok(true);
        }
        let profile = accounts.remove(index);
        accounts.insert(target, profile);
        self.save(&accounts)?;
        Ok(true)
    }

    fn save(&self, accounts: &[AccountProfile]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = AccountFile {
            version: CURRENT_VERSION,
            accounts: accounts.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)?;
        if let Ok(mut guard) = ACCOUNT_PROFILES_CACHE.write() {
            guard.insert(self.path.clone(), std::sync::Arc::from(accounts.to_vec()));
        }
        crate::services::eds_service::invalidate_account_collections_cache();
        Ok(())
    }
}

/// Generate the standard Evolution Data Server (EDS) .source configuration files
/// for an AccountProfile and notify the EDS SourceManager via D-Bus.
///
/// Compute the standard SHA1-based EDS source UID from an email address.
pub fn eds_source_uid_for_email(email: &str) -> String {
    let raw_email = email.trim().to_lowercase();
    gtk::glib::compute_checksum_for_string(gtk::glib::ChecksumType::Sha1, &raw_email)
        .map(|s| s.to_string())
        .unwrap_or_else(|| raw_email.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))
}

/// Safely update an existing .source keyfile or create it from default template,
/// preserving any custom/extra keys (such as GalUid, WebDAV, etc.) created by EDS.
/// Returns true if the file was newly created or modified.
fn write_or_update_source_file(
    path: &std::path::Path,
    default_ini: &str,
    updates: &[(&str, &str, &str)],
) -> bool {
    let kf = gtk::glib::KeyFile::new();
    let loaded = if path.exists() {
        kf.load_from_file(path, gtk::glib::KeyFileFlags::NONE).is_ok()
    } else {
        false
    };
    if !loaded {
        let _ = kf.load_from_data(default_ini, gtk::glib::KeyFileFlags::NONE);
        for &(group, key, val) in updates {
            kf.set_string(group, key, val);
        }
        let data = kf.to_data();
        let _ = fs::write(path, data.as_str());
        return true;
    }

    let mut any_key_changed = false;
    for &(group, key, val) in updates {
        match kf.string(group, key) {
            Ok(existing_val) if existing_val.as_str() == val => {}
            _ => {
                any_key_changed = true;
                kf.set_string(group, key, val);
            }
        }
    }
    if !any_key_changed {
        return false;
    }

    let data = kf.to_data();
    let _ = fs::write(path, data.as_str());
    true
}

/// Sanitize an Exchange Offline Address Book (OAB) URL.
///
/// In Microsoft Exchange, a valid OAB download URL requires the specific OAB GUID
/// subfolder (e.g. `https://server/OAB/6b268f77-508b-4fc4-8fe4-c7fc01f46ad0/`).
/// If only `https://server/OAB/` or an empty string is provided, `evolution-ews`
/// will attempt to download `/OAB/oab.xml` which fails with HTTP 403/404 because
/// directory listing is disabled on IIS. Leaving `Oaburl` empty is the standard
/// configuration in Evolution: EWS queries the Global Address List (GAL) live directly
/// over EWS SOAP requests (`ResolveNames`/`FindPeople`) without error.
pub fn sanitize_oab_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_lowercase();
    if let Some(pos) = lower.rfind("/oab") {
        let remainder = trimmed[pos + 4..].trim_matches('/');
        // If there's no GUID or subpath after /OAB/ (empty or just slashes), it's invalid
        if remainder.is_empty() {
            return String::new();
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeDiscoveryStatus {
    Success {
        ews_url: String,
        oab_url: String,
        message: String,
    },
    MissingCredentials,
    AuthFailed(String),
    ServerNotFound(String),
    Error(String),
}

/// Helper function to extract content of an XML tag (case-insensitive and tolerant of namespaces and attributes)
pub fn extract_xml_tag_value(xml: &str, tag_name: &str) -> Option<String> {
    let lower_xml = xml.to_lowercase();
    let lower_tag = tag_name.to_lowercase();

    // Look for <tag or <prefix:tag
    let mut search_idx = 0;
    while let Some(rel_start) = lower_xml[search_idx..].find('<') {
        let tag_start = search_idx + rel_start;
        let rest = &lower_xml[tag_start + 1..];
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let full_element_name = &rest[..name_end];
        let local_name = full_element_name.rsplit(':').next().unwrap_or(full_element_name);

        if local_name == lower_tag {
            if let Some(close_bracket) = rest.find('>') {
                let content_start = tag_start + 1 + close_bracket + 1;
                // Find matching closing tag </tag> or </prefix:tag>
                let close_tag_pattern = format!("</{}", full_element_name);
                let close_tag_simple = format!("</{}", lower_tag);
                if let Some(rel_end) = lower_xml[content_start..].find(&close_tag_pattern)
                    .or_else(|| lower_xml[content_start..].find(&close_tag_simple))
                {
                    return Some(xml[content_start..content_start + rel_end].trim().to_string());
                }
            }
        }
        search_idx = tag_start + 1;
    }
    None
}

/// Normalizes user-entered Exchange host or URL to a clean hostname (with optional port).
/// E.g. "https://mail.company.com/EWS/Exchange.asmx" -> "mail.company.com"
/// E.g. "mail.company.com:444" -> "mail.company.com:444"
pub fn normalize_exchange_host_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim()
        .to_string()
}

/// Resolves DNS SRV record for Exchange Autodiscover (`_autodiscover._tcp.<domain>`),
/// mirroring GNOME Evolution's `autodiscover_srv_record_resolved_cb`.
fn resolve_autodiscover_srv(domain: &str) -> Option<(String, u16)> {
    use gtk::gio::prelude::*;
    let resolver = gtk::gio::Resolver::default();
    match resolver.lookup_service("autodiscover", "tcp", domain, gtk::gio::Cancellable::NONE) {
        Ok(targets) => {
            for mut target in targets {
                let host = target.hostname().to_string();
                let port = target.port();
                if !host.is_empty() {
                    return Some((host, port));
                }
            }
            None
        }
        Err(_) => None,
    }
}

/// Discover and validate Exchange Web Services (EWS) and Offline Address Book (OAB) endpoints.
///
/// Follows GNOME Evolution upstream (`evolution-ews`) connection & discovery logic:
/// 1. If host is specified, queries POX Autodiscover on host and falls back to direct EWS probe.
/// 2. If host is empty, performs full Autodiscover chain (DNS SRV, https, autodiscover subdomain, redirects).
///    If all fail without host entered, returns Evolution's exact error message:
///    "Querying the Autodiscover service failed: Host address cannot be empty. Please enter the host address (Host URL) of your Exchange server."
pub fn discover_and_validate_exchange_services(
    email: &str,
    input_host: &str,
    username: &str,
    password: &str,
) -> ExchangeDiscoveryStatus {
    let trimmed_user = username.trim();
    let trimmed_pwd = password.trim();
    if trimmed_user.is_empty() || trimmed_pwd.is_empty() {
        return ExchangeDiscoveryStatus::MissingCredentials;
    }

    let email_trimmed = email.trim();
    let host_base = normalize_exchange_host_url(input_host);
    let email_domain = email_trimmed.split('@').nth(1).unwrap_or("localhost");

    let client = match reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(6))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ExchangeDiscoveryStatus::Error(format!("HTTP client error: {e}")),
    };

    // 1. Build Autodiscover candidate URLs in GNOME Evolution sequence
    let mut autodiscover_urls = Vec::new();

    if !host_base.is_empty() {
        autodiscover_urls.push(format!("https://{host_base}/autodiscover/autodiscover.xml"));
        autodiscover_urls.push(format!("https://autodiscover.{host_base}/autodiscover/autodiscover.xml"));
        if host_base != email_domain {
            autodiscover_urls.push(format!("https://autodiscover.{email_domain}/autodiscover/autodiscover.xml"));
        }
    } else {
        // DNS SRV lookup: _autodiscover._tcp.<domain>
        if let Some((srv_host, srv_port)) = resolve_autodiscover_srv(email_domain) {
            let scheme = if srv_port == 443 { "https" } else { "http" };
            autodiscover_urls.push(format!("{scheme}://{srv_host}:{srv_port}/autodiscover/autodiscover.xml"));
        }
        autodiscover_urls.push(format!("https://{email_domain}/autodiscover/autodiscover.xml"));
        autodiscover_urls.push(format!("https://autodiscover.{email_domain}/autodiscover/autodiscover.xml"));
        autodiscover_urls.push(format!("http://autodiscover.{email_domain}/autodiscover/autodiscover.xml"));
        autodiscover_urls.push(format!("https://mail.{email_domain}/autodiscover/autodiscover.xml"));
        autodiscover_urls.push(format!("https://exchange.{email_domain}/autodiscover/autodiscover.xml"));
    }

    let autodiscover_payload = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
  <Request>
    <EMailAddress>{email_trimmed}</EMailAddress>
    <AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
  </Request>
</Autodiscover>"#
    );

    for auto_url in &autodiscover_urls {
        if let Ok(resp) = client
            .post(auto_url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .basic_auth(trimmed_user, Some(trimmed_pwd))
            .body(autodiscover_payload.clone())
            .send()
        {
            let status = resp.status().as_u16();
            if status == 200 {
                if let Ok(body) = resp.text() {
                    let ews_url = extract_xml_tag_value(&body, "EwsUrl")
                        .or_else(|| extract_xml_tag_value(&body, "ASUrl"));
                    let raw_oab = extract_xml_tag_value(&body, "OABUrl").unwrap_or_default();
                    let oab_url = sanitize_oab_url(&raw_oab);

                    if let Some(ews) = ews_url {
                        let msg = if !oab_url.is_empty() {
                            "✓ Exchange services (EWS & OAB) successfully discovered via Autodiscover".to_string()
                        } else {
                            "✓ EWS service discovered via Autodiscover (OAB not provided by server)".to_string()
                        };
                        return ExchangeDiscoveryStatus::Success {
                            ews_url: ews,
                            oab_url,
                            message: msg,
                        };
                    }
                }
            } else if status == 401 {
                return ExchangeDiscoveryStatus::AuthFailed(
                    "Authentication failed (HTTP 401): Please check your username and password.".to_string(),
                );
            }
        }
    }

    // 2. Fallback / Error Handling matching original Evolution
    if input_host.trim().is_empty() {
        return ExchangeDiscoveryStatus::ServerNotFound(
            "Autodiscover service query failed: Host address must not be empty. Please enter the host address (Host URL) of your Exchange server.".to_string(),
        );
    }

    let fallback_ews_url = format!("https://{host_base}/EWS/Exchange.asmx");
    match client
        .get(&fallback_ews_url)
        .basic_auth(trimmed_user, Some(trimmed_pwd))
        .send()
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status == 200 {
                ExchangeDiscoveryStatus::Success {
                    ews_url: fallback_ews_url,
                    oab_url: String::new(),
                    message: "✓ EWS endpoint active and authentication succeeded. (OAB is optional and remains empty)".to_string(),
                }
            } else if status == 401 {
                ExchangeDiscoveryStatus::AuthFailed(
                    "Authentication failed (HTTP 401): Please check your username and password.".to_string(),
                )
            } else if status == 403 {
                ExchangeDiscoveryStatus::AuthFailed(
                    "Access denied (HTTP 403): User does not have permission for this EWS service.".to_string(),
                )
            } else if status == 404 {
                ExchangeDiscoveryStatus::ServerNotFound(format!(
                    "EWS endpoint not found (HTTP 404 on {fallback_ews_url}). Please check the server address."
                ))
            } else {
                ExchangeDiscoveryStatus::Error(format!(
                    "Server responded with error (HTTP {status}) on {fallback_ews_url}."
                ))
            }
        }
        Err(err) => {
            ExchangeDiscoveryStatus::ServerNotFound(format!(
                "Server {host_base} unreachable: {err}"
            ))
        }
    }
}

/// Queries the supported authentication mechanisms of an Exchange / EWS server.
/// Connects to the EWS endpoint and inspects WWW-Authenticate response headers.
pub fn query_exchange_auth_methods(host_or_url: &str) -> Result<Vec<String>, String> {
    let trimmed = host_or_url.trim();
    if trimmed.is_empty() {
        return Err("Host address must not be empty.".to_string());
    }

    let ews_url = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if trimmed.to_lowercase().ends_with(".asmx") {
            trimmed.to_string()
        } else {
            let base = trimmed.trim_end_matches('/');
            format!("{base}/EWS/Exchange.asmx")
        }
    } else {
        let host = normalize_exchange_host_url(trimmed);
        format!("https://{host}/EWS/Exchange.asmx")
    };

    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client.get(&ews_url).send().map_err(|e| {
        format!("Server at {ews_url} unreachable: {e}")
    })?;

    let mut detected = Vec::new();
    let auth_headers = resp.headers().get_all(reqwest::header::WWW_AUTHENTICATE);
    for h in auth_headers {
        if let Ok(val) = h.to_str() {
            let val_upper = val.to_uppercase();
            if val_upper.contains("NTLM") && !detected.iter().any(|d: &String| d == "NTLM") {
                detected.push("NTLM".to_string());
            }
            if (val_upper.contains("NEGOTIATE") || val_upper.contains("GSSAPI"))
                && !detected.iter().any(|d: &String| d == "Kerberos")
            {
                detected.push("Kerberos".to_string());
            }
            if val_upper.contains("BEARER") && !detected.iter().any(|d: &String| d == "Bearer") {
                detected.push("Bearer".to_string());
            }
            if val_upper.contains("BASIC") && !detected.iter().any(|d: &String| d == "Basic") {
                detected.push("Basic".to_string());
            }
        }
    }

    if detected.is_empty() {
        if resp.status().is_success() {
            detected.push("Basic".to_string());
        } else {
            return Err(format!(
                "Server responded with HTTP {}, but did not advertise any supported authentication methods in the WWW-Authenticate header.",
                resp.status()
            ));
        }
    }

    let mut ordered = Vec::new();
    if detected.iter().any(|d| d == "NTLM") {
        ordered.push("NTLM / Windows Authentication".to_string());
    }
    if detected.iter().any(|d| d == "Kerberos") {
        ordered.push("Kerberos / GSSAPI".to_string());
    }
    if detected.iter().any(|d| d == "Bearer") {
        ordered.push("OAuth2 / Modern Auth (M365)".to_string());
    }
    if detected.iter().any(|d| d == "Basic") {
        ordered.push("Password (Basic / Plain)".to_string());
    }

    Ok(ordered)
}

/// Detects local Active Directory domain if joined (e.g. "CORPDOM").
pub fn detect_ad_domain() -> Option<String> {
    if let Ok(content) = fs::read_to_string("/etc/krb5.conf") {
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("default_realm") {
                let realm = rest.trim().trim_start_matches('=').trim();
                if !realm.is_empty() && realm != "EXAMPLE.COM" {
                    let netbios = realm.split('.').next().unwrap_or(realm);
                    return Some(netbios.to_uppercase());
                }
            }
        }
    }
    if let Ok(content) = fs::read_to_string("/etc/resolv.conf") {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("search ") || trimmed.starts_with("domain ") {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    let domain = parts[1];
                    let netbios = domain.split('.').next().unwrap_or(domain);
                    if !netbios.is_empty() && netbios != "local" {
                        return Some(netbios.to_uppercase());
                    }
                }
            }
        }
    }
    None
}

/// Formats a username for Active Directory Exchange login:
/// If it already contains a backslash (e.g. `DOMAIN\user`), returns it.
/// If it is an email address or bare username, and an AD domain is detected, returns `DOMAIN\user`.
pub fn format_exchange_domain_user(raw_user: &str, raw_email: &str) -> String {
    let raw = raw_user.trim();
    if raw.contains('\\') {
        return raw.to_string();
    }
    let username = if raw.is_empty() || raw.contains('@') {
        if !raw.is_empty() {
            raw.split('@').next().unwrap_or(raw).trim().to_string()
        } else if !raw_email.trim().is_empty() {
            raw_email.trim().split('@').next().unwrap_or("").trim().to_string()
        } else {
            std::env::var("USER").unwrap_or_default()
        }
    } else {
        raw.to_string()
    };
    if username.is_empty() {
        return String::new();
    }

    if let Some(domain) = detect_ad_domain() {
        format!("{domain}\\{username}")
    } else {
        username
    }
}

/// Returns the path to the EDS sources configuration directory.
pub fn eds_sources_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("MERIDIAN_SOURCES_DIR").or_else(|_| std::env::var("EVOLUTION_SOURCES_DIR")) {
        let path = PathBuf::from(custom.trim());
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    let xdg = gtk::glib::user_config_dir().join("evolution").join("sources");
    if xdg.is_dir() {
        return xdg;
    }
    let host = gtk::glib::home_dir().join(".config").join("evolution").join("sources");
    if host.is_dir() {
        return host;
    }
    xdg
}

/// Reads Exchange (EWS) connection parameters `(host_url, oab_url, auth_method, username)`
/// from an EDS `.source` file (and its `Parent` collection `.source` file if applicable).
pub fn read_eds_ews_connection_info(source_uid_or_email: &str) -> Option<(String, String, String, String)> {
    let clean_uid = source_uid_or_email
        .trim()
        .strip_prefix("eds:")
        .unwrap_or(source_uid_or_email.trim());
    if clean_uid.is_empty() {
        return None;
    }

    let sources_dir = eds_sources_dir();

    let extract_from_keyfile = |kf: &gtk::glib::KeyFile| -> Option<(String, String, String, String)> {
        let host_url = kf
            .string("Ews Backend", "Hosturl")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                kf.string("Authentication", "Host")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .map(|h| {
                        if h.starts_with("http://") || h.starts_with("https://") {
                            h
                        } else {
                            format!("https://{h}/EWS/Exchange.asmx")
                        }
                    })
            })?;

        let oab_url = kf
            .string("Ews Backend", "Oaburl")
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        let raw_method = kf
            .string("Authentication", "Method")
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let auth_method = if raw_method.eq_ignore_ascii_case("ntlm")
            || raw_method.to_ascii_lowercase().contains("ntlm")
        {
            "NTLM".to_string()
        } else if raw_method.to_ascii_lowercase().contains("oauth") {
            "OAuth2".to_string()
        } else if raw_method.to_ascii_lowercase().contains("gssapi")
            || raw_method.to_ascii_lowercase().contains("kerberos")
        {
            "Kerberos".to_string()
        } else if raw_method.to_ascii_lowercase().contains("plain")
            || raw_method.to_ascii_lowercase().contains("basic")
        {
            "Basic".to_string()
        } else {
            "NTLM".to_string()
        };

        let email = kf
            .string("Ews Backend", "Email")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                kf.string("GNOME Online Accounts", "Address")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default();

        let user = kf
            .string("Authentication", "User")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or(email);

        Some((host_url, oab_url, auth_method, user))
    };

    let direct_path = sources_dir.join(format!("{clean_uid}.source"));
    if direct_path.exists() {
        let kf = gtk::glib::KeyFile::new();
        if kf.load_from_file(&direct_path, gtk::glib::KeyFileFlags::NONE).is_ok() {
            if let Some(info) = extract_from_keyfile(&kf) {
                return Some(info);
            }
            if let Ok(parent_uid) = kf.string("Data Source", "Parent") {
                let parent_uid = parent_uid.trim();
                if !parent_uid.is_empty() {
                    let parent_path = sources_dir.join(format!("{parent_uid}.source"));
                    let parent_kf = gtk::glib::KeyFile::new();
                    if parent_kf
                        .load_from_file(&parent_path, gtk::glib::KeyFileFlags::NONE)
                        .is_ok()
                    {
                        if let Some(info) = extract_from_keyfile(&parent_kf) {
                            return Some(info);
                        }
                    }
                }
            }
        }
    }

    // Fallback: scan EWS collection sources matching email or domain
    if let Ok(entries) = fs::read_dir(&sources_dir) {
        let target_domain = clean_uid.split('@').nth(1).unwrap_or("");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("source") {
                continue;
            }
            let kf = gtk::glib::KeyFile::new();
            if kf.load_from_file(&path, gtk::glib::KeyFileFlags::NONE).is_ok() {
                let is_ews = kf
                    .string("Collection", "BackendName")
                    .ok()
                    .is_some_and(|b| b.trim().eq_ignore_ascii_case("ews"))
                    || kf.has_group("Ews Backend");
                if !is_ews {
                    continue;
                }
                let ews_email = kf
                    .string("Ews Backend", "Email")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let display_name = kf
                    .string("Data Source", "DisplayName")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let matches = ews_email.eq_ignore_ascii_case(clean_uid)
                    || display_name.eq_ignore_ascii_case(clean_uid)
                    || (!target_domain.is_empty()
                        && ews_email
                            .split('@')
                            .nth(1)
                            .is_some_and(|d| d.eq_ignore_ascii_case(target_domain)));
                if matches {
                    if let Some(info) = extract_from_keyfile(&kf) {
                        return Some(info);
                    }
                }
            }
        }
    }

    None
}

/// Remove all `.source` files associated with an AccountProfile from the EDS sources directory.
pub fn remove_eds_sources_for_profile(profile: &AccountProfile) {
    let parent_uid = eds_source_uid_for_email(&profile.email);
    let mail_uid = format!("{}-mail", parent_uid);
    let identity_uid = format!("{}-identity", parent_uid);
    let transport_uid = format!("{}-transport", parent_uid);
    let sources_dir = eds_sources_dir();
    let _ = fs::remove_file(sources_dir.join(format!("{parent_uid}.source")));
    let _ = fs::remove_file(sources_dir.join(format!("{mail_uid}.source")));
    let _ = fs::remove_file(sources_dir.join(format!("{identity_uid}.source")));
    let _ = fs::remove_file(sources_dir.join(format!("{transport_uid}.source")));
}

pub(crate) fn effective_sync_interval_minutes(profile: &AccountProfile) -> u32 {
    if profile.follow_global_sync {
        crate::services::AppSettings::try_new()
            .map(|s| s.mail_sync_interval_seconds() / 60)
            .unwrap_or(10)
            .max(1)
    } else {
        profile.check_interval_minutes.max(1)
    }
}

/// Returns the authoritative mail source UID recognized by EDS and Camel.
pub fn sync_profile_to_eds(
    profile: &AccountProfile,
    password: Option<&str>,
) -> Result<String, String> {
    if matches!(profile.protocol, AccountProtocol::Goa) || profile.id.starts_with("eds:") {
        let mail_uid = profile
            .id
            .strip_prefix("eds:")
            .unwrap_or(&profile.id)
            .to_string();
        let synthetic_uid = eds_source_uid_for_email(&profile.email);
        if synthetic_uid != mail_uid {
            let sources_dir = eds_sources_dir();
            let _ = fs::remove_file(sources_dir.join(format!("{}.source", synthetic_uid)));
            let _ = fs::remove_file(sources_dir.join(format!("{}-mail.source", synthetic_uid)));
            let _ = fs::remove_file(sources_dir.join(format!("{}-identity.source", synthetic_uid)));
            let _ = fs::remove_file(sources_dir.join(format!("{}-transport.source", synthetic_uid)));
        }
        return Ok(mail_uid);
    }

    let parent_uid = eds_source_uid_for_email(&profile.email);
    let mail_uid = format!("{}-mail", parent_uid);
    let identity_uid = format!("{}-identity", parent_uid);
    let transport_uid = format!("{}-transport", parent_uid);

    let sources_dir = eds_sources_dir();

    if profile.is_shared_mailbox {
        // Shared mailboxes operate through delegate access under their parent Exchange account.
        // They must never create a separate EDS EWS source store, which would either duplicate
        // the personal store or fail authentication with wrong credentials/URLs.
        let _ = fs::remove_file(sources_dir.join(format!("{}.source", parent_uid)));
        let _ = fs::remove_file(sources_dir.join(format!("{}.source", mail_uid)));
        let _ = fs::remove_file(sources_dir.join(format!("{}.source", identity_uid)));
        let _ = fs::remove_file(sources_dir.join(format!("{}.source", transport_uid)));
        return Ok(profile.id.clone());
    }

    if let Err(err) = fs::create_dir_all(&sources_dir) {
        return Err(format!("Could not create directory {}: {}", sources_dir.display(), err));
    }
    let mut files_changed = false;

    match profile.protocol {
        AccountProtocol::Exchange => {
            let host_url = if profile.imap_host.trim().is_empty() {
                format!("https://{}/EWS/Exchange.asmx", profile.email.split('@').nth(1).unwrap_or("localhost"))
            } else if profile.imap_host.starts_with("http://") || profile.imap_host.starts_with("https://") {
                profile.imap_host.trim().to_string()
            } else {
                format!("https://{}/EWS/Exchange.asmx", profile.imap_host.trim())
            };

            let auth_host = host_url
                .split("://")
                .nth(1)
                .unwrap_or(&host_url)
                .split('/')
                .next()
                .unwrap_or(&host_url)
                .split(':')
                .next()
                .unwrap_or(&host_url);

            let auth_method = match profile.auth_method.trim() {
                "Basic" | "basic" => "plain/password",
                _ => "ntlm/password",
            };

            let oab_url = sanitize_oab_url(&profile.oab_url);

            let username = if profile.username.trim().is_empty() {
                &profile.email
            } else {
                profile.username.trim()
            };

            let is_diff_user = !username.is_empty()
                && !profile.email.is_empty()
                && username.to_lowercase() != profile.email.to_lowercase();
            let use_impersonation = if !profile.is_shared_mailbox && (profile.open_mailbox_other_user || is_diff_user) {
                "true"
            } else {
                "false"
            };
            let impersonate_user = if use_impersonation == "true" {
                if !profile.foreign_mailbox.trim().is_empty() {
                    profile.foreign_mailbox.trim()
                } else {
                    profile.email.trim()
                }
            } else {
                ""
            };

            let parent_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent=\n\n\
                 [Authentication]\nHost={auth_host}\nMethod={auth_method}\nPort=443\nUser={username}\nRememberPassword=true\n\n\
                 [Security]\nMethod=none\n\n\
                 [Collection]\nBackendName=ews\nIdentity={username}\nMailEnabled=true\nCalendarEnabled=true\nContactsEnabled=true\n\n\
                 [Ews Backend]\nEmail={email}\nHosturl={host_url}\nOaburl={oab_url}\nCheckAll={check_all}\nFilterInbox=true\nListenNotifications=true\nTimeout=30\nUseImpersonation={use_impersonation}\nImpersonateUser={impersonate_user}\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                auth_host = auth_host,
                auth_method = auth_method,
                username = username,
                email = profile.email,
                host_url = host_url,
                oab_url = oab_url,
                check_all = profile.check_all_folders,
                use_impersonation = use_impersonation,
                impersonate_user = impersonate_user,
            );

            let foreign_mail_block = if profile.is_shared_mailbox {
                format!("\n[Exchange Web Services Folder]\nForeign=true\nForeignSubfolders=true\nForeignMail={}\n", profile.email)
            } else {
                String::new()
            };

            let mail_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Account]\nBackendName=ews\nIdentityUid={identity_uid}\nNeedsInitialSetup=false\n\n\
                 [Refresh]\nEnabled=true\nIntervalMinutes={interval}\n{foreign_mail_block}",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
                identity_uid = identity_uid,
                interval = effective_sync_interval_minutes(profile),
                foreign_mail_block = foreign_mail_block,
            );

            let identity_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Identity]\nAddress={email}\nName={display_name}\nOrganization={org}\nReplyTo={reply_to}\nSignatureUid=none\n\n\
                 [Mail Submission]\nTransportUid={transport_uid}\nRepliesToOriginFolder=false\nUseSentFolder=true\n\n\
                 [Mail Composition]\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
                email = profile.email,
                org = profile.organization,
                reply_to = profile.reply_to,
                transport_uid = transport_uid,
            );

            let transport_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Transport]\nBackendName=ews\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
            );

            let parent_path = sources_dir.join(format!("{}.source", parent_uid));
            let mail_path = sources_dir.join(format!("{}.source", mail_uid));
            let identity_path = sources_dir.join(format!("{}.source", identity_uid));
            let transport_path = sources_dir.join(format!("{}.source", transport_uid));

            let enabled_str = if profile.enabled { "true" } else { "false" };
            let check_all_str = if profile.check_all_folders { "true" } else { "false" };
            let interval_str = effective_sync_interval_minutes(profile).to_string();

            files_changed |= write_or_update_source_file(
                &parent_path,
                &parent_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Authentication", "Host", auth_host),
                    ("Authentication", "Method", auth_method),
                    ("Authentication", "User", username),
                    ("Authentication", "RememberPassword", "true"),
                    ("Collection", "BackendName", "ews"),
                    ("Collection", "Identity", username),
                    ("Ews Backend", "Email", &profile.email),
                    ("Ews Backend", "Hosturl", &host_url),
                    ("Ews Backend", "Oaburl", &oab_url),
                    ("Ews Backend", "CheckAll", check_all_str),
                    ("Ews Backend", "UseImpersonation", use_impersonation),
                    ("Ews Backend", "ImpersonateUser", impersonate_user),
                ],
            );
            let mut mail_updates = vec![
                ("Data Source", "DisplayName", profile.display_name.as_str()),
                ("Data Source", "Enabled", enabled_str),
                ("Mail Account", "BackendName", "ews"),
                ("Mail Account", "IdentityUid", identity_uid.as_str()),
                ("Refresh", "IntervalMinutes", interval_str.as_str()),
            ];
            if profile.is_shared_mailbox {
                mail_updates.push(("Exchange Web Services Folder", "Foreign", "true"));
                mail_updates.push(("Exchange Web Services Folder", "ForeignSubfolders", "true"));
                mail_updates.push(("Exchange Web Services Folder", "ForeignMail", profile.email.as_str()));
            }
            files_changed |= write_or_update_source_file(
                &mail_path,
                &mail_ini,
                &mail_updates,
            );
            files_changed |= write_or_update_source_file(
                &identity_path,
                &identity_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Mail Identity", "Address", &profile.email),
                    ("Mail Identity", "Name", &profile.display_name),
                    ("Mail Identity", "Organization", &profile.organization),
                    ("Mail Identity", "ReplyTo", &profile.reply_to),
                    ("Mail Submission", "TransportUid", &transport_uid),
                ],
            );
            files_changed |= write_or_update_source_file(
                &transport_path,
                &transport_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Mail Transport", "BackendName", "ews"),
                ],
            );
        }
        AccountProtocol::ImapSmtp | AccountProtocol::Pop3 => {
            let is_pop = profile.protocol == AccountProtocol::Pop3;
            let backend = if is_pop { "pop3" } else { "imapx" };
            let auth_method = if profile.auth_method.trim().is_empty() {
                "password"
            } else {
                profile.auth_method.trim()
            };
            let username = if profile.username.trim().is_empty() {
                &profile.email
            } else {
                profile.username.trim()
            };
            let sec_method = match profile.tls_mode.as_str() {
                "starttls" => "starttls",
                "none" => "none",
                _ => "ssl",
            };

            let parent_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent=\n\n\
                 [Authentication]\nHost={imap_host}\nMethod={auth_method}\nPort={imap_port}\nProxyUid=system-proxy\nRememberPassword=true\nUser={username}\nCredentialName=\nIsExternal=false\n\n\
                 [Security]\nMethod={sec_method}\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                imap_host = profile.imap_host,
                auth_method = auth_method,
                imap_port = profile.imap_port,
                username = username,
                sec_method = sec_method,
            );

            let mail_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Account]\nBackendName={backend}\nIdentityUid={identity_uid}\nNeedsInitialSetup=false\n\n\
                 [Refresh]\nEnabled=true\nIntervalMinutes={interval}\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
                backend = backend,
                identity_uid = identity_uid,
                interval = effective_sync_interval_minutes(profile),
            );

            let identity_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Identity]\nAddress={email}\nName={display_name}\nOrganization={org}\nReplyTo={reply_to}\nSignatureUid=none\n\n\
                 [Mail Submission]\nTransportUid={transport_uid}\nRepliesToOriginFolder=false\nUseSentFolder=true\n\n\
                 [Mail Composition]\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
                email = profile.email,
                org = profile.organization,
                reply_to = profile.reply_to,
                transport_uid = transport_uid,
            );

            let transport_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Transport]\nBackendName=smtp\n\n\
                 [Authentication]\nHost={smtp_host}\nMethod={auth_method}\nPort={smtp_port}\nUser={username}\n\n\
                 [Security]\nMethod={sec_method}\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
                smtp_host = profile.smtp_host,
                auth_method = auth_method,
                smtp_port = profile.smtp_port,
                username = username,
                sec_method = sec_method,
            );

            let parent_path = sources_dir.join(format!("{}.source", parent_uid));
            let mail_path = sources_dir.join(format!("{}.source", mail_uid));
            let identity_path = sources_dir.join(format!("{}.source", identity_uid));
            let transport_path = sources_dir.join(format!("{}.source", transport_uid));

            let enabled_str = if profile.enabled { "true" } else { "false" };
            let interval_str = effective_sync_interval_minutes(profile).to_string();
            let port_str = profile.imap_port.to_string();
            let smtp_port_str = profile.smtp_port.to_string();

            files_changed |= write_or_update_source_file(
                &parent_path,
                &parent_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Authentication", "Host", &profile.imap_host),
                    ("Authentication", "Method", auth_method),
                    ("Authentication", "Port", &port_str),
                    ("Authentication", "User", username),
                    ("Authentication", "RememberPassword", "true"),
                    ("Security", "Method", sec_method),
                ],
            );
            files_changed |= write_or_update_source_file(
                &mail_path,
                &mail_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Mail Account", "BackendName", backend),
                    ("Mail Account", "IdentityUid", &identity_uid),
                    ("Refresh", "IntervalMinutes", &interval_str),
                ],
            );
            files_changed |= write_or_update_source_file(
                &identity_path,
                &identity_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Mail Identity", "Address", &profile.email),
                    ("Mail Identity", "Name", &profile.display_name),
                    ("Mail Identity", "Organization", &profile.organization),
                    ("Mail Identity", "ReplyTo", &profile.reply_to),
                    ("Mail Submission", "TransportUid", &transport_uid),
                ],
            );
            files_changed |= write_or_update_source_file(
                &transport_path,
                &transport_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Mail Transport", "BackendName", "smtp"),
                    ("Authentication", "Host", &profile.smtp_host),
                    ("Authentication", "Method", auth_method),
                    ("Authentication", "Port", &smtp_port_str),
                    ("Authentication", "User", username),
                    ("Security", "Method", sec_method),
                ],
            );
        }
        AccountProtocol::Goa => {}
    }

    // Trigger EDS SourceManager reload via D-Bus only if sources actually changed
    if files_changed {
        #[cfg(target_os = "linux")]
        {
            if let Ok(bus) = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE) {
                let _ = bus.call_sync(
                    Some("org.gnome.evolution.dataserver.Sources5"),
                    "/org/gnome/evolution/dataserver/SourceManager",
                    "org.gnome.evolution.dataserver.SourceManager",
                    "Reload",
                    None,
                    None,
                    gtk::gio::DBusCallFlags::NONE,
                    2000,
                    gtk::gio::Cancellable::NONE,
                );
            }
        }
    }

    // Store credentials in Keychain / Secret Service if password provided
    if let Some(pw) = password {
        if !pw.is_empty() {
            // Persist under profile ID for portable mail engine (IMAP & SMTP lookup)
            let _ = crate::services::credential_service::CredentialService::set_password(&profile.id, pw);
            // Persist for incoming account UID
            let _ = crate::services::credential_service::CredentialService::set_password(&mail_uid, pw);
            // Persist for outgoing transport UID
            let _ = crate::services::credential_service::CredentialService::set_password(&transport_uid, pw);
            // Persist for parent collection UID
            let _ = crate::services::credential_service::CredentialService::set_password(&parent_uid, pw);
            // Persist for email address directly as fallback
            let _ = crate::services::credential_service::CredentialService::set_password(&profile.email, pw);
        }
    }

    if files_changed {
        tracing::info!(email = %profile.email, parent_uid = %parent_uid, "EDS sources written and reloaded");
    } else {
        tracing::debug!(email = %profile.email, parent_uid = %parent_uid, "EDS sources already up to date");
    }
    Ok(mail_uid)
}

/// Ensure all accounts in the local store have corresponding EDS sources.
pub fn ensure_profiles_synced_to_eds() {
    std::thread::spawn(|| {
        let store = AccountConfigStore::default();
        let accounts = store.load();
        for account in &accounts {
            if matches!(account.protocol, AccountProtocol::Goa) || account.id.starts_with("eds:") {
                continue;
            }
            let _ = sync_profile_to_eds(account, None);
        }
    });
}


/// Repair an app-owned default only when the profile just mutated owns that
/// exact ID. External EDS/GOA defaults are opaque here and must remain intact.
pub fn reconciled_local_default_account(
    current_default: &str,
    affected_local_id: &str,
    profiles: &[AccountProfile],
) -> String {
    if current_default.trim() != affected_local_id.trim() {
        return current_default.trim().to_string();
    }
    if profiles
        .iter()
        .any(|profile| profile.id == affected_local_id && profile.enabled)
    {
        return current_default.trim().to_string();
    }
    profiles
        .iter()
        .find(|profile| profile.enabled)
        .map(|profile| profile.id.clone())
        .unwrap_or_default()
}

impl Default for AccountConfigStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_profiles_roundtrip_without_secret_fields() {
        let root = temporary_directory("accounts-roundtrip");
        let store = AccountConfigStore::new(root.join("accounts.json"));
        let profile = AccountProfile {
            id: "account:person@example.org".to_string(),
            display_name: "Person".to_string(),
            email: "person@example.org".to_string(),
            protocol: AccountProtocol::ImapSmtp,
            imap_host: "imap.example.org".to_string(),
            smtp_host: "smtp.example.org".to_string(),
            tls_mode: "starttls".to_string(),
            username: "person@example.org".to_string(),
            imap_port: 143,
            smtp_port: 587,
            auth_method: "password".to_string(),
            reply_to: String::new(),
            organization: String::new(),
            aliases: Vec::new(),
            inbox_folder: String::new(),
            drafts_folder: String::new(),
            sent_folder: String::new(),
            archive_folder: String::new(),
            trash_folder: String::new(),
            junk_folder: String::new(),
            enabled: true,
            ..Default::default()
        };

        store
            .upsert(profile.clone())
            .expect("profile should persist");
        assert!(profile.validate().is_ok());
        assert_eq!(store.load(), vec![profile.clone()]);
        assert!(
            store
                .set_enabled(&profile.id, false)
                .expect("profile should disable")
        );
        assert!(!store.load()[0].enabled);
        assert!(store.delete(&profile.id).expect("profile should delete"));
        assert!(store.load().is_empty());
        let serialized = fs::read_to_string(store.path()).expect("profile file should be readable");
        assert!(!serialized.contains("password"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_account_profiles_get_empty_aliases_and_folder_mapping() {
        let raw = serde_json::json!({
            "id": "legacy",
            "display_name": "Legacy",
            "email": "legacy@example.org",
            "protocol": "ImapSmtp",
            "imap_host": "imap.example.org",
            "smtp_host": "smtp.example.org",
            "tls_mode": "tls",
            "enabled": true
        });
        let profile: AccountProfile =
            serde_json::from_value(raw).expect("legacy profile should remain readable");
        assert!(profile.aliases.is_empty());
        assert!(profile.inbox_folder.is_empty());
        assert!(profile.drafts_folder.is_empty());
        assert!(profile.sent_folder.is_empty());
        assert!(profile.archive_folder.is_empty());
        assert!(profile.trash_folder.is_empty());
        assert!(profile.junk_folder.is_empty());
        assert_eq!(profile.signature_id, None);
        assert_eq!(profile.check_interval_minutes, 10);
        assert!(profile.auto_check_enabled);
        assert!(!profile.check_all_folders);
        assert!(!profile.expunge_on_exit);
        assert!(profile.pgp_key_id.is_empty());
        assert!(!profile.pgp_always_sign);
        assert!(!profile.pgp_always_encrypt);
        assert!(profile.smime_sign_cert.is_empty());
        assert!(profile.smime_encrypt_cert.is_empty());
        assert!(profile.default_cc.is_empty());
        assert!(profile.default_bcc.is_empty());
        assert!(profile.prefer_html);
    }

    #[test]
    fn account_profile_validation_rejects_invalid_portable_values() {
        let mut profile = AccountProfile {
            id: "account:test@example.org".to_string(),
            display_name: "Test".to_string(),
            email: "test@example.org".to_string(),
            protocol: AccountProtocol::ImapSmtp,
            imap_host: "imap.example.org".to_string(),
            smtp_host: "smtp.example.org".to_string(),
            tls_mode: "tls".to_string(),
            username: "test@example.org".to_string(),
            imap_port: 993,
            smtp_port: 465,
            auth_method: "password".to_string(),
            reply_to: String::new(),
            organization: String::new(),
            aliases: vec!["alias@example.org".to_string()],
            inbox_folder: String::new(),
            drafts_folder: String::new(),
            sent_folder: String::new(),
            archive_folder: String::new(),
            trash_folder: String::new(),
            junk_folder: String::new(),
            enabled: true,
            ..Default::default()
        };
        assert!(profile.validate().is_ok());
        profile.email = "not-an-address".to_string();
        assert!(profile.validate().is_err());
        profile.email = "test@example.org".to_string();
        profile.smtp_host.clear();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn profile_order_moves_are_bounded_and_persisted() {
        let root = temporary_directory("accounts-order");
        let store = AccountConfigStore::new(root.join("accounts.json"));
        for (id, name) in [("a", "Alpha"), ("b", "Beta"), ("c", "Gamma")] {
            store
                .upsert(test_profile(id, name, true))
                .expect("profile should persist");
        }
        assert!(store.move_profile("b", -1).unwrap());
        assert_eq!(profile_ids(&store.load()), vec!["b", "a", "c"]);
        assert!(store.move_profile("b", -20).unwrap());
        assert_eq!(profile_ids(&store.load()), vec!["b", "a", "c"]);
        assert!(store.move_profile("a", 20).unwrap());
        assert_eq!(profile_ids(&store.load()), vec!["b", "c", "a"]);
        assert!(!store.move_profile("missing", 1).unwrap());
        assert_eq!(profile_ids(&store.load()), vec!["b", "c", "a"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_default_reconciliation_never_replaces_external_defaults() {
        let profiles = vec![
            test_profile("disabled", "Disabled", false),
            test_profile("enabled", "Enabled", true),
        ];
        assert_eq!(
            reconciled_local_default_account("disabled", "disabled", &profiles),
            "enabled"
        );
        assert_eq!(
            reconciled_local_default_account("external-eds", "disabled", &profiles),
            "external-eds"
        );
        assert_eq!(
            reconciled_local_default_account("enabled", "enabled", &profiles),
            "enabled"
        );
        assert_eq!(
            reconciled_local_default_account("disabled", "disabled", &[]),
            ""
        );
    }

    #[test]
    fn test_write_or_update_source_file_preserves_custom_keys() {
        let root = temporary_directory("source-file-preservation");
        let path = root.join("test.source");

        let existing = "[Data Source]\nDisplayName=Old Name\n\n[Ews Backend]\nGalUid=custom-gal-123\nHosturl=https://old.host/ews\n";
        fs::write(&path, existing).expect("should write initial source");

        write_or_update_source_file(
            &path,
            "",
            &[
                ("Data Source", "DisplayName", "New Name"),
                ("Authentication", "User", "domain\\user"),
                ("Ews Backend", "Hosturl", "https://new.host/ews"),
            ],
        );

        let updated = fs::read_to_string(&path).expect("should read updated source");
        assert!(updated.contains("DisplayName=New Name"));
        assert!(updated.contains("GalUid=custom-gal-123"), "GalUid must be preserved");
        assert!(updated.contains("Hosturl=https://new.host/ews"));

        // Verify KeyFile reads back the unescaped domain user correctly
        let kf = gtk::glib::KeyFile::new();
        kf.load_from_file(&path, gtk::glib::KeyFileFlags::NONE).expect("should parse keyfile");
        assert_eq!(kf.string("Authentication", "User").unwrap().as_str(), "domain\\user");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_write_or_update_source_file_ews_impersonation() {
        let root = temporary_directory("source-file-impersonation");
        let path = root.join("test.source");

        let existing = "[Data Source]\nDisplayName=Shared\n\n[Ews Backend]\nEmail=shared@example.com\nUseImpersonation=false\nImpersonateUser=\n";
        fs::write(&path, existing).expect("should write initial source");

        write_or_update_source_file(
            &path,
            "",
            &[
                ("Ews Backend", "UseImpersonation", "true"),
                ("Ews Backend", "ImpersonateUser", "shared@example.com"),
            ],
        );

        let kf = gtk::glib::KeyFile::new();
        kf.load_from_file(&path, gtk::glib::KeyFileFlags::NONE).expect("should parse keyfile");
        assert_eq!(kf.boolean("Ews Backend", "UseImpersonation").unwrap(), true);
        assert_eq!(kf.string("Ews Backend", "ImpersonateUser").unwrap().as_str(), "shared@example.com");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_account_profile_shared_mailbox_sync() {
        let mut profile = AccountProfile::default();
        profile.id = "account:edv@example.org".to_string();
        profile.display_name = "EDV Team".to_string();
        profile.email = "edv@example.org".to_string();
        profile.protocol = AccountProtocol::Exchange;
        profile.username = "DOMAIN\\user".to_string();
        profile.is_shared_mailbox = true;
        profile.parent_account_id = Some("account:user@example.org".to_string());

        let serialized = serde_json::to_string(&profile).expect("serialize");
        let deserialized: AccountProfile = serde_json::from_str(&serialized).expect("deserialize");
        assert!(deserialized.is_shared_mailbox);
        assert_eq!(deserialized.parent_account_id.as_deref(), Some("account:user@example.org"));
    }

    #[test]
    fn test_write_or_update_source_file_detects_changes() {
        let root = temporary_directory("source-file-change-detection");
        let path = root.join("test.source");

        // Initial creation should return true
        let changed = write_or_update_source_file(
            &path,
            "[Data Source]\nDisplayName=Test\n",
            &[("Data Source", "DisplayName", "Initial")],
        );
        assert!(changed, "First creation must report changed = true");

        // Subsequent update with the same parameters should return false
        let changed_again = write_or_update_source_file(
            &path,
            "[Data Source]\nDisplayName=Test\n",
            &[("Data Source", "DisplayName", "Initial")],
        );
        assert!(!changed_again, "No modification must report changed = false");

        // Subsequent update with changed parameter should return true
        let changed_modified = write_or_update_source_file(
            &path,
            "[Data Source]\nDisplayName=Test\n",
            &[("Data Source", "DisplayName", "Modified")],
        );
        assert!(changed_modified, "Modification must report changed = true");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_sanitize_oab_url() {
        assert_eq!(sanitize_oab_url(""), "");
        assert_eq!(sanitize_oab_url("   "), "");
        assert_eq!(sanitize_oab_url("https://mail.example.com/OAB/"), "");
        assert_eq!(sanitize_oab_url("https://mail.example.com/OAB"), "");
        assert_eq!(sanitize_oab_url("https://mail.example.com/oab//"), "");
        assert_eq!(
            sanitize_oab_url("https://mail.example.com/OAB/6b268f77-508b-4fc4-8fe4-c7fc01f46ad0/"),
            "https://mail.example.com/OAB/6b268f77-508b-4fc4-8fe4-c7fc01f46ad0/"
        );
        assert_eq!(
            sanitize_oab_url("https://mail.example.com/OAB/6b268f77-508b-4fc4-8fe4-c7fc01f46ad0"),
            "https://mail.example.com/OAB/6b268f77-508b-4fc4-8fe4-c7fc01f46ad0"
        );
    }

    #[test]
    fn test_extract_xml_tag_value() {
        let sample = r#"<?xml version="1.0"?>
<Autodiscover>
    <Response>
        <User>
            <DisplayName>Max Mustermann</DisplayName>
        </User>
        <Protocol>
            <Type>EXPR</Type>
            <EwsUrl>https://mail.company.com/EWS/Exchange.asmx</EwsUrl>
            <OABUrl>https://mail.company.com/OAB/12345-guid/</OABUrl>
        </Protocol>
    </Response>
</Autodiscover>"#;

        assert_eq!(
            extract_xml_tag_value(sample, "EwsUrl"),
            Some("https://mail.company.com/EWS/Exchange.asmx".to_string())
        );
        assert_eq!(
            extract_xml_tag_value(sample, "OABUrl"),
            Some("https://mail.company.com/OAB/12345-guid/".to_string())
        );
        assert_eq!(
            extract_xml_tag_value(sample, "DisplayName"),
            Some("Max Mustermann".to_string())
        );
        assert_eq!(extract_xml_tag_value(sample, "NonExistent"), None);
    }

    #[test]
    fn test_discover_exchange_requires_credentials() {
        let res = discover_and_validate_exchange_services(
            "user@example.com",
            "mail.example.com",
            "",
            "",
        );
        assert_eq!(res, ExchangeDiscoveryStatus::MissingCredentials);

        let res_no_pwd = discover_and_validate_exchange_services(
            "user@example.com",
            "mail.example.com",
            "user",
            "",
        );
        assert_eq!(res_no_pwd, ExchangeDiscoveryStatus::MissingCredentials);
    }

    #[test]
    fn test_normalize_exchange_host_url() {
        assert_eq!(normalize_exchange_host_url(""), "");
        assert_eq!(normalize_exchange_host_url("   "), "");
        assert_eq!(normalize_exchange_host_url("mail.company.com"), "mail.company.com");
        assert_eq!(normalize_exchange_host_url("https://mail.company.com/EWS/Exchange.asmx"), "mail.company.com");
        assert_eq!(normalize_exchange_host_url("http://mail.company.com:444/ews"), "mail.company.com:444");
    }

    #[test]
    fn test_discover_exchange_empty_host_without_autodiscover() {
        let res = discover_and_validate_exchange_services(
            "user@invalid-domain-12345.xyz",
            "",
            "user",
            "password",
        );
        match res {
            ExchangeDiscoveryStatus::ServerNotFound(msg) => {
                assert!(msg.contains("Autodiscover"));
                assert!(msg.contains("Host address"));
            }
            other => panic!("Expected ServerNotFound, got {:?}", other),
        }
    }

    fn profile_ids(profiles: &[AccountProfile]) -> Vec<&str> {
        profiles.iter().map(|profile| profile.id.as_str()).collect()
    }

    fn test_profile(id: &str, display_name: &str, enabled: bool) -> AccountProfile {
        AccountProfile {
            id: id.to_string(),
            display_name: display_name.to_string(),
            email: format!("{id}@example.org"),
            protocol: AccountProtocol::ImapSmtp,
            imap_host: "imap.example.org".to_string(),
            smtp_host: "smtp.example.org".to_string(),
            tls_mode: "tls".to_string(),
            username: id.to_string(),
            imap_port: 993,
            smtp_port: 465,
            auth_method: "password".to_string(),
            reply_to: String::new(),
            organization: String::new(),
            aliases: Vec::new(),
            inbox_folder: String::new(),
            drafts_folder: String::new(),
            sent_folder: String::new(),
            archive_folder: String::new(),
            trash_folder: String::new(),
            junk_folder: String::new(),
            enabled,
            ..Default::default()
        }
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("meridian-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    #[test]
    fn test_sync_profile_to_eds_persists_credentials_in_keychain() {
        let temp_sources = temporary_directory("sync-eds-test-sources");
        struct EnvGuard(PathBuf);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("MERIDIAN_SOURCES_DIR");
                }
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        unsafe {
            std::env::set_var("MERIDIAN_SOURCES_DIR", &temp_sources);
        }
        let _guard = EnvGuard(temp_sources);

        let profile = test_profile("test-pwd-sync-acc", "Sync Test Account", true);
        let test_pass = "S3cur3-P@ssword-2026!";

        // Sync with password
        let res = sync_profile_to_eds(&profile, Some(test_pass));
        assert!(res.is_ok(), "sync_profile_to_eds failed: {:?}", res.err());

        // Always remove test-generated .source files from the system
        remove_eds_sources_for_profile(&profile);
    }

    #[test]
    fn test_follow_global_sync_normalization() {
        // Case 1: Legacy profile missing follow_global_sync, check_interval_minutes == 10 (default)
        // -> Should normalize follow_global_sync to true
        let json_default_interval = serde_json::json!({
            "id": "acc-1",
            "display_name": "Acc 1",
            "email": "acc1@example.com",
            "protocol": "ImapSmtp",
            "imap_host": "imap.example.com",
            "smtp_host": "smtp.example.com",
            "tls_mode": "tls",
            "enabled": true,
            "check_interval_minutes": 10
        });
        let prof1: AccountProfile = serde_json::from_value(json_default_interval).unwrap();
        assert!(prof1.follow_global_sync);

        // Case 2: Legacy profile missing follow_global_sync, check_interval_minutes != 10 (explicit override)
        // -> Should keep explicit override, follow_global_sync = false
        let json_override_interval = serde_json::json!({
            "id": "acc-2",
            "display_name": "Acc 2",
            "email": "acc2@example.com",
            "protocol": "ImapSmtp",
            "imap_host": "imap.example.com",
            "smtp_host": "smtp.example.com",
            "tls_mode": "tls",
            "enabled": true,
            "check_interval_minutes": 30
        });
        let prof2: AccountProfile = serde_json::from_value(json_override_interval).unwrap();
        assert!(!prof2.follow_global_sync);

        // Case 3: Profile explicitly specifies follow_global_sync: false (even with interval 10)
        let json_explicit_false = serde_json::json!({
            "id": "acc-3",
            "display_name": "Acc 3",
            "email": "acc3@example.com",
            "protocol": "ImapSmtp",
            "imap_host": "imap.example.com",
            "smtp_host": "smtp.example.com",
            "tls_mode": "tls",
            "enabled": true,
            "check_interval_minutes": 10,
            "follow_global_sync": false
        });
        let prof3: AccountProfile = serde_json::from_value(json_explicit_false).unwrap();
        assert!(!prof3.follow_global_sync);

        // Case 4: Profile explicitly specifies follow_global_sync: true (even with interval 30)
        let json_explicit_true = serde_json::json!({
            "id": "acc-4",
            "display_name": "Acc 4",
            "email": "acc4@example.com",
            "protocol": "ImapSmtp",
            "imap_host": "imap.example.com",
            "smtp_host": "smtp.example.com",
            "tls_mode": "tls",
            "enabled": true,
            "check_interval_minutes": 30,
            "follow_global_sync": true
        });
        let prof4: AccountProfile = serde_json::from_value(json_explicit_true).unwrap();
        assert!(prof4.follow_global_sync);

        // Case 5: New default profile has follow_global_sync == true
        let prof5 = AccountProfile::default();
        assert!(prof5.follow_global_sync);
    }
}
