// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailStoreService — local mail ingestion with sidecar UI metadata.
//
// The first local backend deliberately does not create or modify Evolution
// mail data.  Read/flag state is kept in an application-owned sidecar file.
// It discovers mail that Evolution already owns and supports the three
// formats encountered in local Evolution installations:
//
//   * RFC 822 / EML files
//   * Maildir (cur/, new/ and tmp/)
//   * Unix mbox files used by Evolution's local provider
//
// Network synchronization and flags stored in Camel indexes remain separate
// backend work.  Local move/archive/delete actions use only the app-owned
// sidecar and never rewrite these source files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::models::mail_message::{AttachmentInfo, MailMessageObject};
use crate::services::crypto_service::{CryptoProtocol, CryptoService};
use crate::services::html_sanitizer::html_to_plain_text;
use crate::services::mail_filter_service::{
    FilterAction, FilterMessageContext, FilterScope, MailFilterRule, MailFilterStore,
};
use crate::services::mail_metadata_service::{
    MailFollowUpMetadata, MailFollowUpState, MailMessageState, MailMetadataStore,
};
use crate::services::mail_thread_service::{
    ThreadIdentity, extract_message_ids, group_messages, normalize_message_id,
};

/// Result of applying one saved rule to the app-owned local mailbox.
///
/// The report deliberately describes only the portable sidecar operation. A
/// Camel/EDS account is never reported as changed by this path; its native
/// filter execution is covered by `ROADMAP_LINUX.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalFilterRunReport {
    pub matched_messages: usize,
    pub changed_messages: usize,
    pub moved_messages: usize,
    pub copied_messages: usize,
    pub deleted_messages: usize,
}

/// Thread-safe parser output.  GTK objects are created only after this value
/// returns to the main context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MailBackendLocation {
    pub(crate) source_uid: String,
    pub(crate) folder_name: String,
    pub(crate) message_uid: String,
    /// Stable RFC-822 identity used when a provider assigns a new UID after
    /// moving a message between folders.
    #[serde(default)]
    pub(crate) message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedMail {
    pub id: String,
    pub folder_id: String,
    pub folder_name: String,
    pub backend_location: Option<MailBackendLocation>,
    pub sender_name: String,
    pub sender_email: String,
    pub recipient_to: String,
    pub recipient_cc: String,
    pub recipient_bcc: String,
    pub reply_to: String,
    pub subject: String,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub thread_id: String,
    pub thread_size: usize,
    pub thread_position: usize,
    pub date_str: String,
    pub preview_snippet: String,
    pub html_body: String,
    pub(crate) raw_headers: Option<Vec<u8>>,
    pub(crate) raw_mime: Option<Vec<u8>>,
    pub is_unread: bool,
    pub is_flagged: bool,
    pub is_important: bool,
    pub follow_up_state: MailFollowUpState,
    #[serde(default)]
    pub follow_up: MailFollowUpMetadata,
    pub labels: Vec<String>,
    pub attachments: Vec<AttachmentInfo>,
}

impl ParsedMail {
    pub fn into_object(self) -> MailMessageObject {
        let ParsedMail {
            id,
            folder_id,
            sender_name,
            sender_email,
            recipient_to,
            recipient_cc,
            recipient_bcc,
            reply_to,
            subject,
            message_id,
            in_reply_to,
            references,
            thread_id,
            thread_size,
            thread_position,
            date_str,
            preview_snippet,
            html_body,
            raw_headers,
            raw_mime,
            is_unread,
            is_flagged,
            is_important,
            follow_up_state,
            follow_up,
            labels,
            backend_location,
            attachments,
            ..
        } = self;

        let object = MailMessageObject::new(
            &id,
            &folder_id,
            &sender_name,
            &sender_email,
            &recipient_to,
            &recipient_cc,
            &recipient_bcc,
            &reply_to,
            &subject,
            message_id,
            in_reply_to,
            references,
            &thread_id,
            thread_size,
            thread_position,
            &date_str,
            &preview_snippet,
            &html_body,
            is_unread,
            is_flagged,
            is_important,
            follow_up_state,
            follow_up,
            labels,
            backend_location,
            attachments,
        );
        object.set_raw_headers(raw_headers);
        object.set_raw_mime(raw_mime);
        object
    }
}

/* Keep the destructuring above explicit in the GTK conversion while preserving
 * backend location metadata for remote actions in the legacy object. */
impl ParsedMail {
    #[allow(dead_code)]
    pub(crate) fn backend_location(&self) -> Option<&MailBackendLocation> {
        self.backend_location.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct MailStoreService {
    /// Root directory used for local mail discovery.
    ///
    /// `mail_dir` remains public so callers and tests can point the service at
    /// an explicit mailbox without relying on environment variables.
    pub mail_dir: PathBuf,
    metadata: MailMetadataStore,
    writable: bool,
}

/// Plain, owned inputs needed to construct a local store on a worker.
///
/// Environment values are captured together with the settings value before a
/// worker starts.  Keeping the raw overrides here (including an explicitly
/// empty value) preserves the constructor's existing precedence rules without
/// making the worker consult process-global state again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MailStoreWorkerConfig {
    pub(crate) mail_dir_override: Option<PathBuf>,
    pub(crate) local_mail_directory: Option<String>,
    pub(crate) metadata_path_override: Option<PathBuf>,
    pub(crate) app_data_directory: PathBuf,
}

impl MailStoreWorkerConfig {
    pub(crate) fn capture(local_mail_directory: Option<String>) -> Self {
        Self {
            mail_dir_override: std::env::var_os("EVOLUTION_GTK4_MAIL_DIR").map(PathBuf::from),
            local_mail_directory,
            metadata_path_override: std::env::var_os("EVOLUTION_GTK4_MAIL_STATE_FILE")
                .map(PathBuf::from),
            app_data_directory: crate::services::app_data_dir(),
        }
    }
}

impl MailStoreService {
    /// Create a service pointing at the configured local store.
    ///
    /// Existing Evolution directories and an explicit external setting are
    /// read-only sources.  User-initiated imports use [`Self::import_destination`]
    /// so the original store is never modified.  No directory is created and
    /// no sample data is written.  The optional `EVOLUTION_GTK4_MAIL_DIR`
    /// override is useful for tests and for inspecting another mailbox.
    pub fn new() -> Self {
        let mail_dir_override = std::env::var_os("EVOLUTION_GTK4_MAIL_DIR").map(PathBuf::from);
        let local_mail_directory = if mail_dir_override.is_none() {
            crate::services::settings_service::AppSettings::try_new()
                .map(|settings| settings.local_mail_directory())
        } else {
            None
        };
        let config = MailStoreWorkerConfig {
            mail_dir_override,
            local_mail_directory,
            metadata_path_override: std::env::var_os("EVOLUTION_GTK4_MAIL_STATE_FILE")
                .map(PathBuf::from),
            app_data_directory: crate::services::app_data_dir(),
        };
        Self::new_worker_safe(config)
    }

    /// Construct a service from settings already captured before entering a
    /// worker.  This path is deliberately limited to environment and plain
    /// filesystem policy; it must never resolve GSettings or GTK state.
    pub(crate) fn new_worker_safe(config: MailStoreWorkerConfig) -> Self {
        Self::new_worker_safe_with_paths(
            config.mail_dir_override,
            config.local_mail_directory,
            config.metadata_path_override,
            config.app_data_directory,
        )
    }

    fn new_worker_safe_with_paths(
        mail_dir_override: Option<PathBuf>,
        local_mail_directory: Option<String>,
        metadata_path_override: Option<PathBuf>,
        app_data_directory: PathBuf,
    ) -> Self {
        let (mail_dir, writable) = if let Some(path) = mail_dir_override {
            (path, false)
        } else if let Some(path) = local_mail_directory
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
        {
            let writable = path.starts_with(&app_data_directory);
            (path, writable)
        } else {
            // With no installed schema there is no persistent external-path
            // setting. Keep the development fallback app-owned so imports do
            // not silently target the user's Evolution directory.
            (app_data_directory.join("mail"), true)
        };
        let metadata_path =
            metadata_path_override.unwrap_or_else(|| app_data_directory.join("mail-state.json"));

        Self {
            mail_dir,
            metadata: MailMetadataStore::new(metadata_path),
            writable,
        }
    }

    /// Construct a service for an explicit mailbox root.
    #[allow(dead_code)]
    pub fn with_mail_dir<P: Into<PathBuf>>(mail_dir: P) -> Self {
        let mail_dir = mail_dir.into();
        let metadata_path = mail_dir.join(".evolution-gtk4-state.json");
        Self {
            mail_dir,
            metadata: MailMetadataStore::new(metadata_path),
            writable: true,
        }
    }

    /// Construct a local store with an explicit metadata path, useful for
    /// imports and tests that must keep the mailbox and client state apart.
    #[allow(dead_code)]
    pub fn with_mail_dir_and_state_path(
        mail_dir: impl Into<PathBuf>,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            mail_dir: mail_dir.into(),
            metadata: MailMetadataStore::new(state_path),
            writable: true,
        }
    }

    #[cfg(test)]
    fn with_read_only_mail_dir(mail_dir: impl Into<PathBuf>) -> Self {
        let mail_dir = mail_dir.into();
        Self {
            metadata: MailMetadataStore::new(mail_dir.join(".evolution-gtk4-state.json")),
            mail_dir,
            writable: false,
        }
    }

    /// Return the safe app-owned destination for a user-initiated import.
    /// Existing Evolution stores are read-only sources and therefore redirect
    /// imports into the app's local mail tree.
    pub fn import_destination(&self) -> PathBuf {
        if self.writable {
            self.mail_dir.clone()
        } else {
            crate::services::app_data_dir().join("mail")
        }
    }

    /// Copy user-selected EML, Maildir files or mbox files into the configured
    /// writable destination.  The importer never overwrites an existing file;
    /// a numbered name is chosen instead so an import can be repeated safely.
    pub fn import_files(&self, files: &[PathBuf]) -> std::io::Result<usize> {
        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "configured local mail store is read-only",
            ));
        }
        fs::create_dir_all(&self.mail_dir)?;
        let mut sources = Vec::new();
        for source in files {
            if source.is_dir() {
                let mut paths = Vec::new();
                collect_import_files(source, &mut paths);
                sources.extend(paths.into_iter().map(|path| (path, source.clone())));
            } else if source.is_file() {
                sources.push((source.clone(), import_root_for_file(source)));
            }
        }
        self.copy_import_sources(sources)
    }

    /// Copy imported files while preserving their relative Evolution mailbox
    /// layout.  In particular, `mail/local/.Projects/cur/message` remains in
    /// `.Projects/cur` so the local folder can be reconstructed after the
    /// profile migration.  The source is never modified and destinations are
    /// still non-overwriting.
    pub fn import_files_from_root(
        &self,
        source_root: impl AsRef<Path>,
        files: &[PathBuf],
    ) -> std::io::Result<usize> {
        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "configured local mail store is read-only",
            ));
        }
        fs::create_dir_all(&self.mail_dir)?;
        let source_root = source_root.as_ref().to_path_buf();
        let mut sources = files
            .iter()
            .filter(|source| source.is_file())
            .cloned()
            .map(|source| (source, source_root.clone()))
            .collect::<Vec<_>>();
        sources.sort_by(|left, right| left.0.cmp(&right.0));
        sources.dedup_by(|left, right| left.0 == right.0);
        self.copy_import_sources(sources)
    }

    /// Register local folders discovered in an Evolution mailbox layout.
    /// Empty folders are represented only by their mailbox path, so they need
    /// a sidecar definition even when no message file can be copied.
    pub(crate) fn register_folder_layout_from_root(
        &self,
        source_root: impl AsRef<Path>,
        paths: &[PathBuf],
    ) -> std::io::Result<usize> {
        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "configured local mail store is read-only",
            ));
        }
        fs::create_dir_all(&self.mail_dir)?;
        let source_root = source_root.as_ref();
        let mut folder_ids = HashSet::new();
        for source in paths {
            let relative = relative_import_path(source, source_root);
            if relative.as_os_str().is_empty() {
                continue;
            }
            let destination = self.mail_dir.join(relative);
            let folder_id = folder_id_for_path_with_root(&destination, Some(&self.mail_dir));
            if folder_id.starts_with("custom:") {
                folder_ids.insert(folder_id);
            }
        }

        let imported = folder_ids.len();
        for folder_id in folder_ids {
            self.metadata.set_local_folder(&folder_id, true)?;
        }
        Ok(imported)
    }

    fn copy_import_sources(&self, mut sources: Vec<(PathBuf, PathBuf)>) -> std::io::Result<usize> {
        sources.sort_by(|left, right| left.0.cmp(&right.0));
        sources.dedup_by(|left, right| left.0 == right.0);

        let mut imported = 0;
        let mut imported_folders = HashSet::new();
        for (source, source_root) in sources {
            let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let relative = relative_import_path(&source, &source_root);
            let target_root = relative
                .parent()
                .map(|parent| self.mail_dir.join(parent))
                .unwrap_or_else(|| self.mail_dir.clone());
            fs::create_dir_all(&target_root)?;
            let mut destination = self.mail_dir.join(&relative);
            let stem = Path::new(file_name)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("import");
            let extension = Path::new(file_name)
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!(".{value}"))
                .unwrap_or_default();
            let mut suffix = 1;
            while destination.exists() {
                destination = target_root.join(format!("{stem}-{suffix}{extension}"));
                suffix += 1;
            }
            if source == destination {
                continue;
            }
            fs::copy(source, &destination)?;
            let folder_id = folder_id_for_path_with_root(&destination, Some(&self.mail_dir));
            if folder_id.starts_with("custom:") {
                imported_folders.insert(folder_id);
            }
            imported += 1;
        }

        for folder_id in imported_folders {
            self.metadata.set_local_folder(&folder_id, true)?;
        }
        Ok(imported)
    }

    /// Return explicitly created, empty-capable local folders from the
    /// app-owned sidecar.  Message-derived folders are added by the caller
    /// while loading records, so this method never rescans the mailbox.
    pub(crate) fn local_folder_definitions(&self) -> HashSet<String> {
        let mut folders = self.metadata.load_local_folders();
        expand_local_folder_ancestors(&mut folders);
        folders
    }

    /// Create a logical local folder without changing the imported mailbox.
    pub fn create_local_folder(&self, name: &str) -> std::io::Result<String> {
        let folder_id = normalize_custom_folder_id(name)?;
        if self
            .discovered_local_folder_ids()
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&folder_id))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "local folder already exists",
            ));
        }
        self.metadata.set_local_folder(&folder_id, true)?;
        Ok(folder_id)
    }

    /// Rename a logical local folder and persist message placement changes in
    /// the sidecar.  Imported EML, mbox and Maildir files remain untouched.
    pub fn rename_local_folder(&self, from: &str, name: &str) -> std::io::Result<String> {
        let from = normalize_custom_folder_id(from)?;
        let to = normalize_custom_folder_id(name)?;
        if from == to || is_folder_tree_id(&to, &from) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "local folder name did not change",
            ));
        }

        let available = self.discovered_local_folder_ids();
        if !available.iter().any(|candidate| {
            candidate.eq_ignore_ascii_case(&from)
                || is_folder_tree_id_case_insensitive(candidate, &from)
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "local folder does not exist",
            ));
        }
        if available.iter().any(|candidate| {
            !is_folder_tree_id_case_insensitive(candidate, &from)
                && is_folder_tree_id_case_insensitive(candidate, &to)
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "local folder already exists",
            ));
        }

        let overrides = self.metadata.load_folder_overrides();
        let cancelled = AtomicBool::new(false);
        let message_targets = self
            .load_raw_local_records_cancelable(&cancelled)
            .into_iter()
            .filter(|message| {
                let folder_id = overrides
                    .get(&message.id)
                    .map(String::as_str)
                    .unwrap_or(message.folder_id.as_str());
                is_folder_tree_id_case_insensitive(folder_id, &from)
            })
            .map(|message| {
                let folder_id = overrides
                    .get(&message.id)
                    .map(String::as_str)
                    .unwrap_or(message.folder_id.as_str());
                (
                    message.id,
                    rename_folder_tree_id_case_insensitive(folder_id, &from, &to),
                )
            })
            .collect::<HashMap<_, _>>();

        self.metadata
            .rename_local_folder(&from, &to, &message_targets)?;
        Ok(to)
    }

    /// Delete an empty logical local folder.  Non-empty folders are rejected
    /// so a mistaken click cannot hide imported messages.
    pub fn delete_local_folder(&self, folder_id: &str) -> std::io::Result<()> {
        let folder_id = normalize_custom_folder_id(folder_id)?;
        let available = self.discovered_local_folder_ids();
        if !available
            .iter()
            .any(|candidate| is_folder_tree_id_case_insensitive(candidate, &folder_id))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "local folder does not exist",
            ));
        }

        let overrides = self.metadata.load_folder_overrides();
        let cancelled = AtomicBool::new(false);
        let has_messages = self
            .load_raw_local_records_cancelable(&cancelled)
            .into_iter()
            .any(|message| {
                let folder = overrides
                    .get(&message.id)
                    .map(String::as_str)
                    .unwrap_or(message.folder_id.as_str());
                is_folder_tree_id_case_insensitive(folder, &folder_id)
            });
        let has_copies = self.metadata.load_copies().values().any(|folders| {
            folders
                .iter()
                .any(|candidate| is_folder_tree_id_case_insensitive(candidate, &folder_id))
        });
        if has_messages || has_copies {
            return Err(std::io::Error::new(
                std::io::ErrorKind::DirectoryNotEmpty,
                "local folder is not empty",
            ));
        }

        self.metadata.remove_local_folder_tree(&folder_id)
    }

    /// Empty every local logical trash entry without modifying Evolution's
    /// source mailbox.  The source files remain available for the original
    /// Evolution installation; deletion is represented only in the app-owned
    /// metadata sidecar, just like the individual local delete command.
    pub fn empty_local_trash(&self) -> std::io::Result<usize> {
        let cancelled = AtomicBool::new(false);
        let overrides = self.metadata.load_folder_overrides();
        let already_deleted = self.metadata.load_deleted();
        let copies = self.metadata.load_copies();
        let messages = self.load_raw_local_records_cancelable(&cancelled);
        let mut ids = Vec::new();

        for message in messages {
            let folder_id = overrides
                .get(&message.id)
                .map(String::as_str)
                .unwrap_or(message.folder_id.as_str());
            if is_local_trash_folder(folder_id) && !already_deleted.contains(&message.id) {
                ids.push(message.id);
            }
        }

        for (message_id, folder_ids) in copies {
            for folder_id in folder_ids {
                if !is_local_trash_folder(&folder_id) {
                    continue;
                }
                let copy_id = local_copy_id(&message_id, &folder_id);
                if !already_deleted.contains(&copy_id) {
                    ids.push(copy_id);
                }
            }
        }

        self.metadata.set_deleted_many(ids, true)
    }

    /// Mark all messages in the local junk/spam folder as deleted in the app sidecar.
    pub fn empty_local_junk(&self) -> std::io::Result<usize> {
        let cancelled = AtomicBool::new(false);
        let overrides = self.metadata.load_folder_overrides();
        let already_deleted = self.metadata.load_deleted();
        let copies = self.metadata.load_copies();
        let messages = self.load_raw_local_records_cancelable(&cancelled);
        let mut ids = Vec::new();

        for message in messages {
            let folder_id = overrides
                .get(&message.id)
                .map(String::as_str)
                .unwrap_or(message.folder_id.as_str());
            if (folder_id == "junk" || folder_id == "spam" || folder_id.starts_with("junk/"))
                && !already_deleted.contains(&message.id)
            {
                ids.push(message.id);
            }
        }

        for (message_id, folder_ids) in copies {
            for folder_id in folder_ids {
                if folder_id != "junk" && folder_id != "spam" && !folder_id.starts_with("junk/") {
                    continue;
                }
                let copy_id = local_copy_id(&message_id, &folder_id);
                if !already_deleted.contains(&copy_id) {
                    ids.push(copy_id);
                }
            }
        }

        self.metadata.set_deleted_many(ids, true)
    }

    /// Read all discoverable local messages in a deterministic order.
    ///
    /// This method is synchronous by design so that it can also be used by
    /// importers and tests.  The GTK caller must move it off the main thread
    /// once large local stores are supported; the asynchronous UI pipeline is
    /// tracked separately in the roadmap.
    #[allow(dead_code)]
    pub fn load_local_messages(&self) -> Vec<MailMessageObject> {
        self.load_local_records()
            .into_iter()
            .map(ParsedMail::into_object)
            .collect()
    }

    /// Parse local mail without constructing GTK objects.
    ///
    /// This is the method intended for worker threads.  Its return type is
    /// made only of owned, thread-safe Rust data.
    pub fn load_local_records(&self) -> Vec<ParsedMail> {
        let cancelled = AtomicBool::new(false);
        self.load_local_records_cancelable(&cancelled)
    }

    /// Load local messages while allowing a caller to stop a worker that is
    /// no longer needed.  The parser returns records collected before the
    /// cancellation was observed and never mutates the source mailbox.
    pub fn load_local_records_cancelable(&self, cancelled: &AtomicBool) -> Vec<ParsedMail> {
        let filter_store = MailFilterStore::default();
        self.load_local_records_cancelable_with_store(cancelled, &filter_store)
    }

    /// Load local messages with an explicitly captured filter store.
    ///
    /// This is the worker-safe variant: the caller owns the filter-store path
    /// and no environment, app-data, settings, GTK, or GObject state is
    /// resolved here.  The explicit store is the complete source of incoming
    /// filter rules; there is no fallback to process-global filter
    /// configuration.
    pub fn load_local_records_cancelable_with_store(
        &self,
        cancelled: &AtomicBool,
        filter_store: &MailFilterStore,
    ) -> Vec<ParsedMail> {
        let mut metadata = self.metadata.load();
        let folder_overrides = self.metadata.load_folder_overrides();
        let deleted = self.metadata.load_deleted();
        let labels = self.metadata.load_labels();
        let follow_up = self.metadata.load_follow_up();
        let filter_rules = filter_store.load();
        let mut messages = self.load_raw_local_records_cancelable(cancelled);

        messages.retain(|message| !deleted.contains(&message.id));
        let mut filtered_out = std::collections::HashSet::new();
        for message in &mut messages {
            if let Some(folder_id) = folder_overrides.get(&message.id) {
                message.folder_id = folder_id.clone();
            }
            if let Some(message_labels) = labels.get(&message.id) {
                message.labels = sorted_labels(message_labels);
            }
            if let Some(state) = follow_up.get(&message.id) {
                message.follow_up_state = state.state;
                message.follow_up = state.clone();
            }
            let effects =
                apply_client_state(&metadata, &filter_rules, message, Some(&self.metadata));
            if effects.state_changed {
                let state = MailMessageState {
                    is_unread: message.is_unread,
                    is_flagged: message.is_flagged,
                    is_important: message.is_important,
                };
                if let Err(error) = self.metadata.update(&message.id, state) {
                    tracing::warn!(
                        %error,
                        message_id = %message.id,
                        "Could not persist local filter message state"
                    );
                } else {
                    metadata.insert(message.id.clone(), state);
                }
            }
            if effects.deleted {
                filtered_out.insert(message.id.clone());
            }
        }
        messages.retain(|message| !filtered_out.contains(&message.id));

        // Filter actions may have created a new sidecar copy while processing
        // the records above.  Reload this small map so the same refresh shows
        // the logical copy immediately instead of requiring a second reload.
        let copies = self.metadata.load_copies();
        let mut copied_messages = Vec::with_capacity(messages.len());
        for message in messages.iter() {
            let Some(copy_folders) = copies.get(&message.id) else {
                continue;
            };
            let mut copy_folders = copy_folders.iter().collect::<Vec<_>>();
            copy_folders.sort();
            for folder_id in copy_folders {
                if folder_id == &message.folder_id {
                    continue;
                }
                let copy_id = local_copy_id(&message.id, folder_id);
                if deleted.contains(&copy_id) {
                    continue;
                }
                let mut copy = message.clone();
                copy.id = copy_id.clone();
                copy.folder_id = folder_overrides
                    .get(&copy_id)
                    .cloned()
                    .unwrap_or_else(|| folder_id.clone());
                copy.folder_name = copy.folder_id.clone();
                copy.backend_location = None;
                if let Some(state) = metadata.get(&copy_id) {
                    copy.is_unread = state.is_unread;
                    copy.is_flagged = state.is_flagged;
                    copy.is_important = state.is_important;
                }
                if let Some(copy_labels) = labels.get(&copy_id) {
                    copy.labels = sorted_labels(copy_labels);
                }
                if let Some(state) = follow_up.get(&copy_id) {
                    copy.follow_up_state = state.state;
                    copy.follow_up = state.clone();
                }
                copied_messages.push(copy);
            }
        }
        messages.extend(copied_messages);

        assign_thread_metadata(&mut messages);

        messages
    }

    /// Apply one saved rule to the app-owned local mailbox.
    ///
    /// This is the portable equivalent of Evolution's manual filter action.
    /// It parses the source mailbox without first running incoming rules, so a
    /// manual run cannot accidentally hide its own input or apply an incoming
    /// rule twice. Source files remain read-only; all changes go through the
    /// app-owned metadata sidecar.
    pub fn run_filter_rule(&self, rule_id: &str) -> std::io::Result<Option<LocalFilterRunReport>> {
        let filter_store = MailFilterStore::default();
        self.run_filter_rule_with_store(rule_id, &filter_store)
    }

    pub(crate) fn run_filter_rule_with_store(
        &self,
        rule_id: &str,
        filter_store: &MailFilterStore,
    ) -> std::io::Result<Option<LocalFilterRunReport>> {
        let Some(rule) = filter_store
            .load()
            .into_iter()
            .find(|rule| rule.id == rule_id)
        else {
            return Ok(None);
        };

        let metadata = self.metadata.load();
        let folder_overrides = self.metadata.load_folder_overrides();
        let deleted = self.metadata.load_deleted();
        let labels = self.metadata.load_labels();
        let cancelled = AtomicBool::new(false);
        let mut messages = self.load_raw_local_records_cancelable(&cancelled);
        let mut report = LocalFilterRunReport::default();

        for message in &mut messages {
            if deleted.contains(&message.id) {
                continue;
            }
            if let Some(folder_id) = folder_overrides.get(&message.id) {
                message.folder_id = folder_id.clone();
            }
            if let Some(state) = metadata.get(&message.id) {
                message.is_unread = state.is_unread;
                message.is_flagged = state.is_flagged;
                message.is_important = state.is_important;
            }
            if let Some(message_labels) = labels.get(&message.id) {
                message.labels = sorted_labels(message_labels);
            }
            if !message_matches_rule(&rule, message) {
                continue;
            }

            report.matched_messages += 1;
            let effects = apply_filter_rule(&rule, message, Some(&self.metadata));
            if effects.state_changed {
                self.metadata.update(
                    &message.id,
                    MailMessageState {
                        is_unread: message.is_unread,
                        is_flagged: message.is_flagged,
                        is_important: message.is_important,
                    },
                )?;
            }
            report.changed_messages += usize::from(effects.changed);
            report.moved_messages += usize::from(effects.moved);
            report.copied_messages += usize::from(effects.copied);
            report.deleted_messages += usize::from(effects.deleted);
        }

        Ok(Some(report))
    }

    /// Apply all enabled saved filter rules to the app-owned local mailbox.
    pub fn run_all_active_filters(&self) -> std::io::Result<LocalFilterRunReport> {
        let filter_store = MailFilterStore::default();
        self.run_all_active_filters_with_store(&filter_store)
    }

    pub(crate) fn run_all_active_filters_with_store(
        &self,
        filter_store: &MailFilterStore,
    ) -> std::io::Result<LocalFilterRunReport> {
        let rules: Vec<_> = filter_store
            .load()
            .into_iter()
            .filter(|rule| rule.enabled)
            .collect();

        let metadata = self.metadata.load();
        let folder_overrides = self.metadata.load_folder_overrides();
        let deleted = self.metadata.load_deleted();
        let labels = self.metadata.load_labels();
        let cancelled = AtomicBool::new(false);
        let mut messages = self.load_raw_local_records_cancelable(&cancelled);
        let mut report = LocalFilterRunReport::default();

        for message in &mut messages {
            if deleted.contains(&message.id) {
                continue;
            }
            if let Some(folder_id) = folder_overrides.get(&message.id) {
                message.folder_id = folder_id.clone();
            }
            if let Some(state) = metadata.get(&message.id) {
                message.is_unread = state.is_unread;
                message.is_flagged = state.is_flagged;
                message.is_important = state.is_important;
            }
            if let Some(message_labels) = labels.get(&message.id) {
                message.labels = sorted_labels(message_labels);
            }

            for rule in &rules {
                if !message_matches_rule(rule, message) {
                    continue;
                }
                report.matched_messages += 1;
                let effects = apply_filter_rule(rule, message, Some(&self.metadata));
                if effects.state_changed {
                    self.metadata.update(
                        &message.id,
                        MailMessageState {
                            is_unread: message.is_unread,
                            is_flagged: message.is_flagged,
                            is_important: message.is_important,
                        },
                    )?;
                }
                report.changed_messages += usize::from(effects.changed);
                report.moved_messages += usize::from(effects.moved);
                report.copied_messages += usize::from(effects.copied);
                report.deleted_messages += usize::from(effects.deleted);

                if effects.stop_processing || effects.deleted || effects.moved {
                    break;
                }
            }
        }

        Ok(report)
    }

    fn load_raw_local_records_cancelable(&self, cancelled: &AtomicBool) -> Vec<ParsedMail> {
        let mut paths = Vec::new();
        collect_mail_files(&self.mail_dir, &mut paths);
        paths.sort();

        let mut messages = Vec::new();
        for path in paths {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let Ok(raw) = fs::read(&path) else {
                tracing::warn!(path = %path.display(), "Could not read local mail file");
                continue;
            };

            let text = String::from_utf8_lossy(&raw);
            let unread = maildir_message_is_unread(&path);

            if looks_like_mbox(&text) {
                messages.extend(parse_mbox(&path, &text, unread, Some(&self.mail_dir)));
            } else if looks_like_rfc822(&text, &path) {
                let id = message_id(&path, 0);
                let folder_id = folder_id_for_path_with_root(&path, Some(&self.mail_dir));
                if let Some(message) = parse_rfc822_bytes(&id, &folder_id, &raw, unread) {
                    messages.push(message);
                }
            }
        }
        messages
    }

    pub fn persist_message_state(
        &self,
        id: &str,
        is_unread: bool,
        is_flagged: bool,
    ) -> std::io::Result<()> {
        let is_important = self
            .metadata
            .load()
            .get(id)
            .copied()
            .unwrap_or_default()
            .is_important;
        self.metadata.update(
            id,
            MailMessageState {
                is_unread,
                is_flagged,
                is_important,
            },
        )
    }

    pub fn persist_important_state(&self, id: &str, is_important: bool) -> std::io::Result<()> {
        let current = self.metadata.load().get(id).copied().unwrap_or_default();
        self.metadata.update(
            id,
            MailMessageState {
                is_unread: current.is_unread,
                is_flagged: current.is_flagged,
                is_important,
            },
        )
    }

    pub fn persist_follow_up(
        &self,
        id: &str,
        metadata: MailFollowUpMetadata,
    ) -> std::io::Result<()> {
        self.metadata.set_follow_up(id, metadata)
    }

    /// Apply application-owned read/flag state and local filter rules to
    /// records supplied by another local/read-only backend.
    #[cfg(eds_camel_bridge)]
    #[allow(dead_code)]
    pub(crate) fn apply_client_state(&self, messages: &mut [ParsedMail]) {
        let filter_store = MailFilterStore::default();
        self.apply_client_state_with_store(messages, &filter_store);
    }

    /// Apply client state and incoming rules using an explicitly captured
    /// filter store.  This path is safe to call from a worker because it only
    /// consumes the supplied store and service sidecar; it does not resolve
    /// process-global or GTK-backed configuration.
    #[cfg(eds_camel_bridge)]
    #[allow(dead_code)]
    pub(crate) fn apply_client_state_with_store(
        &self,
        messages: &mut [ParsedMail],
        filter_store: &MailFilterStore,
    ) {
        let metadata = self.metadata.load();
        let filter_rules = filter_store.load();

        for message in messages {
            let _ = apply_client_state(&metadata, &filter_rules, message, None);
        }
    }

    /// Apply only local incoming filter actions to messages whose flags are
    /// already authoritative in Camel/EDS.  Remote messages must not be
    /// overwritten by a stale client sidecar after a successful Camel
    /// mutation.
    pub(crate) fn apply_backend_filters(&self, messages: &mut [ParsedMail]) {
        let filter_store = MailFilterStore::default();
        self.apply_backend_filters_with_store(messages, &filter_store);
    }

    /// Apply incoming rules to backend-owned messages using an explicitly
    /// captured filter store.  The backend state remains authoritative; only
    /// the supplied incoming rules are evaluated and no global configuration
    /// is resolved on this path.
    pub(crate) fn apply_backend_filters_with_store(
        &self,
        messages: &mut [ParsedMail],
        filter_store: &MailFilterStore,
    ) {
        let filter_rules = filter_store.load();

        for message in messages {
            let _ = apply_local_filters(&filter_rules, message, None);
        }
    }

    #[allow(dead_code)]
    pub fn metadata_path(&self) -> &Path {
        self.metadata.path()
    }

    pub(crate) fn metadata_store(&self) -> &MailMetadataStore {
        &self.metadata
    }

    fn discovered_local_folder_ids(&self) -> HashSet<String> {
        let mut folders = self.metadata.load_local_folders();
        for folder_id in self.metadata.load_folder_overrides().values() {
            if folder_id.starts_with("custom:") {
                folders.insert(folder_id.clone());
            }
        }
        for folder_id in self.metadata.load_copies().values().flatten() {
            if folder_id.starts_with("custom:") {
                folders.insert(folder_id.clone());
            }
        }

        let cancelled = AtomicBool::new(false);
        for message in self.load_raw_local_records_cancelable(&cancelled) {
            if message.folder_id.starts_with("custom:") {
                folders.insert(message.folder_id);
            }
        }
        expand_local_folder_ancestors(&mut folders);
        folders
    }
}

fn normalize_custom_folder_id(name: &str) -> std::io::Result<String> {
    let normalized = name.trim().trim_start_matches('.');
    let folder_id = if normalized
        .get(.."custom:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("custom:"))
    {
        local_folder_id(normalized)
    } else {
        format!("custom:{normalized}")
    };
    let Some(folder_name) = folder_id.strip_prefix("custom:") else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "built-in folders cannot be created here",
        ));
    };
    if folder_name.trim().is_empty()
        || folder_name.chars().any(char::is_control)
        || !folder_name.split('/').all(|part| {
            let part = part.trim();
            !part.is_empty() && part != "." && part != ".."
        })
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid local folder name",
        ));
    }
    Ok(folder_id)
}

fn expand_local_folder_ancestors(folders: &mut HashSet<String>) {
    let existing = folders.clone();
    for folder_id in existing {
        let Some(path) = folder_id.strip_prefix("custom:") else {
            continue;
        };
        let mut prefix = String::new();
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            folders.insert(format!("custom:{prefix}"));
        }
    }
}

fn is_folder_tree_id(folder_id: &str, root: &str) -> bool {
    folder_id == root
        || folder_id
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn is_folder_tree_id_case_insensitive(folder_id: &str, root: &str) -> bool {
    folder_id.eq_ignore_ascii_case(root)
        || folder_id
            .get(..root.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root))
            && folder_id
                .get(root.len()..)
                .is_some_and(|rest| rest.starts_with('/'))
}

fn rename_folder_tree_id_case_insensitive(folder_id: &str, from: &str, to: &str) -> String {
    if folder_id.eq_ignore_ascii_case(from) {
        return to.to_string();
    }
    let Some(prefix) = folder_id.get(..from.len()) else {
        return folder_id.to_string();
    };
    let Some(rest) = folder_id.get(from.len()..) else {
        return folder_id.to_string();
    };
    if prefix.eq_ignore_ascii_case(from) && rest.starts_with('/') {
        format!("{to}{rest}")
    } else {
        folder_id.to_string()
    }
}

fn apply_client_state(
    metadata: &HashMap<String, MailMessageState>,
    filter_rules: &[MailFilterRule],
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
) -> FilterEffects {
    if let Some(state) = metadata.get(&message.id) {
        message.is_unread = state.is_unread;
        message.is_flagged = state.is_flagged;
        message.is_important = state.is_important;
    }

    apply_local_filter_effects(filter_rules, message, sidecar)
}

#[derive(Debug, Default)]
struct FilterEffects {
    changed: bool,
    state_changed: bool,
    moved: bool,
    copied: bool,
    deleted: bool,
    stop_processing: bool,
}

fn message_matches_rule(rule: &MailFilterRule, message: &ParsedMail) -> bool {
    let body = format!(
        "{} {}",
        message.preview_snippet,
        html_to_plain_text(&message.html_body)
    );
    let sender = format!("{} <{}>", message.sender_name, message.sender_email);
    let recipient = format!(
        "{} {} {}",
        message.recipient_to, message.recipient_cc, message.recipient_bcc
    );
    let headers = format!(
        "From: {}\nTo: {}\nCc: {}\nBcc: {}\nReply-To: {}\nSubject: {}\nMessage-ID: {}\nIn-Reply-To: {}\nReferences: {}",
        sender,
        message.recipient_to,
        message.recipient_cc,
        message.recipient_bcc,
        message.reply_to,
        message.subject,
        message.message_id.as_deref().unwrap_or_default(),
        message.in_reply_to.join(" "),
        message.references.join(" "),
    );

    rule.matches_with_context(
        &message.subject,
        &sender,
        &recipient,
        &body,
        &headers,
        &message.labels,
        FilterMessageContext {
            date: &message.date_str,
            folder: &message.folder_id,
            is_unread: message.is_unread,
            is_flagged: message.is_flagged,
            is_important: message.is_important,
            is_junk: message.folder_id == "junk" || message.folder_id.starts_with("junk/"),
            has_attachment: !message.attachments.is_empty(),
        },
    )
}

fn apply_local_filters(
    rules: &[MailFilterRule],
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
) -> bool {
    apply_local_filter_effects(rules, message, sidecar).deleted
}

fn apply_local_filter_effects(
    rules: &[MailFilterRule],
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
) -> FilterEffects {
    let mut total_effects = FilterEffects::default();
    for rule in rules {
        if rule.scope != FilterScope::Incoming || !message_matches_rule(rule, message) {
            continue;
        }
        let effects = apply_filter_rule(rule, message, sidecar);
        total_effects.changed |= effects.changed;
        total_effects.state_changed |= effects.state_changed;
        total_effects.moved |= effects.moved;
        total_effects.copied |= effects.copied;
        total_effects.deleted |= effects.deleted;
        if effects.stop_processing {
            total_effects.stop_processing = true;
            break;
        }
    }
    total_effects
}

fn apply_filter_rule(
    rule: &MailFilterRule,
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
) -> FilterEffects {
    let mut effects = FilterEffects::default();
    for action in rule.effective_actions() {
        match action {
            FilterAction::MarkRead => {
                if message.is_unread {
                    message.is_unread = false;
                    effects.changed = true;
                    effects.state_changed = true;
                }
            }
            FilterAction::MarkUnread => {
                if !message.is_unread {
                    message.is_unread = true;
                    effects.changed = true;
                    effects.state_changed = true;
                }
            }
            FilterAction::SetFlagged => {
                if !message.is_flagged {
                    message.is_flagged = true;
                    effects.changed = true;
                    effects.state_changed = true;
                }
            }
            FilterAction::ClearFlagged => {
                if message.is_flagged {
                    message.is_flagged = false;
                    effects.changed = true;
                    effects.state_changed = true;
                }
            }
            FilterAction::SetImportant => {
                if !message.is_important {
                    message.is_important = true;
                    effects.changed = true;
                    effects.state_changed = true;
                }
            }
            FilterAction::ClearImportant => {
                if message.is_important {
                    message.is_important = false;
                    effects.changed = true;
                    effects.state_changed = true;
                }
            }
            FilterAction::MoveToFolder(folder) => {
                apply_local_filter_folder_action(rule, message, sidecar, folder, &mut effects);
            }
            FilterAction::CopyToFolder(folder) => {
                let folder_id = local_folder_id(folder);
                if let Some(sidecar) = sidecar {
                    if let Err(error) = sidecar.set_copy(&message.id, &folder_id, true) {
                        tracing::warn!(
                            %error,
                            message_id = %message.id,
                            folder = %folder_id,
                            "Could not persist local filter copy"
                        );
                    } else {
                        effects.changed = true;
                        effects.copied = true;
                    }
                } else {
                    tracing::debug!(
                        rule = %rule.name,
                        folder,
                        "Local filter copy action requires a local sidecar or Camel multi-label backend"
                    );
                }
            }
            FilterAction::Delete => {
                if let Some(sidecar) = sidecar {
                    if let Err(error) = sidecar.set_deleted(&message.id, true) {
                        tracing::warn!(%error, message_id = %message.id, "Could not persist local filter deletion");
                    } else {
                        effects.changed = true;
                        effects.deleted = true;
                    }
                } else {
                    tracing::debug!(
                        rule = %rule.name,
                        "Local filter matched delete action; remote backend must apply it"
                    );
                }
            }
            FilterAction::SetJunk => {
                apply_local_filter_folder_action(rule, message, sidecar, "junk", &mut effects);
            }
            FilterAction::ClearJunk => {
                apply_local_filter_folder_action(rule, message, sidecar, "inbox", &mut effects);
            }
            FilterAction::SetLabel(label) => {
                apply_local_filter_label_action(rule, message, sidecar, label, true, &mut effects);
            }
            FilterAction::ClearLabel(label) => {
                apply_local_filter_label_action(rule, message, sidecar, label, false, &mut effects);
            }
            FilterAction::StopProcessing => {
                effects.stop_processing = true;
                break;
            }
        }
    }
    effects
}

fn apply_local_filter_label_action(
    rule: &MailFilterRule,
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
    label: &str,
    labeled: bool,
    effects: &mut FilterEffects,
) {
    let label = label.trim();
    if label.is_empty() {
        return;
    }
    if let Some(sidecar) = sidecar {
        if let Err(error) = sidecar.set_label(&message.id, label, labeled) {
            tracing::warn!(
                %error,
                message_id = %message.id,
                label,
                "Could not persist local filter label"
            );
            return;
        }
        let changed = if labeled {
            if message
                .labels
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(label))
            {
                false
            } else {
                message.labels.push(label.to_string());
                message
                    .labels
                    .sort_by_key(|value| value.to_ascii_lowercase());
                true
            }
        } else {
            let original_len = message.labels.len();
            message
                .labels
                .retain(|candidate| !candidate.eq_ignore_ascii_case(label));
            original_len != message.labels.len()
        };
        effects.changed |= changed;
    } else {
        tracing::debug!(
            rule = %rule.name,
            label,
            labeled,
            "Local filter label action requires an app-owned sidecar or native Camel backend"
        );
    }
}

fn sorted_labels(labels: &std::collections::HashSet<String>) -> Vec<String> {
    let mut labels = labels.iter().cloned().collect::<Vec<_>>();
    labels.sort_by_key(|value| value.to_ascii_lowercase());
    labels
}

fn apply_local_filter_folder_action(
    rule: &MailFilterRule,
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
    folder: &str,
    effects: &mut FilterEffects,
) {
    let folder_id = local_folder_id(folder);
    if let Some(sidecar) = sidecar {
        if let Err(error) = sidecar.set_folder_override(&message.id, Some(&folder_id)) {
            tracing::warn!(
                %error,
                message_id = %message.id,
                folder = %folder_id,
                "Could not persist local filter folder"
            );
        } else if message.folder_id != folder_id {
            message.folder_id = folder_id;
            effects.changed = true;
            effects.moved = true;
        }
    } else {
        tracing::debug!(
            rule = %rule.name,
            folder,
            "Local filter matched folder action; Camel backend is required to apply it"
        );
    }
}

fn local_folder_id(folder: &str) -> String {
    let normalized = folder.trim().trim_start_matches('.');
    match normalized.to_ascii_lowercase().as_str() {
        "inbox" => "inbox".to_string(),
        "sent" | "sent-mail" => "sent".to_string(),
        "draft" | "drafts" => "drafts".to_string(),
        "trash" | "deleted" => "trash".to_string(),
        "archive" | "all-mail" => "archive".to_string(),
        "junk" | "spam" => "junk".to_string(),
        _ if normalized.starts_with("custom:") => normalized.to_string(),
        _ => format!("custom:{normalized}"),
    }
}

fn is_local_trash_folder(folder_id: &str) -> bool {
    let normalized = folder_id
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    normalized == "trash"
        || normalized == "deleted"
        || normalized.starts_with("trash/")
        || normalized.starts_with("deleted/")
        || normalized == "custom:trash"
        || normalized.starts_with("custom:trash/")
        || normalized == "custom:deleted"
        || normalized.starts_with("custom:deleted/")
}

fn local_copy_id(message_id: &str, folder_id: &str) -> String {
    format!("{message_id}::copy::{folder_id}")
}

impl Default for MailStoreService {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_import_files(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_import_files(&path, paths);
        } else if file_type.is_file() && is_import_candidate(&path) {
            paths.push(path);
        }
    }
}

fn import_root_for_file(source: &Path) -> PathBuf {
    let Some(parent) = source.parent() else {
        return PathBuf::new();
    };
    let is_maildir_message = parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "cur" | "new"));
    if !is_maildir_message {
        return parent.to_path_buf();
    }

    // A directly selected message inside a hidden Maildir folder or an
    // Evolution `.sbd` tree should retain that folder in the destination.
    // Directory imports already provide their explicit root and do not need
    // this heuristic.
    let mut root = parent.parent().unwrap_or(parent).to_path_buf();
    loop {
        let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
            break;
        };
        let hidden_folder = name.starts_with('.') && name != "." && name != "..";
        let sbd_folder = name
            .strip_suffix(".sbd")
            .is_some_and(|name| !name.is_empty());
        if !(hidden_folder || sbd_folder) {
            break;
        }
        let Some(parent) = root.parent() else {
            break;
        };
        root = parent.to_path_buf();
    }
    root
}

fn relative_import_path(source: &Path, source_root: &Path) -> PathBuf {
    let relative = source
        .strip_prefix(source_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| source.file_name().map(PathBuf::from).unwrap_or_default());
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_string)
        .collect::<Vec<_>>();

    // Evolution's local provider normally lives below `mail/local`.  The
    // app-owned store is already the equivalent of that `local` root, so do
    // not create an artificial `mail/local` level during migration.
    let start = components
        .windows(2)
        .enumerate()
        .find(|(_, pair)| {
            pair[0].eq_ignore_ascii_case("mail") && pair[1].eq_ignore_ascii_case("local")
        })
        .map(|(index, _)| index + 2)
        .unwrap_or(0);

    let mut result = PathBuf::new();
    for component in components.into_iter().skip(start) {
        result.push(component);
    }
    result
}

fn is_import_candidate(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    if matches!(extension.as_deref(), Some("eml" | "mbox")) {
        return true;
    }
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "cur" | "new"))
    {
        return true;
    }
    if extension.is_some() {
        return false;
    }
    let Ok(raw) = fs::read(path) else {
        return false;
    };
    let text = String::from_utf8_lossy(&raw);
    looks_like_mbox(&text) || looks_like_rfc822(&text, path)
}

fn collect_mail_files(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            // A symlinked directory can create a cycle.  Mail stores do not
            // need symlink traversal for the initial local backend.
            if !file_type.is_symlink() {
                collect_mail_files(&path, paths);
            }
        } else if file_type.is_file() && is_candidate_mail_file(&path) {
            paths.push(path);
        }
    }
}

fn is_candidate_mail_file(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(extension.as_deref(), Some("eml" | "mbox")) {
        return true;
    }

    // Maildir messages have no extension and live in cur/ or new/.  Files in
    // tmp/ are not delivered messages, so they are intentionally ignored.
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "cur" | "new"))
    {
        return true;
    }

    // Evolution's local provider stores mbox files without an extension.
    // Metadata/index files are filtered later by content inspection.
    path.extension().is_none()
}

fn looks_like_mbox(raw: &str) -> bool {
    let mut lines = raw.lines();
    let Some(first_line) = lines.next() else {
        return false;
    };

    first_line.starts_with("From ") && lines.any(|line| line.starts_with("From: "))
}

fn looks_like_rfc822(raw: &str, path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) == Some("eml") {
        return true;
    }

    // Maildir and mbox detection is intentionally conservative.  This keeps
    // Camel index/cache files out of the message list.
    raw.contains("\nFrom:")
        || raw.starts_with("From:")
        || raw.contains("\nSubject:")
        || raw.starts_with("Subject:")
}

fn maildir_message_is_unread(path: &Path) -> bool {
    let Some(parent) = path.parent().and_then(Path::file_name) else {
        return true;
    };

    if parent.to_str() != Some("cur") {
        return true;
    }

    // Maildir info is encoded after `:2,`; the `S` flag means Seen.
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name.split_once(":2,")
        .map(|(_, flags)| !flags.contains('S'))
        .unwrap_or(true)
}

fn parse_mbox(path: &Path, raw: &str, unread: bool, mail_root: Option<&Path>) -> Vec<ParsedMail> {
    let mut starts = Vec::new();
    let mut offset = 0;

    for line in raw.split_inclusive('\n') {
        if line.starts_with("From ") {
            starts.push(offset);
        }
        offset += line.len();
    }

    if starts.is_empty() {
        return Vec::new();
    }

    starts
        .iter()
        .enumerate()
        .filter_map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(raw.len());
            let record = &raw[*start..end];
            let message_start = record.find('\n').map(|line_end| line_end + 1)?;
            let id = message_id(path, index);
            let folder_id = folder_id_for_path_with_root(path, mail_root);
            parse_rfc822(&id, &folder_id, &record[message_start..], unread)
        })
        .collect()
}

fn message_id(path: &Path, index: usize) -> String {
    format!("local:{}#{index}", path.to_string_lossy())
}

fn folder_id_for_path_with_root(path: &Path, mail_root: Option<&Path>) -> String {
    if let Some(mail_root) = mail_root {
        if let Ok(relative) = path.strip_prefix(mail_root) {
            let components = relative
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if !components.is_empty() {
                let is_maildir_message = components
                    .get(components.len().saturating_sub(2))
                    .is_some_and(|name| matches!(name.as_str(), "cur" | "new"));
                let extension = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase);
                let mut folder_components = if is_maildir_message {
                    components[..components.len().saturating_sub(2)].to_vec()
                } else if extension.as_deref() == Some("eml") {
                    components[..components.len().saturating_sub(1)].to_vec()
                } else {
                    let mut components = components;
                    if extension.as_deref() == Some("mbox") {
                        if let Some(name) = components.last_mut() {
                            let stem = Path::new(name)
                                .file_stem()
                                .and_then(|stem| stem.to_str())
                                .map(str::to_string);
                            if let Some(stem) = stem {
                                *name = stem;
                            }
                        }
                    }
                    components
                };

                for component in &mut folder_components {
                    *component = component.trim_start_matches('.').to_string();
                    if let Some(name) = component.strip_suffix(".sbd") {
                        *component = name.to_string();
                    }
                }
                folder_components.retain(|component| !component.is_empty());
                if folder_components.is_empty() {
                    return "inbox".to_string();
                }
                return local_folder_id(&folder_components.join("/"));
            }
        }
    }

    folder_id_for_path(path)
}

fn folder_id_for_path(path: &Path) -> String {
    let candidate = match path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        Some("cur") | Some("new") => path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str()),
        Some(_) if path.extension().and_then(|extension| extension.to_str()) == Some("eml") => {
            Some("inbox")
        }
        _ => path.file_name().and_then(|name| name.to_str()),
    };

    let candidate = candidate.unwrap_or("inbox").trim_start_matches('.');
    match candidate.to_ascii_lowercase().as_str() {
        "inbox" => "inbox".to_string(),
        "sent" | "sent-mail" => "sent".to_string(),
        "draft" | "drafts" => "drafts".to_string(),
        "trash" | "deleted" => "trash".to_string(),
        "archive" | "all-mail" => "archive".to_string(),
        "junk" | "spam" => "junk".to_string(),
        _ => format!("custom:{candidate}"),
    }
}

pub(crate) fn parse_rfc822(
    id: &str,
    folder_id: &str,
    raw: &str,
    unread: bool,
) -> Option<ParsedMail> {
    parse_rfc822_bytes(id, folder_id, raw.as_bytes(), unread)
}

pub(crate) fn parse_rfc822_bytes(
    id: &str,
    folder_id: &str,
    raw: &[u8],
    unread: bool,
) -> Option<ParsedMail> {
    let (header_bytes, body) = split_mime_headers_and_body(raw);
    let header_text = String::from_utf8_lossy(header_bytes);
    let headers = parse_headers(&header_text);

    if headers.is_empty() && body.is_empty() {
        return None;
    }

    let (sender_name, sender_email) = headers
        .get("from")
        .map(|value| parse_address(value))
        .unwrap_or_else(|| ("Unbekannt".to_string(), String::new()));
    let recipient = headers
        .get("to")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let recipient_cc = headers
        .get("cc")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let recipient_bcc = headers
        .get("bcc")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let reply_to = headers
        .get("reply-to")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let subject = headers
        .get("subject")
        .map(|value| decode_mime_header(value))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "(Kein Betreff)".to_string());
    let date = headers
        .get("date")
        .map(|value| decode_mime_header(value))
        .unwrap_or_default();
    let message_id = headers
        .get("message-id")
        .and_then(|value| normalize_message_id(value));
    let in_reply_to = headers
        .get("in-reply-to")
        .map(|value| extract_message_ids(value))
        .unwrap_or_default();
    let references = headers
        .get("references")
        .map(|value| extract_message_ids(value))
        .unwrap_or_default();

    let mut extracted = MimeExtraction::default();
    extract_mime_content(&headers, body, &mut extracted);

    let plain_body = extracted.plain_body.unwrap_or_default();
    let body_html = if let Some(html_body) = extracted.html_body {
        html_body
    } else if looks_like_html(&plain_body) {
        plain_body.clone()
    } else if !plain_body.trim().is_empty() {
        format!("<pre>{}</pre>", escape_html(plain_body.trim()))
    } else if let Some(html_fallback) = extracted.html_fallback {
        html_fallback
    } else {
        "<p>(Keine Nachricht)</p>".to_string()
    };

    let preview = text_preview(if !plain_body.trim().is_empty() {
        plain_body.as_str()
    } else {
        body_html.as_str()
    });

    let raw_mime = (!matches!(
        CryptoService::verify_signature(raw).protocol,
        CryptoProtocol::None
    ))
    .then(|| raw.to_vec());

    Some(ParsedMail {
        id: id.to_string(),
        folder_id: folder_id.to_string(),
        folder_name: folder_id.to_string(),
        backend_location: None,
        sender_name,
        sender_email,
        recipient_to: recipient,
        recipient_cc,
        recipient_bcc,
        reply_to,
        subject,
        message_id,
        in_reply_to,
        references,
        thread_id: String::new(),
        thread_size: 1,
        thread_position: 0,
        date_str: date,
        preview_snippet: preview,
        html_body: body_html,
        raw_headers: (!header_bytes.is_empty()).then(|| header_bytes.to_vec()),
        raw_mime,
        is_unread: unread,
        is_flagged: false,
        is_important: false,
        follow_up_state: MailFollowUpState::None,
        follow_up: MailFollowUpMetadata::default(),
        labels: Vec::new(),
        attachments: extracted.attachments,
    })
}

/// Parse the original RFC822 header block for Evolution's formatted
/// "All Message Headers" view. Order and repeated field names are preserved;
/// folded values are unfolded only for display and RFC2047 words are decoded.
pub(crate) fn parse_rfc822_header_fields(header_bytes: &[u8]) -> Vec<(String, String)> {
    let normalized = String::from_utf8_lossy(header_bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut fields = Vec::new();
    let mut current: Option<(String, String)> = None;

    for line in normalized.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = current.as_mut() {
                let continuation = line.trim();
                if !continuation.is_empty() {
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(continuation);
                }
            }
            continue;
        }
        if let Some((name, value)) = current.take() {
            fields.push((name, decode_mime_header(value.trim())));
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() {
            current = Some((name.to_string(), value.trim().to_string()));
        }
    }
    if let Some((name, value)) = current {
        fields.push((name, decode_mime_header(value.trim())));
    }
    fields
}

pub(crate) fn assign_thread_metadata(messages: &mut [ParsedMail]) {
    let inputs = messages
        .iter()
        .map(|message| ThreadIdentity {
            message_id: message.message_id.as_deref(),
            in_reply_to: &message.in_reply_to,
            references: &message.references,
            subject: &message.subject,
        })
        .collect::<Vec<_>>();
    let memberships = group_messages(&inputs);
    for (message, membership) in messages.iter_mut().zip(memberships) {
        message.thread_id = membership.thread_id;
        message.thread_size = membership.size;
        message.thread_position = membership.position;
    }
}

fn parse_headers(header_text: &str) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_value = String::new();

    for line in header_text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            if current_name.is_some() {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }

        let Some((name, value)) = line.split_once(':') else {
            continue;
        };

        flush_header(&mut headers, &mut current_name, &mut current_value);
        current_name = Some(name.trim().to_ascii_lowercase());
        current_value.push_str(value.trim());
    }

    flush_header(&mut headers, &mut current_name, &mut current_value);
    headers
}

fn flush_header(
    headers: &mut HashMap<String, String>,
    current_name: &mut Option<String>,
    current_value: &mut String,
) {
    if let Some(name) = current_name.take() {
        headers.insert(name, current_value.trim().to_string());
        current_value.clear();
    }
}

fn parse_address(value: &str) -> (String, String) {
    let decoded = decode_mime_header(value);
    if let Some(start) = decoded.rfind('<') {
        if let Some(end_offset) = decoded[start..].find('>') {
            let end = start + end_offset;
            let email = decoded[start + 1..end].trim().to_string();
            let name = decoded[..start].trim().trim_matches('"').trim().to_string();
            return (if name.is_empty() { email.clone() } else { name }, email);
        }
    }

    let email = decoded.trim().trim_matches('"').to_string();
    (email.clone(), email)
}

#[derive(Debug, Default)]
struct MimeExtraction {
    /// The first usable HTML representation, independent of the order in
    /// which a multipart/alternative entity lists its children.
    html_body: Option<String>,
    /// Retain a non-empty but non-renderable HTML part as a last resort when
    /// the message has no readable plaintext representation.
    html_fallback: Option<String>,
    /// The first readable plaintext representation, used for previews and as
    /// the safe display fallback when HTML is empty or malformed.
    plain_body: Option<String>,
    attachments: Vec<AttachmentInfo>,
}

fn split_mime_headers_and_body(raw: &[u8]) -> (&[u8], &[u8]) {
    for separator in [b"\r\n\r\n".as_slice(), b"\n\n", b"\r\r"] {
        if let Some(index) = raw
            .windows(separator.len())
            .position(|candidate| candidate == separator)
        {
            return (&raw[..index], &raw[index + separator.len()..]);
        }
    }

    // A malformed child without a blank line is still more useful as a
    // text/plain body than as a header block that silently disappears.
    let header_text = String::from_utf8_lossy(raw);
    if parse_headers(&header_text).is_empty() {
        (&[], raw)
    } else {
        (raw, &[])
    }
}

fn split_multipart_parts<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let marker = format!("--{boundary}");
    let marker = marker.as_bytes();
    let mut parts = Vec::new();
    let mut part_start = None;
    let mut cursor = 0;

    while cursor < body.len() {
        let (line_end, next_line) = mime_line_bounds(body, cursor);
        if let Some(is_closing) = multipart_delimiter(&body[cursor..line_end], marker) {
            if let Some(start) = part_start.take() {
                parts.push(trim_mime_line_ending(&body[start..cursor]));
            }
            if is_closing {
                return parts;
            }
            part_start = Some(next_line);
        }

        if next_line == cursor {
            break;
        }
        cursor = next_line;
    }

    // Accept a truncated multipart without a closing delimiter.  This is
    // common enough in recovered mailboxes that dropping the final readable
    // part would be surprising.
    if let Some(start) = part_start {
        parts.push(trim_mime_line_ending(&body[start..]));
    }
    parts
}

fn mime_line_bounds(input: &[u8], start: usize) -> (usize, usize) {
    let mut line_end = start;
    while line_end < input.len() && !matches!(input[line_end], b'\r' | b'\n') {
        line_end += 1;
    }

    let next_line = match input.get(line_end) {
        Some(b'\r') if input.get(line_end + 1) == Some(&b'\n') => line_end + 2,
        Some(b'\r' | b'\n') => line_end + 1,
        _ => line_end,
    };
    (line_end, next_line)
}

fn multipart_delimiter(line: &[u8], marker: &[u8]) -> Option<bool> {
    let remainder = line.strip_prefix(marker)?;
    let (is_closing, remainder) = remainder
        .strip_prefix(b"--")
        .map_or((false, remainder), |remainder| (true, remainder));
    remainder
        .iter()
        .all(|byte| byte.is_ascii_whitespace())
        .then_some(is_closing)
}

fn trim_mime_line_ending(mut value: &[u8]) -> &[u8] {
    if value.ends_with(b"\r\n") {
        value = &value[..value.len() - 2];
    } else if value.ends_with(b"\r") || value.ends_with(b"\n") {
        value = &value[..value.len() - 1];
    }
    value
}

fn detect_boundary_in_body(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    for line in text.lines().take(60) {
        let trimmed = line.trim();
        if trimmed.starts_with("--") && trimmed.len() > 6 && !trimmed.ends_with("--") {
            let candidate = &trimmed[2..];
            if !candidate.contains(' ') && !candidate.contains(':') {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

fn extract_mime_content(
    headers: &HashMap<String, String>,
    body: &[u8],
    extracted: &mut MimeExtraction,
) {
    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("text/plain");
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();

    if media_type.starts_with("multipart/") {
        let boundary =
            mime_parameter(content_type, "boundary").or_else(|| detect_boundary_in_body(body));
        if let Some(boundary) = boundary {
            let parts = split_multipart_parts(body, &boundary);
            if !parts.is_empty() {
                for part in parts {
                    let (part_header_bytes, part_body) = split_mime_headers_and_body(part);
                    let part_header_text = String::from_utf8_lossy(part_header_bytes);
                    let part_headers = parse_headers(&part_header_text);
                    extract_mime_content(&part_headers, part_body, extracted);
                }
                return;
            }
        }
    }

    if media_type.starts_with("text/") || media_type.is_empty() {
        if let Some(boundary) = detect_boundary_in_body(body) {
            let parts = split_multipart_parts(body, &boundary);
            if parts.len() > 1 {
                for part in parts {
                    let (part_header_bytes, part_body) = split_mime_headers_and_body(part);
                    let part_header_text = String::from_utf8_lossy(part_header_bytes);
                    let part_headers = parse_headers(&part_header_text);
                    extract_mime_content(&part_headers, part_body, extracted);
                }
                return;
            }
        }
    }

    let decoded = decode_transfer_encoding(
        body,
        headers
            .get("content-transfer-encoding")
            .map(String::as_str)
            .unwrap_or(""),
    );
    let disposition = headers
        .get("content-disposition")
        .map(String::as_str)
        .unwrap_or("");
    let content_id = headers
        .get("content-id")
        .and_then(|value| normalize_content_id(value));
    let filename =
        mime_parameter(disposition, "filename").or_else(|| mime_parameter(content_type, "name"));

    let is_explicit_attachment = disposition.to_ascii_lowercase().contains("attachment");
    if is_explicit_attachment || (!media_type.starts_with("text/") && filename.is_some()) {
        let filename = filename.unwrap_or_else(|| "attachment".to_string());
        extracted.attachments.push(AttachmentInfo {
            filename: decode_mime_header(&filename),
            size_formatted: format_size(decoded.len()),
            mime_type: media_type,
            content_id,
            content: decoded.into(),
        });
        return;
    }

    match media_type.as_str() {
        "text/html" => {
            let html =
                decode_mime_bytes(&decoded, mime_parameter(content_type, "charset").as_deref());
            extracted.record_html(html);
        }
        "text/plain" | "" => {
            let plain =
                decode_mime_bytes(&decoded, mime_parameter(content_type, "charset").as_deref());
            extracted.record_plain(plain);
        }
        _ => {
            // A non-text part without a filename is still useful to expose as
            // an attachment, but use a stable fallback name for the UI.
            extracted.attachments.push(AttachmentInfo {
                filename: "attachment".to_string(),
                size_formatted: format_size(decoded.len()),
                mime_type: media_type,
                content_id,
                content: decoded.into(),
            });
        }
    }
}

impl MimeExtraction {
    fn record_html(&mut self, value: String) {
        let value = normalize_mime_text(value);
        if value.trim().is_empty() {
            return;
        }

        if is_usable_html(&value) {
            if self.html_body.is_none() {
                self.html_body = Some(value);
            }
        } else if self.html_fallback.is_none() {
            self.html_fallback = Some(value);
        }
    }

    fn record_plain(&mut self, value: String) {
        let value = normalize_mime_text(value);
        if !value.trim().is_empty() && self.plain_body.is_none() {
            self.plain_body = Some(value);
        }
    }
}

fn normalize_mime_text(value: String) -> String {
    value
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn is_usable_html(value: &str) -> bool {
    let readable = html_to_plain_text(value);
    if !readable.trim().is_empty() {
        return true;
    }

    let lower = value.to_ascii_lowercase();
    ["<img", "<br", "<hr", "<table", "<video", "<audio"]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn looks_like_html(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<head")
        || trimmed.starts_with("<body")
        || trimmed.starts_with("<style")
        || trimmed.starts_with("<table")
        || trimmed.starts_with("<div")
        || (trimmed.contains("<p>") && trimmed.contains("</p>"))
        || (trimmed.contains("<br>") || trimmed.contains("<br/>") || trimmed.contains("<br />"))
}

fn mime_parameter(value: &str, parameter: &str) -> Option<String> {
    let target = parameter.trim().to_ascii_lowercase();
    let continuation_prefix = format!("{target}*");
    let mut plain = None;
    let mut extended = None;
    let mut continuations = BTreeMap::<usize, (bool, String)>::new();

    for (name, raw_value) in mime_parameter_parts(value) {
        if name == target {
            plain = Some(raw_value);
        } else if name == continuation_prefix {
            extended = Some(decode_rfc2231_value(&raw_value));
        } else if let Some(index_value) = name.strip_prefix(&continuation_prefix) {
            let (index_value, encoded) = index_value
                .strip_suffix('*')
                .map(|value| (value, true))
                .unwrap_or((index_value, false));
            if let Ok(index) = index_value.parse::<usize>() {
                continuations.insert(index, (encoded, raw_value));
            }
        }
    }

    if let Some((first_index, _)) = continuations.first_key_value()
        && *first_index == 0
    {
        let mut joined = String::new();
        let mut expected = 0;
        for (index, (encoded, part)) in continuations {
            if index != expected {
                break;
            }
            let decoded_part = if encoded {
                decode_rfc2231_value(&part)
            } else {
                part
            };
            joined.push_str(&decoded_part);
            expected += 1;
        }
        if expected > 0 {
            return Some(joined);
        }
    }

    extended.or(plain)
}

fn mime_parameter_parts(value: &str) -> Vec<(String, String)> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;

    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                segments.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    segments.push(&value[start..]);

    segments
        .into_iter()
        .skip(1)
        .filter_map(|segment| {
            let (name, raw_value) = segment.split_once('=')?;
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() {
                return None;
            }
            Some((name, unquote_mime_parameter(raw_value.trim())))
        })
        .collect()
}

fn unquote_mime_parameter(value: &str) -> String {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return value.to_string();
    }

    let mut result = String::with_capacity(value.len() - 2);
    let mut escaped = false;
    for character in value[1..value.len() - 1].chars() {
        if escaped {
            result.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

fn decode_rfc2231_value(value: &str) -> String {
    let value = value.trim();
    let payload = value
        .split_once('\'')
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(_, payload)| payload)
        .unwrap_or(value);
    String::from_utf8_lossy(&percent_decode(payload)).into_owned()
}

fn percent_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                result.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        result.push(bytes[index]);
        index += 1;
    }
    result
}

fn normalize_content_id(value: &str) -> Option<String> {
    let decoded = decode_mime_header(value);
    let value = decoded
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn decode_transfer_encoding(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => decode_base64(body).unwrap_or_else(|| body.to_vec()),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    let mut saw_padding = false;

    for &byte in input {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            saw_padding = true;
            continue;
        }
        if saw_padding {
            return None;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }

    Some(output)
}

fn decode_quoted_printable(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'=' {
            if index + 1 < bytes.len() && bytes[index + 1] == b'\r' {
                index += 2;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
                continue;
            }
            if index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                index += 2;
                continue;
            }
            if index + 2 < bytes.len() {
                if let (Some(high), Some(low)) =
                    (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
                {
                    output.push(high * 16 + low);
                    index += 3;
                    continue;
                }
            }
        }
        output.push(bytes[index]);
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

fn parse_rfc2047_word(encoded: &str) -> Option<(&str, &str, &str, usize)> {
    if !encoded.starts_with("=?") {
        return None;
    }
    let rest = &encoded[2..];
    let charset_end = rest.find('?')?;
    let charset = &rest[..charset_end];
    let rest = &rest[charset_end + 1..];

    let encoding_end = rest.find('?')?;
    let encoding = &rest[..encoding_end];
    let rest = &rest[encoding_end + 1..];

    let payload_end = rest.find("?=")?;
    let payload = &rest[..payload_end];

    let total_len = 2 + charset_end + 1 + encoding_end + 1 + payload_end + 2;
    Some((charset, encoding, payload, total_len))
}

pub(crate) fn decode_mime_header(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;
    let mut last_was_encoded = false;

    while let Some(start) = rest.find("=?") {
        let prefix = &rest[..start];
        if !last_was_encoded || !prefix.chars().all(|c| c.is_ascii_whitespace()) {
            result.push_str(prefix);
        }
        let candidate = &rest[start..];
        let Some((raw_charset, encoding, payload, word_len)) = parse_rfc2047_word(candidate) else {
            result.push_str("=?");
            rest = &candidate[2..];
            last_was_encoded = false;
            continue;
        };

        let charset = raw_charset.split('*').next().unwrap_or(raw_charset);
        let decoded = match encoding {
            e if e.eq_ignore_ascii_case("b") => decode_base64(payload.as_bytes())
                .map(|bytes| decode_mime_bytes(&bytes, Some(charset))),
            e if e.eq_ignore_ascii_case("q") => {
                let payload = payload.replace('_', " ");
                Some(decode_mime_bytes(
                    &decode_quoted_printable(payload.as_bytes()),
                    Some(charset),
                ))
            }
            _ => None,
        };

        if let Some(decoded) = decoded {
            result.push_str(&decoded);
            last_was_encoded = true;
        } else {
            result.push_str(&candidate[..word_len]);
            last_was_encoded = false;
        }
        rest = &candidate[word_len..];
    }

    result.push_str(rest);
    result.trim().to_string()
}

fn decode_mime_bytes(bytes: &[u8], charset: Option<&str>) -> String {
    let charset = charset
        .unwrap_or("utf-8")
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase();
    let decoded = match charset.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => String::from_utf8_lossy(bytes).into_owned(),
        "utf-16" | "utf16" => decode_utf16_bytes(bytes, false),
        "utf-16le" | "utf16le" => decode_utf16_bytes(bytes, true),
        "utf-16be" | "utf16be" => decode_utf16_bytes(bytes, false),
        "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "latin-1" | "l1" | "cp819"
        | "ibm819" => bytes.iter().map(|byte| char::from(*byte)).collect(),
        "iso-8859-15" | "iso8859-15" | "latin9" | "latin-9" => bytes
            .iter()
            .map(|byte| decode_iso_8859_15_byte(*byte))
            .collect(),
        "windows-1252" | "cp1252" => bytes
            .iter()
            .map(|byte| decode_windows_1252_byte(*byte))
            .collect(),
        "us-ascii" | "ascii" => bytes
            .iter()
            .map(|byte| {
                if byte.is_ascii() {
                    char::from(*byte)
                } else {
                    '\u{FFFD}'
                }
            })
            .collect(),
        _ => {
            if let Ok(valid) = std::str::from_utf8(bytes) {
                valid.to_string()
            } else {
                bytes
                    .iter()
                    .map(|byte| decode_windows_1252_byte(*byte))
                    .collect()
            }
        }
    };
    normalize_mime_text(decoded)
}

fn decode_utf16_bytes(bytes: &[u8], little_endian: bool) -> String {
    let (little_endian, bytes) = match bytes {
        [0xff, 0xfe, rest @ ..] => (true, rest),
        [0xfe, 0xff, rest @ ..] => (false, rest),
        _ => (little_endian, bytes),
    };
    let units = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&units)
}

fn decode_iso_8859_15_byte(byte: u8) -> char {
    match byte {
        0xa4 => '\u{20ac}',
        0xa6 => '\u{0160}',
        0xa8 => '\u{0161}',
        0xb4 => '\u{017d}',
        0xb8 => '\u{017e}',
        0xbc => '\u{0152}',
        0xbd => '\u{0153}',
        0xbe => '\u{0178}',
        _ => char::from(byte),
    }
}

fn decode_windows_1252_byte(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        0x81 | 0x8D | 0x8F | 0x90 | 0x9D => '\u{FFFD}',
        _ => char::from(byte),
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text_preview(value: &str) -> String {
    let clean = crate::services::html_sanitizer::html_to_plain_text(value);
    let mut collapsed = String::with_capacity(clean.len());
    for word in clean.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    let preview: String = collapsed.chars().take(120).collect();
    if collapsed.chars().count() > 120 {
        format!("{preview}…")
    } else {
        preview
    }
}

fn format_size(size: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = KIB * 1024;

    if size >= MIB {
        format!("{:.1} MiB", size as f64 / MIB as f64)
    } else if size >= KIB {
        format!("{:.1} KiB", size as f64 / KIB as f64)
    } else {
        format!("{size} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mail_filter_service::{
        FilterAction, FilterCondition, FilterField, FilterOperator, FilterScope,
    };

    #[test]
    fn parses_folded_headers_and_plain_text() {
        let raw = concat!(
            "From: =?UTF-8?Q?M=C3=BCller?= <mueller@example.org>\r\n",
            "Reply-To: Support <support@example.org>\r\n",
            "To: Receiver <receiver@example.org>\r\n",
            "Cc: Team <team@example.org>\r\n",
            "Subject: =?UTF-8?B?U3RhdHVz?=\r\n",
            "  Update\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Hallo Welt\n"
        );

        let message = parse_rfc822("folded", "inbox", raw, true).expect("message should parse");
        assert_eq!(message.sender_name, "Müller");
        assert_eq!(message.sender_email, "mueller@example.org");
        assert_eq!(message.recipient_to, "Receiver <receiver@example.org>");
        assert_eq!(message.recipient_cc, "Team <team@example.org>");
        assert_eq!(message.reply_to, "Support <support@example.org>");
        assert_eq!(message.subject, "Status Update");
        assert_eq!(message.folder_id, "inbox");
        assert_eq!(
            message.raw_headers.as_deref(),
            Some(
                raw.split_once("\r\n\r\n")
                    .expect("header separator")
                    .0
                    .as_bytes()
            )
        );
        assert!(message.html_body.contains("Hallo Welt"));
        assert!(message.is_unread);
    }

    #[test]
    fn retains_original_bytes_only_for_crypto_messages() {
        let signed = b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n--b--\r\n";
        let message = parse_rfc822_bytes("signed", "inbox", signed, false)
            .expect("signed message should parse");
        assert_eq!(message.raw_mime.as_deref(), Some(signed.as_slice()));

        let ordinary = parse_rfc822_bytes(
            "ordinary",
            "inbox",
            b"Content-Type: text/plain\r\n\r\nHello\r\n",
            false,
        )
        .expect("ordinary message should parse");
        assert!(ordinary.raw_mime.is_none());
        assert_eq!(
            ordinary.raw_headers.as_deref(),
            Some(b"Content-Type: text/plain".as_slice())
        );
    }

    #[test]
    fn formatted_header_fields_preserve_order_repeats_and_folded_values() {
        let fields = parse_rfc822_header_fields(
            b"Received: first\r\nReceived: second\r\nSubject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\n\tfrom Evolution\r\nX-Unknown: kept",
        );

        assert_eq!(
            fields,
            vec![
                ("Received".to_string(), "first".to_string()),
                ("Received".to_string(), "second".to_string()),
                ("Subject".to_string(), "Grüße from Evolution".to_string()),
                ("X-Unknown".to_string(), "kept".to_string()),
            ]
        );
    }

    #[test]
    fn decodes_legacy_mime_charsets_in_headers_and_text() {
        assert_eq!(decode_mime_header("=?ISO-8859-1?Q?M=FCller?="), "Müller");
        assert_eq!(decode_mime_header("=?windows-1252?B?gA==?="), "€");
        assert_eq!(
            decode_mime_bytes(&[b'G', b'r', 0xfc, b's', b'e'], Some("iso-8859-1")),
            "Grüse"
        );
    }

    #[test]
    fn prefers_usable_html_over_plaintext_in_multipart_alternative() {
        let raw = concat!(
            "From: Sender <sender@example.invalid>\n",
            "Subject: Alternative example\n",
            "Content-Type: multipart/alternative; boundary=alternative42\n",
            "\n",
            "--alternative42\n",
            "Content-Type: text/plain; charset=utf-8\n",
            "Content-Transfer-Encoding: quoted-printable\n",
            "\n",
            "Fallback text line 1=0Aline 2\n",
            "--alternative42\n",
            "Content-Type: text/html; charset=utf-8\n",
            "Content-Transfer-Encoding: base64\n",
            "\n",
            "PHA+SFRNTCBwcmVmZXJyZWQ8L3A+\n",
            "--alternative42--\n"
        );

        let message = parse_rfc822("alternative", "inbox", raw, false)
            .expect("alternative message should parse");
        assert_eq!(message.html_body, "<p>HTML preferred</p>");
        assert_eq!(message.preview_snippet, "Fallback text line 1 line 2");
    }

    #[test]
    fn falls_back_to_readable_plaintext_for_empty_or_broken_html() {
        let raw = concat!(
            "From: Sender <sender@example.invalid>\n",
            "Subject: Plaintext fallback\n",
            "Content-Type: multipart/alternative; boundary=fallback42\n",
            "\n",
            "--fallback42\n",
            "Content-Type: text/html; charset=utf-8\n",
            "\n",
            "<!-- broken HTML without a closing comment\n",
            "--fallback42\n",
            "Content-Type: text/plain; charset=iso-8859-1\n",
            "Content-Transfer-Encoding: quoted-printable\n",
            "\n",
            "Zeile 1=0D=0AZeile 2: Caf=E9\n",
            "--fallback42--\n"
        );

        let message =
            parse_rfc822("fallback", "inbox", raw, false).expect("fallback message should parse");
        assert_eq!(message.html_body, "<pre>Zeile 1\nZeile 2: Café</pre>");
        assert_eq!(message.preview_snippet, "Zeile 1 Zeile 2: Café");
    }

    #[test]
    fn keeps_eight_bit_body_bytes_until_charset_decoding() {
        let mut raw = b"From: Sender <sender@example.invalid>\n\
            Subject: Eight bit body\n\
            Content-Type: text/plain; charset=iso-8859-1\n\
            Content-Transfer-Encoding: 8bit\n\
            \n\
            Caf"
        .to_vec();
        raw.push(0xe9);
        raw.extend_from_slice(b"\n");

        let message = parse_rfc822_bytes("eight-bit", "inbox", &raw, false)
            .expect("eight-bit message should parse");
        assert_eq!(message.html_body, "<pre>Café</pre>");
    }

    #[test]
    fn parses_nested_multiparts_and_preserves_inline_and_regular_attachments() {
        let raw = concat!(
            "From: Sender <sender@example.invalid>\n",
            "Subject: Nested multipart example\n",
            "Content-Type: multipart/mixed; boundary=outer42\n",
            "\n",
            "--outer42\n",
            "Content-Type: multipart/related; boundary=related42\n",
            "\n",
            "--related42\n",
            "Content-Type: multipart/alternative; boundary=alternative42\n",
            "\n",
            "--alternative42\n",
            "Content-Type: text/plain; charset=utf-8\n",
            "\n",
            "Plain fallback\n",
            "--alternative42\n",
            "Content-Type: text/html; charset=windows-1252\n",
            "Content-Transfer-Encoding: quoted-printable\n",
            "\n",
            "<p>Price: =80</p>\n",
            "--alternative42--\n",
            "--related42\n",
            "Content-Type: image/png\n",
            "Content-ID: <inline42@example.invalid>\n",
            "Content-Disposition: inline\n",
            "Content-Transfer-Encoding: base64\n",
            "\n",
            "UE5H\n",
            "--related42--\n",
            "--outer42\n",
            "Content-Type: application/octet-stream\n",
            "Content-Disposition: attachment; filename=payload.bin\n",
            "Content-Transfer-Encoding: base64\n",
            "\n",
            "AQID\n",
            "--outer42--\n"
        );

        let message = parse_rfc822("nested", "inbox", raw, false)
            .expect("nested multipart message should parse");
        assert_eq!(message.html_body, "<p>Price: €</p>");
        assert_eq!(message.attachments.len(), 2);
        assert_eq!(
            message.attachments[0].content_id.as_deref(),
            Some("inline42@example.invalid")
        );
        assert_eq!(message.attachments[0].content.as_ref(), b"PNG");
        assert_eq!(message.attachments[1].filename, "payload.bin");
        assert_eq!(message.attachments[1].content.as_ref(), [1, 2, 3]);
    }

    #[test]
    fn decodes_complex_rfc2047_subjects_and_adjacent_encoded_words() {
        let subject =
            "=?UTF-8?Q?=E2=80=9ESEO-Spezialist=E2=80=9C:_Threesi?= xtymedia – SEO-Spezialist";
        assert_eq!(
            decode_mime_header(subject),
            "„SEO-Spezialist“: Threesi xtymedia – SEO-Spezialist"
        );

        let adjacent =
            "=?UTF-8?B?4oCeU0VPLVNwZXppYWxpc3TigJw6IA==?= =?UTF-8?B?VGhyZWVzaXh0eW1lZGlh?=";
        assert_eq!(
            decode_mime_header(adjacent),
            "„SEO-Spezialist“: Threesixtymedia"
        );
    }

    #[test]
    fn strips_css_style_blocks_from_preview_snippet() {
        let html = "<html><head><style>* { font-family: 'Glassdoor Sans', sans-serif; }</style></head><body><p>Echte Gehälter ansehen</p></body></html>";
        assert_eq!(text_preview(html), "Echte Gehälter ansehen");
    }

    #[test]
    fn handles_mislabeled_or_nested_multipart_streams_without_leaking_boundaries() {
        let raw = concat!(
            "From: LinkedIn <jobalerts-noreply@linkedin.com>\n",
            "Subject: =?UTF-8?Q?=E2=80=9ESEO-Spezialist=E2=80=9C:_Threesi?= xtymedia\n",
            "Content-Type: text/plain; charset=UTF-8\n",
            "\n",
            "------=_Part_10938037_701289309\n",
            "Content-Type: text/plain; charset=UTF-8\n",
            "\n",
            "Alle Jobs auf LinkedIn anzeigen\n",
            "------=_Part_10938037_701289309\n",
            "Content-Type: text/html; charset=UTF-8\n",
            "\n",
            "<html><body><p>Alle Jobs auf LinkedIn anzeigen</p></body></html>\n",
            "------=_Part_10938037_701289309--\n"
        );

        let message = parse_rfc822("linkedin", "inbox", raw, false)
            .expect("linkedin multipart stream should parse");
        assert_eq!(message.subject, "„SEO-Spezialist“: Threesi xtymedia");
        assert_eq!(
            message.html_body,
            "<html><body><p>Alle Jobs auf LinkedIn anzeigen</p></body></html>"
        );
        assert!(!message.html_body.contains("------=_Part"));
        assert_eq!(message.preview_snippet, "Alle Jobs auf LinkedIn anzeigen");
    }

    #[test]
    fn parses_message_thread_headers_and_assigns_transitive_membership() {
        let root = parse_rfc822(
            "root",
            "inbox",
            concat!(
                "Message-ID: <root@example.org>\n",
                "Subject: Project\n\n",
                "Root"
            ),
            false,
        )
        .expect("root message should parse");
        let reply = parse_rfc822(
            "reply",
            "inbox",
            concat!(
                "Message-ID: <reply@example.org>\n",
                "References: <root@example.org>\n",
                "Subject: Re: Project\n\n",
                "Reply"
            ),
            false,
        )
        .expect("reply message should parse");
        let nested_reply = parse_rfc822(
            "nested",
            "inbox",
            concat!(
                "Message-ID: <nested@example.org>\n",
                "In-Reply-To: <reply@example.org>\n",
                "Subject: Re: Project\n\n",
                "Nested reply"
            ),
            false,
        )
        .expect("nested reply should parse");

        let mut messages = vec![root, reply, nested_reply];
        assign_thread_metadata(&mut messages);

        assert_eq!(messages[0].message_id.as_deref(), Some("root@example.org"));
        assert_eq!(messages[0].thread_id, "root@example.org");
        assert_eq!(messages[1].thread_id, "root@example.org");
        assert_eq!(messages[2].thread_id, "root@example.org");
        assert_eq!(messages[0].thread_size, 3);
        assert_eq!(messages[2].thread_position, 2);
    }

    #[test]
    fn parses_multipart_html_and_attachment() {
        let raw = concat!(
            "From: Sender <sender@example.org>\n",
            "To: Receiver <receiver@example.org>\n",
            "Subject: Report\n",
            "Content-Type: multipart/mixed; boundary=boundary42\n",
            "\n",
            "--boundary42\n",
            "Content-Type: text/html; charset=utf-8\n",
            "\n",
            "<p>Report</p>\n",
            "--boundary42\n",
            "Content-Type: application/pdf\n",
            "Content-Disposition: attachment; filename=report.pdf\n",
            "Content-Transfer-Encoding: base64\n",
            "\n",
            "SGk=\n",
            "--boundary42--\n"
        );

        let message = parse_rfc822("multipart", "sent", raw, false).expect("message should parse");
        assert!(message.html_body.contains("<p>Report</p>"));
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].filename, "report.pdf");
        assert_eq!(message.attachments[0].content.as_ref(), b"Hi");
        assert_eq!(message.folder_id, "sent");
    }

    #[test]
    fn decodes_rfc2231_attachment_filename() {
        let raw = concat!(
            "From: Sender <sender@example.org>\n",
            "Subject: International attachment\n",
            "Content-Type: multipart/mixed; boundary=boundary43\n",
            "\n",
            "--boundary43\n",
            "Content-Type: application/pdf\n",
            "Content-Disposition: attachment; filename*=UTF-8''rapport%20%C3%A4.pdf\n",
            "\n",
            "PDF\n",
            "--boundary43--\n"
        );

        let message = parse_rfc822("rfc2231", "inbox", raw, false).expect("message should parse");
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].filename, "rapport ä.pdf");
    }

    #[test]
    fn joins_rfc2231_attachment_filename_continuations() {
        let raw = concat!(
            "From: Sender <sender@example.org>\n",
            "Subject: Long attachment\n",
            "Content-Type: multipart/mixed; boundary=boundary44\n",
            "\n",
            "--boundary44\n",
            "Content-Type: application/octet-stream\n",
            "Content-Disposition: attachment; filename*0*=UTF-8''very%20long%20; filename*1*=report.pdf\n",
            "\n",
            "data\n",
            "--boundary44--\n"
        );

        let message = parse_rfc822("rfc2231-continuation", "inbox", raw, false)
            .expect("message should parse");
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].filename, "very long report.pdf");
    }

    #[test]
    fn retains_content_id_for_inline_image_parts() {
        let raw = concat!(
            "From: Sender <sender@example.org>\n",
            "To: Receiver <receiver@example.org>\n",
            "Subject: Inline image\n",
            "Content-Type: multipart/related; boundary=related42\n",
            "\n",
            "--related42\n",
            "Content-Type: text/html; charset=utf-8\n",
            "\n",
            "<img src=\"cid:logo@example.org\">\n",
            "--related42\n",
            "Content-Type: image/png\n",
            "Content-ID: <logo@example.org>\n",
            "Content-Disposition: inline\n",
            "Content-Transfer-Encoding: base64\n",
            "\n",
            "UE5H\n",
            "--related42--\n"
        );

        let message = parse_rfc822("inline", "inbox", raw, false).expect("message should parse");
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(
            message.attachments[0].content_id.as_deref(),
            Some("logo@example.org")
        );
        assert_eq!(message.attachments[0].mime_type, "image/png");
        assert_eq!(message.attachments[0].content.as_ref(), b"PNG");
    }

    #[test]
    fn splits_mbox_into_individual_messages() {
        let raw = concat!(
            "From sender@example.org Mon Aug 10 12:00:00 2026\n",
            "From: Sender <sender@example.org>\n",
            "Subject: First\n\nOne\n",
            "From sender@example.org Mon Aug 10 12:01:00 2026\n",
            "From: Sender <sender@example.org>\n",
            "Subject: Second\n\nTwo\n"
        );

        let messages = parse_mbox(Path::new("Inbox"), raw, true, None);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].subject, "First");
        assert_eq!(messages[1].subject, "Second");
    }

    #[test]
    fn missing_mailbox_does_not_create_demo_data() {
        let service = MailStoreService::with_mail_dir(
            std::env::temp_dir().join("evolution-gtk4-nonexistent-mailbox"),
        );
        assert!(service.load_local_messages().is_empty());
    }

    #[test]
    fn worker_safe_constructor_preserves_env_settings_default_and_state_policy() {
        let root = temporary_directory("worker-safe-constructor");
        let env_mail_dir = root.join("env-mail");
        let app_data = crate::services::app_data_dir();
        let external_settings_dir = app_data
            .with_file_name("evolution-gtk4-external")
            .join("mail");
        let app_owned_settings_dir = app_data.join("worker-safe-owned");
        let state_path = root.join("mail-state.json");

        let env_store = MailStoreService::new_worker_safe(MailStoreWorkerConfig {
            mail_dir_override: Some(env_mail_dir.clone()),
            local_mail_directory: Some(external_settings_dir.to_string_lossy().into_owned()),
            metadata_path_override: Some(state_path.clone()),
            app_data_directory: app_data.clone(),
        });
        assert_eq!(env_store.mail_dir, env_mail_dir);
        assert!(!env_store.writable);
        assert_eq!(env_store.metadata_path(), state_path);

        // Preserve the old `var_os(...).map(...)` behavior: even an
        // explicitly empty environment override still wins over settings.
        let empty_env_store = MailStoreService::new_worker_safe(MailStoreWorkerConfig {
            mail_dir_override: Some(PathBuf::new()),
            local_mail_directory: Some(app_owned_settings_dir.to_string_lossy().into_owned()),
            metadata_path_override: None,
            app_data_directory: app_data.clone(),
        });
        assert_eq!(empty_env_store.mail_dir, PathBuf::new());
        assert!(!empty_env_store.writable);

        let external_store = MailStoreService::new_worker_safe(MailStoreWorkerConfig {
            mail_dir_override: None,
            local_mail_directory: Some(external_settings_dir.to_string_lossy().into_owned()),
            metadata_path_override: None,
            app_data_directory: app_data.clone(),
        });
        assert_eq!(external_store.mail_dir, external_settings_dir);
        assert!(!external_store.writable);
        assert_eq!(external_store.import_destination(), app_data.join("mail"));

        let empty_setting_store = MailStoreService::new_worker_safe(MailStoreWorkerConfig {
            mail_dir_override: None,
            local_mail_directory: Some("  ".to_string()),
            metadata_path_override: None,
            app_data_directory: app_data.clone(),
        });
        assert_eq!(
            empty_setting_store.mail_dir,
            crate::services::app_data_dir().join("mail")
        );
        assert!(empty_setting_store.writable);

        let app_owned_store = MailStoreService::new_worker_safe(MailStoreWorkerConfig {
            mail_dir_override: None,
            local_mail_directory: Some(app_owned_settings_dir.to_string_lossy().into_owned()),
            metadata_path_override: None,
            app_data_directory: app_data.clone(),
        });
        assert_eq!(app_owned_store.mail_dir, app_owned_settings_dir);
        assert!(app_owned_store.writable);
        assert_eq!(app_owned_store.import_destination(), app_owned_settings_dir);

        let default_store = MailStoreService::new_worker_safe(MailStoreWorkerConfig {
            mail_dir_override: None,
            local_mail_directory: None,
            metadata_path_override: None,
            app_data_directory: app_data.clone(),
        });
        assert_eq!(
            default_store.mail_dir,
            crate::services::app_data_dir().join("mail")
        );
        assert!(default_store.writable);
        assert_eq!(
            default_store.metadata_path(),
            crate::services::app_data_dir().join("mail-state.json")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_mail_files_without_overwriting_existing_files() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-import-mail-{}",
            uuid::Uuid::new_v4()
        ));
        let source_dir = root.join("source");
        let destination_dir = root.join("destination");
        fs::create_dir_all(&source_dir).expect("source directory should be created");
        fs::create_dir_all(&destination_dir).expect("destination directory should be created");
        let source = source_dir.join("message.eml");
        fs::write(&source, "From: sender@example.org\nSubject: Test\n\nBody")
            .expect("source message should be written");
        fs::write(destination_dir.join("message.eml"), b"existing")
            .expect("existing message should be written");

        let imported = MailStoreService::with_mail_dir(destination_dir.clone())
            .import_files(std::slice::from_ref(&source))
            .expect("mail should import");
        assert_eq!(imported, 1);
        assert_eq!(
            fs::read(destination_dir.join("message.eml")).unwrap(),
            b"existing"
        );
        assert!(destination_dir.join("message-1.eml").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_import_into_read_only_mail_store() {
        let root = temporary_directory("read-only-mail-import");
        let source = root.join("source.eml");
        let destination = root.join("evolution-mail");
        fs::write(&source, "From: sender@example.org\nSubject: Test\n\nBody")
            .expect("source message should be written");

        let error = MailStoreService::with_read_only_mail_dir(&destination)
            .import_files(std::slice::from_ref(&source))
            .expect_err("read-only stores must reject imports");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!destination.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_maildir_directories_and_preserves_seen_flags() {
        let root = temporary_directory("import-maildir");
        let source_dir = root.join("source-maildir");
        let destination_dir = root.join("destination-maildir");
        fs::create_dir_all(source_dir.join("cur")).expect("maildir cur should be created");
        fs::create_dir_all(source_dir.join("tmp")).expect("maildir tmp should be created");
        fs::write(
            source_dir.join("cur/123:2,S"),
            "From: sender@example.org\nSubject: Seen\n\nBody",
        )
        .expect("seen mail should be written");
        fs::write(source_dir.join("tmp/partial"), b"not a delivered message")
            .expect("maildir temporary file should be written");

        let service = MailStoreService::with_mail_dir(destination_dir.clone());
        assert_eq!(
            service
                .import_files(std::slice::from_ref(&source_dir))
                .expect("maildir should import"),
            1
        );
        assert!(destination_dir.join("cur/123:2,S").is_file());
        assert!(!destination_dir.join("tmp/partial").exists());
        let records = service.load_local_records();
        assert_eq!(records.len(), 1);
        assert!(!records[0].is_unread);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_nested_maildir_directories_without_flattening_folders() {
        let root = temporary_directory("import-nested-maildir");
        let source_dir = root.join("source-maildir");
        let destination_dir = root.join("destination-maildir");
        let source = source_dir.join(".Projects/.2026/cur/123:2,S");
        fs::create_dir_all(source.parent().expect("nested maildir parent should exist"))
            .expect("nested maildir should be created");
        fs::write(&source, "From: sender@example.org\nSubject: Nested\n\nBody")
            .expect("nested mail should be written");

        let service = MailStoreService::with_mail_dir_and_state_path(
            &destination_dir,
            root.join("mail-state.json"),
        );
        assert_eq!(
            service
                .import_files_from_root(&source_dir, std::slice::from_ref(&source))
                .expect("nested maildir should import"),
            1
        );
        assert!(
            destination_dir
                .join(".Projects/.2026/cur/123:2,S")
                .is_file()
        );
        let records = service.load_local_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].folder_id, "custom:Projects/2026");
        assert!(
            service
                .local_folder_definitions()
                .contains("custom:Projects/2026")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loads_evolution_sbd_trees_and_mbox_extensions_with_folder_ids() {
        let root = temporary_directory("nested-evolution-mbox");
        let mail_dir = root.join("mail");
        let nested = mail_dir.join("Inbox.sbd/Projects.mbox");
        let archive = mail_dir.join("Archive.mbox");
        fs::create_dir_all(nested.parent().expect("sbd parent should exist"))
            .expect("sbd folder should be created");
        let mbox = concat!(
            "From sender@example.org Mon Aug 10 12:00:00 2026\n",
            "From: Sender <sender@example.org>\n",
            "Subject: Nested mbox\n\nNested\n"
        );
        fs::write(&nested, mbox).expect("nested mbox should be written");
        fs::write(
            &archive,
            concat!(
                "From archive@example.org Mon Aug 10 12:01:00 2026\n",
                "From: Archive <archive@example.org>\n",
                "Subject: Archive mbox\n\nArchive\n"
            ),
        )
        .expect("archive mbox should be written");

        let records =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("mail-state.json"))
                .load_local_records();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .any(|record| record.folder_id == "custom:Inbox/Projects")
        );
        assert!(records.iter().any(|record| record.folder_id == "archive"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incoming_filter_applies_only_supported_local_actions() {
        let mut message = parse_rfc822(
            "filter",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody",
            true,
        )
        .expect("message should parse");
        let rule = MailFilterRule {
            id: "rule-1".to_string(),
            name: "Mark invoices".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MarkRead,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };

        let _ = apply_local_filters(&[rule], &mut message, None);

        assert!(!message.is_unread);
    }

    #[test]
    fn stop_processing_prevents_following_matching_rules() {
        let mut message = parse_rfc822(
            "filter-stop",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody",
            true,
        )
        .expect("message should parse");
        let make_rule = |id: &str, action: FilterAction| MailFilterRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        let first_rule = MailFilterRule {
            actions: vec![FilterAction::MarkRead, FilterAction::StopProcessing],
            ..make_rule("stop-after-read", FilterAction::MarkRead)
        };
        let second_rule = make_rule("mark-unread", FilterAction::MarkUnread);

        let _ = apply_local_filters(&[first_rule, second_rule], &mut message, None);

        assert!(!message.is_unread);
    }

    #[test]
    fn manual_filter_run_persists_portable_actions_and_status() {
        let root = temporary_directory("manual-filter-run");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("message.eml"),
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody\n",
        )
        .expect("mail should be written");

        let filter_store = MailFilterStore::new(root.join("filters.json"));
        filter_store
            .upsert(MailFilterRule {
                id: "manual-invoice".to_string(),
                name: "Invoice manual run".to_string(),
                enabled: true,
                scope: FilterScope::Manual,
                condition: FilterCondition {
                    field: FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: FilterOperator::Contains,
                },
                action: FilterAction::MarkRead,
                conditions: Vec::new(),
                actions: vec![
                    FilterAction::MarkRead,
                    FilterAction::SetFlagged,
                    FilterAction::SetImportant,
                    FilterAction::MoveToFolder("archive".to_string()),
                    FilterAction::CopyToFolder("sent".to_string()),
                ],
                match_all: true,
            })
            .expect("manual rule should persist");

        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("mail-state.json"));
        let message_id = store
            .load_raw_local_records_cancelable(&AtomicBool::new(false))
            .into_iter()
            .next()
            .expect("message should parse")
            .id;
        let report = store
            .run_filter_rule_with_store("manual-invoice", &filter_store)
            .expect("manual rule should run")
            .expect("saved rule should be found");
        assert_eq!(report.matched_messages, 1);
        assert_eq!(report.changed_messages, 1);
        assert_eq!(report.moved_messages, 1);
        assert_eq!(report.copied_messages, 1);
        assert_eq!(report.deleted_messages, 0);

        let metadata = MailMetadataStore::new(store.metadata_path());
        assert_eq!(
            metadata.load_folder_overrides().get(&message_id),
            Some(&"archive".to_string())
        );
        assert!(
            metadata
                .load_copies()
                .get(&message_id)
                .is_some_and(|folders| folders.contains("sent"))
        );
        assert_eq!(
            metadata.load().get(&message_id),
            Some(&MailMessageState {
                is_unread: false,
                is_flagged: true,
                is_important: true,
            })
        );

        let records = store.load_local_records();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| !record.is_unread));
        assert!(records.iter().all(|record| record.is_flagged));
        assert!(records.iter().all(|record| record.is_important));
        assert!(records.iter().any(|record| record.folder_id == "archive"));
        assert!(records.iter().any(|record| record.folder_id == "sent"));

        let mut clear_rule = filter_store
            .load()
            .into_iter()
            .next()
            .expect("saved rule should be available for editing");
        clear_rule.id = "manual-clear-important".to_string();
        clear_rule.name = "Clear important status".to_string();
        clear_rule.action = FilterAction::ClearImportant;
        clear_rule.actions = vec![FilterAction::ClearImportant];
        filter_store
            .upsert(clear_rule)
            .expect("clear rule should persist");
        let clear_report = store
            .run_filter_rule_with_store("manual-clear-important", &filter_store)
            .expect("clear rule should run")
            .expect("clear rule should be found");
        assert_eq!(clear_report.changed_messages, 1);
        assert_eq!(
            metadata.load().get(&message_id).copied(),
            Some(MailMessageState {
                is_unread: false,
                is_flagged: true,
                is_important: false,
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incoming_header_filter_uses_parsed_rfc822_headers() {
        let mut message = parse_rfc822(
            "header-filter",
            "inbox",
            concat!(
                "From: Sender <sender@example.org>\n",
                "To: recipient@example.org\n",
                "Cc: Billing <billing@example.org>\n",
                "Subject: Status\n\nBody"
            ),
            true,
        )
        .expect("message should parse");
        let rule = MailFilterRule {
            id: "rule-header".to_string(),
            name: "Billing headers".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Header,
                value: "billing@example.org".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::SetFlagged,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };

        let _ = apply_local_filters(&[rule], &mut message, None);

        assert!(message.is_flagged);
    }

    #[test]
    fn incoming_filter_matches_important_and_attachment_state() {
        let mut message = parse_rfc822(
            "status-filter",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody",
            true,
        )
        .expect("message should parse");
        message.is_important = true;
        message.attachments.push(AttachmentInfo {
            filename: "invoice.pdf".to_string(),
            size_formatted: "1 KiB".to_string(),
            mime_type: "application/pdf".to_string(),
            content_id: None,
            content: vec![1, 2, 3].into(),
        });

        let make_rule =
            |field: FilterField, value: &str, operator: FilterOperator, action| MailFilterRule {
                id: "status-rule".to_string(),
                name: "Status rule".to_string(),
                enabled: true,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field,
                    value: value.to_string(),
                    operator,
                },
                action,
                conditions: Vec::new(),
                actions: Vec::new(),
                match_all: true,
            };

        let important_rule = make_rule(
            FilterField::Important,
            "ja",
            FilterOperator::Equals,
            FilterAction::SetFlagged,
        );
        let _ = apply_local_filters(&[important_rule], &mut message, None);
        assert!(message.is_flagged);

        message.is_important = false;
        message.is_flagged = false;
        let attachment_rule = make_rule(
            FilterField::Attachment,
            "",
            FilterOperator::Exists,
            FilterAction::SetImportant,
        );
        let _ = apply_local_filters(&[attachment_rule], &mut message, None);
        assert!(message.is_important);
    }

    #[test]
    fn local_folder_and_delete_filters_persist_in_the_app_sidecar() {
        let root = temporary_directory("local-filter-sidecar");
        let metadata = MailMetadataStore::new(root.join("state.json"));
        let mut message = parse_rfc822(
            "filter-sidecar",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody",
            true,
        )
        .expect("message should parse");
        let move_rule = MailFilterRule {
            id: "move-rule".to_string(),
            name: "Archive invoices".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MoveToFolder("archive".to_string()),
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        assert!(
            !apply_client_state(&HashMap::new(), &[move_rule], &mut message, Some(&metadata),)
                .deleted
        );
        assert_eq!(message.folder_id, "archive");
        assert_eq!(
            metadata.load_folder_overrides().get("filter-sidecar"),
            Some(&"archive".to_string())
        );

        let copy_rule = MailFilterRule {
            id: "copy-rule".to_string(),
            name: "Keep an archive copy".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::CopyToFolder("sent".to_string()),
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        assert!(
            !apply_client_state(&HashMap::new(), &[copy_rule], &mut message, Some(&metadata),)
                .deleted
        );
        assert!(
            metadata
                .load_copies()
                .get("filter-sidecar")
                .is_some_and(|folders| folders.contains("sent"))
        );

        let delete_rule = MailFilterRule {
            id: "delete-rule".to_string(),
            name: "Delete invoices".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::Delete,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        assert!(
            apply_client_state(
                &HashMap::new(),
                &[delete_rule],
                &mut message,
                Some(&metadata),
            )
            .deleted
        );
        assert!(metadata.load_deleted().contains("filter-sidecar"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn junk_filter_actions_use_local_folder_semantics() {
        let root = temporary_directory("junk-filter-actions");
        let metadata = MailMetadataStore::new(root.join("state.json"));
        let mut message = parse_rfc822(
            "junk-filter",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Suspicious\n\nBody",
            true,
        )
        .expect("message should parse");
        let make_rule = |action: FilterAction| MailFilterRule {
            id: "junk-rule".to_string(),
            name: "Junk rule".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "suspicious".to_string(),
                operator: FilterOperator::Contains,
            },
            action,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };

        assert!(
            !apply_client_state(
                &HashMap::new(),
                &[make_rule(FilterAction::SetJunk)],
                &mut message,
                Some(&metadata),
            )
            .deleted
        );
        assert_eq!(message.folder_id, "junk");

        assert!(
            !apply_client_state(
                &HashMap::new(),
                &[make_rule(FilterAction::ClearJunk)],
                &mut message,
                Some(&metadata),
            )
            .deleted
        );
        assert_eq!(message.folder_id, "inbox");
        assert_eq!(
            metadata.load_folder_overrides().get("junk-filter"),
            Some(&"inbox".to_string())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn label_filter_actions_persist_and_update_local_messages() {
        let root = temporary_directory("label-filter-actions");
        let metadata = MailMetadataStore::new(root.join("state.json"));
        let mut message = parse_rfc822(
            "label-filter",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Suspicious\n\nBody",
            true,
        )
        .expect("message should parse");
        let make_rule = |action: FilterAction| MailFilterRule {
            id: "label-rule".to_string(),
            name: "Label rule".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "suspicious".to_string(),
                operator: FilterOperator::Contains,
            },
            action,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };

        assert!(
            !apply_client_state(
                &HashMap::new(),
                &[make_rule(FilterAction::SetLabel("Work".to_string()))],
                &mut message,
                Some(&metadata),
            )
            .deleted
        );
        assert_eq!(message.labels, vec!["Work"]);
        assert!(
            metadata
                .load_labels()
                .get("label-filter")
                .is_some_and(|labels| labels.contains("Work"))
        );

        assert!(
            !apply_client_state(
                &HashMap::new(),
                &[make_rule(FilterAction::ClearLabel("work".to_string()))],
                &mut message,
                Some(&metadata),
            )
            .deleted
        );
        assert!(message.labels.is_empty());
        assert!(metadata.load_labels().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_sidecar_copies_are_loaded_as_separate_logical_records() {
        let root = temporary_directory("local-copy-records");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("message.eml"),
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");
        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let original = store
            .load_local_records()
            .into_iter()
            .next()
            .expect("message should parse");
        MailMetadataStore::new(store.metadata_path())
            .set_copy(&original.id, "archive", true)
            .expect("copy should persist");

        let records = store.load_local_records();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .any(|record| record.folder_id == "archive" && record.id.contains("::copy::"))
        );
        assert!(mail_dir.join("message.eml").is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_local_trash_marks_all_source_and_copy_records_without_rewriting_mail() {
        let root = temporary_directory("empty-local-trash");
        let mail_dir = root.join("mail");
        fs::create_dir_all(mail_dir.join("Trash")).expect("trash folder should be created");
        fs::write(
            mail_dir.join("Trash/deleted.eml"),
            "From: Sender <sender@example.org>\nSubject: Deleted\n\nBody\n",
        )
        .expect("trash mail should be written");
        fs::write(
            mail_dir.join("inbox.eml"),
            "From: Sender <sender@example.org>\nSubject: Inbox\n\nBody\n",
        )
        .expect("inbox mail should be written");
        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let inbox_id = store
            .load_local_records()
            .into_iter()
            .find(|message| message.folder_id == "inbox")
            .expect("inbox message should parse")
            .id;
        MailMetadataStore::new(store.metadata_path())
            .set_copy(&inbox_id, "trash", true)
            .expect("trash copy should persist");

        assert_eq!(store.empty_local_trash().expect("trash should empty"), 2);
        let deleted = MailMetadataStore::new(store.metadata_path()).load_deleted();
        assert!(deleted.iter().any(|id| id.contains("deleted.eml")));
        assert!(deleted.contains(&local_copy_id(&inbox_id, "trash")));
        assert!(mail_dir.join("Trash/deleted.eml").is_file());
        assert_eq!(store.load_local_records().len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_folder_lifecycle_preserves_source_files_and_rejects_non_empty_delete() {
        let root = temporary_directory("local-folder-lifecycle");
        let mail_dir = root.join("mail");
        let source_folder = mail_dir.join("Projects");
        fs::create_dir_all(&source_folder).expect("source folder should be created");
        let source_file = source_folder.join("message.eml");
        fs::write(
            &source_file,
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");

        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let empty = store
            .create_local_folder("Empty")
            .expect("empty folder should be created");
        assert!(store.local_folder_definitions().contains(&empty));
        let original = store
            .load_local_records()
            .into_iter()
            .next()
            .expect("message should parse");
        MailMetadataStore::new(store.metadata_path())
            .set_folder_override(&original.id, Some("custom:Projects/2026"))
            .expect("child placement should save");

        let renamed = store
            .rename_local_folder("custom:Projects", "Archive")
            .expect("message folder should be renamed");
        let records = store.load_local_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].folder_id, "custom:Archive/2026");
        assert_eq!(renamed, "custom:Archive");
        assert!(source_file.is_file());

        store
            .delete_local_folder(&empty)
            .expect("empty folder should be deleted");
        assert!(!store.local_folder_definitions().contains(&empty));
        let error = store
            .delete_local_folder(&renamed)
            .expect_err("non-empty folder deletion should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::DirectoryNotEmpty);
        assert!(source_file.is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_load_stops_before_reading_mail_files() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-cancelled-load-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("mail directory should be created");
        fs::write(
            root.join("message.eml"),
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");

        let cancelled = AtomicBool::new(true);
        let records =
            MailStoreService::with_mail_dir(root.clone()).load_local_records_cancelable(&cancelled);

        assert!(records.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_filter_store_controls_local_incoming_rules_and_order() {
        let root = temporary_directory("explicit-filter-store");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("invoice.eml"),
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody\n",
        )
        .expect("mail should be written");

        // A different candidate represents a changed environment/default
        // source.  The explicit worker path must not inspect it.
        let default_candidate = MailFilterStore::new(root.join("default-filters.json"));
        default_candidate
            .upsert(MailFilterRule {
                id: "default-delete".to_string(),
                name: "Default delete".to_string(),
                enabled: true,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field: FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: FilterOperator::Contains,
                },
                action: FilterAction::Delete,
                conditions: Vec::new(),
                actions: Vec::new(),
                match_all: true,
            })
            .expect("default candidate rule should persist");

        let filter_store = MailFilterStore::new(root.join("explicit-filters.json"));
        filter_store
            .upsert(MailFilterRule {
                id: "move-and-read".to_string(),
                name: "Move and mark invoice".to_string(),
                enabled: true,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field: FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: FilterOperator::Contains,
                },
                action: FilterAction::MoveToFolder("archive".to_string()),
                conditions: Vec::new(),
                actions: vec![
                    FilterAction::MarkRead,
                    FilterAction::MoveToFolder("archive".to_string()),
                ],
                match_all: true,
            })
            .expect("first explicit rule should persist");
        filter_store
            .upsert(MailFilterRule {
                id: "flag-archive".to_string(),
                name: "Flag archived invoice".to_string(),
                enabled: true,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field: FilterField::Folder,
                    value: "archive".to_string(),
                    operator: FilterOperator::Equals,
                },
                action: FilterAction::SetFlagged,
                conditions: Vec::new(),
                actions: Vec::new(),
                match_all: true,
            })
            .expect("second explicit rule should persist");

        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("mail-state.json"));
        let records =
            store.load_local_records_cancelable_with_store(&AtomicBool::new(false), &filter_store);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].folder_id, "archive");
        assert!(!records[0].is_unread);
        assert!(records[0].is_flagged);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_filter_store_preserves_cancellation_and_backend_incoming_effects() {
        let root = temporary_directory("explicit-backend-filter-store");
        let filter_store = MailFilterStore::new(root.join("explicit-filters.json"));
        filter_store
            .upsert(MailFilterRule {
                id: "mark-read".to_string(),
                name: "Mark invoice read".to_string(),
                enabled: true,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field: FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: FilterOperator::Contains,
                },
                action: FilterAction::MarkRead,
                conditions: Vec::new(),
                actions: Vec::new(),
                match_all: true,
            })
            .expect("explicit backend rule should persist");

        let store = MailStoreService::with_mail_dir_and_state_path(
            root.join("mail"),
            root.join("mail-state.json"),
        );
        let mut message = parse_rfc822(
            "backend-filter",
            "inbox",
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody",
            true,
        )
        .expect("message should parse");
        store.apply_backend_filters_with_store(std::slice::from_mut(&mut message), &filter_store);
        assert!(!message.is_unread);

        let cancelled = AtomicBool::new(true);
        let records = store.load_local_records_cancelable_with_store(&cancelled, &filter_store);
        assert!(records.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_filter_store_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<MailFilterStore>();
        assert_send_sync::<MailStoreService>();
    }

    #[test]
    fn run_all_active_filters_executes_enabled_rules() {
        let root = temporary_directory("all-active-filters");
        fs::write(
            root.join("invoice.eml"),
            "From: Billing <billing@example.org>\nSubject: Important Invoice\n\nInvoice body\n",
        )
        .expect("mail should be written");

        let filter_store = MailFilterStore::new(root.join("filters.json"));
        let rule = MailFilterRule {
            id: "invoice-rule".to_string(),
            name: "Archive Invoices".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "Invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MoveToFolder("archive".to_string()),
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        filter_store.upsert(rule).expect("rule should be saved");

        let service =
            MailStoreService::with_mail_dir_and_state_path(root.clone(), root.join("state.json"));
        let report = service
            .run_all_active_filters_with_store(&filter_store)
            .expect("filter run should succeed");
        assert_eq!(report.matched_messages, 1);
        assert_eq!(report.moved_messages, 1);

        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-mail-store-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
