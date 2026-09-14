// SPDX-License-Identifier: GPL-3.0-or-later
//
// Direct ECal adapter for synchronized calendars, task lists and memo lists.
// EDS remains authoritative; the local iCalendar importer is only used when
// the native ECal bridge is unavailable (for example on non-Linux dev hosts).

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
use super::calendar_store_service::sort_snapshot;
use super::calendar_store_service::{
    CalendarEventRecord, CalendarSnapshot, CalendarStoreService, MemoRecord, TaskRecord,
    parse_ical_component,
};
use super::exchange_service::graph_service_for_configured_account;
use chrono::{Duration as ChronoDuration, Utc};
#[cfg(all(target_os = "linux", eds_ecal_bridge))]
use std::collections::HashSet;
#[cfg(all(target_os = "linux", eds_ecal_bridge))]
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalendarDataOrigin {
    Eds,
    Graph,
    Local,
}

#[derive(Debug, Clone)]
pub(crate) struct CalendarLoadResult {
    pub(crate) snapshot: CalendarSnapshot,
    pub(crate) origin: CalendarDataOrigin,
    pub(crate) warning: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum ECalServiceError {
    Unsupported,
    Backend(String),
}

impl std::fmt::Display for ECalServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "Der Kalenderdienst ist nicht verfügbar"),
            Self::Backend(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ECalServiceError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum ECalComponentType {
    Event = 0,
    Task = 1,
    Memo = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ECalRefreshOutcome {
    /// The app-owned local calendar was reread; there is no remote provider
    /// refresh to request for this source.
    LocalReloadOnly,
    /// EDS accepted and completed a provider refresh for the exact source.
    RemoteRefreshed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalSourceCapability {
    pub(crate) source_uid: String,
    pub(crate) display_name: String,
    /// A client was connected successfully and its capabilities are current.
    pub(crate) connected: bool,
    /// Kept separately for consumers that distinguish a listed source from an
    /// immediately usable EDS client.
    pub(crate) available: bool,
    /// ESource's own writable flag.  This controls source management such as
    /// rename/properties independently from a connected calendar client's
    /// readonly state.
    pub(crate) source_writable: bool,
    pub(crate) writable: bool,
    pub(crate) refresh_supported: bool,
    pub(crate) removable: bool,
    pub(crate) remote_deletable: bool,
    pub(crate) in_collection: bool,
    /// Only meaningful for an available task-list client.
    pub(crate) task_assignment_supported: bool,
    /// Only meaningful when task assignment itself is supported.
    pub(crate) task_assignment_conversion_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalSourceDiscovery {
    pub(crate) sources: Vec<ECalSourceCapability>,
    /// First non-secret per-source connection warning, while retaining all
    /// exact enabled source metadata in `sources`.
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ECalSourceCopyStatus {
    Complete,
    /// The native worker stopped at the first failed item.  The accompanying
    /// result counts are the exact confirmed prefix already written.
    PartialFailure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalSourceCopyResult {
    /// Number of source components confirmed in the destination (creates and
    /// modifications together).
    pub(crate) copied_count: usize,
    /// Number of confirmed destination modifications within `copied_count`.
    pub(crate) updated_count: usize,
    pub(crate) status: ECalSourceCopyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ECalLocalSourceCreateStatus {
    /// EDS committed the source and its exact ECal client opened successfully.
    Available,
    /// EDS committed the source, but the first client connection did not
    /// succeed.  The UID is still authoritative and can be retried later.
    CreatedUnavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalLocalSourceCreateResult {
    pub(crate) source_uid: String,
    pub(crate) status: ECalLocalSourceCreateStatus,
}

/// Editable local-backend settings sourced from the exact registered ESource.
/// `is_builtin` is informational; callers must not attempt to change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalLocalSourceProperties {
    pub(crate) source_uid: String,
    pub(crate) display_name: String,
    pub(crate) color: String,
    pub(crate) custom_file_uri: Option<String>,
    pub(crate) custom_file_writable: bool,
    pub(crate) email_address: String,
    pub(crate) is_builtin: bool,
}

/// The subset of local-source properties that may be committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalLocalSourcePropertiesUpdate {
    pub(crate) display_name: String,
    pub(crate) color: String,
    pub(crate) custom_file_uri: Option<String>,
    pub(crate) custom_file_writable: bool,
    pub(crate) email_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalLocalSourcePropertiesUpdateResult {
    pub(crate) source_uid: String,
    pub(crate) status: ECalLocalSourceCreateStatus,
}

fn is_valid_ical_source_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

fn task_assignment_capabilities(
    component_type: ECalComponentType,
    connected: bool,
    assignment_supported: bool,
    assignment_conversion_supported: bool,
) -> (bool, bool) {
    let assignment_supported =
        component_type == ECalComponentType::Task && connected && assignment_supported;
    (
        assignment_supported,
        assignment_supported && assignment_conversion_supported,
    )
}

#[derive(Debug)]
pub(crate) enum ECalMutationError {
    Unsupported,
    Conflict(String),
    Backend(String),
}

impl std::fmt::Display for ECalMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "Der Kalenderdienst ist nicht verfügbar"),
            Self::Conflict(message) => write!(formatter, "Kalenderkonflikt: {message}"),
            Self::Backend(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ECalMutationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalMutationResult {
    pub(crate) uid: String,
    pub(crate) component: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ECalChangeKind {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalChangeEvent {
    pub(crate) source_type: ECalComponentType,
    pub(crate) kind: ECalChangeKind,
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) uid: String,
    pub(crate) component: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ECalRecord {
    Event(CalendarEventRecord),
    Task(TaskRecord),
    Memo(MemoRecord),
}

pub(crate) fn parse_change_record(event: &ECalChangeEvent) -> Option<ECalRecord> {
    let component = event.component.as_deref()?;
    let mut snapshot = CalendarSnapshot::default();
    parse_ical_component(
        component,
        &event.source_uid,
        &event.source_display_name,
        &mut snapshot,
    );
    match event.source_type {
        ECalComponentType::Event => snapshot
            .events
            .into_iter()
            .find(|record| record.uid == event.uid)
            .map(ECalRecord::Event),
        ECalComponentType::Task => snapshot
            .tasks
            .into_iter()
            .find(|record| record.uid == event.uid)
            .map(ECalRecord::Task),
        ECalComponentType::Memo => snapshot
            .memos
            .into_iter()
            .find(|record| record.uid == event.uid)
            .map(ECalRecord::Memo),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ECalWatcherStatus {
    pub(crate) configured_sources: usize,
    pub(crate) active_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) warning: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ECalSyncEvent {
    Ready(ECalWatcherStatus),
    Change(ECalChangeEvent),
    Error(String),
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ECalService;

impl ECalService {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn load_snapshot(&self) -> Result<CalendarSnapshot, ECalServiceError> {
        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return load_snapshot_native();
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn refresh_source(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalRefreshOutcome, ECalServiceError> {
        if source_uid.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "ECal-Aktualisierung benötigt eine Quell-UID".to_string(),
            ));
        }
        if source_uid == "local-calendar" {
            // Reload the explicit portable source without claiming it has a
            // remote refresh endpoint.
            let _ = CalendarStoreService::new().load_snapshot();
            return Ok(ECalRefreshOutcome::LocalReloadOnly);
        }
        if graph_service_for_configured_account()
            .is_some_and(|(configured_uid, _)| configured_uid == source_uid)
        {
            // Graph calendar synchronization has no EClient refresh contract.
            return Err(ECalServiceError::Unsupported);
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.refresh_source_native(source_uid, component_type);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn discover_source_capabilities(
        &self,
        component_type: ECalComponentType,
    ) -> Result<ECalSourceDiscovery, ECalServiceError> {
        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.discover_source_capabilities_native(component_type);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn rename_source(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        display_name: &str,
    ) -> Result<(), ECalServiceError> {
        if source_uid.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "ECal-Umbenennung benötigt eine Quell-UID".to_string(),
            ));
        }
        if display_name.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "ECal-Umbenennung benötigt einen Namen".to_string(),
            ));
        }
        if source_uid == "local-calendar"
            || graph_service_for_configured_account()
                .is_some_and(|(configured_uid, _)| configured_uid == source_uid)
        {
            return Err(ECalServiceError::Unsupported);
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.rename_source_native(source_uid, component_type, display_name);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn delete_source(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<(), ECalServiceError> {
        if source_uid.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "ECal-Löschen benötigt eine Quell-UID".to_string(),
            ));
        }
        if source_uid == "local-calendar"
            || graph_service_for_configured_account()
                .is_some_and(|(configured_uid, _)| configured_uid == source_uid)
        {
            return Err(ECalServiceError::Unsupported);
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.delete_source_native(source_uid, component_type);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn copy_source(
        &self,
        from_source_uid: &str,
        to_source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalSourceCopyResult, ECalServiceError> {
        if from_source_uid.trim().is_empty() || to_source_uid.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "ECal-Quellenkopie benötigt zwei Quell-UIDs".to_string(),
            ));
        }
        if from_source_uid == to_source_uid {
            return Err(ECalServiceError::Backend(
                "Quelle und Ziel der ECal-Quellenkopie müssen verschieden sein".to_string(),
            ));
        }
        if from_source_uid == "local-calendar"
            || to_source_uid == "local-calendar"
            || graph_service_for_configured_account().is_some_and(|(configured_uid, _)| {
                configured_uid == from_source_uid || configured_uid == to_source_uid
            })
        {
            return Err(ECalServiceError::Unsupported);
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.copy_source_native(from_source_uid, to_source_uid, component_type);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn create_local_source(
        &self,
        display_name: &str,
        color: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalLocalSourceCreateResult, ECalServiceError> {
        if display_name.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "Eine lokale ECal-Quelle benötigt einen Namen".to_string(),
            ));
        }
        if !is_valid_ical_source_color(color) {
            return Err(ECalServiceError::Backend(
                "Die Farbe der lokalen ECal-Quelle muss #RRGGBB sein".to_string(),
            ));
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.create_local_source_native(display_name, color, component_type);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn local_source_properties(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalLocalSourceProperties, ECalServiceError> {
        if source_uid.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "Lokale ECal-Eigenschaften benötigen eine Quell-UID".to_string(),
            ));
        }
        if source_uid == "local-calendar"
            || graph_service_for_configured_account()
                .is_some_and(|(configured_uid, _)| configured_uid == source_uid)
        {
            return Err(ECalServiceError::Unsupported);
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.local_source_properties_native(source_uid, component_type);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn update_local_source_properties(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        update: &ECalLocalSourcePropertiesUpdate,
    ) -> Result<ECalLocalSourcePropertiesUpdateResult, ECalServiceError> {
        if source_uid.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "Lokale ECal-Eigenschaften benötigen eine Quell-UID".to_string(),
            ));
        }
        if update.display_name.trim().is_empty() {
            return Err(ECalServiceError::Backend(
                "Lokale ECal-Eigenschaften benötigen einen Namen".to_string(),
            ));
        }
        if !is_valid_ical_source_color(&update.color) {
            return Err(ECalServiceError::Backend(
                "Die Farbe der lokalen ECal-Quelle muss #RRGGBB sein".to_string(),
            ));
        }
        if update
            .custom_file_uri
            .as_deref()
            .is_some_and(|uri| uri.trim().is_empty())
        {
            return Err(ECalServiceError::Backend(
                "Die Datei-URI der lokalen ECal-Quelle darf nicht leer sein".to_string(),
            ));
        }
        if source_uid == "local-calendar"
            || graph_service_for_configured_account()
                .is_some_and(|(configured_uid, _)| configured_uid == source_uid)
        {
            return Err(ECalServiceError::Unsupported);
        }

        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            return self.update_local_source_properties_native(source_uid, component_type, update);
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            let _ = component_type;
            Err(ECalServiceError::Unsupported)
        }
    }

    pub(crate) fn create_component(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        component: &str,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        if source_uid == "local-calendar" || source_uid.starts_with("account-tasks-") {
            return self.apply_local_operation(component_type, 1, component, None);
        }
        if let Some(result) =
            self.apply_graph_operation(source_uid, component_type, 1, component, None)
        {
            return result;
        }
        self.apply_operation(source_uid, component_type, 1, component, None)
    }

    pub(crate) fn modify_component(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        component_uid: &str,
        component: &str,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        if source_uid == "local-calendar" || source_uid.starts_with("account-tasks-") {
            return self.apply_local_operation(component_type, 2, component, Some(component_uid));
        }
        if let Some(result) = self.apply_graph_operation(
            source_uid,
            component_type,
            2,
            component,
            Some(component_uid),
        ) {
            return result;
        }
        self.apply_operation(
            source_uid,
            component_type,
            2,
            component,
            Some(component_uid),
        )
    }

    pub(crate) fn remove_component(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        component_uid: &str,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        if source_uid == "local-calendar" || source_uid.starts_with("account-tasks-") {
            return self.apply_local_operation(component_type, 3, "", Some(component_uid));
        }
        if let Some(result) =
            self.apply_graph_operation(source_uid, component_type, 3, "", Some(component_uid))
        {
            return result;
        }
        self.apply_operation(source_uid, component_type, 3, "", Some(component_uid))
    }

    pub(crate) fn start_change_monitor(
        &self,
    ) -> Option<(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        async_channel::Receiver<ECalSyncEvent>,
    )> {
        #[cfg(all(target_os = "linux", eds_ecal_bridge))]
        {
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, Ordering};

            let (sender, receiver) = async_channel::bounded(32);
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancellation = cancelled.clone();

            std::thread::spawn(move || {
                let watcher = match ECalWatcher::new() {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        let _ = sender.try_send(ECalSyncEvent::Error(error.to_string()));
                        return;
                    }
                };

                let _ = sender.try_send(ECalSyncEvent::Ready(watcher.status.clone()));
                let mut watcher = watcher;
                while !cancellation.load(Ordering::Relaxed) {
                    match watcher.next_event(Duration::from_millis(1000)) {
                        Ok(Some(event)) => {
                            if sender.try_send(ECalSyncEvent::Change(event)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = sender.try_send(ECalSyncEvent::Error(error.to_string()));
                            break;
                        }
                    }
                }
            });

            Some((cancelled, receiver))
        }

        #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
        {
            None
        }
    }

    fn apply_local_operation(
        &self,
        _component_type: ECalComponentType,
        operation: i32,
        component: &str,
        component_uid: Option<&str>,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        let service = CalendarStoreService::new();
        let result = match operation {
            1 => service.create_component(component),
            2 => service.modify_component(component_uid.unwrap_or_default(), component),
            3 => service
                .remove_component(component_uid.unwrap_or_default())
                .map(|uid| (uid, String::new())),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Unbekannte lokale ECal-Operation",
            )),
        };
        result
            .map(|(uid, component)| ECalMutationResult {
                uid,
                component: if operation == 3 {
                    None
                } else {
                    Some(component)
                },
            })
            .map_err(|error| ECalMutationError::Backend(error.to_string()))
    }

    fn apply_graph_operation(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        operation: i32,
        component: &str,
        component_uid: Option<&str>,
    ) -> Option<Result<ECalMutationResult, ECalMutationError>> {
        let service =
            graph_service_for_configured_account().and_then(|(configured_uid, service)| {
                (configured_uid == source_uid).then_some(service)
            })?;
        if component_type != ECalComponentType::Event {
            return Some(Err(ECalMutationError::Unsupported));
        }

        let source_display_name = if service.username.trim().is_empty() {
            "Microsoft 365 Kalender"
        } else {
            "Microsoft 365"
        };
        let parse_event = |raw: &str| {
            let mut snapshot = CalendarSnapshot::default();
            parse_ical_component(raw, source_uid, source_display_name, &mut snapshot);
            snapshot.events.into_iter().next().ok_or_else(|| {
                ECalMutationError::Backend(
                    "Die iCalendar-Komponente enthält kein gültiges VEVENT".to_string(),
                )
            })
        };

        let result = match operation {
            1 => parse_event(component).and_then(|event| {
                service
                    .create_calendar_event_sync(&event)
                    .map_err(|error| ECalMutationError::Backend(error.to_string()))?
                    .into_calendar_event(source_uid, source_display_name)
                    .map(|record| ECalMutationResult {
                        uid: record.uid.clone(),
                        component: Some(record.component),
                    })
                    .ok_or_else(|| {
                        ECalMutationError::Backend(
                            "Graph lieferte kein verwendbares Kalenderereignis zurück".to_string(),
                        )
                    })
            }),
            2 => {
                let uid = component_uid.unwrap_or_default();
                if uid.trim().is_empty() {
                    Err(ECalMutationError::Backend(
                        "Zum Ändern eines Graph-Kalenderereignisses fehlt die UID".to_string(),
                    ))
                } else {
                    parse_event(component).and_then(|event| {
                        service
                            .update_calendar_event_sync(uid, &event)
                            .map_err(|error| ECalMutationError::Backend(error.to_string()))?
                            .into_calendar_event(source_uid, source_display_name)
                            .map(|record| ECalMutationResult {
                                uid: record.uid.clone(),
                                component: Some(record.component),
                            })
                            .ok_or_else(|| {
                                ECalMutationError::Backend(
                                    "Graph lieferte kein verwendbares Kalenderereignis zurück"
                                        .to_string(),
                                )
                            })
                    })
                }
            }
            3 => {
                let uid = component_uid.unwrap_or_default();
                if uid.trim().is_empty() {
                    Err(ECalMutationError::Backend(
                        "Zum Löschen eines Graph-Kalenderereignisses fehlt die UID".to_string(),
                    ))
                } else {
                    service
                        .delete_calendar_event_sync(uid)
                        .map(|()| ECalMutationResult {
                            uid: uid.to_string(),
                            component: None,
                        })
                        .map_err(|error| ECalMutationError::Backend(error.to_string()))
                }
            }
            _ => Err(ECalMutationError::Backend(
                "Unbekannte Graph-ECal-Operation".to_string(),
            )),
        };
        Some(result)
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn apply_operation(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        operation: i32,
        component: &str,
        component_uid: Option<&str>,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        use std::ffi::CString;
        use std::ptr;

        let source_uid = CString::new(source_uid)
            .map_err(|_| ECalMutationError::Backend("Quell-UID enthält NUL".to_string()))?;
        let component = CString::new(component)
            .map_err(|_| ECalMutationError::Backend("Komponente enthält NUL".to_string()))?;
        let component_uid = component_uid
            .map(CString::new)
            .transpose()
            .map_err(|_| ECalMutationError::Backend("Komponenten-UID enthält NUL".to_string()))?;
        let component_uid_pointer = component_uid
            .as_ref()
            .map_or(ptr::null(), |value| value.as_ptr());
        let mut out_uid = ptr::null_mut();
        let mut out_component = ptr::null_mut();
        let mut out_error = ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_apply_operation(
                source_uid.as_ptr(),
                component_type as i32,
                operation,
                component.as_ptr(),
                component_uid_pointer,
                &mut out_uid,
                &mut out_component,
                &mut out_error,
            )
        };
        let uid = take_c_string(out_uid);
        let confirmed_component = take_c_string(out_component);
        let error = take_c_string(out_error);

        match result {
            1 => Ok(ECalMutationResult {
                uid: uid.unwrap_or_default(),
                component: confirmed_component,
            }),
            2 => {
                Err(ECalMutationError::Conflict(error.unwrap_or_else(|| {
                    "Backend meldet einen Konflikt".to_string()
                })))
            }
            _ => {
                Err(ECalMutationError::Backend(error.unwrap_or_else(|| {
                    "ECal-Mutation fehlgeschlagen".to_string()
                })))
            }
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn refresh_source_native(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalRefreshOutcome, ECalServiceError> {
        use std::ffi::CString;

        let source_uid = CString::new(source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_refresh_source(
                source_uid.as_ptr(),
                component_type as i32,
                &mut out_error,
            )
        };
        let error = error_string(out_error);
        match result {
            1 => Ok(ECalRefreshOutcome::RemoteRefreshed),
            2 => Err(ECalServiceError::Unsupported),
            _ => Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                "EDS/ECal-Aktualisierung fehlgeschlagen".to_string()
            }))),
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn rename_source_native(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        display_name: &str,
    ) -> Result<(), ECalServiceError> {
        use std::ffi::CString;

        let source_uid = CString::new(source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let display_name = CString::new(display_name)
            .map_err(|_| ECalServiceError::Backend("Quellenname enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_rename_source(
                source_uid.as_ptr(),
                component_type as i32,
                display_name.as_ptr(),
                &mut out_error,
            )
        };
        let error = error_string(out_error);
        if result == 1 {
            Ok(())
        } else {
            Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                "EDS/ECal-Umbenennung fehlgeschlagen".to_string()
            })))
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn delete_source_native(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<(), ECalServiceError> {
        use std::ffi::CString;

        let source_uid = CString::new(source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_delete_source(
                source_uid.as_ptr(),
                component_type as i32,
                &mut out_error,
            )
        };
        let error = error_string(out_error);
        if result == 1 {
            Ok(())
        } else {
            Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                "EDS/ECal-Löschen fehlgeschlagen".to_string()
            })))
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn copy_source_native(
        &self,
        from_source_uid: &str,
        to_source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalSourceCopyResult, ECalServiceError> {
        use std::ffi::CString;

        let from_source_uid = CString::new(from_source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let to_source_uid = CString::new(to_source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut copied_count = 0usize;
        let mut updated_count = 0usize;
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_copy_source(
                from_source_uid.as_ptr(),
                to_source_uid.as_ptr(),
                component_type as i32,
                &mut copied_count,
                &mut updated_count,
                &mut out_error,
            )
        };
        let error = error_string(out_error);
        match result {
            1 => Ok(ECalSourceCopyResult {
                copied_count,
                updated_count,
                status: ECalSourceCopyStatus::Complete,
            }),
            2 => Ok(ECalSourceCopyResult {
                copied_count,
                updated_count,
                status: ECalSourceCopyStatus::PartialFailure(error.unwrap_or_else(|| {
                    format!(
                        "ECal-Quellenkopie nach {copied_count} bestätigten Komponenten fehlgeschlagen"
                    )
                })),
            }),
            _ => Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                "EDS/ECal-Quellenkopie fehlgeschlagen".to_string()
            }))),
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn create_local_source_native(
        &self,
        display_name: &str,
        color: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalLocalSourceCreateResult, ECalServiceError> {
        use std::ffi::CString;

        let display_name = CString::new(display_name)
            .map_err(|_| ECalServiceError::Backend("Quellenname enthält NUL".to_string()))?;
        let color = CString::new(color)
            .map_err(|_| ECalServiceError::Backend("Quellenfarbe enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out_uid = std::ptr::null_mut();
        let mut out_warning = std::ptr::null_mut();
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_create_local_source(
                display_name.as_ptr(),
                color.as_ptr(),
                component_type as i32,
                &mut out_uid,
                &mut out_warning,
                &mut out_error,
            )
        };
        let source_uid = take_c_string(out_uid);
        let warning = take_c_string(out_warning);
        let error = take_c_string(out_error);
        let source_uid = source_uid.filter(|uid| !uid.trim().is_empty());
        match result {
            1 => source_uid
                .map(|source_uid| ECalLocalSourceCreateResult {
                    source_uid,
                    status: ECalLocalSourceCreateStatus::Available,
                })
                .ok_or_else(|| {
                    ECalServiceError::Backend(
                        "EDS bestätigte eine lokale Quelle ohne gültige UID".to_string(),
                    )
                }),
            2 => source_uid
                .map(|source_uid| ECalLocalSourceCreateResult {
                    source_uid,
                    status: ECalLocalSourceCreateStatus::CreatedUnavailable(
                        warning.unwrap_or_else(|| {
                            "Die lokale ECal-Quelle wurde erstellt, ist aber noch nicht erreichbar"
                                .to_string()
                        }),
                    ),
                })
                .ok_or_else(|| {
                    ECalServiceError::Backend(
                        "EDS bestätigte eine lokale Quelle ohne gültige UID".to_string(),
                    )
                }),
            _ => Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                "EDS konnte die lokale ECal-Quelle nicht erstellen".to_string()
            }))),
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn local_source_properties_native(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
    ) -> Result<ECalLocalSourceProperties, ECalServiceError> {
        use std::ffi::CString;

        let source_uid = CString::new(source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut raw: ffi::EvolutionEdsECalLocalSourceProperties = unsafe { std::mem::zeroed() };
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_get_local_source_properties(
                source_uid.as_ptr(),
                component_type as i32,
                &mut raw,
                &mut out_error,
            )
        };
        let error = error_string(out_error);
        let properties = unsafe {
            if result == 1 {
                let returned_uid = c_string(raw.source_uid).filter(|uid| !uid.trim().is_empty());
                let parsed = returned_uid
                    .filter(|uid| uid == source_uid.to_string_lossy().as_ref())
                    .map(|source_uid| ECalLocalSourceProperties {
                        source_uid,
                        display_name: c_string(raw.display_name).unwrap_or_default(),
                        color: c_string(raw.color).unwrap_or_default(),
                        custom_file_uri: c_string(raw.custom_file_uri)
                            .filter(|uri| !uri.trim().is_empty()),
                        custom_file_writable: raw.custom_file_writable != 0,
                        email_address: c_string(raw.email_address).unwrap_or_default(),
                        is_builtin: raw.is_builtin != 0,
                    })
                    .ok_or_else(|| {
                        ECalServiceError::Backend(
                            "EDS lieferte lokale Eigenschaften für eine andere oder ungültige Quelle"
                                .to_string(),
                        )
                    });
                ffi::evolution_eds_ecal_clear_local_source_properties(&mut raw);
                parsed
            } else {
                ffi::evolution_eds_ecal_clear_local_source_properties(&mut raw);
                Err(match result {
                    2 => ECalServiceError::Unsupported,
                    _ => ECalServiceError::Backend(error.unwrap_or_else(|| {
                        "EDS konnte die lokalen ECal-Eigenschaften nicht lesen".to_string()
                    })),
                })
            }
        };
        properties
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn update_local_source_properties_native(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        update: &ECalLocalSourcePropertiesUpdate,
    ) -> Result<ECalLocalSourcePropertiesUpdateResult, ECalServiceError> {
        use std::ffi::CString;

        let source_uid = CString::new(source_uid)
            .map_err(|_| ECalServiceError::Backend("Quell-UID enthält NUL".to_string()))?;
        let display_name = CString::new(update.display_name.as_str())
            .map_err(|_| ECalServiceError::Backend("Quellenname enthält NUL".to_string()))?;
        let color = CString::new(update.color.as_str())
            .map_err(|_| ECalServiceError::Backend("Quellenfarbe enthält NUL".to_string()))?;
        let custom_file_uri = update
            .custom_file_uri
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| ECalServiceError::Backend("Datei-URI enthält NUL".to_string()))?;
        let email_address = CString::new(update.email_address.as_str())
            .map_err(|_| ECalServiceError::Backend("E-Mail-Adresse enthält NUL".to_string()))?;
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out_warning = std::ptr::null_mut();
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_update_local_source_properties(
                source_uid.as_ptr(),
                component_type as i32,
                display_name.as_ptr(),
                color.as_ptr(),
                custom_file_uri
                    .as_ref()
                    .map_or(std::ptr::null(), |uri| uri.as_ptr()),
                i32::from(update.custom_file_writable),
                email_address.as_ptr(),
                &mut out_warning,
                &mut out_error,
            )
        };
        let warning = take_c_string(out_warning);
        let error = take_c_string(out_error);
        match result {
            1 => Ok(ECalLocalSourcePropertiesUpdateResult {
                source_uid: source_uid.to_string_lossy().into_owned(),
                status: ECalLocalSourceCreateStatus::Available,
            }),
            2 => Ok(ECalLocalSourcePropertiesUpdateResult {
                source_uid: source_uid.to_string_lossy().into_owned(),
                status: ECalLocalSourceCreateStatus::CreatedUnavailable(warning.unwrap_or_else(|| {
                    "Die lokalen ECal-Eigenschaften wurden gespeichert, aber die Quelle ist noch nicht erreichbar".to_string()
                })),
            }),
            3 => Err(ECalServiceError::Unsupported),
            _ => Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                "EDS konnte die lokalen ECal-Eigenschaften nicht speichern".to_string()
            }))),
        }
    }

    #[cfg(all(target_os = "linux", eds_ecal_bridge))]
    fn discover_source_capabilities_native(
        &self,
        component_type: ECalComponentType,
    ) -> Result<ECalSourceDiscovery, ECalServiceError> {
        let _guard = ECAL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sources = std::ptr::null_mut();
        let mut count = 0usize;
        let mut out_error = std::ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_list_source_capabilities(
                component_type as i32,
                &mut sources,
                &mut count,
                &mut out_error,
            )
        };
        let warning = error_string(out_error);
        if result == 0 {
            unsafe {
                ffi::evolution_eds_ecal_free_source_capabilities(sources, count);
            }
            return Err(ECalServiceError::Backend(warning.unwrap_or_else(|| {
                "EDS/ECal-Quellen konnten nicht gelesen werden".to_string()
            })));
        }
        let parsed = unsafe {
            if sources.is_null() {
                Ok(Vec::new())
            } else {
                std::slice::from_raw_parts(sources, count)
                    .iter()
                    .map(|source| {
                        let source_uid = c_string(source.source_uid).ok_or_else(|| {
                            ECalServiceError::Backend(
                                "EDS lieferte eine Quelle ohne gültige UID".to_string(),
                            )
                        })?;
                        if source_uid.trim().is_empty() {
                            return Err(ECalServiceError::Backend(
                                "EDS lieferte eine Quelle ohne gültige UID".to_string(),
                            ));
                        }
                        let (task_assignment_supported, task_assignment_conversion_supported) =
                            task_assignment_capabilities(
                                component_type,
                                source.connected != 0,
                                source.task_assignment_supported != 0,
                                source.task_assignment_conversion_supported != 0,
                            );
                        Ok(ECalSourceCapability {
                            source_uid,
                            display_name: c_string(source.source_display_name).unwrap_or_default(),
                            connected: source.connected != 0,
                            available: source.connected != 0,
                            source_writable: source.source_writable != 0,
                            writable: source.connected != 0 && source.writable != 0,
                            refresh_supported: source.connected != 0
                                && source.refresh_supported != 0,
                            removable: source.removable != 0,
                            remote_deletable: source.remote_deletable != 0,
                            in_collection: source.in_collection != 0,
                            task_assignment_supported,
                            task_assignment_conversion_supported,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            }
        };
        unsafe {
            ffi::evolution_eds_ecal_free_source_capabilities(sources, count);
        }
        let sources = parsed?;
        let mut identities = HashSet::new();
        if sources
            .iter()
            .any(|source| !identities.insert(source.source_uid.clone()))
        {
            return Err(ECalServiceError::Backend(
                "EDS lieferte doppelte Quell-UIDs".to_string(),
            ));
        }
        Ok(ECalSourceDiscovery { sources, warning })
    }

    #[cfg(not(all(target_os = "linux", eds_ecal_bridge)))]
    fn apply_operation(
        &self,
        _source_uid: &str,
        _component_type: ECalComponentType,
        _operation: i32,
        _component: &str,
        _component_uid: Option<&str>,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        Err(ECalMutationError::Unsupported)
    }
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
#[derive(Debug)]
struct ECalWatcher {
    raw: *mut std::ffi::c_void,
    status: ECalWatcherStatus,
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
unsafe impl Send for ECalWatcher {}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
impl ECalWatcher {
    fn new() -> Result<Self, ECalServiceError> {
        use std::ffi::c_void;
        use std::ptr;

        let mut raw: *mut c_void = ptr::null_mut();
        let mut configured_sources = 0;
        let mut active_sources = 0;
        let mut failed_sources = 0;
        let mut error = ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ecal_watcher_new(
                &mut raw,
                &mut configured_sources,
                &mut active_sources,
                &mut failed_sources,
                &mut error,
            )
        };
        let warning = c_string(error);
        if !error.is_null() {
            unsafe { ffi::evolution_eds_ecal_free_error(error) };
        }
        if result == 0 || raw.is_null() {
            return Err(ECalServiceError::Backend(warning.unwrap_or_else(|| {
                "EDS/ECal-Signalmonitor konnte nicht gestartet werden".to_string()
            })));
        }

        Ok(Self {
            raw,
            status: ECalWatcherStatus {
                configured_sources,
                active_sources,
                failed_sources,
                warning,
            },
        })
    }

    fn next_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ECalChangeEvent>, ECalServiceError> {
        let timeout_milliseconds = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut raw_event = ffi::EvolutionEdsECalWatcherEvent {
            source_type: 0,
            kind: 0,
            source_uid: std::ptr::null_mut(),
            source_display_name: std::ptr::null_mut(),
            uid: std::ptr::null_mut(),
            component: std::ptr::null_mut(),
            error: std::ptr::null_mut(),
        };
        let result = unsafe {
            ffi::evolution_eds_ecal_watcher_next_event(
                self.raw,
                timeout_milliseconds,
                &mut raw_event,
            )
        };
        if result < 0 {
            return Err(ECalServiceError::Backend(
                "EDS/ECal-Signalmonitor wurde beendet".to_string(),
            ));
        }
        if result == 0 {
            return Ok(None);
        }

        let source_uid = c_string(raw_event.source_uid).unwrap_or_default();
        let source_display_name = c_string(raw_event.source_display_name).unwrap_or_default();
        let uid = c_string(raw_event.uid).unwrap_or_default();
        let component = c_string(raw_event.component);
        let error = c_string(raw_event.error);
        let source_type = match raw_event.source_type {
            0 => ECalComponentType::Event,
            1 => ECalComponentType::Task,
            2 => ECalComponentType::Memo,
            _ => {
                unsafe {
                    ffi::evolution_eds_ecal_free_watcher_event(&mut raw_event);
                }
                return Err(ECalServiceError::Backend(
                    "EDS/ECal-Signalmonitor lieferte einen unbekannten Quellentyp".to_string(),
                ));
            }
        };
        let kind = match raw_event.kind {
            1 => ECalChangeKind::Added,
            2 => ECalChangeKind::Modified,
            3 => ECalChangeKind::Removed,
            4 => {
                unsafe {
                    ffi::evolution_eds_ecal_free_watcher_event(&mut raw_event);
                }
                return Err(ECalServiceError::Backend(error.unwrap_or_else(|| {
                    "EDS/ECal-Signalmonitor meldete einen Fehler".to_string()
                })));
            }
            _ => {
                unsafe {
                    ffi::evolution_eds_ecal_free_watcher_event(&mut raw_event);
                }
                return Err(ECalServiceError::Backend(
                    "EDS/ECal-Signalmonitor lieferte einen unbekannten Ereignistyp".to_string(),
                ));
            }
        };

        unsafe {
            ffi::evolution_eds_ecal_free_watcher_event(&mut raw_event);
        }
        Ok(Some(ECalChangeEvent {
            source_type,
            kind,
            source_uid,
            source_display_name,
            uid,
            component,
        }))
    }
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
impl Drop for ECalWatcher {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { ffi::evolution_eds_ecal_watcher_free(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

pub(crate) fn calendar_cache_path() -> std::path::PathBuf {
    crate::services::app_cache_dir().join("calendar_load_cache.json")
}

pub(crate) fn save_calendar_load_cache(snapshot: &CalendarSnapshot) {
    if let Err(error) = write_calendar_load_cache(&calendar_cache_path(), snapshot) {
        tracing::warn!(%error, "Could not update calendar preview cache");
    }
}

fn write_calendar_load_cache(
    path: &std::path::Path,
    snapshot: &CalendarSnapshot,
) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Missing cache directory"))?;
    std::fs::create_dir_all(parent)?;
    // Concurrent readers see either complete snapshot. Empty successful loads
    // must replace the old cache too, so deleted events cannot reappear at startup.
    let temporary = parent.join(format!(".calendar-cache-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer(&mut writer, snapshot).map_err(std::io::Error::other)?;
        writer.flush()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn load_calendar_load_cache() -> Option<CalendarSnapshot> {
    read_calendar_load_cache(&calendar_cache_path())
}

fn read_calendar_load_cache(path: &std::path::Path) -> Option<CalendarSnapshot> {
    let file = std::fs::File::open(path).ok()?;
    serde_json::from_reader(std::io::BufReader::new(file)).ok()
}

pub(crate) fn load_snapshot_prefer_eds() -> CalendarLoadResult {
    let ecal = ECalService::new();
    match ecal.load_snapshot() {
        Ok(mut snapshot) => {
            let local_service = CalendarStoreService::new();
            if local_service.is_writable() {
                let local = local_service.load_snapshot();
                snapshot.events.extend(local.events);
                snapshot.tasks.extend(local.tasks);
                snapshot.memos.extend(local.memos);
                #[cfg(all(target_os = "linux", eds_ecal_bridge))]
                sort_snapshot(&mut snapshot);
            }
            if snapshot.events.is_empty() && snapshot.tasks.is_empty() && snapshot.memos.is_empty()
            {
                if let Some(graph) = load_snapshot_from_graph() {
                    return graph;
                }
            }
            save_calendar_load_cache(&snapshot);
            CalendarLoadResult {
                snapshot,
                origin: CalendarDataOrigin::Eds,
                warning: None,
            }
        }
        Err(error) => {
            if let Some(graph) = load_snapshot_from_graph() {
                return graph;
            }
            if let Some(cached_snapshot) = load_calendar_load_cache() {
                if !cached_snapshot.events.is_empty()
                    || !cached_snapshot.tasks.is_empty()
                    || !cached_snapshot.memos.is_empty()
                {
                    return CalendarLoadResult {
                        snapshot: cached_snapshot,
                        origin: CalendarDataOrigin::Local,
                        warning: Some(error.to_string()),
                    };
                }
            }
            let local_service = super::calendar_store_service::CalendarStoreService::new();
            let mut snapshot = local_service.load_snapshot();
            #[cfg(all(target_os = "macos", macos_bridge))]
            {
                if snapshot.events.is_empty()
                    && snapshot.tasks.is_empty()
                    && snapshot.memos.is_empty()
                    && local_service.is_writable()
                {
                    let macos = super::macos_integration_service::MacosIntegrationService::new();
                    let _ = macos.sync_apple_calendar_to_store(&local_service);
                    let _ = macos.sync_apple_reminders_to_store(&local_service);
                    snapshot = local_service.load_snapshot();
                }
            }

            CalendarLoadResult {
                snapshot,
                origin: CalendarDataOrigin::Local,
                warning: Some(error.to_string()),
            }
        }
    }
}

fn load_snapshot_from_graph() -> Option<CalendarLoadResult> {
    let (source_uid, service) = graph_service_for_configured_account()?;
    let start = (Utc::now() - ChronoDuration::days(365))
        .format("%Y-%m-%dT00:00:00Z")
        .to_string();
    let end = (Utc::now() + ChronoDuration::days(730))
        .format("%Y-%m-%dT00:00:00Z")
        .to_string();
    let events = service
        .list_calendar_view_sync(&start, &end, 5_000)
        .map_err(|error| error.to_string())
        .ok()?;
    let source_display_name = if service.username.trim().is_empty() {
        "Microsoft 365 Kalender".to_string()
    } else {
        format!("Microsoft 365 ({})", service.username)
    };
    let mut snapshot = CalendarSnapshot::default();
    snapshot.events = events
        .iter()
        .filter_map(|event| event.into_calendar_event(&source_uid, &source_display_name))
        .collect();
    let local_service = CalendarStoreService::new();
    if local_service.is_writable() {
        let local = local_service.load_snapshot();
        snapshot.events.extend(local.events);
        snapshot.tasks.extend(local.tasks);
        snapshot.memos.extend(local.memos);
    }
    super::calendar_store_service::sort_snapshot(&mut snapshot);
    save_calendar_load_cache(&snapshot);
    Some(CalendarLoadResult {
        snapshot,
        origin: CalendarDataOrigin::Graph,
        warning: None,
    })
}

#[cfg(all(test, not(all(target_os = "linux", eds_ecal_bridge))))]
mod tests {
    use super::*;

    #[test]
    fn mutations_fail_explicitly_when_the_native_bridge_is_unavailable() {
        let result = ECalService::new().create_component(
            "source",
            ECalComponentType::Event,
            "BEGIN:VEVENT\r\nEND:VEVENT\r\n",
        );
        assert!(matches!(result, Err(ECalMutationError::Unsupported)));
    }

    #[test]
    fn remote_refresh_fails_explicitly_without_the_native_bridge() {
        let result = ECalService::new().refresh_source("remote-source", ECalComponentType::Task);
        assert!(matches!(result, Err(ECalServiceError::Unsupported)));
    }

    #[test]
    fn refresh_rejects_empty_source_uid_before_provider_dispatch() {
        let result = ECalService::new().refresh_source("  ", ECalComponentType::Task);
        assert!(matches!(result, Err(ECalServiceError::Backend(_))));
    }

    #[test]
    fn source_capability_discovery_is_explicitly_unsupported_without_eds() {
        let result = ECalService::new().discover_source_capabilities(ECalComponentType::Task);
        assert!(matches!(result, Err(ECalServiceError::Unsupported)));
    }

    #[test]
    fn source_management_rejects_invalid_inputs_before_provider_dispatch() {
        let service = ECalService::new();
        assert!(matches!(
            service.rename_source(" ", ECalComponentType::Task, "Tasks"),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.rename_source("tasks", ECalComponentType::Task, " \t"),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.delete_source(" ", ECalComponentType::Task),
            Err(ECalServiceError::Backend(_))
        ));
    }

    #[test]
    fn source_management_is_explicitly_unsupported_without_eds() {
        let service = ECalService::new();
        assert!(matches!(
            service.rename_source("tasks", ECalComponentType::Task, "Renamed"),
            Err(ECalServiceError::Unsupported)
        ));
        assert!(matches!(
            service.delete_source("tasks", ECalComponentType::Task),
            Err(ECalServiceError::Unsupported)
        ));
        assert!(matches!(
            service.rename_source("local-calendar", ECalComponentType::Task, "Renamed"),
            Err(ECalServiceError::Unsupported)
        ));
    }

    #[test]
    fn source_copy_rejects_unsafe_identities_before_provider_dispatch() {
        let service = ECalService::new();
        assert!(matches!(
            service.copy_source(" ", "destination", ECalComponentType::Task),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.copy_source("source", "source", ECalComponentType::Task),
            Err(ECalServiceError::Backend(_))
        ));
    }

    #[test]
    fn source_copy_is_explicitly_unsupported_without_eds() {
        let service = ECalService::new();
        assert!(matches!(
            service.copy_source("source", "destination", ECalComponentType::Task),
            Err(ECalServiceError::Unsupported)
        ));
        assert!(matches!(
            service.copy_source("local-calendar", "destination", ECalComponentType::Task),
            Err(ECalServiceError::Unsupported)
        ));
    }

    #[test]
    fn local_source_creation_validates_name_and_exact_hex_color() {
        assert!(is_valid_ical_source_color("#1aB2c3"));
        assert!(!is_valid_ical_source_color("1aB2c3"));
        assert!(!is_valid_ical_source_color("#1aB2c"));
        assert!(!is_valid_ical_source_color("#1aB2c3d"));
        assert!(!is_valid_ical_source_color("#1aB2xz"));

        let service = ECalService::new();
        assert!(matches!(
            service.create_local_source("  ", "#1aB2c3", ECalComponentType::Task),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.create_local_source("Tasks", "#12345", ECalComponentType::Task),
            Err(ECalServiceError::Backend(_))
        ));
    }

    #[test]
    fn task_assignment_capabilities_fail_closed_without_an_available_task_client() {
        assert_eq!(
            task_assignment_capabilities(ECalComponentType::Task, true, true, true),
            (true, true)
        );
        assert_eq!(
            task_assignment_capabilities(ECalComponentType::Task, true, true, false),
            (true, false)
        );
        assert_eq!(
            task_assignment_capabilities(ECalComponentType::Task, true, false, true),
            (false, false)
        );
        assert_eq!(
            task_assignment_capabilities(ECalComponentType::Task, false, true, true),
            (false, false)
        );
        assert_eq!(
            task_assignment_capabilities(ECalComponentType::Event, true, true, true),
            (false, false)
        );
        assert_eq!(
            task_assignment_capabilities(ECalComponentType::Memo, true, true, true),
            (false, false)
        );
    }

    #[test]
    fn local_source_creation_is_explicitly_unsupported_without_eds() {
        let result =
            ECalService::new().create_local_source("Tasks", "#1aB2c3", ECalComponentType::Task);
        assert!(matches!(result, Err(ECalServiceError::Unsupported)));
    }

    #[test]
    fn local_source_properties_validate_editable_fields_before_provider_dispatch() {
        let service = ECalService::new();
        let valid = ECalLocalSourcePropertiesUpdate {
            display_name: "Local Tasks".to_string(),
            color: "#1aB2c3".to_string(),
            custom_file_uri: None,
            custom_file_writable: true,
            email_address: String::new(),
        };
        assert!(matches!(
            service.local_source_properties(" ", ECalComponentType::Task),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.update_local_source_properties(
                "tasks",
                ECalComponentType::Task,
                &ECalLocalSourcePropertiesUpdate {
                    display_name: " ".to_string(),
                    ..valid.clone()
                }
            ),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.update_local_source_properties(
                "tasks",
                ECalComponentType::Task,
                &ECalLocalSourcePropertiesUpdate {
                    color: "#wrong".to_string(),
                    ..valid.clone()
                }
            ),
            Err(ECalServiceError::Backend(_))
        ));
        assert!(matches!(
            service.update_local_source_properties(
                "tasks",
                ECalComponentType::Task,
                &ECalLocalSourcePropertiesUpdate {
                    custom_file_uri: Some("  ".to_string()),
                    ..valid
                }
            ),
            Err(ECalServiceError::Backend(_))
        ));
    }

    #[test]
    fn local_source_properties_are_explicitly_unsupported_without_eds() {
        let service = ECalService::new();
        let update = ECalLocalSourcePropertiesUpdate {
            display_name: "Local Tasks".to_string(),
            color: "#1aB2c3".to_string(),
            custom_file_uri: Some("file:///tmp/tasks.ics".to_string()),
            custom_file_writable: true,
            email_address: "tasks@example.invalid".to_string(),
        };
        assert!(matches!(
            service.local_source_properties("tasks", ECalComponentType::Task),
            Err(ECalServiceError::Unsupported)
        ));
        assert!(matches!(
            service.update_local_source_properties("tasks", ECalComponentType::Task, &update),
            Err(ECalServiceError::Unsupported)
        ));
        assert!(matches!(
            service.local_source_properties("local-calendar", ECalComponentType::Task),
            Err(ECalServiceError::Unsupported)
        ));
    }
}

#[cfg(test)]
mod change_tests {
    use super::*;

    #[test]
    fn change_components_are_parsed_into_the_matching_record_type() {
        let event = ECalChangeEvent {
            source_type: ECalComponentType::Task,
            kind: ECalChangeKind::Modified,
            source_uid: "tasks".to_string(),
            source_display_name: "Meine Aufgaben".to_string(),
            uid: "task-1".to_string(),
            component: Some(
                "BEGIN:VTODO\r\nUID:task-1\r\nSUMMARY:Bericht\r\n".to_string()
                    + "DUE;VALUE=DATE:20260812\r\nEND:VTODO\r\n",
            ),
        };

        let Some(ECalRecord::Task(task)) = parse_change_record(&event) else {
            panic!("the VTODO change should parse as a task");
        };
        assert_eq!(task.uid, "task-1");
        assert_eq!(task.source_uid, "tasks");
        assert_eq!(task.due, "2026-08-12");
    }

    #[test]
    fn removed_change_does_not_require_a_component_payload() {
        let event = ECalChangeEvent {
            source_type: ECalComponentType::Memo,
            kind: ECalChangeKind::Removed,
            source_uid: "memos".to_string(),
            source_display_name: "Notizen".to_string(),
            uid: "memo-1".to_string(),
            component: None,
        };

        assert!(parse_change_record(&event).is_none());
    }
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub(super) struct EvolutionEdsECalComponent {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) component: *mut c_char,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsECalSourceCapability {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) connected: c_int,
        pub(super) source_writable: c_int,
        pub(super) writable: c_int,
        pub(super) refresh_supported: c_int,
        pub(super) removable: c_int,
        pub(super) remote_deletable: c_int,
        pub(super) in_collection: c_int,
        pub(super) task_assignment_supported: c_int,
        pub(super) task_assignment_conversion_supported: c_int,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsECalLocalSourceProperties {
        pub(super) source_uid: *mut c_char,
        pub(super) display_name: *mut c_char,
        pub(super) color: *mut c_char,
        pub(super) custom_file_uri: *mut c_char,
        pub(super) custom_file_writable: c_int,
        pub(super) email_address: *mut c_char,
        pub(super) is_builtin: c_int,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsECalWatcherEvent {
        pub(super) source_type: c_int,
        pub(super) kind: c_int,
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) uid: *mut c_char,
        pub(super) component: *mut c_char,
        pub(super) error: *mut c_char,
    }

    unsafe extern "C" {
        pub(super) fn evolution_eds_ecal_load_components(
            source_type: c_int,
            out_components: *mut *mut EvolutionEdsECalComponent,
            out_count: *mut usize,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_free_components(
            components: *mut EvolutionEdsECalComponent,
            count: usize,
        );

        pub(super) fn evolution_eds_ecal_apply_operation(
            source_uid: *const c_char,
            source_type: c_int,
            operation: c_int,
            component_string: *const c_char,
            component_uid: *const c_char,
            out_uid: *mut *mut c_char,
            out_component: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_refresh_source(
            source_uid: *const c_char,
            source_type: c_int,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_list_source_capabilities(
            source_type: c_int,
            out_sources: *mut *mut EvolutionEdsECalSourceCapability,
            out_count: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_free_source_capabilities(
            sources: *mut EvolutionEdsECalSourceCapability,
            count: usize,
        );

        pub(super) fn evolution_eds_ecal_rename_source(
            source_uid: *const c_char,
            source_type: c_int,
            display_name: *const c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_delete_source(
            source_uid: *const c_char,
            source_type: c_int,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_copy_source(
            from_source_uid: *const c_char,
            to_source_uid: *const c_char,
            source_type: c_int,
            out_copied_count: *mut usize,
            out_updated_count: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_create_local_source(
            display_name: *const c_char,
            color: *const c_char,
            source_type: c_int,
            out_source_uid: *mut *mut c_char,
            out_warning: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_get_local_source_properties(
            source_uid: *const c_char,
            source_type: c_int,
            out_properties: *mut EvolutionEdsECalLocalSourceProperties,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_clear_local_source_properties(
            properties: *mut EvolutionEdsECalLocalSourceProperties,
        );

        pub(super) fn evolution_eds_ecal_update_local_source_properties(
            source_uid: *const c_char,
            source_type: c_int,
            display_name: *const c_char,
            color: *const c_char,
            custom_file_uri: *const c_char,
            custom_file_writable: c_int,
            email_address: *const c_char,
            out_warning: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_free_error(error_message: *mut c_char);

        pub(super) fn evolution_eds_ecal_watcher_new(
            out_watcher: *mut *mut c_void,
            out_configured_sources: *mut usize,
            out_active_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_watcher_next_event(
            watcher: *mut c_void,
            timeout_milliseconds: c_int,
            out_event: *mut EvolutionEdsECalWatcherEvent,
        ) -> c_int;

        pub(super) fn evolution_eds_ecal_free_watcher_event(
            event: *mut EvolutionEdsECalWatcherEvent,
        );

        pub(super) fn evolution_eds_ecal_watcher_free(watcher: *mut c_void);
    }
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
#[derive(Debug, Default)]
struct NativeTypeLoad {
    warning: Option<String>,
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
static ECAL_NATIVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
fn load_snapshot_native() -> Result<CalendarSnapshot, ECalServiceError> {
    let _guard = ECAL_NATIVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshot = CalendarSnapshot::default();
    let mut warnings = Vec::new();
    let mut native_successes = 0;

    for (source_type, label) in [(0, "Kalender"), (1, "Aufgabenlisten"), (2, "Notizlisten")] {
        match load_components_native(source_type, &mut snapshot) {
            Ok(result) => {
                native_successes += 1;
                if let Some(warning) = result.warning {
                    warnings.push(format!("{label}: {warning}"));
                }
            }
            Err(error) => warnings.push(format!("{label}: {error}")),
        }
    }

    /* A registry failure affects every ECal source type.  An empty source
     * type, on the other hand, is a valid and common state and is represented
     * by a successful native call with zero components. */
    if native_successes == 0
        || (!warnings.is_empty()
            && snapshot.events.is_empty()
            && snapshot.tasks.is_empty()
            && snapshot.memos.is_empty())
    {
        return Err(ECalServiceError::Backend(
            warnings
                .first()
                .cloned()
                .unwrap_or_else(|| "EDS/ECal konnte nicht erreicht werden".to_string()),
        ));
    }

    sort_snapshot(&mut snapshot);
    if !warnings.is_empty() {
        tracing::warn!(warning = %warnings.join("; "), "ECal loaded with source warnings");
    }
    Ok(snapshot)
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
fn load_components_native(
    source_type: i32,
    snapshot: &mut CalendarSnapshot,
) -> Result<NativeTypeLoad, ECalServiceError> {
    let mut components = std::ptr::null_mut();
    let mut count = 0;
    let mut configured_sources = 0;
    let mut successful_sources = 0;
    let mut failed_sources = 0;
    let mut error = std::ptr::null_mut();

    let result = unsafe {
        ffi::evolution_eds_ecal_load_components(
            source_type,
            &mut components,
            &mut count,
            &mut configured_sources,
            &mut successful_sources,
            &mut failed_sources,
            &mut error,
        )
    };

    let warning = error_string(error);
    unsafe {
        if !components.is_null() {
            let entries = std::slice::from_raw_parts(components, count);
            for entry in entries {
                let Some(component) = c_string(entry.component) else {
                    continue;
                };
                let source_uid = c_string(entry.source_uid).unwrap_or_default();
                let source_display_name = c_string(entry.source_display_name).unwrap_or_default();
                parse_ical_component(&component, &source_uid, &source_display_name, snapshot);
            }
            ffi::evolution_eds_ecal_free_components(components, count);
        }
    }

    if result == 0 {
        return Err(ECalServiceError::Backend(warning.unwrap_or_else(|| {
            "EDS/ECal-Ladevorgang fehlgeschlagen".to_string()
        })));
    }

    if configured_sources > 0 && successful_sources == 0 && failed_sources > 0 {
        return Ok(NativeTypeLoad {
            warning: Some(
                warning.unwrap_or_else(|| "Keine ECal-Quelle konnte geöffnet werden".to_string()),
            ),
        });
    }

    Ok(NativeTypeLoad { warning })
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
fn c_string(pointer: *const std::ffi::c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    unsafe {
        std::ffi::CStr::from_ptr(pointer)
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
fn error_string(pointer: *mut std::ffi::c_char) -> Option<String> {
    let value = c_string(pointer);
    if !pointer.is_null() {
        unsafe {
            ffi::evolution_eds_ecal_free_error(pointer);
        }
    }
    value
}

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
fn take_c_string(pointer: *mut std::ffi::c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let value = unsafe {
        std::ffi::CStr::from_ptr(pointer)
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    };
    unsafe {
        ffi::evolution_eds_ecal_free_error(pointer);
    }
    value
}

#[cfg(test)]
mod calendar_preview_cache_tests {
    use super::*;

    fn test_directory() -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("evolution-calendar-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn empty_authoritative_snapshot_replaces_previous_cache() {
        let directory = test_directory();
        let path = directory.join("cache.json");
        let mut previous = CalendarSnapshot::default();
        super::super::calendar_store_service::parse_ical_component(
            "BEGIN:VEVENT\r\nUID:deleted-event\r\nDTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\nSUMMARY:Previous cached event\r\nEND:VEVENT\r\n",
            "calendar-source",
            "Calendar",
            &mut previous,
        );
        assert_eq!(previous.events.len(), 1);
        write_calendar_load_cache(&path, &previous).unwrap();
        assert_eq!(read_calendar_load_cache(&path).unwrap().events.len(), 1);
        write_calendar_load_cache(&path, &CalendarSnapshot::default()).unwrap();
        let snapshot = read_calendar_load_cache(&path).unwrap();
        assert!(snapshot.events.is_empty());
        assert!(snapshot.tasks.is_empty());
        assert!(snapshot.memos.is_empty());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_or_interrupted_cache_is_only_a_cache_miss() {
        let directory = test_directory();
        let path = directory.join("cache.json");
        assert!(read_calendar_load_cache(&path).is_none());
        std::fs::write(&path, b"{\"events\": [").unwrap();
        assert!(read_calendar_load_cache(&path).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_cache_commit_cleans_temporary_file() {
        let directory = test_directory();
        let path = directory.join("cache.json");
        std::fs::create_dir(&path).unwrap();
        assert!(write_calendar_load_cache(&path, &CalendarSnapshot::default()).is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
