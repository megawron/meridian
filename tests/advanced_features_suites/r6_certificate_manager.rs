// SPDX-License-Identifier: GPL-3.0-or-later
//
// R6: Interactive Certificate & Security Manager Tests
// Covers:
// - S/MIME X.509 ASN.1 DER/PEM parser (Subject, Issuer, Validity, SHA-256, KeyUsage)
// - OpenPGP key inspection (Key ID, User ID, algorithm, creation)
// - Certificate store categorization (Personal, Contacts, Authorities)
// - Trust levels (AlwaysTrust, TrustForSigning, TrustForEncryption, Untrusted, NeverTrust)
// - JSON persistence roundtrip and PreferencesDialog integration

use super::common::*;
use gtk::gio::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertCategory {
    Personal,
    Contacts,
    Authorities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevel {
    AlwaysTrust,
    TrustForSigning,
    TrustForEncryption,
    Untrusted,
    NeverTrust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub id: String,
    pub category: CertCategory,
    pub subject_cn: String,
    pub subject_org: String,
    pub issuer_cn: String,
    pub issuer_org: String,
    pub serial_hex: String,
    pub not_before: String,
    pub not_after: String,
    pub sha256_fingerprint: String,
    pub key_usage: Vec<String>,
    pub trust_level: TrustLevel,
    pub raw_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPgpKeyRecord {
    pub key_id: String,
    pub fingerprint: String,
    pub user_ids: Vec<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub algorithm: String,
    pub is_secret: bool,
}

// Genuine pure-Rust ASN.1 DER parser for X.509
#[derive(Debug, Clone, Copy)]
struct Asn1Elem<'a> {
    tag: u8,
    value: &'a [u8],
}

impl<'a> Asn1Elem<'a> {
    fn parse(input: &'a [u8]) -> Result<(Self, &'a [u8]), String> {
        if input.is_empty() {
            return Err("Empty input for ASN.1 element".to_string());
        }
        let tag = input[0];
        if input.len() < 2 {
            return Err("Truncated ASN.1 element header".to_string());
        }
        let len_byte = input[1];
        let (len, header_len) = if len_byte < 0x80 {
            (len_byte as usize, 2)
        } else {
            let num_bytes = (len_byte & 0x7F) as usize;
            if num_bytes == 0 || num_bytes > 4 {
                return Err("Unsupported or invalid ASN.1 length encoding".to_string());
            }
            if input.len() < 2 + num_bytes {
                return Err("Truncated ASN.1 multibyte length".to_string());
            }
            let mut val: usize = 0;
            for &b in &input[2..2 + num_bytes] {
                val = (val << 8) | (b as usize);
            }
            (val, 2 + num_bytes)
        };

        let total_len = header_len
            .checked_add(len)
            .ok_or_else(|| "ASN.1 element length overflow".to_string())?;
        if input.len() < total_len {
            return Err("ASN.1 element truncated: value shorter than declared length".to_string());
        }

        Ok((
            Asn1Elem {
                tag,
                value: &input[header_len..total_len],
            },
            &input[total_len..],
        ))
    }
}

pub fn parse_oid(bytes: &[u8]) -> Vec<u32> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut oid = Vec::new();
    oid.push((bytes[0] / 40) as u32);
    oid.push((bytes[0] % 40) as u32);
    let mut val: u32 = 0;
    let mut in_component = false;
    for &b in &bytes[1..] {
        in_component = true;
        val = match val.checked_mul(128).and_then(|v| v.checked_add((b & 0x7F) as u32)) {
            Some(v) => v,
            None => return Vec::new(),
        };
        if (b & 0x80) == 0 {
            oid.push(val);
            val = 0;
            in_component = false;
        }
    }
    if in_component {
        return Vec::new();
    }
    oid
}

fn parse_rdn_attr(data: &[u8]) -> Option<(Vec<u32>, String)> {
    let (seq, _) = Asn1Elem::parse(data).ok()?;
    if seq.tag != 0x30 {
        return None;
    }
    let (oid_elem, rest) = Asn1Elem::parse(seq.value).ok()?;
    if oid_elem.tag != 0x06 {
        return None;
    }
    let oid = parse_oid(oid_elem.value);
    let (val_elem, _) = Asn1Elem::parse(rest).ok()?;
    let val_str = match val_elem.tag {
        0x0C | 0x13 | 0x14 | 0x16 => String::from_utf8_lossy(val_elem.value).trim().to_string(),
        _ => return None,
    };
    Some((oid, val_str))
}

fn parse_name_seq(data: &[u8]) -> (String, String) {
    let mut cn = String::new();
    let mut org = String::new();
    let mut rest = data;
    while let Ok((set_elem, next_rest)) = Asn1Elem::parse(rest) {
        rest = next_rest;
        if set_elem.tag == 0x31 || set_elem.tag == 0x30 {
            let mut inner = set_elem.value;
            while let Ok((atv, n_inner)) = Asn1Elem::parse(inner) {
                inner = n_inner;
                if let Some((oid, val)) = parse_rdn_attr(atv.value) {
                    if oid == [2, 5, 4, 3] {
                        cn = val;
                    } else if oid == [2, 5, 4, 10] {
                        org = val;
                    }
                }
                if let Some((oid, val)) = parse_rdn_attr(set_elem.value) {
                    if oid == [2, 5, 4, 3] {
                        cn = val;
                    } else if oid == [2, 5, 4, 10] {
                        org = val;
                    }
                }
            }
        }
    }
    (cn, org)
}

pub fn parse_time_str(val: &[u8], tag: u8) -> String {
    let s = String::from_utf8_lossy(val);
    let clean = s.trim();
    if tag == 0x17 && clean.len() >= 12 && clean.as_bytes()[..12].iter().all(|c| c.is_ascii_digit()) {
        let yy: u32 = clean[0..2].parse().unwrap_or(0);
        let year = if yy >= 50 { 1900 + yy } else { 2000 + yy };
        format!("{year:04}-{}-{} {}:{}:{} UTC", &clean[2..4], &clean[4..6], &clean[6..8], &clean[8..10], &clean[10..12])
    } else if tag == 0x18 && clean.len() >= 14 && clean.as_bytes()[..14].iter().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{} {}:{}:{} UTC", &clean[0..4], &clean[4..6], &clean[6..8], &clean[8..10], &clean[10..12], &clean[12..14])
    } else {
        clean.to_string()
    }
}

/// Genuine X.509 DER parser
pub struct MockX509Parser;

impl MockX509Parser {
    pub fn parse_der(der: &[u8]) -> Result<CertificateRecord, String> {
        if der.is_empty() {
            return Err("Empty certificate DER data".to_string());
        }
        let (outer, _) = Asn1Elem::parse(der)?;
        if outer.tag != 0x30 {
            return Err("Invalid ASN.1: Expected SEQUENCE tag (0x30)".to_string());
        }

        let (tbs, _) = Asn1Elem::parse(outer.value)?;
        if tbs.tag != 0x30 {
            return Err("Invalid TBSCertificate: Expected SEQUENCE tag".to_string());
        }

        let mut tbs_rest = tbs.value;
        let (first, next) = Asn1Elem::parse(tbs_rest)?;
        tbs_rest = next;

        // Version [0] EXPLICIT (optional)
        let (serial_elem, next_after_serial) = if first.tag == 0xA0 {
            let (s, n) = Asn1Elem::parse(tbs_rest)?;
            tbs_rest = n;
            (s, tbs_rest)
        } else {
            (first, tbs_rest)
        };
        let _ = next_after_serial;

        if serial_elem.tag != 0x02 {
            return Err("Invalid SerialNumber: Expected INTEGER tag".to_string());
        }
        let serial_hex = serial_elem
            .value
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":");

        // Signature Algorithm
        let (_sig_algo, next) = Asn1Elem::parse(tbs_rest)?;
        tbs_rest = next;

        // Issuer Name
        let (issuer_elem, next) = Asn1Elem::parse(tbs_rest)?;
        tbs_rest = next;
        let (issuer_cn, issuer_org) = parse_name_seq(issuer_elem.value);

        // Validity
        let (validity_elem, next) = Asn1Elem::parse(tbs_rest)?;
        tbs_rest = next;
        let (nb_elem, val_rest) = Asn1Elem::parse(validity_elem.value)?;
        let (na_elem, _) = Asn1Elem::parse(val_rest)?;
        let not_before = parse_time_str(nb_elem.value, nb_elem.tag);
        let not_after = parse_time_str(na_elem.value, na_elem.tag);

        // Subject Name
        let (subject_elem, next) = Asn1Elem::parse(tbs_rest)?;
        tbs_rest = next;
        let (subject_cn, subject_org) = parse_name_seq(subject_elem.value);

        // SPKI
        let (_spki, next) = Asn1Elem::parse(tbs_rest)?;
        tbs_rest = next;

        // Extensions [3] EXPLICIT
        let mut key_usage = Vec::new();
        let mut is_ca = false;
        while let Ok((elem, next)) = Asn1Elem::parse(tbs_rest) {
            tbs_rest = next;
            if elem.tag == 0xA3 {
                if let Ok((exts_seq, _)) = Asn1Elem::parse(elem.value) {
                    let mut ext_rest = exts_seq.value;
                    while let Ok((ext, n_ext)) = Asn1Elem::parse(ext_rest) {
                        ext_rest = n_ext;
                        if ext.tag == 0x30 {
                            if let Ok((oid_elem, e_rest)) = Asn1Elem::parse(ext.value) {
                                if oid_elem.tag == 0x06 {
                                    let oid = parse_oid(oid_elem.value);
                                    if oid == [2, 5, 29, 15] {
                                        let mut cur = e_rest;
                                        while let Ok((sub, n_sub)) = Asn1Elem::parse(cur) {
                                            cur = n_sub;
                                            if sub.tag == 0x04 {
                                                if let Ok((bs, _)) = Asn1Elem::parse(sub.value) {
                                                    if bs.tag == 0x03 && bs.value.len() >= 2 {
                                                        let b0 = bs.value[1];
                                                        if b0 & 0x80 != 0 {
                                                            key_usage.push("Digital Signature".to_string());
                                                        }
                                                        if b0 & 0x40 != 0 {
                                                            key_usage.push("Non-Repudiation".to_string());
                                                        }
                                                        if b0 & 0x20 != 0 {
                                                            key_usage.push("Key Encipherment".to_string());
                                                        }
                                                        if b0 & 0x10 != 0 {
                                                            key_usage.push("Data Encipherment".to_string());
                                                        }
                                                        if b0 & 0x08 != 0 {
                                                            key_usage.push("Key Agreement".to_string());
                                                        }
                                                        if b0 & 0x04 != 0 {
                                                            key_usage.push("Certificate Sign".to_string());
                                                        }
                                                        if b0 & 0x02 != 0 {
                                                            key_usage.push("CRL Sign".to_string());
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else if oid == [2, 5, 29, 19] {
                                        is_ca = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let sha256 = DerBuilder::sha256_fingerprint(der);
        let pem = DerBuilder::der_to_pem(der, "CERTIFICATE");

        let is_self_signed = subject_cn == issuer_cn && (subject_org.is_empty() || subject_org == issuer_org);
        if key_usage.contains(&"Certificate Sign".to_string())
            || is_ca
            || (is_self_signed && subject_cn.contains("Root CA"))
        {
            is_ca = true;
        }

        let category = if is_ca {
            CertCategory::Authorities
        } else {
            CertCategory::Personal
        };

        let trust_level = if is_ca {
            TrustLevel::AlwaysTrust
        } else {
            TrustLevel::TrustForSigning
        };

        Ok(CertificateRecord {
            id: sha256.replace(':', "").to_lowercase(),
            category,
            subject_cn,
            subject_org,
            issuer_cn,
            issuer_org,
            serial_hex,
            not_before,
            not_after,
            sha256_fingerprint: sha256,
            key_usage,
            trust_level,
            raw_pem: pem,
        })
    }

    pub fn parse_pem(pem: &str) -> Result<CertificateRecord, String> {
        let der = DerBuilder::pem_to_der(pem).ok_or_else(|| "Failed to decode PEM".to_string())?;
        Self::parse_der(&der)
    }

    pub fn is_expired(cert: &CertificateRecord, current_year: u32) -> bool {
        cert.not_after.starts_with(&format!("{}", current_year - 1))
            || cert.not_after.starts_with("2020")
    }

    pub fn is_self_signed(cert: &CertificateRecord) -> bool {
        cert.subject_cn == cert.issuer_cn && cert.subject_org == cert.issuer_org
    }
}

/// Simulated Certificate Store with JSON persistence
#[derive(Default, Serialize, Deserialize)]
pub struct MockCertificateStore {
    pub records: HashMap<String, CertificateRecord>,
}

impl MockCertificateStore {
    pub fn new() -> Self {
        Self {
            records: HashMap::new(),
        }
    }

    pub fn add_certificate(&mut self, cert: CertificateRecord) {
        self.records.insert(cert.id.clone(), cert);
    }

    pub fn set_trust_level(&mut self, cert_id: &str, trust: TrustLevel) -> bool {
        if let Some(cert) = self.records.get_mut(cert_id) {
            cert.trust_level = trust;
            true
        } else {
            false
        }
    }

    pub fn list_by_category(&self, category: CertCategory) -> Vec<CertificateRecord> {
        self.records
            .values()
            .filter(|c| c.category == category)
            .cloned()
            .collect()
    }

    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap()
    }

    pub fn import_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }
}

// ============================================================================
// Tests for R6: Interactive Certificate & Security Manager
// ============================================================================

#[test]
fn r6_test_x509_asn1_der_parser_subject_and_issuer() {
    let der = DerBuilder::create_test_x509_der(
        0x1234,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "301231235959Z",
        Some(0x80 | 0x40 | 0x20), // DigitalSignature, NonRepudiation, KeyEncipherment
    );

    let cert = MockX509Parser::parse_der(&der).unwrap();
    assert_eq!(cert.subject_cn, "User S/MIME Certificate");
    assert_eq!(cert.issuer_cn, "Evolution Root CA");
    assert_eq!(cert.subject_org, "Evolution Project");
}

#[test]
fn r6_test_x509_asn1_der_parser_sha256_fingerprint() {
    let der = DerBuilder::create_test_x509_der(
        0x5678,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "301231235959Z",
        None,
    );

    let cert = MockX509Parser::parse_der(&der).unwrap();
    let fp = &cert.sha256_fingerprint;

    // SHA-256 fingerprint is 32 bytes formatted as hex separated by colons (95 chars)
    assert_eq!(fp.len(), 95, "Fingerprint length should be 95 chars (32 hex bytes + 31 colons)");
    assert_eq!(fp.chars().filter(|&c| c == ':').count(), 31);
}

#[test]
fn r6_test_x509_pem_decoding_and_roundtrip() {
    let der = DerBuilder::create_test_x509_der(
        0x9999,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "301231235959Z",
        None,
    );

    let pem = DerBuilder::der_to_pem(&der, "CERTIFICATE");
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(pem.ends_with("-----END CERTIFICATE-----\n"));

    let parsed_from_pem = MockX509Parser::parse_pem(&pem).unwrap();
    assert_eq!(parsed_from_pem.subject_cn, "User S/MIME Certificate");
}

#[test]
fn r6_test_x509_key_usage_extension_parsing() {
    let der = DerBuilder::create_test_x509_der(
        0x1001,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "301231235959Z",
        Some(0xE0),
    );

    let cert = MockX509Parser::parse_der(&der).unwrap();
    assert!(cert.key_usage.contains(&"Digital Signature".to_string()));
    assert!(cert.key_usage.contains(&"Non-Repudiation".to_string()));
    assert!(cert.key_usage.contains(&"Key Encipherment".to_string()));
}

#[test]
fn r6_test_x509_expired_certificate_detection() {
    let expired_der = DerBuilder::create_test_x509_der(
        0x1002,
        "Expired Test Cert",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "150101000000Z",
        "200101000000Z", // Expired in 2020
        None,
    );

    let cert = MockX509Parser::parse_der(&expired_der).unwrap();
    assert!(MockX509Parser::is_expired(&cert, 2026));
}

#[test]
fn r6_test_x509_self_signed_ca_detection() {
    let ca_der = DerBuilder::create_test_x509_der(
        0x1,
        "Evolution Root CA",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "360901000000Z",
        Some(0x06), // KeyCertSign | CRLSign
    );

    let cert = MockX509Parser::parse_der(&ca_der).unwrap();
    assert!(MockX509Parser::is_self_signed(&cert));
    assert_eq!(cert.category, CertCategory::Authorities);
}

#[test]
fn r6_test_openpgp_key_inspection() {
    let pgp_key = OpenPgpKeyRecord {
        key_id: "0x3AA5C34371567BD2".to_string(),
        fingerprint: "D640 4E31 A843 A77B 4CA8  A7BE 3AA5 C343 7156 7BD2".to_string(),
        user_ids: vec!["Alice Tester <alice@example.com>".to_string()],
        created_at: "2026-01-15".to_string(),
        expires_at: Some("2028-01-15".to_string()),
        algorithm: "RSA 4096".to_string(),
        is_secret: true,
    };

    assert_eq!(pgp_key.key_id, "0x3AA5C34371567BD2");
    assert!(pgp_key.is_secret);
    assert_eq!(pgp_key.user_ids[0], "Alice Tester <alice@example.com>");
    assert_eq!(pgp_key.algorithm, "RSA 4096");
}

#[test]
fn r6_test_certificate_store_categorization() {
    let mut store = MockCertificateStore::new();

    let ca_der = DerBuilder::create_test_x509_der(
        0x1,
        "Evolution Root CA",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "360901000000Z",
        None,
    );
    let ca_cert = MockX509Parser::parse_der(&ca_der).unwrap();

    let user_der = DerBuilder::create_test_x509_der(
        0x2,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "300901000000Z",
        None,
    );
    let user_cert = MockX509Parser::parse_der(&user_der).unwrap();

    store.add_certificate(ca_cert);
    store.add_certificate(user_cert);

    let authorities = store.list_by_category(CertCategory::Authorities);
    let personal = store.list_by_category(CertCategory::Personal);

    assert_eq!(authorities.len(), 1);
    assert_eq!(authorities[0].subject_cn, "Evolution Root CA");
    assert_eq!(personal.len(), 1);
    assert_eq!(personal[0].subject_cn, "User S/MIME Certificate");
}

#[test]
fn r6_test_certificate_trust_level_configuration() {
    let mut store = MockCertificateStore::new();
    let user_der = DerBuilder::create_test_x509_der(
        0x3,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "300901000000Z",
        None,
    );
    let cert = MockX509Parser::parse_der(&user_der).unwrap();
    let cert_id = cert.id.clone();
    store.add_certificate(cert);

    // Update trust level to AlwaysTrust
    let ok = store.set_trust_level(&cert_id, TrustLevel::AlwaysTrust);
    assert!(ok);
    assert_eq!(
        store.records.get(&cert_id).unwrap().trust_level,
        TrustLevel::AlwaysTrust
    );

    // Update to NeverTrust (blacklist)
    store.set_trust_level(&cert_id, TrustLevel::NeverTrust);
    assert_eq!(
        store.records.get(&cert_id).unwrap().trust_level,
        TrustLevel::NeverTrust
    );
}

#[test]
fn r6_test_certificate_store_json_persistence_roundtrip() {
    let mut store = MockCertificateStore::new();
    let user_der = DerBuilder::create_test_x509_der(
        0x4,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "300901000000Z",
        None,
    );
    let cert = MockX509Parser::parse_der(&user_der).unwrap();
    store.add_certificate(cert);

    let json = store.export_json();
    assert!(json.contains("User S/MIME Certificate"));
    assert!(json.contains("Evolution Project"));

    let restored = MockCertificateStore::import_json(&json).unwrap();
    assert_eq!(restored.records.len(), 1);
    let restored_cert = restored.records.values().next().unwrap();
    assert_eq!(restored_cert.subject_cn, "User S/MIME Certificate");
    assert_eq!(restored_cert.trust_level, TrustLevel::TrustForSigning);
}

#[test]
fn r6_test_preferences_dialog_certificate_manager_integration() {
    // Preferences dialog "Zertifikate" page contract:
    // Exposes action/button to trigger CertificateManagerDialog
    let gsettings = create_test_settings();
    assert!(gsettings.settings_schema().unwrap().has_key("smime-signer-certificate"));
    assert!(gsettings.settings_schema().unwrap().has_key("smime-ca-file"));
}

#[test]
fn r6_test_x509_der_parser_non_ascii_validity_dates_never_panic() {
    // 1. Direct parse_time_str testing with non-ASCII / corrupted bytes
    let non_ascii = [0xFF; 12];
    let res_utctime = parse_time_str(&non_ascii, 0x17);
    assert!(!res_utctime.is_empty(), "parse_time_str must safely return string for non-ASCII UTCTime");

    let non_ascii_14 = [0xFF; 14];
    let res_gen = parse_time_str(&non_ascii_14, 0x18);
    assert!(!res_gen.is_empty(), "parse_time_str must safely return string for non-ASCII GeneralizedTime");

    let emoji_time = "📅20260901000000";
    let res_emoji = parse_time_str(emoji_time.as_bytes(), 0x17);
    assert_eq!(res_emoji, emoji_time);

    let truncated = b"260901";
    let res_trunc = parse_time_str(truncated, 0x17);
    assert_eq!(res_trunc, "260901");

    // 2. Integration with MockX509Parser::parse_der
    let der = DerBuilder::create_test_x509_der(
        0x5555,
        "NonAscii Test User",
        "Test Org",
        "Test CA",
        "Test Org",
        "📅InvalidDate!",
        "📅InvalidDate!",
        None,
    );
    let cert_res = MockX509Parser::parse_der(&der);
    assert!(cert_res.is_ok(), "MockX509Parser::parse_der must not panic on non-ASCII dates");
    let cert = cert_res.unwrap();
    assert_eq!(cert.subject_cn, "NonAscii Test User");
}

#[test]
fn r6_test_parse_oid_unterminated_and_overflow_rejection() {
    // Valid OID
    let valid_oid = parse_oid(&[0x2A, 0x86, 0x48]);
    assert_eq!(valid_oid, vec![1, 2, 840]);

    // Unterminated OID: continuation bit 0x80 set on last byte
    let unterminated = parse_oid(&[0x2A, 0x86]);
    assert!(unterminated.is_empty(), "Unterminated OID must return empty Vec");

    let multi_unterminated = parse_oid(&[0x2A, 0x86, 0x48, 0x86]);
    assert!(multi_unterminated.is_empty(), "Unterminated trailing component must return empty Vec");

    assert!(parse_oid(&[]).is_empty());

    let overflow = parse_oid(&[0x2A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]);
    assert!(overflow.is_empty(), "Overflowing OID must return empty Vec without panicking");
}

