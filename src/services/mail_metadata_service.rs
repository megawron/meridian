// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent client-side metadata for messages read from local mail stores.
//
// Camel owns the authoritative state for synchronized accounts.  Until that
// backend is connected, this sidecar stores app-owned UI state and logical
// local placement/deletion without modifying Evolution's mbox/Maildir files
// or indexes.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 5;
const MIN_SUPPORTED_VERSION: u32 = 1;

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
    #[serde(default)]
    folder_overrides: HashMap<String, String>,
    #[serde(default)]
    deleted: HashSet<String>,
    #[serde(default)]
    copies: HashMap<String, HashSet<String>>,
    #[serde(default)]
    labels: HashMap<String, HashSet<String>>,
    #[serde(default)]
    local_folders: HashSet<String>,
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
        self.load_file().messages
    }

    pub fn load_folder_overrides(&self) -> HashMap<String, String> {
        self.load_file().folder_overrides
    }

    pub fn load_deleted(&self) -> HashSet<String> {
        self.load_file().deleted
    }

    pub fn load_copies(&self) -> HashMap<String, HashSet<String>> {
        self.load_file().copies
    }

    pub fn load_labels(&self) -> HashMap<String, HashSet<String>> {
        self.load_file().labels
    }

    pub fn load_local_folders(&self) -> HashSet<String> {
        self.load_file().local_folders
    }

    pub fn set_local_folder(&self, folder_id: &str, present: bool) -> io::Result<()> {
        let folder_id = folder_id.trim();
        if !is_valid_local_folder_id(folder_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid local folder id",
            ));
        }

        let mut file = self.load_file();
        if present {
            file.local_folders.insert(folder_id.to_string());
        } else {
            file.local_folders.remove(folder_id);
        }
        self.save_file(&file)
    }

    pub fn rename_local_folder(
        &self,
        from: &str,
        to: &str,
        message_targets: &HashMap<String, String>,
    ) -> io::Result<()> {
        let from = from.trim();
        let to = to.trim();
        if !is_valid_local_folder_id(from) || !is_valid_local_folder_id(to) || from == to {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid local folder rename",
            ));
        }

        let mut file = self.load_file();
        file.local_folders = file
            .local_folders
            .into_iter()
            .map(|folder_id| rename_folder_tree_id(&folder_id, from, to))
            .collect();

        for (message_id, folder_id) in message_targets {
            file.folder_overrides
                .insert(message_id.clone(), folder_id.clone());
        }
        for folder_id in file.folder_overrides.values_mut() {
            *folder_id = rename_folder_tree_id(folder_id, from, to);
        }
        for folders in file.copies.values_mut() {
            *folders = folders
                .drain()
                .map(|folder_id| rename_folder_tree_id(&folder_id, from, to))
                .collect();
        }

        self.save_file(&file)
    }

    pub fn remove_local_folder_tree(&self, folder_id: &str) -> io::Result<()> {
        let folder_id = folder_id.trim();
        if !is_valid_local_folder_id(folder_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid local folder id",
            ));
        }

        let mut file = self.load_file();
        file.local_folders
            .retain(|candidate| !is_folder_tree_id(candidate, folder_id));
        file.folder_overrides
            .retain(|_, candidate| !is_folder_tree_id(candidate, folder_id));
        file.copies.retain(|_, folders| {
            folders.retain(|candidate| !is_folder_tree_id(candidate, folder_id));
            !folders.is_empty()
        });
        self.save_file(&file)
    }

    pub fn set_folder_override(&self, id: &str, folder_id: Option<&str>) -> io::Result<()> {
        let mut file = self.load_file();
        if let Some(folder_id) = folder_id.filter(|folder_id| !folder_id.trim().is_empty()) {
            file.folder_overrides
                .insert(id.to_string(), folder_id.to_string());
        } else {
            file.folder_overrides.remove(id);
        }
        self.save_file(&file)
    }

    pub fn set_deleted(&self, id: &str, deleted: bool) -> io::Result<()> {
        let mut file = self.load_file();
        if deleted {
            file.deleted.insert(id.to_string());
        } else {
            file.deleted.remove(id);
        }
        self.save_file(&file)
    }

    pub fn set_copy(&self, id: &str, folder_id: &str, copied: bool) -> io::Result<()> {
        let mut file = self.load_file();
        let folder_id = folder_id.trim();
        if folder_id.is_empty() {
            return Ok(());
        }
        if copied {
            file.copies
                .entry(id.to_string())
                .or_default()
                .insert(folder_id.to_string());
        } else if let Some(folders) = file.copies.get_mut(id) {
            folders.remove(folder_id);
            if folders.is_empty() {
                file.copies.remove(id);
            }
        }
        self.save_file(&file)
    }

    pub fn set_label(&self, id: &str, label: &str, labeled: bool) -> io::Result<()> {
        let id = id.trim();
        let label = label.trim();
        if id.is_empty() || label.is_empty() {
            return Ok(());
        }

        let mut file = self.load_file();
        if labeled {
            let labels = file.labels.entry(id.to_string()).or_default();
            if !labels
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(label))
            {
                labels.insert(label.to_string());
            }
        } else if let Some(labels) = file.labels.get_mut(id) {
            labels.retain(|candidate| !candidate.eq_ignore_ascii_case(label));
            if labels.is_empty() {
                file.labels.remove(id);
            }
        }
        self.save_file(&file)
    }

    pub fn update(&self, id: &str, state: MailMessageState) -> io::Result<()> {
        let mut file = self.load_file();
        file.messages.insert(id.to_string(), state);
        self.save_file(&file)
    }

    fn load_file(&self) -> StateFile {
        let Ok(raw) = fs::read(&self.path) else {
            return StateFile {
                version: CURRENT_VERSION,
                ..StateFile::default()
            };
        };

        let Ok(file) = serde_json::from_slice::<StateFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid mail metadata file");
            return StateFile {
                version: CURRENT_VERSION,
                ..StateFile::default()
            };
        };

        if !(MIN_SUPPORTED_VERSION..=CURRENT_VERSION).contains(&file.version) {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                minimum = MIN_SUPPORTED_VERSION,
                expected = CURRENT_VERSION,
                "Ignoring unsupported mail metadata version"
            );
            return StateFile {
                version: CURRENT_VERSION,
                ..StateFile::default()
            };
        }
        file
    }

    fn save_file(&self, file: &StateFile) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = StateFile {
            version: CURRENT_VERSION,
            messages: file.messages.clone(),
            folder_overrides: file.folder_overrides.clone(),
            deleted: file.deleted.clone(),
            copies: file.copies.clone(),
            labels: file.labels.clone(),
            local_folders: file.local_folders.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

fn is_valid_local_folder_id(folder_id: &str) -> bool {
    let Some(name) = folder_id.strip_prefix("custom:") else {
        return false;
    };
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return false;
    }
    name.split('/').all(|part| {
        let part = part.trim();
        !part.is_empty() && part != "." && part != ".."
    })
}

fn is_folder_tree_id(folder_id: &str, root: &str) -> bool {
    folder_id == root
        || folder_id
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn rename_folder_tree_id(folder_id: &str, from: &str, to: &str) -> String {
    if folder_id == from {
        return to.to_string();
    }
    let Some(rest) = folder_id.strip_prefix(from) else {
        return folder_id.to_string();
    };
    if !rest.starts_with('/') {
        return folder_id.to_string();
    }
    format!("{to}{rest}")
}

const REMOTE_CONTENT_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct RemoteContentFile {
    version: u32,
    #[serde(default)]
    allowed_message_ids: HashSet<String>,
}

/// Persistent per-message approval for loading remote HTML content/images.
///
/// It is deliberately separate from read/flag metadata so granting remote
/// content access can never overwrite a message's authoritative local state.
#[derive(Debug, Clone)]
pub struct RemoteContentStore {
    path: PathBuf,
}

impl RemoteContentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_REMOTE_CONTENT_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("remote-content.json"));
        Self::new(path)
    }

    pub fn is_allowed(&self, message_id: &str) -> bool {
        if message_id.trim().is_empty() {
            return false;
        }
        let Ok(raw) = fs::read(&self.path) else {
            return false;
        };
        let Ok(file) = serde_json::from_slice::<RemoteContentFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid remote content file");
            return false;
        };
        if file.version != REMOTE_CONTENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = REMOTE_CONTENT_VERSION,
                "Ignoring unsupported remote content version"
            );
            return false;
        }
        file.allowed_message_ids.contains(message_id)
    }

    pub fn set_allowed(&self, message_id: &str, allowed: bool) -> io::Result<()> {
        if message_id.trim().is_empty() {
            return Ok(());
        }
        let mut file = self.load_file();
        if allowed {
            file.allowed_message_ids.insert(message_id.to_string());
        } else {
            file.allowed_message_ids.remove(message_id);
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }

    fn load_file(&self) -> RemoteContentFile {
        fs::read(&self.path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<RemoteContentFile>(&raw).ok())
            .filter(|file| file.version == REMOTE_CONTENT_VERSION)
            .unwrap_or_else(|| RemoteContentFile {
                version: REMOTE_CONTENT_VERSION,
                allowed_message_ids: HashSet::new(),
            })
    }
}

impl Default for RemoteContentStore {
    fn default() -> Self {
        Self::from_environment()
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

    #[test]
    fn version_two_state_is_readable_after_the_copy_extension() {
        let root = tempfile_dir("metadata-version-two");
        let path = root.join("state.json");
        fs::write(
            &path,
            br#"{
                "version": 2,
                "messages": {"message-1": {"is_unread": false, "is_flagged": true}},
                "folder_overrides": {"message-1": "archive"},
                "deleted": ["message-2"]
            }"#,
        )
        .expect("legacy state should be written");

        let store = MailMetadataStore::new(&path);
        assert!(store.load_copies().is_empty());
        assert_eq!(
            store.load_folder_overrides().get("message-1"),
            Some(&"archive".to_string())
        );
        assert!(store.load_deleted().contains("message-2"));

        store
            .set_copy("message-1", "sent", true)
            .expect("legacy state should upgrade on write");
        assert!(
            MailMetadataStore::new(path)
                .load_copies()
                .get("message-1")
                .is_some_and(|folders| folders.contains("sent"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_folder_and_deleted_state_is_persisted_separately_from_flags() {
        let root = tempfile_dir("metadata-local-placement");
        let path = root.join("state.json");
        let store = MailMetadataStore::new(&path);
        store
            .update(
                "message-1",
                MailMessageState {
                    is_unread: false,
                    is_flagged: true,
                },
            )
            .expect("flag state should save");
        store
            .set_folder_override("message-1", Some("archive"))
            .expect("folder override should save");
        store
            .set_deleted("message-2", true)
            .expect("deleted state should save");

        let reloaded = MailMetadataStore::new(&path);
        assert_eq!(
            reloaded.load_folder_overrides().get("message-1"),
            Some(&"archive".to_string())
        );
        assert!(reloaded.load_deleted().contains("message-2"));
        assert_eq!(
            reloaded.load().get("message-1"),
            Some(&MailMessageState {
                is_unread: false,
                is_flagged: true,
            })
        );

        reloaded
            .set_folder_override("message-1", None)
            .expect("folder override should be removable");
        reloaded
            .set_deleted("message-2", false)
            .expect("deleted state should be removable");
        reloaded
            .set_copy("message-1", "archive", true)
            .expect("copy state should save");
        assert_eq!(
            MailMetadataStore::new(&path)
                .load_copies()
                .get("message-1")
                .and_then(|folders| folders.iter().next())
                .map(String::as_str),
            Some("archive")
        );
        reloaded
            .set_copy("message-1", "archive", false)
            .expect("copy state should be removable");
        assert!(
            MailMetadataStore::new(&path)
                .load_folder_overrides()
                .is_empty()
        );
        assert!(MailMetadataStore::new(&path).load_deleted().is_empty());
        assert!(MailMetadataStore::new(&path).load_copies().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn labels_are_persisted_case_insensitively_without_affecting_flags() {
        let root = tempfile_dir("metadata-labels");
        let path = root.join("state.json");
        let store = MailMetadataStore::new(&path);
        store
            .update(
                "message-1",
                MailMessageState {
                    is_unread: true,
                    is_flagged: false,
                },
            )
            .expect("message state should save");
        store
            .set_label("message-1", "Work", true)
            .expect("label should save");
        store
            .set_label("message-1", "work", true)
            .expect("duplicate label should save");

        let reloaded = MailMetadataStore::new(&path);
        let labels = reloaded.load_labels();
        assert_eq!(labels.get("message-1").map(|values| values.len()), Some(1));
        assert_eq!(
            reloaded.load().get("message-1"),
            Some(&MailMessageState {
                is_unread: true,
                is_flagged: false,
            })
        );
        reloaded
            .set_label("message-1", "WORK", false)
            .expect("label should be removable");
        assert!(MailMetadataStore::new(path).load_labels().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_folder_definitions_and_references_are_renamed_atomically() {
        let root = tempfile_dir("metadata-local-folders");
        let path = root.join("state.json");
        let store = MailMetadataStore::new(&path);
        store
            .set_local_folder("custom:Projects", true)
            .expect("local folder should save");
        store
            .set_folder_override("message-1", Some("custom:Projects"))
            .expect("message placement should save");
        store
            .set_copy("message-2", "custom:Projects", true)
            .expect("copy placement should save");
        store
            .set_local_folder("custom:Projects/2026", true)
            .expect("child folder should save");
        store
            .set_copy("message-4", "custom:Projects/2026", true)
            .expect("child copy placement should save");

        store
            .rename_local_folder(
                "custom:Projects",
                "custom:Archive",
                &HashMap::from([("message-3".to_string(), "custom:Archive".to_string())]),
            )
            .expect("local folder should be renamed");

        let reloaded = MailMetadataStore::new(&path);
        assert!(!reloaded.load_local_folders().contains("custom:Projects"));
        assert!(reloaded.load_local_folders().contains("custom:Archive"));
        assert!(
            reloaded
                .load_local_folders()
                .contains("custom:Archive/2026")
        );
        assert_eq!(
            reloaded.load_folder_overrides().get("message-1"),
            Some(&"custom:Archive".to_string())
        );
        assert_eq!(
            reloaded.load_folder_overrides().get("message-3"),
            Some(&"custom:Archive".to_string())
        );
        assert!(
            reloaded
                .load_copies()
                .get("message-2")
                .is_some_and(|folders| folders.contains("custom:Archive"))
        );
        assert!(
            reloaded
                .load_copies()
                .get("message-4")
                .is_some_and(|folders| folders.contains("custom:Archive/2026"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remote_content_approval_is_persisted_separately() {
        let root = tempfile_dir("remote-content");
        let path = root.join("remote-content.json");
        let store = RemoteContentStore::new(&path);
        assert!(!store.is_allowed("message-1"));
        store
            .set_allowed("message-1", true)
            .expect("remote content approval should save");
        assert!(RemoteContentStore::new(&path).is_allowed("message-1"));
        RemoteContentStore::new(&path)
            .set_allowed("message-1", false)
            .expect("remote content approval should be revocable");
        assert!(!RemoteContentStore::new(&path).is_allowed("message-1"));
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
