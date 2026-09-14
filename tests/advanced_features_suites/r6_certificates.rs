// SPDX-License-Identifier: GPL-3.0-or-later
//
// R6: Interactive Certificate & Security Manager Comprehensive E2E Tests
// Covers:
// - RFC 5280 X.509 ASN.1 DER parser (TBS, Version, Serial, Issuer, Validity, Subject, SPKI, Extensions)
// - KeyUsage extensions: DigitalSignature, NonRepudiation, KeyEncipherment, KeyCertSign, CRLSign
// - BasicConstraints CA detection and self-signed certificate categorization
// - SHA-256 fingerprint validation (95 chars, uppercase, colon-delimited)
// - Parser byte-by-byte truncation fuzzing (zero panics guarantee)
// - CertificateStore JSON persistence, isolated temp directory CRUD, and reload verification
// - Multi-threaded concurrent store operations
// - OpenPGP key record inspection and conversion
// - Preferences GSettings keys contract (smime-signer-certificate and smime-ca-file)

use super::common::*;
use super::r6_certificate_manager::{
    CertCategory, MockCertificateStore, MockX509Parser, OpenPgpKeyRecord,
    TrustLevel,
};
use gtk::gio::prelude::*;
use std::fs;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

#[test]
fn r6_test_x509_der_parser_nested_structures_and_validity() {
    let der = DerBuilder::create_test_x509_der(
        0xDEADBEEF,
        "Alice Developer",
        "OpenSource Security Org",
        "Internal Intermediate CA",
        "Trust Services Inc",
        "260101000000Z",
        "300101000000Z",
        Some(0x80 | 0x20), // DigitalSignature | KeyEncipherment
    );

    let cert = MockX509Parser::parse_der(&der).expect("Valid DER must parse successfully");
    assert_eq!(cert.subject_cn, "Alice Developer");
    assert_eq!(cert.subject_org, "OpenSource Security Org");
    assert_eq!(cert.issuer_cn, "Internal Intermediate CA");
    assert_eq!(cert.issuer_org, "Trust Services Inc");
    assert_eq!(cert.category, CertCategory::Personal);
    assert_eq!(cert.trust_level, TrustLevel::TrustForSigning);
    assert!(cert.not_before.contains("2026-01-01"));
    assert!(cert.not_after.contains("2030-01-01"));
    assert!(!MockX509Parser::is_expired(&cert, 2026));
}

#[test]
fn r6_test_x509_der_parser_extensions_all_key_usages() {
    let test_cases = vec![
        (0x80, vec!["Digital Signature"]),
        (0x40, vec!["Non-Repudiation"]),
        (0x20, vec!["Key Encipherment"]),
        (0x10, vec!["Data Encipherment"]),
        (0x08, vec!["Key Agreement"]),
        (0x04, vec!["Certificate Sign"]),
        (0x02, vec!["CRL Sign"]),
        (
            0x80 | 0x40 | 0x20,
            vec!["Digital Signature", "Non-Repudiation", "Key Encipherment"],
        ),
        (0x04 | 0x02, vec!["Certificate Sign", "CRL Sign"]),
    ];

    for (bitmask, expected_usages) in test_cases {
        let der = DerBuilder::create_test_x509_der(
            0x42,
            "Key Usage Subject",
            "Test Org",
            "Test CA",
            "Test Org",
            "260101000000Z",
            "300101000000Z",
            Some(bitmask),
        );

        let cert = MockX509Parser::parse_der(&der).expect("DER parse must succeed");
        for expected in expected_usages {
            assert!(
                cert.key_usage.contains(&expected.to_string()),
                "Expected key usage '{}' missing for mask 0x{:02X}, found {:?}",
                expected,
                bitmask,
                cert.key_usage
            );
        }
    }
}

#[test]
fn r6_test_x509_der_parser_basic_constraints_and_ca_detection() {
    // CA Certificate with KeyCertSign and CRLSign
    let ca_der = DerBuilder::create_test_x509_der(
        0x01,
        "Enterprise Root CA",
        "Global Infrastructure",
        "Enterprise Root CA",
        "Global Infrastructure",
        "260101000000Z",
        "360101000000Z",
        Some(0x06), // KeyCertSign | CRLSign
    );

    let ca_cert = MockX509Parser::parse_der(&ca_der).unwrap();
    assert_eq!(ca_cert.category, CertCategory::Authorities);
    assert_eq!(ca_cert.trust_level, TrustLevel::AlwaysTrust);
    assert!(MockX509Parser::is_self_signed(&ca_cert));

    // End-entity user certificate
    let user_der = DerBuilder::create_test_x509_der(
        0x02,
        "Bob User",
        "Global Infrastructure",
        "Enterprise Root CA",
        "Global Infrastructure",
        "260101000000Z",
        "280101000000Z",
        Some(0x80 | 0x20),
    );

    let user_cert = MockX509Parser::parse_der(&user_der).unwrap();
    assert_eq!(user_cert.category, CertCategory::Personal);
    assert_eq!(user_cert.trust_level, TrustLevel::TrustForSigning);
    assert!(!MockX509Parser::is_self_signed(&user_cert));
}

#[test]
fn r6_test_x509_der_sha256_fingerprint_hex_and_format() {
    let der = DerBuilder::create_test_x509_der(
        0x777,
        "Fingerprint Subject",
        "Org",
        "Issuer CA",
        "Org",
        "260501000000Z",
        "300501000000Z",
        None,
    );

    let cert = MockX509Parser::parse_der(&der).unwrap();
    assert_eq!(cert.sha256_fingerprint.len(), 95);
    assert_eq!(cert.sha256_fingerprint.matches(':').count(), 31);

    // Each chunk between colons must be a valid 2-char uppercase hexadecimal
    for chunk in cert.sha256_fingerprint.split(':') {
        assert_eq!(chunk.len(), 2);
        assert!(
            chunk.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
            "Fingerprint chunk must be uppercase hex: {}",
            chunk
        );
    }
}

#[test]
fn r6_test_x509_der_fuzzing_byte_by_byte_graceful_error() {
    let der = DerBuilder::create_test_x509_der(
        0xABCD,
        "Fuzz Target",
        "Test Org",
        "Test CA",
        "Test Org",
        "260101000000Z",
        "300101000000Z",
        Some(0x80),
    );

    // 1. Truncate from length 0 to full length - 1: must return Err and never panic
    for i in 0..der.len() {
        let truncated = &der[..i];
        let result = MockX509Parser::parse_der(truncated);
        assert!(result.is_err(), "Truncated DER at offset {} must fail", i);
    }

    // 2. Corrupt tag bytes
    let mut corrupted = der.clone();
    corrupted[0] = 0x04; // Octet string instead of SEQUENCE
    assert!(MockX509Parser::parse_der(&corrupted).is_err());

    // 3. Corrupted length byte (overflow/excess)
    let mut bad_len = der.clone();
    bad_len[1] = 0xFF;
    assert!(MockX509Parser::parse_der(&bad_len).is_err());
}

#[test]
fn r6_test_certificate_store_isolated_tempdir_full_crud() {
    let temp_dir = std::env::temp_dir().join(format!("evo-m4-test-store-{}", glib::uuid_string_random()));
    let _ = fs::create_dir_all(&temp_dir);
    let store_file = temp_dir.join("certificates.json");

    let mut store = MockCertificateStore::new();

    // Add CA
    let ca_der = DerBuilder::create_test_x509_der(
        0x10,
        "Isolated Root CA",
        "Isolated Network",
        "Isolated Root CA",
        "Isolated Network",
        "260101000000Z",
        "360101000000Z",
        Some(0x06),
    );
    let ca_cert = MockX509Parser::parse_der(&ca_der).unwrap();
    let ca_id = ca_cert.id.clone();
    store.add_certificate(ca_cert);

    // Add User cert
    let user_der = DerBuilder::create_test_x509_der(
        0x20,
        "Isolated User",
        "Isolated Network",
        "Isolated Root CA",
        "Isolated Network",
        "260101000000Z",
        "300101000000Z",
        Some(0x80 | 0x20),
    );
    let user_cert = MockX509Parser::parse_der(&user_der).unwrap();
    let user_id = user_cert.id.clone();
    store.add_certificate(user_cert);

    assert_eq!(store.list_by_category(CertCategory::Authorities).len(), 1);
    assert_eq!(store.list_by_category(CertCategory::Personal).len(), 1);

    // Save to disk
    let json_data = store.export_json();
    fs::write(&store_file, &json_data).expect("Writing store file must succeed");

    // Load from disk into new store
    let loaded_content = fs::read_to_string(&store_file).expect("Reading store file must succeed");
    let mut restored_store = MockCertificateStore::import_json(&loaded_content).unwrap();

    assert_eq!(restored_store.list_by_category(CertCategory::Authorities).len(), 1);
    assert_eq!(restored_store.list_by_category(CertCategory::Personal).len(), 1);

    // Update trust level
    assert!(restored_store.set_trust_level(&user_id, TrustLevel::NeverTrust));
    assert_eq!(
        restored_store.records.get(&user_id).unwrap().trust_level,
        TrustLevel::NeverTrust
    );

    // Remove certificate
    restored_store.records.remove(&ca_id);
    assert_eq!(restored_store.list_by_category(CertCategory::Authorities).len(), 0);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn r6_test_certificate_store_multi_threaded_concurrency() {
    let store = Arc::new(Mutex::new(MockCertificateStore::new()));
    let num_threads = 8;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = Vec::new();

    // Pre-populate with base certificates
    {
        let mut guard = store.lock().unwrap();
        for i in 0..10 {
            let der = DerBuilder::create_test_x509_der(
                i + 1,
                &format!("User {}", i),
                "Concurrent Org",
                "Root CA",
                "Concurrent Org",
                "260101000000Z",
                "300101000000Z",
                None,
            );
            let cert = MockX509Parser::parse_der(&der).unwrap();
            guard.add_certificate(cert);
        }
    }

    for thread_idx in 0..num_threads {
        let store_clone = Arc::clone(&store);
        let barrier_clone = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier_clone.wait();
            for op in 0..20 {
                let mut guard = store_clone.lock().unwrap();
                let personal = guard.list_by_category(CertCategory::Personal);
                if !personal.is_empty() {
                    let target_id = personal[op % personal.len()].id.clone();
                    let new_trust = if thread_idx % 2 == 0 {
                        TrustLevel::AlwaysTrust
                    } else {
                        TrustLevel::TrustForSigning
                    };
                    guard.set_trust_level(&target_id, new_trust);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Concurrent worker thread panicked");
    }

    let guard = store.lock().unwrap();
    assert_eq!(guard.list_by_category(CertCategory::Personal).len(), 10);
}

#[test]
fn r6_test_openpgp_key_record_full_inspection() {
    let pgp_record = OpenPgpKeyRecord {
        key_id: "0x4F92B65431A792E1".to_string(),
        fingerprint: "E1A8 943B 7F21 4F92 B654 31A7 92E1 0123 4567 89AB".to_string(),
        user_ids: vec![
            "Mallory Cryptographer <mallory@security.example.com>".to_string(),
            "Mallory (Work) <m.sec@work.example.org>".to_string(),
        ],
        created_at: "2026-03-01".to_string(),
        expires_at: Some("2029-03-01".to_string()),
        algorithm: "Ed25519".to_string(),
        is_secret: true,
    };

    assert_eq!(pgp_record.key_id, "0x4F92B65431A792E1");
    assert!(pgp_record.is_secret);
    assert_eq!(pgp_record.user_ids.len(), 2);
    assert_eq!(pgp_record.algorithm, "Ed25519");
    assert_eq!(pgp_record.expires_at.as_deref(), Some("2029-03-01"));
}

#[test]
fn r6_test_preferences_dialog_gsettings_smime_wiring() {
    let settings = create_test_settings();
    let schema = settings.settings_schema().expect("Schema must exist");

    assert!(schema.has_key("smime-signer-certificate"));
    assert!(schema.has_key("smime-ca-file"));

    // Set test certificate path
    let test_signer_path = "/var/certs/my_signer.crt";
    let test_ca_path = "/var/certs/corporate_ca.pem";

    settings
        .set_string("smime-signer-certificate", test_signer_path)
        .expect("Must set smime-signer-certificate");
    settings
        .set_string("smime-ca-file", test_ca_path)
        .expect("Must set smime-ca-file");

    assert_eq!(
        settings.string("smime-signer-certificate").as_str(),
        test_signer_path
    );
    assert_eq!(settings.string("smime-ca-file").as_str(), test_ca_path);
}

#[test]
fn r6_test_preferences_dialog_dynamic_smime_combo_reload_contract() {
    let settings = create_test_settings();
    let schema = settings.settings_schema().expect("Schema must exist");
    assert!(schema.has_key("smime-signer-certificate"));
    assert!(schema.has_key("smime-ca-file"));

    let temp_dir = std::env::temp_dir().join(format!("evo-reload-test-{}", glib::uuid_string_random()));
    fs::create_dir_all(&temp_dir).unwrap();
    let store_file = temp_dir.join("certificates.json");

    let mut store = MockCertificateStore::new();

    // 1. Initial State: No certificates in store
    assert_eq!(store.list_by_category(CertCategory::Personal).len(), 0);
    assert_eq!(store.list_by_category(CertCategory::Authorities).len(), 0);

    // Simulated dropdown label models matching preferences_dialog.rs:4974 & 5016
    let mut signer_labels = vec!["Nicht festgelegt".to_string()];
    let mut ca_labels = vec!["System-Vertrauensspeicher (Standard)".to_string()];
    assert_eq!(signer_labels.len(), 1);
    assert_eq!(ca_labels.len(), 1);

    // 2. User opens CertificateManagerDialog and imports a Personal S/MIME certificate
    let user_der = DerBuilder::create_test_x509_der(
        0x101,
        "Alice Developer",
        "OpenSource Security Org",
        "Internal Intermediate CA",
        "Trust Services Inc",
        "260101000000Z",
        "300101000000Z",
        Some(0x80 | 0x20), // DigitalSignature | KeyEncipherment
    );
    let user_cert = MockX509Parser::parse_der(&user_der).unwrap();
    let user_id = user_cert.id.clone();
    store.add_certificate(user_cert);

    // 3. User imports a CA certificate
    let ca_der = DerBuilder::create_test_x509_der(
        0x202,
        "Trust Services Root CA",
        "Trust Services Inc",
        "Trust Services Root CA",
        "Trust Services Inc",
        "260101000000Z",
        "360101000000Z",
        Some(0x06), // KeyCertSign | CRLSign
    );
    let ca_cert = MockX509Parser::parse_der(&ca_der).unwrap();
    let ca_id = ca_cert.id.clone();
    store.add_certificate(ca_cert);

    // Persist store to verify JSON serialization
    fs::write(&store_file, store.export_json()).unwrap();

    // 4. Dialog is closed -> connect_close_request fires refresh_smime_selectors()
    let personal_certs = store.list_by_category(CertCategory::Personal);
    signer_labels = vec!["Nicht festgelegt".to_string()];
    for cert in &personal_certs {
        let label = cert.subject_cn.clone();
        signer_labels.push(label);
    }

    let authority_certs = store.list_by_category(CertCategory::Authorities);
    ca_labels = vec!["System-Vertrauensspeicher (Standard)".to_string()];
    for ca in &authority_certs {
        let label = format!("{} ({})", ca.subject_cn, ca.issuer_org);
        ca_labels.push(label);
    }

    // Verify dynamic model updates
    assert_eq!(signer_labels.len(), 2);
    assert_eq!(signer_labels[1], "Alice Developer");
    assert_eq!(ca_labels.len(), 2);
    assert_eq!(ca_labels[1], "Trust Services Root CA (Trust Services Inc)");

    // 5. Select Alice in GSettings and verify reload maintains selection
    settings
        .set_string("smime-signer-certificate", &user_id)
        .expect("Setting smime-signer-certificate must succeed");
    settings
        .set_string("smime-ca-file", &ca_id)
        .expect("Setting smime-ca-file must succeed");

    let current_signer = settings.string("smime-signer-certificate");
    let mut selected_signer_idx = 0;
    for (i, cert) in personal_certs.iter().enumerate() {
        if current_signer == cert.id {
            selected_signer_idx = i + 1;
            break;
        }
    }
    assert_eq!(selected_signer_idx, 1);

    // 6. Remove certificate from store and simulate reload
    store.records.remove(&user_id);
    let updated_personal = store.list_by_category(CertCategory::Personal);
    signer_labels = vec!["Nicht festgelegt".to_string()];
    for cert in &updated_personal {
        signer_labels.push(cert.subject_cn.clone());
    }
    assert_eq!(signer_labels.len(), 1); // Returns cleanly to default

    let _ = fs::remove_dir_all(&temp_dir);
}

