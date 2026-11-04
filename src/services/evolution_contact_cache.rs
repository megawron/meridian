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
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READONLY: c_int = 0x00000001;
const SQLITE_OPEN_READWRITE: c_int = 0x00000002;
const SQLITE_OPEN_CREATE: c_int = 0x00000004;
const SQLITE_STATIC: isize = 0;

unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        ppDb: *mut *mut Sqlite3,
        flags: c_int,
        zVfs: *const c_char,
    ) -> c_int;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(
                *mut std::ffi::c_void,
                c_int,
                *mut *mut c_char,
                *mut *mut c_char,
            ) -> c_int,
        >,
        arg: *mut std::ffi::c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        zSql: *const c_char,
        nByte: c_int,
        ppStmt: *mut *mut Sqlite3Stmt,
        pzTail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_reset(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_clear_bindings(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_bind_int64(pStmt: *mut Sqlite3Stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_bind_text(
        pStmt: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_char,
        n: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_column_text(pStmt: *mut Sqlite3Stmt, iCol: c_int) -> *const c_char;
    fn sqlite3_column_int64(pStmt: *mut Sqlite3Stmt, iCol: c_int) -> i64;
    fn sqlite3_finalize(pStmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
}

struct SqliteDb {
    ptr: *mut Sqlite3,
}

impl SqliteDb {
    fn open(path: &Path) -> Option<Self> {
        Self::open_with_mode(path, false)
    }

    fn open_with_mode(path: &Path, writable: bool) -> Option<Self> {
        let path_str = path.to_str()?;
        let c_path = CString::new(path_str).ok()?;
        let mut ptr: *mut Sqlite3 = std::ptr::null_mut();
        let flags = if writable {
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE
        } else {
            SQLITE_OPEN_READONLY
        };
        let rc = unsafe {
            sqlite3_open_v2(
                c_path.as_ptr(),
                &mut ptr,
                flags,
                std::ptr::null(),
            )
        };
        if rc == SQLITE_OK && !ptr.is_null() {
            let pragma = c"PRAGMA mmap_size = 4294967296; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -65536;";
            unsafe {
                let _ = sqlite3_exec(
                    ptr,
                    pragma.as_ptr(),
                    None,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
            Some(Self { ptr })
        } else {
            if !ptr.is_null() {
                unsafe { sqlite3_close(ptr) };
            }
            None
        }
    }

    fn count_contacts(&self, is_cache_table: bool) -> usize {
        // Fast O(1) B-tree right-edge probe via max(rowid), falling back to count(*)
        let fast_sql = if is_cache_table {
            c"SELECT max(rowid) FROM ECacheObjects;"
        } else {
            c"SELECT max(rowid) FROM folder_id;"
        };
        let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
        unsafe {
            if sqlite3_prepare_v2(self.ptr, fast_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
                == SQLITE_OK
                && !stmt.is_null()
            {
                if sqlite3_step(stmt) == SQLITE_ROW {
                    let max_id = sqlite3_column_int64(stmt, 0);
                    if max_id > 0 {
                        sqlite3_finalize(stmt);
                        return max_id as usize;
                    }
                }
                sqlite3_finalize(stmt);
            }
        }

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

    fn query_contacts_window(
        &self,
        is_cache_table: bool,
        offset: usize,
        limit: usize,
    ) -> Vec<(String, String)> {
        if limit == 0 {
            return Vec::new();
        }
        let sql = if is_cache_table {
            c"SELECT ECacheUID, ECacheOBJ FROM ECacheObjects WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2;"
        } else {
            c"SELECT uid, vcard FROM folder_id WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2;"
        };
        let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(self.ptr, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Vec::new();
        }

        let mut results = Vec::with_capacity(limit.min(256));
        unsafe {
            sqlite3_bind_int64(stmt, 1, offset as i64);
            sqlite3_bind_int64(stmt, 2, limit as i64);
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

/// Recursively find all `cache.db` and `contacts.db` files in the given directory,
/// skipping any `trash` subdirectories where deleted address books are moved.
fn collect_db_files(dir: &Path, db_paths: &mut Vec<(PathBuf, bool)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("trash"))
            {
                continue;
            }
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

    let mut db_candidates = Vec::new();

    // 1. Host EDS cache & local databases
    collect_db_files(&home.join(".cache/evolution/addressbook"), &mut db_candidates);
    collect_db_files(
        &home.join(".local/share/evolution/addressbook"),
        &mut db_candidates,
    );

    // 2. Flatpak Evolution cache & data databases (only when no host EDS caches exist)
    if db_candidates.is_empty() {
        collect_db_files(
            &home.join(".var/app/org.gnome.Evolution/cache/evolution/addressbook"),
            &mut db_candidates,
        );
        collect_db_files(
            &home.join(".var/app/org.gnome.Evolution/data/evolution/addressbook"),
            &mut db_candidates,
        );
    }

    let mut candidate_map: HashMap<(String, String), (AddressBookDbTarget, usize)> = HashMap::new();
    let mut seen_uids = HashSet::new();

    for (db_path, is_cache_table) in db_candidates {
        let Some(parent_dir) = db_path.parent() else {
            continue;
        };
        let Some(source_uid_raw) = parent_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        if source_uid_raw.eq_ignore_ascii_case("trash")
            || db_path.components().any(|c| {
                c.as_os_str()
                    .to_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("trash"))
            })
        {
            continue;
        }
        let source_uid = source_uid_raw.to_string();

        // Check if this source is enabled for AddressBook in EDS / GOA
        if !crate::services::eds_service::is_source_enabled_for_service(
            &source_uid,
            crate::services::eds_service::EdsSourceKind::AddressBook,
        ) {
            continue;
        }

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
        if let Some(parent_uid) = raw_info.and_then(|i| i.parent_uid.as_deref()) {
            if !crate::services::eds_service::is_parent_service_enabled(
                parent_uid,
                crate::services::eds_service::EdsSourceKind::AddressBook,
            ) {
                continue;
            }
        }

        let display_name = raw_info
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

        let parent_str = parent_name.as_deref().unwrap_or("");
        if crate::services::eds_service::is_shared_or_delegate_mailbox(parent_str)
            || crate::services::eds_service::is_shared_or_delegate_mailbox(&display_name)
            || crate::services::eds_service::is_goa_identity_service_enabled(
                parent_str,
                crate::services::eds_service::EdsSourceKind::AddressBook,
            ) == Some(false)
            || crate::services::eds_service::is_goa_identity_service_enabled(
                &display_name,
                crate::services::eds_service::EdsSourceKind::AddressBook,
            ) == Some(false)
        {
            continue;
        }

        let backend_name = raw_info.and_then(|i| i.backend_name.clone());

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

/// Return the total contact count for a specific SQLite address book database in $O(1)$ time.
pub fn query_contacts_count_from_sqlite(db_path: &Path, is_cache_table: bool) -> usize {
    let Some(db) = SqliteDb::open(db_path) else {
        return 0;
    };
    db.count_contacts(is_cache_table)
}

/// Load a viewport slice (`offset`, `limit`) of contacts from a specific SQLite address book
/// database in $O(\log N)$ time using `rowid` anchor probing.
pub fn load_contacts_window_from_sqlite(
    db_path: &Path,
    is_cache_table: bool,
    source_uid: &str,
    source_display_name: &str,
    offset: usize,
    limit: usize,
) -> Vec<EBookContactRecord> {
    let Some(db) = SqliteDb::open(db_path) else {
        return Vec::new();
    };
    let raw = db.query_contacts_window(is_cache_table, offset, limit);
    let mut records = Vec::with_capacity(raw.len());
    for (uid, vcard) in raw {
        if let Some(contact) = VCardContact::parse_vcard_string(&vcard) {
            records.push(EBookContactRecord {
                source_uid: source_uid.to_string(),
                source_display_name: source_display_name.to_string(),
                uid,
                vcard,
                contact,
            });
        }
    }
    records
}

/// Populate a synthetic Evolution `EBookCache` SQLite database (`ECacheObjects`) with
/// `total_contacts` enterprise vCard 4.0 contacts for multi-million scale benchmarking.
/// Returns `(elapsed_seconds, db_size_bytes)`.
pub fn populate_synthetic_ebook_cache_db(
    db_path: &Path,
    total_contacts: usize,
) -> Result<(f64, u64), String> {
    let start_instant = std::time::Instant::now();
    let _ = std::fs::remove_file(db_path);
    let Some(db) = SqliteDb::open_with_mode(db_path, true) else {
        return Err("Failed to open synthetic EBookCache SQLite DB".to_string());
    };

    let init_sql = c"PRAGMA synchronous = OFF; \
                     PRAGMA journal_mode = OFF; \
                     PRAGMA locking_mode = EXCLUSIVE; \
                     PRAGMA temp_store = MEMORY; \
                     PRAGMA cache_size = -524288; \
                     PRAGMA page_size = 4096; \
                     CREATE TABLE ECacheObjects ( \
                         ECacheUID TEXT PRIMARY KEY, \
                         ECacheREV TEXT, \
                         ECacheOBJ TEXT, \
                         ECacheState INTEGER, \
                         pgpCert INTEGER, \
                         x509Cert INTEGER, \
                         wants_html INTEGER, \
                         list_show_addresses INTEGER, \
                         is_list INTEGER, \
                         family_name_localized TEXT, \
                         family_name TEXT, \
                         given_name_localized TEXT, \
                         given_name TEXT, \
                         full_name TEXT, \
                         nickname TEXT, \
                         file_as_localized TEXT, \
                         file_as TEXT, \
                         custom_flags INTEGER, \
                         bdata TEXT \
                     ); \
                     BEGIN TRANSACTION;";

    unsafe {
        if sqlite3_exec(
            db.ptr,
            init_sql.as_ptr(),
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) != SQLITE_OK
        {
            return Err("Failed to initialize EBookCache ECacheObjects table".to_string());
        }
    }

    let insert_sql = c"INSERT INTO ECacheObjects ( \
        ECacheUID, ECacheREV, ECacheOBJ, ECacheState, is_list, \
        family_name, given_name, full_name, file_as \
    ) VALUES (?1, '2026-11-03T12:00:00Z', ?2, 0, 0, ?3, ?4, ?5, ?6);";

    let mut stmt: *mut Sqlite3Stmt = std::ptr::null_mut();
    unsafe {
        if sqlite3_prepare_v2(db.ptr, insert_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
            != SQLITE_OK
            || stmt.is_null()
        {
            return Err("Failed to prepare EBookCache INSERT statement".to_string());
        }
    }

    let given_names: [&str; 8] = [
        "Alexander", "Katharina", "Maximilian", "Sophie", "Sebastian", "johanna", "lukas", "elena",
    ];
    let family_names: [&str; 8] = [
        "Mueller", "Schmidt", "Schneider", "Fischer", "Weber", "Meyer", "Wagner", "Becker",
    ];
    let orgs: [&str; 4] = [
        "Meridian Systems GmbH",
        "Alpine Cloud Infrastructure AG",
        "Nordic Photonics Corp",
        "Continental Enterprise Engineering",
    ];

    let mut uid_buf = String::with_capacity(32);
    let mut full_name_buf = String::with_capacity(64);
    let mut file_as_buf = String::with_capacity(64);
    let mut vcard_buf = String::with_capacity(320);

    for i in 0..total_contacts {
        uid_buf.clear();
        full_name_buf.clear();
        file_as_buf.clear();
        vcard_buf.clear();

        let given = given_names[i & 7];
        let family = family_names[(i >> 3) & 7];
        let org = orgs[i & 3];

        use std::fmt::Write as _;
        let _ = write!(&mut uid_buf, "pas-id-{:08}", i + 1);
        let _ = write!(&mut full_name_buf, "{} {} #{}", given, family, i + 1);
        let _ = write!(&mut file_as_buf, "{}, {} #{}", family, given, i + 1);
        let _ = write!(
            &mut vcard_buf,
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{}\r\nFN:{}\r\nN:{};{};;;\r\nEMAIL;TYPE=work:user{}@enterprise.example.com\r\nTEL;TYPE=work,voice:+49-89-555-{:06}\r\nORG:{}\r\nTITLE:Principal Engineer\r\nEND:VCARD\r\n",
            uid_buf,
            full_name_buf,
            family,
            given,
            i + 1,
            (i + 1) % 1_000_000,
            org
        );

        unsafe {
            sqlite3_reset(stmt);
            sqlite3_clear_bindings(stmt);
            sqlite3_bind_text(
                stmt,
                1,
                uid_buf.as_ptr() as *const c_char,
                uid_buf.len() as c_int,
                SQLITE_STATIC,
            );
            sqlite3_bind_text(
                stmt,
                2,
                vcard_buf.as_ptr() as *const c_char,
                vcard_buf.len() as c_int,
                SQLITE_STATIC,
            );
            sqlite3_bind_text(
                stmt,
                3,
                family.as_ptr() as *const c_char,
                family.len() as c_int,
                SQLITE_STATIC,
            );
            sqlite3_bind_text(
                stmt,
                4,
                given.as_ptr() as *const c_char,
                given.len() as c_int,
                SQLITE_STATIC,
            );
            sqlite3_bind_text(
                stmt,
                5,
                full_name_buf.as_ptr() as *const c_char,
                full_name_buf.len() as c_int,
                SQLITE_STATIC,
            );
            sqlite3_bind_text(
                stmt,
                6,
                file_as_buf.as_ptr() as *const c_char,
                file_as_buf.len() as c_int,
                SQLITE_STATIC,
            );

            if sqlite3_step(stmt) != SQLITE_DONE {
                sqlite3_finalize(stmt);
                return Err(format!("Failed to insert synthetic contact at index {i}"));
            }
        }
    }

    let commit_sql = c"COMMIT; \
                       CREATE INDEX IDX_full_name ON ECacheObjects(full_name); \
                       CREATE INDEX IDX_file_as ON ECacheObjects(file_as);";
    unsafe {
        sqlite3_finalize(stmt);
        let _ = sqlite3_exec(
            db.ptr,
            commit_sql.as_ptr(),
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    drop(db);

    let size_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    Ok((start_instant.elapsed().as_secs_f64(), size_bytes))
}

/// Load contacts directly from the local SQLite databases, with an optional limit.
/// Uses parallel scoped threads across multiple discovered address books when loading larger batches.
pub fn load_contacts_from_meridian_sqlite_cache_limited(limit: Option<usize>) -> Vec<EBookContactRecord> {
    let targets = discover_address_book_dbs();
    if targets.is_empty() {
        return Vec::new();
    }

    let mut records = Vec::new();
    let mut seen_contact_identities: HashSet<String> = HashSet::new();

    if targets.len() > 1 && limit.unwrap_or(usize::MAX) > 250 {
        let per_target_results: Vec<Vec<(String, EBookContactRecord)>> = std::thread::scope(|s| {
            let handles: Vec<_> = targets
                .iter()
                .map(|target| {
                    s.spawn(move || {
                        let Some(db) = SqliteDb::open(&target.db_path) else {
                            return Vec::new();
                        };
                        let raw_contacts = if target.is_cache_table {
                            db.query_contacts_cache(limit)
                        } else {
                            db.query_contacts_local(limit)
                        };
                        let parent_prefix = target.parent_name.as_deref().unwrap_or("");
                        let mut out = Vec::with_capacity(raw_contacts.len());
                        for (uid, vcard) in raw_contacts {
                            let Some(contact) = VCardContact::parse_vcard_string(&vcard) else {
                                continue;
                            };
                            let dedup_key = format!("{parent_prefix}:{uid}");
                            out.push((
                                dedup_key,
                                EBookContactRecord {
                                    source_uid: target.source_uid.clone(),
                                    source_display_name: target.display_name.clone(),
                                    uid,
                                    vcard,
                                    contact,
                                },
                            ));
                        }
                        out
                    })
                })
                .collect();

            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .collect()
        });

        for batch in per_target_results {
            for (dedup_key, record) in batch {
                if !seen_contact_identities.insert(dedup_key) {
                    continue;
                }
                records.push(record);
                if let Some(lim) = limit {
                    if records.len() >= lim {
                        return records;
                    }
                }
            }
        }
        return records;
    }

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

/// Look up contact's formatted full name by email address from local SQLite contact caches.
static CONTACT_NAME_MAP: std::sync::RwLock<Option<HashMap<String, String>>> = std::sync::RwLock::new(None);

pub fn lookup_contact_name_by_email(email: &str) -> Option<String> {
    let clean_email = email.trim().to_lowercase();
    if clean_email.is_empty() {
        return None;
    }

    if let Ok(guard) = CONTACT_NAME_MAP.read() {
        if let Some(ref map) = *guard {
            return map.get(&clean_email).cloned();
        }
    }

    let mut map = HashMap::new();
    let records = load_contacts_from_meridian_sqlite_cache();
    for rec in records {
        let name = rec.contact.fn_name.trim();
        if name.is_empty() || name.contains('@') {
            continue;
        }
        let primary_email = rec.contact.email.trim().to_lowercase();
        if !primary_email.is_empty() && !map.contains_key(&primary_email) {
            map.insert(primary_email, name.to_string());
        }
        for em in &rec.contact.emails {
            let e = em.trim().to_lowercase();
            if !e.is_empty() && !map.contains_key(&e) {
                map.insert(e, name.to_string());
            }
        }
    }

    let found = map.get(&clean_email).cloned();
    if let Ok(mut guard) = CONTACT_NAME_MAP.write() {
        *guard = Some(map);
    }
    found
}

/// Invalidate contact name cache when address book changes.
pub fn invalidate_contact_name_cache() {
    if let Ok(mut guard) = CONTACT_NAME_MAP.write() {
        *guard = None;
    }
}

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
