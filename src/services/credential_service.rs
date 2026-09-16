// SPDX-License-Identifier: GPL-3.0-or-later
//
// Cross-Platform Credential Service
// Routes to macOS Keychain via Security.framework on macOS,
// and Freedesktop Secret Service / EDS on Linux.

pub const KEYCHAIN_SERVICE_ID: &str = "org.gnome.Evolution4";

#[derive(Debug, Clone, Default)]
pub struct CredentialService;

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

        #[cfg(target_os = "macos")]
        {
            let macos_svc = crate::services::macos_integration_service::MacosIntegrationService::new();
            macos_svc.keychain_set_password(account_uid, password)
        }

        #[cfg(target_os = "linux")]
        {
            crate::services::camel_mail_service::store_source_password(account_uid, password)
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (account_uid, password);
            Ok(())
        }
    }

    /// Static helper to fetch password.
    pub fn fetch_password(account_uid: &str) -> Result<Option<String>, String> {
        let account_uid = account_uid.trim();
        if account_uid.is_empty() {
            return Ok(None);
        }

        #[cfg(target_os = "macos")]
        {
            let macos_svc = crate::services::macos_integration_service::MacosIntegrationService::new();
            macos_svc.keychain_get_password(account_uid)
        }

        #[cfg(target_os = "linux")]
        {
            // On Linux, EDS Camel manages passwords internally via libsecret / GNOME Keyring.
            let _ = account_uid;
            Ok(None)
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = account_uid;
            Ok(None)
        }
    }

    /// Static helper to remove password.
    pub fn remove_password(account_uid: &str) -> Result<(), String> {
        let account_uid = account_uid.trim();
        if account_uid.is_empty() {
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            let macos_svc = crate::services::macos_integration_service::MacosIntegrationService::new();
            macos_svc.keychain_delete_password(account_uid)
        }

        #[cfg(target_os = "linux")]
        {
            let _ = account_uid;
            Ok(())
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = account_uid;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_service_identifier_matches_spec() {
        assert_eq!(KEYCHAIN_SERVICE_ID, "org.gnome.Evolution4");
    }

    #[test]
    fn credential_service_empty_account_safety() {
        let creds = CredentialService::new();
        assert!(creds.store_password("", "pass").is_err());
        assert_eq!(creds.get_password("").unwrap(), None);
        assert!(creds.delete_password("").is_ok());
    }

    #[test]
    fn credential_service_crud_lifecycle() {
        let creds = CredentialService::new();
        let test_account = "test-credential-facade-account";
        let test_pass = "FacadeSecret2026!";

        let store_res = creds.store_password(test_account, test_pass);
        if !cfg!(target_os = "macos") {
            return;
        }
        assert!(store_res.is_ok());
            let fetched = creds.get_password(test_account);
            assert!(fetched.is_ok());
            assert_eq!(fetched.unwrap(), Some(test_pass.to_string()));

            // Update
            assert!(creds.store_password(test_account, "UpdatedFacadePass!").is_ok());
            assert_eq!(
                creds.get_password(test_account).unwrap(),
                Some("UpdatedFacadePass!".to_string())
            );

            // Delete
            assert!(creds.delete_password(test_account).is_ok());
            assert_eq!(creds.get_password(test_account).unwrap(), None);
    }

    #[test]
    fn credential_service_special_chars_emojis_and_non_ascii() {
        let creds = CredentialService::new();
        if !cfg!(target_os = "macos") {
            return;
        }

        let test_cases = [
            // Emojis and unicode
            ("account-🔑-test", "P@ssw0rd-🔐-🚀-✨"),
            // Non-ASCII German umlauts
            ("mätthäus@jörg.example.com", "Sicheres-P@sswört-2026!"),
            // Non-ASCII CJK and Cyrillic
            ("user_测试_ñ", "密码-пароль-123!"),
            // All special ASCII characters
            ("special-user", "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"),
        ];

        for (acc, pass) in test_cases {
            // Clean before test
            let _ = creds.delete_password(acc);

            // Store
            let store_res = creds.store_password(acc, pass);
            assert!(store_res.is_ok(), "Failed to store for {acc}: {:?}", store_res.err());

            // Retrieve
            let fetched = creds.get_password(acc).expect("Get should not error");
            assert_eq!(fetched.as_deref(), Some(pass), "Mismatch for {acc}");

            // Clean up
            assert!(creds.delete_password(acc).is_ok());
            assert_eq!(creds.get_password(acc).unwrap(), None);
        }
    }

    #[test]
    fn credential_service_duplicate_inserts_and_idempotence() {
        let creds = CredentialService::new();
        if !cfg!(target_os = "macos") {
            return;
        }

        let acc = "test-duplicate-insert-account";
        let _ = creds.delete_password(acc);

        // First insert
        assert!(creds.store_password(acc, "first_pass").is_ok());
        assert_eq!(creds.get_password(acc).unwrap().as_deref(), Some("first_pass"));

        // Duplicate insert with same pass
        assert!(creds.store_password(acc, "first_pass").is_ok());
        assert_eq!(creds.get_password(acc).unwrap().as_deref(), Some("first_pass"));

        // Duplicate insert with updated pass
        assert!(creds.store_password(acc, "second_pass").is_ok());
        assert_eq!(creds.get_password(acc).unwrap().as_deref(), Some("second_pass"));

        // Clean up
        assert!(creds.delete_password(acc).is_ok());
    }

    #[test]
    fn credential_service_non_existent_queries_and_deletes() {
        let creds = CredentialService::new();
        if !cfg!(target_os = "macos") {
            return;
        }

        let nonexistent_acc = "definitely-not-existing-account-987654321";
        // Ensure it doesn't exist
        let _ = creds.delete_password(nonexistent_acc);

        // Query non-existent must return Ok(None)
        let res = creds.get_password(nonexistent_acc);
        assert!(res.is_ok(), "Non-existent account query should succeed");
        assert_eq!(res.unwrap(), None, "Non-existent account must return None");

        // Delete non-existent must return Ok(())
        let del_res = creds.delete_password(nonexistent_acc);
        assert!(del_res.is_ok(), "Deleting non-existent account must be idempotent (return Ok)");
    }

    #[test]
    fn credential_service_rapid_sequential_set_get_cycles() {
        let creds = CredentialService::new();
        if !cfg!(target_os = "macos") {
            return;
        }

        let base_acc = "test-rapid-cycle-acc";
        for i in 0..25 {
            let acc = format!("{}-{}", base_acc, i);
            let pass = format!("rapid_pass_{}", i);

            assert!(creds.store_password(&acc, &pass).is_ok());
            let fetched = creds.get_password(&acc).unwrap();
            assert_eq!(fetched.as_deref(), Some(pass.as_str()));
            assert!(creds.delete_password(&acc).is_ok());
            assert_eq!(creds.get_password(&acc).unwrap(), None);
        }
    }
}

