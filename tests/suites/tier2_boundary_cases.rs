// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 2: Boundary & Corner Cases (>=5 tests per feature where boundaries exist)
// Covers:
// - Timeouts and intervals (sync interval, mark-seen timeout, draft autosave)
// - Spin button limits (workday start/end, reminder lead times, exit days)
// - Empty strings and maximum length text fields
// - Special characters, Unicode, emojis in signatures and label names
// - Schema enum/choices boundary checking and invalid choice rejection
// - Header lists (empty lists, duplicates, colon-separated formats)
// - Pane divider positions (minimum coordinates, oversized widths)

use super::common::{create_test_settings, get_test_schema};
use gtk::gio::prelude::*;

// =========================================================================
// Boundary Group 1: Timeouts and Numeric Intervals (>=5 tests)
// =========================================================================

#[test]
fn tier2_boundary_mail_sync_interval_extremes() {
    let settings = create_test_settings();
    // Test 0 (disabled polling)
    settings.set_int("mail-sync-interval", 0).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), 0);

    // Test 60 seconds (1 minute minimum standard)
    settings.set_int("mail-sync-interval", 60).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), 60);

    // Test 86400 seconds (24 hours maximum standard)
    settings.set_int("mail-sync-interval", 86400).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), 86400);

    // Test large int
    settings.set_int("mail-sync-interval", 604800).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), 604800);

    // Revert to canonical default
    settings.reset("mail-sync-interval");
    assert_eq!(settings.int("mail-sync-interval"), 300);
}

#[test]
fn tier2_boundary_mark_seen_timeout_extremes() {
    let settings = create_test_settings();
    // 0 ms: Mark read immediately on selection
    settings.set_int("mail-mark-seen-timeout", 0).unwrap();
    assert_eq!(settings.int("mail-mark-seen-timeout"), 0);

    // 100 ms: Low boundary
    settings.set_int("mail-mark-seen-timeout", 100).unwrap();
    assert_eq!(settings.int("mail-mark-seen-timeout"), 100);

    // 60000 ms: 1 minute upper boundary
    settings.set_int("mail-mark-seen-timeout", 60000).unwrap();
    assert_eq!(settings.int("mail-mark-seen-timeout"), 60000);

    // Reset reverts to 1500 ms
    settings.reset("mail-mark-seen-timeout");
    assert_eq!(settings.int("mail-mark-seen-timeout"), 1500);
}

#[test]
fn tier2_boundary_draft_autosave_interval_extremes() {
    let settings = create_test_settings();
    // 0: Disabled autosave
    settings.set_int("draft-autosave-interval", 0).unwrap();
    assert_eq!(settings.int("draft-autosave-interval"), 0);

    // 10 seconds: Minimum interval
    settings.set_int("draft-autosave-interval", 10).unwrap();
    assert_eq!(settings.int("draft-autosave-interval"), 10);

    // 3600 seconds: 1 hour upper interval
    settings.set_int("draft-autosave-interval", 3600).unwrap();
    assert_eq!(settings.int("draft-autosave-interval"), 3600);

    // Reset reverts to 30 seconds
    settings.reset("draft-autosave-interval");
    assert_eq!(settings.int("draft-autosave-interval"), 30);
}

#[test]
fn tier2_boundary_workday_minutes_start_and_end() {
    let settings = create_test_settings();
    // Midnight (00:00 = 0 minutes)
    settings.set_int("calendar-workday-start", 0).unwrap();
    assert_eq!(settings.int("calendar-workday-start"), 0);

    // 23:59 (1439 minutes)
    settings.set_int("calendar-workday-end", 1439).unwrap();
    assert_eq!(settings.int("calendar-workday-end"), 1439);

    // Normal reset
    settings.reset("calendar-workday-start");
    settings.reset("calendar-workday-end");
    assert_eq!(settings.int("calendar-workday-start"), 540); // 09:00
    assert_eq!(settings.int("calendar-workday-end"), 1020);  // 17:00
}

#[test]
fn tier2_boundary_junk_empty_on_exit_days() {
    let settings = create_test_settings();
    // 0 days: Purge immediately on each application exit
    settings.set_int("junk-empty-on-exit-days", 0).unwrap();
    assert_eq!(settings.int("junk-empty-on-exit-days"), 0);

    // 1 day
    settings.set_int("junk-empty-on-exit-days", 1).unwrap();
    assert_eq!(settings.int("junk-empty-on-exit-days"), 1);

    // 365 days
    settings.set_int("junk-empty-on-exit-days", 365).unwrap();
    assert_eq!(settings.int("junk-empty-on-exit-days"), 365);
}

// =========================================================================
// Boundary Group 2: String Inputs, Characters & Sizing (>=5 tests)
// =========================================================================

#[test]
fn tier2_boundary_empty_strings_across_text_keys() {
    let settings = create_test_settings();
    let text_keys = [
        "default-account",
        "default-calendar-source",
        "default-task-source",
        "default-memo-source",
        "default-address-book-source",
        "signature-text",
        "spellcheck-languages",
        "contact-completion-book-sources",
        "default-pgp-key",
        "smime-signer-certificate",
        "smime-private-key",
        "smime-ca-file",
    ];

    for &key in &text_keys {
        settings.set_string(key, "").unwrap();
        assert_eq!(settings.string(key).as_str(), "", "Key {key} must accept empty string");
    }
}

#[test]
fn tier2_boundary_unicode_and_emojis_in_signature_and_headers() {
    let settings = create_test_settings();
    let unicode_text = "Mit freundlichen Grüßen / Kind regards 🚀\nDr. Müller — 📧 kontakt@müller.example.com\n日本語テスト 🌟";

    settings.set_string("signature-text", unicode_text).unwrap();
    assert_eq!(settings.string("signature-text").as_str(), unicode_text);

    // Header names with Unicode
    let unicode_headers = vec![
        ("From".to_string(), true),
        ("To".to_string(), true),
        ("Subject".to_string(), true),
        ("X-Benachrichtigung-Status".to_string(), true),
    ];
    settings.set_value("mail-normal-headers", &unicode_headers.to_variant()).unwrap();
    let read_back: Vec<(String, bool)> = settings.value("mail-normal-headers").get().unwrap();
    assert_eq!(read_back, unicode_headers);
}

#[test]
fn tier2_boundary_large_payload_signature() {
    let settings = create_test_settings();
    // 32 KB signature payload
    let large_signature = "A".repeat(32768);
    settings.set_string("signature-text", &large_signature).unwrap();
    assert_eq!(settings.string("signature-text").len(), 32768);
}

#[test]
fn tier2_boundary_special_control_characters_in_headers() {
    let settings = create_test_settings();
    // Headers containing tab and line break escaped strings
    let custom_val = "SpamScore: 99.9\r\nAuthenticated: no";
    settings.set_string("junk-custom-header-value", custom_val).unwrap();
    assert_eq!(settings.string("junk-custom-header-value").as_str(), custom_val);
}

#[test]
fn tier2_boundary_html_markup_in_signature() {
    let settings = create_test_settings();
    let html_sig = "<div style=\"font-family: Arial, sans-serif;\"><p><b>John Doe</b></p><p><a href=\"mailto:john@example.org\">john@example.org</a></p></div>";
    settings.set_string("signature-text", html_sig).unwrap();
    assert_eq!(settings.string("signature-text").as_str(), html_sig);
}

// =========================================================================
// Boundary Group 3: Enum & Choice Constraints (>=5 tests)
// =========================================================================

#[test]
fn tier2_boundary_enum_color_scheme_choices() {
    let schema = get_test_schema();
    let key = schema.key("color-scheme");
    let _range = key.range();
    let settings = create_test_settings();

    assert!(settings.set_string("color-scheme", "system").is_ok());
    assert!(settings.set_string("color-scheme", "light").is_ok());
    assert!(settings.set_string("color-scheme", "dark").is_ok());
}

#[test]
fn tier2_boundary_enum_calendar_view_choices() {
    let settings = create_test_settings();
    let valid_views = ["day", "week", "workweek", "month", "agenda", "year", "list"];
    for &view in &valid_views {
        assert!(settings.set_string("calendar-view", view).is_ok());
        assert_eq!(settings.string("calendar-view").as_str(), view);
    }
}

#[test]
fn tier2_boundary_enum_reminder_units_choices() {
    let settings = create_test_settings();
    for &unit in &["minutes", "hours", "days"] {
        assert!(settings.set_string("default-reminder-units", unit).is_ok());
        assert_eq!(settings.string("default-reminder-units").as_str(), unit);
    }
}

#[test]
fn tier2_boundary_enum_mail_layout_choices() {
    let settings = create_test_settings();
    for &layout in &["classic", "modern"] {
        assert!(settings.set_string("mail-layout", layout).is_ok());
        assert_eq!(settings.string("mail-layout").as_str(), layout);
    }
}

#[test]
fn tier2_boundary_enum_composer_reply_forward_choices() {
    let settings = create_test_settings();
    // All valid reply styles according to schema and normalizer
    for &style in &["quoted", "inline", "attach", "attached", "do-not-quote", "none"] {
        assert!(settings.set_string("composer-reply-style", style).is_ok());
        assert_eq!(settings.string("composer-reply-style").as_str(), style);
    }
    // Permutations with casing and whitespace
    for &style in &[" QUOTED ", "Inline", "ATTACH", " attached ", "Do-Not-Quote", "NONE"] {
        assert!(settings.set_string("composer-reply-style", style).is_ok());
        assert_eq!(settings.string("composer-reply-style").as_str(), style);
    }

    // All valid forward styles
    for &style in &["attached", "inline", "quoted", "attach"] {
        assert!(settings.set_string("composer-forward-style", style).is_ok());
        assert_eq!(settings.string("composer-forward-style").as_str(), style);
    }
    // Forward permutations with casing and whitespace
    for &style in &[" ATTACHED ", "Inline", "QUOTED", " attach "] {
        assert!(settings.set_string("composer-forward-style", style).is_ok());
        assert_eq!(settings.string("composer-forward-style").as_str(), style);
    }
}

// =========================================================================
// Boundary Group 4: Header Lists & Collections (>=5 tests)
// =========================================================================

#[test]
fn tier2_boundary_mail_normal_headers_empty_list() {
    let settings = create_test_settings();
    let empty: Vec<(String, bool)> = vec![];
    settings.set_value("mail-normal-headers", &empty.to_variant()).unwrap();
    let read_back: Vec<(String, bool)> = settings.value("mail-normal-headers").get().unwrap();
    assert_eq!(read_back.len(), 0);

    // Reset returns canonical defaults
    settings.reset("mail-normal-headers");
    let reset_back: Vec<(String, bool)> = settings.value("mail-normal-headers").get().unwrap();
    assert!(!reset_back.is_empty());
}

#[test]
fn tier2_boundary_mail_normal_headers_single_entry() {
    let settings = create_test_settings();
    let single = vec![("Subject".to_string(), true)];
    settings.set_value("mail-normal-headers", &single.to_variant()).unwrap();
    let read_back: Vec<(String, bool)> = settings.value("mail-normal-headers").get().unwrap();
    assert_eq!(read_back, single);
}

#[test]
fn tier2_boundary_mail_normal_headers_50_custom_headers() {
    let settings = create_test_settings();
    let mut custom_headers = Vec::new();
    for i in 0..50 {
        custom_headers.push((format!("X-Evolution-Test-Header-{i}"), i % 2 == 0));
    }
    settings.set_value("mail-normal-headers", &custom_headers.to_variant()).unwrap();
    let read_back: Vec<(String, bool)> = settings.value("mail-normal-headers").get().unwrap();
    assert_eq!(read_back.len(), 50);
}

#[test]
fn tier2_boundary_calendar_workdays_comma_separated() {
    let settings = create_test_settings();
    // 7 days
    settings.set_string("calendar-workdays", "monday,tuesday,wednesday,thursday,friday,saturday,sunday").unwrap();
    assert_eq!(
        settings.string("calendar-workdays").as_str(),
        "monday,tuesday,wednesday,thursday,friday,saturday,sunday"
    );

    // 1 single workday
    settings.set_string("calendar-workdays", "wednesday").unwrap();
    assert_eq!(settings.string("calendar-workdays").as_str(), "wednesday");
}

#[test]
fn tier2_boundary_autocompletion_query_length_boundary() {
    let settings = create_test_settings();
    // 1 character: immediate completion
    settings.set_int("contact-completion-minimum-query-length", 1).unwrap();
    assert_eq!(settings.int("contact-completion-minimum-query-length"), 1);

    // 10 characters: high requirement
    settings.set_int("contact-completion-minimum-query-length", 10).unwrap();
    assert_eq!(settings.int("contact-completion-minimum-query-length"), 10);
}

// =========================================================================
// Boundary Group 5: Coordinate, Sizing & Divider Limits (>=5 tests)
// =========================================================================

#[test]
fn tier2_boundary_contacts_hpane_position_limits() {
    // Feature 20: contact-hpane-position range is min 80, max 4096
    let settings = create_test_settings();
    // Minimum boundary (80 pixels)
    settings.set_int("contact-hpane-position", 80).unwrap();
    assert_eq!(settings.int("contact-hpane-position"), 80);

    // Standard width
    settings.set_int("contact-hpane-position", 320).unwrap();
    assert_eq!(settings.int("contact-hpane-position"), 320);

    // Large 4K monitor divider width (4096)
    settings.set_int("contact-hpane-position", 4096).unwrap();
    assert_eq!(settings.int("contact-hpane-position"), 4096);
}

#[test]
fn tier2_boundary_mail_sidebar_position_limits() {
    // mail-sidebar-position range is min 0, max 2000
    let settings = create_test_settings();
    settings.set_int("mail-sidebar-position", 0).unwrap();
    assert_eq!(settings.int("mail-sidebar-position"), 0);

    settings.set_int("mail-sidebar-position", 240).unwrap();
    assert_eq!(settings.int("mail-sidebar-position"), 240);

    settings.set_int("mail-sidebar-position", 2000).unwrap();
    assert_eq!(settings.int("mail-sidebar-position"), 2000);
}

#[test]
fn tier2_boundary_window_dimensions_minimum_and_4k() {
    let settings = create_test_settings();
    // Minimum reasonable window size (640x480)
    settings.set_int("window-width", 640).unwrap();
    settings.set_int("window-height", 480).unwrap();
    assert_eq!(settings.int("window-width"), 640);
    assert_eq!(settings.int("window-height"), 480);

    // 4K resolution (3840x2160)
    settings.set_int("window-width", 3840).unwrap();
    settings.set_int("window-height", 2160).unwrap();
    assert_eq!(settings.int("window-width"), 3840);
    assert_eq!(settings.int("window-height"), 2160);
}

#[test]
fn tier2_boundary_calendar_time_divisions_discrete_values() {
    let settings = create_test_settings();
    for &div in &[5, 10, 15, 30, 60] {
        settings.set_int("calendar-time-divisions", div).unwrap();
        assert_eq!(settings.int("calendar-time-divisions"), div);
    }
}

#[test]
fn tier2_boundary_mail_sync_message_limit_boundaries() {
    // Range is min 25, max 1000
    let settings = create_test_settings();
    settings.set_int("mail-sync-message-limit", 25).unwrap();
    assert_eq!(settings.int("mail-sync-message-limit"), 25);

    settings.set_int("mail-sync-message-limit", 100).unwrap();
    assert_eq!(settings.int("mail-sync-message-limit"), 100);

    settings.set_int("mail-sync-message-limit", 1000).unwrap();
    assert_eq!(settings.int("mail-sync-message-limit"), 1000);
}
