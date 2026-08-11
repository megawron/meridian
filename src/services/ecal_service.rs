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
#[cfg(all(target_os = "linux", eds_ecal_bridge))]
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalendarDataOrigin {
    Eds,
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
            Self::Unsupported => write!(formatter, "native ECal bridge is unavailable"),
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

#[derive(Debug)]
pub(crate) enum ECalMutationError {
    Unsupported,
    Conflict(String),
    Backend(String),
}

impl std::fmt::Display for ECalMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "native ECal bridge is unavailable"),
            Self::Conflict(message) => write!(formatter, "ECal conflict: {message}"),
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

    pub(crate) fn create_component(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        component: &str,
    ) -> Result<ECalMutationResult, ECalMutationError> {
        if source_uid == "local-calendar" {
            return self.apply_local_operation(component_type, 1, component, None);
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
        if source_uid == "local-calendar" {
            return self.apply_local_operation(component_type, 2, component, Some(component_uid));
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
        if source_uid == "local-calendar" {
            return self.apply_local_operation(component_type, 3, "", Some(component_uid));
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
            CalendarLoadResult {
                snapshot,
                origin: CalendarDataOrigin::Eds,
                warning: None,
            }
        }
        Err(error) => {
            let snapshot =
                super::calendar_store_service::CalendarStoreService::new().load_snapshot();
            CalendarLoadResult {
                snapshot,
                origin: CalendarDataOrigin::Local,
                warning: Some(error.to_string()),
            }
        }
    }
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
fn load_snapshot_native() -> Result<CalendarSnapshot, ECalServiceError> {
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
    if native_successes == 0 {
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

    let warning = unsafe { error_string(error) };
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
