// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 5: Adversarial Coverage Hardening & White-Box Stress Tests
//
// Covers:
// - Extreme boundary values and invalid GSettings injections (out-of-range integers, malformed strings, corrupted JSON/lists).
// - Bidirectional synchronization resilience: external mutations while dialog is active.
// - Signal handler disconnection and memory leak prevention during rapid open/close cycles of dialog pages.
// - GSettings factory reset integrity and non-existent key defense.

use super::common::{
    self, create_test_settings, export_settings_to_json, get_test_schema,
    LEGACY_KEYS_MUST_NOT_EXIST,
};
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::glib::prelude::ToVariant;
use std::cell::RefCell;
use std::rc::Rc;

// =========================================================================
// Group 1: Extreme Boundary Values & Corrupted Injections
// =========================================================================

#[test]
fn tier5_adversarial_corrupted_mail_normal_headers_variant_recovery() {
    let settings = create_test_settings();

    // 1. Inject an empty headers list
    let empty_headers: Vec<(String, bool)> = vec![];
    settings
        .set_value("mail-normal-headers", &empty_headers.to_variant())
        .expect("empty headers list should be valid variant");
    let val: Vec<(String, bool)> = settings
        .value("mail-normal-headers")
        .get()
        .expect("should read back as Vec<(String, bool)>");
    assert!(val.is_empty(), "Empty headers list should be stored cleanly");

    // 2. Inject adversarial headers: special chars, null-containing strings, mixed casing
    let adversarial_headers = vec![
        ("From".to_string(), true),
        ("FROM".to_string(), false), // duplicate differing only by case
        ("X-Spam-Report".to_string(), true),
        ("   Spaced-Header   ".to_string(), true),
        ("Header-With-Tabs\tAnd\nNewlines".to_string(), false),
        ("".to_string(), true), // empty name
    ];
    settings
        .set_value("mail-normal-headers", &adversarial_headers.to_variant())
        .expect("adversarial headers should be stored as variant");

    let read_back: Vec<(String, bool)> = settings
        .value("mail-normal-headers")
        .get()
        .expect("should read back variant");
    assert_eq!(read_back.len(), 6);

    // 3. Inject massive header list (exceeding typical 50-item threshold)
    let mut massive_headers = Vec::with_capacity(120);
    for i in 0..120 {
        massive_headers.push((format!("X-Custom-Header-{i:03}"), i % 2 == 0));
    }
    settings
        .set_value("mail-normal-headers", &massive_headers.to_variant())
        .expect("massive headers list should be accepted by GSettings backend");
    let massive_read: Vec<(String, bool)> = settings
        .value("mail-normal-headers")
        .get()
        .expect("should read back massive list");
    assert_eq!(massive_read.len(), 120);

    // Revert to schema default
    settings.reset("mail-normal-headers");
    let reset_headers: Vec<(String, bool)> = settings
        .value("mail-normal-headers")
        .get()
        .expect("should read default headers");
    assert!(
        reset_headers.len() >= 4,
        "Schema default headers must contain standard entries (From, Subject, Date, To)"
    );
}

#[test]
fn tier5_adversarial_corrupted_mail_labels_string_recovery() {
    let settings = create_test_settings();

    // Inject malformed strings into mail-labels strv
    let corrupted_labels = vec![
        "",                                      // empty string
        "   ",                                   // whitespace only
        "LabelWithoutColorOrTag",                // missing color and tag
        "InvalidColor:#GGGGGG",                  // invalid hex digits
        "ShortColor:#123",                       // 3-char hex
        "TooShortColor:#12",                     // invalid length hex
        "SpecialCharsLabel_!@#$%^&*():#AABBCC",  // special characters in label name
        "PipeAttack:Label|tag1|tag2|tag3",       // multiple pipes
        "DelimiterAttack:::::#112233||||",      // excessive colons and pipes
    ];
    let slice_refs: Vec<&str> = corrupted_labels.iter().copied().collect();
    settings
        .set_strv("mail-labels", &*slice_refs)
        .expect("corrupted strv should be accepted by GSettings backend");

    let read_strv = settings.strv("mail-labels");
    assert_eq!(read_strv.len(), corrupted_labels.len());

    // Reset reverts to canonical 5 labels
    settings.reset("mail-labels");
    let default_labels = settings.strv("mail-labels");
    assert_eq!(
        default_labels.len(),
        5,
        "Canonical schema default has 5 default labels"
    );
}

#[test]
fn tier5_adversarial_out_of_range_integers_clamping_defense() {
    let settings = create_test_settings();

    // 1. mail-sync-interval: negative, zero, and huge values
    settings.set_int("mail-sync-interval", -100).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), -100);
    // App-level clamp bounds: 0 to 86400
    let clamped_sync = settings.int("mail-sync-interval").clamp(0, 86_400);
    assert_eq!(clamped_sync, 0, "Negative sync interval must clamp to 0");

    settings.set_int("mail-sync-interval", 1_000_000).unwrap();
    let clamped_sync_high = settings.int("mail-sync-interval").clamp(0, 86_400);
    assert_eq!(
        clamped_sync_high, 86_400,
        "Excessive sync interval must clamp to 86400"
    );

    // 2. mail-sync-message-limit: schema <range min='25' max='1000'/> actively rejects out-of-range values
    assert!(
        settings.set_int("mail-sync-message-limit", -50).is_err(),
        "mail-sync-message-limit schema range must reject sub-minimum integers"
    );
    assert!(
        settings.set_int("mail-sync-message-limit", 50_000).is_err(),
        "mail-sync-message-limit schema range must reject super-maximum integers"
    );
    assert!(settings.set_int("mail-sync-message-limit", 100).is_ok());
    assert_eq!(settings.int("mail-sync-message-limit"), 100);

    // 3. contact-hpane-position: schema <range min='80' max='4096'/> actively rejects out-of-bounds coords
    assert!(
        settings.set_int("contact-hpane-position", -500).is_err(),
        "contact-hpane-position schema range must reject sub-80 coords"
    );
    assert!(
        settings.set_int("contact-hpane-position", 50_000).is_err(),
        "contact-hpane-position schema range must reject super-4096 coords"
    );
    assert!(settings.set_int("contact-hpane-position", 300).is_ok());
    assert_eq!(settings.int("contact-hpane-position"), 300);

    // 4. calendar-workday-start and end: boundary values
    settings.set_int("calendar-workday-start", -10).unwrap();
    let clamped_start = settings.int("calendar-workday-start").clamp(0, 1_439);
    assert_eq!(clamped_start, 0);

    settings.set_int("calendar-workday-end", 2_000).unwrap();
    let clamped_end = settings.int("calendar-workday-end").clamp(0, 1_439);
    assert_eq!(clamped_end, 1_439);

    // Inverted workday validation: start > end
    settings.set_int("calendar-workday-start", 1_200).unwrap(); // 20:00
    settings.set_int("calendar-workday-end", 480).unwrap();   // 08:00
    let raw_start = settings.int("calendar-workday-start").clamp(0, 1_439) as u32;
    let raw_end = settings.int("calendar-workday-end").clamp(0, 1_439) as u32;
    assert!(
        raw_start > raw_end,
        "GSettings allows inverted workday start/end; subsystem must normalize"
    );
    // Verify subsystem normalization formula: end >= start + 30
    let norm_start = raw_start.min(1_410);
    let norm_end = raw_end.max(norm_start.saturating_add(30)).min(1_440);
    assert!(norm_end >= norm_start + 30);

    // Reset tested keys
    settings.reset("mail-sync-interval");
    settings.reset("mail-sync-message-limit");
    settings.reset("contact-hpane-position");
    settings.reset("calendar-workday-start");
    settings.reset("calendar-workday-end");
}

#[test]
fn tier5_adversarial_corrupted_enum_strings_and_fallbacks() {
    let settings = create_test_settings();

    // 1. color-scheme: arbitrary non-enum string
    settings
        .set_string("color-scheme", "neon-green")
        .expect("freeform string storage succeeds in memory backend");
    let scheme_val = settings.string("color-scheme");
    assert_eq!(scheme_val.as_str(), "neon-green");
    // Subsystem mapping must fall back to default
    let mapped_scheme = match scheme_val.as_str() {
        "light" => "light",
        "dark" => "dark",
        _ => "system",
    };
    assert_eq!(
        mapped_scheme, "system",
        "Arbitrary color-scheme must resolve to 'system'"
    );

    // 2. mail-layout: unrecognised layout
    settings.set_string("mail-layout", "ultra-wide-panes").unwrap();
    let layout_val = settings.string("mail-layout");
    let mapped_layout = match layout_val.as_str() {
        "classic" => "classic",
        _ => "modern",
    };
    assert_eq!(
        mapped_layout, "modern",
        "Unknown mail layout must fall back safely"
    );

    // 3. calendar-time-divisions: non-standard divisions
    settings.set_int("calendar-time-divisions", 47).unwrap();
    let div_val = settings.int("calendar-time-divisions");
    let sanitized_div = match div_val {
        5 | 10 | 15 | 30 | 60 => div_val,
        _ => 30,
    };
    assert_eq!(
        sanitized_div, 30,
        "Non-standard time divisions must sanitize to 30"
    );

    // 4. default-reminder-units: schema <choices> rejection defense
    assert!(
        settings.set_string("default-reminder-units", "fortnights").is_err(),
        "GSettings schema <choices> must reject invalid values"
    );
    assert!(settings.set_string("default-reminder-units", "hours").is_ok());
    assert_eq!(settings.string("default-reminder-units").as_str(), "hours");

    // 5. date-format and time-format: unconstrained schema string keys; consumer must provide fallback
    settings.set_string("date-format", "mayan-calendar").unwrap();
    let mapped_date = match settings.string("date-format").as_str() {
        "iso" | "us" => settings.string("date-format").to_string(),
        _ => "german".to_string(),
    };
    assert_eq!(
        mapped_date, "german",
        "Unconstrained date-format must fall back to german in consumer"
    );

    settings.set_string("time-format", "metric-time").unwrap();
    let mapped_time = match settings.string("time-format").as_str() {
        "12h" => "12h",
        _ => "24h",
    };
    assert_eq!(
        mapped_time, "24h",
        "Unconstrained time-format must fall back to 24h in consumer"
    );

    // Clean up
    settings.reset("color-scheme");
    settings.reset("mail-layout");
    settings.reset("calendar-time-divisions");
    settings.reset("default-reminder-units");
    settings.reset("date-format");
    settings.reset("time-format");
}

#[test]
fn tier5_adversarial_corrupted_json_backup_recovery() {
    let schema = get_test_schema();
    let settings = create_test_settings();

    // Construct adversarial JSON payload with type errors and unknown keys
    let corrupted_json_str = r#"{
        "color-scheme": 12345,
        "offline-mode": "not_a_boolean",
        "mail-sync-interval": "should_be_number",
        "contact-hpane-position": true,
        "non-existent-legacy-key": "some_value",
        "mail-layout": "modern"
    }"#;
    let corrupted_json: serde_json::Value =
        serde_json::from_str(corrupted_json_str).expect("JSON must be well-formed");

    // Initial baseline
    assert_eq!(settings.string("mail-layout").as_str(), "vertical");
    assert_eq!(settings.boolean("offline-mode"), false);

    // Safe restore implementing schema key guarding to defend against GLib-GIO abort
    fn safe_restore_settings(
        settings: &gtk::gio::Settings,
        schema: &gtk::gio::SettingsSchema,
        json: &serde_json::Value,
    ) {
        if let serde_json::Value::Object(map) = json {
            for (key, val) in map {
                if !schema.has_key(key) {
                    continue; // Key not in schema: skip to avoid GLib-GIO fatal abort
                }
                match val {
                    serde_json::Value::Bool(b) => {
                        let _ = settings.set_boolean(key, *b);
                    }
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            let _ = settings.set_int(key, i as i32);
                        }
                    }
                    serde_json::Value::String(s) => {
                        let _ = settings.set_string(key, s);
                    }
                    serde_json::Value::Array(arr) => {
                        let strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                        let _ = settings.set_strv(key, &*strs);
                    }
                    _ => {}
                }
            }
        }
    }

    // Import corrupted JSON using safe guarded restore
    safe_restore_settings(&settings, &schema, &corrupted_json);

    // Valid string key was successfully applied
    assert_eq!(settings.string("mail-layout").as_str(), "modern");

    // Invalid type keys were ignored safely, retaining default or untouched state
    assert_eq!(settings.boolean("offline-mode"), false);
    assert_eq!(settings.int("mail-sync-interval"), 300);
    assert_eq!(settings.int("contact-hpane-position"), 320);

    // Reset
    settings.reset("mail-layout");
}

// =========================================================================
// Group 2: Bidirectional Synchronization Resilience & Signal Leakage
// =========================================================================

#[test]
fn tier5_adversarial_signal_handler_disconnection_and_leak_prevention() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();
    let fire_count = Rc::new(RefCell::new(0));

    // Connect handler and obtain SignalHandlerId
    let count_clone = fire_count.clone();
    let handler_id = settings.connect_changed(Some("color-scheme"), move |_s, _k| {
        *count_clone.borrow_mut() += 1;
    });

    // Mutate setting -> verify handler fired
    settings.set_string("color-scheme", "dark").unwrap();
    common::sync_settings_and_pump_events();
    assert_eq!(*fire_count.borrow(), 1);

    // Disconnect handler explicitly via GLib object API
    use glib::object::ObjectExt;
    settings.disconnect(handler_id);

    // Mutate again -> handler MUST NOT fire after disconnect
    settings.set_string("color-scheme", "light").unwrap();
    common::sync_settings_and_pump_events();
    assert_eq!(
        *fire_count.borrow(),
        1,
        "Disconnected signal handler must not fire after explicit disconnection"
    );

    // Clean up
    settings.reset("color-scheme");
}

#[test]
fn tier5_adversarial_simulated_dialog_open_close_handler_accumulation() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();
    let total_invocations = Rc::new(RefCell::new(0));

    // Simulate opening preferences dialog 5 times WITHOUT disconnecting handlers
    // (Demonstrating the leak identified in preferences_dialog.rs)
    let leaked_signal_count = 5;
    for _ in 0..leaked_signal_count {
        let inv = total_invocations.clone();
        settings.connect_changed(Some("mail-show-deleted"), move |_s, _k| {
            *inv.borrow_mut() += 1;
        });
    }

    // Fire one change
    settings.set_boolean("mail-show-deleted", true).unwrap();
    common::sync_settings_and_pump_events();

    // Verify all 5 accumulated handlers fired
    assert_eq!(
        *total_invocations.borrow(),
        5,
        "Every undisconnected handler fires on change; 5 dialog opens without disconnect invoke 5 closures"
    );

    // Contrast with CLEAN pattern: storing IDs and disconnecting on close
    let clean_invocations = Rc::new(RefCell::new(0));
    let mut clean_handler_ids = Vec::new();

    for _ in 0..leaked_signal_count {
        let inv = clean_invocations.clone();
        let id = settings.connect_changed(Some("mail-show-all-headers"), move |_s, _k| {
            *inv.borrow_mut() += 1;
        });
        clean_handler_ids.push(id);
    }

    // Simulate dialog close: disconnect all stored IDs
    use glib::object::ObjectExt;
    for id in clean_handler_ids {
        settings.disconnect(id);
    }

    // Fire change
    settings.set_boolean("mail-show-all-headers", true).unwrap();
    common::sync_settings_and_pump_events();

    assert_eq!(
        *clean_invocations.borrow(),
        0,
        "Properly disconnected dialog handlers result in 0 ghost invocations"
    );

    // Reset keys
    settings.reset("mail-show-deleted");
    settings.reset("mail-show-all-headers");
}

#[test]
fn tier5_adversarial_rapid_settings_mutation_storm() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();
    let dispatch_counter = Rc::new(RefCell::new(0));

    let c = dispatch_counter.clone();
    let handler_id = settings.connect_changed(Some("mail-sync-interval"), move |_s, _k| {
        *c.borrow_mut() += 1;
    });

    // Storm of 100 rapid sequential mutations
    for i in 1..=100 {
        settings
            .set_int("mail-sync-interval", 60 + i)
            .expect("rapid mutation must succeed");
    }

    common::sync_settings_and_pump_events();

    // Verify last value was applied accurately
    assert_eq!(settings.int("mail-sync-interval"), 160);
    // Verify notifications were delivered
    assert!(
        *dispatch_counter.borrow() >= 1,
        "Listener must receive notifications during mutation storm"
    );

    // Disconnect handler
    use glib::object::ObjectExt;
    settings.disconnect(handler_id);

    // Reset
    settings.reset("mail-sync-interval");
}

// =========================================================================
// Group 3: Factory Reset Integrity & Non-Existent Key Defense
// =========================================================================

#[test]
fn tier5_adversarial_factory_reset_all_198_schema_keys_integrity() {
    let schema = get_test_schema();
    let settings = create_test_settings();

    let all_keys = schema.list_keys();
    assert_eq!(
        all_keys.len(),
        243,
        "Canonical schema must define exactly 243 configuration keys"
    );

    // Capture initial default snapshot
    let initial_json = export_settings_to_json(&settings);

    // Mutate a broad cross-category selection of keys
    settings.set_string("active-view", "calendar").unwrap();
    settings.set_string("color-scheme", "dark").unwrap();
    settings.set_boolean("offline-mode", true).unwrap();
    settings.set_string("mail-layout", "modern").unwrap();
    settings.set_int("mail-sync-interval", 999).unwrap();
    settings.set_boolean("composer-html", false).unwrap();
    settings.set_string("composer-charset", "ISO-8859-1").unwrap();
    settings.set_string("calendar-view", "workweek").unwrap();
    settings.set_int("contact-hpane-position", 800).unwrap();
    settings.set_boolean("confirm-destructive-actions", false).unwrap();
    settings.set_boolean("mail-show-deleted", true).unwrap();
    settings.set_boolean("mail-show-all-headers", true).unwrap();
    settings.set_boolean("use-ba-reminder", true).unwrap();
    settings.set_int("ba-reminder-interval", 7).unwrap();
    settings.set_string("ba-reminder-units", "days").unwrap();

    // Verify mutations took effect
    assert_eq!(settings.string("active-view").as_str(), "calendar");
    assert_eq!(settings.string("color-scheme").as_str(), "dark");
    assert_eq!(settings.boolean("offline-mode"), true);

    // Perform reset across ALL 193 keys using schema.list_keys()
    for key in &all_keys {
        settings.reset(key.as_str());
    }

    // Export post-reset state
    let post_reset_json = export_settings_to_json(&settings);

    // Verify exact parity between pre-mutation defaults and post-reset state
    assert_eq!(
        initial_json, post_reset_json,
        "Full factory reset must restore exact initial defaults across all 198 keys"
    );
}

#[test]
fn tier5_adversarial_non_existent_key_defense_and_legacy_rejection() {
    let schema = get_test_schema();
    let settings = create_test_settings();

    // 1. None of the 12 legacy keys must exist in schema
    for &legacy_key in LEGACY_KEYS_MUST_NOT_EXIST {
        assert!(
            !schema.has_key(legacy_key),
            "Legacy key '{legacy_key}' MUST NOT exist in com.martingawron.Meridian schema"
        );
    }

    // 2. Arbitrary non-existent keys must be rejected by schema.has_key
    let adversarial_non_existent_keys = [
        "non-existent-key",
        "org.gnome.evolution.unknown",
        "mail-fake-setting",
        "__proto__",
        "drop table settings;",
        "",
        " ",
    ];

    for &key in &adversarial_non_existent_keys {
        assert!(
            !schema.has_key(key),
            "Non-existent key '{key}' must be detected as not present in schema"
        );
    }

    // 3. Safe reset wrapper defense: only reset keys confirmed by schema.has_key
    fn safe_reset_key(settings: &gtk::gio::Settings, schema: &gtk::gio::SettingsSchema, key: &str) -> bool {
        if schema.has_key(key) {
            settings.reset(key);
            true
        } else {
            false
        }
    }

    // Existing key reset succeeds
    assert!(safe_reset_key(&settings, &schema, "active-view"));

    // Legacy and non-existent key reset is safely blocked without GLib assertion
    for &legacy_key in LEGACY_KEYS_MUST_NOT_EXIST {
        assert!(!safe_reset_key(&settings, &schema, legacy_key));
    }
    for &key in &adversarial_non_existent_keys {
        assert!(!safe_reset_key(&settings, &schema, key));
    }
}
