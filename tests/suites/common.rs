// SPDX-License-Identifier: GPL-3.0-or-later
//
// Common test fixtures and helpers for Evolution GTK4 Preferences & Settings E2E Test Suite.

use gtk::gio;
use gtk::gio::prelude::*;
use serde_json::{Map, Value};

/// The canonical application ID and schema ID.
pub const SCHEMA_ID: &str = "com.martingawron.Meridian";

/// List of 12 legacy keys that MUST NOT exist in schema.
/// (Feature 2: Clean GSettings Reset Undefined Keys)
pub const LEGACY_KEYS_MUST_NOT_EXIST: &[&str] = &[
    "sidebar-width",
    "show-menubar",
    "show-toolbar",
    "show-sidebar",
    "show-statusbar",
    "toolbar-style",
    "mail-subject-column-width",
    "mail-size-column-width",
    "mail-follow-up-due-column-width",
    "mail-location-column-width",
    "mail-sort-column",
    "mail-list-wide-column-order",
];

/// Obtain the compiled GSettingsSchema for Meridian.
pub fn get_test_schema() -> gio::SettingsSchema {
    let installed_source = gio::SettingsSchemaSource::default();
    installed_source
        .as_ref()
        .and_then(|source| source.lookup(SCHEMA_ID, true))
        .or_else(|| {
            let env_dir = std::env::var("MERIDIAN_SCHEMA_DIR")
                .or_else(|_| std::env::var("EVOLUTION_GTK4_SCHEMA_DIR"))
                .ok();
            let directory = option_env!("MERIDIAN_SCHEMA_DIR")
                .or(option_env!("EVOLUTION_GTK4_SCHEMA_DIR"))
                .or(env_dir.as_deref())?;
            let source = gio::SettingsSchemaSource::from_directory(
                directory,
                installed_source.as_ref(),
                true,
            )
            .ok()?;
            source.lookup(SCHEMA_ID, true)
        })
        .expect("the application settings schema com.martingawron.Meridian must be available in tests")
}

thread_local! {
    static THREAD_CONTEXT: glib::MainContext = {
        let ctx = glib::MainContext::new();
        unsafe {
            use glib::translate::ToGlibPtr;
            glib::ffi::g_main_context_push_thread_default(ctx.to_glib_none().0);
        }
        ctx
    };
}

pub fn get_thread_context() -> glib::MainContext {
    THREAD_CONTEXT.with(|ctx| ctx.clone())
}

/// Create an isolated, in-memory Settings instance for test execution.
/// Each call produces a fresh, independent memory backend with default schema values.
pub fn create_test_settings() -> gio::Settings {
    let _ctx = get_thread_context();
    let schema = get_test_schema();
    let backend = gio::memory_settings_backend_new();
    gio::Settings::new_full(&schema, Some(&backend), None)
}

/// Process-global lock to serialize tests that register GSettings changed signal handlers
/// and pump the process-global default GLib MainContext.
pub static SETTINGS_SIGNAL_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Flush pending settings changes to the backend and process any queued signal dispatches
/// on the thread-local GLib main context.
pub fn sync_settings_and_pump_events() {
    gio::Settings::sync();
    let context = get_thread_context();
    while context.iteration(false) {}
}

/// Export all known schema keys and their current values to a JSON map.
pub fn export_settings_to_json(settings: &gio::Settings) -> Value {
    let schema = get_test_schema();
    let keys = schema.list_keys();
    let mut map = Map::new();

    for key in keys {
        let key_str = key.as_str();
        let val = settings.value(key_str);
        if let Some(b) = val.get::<bool>() {
            map.insert(key_str.to_string(), Value::Bool(b));
        } else if let Some(i) = val.get::<i32>() {
            map.insert(key_str.to_string(), Value::Number(i.into()));
        } else if let Some(u) = val.get::<u32>() {
            map.insert(key_str.to_string(), Value::Number(u.into()));
        } else if let Some(s) = val.get::<String>() {
            map.insert(key_str.to_string(), Value::String(s));
        } else if let Some(list) = val.get::<Vec<String>>() {
            let json_arr = list.into_iter().map(Value::String).collect();
            map.insert(key_str.to_string(), Value::Array(json_arr));
        }
    }

    Value::Object(map)
}

/// Import previously exported JSON values back into a Settings instance.
pub fn restore_settings_from_json(settings: &gio::Settings, json: &Value) {
    if let Value::Object(map) = json {
        let schema = get_test_schema();
        for (key, val) in map {
            if !schema.has_key(key) {
                continue;
            }
            let key_schema = schema.key(key);
            let expected_type = key_schema.value_type();
            let type_str = expected_type.as_str();

            match val {
                Value::Bool(b) if type_str == "b" => {
                    let _ = settings.set_boolean(key, *b);
                }
                Value::Number(n) if type_str == "i" => {
                    if let Some(i) = n.as_i64() {
                        let _ = settings.set_int(key, i as i32);
                    }
                }
                Value::Number(n) if type_str == "u" => {
                    if let Some(u) = n.as_u64() {
                        let _ = settings.set_uint(key, u as u32);
                    }
                }
                Value::String(s) if type_str == "s" => {
                    let _ = settings.set_string(key, s);
                }
                Value::Array(arr) if type_str == "as" => {
                    let strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                    let _ = settings.set_strv(key, &*strs);
                }
                _ => {}
            }
        }
    }
}
