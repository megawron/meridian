// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 3: Cross-Feature Interactions & Combinations (Pairwise Coverage)
// Covers:
// - Mail layout switching with row density changes
// - Offline mode with background mail sync intervals
// - Color scheme theme changes with font settings
// - HTML composer mode with plain text charset and spell checking
// - Calendar workdays with week start and time divisions
// - Destructive action confirmation with junk purge and folder expunge on exit
// - Contacts pane divider with mail sidebar divider independence
// - Remote image policy with contact address book autocompletion sources
// - Message preview toggle with reading timeout and mark-read actions
// - Calendar default view with reminder intervals and weekend compression

use super::common::create_test_settings;
use gtk::gio::prelude::*;

#[test]
fn tier3_interaction_mail_layout_and_row_density_pairwise() {
    let settings = create_test_settings();
    let layouts = ["classic", "modern"];
    let densities = ["compact", "comfortable", "spacious"];

    for layout in &layouts {
        for density in &densities {
            settings.set_string("mail-layout", layout).unwrap();
            settings.set_string("mail-row-density", density).unwrap();

            assert_eq!(settings.string("mail-layout").as_str(), *layout);
            assert_eq!(settings.string("mail-row-density").as_str(), *density);
        }
    }
}

#[test]
fn tier3_interaction_offline_mode_and_sync_interval() {
    let settings = create_test_settings();

    // Online mode with active polling
    settings.set_boolean("offline-mode", false).unwrap();
    settings.set_int("mail-sync-interval", 180).unwrap();
    settings.set_int("mail-sync-message-limit", 500).unwrap();
    assert_eq!(settings.boolean("offline-mode"), false);
    assert_eq!(settings.int("mail-sync-interval"), 180);

    // Switch to offline mode: settings remain stored and uncorrupted
    settings.set_boolean("offline-mode", true).unwrap();
    assert_eq!(settings.boolean("offline-mode"), true);
    assert_eq!(settings.int("mail-sync-interval"), 180);
    assert_eq!(settings.int("mail-sync-message-limit"), 500);

    // Modifying intervals while offline preserves state for when reconnecting
    settings.set_int("mail-sync-interval", 600).unwrap();
    settings.set_boolean("offline-mode", false).unwrap();
    assert_eq!(settings.boolean("offline-mode"), false);
    assert_eq!(settings.int("mail-sync-interval"), 600);
}

#[test]
fn tier3_interaction_color_scheme_and_font_settings() {
    let settings = create_test_settings();

    // Dark mode + custom monospace font
    settings.set_string("color-scheme", "dark").unwrap();
    settings.set_string("mail-font-family", "Monospace").unwrap();
    settings.set_int("mail-font-size", 12).unwrap();

    assert_eq!(settings.string("color-scheme").as_str(), "dark");
    assert_eq!(settings.string("mail-font-family").as_str(), "Monospace");
    assert_eq!(settings.int("mail-font-size"), 12);

    // Light mode + reset fonts
    settings.set_string("color-scheme", "light").unwrap();
    settings.set_string("mail-font-family", "Sans").unwrap();
    settings.set_int("mail-font-size", 10).unwrap();
    assert_eq!(settings.string("color-scheme").as_str(), "light");
    assert_eq!(settings.string("mail-font-family").as_str(), "Sans");
    assert_eq!(settings.int("mail-font-size"), 10);
}

#[test]
fn tier3_interaction_composer_html_and_charset_and_spellcheck() {
    let settings = create_test_settings();

    // Plain text mode with ISO-8859-1 charset and German/English spellcheck
    settings.set_boolean("composer-html", false).unwrap();
    settings.set_string("composer-charset", "ISO-8859-1").unwrap();
    settings.set_boolean("composer-inline-spelling", true).unwrap();
    settings.set_string("spellcheck-languages", "de_DE,en_US").unwrap();

    assert_eq!(settings.boolean("composer-html"), false);
    assert_eq!(settings.string("composer-charset").as_str(), "ISO-8859-1");
    assert_eq!(settings.boolean("composer-inline-spelling"), true);
    assert_eq!(settings.string("spellcheck-languages").as_str(), "de_DE,en_US");

    // Rich HTML mode with UTF-8 charset
    settings.set_boolean("composer-html", true).unwrap();
    settings.set_string("composer-charset", "UTF-8").unwrap();
    assert_eq!(settings.boolean("composer-html"), true);
    assert_eq!(settings.string("composer-charset").as_str(), "UTF-8");
}

#[test]
fn tier3_interaction_calendar_workdays_and_week_start() {
    let settings = create_test_settings();

    // Monday start with Mon-Fri workdays
    settings.set_string("calendar-week-start", "monday").unwrap();
    settings.set_string("calendar-workdays", "monday,tuesday,wednesday,thursday,friday").unwrap();
    settings.set_int("calendar-time-divisions", 30).unwrap();
    assert_eq!(settings.string("calendar-week-start").as_str(), "monday");
    assert!(settings.string("calendar-workdays").contains("friday"));

    // Sunday start with Sun-Thu workdays (Middle Eastern workweek)
    settings.set_string("calendar-week-start", "sunday").unwrap();
    settings.set_string("calendar-workdays", "sunday,monday,tuesday,wednesday,thursday").unwrap();
    settings.set_int("calendar-time-divisions", 60).unwrap();
    assert_eq!(settings.string("calendar-week-start").as_str(), "sunday");
    assert!(settings.string("calendar-workdays").contains("sunday"));
    assert!(!settings.string("calendar-workdays").contains("friday"));
}

#[test]
fn tier3_interaction_destructive_confirmation_and_junk_exit_expunge() {
    let settings = create_test_settings();

    // Enable exit junk purge and require confirmation for destructive actions
    settings.set_boolean("junk-empty-on-exit", true).unwrap();
    settings.set_int("junk-empty-on-exit-days", 0).unwrap();
    settings.set_boolean("confirm-destructive-actions", true).unwrap();

    assert_eq!(settings.boolean("junk-empty-on-exit"), true);
    assert_eq!(settings.boolean("confirm-destructive-actions"), true);

    // Disable confirmation (silent expunge)
    settings.set_boolean("confirm-destructive-actions", false).unwrap();
    assert_eq!(settings.boolean("confirm-destructive-actions"), false);
    assert_eq!(settings.boolean("junk-empty-on-exit"), true);
}

#[test]
fn tier3_interaction_contacts_divider_and_mail_sidebar_complete_isolation() {
    // Feature 20: absolute isolation between contact-hpane-position and mail-sidebar-position
    let settings = create_test_settings();

    let mail_positions = [100, 240, 300, 400];
    let contact_positions = [200, 350, 500, 750];

    for (m, c) in mail_positions.iter().zip(contact_positions.iter()) {
        settings.set_int("mail-sidebar-position", *m).unwrap();
        settings.set_int("contact-hpane-position", *c).unwrap();

        assert_eq!(settings.int("mail-sidebar-position"), *m);
        assert_eq!(settings.int("contact-hpane-position"), *c);
    }
}

#[test]
fn tier3_interaction_remote_image_policy_and_completion_sources() {
    let settings = create_test_settings();

    // Set policy to sometimes
    settings.set_boolean("auto-load-remote-images", true).unwrap();
    settings.set_string("mail-image-loading-policy", "sometimes").unwrap();
    settings.set_string("contact-completion-book-sources", "personal,work").unwrap();

    assert_eq!(settings.boolean("auto-load-remote-images"), true);
    assert_eq!(settings.string("mail-image-loading-policy").as_str(), "sometimes");
    assert_eq!(settings.string("contact-completion-book-sources").as_str(), "personal,work");
}

#[test]
fn tier3_interaction_message_preview_and_mark_seen_timeout() {
    let settings = create_test_settings();

    // Preview enabled with 2000ms delay
    settings.set_boolean("show-message-preview", true).unwrap();
    settings.set_boolean("mail-mark-seen", true).unwrap();
    settings.set_int("mail-mark-seen-timeout", 2000).unwrap();

    assert_eq!(settings.boolean("show-message-preview"), true);
    assert_eq!(settings.boolean("mail-mark-seen"), true);
    assert_eq!(settings.int("mail-mark-seen-timeout"), 2000);

    // Preview disabled (mark seen immediately when opened in separate window)
    settings.set_boolean("show-message-preview", false).unwrap();
    settings.set_int("mail-mark-seen-timeout", 0).unwrap();

    assert_eq!(settings.boolean("show-message-preview"), false);
    assert_eq!(settings.int("mail-mark-seen-timeout"), 0);
}

#[test]
fn tier3_interaction_calendar_view_and_reminder_units_and_weekend_compression() {
    let settings = create_test_settings();

    // Month view with compressed weekends and 1-day reminder
    settings.set_string("calendar-view", "month").unwrap();
    settings.set_boolean("calendar-compress-weekend", true).unwrap();
    settings.set_boolean("use-default-reminder", true).unwrap();
    settings.set_int("default-reminder-interval", 1).unwrap();
    settings.set_string("default-reminder-units", "days").unwrap();

    assert_eq!(settings.string("calendar-view").as_str(), "month");
    assert_eq!(settings.boolean("calendar-compress-weekend"), true);
    assert_eq!(settings.string("default-reminder-units").as_str(), "days");

    // Workweek view without weekend compression and 15-minute reminder
    settings.set_string("calendar-view", "workweek").unwrap();
    settings.set_boolean("calendar-compress-weekend", false).unwrap();
    settings.set_int("default-reminder-interval", 15).unwrap();
    settings.set_string("default-reminder-units", "minutes").unwrap();

    assert_eq!(settings.string("calendar-view").as_str(), "workweek");
    assert_eq!(settings.boolean("calendar-compress-weekend"), false);
    assert_eq!(settings.string("default-reminder-units").as_str(), "minutes");
}
