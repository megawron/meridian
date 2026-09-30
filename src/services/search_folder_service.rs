// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_folder_service — Service for managing and evaluating Virtual Search Folders (vFolders).

use crate::models::search_folder::SearchFolderObject;
use crate::services::search_engine::{
    MailSearchFieldScope, decode_mail_query_fixture, encode_mail_query_fixture, parse_mail_query,
    validate_search_syntax,
};
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

const LEGACY_VERSION: u32 = 1;
const CURRENT_VERSION: u32 = 2;
const MAX_ID_CHARS: usize = 128;
const MAX_NAME_CHARS: usize = 160;
const MAX_QUERY_CHARS: usize = 4096;
const MAX_IDENTITY_CHARS: usize = 1024;

static SEARCH_FILE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// The upstream quick-search field/mode attached to a saved search.
///
/// These names are part of the on-disk contract.  Keep the explicit serde
/// spelling stable when Rust enum names change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedSearchFieldMode {
    SubjectOrAddresses,
    Subject,
    Sender,
    Recipients,
    Message,
    Body,
    FreeForm,
}

impl Default for SavedSearchFieldMode {
    fn default() -> Self {
        Self::FreeForm
    }
}

impl SavedSearchFieldMode {
    fn search_scope(self) -> MailSearchFieldScope {
        match self {
            Self::SubjectOrAddresses => MailSearchFieldScope::SubjectOrAddresses,
            Self::Subject => MailSearchFieldScope::Subject,
            Self::Sender => MailSearchFieldScope::Sender,
            Self::Recipients => MailSearchFieldScope::Recipients,
            Self::Message => MailSearchFieldScope::Message,
            Self::Body => MailSearchFieldScope::Body,
            Self::FreeForm => MailSearchFieldScope::Any,
        }
    }
}

/// The source scope attached to a saved search.
///
/// The source/folder identities on [`SavedSearch`] are exact values, not
/// display labels.  They are deliberately kept separate from this enum so a
/// source can be unavailable without silently changing the scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedSearchScope {
    CurrentFolder,
    CurrentFolderAndSubfolders,
    CurrentAccount,
    AllAccounts,
}

impl Default for SavedSearchScope {
    fn default() -> Self {
        Self::AllAccounts
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    pub query: String,
    pub field: SavedSearchFieldMode,
    pub scope: SavedSearchScope,
    /// Exact local folder ID or remote dynamic folder key.  Account-wide
    /// searches intentionally carry no folder identity.
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub source_uid: Option<String>,
    #[serde(default)]
    pub folder_name: Option<String>,
    /// Versioned, canonical serialization of the parsed mail-search AST.
    pub ast: String,
}

impl SavedSearch {
    /// Build a new free-form/all-account saved search with its AST fixture.
    /// Callers editing an existing record should retain the loaded metadata or
    /// use [`Self::with_metadata`]; `save_saved` never invents a scope for a
    /// record that already carries one.
    #[allow(dead_code)]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        query: impl Into<String>,
    ) -> io::Result<Self> {
        Self::with_metadata(
            id,
            name,
            query,
            SavedSearchFieldMode::FreeForm,
            SavedSearchScope::AllAccounts,
            None,
            None,
            None,
        )
    }

    /// Build a saved search and derive the canonical AST fixture from its
    /// query/mode.  Persisted records are still validated again on write.
    #[allow(dead_code)]
    pub fn with_metadata(
        id: impl Into<String>,
        name: impl Into<String>,
        query: impl Into<String>,
        field: SavedSearchFieldMode,
        scope: SavedSearchScope,
        folder_id: Option<String>,
        source_uid: Option<String>,
        folder_name: Option<String>,
    ) -> io::Result<Self> {
        let mut search = Self {
            id: id.into(),
            name: name.into(),
            query: query.into(),
            field,
            scope,
            folder_id,
            source_uid,
            folder_name,
            ast: String::new(),
        };
        let parsed = search.validate_query_and_metadata()?;
        search.ast = encode_fixture(&parsed)?;
        search.validate_fixture(&parsed)?;
        Ok(search)
    }

    fn validate(&self) -> io::Result<()> {
        let parsed = self.validate_query_and_metadata()?;
        self.validate_fixture(&parsed)
    }

    fn validate_query_and_metadata(
        &self,
    ) -> io::Result<crate::services::search_engine::ParsedMailSearchQuery> {
        validate_bounded_text(&self.id, "ID", MAX_ID_CHARS, true)?;
        validate_bounded_text(&self.name, "Name", MAX_NAME_CHARS, true)?;
        validate_bounded_text(&self.query, "Suchabfrage", MAX_QUERY_CHARS, true)?;

        validate_search_syntax(&self.query).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Ungültige Suchabfrage: {error}"),
            )
        })?;
        let parsed = parse_mail_query(&self.query, self.field.search_scope())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Gespeicherte Suchen benötigen eine Suchabfrage",
                )
            })?;
        validate_folder_id(self.folder_id.as_deref())?;
        validate_identity(self.source_uid.as_deref(), "Quell-ID")?;
        validate_identity(self.folder_name.as_deref(), "Ordnername")?;

        match self.scope {
            SavedSearchScope::CurrentFolder | SavedSearchScope::CurrentFolderAndSubfolders => {
                let local_identity = self.folder_id.is_some()
                    && self.source_uid.is_none()
                    && self.folder_name.is_none();
                let remote_identity = self.folder_id.is_some()
                    && self.source_uid.is_some()
                    && self.folder_name.is_some();
                if !local_identity && !remote_identity {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Ordnersuchen benötigen entweder eine lokale Ordner-ID oder die vollständige Quell-/Ordneridentität",
                    ));
                }
                if local_identity
                    && self
                        .folder_id
                        .as_deref()
                        .is_some_and(|id| id == "all" || id.contains('\0'))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Lokale Ordnersuchen benötigen eine konkrete Ordner-ID ohne dynamischen Quelltrenner",
                    ));
                }
                if remote_identity {
                    let expected = format!(
                        "{}\u{0}{}",
                        self.source_uid.as_deref().unwrap_or_default(),
                        self.folder_name.as_deref().unwrap_or_default()
                    );
                    if self.folder_id.as_deref() != Some(expected.as_str()) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Remote-Ordner-ID stimmt nicht mit Quell-ID und Ordnername überein",
                        ));
                    }
                }
            }
            SavedSearchScope::CurrentAccount => {
                if self.source_uid.is_none()
                    || self.folder_id.is_some()
                    || self.folder_name.is_some()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Kontosuchen benötigen genau eine Quell-ID und keine Ordneridentität",
                    ));
                }
            }
            SavedSearchScope::AllAccounts => {
                if self.folder_id.is_some()
                    || self.source_uid.is_some()
                    || self.folder_name.is_some()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Kontenübergreifende Suchen dürfen keine Quellen- oder Ordneridentität enthalten",
                    ));
                }
            }
        }

        Ok(parsed)
    }

    fn validate_fixture(
        &self,
        parsed: &crate::services::search_engine::ParsedMailSearchQuery,
    ) -> io::Result<()> {
        if self.ast.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gespeicherte Suchen benötigen ein AST-Suchfixture",
            ));
        }
        let decoded = decode_mail_query_fixture(&self.ast).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Ungültiges AST-Suchfixture: {error}"),
            )
        })?;
        // Compare canonical wire forms rather than resolved AST dates. Relative
        // expressions are intentionally re-evaluated against the current local
        // day; wire comparison keeps `after:7d` stable even if validation spans
        // midnight while still rejecting any textual/field/predicate mismatch.
        let decoded_fixture = encode_fixture(&decoded)?;
        let parsed_fixture = encode_fixture(parsed)?;
        if decoded_fixture != parsed_fixture {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "AST-Suchfixture und Suchabfrage stimmen nicht überein",
            ));
        }
        Ok(())
    }
}

fn encode_fixture(
    parsed: &crate::services::search_engine::ParsedMailSearchQuery,
) -> io::Result<String> {
    encode_mail_query_fixture(parsed).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("AST-Suchfixture konnte nicht erzeugt werden: {error}"),
        )
    })
}

fn validate_bounded_text(
    value: &str,
    label: &str,
    max_chars: usize,
    required: bool,
) -> io::Result<()> {
    if required && value.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Gespeicherte Suchen benötigen {label}"),
        ));
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Ungültiger oder zu langer Wert für {label}"),
        ));
    }
    Ok(())
}

fn validate_identity(value: Option<&str>, label: &str) -> io::Result<()> {
    if let Some(value) = value {
        validate_bounded_text(value, label, MAX_IDENTITY_CHARS, true)?;
    }
    Ok(())
}

fn validate_folder_id(value: Option<&str>) -> io::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.trim().is_empty() || value.chars().count() > MAX_IDENTITY_CHARS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültige oder zu lange Ordner-ID",
        ));
    }
    // Remote dynamic folder IDs use one NUL as the stable source/name
    // separator.  No other control character is safe in either identity.
    if value
        .chars()
        .any(|character| character.is_control() && character != '\0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültige Ordner-ID",
        ));
    }
    Ok(())
}

fn migrate_legacy_search(legacy: LegacySavedSearch) -> io::Result<SavedSearch> {
    // v1 had no field or source metadata.  FreeForm + AllAccounts is the only
    // deterministic interpretation that cannot narrow or broaden the old
    // query based on a guessed UI state.
    SavedSearch::with_metadata(
        legacy.id,
        legacy.name,
        legacy.query,
        SavedSearchFieldMode::FreeForm,
        SavedSearchScope::AllAccounts,
        None,
        None,
        None,
    )
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

#[derive(Debug, Deserialize)]
struct SearchFileVersion {
    version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchFileV2 {
    version: u32,
    #[serde(default)]
    searches: Vec<SavedSearch>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySavedSearch {
    id: String,
    name: String,
    query: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchFileV1 {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    searches: Vec<LegacySavedSearch>,
}

impl SearchFolderService {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("MERIDIAN_SEARCH_FILE").or_else(|| std::env::var_os("EVOLUTION_GTK4_SEARCH_FILE"))
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
        let version = serde_json::from_slice::<SearchFileVersion>(&raw).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Ungültige gespeicherte-Suchen-Datei: {error}"),
            )
        })?;

        match version.version {
            LEGACY_VERSION => {
                let file = serde_json::from_slice::<SearchFileV1>(&raw).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Ungültige v1-Suchdatei: {error}"),
                    )
                })?;
                let searches = file
                    .searches
                    .into_iter()
                    .map(migrate_legacy_search)
                    .collect::<io::Result<Vec<_>>>()?;
                validate_searches(&searches)?;
                Ok(searches)
            }
            CURRENT_VERSION => {
                let file = serde_json::from_slice::<SearchFileV2>(&raw).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Ungültige v2-Suchdatei: {error}"),
                    )
                })?;
                validate_searches(&file.searches)?;
                Ok(file.searches)
            }
            unsupported => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Nicht unterstützte gespeicherte-Suchen-Version: {unsupported} (erwartet {LEGACY_VERSION} oder {CURRENT_VERSION})"
                ),
            )),
        }
    }

    fn write_searches(path: &Path, searches: &[SavedSearch]) -> io::Result<()> {
        validate_searches(searches)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let encoded = serde_json::to_vec_pretty(&SearchFileV2 {
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
            "meridian-search-folders-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ))
    }

    fn saved_search(id: &str, name: &str, query: &str) -> SavedSearch {
        SavedSearch::new(id, name, query).expect("test saved search should be valid")
    }

    fn raw_saved_search(id: &str, name: &str, query: &str) -> SavedSearch {
        SavedSearch {
            id: id.to_string(),
            name: name.to_string(),
            query: query.to_string(),
            field: SavedSearchFieldMode::FreeForm,
            scope: SavedSearchScope::AllAccounts,
            folder_id: None,
            source_uid: None,
            folder_name: None,
            ast: "invalid-fixture".to_string(),
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
        let file = SearchFileV2 {
            version: CURRENT_VERSION,
            searches: vec![saved_search(
                "search-1",
                "Rechnungen",
                "from:billing@example.org is:unread",
            )],
        };
        let raw = serde_json::to_vec(&file).expect("search file should serialize");
        let decoded: SearchFileV2 = serde_json::from_slice(&raw).expect("search file should parse");
        assert_eq!(decoded.searches, file.searches);
    }

    #[test]
    fn saved_search_v2_roundtrips_all_modes_and_scopes() {
        let root = test_root("v2-roundtrip");
        let service = SearchFolderService::new(root.join("searches.json"));
        let modes = [
            SavedSearchFieldMode::SubjectOrAddresses,
            SavedSearchFieldMode::Subject,
            SavedSearchFieldMode::Sender,
            SavedSearchFieldMode::Recipients,
            SavedSearchFieldMode::Message,
            SavedSearchFieldMode::Body,
            SavedSearchFieldMode::FreeForm,
        ];
        let scopes = [
            (
                SavedSearchScope::CurrentFolder,
                Some("account-uid\u{0}Inbox".to_string()),
                Some("account-uid".to_string()),
                Some("Inbox".to_string()),
            ),
            (
                SavedSearchScope::CurrentFolderAndSubfolders,
                Some("account-uid\u{0}Projects".to_string()),
                Some("account-uid".to_string()),
                Some("Projects".to_string()),
            ),
            (
                SavedSearchScope::CurrentAccount,
                None,
                Some("account-uid".to_string()),
                None,
            ),
            (SavedSearchScope::AllAccounts, None, None, None),
        ];

        let mut expected = Vec::new();
        for (index, field) in modes.into_iter().enumerate() {
            let search = SavedSearch::with_metadata(
                format!("mode-{index}"),
                format!("Mode {index}"),
                "needle",
                field,
                SavedSearchScope::AllAccounts,
                None,
                None,
                None,
            )
            .expect("mode should create a valid saved search");
            service
                .save_saved(search.clone())
                .expect("mode should persist");
            expected.push(search);
        }
        for (index, (scope, folder_id, source_uid, folder_name)) in scopes.into_iter().enumerate() {
            let search = SavedSearch::with_metadata(
                format!("scope-{index}"),
                format!("Scope {index}"),
                "needle",
                SavedSearchFieldMode::FreeForm,
                scope,
                folder_id,
                source_uid,
                folder_name,
            )
            .expect("scope should create a valid saved search");
            service
                .save_saved(search.clone())
                .expect("scope should persist");
            expected.push(search);
        }

        assert_eq!(service.load_saved(), expected);
        let raw = fs::read(service.path()).expect("v2 file should exist");
        let value: serde_json::Value =
            serde_json::from_slice(&raw).expect("v2 file should be JSON");
        assert_eq!(value["version"], serde_json::json!(CURRENT_VERSION));
        assert_eq!(value["searches"][0]["field"], "subject_or_addresses");
        assert_eq!(value["searches"][0]["scope"], "all_accounts");
        assert!(value["searches"][0]["ast"].is_string());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saved_search_v2_preserves_local_folder_identity_without_remote_metadata() {
        let root = test_root("v2-local-identity");
        let service = SearchFolderService::new(root.join("searches.json"));
        let local_folder = SavedSearch::with_metadata(
            "local-folder",
            "Lokale Projekte",
            "is:unread",
            SavedSearchFieldMode::FreeForm,
            SavedSearchScope::CurrentFolder,
            Some("custom:Projects".to_string()),
            None,
            None,
        )
        .expect("local folder identity should be valid");
        let local_subfolders = SavedSearch::with_metadata(
            "local-subfolders",
            "Lokale Projekte samt Unterordnern",
            "is:starred",
            SavedSearchFieldMode::FreeForm,
            SavedSearchScope::CurrentFolderAndSubfolders,
            Some("custom:Projects".to_string()),
            None,
            None,
        )
        .expect("local subfolder identity should be valid");
        service.save_saved(local_folder.clone()).unwrap();
        service.save_saved(local_subfolders.clone()).unwrap();
        assert_eq!(
            service.load_saved(),
            vec![local_folder.clone(), local_subfolders.clone()]
        );

        for invalid in [
            SavedSearch {
                folder_id: None,
                ..local_subfolders.clone()
            },
            SavedSearch {
                folder_id: Some("all".to_string()),
                ..local_subfolders.clone()
            },
            SavedSearch {
                folder_id: Some("custom:Projects".to_string()),
                source_uid: Some("account-uid".to_string()),
                folder_name: None,
                ..local_subfolders.clone()
            },
        ] {
            let error = service
                .save_saved(invalid)
                .expect_err("incomplete local identity must fail closed");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saved_search_v2_validates_remote_folder_and_account_identity() {
        let root = test_root("v2-remote-identity");
        let service = SearchFolderService::new(root.join("searches.json"));
        let remote_folder = SavedSearch::with_metadata(
            "remote-folder",
            "Remote Inbox",
            "subject:invoice",
            SavedSearchFieldMode::Subject,
            SavedSearchScope::CurrentFolder,
            Some("account-uid\u{0}Inbox".to_string()),
            Some("account-uid".to_string()),
            Some("Inbox".to_string()),
        )
        .expect("remote folder identity should be valid");
        let account = SavedSearch::with_metadata(
            "remote-account",
            "Remote account",
            "is:unread",
            SavedSearchFieldMode::FreeForm,
            SavedSearchScope::CurrentAccount,
            None,
            Some("account-uid".to_string()),
            None,
        )
        .expect("remote account identity should be valid");
        service.save_saved(remote_folder.clone()).unwrap();
        service.save_saved(account.clone()).unwrap();
        assert_eq!(
            service.load_saved(),
            vec![remote_folder.clone(), account.clone()]
        );

        for invalid in [
            SavedSearch {
                folder_id: Some("wrong-id".to_string()),
                ..account.clone()
            },
            SavedSearch {
                source_uid: None,
                ..account.clone()
            },
            SavedSearch {
                folder_name: Some("Inbox".to_string()),
                ..account.clone()
            },
            SavedSearch {
                folder_id: None,
                source_uid: Some("account-uid".to_string()),
                folder_name: Some("Inbox".to_string()),
                ..remote_folder.clone()
            },
            SavedSearch {
                folder_id: Some("account-uid\u{0}Archive".to_string()),
                ..remote_folder.clone()
            },
        ] {
            let error = service
                .save_saved(invalid)
                .expect_err("incomplete remote identity must fail closed");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn v1_saved_searches_migrate_deterministically_and_next_write_is_v2() {
        let root = test_root("v1-migration");
        let path = root.join("searches.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = serde_json::json!({
            "version": LEGACY_VERSION,
            "searches": [{
                "id": "legacy-1",
                "name": "Legacy",
                "query": "from:billing@example.org is:unread"
            }]
        });
        let original = serde_json::to_vec_pretty(&legacy).unwrap();
        fs::write(&path, &original).unwrap();
        let service = SearchFolderService::new(&path);

        let migrated = service.load_saved();
        assert_eq!(migrated.len(), 1);
        assert_eq!(migrated[0].field, SavedSearchFieldMode::FreeForm);
        assert_eq!(migrated[0].scope, SavedSearchScope::AllAccounts);
        assert_eq!(migrated[0].source_uid, None);
        assert_eq!(migrated[0].folder_name, None);
        assert!(!migrated[0].ast.is_empty());
        assert_eq!(fs::read(&path).unwrap(), original);

        // An update through the normal RMW path retains the migrated record
        // and atomically upgrades the file format.
        service
            .save_saved(SavedSearch::new("new-1", "New", "is:starred").unwrap())
            .expect("next write should upgrade legacy storage");
        let upgraded: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(upgraded["version"], serde_json::json!(CURRENT_VERSION));
        assert_eq!(service.load_saved().len(), 2);
        assert_eq!(service.load_saved()[0], migrated[0]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn v2_mismatched_unknown_and_corrupt_fixtures_fail_closed() {
        let cases = [
            "mismatch",
            "unknown-field-mode",
            "corrupt-fixture",
            "unknown-file-field",
        ];
        for label in cases {
            let root = test_root(label);
            let path = root.join("searches.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let valid = saved_search("search-1", "One", "is:unread");
            let mut file = SearchFileV2 {
                version: CURRENT_VERSION,
                searches: vec![valid.clone()],
            };
            let raw = match label {
                "mismatch" => {
                    file.searches[0].ast = saved_search("other", "Other", "is:starred").ast;
                    serde_json::to_vec_pretty(&file).unwrap()
                }
                "unknown-field-mode" => {
                    let mut value = serde_json::to_value(&file).unwrap();
                    value["searches"][0]["field"] = serde_json::json!("future_mode");
                    serde_json::to_vec_pretty(&value).unwrap()
                }
                "corrupt-fixture" => {
                    file.searches[0].ast = "not-json".to_string();
                    serde_json::to_vec_pretty(&file).unwrap()
                }
                "unknown-file-field" => {
                    let mut value = serde_json::to_value(&file).unwrap();
                    value["unknown"] = serde_json::json!(true);
                    serde_json::to_vec_pretty(&value).unwrap()
                }
                _ => unreachable!(),
            };
            fs::write(&path, &raw).unwrap();
            let service = SearchFolderService::new(&path);
            assert!(
                service.load_saved().is_empty(),
                "{label} must not be loaded"
            );
            let error = service
                .save_saved(saved_search("new", "New", "is:starred"))
                .expect_err("{label} must reject RMW");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), raw);
            let _ = fs::remove_dir_all(root);
        }
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
                .save_saved(raw_saved_search(id, name, query))
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
                br#"{"version":3,"searches":[]}"# as &[u8],
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
