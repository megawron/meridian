// SPDX-License-Identifier: GPL-3.0-or-later
//
// Linux Credential Service
// Routes to Freedesktop Secret Service / EDS on Linux.

#[allow(dead_code)]
pub const KEYCHAIN_SERVICE_ID: &str = "com.martingawron.Meridian";

#[derive(Debug, Clone, Default)]
pub struct CredentialService;

#[allow(dead_code)]
impl CredentialService {
    pub fn new() -> Self {
        Self
    }

    /// Store a credential password for an account / source UID.
    pub fn store_password(&self, account_uid: &str, password: &str) -> Result<(), String> {
        Self::set_password(account_uid, password)
    }

    /// Retrieve a credential password for an account / source UID.
    pub fn get_password(&self, account_uid: &str) -> Result<Option<String>, String> {
        Self::fetch_password(account_uid)
    }

    /// Delete a credential password for an account / source UID.
    pub fn delete_password(&self, account_uid: &str) -> Result<(), String> {
        Self::remove_password(account_uid)
    }

    /// Static helper to store password.
    pub fn set_password(account_uid: &str, password: &str) -> Result<(), String> {
        let account_uid = account_uid.trim();
        if account_uid.is_empty() {
            return Err("Kontokennung darf nicht leer sein".into());
        }

        crate::services::camel_mail_service::store_source_password(account_uid, password)
    }

    /// Static helper to fetch password.
    pub fn fetch_password(account_uid: &str) -> Result<Option<String>, String> {
        let account_uid = account_uid.trim();
        if account_uid.is_empty() {
            return Ok(None);
        }

        // On Linux, EDS Camel manages passwords internally via libsecret / GNOME Keyring.
        let _ = account_uid;
        Ok(None)
    }

    /// Static helper to remove password.
    pub fn remove_password(account_uid: &str) -> Result<(), String> {
        let account_uid = account_uid.trim();
        if account_uid.is_empty() {
            return Ok(());
        }

        let _ = account_uid;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_service_identifier_matches_spec() {
        assert_eq!(KEYCHAIN_SERVICE_ID, "com.martingawron.Meridian");
    }

    #[test]
    fn credential_service_empty_account_safety() {
        let creds = CredentialService::new();
        assert!(creds.store_password("", "pass").is_err());
        assert_eq!(creds.get_password("").unwrap(), None);
        assert!(creds.delete_password("").is_ok());
    }
}

