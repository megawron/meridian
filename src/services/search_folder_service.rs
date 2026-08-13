// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_folder_service — Service for managing and evaluating Virtual Search Folders (vFolders).

use crate::models::search_folder::SearchFolderObject;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SearchFolderService {
    path: PathBuf,
}

const CURRENT_VERSION: u32 = 1;

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
        Ok(())
    }
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
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<SearchFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid saved search file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported saved search version"
            );
            return Vec::new();
        }
        file.searches
    }

    pub fn save_saved(&self, search: SavedSearch) -> io::Result<()> {
        search.validate()?;
        let mut searches = self.load_saved();
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
        let mut searches = self.load_saved();
        let original_len = searches.len();
        searches.retain(|search| search.id != id);
        if searches.len() == original_len {
            return Ok(false);
        }
        Self::write_searches(&self.path, &searches)?;
        Ok(true)
    }

    fn write_searches(path: &Path, searches: &[SavedSearch]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_vec_pretty(&SearchFile {
            version: CURRENT_VERSION,
            searches: searches.to_vec(),
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, encoded)?;
        fs::rename(temporary, path)
    }
}

impl Default for SearchFolderService {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-search-folders-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let service = SearchFolderService::new(root.join("nested/searches.json"));
        let search = SavedSearch {
            id: "search-1".to_string(),
            name: "Rechnungen".to_string(),
            query: "from:billing@example.org".to_string(),
        };

        service
            .save_saved(search.clone())
            .expect("saved search should be written");
        assert_eq!(service.load_saved(), vec![search.clone()]);

        let updated = SavedSearch {
            name: "Offene Rechnungen".to_string(),
            query: "from:billing@example.org is:unread".to_string(),
            ..search.clone()
        };
        service
            .save_saved(updated.clone())
            .expect("saved search update should be written");
        assert_eq!(service.load_saved(), vec![updated]);

        assert!(
            service
                .delete_saved("search-1")
                .expect("saved search should be deleted")
        );
        assert!(service.load_saved().is_empty());
        assert!(
            !service
                .delete_saved("missing")
                .expect("missing saved search should be a no-op")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_saved_searches_are_rejected_before_writing() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-search-folders-invalid-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let service = SearchFolderService::new(root.join("searches.json"));

        let error = service
            .save_saved(SavedSearch {
                id: "search-1".to_string(),
                name: "".to_string(),
                query: "is:unread".to_string(),
            })
            .expect_err("a saved search without a name must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!service.path().exists());

        let _ = fs::remove_dir_all(root);
    }
}
