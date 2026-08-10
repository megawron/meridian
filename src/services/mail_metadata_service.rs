// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent client-side metadata for messages read from local mail stores.
//
// Camel owns the authoritative flags for synchronized accounts.  Until that
// backend is connected, this small sidecar stores only UI state and never
// modifies Evolution's mbox/Maildir files or indexes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailMessageState {
    pub is_unread: bool,
    pub is_flagged: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    version: u32,
    #[serde(default)]
    messages: HashMap<String, MailMessageState>,
}

#[derive(Debug, Clone)]
pub struct MailMetadataStore {
    path: PathBuf,
}

impl MailMetadataStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> HashMap<String, MailMessageState> {
        let Ok(raw) = fs::read(&self.path) else {
            return HashMap::new();
        };

        let Ok(file) = serde_json::from_slice::<StateFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid mail metadata file");
            return HashMap::new();
        };

        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported mail metadata version"
            );
            return HashMap::new();
        }

        file.messages
    }

    pub fn update(&self, id: &str, state: MailMessageState) -> io::Result<()> {
        let mut messages = self.load();
        messages.insert(id.to_string(), state);
        self.save(&messages)
    }

    fn save(&self, messages: &HashMap<String, MailMessageState>) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = StateFile {
            version: CURRENT_VERSION,
            messages: messages.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_are_persisted_and_reloaded() {
        let root = tempfile_dir("metadata-roundtrip");
        let path = root.join("state.json");
        let store = MailMetadataStore::new(&path);
        let state = MailMessageState {
            is_unread: false,
            is_flagged: true,
        };

        store.update("message-1", state).expect("state should save");

        let reloaded = MailMetadataStore::new(path).load();
        assert_eq!(reloaded.get("message-1"), Some(&state));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_state_is_ignored() {
        let root = tempfile_dir("metadata-corrupt");
        let path = root.join("state.json");
        fs::write(&path, b"not-json").expect("test state should write");

        assert!(MailMetadataStore::new(path).load().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    fn tempfile_dir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
