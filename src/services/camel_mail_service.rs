// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native Linux mail adapter.  EDS owns account/source configuration and
// Camel owns the actual mail store.  The adapter deliberately returns plain
// Rust records so all GTK work remains on the main thread.

use super::mail_store_service::{MailBackendLocation, ParsedMail};

#[cfg(eds_camel_bridge)]
use super::mail_store_service::{MailStoreService, parse_rfc822};

#[derive(Debug, Clone)]
pub(crate) struct CamelFolderSummary {
    pub(crate) source_uid: String,
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
    pub(crate) warning: Option<String>,
}

#[cfg(eds_camel_bridge)]
mod ffi {
    use std::ffi::c_char;

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

    unsafe extern "C" {
        pub(super) fn evolution_eds_camel_load_messages(
            source_uid: *const c_char,
            message_offset: i32,
            max_messages_per_folder: i32,
            out_messages: *mut *mut EvolutionEdsCamelMessage,
            out_count: *mut usize,
            out_folders: *mut *mut EvolutionEdsCamelFolder,
            out_folder_count: *mut usize,
            out_configured_sources: *mut usize,
            out_error: *mut *mut c_char,
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

pub(crate) fn load_messages_page(
    message_offset: usize,
    max_messages_per_folder: i32,
) -> Result<CamelMailLoad, String> {
    #[cfg(eds_camel_bridge)]
    {
        return load_messages_native(message_offset, max_messages_per_folder);
    }

    #[cfg(not(eds_camel_bridge))]
    {
        let _ = (message_offset, max_messages_per_folder);
        Err("native EDS/Camel adapter is not available in this build".to_string())
    }
}

#[cfg(eds_camel_bridge)]
fn load_messages_native(
    message_offset: usize,
    max_messages_per_folder: i32,
) -> Result<CamelMailLoad, String> {
    use std::ffi::{CStr, c_char};
    use std::slice;

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
    let mut error_message: *mut c_char = std::ptr::null_mut();

    // Passing a null source UID asks the adapter to load all enabled EDS mail
    // accounts.  Account selection is a later UI concern; startup should show
    // every account that the user already configured in GNOME/Evolution.
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages(
            std::ptr::null(),
            message_offset,
            max_messages,
            &mut records,
            &mut record_count,
            &mut folders,
            &mut folder_count,
            &mut configured_sources,
            &mut error_message,
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

    let mut messages = Vec::with_capacity(record_count);
    if !records.is_null() {
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

        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }

    MailStoreService::new().apply_backend_filters(&mut messages);

    Ok(CamelMailLoad {
        messages,
        folders: folder_summaries,
        configured_sources,
        warning,
    })
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

        let success = unsafe {
            ffi::evolution_eds_camel_apply_operation(&ffi_operation, &mut error_message) != 0
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });

        if success {
            Ok(())
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
