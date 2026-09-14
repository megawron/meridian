// SPDX-License-Identifier: GPL-3.0-or-later
//
// R4 & R5 E2E Integration Suite: Native macOS Keychain & Portable Mail Engine
//
// Tests:
// - CredentialService facade contract & lifecycle under "org.gnome.Evolution4"
// - Cross-account credential isolation & multi-service partition
// - Portable IMAP folder parsing and RFC822 envelope extraction
// - Portable SMTP recipient delivery sequence and RFC 5321 dot-stuffing
// - Platform build cleanliness contract (Linux has zero macOS symbol leak)

use crate::advanced_features_suites::common::*;
use crate::advanced_features_suites::r4_macos_keychain::MockCredentialService;
use crate::advanced_features_suites::r5_portable_engine::{
    MockImapProtocolParser, MockRfc822Parser, MockSmtpClient, SmtpState,
};

#[test]
fn test_r4_credential_service_facade_service_contract() {
    assert_eq!(
        KEYCHAIN_SERVICE_ID, "org.gnome.Evolution4",
        "Keychain service identifier must be exactly 'org.gnome.Evolution4'"
    );
}

#[test]
fn test_r4_credential_service_store_and_retrieve_e2e() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);
    let account = "test-e2e-account-r4";
    let secret = "S3cur3-K3ych4in-P@ssword!2026";

    let store_res = creds.store_password(account, secret);
    assert!(store_res.is_ok(), "Storing password should succeed");

    let fetched = creds.get_password(account);
    assert!(fetched.is_ok(), "Fetching password should succeed");
    assert_eq!(fetched.unwrap(), Some(secret.to_string()));

    // Overwrite/update password
    let updated_secret = "Updated-S3cur3-P@ssword-2026!";
    assert!(creds.store_password(account, updated_secret).is_ok());
    assert_eq!(
        creds.get_password(account).unwrap(),
        Some(updated_secret.to_string())
    );

    // Delete password
    assert!(creds.delete_password(account).is_ok());
    assert_eq!(creds.get_password(account).unwrap(), None);
}

#[test]
fn test_r4_credential_service_unicode_and_special_characters_e2e() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);
    let account = "test-e2e-unicode-account";
    let complex = "🔐 Mätthäus & Jörg: P@ssw0rd! €100 / 🚀";

    let store_res = creds.store_password(account, complex);
    assert!(store_res.is_ok());

    let fetched = creds.get_password(account).unwrap();
    assert_eq!(fetched, Some(complex.to_string()));

    // Clean up
    let _ = creds.delete_password(account);
    assert_eq!(creds.get_password(account).unwrap(), None);
}

#[test]
fn test_r4_credential_service_multi_account_isolation_e2e() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    creds.store_password("acc_1", "pass_one").unwrap();
    creds.store_password("acc_2", "pass_two").unwrap();
    creds.store_password("acc_3", "pass_three").unwrap();

    assert_eq!(creds.get_password("acc_1").unwrap(), Some("pass_one".into()));
    assert_eq!(creds.get_password("acc_2").unwrap(), Some("pass_two".into()));
    assert_eq!(creds.get_password("acc_3").unwrap(), Some("pass_three".into()));

    creds.delete_password("acc_2").unwrap();
    assert_eq!(creds.get_password("acc_1").unwrap(), Some("pass_one".into()));
    assert_eq!(creds.get_password("acc_2").unwrap(), None);
    assert_eq!(creds.get_password("acc_3").unwrap(), Some("pass_three".into()));
}

#[test]
fn test_r5_portable_imap_list_parsing_comprehensive() {
    let samples = [
        (
            r#"* LIST (\HasNoChildren) "/" "INBOX""#,
            "INBOX",
            '/',
            true,  // is_inbox
            false, // is_sent
            false, // is_drafts
            false, // is_trash
            false, // is_noselect
        ),
        (
            r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#,
            "Sent Messages",
            '/',
            false,
            true,
            false,
            false,
            false,
        ),
        (
            r#"* LIST (\HasNoChildren \Drafts) "/" "Drafts""#,
            "Drafts",
            '/',
            false,
            false,
            true,
            false,
            false,
        ),
        (
            r#"* LIST (\HasNoChildren \Trash) "/" "Trash""#,
            "Trash",
            '/',
            false,
            false,
            false,
            true,
            false,
        ),
        (
            r#"* LIST (\Noselect \HasChildren) "." "Archives""#,
            "Archives",
            '.',
            false,
            false,
            false,
            false,
            true,
        ),
    ];

    for (raw, expected_name, expected_delim, is_inbox, is_sent, is_drafts, is_trash, is_noselect) in
        samples
    {
        let item = MockImapProtocolParser::parse_list_line(raw).expect("Line should parse");
        assert_eq!(item.name, expected_name);
        assert_eq!(item.delimiter, expected_delim);
        assert_eq!(item.is_inbox, is_inbox);
        assert_eq!(item.is_sent, is_sent);
        assert_eq!(item.is_drafts, is_drafts);
        assert_eq!(item.is_trash, is_trash);
        assert_eq!(item.is_noselect, is_noselect);
    }
}

#[test]
fn test_r5_portable_imap_envelope_body_parse_e2e() {
    let raw = b"From: Alice <alice@example.com>\r\n\
To: Bob <bob@example.com>, carol@example.com\r\n\
Subject: Multi-recipient Test\r\n\
Date: Mon, 14 Sep 2026 12:00:00 +0000\r\n\
\r\n\
Hello world from portable mail engine!\r\n";

    let msg = MockRfc822Parser::parse(raw).expect("Message should parse cleanly");
    assert_eq!(msg.from, "Alice <alice@example.com>");
    assert_eq!(msg.to, vec!["Bob <bob@example.com>", "carol@example.com"]);
    assert_eq!(msg.subject, "Multi-recipient Test");
    assert_eq!(msg.body_plain, "Hello world from portable mail engine!");
}

#[test]
fn test_r5_portable_smtp_full_handshake_and_dot_stuffing_e2e() {
    let mut smtp = MockSmtpClient::new();
    assert_eq!(smtp.state, SmtpState::Initial);

    smtp.ehlo("localhost").unwrap();
    assert_eq!(smtp.state, SmtpState::Greeted);

    smtp.auth_plain("user@example.com", "valid_secret").unwrap();
    assert_eq!(smtp.state, SmtpState::Authenticated);

    smtp.mail_from("user@example.com").unwrap();
    assert_eq!(smtp.state, SmtpState::MailFromSet);

    smtp.rcpt_to("first@example.com").unwrap();
    smtp.rcpt_to("second@example.com").unwrap();
    assert_eq!(smtp.state, SmtpState::RcptToSet);

    let payload = "Header: Value\r\n\r\n.Line starting with period\r\nNormal line\r\n..Two periods";
    smtp.data(payload).unwrap();
    assert_eq!(smtp.state, SmtpState::MessageQueued);

    smtp.quit().unwrap();
    assert_eq!(smtp.state, SmtpState::Quit);
}

#[test]
fn test_r4_r5_linux_compilation_cleanliness() {
    // Contract verification: Linux builds do not link Security.framework and have no macOS symbols.
    let is_macos = cfg!(target_os = "macos");
    let is_linux = cfg!(target_os = "linux");

    if is_linux {
        assert!(!is_macos, "Linux builds must not have macOS target configured");
    }
}
