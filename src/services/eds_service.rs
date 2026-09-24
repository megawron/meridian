// SPDX-License-Identifier: GPL-3.0-or-later
//
// Platform source discovery — EDS/GOA on Linux, EventKit/Contacts on macOS.
//
// Linux uses the asynchronous EDS registry via zbus; macOS enumerates native
// system sources on a worker without D-Bus. Public async operations return Futures that
// can be spawned on the GLib main context via `glib::spawn_future_local()`.
//
// Architecture:
//   UI (main thread)
//   └── glib::spawn_future_local(async { ... })
//       └── zbus::Connection::session()  [async, non-blocking]
//           └── D-Bus call to org.gnome.evolution.dataserver.*
//               └── Result mapped back to Rust structs
//
// In a later phase, this will be complemented by direct libcamel
// bindings (via GIR-generated crates) for low-level mail operations
// that don't have adequate D-Bus coverage.

#[cfg(not(target_os = "macos"))]
use glib::MainContext;
use glib::{KeyFile, KeyFileFlags};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::eds_source_cache_service::{
    EdsSourceCache, EdsSourceCacheLoad, EdsSourceCacheRecovery, EdsSourceCacheStatus,
};

const EDS_RETRY_MIN_SECONDS: u64 = 2;
const EDS_RETRY_MAX_SECONDS: u64 = 300;

/// Represents a data source registered in EDS (mail account, calendar, etc.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdsSource {
    /// Unique identifier of the source
    pub uid: String,
    /// Human-readable display name
    pub display_name: String,
    /// Backend name (e.g., "imapx", "local", "ews", "google")
    pub backend_name: String,
    /// Whether this source is enabled
    pub enabled: bool,
    /// Parent account or group name (e.g. "Auf diesem Computer", "Kontakte", "user@example.com")
    #[serde(default)]
    pub parent_name: Option<String>,
    /// Color associated with this source (e.g. from Google Calendar / EDS [Calendar] Color)
    #[serde(default)]
    pub color: Option<String>,
    /// Associated foreign mailbox email (e.g. from EWS [Exchange Web Services Folder] ForeignMail)
    #[serde(default)]
    pub foreign_mail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdsSourceKind {
    Mail,
    Calendar,
    AddressBook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdsSourceOrigin {
    Live,
    Cache,
    Empty,
}

#[derive(Debug, Clone)]
pub struct EdsSourceDiscovery {
    pub sources: Vec<EdsSource>,
    pub origin: EdsSourceOrigin,
    pub error: Option<String>,
}

impl EdsSourceDiscovery {
    pub fn status_text(&self) -> String {
        match (self.origin, self.sources.len(), self.error.as_deref()) {
            (EdsSourceOrigin::Live, count, Some(error)) => {
                format!("{count} Kontoquelle(n); {error}")
            }
            (EdsSourceOrigin::Live, 0, None) => "Keine Kontoquelle gefunden".to_string(),
            (EdsSourceOrigin::Live, count, None) => format!("{count} Kontoquelle(n)"),
            (EdsSourceOrigin::Cache, count, Some(error)) => format!(
                "{count} zwischengespeicherte Kontoquelle(n); Live-Verbindung fehlgeschlagen: {error}"
            ),
            (EdsSourceOrigin::Cache, count, None) => {
                format!("{count} zwischengespeicherte Kontoquelle(n)")
            }
            (EdsSourceOrigin::Empty, _, Some(error)) => {
                format!("Kontoquellen nicht erreichbar: {error}")
            }
            (EdsSourceOrigin::Empty, _, None) => "Keine Kontoquelle verfügbar".to_string(),
        }
    }
}

impl EdsSourceKind {
    fn matches_interfaces(
        self,
        interfaces: &std::collections::HashMap<
            zbus::names::OwnedInterfaceName,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        >,
    ) -> bool {
        match self {
            Self::Mail => interfaces.keys().any(|interface| {
                let interface = interface.as_str();
                interface.contains("Mail")
                    || interface.contains("Goa")
                    || interface.contains("Camel")
                    || interface.contains("Exchange")
                    || interface.contains("Identity")
            }),
            Self::Calendar => interfaces.keys().any(|interface| {
                let interface = interface.as_str();
                interface.contains("Calendar")
                    || interface.contains("WebDAV")
                    || interface.contains("OAuth2")
                    || interface.contains("CalDAV")
            }),
            Self::AddressBook => interfaces.keys().any(|interface| {
                let interface = interface.as_str();
                interface.contains("AddressBook")
                    || interface.contains("Book")
                    || interface.contains("CardDAV")
            }),
        }
    }
}

/// Represents a mail folder summary from EDS
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MailFolderInfo {
    /// Folder display name
    pub display_name: String,
    /// Full folder path (e.g., "INBOX/Work")
    pub full_name: String,
    /// Number of unread messages
    pub unread_count: u32,
    /// Total message count
    pub total_count: u32,
    /// Child folders
    pub children: Vec<MailFolderInfo>,
}

/// Error type for EDS service operations.
#[derive(Debug)]
pub enum EdsServiceError {
    DBus(zbus::Error),
    DBusFdo(zbus::fdo::Error),
    #[allow(dead_code)]
    RegistryUnavailable,
    #[allow(dead_code)]
    OperationFailed(String),
}

impl std::fmt::Display for EdsServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdsServiceError::DBus(e) => write!(f, "D-Bus error: {}", e),
            EdsServiceError::DBusFdo(e) => write!(f, "D-Bus FDO error: {}", e),
            EdsServiceError::RegistryUnavailable => write!(f, "Kontoquellen sind nicht verfügbar"),
            EdsServiceError::OperationFailed(msg) => write!(f, "Operation failed: {}", msg),
        }
    }
}

impl std::error::Error for EdsServiceError {}

impl From<zbus::Error> for EdsServiceError {
    fn from(e: zbus::Error) -> Self {
        EdsServiceError::DBus(e)
    }
}

impl From<zbus::fdo::Error> for EdsServiceError {
    fn from(e: zbus::fdo::Error) -> Self {
        EdsServiceError::DBusFdo(e)
    }
}

/// Helper: extract a String from a zvariant OwnedValue.
fn value_to_string(val: &zbus::zvariant::OwnedValue) -> Option<String> {
    // OwnedValue derefs to Value; use the Value's TryFrom
    use std::ops::Deref;
    let value: &zbus::zvariant::Value<'_> = val.deref();
    String::try_from(value).ok()
}

/// Helper: extract a bool from a zvariant OwnedValue.
fn value_to_bool(val: &zbus::zvariant::OwnedValue) -> Option<bool> {
    use std::ops::Deref;
    let value: &zbus::zvariant::Value<'_> = val.deref();
    bool::try_from(value).ok()
}

/// Extract source properties from a D-Bus interface property map.
fn extract_source(
    path: &zbus::zvariant::OwnedObjectPath,
    source_props: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    interfaces: &std::collections::HashMap<
        zbus::names::OwnedInterfaceName,
        std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    >,
    kind: EdsSourceKind,
) -> Option<(String, String, String, bool, Option<String>, Option<String>, Option<String>)> {
    let uid = source_props
        .get("UID")
        .and_then(value_to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| path.as_str().rsplit('/').next().unwrap_or("").to_string());

    let property_display_name = source_props
        .get("DisplayName")
        .and_then(value_to_string)
        .unwrap_or_default();

    let property_backend_name = source_props
        .get("BackendName")
        .and_then(value_to_string)
        .unwrap_or_else(|| "unknown".to_string());

    let property_enabled = source_props
        .get("Enabled")
        .and_then(value_to_bool)
        .unwrap_or(true);

    let property_parent = source_props
        .get("Parent")
        .and_then(value_to_string)
        .filter(|s| !s.trim().is_empty());

    let property_color = source_props
        .get("Color")
        .and_then(value_to_string)
        .filter(|s| !s.trim().is_empty());

    let Some(data) = source_props.get("Data").and_then(value_to_string) else {
        return kind.matches_interfaces(interfaces).then_some((
            uid,
            property_display_name,
            property_backend_name,
            property_enabled,
            property_parent,
            property_color,
            None,
        ));
    };

    parse_source_data(
        &data,
        kind,
        property_display_name,
        property_backend_name,
        property_enabled,
        property_parent,
    )
    .map(|(display_name, backend_name, enabled, parent, color, foreign_mail)| {
        (uid, display_name, backend_name, enabled, parent, color.or(property_color), foreign_mail)
    })
}

fn parse_source_data(
    data: &str,
    kind: EdsSourceKind,
    fallback_display_name: String,
    fallback_backend_name: String,
    fallback_enabled: bool,
    fallback_parent: Option<String>,
) -> Option<(String, String, bool, Option<String>, Option<String>, Option<String>)> {
    let key_file = KeyFile::new();
    key_file.load_from_data(data, KeyFileFlags::NONE).ok()?;

    let section = match kind {
        EdsSourceKind::Mail => "Mail Account",
        EdsSourceKind::Calendar => "Calendar",
        EdsSourceKind::AddressBook => "Address Book",
    };
    if !key_file.has_group(section) {
        return None;
    }

    let display_name = key_file
        .string("Data Source", "DisplayName")
        .ok()
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback_display_name);
    let backend_name = key_file
        .string(section, "BackendName")
        .ok()
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback_backend_name);
    let enabled = key_file
        .boolean("Data Source", "Enabled")
        .unwrap_or(fallback_enabled);
    let parent = key_file
        .string("Data Source", "Parent")
        .ok()
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty())
        .or(fallback_parent);

    let color = key_file
        .string(section, "Color")
        .ok()
        .map(|value| value.to_string())
        .or_else(|| key_file.string("Calendar", "Color").ok().map(|v| v.to_string()))
        .or_else(|| key_file.string("Task List", "Color").ok().map(|v| v.to_string()))
        .or_else(|| key_file.string("WebDAV Backend", "Color").ok().map(|v| v.to_string()))
        .filter(|value| !value.trim().is_empty());

    let foreign_mail = key_file
        .string("Exchange Web Services Folder", "ForeignMail")
        .ok()
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty());

    Some((display_name, backend_name, enabled, parent, color, foreign_mail))
}

fn sort_sources(sources: &mut [EdsSource]) {
    sources.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.uid.cmp(&right.uid))
    });
}

/// Connect to EDS and get the managed objects from the SourceManager.
async fn get_eds_managed_objects() -> Result<
    std::collections::HashMap<
        zbus::zvariant::OwnedObjectPath,
        std::collections::HashMap<
            zbus::names::OwnedInterfaceName,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        >,
    >,
    EdsServiceError,
> {
    let connection = zbus::Connection::session().await?;

    let proxy = zbus::fdo::ObjectManagerProxy::builder(&connection)
        .destination("org.gnome.evolution.dataserver.Sources5")?
        .path("/org/gnome/evolution/dataserver/SourceManager")?
        .build()
        .await?;

    let mut last_err = None;
    for attempt in 0..4 {
        if attempt > 0 {
            glib::timeout_future(Duration::from_millis(250)).await;
        }
        match proxy.get_managed_objects().await {
            Ok(objs) => return Ok(objs),
            Err(err) => {
                tracing::debug!(%err, attempt, "EDS SourceManager query failed, retrying...");
                last_err = Some(err);
            }
        }
    }
    Err(EdsServiceError::DBusFdo(last_err.unwrap()))
}

/// List all mail sources registered in the Evolution Data Server.
///
/// This function connects to the EDS Source Registry via D-Bus and
/// enumerates all sources that have a "MailAccount" extension.
///
/// # Example (called from UI layer):
/// ```rust,ignore
/// glib::spawn_future_local(async move {
///     match eds_service::list_mail_sources().await {
///         Ok(sources) => {
///             // Update GListModel with source data
///             for source in sources {
///                 model.append(&SourceObject::new(&source));
///             }
///         }
///         Err(e) => tracing::warn!("EDS: {}", e),
///     }
/// });
/// ```
#[cfg(not(target_os = "macos"))]
pub async fn list_mail_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    tracing::debug!("Connecting to EDS Source Registry & GNOME Online Accounts via D-Bus...");

    let (mut sources, registry_error) = match list_sources(EdsSourceKind::Mail).await {
        Ok(sources) => (sources, None),
        Err(error) => {
            tracing::warn!(%error, "EDS source registry is unavailable, falling back to GNOME Online Accounts");
            (Vec::new(), Some(error))
        }
    };

    // Query GNOME Online Accounts (GOA) via D-Bus as an additional source.
    // GOA is the account creator, but the EDS source (and its Camel
    // extension) is the authoritative mail source.  Only show the direct
    // GOA fallback when the source registry itself is unavailable; otherwise
    // a GOA-only row would be visible but could not load mail through EDS.
    if sources.is_empty() && registry_error.is_some() {
        if let Ok(goa_accounts) = get_goa_mail_accounts().await {
            for goa in goa_accounts {
                if !sources
                    .iter()
                    .any(|s| s.uid == goa.uid || s.display_name == goa.display_name)
                {
                    sources.push(goa);
                }
            }
        }
    }

    sort_sources(&mut sources);

    if sources.is_empty() {
        if let Some(error) = registry_error {
            return Err(error);
        }
        tracing::info!("No configured mail sources found in EDS or GOA");
    } else if registry_error.is_some() {
        tracing::info!(
            "Found {} mail sources via D-Bus (GOA fallback only, EDS was unavailable)",
            sources.len()
        );
    } else {
        tracing::info!("Found {} mail sources via D-Bus (EDS)", sources.len());
    }
    Ok(sources)
}

fn resolve_source_name_from_disk(uid: &str) -> Option<String> {
    for profile in crate::services::account_config_service::AccountConfigStore::default().load() {
        if profile.id == uid {
            let name = if !profile.display_name.trim().is_empty() {
                profile.display_name
            } else {
                profile.email
            };
            return Some(name);
        }
    }

    let mut candidate_paths = Vec::new();
    candidate_paths.push(
        glib::user_config_dir()
            .join("evolution/sources")
            .join(format!("{uid}.source")),
    );
    candidate_paths.push(
        std::path::PathBuf::from("/usr/share/evolution-data-server/sources")
            .join(format!("{uid}.source")),
    );

    for path in candidate_paths {
        if path.exists() {
            let kf = KeyFile::new();
            if kf.load_from_file(&path, KeyFileFlags::NONE).is_ok() {
                if let Ok(dn) = kf.locale_string("Data Source", "DisplayName", None) {
                    if !dn.trim().is_empty() {
                        return Some(dn.to_string());
                    }
                }
                if let Ok(dn) = kf.string("Data Source", "DisplayName") {
                    if !dn.trim().is_empty() {
                        return Some(dn.to_string());
                    }
                }
                if let Ok(identity) = kf.string("Collection", "Identity") {
                    if !identity.trim().is_empty() {
                        return Some(identity.to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn resolve_source_color_from_disk(uid: &str) -> Option<String> {
    let mut candidate_paths = Vec::new();
    candidate_paths.push(
        glib::user_config_dir()
            .join("evolution/sources")
            .join(format!("{uid}.source")),
    );
    candidate_paths.push(
        std::path::PathBuf::from("/usr/share/evolution-data-server/sources")
            .join(format!("{uid}.source")),
    );

    for path in candidate_paths {
        if path.exists() {
            let kf = KeyFile::new();
            if kf.load_from_file(&path, KeyFileFlags::NONE).is_ok() {
                if let Ok(color) = kf.string("Calendar", "Color") {
                    if !color.trim().is_empty() {
                        return Some(color.to_string());
                    }
                }
                if let Ok(color) = kf.string("Task List", "Color") {
                    if !color.trim().is_empty() {
                        return Some(color.to_string());
                    }
                }
                if let Ok(color) = kf.string("WebDAV Backend", "Color") {
                    if !color.trim().is_empty() {
                        return Some(color.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Configured account collection (e.g. Google, EWS, IMAP/SMTP) that may contain
/// calendar, task, memo, and mail resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdsAccountCollection {
    pub uid: String,
    pub display_name: String,
    pub backend_name: String,
    pub enabled: bool,
    pub calendar_enabled: bool,
    pub contacts_enabled: bool,
    pub mail_enabled: bool,
}

static COLLECTIONS_CACHE: RwLock<Option<(std::time::Instant, Vec<EdsAccountCollection>)>> =
    RwLock::new(None);
static PARENT_NAME_CACHE: RwLock<Option<(std::time::Instant, std::collections::HashMap<String, String>)>> =
    RwLock::new(None);

/// Invalidate cached collections and parent names so newly created or updated accounts are immediately visible.
pub fn invalidate_account_collections_cache() {
    if let Ok(mut guard) = COLLECTIONS_CACHE.write() {
        *guard = None;
    }
    if let Ok(mut guard) = PARENT_NAME_CACHE.write() {
        *guard = None;
    }
}

/// Load all enabled account collections configured in Evolution Data Server,
/// GNOME Online Accounts, or Evolution account configuration.
pub fn load_configured_account_collections() -> Vec<EdsAccountCollection> {
    if let Ok(guard) = COLLECTIONS_CACHE.read() {
        if let Some((instant, collections)) = &*guard {
            if instant.elapsed() < std::time::Duration::from_secs(4) {
                return collections.clone();
            }
        }
    }
    let collections = load_configured_account_collections_uncached();
    if let Ok(mut guard) = COLLECTIONS_CACHE.write() {
        *guard = Some((std::time::Instant::now(), collections.clone()));
    }
    collections
}

fn load_configured_account_collections_uncached() -> Vec<EdsAccountCollection> {
    let mut collections = Vec::new();
    let dirs = [
        glib::user_config_dir().join("evolution/sources"),
        glib::home_dir().join(".var/app/org.gnome.Evolution/config/evolution/sources"),
    ];
    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("source") {
                    continue;
                }
                let uid = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                if collections.iter().any(|c: &EdsAccountCollection| c.uid == uid) {
                    continue;
                }
                let kf = KeyFile::new();
                if kf.load_from_file(&path, KeyFileFlags::NONE).is_err() {
                    continue;
                }
                if !kf.has_group("Collection") {
                    continue;
                }
                let enabled = kf.boolean("Data Source", "Enabled").unwrap_or(true);
                if !enabled {
                    continue;
                }
                let display_name = kf
                    .locale_string("Data Source", "DisplayName", None)
                    .or_else(|_| kf.string("Data Source", "DisplayName"))
                    .or_else(|_| kf.string("Collection", "Identity"))
                    .ok()
                    .map(|s| s.to_string())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| uid.clone());
                let backend_name = kf
                    .string("Collection", "BackendName")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| "collection".to_string());
                let calendar_enabled = kf.boolean("Collection", "CalendarEnabled").unwrap_or(true);
                let contacts_enabled = kf.boolean("Collection", "ContactsEnabled").unwrap_or(true);
                let mail_enabled = kf.boolean("Collection", "MailEnabled").unwrap_or(true);

                collections.push(EdsAccountCollection {
                    uid,
                    display_name,
                    backend_name,
                    enabled,
                    calendar_enabled,
                    contacts_enabled,
                    mail_enabled,
                });
            }
        }
    }

    // Also include enabled accounts from AccountConfigStore if not already present
    for profile in crate::services::account_config_service::AccountConfigStore::default().load() {
        if !profile.enabled || profile.is_shared_mailbox || profile.open_mailbox_other_user {
            continue;
        }
        let name = if !profile.display_name.trim().is_empty() {
            profile.display_name.clone()
        } else {
            profile.email.clone()
        };
        if !collections.iter().any(|c| c.uid == profile.id || c.display_name.eq_ignore_ascii_case(&name) || c.display_name.eq_ignore_ascii_case(&profile.email)) {
            let contacts_enabled = profile.protocol == crate::services::account_config_service::AccountProtocol::Exchange;
            collections.push(EdsAccountCollection {
                uid: profile.id,
                display_name: name,
                backend_name: "account".to_string(),
                enabled: true,
                calendar_enabled: true,
                contacts_enabled,
                mail_enabled: true,
            });
        }
    }

    collections.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.uid.cmp(&right.uid))
    });
    collections
}

/// Resolve the parent account or group name for a source UID.
pub fn resolve_source_parent_name(uid: &str) -> String {
    if let Ok(guard) = PARENT_NAME_CACHE.read() {
        if let Some((instant, map)) = &*guard {
            if instant.elapsed() < std::time::Duration::from_secs(4) {
                if let Some(name) = map.get(uid) {
                    return name.clone();
                }
            }
        }
    }
    let name = resolve_source_parent_name_uncached(uid);
    if let Ok(mut guard) = PARENT_NAME_CACHE.write() {
        if let Some((instant, map)) = guard.as_mut() {
            if instant.elapsed() < std::time::Duration::from_secs(4) {
                map.insert(uid.to_string(), name.clone());
            } else {
                let mut new_map = std::collections::HashMap::new();
                new_map.insert(uid.to_string(), name.clone());
                *guard = Some((std::time::Instant::now(), new_map));
            }
        } else {
            let mut new_map = std::collections::HashMap::new();
            new_map.insert(uid.to_string(), name.clone());
            *guard = Some((std::time::Instant::now(), new_map));
        }
    }
    name
}

fn resolve_source_parent_name_uncached(uid: &str) -> String {
    let clean_uid = uid
        .strip_prefix("account-contacts-")
        .or_else(|| uid.strip_prefix("account-tasks-"))
        .or_else(|| uid.strip_prefix("account-calendar-"))
        .or_else(|| uid.strip_prefix("account-memos-"))
        .or_else(|| uid.strip_prefix("col-tasks-"))
        .or_else(|| uid.strip_prefix("col-memos-"))
        .or_else(|| uid.strip_prefix("col-calendar-"))
        .unwrap_or(uid);
    if clean_uid.starts_with("local-") || clean_uid == "local" || clean_uid == "system-calendar" {
        return "Auf diesem Computer".to_string();
    }
    // Check if clean_uid matches an account collection, immediately return its display name
    for col in load_configured_account_collections() {
        if col.uid == clean_uid {
            return col.display_name;
        }
    }

    // Check if known in cached address book sources (e.g. Google or Exchange GAL)
    let cached_sources = crate::services::evolution_contact_cache::list_cached_address_book_sources();
    if let Some(cached) = cached_sources.iter().find(|s| s.uid == clean_uid) {
        if let Some(parent) = &cached.parent_name {
            return parent.clone();
        }
    }

    let mut candidate_paths = Vec::new();
    candidate_paths.push(glib::user_config_dir().join("evolution/sources").join(format!("{clean_uid}.source")));
    candidate_paths.push(glib::home_dir().join(".var/app/org.gnome.Evolution/config/evolution/sources").join(format!("{clean_uid}.source")));
    candidate_paths.push(std::path::PathBuf::from("/usr/share/evolution-data-server/sources").join(format!("{clean_uid}.source")));

    for path in candidate_paths {
        if path.exists() {
            let kf = KeyFile::new();
            if kf.load_from_file(&path, KeyFileFlags::NONE).is_ok() {
                if let Ok(foreign_mail) = kf.string("Exchange Web Services Folder", "ForeignMail") {
                    let fm = foreign_mail.trim();
                    if !fm.is_empty() {
                        let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
                        if let Some(matching) = profiles.iter().find(|p| {
                            p.email.eq_ignore_ascii_case(fm) || p.foreign_mailbox.eq_ignore_ascii_case(fm)
                        }) {
                            return if !matching.display_name.trim().is_empty() {
                                matching.display_name.clone()
                            } else {
                                matching.email.clone()
                            };
                        }
                        return fm.to_string();
                    }
                }
                if let Ok(parent) = kf.string("Data Source", "Parent") {
                    let parent = parent.trim();
                    if !parent.is_empty() {
                        if parent == "local-stub" || parent == "local" {
                            return crate::i18n::gettext("On This Computer");
                        } else if parent == "contacts-stub" {
                            return crate::i18n::gettext("Contacts");
                        } else if let Some(dn) = resolve_source_name_from_disk(parent) {
                            return dn;
                        } else {
                            for col in load_configured_account_collections() {
                                if col.uid == parent {
                                    return col.display_name;
                                }
                            }
                        }
                    }
                }
                if let Ok(dn) = kf.locale_string("Data Source", "DisplayName", None) {
                    if !dn.trim().is_empty() {
                        return dn.to_string();
                    }
                }
                if let Ok(dn) = kf.string("Data Source", "DisplayName") {
                    if !dn.trim().is_empty() {
                        return dn.to_string();
                    }
                }
                if let Ok(identity) = kf.string("Collection", "Identity") {
                    if !identity.trim().is_empty() {
                        return identity.to_string();
                    }
                }
            }
        }
    }
    if let Some(dn) = resolve_source_name_from_disk(clean_uid) {
        return dn;
    }
    "Auf diesem Computer".to_string()
}

/// List EDS sources for one product area through the shared SourceManager
/// object model.  Account mutation and ECal/EBook client streams intentionally
/// remain separate adapter layers.
#[cfg(not(target_os = "macos"))]
pub async fn list_sources(kind: EdsSourceKind) -> Result<Vec<EdsSource>, EdsServiceError> {
    let managed_objects = get_eds_managed_objects().await?;
    let mut display_name_by_uid: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (path, interfaces) in &managed_objects {
        if let Some(source_props) = interfaces.get("org.gnome.evolution.dataserver.Source") {
            let real_uid = source_props
                .get("UID")
                .and_then(value_to_string)
                .filter(|s| !s.trim().is_empty());
            let path_leaf = path
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();

            let mut name = source_props
                .get("DisplayName")
                .and_then(value_to_string)
                .unwrap_or_default();
            if name.is_empty() {
                if let Some(data) = source_props.get("Data").and_then(value_to_string) {
                    let kf = KeyFile::new();
                    if kf.load_from_data(&data, KeyFileFlags::NONE).is_ok() {
                        if let Ok(dn) = kf.locale_string("Data Source", "DisplayName", None) {
                            name = dn.to_string();
                        } else if let Ok(dn) = kf.string("Data Source", "DisplayName") {
                            name = dn.to_string();
                        }
                    }
                }
            }
            if !name.is_empty() {
                if let Some(uid) = real_uid {
                    display_name_by_uid.insert(uid, name.clone());
                }
                if !path_leaf.is_empty() {
                    display_name_by_uid.insert(path_leaf, name);
                }
            }
        }
    }

    let mut sources = Vec::new();

    for (path, interfaces) in &managed_objects {
        let Some(source_props) = interfaces.get("org.gnome.evolution.dataserver.Source") else {
            continue;
        };

        let Some((uid, display_name, backend_name, enabled, parent, color, foreign_mail)) =
            extract_source(path, source_props, interfaces, kind)
        else {
            continue;
        };
        if !display_name.trim().is_empty() {
            let parent_name = if let Some(fm) = foreign_mail.as_deref() {
                let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
                if let Some(matching) = profiles.iter().find(|p| {
                    p.email.eq_ignore_ascii_case(fm) || p.foreign_mailbox.eq_ignore_ascii_case(fm)
                }) {
                    Some(if !matching.display_name.trim().is_empty() {
                        matching.display_name.clone()
                    } else {
                        matching.email.clone()
                    })
                } else {
                    Some(fm.to_string())
                }
            } else {
                parent.and_then(|p| {
                    if p == "local-stub" {
                        Some(crate::i18n::gettext("On This Computer"))
                    } else if p == "contacts-stub" {
                        Some(crate::i18n::gettext("Contacts"))
                    } else if let Some(dn) = display_name_by_uid.get(&p) {
                        Some(dn.clone())
                    } else if let Some(dn) = resolve_source_name_from_disk(&p) {
                        Some(dn)
                    } else {
                        Some(p)
                    }
                })
            };
            let resolved_color = color.or_else(|| resolve_source_color_from_disk(&uid));
            sources.push(EdsSource {
                uid,
                display_name,
                backend_name,
                enabled,
                parent_name,
                color: resolved_color,
                foreign_mail,
            });
        }
    }

    sort_sources(&mut sources);
    Ok(sources)
}

/// Query GNOME Online Accounts (GOA) D-Bus interface `org.gnome.OnlineAccounts`.
async fn get_goa_mail_accounts() -> Result<Vec<EdsSource>, EdsServiceError> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::fdo::ObjectManagerProxy::builder(&connection)
        .destination("org.gnome.OnlineAccounts")?
        .path("/org/gnome/OnlineAccounts")?
        .build()
        .await?;

    let managed_objects = match proxy.get_managed_objects().await {
        Ok(objs) => objs,
        Err(_) => return Ok(Vec::new()),
    };

    let mut goa_sources = Vec::new();

    for (path, interfaces) in &managed_objects {
        let has_mail_interface = interfaces
            .keys()
            .any(|interface| interface.as_str() == "org.gnome.OnlineAccounts.Mail");
        if has_mail_interface {
            if let Some(account_props) = interfaces.get("org.gnome.OnlineAccounts.Account") {
                let uid = path
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or("goa")
                    .to_string();
                let identity = account_props
                    .get("PresentationIdentity")
                    .and_then(value_to_string)
                    .or_else(|| account_props.get("Identity").and_then(value_to_string))
                    .or_else(|| account_props.get("Mail").and_then(value_to_string))
                    .unwrap_or_else(|| "Domain Account".to_string());

                let provider_type = account_props
                    .get("ProviderType")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "domain".to_string());

                let provider_name = account_props
                    .get("ProviderName")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| provider_type.clone());

                goa_sources.push(EdsSource {
                    uid: format!("goa-{}", uid),
                    display_name: format!("{} ({})", identity, provider_name),
                    backend_name: provider_type,
                    enabled: true,
                    parent_name: Some(identity),
                    color: None,
                    foreign_mail: None,
                });
            }
        }
    }
    Ok(goa_sources)
}

/// Query GNOME Online Accounts (GOA) D-Bus interface `org.gnome.OnlineAccounts` for Calendar.
async fn get_goa_calendar_accounts() -> Result<Vec<EdsSource>, EdsServiceError> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::fdo::ObjectManagerProxy::builder(&connection)
        .destination("org.gnome.OnlineAccounts")?
        .path("/org/gnome/OnlineAccounts")?
        .build()
        .await?;

    let managed_objects = match proxy.get_managed_objects().await {
        Ok(objs) => objs,
        Err(_) => return Ok(Vec::new()),
    };

    let mut goa_sources = Vec::new();

    for (path, interfaces) in &managed_objects {
        let has_cal_interface = interfaces
            .keys()
            .any(|interface| interface.as_str() == "org.gnome.OnlineAccounts.Calendar");
        if has_cal_interface {
            if let Some(account_props) = interfaces.get("org.gnome.OnlineAccounts.Account") {
                let uid = path
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or("goa")
                    .to_string();
                let identity = account_props
                    .get("PresentationIdentity")
                    .and_then(value_to_string)
                    .or_else(|| account_props.get("Identity").and_then(value_to_string))
                    .unwrap_or_else(|| "Calendar Account".to_string());

                let provider_name = account_props
                    .get("ProviderName")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "Online Calendar".to_string());

                let provider_type = account_props
                    .get("ProviderType")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "caldav".to_string());

                goa_sources.push(EdsSource {
                    uid: format!("goa-cal-{}", uid),
                    display_name: format!("{} ({})", identity, provider_name),
                    backend_name: provider_type,
                    enabled: true,
                    parent_name: Some(identity),
                    color: None,
                    foreign_mail: None,
                });
            }
        }
    }
    Ok(goa_sources)
}

/// List all calendar sources from EDS (with GOA fallback).
#[allow(dead_code)]
#[cfg(not(target_os = "macos"))]
pub async fn list_calendar_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    tracing::debug!("Listing calendar sources from EDS & GOA...");
    let (mut sources, registry_error) = match list_sources(EdsSourceKind::Calendar).await {
        Ok(sources) => (sources, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    if sources.is_empty() {
        if let Ok(goa_calendars) = get_goa_calendar_accounts().await {
            for goa in goa_calendars {
                if !sources
                    .iter()
                    .any(|s| s.uid == goa.uid || s.display_name == goa.display_name)
                {
                    sources.push(goa);
                }
            }
        }
    }

    sort_sources(&mut sources);
    if sources.is_empty() {
        if let Some(error) = registry_error {
            return Err(error);
        }
    }
    tracing::info!("Found {} calendar sources via D-Bus", sources.len());
    Ok(sources)
}

/// Query GNOME Online Accounts (GOA) D-Bus interface `org.gnome.OnlineAccounts` for Contacts.
async fn get_goa_contacts_accounts() -> Result<Vec<EdsSource>, EdsServiceError> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::fdo::ObjectManagerProxy::builder(&connection)
        .destination("org.gnome.OnlineAccounts")?
        .path("/org/gnome/OnlineAccounts")?
        .build()
        .await?;

    let managed_objects = match proxy.get_managed_objects().await {
        Ok(objs) => objs,
        Err(_) => return Ok(Vec::new()),
    };

    let mut goa_sources = Vec::new();

    for (path, interfaces) in &managed_objects {
        let has_contacts_interface = interfaces
            .keys()
            .any(|interface| interface.as_str() == "org.gnome.OnlineAccounts.Contacts");
        if has_contacts_interface {
            if let Some(account_props) = interfaces.get("org.gnome.OnlineAccounts.Account") {
                let uid = path
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or("goa")
                    .to_string();
                let identity = account_props
                    .get("PresentationIdentity")
                    .and_then(value_to_string)
                    .or_else(|| account_props.get("Identity").and_then(value_to_string))
                    .unwrap_or_else(|| "Online Account".to_string());

                let provider_name = account_props
                    .get("ProviderName")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "Online Address Book".to_string());

                let provider_type = account_props
                    .get("ProviderType")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "carddav".to_string());

                goa_sources.push(EdsSource {
                    uid: format!("goa-contacts-{}", uid),
                    display_name: format!("{} ({})", identity, provider_name),
                    backend_name: provider_type,
                    enabled: true,
                    parent_name: Some(identity),
                    color: None,
                    foreign_mail: None,
                });
            }
        }
    }
    Ok(goa_sources)
}

/// Helper to identify shared, delegate, or functional mailboxes that do not possess
/// their own personal address books.
pub fn is_shared_or_delegate_mailbox(email_or_name: &str) -> bool {
    let lower = email_or_name.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    // Check known shared mailboxes from AccountConfigStore
    let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
    for p in &profiles {
        if p.is_shared_mailbox || p.open_mailbox_other_user || !p.foreign_mailbox.trim().is_empty() {
            if p.email.to_lowercase() == lower
                || p.foreign_mailbox.to_lowercase() == lower
                || p.display_name.to_lowercase() == lower
                || format!("account:{}", p.email).to_lowercase() == lower
                || format!("account:{}", p.display_name).to_lowercase() == lower
            {
                return true;
            }
        }
    }
    // Check known delegate mailbox names and functional addresses
    if lower == "edv" || lower.starts_with("edv@")
        || lower == "datenimport" || lower.starts_with("datenimport@")
        || lower == "dev" || lower.starts_with("dev@")
    {
        return true;
    }
    false
}

/// Normalize address book names to identify duplicates across English/German localizations.
pub fn canonical_book_name(name: &str) -> &'static str {
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case("contacts") || trimmed.eq_ignore_ascii_case("kontakte") {
        "contacts"
    } else if trimmed.eq_ignore_ascii_case("global address list")
        || trimmed.eq_ignore_ascii_case("globales adressbuch")
        || trimmed.eq_ignore_ascii_case("gal")
    {
        "gal"
    } else {
        ""
    }
}

/// List all address book sources from EDS (including configured accounts & GOA).
#[allow(dead_code)]
#[cfg(not(target_os = "macos"))]
pub async fn list_address_book_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    tracing::debug!("Listing address book sources from EDS, configured accounts & GOA...");
    let (mut sources, registry_error) = match list_sources(EdsSourceKind::AddressBook).await {
        Ok(sources) => (sources, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    // Filter out shared/delegate mailboxes which do not have address books
    sources.retain(|s| {
        let parent = s.parent_name.as_deref().unwrap_or("");
        let fm = s.foreign_mail.as_deref().unwrap_or("");
        !is_shared_or_delegate_mailbox(parent)
            && !is_shared_or_delegate_mailbox(fm)
            && !is_shared_or_delegate_mailbox(&s.uid)
            && !is_shared_or_delegate_mailbox(&s.display_name)
    });

    // Canonicalize parent names for discovered sources
    for s in &mut sources {
        if s.parent_name.is_none() || s.parent_name.as_deref().unwrap_or("").starts_with("account-contacts-") {
            let resolved = resolve_source_parent_name(&s.uid);
            if !resolved.is_empty() {
                s.parent_name = Some(resolved);
            }
        }
    }

    if let Ok(goa_contacts) = get_goa_contacts_accounts().await {
        for goa in goa_contacts {
            let parent = goa.parent_name.as_deref().unwrap_or("");
            if is_shared_or_delegate_mailbox(parent) || is_shared_or_delegate_mailbox(&goa.display_name) {
                continue;
            }
            if !sources
                .iter()
                .any(|s| s.uid == goa.uid || s.display_name == goa.display_name)
            {
                sources.push(goa);
            }
        }
    }

    // Add all cached sources discovered from Evolution SQLite caches
    let cached_sources = crate::services::evolution_contact_cache::list_cached_address_book_sources();
    for cached in cached_sources {
        let parent = cached.parent_name.as_deref().unwrap_or("");
        if is_shared_or_delegate_mailbox(parent) || is_shared_or_delegate_mailbox(&cached.display_name) {
            continue;
        }
        let c_kind = canonical_book_name(&cached.display_name);
        let is_dup = sources.iter().any(|s| {
            s.uid == cached.uid
                || (s.parent_name.as_deref().unwrap_or("").eq_ignore_ascii_case(parent)
                    && (!c_kind.is_empty() && c_kind == canonical_book_name(&s.display_name)
                        || s.display_name.eq_ignore_ascii_case(&cached.display_name)))
        });
        if !is_dup {
            sources.push(cached);
        }
    }

    for collection in load_configured_account_collections() {
        if !collection.contacts_enabled || is_shared_or_delegate_mailbox(&collection.display_name) || is_shared_or_delegate_mailbox(&collection.uid) {
            continue;
        }
        let account_name = collection.display_name.clone();
        let already_represented = sources.iter().any(|s| {
            s.parent_name.as_deref().unwrap_or("").eq_ignore_ascii_case(&account_name)
                || s.uid == collection.uid
                || s.uid == format!("account-contacts-{}", collection.uid)
        });
        if !already_represented {
            sources.push(EdsSource {
                uid: format!("account-contacts-{}", collection.uid),
                display_name: crate::i18n::gettext("Contacts"),
                backend_name: collection.backend_name.clone(),
                enabled: collection.enabled,
                parent_name: Some(account_name),
                color: None,
                foreign_mail: None,
            });
        }
    }

    // Omit GOA redundant wrappers if authoritative books exist for that parent
    let non_goa_parents: std::collections::HashSet<String> = sources
        .iter()
        .filter(|s| !s.uid.starts_with("goa-contacts-"))
        .filter_map(|s| s.parent_name.as_ref().map(|p| p.to_lowercase()))
        .collect();
    sources.retain(|s| {
        if s.uid.starts_with("goa-contacts-") {
            let parent = s.parent_name.as_deref().unwrap_or("").to_lowercase();
            !non_goa_parents.contains(&parent)
        } else {
            true
        }
    });

    // Deduplicate any duplicate GAL / Kontakte under the same parent account
    let mut deduped: Vec<EdsSource> = Vec::new();
    for s in sources {
        let p_name = s.parent_name.as_deref().unwrap_or("").to_lowercase();
        let c_kind = canonical_book_name(&s.display_name);
        let is_dup = deduped.iter().any(|existing| {
            existing.uid == s.uid
                || (existing.parent_name.as_deref().unwrap_or("").to_lowercase() == p_name
                    && (!c_kind.is_empty() && c_kind == canonical_book_name(&existing.display_name)
                        || existing.display_name.eq_ignore_ascii_case(&s.display_name)))
        });
        if !is_dup {
            deduped.push(s);
        }
    }
    let mut sources = deduped;

    sort_sources(&mut sources);
    if sources.is_empty() {
        if let Some(error) = registry_error {
            return Err(error);
        }
    }
    tracing::info!("Found {} address book sources via D-Bus and config", sources.len());
    Ok(sources)
}

/// Discover EDS sources and retain only non-secret source metadata for an
/// offline UI state.  A cached result is explicitly marked as such and is
/// never treated as authoritative for mutations or message/calendar data.
#[cfg(not(target_os = "macos"))]
pub async fn discover_sources(kind: EdsSourceKind) -> EdsSourceDiscovery {
    let cache = EdsSourceCache::new();
    let cached = cache.load_with_status(kind);
    let now = current_unix_seconds();
    if let Some(remaining) = recovery_retry_remaining(&cached.recovery, now) {
        return discovery_from_cache(
            cached,
            format!("Wiederverbindung in {remaining} Sekunde(n)"),
        );
    }

    let result = match kind {
        EdsSourceKind::Mail => list_mail_sources().await,
        EdsSourceKind::Calendar => list_calendar_sources().await,
        EdsSourceKind::AddressBook => list_address_book_sources().await,
    };

    match result {
        Ok(sources) => {
            if let Err(error) = cache.save(kind, &sources) {
                tracing::warn!(%error, "Could not persist EDS source cache");
            }
            let recovery = recovery_after_success(&cached.recovery, now);
            if let Err(error) = cache.save_recovery(kind, &recovery) {
                tracing::warn!(%error, "Could not persist successful EDS recovery state");
            }
            EdsSourceDiscovery {
                sources,
                origin: EdsSourceOrigin::Live,
                error: None,
            }
        }
        Err(error) => {
            let error = error.to_string();
            let recovery = recovery_after_failure(&cached.recovery, now, &error);
            if let Err(cache_error) = cache.save_recovery(kind, &recovery) {
                tracing::warn!(%cache_error, "Could not persist failed EDS recovery state");
            }
            discovery_from_cache(cached, error)
        }
    }
}

/// macOS source enumeration uses EventKit/Contacts and the app's own mail
/// profiles. It intentionally does not read the Linux EDS/GOA cache: an old
/// source row is not evidence of macOS permission or a connected system source.
#[cfg(target_os = "macos")]
pub async fn discover_sources(kind: EdsSourceKind) -> EdsSourceDiscovery {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let discovery = discover_macos_sources_blocking(kind);
        let _ = sender.send_blocking(discovery);
    });
    receiver
        .recv()
        .await
        .unwrap_or_else(|_| EdsSourceDiscovery {
            sources: Vec::new(),
            origin: EdsSourceOrigin::Empty,
            error: Some("Die macOS-Quellenerkennung wurde abgebrochen".to_string()),
        })
}

#[cfg(target_os = "macos")]
pub fn discover_sources_blocking(kind: EdsSourceKind) -> EdsSourceDiscovery {
    discover_macos_sources_blocking(kind)
}

#[cfg(target_os = "macos")]
fn discover_macos_sources_blocking(kind: EdsSourceKind) -> EdsSourceDiscovery {
    if kind == EdsSourceKind::Mail {
        let profiles = super::account_config_service::AccountConfigStore::default().load();
        return macos_configured_mail_sources(&profiles);
    }
    macos_system_source_discovery(
        kind,
        super::macos_integration_service::MacosIntegrationService::new().system_sources(),
    )
}

#[cfg(target_os = "macos")]
fn macos_system_source_discovery(
    kind: EdsSourceKind,
    snapshot: Result<super::macos_integration_service::MacosSystemSnapshot, String>,
) -> EdsSourceDiscovery {
    use super::macos_integration_service::MacosSourceKind;
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return EdsSourceDiscovery {
                sources: Vec::new(),
                origin: EdsSourceOrigin::Empty,
                error: Some(error),
            };
        }
    };
    let mut sources: Vec<_> = snapshot
        .sources
        .iter()
        .filter(|source| {
            matches!(
                (kind, source.kind),
                (EdsSourceKind::Calendar, MacosSourceKind::Calendar)
                    | (EdsSourceKind::AddressBook, MacosSourceKind::Contacts)
            )
        })
        .map(|source| source.as_eds_source())
        .collect();
    sort_sources(&mut sources);
    let error = (!snapshot.errors.is_empty()).then(|| snapshot.errors.join("; "));
    EdsSourceDiscovery {
        origin: if sources.is_empty() && error.is_some() {
            EdsSourceOrigin::Empty
        } else {
            EdsSourceOrigin::Live
        },
        sources,
        error,
    }
}

#[cfg(target_os = "macos")]
fn macos_configured_mail_sources(
    profiles: &[super::account_config_service::AccountProfile],
) -> EdsSourceDiscovery {
    let mut sources: Vec<_> = profiles
        .iter()
        .filter(|profile| !profile.id.trim().is_empty())
        .map(|profile| EdsSource {
            // Graph requests and saved folder selections use the exact profile ID.
            uid: profile.id.clone(),
            display_name: profile.display_name.clone(),
            backend_name: profile.protocol.as_str().to_string(),
            enabled: profile.enabled,
            parent_name: None,
            color: None,
            foreign_mail: None,
        })
        .collect();
    sort_sources(&mut sources);
    EdsSourceDiscovery {
        sources,
        origin: EdsSourceOrigin::Live,
        error: Some("Mail accounts are configured in Evolution; macOS does not share mail credentials from Internet Accounts with other apps".to_string()),
    }
}

#[cfg(target_os = "macos")]
pub async fn list_sources(kind: EdsSourceKind) -> Result<Vec<EdsSource>, EdsServiceError> {
    let discovery = discover_sources(kind).await;
    if discovery.sources.is_empty() {
        if let Some(error) = discovery.error {
            return Err(EdsServiceError::OperationFailed(error));
        }
    }
    Ok(discovery.sources)
}

#[cfg(target_os = "macos")]
pub async fn list_mail_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    list_sources(EdsSourceKind::Mail).await
}

#[cfg(target_os = "macos")]
pub async fn list_calendar_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    list_sources(EdsSourceKind::Calendar).await
}

#[cfg(target_os = "macos")]
pub async fn list_address_book_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    list_sources(EdsSourceKind::AddressBook).await
}

fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn recovery_delay_seconds(consecutive_failures: u32) -> u64 {
    if consecutive_failures == 0 {
        return 0;
    }
    let shift = consecutive_failures.saturating_sub(1).min(31);
    EDS_RETRY_MIN_SECONDS
        .saturating_mul(1_u64 << shift)
        .min(EDS_RETRY_MAX_SECONDS)
}

fn recovery_retry_remaining(recovery: &EdsSourceCacheRecovery, now: i64) -> Option<u64> {
    let failed_at = recovery.last_failure_unix_secs?;
    let delay = recovery_delay_seconds(recovery.consecutive_failures);
    let elapsed = now.saturating_sub(failed_at).max(0) as u64;
    delay
        .checked_sub(elapsed)
        .filter(|remaining| *remaining > 0)
}

fn recovery_after_success(previous: &EdsSourceCacheRecovery, now: i64) -> EdsSourceCacheRecovery {
    EdsSourceCacheRecovery {
        generation: previous.generation.wrapping_add(1),
        consecutive_failures: 0,
        last_success_unix_secs: Some(now),
        last_failure_unix_secs: previous.last_failure_unix_secs,
        last_error: None,
    }
}

fn recovery_after_failure(
    previous: &EdsSourceCacheRecovery,
    now: i64,
    error: &str,
) -> EdsSourceCacheRecovery {
    EdsSourceCacheRecovery {
        generation: previous.generation.wrapping_add(1),
        consecutive_failures: previous.consecutive_failures.saturating_add(1),
        last_success_unix_secs: previous.last_success_unix_secs,
        last_failure_unix_secs: Some(now),
        last_error: Some(error.to_string()),
    }
}

fn discovery_from_cache(cached: EdsSourceCacheLoad, error: String) -> EdsSourceDiscovery {
    let cache_warning = match (cached.status, cached.warning) {
        (EdsSourceCacheStatus::Corrupt, Some(warning)) => {
            Some(format!("Quellencache beschädigt: {warning}"))
        }
        (EdsSourceCacheStatus::Unreadable, Some(warning)) => {
            Some(format!("Quellencache nicht lesbar: {warning}"))
        }
        _ => None,
    };
    let error = cache_warning
        .map(|warning| format!("{error}; {warning}"))
        .unwrap_or(error);
    EdsSourceDiscovery {
        origin: if cached.sources.is_empty() {
            EdsSourceOrigin::Empty
        } else {
            EdsSourceOrigin::Cache
        },
        sources: cached.sources,
        error: Some(error),
    }
}

/// Run source discovery on a caller-owned worker context.
///
/// The D-Bus API is asynchronous, but a cold session/provider startup can
/// still keep an executor future pending for a noticeable amount of time.
/// UI callers should invoke this from a worker thread and only hand the
/// resulting plain data back to GTK. A private GLib context keeps the zbus
/// future alive without borrowing the application's main context.
#[cfg(not(target_os = "macos"))]
pub fn discover_sources_blocking(kind: EdsSourceKind) -> EdsSourceDiscovery {
    MainContext::new().block_on(discover_sources(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_source_discovery_keeps_calendar_contact_and_reminder_scopes_distinct() {
        use super::super::macos_integration_service::{
            MacosSourceKind, MacosSystemSnapshot, MacosSystemSource,
        };
        let sources = [
            ("calendar-a", MacosSourceKind::Calendar),
            ("reminders-a", MacosSourceKind::Reminders),
            ("contacts-a", MacosSourceKind::Contacts),
        ]
        .into_iter()
        .map(|(uid, kind)| MacosSystemSource {
            uid: uid.to_string(),
            name: "Work".to_string(),
            kind,
            account_uid: "account-a".to_string(),
            account_name: "System account".to_string(),
            backend: "icloud".to_string(),
            writable: false,
        })
        .collect::<Vec<_>>();
        let expected_calendar_uid = sources[0].source_uid();
        let expected_contact_uid = sources[2].source_uid();
        for (kind, expected_uid) in [
            (EdsSourceKind::Calendar, Some(expected_calendar_uid)),
            (EdsSourceKind::AddressBook, Some(expected_contact_uid)),
            (EdsSourceKind::Mail, None),
        ] {
            let snapshot = MacosSystemSnapshot {
                version: 1,
                sources: sources.clone(),
                errors: vec!["Erinnerungen: Zugriff verweigert".to_string()],
                ..MacosSystemSnapshot::default()
            };
            let discovery = macos_system_source_discovery(kind, Ok(snapshot));
            if let Some(expected_uid) = expected_uid {
                assert_eq!(discovery.sources.len(), 1);
                assert_eq!(discovery.sources[0].uid, expected_uid);
                assert_eq!(discovery.origin, EdsSourceOrigin::Live);
            } else {
                assert!(discovery.sources.is_empty());
            }
            assert!(discovery.status_text().contains("Zugriff verweigert"));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_discovery_permission_error_never_becomes_a_cached_source() {
        let discovery = macos_system_source_discovery(
            EdsSourceKind::Calendar,
            Err("Kalenderzugriff wurde verweigert".to_string()),
        );
        assert!(discovery.sources.is_empty());
        assert_eq!(discovery.origin, EdsSourceOrigin::Empty);
        assert!(discovery.status_text().contains("verweigert"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_mail_discovery_preserves_app_profile_identity_and_enabled_state() {
        use super::super::account_config_service::{AccountProfile, AccountProtocol};
        let enabled = AccountProfile {
            id: "profile-exchange-123".to_string(),
            display_name: "Work".to_string(),
            protocol: AccountProtocol::Exchange,
            enabled: true,
            ..AccountProfile::default()
        };
        let disabled = AccountProfile {
            id: "disabled-account".to_string(),
            display_name: "Archive".to_string(),
            enabled: false,
            ..AccountProfile::default()
        };
        let discovery = macos_configured_mail_sources(&[enabled, disabled]);
        assert_eq!(discovery.sources.len(), 2);
        assert_eq!(discovery.sources[0].uid, "disabled-account");
        assert!(!discovery.sources[0].enabled);
        assert_eq!(discovery.sources[1].uid, "profile-exchange-123");
        assert!(discovery.sources[1].enabled);
        assert_eq!(discovery.sources[1].backend_name, "exchange");
        assert!(discovery.status_text().contains("mail credentials"));
        assert!(macos_configured_mail_sources(&[]).sources.is_empty());
    }

    #[test]
    fn live_source_status_retains_partial_permission_warning() {
        let discovery = EdsSourceDiscovery {
            sources: vec![EdsSource {
                uid: "permitted-source".to_string(),
                display_name: "Calendar".to_string(),
                backend_name: "eventkit".to_string(),
                enabled: true,
                parent_name: None,
                color: None,
                foreign_mail: None,
            }],
            origin: EdsSourceOrigin::Live,
            error: Some("Kontakte: Zugriff verweigert".to_string()),
        };
        assert!(discovery.status_text().contains("1 Kontoquelle"));
        assert!(discovery.status_text().contains("Zugriff verweigert"));
    }

    #[test]
    fn test_eds_source_creation() {
        let source = EdsSource {
            uid: "test-uid-123".to_string(),
            display_name: "Test Account".to_string(),
            backend_name: "imapx".to_string(),
            enabled: true,
            parent_name: None,
            color: None,
            foreign_mail: None,
        };
        assert_eq!(source.uid, "test-uid-123");
        assert!(source.enabled);
    }

    #[test]
    fn test_error_display() {
        let err = EdsServiceError::RegistryUnavailable;
        assert_eq!(err.to_string(), "Kontoquellen sind nicht verfügbar");
    }

    #[test]
    fn mail_sources_are_sorted_case_insensitively() {
        let mut sources = vec![
            EdsSource {
                uid: "z".to_string(),
                display_name: "Work".to_string(),
                backend_name: "imapx".to_string(),
                enabled: true,
                parent_name: None,
                color: None,
                foreign_mail: None,
            },
            EdsSource {
                uid: "b".to_string(),
                display_name: "private".to_string(),
                backend_name: "local".to_string(),
                enabled: true,
                parent_name: None,
                color: None,
                foreign_mail: None,
            },
            EdsSource {
                uid: "a".to_string(),
                display_name: "Private".to_string(),
                backend_name: "local".to_string(),
                enabled: false,
                parent_name: None,
                color: None,
                foreign_mail: None,
            },
        ];

        sort_sources(&mut sources);

        assert_eq!(
            sources
                .iter()
                .map(|source| source.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "z"]
        );
    }

    #[test]
    fn source_data_selects_only_the_requested_eds_extension() {
        let data = "[Data Source]\nDisplayName=Domain Account\nEnabled=true\n[Mail Account]\nBackendName=imapx\n";

        assert_eq!(
            parse_source_data(
                data,
                EdsSourceKind::Mail,
                "fallback".to_string(),
                "unknown".to_string(),
                false,
                None,
            ),
            Some((
                "Domain Account".to_string(),
                "imapx".to_string(),
                true,
                None,
                None,
                None,
            ))
        );
        assert_eq!(
            parse_source_data(
                data,
                EdsSourceKind::Calendar,
                "fallback".to_string(),
                "unknown".to_string(),
                false,
                None,
            ),
            None
        );
    }

    #[test]
    fn test_parse_source_data_with_foreign_mail() {
        let data = "[Data Source]\nDisplayName=Kalender (EDV)\nEnabled=true\nParent=primary-account\n[Exchange Web Services Folder]\nForeignMail=edv@acme-corp.example\n[Calendar]\nBackendName=ews\nColor=#e66100\n";
        let parsed = parse_source_data(
            data,
            EdsSourceKind::Calendar,
            "fallback".to_string(),
            "unknown".to_string(),
            false,
            None,
        );
        assert_eq!(
            parsed,
            Some((
                "Kalender (EDV)".to_string(),
                "ews".to_string(),
                true,
                Some("primary-account".to_string()),
                Some("#e66100".to_string()),
                Some("edv@acme-corp.example".to_string()),
            ))
        );
    }

    #[test]
    fn recovery_backoff_is_exponential_and_bounded() {
        assert_eq!(recovery_delay_seconds(0), 0);
        assert_eq!(recovery_delay_seconds(1), 2);
        assert_eq!(recovery_delay_seconds(2), 4);
        assert_eq!(recovery_delay_seconds(5), 32);
        assert_eq!(recovery_delay_seconds(32), EDS_RETRY_MAX_SECONDS);
    }

    #[test]
    fn recovery_failure_and_success_transitions_survive_restart_time() {
        let initial = EdsSourceCacheRecovery::default();
        let failed = recovery_after_failure(&initial, 1_000, "registry unavailable");
        assert_eq!(failed.generation, 1);
        assert_eq!(failed.consecutive_failures, 1);
        assert_eq!(recovery_retry_remaining(&failed, 1_000), Some(2));
        assert_eq!(recovery_retry_remaining(&failed, 1_002), None);

        let failed_again = recovery_after_failure(&failed, 1_010, "still unavailable");
        assert_eq!(failed_again.consecutive_failures, 2);
        assert_eq!(recovery_retry_remaining(&failed_again, 1_011), Some(3));

        let recovered = recovery_after_success(&failed_again, 1_020);
        assert_eq!(recovered.generation, 3);
        assert_eq!(recovered.consecutive_failures, 0);
        assert_eq!(recovered.last_success_unix_secs, Some(1_020));
        assert_eq!(recovery_retry_remaining(&recovered, 1_020), None);
        assert!(recovered.last_error.is_none());
    }

    #[test]
    fn corrupt_cache_warning_is_not_reported_as_an_empty_valid_cache() {
        let discovery = discovery_from_cache(
            EdsSourceCacheLoad {
                sources: Vec::new(),
                status: EdsSourceCacheStatus::Corrupt,
                warning: Some("invalid JSON".to_string()),
                recovery: EdsSourceCacheRecovery::default(),
            },
            "registry unavailable".to_string(),
        );

        assert_eq!(discovery.origin, EdsSourceOrigin::Empty);
        let error = discovery.error.expect("warning should remain visible");
        assert!(error.contains("registry unavailable"));
        assert!(error.contains("Quellencache beschädigt"));
    }

    #[tokio::test]
    #[ignore = "Requires explicit live account and personal-data access"]
    async fn live_eds_source_discovery_smoke() {
        let cal_discovery = discover_sources(EdsSourceKind::Calendar).await;
        println!(
            "Live EDS Calendar sources discovery: count={}, origin={:?}, err={:?}",
            cal_discovery.sources.len(),
            cal_discovery.origin,
            cal_discovery.error
        );
        for s in &cal_discovery.sources {
            println!(
                "  [Cal] uid={}, name={}, backend={}, parent={:?}",
                s.uid, s.display_name, s.backend_name, s.parent_name
            );
        }

        let book_discovery = discover_sources(EdsSourceKind::AddressBook).await;
        println!(
            "Live EDS AddressBook sources discovery: count={}, origin={:?}",
            book_discovery.sources.len(),
            book_discovery.origin
        );
        for s in &book_discovery.sources {
            println!(
                "  [Book] uid={}, name={}, backend={}, parent={:?}",
                s.uid, s.display_name, s.backend_name, s.parent_name
            );
        }

        let mail_discovery = discover_sources(EdsSourceKind::Mail).await;
        println!(
            "Live EDS Mail sources discovery: count={}, origin={:?}",
            mail_discovery.sources.len(),
            mail_discovery.origin
        );
        for s in &mail_discovery.sources {
            println!(
                "  [Mail] uid={}, name={}, backend={}, parent={:?}",
                s.uid, s.display_name, s.backend_name, s.parent_name
            );
        }
    }

    #[tokio::test]
    #[ignore = "Requires explicit live account and personal-data access"]
    async fn live_eds_components_and_contacts_smoke() {
        use crate::services::ebook_service::EBookService;
        use crate::services::ecal_service::ECalService;

        let ecal = ECalService::new();
        match ecal.load_snapshot() {
            Ok(snapshot) => {
                println!(
                    "Live ECal Snapshot loaded: {} events, {} tasks, {} memos",
                    snapshot.events.len(),
                    snapshot.tasks.len(),
                    snapshot.memos.len()
                );
                for ev in snapshot.events.iter().take(5) {
                    println!(
                        "  [Event] {} (source: {})",
                        ev.summary, ev.source_display_name
                    );
                }
                for t in snapshot.tasks.iter().take(5) {
                    println!("  [Task] {} (source: {})", t.summary, t.source_display_name);
                }
                for m in snapshot.memos.iter().take(5) {
                    println!("  [Memo] {} (source: {})", m.summary, m.source_display_name);
                }
            }
            Err(e) => println!("Live ECal load error: {e}"),
        }

        let ebook = EBookService::new();
        match ebook.load_contacts() {
            Ok((contacts, warning)) => {
                println!(
                    "Live EBook Contacts loaded: {} contacts (warning: {:?})",
                    contacts.len(),
                    warning
                );
                for c in contacts.iter().take(5) {
                    println!(
                        "  [Contact] {} (source: {})",
                        c.contact.fn_name, c.source_display_name
                    );
                }
            }
            Err(e) => println!("Live EBook load error: {e}"),
        }

        use crate::services::camel_mail_service;
        match camel_mail_service::load_messages_page(None, None, 0, 5) {
            Ok(mail_load) => {
                println!(
                    "Live Camel Mail loaded: {} messages, {} folders (warning: {:?})",
                    mail_load.messages.len(),
                    mail_load.folders.len(),
                    mail_load.warning
                );
                for f in mail_load.folders.iter().take(5) {
                    println!(
                        "  [Folder] {} (unread: {}, total: {})",
                        f.folder_name, f.unread_count, f.total_count
                    );
                }
                for msg in mail_load.messages.iter().take(5) {
                    println!(
                        "  [Mail Message] {} from {:?}",
                        msg.subject, msg.sender_name
                    );
                }
            }
            Err(e) => println!("Live Camel load error: {e}"),
        }
    }

    #[tokio::test]
    async fn address_book_sources_incorporate_configured_accounts() {
        invalidate_account_collections_cache();
        let sources = list_address_book_sources().await;
        if let Ok(sources) = sources {
            let collections = load_configured_account_collections();
            for coll in collections {
                if coll.contacts_enabled && !is_shared_or_delegate_mailbox(&coll.display_name) && !is_shared_or_delegate_mailbox(&coll.uid) {
                    assert!(
                        sources.iter().any(|s| {
                            s.parent_name.as_deref() == Some(&coll.display_name)
                                || s.uid == coll.uid
                                || s.uid == format!("account-contacts-{}", coll.uid)
                        }),
                        "Configured account collection {} should be represented in address book sources",
                        coll.display_name
                    );
                }
            }
        }
    }
}

