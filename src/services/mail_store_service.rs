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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::models::mail_message::{AttachmentInfo, MailMessageObject};
use crate::services::mail_filter_service::{
    FilterAction, FilterOperator, FilterScope, MailFilterRule, MailFilterStore,
};
use crate::services::mail_metadata_service::{MailMessageState, MailMetadataStore};
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

#[derive(Debug, Clone)]
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
    pub is_unread: bool,
    pub is_flagged: bool,
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
            is_unread,
            is_flagged,
            backend_location,
            attachments,
            ..
        } = self;

        MailMessageObject::new(
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
            backend_location,
            attachments,
        )
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
}

impl MailStoreService {
    /// Create a service pointing at the user's existing Evolution local store.
    ///
    /// No directory is created and no sample data is written.  The optional
    /// `EVOLUTION_GTK4_MAIL_DIR` override is useful for tests and for users
    /// importing a local mailbox from another location.
    pub fn new() -> Self {
        let mail_dir = std::env::var_os("EVOLUTION_GTK4_MAIL_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                crate::services::settings_service::AppSettings::try_new()
                    .map(|settings| settings.local_mail_directory())
                    .filter(|path| !path.trim().is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution/mail/local"));
        let metadata_path = std::env::var_os("EVOLUTION_GTK4_MAIL_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("mail-state.json"));

        Self {
            mail_dir,
            metadata: MailMetadataStore::new(metadata_path),
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
        }
    }

    /// Copy user-selected EML, Maildir files or mbox files into the configured
    /// read-only import root.  The importer never overwrites an existing file;
    /// a numbered name is chosen instead so an import can be repeated safely.
    pub fn import_files(&self, files: &[PathBuf]) -> std::io::Result<usize> {
        fs::create_dir_all(&self.mail_dir)?;
        let mut sources = Vec::new();
        for source in files {
            if source.is_dir() {
                collect_import_files(source, &mut sources);
            } else if source.is_file() {
                sources.push(source.clone());
            }
        }
        sources.sort();
        sources.dedup();

        let mut imported = 0;
        for source in sources {
            let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let target_root = source
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .filter(|name| matches!(*name, "cur" | "new"))
                .map(|name| self.mail_dir.join(name))
                .unwrap_or_else(|| self.mail_dir.clone());
            fs::create_dir_all(&target_root)?;
            let mut destination = target_root.join(file_name);
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
            fs::copy(source, destination)?;
            imported += 1;
        }
        Ok(imported)
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
        let metadata = self.metadata.load();
        let folder_overrides = self.metadata.load_folder_overrides();
        let deleted = self.metadata.load_deleted();
        let filter_rules = MailFilterStore::default().load();
        let mut messages = self.load_raw_local_records_cancelable(cancelled);

        messages.retain(|message| !deleted.contains(&message.id));
        let mut filtered_out = std::collections::HashSet::new();
        for message in &mut messages {
            if let Some(folder_id) = folder_overrides.get(&message.id) {
                message.folder_id = folder_id.clone();
            }
            if apply_client_state(&metadata, &filter_rules, message, Some(&self.metadata)) {
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
                messages.extend(parse_mbox(&path, &text, unread));
            } else if looks_like_rfc822(&text, &path) {
                let id = message_id(&path, 0);
                let folder_id = folder_id_for_path(&path);
                if let Some(message) = parse_rfc822(&id, &folder_id, &text, unread) {
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
        self.metadata.update(
            id,
            MailMessageState {
                is_unread,
                is_flagged,
            },
        )
    }

    /// Apply application-owned read/flag state and local filter rules to
    /// records supplied by another local/read-only backend.
    #[cfg(eds_camel_bridge)]
    #[allow(dead_code)]
    pub(crate) fn apply_client_state(&self, messages: &mut [ParsedMail]) {
        let metadata = self.metadata.load();
        let filter_rules = MailFilterStore::default().load();

        for message in messages {
            let _ = apply_client_state(&metadata, &filter_rules, message, None);
        }
    }

    /// Apply only local incoming filter actions to messages whose flags are
    /// already authoritative in Camel/EDS.  Remote messages must not be
    /// overwritten by a stale client sidecar after a successful Camel
    /// mutation.
    #[cfg(eds_camel_bridge)]
    pub(crate) fn apply_backend_filters(&self, messages: &mut [ParsedMail]) {
        let filter_rules = MailFilterStore::default().load();

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
}

fn apply_client_state(
    metadata: &HashMap<String, MailMessageState>,
    filter_rules: &[MailFilterRule],
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
) -> bool {
    if let Some(state) = metadata.get(&message.id) {
        message.is_unread = state.is_unread;
        message.is_flagged = state.is_flagged;
    }

    apply_local_filters(filter_rules, message, sidecar)
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
    let body = format!("{} {}", message.preview_snippet, message.html_body);
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

    rule.matches(&message.subject, &sender, &recipient, &body, &headers)
}

fn apply_local_filters(
    rules: &[MailFilterRule],
    message: &mut ParsedMail,
    sidecar: Option<&MailMetadataStore>,
) -> bool {
    let mut deleted = false;
    for rule in rules {
        if rule.scope != FilterScope::Incoming || !message_matches_rule(rule, message) {
            continue;
        }
        let effects = apply_filter_rule(rule, message, sidecar);
        deleted |= effects.deleted;
        if effects.stop_processing {
            break;
        }
    }
    deleted
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
            FilterAction::StopProcessing => {
                effects.stop_processing = true;
                break;
            }
        }
    }
    effects
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
    if path.extension().and_then(|ext| ext.to_str()) == Some("eml") {
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

fn parse_mbox(path: &Path, raw: &str, unread: bool) -> Vec<ParsedMail> {
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
            let folder_id = folder_id_for_path(path);
            parse_rfc822(&id, &folder_id, &record[message_start..], unread)
        })
        .collect()
}

fn message_id(path: &Path, index: usize) -> String {
    format!("local:{}#{index}", path.to_string_lossy())
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
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let (header_text, body) = normalized
        .split_once("\n\n")
        .map(|(headers, body)| (headers, body))
        .unwrap_or((normalized.as_str(), ""));
    let headers = parse_headers(header_text);

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

    let mut html_body = String::new();
    let mut plain_body = String::new();
    let mut attachments = Vec::new();
    extract_mime_content(
        &headers,
        body,
        &mut html_body,
        &mut plain_body,
        &mut attachments,
    );

    let body_html = if !html_body.trim().is_empty() {
        html_body
    } else if !plain_body.trim().is_empty() {
        format!("<pre>{}</pre>", escape_html(plain_body.trim()))
    } else {
        "<p>(Keine Nachricht)</p>".to_string()
    };

    let preview = text_preview(if !plain_body.trim().is_empty() {
        plain_body.as_str()
    } else {
        body_html.as_str()
    });

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
        is_unread: unread,
        is_flagged: false,
        attachments,
    })
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

fn extract_mime_content(
    headers: &HashMap<String, String>,
    body: &str,
    html_body: &mut String,
    plain_body: &mut String,
    attachments: &mut Vec<AttachmentInfo>,
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
        if let Some(boundary) = mime_parameter(content_type, "boundary") {
            let marker = format!("--{boundary}");
            for part in body.split(&marker).skip(1) {
                if part.trim_start().starts_with("--") {
                    break;
                }
                let part =
                    part.trim_start_matches(|character| character == '\n' || character == '\r');
                let part =
                    part.trim_end_matches(|character| character == '\n' || character == '\r');
                let normalized = part.replace("\r\n", "\n").replace('\r', "\n");
                let (part_headers, part_body) = normalized
                    .split_once("\n\n")
                    .map(|(headers, body)| (parse_headers(headers), body))
                    .unwrap_or_else(|| (HashMap::new(), ""));
                extract_mime_content(&part_headers, part_body, html_body, plain_body, attachments);
            }
        }
        return;
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

    if let Some(filename) = filename.filter(|_| {
        disposition.to_ascii_lowercase().contains("attachment") || !media_type.starts_with("text/")
    }) {
        attachments.push(AttachmentInfo {
            filename: decode_mime_header(&filename),
            size_formatted: format_size(decoded.len()),
            mime_type: media_type,
            content_id,
            content: decoded,
        });
        return;
    }

    match media_type.as_str() {
        "text/html" => {
            if html_body.trim().is_empty() {
                *html_body = String::from_utf8_lossy(&decoded).into_owned();
            }
        }
        "text/plain" | "" => {
            if plain_body.trim().is_empty() {
                *plain_body = String::from_utf8_lossy(&decoded).into_owned();
            }
        }
        _ => {
            // A non-text part without a filename is still useful to expose as
            // an attachment, but use a stable fallback name for the UI.
            attachments.push(AttachmentInfo {
                filename: "attachment".to_string(),
                size_formatted: format_size(decoded.len()),
                mime_type: media_type,
                content_id,
                content: decoded,
            });
        }
    }
}

fn mime_parameter(value: &str, parameter: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        if name.trim().eq_ignore_ascii_case(parameter) {
            Some(value.trim().trim_matches('"').to_string())
        } else {
            None
        }
    })
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

fn decode_transfer_encoding(body: &str, encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => decode_base64(body).unwrap_or_else(|| body.as_bytes().to_vec()),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.as_bytes().to_vec(),
    }
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    let mut saw_data = false;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        saw_data = true;
        buffer = (buffer << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }

    saw_data.then_some(output)
}

fn decode_quoted_printable(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'=' {
            if index + 2 < bytes.len() && bytes[index + 1] == b'\n' {
                index += 2;
                continue;
            }
            if index + 3 < bytes.len() && bytes[index + 1] == b'\r' && bytes[index + 2] == b'\n' {
                index += 3;
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

fn decode_mime_header(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;

    while let Some(start) = rest.find("=?") {
        result.push_str(&rest[..start]);
        let encoded = &rest[start..];
        let Some(end) = encoded.find("?=") else {
            result.push_str(encoded);
            rest = "";
            break;
        };
        let word = &encoded[2..end];
        let mut fields = word.splitn(3, '?');
        let _charset = fields.next();
        let encoding = fields.next();
        let payload = fields.next();

        let decoded = match (encoding, payload) {
            (Some(encoding), Some(payload)) if encoding.eq_ignore_ascii_case("b") => {
                decode_base64(payload).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            }
            (Some(encoding), Some(payload)) if encoding.eq_ignore_ascii_case("q") => Some(
                String::from_utf8_lossy(&decode_quoted_printable(&payload.replace('_', " ")))
                    .into_owned(),
            ),
            _ => None,
        };

        if let Some(decoded) = decoded {
            result.push_str(&decoded);
        } else {
            result.push_str(&encoded[..end + 2]);
        }
        rest = &encoded[end + 2..];
    }

    result.push_str(rest);
    result.trim().to_string()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text_preview(value: &str) -> String {
    let mut text = String::new();
    let mut inside_tag = false;

    for character in value.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if inside_tag => {}
            character if character.is_whitespace() => {
                if !text.ends_with(' ') {
                    text.push(' ');
                }
            }
            character => text.push(character),
        }
    }

    let text = text.trim();
    let preview: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
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
        FilterAction, FilterCondition, FilterField, FilterScope,
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
        assert!(message.html_body.contains("Hallo Welt"));
        assert!(message.is_unread);
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
        assert_eq!(message.attachments[0].content, b"Hi");
        assert_eq!(message.folder_id, "sent");
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
        assert_eq!(message.attachments[0].content, b"PNG");
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

        let messages = parse_mbox(Path::new("Inbox"), raw, true);
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
            })
        );

        let records = store.load_local_records();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| !record.is_unread));
        assert!(records.iter().all(|record| record.is_flagged));
        assert!(records.iter().any(|record| record.folder_id == "archive"));
        assert!(records.iter().any(|record| record.folder_id == "sent"));

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
        assert!(!apply_client_state(
            &HashMap::new(),
            &[move_rule],
            &mut message,
            Some(&metadata),
        ));
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
        assert!(!apply_client_state(
            &HashMap::new(),
            &[copy_rule],
            &mut message,
            Some(&metadata),
        ));
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
        assert!(apply_client_state(
            &HashMap::new(),
            &[delete_rule],
            &mut message,
            Some(&metadata),
        ));
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

        assert!(!apply_client_state(
            &HashMap::new(),
            &[make_rule(FilterAction::SetJunk)],
            &mut message,
            Some(&metadata),
        ));
        assert_eq!(message.folder_id, "junk");

        assert!(!apply_client_state(
            &HashMap::new(),
            &[make_rule(FilterAction::ClearJunk)],
            &mut message,
            Some(&metadata),
        ));
        assert_eq!(message.folder_id, "inbox");
        assert_eq!(
            metadata.load_folder_overrides().get("junk-filter"),
            Some(&"inbox".to_string())
        );

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
