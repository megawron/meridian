// SPDX-License-Identifier: GPL-3.0-or-later
//
// Direct read-only loader for Evolution Data Server SQLite address book caches.
// Matches the architecture of evolution_summary_cache for mail, reading
// ~/.cache/evolution/addressbook/*/cache.db and ~/.local/share/evolution/addressbook/*/contacts.db
// with microsecond latency without blocking on D-Bus or remote backends.

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::{Path, PathBuf};

use crate::services::ebook_service::EBookContactRecord;
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

    fn query_contacts_cache(&self) -> Vec<(String, String)> {
        let sql = CString::new("SELECT ECacheUID, ECacheOBJ FROM ECacheObjects;").unwrap();
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

    fn query_contacts_local(&self) -> Vec<(String, String)> {
        let sql = CString::new("SELECT uid, vcard FROM folder_id;").unwrap();
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
#[derive(Debug)]
struct AddressBookDbTarget {
    db_path: PathBuf,
    is_cache_table: bool,
    source_uid: String,
    display_name: String,
}

fn resolve_source_display_name(source_uid: &str, home: &Path) -> Option<String> {
    let candidates = [
        home.join(format!(".config/evolution/sources/{source_uid}.source")),
        home.join(format!(
            ".var/app/org.gnome.Evolution/config/evolution/sources/{source_uid}.source"
        )),
    ];

    for path in &candidates {
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
    None
}

/// Discover all native EDS address book SQLite databases.
fn discover_address_book_dbs() -> Vec<AddressBookDbTarget> {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return Vec::new(),
    };

    let mut targets = Vec::new();

    // 1. Native EDS cache databases (~/.cache/evolution/addressbook/*/cache.db)
    let cache_root = home.join(".cache/evolution/addressbook");
    if let Ok(entries) = std::fs::read_dir(&cache_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let db_path = path.join("cache.db");
                if db_path.is_file() {
                    let source_uid = entry.file_name().to_string_lossy().into_owned();
                    let display_name = resolve_source_display_name(&source_uid, &home)
                        .unwrap_or_else(|| "Kontakte".to_string());
                    targets.push(AddressBookDbTarget {
                        db_path,
                        is_cache_table: true,
                        source_uid,
                        display_name,
                    });
                }
            }
        }
    }

    // 2. Native EDS system contacts (~/.local/share/evolution/addressbook/system/contacts.db)
    let local_system = home.join(".local/share/evolution/addressbook/system/contacts.db");
    if local_system.is_file() {
        targets.push(AddressBookDbTarget {
            db_path: local_system,
            is_cache_table: false,
            source_uid: "system".to_string(),
            display_name: "Persönlich".to_string(),
        });
    }

    // 3. Flatpak fallback
    if targets.is_empty() {
        let flatpak_cache =
            home.join(".var/app/org.gnome.Evolution/cache/evolution/addressbook");
        if let Ok(entries) = std::fs::read_dir(&flatpak_cache) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let db_path = path.join("cache.db");
                    if db_path.is_file() {
                        let source_uid = entry.file_name().to_string_lossy().into_owned();
                        let display_name = resolve_source_display_name(&source_uid, &home)
                            .unwrap_or_else(|| "Kontakte".to_string());
                        targets.push(AddressBookDbTarget {
                            db_path,
                            is_cache_table: true,
                            source_uid,
                            display_name,
                        });
                    }
                }
            }
        }
    }

    targets
}

/// Load all contacts directly from the local Evolution SQLite databases.
pub fn load_contacts_from_evolution_sqlite_cache() -> Vec<EBookContactRecord> {
    let targets = discover_address_book_dbs();
    let mut records = Vec::new();

    for target in targets {
        let Some(db) = SqliteDb::open(&target.db_path) else {
            continue;
        };

        let raw_contacts = if target.is_cache_table {
            db.query_contacts_cache()
        } else {
            db.query_contacts_local()
        };

        for (uid, vcard) in raw_contacts {
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
        }
    }

    records
}
