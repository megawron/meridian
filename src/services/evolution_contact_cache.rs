// SPDX-License-Identifier: GPL-3.0-or-later
//
// Direct read-only loader for Evolution Data Server SQLite address book caches.
// Reads ~/.cache/evolution/addressbook/**/cache.db,
// ~/.var/app/org.gnome.Evolution/cache/evolution/addressbook/**/cache.db, and
// system contacts.db files with microsecond latency without blocking on D-Bus.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, CStr, CString};
use std::path::{Path, PathBuf};

use crate::services::ebook_service::EBookContactRecord;
use crate::services::eds_service::EdsSource;
use crate::services::vcard_service::VCardContact;

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
    fn sqlite3_finalize(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
}

struct SqliteDb {
    ptr: *mut Sqlite3,
}

impl SqliteDb {
    fn open(path: &Path) -> Option<Self> {
        let path_str = path.to_str()?;
        let c_path = CString::new(path_str).ok()?;
        let mut ptr: *mut Sqlite3 = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_open_v2(
                c_path.as_ptr(),
                &mut ptr,
                SQLITE_OPEN_READONLY,
                std::ptr::null(),
            )
        };
        if rc == SQLITE_OK && !ptr.is_null() {
            Some(Self { ptr })
        } else {
            if !ptr.is_null() {
                unsafe { sqlite3_close(ptr) };
            }
            None
        }
    }

    fn count_contacts(&self, is_cache_table: bool) -> usize {
        let sql_str = if is_cache_table {
            "SELECT count(*) FROM ECacheObjects;"
        } else {
            "SELECT count(*) FROM folder_id;"
        };
        let Ok(sql) = CString::new(sql_str) else {
            return 0;
        };
        let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(self.ptr, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return 0;
        }

        let mut count = 0;
        unsafe {
            if sqlite3_step(stmt) == SQLITE_ROW {
                let col_val = sqlite3_column_text(stmt, 0);
                if !col_val.is_null() {
                    if let Ok(s) = CStr::from_ptr(col_val).to_str() {
                        count = s.parse::<usize>().unwrap_or(0);
                    }
                }
            }
            sqlite3_finalize(stmt);
        }
        count
    }

    fn query_contacts_cache(&self, limit: Option<usize>) -> Vec<(String, String)> {
        let sql_str = match limit {
            Some(lim) if lim > 0 => format!("SELECT ECacheUID, ECacheOBJ FROM ECacheObjects LIMIT {lim};"),
            _ => "SELECT ECacheUID, ECacheOBJ FROM ECacheObjects;".to_string(),
        };
        let Ok(sql) = CString::new(sql_str) else {
            return Vec::new();
        };
        let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(self.ptr, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Vec::new();
        }

        let mut results = Vec::new();
        unsafe {
            while sqlite3_step(stmt) == SQLITE_ROW {
                let uid_ptr = sqlite3_column_text(stmt, 0);
                let obj_ptr = sqlite3_column_text(stmt, 1);
                if !uid_ptr.is_null() && !obj_ptr.is_null() {
                    let uid = CStr::from_ptr(uid_ptr).to_string_lossy().into_owned();
                    let vcard = CStr::from_ptr(obj_ptr).to_string_lossy().into_owned();
                    results.push((uid, vcard));
                }
            }
            sqlite3_finalize(stmt);
        }
        results
    }

    fn query_contacts_local(&self, limit: Option<usize>) -> Vec<(String, String)> {
        let sql_str = match limit {
            Some(lim) if lim > 0 => format!("SELECT uid, vcard FROM folder_id LIMIT {lim};"),
            _ => "SELECT uid, vcard FROM folder_id;".to_string(),
        };
        let Ok(sql) = CString::new(sql_str) else {
            return Vec::new();
        };
        let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(self.ptr, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Vec::new();
        }

        let mut results = Vec::new();
        unsafe {
            while sqlite3_step(stmt) == SQLITE_ROW {
                let uid_ptr = sqlite3_column_text(stmt, 0);
                let vcard_ptr = sqlite3_column_text(stmt, 1);
                if !uid_ptr.is_null() && !vcard_ptr.is_null() {
                    let uid = CStr::from_ptr(uid_ptr).to_string_lossy().into_owned();
                    let vcard = CStr::from_ptr(vcard_ptr).to_string_lossy().into_owned();
                    results.push((uid, vcard));
                }
            }
            sqlite3_finalize(stmt);
        }
        results
    }
}

impl Drop for SqliteDb {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                sqlite3_close(self.ptr);
            }
        }
    }
}

/// Address book database target
#[derive(Debug, Clone)]
pub struct AddressBookDbTarget {
    pub db_path: PathBuf,
    pub is_cache_table: bool,
    pub source_uid: String,
    pub display_name: String,
    pub parent_name: Option<String>,
    pub backend_name: Option<String>,
}

#[derive(Default, Debug, Clone)]
struct RawSourceInfo {
    display_name: Option<String>,
    parent_uid: Option<String>,
    backend_name: Option<String>,
    identity: Option<String>,
    user: Option<String>,
    foreign_mail: Option<String>,
}

/// Build an index of all `.source` configuration files found across host EDS
/// and Flatpak Evolution config/cache directories.
fn build_source_index(home: &Path) -> HashMap<String, RawSourceInfo> {
    let search_roots = [
        home.join(".config/evolution/sources"),
        home.join(".cache/evolution/sources"),
        home.join(".var/app/org.gnome.Evolution/config/evolution/sources"),
        home.join(".var/app/org.gnome.Evolution/cache/evolution/sources"),
    ];

    let mut index = HashMap::new();

    for root in &search_roots {
        collect_source_files(root, &mut index);
    }

    index
}

fn collect_source_files(dir: &Path, index: &mut HashMap<String, RawSourceInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_files(&path, index);
        } else if path.is_file() {
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(uid) = file_name.strip_suffix(".source") {
                if !index.contains_key(uid) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let mut info = RawSourceInfo::default();
                        for line in content.lines() {
                            let trimmed = line.trim();
                            if let Some(val) = trimmed.strip_prefix("DisplayName=") {
                                if info.display_name.is_none() && !val.trim().is_empty() {
                                    info.display_name = Some(val.trim().to_string());
                                }
                            } else if let Some(val) = trimmed.strip_prefix("Parent=") {
                                if info.parent_uid.is_none() && !val.trim().is_empty() {
                                    info.parent_uid = Some(val.trim().to_string());
                                }
                            } else if let Some(val) = trimmed.strip_prefix("BackendName=") {
                                if info.backend_name.is_none() && !val.trim().is_empty() {
                                    info.backend_name = Some(val.trim().to_string());
                                }
                            } else if let Some(val) = trimmed.strip_prefix("Identity=") {
                                if info.identity.is_none() && !val.trim().is_empty() {
                                    info.identity = Some(val.trim().to_string());
                                }
                            } else if let Some(val) = trimmed.strip_prefix("User=") {
                                if info.user.is_none() && !val.trim().is_empty() {
                                    info.user = Some(val.trim().to_string());
                                }
                            } else if let Some(val) = trimmed.strip_prefix("ForeignMail=") {
                                if info.foreign_mail.is_none() && !val.trim().is_empty() {
                                    info.foreign_mail = Some(val.trim().to_string());
                                }
                            }
                        }
                        index.insert(uid.to_string(), info);
                    }
                }
            }
        }
    }
}

/// Recursively find all `cache.db` and `contacts.db` files in the given directory.
fn collect_db_files(dir: &Path, db_paths: &mut Vec<(PathBuf, bool)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_db_files(&path, db_paths);
        } else if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == "cache.db" {
                    db_paths.push((path, true));
                } else if name == "contacts.db" {
                    db_paths.push((path, false));
                }
            }
        }
    }
}

/// Discover all native EDS and Flatpak Evolution address book SQLite databases.
pub fn discover_address_book_dbs() -> Vec<AddressBookDbTarget> {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return Vec::new(),
    };

    let source_index = build_source_index(&home);

    // Find any Google account display name in the source index for CardDAV fallback
    let google_account_name = source_index
        .iter()
        .find(|(_, info)| {
            info.backend_name.as_deref() == Some("google")
                || info
                    .display_name
                    .as_deref()
                    .is_some_and(|name| name.contains("@gmail.com"))
        })
        .and_then(|(_, info)| info.display_name.clone())
        .unwrap_or_else(|| "Google".to_string());

    let mut db_candidates = Vec::new();

    // 1. Host EDS cache & local databases
    collect_db_files(&home.join(".cache/evolution/addressbook"), &mut db_candidates);
    collect_db_files(
        &home.join(".local/share/evolution/addressbook"),
        &mut db_candidates,
    );

    // 2. Flatpak Evolution cache & data databases (including trash/ where Google contacts live)
    collect_db_files(
        &home.join(".var/app/org.gnome.Evolution/cache/evolution/addressbook"),
        &mut db_candidates,
    );
    collect_db_files(
        &home.join(".var/app/org.gnome.Evolution/data/evolution/addressbook"),
        &mut db_candidates,
    );

    let mut candidate_map: HashMap<(String, String), (AddressBookDbTarget, usize)> = HashMap::new();
    let mut seen_uids = HashSet::new();

    for (db_path, is_cache_table) in db_candidates {
        let Some(parent_dir) = db_path.parent() else {
            continue;
        };
        let Some(source_uid_raw) = parent_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // If parent is literally named "trash", look at the grandparent
        let source_uid = if source_uid_raw == "trash" {
            continue;
        } else {
            source_uid_raw.to_string()
        };

        // Open database and check contact count
        let Some(db) = SqliteDb::open(&db_path) else {
            continue;
        };

        let contact_count = db.count_contacts(is_cache_table);
        if contact_count == 0 {
            continue;
        }

        // Avoid adding exact duplicate targets by UID
        if !seen_uids.insert(source_uid.clone()) {
            continue;
        }

        // Resolve display name and parent name
        let raw_info = source_index.get(&source_uid);
        let mut display_name = raw_info
            .and_then(|i| i.display_name.clone().or_else(|| i.identity.clone()))
            .unwrap_or_else(|| {
                if source_uid == "system" || source_uid == "system-address-book" {
                    "Persönlich".to_string()
                } else {
                    "Kontakte".to_string()
                }
            });

        let mut parent_name = raw_info
            .and_then(|i| i.parent_uid.as_deref())
            .and_then(|puid| source_index.get(puid))
            .and_then(|pinfo| pinfo.display_name.clone().or_else(|| pinfo.identity.clone()));

        if parent_name.is_none() {
            if let Some(fm) = raw_info.and_then(|i| i.foreign_mail.clone()) {
                parent_name = Some(fm);
            } else if let Some(u) = raw_info.and_then(|i| i.user.clone()) {
                let clean = u.rsplit('\\').next().unwrap_or(&u);
                parent_name = Some(clean.to_string());
            } else if let Some(id) = raw_info.and_then(|i| i.identity.clone()) {
                parent_name = Some(id);
            }
        }

        let mut backend_name = raw_info.and_then(|i| i.backend_name.clone());

        // Special handling for Google CardDAV cache (e.g. 565995fe...)
        let is_google = source_uid.starts_with("5659")
            || raw_info.and_then(|i| i.backend_name.as_deref()) == Some("google")
            || raw_info.and_then(|i| i.user.as_deref()).is_some_and(|u| u.contains("@gmail.com"))
            || parent_name.as_deref().is_some_and(|p| p.contains("@gmail.com"));

        if is_google {
            if parent_name.is_none() {
                parent_name = Some(google_account_name.clone());
            }
            if display_name == "Kontakte" {
                display_name = "Kontakte".to_string();
            }
            backend_name = Some("google".to_string());
        }

        let target = AddressBookDbTarget {
            db_path,
            is_cache_table,
            source_uid,
            display_name,
            parent_name,
            backend_name,
        };

        // Deduplicate: If multiple DB targets have the same (parent_name, display_name),
        // keep the one with higher contact count.
        let dedup_key = (
            target.parent_name.as_deref().unwrap_or("").to_lowercase(),
            target.display_name.to_lowercase(),
        );

        if let Some((_, existing_count)) = candidate_map.get(&dedup_key) {
            if contact_count > *existing_count {
                candidate_map.insert(dedup_key, (target, contact_count));
            }
        } else {
            candidate_map.insert(dedup_key, (target, contact_count));
        }
    }

    let mut targets: Vec<AddressBookDbTarget> = candidate_map.into_values().map(|(t, _)| t).collect();
    targets.sort_by(|a, b| {
        (
            a.parent_name.as_deref().unwrap_or(""),
            a.display_name.as_str(),
        )
            .cmp(&(
                b.parent_name.as_deref().unwrap_or(""),
                b.display_name.as_str(),
            ))
    });
    targets
}

/// Load contacts directly from the local SQLite databases, with an optional limit.
pub fn load_contacts_from_meridian_sqlite_cache_limited(limit: Option<usize>) -> Vec<EBookContactRecord> {
    let targets = discover_address_book_dbs();
    let mut records = Vec::new();
    let mut seen_contact_identities: HashSet<String> = HashSet::new();

    for target in targets {
        let Some(db) = SqliteDb::open(&target.db_path) else {
            continue;
        };

        let raw_contacts = if target.is_cache_table {
            db.query_contacts_cache(limit)
        } else {
            db.query_contacts_local(limit)
        };

        for (uid, vcard) in raw_contacts {
            // Deduplicate contacts that exist identically across Flatpak & native caches
            // by UID (or name if UID is synthetic)
            let dedup_key = format!("{}:{}", target.parent_name.as_deref().unwrap_or(""), uid);
            if !seen_contact_identities.insert(dedup_key) {
                continue;
            }

            let Some(contact) = VCardContact::parse_vcard_string(&vcard) else {
                continue;
            };

            records.push(EBookContactRecord {
                source_uid: target.source_uid.clone(),
                source_display_name: target.display_name.clone(),
                uid,
                vcard,
                contact,
            });

            if let Some(lim) = limit {
                if records.len() >= lim {
                    return records;
                }
            }
        }
    }

    records
}

/// Load all contacts directly from local SQLite databases.
pub fn load_contacts_from_meridian_sqlite_cache() -> Vec<EBookContactRecord> {
    load_contacts_from_meridian_sqlite_cache_limited(None)
}

#[allow(unused_imports)]
pub use load_contacts_from_meridian_sqlite_cache as load_contacts_from_evolution_sqlite_cache;

/// List all address book sources discovered in Evolution caches, formatted as `EdsSource`.
pub fn list_cached_address_book_sources() -> Vec<EdsSource> {
    let targets = discover_address_book_dbs();
    targets
        .into_iter()
        .map(|target| EdsSource {
            uid: target.source_uid,
            display_name: target.display_name,
            backend_name: target
                .backend_name
                .unwrap_or_else(|| "sqlite-cache".to_string()),
            enabled: true,
            writable: true,
            parent_name: target.parent_name,
            color: None,
            foreign_mail: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_address_book_dbs() {
        let targets = discover_address_book_dbs();
        println!("Discovered {} address book DB targets", targets.len());
        for t in &targets {
            println!(
                "  target: uid={}, name={}, parent={:?}, path={:?}",
                t.source_uid, t.display_name, t.parent_name, t.db_path
            );
        }
        let sources = list_cached_address_book_sources();
        println!("Discovered {} EDS sources from cache", sources.len());
    }

    #[test]
    fn test_load_contacts_from_evolution_sqlite_cache() {
        let contacts = load_contacts_from_evolution_sqlite_cache();
        println!("Loaded {} contacts from Evolution cache", contacts.len());
        for c in contacts.iter().take(5) {
            println!("  contact: {} ({}) - source: {}", c.contact.fn_name, c.uid, c.source_uid);
        }
        assert!(!contacts.is_empty(), "Expected contacts from cache, got {}", contacts.len());
    }
}
