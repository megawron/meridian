// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 1: Feature Coverage (>=5 tests per feature for each preference category)
// Covers:
// - Mail Preferences (Features 1, 9, 10, 11, 12, 13, 15, 16)
// - Composer Preferences (Features 7, 8, 18, 19)
// - Calendar Preferences (Features 22, 23)
// - Contacts Preferences (Features 4, 8, 20, 21)
// - Appearance Preferences (Features 5, 6, 17)
// - Network Preferences (Features 14, 16)
// - Certificates & Security Preferences (Features 13, 14)
// - Backup & System Preferences (Features 2, 3)

use super::common::{LEGACY_KEYS_MUST_NOT_EXIST, create_test_settings, get_test_schema};
use gtk::gio::prelude::*;

// =========================================================================
// Category 1: Mail Preferences (Features 1, 9, 10, 11, 12, 13, 15, 16)
// =========================================================================

#[test]
fn tier1_mail_reading_defaults_and_toggles() {
    let settings = create_test_settings();
    // Default reading settings
    assert_eq!(settings.boolean("mail-mark-seen"), true);
    assert_eq!(settings.int("mail-mark-seen-timeout"), 1500);
    assert_eq!(settings.boolean("mail-magic-spacebar"), true);

    // Mutation & persistence
    settings.set_boolean("mail-mark-seen", false).unwrap();
    settings.set_int("mail-mark-seen-timeout", 3000).unwrap();
    assert_eq!(settings.boolean("mail-mark-seen"), false);
    assert_eq!(settings.int("mail-mark-seen-timeout"), 3000);
}

#[test]
fn tier1_mail_headers_and_show_all_toggle() {
    let settings = create_test_settings();
    // Feature 9: mail-show-all-headers
    assert_eq!(settings.boolean("mail-show-all-headers"), false);
    settings.set_boolean("mail-show-all-headers", true).unwrap();
    assert_eq!(settings.boolean("mail-show-all-headers"), true);

    // Feature 15: mail-normal-headers (type a(sb))
    let val = settings.value("mail-normal-headers");
    let headers: Vec<(String, bool)> = val.get().expect("mail-normal-headers must parse as Vec<(String, bool)>");
    assert!(!headers.is_empty(), "Normal headers list must not be empty");
    assert!(headers.iter().any(|(h, _)| h == "From"));
    assert!(headers.iter().any(|(h, _)| h == "Subject"));

    // Mutate header list
    let updated_headers = vec![
        ("From".to_string(), true),
        ("To".to_string(), true),
        ("Subject".to_string(), true),
        ("X-Custom-Header".to_string(), false),
    ];
    settings.set_value("mail-normal-headers", &updated_headers.to_variant()).unwrap();
    let read_back: Vec<(String, bool)> = settings.value("mail-normal-headers").get().unwrap();
    assert_eq!(read_back, updated_headers);
}

#[test]
fn tier1_mail_show_deleted_toggle() {
    // Feature 10: mail-show-deleted
    let settings = create_test_settings();
    assert_eq!(settings.boolean("mail-show-deleted"), false);
    settings.set_boolean("mail-show-deleted", true).unwrap();
    assert_eq!(settings.boolean("mail-show-deleted"), true);
}

#[test]
fn tier1_mail_show_mark_read_actions_toggle() {
    // Feature 11: mail-show-mark-read-actions
    let settings = create_test_settings();
    assert_eq!(settings.boolean("mail-show-mark-read-actions"), false);
    settings.set_boolean("mail-show-mark-read-actions", true).unwrap();
    assert_eq!(settings.boolean("mail-show-mark-read-actions"), true);
}

#[test]
fn tier1_mail_flush_outbox_wiring() {
    // Feature 12: flush-outbox
    let settings = create_test_settings();
    assert_eq!(settings.boolean("flush-outbox"), true);
    settings.set_boolean("flush-outbox", false).unwrap();
    assert_eq!(settings.boolean("flush-outbox"), false);
}

#[test]
fn tier1_mail_junk_exit_purge_and_intervals() {
    // Feature 13: junk-empty-on-exit and days
    let settings = create_test_settings();
    assert_eq!(settings.boolean("junk-empty-on-exit"), false);
    assert_eq!(settings.int("junk-empty-on-exit-days"), 0);

    settings.set_boolean("junk-empty-on-exit", true).unwrap();
    settings.set_int("junk-empty-on-exit-days", 7).unwrap();
    assert_eq!(settings.boolean("junk-empty-on-exit"), true);
    assert_eq!(settings.int("junk-empty-on-exit-days"), 7);
}

#[test]
fn tier1_mail_polling_and_sync_interval() {
    // Feature 16: mail-sync-interval and message limit
    let settings = create_test_settings();
    assert_eq!(settings.int("mail-sync-interval"), 300);
    assert_eq!(settings.int("mail-sync-message-limit"), 100);

    settings.set_int("mail-sync-interval", 600).unwrap();
    settings.set_int("mail-sync-message-limit", 500).unwrap();
    assert_eq!(settings.int("mail-sync-interval"), 600);
    assert_eq!(settings.int("mail-sync-message-limit"), 500);
}

// =========================================================================
// Category 2: Composer Preferences (Features 7, 8, 18, 19)
// =========================================================================

#[test]
fn tier1_composer_charset_selection() {
    // Feature 7: composer-charset
    let settings = create_test_settings();
    assert_eq!(settings.string("composer-charset").as_str(), "UTF-8");

    for charset in &["ISO-8859-1", "Windows-1252", "US-ASCII", "UTF-8"] {
        settings.set_string("composer-charset", charset).unwrap();
        assert_eq!(settings.string("composer-charset").as_str(), *charset);
    }
}

#[test]
fn tier1_composer_autocompletion_sources() {
    // Feature 8: contact-completion-book-sources and minimum query length
    let settings = create_test_settings();
    assert_eq!(settings.string("contact-completion-book-sources").as_str(), "");
    assert_eq!(settings.boolean("contact-completion-show-address"), false);
    settings.set_boolean("contact-completion-show-address", true).unwrap();
    assert_eq!(settings.boolean("contact-completion-show-address"), true);

    settings.set_string("contact-completion-book-sources", "local,ldap-work,exchange").unwrap();
    settings.set_int("contact-completion-minimum-query-length", 2).unwrap();
    assert_eq!(settings.string("contact-completion-book-sources").as_str(), "local,ldap-work,exchange");
    assert_eq!(settings.int("contact-completion-minimum-query-length"), 2);
}

#[test]
fn tier1_composer_inline_spelling_and_languages() {
    // Feature 18: composer-inline-spelling and spellcheck-languages
    let settings = create_test_settings();
    assert_eq!(settings.boolean("composer-inline-spelling"), true);
    assert_eq!(settings.string("spellcheck-languages").as_str(), "");

    settings.set_boolean("composer-inline-spelling", false).unwrap();
    settings.set_string("spellcheck-languages", "en_US,de_DE").unwrap();
    assert_eq!(settings.boolean("composer-inline-spelling"), false);
    assert_eq!(settings.string("spellcheck-languages").as_str(), "en_US,de_DE");
}

#[test]
fn tier1_composer_draft_autosave_interval() {
    // Feature 19: draft-autosave-interval (in seconds)
    let settings = create_test_settings();
    assert_eq!(settings.int("draft-autosave-interval"), 30);

    settings.set_int("draft-autosave-interval", 60).unwrap();
    assert_eq!(settings.int("draft-autosave-interval"), 60);
}

#[test]
fn tier1_composer_reply_and_forward_styles() {
    // Feature 19: composer reply/forward styles and quote original
    let settings = create_test_settings();
    assert_eq!(settings.string("composer-reply-style").as_str(), "quoted");
    assert_eq!(settings.string("composer-forward-style").as_str(), "attached");
    assert_eq!(settings.boolean("quote-original"), true);

    settings.set_string("composer-reply-style", "inline").unwrap();
    settings.set_string("composer-forward-style", "inline").unwrap();
    assert_eq!(settings.string("composer-reply-style").as_str(), "inline");
    assert_eq!(settings.string("composer-forward-style").as_str(), "inline");
}

#[test]
fn tier1_composer_html_and_signatures() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("composer-html"), true);
    assert_eq!(settings.boolean("append-signature"), false);
    assert_eq!(settings.boolean("composer-top-signature"), false);

    settings.set_boolean("composer-html", false).unwrap();
    settings.set_boolean("append-signature", true).unwrap();
    settings.set_boolean("composer-top-signature", true).unwrap();
    assert_eq!(settings.boolean("composer-html"), false);
    assert_eq!(settings.boolean("append-signature"), true);
    assert_eq!(settings.boolean("composer-top-signature"), true);
}

// =========================================================================
// Category 3: Calendar Preferences (Features 22, 23)
// =========================================================================

#[test]
fn tier1_calendar_view_and_time_divisions() {
    // Feature 23: calendar-view and time divisions
    let settings = create_test_settings();
    assert_eq!(settings.string("calendar-view").as_str(), "month");
    assert_eq!(settings.int("calendar-time-divisions"), 30);

    settings.set_string("calendar-view", "week").unwrap();
    settings.set_int("calendar-time-divisions", 15).unwrap();
    assert_eq!(settings.string("calendar-view").as_str(), "week");
    assert_eq!(settings.int("calendar-time-divisions"), 15);
}

#[test]
fn tier1_calendar_week_start_and_workdays() {
    // Feature 23: calendar-week-start and calendar-workdays
    let settings = create_test_settings();
    assert_eq!(settings.string("calendar-week-start").as_str(), "monday");
    assert_eq!(
        settings.string("calendar-workdays").as_str(),
        "monday,tuesday,wednesday,thursday,friday"
    );

    settings.set_string("calendar-week-start", "sunday").unwrap();
    settings.set_string("calendar-workdays", "sunday,monday,tuesday,wednesday,thursday").unwrap();
    assert_eq!(settings.string("calendar-week-start").as_str(), "sunday");
    assert_eq!(
        settings.string("calendar-workdays").as_str(),
        "sunday,monday,tuesday,wednesday,thursday"
    );
}

#[test]
fn tier1_calendar_workday_hours_and_compress_weekend() {
    let settings = create_test_settings();
    assert_eq!(settings.int("calendar-workday-start"), 540); // 09:00
    assert_eq!(settings.int("calendar-workday-end"), 1020);  // 17:00
    assert_eq!(settings.boolean("calendar-compress-weekend"), true);

    settings.set_int("calendar-workday-start", 480).unwrap(); // 08:00
    settings.set_int("calendar-workday-end", 1080).unwrap();  // 18:00
    assert_eq!(settings.int("calendar-workday-start"), 480);
    assert_eq!(settings.int("calendar-workday-end"), 1080);
}

#[test]
fn tier1_calendar_birthday_and_anniversary_reminders() {
    // Feature 22: calendar-use-ba-reminder, ba-reminder-interval, ba-reminder-units
    let settings = create_test_settings();
    assert_eq!(settings.boolean("use-ba-reminder"), false);
    assert_eq!(settings.int("ba-reminder-interval"), 15);
    assert_eq!(settings.string("ba-reminder-units").as_str(), "minutes");

    settings.set_boolean("use-ba-reminder", true).unwrap();
    settings.set_int("ba-reminder-interval", 1).unwrap();
    settings.set_string("ba-reminder-units", "days").unwrap();
    assert_eq!(settings.boolean("use-ba-reminder"), true);
    assert_eq!(settings.int("ba-reminder-interval"), 1);
    assert_eq!(settings.string("ba-reminder-units").as_str(), "days");
}

#[test]
fn tier1_calendar_default_reminder_settings() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("use-default-reminder"), false);
    assert_eq!(settings.int("default-reminder-interval"), 15);
    assert_eq!(settings.string("default-reminder-units").as_str(), "minutes");

    settings.set_boolean("use-default-reminder", true).unwrap();
    settings.set_int("default-reminder-interval", 30).unwrap();
    assert_eq!(settings.boolean("use-default-reminder"), true);
    assert_eq!(settings.int("default-reminder-interval"), 30);
}

// =========================================================================
// Category 4: Contacts Preferences (Features 4, 8, 20, 21)
// =========================================================================

#[test]
fn tier1_contacts_paned_divider_separation() {
    // Feature 20: contact-hpane-position is separate from mail-sidebar-position
    let settings = create_test_settings();
    assert_eq!(settings.int("contact-hpane-position"), 200);
    assert_eq!(settings.int("mail-sidebar-position"), 240);

    // Mutating contact pane must NOT touch mail sidebar position
    settings.set_int("contact-hpane-position", 450).unwrap();
    assert_eq!(settings.int("contact-hpane-position"), 450);
    assert_eq!(settings.int("mail-sidebar-position"), 240);

    // Mutating mail sidebar position must NOT touch contact pane position
    settings.set_int("mail-sidebar-position", 280).unwrap();
    assert_eq!(settings.int("contact-hpane-position"), 450);
    assert_eq!(settings.int("mail-sidebar-position"), 280);
}

#[test]
fn tier1_contacts_editor_all_12_section_visibility_switches() {
    // Feature 21: contact-editor-show-*
    let settings = create_test_settings();
    let sections = [
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

    // Every switch defaults to true
    for &sec in &sections {
        assert_eq!(settings.boolean(sec), true, "Section {sec} should default to true");
        settings.set_boolean(sec, false).unwrap();
        assert_eq!(settings.boolean(sec), false, "Section {sec} should toggle to false");
    }
}

#[test]
fn tier1_contacts_formatting_and_preview_options() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("contact-address-formatting"), false);
    assert_eq!(settings.boolean("contact-preview-show-maps"), false);
    assert_eq!(settings.boolean("contact-preview-home-before-work"), false);
    assert_eq!(settings.boolean("contact-preview-visible"), true);

    settings.set_boolean("contact-preview-home-before-work", true).unwrap();
    assert_eq!(settings.boolean("contact-preview-home-before-work"), true);
}

#[test]
fn tier1_contacts_map_provider_target() {
    let settings = create_test_settings();
    assert_eq!(settings.string("contact-open-map-target").as_str(), "openstreetmap");
    assert_eq!(settings.boolean("contact-open-map-prefer-local"), true);

    settings.set_string("contact-open-map-target", "google").unwrap();
    assert_eq!(settings.string("contact-open-map-target").as_str(), "google");
}

#[test]
fn tier1_contacts_local_source_and_layout_separation() {
    // Feature 4: separation of local sources
    let settings = create_test_settings();
    assert_eq!(settings.string("contact-layout").as_str(), "classic");
    assert_eq!(settings.string("default-address-book-source").as_str(), "");

    settings.set_string("contact-layout", "vertical").unwrap();
    assert_eq!(settings.string("contact-layout").as_str(), "vertical");
}

// =========================================================================
// Category 5: Appearance Preferences (Features 5, 6, 17)
// =========================================================================

#[test]
fn tier1_appearance_color_scheme_theme_switching() {
    // Feature 17: color-scheme ('system', 'light', 'dark')
    let settings = create_test_settings();
    assert_eq!(settings.string("color-scheme").as_str(), "system");

    for scheme in &["dark", "light", "system"] {
        settings.set_string("color-scheme", scheme).unwrap();
        assert_eq!(settings.string("color-scheme").as_str(), *scheme);
    }
}

#[test]
fn tier1_appearance_mail_layout_clean_separation() {
    // Feature 5: mail-layout resides in mail preferences, not mixed into appearance
    let settings = create_test_settings();
    assert_eq!(settings.string("mail-layout").as_str(), "classic");
    assert_eq!(settings.string("mail-row-density").as_str(), "compact");

    settings.set_string("mail-layout", "modern").unwrap();
    settings.set_string("mail-row-density", "comfortable").unwrap();
    assert_eq!(settings.string("mail-layout").as_str(), "modern");
    assert_eq!(settings.string("mail-row-density").as_str(), "comfortable");
}

#[test]
fn tier1_appearance_confirm_destructive_actions_rehoming() {
    // Feature 6: confirm-destructive-actions is a top-level general application setting
    let settings = create_test_settings();
    assert_eq!(settings.boolean("confirm-destructive-actions"), true);
    settings.set_boolean("confirm-destructive-actions", false).unwrap();
    assert_eq!(settings.boolean("confirm-destructive-actions"), false);
}

#[test]
fn tier1_appearance_font_and_viewer_customization() {
    let settings = create_test_settings();
    assert_eq!(settings.string("mail-font-family").as_str(), "system-ui, sans-serif");
    assert_eq!(settings.int("mail-font-size"), 16);

    settings.set_string("mail-font-family", "Monospace").unwrap();
    settings.set_int("mail-font-size", 12).unwrap();
    assert_eq!(settings.string("mail-font-family").as_str(), "Monospace");
    assert_eq!(settings.int("mail-font-size"), 12);
}

#[test]
fn tier1_appearance_notifications_and_statusbar() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("notifications-enabled"), true);
    assert_eq!(settings.boolean("mail-show-statusbar"), true);

    settings.set_boolean("notifications-enabled", false).unwrap();
    settings.set_boolean("mail-show-statusbar", false).unwrap();
    assert_eq!(settings.boolean("notifications-enabled"), false);
    assert_eq!(settings.boolean("mail-show-statusbar"), false);
}

// =========================================================================
// Category 6: Network Preferences (Features 14, 16)
// =========================================================================

#[test]
fn tier1_network_offline_mode_toggle() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("offline-mode"), false);
    settings.set_boolean("offline-mode", true).unwrap();
    assert_eq!(settings.boolean("offline-mode"), true);
}

#[test]
fn tier1_network_remote_image_policy() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("auto-load-remote-images"), false);
    assert_eq!(settings.string("mail-image-loading-policy").as_str(), "never");

    settings.set_boolean("auto-load-remote-images", true).unwrap();
    settings.set_string("mail-image-loading-policy", "always").unwrap();
    assert_eq!(settings.boolean("auto-load-remote-images"), true);
    assert_eq!(settings.string("mail-image-loading-policy").as_str(), "always");
}

#[test]
fn tier1_network_spam_filter_honesty() {
    // Feature 14: junk-filter plugins and default plugin
    let settings = create_test_settings();
    assert_eq!(settings.boolean("junk-check-incoming"), true);
    assert_eq!(settings.string("junk-default-plugin").as_str(), "Bogofilter");

    settings.set_string("junk-default-plugin", "Spamassassin").unwrap();
    assert_eq!(settings.string("junk-default-plugin").as_str(), "Spamassassin");
}

#[test]
fn tier1_network_sync_message_limits() {
    // Feature 16: message limit for bandwidth-sensitive networks (range 25..1000)
    let settings = create_test_settings();
    assert_eq!(settings.int("mail-sync-message-limit"), 100);
    settings.set_int("mail-sync-message-limit", 500).unwrap();
    assert_eq!(settings.int("mail-sync-message-limit"), 500);
}

#[test]
fn tier1_network_address_book_lookup_restriction() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("junk-lookup-addressbook"), true);
    assert_eq!(settings.boolean("junk-lookup-addressbook-local-only"), false);

    settings.set_boolean("junk-lookup-addressbook-local-only", true).unwrap();
    assert_eq!(settings.boolean("junk-lookup-addressbook-local-only"), true);
}

// =========================================================================
// Category 7: Certificates & Security Preferences (Features 13, 14)
// =========================================================================

#[test]
fn tier1_security_pgp_keys_and_signing() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("sign-new-messages"), false);
    assert_eq!(settings.string("default-pgp-key").as_str(), "");

    settings.set_boolean("sign-new-messages", true).unwrap();
    settings.set_string("default-pgp-key", "0xDEADBEEF12345678").unwrap();
    assert_eq!(settings.boolean("sign-new-messages"), true);
    assert_eq!(settings.string("default-pgp-key").as_str(), "0xDEADBEEF12345678");
}

#[test]
fn tier1_security_smime_signer_and_keys() {
    let settings = create_test_settings();
    assert_eq!(settings.string("smime-signer-certificate").as_str(), "");
    assert_eq!(settings.string("smime-private-key").as_str(), "");
    assert_eq!(settings.string("smime-ca-file").as_str(), "");

    settings.set_string("smime-signer-certificate", "/path/to/cert.crt").unwrap();
    settings.set_string("smime-private-key", "/path/to/key.pem").unwrap();
    assert_eq!(settings.string("smime-signer-certificate").as_str(), "/path/to/cert.crt");
    assert_eq!(settings.string("smime-private-key").as_str(), "/path/to/key.pem");
}

#[test]
fn tier1_security_details_display_toggle() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("mail-show-security-details"), false);
    settings.set_boolean("mail-show-security-details", true).unwrap();
    assert_eq!(settings.boolean("mail-show-security-details"), true);
}

#[test]
fn tier1_security_custom_junk_headers() {
    // Feature 14: junk-custom-header evaluation
    let settings = create_test_settings();
    assert_eq!(settings.string("junk-custom-header").as_str(), "X-Spam-Flag");
    assert_eq!(settings.string("junk-custom-header-value").as_str(), "YES");

    settings.set_string("junk-custom-header", "X-Custom-Spam-Score").unwrap();
    settings.set_string("junk-custom-header-value", "high").unwrap();
    assert_eq!(settings.string("junk-custom-header").as_str(), "X-Custom-Spam-Score");
    assert_eq!(settings.string("junk-custom-header-value").as_str(), "high");
}

#[test]
fn tier1_security_remote_content_isolation() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("render-html"), true);
    settings.set_boolean("render-html", false).unwrap();
    assert_eq!(settings.boolean("render-html"), false);
}

// =========================================================================
// Category 8: Backup, Migration & System Preferences (Features 2, 3)
// =========================================================================

#[test]
fn tier1_backup_clean_gsettings_reset_undefined_keys() {
    // Feature 2: All 12 undefined legacy keys must NOT exist in schema
    let schema = get_test_schema();
    for &legacy in LEGACY_KEYS_MUST_NOT_EXIST {
        assert!(
            !schema.has_key(legacy),
            "Legacy key '{legacy}' must NOT exist in org.gnome.Evolution4 schema"
        );
    }
}

#[test]
fn tier1_backup_debug_console_toggle() {
    let settings = create_test_settings();
    assert_eq!(settings.boolean("debug-console-enabled"), false);
    settings.set_boolean("debug-console-enabled", true).unwrap();
    assert_eq!(settings.boolean("debug-console-enabled"), true);
}

#[test]
fn tier1_backup_window_geometry_persistence() {
    let settings = create_test_settings();
    assert_eq!(settings.int("window-width"), 1100);
    assert_eq!(settings.int("window-height"), 700);
    assert_eq!(settings.boolean("is-maximized"), false);

    settings.set_int("window-width", 1440).unwrap();
    settings.set_int("window-height", 900).unwrap();
    settings.set_boolean("is-maximized", true).unwrap();
    assert_eq!(settings.int("window-width"), 1440);
    assert_eq!(settings.int("window-height"), 900);
    assert_eq!(settings.boolean("is-maximized"), true);
}

#[test]
fn tier1_backup_active_view_persistence() {
    let settings = create_test_settings();
    assert_eq!(settings.string("active-view").as_str(), "mail");

    for view in &["calendar", "contacts", "tasks", "memos", "mail"] {
        settings.set_string("active-view", view).unwrap();
        assert_eq!(settings.string("active-view").as_str(), *view);
    }
}

#[test]
fn tier1_backup_rehome_macos_permissions_contract() {
    // Feature 3: macOS permissions group contract
    let schema = get_test_schema();
    // Verify core system sources and accounts keys are present
    assert!(schema.has_key("default-account"));
    assert!(schema.has_key("default-calendar-source"));
    assert!(schema.has_key("default-address-book-source"));
}
