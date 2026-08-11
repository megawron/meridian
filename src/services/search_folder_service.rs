// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_folder_service — Service for managing and evaluating Virtual Search Folders (vFolders).

use crate::models::search_folder::SearchFolderObject;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

#[allow(dead_code)]
pub struct SearchFolderService;

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    pub query: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SearchFile {
    version: u32,
    #[serde(default)]
    searches: Vec<SavedSearch>,
}

#[allow(dead_code)]
impl SearchFolderService {
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
                "emblem-important-symbolic",
                "is:starred",
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

    pub fn load_saved() -> Vec<SavedSearch> {
        let path = Self::path();
        let Ok(raw) = fs::read(&path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<SearchFile>(&raw) else {
            tracing::warn!(path = %path.display(), "Ignoring invalid saved search file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported saved search version"
            );
            return Vec::new();
        }
        file.searches
    }

    pub fn save_saved(search: SavedSearch) -> io::Result<()> {
        let path = Self::path();
        let mut searches = Self::load_saved();
        if let Some(existing) = searches
            .iter_mut()
            .find(|existing| existing.id == search.id)
        {
            *existing = search;
        } else {
            searches.push(search);
        }
        Self::write_searches(&path, &searches)
    }

    pub fn delete_saved(id: &str) -> io::Result<bool> {
        let path = Self::path();
        let mut searches = Self::load_saved();
        let original_len = searches.len();
        searches.retain(|search| search.id != id);
        if searches.len() == original_len {
            return Ok(false);
        }
        Self::write_searches(&path, &searches)?;
        Ok(true)
    }

    fn write_searches(path: &PathBuf, searches: &[SavedSearch]) -> io::Result<()> {
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

    fn path() -> PathBuf {
        std::env::var_os("EVOLUTION_GTK4_SEARCH_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("search-folders.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_vfolders() {
        let vfolders = SearchFolderService::default_vfolders();
        assert_eq!(vfolders.len(), 3);
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
}
