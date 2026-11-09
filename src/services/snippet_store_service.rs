// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable, app-owned composer snippets.
//
// Snippets are intentionally separate from full mail templates: a snippet is
// inserted at the current editor selection and never replaces the subject or
// the complete message body.  The store contains only user-authored content
// and is therefore safe to include in the app-owned backup bundle.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailSnippet {
    pub id: String,
    pub name: String,
    /// Optional short trigger/abbreviation shown to the user and reserved for
    /// a future automatic expansion action.
    #[serde(default)]
    pub trigger: String,
    pub body_html: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SnippetFile {
    version: u32,
    #[serde(default)]
    snippets: Vec<MailSnippet>,
}

#[derive(Debug, Clone)]
pub struct SnippetStore {
    path: PathBuf,
}

impl SnippetStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("MERIDIAN_SNIPPET_FILE").or_else(|| std::env::var_os("EVOLUTION_GTK4_SNIPPET_FILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("mail-snippets.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<MailSnippet> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<SnippetFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid mail snippet file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported mail snippet version"
            );
            return Vec::new();
        }
        file.snippets
    }

    pub fn upsert(&self, snippet: MailSnippet) -> io::Result<()> {
        if snippet.id.trim().is_empty() || snippet.name.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Snippet requires an ID and a name",
            ));
        }
        if snippet.name.chars().count() > 160 || snippet.trigger.chars().count() > 80 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Snippet name or trigger shortcut is too long",
            ));
        }
        let mut snippets = self.load();
        if let Some(existing) = snippets.iter_mut().find(|item| item.id == snippet.id) {
            *existing = snippet;
        } else {
            snippets.push(snippet);
        }
        snippets.sort_by_key(|snippet| snippet.name.to_ascii_lowercase());
        self.save(&snippets)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut snippets = self.load();
        let original_len = snippets.len();
        snippets.retain(|snippet| snippet.id != id);
        if snippets.len() == original_len {
            return Ok(false);
        }
        self.save(&snippets)?;
        Ok(true)
    }

    fn save(&self, snippets: &[MailSnippet]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = SnippetFile {
            version: CURRENT_VERSION,
            snippets: snippets.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

impl Default for SnippetStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_roundtrip_sorted_and_can_be_deleted() {
        let root =
            std::env::temp_dir().join(format!("meridian-snippets-{}", uuid::Uuid::new_v4()));
        let store = SnippetStore::new(root.join("snippets.json"));
        let later = MailSnippet {
            id: "snippet-2".to_string(),
            name: "Receipt".to_string(),
            trigger: "/receipt".to_string(),
            body_html: "<p>Please send the receipt.</p>".to_string(),
        };
        let earlier = MailSnippet {
            id: "snippet-1".to_string(),
            name: "Greeting".to_string(),
            trigger: "/hello".to_string(),
            body_html: "<p>Hello,</p>".to_string(),
        };
        store.upsert(later).expect("snippet should save");
        store.upsert(earlier.clone()).expect("snippet should save");

        let loaded = store.load();
        assert_eq!(loaded[0], earlier);
        assert!(store.delete("snippet-1").expect("snippet should delete"));
        assert_eq!(store.load().len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_snippet_metadata_is_rejected() {
        let store = SnippetStore::new(std::env::temp_dir().join(format!(
            "meridian-snippet-{}.json",
            uuid::Uuid::new_v4()
        )));
        let result = store.upsert(MailSnippet {
            id: "id".to_string(),
            name: " ".to_string(),
            trigger: String::new(),
            body_html: "<p>Text</p>".to_string(),
        });
        assert_eq!(
            result.expect_err("empty name must fail").kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
