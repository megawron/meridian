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
