// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native Linux mail adapter.  EDS owns account/source configuration and
// Camel owns the actual mail store.  The adapter deliberately returns plain
// Rust records so all GTK work remains on the main thread.

use super::mail_store_service::ParsedMail;

#[cfg(eds_camel_bridge)]
use super::mail_store_service::{MailStoreService, parse_rfc822};

#[derive(Debug)]
pub(crate) struct CamelMailLoad {
    pub(crate) messages: Vec<ParsedMail>,
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

    unsafe extern "C" {
        pub(super) fn evolution_eds_camel_load_messages(
            source_uid: *const c_char,
            max_messages_per_folder: i32,
            out_messages: *mut *mut EvolutionEdsCamelMessage,
            out_count: *mut usize,
            out_configured_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_free_messages(
            messages: *mut EvolutionEdsCamelMessage,
            count: usize,
        );

        pub(super) fn evolution_eds_camel_free_error(error_message: *mut c_char);
    }
}

/// Load enabled remote EDS mail accounts through the original Camel
/// providers.  macOS builds intentionally omit this Linux-only adapter; the
/// Linux build script requires the EDS development packages before enabling
/// the same code path.
pub(crate) fn load_messages() -> Result<CamelMailLoad, String> {
    #[cfg(eds_camel_bridge)]
    {
        return load_messages_native();
    }

    #[cfg(not(eds_camel_bridge))]
    {
        Err("native EDS/Camel adapter is not available in this build".to_string())
    }
}

#[cfg(eds_camel_bridge)]
fn load_messages_native() -> Result<CamelMailLoad, String> {
    use std::ffi::{CStr, CString, c_char};
    use std::slice;

    let max_messages = std::env::var("EVOLUTION_GTK4_CAMEL_MESSAGES_PER_FOLDER")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100);

    let mut records = std::ptr::null_mut();
    let mut record_count = 0usize;
    let mut configured_sources = 0usize;
    let mut error_message: *mut c_char = std::ptr::null_mut();

    // Passing a null source UID asks the adapter to load all enabled EDS mail
    // accounts.  Account selection is a later UI concern; startup should show
    // every account that the user already configured in GNOME/Evolution.
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages(
            std::ptr::null(),
            max_messages,
            &mut records,
            &mut record_count,
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
        return Err(warning.unwrap_or_else(|| "EDS/Camel mail loading failed".to_string()));
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
                messages.push(parsed);
            }
        }

        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }

    MailStoreService::new().apply_client_state(&mut messages);

    Ok(CamelMailLoad {
        messages,
        configured_sources,
        warning,
    })
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
