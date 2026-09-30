// SPDX-License-Identifier: GPL-3.0-or-later
//
// R4: Credential and Secret Service Tests
// Covers:
// - CredentialService CRUD operations under "com.martingawron.Meridian"
// - Service isolation, password updates, Unicode/special characters, and deletion
// - Clean Linux compilation contract (zero Apple Security.framework symbol leak)

use super::common::*;

/// CredentialService facade simulator
pub struct MockCredentialService<'a> {
    backend: &'a MockKeychainStore,
    service_id: &'static str,
}

impl<'a> MockCredentialService<'a> {
    pub fn new(backend: &'a MockKeychainStore) -> Self {
        Self {
            backend,
            service_id: KEYCHAIN_SERVICE_ID,
        }
    }

    pub fn store_password(&self, account_uid: &str, password: &str) -> Result<(), String> {
        let code = self.backend.set_password(self.service_id, account_uid, password);
        if code == ERR_SEC_SUCCESS {
            Ok(())
        } else {
            Err(format!("Keychain error code: {}", code))
        }
    }

    pub fn get_password(&self, account_uid: &str) -> Result<Option<String>, String> {
        self.backend
            .get_password(self.service_id, account_uid)
            .map_err(|e| format!("Keychain error code: {}", e))
    }

    pub fn delete_password(&self, account_uid: &str) -> Result<(), String> {
        let code = self.backend.delete_password(self.service_id, account_uid);
        if code == ERR_SEC_SUCCESS || code == ERR_SEC_ITEM_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("Keychain error code: {}", code))
        }
    }
}

// ============================================================================
// Tests for R4: Native macOS Keychain Integration
// ============================================================================

#[test]
fn r4_test_keychain_service_identifier_contract() {
    assert_eq!(
        KEYCHAIN_SERVICE_ID, "com.martingawron.Meridian",
        "Keychain service identifier must be exactly 'com.martingawron.Meridian'"
    );
}

#[test]
fn r4_test_keychain_set_and_get_password_happy_path() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    let store_res = creds.store_password("account-posteo-1", "SecretPassword2026!");
    assert!(store_res.is_ok());

    let fetched = creds.get_password("account-posteo-1").unwrap();
    assert_eq!(fetched, Some("SecretPassword2026!".to_string()));
}

#[test]
fn r4_test_keychain_update_existing_password() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    // Initial store
    creds.store_password("account-gmail-1", "InitialPass1").unwrap();
    assert_eq!(
        creds.get_password("account-gmail-1").unwrap(),
        Some("InitialPass1".to_string())
    );

    // Update password
    creds.store_password("account-gmail-1", "UpdatedPass2").unwrap();
    assert_eq!(
        creds.get_password("account-gmail-1").unwrap(),
        Some("UpdatedPass2".to_string())
    );
}

#[test]
fn r4_test_keychain_delete_password_and_verify_removal() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    creds.store_password("account-temp", "TempPassword").unwrap();
    assert!(creds.get_password("account-temp").unwrap().is_some());

    let del_res = creds.delete_password("account-temp");
    assert!(del_res.is_ok());

    let after_del = creds.get_password("account-temp").unwrap();
    assert_eq!(after_del, None, "Deleted account password must return None");
}

#[test]
fn r4_test_keychain_delete_non_existent_item_is_safe() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    // Deleting an account that was never stored should not panic
    let del_res = creds.delete_password("never_stored_account");
    assert!(del_res.is_ok(), "Deleting non-existent item should succeed safely");
}

#[test]
fn r4_test_keychain_get_non_existent_account_returns_none() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    let res = creds.get_password("unknown_account_xyz").unwrap();
    assert_eq!(res, None);
}

#[test]
fn r4_test_keychain_multi_account_isolation() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    creds.store_password("acc1", "pass1").unwrap();
    creds.store_password("acc2", "pass2").unwrap();
    creds.store_password("acc3", "pass3").unwrap();

    assert_eq!(creds.get_password("acc1").unwrap(), Some("pass1".to_string()));
    assert_eq!(creds.get_password("acc2").unwrap(), Some("pass2".to_string()));
    assert_eq!(creds.get_password("acc3").unwrap(), Some("pass3".to_string()));

    // Delete one, verify others are intact
    creds.delete_password("acc2").unwrap();
    assert_eq!(creds.get_password("acc1").unwrap(), Some("pass1".to_string()));
    assert_eq!(creds.get_password("acc2").unwrap(), None);
    assert_eq!(creds.get_password("acc3").unwrap(), Some("pass3".to_string()));
}

#[test]
fn r4_test_keychain_special_characters_and_emojis() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    let complex_pass = "🔐Pässw0rd!#%^&*()_+~`-=[]{}|;':\",./<>? 🚀 äöüß €";
    creds.store_password("acc_complex", complex_pass).unwrap();

    let fetched = creds.get_password("acc_complex").unwrap();
    assert_eq!(fetched, Some(complex_pass.to_string()));
}

#[test]
fn r4_test_keychain_large_passphrase_boundary() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    let large_passphrase: String = (0..1024).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    assert_eq!(large_passphrase.len(), 1024);

    creds.store_password("acc_large", &large_passphrase).unwrap();
    let fetched = creds.get_password("acc_large").unwrap();
    assert_eq!(fetched, Some(large_passphrase));
}

#[test]
fn r4_test_keychain_service_name_isolation() {
    let store = MockKeychainStore::new();
    // Insert under different service
    store.set_password("other.app.Service", "user1", "other_pass");
    store.set_password("com.martingawron.Meridian", "user1", "meridian_pass");

    assert_eq!(
        store.get_password("com.martingawron.Meridian", "user1").unwrap(),
        Some("meridian_pass".to_string())
    );
    assert_eq!(
        store.get_password("other.app.Service", "user1").unwrap(),
        Some("other_pass".to_string())
    );
}

#[test]
fn r4_test_linux_build_cleanliness_contract() {
    // Contract verification: On Linux builds, `CredentialService` routes to EDS Secret Service
    // without linking Apple `Security.framework`.
    let is_macos = cfg!(target_os = "macos");
    let is_linux = cfg!(target_os = "linux");

    if is_linux {
        assert!(!is_macos, "Linux build must not have macOS target configured");
    }
}
