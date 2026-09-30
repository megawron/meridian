// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable, non-secret signature profiles imported from Evolution.
//
// The native EDS identity remains authoritative on Linux.  This store keeps
// portable signature text and selection state without importing scripts,
// credentials, or any other profile secret.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureProfile {
    pub id: String,
    pub name: String,
    pub text: String,
    #[serde(default)]
    pub format: String,
}

impl SignatureProfile {
    /// Returns true if this signature is formatted as HTML or contains HTML markup/images.
    pub fn is_html(&self) -> bool {
        if self.format.eq_ignore_ascii_case("html")
            || self.format.eq_ignore_ascii_case("text/html")
        {
            return true;
        }
        let lower = self.text.to_ascii_lowercase();
        lower.contains("<img")
            || lower.contains("<p")
            || lower.contains("<div")
            || lower.contains("<br")
            || lower.contains("<span")
            || lower.contains("<table")
            || lower.contains("<a ")
            || lower.contains("<a\n")
            || lower.contains("<a\t")
            || lower.contains("<a\r")
            || lower.contains("<a>")
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SignatureFile {
    version: u32,
    #[serde(default)]
    active_id: Option<String>,
    #[serde(default)]
    signatures: Vec<SignatureProfile>,
}

#[derive(Debug, Clone)]
pub struct SignatureStore {
    path: PathBuf,
}

impl SignatureStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("MERIDIAN_SIGNATURE_FILE").or_else(|| std::env::var_os("EVOLUTION_GTK4_SIGNATURE_FILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("signatures.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<SignatureProfile> {
        self.load_file().signatures
    }

    pub fn active_id(&self) -> Option<String> {
        self.load_file().active_id
    }

    pub fn active(&self) -> Option<SignatureProfile> {
        let file = self.load_file();
        let active_id = file.active_id.as_deref()?;
        file.signatures
            .into_iter()
            .find(|signature| signature.id == active_id)
    }

    pub fn upsert(&self, signature: SignatureProfile) -> io::Result<()> {
        if signature.id.trim().is_empty() || signature.text.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Eine Signatur benötigt ID und Inhalt",
            ));
        }
        let mut file = self.load_file();
        if let Some(existing) = file
            .signatures
            .iter_mut()
            .find(|existing| existing.id == signature.id)
        {
            *existing = signature;
        } else {
            file.signatures.push(signature);
        }
        if file.active_id.is_none() {
            file.active_id = file
                .signatures
                .first()
                .map(|signature| signature.id.clone());
        }
        self.save_file(&file)
    }

    pub fn set_active(&self, id: Option<&str>) -> io::Result<bool> {
        let mut file = self.load_file();
        let Some(id) = id.filter(|id| !id.trim().is_empty()) else {
            file.active_id = None;
            self.save_file(&file)?;
            return Ok(true);
        };
        if !file.signatures.iter().any(|signature| signature.id == id) {
            return Ok(false);
        }
        file.active_id = Some(id.to_string());
        self.save_file(&file)?;
        Ok(true)
    }

    #[allow(dead_code)]
    pub fn remove(&self, id: &str) -> io::Result<bool> {
        let mut file = self.load_file();
        let original_len = file.signatures.len();
        file.signatures.retain(|signature| signature.id != id);
        if original_len == file.signatures.len() {
            return Ok(false);
        }
        if file.active_id.as_deref() == Some(id) {
            file.active_id = file
                .signatures
                .first()
                .map(|signature| signature.id.clone());
        }
        self.save_file(&file)?;
        Ok(true)
    }

    fn load_file(&self) -> SignatureFile {
        let Ok(raw) = fs::read(&self.path) else {
            return SignatureFile {
                version: CURRENT_VERSION,
                ..SignatureFile::default()
            };
        };
        let Ok(file) = serde_json::from_slice::<SignatureFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid signature file");
            return SignatureFile {
                version: CURRENT_VERSION,
                ..SignatureFile::default()
            };
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported signature file version"
            );
            return SignatureFile {
                version: CURRENT_VERSION,
                ..SignatureFile::default()
            };
        }
        file
    }

    fn save_file(&self, file: &SignatureFile) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = SignatureFile {
            version: CURRENT_VERSION,
            active_id: file.active_id.clone(),
            signatures: file.signatures.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

impl Default for SignatureStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_roundtrip_and_active_selection_is_persisted() {
        let root = temporary_directory("signature-roundtrip");
        let store = SignatureStore::new(root.join("signatures.json"));
        let first = SignatureProfile {
            id: "signature-one".to_string(),
            name: "Arbeit".to_string(),
            text: "Viele Grüße\nPerson".to_string(),
            format: "text/plain".to_string(),
        };
        let second = SignatureProfile {
            id: "signature-two".to_string(),
            name: "Privat".to_string(),
            text: "Beste Grüße".to_string(),
            format: "text/plain".to_string(),
        };
        store
            .upsert(first.clone())
            .expect("first signature should save");
        store
            .upsert(second.clone())
            .expect("second signature should save");
        assert_eq!(store.active(), Some(first.clone()));
        assert!(
            store
                .set_active(Some(&second.id))
                .expect("active signature should save")
        );
        assert_eq!(SignatureStore::new(store.path()).active(), Some(second));
        assert!(
            SignatureStore::new(store.path())
                .remove("signature-one")
                .expect("signature should be removable")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_signature_files_are_ignored() {
        let root = temporary_directory("signature-invalid");
        let path = root.join("signatures.json");
        fs::write(&path, b"not-json").expect("invalid fixture should be written");
        assert!(SignatureStore::new(path).load().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("meridian-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
