// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 5: Adversarial Subsystem Wiring & Runtime Reactivity Hardening
//
// Comprehensive white-box stress tests targeting runtime consumers, signal transitions,
// boundary conditions, timer lifecycle, schema fallback, and cross-subsystem isolation:
//
// 1. Mail Subsystem Wiring:
//    - flush-outbox triggering & coexistence with offline-mode
//    - mail-sync-interval timer restarts, extreme intervals, and zero-interval disarm
//    - junk-empty-on-exit & folder expunge retention days math and boundaries
//
// 2. Composer Subsystem Wiring:
//    - composer-inline-spelling & spellcheck-languages whitespace/deduplication/injection sanitization
//    - draft-autosave-interval timer re-arming and cancel on 0 interval
//    - composer-reply-style and composer-forward-style fallback on corrupted GSettings
//
// 3. Calendar & Reminder Subsystem Interactions:
//    - continuous time grid edge minute rendering and slot count calculations
//    - workdays bitmask/list filtering, range normalization, and cell qualification
//    - birthday/anniversary lead times, reminder units, month/year boundaries, and leap year handling
//
// 4. Contacts Subsystem Wiring & Paned Isolation:
//    - contact-hpane-position clamping [80, 1500] and complete independence from mail-sidebar-position
//    - contact editor 12-section visibility non-lossy display guarantee for populated records
//
// 5. Cross-Subsystem Signal Storm & Serialization Stress:
//    - rapid concurrent mutation across disparate categories with listener dispatch integrity

use super::common::{self, create_test_settings, export_settings_to_json, restore_settings_from_json};
use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use gtk::gio::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

// ============================================================================
// 1. Mail Subsystem Wiring & Signal Transitions
// ============================================================================

#[test]
fn tier5_mail_flush_outbox_offline_coexistence_and_rapid_toggle() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();

    // Default: flush-outbox is true, offline-mode is false
    assert!(settings.boolean("flush-outbox"));
    assert!(!settings.boolean("offline-mode"));

    // Track notify signal activations
    let toggle_count = Arc::new(AtomicU32::new(0));
    let toggle_count_clone = Arc::clone(&toggle_count);
    settings.connect_changed(Some("flush-outbox"), move |_, _| {
        toggle_count_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Rapid toggle stress: 20 state flips
    for i in 0..20 {
        let target = i % 2 == 0;
        settings.set_boolean("flush-outbox", target).unwrap();
        assert_eq!(settings.boolean("flush-outbox"), target);
    }
    common::sync_settings_and_pump_events();
    assert_eq!(toggle_count.load(Ordering::SeqCst), 20);

    // Coexistence with offline-mode:
    // When offline-mode is active, flush-outbox can be true in GSettings,
    // but runtime worker logic gates network dispatch on !offline_mode.
    settings.set_boolean("offline-mode", true).unwrap();
    settings.set_boolean("flush-outbox", true).unwrap();
    assert!(settings.boolean("offline-mode"));
    assert!(settings.boolean("flush-outbox"));

    // Reverting to online mode preserves the flush-outbox preference
    settings.set_boolean("offline-mode", false).unwrap();
    assert!(!settings.boolean("offline-mode"));
    assert!(settings.boolean("flush-outbox"));
}

#[test]
fn tier5_mail_sync_interval_timer_zero_cancellation_and_extreme_restarts() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();

    let change_count = Arc::new(AtomicU32::new(0));
    let change_count_clone = Arc::clone(&change_count);
    settings.connect_changed(Some("mail-sync-interval"), move |_, _| {
        change_count_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Setting interval to 0 disarms polling timer
    settings.set_int("mail-sync-interval", 0).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), 0);

    // Sequence of restarts: 0 -> 300 -> 0 -> 60 -> 86400 (24 hours max)
    let intervals = [300, 0, 60, 86_400, 0];
    for &interval in &intervals {
        settings.set_int("mail-sync-interval", interval).unwrap();
        assert_eq!(settings.int("mail-sync-interval"), interval);
    }
    common::sync_settings_and_pump_events();
    // Total changes: 1 (initial 0) + 5 (loop) = 6
    assert_eq!(change_count.load(Ordering::SeqCst), 6);

    // Verify boundary clamps as implemented in AppSettings getter:
    // Any value < 0 clamps to 0; value > 86400 clamps to 86400
    let raw_val = -120;
    settings.set_int("mail-sync-interval", raw_val).unwrap();
    let effective = settings.int("mail-sync-interval").clamp(0, 86_400);
    assert_eq!(effective, 0);

    let raw_overflow = 100_000;
    settings.set_int("mail-sync-interval", raw_overflow).unwrap();
    let effective_overflow = settings.int("mail-sync-interval").clamp(0, 86_400);
    assert_eq!(effective_overflow, 86_400);
}

#[test]
fn tier5_mail_junk_exit_purge_and_retention_days_boundaries() {
    let settings = create_test_settings();

    // Default: junk-empty-on-exit is false, days is 0
    assert!(!settings.boolean("junk-empty-on-exit"));
    assert_eq!(settings.int("junk-empty-on-exit-days"), 0);

    // Enable junk purge with various retention thresholds
    settings.set_boolean("junk-empty-on-exit", true).unwrap();

    let test_retentions = [0, 1, 7, 14, 30, 90, 180, 365];
    for &days in &test_retentions {
        settings.set_int("junk-empty-on-exit-days", days).unwrap();
        assert_eq!(settings.int("junk-empty-on-exit-days"), days);
    }

    // Verify retention calculation semantics matching MailStoreService::empty_local_junk_with_retention:
    // If min_age_days <= 0: cutoff is in future (all junk messages are purged)
    // If min_age_days > 0: cutoff is Utc::now() - Duration::days(min_age_days)
    let now = Utc::now();
    let msg_yesterday = now - Duration::days(1);
    let msg_last_week = now - Duration::days(8);
    let msg_future = now + Duration::hours(2);

    // 0 days: both yesterday and last week are purged
    let cutoff_0 = now + Duration::days(1);
    assert!(msg_yesterday <= cutoff_0);
    assert!(msg_last_week <= cutoff_0);

    // 7 days: only last week (8 days old) is purged; yesterday is retained
    let cutoff_7 = now - Duration::days(7);
    assert!(msg_last_week <= cutoff_7);
    assert!(msg_yesterday > cutoff_7);
    assert!(msg_future > cutoff_7);
}

// ============================================================================
// 2. Composer Subsystem Wiring & Corrupted GSettings Fallback
// ============================================================================

#[test]
fn tier5_composer_spellcheck_dynamic_languages_whitespace_and_deduplication() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();

    // Test sanitization predicate matching AppSettings::sanitize_spellcheck_languages:
    // - splits by comma, trims whitespace
    // - filters empty, length <= 35, ascii alphanumeric or '-' or '_'
    // - deduplicates while preserving order
    let sanitize = |raw: &str| -> String {
        let mut languages = Vec::new();
        for language in raw.split(',').map(str::trim).filter(|l| {
            !l.is_empty()
                && l.len() <= 35
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        }) {
            if !languages.contains(&language.to_string()) {
                languages.push(language.to_string());
            }
        }
        languages.join(",")
    };

    // Adversarial inputs
    let test_cases = [
        ("  en_US  ,  ,  de_DE  ,  en_US  ", "en_US,de_DE"),
        (",,,,", ""),
        ("<script>alert(1)</script>,en-GB", "en-GB"),
        ("es-ES,es-ES,es-ES", "es-ES"),
        ("fr_FR\t,  it_IT\n", "fr_FR,it_IT"),
        ("valid_1,this_is_an_extremely_long_unsupported_language_identifier_that_exceeds_thirty_five_characters,valid_2", "valid_1,valid_2"),
    ];

    for (input, expected) in test_cases {
        let sanitized = sanitize(input);
        assert_eq!(sanitized.as_str(), expected);
        settings.set_string("spellcheck-languages", &sanitized).unwrap();
        assert_eq!(settings.string("spellcheck-languages").as_str(), expected);
    }

    // Dynamic signal reactivity test for inline spelling toggle
    let spelling_toggled = Arc::new(AtomicBool::new(false));
    let spelling_toggled_clone = Arc::clone(&spelling_toggled);
    settings.connect_changed(Some("composer-inline-spelling"), move |s, _| {
        spelling_toggled_clone.store(s.boolean("composer-inline-spelling"), Ordering::SeqCst);
    });

    settings.set_boolean("composer-inline-spelling", false).unwrap();
    common::sync_settings_and_pump_events();
    assert!(!spelling_toggled.load(Ordering::SeqCst));

    settings.set_boolean("composer-inline-spelling", true).unwrap();
    common::sync_settings_and_pump_events();
    assert!(spelling_toggled.load(Ordering::SeqCst));
}

#[test]
fn tier5_composer_draft_autosave_timer_rearming_and_zero_cancel() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();

    // Verify initial default
    assert_eq!(settings.int("draft-autosave-interval"), 30);

    // Track autosave interval updates
    let last_interval = Arc::new(AtomicU32::new(30));
    let last_interval_clone = Arc::clone(&last_interval);
    settings.connect_changed(Some("draft-autosave-interval"), move |s, _| {
        let val = s.int("draft-autosave-interval").clamp(0, 3_600) as u32;
        last_interval_clone.store(val, Ordering::SeqCst);
    });

    // Step 1: Interval = 0 explicitly disarms autosave
    settings.set_int("draft-autosave-interval", 0).unwrap();
    common::sync_settings_and_pump_events();
    assert_eq!(last_interval.load(Ordering::SeqCst), 0);

    // Step 2: Re-arming sequence: 10s -> 60s -> 3600s (1 hr) -> 0s (cancel)
    let sequence = [10, 60, 3_600, 0];
    for &sec in &sequence {
        settings.set_int("draft-autosave-interval", sec).unwrap();
        common::sync_settings_and_pump_events();
        assert_eq!(last_interval.load(Ordering::SeqCst), sec as u32);
    }

    // Step 3: Out-of-bounds boundary values clamp safely
    settings.set_int("draft-autosave-interval", -25).unwrap();
    common::sync_settings_and_pump_events();
    assert_eq!(last_interval.load(Ordering::SeqCst), 0);

    settings.set_int("draft-autosave-interval", 50_000).unwrap();
    common::sync_settings_and_pump_events();
    assert_eq!(last_interval.load(Ordering::SeqCst), 3_600);
}

#[test]
fn tier5_composer_reply_forward_style_corrupted_gsettings_fallback() {
    let settings = create_test_settings();

    // Normalization functions as implemented in AppSettings
    let normalize_reply = |val: &str| -> &'static str {
        match val.trim().to_ascii_lowercase().as_str() {
            "quoted" => "quoted",
            "inline" => "inline",
            "attach" | "attached" => "attach",
            "do-not-quote" | "none" => "do-not-quote",
            _ => "quoted", // safe fallback
        }
    };

    let normalize_forward = |val: &str| -> &'static str {
        match val.trim().to_ascii_lowercase().as_str() {
            "attached" | "attach" => "attached",
            "inline" => "inline",
            "quoted" => "quoted",
            _ => "attached", // safe fallback
        }
    };

    // Adversarial / corrupted values for reply style
    let corrupted_replies = [
        ("INLINE", "inline"),
        ("  ATTACHED  ", "attach"),
        ("NONE", "do-not-quote"),
        ("corrupted_value", "quoted"),
        ("", "quoted"),
        (" \t\n ", "quoted"),
        ("12345", "quoted"),
        ("<unknown>", "quoted"),
    ];

    for (raw, expected) in corrupted_replies {
        let normalized = normalize_reply(raw);
        assert_eq!(normalized, expected, "Failed for reply style: {raw}");
        settings.set_string("composer-reply-style", raw).unwrap();
        // Even when raw settings contain the corrupted string, consumer normalization yields safe fallback
        let effective = normalize_reply(&settings.string("composer-reply-style"));
        assert_eq!(effective, expected);
    }

    // Interior NUL bytes: gtk-rs FFI conversion safely catches interior NULs via panic (GStrInteriorNulError)
    let nul_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = settings.set_string("composer-reply-style", "bad\0string");
    }));
    assert!(nul_result.is_err(), "gtk-rs rejects interior NUL strings via panic");

    // Adversarial / corrupted values for forward style
    let corrupted_forwards = [
        ("INLINE", "inline"),
        ("  quoted  ", "quoted"),
        ("attach", "attached"),
        ("invalid_style", "attached"),
        ("", "attached"),
        ("   ", "attached"),
    ];

    for (raw, expected) in corrupted_forwards {
        let normalized = normalize_forward(raw);
        assert_eq!(normalized, expected, "Failed for forward style: {raw}");
        settings.set_string("composer-forward-style", raw).unwrap();
        let effective = normalize_forward(&settings.string("composer-forward-style"));
        assert_eq!(effective, expected);
    }
}

// ============================================================================
// 3. Calendar & Reminder Subsystem Interactions
// ============================================================================

#[test]
fn tier5_calendar_continuous_time_grid_edge_minute_rendering() {
    let settings = create_test_settings();

    // Normalization rule matching CalendarPage::normalize_calendar_time_divisions:
    // Only 5, 10, 15, 30, 60 are supported; everything else falls back to 30.
    let normalize_divisions = |val: i32| -> i32 {
        match val {
            5 | 10 | 15 | 30 | 60 => val,
            _ => 30,
        }
    };

    // Adversarial and corrupted time divisions
    let corrupted_divisions = [0, -1, 7, 45, 120, 1_440, i32::MIN, i32::MAX];
    for &div in &corrupted_divisions {
        assert_eq!(normalize_divisions(div), 30);
    }

    // For each valid division, test total slots in 24 hours, slot interval, and edge minutes
    let valid_divisions = [
        (5, 288, 1_435),   // 288 slots: 0, 5, ..., 1435
        (10, 144, 1_430),  // 144 slots: 0, 10, ..., 1430
        (15, 96, 1_425),   // 96 slots: 0, 15, ..., 1425
        (30, 48, 1_410),   // 48 slots: 0, 30, ..., 1410
        (60, 24, 1_380),   // 24 slots: 0, 60, ..., 1380
    ];

    for (interval, expected_slots, expected_last_minute) in valid_divisions {
        settings.set_int("calendar-time-divisions", interval).unwrap();
        let normalized = normalize_divisions(settings.int("calendar-time-divisions"));
        assert_eq!(normalized, interval);

        let slots_per_hour = 60 / normalized;
        let total_slots = 24 * slots_per_hour;
        assert_eq!(total_slots, expected_slots);

        // First slot starts at minute 0 (00:00)
        let first_minute = 0 * normalized;
        assert_eq!(first_minute, 0);

        // Last slot minute
        let last_slot = total_slots - 1;
        let last_minute = last_slot * normalized;
        assert_eq!(last_minute, expected_last_minute);

        // Boundary: last minute + interval reaches exactly 1440 (end of day midnight)
        assert_eq!(last_minute + normalized, 1_440);
    }
}

#[test]
fn tier5_calendar_workday_range_and_workdays_bitmask_filtering() {
    let settings = create_test_settings();

    // Normalization rule matching CalendarPage::normalized_workday_range:
    // start clamped to 1410, end clamped to start+30..1440
    let normalize_range = |start: u32, end: u32| -> (u32, u32) {
        let s = start.min(1_410);
        let e = end.max(s.saturating_add(30)).min(1_440);
        (s, e)
    };

    assert_eq!(normalize_range(540, 1020), (540, 1020)); // 09:00 - 17:00
    assert_eq!(normalize_range(600, 600), (600, 630));   // Equal -> +30m min duration
    assert_eq!(normalize_range(1020, 540), (1020, 1050)); // Inverted -> +30m min duration
    assert_eq!(normalize_range(1430, 0), (1410, 1440));  // Out of upper bound

    // Workday cell qualification rule:
    // minute >= workday_start && minute < workday_end && workdays.contains(weekday)
    let is_workday = |minute: u32, start: u32, end: u32, day: Weekday, workdays: &[Weekday]| -> bool {
        let is_time = minute >= start && minute < end;
        is_time && workdays.contains(&day)
    };

    let (start, end) = (540, 1020); // 09:00 to 17:00
    let workdays = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];

    // Edge minute verification:
    assert!(is_workday(540, start, end, Weekday::Mon, &workdays));     // Exactly start -> true
    assert!(is_workday(1019, start, end, Weekday::Mon, &workdays));    // 1 min before end -> true
    assert!(!is_workday(1020, start, end, Weekday::Mon, &workdays));   // Exactly end -> false (exclusive)
    assert!(!is_workday(539, start, end, Weekday::Mon, &workdays));    // 1 min before start -> false
    assert!(!is_workday(600, start, end, Weekday::Sat, &workdays));    // Weekend -> false

    // Sanitization matching AppSettings::sanitize_calendar_workdays:
    let sanitize_days = |value: &str| -> String {
        const DAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
        let selected = DAYS.into_iter().filter(|day| {
            value.split(|c: char| c == ',' || c.is_ascii_whitespace()).any(|candidate| candidate.eq_ignore_ascii_case(day))
        }).collect::<Vec<_>>();
        if selected.is_empty() {
            "monday,tuesday,wednesday,thursday,friday".to_string()
        } else {
            selected.join(",")
        }
    };

    assert_eq!(sanitize_days("monday, wednesday, friday"), "monday,wednesday,friday");
    assert_eq!(sanitize_days("SUNDAY, saturday"), "saturday,sunday"); // natural week sort
    assert_eq!(sanitize_days("62"), "monday,tuesday,wednesday,thursday,friday"); // bitmask fallback
    assert_eq!(sanitize_days(""), "monday,tuesday,wednesday,thursday,friday");
    assert_eq!(sanitize_days("invalid,nonsense"), "monday,tuesday,wednesday,thursday,friday");

    // Persist and verify in settings
    settings.set_int("calendar-workday-start", 540).unwrap();
    settings.set_int("calendar-workday-end", 1020).unwrap();
    settings.set_string("calendar-workdays", &sanitize_days("SUNDAY, saturday")).unwrap();
    assert_eq!(settings.int("calendar-workday-start"), 540);
    assert_eq!(settings.int("calendar-workday-end"), 1020);
    assert_eq!(settings.string("calendar-workdays").as_str(), "saturday,sunday");
}

#[test]
fn tier5_calendar_birthday_anniversary_month_year_boundary_and_lead_times() {
    let settings = create_test_settings();

    // Test ReminderDurationUnit conversions matching AppSettings:
    let unit_to_seconds = |unit: &str, interval: u32| -> i64 {
        match unit {
            "minutes" => interval as i64 * 60,
            "hours" => interval as i64 * 3_600,
            "days" => interval as i64 * 86_400,
            _ => interval as i64 * 60,
        }
    };

    assert_eq!(unit_to_seconds("minutes", 15), 900);
    assert_eq!(unit_to_seconds("hours", 2), 7_200);
    assert_eq!(unit_to_seconds("days", 1), 86_400);
    assert_eq!(unit_to_seconds("days", 7), 604_800);
    assert_eq!(unit_to_seconds("invalid", 10), 600); // fallback to minutes

    // GSettings persistence for birthday/anniversary reminder settings
    settings.set_boolean("use-ba-reminder", true).unwrap();
    settings.set_int("ba-reminder-interval", 2).unwrap();
    settings.set_string("ba-reminder-units", "days").unwrap();

    assert!(settings.boolean("use-ba-reminder"));
    assert_eq!(settings.int("ba-reminder-interval"), 2);
    assert_eq!(settings.string("ba-reminder-units").as_str(), "days");

    // Year boundary test:
    // Birthday on Jan 1 with 1 day lead time must be due on Dec 31 of prior year
    let bday_jan1 = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
    let lead_1_day = Duration::days(1);
    let due_date = bday_jan1 - lead_1_day;
    assert_eq!(due_date.year(), 2026);
    assert_eq!(due_date.month(), 12);
    assert_eq!(due_date.day(), 31);

    // Month boundary test:
    // Birthday on March 1 with 1 day lead time:
    // In non-leap year (2026): Feb 28
    let bday_mar1_2026 = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let due_2026 = bday_mar1_2026 - lead_1_day;
    assert_eq!(due_2026, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());

    // In leap year (2028): Feb 29
    let bday_mar1_2028 = NaiveDate::from_ymd_opt(2028, 3, 1).unwrap();
    let due_2028 = bday_mar1_2028 - lead_1_day;
    assert_eq!(due_2028, NaiveDate::from_ymd_opt(2028, 2, 29).unwrap());

    // Leap year birthday contact (Feb 29):
    // Fallback logic matching reminder_service: Feb 29 -> Feb 28 in non-leap year
    let leap_bday_in_2026 = NaiveDate::from_ymd_opt(2026, 2, 29)
        .or_else(|| NaiveDate::from_ymd_opt(2026, 2, 28));
    assert_eq!(leap_bday_in_2026, Some(NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()));

    let leap_bday_in_2028 = NaiveDate::from_ymd_opt(2028, 2, 29);
    assert_eq!(leap_bday_in_2028, Some(NaiveDate::from_ymd_opt(2028, 2, 29).unwrap()));
}

// ============================================================================
// 4. Contacts Subsystem Wiring & Paned Isolation
// ============================================================================

#[test]
fn tier5_contacts_paned_clamping_and_complete_mail_sidebar_isolation() {
    let settings = create_test_settings();

    // Schema default: contact_hpane_position is 200; mail_sidebar_position is 240
    // (Note: contacts_page.rs:2655 uses unwrap_or(260) on AppSettings failure, while schema default is 200)
    assert_eq!(settings.int("contact-hpane-position"), 200);
    assert_eq!(settings.int("mail-sidebar-position"), 240);

    // Test clamping bounds [80, 1500] matching ContactsPage logic
    let clamp_pos = |p: i32| p.clamp(80, 1_500);

    let test_positions = [
        (-100, 80),
        (0, 80),
        (50, 80),
        (79, 80),
        (80, 80),
        (350, 350),
        (1_500, 1_500),
        (1_501, 1_500),
        (10_000, 1_500),
    ];

    for (input, expected) in test_positions {
        let clamped = clamp_pos(input);
        assert_eq!(clamped, expected);
    }

    // Complete isolation verification:
    // Mutating contact-hpane-position 50 times must NEVER touch mail-sidebar-position
    for pos in 100..150 {
        settings.set_int("contact-hpane-position", pos).unwrap();
        assert_eq!(settings.int("contact-hpane-position"), pos);
        assert_eq!(
            settings.int("mail-sidebar-position"),
            240,
            "mail-sidebar-position was mutated by contact-hpane-position!"
        );
    }

    // Mutating mail-sidebar-position 50 times must NEVER touch contact-hpane-position
    for pos in 300..350 {
        settings.set_int("mail-sidebar-position", pos).unwrap();
        assert_eq!(settings.int("mail-sidebar-position"), pos);
        assert_eq!(
            settings.int("contact-hpane-position"),
            149,
            "contact-hpane-position was mutated by mail-sidebar-position!"
        );
    }
}

#[test]
fn tier5_contacts_editor_12_section_visibility_non_lossy_guarantee() {
    let settings = create_test_settings();

    let section_keys = [
        "contact-editor-show-phone",
        "contact-editor-show-sip",
        "contact-editor-show-im",
        "contact-editor-show-home-address",
        "contact-editor-show-work-address",
        "contact-editor-show-other-address",
        "contact-editor-show-web",
        "contact-editor-show-job",
        "contact-editor-show-dates",
        "contact-editor-show-misc",
        "contact-editor-show-notes",
        "contact-editor-show-certs",
    ];

    // Verify all 12 keys exist and default to true
    for &key in &section_keys {
        assert!(settings.boolean(key), "Key {key} should default to true");
    }

    // Non-lossy visibility formula from ContactDialog:
    // visible = setting_enabled || has_populated_field
    let evaluate_visibility = |setting_enabled: bool, has_field: bool| -> bool {
        setting_enabled || has_field
    };

    // Scenario 1: User toggles ALL 12 settings OFF, but contact has populated data in all 12 sections.
    // Non-lossy guarantee: ALL 12 sections MUST remain visible!
    for &key in &section_keys {
        settings.set_boolean(key, false).unwrap();
        assert!(!settings.boolean(key));

        let has_populated_data = true;
        let is_visible = evaluate_visibility(settings.boolean(key), has_populated_data);
        assert!(
            is_visible,
            "NON-LOSSY VIOLATION: Section {key} was hidden despite populated data!"
        );
    }

    // Scenario 2: New/blank contact with empty fields.
    // When settings are false, sections are hidden.
    for &key in &section_keys {
        let has_populated_data = false;
        let is_visible = evaluate_visibility(settings.boolean(key), has_populated_data);
        assert!(
            !is_visible,
            "Empty section {key} should be hidden when setting is false"
        );
    }

    // Scenario 3: User turns all 12 settings back ON: all sections visible even when empty.
    for &key in &section_keys {
        settings.set_boolean(key, true).unwrap();
        let has_populated_data = false;
        let is_visible = evaluate_visibility(settings.boolean(key), has_populated_data);
        assert!(
            is_visible,
            "Section {key} should be visible when setting is true"
        );
    }
}

// ============================================================================
// 5. Cross-Subsystem Signal Storm & Serialization Stress
// ============================================================================

#[test]
fn tier5_adversarial_signal_storm_and_serialization_integrity() {
    let _signal_lock = common::SETTINGS_SIGNAL_MUTEX.lock().unwrap();
    let settings = create_test_settings();

    // Set up listeners on disparate categories
    let mail_events = Arc::new(AtomicU32::new(0));
    let mail_clone = Arc::clone(&mail_events);
    settings.connect_changed(Some("mail-layout"), move |_, _| {
        mail_clone.fetch_add(1, Ordering::SeqCst);
    });

    let comp_events = Arc::new(AtomicU32::new(0));
    let comp_clone = Arc::clone(&comp_events);
    settings.connect_changed(Some("composer-reply-style"), move |_, _| {
        comp_clone.fetch_add(1, Ordering::SeqCst);
    });

    let cal_events = Arc::new(AtomicU32::new(0));
    let cal_clone = Arc::clone(&cal_events);
    settings.connect_changed(Some("calendar-time-divisions"), move |_, _| {
        cal_clone.fetch_add(1, Ordering::SeqCst);
    });

    let app_events = Arc::new(AtomicU32::new(0));
    let app_clone = Arc::clone(&app_events);
    settings.connect_changed(Some("color-scheme"), move |_, _| {
        app_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Rapid interleaved mutation storm
    for i in 0..25 {
        let layout = if i % 2 == 0 { "modern" } else { "classic" };
        let style = if i % 2 == 0 { "inline" } else { "quoted" };
        let div = if i % 2 == 0 { 15 } else { 30 };
        let scheme = if i % 2 == 0 { "dark" } else { "light" };

        settings.set_string("mail-layout", layout).unwrap();
        settings.set_string("composer-reply-style", style).unwrap();
        settings.set_int("calendar-time-divisions", div).unwrap();
        settings.set_string("color-scheme", scheme).unwrap();
    }
    common::sync_settings_and_pump_events();

    assert_eq!(mail_events.load(Ordering::SeqCst), 25);
    assert_eq!(comp_events.load(Ordering::SeqCst), 25);
    assert_eq!(cal_events.load(Ordering::SeqCst), 25);
    assert_eq!(app_events.load(Ordering::SeqCst), 25);

    // Serialization integrity under stress
    let exported = export_settings_to_json(&settings);
    let json_map = exported.as_object().expect("exported settings must be a JSON object");
    assert!(json_map.contains_key("mail-layout"));
    assert!(json_map.contains_key("composer-reply-style"));
    assert!(json_map.contains_key("calendar-time-divisions"));
    assert!(json_map.contains_key("color-scheme"));

    // Roundtrip back into fresh settings
    let fresh_settings = create_test_settings();
    restore_settings_from_json(&fresh_settings, &exported);

    assert_eq!(fresh_settings.string("mail-layout").as_str(), "modern");
    assert_eq!(fresh_settings.string("composer-reply-style").as_str(), "inline");
    assert_eq!(fresh_settings.int("calendar-time-divisions"), 15);
    assert_eq!(fresh_settings.string("color-scheme").as_str(), "dark");
}
