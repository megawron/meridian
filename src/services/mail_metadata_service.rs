// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent client-side metadata for messages read from local mail stores.
//
// Camel owns the authoritative state for synchronized accounts.  Until that
// backend is connected, this sidecar stores app-owned UI state and logical
// local placement/deletion without modifying Evolution's mbox/Maildir files
// or indexes.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const CURRENT_VERSION: u32 = 8;
const MIN_SUPPORTED_VERSION: u32 = 1;

// `File::lock` serializes independent processes. The process-wide guard also
// makes that guarantee explicit for independent store instances in one
// process, whose operating-system advisory lock semantics may differ.
static APP_SIDECAR_MUTATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailMessageState {
    pub is_unread: bool,
    pub is_flagged: bool,
    #[serde(default)]
    pub is_important: bool,
    #[serde(default)]
    pub is_replied: bool,
    #[serde(default)]
    pub is_forwarded: bool,
}

/// The user-visible follow-up state used by Evolution and Outlook.
///
/// This deliberately remains separate from the ordinary starred/flagged bit:
/// Evolution exposes both columns and stores follow-up through the
/// `follow-up`/`completed-on` user tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MailFollowUpState {
    #[default]
    None,
    FollowUp,
    Completed,
}

impl MailFollowUpState {
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::FollowUp,
            Self::FollowUp => Self::Completed,
            Self::Completed => Self::None,
        }
    }
}

pub const DEFAULT_FOLLOW_UP_TEXT: &str = "Follow Up";

/// The complete Evolution follow-up tag set.  The ordinary star is stored in
/// `MailMessageState::is_flagged` and must never be inferred from this data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailFollowUpMetadata {
    #[serde(default)]
    pub state: MailFollowUpState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_on: Option<String>,
}

impl Default for MailFollowUpMetadata {
    fn default() -> Self {
        Self::none()
    }
}

impl MailFollowUpMetadata {
    pub fn none() -> Self {
        Self {
            state: MailFollowUpState::None,
            text: None,
            due_by: None,
            completed_on: None,
        }
    }

    pub fn pending(text: Option<String>, due_by: Option<String>) -> Self {
        Self {
            state: MailFollowUpState::FollowUp,
            text: normalized_follow_up_text(text),
            due_by,
            completed_on: None,
        }
    }

    pub fn completed_from(previous: &Self, completed_on: String) -> Self {
        Self {
            state: MailFollowUpState::Completed,
            text: normalized_follow_up_text(previous.text.clone()),
            due_by: previous.due_by.clone(),
            completed_on: Some(completed_on),
        }
    }

    pub fn normalized(mut self) -> Self {
        match self.state {
            MailFollowUpState::None => Self::none(),
            MailFollowUpState::FollowUp => {
                self.text = normalized_follow_up_text(self.text);
                self.completed_on = None;
                self
            }
            MailFollowUpState::Completed => {
                self.text = normalized_follow_up_text(self.text);
                self
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LegacyFollowUpValue {
    Metadata(MailFollowUpMetadata),
    State(MailFollowUpState),
}

fn deserialize_follow_up<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, MailFollowUpMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let legacy = HashMap::<String, LegacyFollowUpValue>::deserialize(deserializer)?;
    Ok(legacy
        .into_iter()
        .filter_map(|(id, value)| {
            let metadata = match value {
                LegacyFollowUpValue::Metadata(metadata) => metadata.normalized(),
                LegacyFollowUpValue::State(state) => MailFollowUpMetadata {
                    state,
                    ..MailFollowUpMetadata::default()
                }
                .normalized(),
            };
            (metadata.state != MailFollowUpState::None).then_some((id, metadata))
        })
        .collect())
}

fn normalized_follow_up_text(value: Option<String>) -> Option<String> {
    let value = value.unwrap_or_else(|| DEFAULT_FOLLOW_UP_TEXT.to_string());
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_string())
}

/// Validate the portable editor input and encode it as the RFC 2822 value
/// stored by Evolution's `due-by` tag.  Empty input explicitly clears a due
/// date; imported tag strings are retained untouched until editing.
pub fn parse_follow_up_due_input(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M"))
        .map_err(|_| "Fälligkeit muss YYYY-MM-DD HH:MM entsprechen".to_string())?;
    let Some(local) = Local.from_local_datetime(&parsed).single() else {
        return Err("Fälligkeit ist wegen der Zeitzone nicht eindeutig".to_string());
    };
    Ok(Some(local.to_rfc2822()))
}

pub fn display_follow_up_due(value: Option<&str>) -> String {
    value
        .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
        .map(|value| {
            value
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
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
    #[serde(default, deserialize_with = "deserialize_follow_up")]
    follow_up: HashMap<String, MailFollowUpMetadata>,
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

    pub fn load_follow_up(&self) -> HashMap<String, MailFollowUpMetadata> {
        self.load_file().follow_up
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

        self.mutate(|file| {
            if present {
                file.local_folders.insert(folder_id.to_string());
            } else {
                file.local_folders.remove(folder_id);
            }
            Ok(((), true))
        })
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

        self.mutate(|file| {
            file.local_folders = file
                .local_folders
                .drain()
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
            Ok(((), true))
        })
    }

    pub fn remove_local_folder_tree(&self, folder_id: &str) -> io::Result<()> {
        let folder_id = folder_id.trim();
        if !is_valid_local_folder_id(folder_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid local folder id",
            ));
        }

        self.mutate(|file| {
            file.local_folders
                .retain(|candidate| !is_folder_tree_id(candidate, folder_id));
            file.folder_overrides
                .retain(|_, candidate| !is_folder_tree_id(candidate, folder_id));
            file.copies.retain(|_, folders| {
                folders.retain(|candidate| !is_folder_tree_id(candidate, folder_id));
                !folders.is_empty()
            });
            Ok(((), true))
        })
    }

    pub fn set_folder_override(&self, id: &str, folder_id: Option<&str>) -> io::Result<()> {
        self.mutate(|file| {
            if let Some(folder_id) = folder_id.filter(|folder_id| !folder_id.trim().is_empty()) {
                file.folder_overrides
                    .insert(id.to_string(), folder_id.to_string());
            } else {
                file.folder_overrides.remove(id);
            }
            Ok(((), true))
        })
    }

    pub fn set_deleted(&self, id: &str, deleted: bool) -> io::Result<()> {
        self.mutate(|file| {
            if deleted {
                file.deleted.insert(id.to_string());
            } else {
                file.deleted.remove(id);
            }
            Ok(((), true))
        })
    }

    /// Mark several messages deleted in one metadata transaction.
    ///
    /// Folder-wide operations must not rewrite the Evolution-owned mailbox,
    /// but they still need to be atomic from the UI's perspective.  Loading
    /// and saving the sidecar once also avoids turning an empty-trash action
    /// into one filesystem write per message.
    pub fn set_deleted_many<I>(&self, ids: I, deleted: bool) -> io::Result<usize>
    where
        I: IntoIterator<Item = String>,
    {
        let mut ids = ids
            .into_iter()
            .filter(|id| !id.trim().is_empty())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();

        self.mutate(|file| {
            let mut changed = 0;
            for id in ids {
                let changed_id = if deleted {
                    file.deleted.insert(id)
                } else {
                    file.deleted.remove(&id)
                };
                changed += usize::from(changed_id);
            }
            Ok((changed, changed > 0))
        })
    }

    pub fn set_copy(&self, id: &str, folder_id: &str, copied: bool) -> io::Result<()> {
        let folder_id = folder_id.trim();
        if folder_id.is_empty() {
            return Ok(());
        }
        self.mutate(|file| {
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
            Ok(((), true))
        })
    }

    pub fn set_label(&self, id: &str, label: &str, labeled: bool) -> io::Result<()> {
        let id = id.trim();
        let label = label.trim();
        if id.is_empty() || label.is_empty() {
            return Ok(());
        }

        self.mutate(|file| {
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
            Ok(((), true))
        })
    }

    pub fn set_follow_up(&self, id: &str, metadata: MailFollowUpMetadata) -> io::Result<()> {
        let id = id.trim();
        if id.is_empty() {
            return Ok(());
        }

        let metadata = metadata.normalized();
        self.mutate(|file| {
            if metadata.state == MailFollowUpState::None {
                file.follow_up.remove(id);
            } else {
                file.follow_up.insert(id.to_string(), metadata);
            }
            Ok(((), true))
        })
    }

    pub fn update(&self, id: &str, state: MailMessageState) -> io::Result<()> {
        self.mutate(|file| {
            file.messages.insert(id.to_string(), state);
            Ok(((), true))
        })
    }

    /// Serialize the full read-modify-write transaction, including across
    /// separate service instances and processes sharing this app-owned file.
    /// Keeping the lock around both operations prevents a later writer from
    /// atomically replacing a sidecar based on a stale snapshot.
    fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut StateFile) -> io::Result<(T, bool)>,
    ) -> io::Result<T> {
        let _lock = AppSidecarFileLock::acquire(&self.path)?;
        let mut file = self.load_file_for_update()?;
        let (result, changed) = mutation(&mut file)?;
        if changed {
            self.save_file(&file)?;
        }
        Ok(result)
    }

    fn load_file(&self) -> StateFile {
        let Ok(raw) = fs::read(&self.path) else {
            return empty_state_file();
        };

        self.decode_file(&raw)
    }

    /// Read state for a mutation without converting an I/O failure into an
    /// empty sidecar that could overwrite the unreadable original.
    fn load_file_for_update(&self) -> io::Result<StateFile> {
        match fs::read(&self.path) {
            Ok(raw) => Ok(self.decode_file(&raw)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(empty_state_file()),
            Err(error) => Err(error),
        }
    }

    fn decode_file(&self, raw: &[u8]) -> StateFile {
        let Ok(file) = serde_json::from_slice::<StateFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid mail metadata file");
            return empty_state_file();
        };

        if !(MIN_SUPPORTED_VERSION..=CURRENT_VERSION).contains(&file.version) {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                minimum = MIN_SUPPORTED_VERSION,
                expected = CURRENT_VERSION,
                "Ignoring unsupported mail metadata version"
            );
            return empty_state_file();
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
            follow_up: file.follow_up.clone(),
            local_folders: file.local_folders.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

fn empty_state_file() -> StateFile {
    StateFile {
        version: CURRENT_VERSION,
        ..StateFile::default()
    }
}

/// A persistent sibling lock file stays valid after `save_file` atomically
/// replaces the data file, while its OS-managed advisory lock is released if a
/// process crashes. Locking the data file itself would only protect the old
/// inode after replacement.
struct AppSidecarFileLock {
    _file: fs::File,
    _process_guard: MutexGuard<'static, ()>,
}

impl AppSidecarFileLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let process_guard = APP_SIDECAR_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let basename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mail-metadata");
        let lock_path = parent.join(format!(".{basename}.lock"));
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)?;
        file.lock()?;
        Ok(Self {
            _file: file,
            _process_guard: process_guard,
        })
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
    #[serde(default)]
    allowed_senders: HashSet<String>,
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

    pub fn is_sender_allowed(&self, sender: &str) -> bool {
        let sender = sender.trim().to_lowercase();
        if sender.is_empty() {
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
        file.allowed_senders.contains(&sender)
    }

    /// Load the approval set once for a long-lived viewer. The viewer updates
    /// its in-memory copy after successful writes, avoiding filesystem and
    /// JSON work on every message selection.
    pub(crate) fn allowed_message_ids(&self) -> HashSet<String> {
        self.load_file().allowed_message_ids
    }

    pub(crate) fn allowed_senders(&self) -> HashSet<String> {
        self.load_file().allowed_senders
    }

    pub fn set_allowed(&self, message_id: &str, allowed: bool) -> io::Result<()> {
        if message_id.trim().is_empty() {
            return Ok(());
        }
        let _lock = AppSidecarFileLock::acquire(&self.path)?;
        let mut file = self.load_file_for_update()?;
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

    pub fn set_sender_allowed(&self, sender: &str, allowed: bool) -> io::Result<()> {
        let sender = sender.trim().to_lowercase();
        if sender.is_empty() {
            return Ok(());
        }
        let _lock = AppSidecarFileLock::acquire(&self.path)?;
        let mut file = self.load_file_for_update()?;
        if allowed {
            file.allowed_senders.insert(sender);
        } else {
            file.allowed_senders.remove(&sender);
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
            .map(|raw| self.decode_file(&raw))
            .unwrap_or_else(empty_remote_content_file)
    }

    /// Preserve the permissive read defaults for viewers, but prevent a
    /// mutator from replacing an app-owned sidecar it could not read.
    fn load_file_for_update(&self) -> io::Result<RemoteContentFile> {
        match fs::read(&self.path) {
            Ok(raw) => Ok(self.decode_file(&raw)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(empty_remote_content_file())
            }
            Err(error) => Err(error),
        }
    }

    fn decode_file(&self, raw: &[u8]) -> RemoteContentFile {
        serde_json::from_slice::<RemoteContentFile>(raw)
            .ok()
            .filter(|file| file.version == REMOTE_CONTENT_VERSION)
            .unwrap_or_else(empty_remote_content_file)
    }
}

fn empty_remote_content_file() -> RemoteContentFile {
    RemoteContentFile {
        version: REMOTE_CONTENT_VERSION,
        allowed_message_ids: HashSet::new(),
        allowed_senders: HashSet::new(),
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
            is_important: false,
            is_replied: false,
            is_forwarded: false,
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
            store
                .load()
                .get("message-1")
                .map(|state| state.is_important),
            Some(false)
        );
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
                    is_important: false,
                    is_replied: false,
                    is_forwarded: false,
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
                is_important: false,
                is_replied: false,
                is_forwarded: false,
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
    fn bulk_deleted_state_is_saved_as_one_logical_update() {
        let root = tempfile_dir("metadata-bulk-delete");
        let store = MailMetadataStore::new(root.join("state.json"));

        assert_eq!(
            store
                .set_deleted_many(
                    vec![
                        "message-2".to_string(),
                        "message-1".to_string(),
                        "message-1".to_string()
                    ],
                    true,
                )
                .expect("bulk deletion should save"),
            2
        );
        assert!(store.load_deleted().contains("message-1"));
        assert!(store.load_deleted().contains("message-2"));
        assert_eq!(
            store
                .set_deleted_many(
                    vec!["message-1".to_string(), "message-2".to_string()],
                    false
                )
                .expect("bulk restore should save"),
            2
        );
        assert!(store.load_deleted().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn independent_metadata_stores_keep_concurrent_disjoint_updates() {
        use std::sync::{Arc, Barrier};

        let root = tempfile_dir("metadata-concurrent-writers");
        let path = root.join("state.json");
        let writer_count = 24;
        let barrier = Arc::new(Barrier::new(writer_count));
        let mut writers = Vec::new();

        for index in 0..writer_count {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            writers.push(std::thread::spawn(move || {
                let store = MailMetadataStore::new(path);
                barrier.wait();
                store
                    .update(
                        &format!("message-{index}"),
                        MailMessageState {
                            is_unread: index % 2 == 0,
                            is_flagged: index % 3 == 0,
                            is_important: index % 5 == 0,
                            is_replied: false,
                            is_forwarded: false,
                        },
                    )
                    .expect("concurrent sidecar update should save");
            }));
        }

        for writer in writers {
            writer.join().expect("concurrent writer should not panic");
        }

        let messages = MailMetadataStore::new(&path).load();
        assert_eq!(messages.len(), writer_count);
        for index in 0..writer_count {
            assert_eq!(
                messages.get(&format!("message-{index}")),
                Some(&MailMessageState {
                    is_unread: index % 2 == 0,
                    is_flagged: index % 3 == 0,
                    is_important: index % 5 == 0,
                    is_replied: false,
                    is_forwarded: false,
                })
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mutations_return_sidecar_read_errors_without_replacing_the_target() {
        let root = tempfile_dir("metadata-sidecar-read-error");
        let path = root.join("state.json");
        fs::create_dir(&path).expect("test sidecar directory should be created");
        let preserved = path.join("prior-state");
        fs::write(&preserved, b"must not be replaced")
            .expect("test sidecar contents should be created");

        let error = MailMetadataStore::new(&path)
            .set_deleted("message-1", true)
            .expect_err("a sidecar read error must be returned to the caller");
        assert_eq!(error.kind(), io::ErrorKind::IsADirectory);
        assert!(
            path.is_dir(),
            "the unreadable sidecar target must remain intact"
        );
        assert_eq!(
            fs::read(&preserved).expect("prior sidecar contents should remain readable"),
            b"must not be replaced"
        );

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
                    is_important: false,
                    is_replied: false,
                    is_forwarded: false,
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
                is_important: false,
                is_replied: false,
                is_forwarded: false,
            })
        );
        reloaded
            .set_label("message-1", "WORK", false)
            .expect("label should be removable");
        assert!(MailMetadataStore::new(path).load_labels().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_up_metadata_roundtrips_and_enum_only_json_migrates() {
        let root = tempfile_dir("metadata-follow-up");
        let path = root.join("state.json");
        let store = MailMetadataStore::new(&path);

        let pending = MailFollowUpMetadata::pending(
            Some("Reply to Alice".to_string()),
            Some("Mon, 24 Aug 2026 09:30:00 +0200".to_string()),
        );
        store
            .set_follow_up("message-1", pending.clone())
            .expect("follow-up metadata should save");
        assert_eq!(
            MailMetadataStore::new(&path)
                .load_follow_up()
                .get("message-1"),
            Some(&pending)
        );

        let completed = MailFollowUpMetadata::completed_from(
            &pending,
            "Mon, 24 Aug 2026 10:00:00 +0200".to_string(),
        );
        store
            .set_follow_up("message-1", completed.clone())
            .expect("completed metadata should save");
        assert_eq!(
            MailMetadataStore::new(&path)
                .load_follow_up()
                .get("message-1"),
            Some(&completed)
        );

        store
            .set_follow_up("message-1", MailFollowUpMetadata::none())
            .expect("follow-up state should clear");
        assert!(MailMetadataStore::new(&path).load_follow_up().is_empty());

        fs::write(
            &path,
            br#"{"version":7,"follow_up":{"legacy":"follow-up","done":"completed"}}"#,
        )
        .expect("legacy enum-only sidecar should write");
        let migrated = MailMetadataStore::new(&path).load_follow_up();
        assert_eq!(migrated["legacy"].state, MailFollowUpState::FollowUp);
        assert_eq!(
            migrated["legacy"].text.as_deref(),
            Some(DEFAULT_FOLLOW_UP_TEXT)
        );
        assert_eq!(migrated["done"].state, MailFollowUpState::Completed);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_up_due_input_is_validated_and_roundtrips_for_display() {
        let due_by = parse_follow_up_due_input("2026-08-24 09:30")
            .expect("valid editor date should parse")
            .expect("non-empty editor date should produce a tag value");
        assert_eq!(display_follow_up_due(Some(&due_by)), "2026-08-24 09:30");
        assert_eq!(parse_follow_up_due_input(" ").unwrap(), None);
        assert!(parse_follow_up_due_input("24/08/2026").is_err());
    }

    #[test]
    fn legacy_state_defaults_to_no_follow_up() {
        let root = tempfile_dir("metadata-follow-up-legacy");
        let path = root.join("state.json");
        fs::write(
            &path,
            br#"{"version":6,"messages":{"message-1":{"is_unread":true,"is_flagged":false}}}"#,
        )
        .expect("legacy state should write");

        assert!(MailMetadataStore::new(&path).load_follow_up().is_empty());
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

        // Test sender whitelisting
        assert!(!store.is_sender_allowed("friend@example.com"));
        store
            .set_sender_allowed("friend@example.com", true)
            .expect("sender approval should save");
        assert!(RemoteContentStore::new(&path).is_sender_allowed("friend@example.com"));
        assert!(RemoteContentStore::new(&path).is_sender_allowed("FRIEND@EXAMPLE.COM"));
        assert!(RemoteContentStore::new(&path).allowed_senders().contains("friend@example.com"));
        RemoteContentStore::new(&path)
            .set_sender_allowed("friend@example.com", false)
            .expect("sender approval should be revocable");
        assert!(!RemoteContentStore::new(&path).is_sender_allowed("friend@example.com"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn independent_remote_content_stores_keep_approvals_and_revocations() {
        use std::sync::{Arc, Barrier};

        let root = tempfile_dir("remote-content-concurrent-writers");
        let path = root.join("remote-content.json");
        RemoteContentStore::new(&path)
            .set_allowed("revoked-message", true)
            .expect("initial approval should save");

        let writer_count = 16;
        let barrier = Arc::new(Barrier::new(writer_count + 1));
        let mut writers = Vec::new();
        {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                RemoteContentStore::new(path)
                    .set_allowed("revoked-message", false)
                    .expect("concurrent revocation should save");
            }));
        }
        for index in 0..writer_count {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                RemoteContentStore::new(path)
                    .set_allowed(&format!("approved-message-{index}"), true)
                    .expect("concurrent approval should save");
            }));
        }

        for writer in writers {
            writer.join().expect("concurrent writer should not panic");
        }

        let reloaded = RemoteContentStore::new(&path);
        assert!(!reloaded.is_allowed("revoked-message"));
        for index in 0..writer_count {
            assert!(reloaded.is_allowed(&format!("approved-message-{index}")));
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remote_content_mutations_return_read_errors_without_replacing_the_target() {
        let root = tempfile_dir("remote-content-read-error");
        let path = root.join("remote-content.json");
        fs::create_dir(&path).expect("test sidecar directory should be created");
        let preserved = path.join("prior-state");
        fs::write(&preserved, b"must not be replaced")
            .expect("test sidecar contents should be created");

        let error = RemoteContentStore::new(&path)
            .set_allowed("message-1", true)
            .expect_err("a sidecar read error must be returned to the caller");
        assert_eq!(error.kind(), io::ErrorKind::IsADirectory);
        assert_eq!(
            fs::read(&preserved).expect("prior sidecar contents should remain readable"),
            b"must not be replaced"
        );

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
