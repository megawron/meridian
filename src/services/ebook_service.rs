// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native EBook adapter for synchronized EDS address books.  Local vCards are
// used only as an explicit fallback when the native Linux bridge is absent.

use super::contact_store_service::ContactStoreService;
use super::exchange_service::graph_service_for_configured_account;
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
    Graph,
    Local,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum EBookServiceError {
    Unsupported,
    Backend(String),
}

impl std::fmt::Display for EBookServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "Address book service is unavailable"),
            Self::Backend(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for EBookServiceError {}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum EBookMutationError {
    Unsupported,
    Conflict(String),
    Backend(String),
}

impl std::fmt::Display for EBookMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "Address book service is unavailable"),
            Self::Conflict(message) => write!(formatter, "Address book conflict: {message}"),
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
        let result = self.create_contact_raw(source_uid, vcard);
        if let Ok(ref confirmed) = result {
            let confirmed_vcard = confirmed
                .vcard
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(vcard)
                .to_string();
            register_known_ebook_vcard(source_uid, &confirmed.uid, &confirmed_vcard);
            record_ebook_history_create(source_uid, &confirmed.uid, &confirmed_vcard);
        }
        result
    }

    fn create_contact_raw(
        &self,
        source_uid: &str,
        vcard: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        let result = if source_uid == "local-addressbook" || source_uid.starts_with("account-contacts-") {
            self.apply_local_operation(1, vcard, None)
        } else if let Some(result) = self.apply_graph_operation(source_uid, 1, vcard, None) {
            result
        } else {
            self.apply_operation(source_uid, 1, vcard, None)
        };
        if result.is_ok() {
            invalidate_contacts_cache();
        }
        result
    }

    pub(crate) fn modify_contact(
        &self,
        source_uid: &str,
        contact_uid: &str,
        vcard: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        let before_vcard = lookup_known_ebook_vcard(source_uid, contact_uid);
        let result = self.modify_contact_raw(source_uid, contact_uid, vcard);
        if let Ok(ref confirmed) = result {
            let confirmed_vcard = confirmed
                .vcard
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(vcard)
                .to_string();
            register_known_ebook_vcard(source_uid, &confirmed.uid, &confirmed_vcard);
            if let Some(before) = before_vcard {
                record_ebook_history_modify(
                    source_uid,
                    contact_uid,
                    &confirmed.uid,
                    &before,
                    &confirmed_vcard,
                );
            }
        }
        result
    }

    fn modify_contact_raw(
        &self,
        source_uid: &str,
        contact_uid: &str,
        vcard: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        let result = if source_uid == "local-addressbook" || source_uid.starts_with("account-contacts-") {
            self.apply_local_operation(2, vcard, Some(contact_uid))
        } else if let Some(result) = self.apply_graph_operation(source_uid, 2, vcard, Some(contact_uid)) {
            result
        } else {
            self.apply_operation(source_uid, 2, vcard, Some(contact_uid))
        };
        if result.is_ok() {
            invalidate_contacts_cache();
        }
        result
    }

    pub(crate) fn remove_contact(
        &self,
        source_uid: &str,
        contact_uid: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        let before_vcard = lookup_known_ebook_vcard(source_uid, contact_uid);
        let result = self.remove_contact_raw(source_uid, contact_uid);
        if result.is_ok() {
            remove_known_ebook_vcard(source_uid, contact_uid);
            if let Some(before) = before_vcard {
                record_ebook_history_remove(source_uid, contact_uid, &before);
            }
        }
        result
    }

    fn remove_contact_raw(
        &self,
        source_uid: &str,
        contact_uid: &str,
    ) -> Result<EBookMutationResult, EBookMutationError> {
        let result = if source_uid == "local-addressbook" || source_uid.starts_with("account-contacts-") {
            self.apply_local_operation(3, "", Some(contact_uid))
        } else if let Some(result) = self.apply_graph_operation(source_uid, 3, "", Some(contact_uid)) {
            result
        } else {
            self.apply_operation(source_uid, 3, "", Some(contact_uid))
        };
        if result.is_ok() {
            invalidate_contacts_cache();
        }
        result
    }

    pub(crate) fn undo(&self) -> Result<usize, EBookMutationError> {
        execute_ebook_history_action(self, true)
    }

    pub(crate) fn redo(&self) -> Result<usize, EBookMutationError> {
        execute_ebook_history_action(self, false)
    }

    /// Merge duplicate records into `primary` and then remove the duplicate
    /// records from the same address-book source.  The app-owned local store
    /// uses one atomic multi-card rewrite; EDS and Graph expose only separate
    /// modify/delete operations, so a deletion error is reported explicitly
    /// as a partial backend operation instead of being hidden.
    pub(crate) fn merge_contacts(
        &self,
        primary: &EBookContactRecord,
        duplicates: &[EBookContactRecord],
    ) -> Result<EBookMutationResult, EBookMutationError> {
        if duplicates.is_empty() {
            return Err(EBookMutationError::Backend(
                "Merging requires at least one duplicate".to_string(),
            ));
        }
        if primary.source_uid.trim().is_empty() {
            return Err(EBookMutationError::Backend(
                "Contact comes from a read-only imported source".to_string(),
            ));
        }
        if duplicates.iter().any(|duplicate| {
            duplicate.source_uid != primary.source_uid
                || duplicate.uid == primary.uid
                || duplicate.uid.trim().is_empty()
        }) {
            return Err(EBookMutationError::Backend(
                "Primary contact and duplicates must come from the same source".to_string(),
            ));
        }

        let duplicate_vcards = duplicates
            .iter()
            .map(|duplicate| duplicate.vcard.as_str())
            .collect::<Vec<_>>();
        let merged_vcard = VCardContact::merge_vcards(&primary.vcard, &duplicate_vcards)
            .map_err(EBookMutationError::Backend)?;

        register_known_ebook_vcard(&primary.source_uid, &primary.uid, &primary.vcard);
        for dup in duplicates {
            register_known_ebook_vcard(&dup.source_uid, &dup.uid, &dup.vcard);
        }

        let _batch = EBookBatchGuard::new();
        if primary.source_uid == "local-addressbook" || primary.source_uid.starts_with("account-contacts-") {
            let duplicate_uids = duplicates
                .iter()
                .map(|duplicate| duplicate.uid.clone())
                .collect::<Vec<_>>();
            let res = ContactStoreService::new()
                .merge_contacts(&primary.uid, &duplicate_uids, &merged_vcard)
                .map(|(uid, vcard)| EBookMutationResult {
                    uid,
                    vcard: Some(vcard),
                })
                .map_err(|error| EBookMutationError::Backend(error.to_string()));
            if let Ok(ref confirmed) = res {
                invalidate_contacts_cache();
                let confirmed_vcard = confirmed
                    .vcard
                    .as_deref()
                    .unwrap_or(&merged_vcard)
                    .to_string();
                register_known_ebook_vcard(&primary.source_uid, &confirmed.uid, &confirmed_vcard);
                record_ebook_history_modify(
                    &primary.source_uid,
                    &primary.uid,
                    &confirmed.uid,
                    &primary.vcard,
                    &confirmed_vcard,
                );
                for dup in duplicates {
                    remove_known_ebook_vcard(&dup.source_uid, &dup.uid);
                    record_ebook_history_remove(&dup.source_uid, &dup.uid, &dup.vcard);
                }
            }
            return res;
        }

        let updated = self.modify_contact(&primary.source_uid, &primary.uid, &merged_vcard)?;
        for duplicate in duplicates {
            if let Err(error) = self.remove_contact(&duplicate.source_uid, &duplicate.uid) {
                return Err(EBookMutationError::Backend(format!(
                    "Contact was updated, but duplicate {} could not be removed: {}",
                    duplicate.uid, error
                )));
            }
        }
        Ok(updated)
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
                "Unknown local EBook operation",
            )),
        };
        result
            .map(|(uid, vcard)| EBookMutationResult {
                uid,
                vcard: if operation == 3 { None } else { Some(vcard) },
            })
            .map_err(|error| EBookMutationError::Backend(error.to_string()))
    }

    fn apply_graph_operation(
        &self,
        source_uid: &str,
        operation: i32,
        vcard: &str,
        contact_uid: Option<&str>,
    ) -> Option<Result<EBookMutationResult, EBookMutationError>> {
        let service =
            graph_service_for_configured_account().and_then(|(configured_uid, service)| {
                (configured_uid == source_uid).then_some(service)
            })?;
        let contact = || {
            VCardContact::parse_vcard_string(vcard).ok_or_else(|| {
                EBookMutationError::Backend("vCard is invalid or missing an FN field".to_string())
            })
        };
        let result = match operation {
            1 => contact().and_then(|parsed| {
                service
                    .create_contact_sync(&super::exchange_service::ExchangeContact::from_vcard(
                        "", &parsed,
                    ))
                    .map_err(|error| EBookMutationError::Backend(error.to_string()))
                    .map(|created| EBookMutationResult {
                        uid: created.id.clone(),
                        vcard: Some(created.to_vcard()),
                    })
            }),
            2 => {
                let uid = contact_uid.unwrap_or_default();
                if uid.trim().is_empty() {
                    Err(EBookMutationError::Backend(
                        "Missing UID for modifying Graph contact".to_string(),
                    ))
                } else {
                    contact().and_then(|parsed| {
                        service
                            .update_contact_sync(
                                &super::exchange_service::ExchangeContact::from_vcard(uid, &parsed),
                            )
                            .map_err(|error| EBookMutationError::Backend(error.to_string()))
                            .map(|updated| EBookMutationResult {
                                uid: updated.id.clone(),
                                vcard: Some(updated.to_vcard()),
                            })
                    })
                }
            }
            3 => {
                let uid = contact_uid.unwrap_or_default();
                if uid.trim().is_empty() {
                    Err(EBookMutationError::Backend(
                        "Missing UID for deleting Graph contact".to_string(),
                    ))
                } else {
                    service
                        .delete_contact_sync(uid)
                        .map(|()| EBookMutationResult {
                            uid: uid.to_string(),
                            vcard: None,
                        })
                        .map_err(|error| EBookMutationError::Backend(error.to_string()))
                }
            }
            _ => Err(EBookMutationError::Backend(
                "Unknown Graph EBook operation".to_string(),
            )),
        };
        Some(result)
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
            .map_err(|_| EBookMutationError::Backend("Source UID contains NUL".to_string()))?;
        let vcard = CString::new(vcard)
            .map_err(|_| EBookMutationError::Backend("vCard contains NUL".to_string()))?;
        let contact_uid = contact_uid
            .map(CString::new)
            .transpose()
            .map_err(|_| EBookMutationError::Backend("Contact UID contains NUL".to_string()))?;
        let contact_uid_pointer = contact_uid
            .as_ref()
            .map_or(ptr::null(), |value| value.as_ptr());
        let mut out_uid = ptr::null_mut();
        let mut out_vcard = ptr::null_mut();
        let mut out_error = ptr::null_mut();
        let result = unsafe {
            ffi::meridian_eds_ebook_apply_operation(
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
                    "Backend reported a conflict".to_string()
                })))
            }
            _ => {
                Err(EBookMutationError::Backend(error.unwrap_or_else(|| {
                    "EBook mutation failed".to_string()
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
            ffi::meridian_eds_ebook_watcher_new(
                &mut raw,
                &mut configured_sources,
                &mut active_sources,
                &mut failed_sources,
                &mut error,
            )
        };
        let warning = borrowed_c_string(error);
        if !error.is_null() {
            unsafe { ffi::meridian_eds_ebook_free_error(error) };
        }
        if result == 0 || raw.is_null() {
            return Err(EBookServiceError::Backend(warning.unwrap_or_else(|| {
                "Could not start EDS/EBook signal monitor".to_string()
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
        let mut raw_event = ffi::MeridianEdsEBookWatcherEvent {
            kind: 0,
            source_uid: std::ptr::null_mut(),
            source_display_name: std::ptr::null_mut(),
            contact_uid: std::ptr::null_mut(),
            vcard: std::ptr::null_mut(),
            error: std::ptr::null_mut(),
        };
        let result = unsafe {
            ffi::meridian_eds_ebook_watcher_next_event(
                self.raw,
                timeout_milliseconds,
                &mut raw_event,
            )
        };
        if result < 0 {
            return Err(EBookServiceError::Backend(
                "EDS/EBook signal monitor terminated".to_string(),
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
                    ffi::meridian_eds_ebook_free_watcher_event(&mut raw_event);
                }
                return Err(EBookServiceError::Backend(error.unwrap_or_else(|| {
                    "EDS/EBook signal monitor reported an error".to_string()
                })));
            }
            _ => {
                unsafe {
                    ffi::meridian_eds_ebook_free_watcher_event(&mut raw_event);
                }
                return Err(EBookServiceError::Backend(
                    "EDS/EBook signal monitor returned an unknown event type".to_string(),
                ));
            }
        };

        unsafe {
            ffi::meridian_eds_ebook_free_watcher_event(&mut raw_event);
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
            unsafe { ffi::meridian_eds_ebook_watcher_free(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

static CONTACTS_CACHE: std::sync::RwLock<Option<(std::time::Instant, EBookLoadResult)>> =
    std::sync::RwLock::new(None);

const EBOOK_HISTORY_LIMIT: usize = 100;
static NEXT_EBOOK_BATCH_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

thread_local! {
    static EBOOK_ACTIVE_BATCH_ID: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static EBOOK_IN_HISTORY_REPLAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) struct EBookBatchGuard {
    prev_batch_id: Option<u64>,
}

impl EBookBatchGuard {
    pub(crate) fn new() -> Self {
        let batch_id = NEXT_EBOOK_BATCH_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let prev_batch_id = EBOOK_ACTIVE_BATCH_ID.with(|cell| {
            let prev = cell.get();
            cell.set(Some(batch_id));
            prev
        });
        Self { prev_batch_id }
    }
}

impl Drop for EBookBatchGuard {
    fn drop(&mut self) {
        EBOOK_ACTIVE_BATCH_ID.with(|cell| cell.set(self.prev_batch_id));
    }
}

struct EBookHistoryReplayGuard {
    prev: bool,
}

impl EBookHistoryReplayGuard {
    fn new() -> Self {
        let prev = EBOOK_IN_HISTORY_REPLAY.with(|cell| {
            let old = cell.get();
            cell.set(true);
            old
        });
        Self { prev }
    }
}

impl Drop for EBookHistoryReplayGuard {
    fn drop(&mut self) {
        EBOOK_IN_HISTORY_REPLAY.with(|cell| cell.set(self.prev));
    }
}

#[derive(Debug, Clone)]
enum EBookHistoryOp {
    Create {
        source_uid: String,
        uid: String,
        vcard: String,
    },
    Modify {
        source_uid: String,
        uid: String,
        vcard: String,
    },
    Remove {
        source_uid: String,
        uid: String,
    },
}

impl EBookHistoryOp {
    fn update_uid(&mut self, expected_source_uid: &str, old_uid: &str, new_uid: &str) {
        if old_uid == new_uid {
            return;
        }
        match self {
            Self::Create {
                source_uid,
                uid,
                vcard,
            }
            | Self::Modify {
                source_uid,
                uid,
                vcard,
            } => {
                if source_uid == expected_source_uid && uid == old_uid {
                    *uid = new_uid.to_string();
                    *vcard = replace_vcard_uid_line(vcard, new_uid);
                }
            }
            Self::Remove { source_uid, uid } => {
                if source_uid == expected_source_uid && uid == old_uid {
                    *uid = new_uid.to_string();
                }
            }
        }
    }
}

fn replace_vcard_uid_line(vcard: &str, new_uid: &str) -> String {
    let line_ending = if vcard.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines = Vec::new();
    let mut replaced = false;
    for line in vcard.lines() {
        let trimmed = line.trim_start();
        if trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("UID:") {
            lines.push(format!("UID:{new_uid}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        lines.push(format!("UID:{new_uid}"));
    }
    lines.join(line_ending)
}

#[derive(Debug, Clone)]
struct EBookHistoryCommand {
    forward: EBookHistoryOp,
    inverse: EBookHistoryOp,
    batch_id: Option<u64>,
}

#[derive(Debug, Default)]
struct EBookCommandStack {
    undo: Vec<EBookHistoryCommand>,
    redo: Vec<EBookHistoryCommand>,
}

impl EBookCommandStack {
    fn trim(&mut self) {
        if self.undo.len() > EBOOK_HISTORY_LIMIT {
            let excess = self.undo.len() - EBOOK_HISTORY_LIMIT;
            self.undo.drain(0..excess);
        }
        if self.redo.len() > EBOOK_HISTORY_LIMIT {
            let excess = self.redo.len() - EBOOK_HISTORY_LIMIT;
            self.redo.drain(0..excess);
        }
    }

    fn push_command(&mut self, command: EBookHistoryCommand) {
        self.undo.push(command);
        self.redo.clear();
        self.trim();
    }

    fn pop_batch(source_vec: &mut Vec<EBookHistoryCommand>) -> Vec<EBookHistoryCommand> {
        let Some(head_batch_id) = source_vec.last().map(|cmd| cmd.batch_id) else {
            return Vec::new();
        };
        let mut popped = Vec::new();
        if let Some(batch_id) = head_batch_id {
            while source_vec
                .last()
                .is_some_and(|cmd| cmd.batch_id == Some(batch_id))
            {
                if let Some(cmd) = source_vec.pop() {
                    popped.push(cmd);
                }
            }
            popped.reverse();
        } else if let Some(cmd) = source_vec.pop() {
            popped.push(cmd);
        }
        popped
    }

    fn pop_undo_batch(&mut self) -> Vec<EBookHistoryCommand> {
        Self::pop_batch(&mut self.undo)
    }

    fn pop_redo_batch(&mut self) -> Vec<EBookHistoryCommand> {
        Self::pop_batch(&mut self.redo)
    }

    fn update_uid_references(&mut self, source_uid: &str, old_uid: &str, new_uid: &str) {
        if old_uid == new_uid {
            return;
        }
        for cmd in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            cmd.forward.update_uid(source_uid, old_uid, new_uid);
            cmd.inverse.update_uid(source_uid, old_uid, new_uid);
        }
    }
}

static KNOWN_EBOOK_VCARDS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
> = std::sync::OnceLock::new();

static EBOOK_HISTORY_STACK: std::sync::OnceLock<std::sync::Mutex<EBookCommandStack>> =
    std::sync::OnceLock::new();

fn known_ebook_vcards() -> &'static std::sync::Mutex<
    std::collections::HashMap<(String, String), String>,
> {
    KNOWN_EBOOK_VCARDS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn ebook_history_stack() -> &'static std::sync::Mutex<EBookCommandStack> {
    EBOOK_HISTORY_STACK.get_or_init(|| std::sync::Mutex::new(EBookCommandStack::default()))
}

pub(crate) fn register_known_ebook_vcard(source_uid: &str, uid: &str, vcard: &str) {
    if uid.trim().is_empty() || vcard.trim().is_empty() {
        return;
    }
    if let Ok(mut map) = known_ebook_vcards().lock() {
        map.insert((source_uid.to_string(), uid.to_string()), vcard.to_string());
    }
}

fn remove_known_ebook_vcard(source_uid: &str, uid: &str) {
    if let Ok(mut map) = known_ebook_vcards().lock() {
        map.remove(&(source_uid.to_string(), uid.to_string()));
    }
}

pub(crate) fn register_ebook_records(records: &[EBookContactRecord]) {
    let Ok(mut map) = known_ebook_vcards().lock() else {
        return;
    };
    for rec in records {
        if !rec.uid.trim().is_empty() && !rec.vcard.trim().is_empty() {
            map.insert((rec.source_uid.clone(), rec.uid.clone()), rec.vcard.clone());
        }
    }
}

fn lookup_known_ebook_vcard(source_uid: &str, uid: &str) -> Option<String> {
    if let Ok(map) = known_ebook_vcards().lock() {
        if let Some(vcard) = map.get(&(source_uid.to_string(), uid.to_string())) {
            return Some(vcard.clone());
        }
        if let Some((_, vcard)) = map.iter().find(|((_, u), _)| u == uid) {
            return Some(vcard.clone());
        }
    }
    if let Ok(guard) = CONTACTS_CACHE.read() {
        if let Some((_, ref cached)) = *guard {
            if let Some(rec) = cached
                .contacts
                .iter()
                .find(|c| c.uid == uid && (c.source_uid == source_uid || c.source_uid.is_empty()))
                .or_else(|| cached.contacts.iter().find(|c| c.uid == uid))
            {
                return Some(rec.vcard.clone());
            }
        }
    }
    if source_uid == "local-addressbook" || source_uid.starts_with("account-contacts-") {
        if let Some(rec) = ContactStoreService::new()
            .load_contacts()
            .into_iter()
            .find(|c| c.id == uid)
        {
            return Some(rec.vcard);
        }
    }
    None
}

fn record_ebook_history_create(source_uid: &str, uid: &str, vcard: &str) {
    if EBOOK_IN_HISTORY_REPLAY.with(|c| c.get()) {
        return;
    }
    let batch_id = EBOOK_ACTIVE_BATCH_ID.with(|c| c.get());
    let cmd = EBookHistoryCommand {
        forward: EBookHistoryOp::Create {
            source_uid: source_uid.to_string(),
            uid: uid.to_string(),
            vcard: vcard.to_string(),
        },
        inverse: EBookHistoryOp::Remove {
            source_uid: source_uid.to_string(),
            uid: uid.to_string(),
        },
        batch_id,
    };
    if let Ok(mut stack) = ebook_history_stack().lock() {
        stack.push_command(cmd);
    }
}

fn record_ebook_history_modify(
    source_uid: &str,
    before_uid: &str,
    after_uid: &str,
    before_vcard: &str,
    after_vcard: &str,
) {
    if EBOOK_IN_HISTORY_REPLAY.with(|c| c.get()) || before_vcard == after_vcard {
        return;
    }
    let batch_id = EBOOK_ACTIVE_BATCH_ID.with(|c| c.get());
    let cmd = EBookHistoryCommand {
        forward: EBookHistoryOp::Modify {
            source_uid: source_uid.to_string(),
            uid: after_uid.to_string(),
            vcard: after_vcard.to_string(),
        },
        inverse: EBookHistoryOp::Modify {
            source_uid: source_uid.to_string(),
            uid: before_uid.to_string(),
            vcard: before_vcard.to_string(),
        },
        batch_id,
    };
    if let Ok(mut stack) = ebook_history_stack().lock() {
        stack.push_command(cmd);
    }
}

fn record_ebook_history_remove(source_uid: &str, uid: &str, before_vcard: &str) {
    if EBOOK_IN_HISTORY_REPLAY.with(|c| c.get()) {
        return;
    }
    let batch_id = EBOOK_ACTIVE_BATCH_ID.with(|c| c.get());
    let cmd = EBookHistoryCommand {
        forward: EBookHistoryOp::Remove {
            source_uid: source_uid.to_string(),
            uid: uid.to_string(),
        },
        inverse: EBookHistoryOp::Create {
            source_uid: source_uid.to_string(),
            uid: uid.to_string(),
            vcard: before_vcard.to_string(),
        },
        batch_id,
    };
    if let Ok(mut stack) = ebook_history_stack().lock() {
        stack.push_command(cmd);
    }
}

fn apply_ebook_history_op(
    service: &EBookService,
    op: &EBookHistoryOp,
) -> Result<(String, String, Option<String>), EBookMutationError> {
    match op {
        EBookHistoryOp::Create {
            source_uid,
            uid,
            vcard,
        } => {
            let res = service.create_contact_raw(source_uid, vcard)?;
            let confirmed_uid = if res.uid.trim().is_empty() {
                uid.clone()
            } else {
                res.uid
            };
            let confirmed_vcard = res
                .vcard
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(vcard)
                .to_string();
            register_known_ebook_vcard(source_uid, &confirmed_uid, &confirmed_vcard);
            Ok((
                source_uid.clone(),
                uid.clone(),
                (confirmed_uid != *uid).then_some(confirmed_uid),
            ))
        }
        EBookHistoryOp::Modify {
            source_uid,
            uid,
            vcard,
        } => {
            let res = service.modify_contact_raw(source_uid, uid, vcard)?;
            let confirmed_uid = if res.uid.trim().is_empty() {
                uid.clone()
            } else {
                res.uid
            };
            let confirmed_vcard = res
                .vcard
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(vcard)
                .to_string();
            register_known_ebook_vcard(source_uid, &confirmed_uid, &confirmed_vcard);
            Ok((
                source_uid.clone(),
                uid.clone(),
                (confirmed_uid != *uid).then_some(confirmed_uid),
            ))
        }
        EBookHistoryOp::Remove { source_uid, uid } => {
            service.remove_contact_raw(source_uid, uid)?;
            remove_known_ebook_vcard(source_uid, uid);
            Ok((source_uid.clone(), uid.clone(), None))
        }
    }
}

fn execute_ebook_history_action(
    service: &EBookService,
    undo: bool,
) -> Result<usize, EBookMutationError> {
    let _replay_guard = EBookHistoryReplayGuard::new();
    let mut batch = {
        let Ok(mut stack) = ebook_history_stack().lock() else {
            return Ok(0);
        };
        let source_vec = if undo { &mut stack.undo } else { &mut stack.redo };
        let Some(head_batch_id) = source_vec.last().map(|cmd| cmd.batch_id) else {
            return Ok(0);
        };
        let mut popped = Vec::new();
        if let Some(batch_id) = head_batch_id {
            while source_vec
                .last()
                .is_some_and(|cmd| cmd.batch_id == Some(batch_id))
            {
                if let Some(cmd) = source_vec.pop() {
                    popped.push(cmd);
                }
            }
            popped.reverse();
        } else if let Some(cmd) = source_vec.pop() {
            popped.push(cmd);
        }
        popped
    };

    if batch.is_empty() {
        return Ok(0);
    }

    let mut applied_indices = Vec::with_capacity(batch.len());
    let mut uid_renames = Vec::new();
    let indices: Vec<usize> = if undo {
        (0..batch.len()).rev().collect()
    } else {
        (0..batch.len()).collect()
    };

    for idx in indices {
        let op = if undo {
            batch[idx].inverse.clone()
        } else {
            batch[idx].forward.clone()
        };
        match apply_ebook_history_op(service, &op) {
            Ok((source_uid, old_uid, Some(new_uid))) => {
                batch[idx].forward.update_uid(&source_uid, &old_uid, &new_uid);
                batch[idx].inverse.update_uid(&source_uid, &old_uid, &new_uid);
                for other in &mut batch {
                    other.forward.update_uid(&source_uid, &old_uid, &new_uid);
                    other.inverse.update_uid(&source_uid, &old_uid, &new_uid);
                }
                uid_renames.push((source_uid, old_uid, new_uid));
                applied_indices.push(idx);
            }
            Ok((_, _, None)) => {
                applied_indices.push(idx);
            }
            Err(error) => {
                for prev_idx in applied_indices.into_iter().rev() {
                    let rollback_op = if undo {
                        &batch[prev_idx].forward
                    } else {
                        &batch[prev_idx].inverse
                    };
                    let _ = apply_ebook_history_op(service, rollback_op);
                }
                if let Ok(mut stack) = ebook_history_stack().lock() {
                    for (src, old_u, new_u) in uid_renames {
                        stack.update_uid_references(&src, &old_u, &new_u);
                    }
                    if undo {
                        stack.undo.extend(batch);
                    } else {
                        stack.redo.extend(batch);
                    }
                }
                return Err(error);
            }
        }
    }

    let count = batch.len();
    if let Ok(mut stack) = ebook_history_stack().lock() {
        for (src, old_u, new_u) in uid_renames {
            stack.update_uid_references(&src, &old_u, &new_u);
        }
        if undo {
            stack.redo.extend(batch);
        } else {
            stack.undo.extend(batch);
        }
        stack.trim();
    }
    Ok(count)
}

pub(crate) fn invalidate_contacts_cache() {
    if let Ok(mut guard) = CONTACTS_CACHE.write() {
        *guard = None;
    }
}

pub(crate) fn load_contacts_prefer_eds() -> EBookLoadResult {
    load_contacts_prefer_eds_cached(true)
}

pub(crate) fn load_contacts_prefer_eds_cached(use_cache: bool) -> EBookLoadResult {
    if use_cache {
        if let Ok(guard) = CONTACTS_CACHE.read() {
            if let Some((instant, cached)) = guard.as_ref() {
                if instant.elapsed() < std::time::Duration::from_secs(60) {
                    return cached.clone();
                }
            }
        }
    }
    let res = load_contacts_prefer_eds_fresh();
    register_ebook_records(&res.contacts);
    if let Ok(mut guard) = CONTACTS_CACHE.write() {
        *guard = Some((std::time::Instant::now(), res.clone()));
    }
    res
}

fn load_contacts_prefer_eds_fresh() -> EBookLoadResult {
    let (mut contacts, warning) = match EBookService::new().load_contacts() {
        Ok((loaded, warn)) => (loaded, warn),
        Err(err) => {
            tracing::warn!("Native EBookService load_contacts error: {:?}", err);
            let warn = match err {
                EBookServiceError::Unsupported => None,
                other => Some(other.to_string()),
            };
            (Vec::new(), warn)
        }
    };

    contacts.retain(|c| {
        if c.source_uid == "local-addressbook" {
            return true;
        }
        if !crate::services::eds_service::is_source_enabled_for_service(
            &c.source_uid,
            crate::services::eds_service::EdsSourceKind::AddressBook,
        ) {
            return false;
        }
        let parent = crate::services::eds_service::resolve_source_parent_name(&c.source_uid);
        if crate::services::eds_service::is_shared_or_delegate_mailbox(&parent)
            || crate::services::eds_service::is_shared_or_delegate_mailbox(&c.source_display_name)
            || crate::services::eds_service::is_shared_or_delegate_mailbox(&c.source_uid)
        {
            return false;
        }
        if crate::services::eds_service::is_goa_identity_service_enabled(
            &parent,
            crate::services::eds_service::EdsSourceKind::AddressBook,
        ) == Some(false)
            || crate::services::eds_service::is_goa_identity_service_enabled(
                &c.source_display_name,
                crate::services::eds_service::EdsSourceKind::AddressBook,
            ) == Some(false)
        {
            return false;
        }
        true
    });

    // Augment with SQLite cached contacts (e.g. from Flatpak or offline caches)
    let sqlite_contacts = crate::services::evolution_contact_cache::load_contacts_from_evolution_sqlite_cache();
    let mut known_uids: std::collections::HashSet<String> = contacts.iter().map(|c| c.uid.clone()).collect();
    let mut known_identities: std::collections::HashSet<(String, String)> = contacts
        .iter()
        .filter(|c| !c.contact.fn_name.is_empty())
        .map(|c| (c.contact.fn_name.clone(), c.contact.email.clone()))
        .collect();

    for sc in sqlite_contacts {
        if known_uids.contains(&sc.uid) {
            continue;
        }
        let has_fn = !sc.contact.fn_name.is_empty();
        if has_fn && known_identities.contains(&(sc.contact.fn_name.clone(), sc.contact.email.clone())) {
            continue;
        }
        known_uids.insert(sc.uid.clone());
        if has_fn {
            known_identities.insert((sc.contact.fn_name.clone(), sc.contact.email.clone()));
        }
        contacts.push(sc);
    }

    let local_service = ContactStoreService::new();

    if local_service.is_writable() {
        contacts.extend(local_service.load_contacts().into_iter().map(|record| {
            EBookContactRecord {
                source_uid: "local-addressbook".to_string(),
                source_display_name: "Local Address Book".to_string(),
                uid: record.id,
                vcard: record.vcard,
                contact: record.contact,
            }
        }));
    }

    if contacts.is_empty() {
        if let Some(graph) = load_contacts_from_graph() {
            return graph;
        }
    }

    let origin = if !contacts.is_empty() {
        EBookDataOrigin::Eds
    } else {
        EBookDataOrigin::Local
    };

    EBookLoadResult {
        contacts,
        origin,
        warning,
    }
}

fn load_contacts_from_graph() -> Option<EBookLoadResult> {
    let (source_uid, service) = graph_service_for_configured_account()?;
    let contacts = service.list_contacts_sync("", 5_000).ok()?;
    let source_display_name = if service.username.trim().is_empty() {
        "Microsoft 365 Contacts".to_string()
    } else {
        format!("Microsoft 365 ({})", service.username)
    };
    let mut records = contacts
        .into_iter()
        .map(|contact| {
            let uid = contact.id.clone();
            let vcard = contact.to_vcard();
            let parsed = VCardContact::parse_vcard_string(&vcard)
                .unwrap_or_else(|| VCardContact::new(&contact.display_name, "", "", ""));
            EBookContactRecord {
                source_uid: source_uid.clone(),
                source_display_name: source_display_name.clone(),
                uid,
                vcard,
                contact: parsed,
            }
        })
        .collect::<Vec<_>>();
    let local_service = ContactStoreService::new();
    if local_service.is_writable() {
        records.extend(local_service.load_contacts().into_iter().map(|record| {
            EBookContactRecord {
                source_uid: "local-addressbook".to_string(),
                source_display_name: "Local Address Book".to_string(),
                uid: record.id,
                vcard: record.vcard,
                contact: record.contact,
            }
        }));
    }
    Some(EBookLoadResult {
        contacts: records,
        origin: EBookDataOrigin::Graph,
        warning: None,
    })
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

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn record(source_uid: &str, uid: &str, vcard: &str) -> EBookContactRecord {
        EBookContactRecord {
            source_uid: source_uid.to_string(),
            source_display_name: "Testadressbuch".to_string(),
            uid: uid.to_string(),
            vcard: vcard.to_string(),
            contact: VCardContact::parse_vcard_string(vcard).expect("test vCard should parse"),
        }
    }

    #[test]
    fn merge_rejects_contacts_from_different_sources_before_mutation() {
        let primary_vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:primary\r\nFN:Ada\r\nEND:VCARD\r\n";
        let duplicate_vcard =
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:duplicate\r\nFN:Ada\r\nEND:VCARD\r\n";
        let result = EBookService::new().merge_contacts(
            &record("book-a", "primary", primary_vcard),
            &[record("book-b", "duplicate", duplicate_vcard)],
        );
        assert!(matches!(
            result,
            Err(EBookMutationError::Backend(message))
                if message.contains("same source")
        ));
    }
}

#[cfg(all(target_os = "linux", eds_ebook_bridge))]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub(super) struct MeridianEdsEBookContact {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) contact_uid: *mut c_char,
        pub(super) vcard: *mut c_char,
    }

    #[repr(C)]
    pub(super) struct MeridianEdsEBookWatcherEvent {
        pub(super) kind: c_int,
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) contact_uid: *mut c_char,
        pub(super) vcard: *mut c_char,
        pub(super) error: *mut c_char,
    }

    unsafe extern "C" {
        pub(super) fn meridian_eds_ebook_load_contacts(
            out_contacts: *mut *mut MeridianEdsEBookContact,
            out_count: *mut usize,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn meridian_eds_ebook_free_contacts(
            contacts: *mut MeridianEdsEBookContact,
            count: usize,
        );

        pub(super) fn meridian_eds_ebook_apply_operation(
            source_uid: *const c_char,
            operation: c_int,
            vcard: *const c_char,
            contact_uid: *const c_char,
            out_uid: *mut *mut c_char,
            out_vcard: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn meridian_eds_ebook_free_error(error_message: *mut c_char);

        pub(super) fn meridian_eds_ebook_watcher_new(
            out_watcher: *mut *mut c_void,
            out_configured_sources: *mut usize,
            out_active_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> c_int;

        pub(super) fn meridian_eds_ebook_watcher_next_event(
            watcher: *mut c_void,
            timeout_milliseconds: c_int,
            out_event: *mut MeridianEdsEBookWatcherEvent,
        ) -> c_int;

        pub(super) fn meridian_eds_ebook_free_watcher_event(
            event: *mut MeridianEdsEBookWatcherEvent,
        );

        pub(super) fn meridian_eds_ebook_watcher_free(watcher: *mut c_void);
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
        ffi::meridian_eds_ebook_load_contacts(
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
            "Could not load EBook data".to_string()
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
                let uid = borrowed_c_string(contact.contact_uid).unwrap_or_default();
                let source_uid = borrowed_c_string(contact.source_uid).unwrap_or_default();
                tracing::debug!(%uid, %source_uid, "Ignoring an EDS contact with an invalid vCard");
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
        ffi::meridian_eds_ebook_free_contacts(contacts_pointer, count);
    }

    let warning = error.or_else(|| {
        (failed_sources > 0).then(|| {
            format!(
                "{failed_sources} of {configured_sources} EDS address book source(s) could not be loaded"
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
        ffi::meridian_eds_ebook_free_error(pointer);
    }
    value
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn ebook_history_stack_groups_batches_for_undo_and_redo() {
        let mut stack = EBookCommandStack::default();
        for uid in ["c-1", "c-2", "c-3"] {
            stack.push_command(EBookHistoryCommand {
                forward: EBookHistoryOp::Remove {
                    source_uid: "local-address-book".to_string(),
                    uid: uid.to_string(),
                },
                inverse: EBookHistoryOp::Create {
                    source_uid: "local-address-book".to_string(),
                    uid: uid.to_string(),
                    vcard: format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Test\r\nEND:VCARD\r\n"),
                },
                batch_id: Some(99),
            });
        }

        let undo_batch = stack.pop_undo_batch();
        assert_eq!(undo_batch.len(), 3);
        assert!(stack.undo.is_empty());

        stack.redo.extend(undo_batch);
        let redo_batch = stack.pop_redo_batch();
        assert_eq!(redo_batch.len(), 3);
        assert!(stack.redo.is_empty());
    }
}
