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
#[derive(Debug, Clone)]
pub struct MailCryptoDetails {
    pub signature_status: SignatureStatus,
    pub encryption_status: EncryptionStatus,
    pub signer_identity: Option<String>,
    pub key_fingerprint: Option<String>,
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
        let mime = String::from_utf8_lossy(mime_part_data);
        let looks_signed = mime.contains("BEGIN PGP SIGNED MESSAGE")
            || mime
                .to_ascii_lowercase()
                .contains("application/pkcs7-signature");
        let looks_encrypted = mime.contains("BEGIN PGP MESSAGE")
            || mime.to_ascii_lowercase().contains("application/pkcs7-mime");

        MailCryptoDetails {
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
        assert_eq!(details.signature_status, SignatureStatus::None);
        assert_eq!(details.encryption_status, EncryptionStatus::NotEncrypted);
        assert!(details.signer_identity.is_none());
    }

    #[test]
    fn signed_message_is_explicitly_unverifiable_until_backend_exists() {
        let details = CryptoService::verify_signature(b"-----BEGIN PGP SIGNED MESSAGE-----");
        assert_eq!(details.signature_status, SignatureStatus::NoKeyAvailable);
    }

    #[test]
    fn encrypted_message_is_not_reported_as_decrypted() {
        let details = CryptoService::verify_signature(b"-----BEGIN PGP MESSAGE-----");
        assert_eq!(
            details.encryption_status,
            EncryptionStatus::EncryptedUnknownKey
        );
    }
}
