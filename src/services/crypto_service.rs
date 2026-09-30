// SPDX-License-Identifier: GPL-3.0-or-later
//
// crypto_service — OpenPGP (GnuPG) and S/MIME cipher context wrapper.
//
// The portable OpenPGP path uses the GnuPG executable without a shell and
// emits standards-shaped PGP/MIME.  Native Camel/EDS integration can use the
// same MIME boundary later, while the service remains usable for local
// composer tests and non-Linux development builds.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStatus {
    ValidGood,
    ValidUntrusted,
    InvalidBadSignature,
    ExpiredKey,
    NoKeyAvailable,
    None,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionStatus {
    EncryptedValid,
    EncryptedUnknownKey,
    NotEncrypted,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoProtocol {
    OpenPgp,
    Smime,
    Unknown,
    None,
}

impl CryptoProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenPgp => "OpenPGP",
            Self::Smime => "S/MIME",
            Self::Unknown => "Unbekanntes Format",
            Self::None => "Keine",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MailCryptoDetails {
    pub protocol: CryptoProtocol,
    pub signature_status: SignatureStatus,
    pub encryption_status: EncryptionStatus,
    pub signer_identity: Option<String>,
    pub key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OpenPgpOptions {
    pub(crate) sign: bool,
    pub(crate) encrypt: bool,
    pub(crate) signing_key: String,
    pub(crate) recipients: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SmimeOptions {
    pub(crate) sign: bool,
    pub(crate) encrypt: bool,
    pub(crate) signer_certificate: String,
    pub(crate) private_key: String,
    pub(crate) recipient_certificates: Vec<String>,
    pub(crate) ca_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenPgpKey {
    pub(crate) fingerprint: String,
    pub(crate) identity: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenPgpInspection {
    pub(crate) details: MailCryptoDetails,
    /// Decrypted MIME entity, without the visible RFC822 envelope headers.
    /// The caller may combine it with the original headers before reparsing.
    pub(crate) decrypted_mime: Option<Vec<u8>>,
}

impl MailCryptoDetails {
    /// Human-readable envelope status for the message header.
    ///
    /// This wording deliberately distinguishes detection from verification:
    /// a MIME marker alone must never be presented as a valid signature or a
    /// successfully decrypted message.
    pub fn display_text(&self) -> String {
        let signed = self.signature_status != SignatureStatus::None;
        let encrypted = self.encryption_status != EncryptionStatus::NotEncrypted;
        if !signed && !encrypted {
            return "Keine OpenPGP-/S/MIME-Sicherung erkannt".to_string();
        }

        let mut details = Vec::new();
        if signed {
            details.push(match self.signature_status {
                SignatureStatus::ValidGood => "Signatur gültig",
                SignatureStatus::ValidUntrusted => {
                    "Signatur gültig, Schlüssel nicht vertrauenswürdig"
                }
                SignatureStatus::InvalidBadSignature => "Signatur ungültig",
                SignatureStatus::ExpiredKey => "Signatur vorhanden, Schlüssel abgelaufen",
                SignatureStatus::NoKeyAvailable => "Signatur vorhanden, nicht verifiziert",
                SignatureStatus::None => "",
            });
        }
        if encrypted {
            details.push(match self.encryption_status {
                EncryptionStatus::EncryptedValid => "verschlüsselt",
                EncryptionStatus::EncryptedUnknownKey => {
                    "verschlüsselt, Entschlüsselung nicht verfügbar"
                }
                EncryptionStatus::NotEncrypted => "",
            });
        }
        details.retain(|detail| !detail.is_empty());
        format!("{}: {}", self.protocol.as_str(), details.join(" · "))
    }
}

#[allow(dead_code)]
pub struct CryptoService;

#[allow(dead_code)]
impl CryptoService {
    /// Inspect the MIME envelope without claiming cryptographic verification.
    ///
    /// `inspect_openpgp_mime` performs the portable GnuPG operation for
    /// OpenPGP messages; this lightweight method remains useful while the
    /// worker result is pending and for S/MIME detection.
    pub fn verify_signature(mime_part_data: &[u8]) -> MailCryptoDetails {
        let mime = String::from_utf8_lossy(mime_part_data).to_ascii_lowercase();
        let pgp_signed = mime.contains("begin pgp signed message")
            || mime.contains("application/pgp-signature")
            || mime.contains("protocol=\"application/pgp-signature\"");
        let pgp_encrypted = mime.contains("begin pgp message")
            || mime.contains("application/pgp-encrypted")
            || mime.contains("protocol=\"application/pgp-encrypted\"");
        let smime_signed = mime.contains("application/pkcs7-signature")
            || mime.contains("application/x-pkcs7-signature")
            || mime.contains("protocol=\"application/pkcs7-signature\"")
            || mime.contains("protocol=\"application/x-pkcs7-signature\"");
        let smime_encrypted =
            mime.contains("application/pkcs7-mime") || mime.contains("application/x-pkcs7-mime");
        let looks_signed = pgp_signed || smime_signed;
        let looks_encrypted = pgp_encrypted || smime_encrypted;
        let protocol = if pgp_signed || pgp_encrypted {
            CryptoProtocol::OpenPgp
        } else if smime_signed || smime_encrypted {
            CryptoProtocol::Smime
        } else if looks_signed || looks_encrypted {
            CryptoProtocol::Unknown
        } else {
            CryptoProtocol::None
        };

        MailCryptoDetails {
            protocol,
            signature_status: if looks_signed {
                SignatureStatus::NoKeyAvailable
            } else {
                SignatureStatus::None
            },
            encryption_status: if looks_encrypted {
                EncryptionStatus::EncryptedUnknownKey
            } else {
                EncryptionStatus::NotEncrypted
            },
            signer_identity: None,
            key_fingerprint: None,
        }
    }
}

/// Inspect an RFC822 message with the local GnuPG installation when it has an
/// OpenPGP/MIME envelope.  The operation is deliberately kept outside GTK's
/// main thread by its callers; it may contact gpg-agent or pinentry.
pub(crate) fn inspect_openpgp_mime(raw: &[u8]) -> Result<OpenPgpInspection, String> {
    inspect_openpgp_mime_inner(raw, 0)
}

pub(crate) fn inspect_crypto_mime(
    raw: &[u8],
    smime_options: &SmimeOptions,
) -> Result<OpenPgpInspection, String> {
    match CryptoService::verify_signature(raw).protocol {
        CryptoProtocol::Smime => inspect_smime_mime_inner(raw, smime_options, 0),
        CryptoProtocol::OpenPgp => inspect_openpgp_mime(raw),
        _ => Ok(OpenPgpInspection {
            details: CryptoService::verify_signature(raw),
            decrypted_mime: None,
        }),
    }
}

fn inspect_openpgp_mime_inner(
    raw: &[u8],
    recursion_depth: usize,
) -> Result<OpenPgpInspection, String> {
    if recursion_depth > 2 {
        return Err("Verschachtelte OpenPGP-Nachricht ist zu tief verschachtelt".to_string());
    }

    let mut details = CryptoService::verify_signature(raw);
    if details.protocol != CryptoProtocol::OpenPgp {
        return Ok(OpenPgpInspection {
            details,
            decrypted_mime: None,
        });
    }

    let (headers, body) = parse_mime_entity(raw)?;
    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or_default();
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if media_type == "multipart/signed" {
        let boundary = mime_parameter_value(content_type, "boundary")
            .ok_or_else(|| "OpenPGP-Signatur enthält keine MIME-Grenze".to_string())?;
        let parts = split_multipart_parts(body, &boundary);
        if parts.len() < 2 {
            return Err("OpenPGP-Signatur enthält nicht genügend MIME-Teile".to_string());
        }
        let (signature_headers, signature_body) = parse_mime_entity(parts[1])?;
        let signature = decode_mime_body(&signature_headers, signature_body)?;
        let verification = run_gpg_verify(parts[0], &signature)?;
        details.signature_status = verification.status;
        details.signer_identity = verification.identity;
        details.key_fingerprint = verification.fingerprint;
        return Ok(OpenPgpInspection {
            details,
            decrypted_mime: None,
        });
    }

    if media_type == "multipart/encrypted" {
        let boundary = mime_parameter_value(content_type, "boundary")
            .ok_or_else(|| "OpenPGP-Verschlüsselung enthält keine MIME-Grenze".to_string())?;
        let parts = split_multipart_parts(body, &boundary);
        if parts.len() < 2 {
            return Err("OpenPGP-Verschlüsselung enthält nicht genügend MIME-Teile".to_string());
        }
        let (encrypted_headers, encrypted_body) = parse_mime_entity(parts[1])?;
        let ciphertext = decode_mime_body(&encrypted_headers, encrypted_body)?;
        let decryption = run_gpg_decrypt(&ciphertext)?;
        if !decryption.success {
            details.encryption_status = EncryptionStatus::EncryptedUnknownKey;
            details.key_fingerprint = decryption.key_fingerprint;
            return Ok(OpenPgpInspection {
                details,
                decrypted_mime: None,
            });
        }

        details.encryption_status = EncryptionStatus::EncryptedValid;
        details.key_fingerprint = decryption.key_fingerprint;
        let inner = inspect_openpgp_mime_inner(&decryption.plaintext, recursion_depth + 1)?;
        if inner.details.signature_status != SignatureStatus::None {
            details.signature_status = inner.details.signature_status;
            details.signer_identity = inner.details.signer_identity;
            details.key_fingerprint = inner.details.key_fingerprint.or(details.key_fingerprint);
        }
        return Ok(OpenPgpInspection {
            details,
            decrypted_mime: Some(decryption.plaintext),
        });
    }

    // RFC 3156 normally uses multipart/signed, but accepting a clear-signed
    // text part makes local imports behave like common Evolution messages.
    if body
        .windows(b"-----BEGIN PGP SIGNED MESSAGE-----".len())
        .any(|window| window == b"-----BEGIN PGP SIGNED MESSAGE-----")
    {
        let verification = run_gpg_clear_verify(body)?;
        details.signature_status = verification.status;
        details.signer_identity = verification.identity;
        details.key_fingerprint = verification.fingerprint;
    }

    Ok(OpenPgpInspection {
        details,
        decrypted_mime: None,
    })
}

fn inspect_smime_mime_inner(
    raw: &[u8],
    options: &SmimeOptions,
    recursion_depth: usize,
) -> Result<OpenPgpInspection, String> {
    if recursion_depth > 2 {
        return Err("Verschachtelte S/MIME-Nachricht ist zu tief verschachtelt".to_string());
    }

    let mut details = CryptoService::verify_signature(raw);
    if details.protocol != CryptoProtocol::Smime {
        return Ok(OpenPgpInspection {
            details,
            decrypted_mime: None,
        });
    }
    let (headers, body) = parse_mime_entity(raw)?;
    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or_default();
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if media_type == "multipart/signed" {
        let boundary = mime_parameter_value(content_type, "boundary")
            .ok_or_else(|| "S/MIME-Signatur enthält keine MIME-Grenze".to_string())?;
        let parts = split_multipart_parts(body, &boundary);
        if parts.len() < 2 {
            return Err("S/MIME-Signatur enthält nicht genügend MIME-Teile".to_string());
        }
        let (signature_headers, signature_body) = parse_mime_entity(parts[1])?;
        let signature = decode_mime_body(&signature_headers, signature_body)?;
        let verification = run_openssl_cms_verify(parts[0], &signature, &options.ca_file)?;
        details.signature_status = verification.status;
        details.signer_identity = verification.identity;
        details.key_fingerprint = verification.fingerprint;
        return Ok(OpenPgpInspection {
            details,
            decrypted_mime: None,
        });
    }

    if media_type == "application/pkcs7-mime" || media_type == "application/x-pkcs7-mime" {
        let ciphertext = decode_mime_body(&headers, body)?;
        let decryption = run_openssl_cms_decrypt(&ciphertext, options)?;
        if !decryption.success {
            details.encryption_status = EncryptionStatus::EncryptedUnknownKey;
            return Ok(OpenPgpInspection {
                details,
                decrypted_mime: None,
            });
        }
        details.encryption_status = EncryptionStatus::EncryptedValid;
        let inner = inspect_crypto_mime_inner(&decryption.plaintext, options, recursion_depth + 1)?;
        if inner.details.signature_status != SignatureStatus::None {
            details.signature_status = inner.details.signature_status;
            details.signer_identity = inner.details.signer_identity;
            details.key_fingerprint = inner.details.key_fingerprint;
        }
        return Ok(OpenPgpInspection {
            details,
            decrypted_mime: Some(decryption.plaintext),
        });
    }

    Ok(OpenPgpInspection {
        details,
        decrypted_mime: None,
    })
}

fn inspect_crypto_mime_inner(
    raw: &[u8],
    smime_options: &SmimeOptions,
    recursion_depth: usize,
) -> Result<OpenPgpInspection, String> {
    match CryptoService::verify_signature(raw).protocol {
        CryptoProtocol::Smime => inspect_smime_mime_inner(raw, smime_options, recursion_depth),
        CryptoProtocol::OpenPgp => inspect_openpgp_mime_inner(raw, recursion_depth),
        _ => Ok(OpenPgpInspection {
            details: CryptoService::verify_signature(raw),
            decrypted_mime: None,
        }),
    }
}

type MimeHeaders = HashMap<String, String>;

fn parse_mime_entity(raw: &[u8]) -> Result<(MimeHeaders, &[u8]), String> {
    let (separator, separator_len) = find_header_separator(raw)
        .ok_or_else(|| "MIME-Entity enthält keinen gültigen Headerabschluss".to_string())?;
    let headers = parse_mime_headers(&raw[..separator])?;
    Ok((headers, &raw[separator + separator_len..]))
}

fn parse_mime_headers(raw: &[u8]) -> Result<MimeHeaders, String> {
    let mut headers = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_value = String::new();

    for line in raw.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if current_name.is_some() {
                current_value.push(' ');
                current_value.push_str(String::from_utf8_lossy(line).trim());
            }
            continue;
        }

        if line.is_empty() {
            continue;
        }
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            return Err("MIME-Entity enthält einen ungültigen Header".to_string());
        };
        if let Some(name) = current_name.take() {
            headers.insert(name, current_value.trim().to_string());
            current_value.clear();
        }
        current_name = Some(
            String::from_utf8_lossy(&line[..separator])
                .trim()
                .to_ascii_lowercase(),
        );
        current_value.push_str(String::from_utf8_lossy(&line[separator + 1..]).trim());
    }
    if let Some(name) = current_name {
        headers.insert(name, current_value.trim().to_string());
    }
    Ok(headers)
}

fn mime_parameter_value(value: &str, parameter: &str) -> Option<String> {
    let wanted = parameter.trim().to_ascii_lowercase();
    for part in value.split(';').skip(1) {
        let Some((name, raw_value)) = part.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(&wanted) {
            continue;
        }
        let raw_value = raw_value.trim();
        if raw_value.len() >= 2 && raw_value.starts_with('"') && raw_value.ends_with('"') {
            return Some(raw_value[1..raw_value.len() - 1].replace("\\\"", "\""));
        }
        return Some(raw_value.to_string());
    }
    None
}

fn split_multipart_parts<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let marker = format!("--{boundary}").into_bytes();
    let mut markers = Vec::new();
    let mut line_start = 0;
    while line_start < body.len() {
        if body[line_start..].starts_with(&marker) {
            let after_marker = line_start + marker.len();
            let suffix = &body[after_marker..];
            if suffix.starts_with(b"--") || suffix.starts_with(b"\r\n") || suffix.starts_with(b"\n")
            {
                markers.push((line_start, after_marker));
            }
        }
        let Some(relative_end) = body[line_start..].iter().position(|byte| *byte == b'\n') else {
            break;
        };
        line_start += relative_end + 1;
    }

    let mut parts = Vec::new();
    for (index, (_, after_marker)) in markers.iter().enumerate() {
        let suffix = &body[*after_marker..];
        if suffix.starts_with(b"--") {
            break;
        }
        let content_start = if suffix.starts_with(b"\r\n") {
            after_marker + 2
        } else if suffix.starts_with(b"\n") {
            after_marker + 1
        } else {
            *after_marker
        };
        let content_end = markers
            .get(index + 1)
            .map(|(start, _)| *start)
            .unwrap_or(body.len());
        if content_start <= content_end {
            parts.push(&body[content_start..content_end]);
        }
    }
    parts
}

fn decode_mime_body(headers: &MimeHeaders, body: &[u8]) -> Result<Vec<u8>, String> {
    match headers
        .get("content-transfer-encoding")
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("base64") => decode_base64_payload(body),
        Some("quoted-printable") => Ok(decode_quoted_printable(body)),
        _ => Ok(body.to_vec()),
    }
}

fn decode_base64_payload(raw: &[u8]) -> Result<Vec<u8>, String> {
    let values = raw
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Ok(Vec::new());
    }
    if values.len() % 4 != 0 {
        return Err("OpenPGP-Teil enthält ungültiges Base64".to_string());
    }

    let mut decoded = Vec::with_capacity(values.len() / 4 * 3);
    for (index, chunk) in values.chunks_exact(4).enumerate() {
        let first = base64_value(chunk[0])
            .ok_or_else(|| "OpenPGP-Teil enthält ungültiges Base64".to_string())?;
        let second = base64_value(chunk[1])
            .ok_or_else(|| "OpenPGP-Teil enthält ungültiges Base64".to_string())?;
        let last = index == values.len() / 4 - 1;
        if chunk[2] == b'=' {
            if !last || chunk[3] != b'=' {
                return Err("OpenPGP-Teil enthält ungültiges Base64-Padding".to_string());
            }
            decoded.push((first << 2) | (second >> 4));
            continue;
        }
        let third = base64_value(chunk[2])
            .ok_or_else(|| "OpenPGP-Teil enthält ungültiges Base64".to_string())?;
        decoded.push((first << 2) | (second >> 4));
        decoded.push((second << 4) | (third >> 2));
        if chunk[3] == b'=' {
            if !last {
                return Err("OpenPGP-Teil enthält ungültiges Base64-Padding".to_string());
            }
            continue;
        }
        let fourth = base64_value(chunk[3])
            .ok_or_else(|| "OpenPGP-Teil enthält ungültiges Base64".to_string())?;
        decoded.push((third << 6) | fourth);
    }
    Ok(decoded)
}

fn base64_value(value: u8) -> Option<u8> {
    match value {
        b'A'..=b'Z' => Some(value - b'A'),
        b'a'..=b'z' => Some(value - b'a' + 26),
        b'0'..=b'9' => Some(value - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn decode_quoted_printable(raw: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'=' {
            if raw.get(index + 1) == Some(&b'\r') && raw.get(index + 2) == Some(&b'\n') {
                index += 3;
                continue;
            }
            if raw.get(index + 1) == Some(&b'\n') {
                index += 2;
                continue;
            }
            if let (Some(high), Some(low)) = (raw.get(index + 1), raw.get(index + 2))
                && let (Some(high), Some(low)) = (hex_value(*high), hex_value(*low))
            {
                output.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        output.push(raw[index]);
        index += 1;
    }
    output
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct GpgStatusInfo {
    valid_signature: bool,
    bad_signature: bool,
    no_key: bool,
    expired_key: bool,
    signer_identity: Option<String>,
    fingerprint: Option<String>,
    trusted: bool,
    decryption_okay: bool,
    decryption_failed: bool,
    decryption_key: Option<String>,
}

#[derive(Debug)]
struct GpgVerifyResult {
    status: SignatureStatus,
    identity: Option<String>,
    fingerprint: Option<String>,
}

#[derive(Debug)]
struct GpgDecryptResult {
    success: bool,
    plaintext: Vec<u8>,
    key_fingerprint: Option<String>,
}

#[derive(Debug)]
struct OpenSslCommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug)]
struct SmimeVerifyResult {
    status: SignatureStatus,
    identity: Option<String>,
    fingerprint: Option<String>,
}

#[derive(Debug)]
struct SmimeDecryptResult {
    success: bool,
    plaintext: Vec<u8>,
}

fn parse_gpg_status(stderr: &[u8]) -> GpgStatusInfo {
    let mut status = GpgStatusInfo::default();
    for line in String::from_utf8_lossy(stderr).lines() {
        let Some(payload) = line.strip_prefix("[GNUPG:] ") else {
            continue;
        };
        let Some(code) = payload.split_whitespace().next() else {
            continue;
        };
        match code {
            "GOODSIG" => {
                if let Some(rest) = payload.strip_prefix("GOODSIG ") {
                    let mut fields = rest.splitn(2, ' ');
                    let key_id = fields.next().unwrap_or_default().trim();
                    let identity = fields.next().unwrap_or_default().trim();
                    status.signer_identity = Some(if identity.is_empty() {
                        key_id.to_string()
                    } else {
                        identity.to_string()
                    });
                }
            }
            "VALIDSIG" => {
                if let Some(fingerprint) = payload.split_whitespace().nth(1) {
                    status.fingerprint = Some(fingerprint.to_ascii_uppercase());
                    status.valid_signature = true;
                }
            }
            "BADSIG" => status.bad_signature = true,
            "ERRSIG" | "NO_PUBKEY" => status.no_key = true,
            "EXPSIG" | "EXPKEYSIG" | "REVKEYSIG" => status.expired_key = true,
            "TRUST_FULLY" | "TRUST_ULTIMATE" => status.trusted = true,
            "DECRYPTION_OKAY" => status.decryption_okay = true,
            "DECRYPTION_FAILED" => status.decryption_failed = true,
            "DECRYPTION_KEY" => {
                status.decryption_key = payload
                    .split_whitespace()
                    .nth(1)
                    .map(str::to_ascii_uppercase);
            }
            _ => {}
        }
    }
    status
}

fn signature_status_from_gpg(status: &GpgStatusInfo) -> SignatureStatus {
    if status.valid_signature {
        if status.expired_key {
            SignatureStatus::ExpiredKey
        } else if status.trusted {
            SignatureStatus::ValidGood
        } else {
            SignatureStatus::ValidUntrusted
        }
    } else if status.bad_signature {
        SignatureStatus::InvalidBadSignature
    } else if status.expired_key {
        SignatureStatus::ExpiredKey
    } else {
        SignatureStatus::NoKeyAvailable
    }
}

fn run_gpg_verify(data: &[u8], signature: &[u8]) -> Result<GpgVerifyResult, String> {
    let data_path = write_secure_temp_file("verify-data", data)?;
    let signature_path = match write_secure_temp_file("verify-signature", signature) {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&data_path);
            return Err(error);
        }
    };
    let binary = std::env::var_os("MERIDIAN_GPG_BINARY").or_else(|| std::env::var_os("EVOLUTION_GPG_BINARY")).unwrap_or_else(|| "gpg".into());
    let result = Command::new(binary)
        .args(["--batch", "--no-tty", "--status-fd=2", "--verify"])
        .arg(&signature_path)
        .arg(&data_path)
        .output();
    let _ = fs::remove_file(&data_path);
    let _ = fs::remove_file(&signature_path);
    let output = result
        .map_err(|error| format!("GnuPG-Verifikation konnte nicht gestartet werden: {error}"))?;
    let status = parse_gpg_status(&output.stderr);
    Ok(GpgVerifyResult {
        status: signature_status_from_gpg(&status),
        identity: status.signer_identity,
        fingerprint: status.fingerprint,
    })
}

fn run_gpg_clear_verify(input: &[u8]) -> Result<GpgVerifyResult, String> {
    let binary = std::env::var_os("MERIDIAN_GPG_BINARY").or_else(|| std::env::var_os("EVOLUTION_GPG_BINARY")).unwrap_or_else(|| "gpg".into());
    let mut child = Command::new(binary)
        .args(["--batch", "--no-tty", "--status-fd=2", "--verify"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("GnuPG-Verifikation konnte nicht gestartet werden: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("GnuPG-Eingabe konnte nicht geschrieben werden: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("GnuPG-Prozess konnte nicht beendet werden: {error}"))?;
    let status = parse_gpg_status(&output.stderr);
    Ok(GpgVerifyResult {
        status: signature_status_from_gpg(&status),
        identity: status.signer_identity,
        fingerprint: status.fingerprint,
    })
}

fn run_gpg_decrypt(ciphertext: &[u8]) -> Result<GpgDecryptResult, String> {
    let binary = std::env::var_os("MERIDIAN_GPG_BINARY").or_else(|| std::env::var_os("EVOLUTION_GPG_BINARY")).unwrap_or_else(|| "gpg".into());
    let mut child = Command::new(binary)
        .args([
            "--batch",
            "--no-tty",
            "--status-fd=2",
            "--output=-",
            "--decrypt",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("GnuPG-Entschlüsselung konnte nicht gestartet werden: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(ciphertext)
            .map_err(|error| format!("GnuPG-Eingabe konnte nicht geschrieben werden: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("GnuPG-Prozess konnte nicht beendet werden: {error}"))?;
    let status = parse_gpg_status(&output.stderr);
    Ok(GpgDecryptResult {
        success: status.decryption_okay && !status.decryption_failed,
        plaintext: output.stdout,
        key_fingerprint: status.decryption_key,
    })
}

fn run_openssl_cms_sign(input: &[u8], options: &SmimeOptions) -> Result<Vec<u8>, String> {
    if options.signer_certificate.trim().is_empty() || options.private_key.trim().is_empty() {
        return Err(
            "S/MIME-Signatur aktiviert, aber Zertifikat oder privater Schlüssel fehlt".to_string(),
        );
    }
    let arguments = vec![
        "cms".to_string(),
        "-sign".to_string(),
        "-binary".to_string(),
        "-md".to_string(),
        "sha256".to_string(),
        "-signer".to_string(),
        options.signer_certificate.trim().to_string(),
        "-inkey".to_string(),
        options.private_key.trim().to_string(),
        "-nosmimecap".to_string(),
        "-outform".to_string(),
        "DER".to_string(),
        "-out".to_string(),
        "-".to_string(),
    ];
    let output = run_openssl_with_input(&arguments, input, "S/MIME-Signieren")?;
    if !output.success {
        return Err(format!(
            "S/MIME-Signieren fehlgeschlagen: {}",
            command_diagnostic(&output.stderr)
        ));
    }
    if output.stdout.is_empty() {
        return Err("S/MIME-Signieren lieferte keine Signatur".to_string());
    }
    Ok(output.stdout)
}

fn run_openssl_cms_encrypt(
    input: &[u8],
    recipient_certificates: &[String],
) -> Result<Vec<u8>, String> {
    if recipient_certificates.is_empty() {
        return Err(
            "S/MIME-Verschlüsselung aktiviert, aber kein Empfängerzertifikat ist konfiguriert"
                .to_string(),
        );
    }
    let mut arguments = vec![
        "cms".to_string(),
        "-encrypt".to_string(),
        "-binary".to_string(),
        "-aes-256-cbc".to_string(),
        "-outform".to_string(),
        "DER".to_string(),
        "-out".to_string(),
        "-".to_string(),
    ];
    for certificate in recipient_certificates {
        if certificate.trim().is_empty() {
            continue;
        }
        arguments.push("-recip".to_string());
        arguments.push(certificate.trim().to_string());
    }
    let output = run_openssl_with_input(&arguments, input, "S/MIME-Verschlüsseln")?;
    if !output.success {
        return Err(format!(
            "S/MIME-Verschlüsseln fehlgeschlagen: {}",
            command_diagnostic(&output.stderr)
        ));
    }
    if output.stdout.is_empty() {
        return Err("S/MIME-Verschlüsseln lieferte keine Nachricht".to_string());
    }
    Ok(output.stdout)
}

fn run_openssl_cms_verify(
    data: &[u8],
    signature: &[u8],
    ca_file: &str,
) -> Result<SmimeVerifyResult, String> {
    let with_trust = !ca_file.trim().is_empty();
    let first = run_openssl_cms_verify_once(data, signature, ca_file, !with_trust)?;
    if first.success {
        return Ok(SmimeVerifyResult {
            status: if with_trust {
                SignatureStatus::ValidGood
            } else {
                SignatureStatus::ValidUntrusted
            },
            identity: None,
            fingerprint: None,
        });
    }

    // A valid CMS signature can still fail the configured chain check.  Run
    // the cryptographic check without trust validation to distinguish that
    // case from a modified payload or invalid signature.
    if with_trust {
        let cryptographic = run_openssl_cms_verify_once(data, signature, "", true)?;
        if cryptographic.success {
            return Ok(SmimeVerifyResult {
                status: if command_mentions_expiry(&first.stderr) {
                    SignatureStatus::ExpiredKey
                } else {
                    SignatureStatus::ValidUntrusted
                },
                identity: None,
                fingerprint: None,
            });
        }
    }

    Ok(SmimeVerifyResult {
        status: if command_mentions_expiry(&first.stderr) {
            SignatureStatus::ExpiredKey
        } else {
            SignatureStatus::InvalidBadSignature
        },
        identity: None,
        fingerprint: None,
    })
}

fn run_openssl_cms_verify_once(
    data: &[u8],
    signature: &[u8],
    ca_file: &str,
    noverify: bool,
) -> Result<OpenSslCommandOutput, String> {
    let data_path = write_secure_temp_file("smime-data", data)?;
    let signature_path = match write_secure_temp_file("smime-signature", signature) {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&data_path);
            return Err(error);
        }
    };
    let mut arguments = vec![
        "cms".to_string(),
        "-verify".to_string(),
        "-binary".to_string(),
        "-inform".to_string(),
        "DER".to_string(),
        "-in".to_string(),
        signature_path.to_string_lossy().into_owned(),
        "-content".to_string(),
        data_path.to_string_lossy().into_owned(),
        "-out".to_string(),
        "-".to_string(),
        "-verify_retcode".to_string(),
    ];
    if noverify {
        arguments.push("-noverify".to_string());
    } else {
        arguments.push("-CAfile".to_string());
        arguments.push(ca_file.trim().to_string());
    }
    let result = run_openssl_command(&arguments);
    let _ = fs::remove_file(&data_path);
    let _ = fs::remove_file(&signature_path);
    result
}

fn run_openssl_cms_decrypt(
    ciphertext: &[u8],
    options: &SmimeOptions,
) -> Result<SmimeDecryptResult, String> {
    if options.signer_certificate.trim().is_empty() || options.private_key.trim().is_empty() {
        return Ok(SmimeDecryptResult {
            success: false,
            plaintext: Vec::new(),
        });
    }
    let arguments = vec![
        "cms".to_string(),
        "-decrypt".to_string(),
        "-binary".to_string(),
        "-inform".to_string(),
        "DER".to_string(),
        "-recip".to_string(),
        options.signer_certificate.trim().to_string(),
        "-inkey".to_string(),
        options.private_key.trim().to_string(),
        "-out".to_string(),
        "-".to_string(),
    ];
    let output = run_openssl_with_input(&arguments, ciphertext, "S/MIME-Entschlüsseln")?;
    Ok(SmimeDecryptResult {
        success: output.success,
        plaintext: output.stdout,
    })
}

fn run_openssl_with_input(
    arguments: &[String],
    input: &[u8],
    operation: &str,
) -> Result<OpenSslCommandOutput, String> {
    let input_path = write_secure_temp_file("smime-input", input)?;
    let mut command_arguments = arguments.to_vec();
    command_arguments.push("-in".to_string());
    command_arguments.push(input_path.to_string_lossy().into_owned());
    let result = run_openssl_command(&command_arguments)
        .map_err(|error| format!("{operation} konnte nicht ausgeführt werden: {error}"));
    let _ = fs::remove_file(&input_path);
    result
}

fn run_openssl_command(arguments: &[String]) -> Result<OpenSslCommandOutput, String> {
    let binary = std::env::var_os("MERIDIAN_OPENSSL_BINARY").or_else(|| std::env::var_os("EVOLUTION_OPENSSL_BINARY")).unwrap_or_else(|| "openssl".into());
    let output = Command::new(binary)
        .args(arguments)
        .output()
        .map_err(|error| format!("OpenSSL konnte nicht gestartet werden: {error}"))?;
    Ok(OpenSslCommandOutput {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn command_diagnostic(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or("unbekannter OpenSSL-Fehler")
        .to_string()
}

fn command_mentions_expiry(stderr: &[u8]) -> bool {
    let diagnostic = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    diagnostic.contains("expired") || diagnostic.contains("not yet valid")
}

fn write_secure_temp_file(label: &str, contents: &[u8]) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!(
        "meridian-{label}-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("Temporäre GnuPG-Datei konnte nicht erstellt werden: {error}"))?;
    if let Err(error) = file.write_all(contents) {
        let _ = fs::remove_file(&path);
        return Err(format!(
            "Temporäre GnuPG-Datei konnte nicht geschrieben werden: {error}"
        ));
    }
    Ok(path)
}

/// Protect a complete RFC822 message with OpenPGP/MIME.
///
/// The message headers remain visible while the generated MIME entity is
/// signed and/or encrypted.  When both options are enabled, signing happens
/// first and the signed entity is then encrypted, which is the normal
/// PGP/MIME composition order.
pub(crate) fn protect_rfc822(raw: &[u8], options: &OpenPgpOptions) -> Result<Vec<u8>, String> {
    if !options.sign && !options.encrypt {
        return Ok(raw.to_vec());
    }
    if options.sign && options.signing_key.trim().is_empty() {
        return Err(
            "OpenPGP-Signatur aktiviert, aber kein passender Schlüssel ist verfügbar".to_string(),
        );
    }

    let (outer_headers, mut entity) = split_rfc822_entity(raw)?;
    if options.sign {
        let signature = run_gpg_sign(&entity, options.signing_key.trim())?;
        entity = wrap_signed_entity(&entity, &signature);
    }
    if options.encrypt {
        let mut recipients = normalize_recipient_addresses(&options.recipients)?;
        if !options.signing_key.trim().is_empty()
            && !recipients
                .iter()
                .any(|recipient| recipient.eq_ignore_ascii_case(options.signing_key.trim()))
        {
            // Keep a sender copy decryptable when the configured signing key
            // is also a usable encryption key.
            recipients.push(options.signing_key.trim().to_string());
        }
        if recipients.is_empty() {
            return Err(
                "OpenPGP-Verschlüsselung aktiviert, aber kein Empfängerschlüssel gefunden"
                    .to_string(),
            );
        }
        let encrypted = run_gpg_encrypt(&entity, &recipients)?;
        entity = wrap_encrypted_entity(&encrypted);
    }

    let mut message = outer_headers;
    message.extend_from_slice(b"MIME-Version: 1.0\r\n");
    message.extend_from_slice(&entity);
    Ok(message)
}

/// Protect a complete RFC822 message with portable S/MIME/CMS.  Certificate
/// and private-key paths are configuration values only; their contents never
/// enter the application-owned settings or draft stores.
pub(crate) fn protect_smime_rfc822(raw: &[u8], options: &SmimeOptions) -> Result<Vec<u8>, String> {
    if !options.sign && !options.encrypt {
        return Ok(raw.to_vec());
    }
    if options.sign
        && (options.signer_certificate.trim().is_empty() || options.private_key.trim().is_empty())
    {
        return Err(
            "S/MIME-Signatur aktiviert, aber Zertifikat oder privater Schlüssel fehlt".to_string(),
        );
    }

    let (outer_headers, mut entity) = split_rfc822_entity(raw)?;
    if options.sign {
        let signature = run_openssl_cms_sign(&entity, options)?;
        entity = wrap_smime_signed_entity(&entity, &signature);
    }
    if options.encrypt {
        let mut recipients = options
            .recipient_certificates
            .iter()
            .filter(|certificate| !certificate.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if options.sign
            && !recipients
                .iter()
                .any(|certificate| certificate.trim() == options.signer_certificate.trim())
        {
            recipients.push(options.signer_certificate.trim().to_string());
        }
        let encrypted = run_openssl_cms_encrypt(&entity, &recipients)?;
        entity = wrap_smime_encrypted_entity(&encrypted);
    }

    let mut message = outer_headers;
    message.extend_from_slice(b"MIME-Version: 1.0\r\n");
    message.extend_from_slice(&entity);
    Ok(message)
}

fn wrap_smime_signed_entity(entity: &[u8], signature: &[u8]) -> Vec<u8> {
    let boundary = format!(
        "=_Meridian_SMIME_Signed_{}",
        uuid::Uuid::new_v4().simple()
    );
    let mut output = Vec::new();
    output.extend_from_slice(
        format!(
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg=sha-256; boundary=\"{boundary}\"\r\n\r\n"
        )
        .as_bytes(),
    );
    output.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    append_crlf_bytes(&mut output, entity);
    ensure_crlf(&mut output);
    output.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    output.extend_from_slice(
        b"Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\nContent-Description: S/MIME digital signature\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\nContent-Transfer-Encoding: base64\r\n\r\n",
    );
    append_base64_lines(&mut output, signature);
    output.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    output
}

fn wrap_smime_encrypted_entity(ciphertext: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(
        b"Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\r\nContent-Description: S/MIME encrypted message\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\n\r\n",
    );
    append_base64_lines(&mut output, ciphertext);
    output
}

fn append_base64_lines(output: &mut Vec<u8>, input: &[u8]) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in input.chunks(57) {
        if chunk.is_empty() {
            continue;
        }
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        output.push(TABLE[(first >> 2) as usize]);
        output.push(TABLE[(((first & 0x03) << 4) | second.map_or(0, |value| value >> 4)) as usize]);
        output.push(second.map_or(b'=', |value| {
            TABLE[(((value & 0x0f) << 2) | third.map_or(0, |value| value >> 6)) as usize]
        }));
        output.push(third.map_or(b'=', |value| TABLE[(value & 0x3f) as usize]));
        output.extend_from_slice(b"\r\n");
    }
}

/// Extract the visible RFC822 headers and the complete MIME entity that
/// follows them.  Content headers are kept inside the entity so a signature
/// covers the exact body type, encoding, and payload rather than only the
/// decoded text.
fn split_rfc822_entity(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (separator, separator_len) = find_header_separator(raw).ok_or_else(|| {
        "Die RFC822-Nachricht enthält keinen gültigen Headerabschluss".to_string()
    })?;
    let header_bytes = &raw[..separator];
    let body = &raw[separator + separator_len..];

    let mut outer_lines: Vec<&[u8]> = Vec::new();
    let mut entity_lines: Vec<&[u8]> = Vec::new();
    let mut current_entity_header = false;
    for line in header_bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
            let Some(separator) = line.iter().position(|byte| *byte == b':') else {
                return Err("Die RFC822-Nachricht enthält einen ungültigen Header".to_string());
            };
            let name = String::from_utf8_lossy(&line[..separator]).to_ascii_lowercase();
            if name == "mime-version" {
                current_entity_header = false;
                continue;
            }
            current_entity_header = is_mime_entity_header(&name);
        }
        if current_entity_header {
            entity_lines.push(line);
        } else {
            outer_lines.push(line);
        }
    }
    if entity_lines.is_empty() {
        return Err("Die RFC822-Nachricht enthält keinen MIME-Content-Type".to_string());
    }

    let mut outer = serialize_header_lines(&outer_lines);
    if !outer.ends_with(b"\r\n") {
        outer.extend_from_slice(b"\r\n");
    }
    let mut entity = serialize_header_lines(&entity_lines);
    entity.extend_from_slice(b"\r\n");
    entity.extend_from_slice(body);
    ensure_crlf(&mut entity);
    Ok((outer, entity))
}

fn find_header_separator(raw: &[u8]) -> Option<(usize, usize)> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4))
        .or_else(|| {
            raw.windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| (position, 2))
        })
}

fn is_mime_entity_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-transfer-encoding"
            | "content-disposition"
            | "content-description"
            | "content-id"
            | "content-location"
            | "content-base"
    )
}

fn serialize_header_lines(lines: &[&[u8]]) -> Vec<u8> {
    let mut output = Vec::new();
    for line in lines {
        output.extend_from_slice(line);
        output.extend_from_slice(b"\r\n");
    }
    output
}

fn wrap_signed_entity(entity: &[u8], signature: &[u8]) -> Vec<u8> {
    let boundary = format!(
        "=_Meridian_PGP_Signed_{}",
        uuid::Uuid::new_v4().simple()
    );
    let mut output = Vec::new();
    output.extend_from_slice(
        format!(
            "Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; micalg=pgp-sha256; boundary=\"{boundary}\"\r\n\r\n"
        )
        .as_bytes(),
    );
    output.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    append_crlf_bytes(&mut output, entity);
    ensure_crlf(&mut output);
    output.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    output.extend_from_slice(
        b"Content-Type: application/pgp-signature; name=\"signature.asc\"\r\nContent-Description: OpenPGP digital signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\n",
    );
    append_crlf_bytes(&mut output, signature);
    ensure_crlf(&mut output);
    output.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    output
}

fn wrap_encrypted_entity(ciphertext: &[u8]) -> Vec<u8> {
    let boundary = format!(
        "=_Meridian_PGP_Encrypted_{}",
        uuid::Uuid::new_v4().simple()
    );
    let mut output = Vec::new();
    output.extend_from_slice(
        format!(
            "Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"{boundary}\"\r\n\r\n"
        )
        .as_bytes(),
    );
    output.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    output.extend_from_slice(
        b"Content-Type: application/pgp-encrypted\r\nContent-Description: PGP/MIME version identification\r\nContent-Transfer-Encoding: 7bit\r\n\r\nVersion: 1\r\n",
    );
    output.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    output.extend_from_slice(
        b"Content-Type: application/octet-stream; name=\"encrypted.asc\"\r\nContent-Description: OpenPGP encrypted message\r\nContent-Disposition: inline; filename=\"encrypted.asc\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\n",
    );
    append_crlf_bytes(&mut output, ciphertext);
    ensure_crlf(&mut output);
    output.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    output
}

fn append_crlf_bytes(output: &mut Vec<u8>, input: &[u8]) {
    let mut previous_was_cr = false;
    for byte in input {
        match byte {
            b'\n' if !previous_was_cr => output.extend_from_slice(b"\r\n"),
            b'\n' => output.push(b'\n'),
            b'\r' => output.push(b'\r'),
            byte => output.push(*byte),
        }
        previous_was_cr = *byte == b'\r';
    }
}

fn ensure_crlf(value: &mut Vec<u8>) {
    if !value.ends_with(b"\r\n") {
        value.extend_from_slice(b"\r\n");
    }
}

fn run_gpg_sign(input: &[u8], signing_key: &str) -> Result<Vec<u8>, String> {
    let mut arguments = vec![
        "--batch".to_string(),
        "--no-tty".to_string(),
        "--armor".to_string(),
        "--detach-sign".to_string(),
        "--digest-algo=SHA256".to_string(),
        "--output=-".to_string(),
    ];
    arguments.push(format!("--local-user={signing_key}"));
    run_gpg(&arguments, input, "Signieren")
}

fn run_gpg_encrypt(input: &[u8], recipients: &[String]) -> Result<Vec<u8>, String> {
    let mut arguments = vec![
        "--batch".to_string(),
        "--no-tty".to_string(),
        "--armor".to_string(),
        "--encrypt".to_string(),
        "--output=-".to_string(),
    ];
    for recipient in recipients {
        arguments.push(format!("--recipient={recipient}"));
    }
    run_gpg(&arguments, input, "Verschlüsseln")
}

fn run_gpg(arguments: &[String], input: &[u8], operation: &str) -> Result<Vec<u8>, String> {
    let binary = std::env::var_os("MERIDIAN_GPG_BINARY").or_else(|| std::env::var_os("EVOLUTION_GPG_BINARY")).unwrap_or_else(|| "gpg".into());
    let mut child = Command::new(binary)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("GnuPG konnte nicht gestartet werden: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("GnuPG-Eingabe konnte nicht geschrieben werden: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("GnuPG-Prozess konnte nicht beendet werden: {error}"))?;
    if !output.status.success() {
        let detail = output
            .stderr
            .as_slice()
            .split(|byte| *byte == b'\n')
            .map(|line| String::from_utf8_lossy(line).trim().to_string())
            .filter(|line| !line.is_empty() && !line.starts_with("[GNUPG:]"))
            .last()
            .unwrap_or_else(|| "unbekannter GnuPG-Fehler".to_string());
        return Err(format!("OpenPGP-{operation} fehlgeschlagen: {detail}"));
    }
    if output.stdout.is_empty() {
        return Err(format!("OpenPGP-{operation} lieferte keine Ausgabe"));
    }
    Ok(output.stdout)
}

pub(crate) fn normalize_recipient_addresses(fields: &[String]) -> Result<Vec<String>, String> {
    let mut recipients = Vec::new();
    for field in fields {
        for token in split_address_list(field) {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let address = token
                .rsplit_once('<')
                .and_then(|(_, value)| value.strip_suffix('>'))
                .unwrap_or(token)
                .trim();
            if address.is_empty()
                || address.chars().any(|character| character.is_control())
                || (!address.contains('@') && !address.starts_with("0x"))
            {
                return Err(format!("Ungültiger OpenPGP-Empfänger: {token}"));
            }
            if !recipients
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(address))
            {
                recipients.push(address.to_string());
            }
        }
    }
    Ok(recipients)
}

/// Read public OpenPGP identities without parsing human-localized GnuPG
/// output.  Fingerprints, rather than short key IDs, are returned so the
/// selected signing key cannot become ambiguous after a keyring change.
pub(crate) fn list_openpgp_keys() -> Result<Vec<OpenPgpKey>, String> {
    let binary = std::env::var_os("MERIDIAN_GPG_BINARY").or_else(|| std::env::var_os("EVOLUTION_GPG_BINARY")).unwrap_or_else(|| "gpg".into());
    let output = Command::new(binary)
        .args(["--batch", "--with-colons", "--list-keys"])
        .output()
        .map_err(|error| format!("GnuPG-Schlüsselbund konnte nicht gelesen werden: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("[GNUPG:]"))
            .last()
            .unwrap_or("unbekannter GnuPG-Fehler");
        return Err(format!(
            "GnuPG-Schlüsselbund konnte nicht gelesen werden: {detail}"
        ));
    }

    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(parse_openpgp_key_listing(&listing))
}

fn parse_openpgp_key_listing(listing: &str) -> Vec<OpenPgpKey> {
    let mut keys = Vec::new();
    let mut current: Option<OpenPgpKey> = None;
    for line in listing.lines() {
        let fields: Vec<_> = line.split(':').collect();
        match fields.first().copied() {
            Some("pub") => {
                if let Some(key) = current.take() {
                    if !key.fingerprint.is_empty() {
                        keys.push(key);
                    }
                }
                current = Some(OpenPgpKey {
                    fingerprint: String::new(),
                    identity: String::new(),
                });
            }
            Some("fpr") => {
                if let Some(key) = current.as_mut()
                    && key.fingerprint.is_empty()
                {
                    key.fingerprint = fields
                        .get(9)
                        .map(|value| value.to_ascii_uppercase())
                        .unwrap_or_default();
                }
            }
            Some("uid") => {
                if let Some(key) = current.as_mut()
                    && key.identity.is_empty()
                {
                    key.identity = decode_gpg_colon_field(fields.get(9).copied().unwrap_or(""));
                }
            }
            _ => {}
        }
    }
    if let Some(key) = current
        && !key.fingerprint.is_empty()
    {
        keys.push(key);
    }
    keys.sort_by_cached_key(|key| (key.identity.to_ascii_lowercase(), key.fingerprint.clone()));
    keys.dedup_by(|left, right| left.fingerprint == right.fingerprint);
    keys
}

fn decode_gpg_colon_field(value: &str) -> String {
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

fn split_address_list(value: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, character) in value.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ',' | ';' if !quoted => {
                result.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    result.push(&value[start..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_message_is_not_reported_as_valid() {
        let details = CryptoService::verify_signature(b"ordinary mime payload");
        assert_eq!(details.protocol, CryptoProtocol::None);
        assert_eq!(details.signature_status, SignatureStatus::None);
        assert_eq!(details.encryption_status, EncryptionStatus::NotEncrypted);
        assert!(details.signer_identity.is_none());
        assert_eq!(
            details.display_text(),
            "Keine OpenPGP-/S/MIME-Sicherung erkannt"
        );
    }

    #[test]
    fn signed_message_is_explicitly_unverifiable_until_backend_exists() {
        let details = CryptoService::verify_signature(b"-----BEGIN PGP SIGNED MESSAGE-----");
        assert_eq!(details.protocol, CryptoProtocol::OpenPgp);
        assert_eq!(details.signature_status, SignatureStatus::NoKeyAvailable);
        assert!(details.display_text().contains("nicht verifiziert"));
    }

    #[test]
    fn encrypted_message_is_not_reported_as_decrypted() {
        let details = CryptoService::verify_signature(b"-----BEGIN PGP MESSAGE-----");
        assert_eq!(details.protocol, CryptoProtocol::OpenPgp);
        assert_eq!(
            details.encryption_status,
            EncryptionStatus::EncryptedUnknownKey
        );
        assert!(
            details
                .display_text()
                .contains("Entschlüsselung nicht verfügbar")
        );
    }

    #[test]
    fn detects_smime_encryption_without_falsely_claiming_a_signature() {
        let details = CryptoService::verify_signature(
            b"Content-Type: application/pkcs7-mime; smime-type=enveloped-data\r\n",
        );
        assert_eq!(details.protocol, CryptoProtocol::Smime);
        assert_eq!(details.signature_status, SignatureStatus::None);
        assert_eq!(
            details.encryption_status,
            EncryptionStatus::EncryptedUnknownKey
        );
        assert_eq!(
            details.display_text(),
            "S/MIME: verschlüsselt, Entschlüsselung nicht verfügbar"
        );
    }

    #[test]
    fn detects_multipart_pgp_signature_case_insensitively() {
        let details = CryptoService::verify_signature(
            b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\"\r\n",
        );
        assert_eq!(details.protocol, CryptoProtocol::OpenPgp);
        assert_eq!(details.signature_status, SignatureStatus::NoKeyAvailable);
        assert_eq!(details.encryption_status, EncryptionStatus::NotEncrypted);
    }

    #[test]
    fn recipient_parser_handles_display_names_and_deduplicates() {
        let recipients = normalize_recipient_addresses(&[
            "Alice <alice@example.org>, bob@example.org".to_string(),
            "alice@example.org".to_string(),
        ])
        .expect("recipient list should parse");
        assert_eq!(recipients, vec!["alice@example.org", "bob@example.org"]);
    }

    #[test]
    fn signed_wrapper_keeps_original_mime_entity_inside_signed_part() {
        let raw = b"From: sender@example.org\r\nSubject: Test\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\nHello\r\n";
        let (outer, entity) = split_rfc822_entity(raw).expect("message should split");
        assert!(String::from_utf8_lossy(&outer).contains("Subject: Test"));
        assert!(!String::from_utf8_lossy(&outer).contains("Content-Type: text/plain"));
        let wrapped = wrap_signed_entity(&entity, b"-----BEGIN PGP SIGNATURE-----\nabc\n");
        let text = String::from_utf8(wrapped).expect("wrapper should be text");
        assert!(text.contains("multipart/signed"));
        assert!(text.contains("Content-Type: text/plain; charset=UTF-8"));
        assert!(text.contains("BEGIN PGP SIGNATURE"));
    }

    #[test]
    fn encrypted_wrapper_contains_version_and_ciphertext_parts() {
        let wrapped = wrap_encrypted_entity(b"-----BEGIN PGP MESSAGE-----\nabc\n");
        let text = String::from_utf8(wrapped).expect("wrapper should be text");
        assert!(text.contains("multipart/encrypted"));
        assert!(text.contains("Version: 1"));
        assert!(text.contains("application/octet-stream"));
        assert!(text.contains("BEGIN PGP MESSAGE"));
    }

    #[test]
    fn smime_signed_wrapper_contains_pkcs7_signature_part() {
        let entity = b"Content-Type: text/plain; charset=UTF-8\r\n\r\nHello\r\n";
        let wrapped = wrap_smime_signed_entity(entity, b"abc");
        let text = String::from_utf8(wrapped).expect("S/MIME wrapper should be text");
        assert!(text.contains("multipart/signed"));
        assert!(text.contains("protocol=\"application/pkcs7-signature\""));
        assert!(text.contains("name=\"smime.p7s\""));
        assert!(text.contains("YWJj\r\n"));
    }

    #[test]
    fn smime_encrypted_wrapper_contains_pkcs7_mime_part() {
        let wrapped = wrap_smime_encrypted_entity(b"abc");
        let text = String::from_utf8(wrapped).expect("S/MIME wrapper should be text");
        assert!(text.contains("application/pkcs7-mime; smime-type=enveloped-data"));
        assert!(text.contains("filename=\"smime.p7m\""));
        assert!(text.contains("YWJj\r\n"));
    }

    #[test]
    fn gpg_colon_identity_escapes_are_decoded() {
        assert_eq!(
            decode_gpg_colon_field("Alice\\x3a Example"),
            "Alice: Example"
        );
    }

    #[test]
    fn parses_machine_readable_gpg_key_listing() {
        let keys = parse_openpgp_key_listing(concat!(
            "pub:-: ed25519:1:ABCDEF:1:::::::\n",
            "fpr:::::::::BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB:\n",
            "uid:::::::::Alice\\x20Example\\x20<alice@example.org>:\n",
            "pub:-: ed25519:1:123456:1:::::::\n",
            "fpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:\n",
            "uid:::::::::Bob\\x20Example\\x20<bob@example.org>:\n",
        ));
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].identity, "Alice Example <alice@example.org>");
        assert_eq!(
            keys[0].fingerprint,
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
        );
        assert_eq!(keys[1].identity, "Bob Example <bob@example.org>");
    }

    #[test]
    fn multipart_parser_preserves_signed_part_bytes_and_decodes_base64() {
        let body = concat!(
            "preamble\r\n",
            "--boundary\r\n",
            "Content-Type: text/plain\r\n\r\n",
            "Hello\r\n",
            "--boundary\r\n",
            "Content-Type: application/pgp-signature\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "YWJj\r\n",
            "--boundary--\r\n",
        );
        let parts = split_multipart_parts(body.as_bytes(), "boundary");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].ends_with(b"Hello\r\n"));
        let (headers, encoded) = parse_mime_entity(parts[1]).expect("signature part parses");
        assert_eq!(
            decode_mime_body(&headers, encoded).expect("base64 decodes"),
            b"abc"
        );
    }

    #[test]
    fn gpg_status_maps_good_untrusted_and_bad_signatures() {
        let good = parse_gpg_status(
            b"[GNUPG:] GOODSIG ABCD Alice Example\n[GNUPG:] VALIDSIG AABB 2026 0 0 1 10 0\n[GNUPG:] TRUST_UNDEFINED\n",
        );
        assert_eq!(
            signature_status_from_gpg(&good),
            SignatureStatus::ValidUntrusted
        );
        assert_eq!(good.signer_identity.as_deref(), Some("Alice Example"));
        assert_eq!(good.fingerprint.as_deref(), Some("AABB"));

        let trusted =
            parse_gpg_status(b"[GNUPG:] VALIDSIG AABB 2026 0 0 1 10 0\n[GNUPG:] TRUST_FULLY\n");
        assert_eq!(
            signature_status_from_gpg(&trusted),
            SignatureStatus::ValidGood
        );

        let bad = parse_gpg_status(b"[GNUPG:] BADSIG ABCD Alice Example\n");
        assert_eq!(
            signature_status_from_gpg(&bad),
            SignatureStatus::InvalidBadSignature
        );
    }

    #[test]
    fn inspection_rejects_a_malformed_openpgp_mime_envelope() {
        let raw = b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n--b\r\n";
        let error = inspect_openpgp_mime(raw).expect_err("missing MIME parts should fail");
        assert!(error.contains("nicht genügend MIME-Teile"));
    }
}
