// SPDX-License-Identifier: GPL-3.0-or-later
//
// Application-owned draft persistence used until Camel/EDS Drafts are wired.
// Passwords and account secrets are intentionally never stored here.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMessage {
    pub id: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub html_body: String,
    pub updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DraftFile {
    version: u32,
    #[serde(default)]
    drafts: Vec<DraftMessage>,
}

#[derive(Debug, Clone)]
pub struct DraftStoreService {
    path: PathBuf,
}

impl DraftStoreService {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_DRAFT_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4/drafts.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<DraftMessage> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<DraftFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid draft file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported draft file version"
            );
            return Vec::new();
        }
        file.drafts
    }

    pub fn save(&self, draft: DraftMessage) -> io::Result<()> {
        let mut drafts = self.load();
        if let Some(existing) = drafts.iter_mut().find(|existing| existing.id == draft.id) {
            *existing = draft;
        } else {
            drafts.push(draft);
        }
        self.save_all(&drafts)
    }

    fn save_all(&self, drafts: &[DraftMessage]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = DraftFile {
            version: CURRENT_VERSION,
            drafts: drafts.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

impl Default for DraftStoreService {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_roundtrip_replaces_same_id() {
        let root = temporary_directory("draft-roundtrip");
        let store = DraftStoreService::new(root.join("drafts.json"));
        let draft = DraftMessage {
            id: "draft-1".to_string(),
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Subject".to_string(),
            html_body: "<p>Hello</p>".to_string(),
            updated_at: "2026-08-10T12:00:00Z".to_string(),
        };

        store.save(draft.clone()).expect("draft should persist");
        let mut updated = draft.clone();
        updated.subject = "Updated".to_string();
        store.save(updated.clone()).expect("draft should update");

        assert_eq!(store.load(), vec![updated]);
        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
