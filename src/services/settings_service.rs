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
use gtk::glib;
use gtk::glib::prelude::{StaticVariantType, ToVariant};
use std::collections::{BTreeMap, HashSet};

use super::memo_filter_service::{
    MemoCategoryFilter, MemoSearchMode, validate_category_name, validate_source_filter,
};
use super::memo_surface_service::MemoPreviewOrientation;

#[derive(Clone, Debug)]
pub struct AppSettings {
    settings: gio::Settings,
}

fn normalized_mail_sort_field(value: &str) -> &str {
    match value {
        "subject" | "sender" | "date" | "unread" | "flagged" | "important" | "attachments" => value,
        _ => "date",
    }
}

fn normalize_mail_folder_sort_order(value: &str) -> Result<String, String> {
    let order = serde_json::from_str::<BTreeMap<String, u32>>(value)
        .map_err(|_| "ungültige Ordnersortierung".to_string())?;
    if order
        .iter()
        .any(|(folder_id, rank)| folder_id.trim().is_empty() || *rank == 0)
    {
        return Err("ungültige Ordnersortierung".to_string());
    }
    serde_json::to_string(&order).map_err(|_| "ungültige Ordnersortierung".to_string())
}

fn normalized_task_filter(value: &str) -> &str {
    match value {
        "all" | "today" | "next-7" | "next-30" | "next-365" | "important" | "overdue"
        | "in-progress" | "cancelled" | "completed" | "uncompleted" | "scheduled" | "no-due"
        | "with-attachments" | "started" | "without-category" => value,
        "active" => "next-365",
        _ => "all",
    }
}

fn normalized_task_search_mode(value: &str) -> &str {
    match value {
        "any-field" | "summary" | "description" | "advanced" => value,
        _ => "any-field",
    }
}

pub(crate) fn normalize_memo_search_mode_value(value: &str) -> Result<String, String> {
    MemoSearchMode::parse_setting(value).map(|mode| mode.setting_value().to_string())
}

pub(crate) fn normalize_memo_category_filter_value(value: &str) -> Result<String, String> {
    MemoCategoryFilter::parse_setting(value).map(|filter| filter.setting_value())
}

pub(crate) fn normalize_memo_source_filter_value(value: &str) -> Result<String, String> {
    validate_source_filter(value)
}

pub(crate) fn normalize_memo_preview_orientation_value(value: &str) -> Result<String, String> {
    MemoPreviewOrientation::parse_setting(value)
        .map(|orientation| orientation.setting_value().to_string())
}

pub(crate) fn normalize_task_preview_orientation_value(value: &str) -> Result<String, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "classic" => Ok("classic".to_string()),
        "vertical" => Ok("vertical".to_string()),
        _ => Err("Unbekannte Aufgaben-Vorschauausrichtung".to_string()),
    }
}

pub(crate) fn validate_memo_category_filter_value(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(
            "Der Memo-Kategoriefilter darf bei einer Migration nicht leer sein".to_string(),
        );
    }
    if value.eq_ignore_ascii_case("any")
        || value.eq_ignore_ascii_case("without")
        || value.eq_ignore_ascii_case("without-category")
    {
        return normalize_memo_category_filter_value(value);
    }
    validate_category_name(value).map(|value| value)
}

fn normalized_switcher_location(value: &str) -> &'static str {
    match value {
        "top" => "top",
        _ => "bottom-left",
    }
}

pub(crate) const MAX_TASK_CATEGORY_FILTER_BYTES: usize = 128;

pub(crate) fn normalized_task_category_filter(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_TASK_CATEGORY_FILTER_BYTES
        || value.chars().any(char::is_control)
    {
        String::new()
    } else {
        value.to_string()
    }
}

const DEFAULT_MAIL_NORMAL_HEADERS: [(&str, bool); 10] = [
    ("From", true),
    ("Reply-To", true),
    ("To", true),
    ("Cc", true),
    ("Bcc", true),
    ("Subject", true),
    ("Date", true),
    ("Newsgroups", true),
    ("Face", true),
    ("x-evolution-mailer", false),
];

const MAX_MAIL_NORMAL_HEADERS: usize = 64;
const MAX_MAIL_NORMAL_HEADER_NAME_BYTES: usize = 128;

fn default_mail_normal_headers() -> Vec<(String, bool)> {
    DEFAULT_MAIL_NORMAL_HEADERS
        .iter()
        .map(|(name, visible)| ((*name).to_string(), *visible))
        .collect()
}

/// Validate a field name before it enters the persistent normal-header list.
///
/// RFC 5322 field names are printable text without whitespace, controls, or a
/// colon.  Keeping the portable setting to ASCII also makes validation
/// deterministic across locales and prevents a name from being interpreted as
/// a second header line by a downstream formatter.
pub(crate) fn validate_mail_normal_header_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err("Der Headername darf nicht leer sein.".to_string());
    }
    if name.len() > MAX_MAIL_NORMAL_HEADER_NAME_BYTES {
        return Err(format!(
            "Der Headername darf höchstens {MAX_MAIL_NORMAL_HEADER_NAME_BYTES} Zeichen lang sein."
        ));
    }
    if !name.is_ascii()
        || name
            .chars()
            .any(|character| !character.is_ascii_graphic() || character == ':')
    {
        return Err(
            "Der Headername darf nur sichtbare ASCII-Zeichen enthalten und keinen Doppelpunkt."
                .to_string(),
        );
    }
    Ok(name.to_string())
}

fn normalize_mail_normal_headers(headers: &[(String, bool)]) -> Vec<(String, bool)> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::with_capacity(headers.len().min(MAX_MAIL_NORMAL_HEADERS));

    for (name, visible) in headers {
        let Ok(name) = validate_mail_normal_header_name(name) else {
            continue;
        };
        let identity = name.to_ascii_lowercase();
        if !seen.insert(identity) {
            continue;
        }
        normalized.push((name, *visible));
        if normalized.len() == MAX_MAIL_NORMAL_HEADERS {
            break;
        }
    }

    normalized
}

fn mail_normal_headers_variant_type() -> std::borrow::Cow<'static, glib::VariantTy> {
    <Vec<(String, bool)> as StaticVariantType>::static_variant_type()
}

pub(crate) fn parse_mail_normal_headers_value(value: &str) -> Result<Vec<(String, bool)>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Die Headerliste ist leer.".to_string());
    }

    let variant = glib::Variant::parse(Some(mail_normal_headers_variant_type().as_ref()), value)
        .map_err(|_| "ungültige show-headers-Liste".to_string())?;
    let headers = variant
        .get::<Vec<(String, bool)>>()
        .ok_or_else(|| "ungültiger Typ für die show-headers-Liste".to_string())?;
    if headers.len() > MAX_MAIL_NORMAL_HEADERS {
        return Err(format!(
            "Die Headerliste darf höchstens {MAX_MAIL_NORMAL_HEADERS} Einträge enthalten."
        ));
    }

    let mut seen = HashSet::with_capacity(headers.len());
    headers
        .into_iter()
        .map(|(name, visible)| {
            let name = validate_mail_normal_header_name(&name)?;
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err("Die Headerliste enthält doppelte Feldnamen.".to_string());
            }
            Ok((name, visible))
        })
        .collect()
}

const MIN_MAIL_SYNC_MESSAGE_LIMIT: i32 = 25;
const MAX_MAIL_SYNC_MESSAGE_LIMIT: i32 = 1_000;

pub(crate) fn normalize_composer_charset(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => Some("UTF-8"),
        "iso-8859-1" | "latin1" | "latin-1" => Some("ISO-8859-1"),
        "iso-8859-15" | "latin9" | "latin-9" => Some("ISO-8859-15"),
        "windows-1252" | "cp1252" => Some("Windows-1252"),
        "us-ascii" | "ascii" => Some("US-ASCII"),
        _ => None,
    }
}

fn clamp_mail_sync_message_limit(value: i32) -> i32 {
    value.clamp(MIN_MAIL_SYNC_MESSAGE_LIMIT, MAX_MAIL_SYNC_MESSAGE_LIMIT)
}

impl AppSettings {
    /// Open the application schema if it is available in the current process.
    pub fn try_new() -> Option<Self> {
        let installed_source = gio::SettingsSchemaSource::default();
        let schema = installed_source
            .as_ref()
            .and_then(|source| source.lookup(crate::APP_ID, true))
            .or_else(|| {
                let directory = option_env!("EVOLUTION_GTK4_SCHEMA_DIR")?;
                let source = gio::SettingsSchemaSource::from_directory(
                    directory,
                    installed_source.as_ref(),
                    true,
                )
                .ok()?;
                source.lookup(crate::APP_ID, true)
            })?;
        Some(Self {
            settings: gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None),
        })
    }

    pub fn auto_load_remote_images(&self) -> bool {
        self.settings.boolean("auto-load-remote-images")
    }

    /// Keep backend discovery, synchronization, and remote mutations out of
    /// the current UI session while retaining the local app-owned stores.
    pub fn offline_mode(&self) -> bool {
        self.settings.boolean("offline-mode")
    }

    pub fn set_offline_mode(&self, value: bool) {
        self.set_boolean("offline-mode", value);
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

    pub fn switcher_location(&self) -> String {
        normalized_switcher_location(&self.settings.string("switcher-location")).to_string()
    }

    pub fn set_switcher_location(&self, value: &str) {
        let value = normalized_switcher_location(value);
        if let Err(error) = self.settings.set_string("switcher-location", value) {
            tracing::warn!(%error, "Could not persist switcher location");
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

    pub fn calendar_workdays(&self) -> String {
        sanitize_calendar_workdays(&self.settings.string("calendar-workdays"))
    }

    pub fn set_calendar_workdays(&self, value: &str) {
        let value = sanitize_calendar_workdays(value);
        if let Err(error) = self.settings.set_string("calendar-workdays", &value) {
            tracing::warn!(%error, "Could not persist calendar workdays");
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

    pub fn calendar_hidden_sources(&self) -> Vec<String> {
        self.settings
            .string("calendar-hidden-sources")
            .split(',')
            .map(str::trim)
            .filter(|uid| !uid.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_calendar_hidden_sources(&self, source_uids: &[String]) {
        let mut source_uids = source_uids
            .iter()
            .map(|uid| uid.trim())
            .filter(|uid| !uid.is_empty())
            .collect::<Vec<_>>();
        source_uids.sort_unstable();
        source_uids.dedup();
        if let Err(error) = self
            .settings
            .set_string("calendar-hidden-sources", &source_uids.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden calendar sources");
        }
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

    pub fn calendar_time_zone(&self) -> String {
        let value = self.settings.string("calendar-time-zone");
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("system") {
            "system".to_string()
        } else {
            value.to_string()
        }
    }

    pub fn set_calendar_time_zone(&self, value: &str) {
        let value = value.trim();
        let value = if value.is_empty() { "system" } else { value };
        if let Err(error) = self.settings.set_string("calendar-time-zone", value) {
            tracing::warn!(%error, "Could not persist calendar time zone");
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

    /// Persistent default used when opening a new composer. Individual
    /// messages keep their selected charset in their recovery draft.
    pub fn composer_charset(&self) -> String {
        normalize_composer_charset(&self.settings.string("composer-charset"))
            .unwrap_or("UTF-8")
            .to_string()
    }

    pub fn set_composer_charset(&self, value: &str) {
        let value = normalize_composer_charset(value).unwrap_or("UTF-8");
        self.set_string("composer-charset", value);
    }

    pub fn request_read_receipt(&self) -> bool {
        self.settings.boolean("request-read-receipt")
    }

    pub fn set_request_read_receipt(&self, value: bool) {
        self.set_boolean("request-read-receipt", value);
    }

    pub fn spellcheck_languages(&self) -> String {
        sanitize_spellcheck_languages(&self.settings.string("spellcheck-languages"))
    }

    pub fn set_spellcheck_languages(&self, value: &str) {
        let value = sanitize_spellcheck_languages(value);
        if let Err(error) = self.settings.set_string("spellcheck-languages", &value) {
            tracing::warn!(%error, "Could not persist spell-checking languages");
        }
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

    pub fn mark_read_timeout(&self) -> i32 {
        self.settings.int("mark-read-timeout")
    }

    pub fn set_mark_read_timeout(&self, value: i32) {
        self.set_int("mark-read-timeout", value.clamp(0, 30_000));
    }

    pub fn show_message_preview(&self) -> bool {
        self.settings.boolean("show-message-preview")
    }

    pub fn set_show_message_preview(&self, value: bool) {
        self.set_boolean("show-message-preview", value);
    }

    pub fn mail_show_security_details(&self) -> bool {
        self.settings.boolean("mail-show-security-details")
    }

    pub fn set_mail_show_security_details(&self, value: bool) {
        self.set_boolean("mail-show-security-details", value);
    }

    pub fn mail_show_all_headers(&self) -> bool {
        self.settings.boolean("mail-show-all-headers")
    }

    pub fn set_mail_show_all_headers(&self, value: bool) {
        self.set_boolean("mail-show-all-headers", value);
    }

    /// Return the ordered normal message-header list used by the reader.
    ///
    /// The GSettings value is normalized at the service boundary so malformed
    /// or legacy values cannot escape into the formatter.  The first entry
    /// wins when names differ only by case, preserving the user's order and
    /// visibility choice.  An explicitly stored empty list is the Evolution
    /// reset sentinel and resolves to the schema's default list here.
    pub fn mail_normal_headers(&self) -> Vec<(String, bool)> {
        let value = self.settings.value("mail-normal-headers");
        let headers = value.get::<Vec<(String, bool)>>().unwrap_or_default();
        let headers = normalize_mail_normal_headers(&headers);
        if headers.is_empty() {
            default_mail_normal_headers()
        } else {
            headers
        }
    }

    /// Persist an Evolution-compatible ordered normal-header list.  An empty
    /// input is intentionally stored as an empty `a(sb)` value; the getter
    /// resolves it to the exact Evolution defaults when the setting is read.
    pub fn set_mail_normal_headers(&self, headers: &[(String, bool)]) {
        let normalized = normalize_mail_normal_headers(headers);
        if let Err(error) = self
            .settings
            .set_value("mail-normal-headers", &normalized.to_variant())
        {
            tracing::warn!(%error, "Could not persist normal mail headers");
        }
    }

    pub fn mail_show_statusbar(&self) -> bool {
        self.settings.boolean("mail-show-statusbar")
    }

    pub fn set_mail_show_statusbar(&self, value: bool) {
        self.set_boolean("mail-show-statusbar", value);
    }

    /// Select the folder shown when the mail view is opened.
    pub fn mail_default_folder(&self) -> String {
        match self.settings.string("mail-default-folder").as_str() {
            "all" | "inbox" | "sent" | "drafts" | "trash" | "archive" | "spam" => {
                self.settings.string("mail-default-folder").to_string()
            }
            _ => "inbox".to_string(),
        }
    }

    pub fn set_mail_default_folder(&self, value: &str) {
        let value = match value {
            "all" | "inbox" | "sent" | "drafts" | "trash" | "archive" | "spam" => value,
            _ => "inbox",
        };
        if let Err(error) = self.settings.set_string("mail-default-folder", value) {
            tracing::warn!(%error, "Could not persist default mail folder");
        }
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

    pub fn mail_show_toolbar(&self) -> bool {
        self.settings.boolean("mail-show-toolbar")
    }

    pub fn set_mail_show_toolbar(&self, value: bool) {
        self.set_boolean("mail-show-toolbar", value);
    }

    pub fn mail_show_sidebar(&self) -> bool {
        self.settings.boolean("mail-show-sidebar")
    }

    pub fn set_mail_show_sidebar(&self, value: bool) {
        self.set_boolean("mail-show-sidebar", value);
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
            "comfortable" | "spacious" => value,
            _ => "compact".to_string(),
        }
    }

    pub fn set_mail_row_density(&self, value: &str) {
        let value = match value {
            "compact" | "comfortable" | "spacious" => value,
            _ => "compact",
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

    pub fn crypto_protocol(&self) -> String {
        match self.settings.string("crypto-protocol").as_str() {
            "smime" => "smime".to_string(),
            _ => "openpgp".to_string(),
        }
    }

    pub fn set_crypto_protocol(&self, value: &str) {
        let value = if value.eq_ignore_ascii_case("smime") {
            "smime"
        } else {
            "openpgp"
        };
        if let Err(error) = self.settings.set_string("crypto-protocol", value) {
            tracing::warn!(%error, "Could not persist crypto protocol");
        }
    }

    pub fn smime_signer_certificate(&self) -> String {
        self.settings.string("smime-signer-certificate").to_string()
    }

    pub fn set_smime_signer_certificate(&self, value: &str) {
        self.set_string("smime-signer-certificate", value.trim());
    }

    pub fn smime_private_key(&self) -> String {
        self.settings.string("smime-private-key").to_string()
    }

    pub fn set_smime_private_key(&self, value: &str) {
        self.set_string("smime-private-key", value.trim());
    }

    pub fn smime_recipient_certificates(&self) -> Vec<String> {
        self.settings
            .string("smime-recipient-certificates")
            .split('\n')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn set_smime_recipient_certificates(&self, values: &[String]) {
        let value = values
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        self.set_string("smime-recipient-certificates", &value);
    }

    pub fn smime_ca_file(&self) -> String {
        self.settings.string("smime-ca-file").to_string()
    }

    pub fn set_smime_ca_file(&self, value: &str) {
        self.set_string("smime-ca-file", value.trim());
    }

    pub(crate) fn smime_options(
        &self,
        sign: bool,
        encrypt: bool,
    ) -> crate::services::crypto_service::SmimeOptions {
        crate::services::crypto_service::SmimeOptions {
            sign,
            encrypt,
            signer_certificate: self.smime_signer_certificate(),
            private_key: self.smime_private_key(),
            recipient_certificates: self.smime_recipient_certificates(),
            ca_file: self.smime_ca_file(),
        }
    }

    pub fn mail_sync_interval_seconds(&self) -> u32 {
        self.settings.int("mail-sync-interval").clamp(0, 86_400) as u32
    }

    pub fn set_mail_sync_interval_seconds(&self, value: u32) {
        self.set_int("mail-sync-interval", value.min(86_400) as i32);
    }

    /// Number of messages fetched for each remote folder page.
    ///
    /// The value is deliberately bounded: a too-small page makes normal mail
    /// work needlessly click-heavy, while an unbounded page can make a large
    /// mailbox monopolize the worker and GTK model application.
    pub fn mail_sync_message_limit(&self) -> u32 {
        clamp_mail_sync_message_limit(self.settings.int("mail-sync-message-limit")) as u32
    }

    pub fn set_mail_sync_message_limit(&self, value: u32) {
        self.set_int(
            "mail-sync-message-limit",
            clamp_mail_sync_message_limit(value.min(i32::MAX as u32) as i32),
        );
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
        normalized_mail_sort_field(&value).to_string()
    }

    pub fn set_mail_sort_field(&self, value: &str) {
        let value = normalized_mail_sort_field(value);
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

    /// Persisted explicit folder ordinals.  The sidebar validates and applies
    /// this portable JSON map because only it knows which folder identities
    /// are currently loaded and which ones are siblings.
    pub fn mail_folder_sort_order(&self) -> String {
        self.settings.string("mail-folder-sort-order").to_string()
    }

    pub fn set_mail_folder_sort_order(&self, value: &str) {
        let Ok(value) = normalize_mail_folder_sort_order(value) else {
            tracing::warn!("Could not persist invalid folder sort order");
            return;
        };
        if let Err(error) = self.settings.set_string("mail-folder-sort-order", &value) {
            tracing::warn!(%error, "Could not persist folder sort order");
        }
    }

    pub fn mail_show_deleted(&self) -> bool {
        self.settings.boolean("mail-show-deleted")
    }

    pub fn set_mail_show_deleted(&self, value: bool) {
        self.set_boolean("mail-show-deleted", value);
    }

    pub fn mail_show_avatar(&self) -> bool {
        self.settings.boolean("mail-show-avatar")
    }

    pub fn set_mail_show_avatar(&self, value: bool) {
        self.set_boolean("mail-show-avatar", value);
    }

    pub fn mail_show_sender(&self) -> bool {
        self.settings.boolean("mail-show-sender")
    }

    pub fn set_mail_show_sender(&self, value: bool) {
        self.set_boolean("mail-show-sender", value);
    }

    pub fn mail_show_recipient(&self) -> bool {
        self.settings.boolean("mail-show-recipient")
    }

    pub fn set_mail_show_recipient(&self, value: bool) {
        self.set_boolean("mail-show-recipient", value);
    }

    pub fn mail_show_date(&self) -> bool {
        self.settings.boolean("mail-show-date")
    }

    pub fn set_mail_show_date(&self, value: bool) {
        self.set_boolean("mail-show-date", value);
    }

    pub fn mail_show_attachment(&self) -> bool {
        self.settings.boolean("mail-show-attachment")
    }

    pub fn set_mail_show_attachment(&self, value: bool) {
        self.set_boolean("mail-show-attachment", value);
    }

    pub fn mail_show_preview_text(&self) -> bool {
        self.settings.boolean("mail-show-preview-text")
    }

    pub fn set_mail_show_preview_text(&self, value: bool) {
        self.set_boolean("mail-show-preview-text", value);
    }

    pub fn mail_show_labels(&self) -> bool {
        self.settings.boolean("mail-show-labels")
    }

    pub fn set_mail_show_labels(&self, value: bool) {
        self.set_boolean("mail-show-labels", value);
    }

    pub fn mail_show_size(&self) -> bool {
        self.settings.boolean("mail-show-size")
    }

    pub fn set_mail_show_size(&self, value: bool) {
        self.set_boolean("mail-show-size", value);
    }

    pub fn mail_show_follow_up_due(&self) -> bool {
        self.settings.boolean("mail-show-follow-up-due")
    }

    pub fn set_mail_show_follow_up_due(&self, value: bool) {
        self.set_boolean("mail-show-follow-up-due", value);
    }

    pub fn mail_show_location(&self) -> bool {
        self.settings.boolean("mail-show-location")
    }

    pub fn set_mail_show_location(&self, value: bool) {
        self.set_boolean("mail-show-location", value);
    }

    pub fn mail_sender_column_width(&self) -> i32 {
        self.settings.int("mail-sender-column-width").clamp(80, 360)
    }

    pub fn set_mail_sender_column_width(&self, value: i32) {
        self.set_int("mail-sender-column-width", value.clamp(80, 360));
    }

    pub fn mail_date_column_width(&self) -> i32 {
        let val = self.settings.int("mail-date-column-width");
        if val <= 0 { 130 } else { val.clamp(60, 300) }
    }

    pub fn set_mail_date_column_width(&self, value: i32) {
        self.set_int("mail-date-column-width", value.clamp(60, 300));
    }

    pub fn task_filter(&self) -> String {
        normalized_task_filter(&self.settings.string("task-filter")).to_string()
    }

    pub fn set_task_filter(&self, value: &str) {
        let value = normalized_task_filter(value);
        if let Err(error) = self.settings.set_string("task-filter", value) {
            tracing::warn!(%error, "Could not persist task filter");
        }
    }

    pub fn task_category_filter(&self) -> String {
        normalized_task_category_filter(&self.settings.string("task-category-filter"))
    }

    pub fn set_task_category_filter(&self, value: &str) {
        let value = normalized_task_category_filter(value);
        if let Err(error) = self.settings.set_string("task-category-filter", &value) {
            tracing::warn!(%error, "Could not persist task category filter");
        }
    }

    pub fn task_search_mode(&self) -> String {
        normalized_task_search_mode(&self.settings.string("task-search-mode")).to_string()
    }

    pub fn set_task_search_mode(&self, value: &str) {
        let value = normalized_task_search_mode(value);
        if let Err(error) = self.settings.set_string("task-search-mode", value) {
            tracing::warn!(%error, "Could not persist task search mode");
        }
    }

    pub fn task_source_filter(&self) -> String {
        self.settings.string("task-source-filter").to_string()
    }

    pub fn set_task_source_filter(&self, value: &str) {
        if let Err(error) = self.settings.set_string("task-source-filter", value.trim()) {
            tracing::warn!(%error, "Could not persist task source filter");
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

    pub fn task_sort_descending(&self) -> bool {
        self.settings.boolean("task-sort-descending")
    }

    pub fn set_task_sort_descending(&self, value: bool) {
        self.set_boolean("task-sort-descending", value);
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

    pub fn memo_source_filter(&self) -> String {
        normalize_memo_source_filter_value(&self.settings.string("memo-source-filter"))
            .unwrap_or_default()
    }

    pub fn set_memo_source_filter(&self, value: &str) {
        let Ok(value) = normalize_memo_source_filter_value(value) else {
            tracing::warn!("Refusing invalid memo source filter");
            return;
        };
        if let Err(error) = self.settings.set_string("memo-source-filter", &value) {
            tracing::warn!(%error, "Could not persist memo source filter");
        }
    }

    pub fn memo_sort_descending(&self) -> bool {
        self.settings.boolean("memo-sort-descending")
    }

    pub fn set_memo_sort_descending(&self, value: bool) {
        self.set_boolean("memo-sort-descending", value);
    }

    pub fn memo_search_mode(&self) -> String {
        normalize_memo_search_mode_value(&self.settings.string("memo-search-mode"))
            .unwrap_or_else(|_| MemoSearchMode::default().setting_value().to_string())
    }

    pub fn set_memo_search_mode(&self, value: &str) {
        let value = normalize_memo_search_mode_value(value)
            .unwrap_or_else(|_| MemoSearchMode::default().setting_value().to_string());
        self.set_string("memo-search-mode", &value);
    }

    pub fn memo_category_filter(&self) -> String {
        normalize_memo_category_filter_value(&self.settings.string("memo-category-filter"))
            .unwrap_or_else(|_| MemoCategoryFilter::default().setting_value())
    }

    pub fn set_memo_category_filter(&self, value: &str) {
        let value = normalize_memo_category_filter_value(value)
            .unwrap_or_else(|_| MemoCategoryFilter::default().setting_value());
        self.set_string("memo-category-filter", &value);
    }

    pub fn memo_preview_visible(&self) -> bool {
        self.settings.boolean("memo-preview-visible")
    }

    pub fn set_memo_preview_visible(&self, value: bool) {
        self.set_boolean("memo-preview-visible", value);
    }

    pub fn memo_preview_orientation(&self) -> String {
        normalize_memo_preview_orientation_value(&self.settings.string("memo-preview-orientation"))
            .unwrap_or_else(|_| {
                MemoPreviewOrientation::default()
                    .setting_value()
                    .to_string()
            })
    }

    pub fn set_memo_preview_orientation(&self, value: &str) {
        let value = normalize_memo_preview_orientation_value(value).unwrap_or_else(|_| {
            MemoPreviewOrientation::default()
                .setting_value()
                .to_string()
        });
        self.set_string("memo-preview-orientation", &value);
    }

    pub fn task_preview_visible(&self) -> bool {
        self.settings.boolean("task-preview-visible")
    }

    pub fn set_task_preview_visible(&self, value: bool) {
        self.set_boolean("task-preview-visible", value);
    }

    pub fn task_preview_orientation(&self) -> String {
        normalize_task_preview_orientation_value(&self.settings.string("task-preview-orientation"))
            .unwrap_or_else(|_| "classic".to_string())
    }

    pub fn set_task_preview_orientation(&self, value: &str) {
        let value = normalize_task_preview_orientation_value(value)
            .unwrap_or_else(|_| "classic".to_string());
        self.set_string("task-preview-orientation", &value);
    }

    pub fn task_preview_position(&self, orientation: &str) -> i32 {
        let key = if orientation == "vertical" {
            "task-hpane-position"
        } else {
            "task-vpane-position"
        };
        self.settings.int(key).clamp(80, 4_096)
    }

    pub fn set_task_preview_position(&self, orientation: &str, value: i32) {
        let key = if orientation == "vertical" {
            "task-hpane-position"
        } else {
            "task-vpane-position"
        };
        self.set_int(key, value.clamp(80, 4_096));
    }

    /// Apply one already-normalized, non-secret setting from an explicit
    /// profile migration.  The migration parser only exposes keys from the
    /// installed schema; keeping the final type conversion here makes sure
    /// imported values use exactly the same validation and normalization as
    /// the Preferences dialog.
    pub fn apply_migrated_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let value = value.trim();
        match key {
            "window-width" => {
                self.set_window_width(parse_migrated_i32_in_range(key, value, 360, i32::MAX)?);
            }
            "window-height" => {
                self.set_window_height(parse_migrated_i32_in_range(key, value, 480, i32::MAX)?);
            }
            "is-maximized" => self.set_is_maximized(parse_migrated_bool(key, value)?),
            "active-view" => self.set_active_view(value),
            "switcher-location" => {
                if !matches!(value, "top" | "bottom-left") {
                    return Err(format!(
                        "ungültiger Wert für „{key}“ (erwartet „top“ oder „bottom-left“)"
                    ));
                }
                self.set_switcher_location(value);
            }
            "default-account" => self.set_default_account_id(value),
            "default-calendar-source" => self.set_default_calendar_source_uid(value),
            "default-task-source" => self.set_default_task_source_uid(value),
            "default-memo-source" => self.set_default_memo_source_uid(value),
            "default-address-book-source" => self.set_default_address_book_source_uid(value),
            "calendar-view" => self.set_calendar_view(value),
            "calendar-week-start" => self.set_calendar_week_start(value),
            "calendar-workdays" => self.set_calendar_workdays(value),
            "calendar-workday-start" => {
                self.set_calendar_workday_start_minutes(parse_migrated_u32_in_range(
                    key, value, 0, 1_439,
                )?);
            }
            "calendar-workday-end" => {
                self.set_calendar_workday_end_minutes(parse_migrated_u32_in_range(
                    key, value, 0, 1_439,
                )?);
            }
            "calendar-default-reminder" => {
                self.set_calendar_default_reminder_minutes(parse_migrated_u32_in_range(
                    key, value, 0, 10_080,
                )?);
            }
            "notifications-enabled" => {
                self.set_notifications_enabled(parse_migrated_bool(key, value)?);
            }
            "confirm-destructive-actions" => {
                self.set_confirm_destructive_actions(parse_migrated_bool(key, value)?);
            }
            "calendar-show-week-numbers" => {
                self.set_calendar_show_week_numbers(parse_migrated_bool(key, value)?);
            }
            "calendar-hidden-sources" => {
                let source_uids = value
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|source_uid| !source_uid.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                if source_uids.iter().any(|source_uid| {
                    source_uid.is_empty() || source_uid.chars().any(char::is_control)
                }) {
                    return Err(format!("ungültige Quellenliste für „{key}“"));
                }
                self.set_calendar_hidden_sources(&source_uids);
            }
            "date-format" => self.set_date_format(value),
            "time-format" => self.set_time_format(value),
            "calendar-time-zone" => self.set_calendar_time_zone(value),
            "auto-load-remote-images" => {
                self.set_auto_load_remote_images(parse_migrated_bool(key, value)?);
            }
            "offline-mode" => self.set_offline_mode(parse_migrated_bool(key, value)?),
            "force-dark-mode" => self.set_force_dark_mode(parse_migrated_bool(key, value)?),
            "render-html" => self.set_render_html(parse_migrated_bool(key, value)?),
            "composer-html" => self.set_composer_html(parse_migrated_bool(key, value)?),
            "composer-charset" => {
                let value = normalize_composer_charset(value)
                    .ok_or_else(|| format!("ungültige Zeichenkodierung für „{key}“"))?;
                self.set_composer_charset(value);
            }
            "request-read-receipt" => {
                self.set_request_read_receipt(parse_migrated_bool(key, value)?);
            }
            "spellcheck-languages" => self.set_spellcheck_languages(value),
            "mail-font-family" => self.set_mail_font_family(value),
            "mail-font-size" => {
                self.set_mail_font_size(parse_migrated_u32_in_range(key, value, 8, 48)?);
            }
            "mark-read-on-selection" => {
                self.set_mark_read_on_selection(parse_migrated_bool(key, value)?);
            }
            "show-message-preview" => {
                self.set_show_message_preview(parse_migrated_bool(key, value)?);
            }
            "mail-show-security-details" => {
                self.set_mail_show_security_details(parse_migrated_bool(key, value)?);
            }
            "mail-show-all-headers" => {
                self.set_mail_show_all_headers(parse_migrated_bool(key, value)?);
            }
            "mail-normal-headers" | "show-headers" => {
                let headers = parse_mail_normal_headers_value(value)?;
                self.set_mail_normal_headers(&headers);
            }
            "mail-show-statusbar" => {
                self.set_mail_show_statusbar(parse_migrated_bool(key, value)?);
            }
            "mail-default-folder" => self.set_mail_default_folder(value),
            "mail-layout" => self.set_mail_layout(value),
            "mail-show-toolbar" => self.set_mail_show_toolbar(parse_migrated_bool(key, value)?),
            "mail-show-sidebar" => self.set_mail_show_sidebar(parse_migrated_bool(key, value)?),
            "mail-sidebar-position" => {
                self.set_mail_sidebar_position(parse_migrated_i32_in_range(key, value, 0, 2_000)?);
            }
            "mail-modern-preview-position" => {
                self.set_mail_preview_position(
                    "modern",
                    parse_migrated_i32_in_range(key, value, 120, 4_096)?,
                );
            }
            "mail-classic-preview-position" => {
                self.set_mail_preview_position(
                    "classic",
                    parse_migrated_i32_in_range(key, value, 120, 4_096)?,
                );
            }
            "mail-row-density" => self.set_mail_row_density(value),
            "mail-show-avatar" => self.set_mail_show_avatar(parse_migrated_bool(key, value)?),
            "mail-show-sender" => self.set_mail_show_sender(parse_migrated_bool(key, value)?),
            "mail-show-recipient" => {
                self.set_mail_show_recipient(parse_migrated_bool(key, value)?);
            }
            "mail-show-date" => self.set_mail_show_date(parse_migrated_bool(key, value)?),
            "mail-show-attachment" => {
                self.set_mail_show_attachment(parse_migrated_bool(key, value)?);
            }
            "mail-show-preview-text" => {
                self.set_mail_show_preview_text(parse_migrated_bool(key, value)?);
            }
            "mail-show-labels" => self.set_mail_show_labels(parse_migrated_bool(key, value)?),
            "mail-show-size" => self.set_mail_show_size(parse_migrated_bool(key, value)?),
            "mail-show-follow-up-due" => {
                self.set_mail_show_follow_up_due(parse_migrated_bool(key, value)?);
            }
            "mail-show-location" => self.set_mail_show_location(parse_migrated_bool(key, value)?),
            "mail-sender-column-width" => {
                let value = parse_migrated_i32_in_range(key, value, 80, 360)?;
                self.set_mail_sender_column_width(value);
            }
            "color-scheme" => self.set_color_scheme(value),
            "sign-new-messages" => self.set_sign_new_messages(parse_migrated_bool(key, value)?),
            "default-pgp-key" => self.set_default_pgp_key(value),
            "crypto-protocol" => self.set_crypto_protocol(value),
            "smime-signer-certificate" => self.set_smime_signer_certificate(value),
            "smime-private-key" => {
                return Err("geheimes S/MIME-Schlüsselfeld wird nicht importiert".to_string());
            }
            "smime-recipient-certificates" => self.set_smime_recipient_certificates(
                &value
                    .split('\n')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            ),
            "smime-ca-file" => self.set_smime_ca_file(value),
            "mail-sync-interval" => {
                self.set_mail_sync_interval_seconds(parse_migrated_u32_in_range(
                    key, value, 0, 86_400,
                )?);
            }
            "mail-sync-message-limit" => {
                self.set_mail_sync_message_limit(parse_migrated_u32_in_range(
                    key,
                    value,
                    MIN_MAIL_SYNC_MESSAGE_LIMIT as u32,
                    MAX_MAIL_SYNC_MESSAGE_LIMIT as u32,
                )?);
            }
            "draft-autosave-interval" => {
                self.set_draft_autosave_interval_seconds(parse_migrated_u32_in_range(
                    key, value, 0, 3_600,
                )?);
            }
            "append-signature" => self.set_append_signature(parse_migrated_bool(key, value)?),
            "signature-text" => self.set_signature_text(value),
            "local-mail-directory" | "local-calendar-directory" | "local-contact-directory" => {
                let value = parse_migrated_directory(key, value)?;
                match key {
                    "local-mail-directory" => self.set_local_mail_directory(&value),
                    "local-calendar-directory" => self.set_local_calendar_directory(&value),
                    "local-contact-directory" => self.set_local_contact_directory(&value),
                    _ => unreachable!("directory key matched above"),
                }
            }
            "quote-original" => self.set_quote_original(parse_migrated_bool(key, value)?),
            "quote-style" => self.set_quote_style(value),
            "mail-sort-field" => self.set_mail_sort_field(value),
            "mail-sort-descending" => {
                self.set_mail_sort_descending(parse_migrated_bool(key, value)?);
            }
            "mail-threading" => self.set_mail_threading(parse_migrated_bool(key, value)?),
            "mail-folder-sort-order" => {
                let value = normalize_mail_folder_sort_order(value)?;
                self.set_mail_folder_sort_order(&value);
            }
            "mail-show-deleted" => self.set_mail_show_deleted(parse_migrated_bool(key, value)?),
            "task-filter" => self.set_task_filter(value),
            "task-category-filter" => self.set_task_category_filter(value),
            "task-search-mode" => self.set_task_search_mode(value),
            "task-source-filter" => self.set_task_source_filter(value),
            "task-sort-field" => self.set_task_sort_field(value),
            "task-sort-descending" => {
                self.set_task_sort_descending(parse_migrated_bool(key, value)?);
            }
            "task-preview-visible" => {
                self.set_task_preview_visible(parse_migrated_bool(key, value)?);
            }
            "task-preview-orientation" => {
                let value = normalize_task_preview_orientation_value(value)?;
                self.set_string("task-preview-orientation", &value);
            }
            "task-hpane-position" => self.set_task_preview_position(
                "vertical",
                parse_migrated_i32_in_range(key, value, 80, 4_096)?,
            ),
            "task-vpane-position" => self.set_task_preview_position(
                "classic",
                parse_migrated_i32_in_range(key, value, 80, 4_096)?,
            ),
            "memo-sort-field" => self.set_memo_sort_field(value),
            "memo-source-filter" => {
                let value = normalize_memo_source_filter_value(value)?;
                self.set_string("memo-source-filter", &value);
            }
            "memo-sort-descending" => {
                self.set_memo_sort_descending(parse_migrated_bool(key, value)?);
            }
            "memo-search-mode" => {
                let value = normalize_memo_search_mode_value(value)?;
                self.set_string("memo-search-mode", &value);
            }
            "memo-category-filter" => {
                let value = validate_memo_category_filter_value(value)?;
                self.set_string("memo-category-filter", &value);
            }
            "memo-preview-visible" => {
                self.set_memo_preview_visible(parse_migrated_bool(key, value)?);
            }
            "memo-preview-orientation" => {
                let value = normalize_memo_preview_orientation_value(value)?;
                self.set_string("memo-preview-orientation", &value);
            }
            _ => return Err(format!("unbekannter Migrationsschlüssel „{key}“")),
        }
        Ok(())
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
            "switcher-location",
            "default-account",
            "default-calendar-source",
            "default-address-book-source",
            "default-task-source",
            "default-memo-source",
            "calendar-view",
            "calendar-week-start",
            "calendar-workdays",
            "calendar-workday-start",
            "calendar-workday-end",
            "calendar-default-reminder",
            "notifications-enabled",
            "confirm-destructive-actions",
            "calendar-show-week-numbers",
            "date-format",
            "time-format",
            "calendar-time-zone",
            "auto-load-remote-images",
            "offline-mode",
            "force-dark-mode",
            "render-html",
            "composer-html",
            "composer-charset",
            "request-read-receipt",
            "spellcheck-languages",
            "mail-font-family",
            "mail-font-size",
            "mark-read-on-selection",
            "show-message-preview",
            "mail-show-security-details",
            "mail-show-all-headers",
            "mail-normal-headers",
            "mail-show-statusbar",
            "mail-default-folder",
            "mail-layout",
            "mail-show-toolbar",
            "mail-show-sidebar",
            "mail-sidebar-position",
            "mail-modern-preview-position",
            "mail-classic-preview-position",
            "mail-row-density",
            "mail-show-avatar",
            "mail-show-sender",
            "mail-show-recipient",
            "mail-show-date",
            "mail-show-attachment",
            "mail-show-preview-text",
            "mail-show-labels",
            "mail-show-size",
            "mail-show-follow-up-due",
            "mail-show-location",
            "mail-sender-column-width",
            "color-scheme",
            "sign-new-messages",
            "default-pgp-key",
            "crypto-protocol",
            "smime-signer-certificate",
            "smime-private-key",
            "smime-recipient-certificates",
            "smime-ca-file",
            "mail-sync-interval",
            "mail-sync-message-limit",
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
            "mail-folder-sort-order",
            "mail-show-deleted",
            "task-filter",
            "task-category-filter",
            "task-search-mode",
            "task-source-filter",
            "task-sort-field",
            "task-sort-descending",
            "task-preview-visible",
            "task-preview-orientation",
            "task-hpane-position",
            "task-vpane-position",
            "memo-sort-field",
            "memo-source-filter",
            "memo-sort-descending",
            "memo-search-mode",
            "memo-category-filter",
            "memo-preview-visible",
            "memo-preview-orientation",
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

    fn set_string(&self, key: &str, value: &str) {
        if let Err(error) = self.settings.set_string(key, value) {
            tracing::warn!(key, %error, "Could not persist application setting");
        }
    }
}

fn parse_migrated_bool(key: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(format!("ungültiger boolescher Wert für „{key}“")),
    }
}

fn parse_migrated_i32(key: &str, value: &str) -> Result<i32, String> {
    value
        .trim()
        .parse::<i32>()
        .map_err(|_| format!("ungültiger Ganzzahlwert für „{key}“"))
}

fn parse_migrated_i32_in_range(
    key: &str,
    value: &str,
    minimum: i32,
    maximum: i32,
) -> Result<i32, String> {
    let value = parse_migrated_i32(key, value)?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!(
            "Wert für „{key}“ liegt außerhalb des Bereichs {minimum}..={maximum}"
        ));
    }
    Ok(value)
}

fn parse_migrated_u32(key: &str, value: &str) -> Result<u32, String> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("ungültiger positiver Ganzzahlwert für „{key}“"))
}

fn parse_migrated_u32_in_range(
    key: &str,
    value: &str,
    minimum: u32,
    maximum: u32,
) -> Result<u32, String> {
    let value = parse_migrated_u32(key, value)?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!(
            "Wert für „{key}“ liegt außerhalb des Bereichs {minimum}..={maximum}"
        ));
    }
    Ok(value)
}

fn parse_migrated_directory(key: &str, value: &str) -> Result<String, String> {
    if value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(format!("ungültiger Pfadwert für „{key}“"));
    }
    Ok(value.to_string())
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

fn sanitize_spellcheck_languages(value: &str) -> String {
    let mut languages = Vec::new();
    for language in value.split(',').map(str::trim).filter(|language| {
        !language.is_empty()
            && language.len() <= 35
            && language
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    }) {
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    languages.join(",")
}

fn sanitize_calendar_workdays(value: &str) -> String {
    const DAYS: [&str; 7] = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    let selected = DAYS
        .into_iter()
        .filter(|day| {
            value
                .split(|character: char| character == ',' || character.is_ascii_whitespace())
                .any(|candidate| candidate.eq_ignore_ascii_case(day))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        "monday,tuesday,wednesday,thursday,friday".to_string()
    } else {
        selected.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, DEFAULT_MAIL_NORMAL_HEADERS, MAX_MAIL_NORMAL_HEADERS,
        clamp_mail_sync_message_limit, default_mail_normal_headers, normalize_composer_charset,
        normalized_mail_sort_field, normalized_switcher_location, sanitize_calendar_workdays,
        sanitize_spellcheck_languages, validate_mail_normal_header_name,
    };
    use gtk::gio;
    use gtk::gio::prelude::*;

    fn settings_schema() -> gio::SettingsSchema {
        let installed_source = gio::SettingsSchemaSource::default();
        installed_source
            .as_ref()
            .and_then(|source| source.lookup(crate::APP_ID, true))
            .or_else(|| {
                let directory = option_env!("EVOLUTION_GTK4_SCHEMA_DIR")?;
                let source = gio::SettingsSchemaSource::from_directory(
                    directory,
                    installed_source.as_ref(),
                    true,
                )
                .ok()?;
                source.lookup(crate::APP_ID, true)
            })
            .expect("the application settings schema should be available in tests")
    }

    fn settings_with_backend(backend: &gio::SettingsBackend) -> AppSettings {
        AppSettings {
            settings: gio::Settings::new_full(&settings_schema(), Some(backend), None),
        }
    }

    #[test]
    fn spellcheck_language_values_are_sanitized_and_deduplicated() {
        assert_eq!(
            sanitize_spellcheck_languages(" de-DE,invalid! ,en-US,de-DE "),
            "de-DE,en-US"
        );
        assert_eq!(sanitize_spellcheck_languages(""), "");
    }

    #[test]
    fn calendar_workdays_are_normalized_in_week_order() {
        assert_eq!(
            sanitize_calendar_workdays("sunday,Monday,friday,Monday"),
            "monday,friday,sunday"
        );
        assert_eq!(
            sanitize_calendar_workdays(""),
            "monday,tuesday,wednesday,thursday,friday"
        );
    }

    #[test]
    fn mail_sort_field_accepts_important_and_rejects_unknown_values() {
        assert_eq!(normalized_mail_sort_field("important"), "important");
        assert_eq!(normalized_mail_sort_field("unknown"), "date");
    }

    #[test]
    fn mail_folder_sort_order_round_trips_as_portable_text() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        assert_eq!(settings.mail_folder_sort_order(), "{}");
        settings.set_mail_folder_sort_order(r#"{"account\u0000Inbox":1}"#);
        assert_eq!(
            settings_with_backend(&backend).mail_folder_sort_order(),
            r#"{"account\u0000Inbox":1}"#
        );
        settings_with_backend(&backend)
            .apply_migrated_setting("mail-folder-sort-order", r#"{"custom:Projects":2}"#)
            .expect("valid folder order should migrate");
        assert_eq!(
            settings_with_backend(&backend).mail_folder_sort_order(),
            r#"{"custom:Projects":2}"#
        );
        assert!(
            settings_with_backend(&backend)
                .apply_migrated_setting("mail-folder-sort-order", "not-json")
                .is_err()
        );
        settings_with_backend(&backend).reset_to_defaults();
        assert_eq!(
            settings_with_backend(&backend).mail_folder_sort_order(),
            "{}"
        );
    }

    #[test]
    fn mail_sync_message_limit_is_bounded_for_safe_page_sizes() {
        assert_eq!(clamp_mail_sync_message_limit(0), 25);
        assert_eq!(clamp_mail_sync_message_limit(100), 100);
        assert_eq!(clamp_mail_sync_message_limit(10_000), 1_000);
    }

    #[test]
    fn normal_mail_header_defaults_match_evolution_order_and_visibility() {
        assert_eq!(
            default_mail_normal_headers(),
            DEFAULT_MAIL_NORMAL_HEADERS
                .iter()
                .map(|(name, visible)| ((*name).to_string(), *visible))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn normal_mail_header_names_reject_injection_and_normalize_spacing() {
        assert_eq!(
            validate_mail_normal_header_name("  X-Trace  ").unwrap(),
            "X-Trace"
        );
        assert!(validate_mail_normal_header_name("").is_err());
        assert!(validate_mail_normal_header_name("X-Bad: value").is_err());
        assert!(validate_mail_normal_header_name("X-Bad\r\nInjected").is_err());
        assert!(validate_mail_normal_header_name("ä-header").is_err());
    }

    #[test]
    fn normal_mail_headers_are_deduplicated_bounded_and_ordered() {
        let mut input = vec![
            ("  X-First  ".to_string(), false),
            ("x-first".to_string(), true),
            ("X:Invalid".to_string(), true),
            ("X-Second".to_string(), true),
            ("".to_string(), true),
        ];
        input.extend(
            (0..(MAX_MAIL_NORMAL_HEADERS + 4))
                .map(|index| (format!("X-Extra-{index}"), index % 2 == 0)),
        );

        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        settings.set_mail_normal_headers(&input);
        let headers = settings.mail_normal_headers();

        assert_eq!(headers[0], ("X-First".to_string(), false));
        assert_eq!(headers[1], ("X-Second".to_string(), true));
        assert_eq!(headers.len(), MAX_MAIL_NORMAL_HEADERS);
        assert_eq!(
            headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("x-first"))
                .count(),
            1
        );

        settings.set_mail_normal_headers(&[]);
        assert!(
            settings
                .settings
                .value("mail-normal-headers")
                .get::<Vec<(String, bool)>>()
                .is_some_and(|headers| headers.is_empty())
        );
        assert_eq!(
            settings.mail_normal_headers(),
            default_mail_normal_headers()
        );
    }

    #[test]
    fn normal_mail_headers_round_trip_and_migration_use_a_sb_storage() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        let custom = vec![
            ("Subject".to_string(), false),
            ("X-Project".to_string(), true),
        ];
        settings.set_mail_normal_headers(&custom);

        let reloaded = settings_with_backend(&backend);
        assert_eq!(reloaded.mail_normal_headers(), custom);

        reloaded
            .apply_migrated_setting(
                "mail-normal-headers",
                "[('From', true), ('x-test', false), ('X-Other', true)]",
            )
            .expect("GVariant show-headers value should migrate");
        assert_eq!(
            reloaded.mail_normal_headers(),
            vec![
                ("From".to_string(), true),
                ("x-test".to_string(), false),
                ("X-Other".to_string(), true),
            ]
        );
        assert!(
            reloaded
                .apply_migrated_setting("mail-normal-headers", "[('From', true), ('from', false)]",)
                .is_err()
        );

        reloaded.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.mail_normal_headers(), default_mail_normal_headers());
    }

    #[test]
    fn mail_security_details_and_font_size_defaults_match_the_mail_baseline() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        assert!(!settings.mail_show_security_details());
        assert!(!settings.mail_show_all_headers());
        assert!(!settings.mail_show_deleted());
        assert!(settings.mail_show_statusbar());
        assert_eq!(settings.mail_font_size(), 16);
    }

    #[test]
    fn mail_list_metadata_column_settings_migrate_and_reset() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        assert!(!settings.mail_show_size());
        assert!(!settings.mail_show_follow_up_due());
        assert!(!settings.mail_show_location());

        for key in [
            "mail-show-size",
            "mail-show-follow-up-due",
            "mail-show-location",
        ] {
            settings
                .apply_migrated_setting(key, "true")
                .expect("metadata column setting should migrate");
        }
        assert!(settings.mail_show_size());
        assert!(settings.mail_show_follow_up_due());
        assert!(settings.mail_show_location());

        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert!(!reset.mail_show_size());
        assert!(!reset.mail_show_follow_up_due());
        assert!(!reset.mail_show_location());
    }

    #[test]
    fn mail_security_details_and_custom_font_size_round_trip() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        settings.set_mail_show_security_details(true);
        settings.set_mail_show_all_headers(true);
        settings.set_mail_show_deleted(true);
        settings.set_mail_show_statusbar(false);
        settings.set_mail_font_size(23);

        let reloaded = settings_with_backend(&backend);
        assert!(reloaded.mail_show_security_details());
        assert!(reloaded.mail_show_all_headers());
        assert!(reloaded.mail_show_deleted());
        assert!(!reloaded.mail_show_statusbar());
        assert_eq!(reloaded.mail_font_size(), 23);

        reloaded.set_mail_show_security_details(false);
        reloaded.set_mail_show_all_headers(false);
        reloaded.set_mail_show_deleted(false);
        reloaded.set_mail_show_statusbar(true);
        reloaded.set_mail_font_size(11);
        let updated = settings_with_backend(&backend);
        assert!(!updated.mail_show_security_details());
        assert!(!updated.mail_show_all_headers());
        assert!(!updated.mail_show_deleted());
        assert!(updated.mail_show_statusbar());
        assert_eq!(updated.mail_font_size(), 11);

        updated.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert!(!reset.mail_show_security_details());
        assert!(!reset.mail_show_all_headers());
        assert!(!reset.mail_show_deleted());
        assert!(reset.mail_show_statusbar());
        assert_eq!(reset.mail_font_size(), 16);
    }

    #[test]
    fn composer_charset_is_canonical_persistent_and_resettable() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        assert_eq!(settings.composer_charset(), "UTF-8");

        settings.set_composer_charset("latin-1");
        let reloaded = settings_with_backend(&backend);
        assert_eq!(reloaded.composer_charset(), "ISO-8859-1");
        assert_eq!(reloaded.settings.string("composer-charset"), "ISO-8859-1");

        reloaded.set_composer_charset("not-a-charset");
        assert_eq!(reloaded.composer_charset(), "UTF-8");
        reloaded.reset_to_defaults();
        assert_eq!(reloaded.composer_charset(), "UTF-8");
    }

    #[test]
    fn set01_settings_round_trip_and_reject_invalid_portable_values() {
        assert_eq!(normalized_switcher_location("top"), "top");
        assert_eq!(normalized_switcher_location("unknown"), "bottom-left");
        assert_eq!(normalize_composer_charset("utf8"), Some("UTF-8"));
        assert_eq!(normalize_composer_charset("unknown"), None);

        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        for (key, value) in [
            ("switcher-location", "top"),
            ("mail-sender-column-width", "240"),
            ("mail-show-security-details", "true"),
            ("mail-show-all-headers", "true"),
            ("mail-show-statusbar", "false"),
            ("local-mail-directory", "/portable/mail"),
            ("local-calendar-directory", "/portable/calendar"),
            ("local-contact-directory", "/portable/contacts"),
            ("composer-charset", "ISO-8859-15"),
            ("mail-sync-message-limit", "250"),
        ] {
            settings
                .apply_migrated_setting(key, value)
                .unwrap_or_else(|error| panic!("{key} should be accepted: {error}"));
        }

        let reloaded = settings_with_backend(&backend);
        assert_eq!(reloaded.switcher_location(), "top");
        assert_eq!(reloaded.mail_sender_column_width(), 240);
        assert!(reloaded.mail_show_security_details());
        assert!(reloaded.mail_show_all_headers());
        assert!(!reloaded.mail_show_statusbar());
        assert_eq!(reloaded.local_mail_directory(), "/portable/mail");
        assert_eq!(reloaded.local_calendar_directory(), "/portable/calendar");
        assert_eq!(reloaded.local_contact_directory(), "/portable/contacts");
        assert_eq!(reloaded.composer_charset(), "ISO-8859-15");
        assert_eq!(reloaded.mail_sync_message_limit(), 250);

        for (key, value) in [
            ("switcher-location", "sidebar"),
            ("mail-sender-column-width", "79"),
            ("mail-show-statusbar", "not-a-bool"),
            ("local-mail-directory", "/bad\npath"),
            ("composer-charset", "X-UNKNOWN"),
            ("mail-sync-message-limit", "24"),
        ] {
            assert!(
                reloaded.apply_migrated_setting(key, value).is_err(),
                "{key}"
            );
        }
        let error = reloaded
            .apply_migrated_setting("smime-private-key", "private-key-path")
            .expect_err("private key migration must be refused");
        assert!(!error.contains("private-key-path"));

        reloaded.reset_to_defaults();
        assert_eq!(reloaded.switcher_location(), "bottom-left");
        assert_eq!(reloaded.mail_sender_column_width(), 180);
        assert!(!reloaded.mail_show_security_details());
        assert!(!reloaded.mail_show_all_headers());
        assert!(reloaded.mail_show_statusbar());
        assert!(reloaded.local_mail_directory().is_empty());
        assert!(reloaded.local_calendar_directory().is_empty());
        assert!(reloaded.local_contact_directory().is_empty());
        assert_eq!(reloaded.composer_charset(), "UTF-8");
        assert_eq!(reloaded.mail_sync_message_limit(), 100);
    }

    #[test]
    fn task_filter_values_round_trip_with_validation_and_migration_aliases() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        for value in [
            "all",
            "today",
            "next-7",
            "next-30",
            "next-365",
            "important",
            "overdue",
            "in-progress",
            "cancelled",
            "completed",
            "uncompleted",
            "scheduled",
            "no-due",
            "with-attachments",
            "started",
            "without-category",
        ] {
            settings.set_task_filter(value);
            assert_eq!(settings.task_filter(), value);
        }
        settings.set_task_filter("next-30");
        settings.set_task_category_filter("  Work Team  ");
        settings.set_task_search_mode("any-field");
        assert_eq!(settings.task_search_mode(), "any-field");
        settings.set_task_search_mode("advanced");
        let reloaded = settings_with_backend(&backend);
        assert_eq!(reloaded.task_filter(), "next-30");
        assert_eq!(reloaded.task_category_filter(), "Work Team");
        assert_eq!(reloaded.task_search_mode(), "advanced");

        reloaded.set_task_filter("not-a-filter");
        reloaded.set_task_category_filter("bad\ncategory");
        reloaded.set_task_search_mode("not-a-mode");
        reloaded.set_task_preview_orientation("diagonal");
        assert_eq!(reloaded.task_filter(), "all");
        assert!(reloaded.task_category_filter().is_empty());
        assert_eq!(reloaded.task_search_mode(), "any-field");
        assert_eq!(reloaded.task_preview_orientation(), "classic");

        assert!(
            reloaded
                .settings
                .set_string("task-filter", "not-a-filter")
                .is_err()
        );
        assert_eq!(reloaded.task_filter(), "all");

        reloaded
            .apply_migrated_setting("task-filter", "active")
            .expect("upstream active filter should migrate");
        reloaded
            .apply_migrated_setting("task-search-mode", "description")
            .expect("search mode should migrate");
        reloaded
            .apply_migrated_setting("task-category-filter", "  Finance  ")
            .expect("category filter should migrate");
        reloaded
            .apply_migrated_setting("task-preview-visible", "false")
            .expect("task preview visibility should migrate");
        reloaded
            .apply_migrated_setting("task-preview-orientation", "vertical")
            .expect("task preview orientation should migrate");
        reloaded
            .apply_migrated_setting("task-hpane-position", "275")
            .expect("horizontal task preview position should migrate");
        reloaded
            .apply_migrated_setting("task-vpane-position", "525")
            .expect("vertical task preview position should migrate");
        assert_eq!(reloaded.task_filter(), "next-365");
        assert_eq!(reloaded.task_search_mode(), "description");
        assert_eq!(reloaded.task_category_filter(), "Finance");
        assert!(!reloaded.task_preview_visible());
        assert_eq!(reloaded.task_preview_orientation(), "vertical");
        assert_eq!(reloaded.task_preview_position("vertical"), 275);
        assert_eq!(reloaded.task_preview_position("classic"), 525);
        assert!(
            reloaded
                .apply_migrated_setting("task-preview-visible", "maybe")
                .is_err()
        );
        assert!(
            reloaded
                .apply_migrated_setting("task-preview-orientation", "diagonal")
                .is_err()
        );

        reloaded.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.task_filter(), "all");
        assert!(reset.task_category_filter().is_empty());
        assert_eq!(reset.task_search_mode(), "any-field");
        assert!(reset.task_preview_visible());
        assert_eq!(reset.task_preview_orientation(), "classic");
        assert_eq!(reset.task_preview_position("vertical"), 200);
        assert_eq!(reset.task_preview_position("classic"), 400);
    }

    #[test]
    fn memo_surface_settings_round_trip_migrate_and_reset_strictly() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        settings
            .apply_migrated_setting("memo-search-mode", "description")
            .expect("memo search mode should migrate");
        settings
            .apply_migrated_setting("memo-category-filter", "  Work Team  ")
            .expect("memo category should migrate");
        settings
            .apply_migrated_setting("memo-source-filter", "  source-a  ")
            .expect("memo source should migrate");
        settings
            .apply_migrated_setting("memo-preview-visible", "false")
            .expect("memo preview visibility should migrate");
        settings
            .apply_migrated_setting("memo-preview-orientation", "vertical")
            .expect("memo preview orientation should migrate");

        let reloaded = settings_with_backend(&backend);
        assert_eq!(reloaded.memo_search_mode(), "description");
        assert_eq!(reloaded.memo_category_filter(), "Work Team");
        assert_eq!(reloaded.memo_source_filter(), "source-a");
        assert!(!reloaded.memo_preview_visible());
        assert_eq!(reloaded.memo_preview_orientation(), "vertical");

        for (key, value) in [
            ("memo-search-mode", "unknown"),
            ("memo-category-filter", "\"quoted\""),
            ("memo-category-filter", ""),
            ("memo-source-filter", "source a"),
            ("memo-source-filter", "source\"a"),
            ("memo-preview-visible", "maybe"),
            ("memo-preview-orientation", "diagonal"),
        ] {
            assert!(
                reloaded.apply_migrated_setting(key, value).is_err(),
                "{key} should reject invalid migration values"
            );
        }

        reloaded.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.memo_search_mode(), "any-field");
        assert_eq!(reset.memo_category_filter(), "any");
        assert_eq!(reset.memo_source_filter(), "");
        assert!(reset.memo_preview_visible());
        assert_eq!(reset.memo_preview_orientation(), "classic");
    }
}
