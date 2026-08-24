// SPDX-License-Identifier: GPL-3.0-or-later
//
// Application-owned draft persistence used until Camel/EDS Drafts are wired.
// Passwords and account secrets are intentionally never stored here.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::crypto_service::SmimeOptions;

const CURRENT_VERSION: u32 = 1;

// Draft saves can be started by the autosave timer, the explicit save action,
// and the send worker at the same time.  Serializing each read/modify/write
// transaction prevents torn files; optimistic snapshot checks below prevent a
// slower writer from replacing a newer snapshot.
static DRAFT_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static DRAFT_SEND_CLAIMS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn draft_store_lock() -> std::sync::MutexGuard<'static, ()> {
    DRAFT_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn draft_send_claims() -> std::sync::MutexGuard<'static, HashSet<String>> {
    DRAFT_SEND_CLAIMS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug)]
pub(crate) struct DraftSendClaim {
    key: String,
}

impl Drop for DraftSendClaim {
    fn drop(&mut self) {
        draft_send_claims().remove(&self.key);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMessage {
    pub id: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub source_uid: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub html_body: String,
    /// Whether `html_body` contains the plain-text composer representation.
    /// Older drafts default to rich HTML for backwards compatibility.
    #[serde(default)]
    pub plain_text: bool,
    pub updated_at: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    /// Optional iTIP payload for a portable invitation response draft.
    #[serde(default)]
    pub calendar_method: Option<String>,
    #[serde(default)]
    pub calendar_component: Option<String>,
    /// OpenPGP choices are stored as draft metadata, never as credentials.
    #[serde(default)]
    pub pgp_sign: bool,
    #[serde(default)]
    pub pgp_encrypt: bool,
    #[serde(default)]
    pub pgp_key: String,
    /// Legacy-compatible container for per-message composer options and
    /// optional S/MIME settings. See [`DraftComposerOptions`].
    #[serde(default)]
    pub(crate) smime_options: Option<DraftComposerOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub path: String,
    pub filename: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size: u64,
    /// Preserved MIME Content-ID for inline resources referenced by `cid:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// Base64-encoded application-owned payload. Ordinary user-selected files
    /// keep this empty and continue to use `path`; embedded RFC822 attachments
    /// survive autosave/reopen without relying on a temporary file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub embedded_content_base64: String,
}

/// Composer-only metadata kept in the recovery draft.
///
/// The outer JSON property is still called `smime_options` for compatibility
/// with the first draft-store format. The nested `smime` value contains the
/// existing S/MIME material, while the other values are per-message choices.
/// They must not be promoted to application defaults when a draft is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DraftComposerOptions {
    #[serde(default)]
    pub(crate) request_read_receipt: bool,
    #[serde(default)]
    pub(crate) priority: bool,
    #[serde(default = "default_charset")]
    pub(crate) charset: String,
    #[serde(default)]
    pub(crate) smime: Option<SmimeOptions>,
}

impl Default for DraftComposerOptions {
    fn default() -> Self {
        Self {
            request_read_receipt: false,
            priority: false,
            charset: default_charset(),
            smime: None,
        }
    }
}

impl<'de> Deserialize<'de> for DraftComposerOptions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Current {
            #[serde(default)]
            request_read_receipt: bool,
            #[serde(default)]
            priority: bool,
            #[serde(default = "default_charset")]
            charset: String,
            #[serde(default)]
            smime: Option<SmimeOptions>,
        }

        #[derive(Debug, Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Current(Current),
            Legacy(SmimeOptions),
        }

        match Stored::deserialize(deserializer)? {
            Stored::Current(options) => Ok(Self {
                request_read_receipt: options.request_read_receipt,
                priority: options.priority,
                charset: options.charset,
                smime: options.smime,
            }),
            Stored::Legacy(smime) => Ok(Self {
                smime: Some(smime),
                ..Self::default()
            }),
        }
    }
}

fn default_charset() -> String {
    "UTF-8".to_string()
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
            .unwrap_or_else(|| crate::services::app_data_dir().join("drafts.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<DraftMessage> {
        let _lock = draft_store_lock();
        match self.load_unlocked() {
            Ok(drafts) => drafts,
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "Could not load draft store");
                Vec::new()
            }
        }
    }

    /// Load one recoverable draft by its stable identifier.
    ///
    /// The lookup is intentionally performed against the same serialized
    /// store transaction as saves and deletes, so a composer never restores a
    /// half-written snapshot.
    #[allow(dead_code)]
    pub fn find(&self, id: &str) -> Option<DraftMessage> {
        let _lock = draft_store_lock();
        match self.load_unlocked() {
            Ok(drafts) => drafts.into_iter().find(|draft| draft.id == id),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "Could not load draft store");
                None
            }
        }
    }

    fn load_unlocked(&self) -> io::Result<Vec<DraftMessage>> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let file = serde_json::from_slice::<DraftFile>(&raw).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid draft store {}: {error}", self.path.display()),
            )
        })?;
        if file.version != CURRENT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported draft store version {} in {} (expected {})",
                    file.version,
                    self.path.display(),
                    CURRENT_VERSION
                ),
            ));
        }
        Ok(file.drafts)
    }

    pub fn save(&self, draft: DraftMessage) -> io::Result<()> {
        let _lock = draft_store_lock();
        let mut drafts = self.load_unlocked()?;
        upsert_draft(&mut drafts, draft);
        self.save_all(&drafts)
    }

    /// Save a snapshot only when the draft version observed by the composer
    /// is still current.  `updated_at` is the store's existing revision token;
    /// using it instead of the complete payload also permits recovery to
    /// sanitize legacy HTML or fill newly introduced per-message defaults
    /// without falsely reporting a conflict.
    pub(crate) fn save_if_matches(
        &self,
        expected: Option<&DraftMessage>,
        draft: DraftMessage,
    ) -> io::Result<bool> {
        let _lock = draft_store_lock();
        let mut drafts = self.load_unlocked()?;
        let current_index = drafts.iter().position(|stored| stored.id == draft.id);
        let can_replace = match (expected, current_index) {
            (None, None) => true,
            (Some(expected), Some(index)) => {
                drafts[index].id == expected.id && drafts[index].updated_at == expected.updated_at
            }
            _ => false,
        };
        if !can_replace {
            return Ok(false);
        }
        upsert_draft(&mut drafts, draft);
        self.save_all(&drafts)?;
        Ok(true)
    }

    /// Prevent two open composers in this process from transporting the same
    /// recovery draft concurrently. The guard is intentionally held by the
    /// worker across the complete transport attempt.
    pub(crate) fn try_claim_send(&self, id: &str) -> Option<DraftSendClaim> {
        let path = self
            .path
            .canonicalize()
            .or_else(|_| {
                self.path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()
                    .map(|parent| parent.join(self.path.file_name().unwrap_or_default()))
            })
            .unwrap_or_else(|_| self.path.clone());
        let key = format!("{}\0{id}", path.to_string_lossy());
        let mut claims = draft_send_claims();
        if !claims.insert(key.clone()) {
            return None;
        }
        Some(DraftSendClaim { key })
    }

    /// Delete only the exact snapshot that was confirmed as sent. A newer
    /// autosave from another composer with the same ID is preserved.
    pub(crate) fn delete_if_matches(&self, expected: &DraftMessage) -> io::Result<bool> {
        let _lock = draft_store_lock();
        let mut drafts = self.load_unlocked()?;
        let Some(index) = drafts.iter().position(|draft| draft == expected) else {
            return Ok(false);
        };
        drafts.remove(index);
        self.save_all(&drafts)?;
        Ok(true)
    }

    /// Remove one draft by its stable local identifier.
    ///
    /// Deleting an already absent draft is intentionally idempotent.  This is
    /// useful when a draft was sent or removed from another open composer
    /// while the draft manager was still visible.
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let _lock = draft_store_lock();
        let mut drafts = self.load_unlocked()?;
        let original_len = drafts.len();
        drafts.retain(|draft| draft.id != id);
        if drafts.len() == original_len {
            return Ok(false);
        }
        self.save_all(&drafts)?;
        Ok(true)
    }

    fn save_all(&self, drafts: &[DraftMessage]) -> io::Result<()> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let file = DraftFile {
            version: CURRENT_VERSION,
            drafts: drafts.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("drafts.json");
        let temporary_path = self
            .path
            .with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
        let write_result = (|| -> io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&temporary_path, &self.path)?;
            if let Some(parent) = self
                .path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                && let Ok(directory) = fs::File::open(parent)
            {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
    }
}

fn upsert_draft(drafts: &mut Vec<DraftMessage>, draft: DraftMessage) {
    if let Some(existing) = drafts.iter_mut().find(|existing| existing.id == draft.id) {
        *existing = draft;
    } else {
        drafts.push(draft);
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
            from: String::new(),
            source_uid: String::new(),
            to: "person@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Subject".to_string(),
            html_body: "<p>Hello</p>".to_string(),
            plain_text: false,
            updated_at: "2026-08-10T12:00:00Z".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: true,
            pgp_encrypt: true,
            pgp_key: "AABBCCDDEEFF0011".to_string(),
            smime_options: Some(DraftComposerOptions {
                request_read_receipt: true,
                priority: true,
                charset: "ISO-8859-1".to_string(),
                smime: Some(SmimeOptions {
                    sign: true,
                    encrypt: false,
                    signer_certificate: "/tmp/signer.pem".to_string(),
                    private_key: "/tmp/signer.key".to_string(),
                    recipient_certificates: Vec::new(),
                    ca_file: "/tmp/ca.pem".to_string(),
                }),
            }),
        };

        store.save(draft.clone()).expect("draft should persist");
        let mut updated = draft.clone();
        updated.subject = "Updated".to_string();
        store.save(updated.clone()).expect("draft should update");

        assert_eq!(store.load(), vec![updated]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_smime_metadata_still_loads_as_composer_options() {
        let options: DraftComposerOptions = serde_json::from_str(
            r#"{
                "sign": true,
                "encrypt": false,
                "signer_certificate": "signer.pem",
                "private_key": "signer.key",
                "recipient_certificates": [],
                "ca_file": "ca.pem"
            }"#,
        )
        .expect("legacy S/MIME metadata should deserialize");

        assert!(!options.request_read_receipt);
        assert!(!options.priority);
        assert_eq!(options.charset, "UTF-8");
        assert_eq!(options.smime.as_ref().map(|smime| smime.sign), Some(true));
    }

    #[test]
    fn composer_options_roundtrip_keeps_per_message_values() {
        let options = DraftComposerOptions {
            request_read_receipt: true,
            priority: true,
            charset: "Windows-1252".to_string(),
            smime: None,
        };

        let encoded = serde_json::to_string(&options).expect("composer options should serialize");
        let decoded: DraftComposerOptions =
            serde_json::from_str(&encoded).expect("composer options should deserialize");
        assert_eq!(decoded, options);
    }

    #[test]
    fn delete_is_idempotent_and_removes_only_requested_draft() {
        let root = temporary_directory("draft-delete");
        let store = DraftStoreService::new(root.join("drafts.json"));
        for id in ["draft-1", "draft-2"] {
            store
                .save(DraftMessage {
                    id: id.to_string(),
                    from: String::new(),
                    source_uid: String::new(),
                    to: String::new(),
                    cc: String::new(),
                    bcc: String::new(),
                    subject: id.to_string(),
                    html_body: String::new(),
                    plain_text: false,
                    updated_at: String::new(),
                    attachments: Vec::new(),
                    calendar_method: None,
                    calendar_component: None,
                    pgp_sign: false,
                    pgp_encrypt: false,
                    pgp_key: String::new(),
                    smime_options: None,
                })
                .expect("draft should persist");
        }

        assert!(store.delete("draft-1").expect("draft should be deleted"));
        assert!(!store.delete("draft-1").expect("missing draft is harmless"));
        assert_eq!(
            store
                .load()
                .into_iter()
                .map(|draft| draft.id)
                .collect::<Vec<_>>(),
            vec!["draft-2"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_returns_only_the_requested_recovery_draft() {
        let root = temporary_directory("draft-find");
        let store = DraftStoreService::new(root.join("drafts.json"));
        store
            .save(test_draft("draft-a"))
            .expect("first draft should persist");
        store
            .save(test_draft("draft-b"))
            .expect("second draft should persist");

        assert_eq!(
            store.find("draft-a").map(|draft| draft.id),
            Some("draft-a".to_string())
        );
        assert!(store.find("missing").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_recovery_saves_do_not_drop_drafts() {
        let root = temporary_directory("draft-concurrent");
        let store = DraftStoreService::new(root.join("drafts.json"));

        std::thread::scope(|scope| {
            for index in 0..8 {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .save(test_draft(&format!("draft-{index}")))
                        .expect("concurrent draft should persist");
                });
            }
        });

        let mut ids = store
            .load()
            .into_iter()
            .map(|draft| draft.id)
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids.len(), 8);
        assert_eq!(ids.first().map(String::as_str), Some("draft-0"));
        assert_eq!(ids.last().map(String::as_str), Some("draft-7"));
        assert!(!root.join("drafts.json.tmp").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn itip_response_draft_roundtrips_calendar_payload() {
        let root = temporary_directory("draft-itip-response");
        let store = DraftStoreService::new(root.join("drafts.json"));
        let draft = DraftMessage {
            id: "itip-reply-meeting-1".to_string(),
            from: "person@example.org".to_string(),
            source_uid: "account-1".to_string(),
            to: "organizer@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Re: Meeting".to_string(),
            html_body: "Ich nehme teil.".to_string(),
            plain_text: true,
            updated_at: "2026-08-10T12:00:00Z".to_string(),
            attachments: Vec::new(),
            calendar_method: Some("REPLY".to_string()),
            calendar_component: Some(
                "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".to_string(),
            ),
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };

        store
            .save(draft.clone())
            .expect("iTIP draft should persist");
        assert_eq!(store.load(), vec![draft]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_store_is_not_overwritten_by_save() {
        let root = temporary_directory("draft-corrupt-preserved");
        let path = root.join("drafts.json");
        let corrupt = b"{ definitely not valid json";
        fs::write(&path, corrupt).expect("corrupt fixture should be written");
        let store = DraftStoreService::new(&path);

        let error = store
            .save(test_draft("draft-new"))
            .expect_err("save must fail closed for a corrupt store");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read(&path).expect("fixture should remain readable"),
            corrupt
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn send_cleanup_preserves_a_newer_draft_snapshot() {
        let root = temporary_directory("draft-revision-cleanup");
        let store = DraftStoreService::new(root.join("drafts.json"));
        let sent = test_draft("draft-shared");
        store
            .save(sent.clone())
            .expect("sent snapshot should persist");
        let mut newer = sent.clone();
        newer.subject = "newer revision".to_string();
        newer.updated_at = "2026-08-12T13:00:00Z".to_string();
        store
            .save(newer.clone())
            .expect("newer snapshot should persist");

        assert!(
            !store
                .delete_if_matches(&sent)
                .expect("revision-aware cleanup should succeed")
        );
        assert_eq!(store.load(), vec![newer]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_composer_snapshot_cannot_replace_a_newer_revision() {
        let root = temporary_directory("draft-stale-save");
        let store = DraftStoreService::new(root.join("drafts.json"));
        let original = test_draft("draft-shared");
        store
            .save(original.clone())
            .expect("initial draft should persist");

        let mut newer = original.clone();
        newer.updated_at = "2026-08-12T13:00:00Z".to_string();
        newer.subject = "newer revision".to_string();
        store
            .save(newer.clone())
            .expect("newer draft should persist");

        let mut stale_update = original.clone();
        stale_update.updated_at = "2026-08-12T14:00:00Z".to_string();
        stale_update.subject = "stale update".to_string();
        assert!(
            !store
                .save_if_matches(Some(&original), stale_update)
                .expect("stale save should be checked")
        );
        assert_eq!(store.load(), vec![newer]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn send_claim_serializes_open_composers_for_one_draft() {
        let root = temporary_directory("draft-send-claim");
        let store = DraftStoreService::new(root.join("drafts.json"));
        let first = store
            .try_claim_send("draft-shared")
            .expect("first composer should claim the draft");
        assert!(store.try_claim_send("draft-shared").is_none());
        drop(first);
        assert!(store.try_claim_send("draft-shared").is_some());
        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    fn test_draft(id: &str) -> DraftMessage {
        DraftMessage {
            id: id.to_string(),
            from: "sender@example.org".to_string(),
            source_uid: "source-1".to_string(),
            to: "recipient@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: id.to_string(),
            html_body: "<p>Recovery</p>".to_string(),
            plain_text: false,
            updated_at: "2026-08-12T12:00:00Z".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        }
    }
}
