// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native Linux mail adapter.  EDS owns account/source configuration and
// Camel owns the actual mail store.  The adapter deliberately returns plain
// Rust records so all GTK work remains on the main thread.

use super::mail_store_service::{MailBackendLocation, ParsedMail};

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(eds_camel_bridge)]
use std::sync::atomic::Ordering;

#[cfg(eds_camel_bridge)]
use super::mail_store_service::{MailStoreService, assign_thread_metadata, parse_rfc822};

#[derive(Debug, Clone)]
pub(crate) struct CamelFolderSummary {
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) folder_name: String,
    pub(crate) folder_id: String,
    pub(crate) display_name: String,
    pub(crate) unread_count: u32,
    pub(crate) total_count: u32,
    pub(crate) is_selectable: bool,
}

#[derive(Debug)]
pub(crate) struct CamelMailLoad {
    pub(crate) messages: Vec<ParsedMail>,
    pub(crate) folders: Vec<CamelFolderSummary>,
    pub(crate) configured_sources: usize,
    pub(crate) successful_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CamelSyncPhase {
    Discovering,
    Connecting,
    LoadingFolders,
    LoadingMessages,
    SourceComplete,
    Complete,
    Failed,
    Retrying,
}

impl CamelSyncPhase {
    fn from_native(value: i32) -> Option<Self> {
        Some(match value {
            1 => Self::Discovering,
            2 => Self::Connecting,
            3 => Self::LoadingFolders,
            4 => Self::LoadingMessages,
            5 => Self::SourceComplete,
            6 => Self::Complete,
            7 => Self::Failed,
            8 => Self::Retrying,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CamelSyncProgress {
    pub(crate) phase: CamelSyncPhase,
    pub(crate) source_uid: Option<String>,
    pub(crate) source_display_name: Option<String>,
    pub(crate) folder_name: Option<String>,
    pub(crate) configured_sources: usize,
    pub(crate) processed_sources: usize,
    pub(crate) successful_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) folders_seen: usize,
    pub(crate) messages_loaded: usize,
    pub(crate) attempt: usize,
    pub(crate) max_attempts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CamelChangeKind {
    Changed,
    FolderDeleted,
    FolderRenamed,
}

#[derive(Debug, Clone)]
pub(crate) struct CamelChangeEvent {
    pub(crate) kind: CamelChangeKind,
    pub(crate) source_uid: String,
    pub(crate) folder_name: String,
    pub(crate) old_folder_name: Option<String>,
    pub(crate) added_count: usize,
    pub(crate) removed_count: usize,
    pub(crate) changed_count: usize,
    pub(crate) recent_count: usize,
    pub(crate) added_uids: Vec<String>,
    pub(crate) removed_uids: Vec<String>,
    pub(crate) changed_uids: Vec<String>,
    pub(crate) recent_uids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CamelWatcherStatus {
    pub(crate) configured_sources: usize,
    pub(crate) successful_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum CamelSyncEvent {
    Ready(CamelWatcherStatus),
    Change(CamelChangeEvent),
    Error(String),
}

impl CamelMailLoad {
    pub(crate) fn has_more_for(
        &self,
        source_uid: Option<&str>,
        folder_name: Option<&str>,
        message_offset: usize,
        max_messages_per_folder: i32,
    ) -> bool {
        let page_size = max_messages_per_folder.max(1) as usize;
        let next_offset = message_offset.saturating_add(page_size);

        self.folders.iter().any(|folder| {
            let source_matches = source_uid
                .map(|uid| folder.source_uid == uid)
                .unwrap_or(true);
            let folder_matches = folder_name
                .map(|name| folder.folder_name == name)
                .unwrap_or(true);

            source_matches
                && folder_matches
                && folder.is_selectable
                && (folder.total_count as usize) > next_offset
        })
    }
}

#[cfg(eds_camel_bridge)]
mod ffi {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelMessage {
        pub(super) source_uid: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) folder_id: *mut c_char,
        pub(super) message_uid: *mut c_char,
        pub(super) raw_message: *mut u8,
        pub(super) raw_message_len: usize,
        pub(super) is_unread: i32,
        pub(super) is_flagged: i32,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelFolder {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) folder_id: *mut c_char,
        pub(super) display_name: *mut c_char,
        pub(super) unread_count: u32,
        pub(super) total_count: u32,
        pub(super) is_selectable: i32,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelOperation {
        pub(super) kind: i32,
        pub(super) source_uid: *const c_char,
        pub(super) folder_name: *const c_char,
        pub(super) message_uid: *const c_char,
        pub(super) destination_folder: *const c_char,
        pub(super) is_unread: i32,
        pub(super) is_flagged: i32,
        pub(super) is_junk: i32,
        pub(super) is_important: i32,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelSyncProgress {
        pub(super) phase: i32,
        pub(super) source_uid: *const c_char,
        pub(super) source_display_name: *const c_char,
        pub(super) folder_name: *const c_char,
        pub(super) configured_sources: usize,
        pub(super) processed_sources: usize,
        pub(super) successful_sources: usize,
        pub(super) failed_sources: usize,
        pub(super) folders_seen: usize,
        pub(super) messages_loaded: usize,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelChangeEvent {
        pub(super) kind: i32,
        pub(super) source_uid: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) old_folder_name: *mut c_char,
        pub(super) added_count: usize,
        pub(super) removed_count: usize,
        pub(super) changed_count: usize,
        pub(super) recent_count: usize,
        pub(super) added_uids: *mut *mut c_char,
        pub(super) added_uid_count: usize,
        pub(super) removed_uids: *mut *mut c_char,
        pub(super) removed_uid_count: usize,
        pub(super) changed_uids: *mut *mut c_char,
        pub(super) changed_uid_count: usize,
        pub(super) recent_uids: *mut *mut c_char,
        pub(super) recent_uid_count: usize,
    }

    pub(super) type EvolutionEdsCamelProgressCallback = unsafe extern "C" fn(
        progress: *const EvolutionEdsCamelSyncProgress,
        user_data: *mut c_void,
    );

    pub(super) type EvolutionEdsCamelCancellationCallback =
        unsafe extern "C" fn(user_data: *mut c_void) -> i32;

    unsafe extern "C" {
        pub(super) fn evolution_eds_camel_load_messages(
            source_uid: *const c_char,
            folder_name: *const c_char,
            message_offset: i32,
            max_messages_per_folder: i32,
            out_messages: *mut *mut EvolutionEdsCamelMessage,
            out_count: *mut usize,
            out_folders: *mut *mut EvolutionEdsCamelFolder,
            out_folder_count: *mut usize,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
            progress_callback: Option<EvolutionEdsCamelProgressCallback>,
            progress_user_data: *mut c_void,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_load_messages_by_uid(
            source_uid: *const c_char,
            folder_name: *const c_char,
            message_uids: *const *const c_char,
            message_uid_count: usize,
            out_messages: *mut *mut EvolutionEdsCamelMessage,
            out_count: *mut usize,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_free_messages(
            messages: *mut EvolutionEdsCamelMessage,
            count: usize,
        );

        pub(super) fn evolution_eds_camel_free_folders(
            folders: *mut EvolutionEdsCamelFolder,
            count: usize,
        );

        pub(super) fn evolution_eds_camel_apply_operation(
            operation: *const EvolutionEdsCamelOperation,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_watcher_new(
            source_uid: *const c_char,
            out_watcher: *mut *mut c_void,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_watcher_next_event(
            watcher: *mut c_void,
            timeout_milliseconds: i32,
            out_event: *mut EvolutionEdsCamelChangeEvent,
        ) -> i32;

        pub(super) fn evolution_eds_camel_free_change_event(
            event: *mut EvolutionEdsCamelChangeEvent,
        );

        pub(super) fn evolution_eds_camel_watcher_free(watcher: *mut c_void);

        pub(super) fn evolution_eds_camel_free_error(error_message: *mut c_char);
    }
}

/// Load enabled remote EDS mail accounts through the original Camel
/// providers.  macOS builds intentionally omit this Linux-only adapter; the
/// Linux build script requires the EDS development packages before enabling
/// the same code path.
pub(crate) fn default_page_limit() -> i32 {
    std::env::var("EVOLUTION_GTK4_CAMEL_MESSAGES_PER_FOLDER")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100)
}

#[allow(dead_code)]
pub(crate) fn load_messages_page(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
) -> Result<CamelMailLoad, String> {
    load_messages_page_with_progress(
        source_uid,
        folder_name,
        message_offset,
        max_messages_per_folder,
        None,
    )
}

pub(crate) fn load_messages_page_with_progress(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    load_messages_page_with_progress_and_cancellation(
        source_uid,
        folder_name,
        message_offset,
        max_messages_per_folder,
        None,
        progress_sender,
    )
}

pub(crate) fn load_messages_page_with_progress_and_cancellation(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    cancelled: Option<&AtomicBool>,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    #[cfg(eds_camel_bridge)]
    {
        const MAX_ATTEMPTS: usize = 3;

        for attempt in 1..=MAX_ATTEMPTS {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err("Mail-Ladevorgang wurde abgebrochen.".to_string());
            }

            let result = load_messages_native(
                source_uid,
                folder_name,
                message_offset,
                max_messages_per_folder,
                cancelled,
                progress_sender,
            );
            let retryable = match &result {
                Ok(load) => {
                    load.configured_sources > 0
                        && load.successful_sources == 0
                        && load.failed_sources > 0
                        && load.warning.is_some()
                }
                Err(_) => true,
            };

            if attempt == MAX_ATTEMPTS || !retryable {
                return result;
            }

            if let Some(sender) = progress_sender {
                let _ = sender.try_send(CamelSyncProgress {
                    phase: CamelSyncPhase::Retrying,
                    source_uid: source_uid.map(str::to_string),
                    source_display_name: None,
                    folder_name: folder_name.map(str::to_string),
                    configured_sources: 0,
                    processed_sources: 0,
                    successful_sources: 0,
                    failed_sources: 0,
                    folders_seen: 0,
                    messages_loaded: 0,
                    attempt: attempt + 1,
                    max_attempts: MAX_ATTEMPTS,
                });
            }

            let delay_milliseconds = 250_u64.saturating_mul(1_u64 << (attempt - 1));
            let mut waited = 0;
            while waited < delay_milliseconds {
                if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    return Err("Mail-Ladevorgang wurde abgebrochen.".to_string());
                }
                let slice = (delay_milliseconds - waited).min(25);
                std::thread::sleep(std::time::Duration::from_millis(slice));
                waited += slice;
            }
        }

        unreachable!("Camel retry loop always returns on its final attempt")
    }

    #[cfg(not(eds_camel_bridge))]
    {
        let _ = (
            source_uid,
            folder_name,
            message_offset,
            max_messages_per_folder,
            cancelled,
            progress_sender,
        );
        Err("native EDS/Camel adapter is not available in this build".to_string())
    }
}

#[cfg(eds_camel_bridge)]
fn load_messages_native(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    cancelled: Option<&AtomicBool>,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::slice;

    struct ProgressContext<'a> {
        sender: &'a async_channel::Sender<CamelSyncProgress>,
    }

    unsafe extern "C" fn progress_callback(
        progress: *const ffi::EvolutionEdsCamelSyncProgress,
        user_data: *mut c_void,
    ) {
        if progress.is_null() || user_data.is_null() {
            return;
        }

        let progress = unsafe { &*progress };
        let context = unsafe { &*(user_data as *const ProgressContext<'_>) };
        let Some(phase) = CamelSyncPhase::from_native(progress.phase) else {
            return;
        };

        let _ = context.sender.try_send(CamelSyncProgress {
            phase,
            source_uid: c_string(progress.source_uid).into_option(),
            source_display_name: c_string(progress.source_display_name).into_option(),
            folder_name: c_string(progress.folder_name).into_option(),
            configured_sources: progress.configured_sources,
            processed_sources: progress.processed_sources,
            successful_sources: progress.successful_sources,
            failed_sources: progress.failed_sources,
            folders_seen: progress.folders_seen,
            messages_loaded: progress.messages_loaded,
            attempt: 1,
            max_attempts: 1,
        });
    }

    trait EmptyStringOption {
        fn into_option(self) -> Option<String>;
    }

    impl EmptyStringOption for String {
        fn into_option(self) -> Option<String> {
            (!self.is_empty()).then_some(self)
        }
    }

    unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
        if user_data.is_null() {
            return 0;
        }

        let cancelled = unsafe { &*(user_data as *const AtomicBool) };
        cancelled.load(Ordering::Relaxed) as i32
    }

    let message_offset = message_offset.min(i32::MAX as usize) as i32;
    let max_messages = if max_messages_per_folder > 0 {
        max_messages_per_folder
    } else {
        default_page_limit()
    };

    let mut records = std::ptr::null_mut();
    let mut record_count = 0usize;
    let mut folders = std::ptr::null_mut();
    let mut folder_count = 0usize;
    let mut configured_sources = 0usize;
    let mut successful_sources = 0usize;
    let mut failed_sources = 0usize;
    let mut error_message: *mut c_char = std::ptr::null_mut();
    let source_uid = source_uid
        .map(CString::new)
        .transpose()
        .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
    let folder_name = folder_name
        .map(CString::new)
        .transpose()
        .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
    let source_uid_ptr = source_uid
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    let folder_name_ptr = folder_name
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    let progress_context = progress_sender.map(|sender| ProgressContext { sender });
    let progress_user_data = progress_context
        .as_ref()
        .map_or(std::ptr::null_mut(), |context| {
            context as *const ProgressContext<'_> as *mut c_void
        });
    let progress_callback = progress_context
        .as_ref()
        .map(|_| progress_callback as ffi::EvolutionEdsCamelProgressCallback);
    let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
        cancelled as *const AtomicBool as *mut c_void
    });
    let cancellation_callback =
        cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);

    // Passing null filters asks the adapter to load every enabled EDS mail
    // account and selectable folder.  The GTK view supplies source/folder
    // filters after the user selects an account-specific row.
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages(
            source_uid_ptr,
            folder_name_ptr,
            message_offset,
            max_messages,
            &mut records,
            &mut record_count,
            &mut folders,
            &mut folder_count,
            &mut configured_sources,
            &mut successful_sources,
            &mut failed_sources,
            &mut error_message,
            progress_callback,
            progress_user_data,
            cancellation_callback,
            cancellation_user_data,
        ) != 0
    };

    let warning = (!error_message.is_null()).then(|| unsafe {
        let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
        ffi::evolution_eds_camel_free_error(error_message);
        message
    });

    if !success {
        if !records.is_null() {
            unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
        }
        if !folders.is_null() {
            unsafe { ffi::evolution_eds_camel_free_folders(folders, folder_count) };
        }
        return Err(warning.unwrap_or_else(|| "EDS/Camel mail loading failed".to_string()));
    }

    let mut folder_summaries = Vec::with_capacity(folder_count);
    if !folders.is_null() {
        let folders_slice = unsafe { slice::from_raw_parts(folders, folder_count) };
        for folder in folders_slice {
            folder_summaries.push(CamelFolderSummary {
                source_uid: c_string(folder.source_uid),
                source_display_name: c_string(folder.source_display_name),
                folder_name: c_string(folder.folder_name),
                folder_id: c_string(folder.folder_id),
                display_name: c_string(folder.display_name),
                unread_count: folder.unread_count,
                total_count: folder.total_count,
                is_selectable: folder.is_selectable != 0,
            });
        }
        unsafe { ffi::evolution_eds_camel_free_folders(folders, folder_count) };
    }

    let messages = parse_native_messages(records, record_count);
    if !records.is_null() {
        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }

    Ok(CamelMailLoad {
        messages,
        folders: folder_summaries,
        configured_sources,
        successful_sources,
        failed_sources,
        warning,
    })
}

#[cfg(eds_camel_bridge)]
fn parse_native_messages(
    records: *const ffi::EvolutionEdsCamelMessage,
    record_count: usize,
) -> Vec<ParsedMail> {
    use std::slice;

    let mut messages = Vec::with_capacity(record_count);
    if records.is_null() {
        return messages;
    }

    let records_slice = unsafe { slice::from_raw_parts(records, record_count) };
    for record in records_slice {
        if record.raw_message.is_null() || record.raw_message_len == 0 {
            continue;
        }

        let source_uid = c_string(record.source_uid);
        let folder_name = c_string(record.folder_name);
        let folder_id = c_string(record.folder_id);
        let message_uid = c_string(record.message_uid);
        let location = MailBackendLocation {
            source_uid: source_uid.clone(),
            folder_name: folder_name.clone(),
            message_uid: message_uid.clone(),
        };
        let raw = unsafe { slice::from_raw_parts(record.raw_message, record.raw_message_len) };
        let raw = String::from_utf8_lossy(raw);
        let id = format!("eds:{source_uid}:{folder_name}:{message_uid}");

        if let Some(mut parsed) = parse_rfc822(
            &id,
            if folder_id.is_empty() {
                "all"
            } else {
                &folder_id
            },
            &raw,
            record.is_unread != 0,
        ) {
            parsed.is_flagged = record.is_flagged != 0;
            parsed.folder_name = folder_name;
            parsed.backend_location = Some(location);
            messages.push(parsed);
        }
    }

    MailStoreService::new().apply_backend_filters(&mut messages);
    assign_thread_metadata(&mut messages);
    messages
}

pub(crate) fn load_messages_for_uids(
    source_uid: &str,
    folder_name: &str,
    message_uids: &[String],
    cancelled: &AtomicBool,
) -> Result<Vec<ParsedMail>, String> {
    #[cfg(eds_camel_bridge)]
    {
        return load_messages_for_uids_native(source_uid, folder_name, message_uids, cancelled);
    }

    #[cfg(not(eds_camel_bridge))]
    {
        let _ = (source_uid, folder_name, message_uids, cancelled);
        Err("native EDS/Camel adapter is not available in this build".to_string())
    }
}

#[cfg(eds_camel_bridge)]
fn load_messages_for_uids_native(
    source_uid: &str,
    folder_name: &str,
    message_uids: &[String],
    cancelled: &AtomicBool,
) -> Result<Vec<ParsedMail>, String> {
    use std::ffi::{CStr, CString, c_char, c_void};

    if message_uids.is_empty() {
        return Ok(Vec::new());
    }

    unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
        if user_data.is_null() {
            return 0;
        }

        let cancelled = unsafe { &*(user_data as *const AtomicBool) };
        cancelled.load(Ordering::Relaxed) as i32
    }

    let source_uid = CString::new(source_uid)
        .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
    let folder_name = CString::new(folder_name)
        .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
    let message_uids = message_uids
        .iter()
        .map(|uid| {
            CString::new(uid.as_str())
                .map_err(|_| "EDS message UID contains an invalid NUL byte".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let message_uid_ptrs = message_uids
        .iter()
        .map(|uid| uid.as_ptr())
        .collect::<Vec<*const c_char>>();
    let cancellation_user_data = cancelled as *const AtomicBool as *mut c_void;
    let mut records = std::ptr::null_mut();
    let mut record_count = 0usize;
    let mut error_message: *mut c_char = std::ptr::null_mut();
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages_by_uid(
            source_uid.as_ptr(),
            folder_name.as_ptr(),
            message_uid_ptrs.as_ptr(),
            message_uid_ptrs.len(),
            &mut records,
            &mut record_count,
            &mut error_message,
            Some(cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback),
            cancellation_user_data,
        ) != 0
    };
    let warning = (!error_message.is_null()).then(|| unsafe {
        let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
        ffi::evolution_eds_camel_free_error(error_message);
        message
    });

    if !success {
        if !records.is_null() {
            unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
        }
        return Err(warning
            .unwrap_or_else(|| "Inkrementelles EDS/Camel-Laden ist fehlgeschlagen".to_string()));
    }

    let messages = parse_native_messages(records, record_count);
    if !records.is_null() {
        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err("Inkrementelles Camel-Laden wurde abgebrochen".to_string());
    }
    Ok(messages)
}

pub(crate) fn start_change_monitor()
-> Option<(Arc<AtomicBool>, async_channel::Receiver<CamelSyncEvent>)> {
    #[cfg(eds_camel_bridge)]
    {
        let (sender, receiver) = async_channel::bounded(32);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();

        std::thread::spawn(move || {
            use std::ffi::{CStr, c_char, c_void};

            let mut watcher: *mut c_void = std::ptr::null_mut();
            let mut configured_sources = 0usize;
            let mut successful_sources = 0usize;
            let mut failed_sources = 0usize;
            let mut error_message: *mut c_char = std::ptr::null_mut();
            let result = unsafe {
                ffi::evolution_eds_camel_watcher_new(
                    std::ptr::null(),
                    &mut watcher,
                    &mut configured_sources,
                    &mut successful_sources,
                    &mut failed_sources,
                    &mut error_message,
                )
            };

            let warning = (!error_message.is_null()).then(|| unsafe {
                let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
                ffi::evolution_eds_camel_free_error(error_message);
                message
            });

            if result == 0 || watcher.is_null() {
                let _ = sender.try_send(CamelSyncEvent::Error(warning.unwrap_or_else(|| {
                    "EDS/Camel-Signalmonitor konnte nicht gestartet werden".to_string()
                })));
                return;
            }

            let _ = sender.try_send(CamelSyncEvent::Ready(CamelWatcherStatus {
                configured_sources,
                successful_sources,
                failed_sources,
                warning,
            }));

            while !cancellation.load(Ordering::Relaxed) {
                let mut raw_event = ffi::EvolutionEdsCamelChangeEvent {
                    kind: 0,
                    source_uid: std::ptr::null_mut(),
                    folder_name: std::ptr::null_mut(),
                    old_folder_name: std::ptr::null_mut(),
                    added_count: 0,
                    removed_count: 0,
                    changed_count: 0,
                    recent_count: 0,
                    added_uids: std::ptr::null_mut(),
                    added_uid_count: 0,
                    removed_uids: std::ptr::null_mut(),
                    removed_uid_count: 0,
                    changed_uids: std::ptr::null_mut(),
                    changed_uid_count: 0,
                    recent_uids: std::ptr::null_mut(),
                    recent_uid_count: 0,
                };
                let next = unsafe {
                    ffi::evolution_eds_camel_watcher_next_event(watcher, 1000, &mut raw_event)
                };
                if next < 0 {
                    break;
                }
                if next == 0 {
                    continue;
                }

                let Some(kind) = (match raw_event.kind {
                    1 => Some(CamelChangeKind::Changed),
                    2 => Some(CamelChangeKind::FolderDeleted),
                    3 => Some(CamelChangeKind::FolderRenamed),
                    _ => None,
                }) else {
                    unsafe { ffi::evolution_eds_camel_free_change_event(&mut raw_event) };
                    continue;
                };
                let event = CamelChangeEvent {
                    kind,
                    source_uid: c_string(raw_event.source_uid),
                    folder_name: c_string(raw_event.folder_name),
                    old_folder_name: (!raw_event.old_folder_name.is_null())
                        .then(|| c_string(raw_event.old_folder_name)),
                    added_count: raw_event.added_count,
                    removed_count: raw_event.removed_count,
                    changed_count: raw_event.changed_count,
                    recent_count: raw_event.recent_count,
                    added_uids: c_strings(raw_event.added_uids, raw_event.added_uid_count),
                    removed_uids: c_strings(raw_event.removed_uids, raw_event.removed_uid_count),
                    changed_uids: c_strings(raw_event.changed_uids, raw_event.changed_uid_count),
                    recent_uids: c_strings(raw_event.recent_uids, raw_event.recent_uid_count),
                };
                unsafe { ffi::evolution_eds_camel_free_change_event(&mut raw_event) };
                if sender.try_send(CamelSyncEvent::Change(event)).is_err() {
                    break;
                }
            }

            unsafe { ffi::evolution_eds_camel_watcher_free(watcher) };
        });

        Some((cancelled, receiver))
    }

    #[cfg(not(eds_camel_bridge))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_more_respects_source_folder_and_page_offset() {
        let load = CamelMailLoad {
            messages: Vec::new(),
            folders: vec![
                CamelFolderSummary {
                    source_uid: "account-a".to_string(),
                    source_display_name: "Account A".to_string(),
                    folder_name: "INBOX".to_string(),
                    folder_id: "inbox".to_string(),
                    display_name: "Inbox".to_string(),
                    unread_count: 4,
                    total_count: 12,
                    is_selectable: true,
                },
                CamelFolderSummary {
                    source_uid: "account-b".to_string(),
                    source_display_name: "Account B".to_string(),
                    folder_name: "INBOX".to_string(),
                    folder_id: "inbox".to_string(),
                    display_name: "Inbox".to_string(),
                    unread_count: 0,
                    total_count: 8,
                    is_selectable: true,
                },
            ],
            configured_sources: 2,
            successful_sources: 2,
            failed_sources: 0,
            warning: None,
        };

        assert!(load.has_more_for(Some("account-a"), Some("INBOX"), 0, 10));
        assert!(!load.has_more_for(Some("account-b"), Some("INBOX"), 0, 10));
        assert!(!load.has_more_for(Some("account-a"), Some("Sent"), 0, 10));
        assert!(!load.has_more_for(Some("account-a"), Some("INBOX"), 10, 10));
    }

    #[test]
    fn native_sync_phases_are_represented_without_accepting_unknown_values() {
        assert_eq!(
            CamelSyncPhase::from_native(1),
            Some(CamelSyncPhase::Discovering)
        );
        assert_eq!(
            CamelSyncPhase::from_native(8),
            Some(CamelSyncPhase::Retrying)
        );
        assert_eq!(CamelSyncPhase::from_native(99), None);
    }
}

#[allow(dead_code)]
pub(crate) enum CamelMailOperation {
    SetMessageState {
        location: MailBackendLocation,
        is_unread: bool,
        is_flagged: bool,
    },
    Delete {
        location: MailBackendLocation,
    },
    Move {
        location: MailBackendLocation,
        destination: String,
    },
    Archive {
        location: MailBackendLocation,
    },
    SetJunk {
        location: MailBackendLocation,
        is_junk: bool,
    },
    SetImportant {
        location: MailBackendLocation,
        is_important: bool,
    },
}

pub(crate) fn apply_operation(operation: CamelMailOperation) -> Result<(), String> {
    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char};

        let (kind, location, destination, is_unread, is_flagged, is_junk, is_important) =
            match operation {
                CamelMailOperation::SetMessageState {
                    location,
                    is_unread,
                    is_flagged,
                } => (1, location, None, is_unread, is_flagged, false, false),
                CamelMailOperation::Delete { location } => {
                    (2, location, None, false, false, false, false)
                }
                CamelMailOperation::Move {
                    location,
                    destination,
                } => (3, location, Some(destination), false, false, false, false),
                CamelMailOperation::Archive { location } => {
                    (4, location, None, false, false, false, false)
                }
                CamelMailOperation::SetJunk { location, is_junk } => {
                    (5, location, None, false, false, is_junk, false)
                }
                CamelMailOperation::SetImportant {
                    location,
                    is_important,
                } => (6, location, None, false, false, false, is_important),
            };

        let source_uid = CString::new(location.source_uid)
            .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
        let folder_name = CString::new(location.folder_name)
            .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
        let message_uid = CString::new(location.message_uid)
            .map_err(|_| "EDS message UID contains an invalid NUL byte".to_string())?;
        let destination = destination
            .map(CString::new)
            .transpose()
            .map_err(|_| "EDS destination contains an invalid NUL byte".to_string())?;
        let destination_ptr = destination
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr());
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let ffi_operation = ffi::EvolutionEdsCamelOperation {
            kind,
            source_uid: source_uid.as_ptr(),
            folder_name: folder_name.as_ptr(),
            message_uid: message_uid.as_ptr(),
            destination_folder: destination_ptr,
            is_unread: is_unread as i32,
            is_flagged: is_flagged as i32,
            is_junk: is_junk as i32,
            is_important: is_important as i32,
        };

        let result =
            unsafe { ffi::evolution_eds_camel_apply_operation(&ffi_operation, &mut error_message) };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });

        if result == 1 {
            Ok(())
        } else if result == 2 {
            let message =
                error.unwrap_or_else(|| "Der Serverzustand hat sich geändert".to_string());
            Err(
                if message
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("conflict:")
                {
                    message
                } else {
                    format!("conflict: {message}")
                },
            )
        } else {
            Err(error.unwrap_or_else(|| "EDS/Camel operation failed".to_string()))
        }
    }

    #[cfg(not(eds_camel_bridge))]
    {
        let _ = operation;
        Err("native EDS/Camel adapter is not available in this build".to_string())
    }
}

#[cfg(eds_camel_bridge)]
fn c_string(pointer: *const std::ffi::c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }

    unsafe { std::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(eds_camel_bridge)]
fn c_strings(pointer: *mut *mut std::ffi::c_char, count: usize) -> Vec<String> {
    if pointer.is_null() || count == 0 {
        return Vec::new();
    }

    let pointers = unsafe { std::slice::from_raw_parts(pointer, count) };
    pointers.iter().map(|pointer| c_string(*pointer)).collect()
}
