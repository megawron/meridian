// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_folder_service — Service for managing and evaluating Virtual Search Folders (vFolders).

use crate::models::search_folder::SearchFolderObject;

#[allow(dead_code)]
pub struct SearchFolderService;

#[allow(dead_code)]
impl SearchFolderService {
    pub fn default_vfolders() -> Vec<SearchFolderObject> {
        vec![
            SearchFolderObject::new(
                "vfolder-unread",
                "Ungelesen (Alle Konten)",
                "mail-unread-symbolic",
                "is_read == false",
                5,
            ),
            SearchFolderObject::new(
                "vfolder-flagged",
                "Wichtig & Markiert",
                "emblem-important-symbolic",
                "is_flagged == true",
                2,
            ),
            SearchFolderObject::new(
                "vfolder-recent",
                "Letzte 7 Tage",
                "appointment-soon-symbolic",
                "date >= now() - 7d",
                12,
            ),
        ]
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
        assert_eq!(vfolders[0].unread_count(), 5);
    }
}
