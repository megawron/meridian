// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_longlong, CStr, CString};
use std::path::{Path, PathBuf};

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

/// Evolution mail cache account descriptor.
#[derive(Debug, Clone)]
pub struct EvolutionAccountCache {
    pub source_uid: String,
    pub display_name: String,
    pub folders_db: PathBuf,
    pub cache_dir: PathBuf,
}

/// Helper to extract `DisplayName` from an EDS `.source` file or AccountConfigStore.
fn resolve_source_display_name(source_uid: &str, home: &Path) -> Option<String> {
    let clean = source_uid.strip_suffix("-mail").unwrap_or(source_uid);
    let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
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

    let candidates = [
        home.join(format!(".config/evolution/sources/{source_uid}.source")),
        home.join(format!(".config/evolution/sources/{clean}.source")),
        home.join(format!(".var/app/org.gnome.Evolution/config/evolution/sources/{source_uid}.source")),
        home.join(format!(".var/app/org.gnome.Evolution/config/evolution/sources/{clean}.source")),
    ];

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

/// Discover all local Evolution mail cache directories containing `folders.db`.
pub fn discover_evolution_mail_caches() -> Vec<EvolutionAccountCache> {
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
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let db_path = path.join("folders.db");
                if db_path.is_file() {
                    let source_uid = entry.file_name().to_string_lossy().to_string();
                    if !results.iter().any(|r: &EvolutionAccountCache| r.source_uid == source_uid) {
                        let display_name = resolve_source_display_name(&source_uid, &home)
                            .unwrap_or_else(|| "E-Mail-Konto".to_string());
                        results.push(EvolutionAccountCache {
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

/// Find the cache for a specific account UID, or the default account cache.
pub fn find_evolution_account_cache(source_uid_hint: Option<&str>) -> Option<EvolutionAccountCache> {
    let caches = discover_evolution_mail_caches();
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

    fn query_folders_map(&self) -> Result<HashMap<String, (i64, String)>, String> {
        let sql = CString::new("SELECT folder_name, folder_id FROM folders;").unwrap();
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
                if !text_ptr.is_null() {
                    let name = CStr::from_ptr(text_ptr).to_string_lossy().into_owned();
                    map.insert(name.to_lowercase(), (id, name.clone()));
                    // Also store with stripped "[Gmail]/" prefix for easy matching
                    if let Some(stripped) = name.strip_prefix("[Gmail]/") {
                        map.insert(stripped.to_lowercase(), (id, name.clone()));
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

        // 1. Exact or lower match
        if let Some(res) = map.get(&target_lower) {
            return Ok(Some(res.clone()));
        }

        // 1b. Dash-normalized match (handles em-dash '—' vs en-dash '–' vs hyphen '-')
        let norm = |s: &str| s.replace('—', "-").replace('–', "-").to_lowercase();
        let target_norm = norm(target_folder.trim());
        for (key, res) in &map {
            if norm(key) == target_norm {
                return Ok(Some(res.clone()));
            }
        }

        // 2. Common German/English aliases (only for non-foreign folders to avoid cross-account hijacking)
        if !target_lower.starts_with("foreign folders") {
            let aliases: &[(&[&str], &[&str])] = &[
                (
                    &["posteingang", "inbox"],
                    &["inbox", "posteingang"],
                ),
                (
                    &["gesendet", "gesendete elemente", "sent", "sent items"],
                    &["[gmail]/gesendet", "gesendet", "sent", "sent items"],
                ),
                (
                    &["entwürfe", "entwuerfe", "drafts"],
                    &["[gmail]/entwürfe", "entwürfe", "drafts"],
                ),
                (
                    &["papierkorb", "trash", "gelöschte elemente"],
                    &["[gmail]/papierkorb", "papierkorb", "trash"],
                ),
                (
                    &["spam", "junk", "junk-e-mail"],
                    &["[gmail]/spam", "spam", "junk"],
                ),
                (
                    &["markiert", "starred"],
                    &["[gmail]/markiert", "markiert", "starred"],
                ),
                (
                    &["wichtig", "important"],
                    &["[gmail]/wichtig", "wichtig", "important"],
                ),
                (
                    &["alle nachrichten", "all mail"],
                    &["[gmail]/alle nachrichten", "alle nachrichten", "all mail"],
                ),
            ];

            for (inputs, targets) in aliases {
                if inputs.iter().any(|&i| i == target_lower || target_lower.ends_with(i)) {
                    for &cand in *targets {
                        if let Some(res) = map.get(cand) {
                            return Ok(Some(res.clone()));
                        }
                    }
                }
            }
        }

        // 3. Substring matching
        for (key, res) in &map {
            if norm(key).contains(&target_norm) || target_norm.contains(&norm(key)) {
                return Ok(Some(res.clone()));
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
        .load()
        .into_iter()
        .filter(|p| p.is_shared_mailbox)
        .collect::<Vec<_>>();

    let mut folders = Vec::new();
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

                let folder_id = format!("{}:{}", mapped_source_uid, mapped_folder_name);
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

/// Keyset cursor for pagination in Evolution SQLite mail summaries: (dreceived, dsent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageCursor {
    pub dreceived: i64,
    pub dsent: i64,
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

    let where_clause = match cursor {
        Some(c) => format!(
            " WHERE (dreceived < {}) OR (dreceived = {} AND dsent < {})",
            c.dreceived, c.dreceived, c.dsent
        ),
        None => String::new(),
    };

    let sql_str = format!(
        "SELECT uid, flags, size, dsent, dreceived, subject, mail_from, mail_to, preview \
         FROM {table_name}{where_clause} ORDER BY dreceived DESC, dsent DESC{limit_clause};"
    );

    let c_sql = CString::new(sql_str).map_err(|_| "Ungültiger SQL-String".to_string())?;
    let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
    let rc = unsafe { sqlite3_prepare_v2(db.ptr, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) };
    if rc != SQLITE_OK || stmt.is_null() {
        return Err(format!("Konnte Abfrage auf {table_name} nicht vorbereiten"));
    }

    let shared_profiles = crate::services::account_config_service::AccountConfigStore::default()
        .load()
        .into_iter()
        .filter(|p| p.is_shared_mailbox)
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

            // Flags mapping:
            // 16 = CAMEL_MESSAGE_SEEN
            // 8  = CAMEL_MESSAGE_FLAGGED
            // 1  = CAMEL_MESSAGE_ANSWERED, 64 = CAMEL_MESSAGE_ANSWERED_ALL
            // 128 = CAMEL_MESSAGE_FORWARDED
            // 32 = CAMEL_MESSAGE_ATTACHMENTS
            let is_unread = (flags & 16) == 0;
            let is_flagged = (flags & 8) != 0;
            let is_replied = (flags & (1 | 64)) != 0;
            let is_forwarded = (flags & 128) != 0;
            let has_attachments = (flags & 32) != 0;

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

            let attachments = if has_attachments {
                vec![AttachmentInfo {
                    filename: "Anhang".to_string(),
                    size_formatted: String::new(),
                    mime_type: "application/octet-stream".to_string(),
                    content_id: None,
                    content: std::sync::Arc::from([]),
                }]
            } else {
                Vec::new()
            };

            let normalized_folder_id =
                crate::services::camel_mail_service::normalized_camel_folder_id("", &mapped_folder_name);
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
                message_id: None,
                in_reply_to: Vec::new(),
                references: Vec::new(),
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
                is_important: false,
                is_replied,
                is_forwarded,
                follow_up_state: MailFollowUpState::None,
                follow_up: MailFollowUpMetadata::default(),
                labels: Vec::new(),
                attachments,
                is_preview_only: true,
            });
        }
        sqlite3_finalize(stmt);
    }

    let next_cursor = if limit > 0 && limit < usize::MAX && messages.len() == limit {
        last_cursor
    } else {
        None
    };

    Ok((messages, next_cursor))
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
}
