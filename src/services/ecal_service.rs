// SPDX-License-Identifier: GPL-3.0-or-later
//
// Direct ECal adapter for synchronized calendars, task lists and memo lists.
// EDS remains authoritative; the local iCalendar importer is only used when
// the native ECal bridge is unavailable (for example on non-Linux dev hosts).

use super::calendar_store_service::CalendarSnapshot;
#[cfg(all(target_os = "linux", eds_ecal_bridge))]
use super::calendar_store_service::{parse_ical_component, sort_snapshot};

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
        self.apply_operation(source_uid, component_type, 1, component, None)
    }

    pub(crate) fn modify_component(
        &self,
        source_uid: &str,
        component_type: ECalComponentType,
        component_uid: &str,
        component: &str,
    ) -> Result<ECalMutationResult, ECalMutationError> {
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
        self.apply_operation(source_uid, component_type, 3, "", Some(component_uid))
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

pub(crate) fn load_snapshot_prefer_eds() -> CalendarLoadResult {
    let ecal = ECalService::new();
    match ecal.load_snapshot() {
        Ok(snapshot) => CalendarLoadResult {
            snapshot,
            origin: CalendarDataOrigin::Eds,
            warning: None,
        },
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

#[cfg(all(target_os = "linux", eds_ecal_bridge))]
mod ffi {
    use std::ffi::{c_char, c_int};

    #[repr(C)]
    pub(super) struct EvolutionEdsECalComponent {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) component: *mut c_char,
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
