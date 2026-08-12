// SPDX-License-Identifier: GPL-3.0-or-later
//
// Application-owned draft persistence used until Camel/EDS Drafts are wired.
// Passwords and account secrets are intentionally never stored here.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::crypto_service::SmimeOptions;

const CURRENT_VERSION: u32 = 1;

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
    #[serde(default)]
    pub(crate) smime_options: Option<SmimeOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub path: String,
    pub filename: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size: u64,
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

    /// Remove one draft by its stable local identifier.
    ///
    /// Deleting an already absent draft is intentionally idempotent.  This is
    /// useful when a draft was sent or removed from another open composer
    /// while the draft manager was still visible.
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut drafts = self.load();
        let original_len = drafts.len();
        drafts.retain(|draft| draft.id != id);
        if drafts.len() == original_len {
            return Ok(false);
        }
        self.save_all(&drafts)?;
        Ok(true)
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
            smime_options: Some(SmimeOptions {
                sign: true,
                encrypt: false,
                signer_certificate: "/tmp/signer.pem".to_string(),
                private_key: "/tmp/signer.key".to_string(),
                recipient_certificates: Vec::new(),
                ca_file: "/tmp/ca.pem".to_string(),
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

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
