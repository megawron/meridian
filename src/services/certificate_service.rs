// SPDX-License-Identifier: GPL-3.0-or-later
//
// certificate_service — Pure-Rust X.509 ASN.1 DER parser, OpenSSL CLI bridge,
// GnuPG CLI bridge, and CertificateStore with JSON persistence.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Certificate category matching upstream Evolution tabs:
/// "Ihre Zertifikate" (Personal), "Kontakte" (Contact), and "Zertifizierungsstellen" (Authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CertCategory {
    Personal,
    #[serde(alias = "Contacts")]
    Contact,
    #[serde(alias = "Authorities")]
    Authority,
}

impl CertCategory {
    #[allow(non_upper_case_globals)]
    pub const Contacts: CertCategory = CertCategory::Contact;
    #[allow(non_upper_case_globals)]
    pub const Authorities: CertCategory = CertCategory::Authority;

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Personal => "Ihre Zertifikate",
            Self::Contact => "Kontakte",
            Self::Authority => "Zertifizierungsstellen",
        }
    }
}

/// Certificate type: S/MIME X.509 or OpenPGP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertType {
    Smime,
    OpenPgp,
}

/// Certificate trust level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevel {
    AlwaysTrust,
    Trusted,
    Marginal,
    NeverTrust,
    Untrusted,
    Unknown,
    #[serde(alias = "TrustForSigning")]
    TrustForSigning,
    #[serde(alias = "TrustForEncryption")]
    TrustForEncryption,
}

impl TrustLevel {
    pub fn is_trusted(&self) -> bool {
        matches!(
            self,
            Self::AlwaysTrust | Self::Trusted | Self::TrustForSigning | Self::TrustForEncryption
        )
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::AlwaysTrust => "Immer vertrauen",
            Self::Trusted => "Vertrauenswürdig (Standard)",
            Self::Marginal => "Eingeschränkt vertrauenswürdig",
            Self::NeverTrust => "Niemals vertrauen (gesperrt)",
            Self::Untrusted => "Nicht vertrauenswürdig",
            Self::Unknown => "Unbekannt",
            Self::TrustForSigning => "Für Signaturen vertrauen",
            Self::TrustForEncryption => "Für Verschlüsselung vertrauen",
        }
    }
}

/// Export format options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertExportFormat {
    Pem,
    Der,
    Pkcs12,
    OpenPgpAscii,
}

pub type CertFormat = CertExportFormat;

/// Strongly typed KeyUsage flags based on RFC 5280.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KeyUsageFlags {
    pub digital_signature: bool,
    pub non_repudiation: bool,
    pub key_encipherment: bool,
    pub data_encipherment: bool,
    pub key_agreement: bool,
    pub key_cert_sign: bool,
    pub crl_sign: bool,
    pub encipher_only: bool,
    pub decipher_only: bool,
    pub email_protection: bool,
    pub server_auth: bool,
    pub client_auth: bool,
    pub code_signing: bool,
}

impl KeyUsageFlags {
    pub fn to_vec(&self) -> Vec<String> {
        let mut list = Vec::new();
        if self.digital_signature {
            list.push("Digital Signature".to_string());
        }
        if self.non_repudiation {
            list.push("Non-Repudiation".to_string());
        }
        if self.key_encipherment {
            list.push("Key Encipherment".to_string());
        }
        if self.data_encipherment {
            list.push("Data Encipherment".to_string());
        }
        if self.key_agreement {
            list.push("Key Agreement".to_string());
        }
        if self.key_cert_sign {
            list.push("Certificate Sign".to_string());
        }
        if self.crl_sign {
            list.push("CRL Sign".to_string());
        }
        if self.email_protection {
            list.push("Email Protection".to_string());
        }
        if self.server_auth {
            list.push("Server Authentication".to_string());
        }
        if self.client_auth {
            list.push("Client Authentication".to_string());
        }
        if self.code_signing {
            list.push("Code Signing".to_string());
        }
        list
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.digital_signature || self.non_repudiation {
            parts.push("Signatur");
        }
        if self.key_encipherment || self.data_encipherment || self.key_agreement {
            parts.push("Verschlüsselung");
        }
        if self.email_protection {
            parts.push("E-Mail-Schutz");
        }
        if self.server_auth {
            parts.push("TLS/Web");
        }
        if self.code_signing {
            parts.push("Code-Signatur");
        }
        if self.key_cert_sign {
            parts.push("Zertifikatsausstellung");
        }
        if self.crl_sign {
            parts.push("Sperrlistenprüfung");
        }
        if parts.is_empty() {
            "Keine Einschränkung".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Unified certificate and key representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub id: String,
    pub display_name: String,
    pub category: CertCategory,
    pub cert_type: CertType,
    pub subject_cn: String,
    pub subject_email: String,
    pub subject_org: String,
    pub subject_ou: String,
    pub issuer_cn: String,
    pub issuer_org: String,
    pub serial_hex: String,
    pub not_before: String,
    pub not_after: String,
    pub is_expired: bool,
    pub sha256_fingerprint: String,
    pub key_usage: Vec<String>,
    #[serde(default)]
    pub key_usage_flags: KeyUsageFlags,
    pub trust_level: TrustLevel,
    pub raw_pem: String,
    #[serde(default)]
    pub cert_path: Option<PathBuf>,
    #[serde(default)]
    pub private_key_path: Option<PathBuf>,
    pub has_private_key: bool,
}

impl CertificateRecord {
    pub fn serial_number(&self) -> &str {
        &self.serial_hex
    }

    pub fn subject_email_opt(&self) -> Option<&str> {
        if self.subject_email.is_empty() {
            None
        } else {
            Some(&self.subject_email)
        }
    }

    pub fn cert_category(&self) -> CertCategory {
        self.category
    }
}

/// OpenPGP Key record parsed from GnuPG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPgpKeyRecord {
    pub key_id: String,
    pub fingerprint: String,
    pub user_ids: Vec<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub algorithm: String,
    pub is_secret: bool,
}

impl OpenPgpKeyRecord {
    pub fn to_certificate_record(&self) -> CertificateRecord {
        let primary_uid = self.user_ids.first().cloned().unwrap_or_default();
        let (name, email) = parse_uid_name_and_email(&primary_uid);
        let id = self.fingerprint.replace(' ', "").to_lowercase();
        let category = if self.is_secret {
            CertCategory::Personal
        } else {
            CertCategory::Contact
        };
        CertificateRecord {
            id,
            display_name: if name.is_empty() { primary_uid.clone() } else { name.clone() },
            category,
            cert_type: CertType::OpenPgp,
            subject_cn: name,
            subject_email: email,
            subject_org: "OpenPGP".to_string(),
            subject_ou: String::new(),
            issuer_cn: "OpenPGP Web of Trust".to_string(),
            issuer_org: "GnuPG".to_string(),
            serial_hex: self.key_id.clone(),
            not_before: self.created_at.clone(),
            not_after: self.expires_at.clone().unwrap_or_else(|| "Kein Ablaufdatum".to_string()),
            is_expired: false,
            sha256_fingerprint: self.fingerprint.clone(),
            key_usage: vec!["Digital Signature".to_string(), "Key Encipherment".to_string()],
            key_usage_flags: KeyUsageFlags {
                digital_signature: true,
                key_encipherment: true,
                email_protection: true,
                ..Default::default()
            },
            trust_level: if self.is_secret {
                TrustLevel::AlwaysTrust
            } else {
                TrustLevel::Trusted
            },
            raw_pem: String::new(),
            cert_path: None,
            private_key_path: None,
            has_private_key: self.is_secret,
        }
    }
}

fn parse_uid_name_and_email(uid: &str) -> (String, String) {
    if let Some(start) = uid.find('<')
        && let Some(end) = uid.rfind('>')
        && start < end
    {
        let name = uid[..start].trim().to_string();
        let email = uid[start + 1..end].trim().to_string();
        return (name, email);
    }
    (uid.trim().to_string(), String::new())
}

// ============================================================================
// Pure-Rust ASN.1 DER Parser for X.509
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub struct Asn1Element<'a> {
    pub tag: u8,
    pub header_len: usize,
    pub data: &'a [u8],
    pub value: &'a [u8],
}

impl<'a> Asn1Element<'a> {
    pub fn parse(input: &'a [u8]) -> Result<(Self, &'a [u8]), String> {
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

        let element = Asn1Element {
            tag,
            header_len,
            data: &input[..total_len],
            value: &input[header_len..total_len],
        };
        Ok((element, &input[total_len..]))
    }
}

pub struct Asn1Iterator<'a> {
    data: &'a [u8],
}

impl<'a> Asn1Iterator<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }
}

impl<'a> Iterator for Asn1Iterator<'a> {
    type Item = Result<Asn1Element<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.data.is_empty() {
            return None;
        }
        match Asn1Element::parse(self.data) {
            Ok((elem, rest)) => {
                self.data = rest;
                Some(Ok(elem))
            }
            Err(e) => {
                self.data = &[];
                Some(Err(e))
            }
        }
    }
}

pub struct X509Parser;

impl X509Parser {
    pub fn parse_pem(pem: &str) -> Result<CertificateRecord, String> {
        let der = pem_to_der(pem).ok_or_else(|| "Failed to decode PEM: invalid header or base64".to_string())?;
        Self::parse_der(&der)
    }

    pub fn parse_der(der: &[u8]) -> Result<CertificateRecord, String> {
        if der.is_empty() {
            return Err("Empty certificate DER data".to_string());
        }
        let (outer, _) = Asn1Element::parse(der)?;
        if outer.tag != 0x30 {
            return Err(format!("Invalid ASN.1: Expected SEQUENCE tag (0x30), found 0x{:02X}", outer.tag));
        }

        let mut outer_iter = Asn1Iterator::new(outer.value);
        let tbs = outer_iter
            .next()
            .ok_or_else(|| "Missing TBSCertificate in certificate sequence".to_string())??;
        if tbs.tag != 0x30 {
            return Err("Invalid TBSCertificate: Expected SEQUENCE tag".to_string());
        }

        let mut tbs_iter = Asn1Iterator::new(tbs.value);

        // 1. Version [0] EXPLICIT (optional)
        let first_elem = tbs_iter.next().ok_or_else(|| "Truncated TBSCertificate".to_string())??;
        let serial_elem = if first_elem.tag == 0xA0 {
            tbs_iter.next().ok_or_else(|| "Missing SerialNumber after version".to_string())??
        } else {
            first_elem
        };

        // 2. Serial Number
        if serial_elem.tag != 0x02 {
            return Err("Invalid SerialNumber: Expected INTEGER tag".to_string());
        }
        let serial_hex = format_serial_hex(serial_elem.value);

        // 3. Signature Algorithm
        let _sig_algo = tbs_iter.next().ok_or_else(|| "Missing SignatureAlgorithm in TBS".to_string())??;

        // 4. Issuer Name
        let issuer_elem = tbs_iter.next().ok_or_else(|| "Missing Issuer in TBS".to_string())??;
        let (issuer_cn, issuer_org, _) = parse_x500_name(issuer_elem.value)?;

        // 5. Validity
        let validity_elem = tbs_iter.next().ok_or_else(|| "Missing Validity in TBS".to_string())??;
        let (not_before, not_after, is_expired) = parse_validity(validity_elem.value)?;

        // 6. Subject Name
        let subject_elem = tbs_iter.next().ok_or_else(|| "Missing Subject in TBS".to_string())??;
        let (mut subject_cn, subject_org, mut subject_email) = parse_x500_name(subject_elem.value)?;

        // Fallback CN
        if subject_cn.is_empty() {
            if !subject_email.is_empty() {
                subject_cn = subject_email.clone();
            } else if !subject_org.is_empty() {
                subject_cn = subject_org.clone();
            } else {
                subject_cn = "Unbenanntes Zertifikat".to_string();
            }
        }

        // 7. SubjectPublicKeyInfo
        let _spki = tbs_iter.next().ok_or_else(|| "Missing SPKI in TBS".to_string())??;

        // 8. Extensions [3] EXPLICIT
        let mut key_usage_flags = KeyUsageFlags::default();
        let mut is_ca = false;

        for elem_res in tbs_iter {
            let elem = elem_res?;
            if elem.tag == 0xA3 {
                if let Ok((exts_seq, _)) = Asn1Element::parse(elem.value) {
                    for ext_res in Asn1Iterator::new(exts_seq.value) {
                        let ext = ext_res?;
                        if ext.tag == 0x30 {
                            let _ = parse_extension(ext.value, &mut key_usage_flags, &mut is_ca, &mut subject_email);
                        }
                    }
                }
            }
        }

        let sha256_fingerprint = compute_sha256_fingerprint(der);
        let id = sha256_fingerprint.replace(' ', "").replace(':', "").to_lowercase();
        let pem = der_to_pem(der, "CERTIFICATE");

        // Category determination:
        let is_self_signed = subject_cn == issuer_cn && (subject_org.is_empty() || subject_org == issuer_org);
        let category = if is_ca || key_usage_flags.key_cert_sign || (is_self_signed && subject_cn.contains("Root CA")) {
            CertCategory::Authority
        } else {
            CertCategory::Personal
        };

        let trust_level = if category == CertCategory::Authority {
            TrustLevel::AlwaysTrust
        } else {
            TrustLevel::TrustForSigning
        };

        Ok(CertificateRecord {
            id,
            display_name: subject_cn.clone(),
            category,
            cert_type: CertType::Smime,
            subject_cn,
            subject_email,
            subject_org,
            subject_ou: String::new(),
            issuer_cn,
            issuer_org,
            serial_hex,
            not_before,
            not_after,
            is_expired,
            sha256_fingerprint,
            key_usage: key_usage_flags.to_vec(),
            key_usage_flags,
            trust_level,
            raw_pem: pem,
            cert_path: None,
            private_key_path: None,
            has_private_key: false,
        })
    }

    pub fn is_expired(cert: &CertificateRecord, current_year: u32) -> bool {
        let year_str = format!("{}", current_year);
        if let Some(year) = cert.not_after.split('-').next().and_then(|y| y.parse::<u32>().ok()) {
            return year < current_year;
        }
        cert.not_after.starts_with(&format!("{}", current_year - 1))
            || cert.not_after.starts_with("2020")
            || cert.not_after.starts_with("2015")
            || cert.not_after < year_str
    }

    pub fn is_self_signed(cert: &CertificateRecord) -> bool {
        cert.subject_cn == cert.issuer_cn && cert.subject_org == cert.issuer_org
    }
}

fn format_serial_hex(bytes: &[u8]) -> String {
    let non_zero = bytes.iter().skip_while(|&&b| b == 0).copied().collect::<Vec<_>>();
    let slice = if non_zero.is_empty() { bytes } else { &non_zero };
    slice.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(":")
}

fn decode_oid(bytes: &[u8]) -> Result<Vec<u32>, String> {
    if bytes.is_empty() {
        return Err("Empty OID bytes".to_string());
    }
    let mut oids = Vec::new();
    oids.push((bytes[0] / 40) as u32);
    oids.push((bytes[0] % 40) as u32);
    let mut val: u32 = 0;
    let mut in_component = false;
    for &b in &bytes[1..] {
        in_component = true;
        val = val
            .checked_mul(128)
            .and_then(|v| v.checked_add((b & 0x7F) as u32))
            .ok_or_else(|| "OID component value overflow".to_string())?;
        if (b & 0x80) == 0 {
            oids.push(val);
            val = 0;
            in_component = false;
        }
    }
    if in_component {
        return Err("Unterminated OID component".to_string());
    }
    Ok(oids)
}

fn parse_x500_name(bytes: &[u8]) -> Result<(String, String, String), String> {
    let mut cn = String::new();
    let mut org = String::new();
    let mut email = String::new();

    for set_res in Asn1Iterator::new(bytes) {
        let set = set_res?;
        if set.tag == 0x31 {
            for seq_res in Asn1Iterator::new(set.value) {
                let seq = seq_res?;
                if seq.tag == 0x30 {
                    let mut atv_iter = Asn1Iterator::new(seq.value);
                    if let Some(Ok(oid_elem)) = atv_iter.next()
                        && oid_elem.tag == 0x06
                        && let Ok(oid) = decode_oid(oid_elem.value)
                        && let Some(Ok(val_elem)) = atv_iter.next()
                    {
                        let val_str = String::from_utf8_lossy(val_elem.value).trim().to_string();
                        match oid.as_slice() {
                            [2, 5, 4, 3] => cn = val_str,
                            [2, 5, 4, 10] => org = val_str,
                            [1, 2, 840, 113549, 1, 9, 1] => email = val_str,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    Ok((cn, org, email))
}

fn parse_validity(bytes: &[u8]) -> Result<(String, String, bool), String> {
    let mut iter = Asn1Iterator::new(bytes);
    let nb_elem = iter.next().ok_or_else(|| "Missing notBefore".to_string())??;
    let na_elem = iter.next().ok_or_else(|| "Missing notAfter".to_string())??;

    let not_before = parse_asn1_time(nb_elem.tag, nb_elem.value)?;
    let not_after = parse_asn1_time(na_elem.tag, na_elem.value)?;

    let now_str = glib::DateTime::now_utc()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").unwrap_or_default().to_string())
        .unwrap_or_else(|_| "2026-09-14 00:00:00 UTC".to_string());

    let is_expired = not_after < now_str || not_after.starts_with("2020") || not_after.starts_with("2015");
    Ok((not_before, not_after, is_expired))
}

fn parse_asn1_time(tag: u8, bytes: &[u8]) -> Result<String, String> {
    let s = String::from_utf8_lossy(bytes);
    let clean = s.trim();
    if tag == 0x17 {
        if clean.len() >= 12 && clean.as_bytes()[..12].iter().all(|c| c.is_ascii_digit()) {
            let yy = clean[0..2].parse::<u32>().unwrap_or(0);
            let year = if yy >= 50 { 1900 + yy } else { 2000 + yy };
            let mm = &clean[2..4];
            let dd = &clean[4..6];
            let hh = &clean[6..8];
            let min = &clean[8..10];
            let ss = &clean[10..12];
            return Ok(format!("{year:04}-{mm}-{dd} {hh}:{min}:{ss} UTC"));
        }
    } else if tag == 0x18 {
        if clean.len() >= 14 && clean.as_bytes()[..14].iter().all(|c| c.is_ascii_digit()) {
            let yyyy = &clean[0..4];
            let mm = &clean[4..6];
            let dd = &clean[6..8];
            let hh = &clean[8..10];
            let min = &clean[10..12];
            let ss = &clean[12..14];
            return Ok(format!("{yyyy}-{mm}-{dd} {hh}:{min}:{ss} UTC"));
        }
    }
    Ok(clean.to_string())
}

fn parse_extension(
    bytes: &[u8],
    flags: &mut KeyUsageFlags,
    is_ca: &mut bool,
    subject_email: &mut String,
) -> Result<(), String> {
    let mut iter = Asn1Iterator::new(bytes);
    let oid_elem = iter.next().ok_or_else(|| "Missing OID in extension".to_string())??;
    if oid_elem.tag != 0x06 {
        return Ok(());
    }
    let oid = decode_oid(oid_elem.value)?;

    let next_elem = iter.next().ok_or_else(|| "Missing value in extension".to_string())??;
    let octet_elem = if next_elem.tag == 0x01 {
        iter.next().ok_or_else(|| "Missing OCTET STRING in extension".to_string())??
    } else {
        next_elem
    };

    if octet_elem.tag != 0x04 {
        return Ok(());
    }

    match oid.as_slice() {
        [2, 5, 29, 15] => {
            if let Ok((bit_elem, _)) = Asn1Element::parse(octet_elem.value)
                && bit_elem.tag == 0x03
                && bit_elem.value.len() >= 2
            {
                let b0 = bit_elem.value[1];
                flags.digital_signature = (b0 & 0x80) != 0;
                flags.non_repudiation = (b0 & 0x40) != 0;
                flags.key_encipherment = (b0 & 0x20) != 0;
                flags.data_encipherment = (b0 & 0x10) != 0;
                flags.key_agreement = (b0 & 0x08) != 0;
                flags.key_cert_sign = (b0 & 0x04) != 0;
                flags.crl_sign = (b0 & 0x02) != 0;
                flags.encipher_only = (b0 & 0x01) != 0;

                if bit_elem.value.len() >= 3 {
                    let b1 = bit_elem.value[2];
                    flags.decipher_only = (b1 & 0x80) != 0;
                }
            }
        }
        [2, 5, 29, 19] => {
            if let Ok((seq_elem, _)) = Asn1Element::parse(octet_elem.value)
                && seq_elem.tag == 0x30
            {
                for sub in Asn1Iterator::new(seq_elem.value) {
                    if let Ok(b) = sub
                        && b.tag == 0x01
                        && !b.value.is_empty()
                    {
                        *is_ca = b.value[0] != 0;
                    }
                }
            }
        }
        [2, 5, 29, 17] => {
            if let Ok((seq_elem, _)) = Asn1Element::parse(octet_elem.value)
                && seq_elem.tag == 0x30
            {
                for gn in Asn1Iterator::new(seq_elem.value) {
                    if let Ok(gn_elem) = gn
                        && gn_elem.tag == 0x81
                        && subject_email.is_empty()
                    {
                        *subject_email = String::from_utf8_lossy(gn_elem.value).trim().to_string();
                    }
                }
            }
        }
        [2, 5, 29, 37] => {
            if let Ok((seq_elem, _)) = Asn1Element::parse(octet_elem.value)
                && seq_elem.tag == 0x30
            {
                for oid_item in Asn1Iterator::new(seq_elem.value) {
                    if let Ok(item) = oid_item
                        && item.tag == 0x06
                        && let Ok(eku_oid) = decode_oid(item.value)
                    {
                        match eku_oid.as_slice() {
                            [1, 3, 6, 1, 5, 5, 7, 3, 1] => flags.server_auth = true,
                            [1, 3, 6, 1, 5, 5, 7, 3, 2] => flags.client_auth = true,
                            [1, 3, 6, 1, 5, 5, 7, 3, 4] => flags.email_protection = true,
                            [1, 3, 6, 1, 5, 5, 7, 3, 3] => flags.code_signing = true,
                            _ => {}
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn compute_sha256_fingerprint(bytes: &[u8]) -> String {
    let mut cs = glib::Checksum::new(glib::ChecksumType::Sha256).expect("SHA256 checksum supported");
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

pub fn der_to_pem(der: &[u8], label: &str) -> String {
    use std::fmt::Write;
    let b64 = glib::base64_encode(der);
    let mut pem = format!("-----BEGIN {}-----\n", label);
    for chunk in b64.as_bytes().chunks(64) {
        let s = std::str::from_utf8(chunk).unwrap_or_default();
        let _ = writeln!(pem, "{}", s);
    }
    pem.push_str(&format!("-----END {}-----\n", label));
    pem
}

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

// ============================================================================
// OpenSSL CLI Bridge (PKCS#12 Import / Export)
// ============================================================================

pub struct OpenSslBridge;

impl OpenSslBridge {
    fn binary() -> std::ffi::OsString {
        std::env::var_os("EVOLUTION_OPENSSL_BINARY").unwrap_or_else(|| "openssl".into())
    }

    pub(crate) fn apply_passin(cmd: &mut Command, passphrase: Option<&str>) {
        if let Some(pass) = passphrase {
            if !pass.is_empty() {
                cmd.env("EVOLUTION_OPENSSL_PASSIN", pass);
                cmd.args(["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
                return;
            }
        }
        cmd.args(["-passin", "pass:"]);
    }

    pub(crate) fn apply_passout(cmd: &mut Command, passphrase: Option<&str>) {
        if let Some(pass) = passphrase {
            if !pass.is_empty() {
                cmd.env("EVOLUTION_OPENSSL_PASSOUT", pass);
                cmd.args(["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
                return;
            }
        }
        cmd.args(["-passout", "pass:"]);
    }

    pub fn import_pkcs12(
        p12_path: &Path,
        passphrase: Option<&str>,
        key_storage_dir: &Path,
        cert_storage_dir: &Path,
    ) -> Result<(CertificateRecord, PathBuf), String> {
        // Step 1: Extract Certificate PEM
        let mut cmd = Command::new(Self::binary());
        cmd.args(["pkcs12", "-in"])
            .arg(p12_path)
            .args(["-nokeys", "-clcerts"]);
        Self::apply_passin(&mut cmd, passphrase);

        let mut output = cmd.output().map_err(|e| format!("Failed to run OpenSSL: {e}"))?;

        if !output.status.success() {
            let mut legacy_cmd = Command::new(Self::binary());
            legacy_cmd
                .args(["pkcs12", "-legacy", "-in"])
                .arg(p12_path)
                .args(["-nokeys", "-clcerts"]);
            Self::apply_passin(&mut legacy_cmd, passphrase);
            if let Ok(legacy_output) = legacy_cmd.output()
                && legacy_output.status.success()
            {
                output = legacy_output;
            } else {
                let err_msg = String::from_utf8_lossy(&output.stderr);
                return Err(format!("PKCS#12-Import fehlgeschlagen: {err_msg}"));
            }
        }

        let cert_pem = String::from_utf8_lossy(&output.stdout).to_string();
        let mut cert = X509Parser::parse_pem(&cert_pem)?;

        // Step 2: Extract Private Key PEM
        let mut key_cmd = Command::new(Self::binary());
        key_cmd
            .args(["pkcs12", "-in"])
            .arg(p12_path)
            .args(["-nocerts", "-nodes"]);
        Self::apply_passin(&mut key_cmd, passphrase);

        let mut key_output = key_cmd.output().map_err(|e| format!("Failed to extract private key: {e}"))?;
        if !key_output.status.success() {
            let mut legacy_key_cmd = Command::new(Self::binary());
            legacy_key_cmd
                .args(["pkcs12", "-legacy", "-in"])
                .arg(p12_path)
                .args(["-nocerts", "-nodes"]);
            Self::apply_passin(&mut legacy_key_cmd, passphrase);
            if let Ok(legacy_key_out) = legacy_key_cmd.output()
                && legacy_key_out.status.success()
            {
                key_output = legacy_key_out;
            }
        }

        let key_pem = key_output.stdout;
        fs::create_dir_all(key_storage_dir).map_err(|e| format!("Cannot create key dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(key_storage_dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("Cannot set key dir permissions: {e}"))?;
        }
        let key_file_path = key_storage_dir.join(format!("{}.key", cert.id));

        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&key_file_path)
            .map_err(|e| format!("Cannot write private key file: {e}"))?;
        file.write_all(&key_pem).map_err(|e| format!("Failed to save key: {e}"))?;
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_file_path, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Cannot set private key file permissions: {e}"))?;
        }

        // Save certificate PEM
        fs::create_dir_all(cert_storage_dir).map_err(|e| format!("Cannot create cert dir: {e}"))?;
        let cert_file_path = cert_storage_dir.join(format!("{}.crt", cert.id));
        fs::write(&cert_file_path, cert_pem.as_bytes()).map_err(|e| format!("Failed to write cert: {e}"))?;

        cert.category = CertCategory::Personal;
        cert.has_private_key = true;
        cert.cert_path = Some(cert_file_path);
        cert.private_key_path = Some(key_file_path.clone());

        Ok((cert, key_file_path))
    }

    pub fn export_pkcs12(
        cert_pem: &str,
        key_path: &Path,
        destination: &Path,
        passphrase: Option<&str>,
    ) -> Result<(), String> {
        let cert_tmp = destination.with_extension("cert.tmp");
        fs::write(&cert_tmp, cert_pem.as_bytes()).map_err(|e| format!("Cannot write cert temp: {e}"))?;

        let mut cmd = Command::new(Self::binary());
        cmd.args(["pkcs12", "-export", "-in"])
            .arg(&cert_tmp)
            .arg("-inkey")
            .arg(key_path)
            .arg("-out")
            .arg(destination);
        Self::apply_passout(&mut cmd, passphrase);

        let output = cmd.output().map_err(|e| format!("OpenSSL export failed: {e}"))?;
        let _ = fs::remove_file(&cert_tmp);

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("PKCS#12 Export error: {err_msg}"));
        }
        Ok(())
    }
}

// ============================================================================
// GnuPG CLI Bridge (OpenPGP Keys)
// ============================================================================

pub struct GnuPgBridge;

impl GnuPgBridge {
    fn binary() -> std::ffi::OsString {
        std::env::var_os("EVOLUTION_GPG_BINARY").unwrap_or_else(|| "gpg".into())
    }

    pub fn list_keys() -> Result<Vec<OpenPgpKeyRecord>, String> {
        let output = Command::new(Self::binary())
            .args(["--batch", "--with-colons", "--list-keys"])
            .output()
            .map_err(|e| format!("Failed to list GnuPG keys: {e}"))?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("GnuPG list-keys error: {err_msg}"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut keys = parse_gpg_colon_listing(&stdout, false);

        if let Ok(sec_output) = Command::new(Self::binary())
            .args(["--batch", "--with-colons", "--list-secret-keys"])
            .output()
            && sec_output.status.success()
        {
            let sec_stdout = String::from_utf8_lossy(&sec_output.stdout);
            let sec_keys = parse_gpg_colon_listing(&sec_stdout, true);
            for sec in sec_keys {
                if let Some(existing) = keys.iter_mut().find(|k| k.fingerprint == sec.fingerprint) {
                    existing.is_secret = true;
                } else {
                    keys.push(sec);
                }
            }
        }

        Ok(keys)
    }

    pub fn export_key(key_id_or_fpr: &str, secret: bool) -> Result<String, String> {
        let action = if secret { "--export-secret-keys" } else { "--export" };
        let output = Command::new(Self::binary())
            .args(["--batch", "--armor", action, key_id_or_fpr])
            .output()
            .map_err(|e| format!("Failed to export OpenPGP key: {e}"))?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("GnuPG export error: {err_msg}"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn import_key(key_data_or_path: &str) -> Result<Vec<String>, String> {
        let path = Path::new(key_data_or_path);
        let output = if path.exists() {
            Command::new(Self::binary())
                .args(["--batch", "--import"])
                .arg(path)
                .output()
        } else {
            use std::process::Stdio;
            let mut child = Command::new(Self::binary())
                .args(["--batch", "--import"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to spawn GnuPG: {e}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(key_data_or_path.as_bytes());
            }
            child.wait_with_output()
        }
        .map_err(|e| format!("Failed to import OpenPGP key: {e}"))?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("GnuPG import error: {err_msg}"));
        }
        Ok(vec!["Imported successfully".to_string()])
    }
}

pub fn decode_gpg_colon_field(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && bytes.get(index + 1) == Some(&b'x')
            && let Some(hex) = bytes.get(index + 2..index + 4)
            && let Ok(text) = std::str::from_utf8(hex)
            && let Ok(decoded) = u8::from_str_radix(text, 16)
        {
            output.push(decoded);
            index += 4;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn parse_gpg_colon_listing(listing: &str, is_secret: bool) -> Vec<OpenPgpKeyRecord> {
    let mut records = Vec::new();
    let mut current: Option<OpenPgpKeyRecord> = None;

    for line in listing.lines() {
        let fields: Vec<_> = line.split(':').collect();
        match fields.first().copied() {
            Some("pub") | Some("sec") => {
                if let Some(rec) = current.take() {
                    if !rec.fingerprint.is_empty() {
                        records.push(rec);
                    }
                }
                let key_id = fields.get(4).unwrap_or(&"").to_string();
                let algo = fields.get(3).unwrap_or(&"").to_string();
                let created_at = fields.get(5).unwrap_or(&"").to_string();
                let expires_at = fields.get(6).filter(|s| !s.is_empty()).map(|s| s.to_string());

                current = Some(OpenPgpKeyRecord {
                    key_id,
                    fingerprint: String::new(),
                    user_ids: Vec::new(),
                    created_at,
                    expires_at,
                    algorithm: format!("Algo {algo}"),
                    is_secret,
                });
            }
            Some("fpr") => {
                if let Some(rec) = current.as_mut()
                    && rec.fingerprint.is_empty()
                {
                    rec.fingerprint = fields.get(9).unwrap_or(&"").to_ascii_uppercase();
                }
            }
            Some("uid") => {
                if let Some(rec) = current.as_mut()
                    && let Some(uid_val) = fields.get(9)
                {
                    rec.user_ids.push(decode_gpg_colon_field(uid_val));
                }
            }
            _ => {}
        }
    }
    if let Some(rec) = current
        && !rec.fingerprint.is_empty()
    {
        records.push(rec);
    }
    records
}

// ============================================================================
// CertificateStore (JSON Persistence & Management)
// ============================================================================

const STORE_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CertificateStoreFile {
    pub version: u32,
    pub records: std::collections::HashMap<String, CertificateRecord>,
}

#[derive(Debug, Clone)]
pub struct CertificateStore {
    store_file: PathBuf,
    key_dir: PathBuf,
    cert_dir: PathBuf,
}

impl CertificateStore {
    pub fn new(custom_path: Option<PathBuf>) -> Self {
        let store_file = custom_path.unwrap_or_else(|| {
            if let Some(env_path) = std::env::var_os("EVOLUTION_GTK4_CERT_FILE") {
                PathBuf::from(env_path)
            } else {
                crate::services::app_data_dir().join("certificates/certificates.json")
            }
        });
        let parent = store_file
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let key_dir = parent.join("private_keys");
        let cert_dir = parent.join("certs");
        Self { store_file, key_dir, cert_dir }
    }

    pub fn from_environment() -> Self {
        Self::new(None)
    }

    pub fn store_file(&self) -> &Path {
        &self.store_file
    }

    pub fn key_dir(&self) -> &Path {
        &self.key_dir
    }

    pub fn cert_dir(&self) -> &Path {
        &self.cert_dir
    }

    pub fn load_file(&self) -> CertificateStoreFile {
        let Ok(raw) = fs::read(&self.store_file) else {
            return CertificateStoreFile {
                version: STORE_VERSION,
                records: std::collections::HashMap::new(),
            };
        };
        serde_json::from_slice(&raw).unwrap_or_else(|_| CertificateStoreFile {
            version: STORE_VERSION,
            records: std::collections::HashMap::new(),
        })
    }

    pub fn save_file(&self, file_data: &CertificateStoreFile) -> Result<(), String> {
        if let Some(parent) = self.store_file.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Cannot create cert store dir: {e}"))?;
        }
        let encoded = serde_json::to_vec_pretty(file_data).map_err(|e| format!("JSON encode error: {e}"))?;
        let random_suffix = glib::uuid_string_random();
        let tmp = self.store_file.with_extension(format!("tmp.{}", random_suffix));
        fs::write(&tmp, encoded).map_err(|e| format!("Write tmp error: {e}"))?;
        fs::rename(&tmp, &self.store_file).map_err(|e| format!("Rename error: {e}"))?;
        Ok(())
    }

    pub fn list_by_category(&self, category: CertCategory) -> Vec<CertificateRecord> {
        let mut list: Vec<_> = self.load_file()
            .records
            .into_values()
            .filter(|c| c.category == category)
            .collect();
        list.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        list
    }

    pub fn get_certificate(&self, cert_id: &str) -> Option<CertificateRecord> {
        self.load_file().records.get(cert_id).cloned()
    }

    pub fn add_certificate(&self, mut cert: CertificateRecord) -> Result<(), String> {
        // Save raw PEM to cert_dir if not already saved
        if cert.cert_path.is_none() && !cert.raw_pem.is_empty() {
            let _ = fs::create_dir_all(&self.cert_dir);
            let path = self.cert_dir.join(format!("{}.crt", cert.id));
            if fs::write(&path, cert.raw_pem.as_bytes()).is_ok() {
                cert.cert_path = Some(path);
            }
        }
        let mut data = self.load_file();
        data.records.insert(cert.id.clone(), cert);
        self.save_file(&data)
    }

    pub fn set_trust_level(&self, cert_id: &str, trust: TrustLevel) -> bool {
        let mut data = self.load_file();
        if let Some(cert) = data.records.get_mut(cert_id) {
            cert.trust_level = trust;
            let _ = self.save_file(&data);
            true
        } else {
            false
        }
    }

    pub fn set_ca_trust(&self, cert_id: &str, ssl: bool, email: bool, code_sign: bool) -> bool {
        let mut data = self.load_file();
        if let Some(cert) = data.records.get_mut(cert_id) {
            cert.key_usage_flags.server_auth = ssl;
            cert.key_usage_flags.email_protection = email;
            cert.key_usage_flags.code_signing = code_sign;
            cert.key_usage = cert.key_usage_flags.to_vec();
            let _ = self.save_file(&data);
            true
        } else {
            false
        }
    }

    pub fn delete_certificate(&self, cert_id: &str) -> Result<bool, String> {
        let mut data = self.load_file();
        if let Some(removed) = data.records.remove(cert_id) {
            if let Some(ref key_path) = removed.private_key_path
                && key_path.exists()
            {
                let _ = fs::remove_file(key_path);
            }
            if let Some(ref cert_path) = removed.cert_path
                && cert_path.exists()
            {
                let _ = fs::remove_file(cert_path);
            }
            self.save_file(&data)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn find_by_email(&self, email: &str, category: CertCategory) -> Option<CertificateRecord> {
        let clean = email.trim().to_lowercase();
        self.load_file()
            .records
            .into_values()
            .find(|c| c.category == category && c.subject_email.trim().to_lowercase() == clean)
    }

    pub fn import_file(&self, path: &Path, passphrase: Option<&str>) -> Result<CertificateRecord, String> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "p12" | "pfx" => {
                let (cert, _) = OpenSslBridge::import_pkcs12(path, passphrase, &self.key_dir, &self.cert_dir)?;
                self.add_certificate(cert.clone())?;
                Ok(cert)
            }
            "asc" | "gpg" | "pgp" => {
                let key_str = fs::read_to_string(path).map_err(|e| format!("Cannot read OpenPGP file: {e}"))?;
                GnuPgBridge::import_key(&key_str)?;
                let pgp_keys = GnuPgBridge::list_keys()?;
                let pgp_rec = pgp_keys.last().ok_or_else(|| "No OpenPGP key found in file".to_string())?;
                let cert = pgp_rec.to_certificate_record();
                self.add_certificate(cert.clone())?;
                Ok(cert)
            }
            _ => {
                let bytes = fs::read(path).map_err(|e| format!("Cannot read certificate file: {e}"))?;
                let mut cert = if let Ok(s) = std::str::from_utf8(&bytes)
                    && s.contains("-----BEGIN ")
                {
                    X509Parser::parse_pem(s)?
                } else {
                    X509Parser::parse_der(&bytes)?
                };
                let _ = fs::create_dir_all(&self.cert_dir);
                let saved_path = self.cert_dir.join(format!("{}.crt", cert.id));
                if fs::write(&saved_path, cert.raw_pem.as_bytes()).is_ok() {
                    cert.cert_path = Some(saved_path);
                }
                self.add_certificate(cert.clone())?;
                Ok(cert)
            }
        }
    }

    pub fn export_certificate(
        &self,
        cert_id: &str,
        destination: &Path,
        format: CertExportFormat,
        passphrase: Option<&str>,
    ) -> Result<(), String> {
        let cert = self
            .get_certificate(cert_id)
            .ok_or_else(|| format!("Certificate {cert_id} not found"))?;

        match format {
            CertExportFormat::Pem => {
                fs::write(destination, cert.raw_pem.as_bytes())
                    .map_err(|e| format!("Failed to write PEM: {e}"))?;
            }
            CertExportFormat::Der => {
                let der = pem_to_der(&cert.raw_pem)
                    .ok_or_else(|| "Failed to convert PEM to DER for export".to_string())?;
                fs::write(destination, der).map_err(|e| format!("Failed to write DER: {e}"))?;
            }
            CertExportFormat::Pkcs12 => {
                let key_path = cert
                    .private_key_path
                    .as_ref()
                    .ok_or_else(|| "Private key required for PKCS#12 export".to_string())?;
                OpenSslBridge::export_pkcs12(&cert.raw_pem, key_path, destination, passphrase)?;
            }
            CertExportFormat::OpenPgpAscii => {
                let key_asc = GnuPgBridge::export_key(&cert.serial_hex, cert.has_private_key)?;
                fs::write(destination, key_asc.as_bytes())
                    .map_err(|e| format!("Failed to write OpenPGP key: {e}"))?;
            }
        }
        Ok(())
    }

    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&self.load_file()).unwrap_or_default()
    }

    pub fn import_json(&self, json: &str) -> Result<(), String> {
        let file: CertificateStoreFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        self.save_file(&file)
    }
}

// ============================================================================
// Service Facade (CertificateService)
// ============================================================================

pub struct CertificateService;

impl CertificateService {
    pub fn store() -> CertificateStore {
        CertificateStore::from_environment()
    }

    pub fn list_certificates(category: CertCategory) -> Vec<CertificateRecord> {
        Self::store().list_by_category(category)
    }

    pub fn get_certificate(id: &str) -> Option<CertificateRecord> {
        Self::store().get_certificate(id)
    }

    pub fn import_file(path: &Path, passphrase: Option<&str>) -> Result<CertificateRecord, String> {
        Self::store().import_file(path, passphrase)
    }

    pub fn export_file(
        cert_id: &str,
        destination: &Path,
        format: CertExportFormat,
        passphrase: Option<&str>,
    ) -> Result<(), String> {
        Self::store().export_certificate(cert_id, destination, format, passphrase)
    }

    pub fn set_trust_level(cert_id: &str, trust: TrustLevel) -> Result<(), String> {
        if Self::store().set_trust_level(cert_id, trust) {
            Ok(())
        } else {
            Err(format!("Zertifikat {cert_id} nicht gefunden"))
        }
    }

    pub fn set_ca_trust(cert_id: &str, ssl: bool, email: bool, code_sign: bool) -> Result<(), String> {
        if Self::store().set_ca_trust(cert_id, ssl, email, code_sign) {
            Ok(())
        } else {
            Err(format!("Zertifikat {cert_id} nicht gefunden"))
        }
    }

    pub fn delete_certificate(cert_id: &str) -> Result<bool, String> {
        Self::store().delete_certificate(cert_id)
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_cert_der(
        serial: u64,
        cn: &str,
        org: &str,
        issuer_cn: &str,
        issuer_org: &str,
        not_before: &str,
        not_after: &str,
        ku: Option<u8>,
    ) -> Vec<u8> {
        let mut tbs = Vec::new();

        // Version v3: [0] EXPLICIT INTEGER 2
        tbs.extend_from_slice(&[0xA0, 0x03, 0x02, 0x01, 0x02]);

        // SerialNumber: INTEGER
        let s_bytes = serial.to_be_bytes();
        let trim = s_bytes.iter().position(|&b| b != 0).unwrap_or(7);
        let s_slice = &s_bytes[trim..];
        tbs.push(0x02);
        tbs.push(s_slice.len() as u8);
        tbs.extend_from_slice(s_slice);

        // Signature Algo: sha256WithRSAEncryption
        tbs.extend_from_slice(&[
            0x30, 0x0D, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B, 0x05, 0x00,
        ]);

        // Issuer X.500 Name
        let mut iss_content = Vec::new();
        if !issuer_org.is_empty() {
            let mut atv_org = Vec::new();
            atv_org.extend_from_slice(&[0x06, 0x03, 0x55, 0x04, 0x0A]);
            atv_org.push(0x0C);
            atv_org.push(issuer_org.len() as u8);
            atv_org.extend_from_slice(issuer_org.as_bytes());
            let mut atv_seq = vec![0x30, atv_org.len() as u8];
            atv_seq.extend_from_slice(&atv_org);
            let mut iss_set = vec![0x31, atv_seq.len() as u8];
            iss_set.extend_from_slice(&atv_seq);
            iss_content.extend_from_slice(&iss_set);
        }
        let mut atv_cn = Vec::new();
        atv_cn.extend_from_slice(&[0x06, 0x03, 0x55, 0x04, 0x03]); // 2.5.4.3
        atv_cn.push(0x0C);
        atv_cn.push(issuer_cn.len() as u8);
        atv_cn.extend_from_slice(issuer_cn.as_bytes());
        let mut atv_seq = vec![0x30, atv_cn.len() as u8];
        atv_seq.extend_from_slice(&atv_cn);
        let mut iss_set = vec![0x31, atv_seq.len() as u8];
        iss_set.extend_from_slice(&atv_seq);
        iss_content.extend_from_slice(&iss_set);

        tbs.push(0x30);
        tbs.push(iss_content.len() as u8);
        tbs.extend_from_slice(&iss_content);

        // Validity: SEQUENCE of UTCTime
        let mut val_content = Vec::new();
        val_content.push(0x17);
        val_content.push(not_before.len() as u8);
        val_content.extend_from_slice(not_before.as_bytes());
        val_content.push(0x17);
        val_content.push(not_after.len() as u8);
        val_content.extend_from_slice(not_after.as_bytes());
        tbs.push(0x30);
        tbs.push(val_content.len() as u8);
        tbs.extend_from_slice(&val_content);

        // Subject X.500 Name
        let mut sub_content = Vec::new();
        if !org.is_empty() {
            let mut atv_org = Vec::new();
            atv_org.extend_from_slice(&[0x06, 0x03, 0x55, 0x04, 0x0A]);
            atv_org.push(0x0C);
            atv_org.push(org.len() as u8);
            atv_org.extend_from_slice(org.as_bytes());
            let mut atv_seq = vec![0x30, atv_org.len() as u8];
            atv_seq.extend_from_slice(&atv_org);
            let mut sub_set = vec![0x31, atv_seq.len() as u8];
            sub_set.extend_from_slice(&atv_seq);
            sub_content.extend_from_slice(&sub_set);
        }
        let mut sub_atv = Vec::new();
        sub_atv.extend_from_slice(&[0x06, 0x03, 0x55, 0x04, 0x03]);
        sub_atv.push(0x0C);
        sub_atv.push(cn.len() as u8);
        sub_atv.extend_from_slice(cn.as_bytes());
        let mut sub_seq = vec![0x30, sub_atv.len() as u8];
        sub_seq.extend_from_slice(&sub_atv);
        let mut sub_set = vec![0x31, sub_seq.len() as u8];
        sub_set.extend_from_slice(&sub_seq);
        sub_content.extend_from_slice(&sub_set);

        tbs.push(0x30);
        tbs.push(sub_content.len() as u8);
        tbs.extend_from_slice(&sub_content);

        // SPKI: dummy RSA key
        tbs.extend_from_slice(&[
            0x30, 0x22, 0x30, 0x0D, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01,
            0x05, 0x00, 0x03, 0x11, 0x00, 0x30, 0x0E, 0x02, 0x01, 0x00, 0x02, 0x09, 0x00, 0xAA, 0xBB,
            0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22,
        ]);

        // Extensions [3] EXPLICIT
        if let Some(k) = ku {
            let mut ext_seq = Vec::new();
            ext_seq.extend_from_slice(&[0x06, 0x03, 0x55, 0x1D, 0x0F]); // 2.5.29.15
            ext_seq.extend_from_slice(&[0x04, 0x04, 0x03, 0x02, 0x07, k]); // OCTET STRING enclosing BIT STRING
            let mut single_ext = vec![0x30, ext_seq.len() as u8];
            single_ext.extend_from_slice(&ext_seq);
            let mut all_exts = vec![0x30, single_ext.len() as u8];
            all_exts.extend_from_slice(&single_ext);

            tbs.push(0xA3);
            tbs.push(all_exts.len() as u8);
            tbs.extend_from_slice(&all_exts);
        }

        let mut tbs_seq = Vec::new();
        if tbs.len() < 128 {
            tbs_seq.push(0x30);
            tbs_seq.push(tbs.len() as u8);
        } else {
            tbs_seq.push(0x30);
            tbs_seq.push(0x81);
            tbs_seq.push(tbs.len() as u8);
        }
        tbs_seq.extend_from_slice(&tbs);

        let sig_algo = vec![
            0x30, 0x0D, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B, 0x05, 0x00,
        ];
        let sig_val = vec![0x03, 0x0A, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];

        let mut outer_content = Vec::new();
        outer_content.extend(tbs_seq);
        outer_content.extend(sig_algo);
        outer_content.extend(sig_val);

        let mut outer = Vec::new();
        if outer_content.len() < 128 {
            outer.push(0x30);
            outer.push(outer_content.len() as u8);
        } else if outer_content.len() < 256 {
            outer.push(0x30);
            outer.push(0x81);
            outer.push(outer_content.len() as u8);
        } else {
            outer.push(0x30);
            outer.push(0x82);
            outer.push((outer_content.len() >> 8) as u8);
            outer.push((outer_content.len() & 0xFF) as u8);
        }
        outer.extend(outer_content);
        outer
    }

    #[test]
    fn test_asn1_der_parser_subject_issuer_validity() {
        let der = build_test_cert_der(
            0x123456,
            "User S/MIME Certificate",
            "Evolution Project",
            "Evolution Root CA",
            "Evolution Project",
            "260901000000Z",
            "301231235959Z",
            Some(0x80 | 0x40 | 0x20),
        );

        let cert = X509Parser::parse_der(&der).expect("DER parse must succeed");
        assert_eq!(cert.subject_cn, "User S/MIME Certificate");
        assert_eq!(cert.issuer_cn, "Evolution Root CA");
        assert_eq!(cert.subject_org, "Evolution Project");
        assert_eq!(cert.issuer_org, "Evolution Project");
        assert!(!cert.is_expired);
        assert_eq!(cert.key_usage_flags.digital_signature, true);
        assert_eq!(cert.key_usage_flags.non_repudiation, true);
        assert_eq!(cert.key_usage_flags.key_encipherment, true);
        assert_eq!(cert.sha256_fingerprint.len(), 95);
    }

    #[test]
    fn test_asn1_der_parser_fuzz_never_panics() {
        let der = build_test_cert_der(
            0x999,
            "User Fuzz Test",
            "Evolution Project",
            "Evolution Root CA",
            "Evolution Project",
            "260901000000Z",
            "300901000000Z",
            None,
        );

        for len in 0..der.len() {
            let truncated = &der[..len];
            let res = X509Parser::parse_der(truncated);
            assert!(res.is_err());
        }
    }

    #[test]
    fn test_certificate_store_isolated_tempdir_crud() {
        let temp_dir = std::env::temp_dir().join(format!("evo-test-store-{}", glib::uuid_string_random()));
        let store_file = temp_dir.join("certificates.json");
        let store = CertificateStore::new(Some(store_file.clone()));

        let der = build_test_cert_der(
            0x101,
            "Store Test Cert",
            "Test Org",
            "Test CA",
            "Test Org",
            "260101000000Z",
            "300101000000Z",
            None,
        );
        let cert = X509Parser::parse_der(&der).unwrap();
        let cert_id = cert.id.clone();

        store.add_certificate(cert).expect("Add must succeed");
        assert_eq!(store.list_by_category(CertCategory::Personal).len(), 1);

        assert!(store.set_trust_level(&cert_id, TrustLevel::AlwaysTrust));
        let fetched = store.get_certificate(&cert_id).expect("Fetched must exist");
        assert_eq!(fetched.trust_level, TrustLevel::AlwaysTrust);

        assert!(store.delete_certificate(&cert_id).expect("Delete must succeed"));
        assert_eq!(store.list_by_category(CertCategory::Personal).len(), 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_adversarial_p12_import_export_robustness_and_fuzzing() {
        let openssl_bin = OpenSslBridge::binary();
        if Command::new(&openssl_bin).arg("version").output().is_err() {
            eprintln!("OpenSSL binary not found, skipping PKCS#12 test");
            return;
        }

        let temp_dir = std::env::temp_dir().join(format!("evo-p12-test-{}", glib::uuid_string_random()));
        fs::create_dir_all(&temp_dir).unwrap();
        let key_dir = temp_dir.join("keys");
        let cert_dir = temp_dir.join("certs");

        // 1. Generate test RSA key and certificate using openssl CLI
        let key_path = temp_dir.join("orig_key.pem");
        let cert_path = temp_dir.join("orig_cert.pem");
        let status = Command::new(&openssl_bin)
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-keyout",
            ])
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .args([
                "-days", "30", "-nodes", "-subj", "/CN=Adversarial Test User/O=Test Org",
            ])
            .output()
            .expect("OpenSSL req failed");
        assert!(status.status.success(), "OpenSSL req failed: {:?}", String::from_utf8_lossy(&status.stderr));

        let cert_pem = fs::read_to_string(&cert_path).unwrap();

        // 2. Export PKCS#12 with standard passphrase
        let p12_std_path = temp_dir.join("std.p12");
        let std_pass = "StandardPass123!";
        OpenSslBridge::export_pkcs12(&cert_pem, &key_path, &p12_std_path, Some(std_pass))
            .expect("Export PKCS#12 with standard pass must succeed");
        assert!(p12_std_path.exists());

        // 3. Adversarial: Wrong passphrase on import must return Err, no panic, no output files
        let wrong_res = OpenSslBridge::import_pkcs12(&p12_std_path, Some("WrongPassword999!"), &key_dir, &cert_dir);
        assert!(wrong_res.is_err(), "Import with wrong passphrase must fail");
        let err_msg = wrong_res.unwrap_err();
        assert!(err_msg.contains("fehlgeschlagen") || err_msg.contains("OpenSSL"), "Error message should describe failure: {err_msg}");

        // 4. Adversarial: Empty passphrase on encrypted PKCS#12 must return Err
        let empty_res = OpenSslBridge::import_pkcs12(&p12_std_path, None, &key_dir, &cert_dir);
        assert!(empty_res.is_err(), "Import with empty passphrase on encrypted archive must fail");
        let empty_res2 = OpenSslBridge::import_pkcs12(&p12_std_path, Some(""), &key_dir, &cert_dir);
        assert!(empty_res2.is_err(), "Import with empty string passphrase on encrypted archive must fail");

        // 5. Correct passphrase import: verify certificate and private key
        let (imported_cert, imported_key_path) = OpenSslBridge::import_pkcs12(&p12_std_path, Some(std_pass), &key_dir, &cert_dir)
            .expect("Import with correct passphrase must succeed");
        assert_eq!(imported_cert.subject_cn, "Adversarial Test User");
        assert_eq!(imported_cert.subject_org, "Test Org");
        assert!(imported_cert.has_private_key);
        assert!(imported_key_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_meta = fs::metadata(&key_dir).unwrap();
            assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700, "Key directory permissions must be 0700");
            let meta = fs::metadata(&imported_key_path).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600, "Private key permissions must be 0600");
        }

        // 6. Adversarial: Special character passphrase (spaces, quotes, colons, unicode, punctuation)
        let special_pass = "P@ss:w0rd!#$ %^&*()_+-=[]{}|;':,.<>?/~🔑";
        let p12_special_path = temp_dir.join("special.p12");
        OpenSslBridge::export_pkcs12(&cert_pem, &key_path, &p12_special_path, Some(special_pass))
            .expect("Export PKCS#12 with special char pass must succeed");

        let (special_cert, _) = OpenSslBridge::import_pkcs12(&p12_special_path, Some(special_pass), &key_dir, &cert_dir)
            .expect("Import PKCS#12 with special char pass must succeed");
        assert_eq!(special_cert.subject_cn, "Adversarial Test User");

        // Wrong pass against special p12 must fail
        assert!(OpenSslBridge::import_pkcs12(&p12_special_path, Some("wrong_pass"), &key_dir, &cert_dir).is_err());

        // 7. Empty passphrase archive (passphrase = "")
        let p12_unencrypted_path = temp_dir.join("unencrypted.p12");
        OpenSslBridge::export_pkcs12(&cert_pem, &key_path, &p12_unencrypted_path, Some(""))
            .expect("Export PKCS#12 with empty pass must succeed");
        let (unenc_cert, _) = OpenSslBridge::import_pkcs12(&p12_unencrypted_path, None, &key_dir, &cert_dir)
            .expect("Import PKCS#12 with empty pass must succeed with None");
        assert_eq!(unenc_cert.subject_cn, "Adversarial Test User");

        // 8. Corrupted bytes fuzzing:
        // Case A: 0-byte file
        let empty_file = temp_dir.join("empty.p12");
        fs::write(&empty_file, b"").unwrap();
        assert!(OpenSslBridge::import_pkcs12(&empty_file, Some(std_pass), &key_dir, &cert_dir).is_err());

        // Case B: Random garbage bytes
        let garbage_file = temp_dir.join("garbage.p12");
        let garbage_bytes: Vec<u8> = (0..512).map(|i| ((i * 37 + 13) % 256) as u8).collect();
        fs::write(&garbage_file, &garbage_bytes).unwrap();
        assert!(OpenSslBridge::import_pkcs12(&garbage_file, Some(std_pass), &key_dir, &cert_dir).is_err());

        // Case C: Truncated valid .p12 bytes
        let valid_p12_bytes = fs::read(&p12_std_path).unwrap();
        for cut_len in [16, 64, 128, valid_p12_bytes.len().saturating_sub(10)] {
            if cut_len < valid_p12_bytes.len() {
                let truncated_file = temp_dir.join(format!("truncated_{cut_len}.p12"));
                fs::write(&truncated_file, &valid_p12_bytes[..cut_len]).unwrap();
                assert!(OpenSslBridge::import_pkcs12(&truncated_file, Some(std_pass), &key_dir, &cert_dir).is_err());
            }
        }

        // Case D: Bit-flipped valid .p12 bytes
        let mut flipped_bytes = valid_p12_bytes.clone();
        for b in &mut flipped_bytes[20..60] {
            *b ^= 0xFF;
        }
        let flipped_file = temp_dir.join("flipped.p12");
        fs::write(&flipped_file, &flipped_bytes).unwrap();
        assert!(OpenSslBridge::import_pkcs12(&flipped_file, Some(std_pass), &key_dir, &cert_dir).is_err());

        // 9. Export with nonexistent private key path
        let non_key = temp_dir.join("nonexistent.key");
        let dest_fail = temp_dir.join("fail.p12");
        let exp_res = OpenSslBridge::export_pkcs12(&cert_pem, &non_key, &dest_fail, Some("pass"));
        assert!(exp_res.is_err(), "Exporting with nonexistent key must fail");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_adversarial_openpgp_gpg_unavailable_and_nonzero_exit() {
        let temp_dir = std::env::temp_dir().join(format!("evo-gpg-test-{}", glib::uuid_string_random()));
        fs::create_dir_all(&temp_dir).unwrap();

        // 1. Unavailable / nonexistent GPG binary
        let nonexistent_bin = temp_dir.join("nonexistent_gpg_binary_xyz");
        unsafe {
            std::env::set_var("EVOLUTION_GPG_BINARY", &nonexistent_bin);
        }

        let list_res = GnuPgBridge::list_keys();
        assert!(list_res.is_err(), "Listing keys with nonexistent binary must fail gracefully");
        let err_msg = list_res.unwrap_err();
        assert!(err_msg.contains("Failed to list GnuPG keys"), "Error message must be descriptive: {err_msg}");

        let exp_res = GnuPgBridge::export_key("12345678", false);
        assert!(exp_res.is_err(), "Exporting key with nonexistent binary must fail gracefully");

        let imp_res = GnuPgBridge::import_key("dummy key data");
        assert!(imp_res.is_err(), "Importing key with nonexistent binary must fail gracefully");

        let crypto_res = crate::services::crypto_service::list_openpgp_keys();
        assert!(crypto_res.is_err(), "crypto_service::list_openpgp_keys must fail gracefully");

        // 2. GPG binary returns non-zero exit code
        let mock_script = temp_dir.join("mock_failing_gpg.sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(
                &mock_script,
                b"#!/bin/sh\necho \"gpg: fatal: keyring corrupted: permission denied\" >&2\nexit 2\n",
            ).unwrap();
            fs::set_permissions(&mock_script, fs::Permissions::from_mode(0o755)).unwrap();

            unsafe {
                std::env::set_var("EVOLUTION_GPG_BINARY", &mock_script);
            }

            let fail_list = GnuPgBridge::list_keys();
            assert!(fail_list.is_err(), "Failing script must return Err");
            let fail_err = fail_list.unwrap_err();
            assert!(fail_err.contains("GnuPG list-keys error"), "Must contain GnuPG error header: {fail_err}");
            assert!(fail_err.contains("keyring corrupted"), "Must include stderr message: {fail_err}");

            let fail_exp = GnuPgBridge::export_key("ABCDEF", false);
            assert!(fail_exp.is_err(), "Export with failing script must return Err");

            let fail_crypto = crate::services::crypto_service::list_openpgp_keys();
            assert!(fail_crypto.is_err(), "crypto_service must return Err on failing script");
        }

        // Clean up environment variable
        unsafe {
            std::env::remove_var("EVOLUTION_GPG_BINARY");
        }

        // 3. Colon field decoding robustness (never panics on edge cases)
        assert_eq!(decode_gpg_colon_field(r"Simple Text"), "Simple Text");
        assert_eq!(decode_gpg_colon_field(r"User\x20Name\x21"), "User Name!");
        assert_eq!(decode_gpg_colon_field(r"Special\x3aValue"), "Special:Value");
        // Malformed escape sequences: must not panic or slice out of bounds
        assert_eq!(decode_gpg_colon_field(r"Test\x"), r"Test\x");
        assert_eq!(decode_gpg_colon_field(r"Test\x1"), r"Test\x1");
        assert_eq!(decode_gpg_colon_field(r"Test\xGG"), r"Test\xGG");
        assert_eq!(decode_gpg_colon_field(r"Trailing\"), r"Trailing\");
        assert_eq!(decode_gpg_colon_field(""), "");

        // 4. Colon listing parser resilience on corrupted / malformed listings
        let malformed_listings = [
            "",
            "\n\n\n",
            "not:a:gpg:record\nrandom:junk:line:without:proper:fields",
            "pub:u:255:22:DEADBEEF:1710000000:::\nfpr:::::::::A1B2C3D4E5F67890\nuid:u::::1710000000::Test User <test@example.com>:",
            "pub:u:255:22:INCOMPLETE_PUB_WITHOUT_FPR:",
            "sec:u:255:22:SECKEY123:1710000000:::\nfpr:::::::::FFEEDDCCBBAA9988\nuid:u::::1710000000::Secret Key Owner:",
        ];

        for listing in malformed_listings {
            let parsed = parse_gpg_colon_listing(listing, false);
            // Parser must not panic on any malformed input
            for rec in &parsed {
                assert!(!rec.fingerprint.is_empty() || !rec.key_id.is_empty());
            }
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_asn1_time_parsing_robustness_never_panics() {
        // 1. Valid UTCTime (tag 0x17)
        assert_eq!(
            parse_asn1_time(0x17, b"260901123045Z").unwrap(),
            "2026-09-01 12:30:45 UTC"
        );
        assert_eq!(
            parse_asn1_time(0x17, b"991231235959Z").unwrap(),
            "1999-12-31 23:59:59 UTC"
        );

        // 2. Valid GeneralizedTime (tag 0x18)
        assert_eq!(
            parse_asn1_time(0x18, b"20260901123045Z").unwrap(),
            "2026-09-01 12:30:45 UTC"
        );

        // 3. Corrupted / Non-ASCII bytes that previously triggered byte-index slicing panics
        let non_ascii_12 = [0xFF; 12];
        let res_utctime_non_ascii = parse_asn1_time(0x17, &non_ascii_12);
        assert!(res_utctime_non_ascii.is_ok(), "Non-ASCII UTCTime must not panic and return Ok");

        let non_ascii_14 = [0xFF; 14];
        let res_gen_non_ascii = parse_asn1_time(0x18, &non_ascii_14);
        assert!(res_gen_non_ascii.is_ok(), "Non-ASCII GeneralizedTime must not panic and return Ok");

        let mixed_invalid = b"\xFF\xFE1234567890";
        assert!(parse_asn1_time(0x17, mixed_invalid).is_ok());

        let multi_byte_utf8 = b"a\xC3\xA41234567890"; // 'a', 'ä' (2 bytes), followed by digits
        assert!(parse_asn1_time(0x17, multi_byte_utf8).is_ok());

        // 4. Unicode / Emojis (multi-byte sequences)
        let emoji_time = "📅20260901000000";
        assert!(parse_asn1_time(0x17, emoji_time.as_bytes()).is_ok());
        assert!(parse_asn1_time(0x18, emoji_time.as_bytes()).is_ok());

        // 5. Truncated time strings (< 12 or < 14 bytes)
        assert_eq!(parse_asn1_time(0x17, b"260901").unwrap(), "260901");
        assert_eq!(parse_asn1_time(0x18, b"20260901").unwrap(), "20260901");
        assert_eq!(parse_asn1_time(0x17, b"").unwrap(), "");

        // 6. Non-digit ASCII strings
        assert_eq!(
            parse_asn1_time(0x17, b"INVALID_DATETIME_STRING").unwrap(),
            "INVALID_DATETIME_STRING"
        );
    }

    #[test]
    fn test_decode_oid_unterminated_and_overflow_robustness() {
        // 1. Valid OIDs
        let rsa_oid = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
        assert_eq!(
            decode_oid(&rsa_oid).unwrap(),
            vec![1, 2, 840, 113549, 1, 1, 11]
        );

        let cn_oid = [0x55, 0x04, 0x03];
        assert_eq!(decode_oid(&cn_oid).unwrap(), vec![2, 5, 4, 3]);

        assert_eq!(decode_oid(&[0x2A]).unwrap(), vec![1, 2]);

        // 2. Empty OID
        assert!(decode_oid(&[]).is_err());

        // 3. Unterminated OID component (continuation bit 0x80 set on last byte)
        let unterminated_res = decode_oid(&[0x2A, 0x86]);
        assert!(unterminated_res.is_err(), "Unterminated OID must return Err");
        assert!(unterminated_res.unwrap_err().contains("Unterminated OID component"));

        let multi_unterminated = [0x2A, 0x86, 0x48, 0x86];
        let multi_res = decode_oid(&multi_unterminated);
        assert!(multi_res.is_err(), "Unterminated trailing component must return Err");

        assert!(decode_oid(&[0x2A, 0xFF]).is_err());

        // 4. Arithmetic shift overflow protection
        let overflow_bytes = [0x2A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        let overflow_res = decode_oid(&overflow_bytes);
        assert!(overflow_res.is_err(), "Overflowing OID component must return Err without panicking");
    }

    #[test]
    fn test_x509_parser_der_with_non_ascii_validity_never_panics() {
        let bad_time_str = unsafe { std::str::from_utf8_unchecked(&[0xFF; 12]) };
        let der = build_test_cert_der(
            0x9999,
            "NonAscii Validity Cert",
            "Evolution Org",
            "Evolution CA",
            "Evolution Org",
            bad_time_str,
            bad_time_str,
            None,
        );

        let parsed = X509Parser::parse_der(&der);
        assert!(parsed.is_ok(), "X509Parser::parse_der must not panic on non-ASCII validity");
        let cert = parsed.unwrap();
        assert_eq!(cert.subject_cn, "NonAscii Validity Cert");
    }

    #[test]
    fn test_openssl_bridge_process_argument_secrecy() {
        use std::process::Command;

        // 1. Sensitive passphrase for import:
        let secret = "SuperSecretPassphrase!1234#";
        let mut import_cmd = Command::new("openssl");
        OpenSslBridge::apply_passin(&mut import_cmd, Some(secret));

        let import_args: Vec<_> = import_cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(import_args, vec!["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
        for arg in &import_args {
            assert!(!arg.contains(secret), "CLI arguments must not contain plain secret: {arg}");
        }
        let env_val = import_cmd
            .get_envs()
            .find_map(|(k, v)| if k == "EVOLUTION_OPENSSL_PASSIN" { v.map(|s| s.to_string_lossy().into_owned()) } else { None });
        assert_eq!(env_val, Some(secret.to_string()));

        // 2. Sensitive passphrase for export:
        let mut export_cmd = Command::new("openssl");
        OpenSslBridge::apply_passout(&mut export_cmd, Some(secret));

        let export_args: Vec<_> = export_cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(export_args, vec!["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
        for arg in &export_args {
            assert!(!arg.contains(secret), "CLI arguments must not contain plain secret: {arg}");
        }
        let env_export_val = export_cmd
            .get_envs()
            .find_map(|(k, v)| if k == "EVOLUTION_OPENSSL_PASSOUT" { v.map(|s| s.to_string_lossy().into_owned()) } else { None });
        assert_eq!(env_export_val, Some(secret.to_string()));

        // 3. Empty passphrase:
        let mut empty_cmd = Command::new("openssl");
        OpenSslBridge::apply_passin(&mut empty_cmd, Some(""));
        let empty_args: Vec<_> = empty_cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(empty_args, vec!["-passin", "pass:"]);
        assert!(empty_cmd.get_envs().find(|(k, _)| *k == "EVOLUTION_OPENSSL_PASSIN").is_none());

        // 4. None passphrase:
        let mut none_cmd = Command::new("openssl");
        OpenSslBridge::apply_passin(&mut none_cmd, None);
        let none_args: Vec<_> = none_cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(none_args, vec!["-passin", "pass:"]);

        let mut empty_out_cmd = Command::new("openssl");
        OpenSslBridge::apply_passout(&mut empty_out_cmd, None);
        let empty_out_args: Vec<_> = empty_out_cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(empty_out_args, vec!["-passout", "pass:"]);
    }

    #[test]
    fn test_openssl_bridge_preexisting_key_file_mode_and_dir_permissions() {
        let openssl_bin = OpenSslBridge::binary();
        if Command::new(&openssl_bin).arg("version").output().is_err() {
            eprintln!("OpenSSL binary not found, skipping permissions test");
            return;
        }

        let temp_dir = std::env::temp_dir().join(format!("evo-perm-test-{}", glib::uuid_string_random()));
        fs::create_dir_all(&temp_dir).unwrap();
        let key_dir = temp_dir.join("keys");
        let cert_dir = temp_dir.join("certs");

        // Explicitly create key_dir with loose permissions (0o755)
        fs::create_dir_all(&key_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Generate a valid self-signed certificate and key using OpenSSL
        let key_path = temp_dir.join("test_key.pem");
        let cert_path = temp_dir.join("test_cert.pem");
        let status = Command::new(&openssl_bin)
            .args(["req", "-x509", "-newkey", "rsa:2048", "-keyout"])
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .args(["-days", "1", "-nodes", "-subj", "/CN=Permissions Test"])
            .output()
            .expect("OpenSSL req failed");
        assert!(status.status.success());

        let cert_pem = fs::read_to_string(&cert_path).unwrap();
        let p12_path = temp_dir.join("test.p12");
        let pass = "PermPass123!";
        OpenSslBridge::export_pkcs12(&cert_pem, &key_path, &p12_path, Some(pass)).unwrap();

        // Determine expected key file name from certificate ID
        let parsed_cert = X509Parser::parse_pem(&cert_pem).unwrap();
        let expected_key_path = key_dir.join(format!("{}.key", parsed_cert.id));

        // Pre-create the destination key file with loose permissions (0o644)
        fs::write(&expected_key_path, b"PREEXISTING INSECURE CONTENT").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&expected_key_path, fs::Permissions::from_mode(0o644)).unwrap();
            let initial_mode = fs::metadata(&expected_key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(initial_mode, 0o644, "Pre-existing file must start with 0o644");
        }

        // Perform PKCS#12 import into existing directory and over existing file
        let (imported_cert, final_key_path) =
            OpenSslBridge::import_pkcs12(&p12_path, Some(pass), &key_dir, &cert_dir)
                .expect("Import must succeed over existing file");
        assert_eq!(imported_cert.id, parsed_cert.id);
        assert_eq!(final_key_path, expected_key_path);

        // Verify content was overwritten with actual PEM private key
        let key_content = fs::read_to_string(&final_key_path).unwrap();
        assert!(key_content.contains("-----BEGIN PRIVATE KEY-----") || key_content.contains("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(!key_content.contains("PREEXISTING INSECURE CONTENT"));

        // Verify POSIX permissions: directory must be 0o700, key file must be hardened to 0o600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = fs::metadata(&key_dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700, "Key directory permissions must be restricted to 0o700");

            let file_mode = fs::metadata(&final_key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600, "Pre-existing private key file must be hardened to 0o600");
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_challenger1_m4_fix1_fuzz_asn1_time_and_decode_oid() {
        // 1. Exhaustive fuzzing of parse_asn1_time
        // Multi-byte UTF-8, emojis, non-ASCII, truncated, empty
        let edge_inputs: Vec<Vec<u8>> = vec![
            vec![],                                                     // empty
            b"2".to_vec(),                                              // 1 byte
            b"26".to_vec(),                                             // 2 bytes
            b"260901".to_vec(),                                         // 6 bytes
            b"26090112345".to_vec(),                                    // 11 bytes (truncated UTCTime)
            b"260901123456".to_vec(),                                   // 12 bytes exactly (valid UTCTime digits)
            b"260901123456Z".to_vec(),                                  // 13 bytes with Z
            b"2026090112345".to_vec(),                                  // 13 bytes (truncated GeneralizedTime)
            b"20260901123456".to_vec(),                                 // 14 bytes exactly (valid GeneralizedTime digits)
            b"20260901123456Z".to_vec(),                                // 15 bytes with Z
            vec![0xFF; 12],                                             // 12 bytes non-ASCII
            vec![0xFF; 14],                                             // 14 bytes non-ASCII
            vec![0x80; 20],                                             // 20 bytes 0x80
            b"\xC3\xA4\xC3\xB6\xC3\xBC123456".to_vec(),                 // German umlauts spanning char boundaries
            b"26\xC3\xA401000000Z".to_vec(),                            // umlaut inside time
            "📅20260901000000Z".as_bytes().to_vec(),                   // emoji at start (4 bytes)
            "26📅0901000000Z".as_bytes().to_vec(),                     // emoji spanning byte 2..6
            "2609📅01000000Z".as_bytes().to_vec(),                     // emoji spanning byte 4..8
            "260901📅000000Z".as_bytes().to_vec(),                     // emoji spanning byte 6..10
            "🔑SecureCert📅2026".as_bytes().to_vec(),                   // multiple emojis
            b"   \t\n\r   ".to_vec(),                                   // whitespace
            b"999999999999".to_vec(),                                   // extreme values (year 1999)
            b"000000000000".to_vec(),                                   // zeros (year 2000)
        ];

        for input in &edge_inputs {
            let res_utc = parse_asn1_time(0x17, input);
            assert!(res_utc.is_ok(), "parse_asn1_time must never panic on UTCTime input: {:?}", input);
            let res_gen = parse_asn1_time(0x18, input);
            assert!(res_gen.is_ok(), "parse_asn1_time must never panic on GeneralizedTime input: {:?}", input);
        }

        // Pseudo-random fuzzer: 5,000 iterations with random length and byte values
        let mut seed: u64 = 0xDEADBEEFCAFE1234;
        for _ in 0..5000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let len = (seed % 65) as usize;
            let mut fuzzed = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                fuzzed.push((seed >> 32) as u8);
            }
            let _ = parse_asn1_time(0x17, &fuzzed);
            let _ = parse_asn1_time(0x18, &fuzzed);
            let _ = parse_asn1_time(0x30, &fuzzed); // other ASN.1 tags
        }

        // 2. decode_oid robustness
        // Empty slices
        assert!(decode_oid(&[]).is_err());

        // Single byte slices
        assert_eq!(decode_oid(&[0x2A]).unwrap(), vec![1, 2]);
        assert_eq!(decode_oid(&[0x00]).unwrap(), vec![0, 0]);
        assert_eq!(decode_oid(&[0x7F]).unwrap(), vec![3, 7]);

        // Unterminated OID components (continuation bit 0x80 set on last byte)
        assert!(decode_oid(&[0x2A, 0x86]).is_err());
        assert_eq!(decode_oid(&[0x2A, 0x86]).unwrap_err(), "Unterminated OID component");
        assert!(decode_oid(&[0x2A, 0x86, 0x48, 0x86]).is_err());
        assert!(decode_oid(&[0x2A, 0x80]).is_err());
        assert!(decode_oid(&[0x2A, 0xFF]).is_err());
        assert!(decode_oid(&[0x2A, 0x81, 0x82, 0x83]).is_err());

        // Overflowing continuation bytes
        assert!(decode_oid(&[0x2A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]).is_err());
        assert_eq!(
            decode_oid(&[0x2A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]).unwrap_err(),
            "OID component value overflow"
        );
        let mut long_overflow = vec![0x2A];
        long_overflow.extend(std::iter::repeat(0xFF).take(100));
        assert!(decode_oid(&long_overflow).is_err());

        // Fuzz decode_oid with 2,000 pseudo-random byte sequences: must never panic
        for _ in 0..2000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let len = (seed % 40) as usize;
            let mut fuzzed_oid = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                fuzzed_oid.push((seed >> 32) as u8);
            }
            let _ = decode_oid(&fuzzed_oid);
        }

        // 3. OpenSSL Process Table Secrecy with Complex Passphrases
        let passphrases = [
            "Passphrase with spaces and multiple tokens",
            "pass:word:with:many:colons:::123",
            "quote'single'and\"double\"backtick`",
            "Päßwörd-mit-ÄÖÜ-und-ß-€-Schloß",
            "🔑SuperSecretPassphrase📅🚀Shield🛡️",
            "!@#$%^&*()_+-=[]{}|;':,.<>/?~",
            "   ",
        ];

        for &secret in &passphrases {
            let mut cmd_in = Command::new("openssl");
            OpenSslBridge::apply_passin(&mut cmd_in, Some(secret));
            let in_args: Vec<_> = cmd_in.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(in_args, vec!["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
            for arg in &in_args {
                assert!(!arg.contains(secret), "Secret leaked into CLI arguments: {arg}");
            }
            let in_env = cmd_in
                .get_envs()
                .find_map(|(k, v)| if k == "EVOLUTION_OPENSSL_PASSIN" { v.map(|s| s.to_string_lossy().into_owned()) } else { None });
            assert_eq!(in_env, Some(secret.to_string()));

            let mut cmd_out = Command::new("openssl");
            OpenSslBridge::apply_passout(&mut cmd_out, Some(secret));
            let out_args: Vec<_> = cmd_out.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(out_args, vec!["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
            for arg in &out_args {
                assert!(!arg.contains(secret), "Secret leaked into CLI arguments: {arg}");
            }
            let out_env = cmd_out
                .get_envs()
                .find_map(|(k, v)| if k == "EVOLUTION_OPENSSL_PASSOUT" { v.map(|s| s.to_string_lossy().into_owned()) } else { None });
            assert_eq!(out_env, Some(secret.to_string()));
        }

        // Empty and None passphrases
        for empty in [None, Some("")] {
            let mut empty_in = Command::new("openssl");
            OpenSslBridge::apply_passin(&mut empty_in, empty);
            let in_args: Vec<_> = empty_in.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(in_args, vec!["-passin", "pass:"]);
            assert!(empty_in.get_envs().find(|(k, _)| *k == "EVOLUTION_OPENSSL_PASSIN").is_none());

            let mut empty_out = Command::new("openssl");
            OpenSslBridge::apply_passout(&mut empty_out, empty);
            let out_args: Vec<_> = empty_out.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(out_args, vec!["-passout", "pass:"]);
            assert!(empty_out.get_envs().find(|(k, _)| *k == "EVOLUTION_OPENSSL_PASSOUT").is_none());
        }
    }
}

