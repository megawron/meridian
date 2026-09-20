// SPDX-License-Identifier: GPL-3.0-or-later
//
// Common test fixtures, helpers, and mock models for Evolution GTK4 Advanced Features (R1-R6)

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use gtk::gio;
use gtk::gio::prelude::*;

pub const SCHEMA_ID: &str = "com.martingawron.Meridian";
pub const KEYCHAIN_SERVICE_ID: &str = "com.martingawron.Meridian";

pub const ERR_SEC_SUCCESS: i32 = 0;
pub const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
pub const ERR_SEC_DUPLICATE_ITEM: i32 = -25299;
pub const ERR_SEC_PARAM: i32 = -50;

/// Ensure GTK is initialized safely for tests that instantiate widgets.
pub fn init_gtk() -> bool {
    static GTK_INIT: OnceLock<bool> = OnceLock::new();
    *GTK_INIT.get_or_init(|| {
        // GTK init can succeed in headless mode or return true
        gtk::init().is_ok()
    })
}

/// Obtain the compiled GSettingsSchema for Meridian.
pub fn get_test_schema() -> gio::SettingsSchema {
    let installed_source = gio::SettingsSchemaSource::default();
    installed_source
        .as_ref()
        .and_then(|source| source.lookup(SCHEMA_ID, true))
        .or_else(|| {
            let env_dir = std::env::var("MERIDIAN_SCHEMA_DIR")
                .or_else(|_| std::env::var("EVOLUTION_GTK4_SCHEMA_DIR"))
                .ok();
            let directory = option_env!("MERIDIAN_SCHEMA_DIR")
                .or(option_env!("EVOLUTION_GTK4_SCHEMA_DIR"))
                .or(env_dir.as_deref())?;
            let source = gio::SettingsSchemaSource::from_directory(
                directory,
                installed_source.as_ref(),
                true,
            )
            .ok()?;
            source.lookup(SCHEMA_ID, true)
        })
        .expect("the application settings schema com.martingawron.Meridian must be available in tests")
}

/// Create an isolated, in-memory Settings instance for test execution.
pub fn create_test_settings() -> gio::Settings {
    let schema = get_test_schema();
    let backend = gio::memory_settings_backend_new();
    gio::Settings::new_full(&schema, Some(&backend), None)
}

// ============================================================================
// Data Models for Testing
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestAttachment {
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct TestMessage {
    pub source_uid: String,
    pub folder_name: String,
    pub msg_uid: String,
    pub subject: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub date: String,
    pub body_plain: String,
    pub body_html: String,
    pub attachments: Vec<TestAttachment>,
    pub is_read: bool,
    pub is_junk: bool,
    pub is_deleted: bool,
    pub has_itip: bool,
    pub raw_rfc822: Vec<u8>,
}

impl TestMessage {
    pub fn new(source_uid: &str, folder: &str, msg_uid: &str, subject: &str) -> Self {
        let raw = format!(
            "From: sender@example.com\r\nTo: recipient@example.com\r\nSubject: {}\r\nDate: Sun, 13 Sep 2026 18:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThis is test message body.",
            subject
        )
        .into_bytes();

        Self {
            source_uid: source_uid.to_string(),
            folder_name: folder.to_string(),
            msg_uid: msg_uid.to_string(),
            subject: subject.to_string(),
            from: "sender@example.com".to_string(),
            to: vec!["recipient@example.com".to_string()],
            cc: Vec::new(),
            date: "2026-09-13 18:00:00".to_string(),
            body_plain: "This is test message body.".to_string(),
            body_html: "<p>This is test message body.</p>".to_string(),
            attachments: Vec::new(),
            is_read: false,
            is_junk: false,
            is_deleted: false,
            has_itip: false,
            raw_rfc822: raw,
        }
    }

    pub fn composite_key(&self) -> String {
        format!("backend\x1f{}\x1f{}\x1f{}", self.source_uid, self.folder_name, self.msg_uid)
    }
}

#[derive(Debug, Clone)]
pub struct TestFolder {
    pub id: String,
    pub name: String,
    pub unread_count: u32,
    pub total_count: u32,
    pub is_trash: bool,
    pub is_junk: bool,
}

impl TestFolder {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            unread_count: 0,
            total_count: 0,
            is_trash: name.eq_ignore_ascii_case("trash"),
            is_junk: name.eq_ignore_ascii_case("junk"),
        }
    }
}

// ============================================================================
// macOS Keychain Mock Backend
// ============================================================================

#[derive(Default)]
pub struct MockKeychainStore {
    // (service, account) -> password
    entries: RwLock<HashMap<(String, String), String>>,
}

impl MockKeychainStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_password(&self, service: &str, account: &str, password: &str) -> i32 {
        if service.is_empty() || account.is_empty() {
            return ERR_SEC_PARAM;
        }
        let mut map = self.entries.write().unwrap();
        map.insert((service.to_string(), account.to_string()), password.to_string());
        ERR_SEC_SUCCESS
    }

    pub fn get_password(&self, service: &str, account: &str) -> Result<Option<String>, i32> {
        if service.is_empty() || account.is_empty() {
            return Err(ERR_SEC_PARAM);
        }
        let map = self.entries.read().unwrap();
        Ok(map.get(&(service.to_string(), account.to_string())).cloned())
    }

    pub fn delete_password(&self, service: &str, account: &str) -> i32 {
        if service.is_empty() || account.is_empty() {
            return ERR_SEC_PARAM;
        }
        let mut map = self.entries.write().unwrap();
        if map.remove(&(service.to_string(), account.to_string())).is_some() {
            ERR_SEC_SUCCESS
        } else {
            ERR_SEC_ITEM_NOT_FOUND
        }
    }

    pub fn count_service_entries(&self, service: &str) -> usize {
        let map = self.entries.read().unwrap();
        map.keys().filter(|(s, _)| s == service).count()
    }
}

// ============================================================================
// Pure-Rust Minimal ASN.1 DER Encoder / Helper for X.509 Testing
// ============================================================================

pub struct DerBuilder;

impl DerBuilder {
    pub fn encode_length(len: usize) -> Vec<u8> {
        if len < 128 {
            vec![len as u8]
        } else if len < 256 {
            vec![0x81, len as u8]
        } else if len < 65536 {
            vec![0x82, (len >> 8) as u8, (len & 0xff) as u8]
        } else {
            vec![
                0x83,
                (len >> 16) as u8,
                ((len >> 8) & 0xff) as u8,
                (len & 0xff) as u8,
            ]
        }
    }

    pub fn sequence(contents: &[u8]) -> Vec<u8> {
        let mut out = vec![0x30];
        out.extend(Self::encode_length(contents.len()));
        out.extend_from_slice(contents);
        out
    }

    pub fn set(contents: &[u8]) -> Vec<u8> {
        let mut out = vec![0x31];
        out.extend(Self::encode_length(contents.len()));
        out.extend_from_slice(contents);
        out
    }

    pub fn integer(val: &[u8]) -> Vec<u8> {
        let mut out = vec![0x02];
        // Ensure positive representation
        if !val.is_empty() && (val[0] & 0x80) != 0 {
            out.extend(Self::encode_length(val.len() + 1));
            out.push(0x00);
        } else {
            out.extend(Self::encode_length(val.len()));
        }
        out.extend_from_slice(val);
        out
    }

    pub fn bit_string(unused_bits: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x03];
        out.extend(Self::encode_length(data.len() + 1));
        out.push(unused_bits);
        out.extend_from_slice(data);
        out
    }

    pub fn octet_string(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x04];
        out.extend(Self::encode_length(data.len()));
        out.extend_from_slice(data);
        out
    }

    pub fn utf8_string(s: &str) -> Vec<u8> {
        let mut out = vec![0x0C];
        out.extend(Self::encode_length(s.len()));
        out.extend_from_slice(s.as_bytes());
        out
    }

    pub fn printable_string(s: &str) -> Vec<u8> {
        let mut out = vec![0x13];
        out.extend(Self::encode_length(s.len()));
        out.extend_from_slice(s.as_bytes());
        out
    }

    pub fn ia5_string(s: &str) -> Vec<u8> {
        let mut out = vec![0x16];
        out.extend(Self::encode_length(s.len()));
        out.extend_from_slice(s.as_bytes());
        out
    }

    pub fn utc_time(time_str: &str) -> Vec<u8> {
        // e.g. "260913120000Z" (YYMMDDHHMMSSZ)
        let mut out = vec![0x17];
        out.extend(Self::encode_length(time_str.len()));
        out.extend_from_slice(time_str.as_bytes());
        out
    }

    pub fn oid(components: &[u32]) -> Vec<u8> {
        let mut encoded = Vec::new();
        if components.len() >= 2 {
            encoded.push((components[0] * 40 + components[1]) as u8);
            for &c in &components[2..] {
                let mut val = c;
                let mut buf = Vec::new();
                buf.push((val & 0x7f) as u8);
                val >>= 7;
                while val > 0 {
                    buf.push(((val & 0x7f) | 0x80) as u8);
                    val >>= 7;
                }
                buf.reverse();
                encoded.extend_from_slice(&buf);
            }
        }
        let mut out = vec![0x06];
        out.extend(Self::encode_length(encoded.len()));
        out.extend(encoded);
        out
    }

    /// Build an X.500 Name RelativeDistinguishedName (e.g. CN=name)
    pub fn rdn_attribute(oid_components: &[u32], value: &str, is_utf8: bool) -> Vec<u8> {
        let oid_bytes = Self::oid(oid_components);
        let val_bytes = if is_utf8 {
            Self::utf8_string(value)
        } else {
            Self::printable_string(value)
        };
        let mut seq_content = Vec::new();
        seq_content.extend(oid_bytes);
        seq_content.extend(val_bytes);
        let atv = Self::sequence(&seq_content);
        Self::set(&atv)
    }

    /// Build X.500 Name SEQUENCE
    pub fn x500_name(cn: &str, org: &str, country: &str) -> Vec<u8> {
        let mut content = Vec::new();
        if !country.is_empty() {
            // C = 2.5.4.6
            content.extend(Self::rdn_attribute(&[2, 5, 4, 6], country, false));
        }
        if !org.is_empty() {
            // O = 2.5.4.10
            content.extend(Self::rdn_attribute(&[2, 5, 4, 10], org, true));
        }
        if !cn.is_empty() {
            // CN = 2.5.4.3
            content.extend(Self::rdn_attribute(&[2, 5, 4, 3], cn, true));
        }
        Self::sequence(&content)
    }

    /// Generate a valid RFC 5280 X.509 DER Certificate
    pub fn create_test_x509_der(
        serial: u64,
        subject_cn: &str,
        subject_org: &str,
        issuer_cn: &str,
        issuer_org: &str,
        not_before_utc: &str,
        not_after_utc: &str,
        key_usage_byte: Option<u8>,
    ) -> Vec<u8> {
        // AlgorithmIdentifier: sha256WithRSAEncryption (1.2.840.113549.1.1.11)
        let sig_algo = {
            let oid_bytes = Self::oid(&[1, 2, 840, 113549, 1, 1, 11]);
            let null_param = vec![0x05, 0x00];
            let mut c = Vec::new();
            c.extend(oid_bytes);
            c.extend(null_param);
            Self::sequence(&c)
        };

        // TBSCertificate
        let mut tbs_content = Vec::new();

        // Version: v3 [0] EXPLICIT INTEGER 2
        let version_elem = {
            let v_int = Self::integer(&[0x02]);
            let mut v_exp = vec![0xA0];
            v_exp.extend(Self::encode_length(v_int.len()));
            v_exp.extend(v_int);
            v_exp
        };
        tbs_content.extend(version_elem);

        // SerialNumber
        let serial_bytes = serial.to_be_bytes();
        let trim_idx = serial_bytes.iter().position(|&b| b != 0).unwrap_or(7);
        tbs_content.extend(Self::integer(&serial_bytes[trim_idx..]));

        // Signature algo in TBS
        tbs_content.extend_from_slice(&sig_algo);

        // Issuer
        let issuer = Self::x500_name(issuer_cn, issuer_org, "US");
        tbs_content.extend(issuer);

        // Validity
        let validity = {
            let mut v_c = Vec::new();
            v_c.extend(Self::utc_time(not_before_utc));
            v_c.extend(Self::utc_time(not_after_utc));
            Self::sequence(&v_c)
        };
        tbs_content.extend(validity);

        // Subject
        let subject = Self::x500_name(subject_cn, subject_org, "US");
        tbs_content.extend(subject);

        // SubjectPublicKeyInfo (dummy RSA 2048)
        let spki = {
            let rsa_algo = {
                let oid_bytes = Self::oid(&[1, 2, 840, 113549, 1, 1, 1]); // rsaEncryption
                let null_p = vec![0x05, 0x00];
                let mut c = Vec::new();
                c.extend(oid_bytes);
                c.extend(null_p);
                Self::sequence(&c)
            };
            let dummy_key = vec![0x42; 256];
            let key_bits = Self::bit_string(0, &dummy_key);
            let mut spki_c = Vec::new();
            spki_c.extend(rsa_algo);
            spki_c.extend(key_bits);
            Self::sequence(&spki_c)
        };
        tbs_content.extend(spki);

        // Extensions [3] EXPLICIT
        if let Some(ku) = key_usage_byte {
            // OID 2.5.29.15 (keyUsage)
            let ku_oid = Self::oid(&[2, 5, 29, 15]);
            let ku_bits = Self::bit_string(7, &[ku]); // 1 byte of flags
            let ku_ext_content = {
                let mut ext = Vec::new();
                ext.extend(ku_oid);
                ext.extend(Self::octet_string(&ku_bits));
                Self::sequence(&ext)
            };
            let exts_seq = Self::sequence(&ku_ext_content);
            let mut exts_exp = vec![0xA3];
            exts_exp.extend(Self::encode_length(exts_seq.len()));
            exts_exp.extend(exts_seq);
            tbs_content.extend(exts_exp);
        }

        let tbs_cert = Self::sequence(&tbs_content);

        // Dummy signature value BIT STRING (256 bytes)
        let dummy_sig = vec![0x33; 256];
        let sig_bits = Self::bit_string(0, &dummy_sig);

        // Outer Certificate SEQUENCE
        let mut cert_content = Vec::new();
        cert_content.extend(tbs_cert);
        cert_content.extend(sig_algo);
        cert_content.extend(sig_bits);

        Self::sequence(&cert_content)
    }

    /// Convert DER bytes to PEM string
    pub fn der_to_pem(der: &[u8], label: &str) -> String {
        use std::fmt::Write;
        let b64 = glib::base64_encode(der);
        let mut pem = format!("-----BEGIN {}-----\n", label);
        for chunk in b64.as_bytes().chunks(64) {
            let s = std::str::from_utf8(chunk).unwrap();
            let _ = writeln!(pem, "{}", s);
        }
        pem.push_str(&format!("-----END {}-----\n", label));
        pem
    }

    /// Convert PEM string back to DER bytes
    pub fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
        let mut in_cert = false;
        let mut b64_clean = String::new();

        for line in pem.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("-----BEGIN ") {
                in_cert = true;
                continue;
            }
            if trimmed.starts_with("-----END ") {
                break;
            }
            if in_cert {
                b64_clean.push_str(trimmed);
            }
        }

        if b64_clean.is_empty() {
            return None;
        }

        Some(glib::base64_decode(&b64_clean))
    }

    /// Calculate SHA-256 fingerprint in colon-separated hex format
    pub fn sha256_fingerprint(bytes: &[u8]) -> String {
        use glib::Checksum;
        use glib::ChecksumType;
        let mut cs = Checksum::new(ChecksumType::Sha256).expect("SHA256 checksum supported");
        cs.update(bytes);
        let hex = cs.string().unwrap_or_default().to_uppercase();
        let mut out = String::new();
        for (i, ch) in hex.chars().enumerate() {
            if i > 0 && i % 2 == 0 {
                out.push(':');
            }
            out.push(ch);
        }
        out
    }
}

// ============================================================================
// Sample Mozilla ISPDB XML Generator
// ============================================================================

pub fn build_ispdb_xml(
    domain: &str,
    incoming_host: &str,
    incoming_port: u16,
    incoming_socket: &str,
    outgoing_host: &str,
    outgoing_port: u16,
    outgoing_socket: &str,
) -> String {
    format!(
        r#"<?xml version="1.1"?>
<clientConfig version="1.1">
  <emailProvider id="{domain}">
    <domain>{domain}</domain>
    <displayName>{domain} Mail</displayName>
    <incomingServer type="imap">
      <hostname>{incoming_host}</hostname>
      <port>{incoming_port}</port>
      <socketType>{incoming_socket}</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>{outgoing_host}</hostname>
      <port>{outgoing_port}</port>
      <socketType>{outgoing_socket}</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#
    )
}
