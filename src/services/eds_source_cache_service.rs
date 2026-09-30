// SPDX-License-Identifier: GPL-3.0-or-later
//
// Last-known EDS source metadata.  This is only an offline UI cache: the
// Evolution Data Server remains authoritative whenever it is reachable.  The
// cache never contains credentials, mail, calendar entries or contacts.

use super::eds_service::{EdsSource, EdsSourceKind};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_FORMAT_VERSION: u32 = 2;
static CACHE_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Health of the app-owned source cache file.
///
/// A cache problem is deliberately represented separately from an empty
/// cache.  This lets discovery expose a recoverable offline state without
/// ever treating a corrupt file as a valid source list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdsSourceCacheStatus {
    Missing,
    Valid,
    Corrupt,
    Unreadable,
}

/// Result of reading one category from the source cache.
#[derive(Debug, Clone)]
pub struct EdsSourceCacheLoad {
    pub sources: Vec<EdsSource>,
    pub status: EdsSourceCacheStatus,
    pub warning: Option<String>,
    pub recovery: EdsSourceCacheRecovery,
}

/// Non-secret recovery metadata persisted next to source metadata.
///
/// This is intentionally limited to timing/counter information.  It contains
/// neither credentials nor backend payloads and is safe to carry across an
/// application restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EdsSourceCacheRecovery {
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_success_unix_secs: Option<i64>,
    #[serde(default)]
    pub last_failure_unix_secs: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default = "default_cache_format_version")]
    format_version: u32,
    mail: Vec<EdsSource>,
    calendar: Vec<EdsSource>,
    address_book: Vec<EdsSource>,
    #[serde(default)]
    recovery: CacheRecoveryFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheRecoveryFile {
    #[serde(default)]
    mail: EdsSourceCacheRecovery,
    #[serde(default)]
    calendar: EdsSourceCacheRecovery,
    #[serde(default)]
    address_book: EdsSourceCacheRecovery,
}

impl Default for CacheFile {
    fn default() -> Self {
        Self {
            format_version: CACHE_FORMAT_VERSION,
            mail: Vec::new(),
            calendar: Vec::new(),
            address_book: Vec::new(),
            recovery: CacheRecoveryFile::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EdsSourceCache {
    path: PathBuf,
}

impl EdsSourceCache {
    pub fn new() -> Self {
        let path = std::env::var_os("MERIDIAN_EDS_SOURCE_CACHE").or_else(|| std::env::var_os("EVOLUTION_GTK4_EDS_SOURCE_CACHE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("eds-sources.json"));
        Self { path }
    }

    #[cfg(test)]
    pub(crate) fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self, kind: EdsSourceKind) -> Vec<EdsSource> {
        self.load_with_status(kind).sources
    }

    /// Read source metadata while retaining enough information to distinguish
    /// a missing cache from a corrupt or unreadable one.
    pub fn load_with_status(&self, kind: EdsSourceKind) -> EdsSourceCacheLoad {
        let lock = self.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match self.read_cache() {
            CacheRead::Missing => EdsSourceCacheLoad {
                sources: Vec::new(),
                status: EdsSourceCacheStatus::Missing,
                warning: None,
                recovery: EdsSourceCacheRecovery::default(),
            },
            CacheRead::Valid(cache) => EdsSourceCacheLoad {
                sources: sources_for_kind(cache.clone(), kind),
                status: EdsSourceCacheStatus::Valid,
                warning: None,
                recovery: recovery_for_kind(&cache.recovery, kind),
            },
            CacheRead::Corrupt(warning) => {
                tracing::warn!(path = %self.path.display(), %warning, "Ignoring corrupt EDS source cache");
                EdsSourceCacheLoad {
                    sources: Vec::new(),
                    status: EdsSourceCacheStatus::Corrupt,
                    warning: Some(warning),
                    recovery: EdsSourceCacheRecovery::default(),
                }
            }
            CacheRead::Unreadable(error) => {
                tracing::warn!(path = %self.path.display(), %error, "Could not read EDS source cache");
                EdsSourceCacheLoad {
                    sources: Vec::new(),
                    status: EdsSourceCacheStatus::Unreadable,
                    warning: Some(error.to_string()),
                    recovery: EdsSourceCacheRecovery::default(),
                }
            }
        }
    }

    pub fn save(&self, kind: EdsSourceKind, sources: &[EdsSource]) -> io::Result<()> {
        let lock = self.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut cache = match self.read_cache() {
            CacheRead::Missing => CacheFile::default(),
            CacheRead::Valid(cache) => cache,
            CacheRead::Corrupt(warning) => {
                tracing::warn!(path = %self.path.display(), %warning, "Quarantining corrupt EDS source cache before save");
                self.quarantine_corrupt_cache()?;
                CacheFile::default()
            }
            CacheRead::Unreadable(error) => return Err(error),
        };
        cache.format_version = CACHE_FORMAT_VERSION;
        match kind {
            EdsSourceKind::Mail => cache.mail = sources.to_vec(),
            EdsSourceKind::Calendar => cache.calendar = sources.to_vec(),
            EdsSourceKind::AddressBook => cache.address_book = sources.to_vec(),
        }

        self.write_cache_atomically(&cache)
    }

    /// Persist recovery metadata atomically.  A corrupt cache is not silently
    /// overwritten here: the source save that follows a successful live
    /// discovery is responsible for rebuilding it first.
    pub(crate) fn save_recovery(
        &self,
        kind: EdsSourceKind,
        recovery: &EdsSourceCacheRecovery,
    ) -> io::Result<()> {
        let lock = self.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut cache = match self.read_cache() {
            CacheRead::Missing => CacheFile::default(),
            CacheRead::Valid(cache) => cache,
            CacheRead::Corrupt(warning) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("cannot persist recovery state in corrupt cache: {warning}"),
                ));
            }
            CacheRead::Unreadable(error) => return Err(error),
        };
        cache.format_version = CACHE_FORMAT_VERSION;
        match kind {
            EdsSourceKind::Mail => cache.recovery.mail = recovery.clone(),
            EdsSourceKind::Calendar => cache.recovery.calendar = recovery.clone(),
            EdsSourceKind::AddressBook => cache.recovery.address_book = recovery.clone(),
        }
        self.write_cache_atomically(&cache)
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> Arc<Mutex<()>> {
        static CACHE_LOCKS: OnceLock<Mutex<std::collections::HashMap<PathBuf, Arc<Mutex<()>>>>> =
            OnceLock::new();
        let locks = CACHE_LOCKS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        let mut locks = locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(self.path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn read_cache(&self) -> CacheRead {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == ErrorKind::NotFound => return CacheRead::Missing,
            Err(error) => return CacheRead::Unreadable(error),
        };
        let cache = match serde_json::from_slice::<CacheFile>(&raw) {
            Ok(cache) => cache,
            Err(error) => {
                return CacheRead::Corrupt(format!("invalid JSON: {error}"));
            }
        };
        if cache.format_version > CACHE_FORMAT_VERSION {
            return CacheRead::Corrupt(format!(
                "unsupported cache format version {}",
                cache.format_version
            ));
        }
        CacheRead::Valid(cache)
    }

    fn write_cache_atomically(&self, cache: &CacheFile) -> io::Result<()> {
        let Some(parent) = self.path.parent() else {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "EDS source cache path has no parent directory",
            ));
        };
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(cache)
            .map_err(|error| io::Error::other(format!("serialize EDS source cache: {error}")))?;
        let temporary_path = self.temporary_path();
        let write_result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary_path, &self.path)
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
    }

    fn temporary_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("eds-sources.json");
        let serial = CACHE_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.path
            .with_file_name(format!(".{name}.tmp-{}-{serial}", std::process::id()))
    }

    fn quarantine_corrupt_cache(&self) -> io::Result<PathBuf> {
        let serial = CACHE_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("eds-sources.json");
        let quarantine = self
            .path
            .with_file_name(format!(".{name}.corrupt-{}-{serial}", std::process::id()));
        fs::rename(&self.path, &quarantine)?;
        Ok(quarantine)
    }
}

impl Default for EdsSourceCache {
    fn default() -> Self {
        Self::new()
    }
}

fn sources_for_kind(cache: CacheFile, kind: EdsSourceKind) -> Vec<EdsSource> {
    match kind {
        EdsSourceKind::Mail => cache.mail,
        EdsSourceKind::Calendar => cache.calendar,
        EdsSourceKind::AddressBook => cache.address_book,
    }
}

fn default_cache_format_version() -> u32 {
    0
}

fn recovery_for_kind(cache: &CacheRecoveryFile, kind: EdsSourceKind) -> EdsSourceCacheRecovery {
    match kind {
        EdsSourceKind::Mail => cache.mail.clone(),
        EdsSourceKind::Calendar => cache.calendar.clone(),
        EdsSourceKind::AddressBook => cache.address_book.clone(),
    }
}

enum CacheRead {
    Missing,
    Valid(CacheFile),
    Corrupt(String),
    Unreadable(io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "meridian-eds-cache-{label}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn cache_roundtrip_keeps_source_categories_separate() {
        let path = test_cache_path("roundtrip");
        let _ = fs::remove_file(&path);
        let cache = EdsSourceCache::with_path(&path);
        let mail = EdsSource {
            uid: "mail-1".to_string(),
            display_name: "Mail".to_string(),
            backend_name: "imapx".to_string(),
            enabled: true,
            writable: true,
            parent_name: None,
            color: None,
            foreign_mail: None,
        };
        let calendar = EdsSource {
            uid: "calendar-1".to_string(),
            display_name: "Kalender".to_string(),
            backend_name: "caldav".to_string(),
            enabled: true,
            writable: true,
            parent_name: None,
            color: None,
            foreign_mail: None,
        };

        cache
            .save(EdsSourceKind::Mail, std::slice::from_ref(&mail))
            .expect("mail cache should save");
        cache
            .save(EdsSourceKind::Calendar, std::slice::from_ref(&calendar))
            .expect("calendar cache should save");

        assert_eq!(cache.load(EdsSourceKind::Mail), vec![mail]);
        assert_eq!(cache.load(EdsSourceKind::Calendar), vec![calendar]);
        assert!(cache.load(EdsSourceKind::AddressBook).is_empty());
        let _ = fs::remove_file(cache.path());
    }

    #[test]
    fn corrupt_cache_is_treated_as_empty() {
        let path = test_cache_path("corrupt");
        fs::write(&path, b"not-json").expect("cache fixture should be written");
        let cache = EdsSourceCache::with_path(&path);
        let load = cache.load_with_status(EdsSourceKind::Mail);
        assert!(load.sources.is_empty());
        assert_eq!(load.status, EdsSourceCacheStatus::Corrupt);
        assert!(load.warning.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn recovery_metadata_survives_source_updates_and_restart() {
        let path = test_cache_path("recovery");
        let cache = EdsSourceCache::with_path(&path);
        let recovery = EdsSourceCacheRecovery {
            generation: 7,
            consecutive_failures: 3,
            last_success_unix_secs: Some(100),
            last_failure_unix_secs: Some(200),
            last_error: Some("registry unavailable".to_string()),
        };
        cache
            .save_recovery(EdsSourceKind::Mail, &recovery)
            .expect("recovery should save");
        cache
            .save(
                EdsSourceKind::Mail,
                &[EdsSource {
                    uid: "mail-1".to_string(),
                    display_name: "Mail".to_string(),
                    backend_name: "imapx".to_string(),
                    enabled: true,
                    writable: true,
                    parent_name: None,
                    color: None,
                    foreign_mail: None,
                }],
            )
            .expect("sources should save");

        let restarted = EdsSourceCache::with_path(&path).load_with_status(EdsSourceKind::Mail);
        assert_eq!(restarted.status, EdsSourceCacheStatus::Valid);
        assert_eq!(restarted.recovery, recovery);
        assert_eq!(restarted.sources.len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn successful_save_quarantines_corrupt_cache_before_rebuilding() {
        let path = test_cache_path("quarantine");
        fs::write(&path, b"not-json").expect("cache fixture should be written");
        let cache = EdsSourceCache::with_path(&path);
        cache
            .save(EdsSourceKind::Mail, &[])
            .expect("successful discovery should rebuild a corrupt cache");

        assert_eq!(
            cache.load_with_status(EdsSourceKind::Mail).status,
            EdsSourceCacheStatus::Valid
        );
        let prefix = format!(
            ".{}.corrupt-",
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("test cache filename")
        );
        let quarantined = path
            .parent()
            .expect("test cache parent")
            .read_dir()
            .expect("temporary directory should be readable")
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
        assert!(quarantined.is_some());
        if let Some(quarantined) = quarantined {
            let _ = fs::remove_file(quarantined.path());
        }
        let _ = fs::remove_file(path);
    }
}
