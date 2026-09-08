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
            return Err("Bitte eine gültige E-Mail-Adresse eingeben".to_string());
        }
        if self.display_name.trim().is_empty() {
            return Err("Der Anzeigename darf nicht leer sein".to_string());
        }
        if self.id.trim().is_empty() {
            return Err("Das Konto besitzt keine gültige interne ID".to_string());
        }
        if !matches!(self.tls_mode.as_str(), "tls" | "starttls" | "none") {
            return Err("Die Verschlüsselungsart ist ungültig".to_string());
        }
        if self.imap_port == 0 || self.smtp_port == 0 {
            return Err("IMAP-/SMTP-Ports müssen zwischen 1 und 65535 liegen".to_string());
        }
        if self.protocol.requires_mail_servers() && self.imap_host.trim().is_empty() {
            return Err(format!(
                "Bitte einen {}-Posteingangsserver eingeben",
                self.protocol.incoming_server_label()
            ));
        }
        if matches!(self.protocol, AccountProtocol::ImapSmtp) && self.smtp_host.trim().is_empty() {
            return Err("Bitte einen SMTP-Postausgangsserver eingeben".to_string());
        }
        if self
            .aliases
            .iter()
            .any(|alias| !alias.trim().is_empty() && !is_plausible_address(alias.trim()))
        {
            return Err("Jede Alias-Adresse muss eine gültige E-Mail-Adresse sein".to_string());
        }
        if !self.reply_to.trim().is_empty() && !is_plausible_address(self.reply_to.trim()) {
            return Err("Die Antwortadresse ist ungültig".to_string());
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

fn is_plausible_address(value: &str) -> bool {
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

#[derive(Debug, Clone)]
pub struct AccountConfigStore {
    path: PathBuf,
}

impl AccountConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_ACCOUNT_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("accounts.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
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
        accounts.retain(|account| account.id != id);
        if accounts.len() == original_len {
            return Ok(false);
        }
        self.save(&accounts)?;
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
        fs::rename(temporary_path, &self.path)
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

/// Returns the authoritative mail source UID recognized by EDS and Camel.
pub fn sync_profile_to_eds(
    profile: &AccountProfile,
    password: Option<&str>,
) -> Result<String, String> {
    if matches!(profile.protocol, AccountProtocol::Goa) {
        return Ok(profile.id.clone());
    }

    let parent_uid = eds_source_uid_for_email(&profile.email);
    let mail_uid = format!("{}-mail", parent_uid);
    let identity_uid = format!("{}-identity", parent_uid);
    let transport_uid = format!("{}-transport", parent_uid);

    let sources_dir = gtk::glib::user_config_dir().join("evolution").join("sources");
    if let Err(err) = fs::create_dir_all(&sources_dir) {
        return Err(format!("Verzeichnis {} konnte nicht erstellt werden: {}", sources_dir.display(), err));
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

            let parent_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent=\n\n\
                 [Authentication]\nHost={auth_host}\nMethod={auth_method}\nPort=443\nUser={username}\nRememberPassword=true\n\n\
                 [Security]\nMethod=none\n\n\
                 [Collection]\nBackendName=ews\nIdentity={username}\nMailEnabled=true\nCalendarEnabled=true\nContactsEnabled=true\n\n\
                 [Ews Backend]\nEmail={email}\nHosturl={host_url}\nOaburl={oab_url}\nCheckAll={check_all}\nFilterInbox=true\nListenNotifications=true\nTimeout=30\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                auth_host = auth_host,
                auth_method = auth_method,
                username = username,
                email = profile.email,
                host_url = host_url,
                oab_url = oab_url,
                check_all = profile.check_all_folders,
            );

            let mail_ini = format!(
                "[Data Source]\nDisplayName={display_name}\nEnabled={enabled}\nParent={parent_uid}\n\n\
                 [Mail Account]\nBackendName=ews\nIdentityUid={identity_uid}\nNeedsInitialSetup=false\n\n\
                 [Refresh]\nEnabled=true\nIntervalMinutes={interval}\n",
                display_name = profile.display_name,
                enabled = profile.enabled,
                parent_uid = parent_uid,
                identity_uid = identity_uid,
                interval = profile.check_interval_minutes.max(1),
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
            let interval_str = profile.check_interval_minutes.max(1).to_string();

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
                ],
            );
            files_changed |= write_or_update_source_file(
                &mail_path,
                &mail_ini,
                &[
                    ("Data Source", "DisplayName", &profile.display_name),
                    ("Data Source", "Enabled", enabled_str),
                    ("Mail Account", "BackendName", "ews"),
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
                interval = profile.check_interval_minutes.max(1),
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
            let interval_str = profile.check_interval_minutes.max(1).to_string();
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

    // Store credentials in Secret Service if password provided
    if let Some(pw) = password {
        if !pw.is_empty() {
            let _ = crate::services::camel_mail_service::store_source_password(&parent_uid, pw);
            let _ = crate::services::camel_mail_service::store_source_password(&mail_uid, pw);
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
            if matches!(account.protocol, AccountProtocol::Goa) {
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
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
