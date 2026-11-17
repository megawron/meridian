// SPDX-License-Identifier: GPL-3.0-or-later
//
// SnoozeService — Email snooze ("remind me & resurface later"), muted/watched
// conversation threads, and pinned favorite folders.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CURRENT_VERSION: u32 = 1;

static SNOOZE_KEYS_CACHE: OnceLock<Mutex<Option<(Instant, HashSet<String>)>>> = OnceLock::new();

fn invalidate_snooze_cache() {
    if let Some(lock) = SNOOZE_KEYS_CACHE.get()
        && let Ok(mut guard) = lock.lock()
    {
        *guard = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnoozedMailEntry {
    pub message_key: String,
    #[serde(default)]
    pub message_id_header: String,
    pub source_uid: String,
    pub folder_name: String,
    pub subject: String,
    pub sender: String,
    pub snooze_until_utc: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SnoozeStateFile {
    version: u32,
    #[serde(default)]
    snoozed: Vec<SnoozedMailEntry>,
    #[serde(default)]
    muted_threads: HashSet<String>,
    #[serde(default)]
    watched_threads: HashSet<String>,
    #[serde(default)]
    favorite_folders: Vec<String>,
    #[serde(default)]
    answered_mdn_keys: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct SnoozeStore {
    path: PathBuf,
}

impl Default for SnoozeStore {
    fn default() -> Self {
        let path = std::env::var_os("MERIDIAN_SNOOZE_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| super::app_data_dir().join("mail-triage-state.json"));
        Self { path }
    }
}

impl SnoozeStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load_file(&self) -> SnoozeStateFile {
        let Ok(raw) = fs::read(&self.path) else {
            return SnoozeStateFile {
                version: CURRENT_VERSION,
                ..SnoozeStateFile::default()
            };
        };
        serde_json::from_slice(&raw).unwrap_or(SnoozeStateFile {
            version: CURRENT_VERSION,
            ..SnoozeStateFile::default()
        })
    }

    fn save_file(&self, file: &SnoozeStateFile) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_vec_pretty(file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, payload)?;
        let res = fs::rename(tmp, &self.path);
        invalidate_snooze_cache();
        res
    }

    pub fn cached_active_snoozed_keys(&self) -> HashSet<String> {
        let lock = SNOOZE_KEYS_CACHE.get_or_init(|| Mutex::new(None));
        if let Ok(guard) = lock.lock()
            && let Some((ts, ref keys)) = *guard
            && ts.elapsed() < Duration::from_secs(2)
        {
            return keys.clone();
        }
        let fresh = self.active_snoozed_keys(Utc::now());
        if let Ok(mut guard) = lock.lock() {
            *guard = Some((Instant::now(), fresh.clone()));
        }
        fresh
    }

    pub fn snooze_message(&self, entry: SnoozedMailEntry) -> io::Result<()> {
        let mut data = self.load_file();
        data.snoozed
            .retain(|existing| existing.message_key != entry.message_key);
        data.snoozed.push(entry);
        data.snoozed.sort_by_key(|e| e.snooze_until_utc);
        self.save_file(&data)
    }

    pub fn unsnooze_message(&self, message_key: &str) -> io::Result<bool> {
        let mut data = self.load_file();
        let before = data.snoozed.len();
        data.snoozed.retain(|e| e.message_key != message_key);
        if data.snoozed.len() != before {
            self.save_file(&data)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn active_snoozed_keys(&self, now_utc: DateTime<Utc>) -> HashSet<String> {
        self.load_file()
            .snoozed
            .into_iter()
            .filter(|e| e.snooze_until_utc > now_utc)
            .flat_map(|e| {
                let mut keys = vec![e.message_key];
                if !e.message_id_header.is_empty() {
                    keys.push(e.message_id_header);
                }
                keys
            })
            .collect()
    }

    pub fn active_snoozed_entries(&self, now_utc: DateTime<Utc>) -> Vec<SnoozedMailEntry> {
        self.load_file()
            .snoozed
            .into_iter()
            .filter(|e| e.snooze_until_utc > now_utc)
            .collect()
    }

    /// Remove and return all snoozed messages whose target time has arrived.
    pub fn pop_expired_snoozes(&self, now_utc: DateTime<Utc>) -> Vec<SnoozedMailEntry> {
        let mut data = self.load_file();
        let mut expired = Vec::new();
        let mut remaining = Vec::new();
        for entry in data.snoozed {
            if entry.snooze_until_utc <= now_utc {
                expired.push(entry);
            } else {
                remaining.push(entry);
            }
        }
        if !expired.is_empty() {
            data.snoozed = remaining;
            let _ = self.save_file(&data);
        }
        expired
    }

    // ── Muted / Watched Threads ──────────────────────────────────────────────

    pub fn toggle_thread_muted(&self, thread_key: &str) -> io::Result<bool> {
        let key = thread_key.trim();
        if key.is_empty() {
            return Ok(false);
        }
        let mut data = self.load_file();
        let now_muted = if data.muted_threads.remove(key) {
            false
        } else {
            data.watched_threads.remove(key);
            data.muted_threads.insert(key.to_string());
            true
        };
        self.save_file(&data)?;
        Ok(now_muted)
    }

    pub fn toggle_thread_watched(&self, thread_key: &str) -> io::Result<bool> {
        let key = thread_key.trim();
        if key.is_empty() {
            return Ok(false);
        }
        let mut data = self.load_file();
        let now_watched = if data.watched_threads.remove(key) {
            false
        } else {
            data.muted_threads.remove(key);
            data.watched_threads.insert(key.to_string());
            true
        };
        self.save_file(&data)?;
        Ok(now_watched)
    }

    pub fn muted_threads(&self) -> HashSet<String> {
        self.load_file().muted_threads
    }

    pub fn watched_threads(&self) -> HashSet<String> {
        self.load_file().watched_threads
    }

    // ── Favorite Folders ─────────────────────────────────────────────────────

    pub fn favorite_folders(&self) -> Vec<String> {
        self.load_file().favorite_folders
    }

    pub fn is_favorite_folder(&self, folder_id: &str) -> bool {
        self.load_file()
            .favorite_folders
            .iter()
            .any(|f| f == folder_id)
    }

    pub fn toggle_favorite_folder(&self, folder_id: &str) -> io::Result<bool> {
        let clean = folder_id.trim();
        if clean.is_empty() {
            return Ok(false);
        }
        let mut data = self.load_file();
        let added = if let Some(pos) = data.favorite_folders.iter().position(|f| f == clean) {
            data.favorite_folders.remove(pos);
            false
        } else {
            data.favorite_folders.push(clean.to_string());
            true
        };
        self.save_file(&data)?;
        Ok(added)
    }

    // ── Read Receipt (MDN) Prompt Tracking ───────────────────────────────────

    pub fn is_mdn_handled(&self, message_key: &str) -> bool {
        self.load_file().answered_mdn_keys.contains(message_key)
    }

    pub fn mark_mdn_handled(&self, message_key: &str) -> io::Result<()> {
        if message_key.trim().is_empty() {
            return Ok(());
        }
        let mut data = self.load_file();
        if data.answered_mdn_keys.insert(message_key.to_string()) {
            self.save_file(&data)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn snooze_mute_and_favorites_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("meridian-snooze-{}.json", uuid::Uuid::new_v4()));
        let store = SnoozeStore::new(&path);

        store
            .snooze_message(SnoozedMailEntry {
                message_key: "msg-1".to_string(),
                message_id_header: "<1@example.org>".to_string(),
                source_uid: "local".to_string(),
                folder_name: "Inbox".to_string(),
                subject: "Follow up".to_string(),
                sender: "alice@example.org".to_string(),
                snooze_until_utc: Utc::now() + Duration::hours(2),
            })
            .unwrap();
        assert!(store.active_snoozed_keys(Utc::now()).contains("msg-1"));
        assert!(store.pop_expired_snoozes(Utc::now()).is_empty());

        let popped = store.pop_expired_snoozes(Utc::now() + Duration::hours(3));
        assert_eq!(popped.len(), 1);
        assert!(store.active_snoozed_keys(Utc::now()).is_empty());

        assert!(store.toggle_thread_muted("thread-abc").unwrap());
        assert!(store.muted_threads().contains("thread-abc"));
        assert!(!store.toggle_thread_muted("thread-abc").unwrap());

        assert!(store.toggle_favorite_folder("remote:work/projects").unwrap());
        assert!(store.is_favorite_folder("remote:work/projects"));

        let _ = fs::remove_file(path);
    }
}
