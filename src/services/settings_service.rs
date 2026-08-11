// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent application settings.
//
// Keeping access in one small service prevents individual dialogs and views
// from inventing their own defaults.  The service is optional during a plain
// `cargo run`: GSettings schemas are normally installed by Meson/Flatpak, so
// a development checkout without an installed schema must still be able to
// construct its UI.

use gtk::gio;
use gtk::gio::prelude::*;

#[derive(Clone, Debug)]
pub struct AppSettings {
    settings: gio::Settings,
}

impl AppSettings {
    /// Open the application schema if it is available in the current process.
    pub fn try_new() -> Option<Self> {
        let source = gio::SettingsSchemaSource::default()?;
        let schema = source.lookup(crate::APP_ID, true)?;
        Some(Self {
            settings: gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None),
        })
    }

    pub fn auto_load_remote_images(&self) -> bool {
        self.settings.boolean("auto-load-remote-images")
    }

    pub fn window_width(&self) -> i32 {
        self.settings.int("window-width")
    }

    pub fn set_window_width(&self, value: i32) {
        self.set_int("window-width", value.max(360));
    }

    pub fn window_height(&self) -> i32 {
        self.settings.int("window-height")
    }

    pub fn set_window_height(&self, value: i32) {
        self.set_int("window-height", value.max(480));
    }

    pub fn is_maximized(&self) -> bool {
        self.settings.boolean("is-maximized")
    }

    pub fn set_is_maximized(&self, value: bool) {
        self.set_boolean("is-maximized", value);
    }

    pub fn active_view(&self) -> String {
        self.settings.string("active-view").to_string()
    }

    pub fn set_active_view(&self, value: &str) {
        if let Err(error) = self.settings.set_string("active-view", value) {
            tracing::warn!(%error, "Could not persist active view");
        }
    }

    pub fn default_account_id(&self) -> String {
        self.settings.string("default-account").to_string()
    }

    pub fn set_default_account_id(&self, value: &str) {
        if let Err(error) = self.settings.set_string("default-account", value.trim()) {
            tracing::warn!(%error, "Could not persist default account");
        }
    }

    pub fn default_calendar_source_uid(&self) -> String {
        self.settings.string("default-calendar-source").to_string()
    }

    pub fn set_default_calendar_source_uid(&self, value: &str) {
        if let Err(error) = self
            .settings
            .set_string("default-calendar-source", value.trim())
        {
            tracing::warn!(%error, "Could not persist default calendar source");
        }
    }

    pub fn default_address_book_source_uid(&self) -> String {
        self.settings
            .string("default-address-book-source")
            .to_string()
    }

    pub fn set_default_address_book_source_uid(&self, value: &str) {
        if let Err(error) = self
            .settings
            .set_string("default-address-book-source", value.trim())
        {
            tracing::warn!(%error, "Could not persist default address book source");
        }
    }

    pub fn default_task_source_uid(&self) -> String {
        self.settings.string("default-task-source").to_string()
    }

    pub fn set_default_task_source_uid(&self, value: &str) {
        if let Err(error) = self
            .settings
            .set_string("default-task-source", value.trim())
        {
            tracing::warn!(%error, "Could not persist default task source");
        }
    }

    pub fn default_memo_source_uid(&self) -> String {
        self.settings.string("default-memo-source").to_string()
    }

    pub fn set_default_memo_source_uid(&self, value: &str) {
        if let Err(error) = self
            .settings
            .set_string("default-memo-source", value.trim())
        {
            tracing::warn!(%error, "Could not persist default memo source");
        }
    }

    pub fn calendar_view(&self) -> String {
        let value = self.settings.string("calendar-view").to_string();
        match value.as_str() {
            "day" | "week" | "workweek" | "month" | "agenda" => value,
            _ => "month".to_string(),
        }
    }

    pub fn set_calendar_view(&self, value: &str) {
        let value = match value {
            "day" | "week" | "workweek" | "month" | "agenda" => value,
            _ => "month",
        };
        if let Err(error) = self.settings.set_string("calendar-view", value) {
            tracing::warn!(%error, "Could not persist calendar view");
        }
    }

    pub fn calendar_week_start(&self) -> String {
        match self.settings.string("calendar-week-start").as_str() {
            "sunday" => "sunday".to_string(),
            _ => "monday".to_string(),
        }
    }

    pub fn set_calendar_week_start(&self, value: &str) {
        let value = if value == "sunday" {
            "sunday"
        } else {
            "monday"
        };
        if let Err(error) = self.settings.set_string("calendar-week-start", value) {
            tracing::warn!(%error, "Could not persist calendar week start");
        }
    }

    pub fn calendar_workday_start_minutes(&self) -> u32 {
        self.settings.int("calendar-workday-start").clamp(0, 1_439) as u32
    }

    pub fn set_calendar_workday_start_minutes(&self, value: u32) {
        self.set_int("calendar-workday-start", value.min(1_439) as i32);
    }

    pub fn calendar_workday_end_minutes(&self) -> u32 {
        self.settings.int("calendar-workday-end").clamp(0, 1_439) as u32
    }

    pub fn set_calendar_workday_end_minutes(&self, value: u32) {
        self.set_int("calendar-workday-end", value.min(1_439) as i32);
    }

    pub fn calendar_default_reminder_minutes(&self) -> u32 {
        self.settings
            .int("calendar-default-reminder")
            .clamp(0, 10_080) as u32
    }

    pub fn set_calendar_default_reminder_minutes(&self, value: u32) {
        self.set_int("calendar-default-reminder", value.min(10_080) as i32);
    }

    pub fn notifications_enabled(&self) -> bool {
        self.settings.boolean("notifications-enabled")
    }

    pub fn set_notifications_enabled(&self, value: bool) {
        self.set_boolean("notifications-enabled", value);
    }

    pub fn confirm_destructive_actions(&self) -> bool {
        self.settings.boolean("confirm-destructive-actions")
    }

    pub fn set_confirm_destructive_actions(&self, value: bool) {
        self.set_boolean("confirm-destructive-actions", value);
    }

    pub fn calendar_show_week_numbers(&self) -> bool {
        self.settings.boolean("calendar-show-week-numbers")
    }

    pub fn set_calendar_show_week_numbers(&self, value: bool) {
        self.set_boolean("calendar-show-week-numbers", value);
    }

    pub fn date_format(&self) -> String {
        let value = self.settings.string("date-format").to_string();
        match value.as_str() {
            "iso" | "us" => value,
            _ => "german".to_string(),
        }
    }

    pub fn set_date_format(&self, value: &str) {
        let value = match value {
            "iso" | "us" => value,
            _ => "german",
        };
        if let Err(error) = self.settings.set_string("date-format", value) {
            tracing::warn!(%error, "Could not persist date format");
        }
    }

    pub fn time_format(&self) -> String {
        if self.settings.string("time-format").as_str() == "12h" {
            "12h".to_string()
        } else {
            "24h".to_string()
        }
    }

    pub fn set_time_format(&self, value: &str) {
        let value = if value == "12h" { "12h" } else { "24h" };
        if let Err(error) = self.settings.set_string("time-format", value) {
            tracing::warn!(%error, "Could not persist time format");
        }
    }

    pub fn set_auto_load_remote_images(&self, value: bool) {
        self.set_boolean("auto-load-remote-images", value);
    }

    pub fn render_html(&self) -> bool {
        self.settings.boolean("render-html")
    }

    pub fn set_render_html(&self, value: bool) {
        self.set_boolean("render-html", value);
    }

    pub fn composer_html(&self) -> bool {
        self.settings.boolean("composer-html")
    }

    pub fn set_composer_html(&self, value: bool) {
        self.set_boolean("composer-html", value);
    }

    pub fn mail_font_family(&self) -> String {
        let value = self.settings.string("mail-font-family").to_string();
        sanitize_font_family(&value)
    }

    pub fn set_mail_font_family(&self, value: &str) {
        let value = sanitize_font_family(value);
        if let Err(error) = self.settings.set_string("mail-font-family", &value) {
            tracing::warn!(%error, "Could not persist mail font family");
        }
    }

    pub fn mail_font_size(&self) -> u32 {
        self.settings.int("mail-font-size").clamp(8, 48) as u32
    }

    pub fn set_mail_font_size(&self, value: u32) {
        self.set_int("mail-font-size", value.clamp(8, 48) as i32);
    }

    pub fn mark_read_on_selection(&self) -> bool {
        self.settings.boolean("mark-read-on-selection")
    }

    pub fn set_mark_read_on_selection(&self, value: bool) {
        self.set_boolean("mark-read-on-selection", value);
    }

    pub fn show_message_preview(&self) -> bool {
        self.settings.boolean("show-message-preview")
    }

    pub fn set_show_message_preview(&self, value: bool) {
        self.set_boolean("show-message-preview", value);
    }

    /// Select the mail workflow layout.  `modern` keeps the message list and
    /// preview side by side; `classic` mirrors Evolution's traditional
    /// folder-left, list-above-preview arrangement.
    pub fn mail_layout(&self) -> String {
        match self.settings.string("mail-layout").as_str() {
            "classic" => "classic".to_string(),
            _ => "modern".to_string(),
        }
    }

    pub fn set_mail_layout(&self, value: &str) {
        let value = if value == "classic" {
            "classic"
        } else {
            "modern"
        };
        if let Err(error) = self.settings.set_string("mail-layout", value) {
            tracing::warn!(%error, "Could not persist mail layout");
        }
    }

    pub fn mail_sidebar_position(&self) -> i32 {
        self.settings.int("mail-sidebar-position").clamp(0, 2_000)
    }

    pub fn set_mail_sidebar_position(&self, value: i32) {
        self.set_int("mail-sidebar-position", value.clamp(0, 2_000));
    }

    pub fn mail_preview_position(&self, layout: &str) -> i32 {
        let key = if layout == "classic" {
            "mail-classic-preview-position"
        } else {
            "mail-modern-preview-position"
        };
        self.settings.int(key).clamp(120, 4_096)
    }

    pub fn set_mail_preview_position(&self, layout: &str, value: i32) {
        let key = if layout == "classic" {
            "mail-classic-preview-position"
        } else {
            "mail-modern-preview-position"
        };
        self.set_int(key, value.clamp(120, 4_096));
    }

    pub fn mail_row_density(&self) -> String {
        let value = self.settings.string("mail-row-density").to_string();
        match value.as_str() {
            "compact" | "spacious" => value,
            _ => "comfortable".to_string(),
        }
    }

    pub fn set_mail_row_density(&self, value: &str) {
        let value = match value {
            "compact" | "spacious" => value,
            _ => "comfortable",
        };
        if let Err(error) = self.settings.set_string("mail-row-density", value) {
            tracing::warn!(%error, "Could not persist mail row density");
        }
    }

    pub fn connect_changed<F>(&self, key: Option<&str>, callback: F)
    where
        F: Fn(&gio::Settings, &str) + 'static,
    {
        self.settings.connect_changed(key, callback);
    }

    pub fn color_scheme(&self) -> String {
        self.settings.string("color-scheme").to_string()
    }

    pub fn set_color_scheme(&self, value: &str) {
        let value = match value {
            "light" | "dark" => value,
            _ => "system",
        };
        if let Err(error) = self.settings.set_string("color-scheme", value) {
            tracing::warn!(%error, "Could not persist color scheme");
        }
    }

    pub fn force_dark_mode(&self) -> bool {
        self.settings.boolean("force-dark-mode")
    }

    pub fn set_force_dark_mode(&self, value: bool) {
        self.set_boolean("force-dark-mode", value);
    }

    pub fn sign_new_messages(&self) -> bool {
        self.settings.boolean("sign-new-messages")
    }

    pub fn set_sign_new_messages(&self, value: bool) {
        self.set_boolean("sign-new-messages", value);
    }

    pub fn default_pgp_key(&self) -> String {
        self.settings.string("default-pgp-key").to_string()
    }

    pub fn set_default_pgp_key(&self, value: &str) {
        if let Err(error) = self.settings.set_string("default-pgp-key", value) {
            tracing::warn!(%error, "Could not persist default PGP key");
        }
    }

    pub fn mail_sync_interval_seconds(&self) -> u32 {
        self.settings.int("mail-sync-interval").clamp(0, 86_400) as u32
    }

    pub fn set_mail_sync_interval_seconds(&self, value: u32) {
        self.set_int("mail-sync-interval", value.min(86_400) as i32);
    }

    pub fn draft_autosave_interval_seconds(&self) -> u32 {
        self.settings.int("draft-autosave-interval").clamp(0, 3_600) as u32
    }

    pub fn set_draft_autosave_interval_seconds(&self, value: u32) {
        self.set_int("draft-autosave-interval", value.min(3_600) as i32);
    }

    pub fn append_signature(&self) -> bool {
        self.settings.boolean("append-signature")
    }

    pub fn set_append_signature(&self, value: bool) {
        self.set_boolean("append-signature", value);
    }

    pub fn signature_text(&self) -> String {
        self.settings.string("signature-text").to_string()
    }

    pub fn set_signature_text(&self, value: &str) {
        if let Err(error) = self.settings.set_string("signature-text", value) {
            tracing::warn!(%error, "Could not persist signature text");
        }
    }

    pub fn local_mail_directory(&self) -> String {
        self.settings.string("local-mail-directory").to_string()
    }

    pub fn set_local_mail_directory(&self, value: &str) {
        if let Err(error) = self.settings.set_string("local-mail-directory", value) {
            tracing::warn!(%error, "Could not persist local mail directory");
        }
    }

    pub fn local_calendar_directory(&self) -> String {
        self.settings.string("local-calendar-directory").to_string()
    }

    pub fn set_local_calendar_directory(&self, value: &str) {
        if let Err(error) = self.settings.set_string("local-calendar-directory", value) {
            tracing::warn!(%error, "Could not persist local calendar directory");
        }
    }

    pub fn local_contact_directory(&self) -> String {
        self.settings.string("local-contact-directory").to_string()
    }

    pub fn set_local_contact_directory(&self, value: &str) {
        if let Err(error) = self.settings.set_string("local-contact-directory", value) {
            tracing::warn!(%error, "Could not persist local contact directory");
        }
    }

    pub fn quote_original(&self) -> bool {
        self.settings.boolean("quote-original")
    }

    pub fn set_quote_original(&self, value: bool) {
        self.set_boolean("quote-original", value);
    }

    pub fn quote_style(&self) -> String {
        self.settings.string("quote-style").to_string()
    }

    pub fn set_quote_style(&self, value: &str) {
        let value = if value == "prefix" {
            "prefix"
        } else {
            "blockquote"
        };
        if let Err(error) = self.settings.set_string("quote-style", value) {
            tracing::warn!(%error, "Could not persist quote style");
        }
    }

    pub fn mail_sort_field(&self) -> String {
        let value = self.settings.string("mail-sort-field").to_string();
        match value.as_str() {
            "subject" | "sender" | "date" => value,
            _ => "date".to_string(),
        }
    }

    pub fn set_mail_sort_field(&self, value: &str) {
        let value = match value {
            "subject" | "sender" | "date" => value,
            _ => "date",
        };
        if let Err(error) = self.settings.set_string("mail-sort-field", value) {
            tracing::warn!(%error, "Could not persist mail sort field");
        }
    }

    pub fn mail_sort_descending(&self) -> bool {
        self.settings.boolean("mail-sort-descending")
    }

    pub fn set_mail_sort_descending(&self, value: bool) {
        self.set_boolean("mail-sort-descending", value);
    }

    pub fn mail_threading(&self) -> bool {
        self.settings.boolean("mail-threading")
    }

    pub fn set_mail_threading(&self, value: bool) {
        self.set_boolean("mail-threading", value);
    }

    pub fn task_filter(&self) -> String {
        let value = self.settings.string("task-filter").to_string();
        match value.as_str() {
            "today" | "important" | "completed" => value,
            _ => "all".to_string(),
        }
    }

    pub fn set_task_filter(&self, value: &str) {
        let value = match value {
            "today" | "important" | "completed" => value,
            _ => "all",
        };
        if let Err(error) = self.settings.set_string("task-filter", value) {
            tracing::warn!(%error, "Could not persist task filter");
        }
    }

    pub fn task_sort_field(&self) -> String {
        let value = self.settings.string("task-sort-field").to_string();
        match value.as_str() {
            "title" | "priority" => value,
            _ => "due".to_string(),
        }
    }

    pub fn set_task_sort_field(&self, value: &str) {
        let value = match value {
            "title" | "priority" => value,
            _ => "due",
        };
        if let Err(error) = self.settings.set_string("task-sort-field", value) {
            tracing::warn!(%error, "Could not persist task sort field");
        }
    }

    pub fn memo_sort_field(&self) -> String {
        let value = self.settings.string("memo-sort-field").to_string();
        match value.as_str() {
            "title" | "source" => value,
            _ => "updated".to_string(),
        }
    }

    pub fn set_memo_sort_field(&self, value: &str) {
        let value = match value {
            "title" | "source" => value,
            _ => "updated",
        };
        if let Err(error) = self.settings.set_string("memo-sort-field", value) {
            tracing::warn!(%error, "Could not persist memo sort field");
        }
    }

    /// Reset every application-owned key to its schema default.
    ///
    /// This is used by the explicit app-data deletion action.  EDS, Camel,
    /// Secret Service, and user-selected external data paths are not touched.
    pub fn reset_to_defaults(&self) {
        for key in [
            "window-width",
            "window-height",
            "is-maximized",
            "active-view",
            "default-account",
            "default-calendar-source",
            "default-address-book-source",
            "default-task-source",
            "default-memo-source",
            "calendar-view",
            "calendar-week-start",
            "calendar-workday-start",
            "calendar-workday-end",
            "calendar-default-reminder",
            "notifications-enabled",
            "confirm-destructive-actions",
            "calendar-show-week-numbers",
            "date-format",
            "time-format",
            "auto-load-remote-images",
            "force-dark-mode",
            "render-html",
            "composer-html",
            "mail-font-family",
            "mail-font-size",
            "mark-read-on-selection",
            "show-message-preview",
            "mail-layout",
            "mail-sidebar-position",
            "mail-modern-preview-position",
            "mail-classic-preview-position",
            "mail-row-density",
            "color-scheme",
            "sign-new-messages",
            "default-pgp-key",
            "mail-sync-interval",
            "draft-autosave-interval",
            "append-signature",
            "signature-text",
            "local-mail-directory",
            "local-calendar-directory",
            "local-contact-directory",
            "quote-original",
            "quote-style",
            "mail-sort-field",
            "mail-sort-descending",
            "mail-threading",
            "task-filter",
            "task-sort-field",
            "memo-sort-field",
        ] {
            self.settings.reset(key);
        }
    }

    fn set_boolean(&self, key: &str, value: bool) {
        if let Err(error) = self.settings.set_boolean(key, value) {
            tracing::warn!(key, %error, "Could not persist application setting");
        }
    }

    fn set_int(&self, key: &str, value: i32) {
        if let Err(error) = self.settings.set_int(key, value) {
            tracing::warn!(key, %error, "Could not persist application setting");
        }
    }
}

fn sanitize_font_family(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 120
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || " ,-_'\"".contains(character)))
    {
        "system-ui, sans-serif".to_string()
    } else {
        value.to_string()
    }
}
