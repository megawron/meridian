// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native EBook adapter for synchronized EDS address books.  Local vCards are
// used only as an explicit fallback when the native Linux bridge is absent.

use super::contact_store_service::ContactStoreService;
use super::vcard_service::VCardContact;
#[cfg(all(target_os = "linux", eds_ebook_bridge))]
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EBookContactRecord {
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) uid: String,
    pub(crate) vcard: String,
    pub(crate) contact: VCardContact,
}

#[derive(Debug, Clone)]
pub(crate) struct EBookLoadResult {
    pub(crate) contacts: Vec<EBookContactRecord>,
    pub(crate) origin: EBookDataOrigin,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EBookDataOrigin {
    Eds,
    Local,
}

#[derive(Debug)]
pub(crate) enum EBookServiceError {
    Unsupported,
    Backend(String),
}

impl std::fmt::Display for EBookServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "native EBook bridge is unavailable"),
            Self::Backend(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for EBookServiceError {}

#[derive(Debug)]
pub(crate) enum EBookMutationError {
    Unsupported,
    Conflict(String),
    Backend(String),
}

impl std::fmt::Display for EBookMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "native EBook bridge is unavailable"),
            Self::Conflict(message) => write!(formatter, "EBook conflict: {message}"),
            Self::Backend(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for EBookMutationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EBookMutationResult {
    pub(crate) uid: String,
    pub(crate) vcard: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EBookChangeKind {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EBookChangeEvent {
    pub(crate) kind: EBookChangeKind,
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) uid: String,
    pub(crate) vcard: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EBookWatcherStatus {
    pub(crate) configured_sources: usize,
    pub(crate) active_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) warning: Option<String>,
}

#[derive(Debug)]
pub(crate) enum EBookSyncEvent {
    Ready(EBookWatcherStatus),
    Change(EBookChangeEvent),
    Error(String),
}

pub(crate) fn parse_change_record(event: &EBookChangeEvent) -> Option<EBookContactRecord> {
    let vcard = event.vcard.as_deref()?;
    let contact = VCardContact::parse_vcard_string(vcard)?;
    Some(EBookContactRecord {
        source_uid: event.source_uid.clone(),
        source_display_name: event.source_display_name.clone(),
        uid: event.uid.clone(),
        vcard: vcard.to_string(),
        contact,
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct EBookService;

impl EBookService {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn load_contacts(
        &self,
    ) -> Result<(Vec<EBookContactRecord>, Option<String>), EBookServiceError> {
        #[cfg(all(target_os = "linux", eds_ebook_bridge))]
        {
            return load_contacts_native();
        }

        #[cfg(not(all(target_os = "linux", eds_ebook_bridge)))]
        {
            Err(EBookServiceError::Unsupported)
        }
    }

    pub(crate) fn create_contact(
        &self,
        source_uid: &str,
        vcard: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        if source_uid == "local-addressbook" {
            return self.apply_local_operation(1, vcard, None);
        }
        self.apply_operation(source_uid, 1, vcard, None)
    }

    pub(crate) fn modify_contact(
        &self,
        source_uid: &str,
        contact_uid: &str,
        vcard: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        if source_uid == "local-addressbook" {
            return self.apply_local_operation(2, vcard, Some(contact_uid));
        }
        self.apply_operation(source_uid, 2, vcard, Some(contact_uid))
    }

    pub(crate) fn remove_contact(
        &self,
        source_uid: &str,
        contact_uid: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        if source_uid == "local-addressbook" {
            return self.apply_local_operation(3, "", Some(contact_uid));
        }
        self.apply_operation(source_uid, 3, "", Some(contact_uid))
    }

    pub(crate) fn start_change_monitor(
        &self,
    ) -> Option<(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        async_channel::Receiver<EBookSyncEvent>,
    )> {
        #[cfg(all(target_os = "linux", eds_ebook_bridge))]
        {
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, Ordering};

            let (sender, receiver) = async_channel::bounded(32);
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancellation = cancelled.clone();

            std::thread::spawn(move || {
                let watcher = match EBookWatcher::new() {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        let _ = sender.try_send(EBookSyncEvent::Error(error.to_string()));
                        return;
                    }
                };

                let _ = sender.try_send(EBookSyncEvent::Ready(watcher.status.clone()));
                let mut watcher = watcher;
                while !cancellation.load(Ordering::Relaxed) {
                    match watcher.next_event(Duration::from_millis(1000)) {
                        Ok(Some(event)) => {
                            if sender.try_send(EBookSyncEvent::Change(event)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = sender.try_send(EBookSyncEvent::Error(error.to_string()));
                            break;
                        }
                    }
                }
            });

            Some((cancelled, receiver))
        }

        #[cfg(not(all(target_os = "linux", eds_ebook_bridge)))]
        {
            None
        }
    }

    fn apply_local_operation(
        &self,
        operation: i32,
        vcard: &str,
        contact_uid: Option<&str>,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        let service = ContactStoreService::new();
        let result = match operation {
            1 => service.create_contact(vcard),
            2 => service.modify_contact(contact_uid.unwrap_or_default(), vcard),
            3 => service
                .remove_contact(contact_uid.unwrap_or_default())
                .map(|uid| (uid, String::new())),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Unbekannte lokale EBook-Operation",
            )),
        };
        result
            .map(|(uid, vcard)| EBookMutationResult {
                uid,
                vcard: if operation == 3 { None } else { Some(vcard) },
            })
            .map_err(|error| EBookMutationError::Backend(error.to_string()))
    }

    #[cfg(all(target_os = "linux", eds_ebook_bridge))]
    fn apply_operation(
        &self,
        source_uid: &str,
        operation: i32,
        vcard: &str,
        contact_uid: Option<&str>,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        use std::ffi::CString;
        use std::ptr;

        let source_uid = CString::new(source_uid)
            .map_err(|_| EBookMutationError::Backend("Quell-UID enthält NUL".to_string()))?;
        let vcard = CString::new(vcard)
            .map_err(|_| EBookMutationError::Backend("vCard enthält NUL".to_string()))?;
        let contact_uid = contact_uid
            .map(CString::new)
            .transpose()
            .map_err(|_| EBookMutationError::Backend("Kontakt-UID enthält NUL".to_string()))?;
        let contact_uid_pointer = contact_uid
            .as_ref()
            .map_or(ptr::null(), |value| value.as_ptr());
        let mut out_uid = ptr::null_mut();
        let mut out_vcard = ptr::null_mut();
        let mut out_error = ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ebook_apply_operation(
                source_uid.as_ptr(),
                operation,
                vcard.as_ptr(),
                contact_uid_pointer,
                &mut out_uid,
                &mut out_vcard,
                &mut out_error,
            )
        };
        let uid = take_c_string(out_uid);
        let confirmed_vcard = take_c_string(out_vcard);
        let error = take_c_string(out_error);

        match result {
            1 => Ok(EBookMutationResult {
                uid: uid.unwrap_or_default(),
                vcard: confirmed_vcard,
            }),
            2 => {
                Err(EBookMutationError::Conflict(error.unwrap_or_else(|| {
                    "Backend meldet einen Konflikt".to_string()
                })))
            }
            _ => {
                Err(EBookMutationError::Backend(error.unwrap_or_else(|| {
                    "EBook-Mutation fehlgeschlagen".to_string()
                })))
            }
        }
    }

    #[cfg(not(all(target_os = "linux", eds_ebook_bridge)))]
    fn apply_operation(
        &self,
        _source_uid: &str,
        _operation: i32,
        _vcard: &str,
        _contact_uid: Option<&str>,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        Err(EBookMutationError::Unsupported)
    }
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
#[derive(Debug)]
struct EBookWatcher {
    raw: *mut std::ffi::c_void,
    status: EBookWatcherStatus,
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
unsafe impl Send for EBookWatcher {}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
impl EBookWatcher {
    fn new() -> Result<Self, EBookServiceError> {
        use std::ffi::c_void;
        use std::ptr;

        let mut raw: *mut c_void = ptr::null_mut();
        let mut configured_sources = 0;
        let mut active_sources = 0;
        let mut failed_sources = 0;
        let mut error = ptr::null_mut();
        let result = unsafe {
            ffi::evolution_eds_ebook_watcher_new(
                &mut raw,
                &mut configured_sources,
                &mut active_sources,
                &mut failed_sources,
                &mut error,
            )
        };
        let warning = borrowed_c_string(error);
        if !error.is_null() {
            unsafe { ffi::evolution_eds_ebook_free_error(error) };
        }
        if result == 0 || raw.is_null() {
            return Err(EBookServiceError::Backend(warning.unwrap_or_else(|| {
                "EDS/EBook-Signalmonitor konnte nicht gestartet werden".to_string()
            })));
        }

        Ok(Self {
            raw,
            status: EBookWatcherStatus {
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
    ) -> Result<Option<EBookChangeEvent>, EBookServiceError> {
        let timeout_milliseconds = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut raw_event = ffi::EvolutionEdsEBookWatcherEvent {
            kind: 0,
            source_uid: std::ptr::null_mut(),
            source_display_name: std::ptr::null_mut(),
            contact_uid: std::ptr::null_mut(),
            vcard: std::ptr::null_mut(),
            error: std::ptr::null_mut(),
        };
        let result = unsafe {
            ffi::evolution_eds_ebook_watcher_next_event(
                self.raw,
                timeout_milliseconds,
                &mut raw_event,
            )
        };
        if result < 0 {
            return Err(EBookServiceError::Backend(
                "EDS/EBook-Signalmonitor wurde beendet".to_string(),
            ));
        }
        if result == 0 {
            return Ok(None);
        }

        let source_uid = borrowed_c_string(raw_event.source_uid).unwrap_or_default();
        let source_display_name =
            borrowed_c_string(raw_event.source_display_name).unwrap_or_default();
        let uid = borrowed_c_string(raw_event.contact_uid).unwrap_or_default();
        let vcard = borrowed_c_string(raw_event.vcard);
        let error = borrowed_c_string(raw_event.error);
        let kind = match raw_event.kind {
            1 => EBookChangeKind::Added,
            2 => EBookChangeKind::Modified,
            3 => EBookChangeKind::Removed,
            4 => {
                unsafe {
                    ffi::evolution_eds_ebook_free_watcher_event(&mut raw_event);
                }
                return Err(EBookServiceError::Backend(error.unwrap_or_else(|| {
                    "EDS/EBook-Signalmonitor meldete einen Fehler".to_string()
                })));
            }
            _ => {
                unsafe {
                    ffi::evolution_eds_ebook_free_watcher_event(&mut raw_event);
                }
                return Err(EBookServiceError::Backend(
                    "EDS/EBook-Signalmonitor lieferte einen unbekannten Ereignistyp".to_string(),
                ));
            }
        };

        unsafe {
            ffi::evolution_eds_ebook_free_watcher_event(&mut raw_event);
        }
        Ok(Some(EBookChangeEvent {
            kind,
            source_uid,
            source_display_name,
            uid,
            vcard,
        }))
    }
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
impl Drop for EBookWatcher {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { ffi::evolution_eds_ebook_watcher_free(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

pub(crate) fn load_contacts_prefer_eds() -> EBookLoadResult {
    match EBookService::new().load_contacts() {
        Ok((mut contacts, warning)) => {
            let local_service = ContactStoreService::new();
            if local_service.is_writable() {
                contacts.extend(local_service.load_contacts().into_iter().map(|record| {
                    EBookContactRecord {
                        source_uid: "local-addressbook".to_string(),
                        source_display_name: "Lokales Adressbuch".to_string(),
                        uid: record.id,
                        vcard: record.vcard,
                        contact: record.contact,
                    }
                }));
            }
            EBookLoadResult {
                contacts,
                origin: EBookDataOrigin::Eds,
                warning,
            }
        }
        Err(error) => {
            let local_service = ContactStoreService::new();
            let local_source_uid = if local_service.is_writable() {
                "local-addressbook"
            } else {
                ""
            };
            let local_source_name = if local_service.is_writable() {
                "Lokales Adressbuch"
            } else {
                ""
            };
            let contacts = local_service
                .load_contacts()
                .into_iter()
                .map(|record| EBookContactRecord {
                    source_uid: local_source_uid.to_string(),
                    source_display_name: local_source_name.to_string(),
                    uid: record.id,
                    vcard: record.vcard,
                    contact: record.contact,
                })
                .collect();
            EBookLoadResult {
                contacts,
                origin: EBookDataOrigin::Local,
                warning: Some(error.to_string()),
            }
        }
    }
}

#[cfg(all(test, not(all(target_os = "linux", eds_ebook_bridge))))]
mod tests {
    use super::*;

    #[test]
    fn mutations_fail_explicitly_without_the_native_bridge() {
        let result = EBookService::new().create_contact(
            "source",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Test\r\nEND:VCARD\r\n",
        );
        assert!(matches!(result, Err(EBookMutationError::Unsupported)));
    }
}

#[cfg(test)]
mod change_tests {
    use super::*;

    #[test]
    fn change_vcards_are_parsed_with_source_identity() {
        let event = EBookChangeEvent {
            kind: EBookChangeKind::Modified,
            source_uid: "address-book".to_string(),
            source_display_name: "Privat".to_string(),
            uid: "contact-1".to_string(),
            vcard: Some(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact-1\r\n".to_string()
                    + "FN:Max Mustermann\r\nEMAIL:max@example.test\r\nEND:VCARD\r\n",
            ),
        };

        let contact = parse_change_record(&event).expect("vCard should parse");
        assert_eq!(contact.uid, "contact-1");
        assert_eq!(contact.source_uid, "address-book");
        assert_eq!(contact.contact.fn_name, "Max Mustermann");
        assert_eq!(contact.contact.email, "max@example.test");
    }

    #[test]
    fn removed_change_does_not_require_a_vcard() {
        let event = EBookChangeEvent {
            kind: EBookChangeKind::Removed,
            source_uid: "address-book".to_string(),
            source_display_name: "Privat".to_string(),
            uid: "contact-1".to_string(),
            vcard: None,
        };

        assert!(parse_change_record(&event).is_none());
    }
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub(super) struct EvolutionEdsEBookContact {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) contact_uid: *mut c_char,
        pub(super) vcard: *mut c_char,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsEBookWatcherEvent {
        pub(super) kind: c_int,
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) contact_uid: *mut c_char,
        pub(super) vcard: *mut c_char,
        pub(super) error: *mut c_char,
    }

    unsafe extern "C" {
        pub(super) fn evolution_eds_ebook_load_contacts(
            out_contacts: *mut *mut EvolutionEdsEBookContact,
            out_count: *mut usize,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ebook_free_contacts(
            contacts: *mut EvolutionEdsEBookContact,
            count: usize,
        );

        pub(super) fn evolution_eds_ebook_apply_operation(
            source_uid: *const c_char,
            operation: c_int,
            vcard: *const c_char,
            contact_uid: *const c_char,
            out_uid: *mut *mut c_char,
            out_vcard: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ebook_free_error(error_message: *mut c_char);

        pub(super) fn evolution_eds_ebook_watcher_new(
            out_watcher: *mut *mut c_void,
            out_configured_sources: *mut usize,
            out_active_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn evolution_eds_ebook_watcher_next_event(
            watcher: *mut c_void,
            timeout_milliseconds: c_int,
            out_event: *mut EvolutionEdsEBookWatcherEvent,
        ) -> c_int;

        pub(super) fn evolution_eds_ebook_free_watcher_event(
            event: *mut EvolutionEdsEBookWatcherEvent,
        );

        pub(super) fn evolution_eds_ebook_watcher_free(watcher: *mut c_void);
    }
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
fn load_contacts_native() -> Result<(Vec<EBookContactRecord>, Option<String>), EBookServiceError> {
    use std::ptr;

    let mut contacts_pointer = ptr::null_mut();
    let mut count = 0;
    let mut configured_sources = 0;
    let mut successful_sources = 0;
    let mut failed_sources = 0;
    let mut error_pointer = ptr::null_mut();
    let result = unsafe {
        ffi::evolution_eds_ebook_load_contacts(
            &mut contacts_pointer,
            &mut count,
            &mut configured_sources,
            &mut successful_sources,
            &mut failed_sources,
            &mut error_pointer,
        )
    };
    let error = take_c_string(error_pointer);
    if result == 0 {
        return Err(EBookServiceError::Backend(error.unwrap_or_else(|| {
            "EBook-Daten konnten nicht geladen werden".to_string()
        })));
    }

    let mut contacts = Vec::with_capacity(count);
    if !contacts_pointer.is_null() {
        let native_contacts = unsafe { std::slice::from_raw_parts(contacts_pointer, count) };
        for contact in native_contacts {
            let Some(vcard) = borrowed_c_string(contact.vcard) else {
                continue;
            };
            let Some(parsed) = VCardContact::parse_vcard_string(&vcard) else {
                tracing::warn!("Ignoring an EDS contact with an invalid vCard");
                continue;
            };
            contacts.push(EBookContactRecord {
                source_uid: borrowed_c_string(contact.source_uid).unwrap_or_default(),
                source_display_name: borrowed_c_string(contact.source_display_name)
                    .unwrap_or_default(),
                uid: borrowed_c_string(contact.contact_uid).unwrap_or_default(),
                vcard,
                contact: parsed,
            });
        }
    }
    unsafe {
        ffi::evolution_eds_ebook_free_contacts(contacts_pointer, count);
    }

    let warning = error.or_else(|| {
        (failed_sources > 0).then(|| {
            format!(
                "{failed_sources} von {configured_sources} EDS-Adressbuchquelle(n) konnten nicht geladen werden"
            )
        })
    });
    let _ = successful_sources;
    Ok((contacts, warning))
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
fn borrowed_c_string(pointer: *mut std::ffi::c_char) -> Option<String> {
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

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
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
        ffi::evolution_eds_ebook_free_error(pointer);
    }
    value
}
