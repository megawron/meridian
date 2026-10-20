// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_longlong, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static LIMIT_MEMORY_USAGE: AtomicBool = AtomicBool::new(false);

pub fn set_limit_memory_usage(limit: bool) {
    LIMIT_MEMORY_USAGE.store(limit, Ordering::Relaxed);
}

pub fn limit_memory_usage() -> bool {
    LIMIT_MEMORY_USAGE.load(Ordering::Relaxed)
}

pub fn effective_sqlite_folder_limit(page_limit: usize) -> usize {
    if limit_memory_usage() {
        page_limit.max(25)
    } else {
        page_limit.max(25_000)
    }
}

use chrono::{DateTime, TimeZone, Utc};

use crate::models::mail_message::AttachmentInfo;
use crate::services::mail_metadata_service::{MailFollowUpMetadata, MailFollowUpState};
use crate::services::mail_store_service::{MailBackendLocation, ParsedMail};

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READONLY: c_int = 0x00000001;

unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        ppDb: *mut *mut Sqlite3,
        flags: c_int,
        zVfs: *const c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        zSql: *const c_char,
        nByte: c_int,
        ppStmt: *mut *mut Sqlite3Stmt,
        pzTail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_column_text(pStmt: *mut Sqlite3Stmt, iCol: c_int) -> *const c_char;
    fn sqlite3_column_int(pStmt: *mut Sqlite3Stmt, iCol: c_int) -> c_int;
    fn sqlite3_column_int64(pStmt: *mut Sqlite3Stmt, iCol: c_int) -> c_longlong;
    fn sqlite3_finalize(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
}

/// Meridian mail cache account descriptor.
#[derive(Debug, Clone)]
pub struct MeridianAccountCache {
    pub source_uid: String,
    pub display_name: String,
    pub folders_db: PathBuf,
    pub cache_dir: PathBuf,
}

pub type EvolutionAccountCache = MeridianAccountCache;

/// Helper to extract `DisplayName` from an EDS `.source` file or AccountConfigStore.
fn resolve_source_display_name(source_uid: &str, home: &Path) -> Option<String> {
    let clean = source_uid.strip_suffix("-mail").unwrap_or(source_uid);
    let profiles = crate::services::account_config_service::AccountConfigStore::default().load_cached();
    if let Some(p) = profiles.iter().find(|p| {
        p.id == source_uid
            || p.id.strip_prefix("eds:") == Some(source_uid)
            || p.id == clean
            || p.id.strip_prefix("eds:") == Some(clean)
            || p.email.eq_ignore_ascii_case(source_uid)
            || p.email.eq_ignore_ascii_case(clean)
    }) {
        if !p.display_name.trim().is_empty() {
            return Some(p.display_name.trim().to_string());
        }
        if !p.email.trim().is_empty() {
            return Some(p.email.trim().to_string());
        }
    }

    let native_sources_dir = home.join(".config/evolution/sources");
    let has_native_sources = std::fs::read_dir(&native_sources_dir)
        .ok()
        .is_some_and(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("source"))
        });

    let mut candidates = vec![
        native_sources_dir.join(format!("{source_uid}.source")),
        native_sources_dir.join(format!("{clean}.source")),
    ];
    if !has_native_sources {
        candidates.push(home.join(format!(".var/app/org.gnome.Evolution/config/evolution/sources/{source_uid}.source")));
        candidates.push(home.join(format!(".var/app/org.gnome.Evolution/config/evolution/sources/{clean}.source")));
    }

    for path in &candidates {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(val) = trimmed.strip_prefix("DisplayName=") {
                        let val = val.trim();
                        if !val.is_empty() {
                            return Some(val.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Discover all local mail cache directories containing `folders.db`.
pub fn discover_meridian_mail_caches() -> Vec<MeridianAccountCache> {
    let mut results = Vec::new();
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => return results,
    };

    let mut search_roots = Vec::new();
    if let Some(custom) = std::env::var_os("MERIDIAN_CAMEL_CACHE_DIR") {
        search_roots.push(PathBuf::from(custom));
    }
    search_roots.push(crate::services::app_cache_dir().join("camel"));
    search_roots.push(home.join(".cache/meridian/camel"));
    search_roots.push(home.join(".cache/evolution-gtk4/camel"));
    search_roots.push(home.join(".cache/evolution/mail"));
    search_roots.push(home.join(".local/share/evolution/mail"));
    search_roots.push(home.join(".var/app/org.gnome.Evolution/cache/evolution/mail"));

    for root in &search_roots {
        if !root.is_dir() {
            continue;
        }
        if root
            .to_string_lossy()
            .contains(".var/app/org.gnome.Evolution")
            && !results.is_empty()
        {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let db_path = path.join("folders.db");
                if db_path.is_file() {
                    let source_uid = entry.file_name().to_string_lossy().to_string();
                    if matches!(
                        source_uid.as_str(),
                        "local" | "vfolder" | "rss" | "trash" | "system-proxy"
                    ) {
                        continue;
                    }
                    if !crate::services::eds_service::is_source_enabled_for_service(
                        &source_uid,
                        crate::services::eds_service::EdsSourceKind::Mail,
                    ) {
                        continue;
                    }
                    if !results
                        .iter()
                        .any(|r: &MeridianAccountCache| r.source_uid.eq_ignore_ascii_case(&source_uid))
                    {
                        let resolved_name = resolve_source_display_name(&source_uid, &home);
                        let display_name = match resolved_name {
                            Some(name) => name,
                            None if std::env::var_os("MERIDIAN_CAMEL_CACHE_DIR").is_some() => {
                                "E-Mail-Konto".to_string()
                            }
                            None => continue,
                        };
                        if results
                            .iter()
                            .any(|r: &MeridianAccountCache| r.display_name.eq_ignore_ascii_case(&display_name))
                        {
                            continue;
                        }
                        results.push(MeridianAccountCache {
                            source_uid,
                            display_name,
                            folders_db: db_path,
                            cache_dir: path,
                        });
                    }
                }
            }
        }
    }

    results
}

#[allow(unused_imports)]
pub use discover_meridian_mail_caches as discover_evolution_mail_caches;

/// Find the cache for a specific account UID, or the default account cache.
pub fn find_meridian_account_cache(source_uid_hint: Option<&str>) -> Option<MeridianAccountCache> {
    let caches = discover_meridian_mail_caches();
    if caches.is_empty() {
        return None;
    }

    if let Some(hint) = source_uid_hint {
        let hint_clean = hint.trim();
        if !hint_clean.is_empty() {
            // 1. Direct match by source_uid or stripped suffix
            if let Some(found) = caches.iter().find(|c| {
                c.source_uid.eq_ignore_ascii_case(hint_clean)
                    || c.source_uid.strip_suffix("-mail").is_some_and(|s| s.eq_ignore_ascii_case(hint_clean))
            }) {
                return Some(found.clone());
            }

            // 2. Lookup via AccountConfigStore (handles shared mailboxes & email/id aliases)
            let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
            if let Some(profile) = profiles.iter().find(|p| {
                p.id == hint_clean
                    || p.id.strip_prefix("eds:") == Some(hint_clean)
                    || p.email.eq_ignore_ascii_case(hint_clean)
            }) {
                let target_id = if profile.is_shared_mailbox {
                    profile.parent_account_id.as_deref().unwrap_or(&profile.id)
                } else {
                    &profile.id
                };
                let clean_target = target_id.strip_prefix("eds:").unwrap_or(target_id);
                if let Some(found) = caches.iter().find(|c| {
                    c.source_uid.eq_ignore_ascii_case(clean_target)
                        || c.source_uid.strip_suffix("-mail").is_some_and(|s| s.eq_ignore_ascii_case(clean_target))
                }) {
                    return Some(found.clone());
                }

                // Also try matching by parent profile email if target_id didn't match directly
                if let Some(parent_profile) = profiles.iter().find(|p| {
                    p.id == target_id
                        || p.id.strip_prefix("eds:") == Some(target_id)
                        || p.email.eq_ignore_ascii_case(target_id)
                }) {
                    let parent_eds_uid = crate::services::account_config_service::eds_source_uid_for_email(&parent_profile.email);
                    if let Some(found) = caches.iter().find(|c| {
                        c.source_uid.eq_ignore_ascii_case(&parent_eds_uid)
                            || c.source_uid.strip_suffix("-mail").is_some_and(|s| s.eq_ignore_ascii_case(&parent_eds_uid))
                    }) {
                        return Some(found.clone());
                    }
                }
            }
        }
    }

    // Default to the first discovered account cache
    caches.into_iter().next()
}

pub use find_meridian_account_cache as find_evolution_account_cache;

/// Helper struct wrapping an open read-only SQLite database.
struct SqliteDb {
    ptr: *mut Sqlite3,
}

impl SqliteDb {
    fn open(path: &Path) -> Result<Self, String> {
        let path_str = path
            .to_str()
            .ok_or_else(|| "Ungültiger Pfad zur folders.db".to_string())?;
        let c_path = CString::new(path_str)
            .map_err(|_| "NUL-Byte im Pfad zur folders.db".to_string())?;

        let mut ptr: *mut Sqlite3 = std::ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(c_path.as_ptr(), &mut ptr, SQLITE_OPEN_READONLY, std::ptr::null()) };
        if rc != SQLITE_OK || ptr.is_null() {
            let err_msg = if !ptr.is_null() {
                let msg = unsafe { CStr::from_ptr(sqlite3_errmsg(ptr)).to_string_lossy().into_owned() };
                unsafe { sqlite3_close(ptr) };
                msg
            } else {
                format!("Fehlercode {rc}")
            };
            return Err(format!("Konnte folders.db nicht öffnen: {err_msg}"));
        }

        Ok(Self { ptr })
    }

    fn query_folders_map(&self) -> Result<HashMap<String, (i64, String, i64)>, String> {
        let sql = CString::new("SELECT folder_name, folder_id, saved_count FROM folders ORDER BY saved_count ASC;").unwrap();
        let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
        let rc = unsafe { sqlite3_prepare_v2(self.ptr, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) };
        if rc != SQLITE_OK || stmt.is_null() {
            return Err("Konnte folders-Tabelle nicht vorbereiten".to_string());
        }

        let mut map = HashMap::new();
        unsafe {
            while sqlite3_step(stmt) == SQLITE_ROW {
                let text_ptr = sqlite3_column_text(stmt, 0);
                let id = sqlite3_column_int64(stmt, 1);
                let saved_count = sqlite3_column_int64(stmt, 2).max(0);
                if !text_ptr.is_null() {
                    let name = CStr::from_ptr(text_ptr).to_string_lossy().into_owned();
                    map.insert(name.to_lowercase(), (id, name.clone(), saved_count));
                    // Also store with stripped "[Gmail]/" prefix for easy matching
                    if let Some(stripped) = name.strip_prefix("[Gmail]/") {
                        map.insert(stripped.to_lowercase(), (id, name.clone(), saved_count));
                    }
                }
            }
            sqlite3_finalize(stmt);
        }

        Ok(map)
    }

    fn resolve_folder_id(&self, target_folder: &str) -> Result<Option<(i64, String)>, String> {
        let map = self.query_folders_map()?;
        let target_lower = target_folder.trim().to_lowercase();

        // 0. Empty folder name defaults to inbox
        if target_lower.is_empty() {
            for cand in ["posteingang", "inbox", "[gmail]/alle nachrichten"] {
                if let Some((id, name, _)) = map.get(cand) {
                    return Ok(Some((*id, name.clone())));
                }
            }
            return Ok(None);
        }

        let norm = |s: &str| s.replace('—', "-").replace('–', "-").to_lowercase();
        let target_norm = norm(target_folder.trim());

        // Handle Foreign Folders (shared mailboxes) strictly within Foreign Folders namespace
        if target_lower.starts_with("foreign folders") {
            let mut best_match: Option<(i64, String, i64)> = None;

            if let Some(res) = map.get(&target_lower) {
                if res.2 > 0 {
                    return Ok(Some((res.0, res.1.clone())));
                }
                best_match = Some(res.clone());
            }

            for (key, res) in &map {
                if norm(key) == target_norm {
                    if res.2 > 0 {
                        return Ok(Some((res.0, res.1.clone())));
                    }
                    if best_match.as_ref().is_none_or(|b| res.2 > b.2) {
                        best_match = Some(res.clone());
                    }
                }
            }

            // Match across alternative shared mailbox container prefixes (e.g. "Postfach — EDV" vs "Postfach — edv@...")
            fn foreign_subpath(s: &str) -> Option<&str> {
                let rest = s.strip_prefix("foreign folders/")?;
                let (_, sub) = rest.split_once('/')?;
                (!sub.is_empty()).then_some(sub)
            }

            if let Some(target_sub) = foreign_subpath(&target_norm) {
                let target_id = crate::services::camel_mail_service::normalized_camel_folder_id("", target_sub);
                for (key, res) in &map {
                    let key_norm = norm(key);
                    if let Some(key_sub) = foreign_subpath(&key_norm) {
                        let key_id = crate::services::camel_mail_service::normalized_camel_folder_id("", key_sub);
                        if key_sub == target_sub || key_id == target_id {
                            if best_match.as_ref().is_none_or(|b| res.2 > b.2) {
                                best_match = Some(res.clone());
                            }
                        }
                    }
                }
            }

            return Ok(best_match.map(|(id, name, _)| (id, name)));
        }

        // 1. Exact or lower match (non-foreign)
        if let Some((id, name, _)) = map.get(&target_lower) {
            if !name.to_lowercase().starts_with("foreign folders") {
                return Ok(Some((*id, name.clone())));
            }
        }

        // 1b. Dash-normalized match (handles em-dash '—' vs en-dash '–' vs hyphen '-')
        for (key, (id, name, _)) in &map {
            if !key.starts_with("foreign folders") && norm(key) == target_norm {
                return Ok(Some((*id, name.clone())));
            }
        }

        // 2. Common German/English aliases for top-level system folders only
        let aliases: &[(&[&str], &[&str])] = &[
            (
                &["posteingang", "inbox"],
                &["inbox", "posteingang"],
            ),
            (
                &["gesendet", "gesendete elemente", "sent", "sent items", "[gmail]/gesendet", "[gmail]/sent mail"],
                &["[gmail]/gesendet", "gesendet", "sent", "sent items", "gesendete elemente", "[gmail]/sent mail"],
            ),
            (
                &["entwürfe", "entwuerfe", "drafts", "draft", "[gmail]/entwürfe", "[gmail]/drafts"],
                &["[gmail]/entwürfe", "entwürfe", "entwuerfe", "drafts", "draft", "[gmail]/drafts"],
            ),
            (
                &["papierkorb", "trash", "deleted items", "gelöschte elemente", "[gmail]/papierkorb", "[gmail]/trash"],
                &["[gmail]/papierkorb", "papierkorb", "trash", "deleted items", "gelöschte elemente", "[gmail]/trash"],
            ),
            (
                &["spam", "junk", "junk-e-mail", "junk email", "[gmail]/spam"],
                &["[gmail]/spam", "spam", "junk", "junk-e-mail", "junk email"],
            ),
            (
                &["markiert", "starred", "[gmail]/markiert", "[gmail]/starred"],
                &["[gmail]/markiert", "markiert", "starred", "[gmail]/starred"],
            ),
            (
                &["wichtig", "important", "[gmail]/wichtig", "[gmail]/important"],
                &["[gmail]/wichtig", "wichtig", "important", "[gmail]/important"],
            ),
            (
                &["alle nachrichten", "all mail", "archiv", "archive", "[gmail]/alle nachrichten", "[gmail]/all mail"],
                &["[gmail]/alle nachrichten", "alle nachrichten", "all mail", "archiv", "archive", "[gmail]/all mail"],
            ),
        ];

        for (inputs, targets) in aliases {
            if inputs.contains(&target_lower.as_str()) {
                for &cand in *targets {
                    if let Some((id, name, _)) = map.get(cand) {
                        return Ok(Some((*id, name.clone())));
                    }
                }
            }
        }

        Ok(None)
    }
}

impl Drop for SqliteDb {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { sqlite3_close(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

/// Load all folder summaries directly from SQLite folders.db.
pub fn load_folders_from_sqlite(
    folders_db: &Path,
    source_uid: &str,
    account_name: &str,
) -> Result<Vec<crate::services::camel_mail_service::CamelFolderSummary>, String> {
    let db = SqliteDb::open(folders_db)?;
    let sql = CString::new("SELECT folder_name, unread_count, saved_count FROM folders;").unwrap();
    let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
    let rc = unsafe { sqlite3_prepare_v2(db.ptr, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) };
    if rc != SQLITE_OK || stmt.is_null() {
        return Err("Konnte folders-Tabelle nicht vorbereiten".to_string());
    }

    let shared_profiles = crate::services::account_config_service::AccountConfigStore::default()
        .load_cached()
        .iter()
        .filter(|p| p.is_shared_mailbox)
        .cloned()
        .collect::<Vec<_>>();

    let mut folders: Vec<crate::services::camel_mail_service::CamelFolderSummary> = Vec::new();
    let mut index_by_key: HashMap<(String, String), usize> = HashMap::new();
    unsafe {
        while sqlite3_step(stmt) == SQLITE_ROW {
            let text_ptr = sqlite3_column_text(stmt, 0);
            let unread = sqlite3_column_int64(stmt, 1).max(0) as u32;
            let total = sqlite3_column_int64(stmt, 2).max(0) as u32;
            if !text_ptr.is_null() {
                let name = CStr::from_ptr(text_ptr).to_string_lossy().into_owned();
                // Filter out internal dot-folders like .#evolution/*
                if name.starts_with('.') || name.starts_with(".#") || name == "Foreign Folders" {
                    continue;
                }

                let mut mapped_source_uid = source_uid.to_string();
                let mut mapped_source_display_name = account_name.to_string();
                let mut mapped_folder_name = name.clone();
                let leaf_name = name.rsplit('/').next().unwrap_or(&name);
                let mut mapped_display_name = if let Some(stripped) = name.strip_prefix("[Gmail]/") {
                    stripped.rsplit('/').next().unwrap_or(stripped).to_string()
                } else if leaf_name.eq_ignore_ascii_case("INBOX") {
                    "Posteingang".to_string()
                } else {
                    leaf_name.to_string()
                };

                let mut matched_shared = false;
                let mut skip_folder = false;
                for profile in &shared_profiles {
                    if let Some((rel_path, is_container)) =
                        crate::services::camel_mail_service::strip_shared_mailbox_prefix(&name, &profile.email, &profile.display_name)
                    {
                        matched_shared = true;
                        if is_container || rel_path.is_empty() {
                            skip_folder = true;
                            break;
                        }
                        let dname = if !profile.display_name.trim().is_empty() {
                            profile.display_name.trim()
                        } else {
                            profile.email.trim()
                        };
                        mapped_source_uid = profile.id.clone();
                        mapped_source_display_name = dname.to_string();
                        mapped_folder_name = rel_path.to_string();
                        mapped_display_name = rel_path.rsplit('/').next().unwrap_or(rel_path).to_string();
                        break;
                    }
                }

                if skip_folder {
                    continue;
                }

                if !matched_shared && name.starts_with("Foreign Folders") {
                    continue;
                }

                let folder_id =
                    crate::services::camel_mail_service::normalized_camel_folder_id("", &mapped_folder_name);
                let key = (mapped_source_uid.clone(), mapped_folder_name.clone());
                if let Some(&existing_idx) = index_by_key.get(&key) {
                    let existing = &mut folders[existing_idx];
                    existing.unread_count = existing.unread_count.max(unread);
                    existing.total_count = existing.total_count.max(total);
                } else {
                    index_by_key.insert(key, folders.len());
                    folders.push(crate::services::camel_mail_service::CamelFolderSummary {
                        source_uid: mapped_source_uid,
                        source_display_name: mapped_source_display_name,
                        folder_name: mapped_folder_name,
                        folder_id,
                        display_name: mapped_display_name,
                        unread_count: unread,
                        total_count: total,
                        is_selectable: true,
                    });
                }
            }
        }
        sqlite3_finalize(stmt);
    }
    Ok(folders)
}

/// Helper to parse "Name <email>" or just "email".
fn parse_address(raw: &str) -> (String, String) {
    let decoded = crate::services::mail_store_service::decode_mime_header(raw);
    let trimmed = decoded.trim();
    if let Some(start) = trimmed.rfind('<') {
        if let Some(end_offset) = trimmed[start..].find('>') {
            let end = start + end_offset;
            let email = trimmed[start + 1..end].trim().to_string();
            let name = trimmed[..start].trim().trim_matches('"').trim().to_string();
            return (if name.is_empty() { email.clone() } else { name }, email);
        }
    }
    let email = trimmed.trim_matches('"').to_string();
    (email.clone(), email)
}

/// Helper to determine if an email is a calendar invitation from its subject and preview snippet.
pub fn is_calendar_invitation_subject_or_preview(subject: &str, preview: &str) -> bool {
    let p_lower = preview.to_lowercase();
    if p_lower.contains("begin:vcalendar")
        || p_lower.contains("method:request")
        || p_lower.contains("method:reply")
        || p_lower.contains("method:cancel")
        || p_lower.contains("vcalendar")
    {
        return true;
    }

    let s_lower = subject.to_lowercase();
    // Strip common Re/Aw/Fwd/Wg prefixes iteratively
    let mut stripped_subj = s_lower.trim();
    loop {
        let prev = stripped_subj;
        for prefix in &["re:", "aw:", "fwd:", "wg:", "fw:"] {
            if let Some(rest) = stripped_subj.strip_prefix(prefix) {
                stripped_subj = rest.trim();
            }
        }
        if stripped_subj == prev {
            break;
        }
    }

    let is_cal_prefix = stripped_subj.starts_with("einladung:")
        || stripped_subj.starts_with("invitation:")
        || stripped_subj.starts_with("aktualisiert:")
        || stripped_subj.starts_with("updated:")
        || stripped_subj.starts_with("abgesagt:")
        || stripped_subj.starts_with("canceled:")
        || stripped_subj.starts_with("cancelled:")
        || stripped_subj.starts_with("angenommen:")
        || stripped_subj.starts_with("accepted:")
        || stripped_subj.starts_with("abgelehnt:")
        || stripped_subj.starts_with("declined:")
        || stripped_subj.starts_with("vorbehaltlich:")
        || stripped_subj.starts_with("tentative:");

    if is_cal_prefix {
        return true;
    }

    if (stripped_subj.contains("termin:") || stripped_subj.contains("meeting:"))
        && (p_lower.contains("wann:")
            || p_lower.contains("when:")
            || p_lower.contains("wo:")
            || p_lower.contains("where:")
            || p_lower.contains("zeit:")
            || p_lower.contains("ort:"))
    {
        return true;
    }

    false
}

/// Parse Evolution's Camel usertags composite string into follow-up state and metadata.
///
/// Format in Camel: `<count> <len>-<key> <len>-<val> ...`
/// where each string token is preceded by its byte length and a hyphen `-`.
pub fn parse_evolution_usertags_bytes(raw: &[u8]) -> (MailFollowUpState, MailFollowUpMetadata) {
    let mut bytes = raw;

    // Skip leading whitespace
    while let Some((&b, rest)) = bytes.split_first() {
        if b.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }

    if bytes.is_empty() || (bytes.starts_with(b"0") && (bytes.len() == 1 || bytes[1].is_ascii_whitespace())) {
        return (MailFollowUpState::None, MailFollowUpMetadata::default());
    }

    // Read count of key-value pairs
    let mut count: usize = 0;
    let mut has_count = false;
    while let Some((&b, rest)) = bytes.split_first() {
        if b.is_ascii_digit() {
            count = count.saturating_mul(10).saturating_add((b - b'0') as usize);
            has_count = true;
            bytes = rest;
        } else {
            break;
        }
    }

    if !has_count || count == 0 {
        return (MailFollowUpState::None, MailFollowUpMetadata::default());
    }

    let mut follow_up: Option<String> = None;
    let mut completed_on: Option<String> = None;
    let mut due_by: Option<String> = None;

    let read_string = |bytes: &mut &[u8]| -> Option<String> {
        // Skip whitespace
        while let Some((&b, rest)) = bytes.split_first() {
            if b.is_ascii_whitespace() {
                *bytes = rest;
            } else {
                break;
            }
        }

        // Read length digits
        let mut len: usize = 0;
        let mut has_len = false;
        while let Some((&b, rest)) = bytes.split_first() {
            if b.is_ascii_digit() {
                len = len.saturating_mul(10).saturating_add((b - b'0') as usize);
                has_len = true;
                *bytes = rest;
            } else {
                break;
            }
        }

        if !has_len {
            return None;
        }

        // Must be followed by '-'
        if let Some((&b'-', rest)) = bytes.split_first() {
            *bytes = rest;
        } else {
            return None;
        }

        // Extract `len` bytes
        let slice_len = len.min(bytes.len());
        let (val_bytes, rest) = bytes.split_at(slice_len);
        *bytes = rest;

        let s = String::from_utf8_lossy(val_bytes).to_string();
        Some(s)
    };

    for _ in 0..count {
        let key = match read_string(&mut bytes) {
            Some(k) => k,
            None => break,
        };
        let val = match read_string(&mut bytes) {
            Some(v) => v,
            None => break,
        };

        if key.eq_ignore_ascii_case("follow-up") {
            if !val.trim().is_empty() {
                follow_up = Some(val);
            }
        } else if key.eq_ignore_ascii_case("completed-on") {
            if !val.trim().is_empty() {
                completed_on = Some(val);
            }
        } else if key.eq_ignore_ascii_case("due-by") {
            if !val.trim().is_empty() {
                due_by = Some(val);
            }
        }
    }

    let state = if follow_up.is_none() {
        MailFollowUpState::None
    } else if completed_on.is_some() {
        MailFollowUpState::Completed
    } else {
        MailFollowUpState::FollowUp
    };

    let metadata = MailFollowUpMetadata {
        state,
        text: follow_up,
        due_by,
        completed_on,
    }
    .normalized();

    (metadata.state, metadata)
}

/// Convenience wrapper for str input.
pub fn parse_evolution_usertags(raw: &str) -> (MailFollowUpState, MailFollowUpMetadata) {
    parse_evolution_usertags_bytes(raw.as_bytes())
}

/// Keyset cursor for pagination in Evolution SQLite mail summaries: (dreceived, dsent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageCursor {
    pub dreceived: i64,
    pub dsent: i64,
}

/// Parse Evolution Camel `part` summary column:
/// Format: `<mid_hi> <mid_lo> <ref_count> [<ref_hi> <ref_lo>]*`
#[inline]
pub(crate) fn parse_camel_part_fast(part_str: &str) -> (Option<String>, Vec<String>, Vec<String>) {
    let mut tokens = part_str.split_ascii_whitespace();
    let mid_hi = tokens.next().and_then(|s| s.parse::<u64>().ok());
    let mid_lo = tokens.next().and_then(|s| s.parse::<u64>().ok());
    let message_id = match (mid_hi, mid_lo) {
        (Some(hi), Some(lo)) if hi != 0 || lo != 0 => Some(format!("{hi:x}-{lo:x}@camel")),
        _ => None,
    };
    let ref_count = tokens.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    if ref_count == 0 {
        return (message_id, Vec::new(), Vec::new());
    }
    let mut references = Vec::with_capacity(ref_count.min(16));
    for _ in 0..ref_count {
        let Some(r_hi) = tokens.next().and_then(|s| s.parse::<u64>().ok()) else { break; };
        let Some(r_lo) = tokens.next().and_then(|s| s.parse::<u64>().ok()) else { break; };
        if r_hi != 0 || r_lo != 0 {
            let ref_id = format!("{r_hi:x}-{r_lo:x}@camel");
            if !references.contains(&ref_id) {
                references.push(ref_id);
            }
        }
    }
    let in_reply_to = if let Some(last) = references.last() {
        vec![last.clone()]
    } else {
        Vec::new()
    };
    (message_id, in_reply_to, references)
}

/// Fast reader for a page of folder messages directly from Evolution's local `folders.db`
/// using Keyset Pagination (O(1) seek via index on (dreceived, dsent)).
pub fn load_folder_messages_page_from_sqlite(
    db_path: &Path,
    source_uid: &str,
    folder_name: &str,
    cursor: Option<MessageCursor>,
    limit: usize,
) -> Result<(Vec<ParsedMail>, Option<MessageCursor>), String> {
    let (effective_source_uid, effective_folder_name) =
        crate::services::camel_mail_service::resolve_shared_mailbox_target(source_uid, folder_name);

    let db = SqliteDb::open(db_path)?;
    let Some((folder_id, real_folder_name)) = db.resolve_folder_id(&effective_folder_name)? else {
        return Ok((Vec::new(), None));
    };

    let table_name = format!("messages_{folder_id}");
    let limit_clause = if limit > 0 && limit < usize::MAX {
        format!(" LIMIT {limit}")
    } else {
        String::new()
    };

    let is_trash_folder = {
        let lower = real_folder_name.to_ascii_lowercase();
        lower.contains("trash")
            || lower.contains("deleted")
            || lower.contains("papierkorb")
            || lower.contains("gelöschte")
            || lower.contains("geloeschte")
            || lower.contains("mülleimer")
    };

    let where_clause = match (cursor, is_trash_folder) {
        (Some(c), true) => format!(
            " WHERE (dreceived < {}) OR (dreceived = {} AND dsent < {})",
            c.dreceived, c.dreceived, c.dsent
        ),
        (Some(c), false) => format!(
            " WHERE ((flags & 2) = 0) AND ((dreceived < {}) OR (dreceived = {} AND dsent < {}))",
            c.dreceived, c.dreceived, c.dsent
        ),
        (None, true) => String::new(),
        (None, false) => " WHERE (flags & 2) = 0".to_string(),
    };

    let sql_str = format!(
        "SELECT uid, flags, size, dsent, dreceived, subject, mail_from, mail_to, preview, labels, bdata, usertags, part \
         FROM {table_name}{where_clause} ORDER BY dreceived DESC, dsent DESC{limit_clause};"
    );

    let c_sql = CString::new(sql_str).map_err(|_| "Ungültiger SQL-String".to_string())?;
    let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
    let rc = unsafe { sqlite3_prepare_v2(db.ptr, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) };
    if rc != SQLITE_OK || stmt.is_null() {
        return Err(format!("Konnte Abfrage auf {table_name} nicht vorbereiten"));
    }

    let shared_profiles = crate::services::account_config_service::AccountConfigStore::default()
        .load_cached()
        .iter()
        .filter(|p| p.is_shared_mailbox)
        .cloned()
        .collect::<Vec<_>>();

    let is_shared_target = effective_source_uid != source_uid;

    let mut messages = Vec::new();
    let mut last_cursor: Option<MessageCursor> = None;
    unsafe {
        while sqlite3_step(stmt) == SQLITE_ROW {
            let uid_ptr = sqlite3_column_text(stmt, 0);
            if uid_ptr.is_null() {
                continue;
            }
            let uid = CStr::from_ptr(uid_ptr).to_string_lossy().into_owned();
            let flags = sqlite3_column_int(stmt, 1) as u32;
            let size = sqlite3_column_int64(stmt, 2) as usize;
            let dsent = sqlite3_column_int64(stmt, 3);
            let dreceived = sqlite3_column_int64(stmt, 4);

            last_cursor = Some(MessageCursor { dreceived, dsent });

            let subject_ptr = sqlite3_column_text(stmt, 5);
            let subject = if !subject_ptr.is_null() {
                CStr::from_ptr(subject_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let mail_from_ptr = sqlite3_column_text(stmt, 6);
            let mail_from = if !mail_from_ptr.is_null() {
                CStr::from_ptr(mail_from_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let mail_to_ptr = sqlite3_column_text(stmt, 7);
            let mail_to = if !mail_to_ptr.is_null() {
                CStr::from_ptr(mail_to_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let preview_ptr = sqlite3_column_text(stmt, 8);
            let preview = if !preview_ptr.is_null() {
                CStr::from_ptr(preview_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let labels_ptr = sqlite3_column_text(stmt, 9);
            let labels = if !labels_ptr.is_null() {
                CStr::from_ptr(labels_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let usertags_ptr = sqlite3_column_text(stmt, 11);
            let (follow_up_state, follow_up) = if !usertags_ptr.is_null() {
                let raw_bytes = CStr::from_ptr(usertags_ptr).to_bytes();
                parse_evolution_usertags_bytes(raw_bytes)
            } else {
                (MailFollowUpState::None, MailFollowUpMetadata::default())
            };

            // Flags mapping according to Camel message summary flags:
            // 16 = CAMEL_MESSAGE_SEEN
            // 8  = CAMEL_MESSAGE_FLAGGED
            // 1  = CAMEL_MESSAGE_ANSWERED, 64 = CAMEL_MESSAGE_ANSWERED_ALL
            // 128 = CAMEL_MESSAGE_FORWARDED
            // 32 = CAMEL_MESSAGE_ATTACHMENTS
            // Note: bdata stores provider private metadata (e.g. sync state/change tokens), NOT Camel message flags.
            let is_unread = (flags & 16) == 0;
            let is_flagged = (flags & 8) != 0 || labels.contains("$Flagged");
            let is_important = labels.contains("$Important")
                || labels.contains("Important")
                || labels.contains("Wichtig")
                || labels.contains("wichtig");
            let is_replied = (flags & (1 | 64)) != 0 || labels.contains("$Answered");
            let is_forwarded = (flags & 128) != 0 || labels.contains("$Forwarded");
            let has_cal = labels.contains("$has_cal")
                || is_calendar_invitation_subject_or_preview(&subject, &preview);
            let has_attachments = (flags & 32) != 0 || has_cal;
            let parsed_labels = labels
                .split_whitespace()
                .filter(|s| !s.starts_with('$'))
                .map(|s| s.to_string())
                .collect::<Vec<_>>();

            // Date resolution
            let effective_ts = if dreceived > 0 { dreceived } else { dsent };
            let date_str = if effective_ts > 0 {
                Utc.timestamp_opt(effective_ts, 0)
                    .single()
                    .map(|dt: DateTime<Utc>| dt.to_rfc2822())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let (sender_name, sender_email) = parse_address(&mail_from);

            let mut mapped_source_uid = source_uid.to_string();
            let mut mapped_folder_name = real_folder_name.clone();

            for profile in &shared_profiles {
                if let Some((rel, is_container)) =
                    crate::services::camel_mail_service::strip_shared_mailbox_prefix(&real_folder_name, &profile.email, &profile.display_name)
                {
                    let target_rel = if is_container || rel.is_empty() {
                        "Posteingang".to_string()
                    } else {
                        rel.to_string()
                    };
                    mapped_source_uid = profile.id.clone();
                    mapped_folder_name = target_rel;
                    break;
                } else if is_shared_target && (profile.id == source_uid || profile.email == source_uid) {
                    mapped_source_uid = profile.id.clone();
                    mapped_folder_name = if folder_name.trim().is_empty() {
                        "Posteingang".to_string()
                    } else {
                        folder_name.to_string()
                    };
                    break;
                }
            }

            let id = format!("eds:{mapped_source_uid}:{mapped_folder_name}:{uid}");

            let backend_location = Some(MailBackendLocation {
                source_uid: mapped_source_uid,
                folder_name: mapped_folder_name.clone(),
                message_uid: uid.clone(),
                message_id: Some(id.clone()),
            });

            let attachments = if has_cal {
                vec![AttachmentInfo {
                    filename: "invite.ics".to_string(),
                    size_formatted: String::new(),
                    mime_type: "text/calendar".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([]),
                }]
            } else if has_attachments {
                vec![AttachmentInfo {
                    filename: "Anhang".to_string(),
                    size_formatted: String::new(),
                    mime_type: "application/octet-stream".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([]),
                }]
            } else {
                Vec::new()
            };

            let normalized_folder_id =
                crate::services::camel_mail_service::normalized_camel_folder_id("", &mapped_folder_name);
            let part_ptr = sqlite3_column_text(stmt, 12);
            let (parsed_msg_id, in_reply_to, references) = if !part_ptr.is_null() {
                let part_bytes = CStr::from_ptr(part_ptr).to_bytes();
                if let Ok(part_str) = std::str::from_utf8(part_bytes) {
                    parse_camel_part_fast(part_str)
                } else {
                    (None, Vec::new(), Vec::new())
                }
            } else {
                (None, Vec::new(), Vec::new())
            };

            messages.push(ParsedMail {
                id,
                folder_id: normalized_folder_id,
                folder_name: mapped_folder_name,
                backend_location,
                sender_name,
                sender_email,
                recipient_to: mail_to,
                recipient_cc: String::new(),
                recipient_bcc: String::new(),
                reply_to: String::new(),
                subject,
                message_id: parsed_msg_id,
                in_reply_to,
                references,
                thread_id: uid,
                thread_size: 1,
                thread_position: 0,
                date_str,
                message_size: size,
                preview_snippet: preview,
                html_body: String::new(),
                raw_headers: None,
                raw_mime: None,
                is_unread,
                is_flagged,
                is_important,
                is_replied,
                is_forwarded,
                follow_up_state,
                follow_up,
                labels: parsed_labels,
                attachments,
                is_preview_only: true,
            });
        }
        sqlite3_finalize(stmt);
    }

    crate::services::mail_store_service::assign_thread_metadata(&mut messages);

    let next_cursor = if limit > 0 && limit < usize::MAX && messages.len() == limit {
        last_cursor
    } else {
        None
    };

    Ok((messages, next_cursor))
}

/// Fast reader for unread or flagged messages in a specific folder from Evolution's local `folders.db`.
pub fn load_folder_unread_or_flagged_from_sqlite(
    db_path: &Path,
    source_uid: &str,
    folder_name: &str,
    limit: usize,
) -> Result<Vec<ParsedMail>, String> {
    let (effective_source_uid, effective_folder_name) =
        crate::services::camel_mail_service::resolve_shared_mailbox_target(source_uid, folder_name);

    let db = SqliteDb::open(db_path)?;
    let Some((folder_id, real_folder_name)) = db.resolve_folder_id(&effective_folder_name)? else {
        return Ok(Vec::new());
    };

    let table_name = format!("messages_{folder_id}");
    let limit_clause = if limit > 0 && limit < usize::MAX {
        format!(" LIMIT {limit}")
    } else {
        String::new()
    };

    let sql_str = format!(
        "SELECT uid, flags, size, dsent, dreceived, subject, mail_from, mail_to, preview, labels, bdata, usertags, part \
         FROM {table_name} WHERE ((flags & 16) = 0 OR (flags & 8) != 0 OR labels LIKE '%$Flagged%') \
         ORDER BY dreceived DESC, dsent DESC{limit_clause};"
    );

    let c_sql = CString::new(sql_str).map_err(|_| "Ungültiger SQL-String".to_string())?;
    let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
    let rc = unsafe { sqlite3_prepare_v2(db.ptr, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) };
    if rc != SQLITE_OK || stmt.is_null() {
        return Ok(Vec::new());
    }

    let shared_profiles = crate::services::account_config_service::AccountConfigStore::default()
        .load_cached()
        .iter()
        .filter(|p| p.is_shared_mailbox)
        .cloned()
        .collect::<Vec<_>>();

    let is_shared_target = effective_source_uid != source_uid;
    let mut messages = Vec::new();

    unsafe {
        while sqlite3_step(stmt) == SQLITE_ROW {
            let uid_ptr = sqlite3_column_text(stmt, 0);
            if uid_ptr.is_null() {
                continue;
            }
            let uid = CStr::from_ptr(uid_ptr).to_string_lossy().into_owned();
            let flags = sqlite3_column_int(stmt, 1) as u32;
            let size = sqlite3_column_int64(stmt, 2) as usize;
            let dsent = sqlite3_column_int64(stmt, 3);
            let dreceived = sqlite3_column_int64(stmt, 4);

            let subject_ptr = sqlite3_column_text(stmt, 5);
            let subject = if !subject_ptr.is_null() {
                CStr::from_ptr(subject_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let mail_from_ptr = sqlite3_column_text(stmt, 6);
            let mail_from = if !mail_from_ptr.is_null() {
                CStr::from_ptr(mail_from_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let mail_to_ptr = sqlite3_column_text(stmt, 7);
            let mail_to = if !mail_to_ptr.is_null() {
                CStr::from_ptr(mail_to_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let preview_ptr = sqlite3_column_text(stmt, 8);
            let preview = if !preview_ptr.is_null() {
                CStr::from_ptr(preview_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let labels_ptr = sqlite3_column_text(stmt, 9);
            let labels = if !labels_ptr.is_null() {
                CStr::from_ptr(labels_ptr).to_string_lossy().into_owned()
            } else {
                String::new()
            };

            let usertags_ptr = sqlite3_column_text(stmt, 11);
            let (follow_up_state, follow_up) = if !usertags_ptr.is_null() {
                let raw_bytes = CStr::from_ptr(usertags_ptr).to_bytes();
                parse_evolution_usertags_bytes(raw_bytes)
            } else {
                (MailFollowUpState::None, MailFollowUpMetadata::default())
            };

            let is_unread = (flags & 16) == 0;
            let is_flagged = (flags & 8) != 0 || labels.contains("$Flagged");
            let is_important = labels.contains("$Important")
                || labels.contains("Important")
                || labels.contains("Wichtig")
                || labels.contains("wichtig");
            let is_replied = (flags & (1 | 64)) != 0 || labels.contains("$Answered");
            let is_forwarded = (flags & 128) != 0 || labels.contains("$Forwarded");
            let has_cal = labels.contains("$has_cal")
                || is_calendar_invitation_subject_or_preview(&subject, &preview);
            let has_attachments = (flags & 32) != 0 || has_cal;
            let parsed_labels = labels
                .split_whitespace()
                .filter(|s| !s.starts_with('$'))
                .map(|s| s.to_string())
                .collect::<Vec<_>>();

            let effective_ts = if dreceived > 0 { dreceived } else { dsent };
            let date_str = if effective_ts > 0 {
                Utc.timestamp_opt(effective_ts, 0)
                    .single()
                    .map(|dt: DateTime<Utc>| dt.to_rfc2822())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let (sender_name, sender_email) = parse_address(&mail_from);

            let mut mapped_source_uid = source_uid.to_string();
            let mut mapped_folder_name = real_folder_name.clone();

            for profile in &shared_profiles {
                if let Some((rel, is_container)) =
                    crate::services::camel_mail_service::strip_shared_mailbox_prefix(&real_folder_name, &profile.email, &profile.display_name)
                {
                    let target_rel = if is_container || rel.is_empty() {
                        "Posteingang".to_string()
                    } else {
                        rel.to_string()
                    };
                    mapped_source_uid = profile.id.clone();
                    mapped_folder_name = target_rel;
                    break;
                } else if is_shared_target && (profile.id == source_uid || profile.email == source_uid) {
                    mapped_source_uid = profile.id.clone();
                    mapped_folder_name = if folder_name.trim().is_empty() {
                        "Posteingang".to_string()
                    } else {
                        folder_name.to_string()
                    };
                    break;
                }
            }

            let id = format!("eds:{mapped_source_uid}:{mapped_folder_name}:{uid}");
            let backend_location = Some(MailBackendLocation {
                source_uid: mapped_source_uid,
                folder_name: mapped_folder_name.clone(),
                message_uid: uid.clone(),
                message_id: Some(id.clone()),
            });

            let attachments = if has_cal {
                vec![AttachmentInfo {
                    filename: "invite.ics".to_string(),
                    size_formatted: String::new(),
                    mime_type: "text/calendar".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([]),
                }]
            } else if has_attachments {
                vec![AttachmentInfo {
                    filename: "Anhang".to_string(),
                    size_formatted: String::new(),
                    mime_type: "application/octet-stream".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([]),
                }]
            } else {
                Vec::new()
            };

            let normalized_folder_id =
                crate::services::camel_mail_service::normalized_camel_folder_id("", &mapped_folder_name);
            let part_ptr = sqlite3_column_text(stmt, 12);
            let (parsed_msg_id, in_reply_to, references) = if !part_ptr.is_null() {
                let part_bytes = CStr::from_ptr(part_ptr).to_bytes();
                if let Ok(part_str) = std::str::from_utf8(part_bytes) {
                    parse_camel_part_fast(part_str)
                } else {
                    (None, Vec::new(), Vec::new())
                }
            } else {
                (None, Vec::new(), Vec::new())
            };

            messages.push(ParsedMail {
                id,
                folder_id: normalized_folder_id,
                folder_name: mapped_folder_name,
                backend_location,
                sender_name,
                sender_email,
                recipient_to: mail_to,
                recipient_cc: String::new(),
                recipient_bcc: String::new(),
                reply_to: String::new(),
                subject,
                message_id: parsed_msg_id,
                in_reply_to,
                references,
                thread_id: uid,
                thread_size: 1,
                thread_position: 0,
                date_str,
                message_size: size,
                preview_snippet: preview,
                html_body: String::new(),
                raw_headers: None,
                raw_mime: None,
                is_unread,
                is_flagged,
                is_important,
                is_replied,
                is_forwarded,
                follow_up_state,
                follow_up,
                labels: parsed_labels,
                attachments,
                is_preview_only: true,
            });
        }
        sqlite3_finalize(stmt);
    }

    crate::services::mail_store_service::assign_thread_metadata(&mut messages);
    Ok(messages)
}

/// Fast multi-account SQLite loader for Unified Views (All Inboxes, All Unread, All Flagged, All Sent).
/// Loads folder trees for all discovered accounts, plus Inbox messages, Sent messages, and any
/// unread or flagged messages from non-trash/non-junk folders across all accounts.
pub fn load_unified_accounts_from_sqlite(
    page_limit_per_folder: usize,
) -> (
    Vec<crate::services::camel_mail_service::CamelFolderSummary>,
    Vec<ParsedMail>,
    usize,
) {
    let caches = discover_meridian_mail_caches();
    if caches.is_empty() {
        return (Vec::new(), Vec::new(), 0);
    }

    let mut all_folders = Vec::new();
    let mut all_messages = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let mut successful_sources = 0usize;
    let inbox_limit = effective_sqlite_folder_limit(page_limit_per_folder);
    let sent_limit = if limit_memory_usage() {
        page_limit_per_folder.min(100)
    } else {
        inbox_limit.min(5_000)
    };
    let unread_limit = if limit_memory_usage() {
        page_limit_per_folder.max(500)
    } else {
        inbox_limit
    };

    for cache in &caches {
        let folders = load_folders_from_sqlite(&cache.folders_db, &cache.source_uid, &cache.display_name)
            .unwrap_or_default();
        if !folders.is_empty() {
            successful_sources += 1;
        }

        let mut loaded_primary_inbox = false;
        for f in &folders {
            let norm_id =
                crate::services::camel_mail_service::normalized_camel_folder_id("", &f.folder_name);
            if norm_id.eq_ignore_ascii_case("inbox") {
                if f.source_uid == cache.source_uid {
                    loaded_primary_inbox = true;
                }
                if let Ok(msgs) = load_folder_messages_from_sqlite(
                    &cache.folders_db,
                    &f.source_uid,
                    &f.folder_name,
                    Some(inbox_limit),
                ) {
                    for m in msgs {
                        if seen_ids.insert(m.id.clone()) {
                            all_messages.push(m);
                        }
                    }
                }
            } else if norm_id.eq_ignore_ascii_case("sent") {
                if let Ok(msgs) = load_folder_messages_from_sqlite(
                    &cache.folders_db,
                    &f.source_uid,
                    &f.folder_name,
                    Some(sent_limit),
                ) {
                    for m in msgs {
                        if seen_ids.insert(m.id.clone()) {
                            all_messages.push(m);
                        }
                    }
                }
            }
            if !norm_id.eq_ignore_ascii_case("trash")
                && !norm_id.eq_ignore_ascii_case("junk")
                && !norm_id.eq_ignore_ascii_case("spam")
                && (f.total_count > 0 || f.unread_count > 0)
            {
                if let Ok(msgs) = load_folder_unread_or_flagged_from_sqlite(
                    &cache.folders_db,
                    &f.source_uid,
                    &f.folder_name,
                    unread_limit,
                ) {
                    for m in msgs {
                        if seen_ids.insert(m.id.clone()) {
                            all_messages.push(m);
                        }
                    }
                }
            }
        }

        if !loaded_primary_inbox {
            if let Ok(msgs) = load_folder_messages_from_sqlite(
                &cache.folders_db,
                &cache.source_uid,
                "",
                Some(inbox_limit),
            ) {
                for m in msgs {
                    if seen_ids.insert(m.id.clone()) {
                        all_messages.push(m);
                    }
                }
            }
        }

        all_folders.extend(folders);
    }

    all_messages.sort_by(|a, b| {
        let ts_a = chrono::DateTime::parse_from_rfc2822(&a.date_str)
            .map(|dt| dt.timestamp())
            .unwrap_or(0);
        let ts_b = chrono::DateTime::parse_from_rfc2822(&b.date_str)
            .map(|dt| dt.timestamp())
            .unwrap_or(0);
        ts_b.cmp(&ts_a)
    });

    (all_folders, all_messages, successful_sources.max(1))
}

/// Fast reader for folder messages directly from Evolution's local `folders.db`.
pub fn load_folder_messages_from_sqlite(
    db_path: &Path,
    source_uid: &str,
    folder_name: &str,
    limit: Option<usize>,
) -> Result<Vec<ParsedMail>, String> {
    let (messages, _) = load_folder_messages_page_from_sqlite(
        db_path,
        source_uid,
        folder_name,
        None,
        limit.unwrap_or(usize::MAX),
    )?;
    Ok(messages)
}

/// Find local cached raw message body in Evolution's disk cache (`folders/<folder>/cur/*/<uid>`).
pub fn find_cached_raw_message_file(
    cache_dir: &Path,
    folder_name: &str,
    uid: &str,
) -> Option<PathBuf> {
    let folders_root = cache_dir.join("folders");
    if !folders_root.is_dir() {
        return None;
    }

    // Try multiple folder candidate paths (e.g. "INBOX", "[Gmail]/Gesendet", etc.)
    let mut candidates = vec![
        folders_root.join(folder_name).join("cur"),
    ];

    // If folder_name is like "Posteingang", also check "INBOX"
    if folder_name.eq_ignore_ascii_case("posteingang") || folder_name.eq_ignore_ascii_case("inbox") {
        candidates.push(folders_root.join("INBOX").join("cur"));
    }

    let uid_hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, uid)
        .map(|s| s.to_string());

    for cur_dir in candidates {
        if !cur_dir.is_dir() {
            continue;
        }
        // Scan subdirectories (which are 2-character hex prefixes like "0a")
        let Ok(entries) = std::fs::read_dir(&cur_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let target = path.join(uid);
                if target.is_file() {
                    return Some(target);
                }
                if let Some(ref hash) = uid_hash {
                    let target_hash = path.join(hash);
                    if target_hash.is_file() {
                        return Some(target_hash);
                    }
                }
            }
        }
    }

    None
}

/// Read local cached message file directly from disk without blocking on Camel locks.
pub fn read_cached_raw_message_direct(
    cache_dir: &Path,
    folder_name: &str,
    uid: &str,
) -> Option<Vec<u8>> {
    let file_path = find_cached_raw_message_file(cache_dir, folder_name, uid)?;
    std::fs::read(&file_path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_address() {
        let (name, email) = parse_address("Canva <updates@canva.com>");
        assert_eq!(name, "Canva");
        assert_eq!(email, "updates@canva.com");

        let (name2, email2) = parse_address("user@example.com");
        assert_eq!(name2, "user@example.com");
        assert_eq!(email2, "user@example.com");
    }

    #[test]
    fn test_discovery_and_sqlite_reader() {
        let caches = discover_evolution_mail_caches();
        if caches.is_empty() {
            // In CI or environment without Evolution cache, skip gracefully
            return;
        }

        let cache = &caches[0];
        assert!(cache.folders_db.is_file());

        // Test reading folders
        let folders = load_folders_from_sqlite(&cache.folders_db, &cache.source_uid, "Test Account")
            .expect("Reading folders must succeed");
        assert!(!folders.is_empty());
        assert!(folders.iter().any(|f| f.folder_name.eq_ignore_ascii_case("inbox") || f.folder_name == "Posteingang"));

        // Test reading INBOX or Posteingang
        let inbox_folder = folders
            .iter()
            .find(|f| f.folder_name.eq_ignore_ascii_case("inbox") || f.folder_name == "Posteingang")
            .map(|f| f.folder_name.as_str())
            .unwrap_or(&folders[0].folder_name);
        let msgs = load_folder_messages_from_sqlite(
            &cache.folders_db,
            &cache.source_uid,
            inbox_folder,
            Some(10),
        ).expect("Reading folders.db must succeed");

        assert!(!msgs.is_empty());
        assert_eq!(msgs[0].is_preview_only, true);
        assert!(!msgs[0].id.is_empty());

        // Test shared mailbox discovery and message loading if configured
        let shared_profile = crate::services::account_config_service::AccountConfigStore::default()
            .load()
            .into_iter()
            .find(|p| p.is_shared_mailbox);
        if let Some(shared) = shared_profile {
            let shared_cache = find_evolution_account_cache(Some(&shared.id));
            assert!(shared_cache.is_some(), "Shared mailbox account cache must be found");
            let shared_msgs = load_folder_messages_from_sqlite(
                &shared_cache.unwrap().folders_db,
                &shared.id,
                "Posteingang",
                Some(10),
            ).expect("Reading shared mailbox messages must succeed");
            if !shared_msgs.is_empty() {
                assert_eq!(shared_msgs[0].backend_location.as_ref().unwrap().source_uid, shared.id);
            }
        }
    }

    #[test]
    fn test_keyset_pagination() {
        let caches = discover_evolution_mail_caches();
        if caches.is_empty() {
            return;
        }
        let cache = &caches[0];
        let folders = load_folders_from_sqlite(&cache.folders_db, &cache.source_uid, "Test Account")
            .expect("Reading folders must succeed");
        if folders.is_empty() {
            return;
        }
        let inbox_folder = folders
            .iter()
            .find(|f| f.folder_name.eq_ignore_ascii_case("inbox") || f.folder_name == "Posteingang")
            .map(|f| f.folder_name.as_str())
            .unwrap_or(&folders[0].folder_name);

        let (page1, cursor1) = load_folder_messages_page_from_sqlite(
            &cache.folders_db,
            &cache.source_uid,
            inbox_folder,
            None,
            3,
        ).expect("Page 1 must succeed");

        if page1.len() == 3 {
            assert!(cursor1.is_some(), "Full page must yield a next cursor");
            let (page2, _) = load_folder_messages_page_from_sqlite(
                &cache.folders_db,
                &cache.source_uid,
                inbox_folder,
                cursor1,
                3,
            ).expect("Page 2 must succeed");

            // Verify pages are disjoint
            for m1 in &page1 {
                for m2 in &page2 {
                    assert_ne!(m1.id, m2.id, "Page 1 and Page 2 must not contain identical UIDs");
                }
            }
        }
    }

    #[test]
    fn test_is_calendar_invitation_detection() {
        // German subjects
        assert!(is_calendar_invitation_subject_or_preview("Einladung: Team-Meeting", ""));
        assert!(is_calendar_invitation_subject_or_preview("Aw: Einladung: Sprint Planning", ""));
        assert!(is_calendar_invitation_subject_or_preview("WG: Aktualisiert: Architektur Sync", ""));
        assert!(is_calendar_invitation_subject_or_preview("Abgesagt: Jour Fixe", ""));
        assert!(is_calendar_invitation_subject_or_preview("Angenommen: Weekly", ""));
        assert!(is_calendar_invitation_subject_or_preview("Abgelehnt: Workshop", ""));
        assert!(is_calendar_invitation_subject_or_preview("Vorbehaltlich: Review", ""));

        // English subjects
        assert!(is_calendar_invitation_subject_or_preview("Invitation: 1-on-1 Catchup", ""));
        assert!(is_calendar_invitation_subject_or_preview("Re: Updated: Release sync", ""));
        assert!(is_calendar_invitation_subject_or_preview("Canceled: Standup", ""));
        assert!(is_calendar_invitation_subject_or_preview("Accepted: Interview", ""));
        assert!(is_calendar_invitation_subject_or_preview("Declined: Lunch", ""));
        assert!(is_calendar_invitation_subject_or_preview("Tentative: Strategy Meeting", ""));

        // Preview content
        assert!(is_calendar_invitation_subject_or_preview("Kein Betreff", "BEGIN:VCALENDAR\nMETHOD:REQUEST\n..."));
        assert!(is_calendar_invitation_subject_or_preview("Hallo", "BEGIN:VCALENDAR..."));
        assert!(is_calendar_invitation_subject_or_preview("Termin: Kundengespräch", "Wann: Montag, 14:00 Uhr\nWo: Konferenzraum"));
        assert!(is_calendar_invitation_subject_or_preview("Meeting: Product Review", "When: Tuesday 10am\nWhere: Room 3"));

        // Normal messages should not match
        assert!(!is_calendar_invitation_subject_or_preview("Rechnung für März", "Hallo Herr Müller, anbei die Rechnung."));
        assert!(!is_calendar_invitation_subject_or_preview("Projektstatus Bericht", "Hier ist der wöchentliche Bericht."));
    }

    #[test]
    fn test_find_cached_raw_message_sha256() {
        let temp_dir = std::env::temp_dir().join(format!("meridian_test_cache_{}", glib::uuid_string_random()));
        let cur_dir = temp_dir.join("folders").join("Posteingang").join("cur").join("1d");
        std::fs::create_dir_all(&cur_dir).unwrap();

        let uid = "AAMkADRlMWRiOWI0LWM0YTUtNDFmZC1iMzU2LWQzYmU4MzJjMWQyZABGAAA=";
        let sha256_hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, uid).unwrap();
        let file_path = cur_dir.join(sha256_hash.as_str());
        std::fs::write(&file_path, b"From: test@example.com\r\nSubject: Test\r\n\r\nHello").unwrap();

        let found = find_cached_raw_message_file(&temp_dir, "Posteingang", uid);
        assert_eq!(found, Some(file_path));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_edv_sqlite_flags_accurate() {
        if let Some(cache) = find_evolution_account_cache(Some("account:edv@acme-corp.example")) {
            let msgs = load_folder_messages_from_sqlite(&cache.folders_db, "account:edv@acme-corp.example", "Posteingang", Some(50)).unwrap();
            assert!(!msgs.is_empty(), "EDV Posteingang should contain messages in cache");
            
            // Check that not all messages have attachments (previously all had attachments due to bdata bug)
            let without_attachments = msgs.iter().filter(|m| m.attachments.is_empty()).count();
            let with_attachments = msgs.iter().filter(|m| !m.attachments.is_empty()).count();
            assert!(without_attachments > 0, "Messages without attachments must not have attachments populated");
            assert!(with_attachments > 0, "Messages with attachments must still have attachments");

            // Check that flagged messages are correctly recognized from flags
            let flagged = msgs.iter().filter(|m| m.is_flagged).count();
            assert!(flagged > 0, "Flagged messages must be identified from flags bitmask");

            // Check that follow-up messages are correctly identified with their follow-up state
            let follow_ups = msgs.iter().filter(|m| m.follow_up_state != MailFollowUpState::None).count();
            assert!(follow_ups > 0, "Follow-up messages in EDV Posteingang must be extracted from SQLite usertags");
        }
    }

    #[test]
    fn test_parse_evolution_usertags() {
        // Empty / Zero cases
        assert_eq!(parse_evolution_usertags("").0, MailFollowUpState::None);
        assert_eq!(parse_evolution_usertags("0").0, MailFollowUpState::None);
        assert_eq!(parse_evolution_usertags("   0   ").0, MailFollowUpState::None);

        // Active follow-up with due date
        let raw_active = "2 9-follow-up 18-Zur Nachverfolgung 6-due-by 31-Fri, 03 Jul 2026 15:00:00 +0000";
        let (state_active, meta_active) = parse_evolution_usertags(raw_active);
        assert_eq!(state_active, MailFollowUpState::FollowUp);
        assert_eq!(meta_active.text.as_deref(), Some("Zur Nachverfolgung"));
        assert_eq!(meta_active.due_by.as_deref(), Some("Fri, 03 Jul 2026 15:00:00 +0000"));
        assert_eq!(meta_active.completed_on, None);

        // Completed follow-up (matches user's real Evolution data)
        let raw_completed = "2 9-follow-up 18-Zur Nachverfolgung 12-completed-on 31-Mon, 29 Jun 2026 05:45:00 +0000";
        let (state_completed, meta_completed) = parse_evolution_usertags(raw_completed);
        assert_eq!(state_completed, MailFollowUpState::Completed);
        assert_eq!(meta_completed.text.as_deref(), Some("Zur Nachverfolgung"));
        assert_eq!(meta_completed.completed_on.as_deref(), Some("Mon, 29 Jun 2026 05:45:00 +0000"));

        // Unrelated user tags
        let raw_other = "1 11-custom-flag 5-hello";
        let (state_other, _) = parse_evolution_usertags(raw_other);
        assert_eq!(state_other, MailFollowUpState::None);
    }

    #[test]
    fn test_rfc822_priority_detection_for_important() {
        let raw = b"From: boss@example.com\r\nSubject: Urgent Meeting\r\nImportance: High\r\n\r\nPlease attend.";
        let parsed = crate::services::mail_store_service::parse_rfc822_bytes("test-1", "INBOX", raw, true).unwrap();
        assert!(parsed.is_important, "Message with Importance: High must be marked is_important");

        let raw_priority = b"From: server@example.com\r\nSubject: Alert\r\nX-Priority: 1 (Highest)\r\n\r\nCPU alert.";
        let parsed2 = crate::services::mail_store_service::parse_rfc822_bytes("test-2", "INBOX", raw_priority, true).unwrap();
        assert!(parsed2.is_important, "Message with X-Priority: 1 must be marked is_important");
    }
}
