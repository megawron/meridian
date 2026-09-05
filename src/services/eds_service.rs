// SPDX-License-Identifier: GPL-3.0-or-later
//
// EDS Service — Async D-Bus communication with Evolution Data Server.
//
// This module demonstrates the pattern for non-blocking data fetching
// from EDS using the zbus crate. All operations return Futures that
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

use glib::{KeyFile, KeyFileFlags, MainContext};
use serde::{Deserialize, Serialize};
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
            (EdsSourceOrigin::Live, 0, _) => "Keine Kontoquelle gefunden".to_string(),
            (EdsSourceOrigin::Live, count, _) => format!("{count} Kontoquelle(n)"),
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
) -> Option<(String, String, String, bool)> {
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

    let Some(data) = source_props.get("Data").and_then(value_to_string) else {
        return kind.matches_interfaces(interfaces).then_some((
            uid,
            property_display_name,
            property_backend_name,
            property_enabled,
        ));
    };

    parse_source_data(
        &data,
        kind,
        property_display_name,
        property_backend_name,
        property_enabled,
    )
    .map(|(display_name, backend_name, enabled)| (uid, display_name, backend_name, enabled))
}

fn parse_source_data(
    data: &str,
    kind: EdsSourceKind,
    fallback_display_name: String,
    fallback_backend_name: String,
    fallback_enabled: bool,
) -> Option<(String, String, bool)> {
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

    Some((display_name, backend_name, enabled))
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

    let managed_objects = proxy.get_managed_objects().await?;
    Ok(managed_objects)
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
pub async fn list_mail_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    tracing::debug!("Connecting to EDS Source Registry & GNOME Online Accounts via D-Bus...");

    let (mut sources, registry_error) = match list_sources(EdsSourceKind::Mail).await {
        Ok(sources) => (sources, None),
        Err(error) => {
            tracing::debug!(%error, "EDS source registry is unavailable");
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
    } else {
        tracing::info!(
            "Found {} mail sources via D-Bus (EDS; GOA fallback only)",
            sources.len()
        );
    }
    Ok(sources)
}

/// List EDS sources for one product area through the shared SourceManager
/// object model.  Account mutation and ECal/EBook client streams intentionally
/// remain separate adapter layers.
pub async fn list_sources(kind: EdsSourceKind) -> Result<Vec<EdsSource>, EdsServiceError> {
    let managed_objects = get_eds_managed_objects().await?;
    let mut sources = Vec::new();

    for (path, interfaces) in &managed_objects {
        let Some(source_props) = interfaces.get("org.gnome.evolution.dataserver.Source") else {
            continue;
        };

        let Some((uid, display_name, backend_name, enabled)) =
            extract_source(path, source_props, interfaces, kind)
        else {
            continue;
        };
        if !display_name.trim().is_empty() {
            sources.push(EdsSource {
                uid,
                display_name,
                backend_name,
                enabled,
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
                    .unwrap_or_else(|| "Kalenderkonto".to_string());

                let provider_name = account_props
                    .get("ProviderName")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "Online-Kalender".to_string());

                let provider_type = account_props
                    .get("ProviderType")
                    .and_then(value_to_string)
                    .unwrap_or_else(|| "caldav".to_string());

                goa_sources.push(EdsSource {
                    uid: format!("goa-cal-{}", uid),
                    display_name: format!("{} ({})", identity, provider_name),
                    backend_name: provider_type,
                    enabled: true,
                });
            }
        }
    }
    Ok(goa_sources)
}

/// List all calendar sources from EDS (with GOA fallback).
#[allow(dead_code)]
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

/// List all address book sources from EDS.
#[allow(dead_code)]
pub async fn list_address_book_sources() -> Result<Vec<EdsSource>, EdsServiceError> {
    tracing::debug!("Listing address book sources from EDS...");
    let sources = list_sources(EdsSourceKind::AddressBook).await?;
    tracing::info!("Found {} address book sources via D-Bus", sources.len());
    Ok(sources)
}

/// Discover EDS sources and retain only non-secret source metadata for an
/// offline UI state.  A cached result is explicitly marked as such and is
/// never treated as authoritative for mutations or message/calendar data.
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
pub fn discover_sources_blocking(kind: EdsSourceKind) -> EdsSourceDiscovery {
    MainContext::new().block_on(discover_sources(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eds_source_creation() {
        let source = EdsSource {
            uid: "test-uid-123".to_string(),
            display_name: "Test Account".to_string(),
            backend_name: "imapx".to_string(),
            enabled: true,
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
            },
            EdsSource {
                uid: "b".to_string(),
                display_name: "private".to_string(),
                backend_name: "local".to_string(),
                enabled: true,
            },
            EdsSource {
                uid: "a".to_string(),
                display_name: "Private".to_string(),
                backend_name: "local".to_string(),
                enabled: false,
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
            ),
            Some(("Domain Account".to_string(), "imapx".to_string(), true))
        );
        assert_eq!(
            parse_source_data(
                data,
                EdsSourceKind::Calendar,
                "fallback".to_string(),
                "unknown".to_string(),
                false,
            ),
            None
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
    async fn live_eds_source_discovery_smoke() {
        if let Ok(sources) = list_sources(EdsSourceKind::Calendar).await {
            println!("Live EDS Calendar sources count: {}", sources.len());
            for s in &sources {
                println!("  [Cal] uid={}, name={}, backend={}", s.uid, s.display_name, s.backend_name);
            }
        }
        if let Ok(sources) = list_sources(EdsSourceKind::AddressBook).await {
            println!("Live EDS AddressBook sources count: {}", sources.len());
            for s in &sources {
                println!("  [Book] uid={}, name={}, backend={}", s.uid, s.display_name, s.backend_name);
            }
        }
        if let Ok(sources) = list_sources(EdsSourceKind::Mail).await {
            println!("Live EDS Mail sources count: {}", sources.len());
            for s in &sources {
                println!("  [Mail] uid={}, name={}, backend={}", s.uid, s.display_name, s.backend_name);
            }
        }
    }

    #[tokio::test]
    async fn live_eds_components_and_contacts_smoke() {
        use crate::services::ecal_service::ECalService;
        use crate::services::ebook_service::EBookService;

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
                    println!("  [Event] {} (source: {})", ev.summary, ev.source_display_name);
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
                    println!("  [Contact] {} (source: {})", c.contact.fn_name, c.source_display_name);
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
                    println!("  [Folder] {} (unread: {}, total: {})", f.folder_name, f.unread_count, f.total_count);
                }
                for msg in mail_load.messages.iter().take(5) {
                    println!("  [Mail Message] {} from {:?}", msg.subject, msg.sender_name);
                }
            }
            Err(e) => println!("Live Camel load error: {e}"),
        }
    }
}
