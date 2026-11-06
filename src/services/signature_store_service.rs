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

/// Sanitizes signature HTML by stripping intrusive default white/opaque backgrounds
/// and caret-color styles often injected when copying from WebKit previews, browsers,
/// or word processors. This ensures signatures adapt seamlessly to composer themes
/// (both light and dark mode) without leaving an opaque white rectangular block.
pub fn sanitize_signature_html(html: &str) -> String {
    static RE_BG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)\b(?:background(?:-color)?)\s*:\s*(?:rgb\(\s*255\s*,\s*255\s*,\s*255\s*\)|rgba\(\s*255\s*,\s*255\s*,\s*255\s*,\s*1(?:\.0+)?\s*\)|#(?:ffffff|fff)\b|white\b)\s*(?:!\s*important)?\s*;?"#
        ).expect("valid regex for background color")
    });
    static RE_CARET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\bcaret-color\s*:\s*[^;"]+;?"#).expect("valid regex for caret-color")
    });
    static RE_BGCOLOR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)\s*bgcolor\s*=\s*["']\s*(?:#ffffff|#fff|white|rgb\(\s*255\s*,\s*255\s*,\s*255\s*\))\s*["']"#
        ).expect("valid regex for bgcolor")
    });
    static RE_EMPTY_STYLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\s*style\s*=\s*["']\s*;?\s*["']"#).expect("valid regex for empty style")
    });
    static RE_LEADING_SEMI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(style\s*=\s*["'])\s*;\s*"#).expect("valid regex for leading semi")
    });

    let s1 = RE_BG.replace_all(html, "");
    let s2 = RE_CARET.replace_all(&s1, "");
    let s3 = RE_BGCOLOR.replace_all(&s2, "");
    let s4 = RE_EMPTY_STYLE.replace_all(&s3, "");
    let s5 = RE_LEADING_SEMI.replace_all(&s4, "$1");
    s5.to_string()
}

/// Extracts a concise single-line plain-text preview from HTML or plain signature text.
pub fn html_to_plain_preview(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => {
                in_tag = false;
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let collapsed: String = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
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
        let signatures = self.load_file().signatures;
        let mut unique: Vec<SignatureProfile> = Vec::with_capacity(signatures.len());
        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_names = std::collections::HashSet::new();
        for mut sig in signatures {
            let id = sig.id.trim().to_string();
            let name_norm = sig.name.trim().to_lowercase();
            if !id.is_empty() && seen_ids.insert(id) && seen_names.insert(name_norm) {
                sig.text = sanitize_signature_html(&sig.text);
                unique.push(sig);
            }
        }
        unique
    }

    pub fn active_id(&self) -> Option<String> {
        self.load_file().active_id
    }

    pub fn active(&self) -> Option<SignatureProfile> {
        let file = self.load_file();
        let active_id = file.active_id.as_deref()?;
        self.load()
            .into_iter()
            .find(|signature| signature.id == active_id)
    }

    /// Check if a signature with the given name already exists, optionally excluding a specific ID.
    pub fn has_name(&self, name: &str, exclude_id: Option<&str>) -> bool {
        let norm = name.trim().to_lowercase();
        if norm.is_empty() {
            return false;
        }
        self.load().iter().any(|sig| {
            if let Some(ex_id) = exclude_id {
                if sig.id == ex_id {
                    return false;
                }
            }
            sig.name.trim().to_lowercase() == norm
        })
    }

    pub fn upsert(&self, mut signature: SignatureProfile) -> io::Result<()> {
        signature.text = sanitize_signature_html(&signature.text);
        if signature.id.trim().is_empty() || signature.text.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Eine Signatur benötigt ID und Inhalt",
            ));
        }
        let norm_name = signature.name.trim().to_lowercase();
        let mut file = self.load_file();

        // Check if an existing signature matches by ID OR by name
        if let Some(existing) = file.signatures.iter_mut().find(|existing| {
            existing.id == signature.id || existing.name.trim().to_lowercase() == norm_name
        }) {
            let orig_id = existing.id.clone();
            *existing = signature;
            // Preserve the original ID so existing associations don't break
            existing.id = orig_id;
        } else {
            file.signatures.push(signature);
        }

        // Deduplicate entire list by ID and normalized name
        let mut unique: Vec<SignatureProfile> = Vec::with_capacity(file.signatures.len());
        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_names = std::collections::HashSet::new();
        for sig in file.signatures {
            let id = sig.id.trim().to_string();
            let name_norm = sig.name.trim().to_lowercase();
            if !id.is_empty() && seen_ids.insert(id) && seen_names.insert(name_norm) {
                unique.push(sig);
            }
        }
        file.signatures = unique;

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

    #[test]
    fn signature_name_uniqueness_and_deduplication() {
        let root = temporary_directory("signature-uniqueness");
        let store = SignatureStore::new(root.join("signatures.json"));

        let initial = SignatureProfile {
            id: "sig-1".to_string(),
            name: "Standard".to_string(),
            text: "<p>Original Text</p>".to_string(),
            format: "html".to_string(),
        };
        store.upsert(initial).expect("must save initial");

        assert!(store.has_name("Standard", None));
        assert!(store.has_name("standard", None));
        assert!(!store.has_name("Standard", Some("sig-1")));
        assert!(!store.has_name("Anders", None));

        // Inserting a signature with the SAME name but a different ID updates existing and avoids duplicate
        let duplicate_name = SignatureProfile {
            id: "sig-2-new-id".to_string(),
            name: " standard ".to_string(),
            text: "<p>Updated Text</p>".to_string(),
            format: "html".to_string(),
        };
        store.upsert(duplicate_name).expect("must upsert with same name");

        let loaded = store.load();
        assert_eq!(loaded.len(), 1, "Must have exactly 1 signature (no duplicates)");
        assert_eq!(loaded[0].id, "sig-1", "Original ID must be preserved");
        assert_eq!(loaded[0].text, "<p>Updated Text</p>");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sanitize_signature_html_strips_white_backgrounds_and_caret_color() {
        let input = r#"<p class="MsoNormal" style="max-width: 100%; margin: 0cm; font-family: Aptos, sans-serif; caret-color: rgb(46, 52, 54); color: rgb(46, 52, 54); background-color: rgb(255, 255, 255); min-width: 0px !important;">Martin</p>"#;
        let sanitized = sanitize_signature_html(input);
        assert!(!sanitized.contains("background-color"));
        assert!(!sanitized.contains("caret-color"));
        assert!(sanitized.contains("font-family: Aptos"));
        assert!(sanitized.contains("Martin"));

        let input_hex = r#"<div style="background-color: #ffffff; color: red;">Test</div>"#;
        assert_eq!(sanitize_signature_html(input_hex), r#"<div style=" color: red;">Test</div>"#);

        let input_short_hex = r#"<div style="background-color: #fff">Short</div>"#;
        assert_eq!(sanitize_signature_html(input_short_hex), r#"<div>Short</div>"#);

        let input_white_keyword = r#"<div style="background: white !important;">Keyword</div>"#;
        assert_eq!(sanitize_signature_html(input_white_keyword), r#"<div>Keyword</div>"#);

        let input_bgcolor = r##"<table bgcolor="#ffffff"><tr><td bgcolor="white">Cell</td></tr></table>"##;
        assert_eq!(sanitize_signature_html(input_bgcolor), r#"<table><tr><td>Cell</td></tr></table>"#);

        // Keep non-white backgrounds and background-image
        let input_keep = r#"<p style="background-color: yellow; background-image: url('pic.png');">Keep</p>"#;
        let sanitized_keep = sanitize_signature_html(input_keep);
        assert!(sanitized_keep.contains("background-color: yellow"));
        assert!(sanitized_keep.contains("background-image: url('pic.png')"));
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("meridian-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
