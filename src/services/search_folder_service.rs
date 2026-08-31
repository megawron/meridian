// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_folder_service — Service for managing and evaluating Virtual Search Folders (vFolders).

use crate::models::search_folder::SearchFolderObject;
use crate::services::search_engine::validate_search_syntax;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SearchFolderService {
    path: PathBuf,
}

const CURRENT_VERSION: u32 = 1;

static SEARCH_FILE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    pub query: String,
}

impl SavedSearch {
    fn validate(&self) -> io::Result<()> {
        if self.id.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gespeicherte Suchen benötigen eine ID",
            ));
        }
        if self.name.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gespeicherte Suchen benötigen einen Namen",
            ));
        }
        if self.query.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gespeicherte Suchen benötigen eine Suchabfrage",
            ));
        }
        validate_search_syntax(&self.query).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Ungültige Suchabfrage: {error}"),
            )
        })?;
        Ok(())
    }
}

fn validate_searches(searches: &[SavedSearch]) -> io::Result<()> {
    let mut ids = HashSet::with_capacity(searches.len());
    for search in searches {
        search.validate()?;
        if !ids.insert(search.id.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Doppelte ID für gespeicherte Suche: {}", search.id),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SearchFile {
    version: u32,
    #[serde(default)]
    searches: Vec<SavedSearch>,
}

impl SearchFolderService {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_SEARCH_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("search-folders.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn default_vfolders() -> Vec<SearchFolderObject> {
        vec![
            SearchFolderObject::new(
                "vfolder-unread",
                "Ungelesen (Alle Konten)",
                "mail-unread-symbolic",
                "is:unread",
                0,
            ),
            SearchFolderObject::new(
                "vfolder-flagged",
                "Wichtig & Markiert",
                "mail-mark-important-symbolic",
                "is:starred",
                0,
            ),
            SearchFolderObject::new(
                "vfolder-important",
                "Wichtig",
                "mail-mark-important-symbolic",
                "is:important",
                0,
            ),
            SearchFolderObject::new(
                "vfolder-recent",
                "Letzte 7 Tage",
                "appointment-soon-symbolic",
                "after:7d",
                0,
            ),
        ]
    }

    pub fn load_saved(&self) -> Vec<SavedSearch> {
        match self.load_saved_strict() {
            Ok(searches) => searches,
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %error,
                    "Ignoring invalid saved search file"
                );
                Vec::new()
            }
        }
    }

    pub fn save_saved(&self, search: SavedSearch) -> io::Result<()> {
        search.validate()?;
        let lock = Self::path_lock(&self.path)?;
        let _guard = lock
            .lock()
            .map_err(|_| io::Error::other("Gespeicherte-Suchen-Sperre ist vergiftet"))?;
        let mut searches = self.load_saved_strict()?;
        if let Some(existing) = searches
            .iter_mut()
            .find(|existing| existing.id == search.id)
        {
            *existing = search;
        } else {
            searches.push(search);
        }
        Self::write_searches(&self.path, &searches)
    }

    pub fn delete_saved(&self, id: &str) -> io::Result<bool> {
        let lock = Self::path_lock(&self.path)?;
        let _guard = lock
            .lock()
            .map_err(|_| io::Error::other("Gespeicherte-Suchen-Sperre ist vergiftet"))?;
        let mut searches = self.load_saved_strict()?;
        let original_len = searches.len();
        if let Some(index) = searches.iter().position(|search| search.id == id) {
            searches.remove(index);
        }
        if searches.len() == original_len {
            return Ok(false);
        }
        Self::write_searches(&self.path, &searches)?;
        Ok(true)
    }

    fn path_lock(path: &Path) -> io::Result<Arc<Mutex<()>>> {
        let key = normalized_lock_key(path)?;
        let locks = SEARCH_FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut locks = locks
            .lock()
            .map_err(|_| io::Error::other("Gespeicherte-Suchen-Lock-Registry ist vergiftet"))?;
        Ok(locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }

    fn load_saved_strict(&self) -> io::Result<Vec<SavedSearch>> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let file = serde_json::from_slice::<SearchFile>(&raw).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Ungültige gespeicherte-Suchen-Datei: {error}"),
            )
        })?;
        if file.version != CURRENT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Nicht unterstützte gespeicherte-Suchen-Version: {} (erwartet {})",
                    file.version, CURRENT_VERSION
                ),
            ));
        }
        validate_searches(&file.searches)?;
        Ok(file.searches)
    }

    fn write_searches(path: &Path, searches: &[SavedSearch]) -> io::Result<()> {
        validate_searches(searches)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let encoded = serde_json::to_vec_pretty(&SearchFile {
            version: CURRENT_VERSION,
            searches: searches.to_vec(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let basename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("search-folders.json");
        let temporary = parent.join(format!(".{basename}.tmp-{}", Uuid::new_v4()));

        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&encoded)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;

            // The replacement is already complete; directory syncing is a
            // best-effort durability enhancement on platforms supporting it.
            if let Ok(directory) = fs::File::open(parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

/// Build a process-local lock key without touching the filesystem.  This
/// deliberately does not resolve symlinks, so symlink aliases remain distinct
/// until a cross-process lock is introduced.
fn normalized_lock_key(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

impl Default for SearchFolderService {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "evolution-gtk4-search-folders-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ))
    }

    fn saved_search(id: &str, name: &str, query: &str) -> SavedSearch {
        SavedSearch {
            id: id.to_string(),
            name: name.to_string(),
            query: query.to_string(),
        }
    }

    #[test]
    fn test_default_vfolders() {
        let vfolders = SearchFolderService::default_vfolders();
        assert_eq!(vfolders.len(), 4);
        assert_eq!(vfolders[0].name(), "Ungelesen (Alle Konten)");
        assert_eq!(vfolders[0].query_expression(), "is:unread");
        assert_eq!(vfolders[0].unread_count(), 0);
    }

    #[test]
    fn saved_search_serialization_roundtrips_without_backend_state() {
        let file = SearchFile {
            version: CURRENT_VERSION,
            searches: vec![SavedSearch {
                id: "search-1".to_string(),
                name: "Rechnungen".to_string(),
                query: "from:billing@example.org is:unread".to_string(),
            }],
        };
        let raw = serde_json::to_vec(&file).expect("search file should serialize");
        let decoded: SearchFile = serde_json::from_slice(&raw).expect("search file should parse");
        assert_eq!(decoded.searches, file.searches);
    }

    #[test]
    fn saved_searches_persist_update_and_delete_deterministically() {
        let root = test_root("upsert-delete");
        let service = SearchFolderService::new(root.join("nested/searches.json"));
        let search = saved_search("search-1", "Rechnungen", "from:billing@example.org");
        let other = saved_search("search-2", "Newsletter", "is:unread");

        service
            .save_saved(search.clone())
            .expect("saved search should be written");
        assert_eq!(service.load_saved(), vec![search.clone()]);
        service
            .save_saved(other.clone())
            .expect("second saved search should be written");
        assert_eq!(service.load_saved(), vec![search.clone(), other.clone()]);

        let updated = saved_search(
            "search-1",
            "Offene Rechnungen",
            "from:billing@example.org is:unread",
        );
        service
            .save_saved(updated.clone())
            .expect("saved search update should be written");
        assert_eq!(service.load_saved(), vec![updated.clone(), other.clone()]);

        assert!(
            service
                .delete_saved("search-2")
                .expect("saved search should be deleted")
        );
        assert_eq!(service.load_saved(), vec![updated]);
        let bytes_before_missing_delete = fs::read(service.path()).unwrap();
        assert!(
            !service
                .delete_saved("missing")
                .expect("missing saved search should be a no-op")
        );
        assert_eq!(
            fs::read(service.path()).unwrap(),
            bytes_before_missing_delete
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_saved_searches_are_rejected_before_writing() {
        let root = test_root("invalid-input");
        let service = SearchFolderService::new(root.join("searches.json"));

        for (id, name, query) in [
            ("", "Name", "is:unread"),
            ("search-1", "   ", "is:unread"),
            ("search-1", "Name", "\t"),
            ("search-1", "Name", "unknown:value"),
            ("search-1", "Name", "or:(subject:one"),
            ("search-1", "Name", "subject:\"unterminated"),
        ] {
            let error = service
                .save_saved(saved_search(id, name, query))
                .expect_err("invalid saved search must be rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        assert!(!service.path().exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mutation_rejects_corrupt_version_and_duplicate_files_without_overwriting() {
        let cases = [
            ("corrupt", br#"{"version":1,"searches":["# as &[u8]),
            (
                "version",
                br#"{"version":2,"searches":[]}"# as &[u8],
            ),
            (
                "duplicate",
                br#"{"version":1,"searches":[{"id":"same","name":"One","query":"is:unread"},{"id":"same","name":"Two","query":"is:starred"}]}"# as &[u8],
            ),
        ];

        for (label, original) in cases {
            let root = test_root(label);
            let path = root.join("searches.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, original).unwrap();
            let service = SearchFolderService::new(&path);

            let error = service
                .save_saved(saved_search("new", "New", "is:unread"))
                .expect_err("invalid persisted data must reject a save");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), original);

            let error = service
                .delete_saved("same")
                .expect_err("invalid persisted data must reject a delete");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), original);

            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn concurrent_saves_from_multiple_service_instances_keep_both_records() {
        let root = test_root("concurrent");
        let service = SearchFolderService::new(root.join("nested/searches.json"));
        let barrier = Arc::new(Barrier::new(2));
        let searches = [
            saved_search("search-1", "One", "is:unread"),
            saved_search("search-2", "Two", "is:starred"),
        ];

        let handles: Vec<_> = searches
            .into_iter()
            .map(|search| {
                let service = service.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    service.save_saved(search)
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .expect("concurrent save worker should not panic")
                .expect("concurrent save should succeed");
        }

        let mut actual = service.load_saved();
        actual.sort_by(|left, right| left.id.cmp(&right.id));
        let mut expected = vec![
            saved_search("search-1", "One", "is:unread"),
            saved_search("search-2", "Two", "is:starred"),
        ];
        expected.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(actual, expected);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_saves_with_lexical_path_aliases_keep_both_records() {
        let root = test_root("alias-concurrent");
        fs::create_dir_all(root.join("nested")).unwrap();
        let canonical = SearchFolderService::new(root.join("searches.json"));
        let aliased = SearchFolderService::new(root.join("nested/./../searches.json"));
        let canonical_lock = SearchFolderService::path_lock(canonical.path()).unwrap();
        let aliased_lock = SearchFolderService::path_lock(aliased.path()).unwrap();
        assert!(Arc::ptr_eq(&canonical_lock, &aliased_lock));

        let barrier = Arc::new(Barrier::new(2));
        let first = canonical.clone();
        let first_barrier = Arc::clone(&barrier);
        let first_handle = std::thread::spawn(move || {
            first_barrier.wait();
            first.save_saved(saved_search("search-1", "One", "is:unread"))
        });
        let second = aliased.clone();
        let second_barrier = Arc::clone(&barrier);
        let second_handle = std::thread::spawn(move || {
            second_barrier.wait();
            second.save_saved(saved_search("search-2", "Two", "is:starred"))
        });

        first_handle
            .join()
            .expect("canonical save worker should not panic")
            .expect("canonical save should succeed");
        second_handle
            .join()
            .expect("aliased save worker should not panic")
            .expect("aliased save should succeed");

        let mut actual = canonical.load_saved();
        actual.sort_by(|left, right| left.id.cmp(&right.id));
        let mut expected = vec![
            saved_search("search-1", "One", "is:unread"),
            saved_search("search-2", "Two", "is:starred"),
        ];
        expected.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(actual, expected);

        let _ = fs::remove_dir_all(root);
    }
}
