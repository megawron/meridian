// SPDX-License-Identifier: GPL-3.0-or-later
//
// Last-known EDS source metadata.  This is only an offline UI cache: the
// Evolution Data Server remains authoritative whenever it is reachable.  The
// cache never contains credentials, mail, calendar entries or contacts.

use super::eds_service::{EdsSource, EdsSourceKind};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

#[cfg(test)]
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheFile {
    mail: Vec<EdsSource>,
    calendar: Vec<EdsSource>,
    address_book: Vec<EdsSource>,
}

#[derive(Debug, Clone)]
pub struct EdsSourceCache {
    path: PathBuf,
}

impl EdsSourceCache {
    pub fn new() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_EDS_SOURCE_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| glib::user_data_dir().join("evolution-gtk4/eds-sources.json"));
        Self { path }
    }

    #[cfg(test)]
    fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self, kind: EdsSourceKind) -> Vec<EdsSource> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(cache) = serde_json::from_slice::<CacheFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring corrupt EDS source cache");
            return Vec::new();
        };
        sources_for_kind(cache, kind)
    }

    pub fn save(&self, kind: EdsSourceKind, sources: &[EdsSource]) -> io::Result<()> {
        let mut cache = self.read_cache();
        match kind {
            EdsSourceKind::Mail => cache.mail = sources.to_vec(),
            EdsSourceKind::Calendar => cache.calendar = sources.to_vec(),
            EdsSourceKind::AddressBook => cache.address_book = sources.to_vec(),
        }

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&cache)
            .map_err(|error| io::Error::other(format!("serialize EDS source cache: {error}")))?;
        fs::write(&self.path, bytes)
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    fn read_cache(&self) -> CacheFile {
        fs::read(&self.path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<CacheFile>(&raw).ok())
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip_keeps_source_categories_separate() {
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-eds-cache-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let cache = EdsSourceCache::with_path(&path);
        let mail = EdsSource {
            uid: "mail-1".to_string(),
            display_name: "Mail".to_string(),
            backend_name: "imapx".to_string(),
            enabled: true,
        };
        let calendar = EdsSource {
            uid: "calendar-1".to_string(),
            display_name: "Kalender".to_string(),
            backend_name: "caldav".to_string(),
            enabled: true,
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
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-eds-cache-corrupt-{}.json",
            std::process::id()
        ));
        fs::write(&path, b"not-json").expect("cache fixture should be written");
        let cache = EdsSourceCache::with_path(&path);
        assert!(cache.load(EdsSourceKind::Mail).is_empty());
        let _ = fs::remove_file(path);
    }
}
