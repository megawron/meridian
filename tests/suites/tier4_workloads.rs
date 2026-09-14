// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 4: Real-World Application Workloads
// Covers:
// - Initial onboarding workflow (fresh profile verification)
// - Power-user multi-category customization session
// - Full factory reset roundtrip (Feature 2 verification without GLib assertion failures)
// - Backup & restore JSON serialization / deserialization roundtrip
// - Multi-subsystem dynamic change notification stress test

use super::common::{
    self, LEGACY_KEYS_MUST_NOT_EXIST, create_test_settings, export_settings_to_json, get_test_schema,
    restore_settings_from_json,
};
use gtk::gio::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn tier4_workload_initial_onboarding_and_defaults_validation() {
    let settings = create_test_settings();

    // 1. Initial view is mail
    assert_eq!(settings.string("active-view").as_str(), "mail");

    // 2. Mail defaults
    assert_eq!(settings.boolean("mail-mark-seen"), true);
    assert_eq!(settings.int("mail-mark-seen-timeout"), 1500);
    assert_eq!(settings.string("mail-layout").as_str(), "classic");
    assert_eq!(settings.string("mail-row-density").as_str(), "compact");
    assert_eq!(settings.boolean("flush-outbox"), true);
    assert_eq!(settings.int("mail-sync-interval"), 300);

    // 3. Composer defaults
    assert_eq!(settings.boolean("composer-html"), true);
    assert_eq!(settings.string("composer-charset").as_str(), "UTF-8");
    assert_eq!(settings.int("draft-autosave-interval"), 30);
    assert_eq!(settings.boolean("composer-inline-spelling"), true);

    // 4. Calendar defaults
    assert_eq!(settings.string("calendar-view").as_str(), "month");
    assert_eq!(settings.string("calendar-week-start").as_str(), "monday");
    assert_eq!(settings.int("calendar-time-divisions"), 30);
    assert_eq!(settings.int("calendar-workday-start"), 540); // 09:00
    assert_eq!(settings.int("calendar-workday-end"), 1020);  // 17:00

    // 5. Contacts defaults
    assert_eq!(settings.int("contact-hpane-position"), 200);
    assert_eq!(settings.boolean("contact-preview-visible"), true);
    assert_eq!(settings.string("contact-open-map-target").as_str(), "openstreetmap");

    // 6. Appearance & System defaults
    assert_eq!(settings.string("color-scheme").as_str(), "system");
    assert_eq!(settings.boolean("confirm-destructive-actions"), true);
    assert_eq!(settings.boolean("offline-mode"), false);
}

#[test]
fn tier4_workload_power_user_multi_category_session() {
    let settings = create_test_settings();

    // Step 1: Customize Mail
    settings.set_string("mail-layout", "modern").unwrap();
    settings.set_string("mail-row-density", "comfortable").unwrap();
    settings.set_boolean("mail-show-deleted", true).unwrap();
    settings.set_boolean("mail-show-all-headers", true).unwrap();
    settings.set_boolean("mail-show-mark-read-actions", true).unwrap();
    settings.set_int("mail-sync-interval", 120).unwrap();
    settings.set_boolean("junk-empty-on-exit", true).unwrap();
    settings.set_int("junk-empty-on-exit-days", 3).unwrap();

    // Step 2: Customize Composer
    settings.set_boolean("composer-html", false).unwrap();
    settings.set_string("composer-charset", "ISO-8859-1").unwrap();
    settings.set_int("draft-autosave-interval", 15).unwrap();
    settings.set_string("composer-reply-style", "inline").unwrap();
    settings.set_string("composer-forward-style", "inline").unwrap();
    settings.set_boolean("composer-top-signature", true).unwrap();
    settings.set_string("signature-text", "-- \nPower User").unwrap();

    // Step 3: Customize Calendar
    settings.set_string("calendar-view", "workweek").unwrap();
    settings.set_string("calendar-week-start", "sunday").unwrap();
    settings.set_int("calendar-workday-start", 480).unwrap();
    settings.set_int("calendar-workday-end", 1140).unwrap();
    settings.set_boolean("calendar-compress-weekend", false).unwrap();
    settings.set_boolean("use-ba-reminder", true).unwrap();
    settings.set_int("ba-reminder-interval", 2).unwrap();
    settings.set_string("ba-reminder-units", "days").unwrap();

    // Step 4: Customize Contacts
    settings.set_int("contact-hpane-position", 500).unwrap();
    settings.set_string("contact-open-map-target", "google").unwrap();
    settings.set_boolean("contact-preview-home-before-work", false).unwrap();
    settings.set_boolean("contact-editor-show-phone", true).unwrap();
    settings.set_boolean("contact-editor-show-notes", false).unwrap();

    // Step 5: Customize Appearance & Network
    settings.set_string("color-scheme", "dark").unwrap();
    settings.set_string("mail-font-family", "Monospace").unwrap();
    settings.set_int("mail-font-size", 12).unwrap();
    settings.set_boolean("confirm-destructive-actions", false).unwrap();

    // Step 6: Verify ALL customized settings retain exact values
    assert_eq!(settings.string("mail-layout").as_str(), "modern");
    assert_eq!(settings.string("mail-row-density").as_str(), "comfortable");
    assert_eq!(settings.boolean("mail-show-deleted"), true);
    assert_eq!(settings.boolean("mail-show-all-headers"), true);
    assert_eq!(settings.boolean("mail-show-mark-read-actions"), true);
    assert_eq!(settings.int("mail-sync-interval"), 120);
    assert_eq!(settings.boolean("composer-html"), false);
    assert_eq!(settings.string("composer-charset").as_str(), "ISO-8859-1");
    assert_eq!(settings.int("draft-autosave-interval"), 15);
    assert_eq!(settings.string("calendar-view").as_str(), "workweek");
    assert_eq!(settings.string("calendar-week-start").as_str(), "sunday");
    assert_eq!(settings.int("contact-hpane-position"), 500);
    assert_eq!(settings.string("color-scheme").as_str(), "dark");
    assert_eq!(settings.boolean("confirm-destructive-actions"), false);
}

#[test]
fn tier4_workload_factory_reset_integrity_across_all_keys() {
    // Feature 2: Clean GSettings Reset Undefined Keys
    // Mutate various keys, reset them, verify zero assertion errors and full default restoration
    let schema = get_test_schema();
    let settings = create_test_settings();

    // Mutate diverse settings
    settings.set_string("active-view", "calendar").unwrap();
    settings.set_string("color-scheme", "dark").unwrap();
    settings.set_boolean("offline-mode", true).unwrap();
    settings.set_int("mail-sync-interval", 900).unwrap();
    settings.set_int("contact-hpane-position", 600).unwrap();
    settings.set_boolean("confirm-destructive-actions", false).unwrap();

    // Perform reset across all schema keys
    let keys = schema.list_keys();
    for key in &keys {
        settings.reset(key.as_str());
    }

    // Verify reset to canonical defaults
    assert_eq!(settings.string("active-view").as_str(), "mail");
    assert_eq!(settings.string("color-scheme").as_str(), "system");
    assert_eq!(settings.boolean("offline-mode"), false);
    assert_eq!(settings.int("mail-sync-interval"), 300);
    assert_eq!(settings.int("contact-hpane-position"), 200);
    assert_eq!(settings.boolean("confirm-destructive-actions"), true);

    // Verify none of the 12 legacy keys triggered any schema failures
    for &legacy in LEGACY_KEYS_MUST_NOT_EXIST {
        assert!(!schema.has_key(legacy));
    }
}

#[test]
fn tier4_workload_backup_and_restore_settings_serialization_roundtrip() {
    let settings_source = create_test_settings();

    // Mutate state in source
    settings_source.set_string("color-scheme", "dark").unwrap();
    settings_source.set_string("mail-layout", "modern").unwrap();
    settings_source.set_int("mail-sync-interval", 420).unwrap();
    settings_source.set_boolean("composer-html", false).unwrap();
    settings_source.set_string("composer-charset", "Windows-1252").unwrap();
    settings_source.set_string("signature-text", "Backup Test Signature").unwrap();
    settings_source.set_int("contact-hpane-position", 480).unwrap();

    // 1. Export settings to JSON backup
    let backup_json = export_settings_to_json(&settings_source);

    // 2. Create target instance (fresh settings)
    let settings_target = create_test_settings();
    assert_eq!(settings_target.string("color-scheme").as_str(), "system");
    assert_eq!(settings_target.string("mail-layout").as_str(), "classic");
    assert_eq!(settings_target.int("mail-sync-interval"), 300);

    // 3. Restore backup into target
    restore_settings_from_json(&settings_target, &backup_json);

    // 4. Verify target now matches source exactly
    assert_eq!(settings_target.string("color-scheme").as_str(), "dark");
    assert_eq!(settings_target.string("mail-layout").as_str(), "modern");
    assert_eq!(settings_target.int("mail-sync-interval"), 420);
    assert_eq!(settings_target.boolean("composer-html"), false);
    assert_eq!(settings_target.string("composer-charset").as_str(), "Windows-1252");
    assert_eq!(settings_target.string("signature-text").as_str(), "Backup Test Signature");
    assert_eq!(settings_target.int("contact-hpane-position"), 480);
}

#[test]
fn tier4_workload_dynamic_signal_dispatch_multi_listener() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();
    let counter_theme = Rc::new(RefCell::new(0));
    let counter_layout = Rc::new(RefCell::new(0));
    let counter_offline = Rc::new(RefCell::new(0));

    let c_theme = counter_theme.clone();
    settings.connect_changed(Some("color-scheme"), move |_settings, _key| {
        *c_theme.borrow_mut() += 1;
    });

    let c_layout = counter_layout.clone();
    settings.connect_changed(Some("mail-layout"), move |_settings, _key| {
        *c_layout.borrow_mut() += 1;
    });

    let c_offline = counter_offline.clone();
    settings.connect_changed(Some("offline-mode"), move |_settings, _key| {
        *c_offline.borrow_mut() += 1;
    });

    // Fire changes
    settings.set_string("color-scheme", "dark").unwrap();
    settings.set_string("color-scheme", "light").unwrap();
    settings.set_string("mail-layout", "modern").unwrap();
    settings.set_boolean("offline-mode", true).unwrap();
    // Flush pending changes and deliver changed notifications
    common::sync_settings_and_pump_events();

    // Verify listeners received notifications
    assert_eq!(*counter_theme.borrow(), 2);
    assert_eq!(*counter_layout.borrow(), 1);
    assert_eq!(*counter_offline.borrow(), 1);
}
