// SPDX-License-Identifier: GPL-3.0-or-later
//
// crypto_service — OpenPGP (GnuPG) and S/MIME cipher context wrapper.
//
// Wraps CamelCipherContext for signature verification and message encryption/decryption.

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
    /// Actual OpenPGP/S-MIME verification belongs to the Camel/GnuPG backend.
    /// Until that backend is wired, an unsigned message is reported as such
    /// and a signed/encrypted message is explicitly marked as unverifiable.
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
}
