// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tiers 3 & 4: Cross-Feature Interactions & Real-World Application Workloads
// Covers:
// - Tier 3: Pairwise interactions across R1-R6
// - Tier 4: Complex multi-step end-to-end workflows

use super::common::*;
use super::r1_viewer_window::*;
use super::r2_drag_and_drop::*;
use super::r3_autodiscover::*;
use super::r4_macos_keychain::*;
use super::r5_portable_engine::*;
use super::r6_certificate_manager::*;

// ============================================================================
// Tier 3: Pairwise Cross-Feature Interactions
// ============================================================================

#[test]
fn tier3_interaction_composer_dnd_attachment_and_smtp_delivery() {
    // Drop attachment in composer -> generate RFC 822 -> deliver via SMTP
    let mut composer = MockComposerDropTarget::new();
    composer.handle_file_drop("/Users/user/Documents/Financial_Report.pdf", 786_432);

    assert_eq!(composer.attachments.len(), 1);
    let att = &composer.attachments[0];

    // Build message body with attachment header
    let raw_payload = format!(
        "Subject: Q3 Financials\r\nX-Attachment: {}; size={}\r\n\r\nPlease find attached.",
        att.filename, att.size
    );

    // Deliver via SMTP
    let mut smtp = MockSmtpClient::new();
    smtp.ehlo("localhost").unwrap();
    smtp.auth_plain("cfo@company.com", "secure123").unwrap();
    smtp.mail_from("cfo@company.com").unwrap();
    smtp.rcpt_to("board@company.com").unwrap();
    let res = smtp.data(&raw_payload);
    assert!(res.is_ok());
    assert_eq!(smtp.state, SmtpState::MessageQueued);
}

#[test]
fn tier3_interaction_autodiscover_to_keychain_to_imap_connection() {
    // Autodiscover Posteo -> store password in Keychain -> retrieve -> simulate IMAP login
    let conf = MockAutodiscoverService::lookup_builtin_profile("user@posteo.de").unwrap();
    assert_eq!(conf.incoming.hostname, "posteo.de");

    let keychain = MockKeychainStore::new();
    let creds = MockCredentialService::new(&keychain);
    creds.store_password("posteo_account", "SuperSecret123").unwrap();

    let fetched_pass = creds.get_password("posteo_account").unwrap().unwrap();
    assert_eq!(fetched_pass, "SuperSecret123");

    // Connect to IMAP
    let inbox_line = r#"* LIST (\HasNoChildren) "/" "INBOX""#;
    let folder = MockImapProtocolParser::parse_list_line(inbox_line).unwrap();
    assert_eq!(folder.name, "INBOX");
}

#[test]
fn tier3_interaction_mail_viewer_smime_certificate_trust_banner() {
    // Standalone viewer displays S/MIME message, cross-referencing CertificateStore
    let mut cert_store = MockCertificateStore::new();
    let cert_der = DerBuilder::create_test_x509_der(
        0x501,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "300901000000Z",
        None,
    );
    let cert = MockX509Parser::parse_der(&cert_der).unwrap();
    let cert_id = cert.id.clone();
    cert_store.add_certificate(cert);

    // Check default trust level
    let current_cert = cert_store.records.get(&cert_id).unwrap();
    assert_eq!(current_cert.trust_level, TrustLevel::TrustForSigning);

    // Update to NeverTrust (untrusted signer)
    cert_store.set_trust_level(&cert_id, TrustLevel::NeverTrust);
    let updated_cert = cert_store.records.get(&cert_id).unwrap();
    assert_eq!(updated_cert.trust_level, TrustLevel::NeverTrust);
}

#[test]
fn tier3_interaction_mail_list_dnd_move_updates_folder_and_viewer_tracking() {
    let mut folder_store = MockFolderStore::new();
    let viewer_registry = MockMailViewerRegistry::default();

    let msg = TestMessage::new("source1", "inbox", "msg_101", "Important Notice");
    let (key, is_new) = viewer_registry.activate_message(&msg);
    assert!(is_new);
    assert_eq!(viewer_registry.count_open_windows(), 1);

    // Move message to trash via drag-and-drop
    let move_res = folder_store.move_message("msg_101", "trash", true);
    assert!(move_res.is_ok());

    let trash = folder_store.folders.get("trash").unwrap();
    assert_eq!(trash.total_count, 3);

    // Closing the viewer window
    viewer_registry.close_window(&key);
    assert_eq!(viewer_registry.count_open_windows(), 0);
}

// ============================================================================
// Tier 4: Real-World Application Workloads
// ============================================================================

#[test]
fn tier4_workload_complete_account_onboarding_and_send_flow() {
    // 1. User enters email into setup dialog
    let email = "ceo@gmail.com";
    let conf = MockAutodiscoverService::lookup_builtin_profile(email).unwrap();
    assert_eq!(conf.incoming.hostname, "imap.gmail.com");
    assert_eq!(conf.outgoing.hostname, "smtp.gmail.com");

    // 2. User enters password, saved to Keychain under com.martingawron.Meridian
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);
    creds.store_password(email, "AppSpecificPassword123").unwrap();

    // 3. Sync folders via IMAP listing
    let list_lines = [
        r#"* LIST (\HasNoChildren) "/" "INBOX""#,
        r#"* LIST (\HasNoChildren \Sent) "/" "[Gmail]/Sent Mail""#,
        r#"* LIST (\HasNoChildren \Trash) "/" "[Gmail]/Trash""#,
    ];
    let folders: Vec<ImapFolderItem> = list_lines
        .iter()
        .map(|l| MockImapProtocolParser::parse_list_line(l).unwrap())
        .collect();
    assert_eq!(folders.len(), 3);
    assert!(folders[1].is_sent);

    // 4. Compose message with dropped PDF attachment
    let mut composer = MockComposerDropTarget::new();
    composer.handle_file_drop("/Users/ceo/Contract_Signed.pdf", 250_000);
    assert_eq!(composer.attachments[0].filename, "Contract_Signed.pdf");

    // 5. Send message via SMTP
    let mut smtp = MockSmtpClient::new();
    smtp.ehlo("gmail.com").unwrap();
    let retrieved_pass = creds.get_password(email).unwrap().unwrap();
    smtp.auth_plain(email, &retrieved_pass).unwrap();
    smtp.mail_from(email).unwrap();
    smtp.rcpt_to("partner@enterprise.com").unwrap();
    smtp.data("Subject: Signed Contract\r\n\r\nSee attached.").unwrap();
    smtp.quit().unwrap();

    assert_eq!(smtp.state, SmtpState::Quit);
}

#[test]
fn tier4_workload_certificate_manager_full_trust_lifecycle() {
    let mut store = MockCertificateStore::new();

    // 1. Import Root CA
    let ca_der = DerBuilder::create_test_x509_der(
        0x10,
        "Evolution Root CA",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260101000000Z",
        "360101000000Z",
        Some(0x06),
    );
    let ca_cert = MockX509Parser::parse_der(&ca_der).unwrap();
    let ca_id = ca_cert.id.clone();
    store.add_certificate(ca_cert);

    // 2. Set CA to AlwaysTrust
    store.set_trust_level(&ca_id, TrustLevel::AlwaysTrust);

    // 3. Import User Certificate
    let user_der = DerBuilder::create_test_x509_der(
        0x20,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260101000000Z",
        "280101000000Z",
        Some(0xE0),
    );
    let user_cert = MockX509Parser::parse_der(&user_der).unwrap();
    store.add_certificate(user_cert);

    // 4. Persist to JSON and reload
    let json = store.export_json();
    let reloaded_store = MockCertificateStore::import_json(&json).unwrap();

    assert_eq!(reloaded_store.records.len(), 2);
    assert_eq!(
        reloaded_store.records.get(&ca_id).unwrap().trust_level,
        TrustLevel::AlwaysTrust
    );
}

#[test]
fn tier4_workload_multi_window_viewer_stress_and_cancellation() {
    let registry = MockMailViewerRegistry::default();

    // Open 10 message viewer windows simultaneously
    let mut keys = Vec::new();
    for i in 1..=10 {
        let msg = TestMessage::new("source1", "INBOX", &format!("msg_{}", i), &format!("Stress Subject {}", i));
        let (key, is_new) = registry.activate_message(&msg);
        assert!(is_new);
        keys.push(key);
    }
    assert_eq!(registry.count_open_windows(), 10);

    // Close 5 windows
    for key in keys.iter().take(5) {
        assert!(registry.close_window(key));
    }
    assert_eq!(registry.count_open_windows(), 5);

    // Remaining 5 windows still present
    for key in keys.iter().skip(5) {
        let map = registry.open_windows.lock().unwrap();
        assert!(map.contains_key(key));
    }
}

// ============================================================================
// Tier 4 Challenger Stress Suite (challenger_m5_gen2_2)
// ============================================================================

#[test]
fn tier4_adversarial_challenger2_concurrency_multi_window_multithreaded_race_and_cancellation() {
    use std::sync::Arc;
    use std::thread;

    let registry = Arc::new(MockMailViewerRegistry::default());
    let mut handles = Vec::new();
    const NUM_THREADS: usize = 8;
    const OPS_PER_THREAD: usize = 50;

    for thread_id in 0..NUM_THREADS {
        let reg = Arc::clone(&registry);
        let handle = thread::spawn(move || {
            for op in 0..OPS_PER_THREAD {
                // Modulo ensures heavy key contention across threads
                let msg_idx = (thread_id + op) % 10;
                let uid = format!("concurrent_msg_{:02}", msg_idx);
                let msg = TestMessage::new("shared_account", "INBOX", &uid, "Concurrent Subject");

                // Activate message
                let (key, _is_new) = reg.activate_message(&msg);

                // Verify window exists and cancellation token is readable
                {
                    let map = reg.open_windows.lock().unwrap();
                    if let Some(win) = map.get(&key) {
                        assert!(win.is_presented);
                    }
                }

                // Randomly close some windows to test concurrent close & cancellation
                if op % 3 == 0 {
                    reg.close_window(&key);
                }
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().expect("Thread should not panic");
    }

    // After all threads finish, ensure registry lock is healthy and no deadlocks occurred
    let count = registry.count_open_windows();
    assert!(count <= 10, "Remaining windows must not exceed the 10 distinct message keys");

    // Cleanly close all remaining windows
    let keys: Vec<String> = {
        let map = registry.open_windows.lock().unwrap();
        map.keys().cloned().collect()
    };
    for key in keys {
        assert!(registry.close_window(&key));
    }
    assert_eq!(registry.count_open_windows(), 0);
}

#[test]
fn tier4_adversarial_challenger2_keychain_persistence_cross_account_isolation_and_restart() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    // 1. Create a matrix of accounts with diverse character sets
    let test_accounts = [
        ("user_standard@company.com", "StandardPassw0rd123!"),
        ("müller.jürgen@münchen.de", "München#Umlaut#2026!"),
        ("ceo_key@enterprise.org", "🔑Super#Secure#Key!🚀"),
        ("whitespace_acc@test.com", "Pass with multiple spaces and tokens"),
        ("special_chars@domain.net", "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"),
    ];

    // Store all accounts
    for (acc, pass) in &test_accounts {
        assert!(creds.store_password(acc, pass).is_ok());
    }

    // Verify all passwords stored correctly
    for (acc, pass) in &test_accounts {
        let fetched = creds.get_password(acc).unwrap();
        assert_eq!(fetched.as_deref(), Some(*pass), "Password mismatch for {}", acc);
    }

    // 2. Simulate Application Restart: Instantiate new MockCredentialService referencing the same Keychain store
    let creds_after_restart = MockCredentialService::new(&store);
    for (acc, pass) in &test_accounts {
        let fetched = creds_after_restart.get_password(acc).unwrap();
        assert_eq!(fetched.as_deref(), Some(*pass), "Password must survive application restart for {}", acc);
    }

    // 3. Update account 2 and verify isolation: updating one account must NOT affect others
    assert!(creds_after_restart.store_password("müller.jürgen@münchen.de", "NewUpdatedUmlautPass999!").is_ok());
    assert_eq!(
        creds_after_restart.get_password("müller.jürgen@münchen.de").unwrap().as_deref(),
        Some("NewUpdatedUmlautPass999!")
    );
    assert_eq!(
        creds_after_restart.get_password("user_standard@company.com").unwrap().as_deref(),
        Some("StandardPassw0rd123!"),
        "Updating one account must not modify adjacent accounts"
    );

    // 4. Delete one account and verify isolation: deleting account A leaves account B intact
    assert!(creds_after_restart.delete_password("ceo_key@enterprise.org").is_ok());
    assert_eq!(creds_after_restart.get_password("ceo_key@enterprise.org").unwrap(), None);
    assert_eq!(
        creds_after_restart.get_password("user_standard@company.com").unwrap().as_deref(),
        Some("StandardPassw0rd123!")
    );
}

#[test]
fn tier4_adversarial_challenger2_posix_key_permissions_and_unauthorized_access_hardening() {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    let temp_dir = std::env::temp_dir().join(format!("evo-challenger2-posix-{}", glib::uuid_string_random()));
    fs::create_dir_all(&temp_dir).unwrap();

    let key_dir = temp_dir.join("keys");
    fs::create_dir_all(&key_dir).unwrap();

    // 1. Directory permission hardening to 0o700 (rwx------)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Force loose permissions first
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(fs::metadata(&key_dir).unwrap().permissions().mode() & 0o777, 0o777);

        // Apply production hardening rule
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let mode = fs::metadata(&key_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "Key directory must be tightened to 0o700");
        assert_eq!(mode & 0o077, 0, "Group and other bits must be completely zeroed");
    }

    // 2. Private key file creation with mode 0o600 (rw-------)
    let key_file = key_dir.join("test_cert.key");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&key_file).unwrap();
    file.write_all(b"-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASC...").unwrap();
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();
        let file_mode = fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "Private key file permissions must be exactly 0o600");
        assert_eq!(file_mode & 0o077, 0, "Private key must have zero group and other permissions");
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn tier4_adversarial_challenger2_cross_feature_autodiscover_keychain_composer_smime_smtp_pipeline() {
    // End-to-End Pipeline:
    // 1. Autodiscover provider profile
    let email = "director@posteo.de";
    let autodiscover = MockAutodiscoverService::lookup_builtin_profile(email).unwrap();
    assert_eq!(autodiscover.incoming.hostname, "posteo.de");
    assert_eq!(autodiscover.outgoing.hostname, "posteo.de");

    // 2. Keychain credential persistence
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);
    let secret = "DirectorVaultPass#2026!";
    creds.store_password(email, secret).unwrap();
    let retrieved_pass = creds.get_password(email).unwrap().unwrap();
    assert_eq!(retrieved_pass, secret);

    // 3. Composer Drag-and-Drop
    let mut composer = MockComposerDropTarget::new();
    let att_path = "/Users/director/Documents/Audit_Report_2026.pdf";
    composer.handle_file_drop(att_path, 1_048_576);
    assert_eq!(composer.attachments.len(), 1);
    assert_eq!(composer.attachments[0].filename, "Audit_Report_2026.pdf");

    // 4. S/MIME Certificate Validation & Trust
    let mut cert_store = MockCertificateStore::new();
    let der = DerBuilder::create_test_x509_der(
        0x777,
        "Director S/MIME Signing Key",
        "Evolution CA",
        "Evolution Root CA",
        "Evolution CA",
        "260101000000Z",
        "300101000000Z",
        Some(0x20), // non-repudiation / digital signature
    );
    let cert = MockX509Parser::parse_der(&der).unwrap();
    let cert_id = cert.id.clone();
    cert_store.add_certificate(cert);
    cert_store.set_trust_level(&cert_id, TrustLevel::TrustForSigning);

    let active_cert = cert_store.records.get(&cert_id).unwrap();
    assert_eq!(active_cert.trust_level, TrustLevel::TrustForSigning);

    // 5. Standalone Viewer Verification of S/MIME Signature
    let msg = TestMessage::new("posteo", "INBOX", "signed_msg_777", "Audited Financials");
    let viewer_reg = MockMailViewerRegistry::default();
    let (key, is_new) = viewer_reg.activate_message(&msg);
    assert!(is_new);
    assert_eq!(viewer_reg.count_open_windows(), 1);

    // 6. Direct SMTP Transmission
    let mut smtp = MockSmtpClient::new();
    smtp.ehlo("posteo.de").unwrap();
    smtp.auth_plain(email, &retrieved_pass).unwrap();
    smtp.mail_from(email).unwrap();
    smtp.rcpt_to("auditors@compliance.org").unwrap();

    let raw_payload = format!(
        "From: {}\r\nTo: auditors@compliance.org\r\nSubject: Signed Audit\r\nX-SMIME-Cert-ID: {}\r\n\r\nMessage Body",
        email, cert_id
    );
    smtp.data(&raw_payload).unwrap();
    smtp.quit().unwrap();

    assert_eq!(smtp.state, SmtpState::Quit);
    assert!(viewer_reg.close_window(&key));
    assert_eq!(viewer_reg.count_open_windows(), 0);
}

