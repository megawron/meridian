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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub protocol: AccountProtocol,
    pub imap_host: String,
    pub smtp_host: String,
    pub tls_mode: String,
    pub enabled: bool,
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
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4/accounts.json"));
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
            enabled: true,
        };

        store
            .upsert(profile.clone())
            .expect("profile should persist");
        assert_eq!(store.load(), vec![profile]);
        let serialized = fs::read_to_string(store.path()).expect("profile file should be readable");
        assert!(!serialized.contains("password"));

        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
