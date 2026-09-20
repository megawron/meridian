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

pub(crate) const MAIL_LIST_COLUMN_IDS: [&str; 15] = [
    "status",
    "attachment",
    "flagged",
    "sender",
    "recipient",
    "subject",
    "date",
    "follow-up",
    "important",
    "avatar",
    "preview",
    "size",
    "follow-up-due",
    "location",
    "messages",
];

const MAIL_LIST_WIDE_COLUMN_IDS: [&str; 15] = [
    "status",
    "flagged",
    "messages",
    "attachment",
    "date",
    "follow-up",
    "important",
    "avatar",
    "preview",
    "size",
    "follow-up-due",
    "location",
    "sender",
    "recipient",
    "subject",
];

pub(crate) fn normalized_mail_list_view_profile(value: &str) -> &'static str {
    match value {
        "wide" => "wide",
        "cards" => "cards",
        "custom" => "custom",
        _ => "messages",
    }
}

pub(crate) fn normalized_mail_header_style(value: &str) -> &'static str {
    match value {
        "classic" => "classic",
        _ => "modern",
    }
}

pub(crate) fn mail_list_profile_column_order(profile: &str) -> Vec<String> {
    let columns = if normalized_mail_list_view_profile(profile) == "wide" {
        &MAIL_LIST_WIDE_COLUMN_IDS
    } else {
        &MAIL_LIST_COLUMN_IDS
    };
    columns.iter().map(|column| (*column).to_string()).collect()
}

pub(crate) fn normalize_mail_list_column_order(value: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut order = value
        .split(',')
        .map(str::trim)
        .filter(|column| MAIL_LIST_COLUMN_IDS.contains(column))
        .filter(|column| seen.insert((*column).to_string()))
        .map(str::to_string)
        .collect::<Vec<_>>();
    order.extend(
        MAIL_LIST_COLUMN_IDS
            .iter()
            .filter(|column| seen.insert((**column).to_string()))
            .map(|column| (*column).to_string()),
    );
    order
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

pub(crate) fn normalize_composer_reply_style(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "quoted" => Some("quoted"),
        "inline" => Some("inline"),
        "attach" | "attached" => Some("attach"),
        "do-not-quote" | "none" => Some("do-not-quote"),
        _ => None,
    }
}

pub(crate) fn normalize_composer_forward_style(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "attached" | "attach" => Some("attached"),
        "inline" => Some("inline"),
        "quoted" => Some("quoted"),
        _ => None,
    }
}

pub(crate) fn normalize_image_loading_policy(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "never" => Some("never"),
        "always" => Some("always"),
        "sometimes" | "contacts" => Some("sometimes"),
        _ => None,
    }
}

fn clamp_mail_sync_message_limit(value: i32) -> i32 {
    value.clamp(MIN_MAIL_SYNC_MESSAGE_LIMIT, MAX_MAIL_SYNC_MESSAGE_LIMIT)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MailLabelDefinition {
    pub name: String,
    pub color: String,
    pub tag: String,
}

impl MailLabelDefinition {
    pub fn new(name: impl Into<String>, color: impl Into<String>, tag: Option<String>) -> Self {
        let name = name.into().trim().to_string();
        let color = normalize_hex_color(&color.into());
        let tag = tag
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| Self::tag_from_name(&name));
        Self { name, color, tag }
    }

    pub fn display_name(&self) -> String {
        self.name.replace('_', "")
    }

    pub fn tag_from_name(name: &str) -> String {
        let clean = name.replace('_', "");
        let clean = clean.trim().to_ascii_lowercase();
        match clean.as_str() {
            "important" | "wichtig" => "$Labelimportant".to_string(),
            "work" | "arbeit" => "$Labelwork".to_string(),
            "personal" | "persönlich" | "persoenlich" => "$Labelpersonal".to_string(),
            "to do" | "todo" | "zu erledigen" => "$Labeltodo".to_string(),
            "later" | "später" | "spaeter" => "$Labellater".to_string(),
            _ => {
                let mut sanitized = String::new();
                for c in clean.chars() {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        sanitized.push(c);
                    } else if c.is_whitespace() {
                        sanitized.push('_');
                    }
                }
                if sanitized.is_empty() {
                    "custom_label".to_string()
                } else {
                    sanitized
                }
            }
        }
    }

    pub fn parse(entry: &str) -> Option<Self> {
        let entry = entry.trim();
        if entry.is_empty() {
            return None;
        }
        let (name_color, tag_part) = if let Some((nc, t)) = entry.split_once('|') {
            (nc.trim(), Some(t.trim().to_string()))
        } else {
            (entry, None)
        };
        let (name, color) = if let Some((n, c)) = name_color.split_once(':') {
            (n.trim(), c.trim())
        } else {
            (name_color, "#3465A4")
        };
        if name.is_empty() {
            return None;
        }
        let color = normalize_hex_color(color);
        let tag = tag_part
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| Self::tag_from_name(name));
        Some(Self {
            name: name.to_string(),
            color,
            tag,
        })
    }

    pub fn to_setting_string(&self) -> String {
        if self.tag.starts_with('$') || !self.tag.is_empty() {
            format!("{}:{}|{}", self.name, self.color, self.tag)
        } else {
            format!("{}:{}", self.name, self.color)
        }
    }

    pub fn default_labels() -> Vec<Self> {
        vec![
            Self {
                name: "Important".to_string(),
                color: "#EF2929".to_string(),
                tag: "$Labelimportant".to_string(),
            },
            Self {
                name: "Work".to_string(),
                color: "#F57900".to_string(),
                tag: "$Labelwork".to_string(),
            },
            Self {
                name: "Personal".to_string(),
                color: "#4E9A06".to_string(),
                tag: "$Labelpersonal".to_string(),
            },
            Self {
                name: "To Do".to_string(),
                color: "#3465A4".to_string(),
                tag: "$Labeltodo".to_string(),
            },
            Self {
                name: "Later".to_string(),
                color: "#75507B".to_string(),
                tag: "$Labellater".to_string(),
            },
        ]
    }
}

pub fn normalize_hex_color(color: &str) -> String {
    let trimmed = color.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if (hex.len() == 6 || hex.len() == 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", &hex[..6].to_ascii_uppercase())
    } else if hex.len() == 3 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut expanded = String::with_capacity(7);
        expanded.push('#');
        for c in hex.chars() {
            expanded.push(c.to_ascii_uppercase());
            expanded.push(c.to_ascii_uppercase());
        }
        expanded
    } else {
        "#3465A4".to_string()
    }
}

thread_local! {
    static SHARED_SETTINGS: std::cell::RefCell<Option<gio::Settings>> = const { std::cell::RefCell::new(None) };
}

impl AppSettings {
    /// Open the application schema if it is available in the current process.
    pub fn try_new() -> Option<Self> {
        SHARED_SETTINGS.with(|cell| {
            let mut borrow = cell.borrow_mut();
            if let Some(s) = borrow.as_ref() {
                return Some(Self {
                    settings: s.clone(),
                });
            }
            let installed_source = gio::SettingsSchemaSource::default();
            let schema = installed_source
                .as_ref()
                .and_then(|source| source.lookup(crate::APP_ID, true))
                .or_else(|| {
                    let candidates = [
                        std::env::var("GSETTINGS_SCHEMA_DIR").ok(),
                        option_env!("EVOLUTION_GTK4_SCHEMA_DIR").map(|s| s.to_string()),
                        Some("data".to_string()),
                        Some("../data".to_string()),
                    ];
                    for dir in candidates.into_iter().flatten() {
                        if let Ok(source) = gio::SettingsSchemaSource::from_directory(
                            &dir,
                            installed_source.as_ref(),
                            true,
                        ) {
                            if let Some(schema) = source.lookup(crate::APP_ID, true) {
                                return Some(schema);
                            }
                        }
                    }
                    None
                })?;
            let s = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
            *borrow = Some(s.clone());
            Some(Self { settings: s })
        })
    }

    #[cfg(test)]
    pub(crate) fn try_new_with_backend(backend: &gio::SettingsBackend) -> Option<Self> {
        let installed_source = gio::SettingsSchemaSource::default();
        let schema = installed_source
            .as_ref()
            .and_then(|source| source.lookup(crate::APP_ID, true))
            .or_else(|| {
                let candidates = [
                    std::env::var("GSETTINGS_SCHEMA_DIR").ok(),
                    option_env!("EVOLUTION_GTK4_SCHEMA_DIR").map(|s| s.to_string()),
                    Some("data".to_string()),
                    Some("../data".to_string()),
                ];
                for dir in candidates.into_iter().flatten() {
                    if let Ok(source) = gio::SettingsSchemaSource::from_directory(
                        &dir,
                        installed_source.as_ref(),
                        true,
                    ) {
                        if let Some(schema) = source.lookup(crate::APP_ID, true) {
                            return Some(schema);
                        }
                    }
                }
                None
            })?;
        Some(Self {
            settings: gio::Settings::new_full(&schema, Some(backend), None),
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
            "day" | "week" | "workweek" | "month" | "agenda" | "year" | "list" => value,
            _ => "month".to_string(),
        }
    }

    pub fn set_calendar_view(&self, value: &str) {
        let value = match value {
            "day" | "week" | "workweek" | "month" | "agenda" | "year" | "list" => value,
            _ => "month",
        };
        if let Err(error) = self.settings.set_string("calendar-view", value) {
            tracing::warn!(%error, "Could not persist calendar view");
        }
    }

    pub fn calendar_click_action(&self) -> String {
        let has_key = self
            .settings
            .settings_schema()
            .map(|s| s.has_key("calendar-click-action"))
            .unwrap_or(false);
        if has_key {
            let value = self.settings.string("calendar-click-action").to_string();
            if value == "create" {
                return "create".to_string();
            }
        }
        "select".to_string()
    }

    pub fn set_calendar_click_action(&self, value: &str) {
        let has_key = self
            .settings
            .settings_schema()
            .map(|s| s.has_key("calendar-click-action"))
            .unwrap_or(false);
        if has_key {
            let value = if value == "create" { "create" } else { "select" };
            if let Err(error) = self.settings.set_string("calendar-click-action", value) {
                tracing::warn!(%error, "Could not persist calendar click action");
            }
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

    pub fn use_default_reminder(&self) -> bool {
        self.settings.boolean("use-default-reminder")
    }

    pub fn set_use_default_reminder(&self, value: bool) {
        self.set_boolean("use-default-reminder", value);
    }

    pub fn default_reminder_interval(&self) -> u32 {
        self.settings
            .int("default-reminder-interval")
            .clamp(0, 100_000) as u32
    }

    pub fn set_default_reminder_interval(&self, value: u32) {
        let clamped = value.min(100_000);
        self.set_int("default-reminder-interval", clamped as i32);
        let minutes = self.default_reminder_units().to_minutes(clamped);
        self.set_calendar_default_reminder_minutes(minutes);
    }

    pub fn default_reminder_units(&self) -> ReminderDurationUnit {
        ReminderDurationUnit::from_str(&self.settings.string("default-reminder-units"))
    }

    pub fn set_default_reminder_units(&self, unit: ReminderDurationUnit) {
        self.set_string("default-reminder-units", unit.as_str());
        let minutes = unit.to_minutes(self.default_reminder_interval());
        self.set_calendar_default_reminder_minutes(minutes);
    }

    pub fn default_reminder_total_minutes(&self) -> u32 {
        self.default_reminder_units()
            .to_minutes(self.default_reminder_interval())
    }

    pub fn use_ba_reminder(&self) -> bool {
        self.settings.boolean("use-ba-reminder")
    }

    pub fn set_use_ba_reminder(&self, value: bool) {
        self.set_boolean("use-ba-reminder", value);
    }

    pub fn ba_reminder_interval(&self) -> u32 {
        self.settings.int("ba-reminder-interval").clamp(0, 100_000) as u32
    }

    pub fn set_ba_reminder_interval(&self, value: u32) {
        self.set_int("ba-reminder-interval", value.min(100_000) as i32);
    }

    pub fn ba_reminder_units(&self) -> ReminderDurationUnit {
        ReminderDurationUnit::from_str(&self.settings.string("ba-reminder-units"))
    }

    pub fn set_ba_reminder_units(&self, unit: ReminderDurationUnit) {
        self.set_string("ba-reminder-units", unit.as_str());
    }

    pub fn ba_reminder_total_minutes(&self) -> u32 {
        self.ba_reminder_units()
            .to_minutes(self.ba_reminder_interval())
    }

    pub fn ba_reminder_offset_seconds(&self) -> i64 {
        self.ba_reminder_units()
            .to_seconds(self.ba_reminder_interval()) as i64
    }

    pub fn hide_completed_tasks(&self) -> bool {
        self.settings.boolean("hide-completed-tasks")
    }

    pub fn set_hide_completed_tasks(&self, value: bool) {
        self.set_boolean("hide-completed-tasks", value);
    }

    pub fn hide_completed_tasks_value(&self) -> u32 {
        self.settings
            .int("hide-completed-tasks-value")
            .clamp(0, 100_000) as u32
    }

    pub fn set_hide_completed_tasks_value(&self, value: u32) {
        self.set_int("hide-completed-tasks-value", value.min(100_000) as i32);
    }

    pub fn hide_completed_tasks_units(&self) -> ReminderDurationUnit {
        ReminderDurationUnit::from_str(&self.settings.string("hide-completed-tasks-units"))
    }

    pub fn set_hide_completed_tasks_units(&self, unit: ReminderDurationUnit) {
        self.set_string("hide-completed-tasks-units", unit.as_str());
    }

    pub fn hide_completed_tasks_cutoff_timestamp(&self, now_secs: i64) -> Option<i64> {
        if !self.hide_completed_tasks() {
            return None;
        }
        let value = self.hide_completed_tasks_value();
        if value == 0 {
            Some(now_secs)
        } else {
            let delta = self.hide_completed_tasks_units().to_seconds(value) as i64;
            Some(now_secs.saturating_sub(delta))
        }
    }

    pub fn hide_cancelled_tasks(&self) -> bool {
        self.settings.boolean("hide-cancelled-tasks")
    }

    pub fn set_hide_cancelled_tasks(&self, value: bool) {
        self.set_boolean("hide-cancelled-tasks", value);
    }

    pub fn hide_cancelled_events(&self) -> bool {
        self.settings.boolean("hide-cancelled-events")
    }

    pub fn set_hide_cancelled_events(&self, value: bool) {
        self.set_boolean("hide-cancelled-events", value);
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

    pub fn calendar_show_tag_vpane(&self) -> bool {
        self.settings.boolean("calendar-show-tag-vpane")
    }

    pub fn set_calendar_show_tag_vpane(&self, value: bool) {
        self.set_boolean("calendar-show-tag-vpane", value);
    }

    pub fn calendar_tag_vpane_position(&self) -> i32 {
        let val = self.settings.int("calendar-tag-vpane-position");
        if val <= 0 { 320 } else { val }
    }

    pub fn set_calendar_tag_vpane_position(&self, value: i32) {
        self.set_int("calendar-tag-vpane-position", value.clamp(100, 2000));
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
        let source_uids = sanitize_source_list(source_uids);
        if let Err(error) = self
            .settings
            .set_string("calendar-hidden-sources", &source_uids.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden calendar sources");
        }
    }

    pub fn task_hidden_sources(&self) -> Vec<String> {
        self.settings
            .string("task-hidden-sources")
            .split(',')
            .map(str::trim)
            .filter(|uid| !uid.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_task_hidden_sources(&self, source_uids: &[String]) {
        let source_uids = sanitize_source_list(source_uids);
        if let Err(error) = self
            .settings
            .set_string("task-hidden-sources", &source_uids.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden task sources");
        }
    }

    pub fn memo_hidden_sources(&self) -> Vec<String> {
        self.settings
            .string("memo-hidden-sources")
            .split(',')
            .map(str::trim)
            .filter(|uid| !uid.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_memo_hidden_sources(&self, source_uids: &[String]) {
        let source_uids = sanitize_source_list(source_uids);
        if let Err(error) = self
            .settings
            .set_string("memo-hidden-sources", &source_uids.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden memo sources");
        }
    }

    pub fn addressbook_hidden_sources(&self) -> Vec<String> {
        self.settings
            .string("addressbook-hidden-sources")
            .split(',')
            .map(str::trim)
            .filter(|uid| !uid.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_addressbook_hidden_sources(&self, source_uids: &[String]) {
        let source_uids = sanitize_source_list(source_uids);
        if let Err(error) = self
            .settings
            .set_string("addressbook-hidden-sources", &source_uids.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden address book sources");
        }
    }

    pub fn calendar_hidden_groups(&self) -> Vec<String> {
        self.settings
            .string("calendar-hidden-groups")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_calendar_hidden_groups(&self, groups: &[String]) {
        let groups = sanitize_source_list(groups);
        if let Err(error) = self
            .settings
            .set_string("calendar-hidden-groups", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden calendar groups");
        }
    }

    pub fn calendar_groups_order(&self) -> Vec<String> {
        self.settings
            .string("calendar-groups-order")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_calendar_groups_order(&self, groups: &[String]) {
        let groups = sanitize_ordered_list(groups);
        if let Err(error) = self
            .settings
            .set_string("calendar-groups-order", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist calendar groups order");
        }
    }

    pub fn task_hidden_groups(&self) -> Vec<String> {
        self.settings
            .string("task-hidden-groups")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_task_hidden_groups(&self, groups: &[String]) {
        let groups = sanitize_source_list(groups);
        if let Err(error) = self
            .settings
            .set_string("task-hidden-groups", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden task groups");
        }
    }

    pub fn task_groups_order(&self) -> Vec<String> {
        self.settings
            .string("task-groups-order")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_task_groups_order(&self, groups: &[String]) {
        let groups = sanitize_ordered_list(groups);
        if let Err(error) = self
            .settings
            .set_string("task-groups-order", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist task groups order");
        }
    }

    pub fn memo_hidden_groups(&self) -> Vec<String> {
        self.settings
            .string("memo-hidden-groups")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_memo_hidden_groups(&self, groups: &[String]) {
        let groups = sanitize_source_list(groups);
        if let Err(error) = self
            .settings
            .set_string("memo-hidden-groups", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden memo groups");
        }
    }

    pub fn memo_groups_order(&self) -> Vec<String> {
        self.settings
            .string("memo-groups-order")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_memo_groups_order(&self, groups: &[String]) {
        let groups = sanitize_ordered_list(groups);
        if let Err(error) = self
            .settings
            .set_string("memo-groups-order", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist memo groups order");
        }
    }

    pub fn addressbook_hidden_groups(&self) -> Vec<String> {
        self.settings
            .string("addressbook-hidden-groups")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_addressbook_hidden_groups(&self, groups: &[String]) {
        let groups = sanitize_source_list(groups);
        if let Err(error) = self
            .settings
            .set_string("addressbook-hidden-groups", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist hidden address book groups");
        }
    }

    pub fn addressbook_groups_order(&self) -> Vec<String> {
        self.settings
            .string("addressbook-groups-order")
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_addressbook_groups_order(&self, groups: &[String]) {
        let groups = sanitize_ordered_list(groups);
        if let Err(error) = self
            .settings
            .set_string("addressbook-groups-order", &groups.join(","))
        {
            tracing::warn!(%error, "Could not persist address book groups order");
        }
    }

    pub fn prune_deleted_source(&self, deleted_uid: &str) {
        let uid = deleted_uid.trim();
        if uid.is_empty() {
            return;
        }
        if self.default_calendar_source_uid() == uid {
            self.set_default_calendar_source_uid("");
        }
        if self.default_task_source_uid() == uid {
            self.set_default_task_source_uid("");
        }
        if self.default_memo_source_uid() == uid {
            self.set_default_memo_source_uid("");
        }
        if self.default_address_book_source_uid() == uid {
            self.set_default_address_book_source_uid("");
        }
        if self.task_source_filter() == uid {
            self.set_task_source_filter("");
        }
        if self.memo_source_filter() == uid {
            self.set_memo_source_filter("");
        }

        let prune = |list: Vec<String>| -> Vec<String> {
            list.into_iter().filter(|item| item != uid).collect()
        };

        self.set_calendar_hidden_sources(&prune(self.calendar_hidden_sources()));
        self.set_task_hidden_sources(&prune(self.task_hidden_sources()));
        self.set_memo_hidden_sources(&prune(self.memo_hidden_sources()));
        self.set_addressbook_hidden_sources(&prune(self.addressbook_hidden_sources()));

        self.set_calendar_hidden_groups(&prune(self.calendar_hidden_groups()));
        self.set_calendar_groups_order(&prune(self.calendar_groups_order()));
        self.set_task_hidden_groups(&prune(self.task_hidden_groups()));
        self.set_task_groups_order(&prune(self.task_groups_order()));
        self.set_memo_hidden_groups(&prune(self.memo_hidden_groups()));
        self.set_memo_groups_order(&prune(self.memo_groups_order()));
        self.set_addressbook_hidden_groups(&prune(self.addressbook_hidden_groups()));
        self.set_addressbook_groups_order(&prune(self.addressbook_groups_order()));
    }

    pub fn calendar_time_divisions(&self) -> i32 {
        sanitize_time_divisions(self.settings.int("calendar-time-divisions"))
    }

    pub fn set_calendar_time_divisions(&self, value: i32) {
        self.set_int("calendar-time-divisions", sanitize_time_divisions(value));
    }

    pub fn calendar_compress_weekend(&self) -> bool {
        self.settings.boolean("calendar-compress-weekend")
    }

    pub fn set_calendar_compress_weekend(&self, value: bool) {
        self.set_boolean("calendar-compress-weekend", value);
    }

    pub fn calendar_month_scroll_by_week(&self) -> bool {
        self.settings.boolean("calendar-month-scroll-by-week")
    }

    pub fn set_calendar_month_scroll_by_week(&self, value: bool) {
        self.set_boolean("calendar-month-scroll-by-week", value);
    }

    pub fn calendar_second_zone(&self) -> String {
        self.settings.string("calendar-second-zone").to_string()
    }

    pub fn set_calendar_second_zone(&self, value: &str) {
        let trimmed = value.trim();
        if let Err(error) = self.settings.set_string("calendar-second-zone", trimmed) {
            tracing::warn!(%error, "Could not persist calendar second zone");
        }
        if !trimmed.is_empty() && trimmed != "system" {
            self.add_calendar_second_zone_to_recent(trimmed);
        }
    }

    pub fn calendar_second_zones(&self) -> Vec<String> {
        self.settings
            .string("calendar-second-zones")
            .split(',')
            .map(str::trim)
            .filter(|z| !z.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn set_calendar_second_zones(&self, zones: &[String]) {
        let mut clean = Vec::new();
        for zone in zones {
            let trimmed = zone.trim();
            if !trimmed.is_empty() && !clean.iter().any(|z: &String| z == trimmed) {
                clean.push(trimmed.to_string());
            }
            if clean.len() >= 5 {
                break;
            }
        }
        if let Err(error) = self
            .settings
            .set_string("calendar-second-zones", &clean.join(","))
        {
            tracing::warn!(%error, "Could not persist calendar second zones");
        }
    }

    pub fn add_calendar_second_zone_to_recent(&self, location: &str) {
        let location = location.trim();
        if location.is_empty() {
            return;
        }
        let mut list = self.calendar_second_zones();
        list.retain(|z| z != location);
        list.insert(0, location.to_string());
        list.truncate(5);
        self.set_calendar_second_zones(&list);
    }

    pub fn system_locale_date_format() -> &'static str {
        if let Ok(link) = std::fs::read_link("/etc/localtime") {
            let path = link.to_string_lossy();
            if path.contains("Berlin") || path.contains("Busingen") || path.contains("Germany") {
                return "german";
            }
        }
        let tz = std::env::var("TZ").unwrap_or_default();
        if tz.contains("Berlin") || tz.contains("Germany") {
            return "german";
        }
        let locale = std::env::var("LC_TIME")
            .or_else(|_| std::env::var("LC_ALL"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let lower = locale.to_lowercase();
        if lower.starts_with("de") {
            return "german";
        }
        if lower.starts_with("en_us") || lower.starts_with("en_ph") {
            "us"
        } else if lower.starts_with("zh_cn")
            || lower.starts_with("ja_jp")
            || lower.starts_with("ko_kr")
            || lower.starts_with("sv_se")
        {
            "iso"
        } else {
            "german"
        }
    }

    pub fn system_prefers_12h() -> bool {
        if gio::SettingsSchemaSource::default()
            .and_then(|s| s.lookup("org.gnome.desktop.interface", true))
            .is_some()
        {
            let desktop = gio::Settings::new("org.gnome.desktop.interface");
            if desktop.string("clock-format").as_str() == "12h" {
                return true;
            }
        }
        let locale = std::env::var("LC_TIME")
            .or_else(|_| std::env::var("LC_ALL"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let lower = locale.to_lowercase();
        lower.starts_with("en_us") || lower.starts_with("en_ca") || lower.starts_with("en_ph")
    }

    pub fn date_format(&self) -> String {
        let value = self.settings.string("date-format").to_string();
        match value.as_str() {
            "iso" | "us" | "german" => value,
            _ => "locale".to_string(),
        }
    }

    pub fn resolved_date_format(&self) -> String {
        let fmt = self.date_format();
        if fmt == "locale" {
            Self::system_locale_date_format().to_string()
        } else {
            fmt
        }
    }

    pub fn set_date_format(&self, value: &str) {
        let value = match value {
            "iso" | "us" | "german" => value,
            _ => "locale",
        };
        if let Err(error) = self.settings.set_string("date-format", value) {
            tracing::warn!(%error, "Could not persist date format");
        }
    }

    pub fn time_format(&self) -> String {
        let val = self.settings.string("time-format");
        match val.as_str() {
            "12h" => "12h".to_string(),
            "24h" => "24h".to_string(),
            _ => "locale".to_string(),
        }
    }

    pub fn resolved_time_format(&self) -> String {
        match self.time_format().as_str() {
            "12h" => "12h".to_string(),
            "24h" => "24h".to_string(),
            _ => {
                if Self::system_prefers_12h() {
                    "12h".to_string()
                } else {
                    "24h".to_string()
                }
            }
        }
    }

    pub fn set_time_format(&self, value: &str) {
        let value = match value {
            "12h" => "12h",
            "24h" => "24h",
            _ => "locale",
        };
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

    pub fn composer_show_from(&self) -> bool {
        self.settings.boolean("composer-show-from")
    }

    pub fn set_composer_show_from(&self, value: bool) {
        self.set_boolean("composer-show-from", value);
    }

    pub fn composer_show_cc(&self) -> bool {
        self.settings.boolean("composer-show-cc")
    }

    pub fn set_composer_show_cc(&self, value: bool) {
        self.set_boolean("composer-show-cc", value);
    }

    pub fn composer_show_bcc(&self) -> bool {
        self.settings.boolean("composer-show-bcc")
    }

    pub fn set_composer_show_bcc(&self, value: bool) {
        self.set_boolean("composer-show-bcc", value);
    }

    pub fn composer_show_reply_to(&self) -> bool {
        self.settings.boolean("composer-show-reply-to")
    }

    pub fn set_composer_show_reply_to(&self, value: bool) {
        self.set_boolean("composer-show-reply-to", value);
    }

    pub fn composer_prompt_empty_subject(&self) -> bool {
        self.settings.boolean("composer-prompt-empty-subject")
    }

    pub fn set_composer_prompt_empty_subject(&self, value: bool) {
        self.set_boolean("composer-prompt-empty-subject", value);
    }

    pub fn composer_prompt_only_bcc(&self) -> bool {
        self.settings.boolean("composer-prompt-only-bcc")
    }

    pub fn set_composer_prompt_only_bcc(&self, value: bool) {
        self.set_boolean("composer-prompt-only-bcc", value);
    }

    pub fn composer_inline_spelling(&self) -> bool {
        self.settings.boolean("composer-inline-spelling")
    }

    pub fn set_composer_inline_spelling(&self, value: bool) {
        self.set_boolean("composer-inline-spelling", value);
    }

    pub fn composer_reply_style(&self) -> String {
        normalize_composer_reply_style(&self.settings.string("composer-reply-style"))
            .unwrap_or("quoted")
            .to_string()
    }

    pub fn set_composer_reply_style(&self, value: &str) {
        let value = normalize_composer_reply_style(value).unwrap_or("quoted");
        self.set_string("composer-reply-style", value);
    }

    pub fn composer_forward_style(&self) -> String {
        normalize_composer_forward_style(&self.settings.string("composer-forward-style"))
            .unwrap_or("attached")
            .to_string()
    }

    pub fn set_composer_forward_style(&self, value: &str) {
        let value = normalize_composer_forward_style(value).unwrap_or("attached");
        self.set_string("composer-forward-style", value);
    }

    pub fn composer_top_signature(&self) -> bool {
        self.settings.boolean("composer-top-signature")
    }

    pub fn set_composer_top_signature(&self, value: bool) {
        self.set_boolean("composer-top-signature", value);
    }

    pub fn composer_signature_outline(&self) -> bool {
        self.settings.boolean("composer-signature-outline")
    }

    pub fn set_composer_signature_outline(&self, value: bool) {
        self.set_boolean("composer-signature-outline", value);
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

    pub fn mail_image_loading_policy(&self) -> String {
        normalize_image_loading_policy(&self.settings.string("mail-image-loading-policy"))
            .unwrap_or("never")
            .to_string()
    }

    pub fn set_mail_image_loading_policy(&self, value: &str) {
        let value = normalize_image_loading_policy(value).unwrap_or("never");
        self.set_string("mail-image-loading-policy", value);
        self.set_boolean("auto-load-remote-images", value == "always");
    }

    pub fn mail_mark_seen(&self) -> bool {
        self.settings.boolean("mail-mark-seen")
    }

    pub fn set_mail_mark_seen(&self, value: bool) {
        self.set_boolean("mail-mark-seen", value);
        self.set_boolean("mark-read-on-selection", value);
    }

    pub fn mail_mark_seen_timeout(&self) -> u32 {
        self.settings.int("mail-mark-seen-timeout").clamp(0, 60_000) as u32
    }

    pub fn set_mail_mark_seen_timeout(&self, value: u32) {
        let clamped = value.clamp(0, 60_000) as i32;
        self.set_int("mail-mark-seen-timeout", clamped);
        self.set_int("mark-read-timeout", clamped.clamp(0, 30_000));
    }

    pub fn mail_caret_mode(&self) -> bool {
        self.settings.boolean("mail-caret-mode")
    }

    pub fn set_mail_caret_mode(&self, value: bool) {
        self.set_boolean("mail-caret-mode", value);
    }

    pub fn mail_magic_spacebar(&self) -> bool {
        self.settings.boolean("mail-magic-spacebar")
    }

    pub fn set_mail_magic_spacebar(&self, value: bool) {
        self.set_boolean("mail-magic-spacebar", value);
    }

    pub fn mail_citation_highlighting(&self) -> bool {
        self.settings.boolean("mail-citation-highlighting")
    }

    pub fn set_mail_citation_highlighting(&self, value: bool) {
        self.set_boolean("mail-citation-highlighting", value);
    }

    pub fn mail_citation_color_1(&self) -> String {
        let val = self.settings.string("mail-citation-color-1");
        normalize_hex_color(&val)
    }

    pub fn set_mail_citation_color_1(&self, value: &str) {
        let value = normalize_hex_color(value);
        self.set_string("mail-citation-color-1", &value);
    }

    pub fn mail_citation_color_2(&self) -> String {
        let val = self.settings.string("mail-citation-color-2");
        normalize_hex_color(&val)
    }

    pub fn set_mail_citation_color_2(&self, value: &str) {
        let value = normalize_hex_color(value);
        self.set_string("mail-citation-color-2", &value);
    }

    pub fn mail_citation_color_3(&self) -> String {
        let val = self.settings.string("mail-citation-color-3");
        normalize_hex_color(&val)
    }

    pub fn set_mail_citation_color_3(&self, value: &str) {
        let value = normalize_hex_color(value);
        self.set_string("mail-citation-color-3", &value);
    }

    pub fn mail_citation_color_4(&self) -> String {
        let val = self.settings.string("mail-citation-color-4");
        normalize_hex_color(&val)
    }

    pub fn set_mail_citation_color_4(&self, value: &str) {
        let value = normalize_hex_color(value);
        self.set_string("mail-citation-color-4", &value);
    }

    pub fn mail_citation_color_5(&self) -> String {
        let val = self.settings.string("mail-citation-color-5");
        normalize_hex_color(&val)
    }

    pub fn set_mail_citation_color_5(&self, value: &str) {
        let value = normalize_hex_color(value);
        self.set_string("mail-citation-color-5", &value);
    }

    pub fn mail_citation_colors(&self) -> [String; 5] {
        [
            self.mail_citation_color_1(),
            self.mail_citation_color_2(),
            self.mail_citation_color_3(),
            self.mail_citation_color_4(),
            self.mail_citation_color_5(),
        ]
    }

    pub fn mail_headers_collapsed(&self) -> bool {
        self.settings.boolean("mail-headers-collapsed")
    }

    pub fn set_mail_headers_collapsed(&self, value: bool) {
        self.set_boolean("mail-headers-collapsed", value);
    }

    pub fn mail_show_real_date(&self) -> bool {
        self.settings.boolean("mail-show-real-date")
    }

    pub fn set_mail_show_real_date(&self, value: bool) {
        self.set_boolean("mail-show-real-date", value);
    }

    pub fn mark_read_on_selection(&self) -> bool {
        self.settings.boolean("mark-read-on-selection")
    }

    pub fn set_mark_read_on_selection(&self, value: bool) {
        self.set_boolean("mark-read-on-selection", value);
        self.set_boolean("mail-mark-seen", value);
    }

    pub fn mark_read_timeout(&self) -> i32 {
        self.settings.int("mark-read-timeout")
    }

    pub fn set_mark_read_timeout(&self, value: i32) {
        let clamped = value.clamp(0, 30_000);
        self.set_int("mark-read-timeout", clamped);
        self.set_int("mail-mark-seen-timeout", clamped);
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

    pub fn debug_console_enabled(&self) -> bool {
        self.settings.boolean("debug-console-enabled")
    }

    pub fn set_debug_console_enabled(&self, value: bool) {
        self.set_boolean("debug-console-enabled", value);
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

    /// Select the mail workflow layout.
    /// 'vertical' (or legacy 'modern') keeps the message list and preview side by side (3-panel).
    /// 'horizontal' (or legacy 'classic') mirrors Evolution's horizontal arrangement (preview bottom).
    pub fn mail_layout(&self) -> String {
        match self.settings.string("mail-layout").as_str() {
            "horizontal" | "classic" => "horizontal".to_string(),
            _ => "vertical".to_string(),
        }
    }

    pub fn set_mail_layout(&self, value: &str) {
        let value = match value {
            "horizontal" | "classic" => "horizontal",
            _ => "vertical",
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

    pub fn mail_show_mark_read_actions(&self) -> bool {
        self.settings.boolean("mail-show-mark-read-actions")
    }

    pub fn set_mail_show_mark_read_actions(&self, value: bool) {
        self.set_boolean("mail-show-mark-read-actions", value);
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

    pub fn mail_show_todo_bar(&self) -> bool {
        self.settings.boolean("mail-show-todo-bar")
    }

    pub fn set_mail_show_todo_bar(&self, value: bool) {
        self.set_boolean("mail-show-todo-bar", value);
    }

    pub fn agenda_show_tasks(&self) -> bool {
        self.settings.boolean("agenda-show-tasks")
    }

    pub fn set_agenda_show_tasks(&self, value: bool) {
        self.set_boolean("agenda-show-tasks", value);
    }

    pub fn mail_todo_bar_position(&self) -> i32 {
        self.settings.int("mail-todo-bar-position").clamp(160, 1_200)
    }

    pub fn set_mail_todo_bar_position(&self, value: i32) {
        self.set_int("mail-todo-bar-position", value.clamp(160, 1_200));
    }

    pub fn mail_preview_position(&self, layout: &str) -> i32 {
        let key = if layout == "classic" || layout == "horizontal" {
            "mail-classic-preview-position"
        } else {
            "mail-modern-preview-position"
        };
        self.settings.int(key).clamp(120, 4_096)
    }

    pub fn set_mail_preview_position(&self, layout: &str, value: i32) {
        let key = if layout == "classic" || layout == "horizontal" {
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

    pub fn connect_changed<F>(&self, key: Option<&str>, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&gio::Settings, &str) + 'static,
    {
        self.settings.connect_changed(key, callback)
    }

    pub fn disconnect(&self, id: glib::SignalHandlerId) {
        use glib::object::ObjectExt;
        self.settings.disconnect(id);
    }

    pub fn color_scheme(&self) -> String {
        self.settings.string("color-scheme").to_string()
    }

    pub fn adw_color_scheme(&self) -> adw::ColorScheme {
        Self::parse_adw_color_scheme(&self.color_scheme())
    }

    pub fn parse_adw_color_scheme(scheme: &str) -> adw::ColorScheme {
        match scheme {
            "light" => adw::ColorScheme::ForceLight,
            "dark" => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        }
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

    pub fn mail_dark_mode(&self) -> bool {
        self.settings.boolean("mail-dark-mode")
    }

    pub fn set_mail_dark_mode(&self, value: bool) {
        self.set_boolean("mail-dark-mode", value);
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

    pub fn mail_expanded_folders(&self) -> Vec<String> {
        self.settings
            .strv("mail-expanded-folders")
            .into_iter()
            .map(|s| {
                s.replace("__NUL__", "\0")
                    .replace("::", "\0")
                    .to_string()
            })
            .collect()
    }

    pub fn set_mail_expanded_folders(&self, folders: &[String]) {
        let sanitized: Vec<String> = folders
            .iter()
            .map(|s| s.replace('\0', "__NUL__"))
            .collect();
        let refs: Vec<&str> = sanitized.iter().map(String::as_str).collect();
        if let Err(error) = self.settings.set_strv("mail-expanded-folders", &*refs) {
            tracing::warn!(%error, "Could not persist mail-expanded-folders");
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

    pub fn mail_recipient_column_width(&self) -> i32 {
        let val = self.settings.int("mail-recipient-column-width");
        if val <= 0 { 180 } else { val.clamp(80, 360) }
    }

    pub fn set_mail_recipient_column_width(&self, value: i32) {
        self.set_int("mail-recipient-column-width", value.clamp(80, 360));
    }

    pub fn mail_date_column_width(&self) -> i32 {
        let val = self.settings.int("mail-date-column-width");
        if val <= 0 { 130 } else { val.clamp(60, 300) }
    }

    pub fn set_mail_date_column_width(&self, value: i32) {
        self.set_int("mail-date-column-width", value.clamp(60, 300));
    }

    pub fn mail_date_format(&self) -> String {
        let val = self.settings.string("mail-date-format").to_string();
        match val.as_str() {
            "smart" | "short" | "full" | "datetime" | "iso" | "custom" => val,
            _ => "standard".to_string(),
        }
    }

    pub fn set_mail_date_format(&self, value: &str) {
        let val = match value {
            "smart" | "short" | "full" | "datetime" | "iso" | "custom" => value,
            _ => "standard",
        };
        self.set_string("mail-date-format", val);
    }

    pub fn mail_date_custom_format(&self) -> String {
        self.settings.string("mail-date-custom-format").to_string()
    }

    pub fn set_mail_date_custom_format(&self, value: &str) {
        self.set_string("mail-date-custom-format", value);
    }

    pub fn mail_date_align(&self) -> String {
        let val = self.settings.string("mail-date-align").to_string();
        if val == "left" { "left".to_string() } else { "right".to_string() }
    }

    pub fn set_mail_date_align(&self, value: &str) {
        let val = if value == "left" { "left" } else { "right" };
        self.set_string("mail-date-align", val);
    }

    pub fn mail_list_view_profile(&self) -> String {
        normalized_mail_list_view_profile(&self.settings.string("mail-list-view-profile"))
            .to_string()
    }

    pub fn set_mail_list_view_profile(&self, profile: &str) {
        let profile = normalized_mail_list_view_profile(profile);
        self.set_string("mail-list-view-profile", profile);
    }

    pub fn mail_header_style(&self) -> String {
        let has_key = self
            .settings
            .settings_schema()
            .map(|s| s.has_key("mail-header-style"))
            .unwrap_or(false);
        let raw = if has_key {
            self.settings.string("mail-header-style").to_string()
        } else {
            "modern".to_string()
        };
        normalized_mail_header_style(&raw).to_string()
    }

    pub fn set_mail_header_style(&self, style: &str) {
        let style = normalized_mail_header_style(style);
        let has_key = self
            .settings
            .settings_schema()
            .map(|s| s.has_key("mail-header-style"))
            .unwrap_or(false);
        if has_key {
            self.set_string("mail-header-style", style);
        }
    }

    pub fn mail_list_column_order(&self) -> Vec<String> {
        normalize_mail_list_column_order(&self.settings.string("mail-list-column-order"))
    }

    pub fn set_mail_list_column_order(&self, order: &[String]) {
        let normalized = normalize_mail_list_column_order(&order.join(","));
        self.set_string("mail-list-column-order", &normalized.join(","));
        self.set_string("mail-list-view-profile", "custom");
    }

    pub fn apply_mail_list_view_profile(&self, profile: &str) {
        let profile = normalized_mail_list_view_profile(profile);
        let order = mail_list_profile_column_order(profile);
        self.set_string("mail-list-column-order", &order.join(","));
        self.set_string("mail-list-view-profile", profile);
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

    pub fn memo_show_toolbar(&self) -> bool {
        self.settings.boolean("memo-show-toolbar")
    }

    pub fn set_memo_show_toolbar(&self, value: bool) {
        self.set_boolean("memo-show-toolbar", value);
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

    pub fn memo_preview_position(&self, orientation: &str) -> i32 {
        let key = if orientation == "vertical" {
            "memo-hpane-position"
        } else {
            "memo-vpane-position"
        };
        self.settings.int(key).clamp(80, 4_096)
    }

    pub fn set_memo_preview_position(&self, orientation: &str, value: i32) {
        let key = if orientation == "vertical" {
            "memo-hpane-position"
        } else {
            "memo-vpane-position"
        };
        self.set_int(key, value.clamp(80, 4_096));
    }

    pub fn task_preview_visible(&self) -> bool {
        self.settings.boolean("task-preview-visible")
    }

    pub fn set_task_preview_visible(&self, value: bool) {
        self.set_boolean("task-preview-visible", value);
    }

    pub fn task_show_toolbar(&self) -> bool {
        self.settings.boolean("task-show-toolbar")
    }

    pub fn set_task_show_toolbar(&self, value: bool) {
        self.set_boolean("task-show-toolbar", value);
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

    pub fn contact_completion_minimum_query_length(&self) -> u32 {
        self.settings
            .int("contact-completion-minimum-query-length")
            .clamp(1, 100) as u32
    }

    pub fn set_contact_completion_minimum_query_length(&self, value: u32) {
        self.set_int(
            "contact-completion-minimum-query-length",
            value.clamp(1, 100) as i32,
        );
    }

    pub fn contact_completion_show_address(&self) -> bool {
        self.settings.boolean("contact-completion-show-address")
    }

    pub fn set_contact_completion_show_address(&self, value: bool) {
        self.set_boolean("contact-completion-show-address", value);
    }

    pub fn contact_completion_book_sources(&self) -> String {
        self.settings
            .string("contact-completion-book-sources")
            .to_string()
    }

    pub fn set_contact_completion_book_sources(&self, value: &str) {
        let uids: Vec<String> = value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let cleaned = sanitize_source_list(&uids);
        self.set_string("contact-completion-book-sources", &cleaned.join(","));
    }

    pub fn contact_address_formatting(&self) -> bool {
        self.settings.boolean("contact-address-formatting")
    }

    pub fn set_contact_address_formatting(&self, value: bool) {
        self.set_boolean("contact-address-formatting", value);
    }

    pub fn contact_preview_show_maps(&self) -> bool {
        self.settings.boolean("contact-preview-show-maps")
    }

    pub fn set_contact_preview_show_maps(&self, value: bool) {
        self.set_boolean("contact-preview-show-maps", value);
    }

    pub fn contact_preview_home_before_work(&self) -> bool {
        self.settings.boolean("contact-preview-home-before-work")
    }

    pub fn set_contact_preview_home_before_work(&self, value: bool) {
        self.set_boolean("contact-preview-home-before-work", value);
    }

    pub fn contact_open_map_target(&self) -> String {
        let value = self.settings.string("contact-open-map-target");
        if value.eq_ignore_ascii_case("google") {
            "google".to_string()
        } else {
            "openstreetmap".to_string()
        }
    }

    pub fn set_contact_open_map_target(&self, value: &str) {
        let normalized = if value.trim().eq_ignore_ascii_case("google") {
            "google"
        } else {
            "openstreetmap"
        };
        self.set_string("contact-open-map-target", normalized);
    }

    pub fn contact_open_map_prefer_local(&self) -> bool {
        self.settings.boolean("contact-open-map-prefer-local")
    }

    pub fn set_contact_open_map_prefer_local(&self, value: bool) {
        self.set_boolean("contact-open-map-prefer-local", value);
    }

    pub fn contact_layout(&self) -> String {
        let value = self.settings.string("contact-layout");
        if value.eq_ignore_ascii_case("vertical") {
            "vertical".to_string()
        } else {
            "classic".to_string()
        }
    }

    pub fn set_contact_layout(&self, value: &str) {
        let normalized = if value.trim().eq_ignore_ascii_case("vertical") {
            "vertical"
        } else {
            "classic"
        };
        self.set_string("contact-layout", normalized);
    }

    pub fn contact_preview_visible(&self) -> bool {
        self.settings.boolean("contact-preview-visible")
    }

    pub fn set_contact_preview_visible(&self, value: bool) {
        self.set_boolean("contact-preview-visible", value);
    }

    pub fn contact_show_toolbar(&self) -> bool {
        self.settings.boolean("contact-show-toolbar")
    }

    pub fn set_contact_show_toolbar(&self, value: bool) {
        self.set_boolean("contact-show-toolbar", value);
    }

    pub fn contact_hpane_position(&self) -> i32 {
        self.settings.int("contact-hpane-position").clamp(80, 4_096)
    }

    pub fn set_contact_hpane_position(&self, value: i32) {
        self.set_int("contact-hpane-position", value.clamp(80, 4_096));
    }

    pub fn contact_vpane_position(&self) -> i32 {
        self.settings.int("contact-vpane-position").clamp(80, 4_096)
    }

    pub fn set_contact_vpane_position(&self, value: i32) {
        self.set_int("contact-vpane-position", value.clamp(80, 4_096));
    }

    pub fn contact_editor_show_phone(&self) -> bool {
        self.settings.boolean("contact-editor-show-phone")
    }

    pub fn set_contact_editor_show_phone(&self, value: bool) {
        self.set_boolean("contact-editor-show-phone", value);
    }

    pub fn contact_editor_show_sip(&self) -> bool {
        self.settings.boolean("contact-editor-show-sip")
    }

    pub fn set_contact_editor_show_sip(&self, value: bool) {
        self.set_boolean("contact-editor-show-sip", value);
    }

    pub fn contact_editor_show_im(&self) -> bool {
        self.settings.boolean("contact-editor-show-im")
    }

    pub fn set_contact_editor_show_im(&self, value: bool) {
        self.set_boolean("contact-editor-show-im", value);
    }

    pub fn contact_editor_show_home_address(&self) -> bool {
        self.settings.boolean("contact-editor-show-home-address")
    }

    pub fn set_contact_editor_show_home_address(&self, value: bool) {
        self.set_boolean("contact-editor-show-home-address", value);
    }

    pub fn contact_editor_show_work_address(&self) -> bool {
        self.settings.boolean("contact-editor-show-work-address")
    }

    pub fn set_contact_editor_show_work_address(&self, value: bool) {
        self.set_boolean("contact-editor-show-work-address", value);
    }

    pub fn contact_editor_show_other_address(&self) -> bool {
        self.settings.boolean("contact-editor-show-other-address")
    }

    pub fn set_contact_editor_show_other_address(&self, value: bool) {
        self.set_boolean("contact-editor-show-other-address", value);
    }

    pub fn contact_editor_show_web(&self) -> bool {
        self.settings.boolean("contact-editor-show-web")
    }

    pub fn set_contact_editor_show_web(&self, value: bool) {
        self.set_boolean("contact-editor-show-web", value);
    }

    pub fn contact_editor_show_job(&self) -> bool {
        self.settings.boolean("contact-editor-show-job")
    }

    pub fn set_contact_editor_show_job(&self, value: bool) {
        self.set_boolean("contact-editor-show-job", value);
    }

    pub fn contact_editor_show_dates(&self) -> bool {
        self.settings.boolean("contact-editor-show-dates")
    }

    pub fn set_contact_editor_show_dates(&self, value: bool) {
        self.set_boolean("contact-editor-show-dates", value);
    }

    pub fn contact_editor_show_misc(&self) -> bool {
        self.settings.boolean("contact-editor-show-misc")
    }

    pub fn set_contact_editor_show_misc(&self, value: bool) {
        self.set_boolean("contact-editor-show-misc", value);
    }

    pub fn contact_editor_show_notes(&self) -> bool {
        self.settings.boolean("contact-editor-show-notes")
    }

    pub fn set_contact_editor_show_notes(&self, value: bool) {
        self.set_boolean("contact-editor-show-notes", value);
    }

    pub fn contact_editor_show_certs(&self) -> bool {
        self.settings.boolean("contact-editor-show-certs")
    }

    pub fn set_contact_editor_show_certs(&self, value: bool) {
        self.set_boolean("contact-editor-show-certs", value);
    }

    pub fn filters_log_actions(&self) -> bool {
        self.settings.boolean("filters-log-actions")
    }

    pub fn set_filters_log_actions(&self, value: bool) {
        self.set_boolean("filters-log-actions", value);
    }

    pub fn filters_log_file(&self) -> String {
        self.settings.string("filters-log-file").to_string()
    }

    pub fn set_filters_log_file(&self, value: &str) {
        if let Err(error) = self.settings.set_string("filters-log-file", value.trim()) {
            tracing::warn!(%error, "Could not persist filters log file");
        }
    }

    pub fn flush_outbox(&self) -> bool {
        self.settings.boolean("flush-outbox")
    }

    pub fn set_flush_outbox(&self, value: bool) {
        self.set_boolean("flush-outbox", value);
    }

    pub fn mail_labels(&self) -> Vec<MailLabelDefinition> {
        let strv = self.settings.strv("mail-labels");
        let mut labels = Vec::new();
        for entry in strv {
            if let Some(def) = MailLabelDefinition::parse(entry.as_str()) {
                labels.push(def);
            }
        }
        if labels.is_empty() {
            MailLabelDefinition::default_labels()
        } else {
            labels
        }
    }

    pub fn set_mail_labels(&self, labels: &[MailLabelDefinition]) -> bool {
        let raw: Vec<String> = labels.iter().map(|l| l.to_setting_string()).collect();
        let slice_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        if let Err(error) = self.settings.set_strv("mail-labels", &*slice_refs) {
            tracing::warn!(%error, "Could not persist mail labels");
            false
        } else {
            true
        }
    }

    pub fn junk_check_incoming(&self) -> bool {
        self.settings.boolean("junk-check-incoming")
    }

    pub fn set_junk_check_incoming(&self, value: bool) {
        self.set_boolean("junk-check-incoming", value);
    }

    pub fn junk_empty_on_exit(&self) -> bool {
        self.settings.boolean("junk-empty-on-exit")
    }

    pub fn set_junk_empty_on_exit(&self, value: bool) {
        self.set_boolean("junk-empty-on-exit", value);
    }

    pub fn junk_empty_on_exit_days(&self) -> i32 {
        self.settings.int("junk-empty-on-exit-days")
    }

    pub fn set_junk_empty_on_exit_days(&self, value: i32) {
        self.set_int("junk-empty-on-exit-days", value);
    }

    pub fn junk_lookup_addressbook(&self) -> bool {
        self.settings.boolean("junk-lookup-addressbook")
    }

    pub fn set_junk_lookup_addressbook(&self, value: bool) {
        self.set_boolean("junk-lookup-addressbook", value);
    }

    pub fn junk_lookup_addressbook_local_only(&self) -> bool {
        self.settings.boolean("junk-lookup-addressbook-local-only")
    }

    pub fn set_junk_lookup_addressbook_local_only(&self, value: bool) {
        self.set_boolean("junk-lookup-addressbook-local-only", value);
    }

    pub fn junk_default_plugin(&self) -> String {
        self.settings.string("junk-default-plugin").to_string()
    }

    pub fn set_junk_default_plugin(&self, value: &str) {
        self.set_string("junk-default-plugin", value);
    }

    pub fn junk_custom_header(&self) -> String {
        self.settings.string("junk-custom-header").to_string()
    }

    pub fn set_junk_custom_header(&self, value: &str) {
        self.set_string("junk-custom-header", value);
    }

    pub fn junk_custom_header_value(&self) -> String {
        self.settings.string("junk-custom-header-value").to_string()
    }

    pub fn set_junk_custom_header_value(&self, value: &str) {
        self.set_string("junk-custom-header-value", value);
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
            "use-default-reminder" => {
                self.set_use_default_reminder(parse_migrated_bool(key, value)?);
            }
            "default-reminder-interval" => {
                self.set_default_reminder_interval(parse_migrated_u32_in_range(
                    key, value, 0, 100_000,
                )?);
            }
            "default-reminder-units" => {
                self.set_default_reminder_units(ReminderDurationUnit::from_str(value));
            }
            "use-ba-reminder" => {
                self.set_use_ba_reminder(parse_migrated_bool(key, value)?);
            }
            "ba-reminder-interval" => {
                self.set_ba_reminder_interval(parse_migrated_u32_in_range(key, value, 0, 100_000)?);
            }
            "ba-reminder-units" => {
                self.set_ba_reminder_units(ReminderDurationUnit::from_str(value));
            }
            "hide-completed-tasks" => {
                self.set_hide_completed_tasks(parse_migrated_bool(key, value)?);
            }
            "hide-completed-tasks-value" => {
                self.set_hide_completed_tasks_value(parse_migrated_u32_in_range(
                    key, value, 0, 100_000,
                )?);
            }
            "hide-completed-tasks-units" => {
                self.set_hide_completed_tasks_units(ReminderDurationUnit::from_str(value));
            }
            "hide-cancelled-tasks" => {
                self.set_hide_cancelled_tasks(parse_migrated_bool(key, value)?);
            }
            "hide-cancelled-events" => {
                self.set_hide_cancelled_events(parse_migrated_bool(key, value)?);
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
            "calendar-show-tag-vpane" => {
                self.set_calendar_show_tag_vpane(parse_migrated_bool(key, value)?);
            }
            "calendar-tag-vpane-position" => {
                self.set_calendar_tag_vpane_position(parse_migrated_i32_in_range(
                    key, value, 100, 2_000,
                )?);
            }
            "calendar-time-divisions" => {
                let div = parse_migrated_i32(key, value)?;
                if !matches!(div, 5 | 10 | 15 | 30 | 60) {
                    return Err(format!("ungültige Zeiteinteilung für „{key}“"));
                }
                self.set_calendar_time_divisions(div);
            }
            "calendar-compress-weekend" => {
                self.set_calendar_compress_weekend(parse_migrated_bool(key, value)?);
            }
            "calendar-month-scroll-by-week" => {
                self.set_calendar_month_scroll_by_week(parse_migrated_bool(key, value)?);
            }
            "calendar-second-zone" => {
                self.set_calendar_second_zone(value);
            }
            "calendar-second-zones" => {
                let zones = value
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|z| !z.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                self.set_calendar_second_zones(&zones);
            }
            "calendar-hidden-sources"
            | "task-hidden-sources"
            | "memo-hidden-sources"
            | "addressbook-hidden-sources"
            | "calendar-hidden-groups"
            | "calendar-groups-order"
            | "task-hidden-groups"
            | "task-groups-order"
            | "memo-hidden-groups"
            | "memo-groups-order"
            | "addressbook-hidden-groups"
            | "addressbook-groups-order" => {
                let items = value
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                if items
                    .iter()
                    .any(|item| item.is_empty() || item.chars().any(char::is_control))
                {
                    return Err(format!("ungültige Quellenliste für „{key}“"));
                }
                match key {
                    "calendar-hidden-sources" => self.set_calendar_hidden_sources(&items),
                    "task-hidden-sources" => self.set_task_hidden_sources(&items),
                    "memo-hidden-sources" => self.set_memo_hidden_sources(&items),
                    "addressbook-hidden-sources" => self.set_addressbook_hidden_sources(&items),
                    "calendar-hidden-groups" => self.set_calendar_hidden_groups(&items),
                    "calendar-groups-order" => self.set_calendar_groups_order(&items),
                    "task-hidden-groups" => self.set_task_hidden_groups(&items),
                    "task-groups-order" => self.set_task_groups_order(&items),
                    "memo-hidden-groups" => self.set_memo_hidden_groups(&items),
                    "memo-groups-order" => self.set_memo_groups_order(&items),
                    "addressbook-hidden-groups" => self.set_addressbook_hidden_groups(&items),
                    "addressbook-groups-order" => self.set_addressbook_groups_order(&items),
                    _ => unreachable!(),
                }
            }
            "date-format" => self.set_date_format(value),
            "time-format" => self.set_time_format(value),
            "calendar-time-zone" => self.set_calendar_time_zone(value),
            "auto-load-remote-images" => {
                self.set_auto_load_remote_images(parse_migrated_bool(key, value)?);
            }
            "offline-mode" => self.set_offline_mode(parse_migrated_bool(key, value)?),
            "force-dark-mode" => self.set_force_dark_mode(parse_migrated_bool(key, value)?),
            "mail-dark-mode" => self.set_mail_dark_mode(parse_migrated_bool(key, value)?),
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
            "mail-show-mark-read-actions" => {
                self.set_mail_show_mark_read_actions(parse_migrated_bool(key, value)?);
            }
            "mail-show-sidebar" => self.set_mail_show_sidebar(parse_migrated_bool(key, value)?),
            "mail-show-todo-bar" => {
                self.set_mail_show_todo_bar(parse_migrated_bool(key, value)?);
            }
            "mail-todo-bar-position" => {
                self.set_mail_todo_bar_position(parse_migrated_i32_in_range(key, value, 160, 1_200)?);
            }
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
            "mail-recipient-column-width" => {
                let value = parse_migrated_i32_in_range(key, value, 80, 360)?;
                self.set_mail_recipient_column_width(value);
            }
            "mail-date-column-width" => {
                let value = parse_migrated_i32_in_range(key, value, 60, 300)?;
                self.set_mail_date_column_width(value);
            }
            "mail-date-format" => self.set_mail_date_format(value),
            "mail-date-custom-format" => self.set_mail_date_custom_format(value),
            "mail-date-align" => self.set_mail_date_align(value),
            "mail-list-view-profile" => {
                if !matches!(value, "messages" | "wide" | "custom") {
                    return Err(format!("ungültiges Mail-Ansichtsprofil für „{key}“"));
                }
                if value == "custom" {
                    self.set_string("mail-list-view-profile", value);
                } else {
                    self.apply_mail_list_view_profile(value);
                }
            }
            "mail-header-style" => {
                let style = normalized_mail_header_style(value);
                self.set_mail_header_style(style);
            }
            "mail-list-column-order" => {
                let order = normalize_mail_list_column_order(value);
                self.set_mail_list_column_order(&order);
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
            "mail-expanded-folders" => {
                let parsed: Result<Vec<String>, _> = serde_json::from_str(value);
                if let Ok(folders) = parsed {
                    self.set_mail_expanded_folders(&folders);
                }
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
            "task-show-toolbar" => {
                self.set_task_show_toolbar(parse_migrated_bool(key, value)?);
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
            "memo-show-toolbar" => {
                self.set_memo_show_toolbar(parse_migrated_bool(key, value)?);
            }
            "memo-preview-orientation" => {
                let value = normalize_memo_preview_orientation_value(value)?;
                self.set_string("memo-preview-orientation", &value);
            }
            "memo-hpane-position" => self.set_memo_preview_position(
                "vertical",
                parse_migrated_i32_in_range(key, value, 80, 4_096)?,
            ),
            "memo-vpane-position" => self.set_memo_preview_position(
                "classic",
                parse_migrated_i32_in_range(key, value, 80, 4_096)?,
            ),
            "contact-completion-minimum-query-length" => {
                let val = parse_migrated_u32_in_range(key, value, 1, 100)?;
                self.set_contact_completion_minimum_query_length(val);
            }
            "contact-completion-show-address" => {
                self.set_contact_completion_show_address(parse_migrated_bool(key, value)?);
            }
            "contact-completion-book-sources" => {
                self.set_contact_completion_book_sources(value);
            }
            "contact-address-formatting" => {
                self.set_contact_address_formatting(parse_migrated_bool(key, value)?);
            }
            "contact-preview-show-maps" => {
                self.set_contact_preview_show_maps(parse_migrated_bool(key, value)?);
            }
            "contact-preview-home-before-work" => {
                self.set_contact_preview_home_before_work(parse_migrated_bool(key, value)?);
            }
            "contact-open-map-target" => {
                self.set_contact_open_map_target(value);
            }
            "contact-open-map-prefer-local" => {
                self.set_contact_open_map_prefer_local(parse_migrated_bool(key, value)?);
            }
            "contact-layout" => {
                self.set_contact_layout(value);
            }
            "contact-preview-visible" => {
                self.set_contact_preview_visible(parse_migrated_bool(key, value)?);
            }
            "contact-show-toolbar" => {
                self.set_contact_show_toolbar(parse_migrated_bool(key, value)?);
            }
            "contact-hpane-position" => {
                let val = parse_migrated_i32_in_range(key, value, 80, 4_096)?;
                self.set_contact_hpane_position(val);
            }
            "contact-vpane-position" => {
                let val = parse_migrated_i32_in_range(key, value, 80, 4_096)?;
                self.set_contact_vpane_position(val);
            }
            "contact-editor-show-phone" => {
                self.set_contact_editor_show_phone(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-sip" => {
                self.set_contact_editor_show_sip(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-im" => {
                self.set_contact_editor_show_im(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-home-address" => {
                self.set_contact_editor_show_home_address(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-work-address" => {
                self.set_contact_editor_show_work_address(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-other-address" => {
                self.set_contact_editor_show_other_address(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-web" => {
                self.set_contact_editor_show_web(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-job" => {
                self.set_contact_editor_show_job(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-dates" => {
                self.set_contact_editor_show_dates(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-misc" => {
                self.set_contact_editor_show_misc(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-notes" => {
                self.set_contact_editor_show_notes(parse_migrated_bool(key, value)?);
            }
            "contact-editor-show-certs" => {
                self.set_contact_editor_show_certs(parse_migrated_bool(key, value)?);
            }
            "filters-log-actions" => {
                self.set_filters_log_actions(parse_migrated_bool(key, value)?);
            }
            "filters-log-file" => {
                self.set_filters_log_file(value);
            }
            "flush-outbox" => {
                self.set_flush_outbox(parse_migrated_bool(key, value)?);
            }
            "mail-labels" | "labels" => {
                let mut labels = Vec::new();
                for entry in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let clean_entry = entry.trim_matches(|c| c == '[' || c == ']' || c == '\'' || c == '"');
                    if let Some(label) = MailLabelDefinition::parse(clean_entry) {
                        labels.push(label);
                    }
                }
                if labels.is_empty() {
                    return Err("ungültige mail-labels-Liste".to_string());
                }
                self.set_mail_labels(&labels);
            }
            "junk-check-incoming" => {
                self.set_junk_check_incoming(parse_migrated_bool(key, value)?);
            }
            "junk-empty-on-exit" => {
                self.set_junk_empty_on_exit(parse_migrated_bool(key, value)?);
            }
            "junk-empty-on-exit-days" => {
                self.set_junk_empty_on_exit_days(parse_migrated_i32_in_range(key, value, 0, 365)?);
            }
            "junk-lookup-addressbook" => {
                self.set_junk_lookup_addressbook(parse_migrated_bool(key, value)?);
            }
            "junk-lookup-addressbook-local-only" => {
                self.set_junk_lookup_addressbook_local_only(parse_migrated_bool(key, value)?);
            }
            "junk-default-plugin" => {
                if !matches!(value, "Bogofilter" | "SpamAssassin") {
                    return Err(format!("ungültiges Junk-Plugin für „{key}“"));
                }
                self.set_junk_default_plugin(value);
            }
            "junk-custom-header" => {
                self.set_junk_custom_header(value);
            }
            "junk-custom-header-value" => {
                self.set_junk_custom_header_value(value);
            }
            "composer-show-from" => {
                self.set_composer_show_from(parse_migrated_bool(key, value)?);
            }
            "composer-show-cc" => {
                self.set_composer_show_cc(parse_migrated_bool(key, value)?);
            }
            "composer-show-bcc" => {
                self.set_composer_show_bcc(parse_migrated_bool(key, value)?);
            }
            "composer-show-reply-to" => {
                self.set_composer_show_reply_to(parse_migrated_bool(key, value)?);
            }
            "composer-prompt-empty-subject" => {
                self.set_composer_prompt_empty_subject(parse_migrated_bool(key, value)?);
            }
            "composer-prompt-only-bcc" => {
                self.set_composer_prompt_only_bcc(parse_migrated_bool(key, value)?);
            }
            "composer-inline-spelling" => {
                self.set_composer_inline_spelling(parse_migrated_bool(key, value)?);
            }
            "composer-reply-style" => {
                let value = normalize_composer_reply_style(value)
                    .ok_or_else(|| format!("ungültiger Antwort-Stil für „{key}“"))?;
                self.set_composer_reply_style(value);
            }
            "composer-forward-style" => {
                let value = normalize_composer_forward_style(value)
                    .ok_or_else(|| format!("ungültiger Weiterleitungs-Stil für „{key}“"))?;
                self.set_composer_forward_style(value);
            }
            "composer-top-signature" => {
                self.set_composer_top_signature(parse_migrated_bool(key, value)?);
            }
            "composer-signature-outline" => {
                self.set_composer_signature_outline(parse_migrated_bool(key, value)?);
            }
            "mail-image-loading-policy" => {
                let value = normalize_image_loading_policy(value)
                    .ok_or_else(|| format!("ungültige Bildladerichtlinie für „{key}“"))?;
                self.set_mail_image_loading_policy(value);
            }
            "mail-mark-seen" => {
                self.set_mail_mark_seen(parse_migrated_bool(key, value)?);
            }
            "mail-mark-seen-timeout" => {
                self.set_mail_mark_seen_timeout(
                    parse_migrated_i32(key, value)?.clamp(0, 60_000) as u32,
                );
            }
            "mail-caret-mode" => {
                self.set_mail_caret_mode(parse_migrated_bool(key, value)?);
            }
            "mail-magic-spacebar" => {
                self.set_mail_magic_spacebar(parse_migrated_bool(key, value)?);
            }
            "mail-citation-highlighting" => {
                self.set_mail_citation_highlighting(parse_migrated_bool(key, value)?);
            }
            "mail-citation-color-1" => {
                self.set_mail_citation_color_1(&normalize_hex_color(value));
            }
            "mail-citation-color-2" => {
                self.set_mail_citation_color_2(&normalize_hex_color(value));
            }
            "mail-citation-color-3" => {
                self.set_mail_citation_color_3(&normalize_hex_color(value));
            }
            "mail-citation-color-4" => {
                self.set_mail_citation_color_4(&normalize_hex_color(value));
            }
            "mail-citation-color-5" => {
                self.set_mail_citation_color_5(&normalize_hex_color(value));
            }
            "mail-headers-collapsed" => {
                self.set_mail_headers_collapsed(parse_migrated_bool(key, value)?);
            }
            "mail-show-real-date" => {
                self.set_mail_show_real_date(parse_migrated_bool(key, value)?);
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
            "use-default-reminder",
            "default-reminder-interval",
            "default-reminder-units",
            "use-ba-reminder",
            "ba-reminder-interval",
            "ba-reminder-units",
            "hide-completed-tasks",
            "hide-completed-tasks-units",
            "hide-completed-tasks-value",
            "hide-cancelled-tasks",
            "hide-cancelled-events",
            "notifications-enabled",
            "confirm-destructive-actions",
            "debug-console-enabled",
            "calendar-show-week-numbers",
            "calendar-time-divisions",
            "calendar-compress-weekend",
            "calendar-month-scroll-by-week",
            "calendar-second-zone",
            "calendar-second-zones",
            "calendar-hidden-sources",
            "calendar-tag-vpane-position",
            "calendar-show-tag-vpane",
            "task-hidden-sources",
            "memo-hidden-sources",
            "addressbook-hidden-sources",
            "calendar-hidden-groups",
            "calendar-groups-order",
            "task-hidden-groups",
            "task-groups-order",
            "memo-hidden-groups",
            "memo-groups-order",
            "addressbook-hidden-groups",
            "addressbook-groups-order",
            "date-format",
            "time-format",
            "calendar-time-zone",
            "auto-load-remote-images",
            "offline-mode",
            "force-dark-mode",
            "mail-dark-mode",
            "render-html",
            "composer-html",
            "composer-charset",
            "request-read-receipt",
            "spellcheck-languages",
            "mail-font-family",
            "mail-font-size",
            "mark-read-on-selection",
            "mark-read-timeout",
            "show-message-preview",
            "mail-show-security-details",
            "mail-show-all-headers",
            "mail-show-mark-read-actions",
            "mail-normal-headers",
            "mail-show-statusbar",
            "mail-default-folder",
            "mail-layout",
            "mail-show-toolbar",
            "mail-show-sidebar",
            "mail-show-todo-bar",
            "mail-todo-bar-position",
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
            "mail-labels",
            "mail-show-size",
            "mail-show-follow-up-due",
            "mail-show-location",
            "mail-sender-column-width",
            "mail-recipient-column-width",
            "mail-date-column-width",
            "mail-date-format",
            "mail-date-custom-format",
            "mail-date-align",
            "mail-list-view-profile",
            "mail-header-style",
            "mail-list-column-order",
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
            "mail-expanded-folders",
            "mail-show-deleted",
            "task-filter",
            "task-category-filter",
            "task-search-mode",
            "task-source-filter",
            "task-sort-field",
            "task-sort-descending",
            "task-preview-visible",
            "task-show-toolbar",
            "task-preview-orientation",
            "task-hpane-position",
            "task-vpane-position",
            "memo-sort-field",
            "memo-source-filter",
            "memo-sort-descending",
            "memo-search-mode",
            "memo-category-filter",
            "memo-preview-visible",
            "memo-show-toolbar",
            "memo-preview-orientation",
            "memo-hpane-position",
            "memo-vpane-position",
            "contact-completion-minimum-query-length",
            "contact-completion-show-address",
            "contact-completion-book-sources",
            "contact-address-formatting",
            "contact-preview-show-maps",
            "contact-preview-home-before-work",
            "contact-open-map-target",
            "contact-open-map-prefer-local",
            "contact-layout",
            "contact-preview-visible",
            "contact-show-toolbar",
            "contact-hpane-position",
            "contact-vpane-position",
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
            "filters-log-actions",
            "filters-log-file",
            "flush-outbox",
            "junk-check-incoming",
            "junk-empty-on-exit",
            "junk-empty-on-exit-days",
            "junk-lookup-addressbook",
            "junk-lookup-addressbook-local-only",
            "junk-default-plugin",
            "junk-custom-header",
            "junk-custom-header-value",
            "composer-show-from",
            "composer-show-cc",
            "composer-show-bcc",
            "composer-show-reply-to",
            "composer-prompt-empty-subject",
            "composer-prompt-only-bcc",
            "composer-inline-spelling",
            "composer-reply-style",
            "composer-forward-style",
            "composer-top-signature",
            "composer-signature-outline",
            "mail-image-loading-policy",
            "mail-mark-seen",
            "mail-mark-seen-timeout",
            "mail-caret-mode",
            "mail-magic-spacebar",
            "mail-citation-highlighting",
            "mail-citation-color-1",
            "mail-citation-color-2",
            "mail-citation-color-3",
            "mail-citation-color-4",
            "mail-citation-color-5",
            "mail-headers-collapsed",
            "mail-show-real-date",
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
                .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
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

pub fn sanitize_time_divisions(value: i32) -> i32 {
    match value {
        5 | 10 | 15 | 30 | 60 => value,
        _ => 30,
    }
}

pub fn sanitize_source_list(uids: &[String]) -> Vec<String> {
    let mut cleaned = uids
        .iter()
        .map(|uid| uid.trim())
        .filter(|uid| !uid.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    cleaned.sort_unstable();
    cleaned.dedup();
    cleaned
}

pub fn sanitize_ordered_list(items: &[String]) -> Vec<String> {
    let mut cleaned = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if seen.insert(item.to_string()) {
            cleaned.push(item.to_string());
        }
    }
    cleaned
}

pub fn build_contact_map_uri(
    address: &str,
    target: &str,
    prefer_local: bool,
) -> (String, Option<String>) {
    let encoded: String = address
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                '+'
            }
        })
        .collect();
    let query = encoded.replace(' ', "+");
    let web_url = if target.trim().eq_ignore_ascii_case("google") {
        format!("https://www.google.com/maps/search/?api=1&query={query}")
    } else {
        format!("https://www.openstreetmap.org/search?query={query}")
    };

    if prefer_local {
        (format!("geo:0,0?q={query}"), Some(web_url))
    } else {
        (web_url, None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReminderDurationUnit {
    #[default]
    Minutes,
    Hours,
    Days,
}

impl ReminderDurationUnit {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minutes => "minutes",
            Self::Hours => "hours",
            Self::Days => "days",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "hours" | "hour" | "1" => Self::Hours,
            "days" | "day" | "2" => Self::Days,
            _ => Self::Minutes,
        }
    }

    pub fn to_seconds(&self, value: u32) -> u64 {
        match self {
            Self::Minutes => (value as u64) * 60,
            Self::Hours => (value as u64) * 3600,
            Self::Days => (value as u64) * 86400,
        }
    }

    pub fn to_minutes(&self, value: u32) -> u32 {
        match self {
            Self::Minutes => value,
            Self::Hours => value.saturating_mul(60),
            Self::Days => value.saturating_mul(1440),
        }
    }
}

#[allow(dead_code)]
pub fn sanitize_duration_units(units: &str) -> &'static str {
    ReminderDurationUnit::from_str(units).as_str()
}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, DEFAULT_MAIL_NORMAL_HEADERS, MAX_MAIL_NORMAL_HEADERS, MailLabelDefinition,
        build_contact_map_uri, clamp_mail_sync_message_limit, default_mail_normal_headers,
        mail_list_profile_column_order, normalize_composer_charset,
        normalize_composer_forward_style, normalize_composer_reply_style,
        normalize_image_loading_policy, normalize_mail_list_column_order,
        normalized_mail_list_view_profile, normalized_mail_sort_field, normalized_switcher_location,
        sanitize_calendar_workdays, sanitize_spellcheck_languages,
        validate_mail_normal_header_name,
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
    fn mail_list_profiles_and_corrupt_orders_normalize_without_losing_columns() {
        let messages = mail_list_profile_column_order("messages");
        assert_eq!(
            &messages[..7],
            &[
                "status",
                "attachment",
                "flagged",
                "sender",
                "recipient",
                "subject",
                "date"
            ]
        );
        let wide = mail_list_profile_column_order("wide");
        assert_eq!(&wide[..3], &["status", "flagged", "messages"]);
        assert_eq!(normalized_mail_list_view_profile("unknown"), "messages");

        let normalized = normalize_mail_list_column_order("date,subject,date,unknown,status");
        assert_eq!(&normalized[..3], &["date", "subject", "status"]);
        assert_eq!(normalized.len(), super::MAIL_LIST_COLUMN_IDS.len());
        for column in super::MAIL_LIST_COLUMN_IDS {
            assert_eq!(
                normalized.iter().filter(|value| *value == column).count(),
                1
            );
        }
    }

    #[test]
    fn mail_list_profile_order_and_both_widths_round_trip_and_migrate() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        settings.apply_mail_list_view_profile("wide");
        settings.set_mail_sender_column_width(245);
        settings.set_mail_recipient_column_width(210);
        settings.set_mail_date_column_width(175);
        assert_eq!(settings.mail_list_view_profile(), "wide");
        assert_eq!(
            settings.mail_list_column_order(),
            mail_list_profile_column_order("wide")
        );
        assert_eq!(settings.mail_sender_column_width(), 245);
        assert_eq!(settings.mail_recipient_column_width(), 210);
        assert_eq!(settings.mail_date_column_width(), 175);

        settings
            .apply_migrated_setting("mail-list-column-order", "subject,date,status")
            .unwrap();
        assert_eq!(settings.mail_list_view_profile(), "custom");
        assert_eq!(
            &settings.mail_list_column_order()[..3],
            &["subject", "date", "status"]
        );
        settings
            .apply_migrated_setting("mail-recipient-column-width", "205")
            .unwrap();
        assert_eq!(settings.mail_recipient_column_width(), 205);
        settings
            .apply_migrated_setting("mail-date-column-width", "190")
            .unwrap();
        assert_eq!(settings.mail_date_column_width(), 190);
        assert!(
            settings
                .apply_migrated_setting("mail-list-view-profile", "invalid")
                .is_err()
        );
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
    fn mail_header_style_round_trips_and_normalizes() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        assert_eq!(settings.mail_header_style(), "modern");
        settings.set_mail_header_style("classic");
        assert_eq!(settings_with_backend(&backend).mail_header_style(), "classic");
        settings.set_mail_header_style("invalid");
        assert_eq!(settings_with_backend(&backend).mail_header_style(), "modern");
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

    #[test]
    fn calendar_view_and_timezone_settings_round_trip_migrate_and_reset_strictly() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // Initial defaults
        assert_eq!(settings.calendar_view(), "month");
        assert_eq!(settings.calendar_time_divisions(), 30);
        assert!(settings.calendar_compress_weekend());
        assert!(settings.calendar_month_scroll_by_week());
        assert_eq!(settings.calendar_second_zone(), "");
        assert!(settings.calendar_second_zones().is_empty());

        // View selection including year and list
        settings.set_calendar_view("year");
        assert_eq!(settings.calendar_view(), "year");
        settings.set_calendar_view("list");
        assert_eq!(settings.calendar_view(), "list");
        settings.set_calendar_view("invalid-view");
        assert_eq!(settings.calendar_view(), "month");

        // Time divisions: valid and sanitization
        for valid in [5, 10, 15, 30, 60] {
            settings.set_calendar_time_divisions(valid);
            assert_eq!(settings.calendar_time_divisions(), valid);
        }
        settings.set_calendar_time_divisions(45);
        assert_eq!(settings.calendar_time_divisions(), 30);

        // Booleans
        settings.set_calendar_compress_weekend(false);
        assert!(!settings.calendar_compress_weekend());
        settings.set_calendar_month_scroll_by_week(false);
        assert!(!settings.calendar_month_scroll_by_week());

        // Second zone and recent zones tracking
        settings.set_calendar_second_zone("UTC");
        assert_eq!(settings.calendar_second_zone(), "UTC");
        assert_eq!(settings.calendar_second_zones(), vec!["UTC".to_string()]);

        settings.set_calendar_second_zone("Europe/Berlin");
        assert_eq!(settings.calendar_second_zone(), "Europe/Berlin");
        assert_eq!(
            settings.calendar_second_zones(),
            vec!["Europe/Berlin".to_string(), "UTC".to_string()]
        );

        // More zones up to max 5
        for tz in [
            "America/New_York",
            "Asia/Tokyo",
            "Australia/Sydney",
            "Pacific/Auckland",
        ] {
            settings.add_calendar_second_zone_to_recent(tz);
        }
        let recent = settings.calendar_second_zones();
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0], "Pacific/Auckland");

        // Migration testing
        settings
            .apply_migrated_setting("calendar-time-divisions", "15")
            .expect("15 min divisions should migrate");
        assert_eq!(settings.calendar_time_divisions(), 15);

        assert!(
            settings
                .apply_migrated_setting("calendar-time-divisions", "25")
                .is_err(),
            "25 min should be rejected"
        );

        settings
            .apply_migrated_setting("calendar-compress-weekend", "true")
            .expect("compress-weekend should migrate");
        assert!(settings.calendar_compress_weekend());

        settings
            .apply_migrated_setting("calendar-month-scroll-by-week", "true")
            .expect("month-scroll-by-week should migrate");
        assert!(settings.calendar_month_scroll_by_week());

        settings
            .apply_migrated_setting("calendar-second-zone", "America/Chicago")
            .expect("second-zone should migrate");
        assert_eq!(settings.calendar_second_zone(), "America/Chicago");

        // Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.calendar_view(), "month");
        assert_eq!(reset.calendar_time_divisions(), 30);
        assert!(reset.calendar_compress_weekend());
        assert!(reset.calendar_month_scroll_by_week());
        assert_eq!(reset.calendar_second_zone(), "");
        assert!(reset.calendar_second_zones().is_empty());
    }

    #[test]
    fn source_and_group_settings_and_pruning_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Initial defaults
        assert!(settings.task_hidden_sources().is_empty());
        assert!(settings.memo_hidden_sources().is_empty());
        assert!(settings.addressbook_hidden_sources().is_empty());
        assert!(settings.calendar_hidden_groups().is_empty());
        assert!(settings.calendar_groups_order().is_empty());
        assert!(settings.task_hidden_groups().is_empty());
        assert!(settings.task_groups_order().is_empty());
        assert!(settings.memo_hidden_groups().is_empty());
        assert!(settings.memo_groups_order().is_empty());
        assert!(settings.addressbook_hidden_groups().is_empty());
        assert!(settings.addressbook_groups_order().is_empty());

        // 2. Setting and sanitization (sorting/dedup for hidden, order preservation for groups_order)
        settings.set_task_hidden_sources(&[
            "task-src-b".to_string(),
            "task-src-a".to_string(),
            "task-src-b".to_string(),
            "".to_string(),
        ]);
        assert_eq!(
            settings.task_hidden_sources(),
            vec!["task-src-a", "task-src-b"]
        );

        settings.set_memo_hidden_sources(&["memo-x".to_string(), "memo-y".to_string()]);
        assert_eq!(settings.memo_hidden_sources(), vec!["memo-x", "memo-y"]);

        settings.set_addressbook_hidden_sources(&["book-1".to_string()]);
        assert_eq!(settings.addressbook_hidden_sources(), vec!["book-1"]);

        settings.set_calendar_groups_order(&[
            "Group Z".to_string(),
            "Group A".to_string(),
            "Group Z".to_string(),
        ]);
        // groups order preserves first appearance order
        assert_eq!(settings.calendar_groups_order(), vec!["Group Z", "Group A"]);

        // 3. Set default sources
        settings.set_default_calendar_source_uid("cal-to-delete");
        settings.set_default_task_source_uid("task-to-delete");
        settings.set_default_memo_source_uid("memo-to-delete");
        settings.set_default_address_book_source_uid("book-to-delete");
        settings
            .set_calendar_hidden_sources(&["cal-to-delete".to_string(), "cal-keep".to_string()]);
        settings.set_task_hidden_sources(&["task-to-delete".to_string(), "task-keep".to_string()]);

        // 4. Prune deleted source
        settings.prune_deleted_source("cal-to-delete");
        assert_eq!(settings.default_calendar_source_uid(), "");
        assert_eq!(settings.calendar_hidden_sources(), vec!["cal-keep"]);

        settings.prune_deleted_source("task-to-delete");
        assert_eq!(settings.default_task_source_uid(), "");
        assert_eq!(settings.task_hidden_sources(), vec!["task-keep"]);

        // 5. Import setting for groups and hidden sources
        settings
            .apply_migrated_setting("task-hidden-groups", "Personal, Work")
            .expect("should import task-hidden-groups");
        assert_eq!(settings.task_hidden_groups(), vec!["Personal", "Work"]);

        settings
            .apply_migrated_setting("task-groups-order", "Work, Personal")
            .expect("should import task-groups-order");
        assert_eq!(settings.task_groups_order(), vec!["Work", "Personal"]);

        // 6. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert!(reset.calendar_hidden_sources().is_empty());
        assert!(reset.task_hidden_sources().is_empty());
        assert!(reset.memo_hidden_sources().is_empty());
        assert!(reset.addressbook_hidden_sources().is_empty());
        assert!(reset.calendar_hidden_groups().is_empty());
        assert!(reset.calendar_groups_order().is_empty());
        assert!(reset.task_hidden_groups().is_empty());
        assert!(reset.task_groups_order().is_empty());
    }

    #[test]
    fn reminder_and_task_filter_settings_roundtrip_and_cutoff() {
        use super::ReminderDurationUnit;

        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Check initial defaults
        assert!(!settings.use_default_reminder());
        assert_eq!(settings.default_reminder_interval(), 15);
        assert_eq!(
            settings.default_reminder_units(),
            ReminderDurationUnit::Minutes
        );
        assert_eq!(settings.default_reminder_total_minutes(), 15);
        assert!(!settings.use_ba_reminder());
        assert_eq!(settings.ba_reminder_interval(), 15);
        assert_eq!(settings.ba_reminder_units(), ReminderDurationUnit::Minutes);
        assert!(!settings.hide_completed_tasks());
        assert_eq!(settings.hide_completed_tasks_value(), 1);
        assert_eq!(
            settings.hide_completed_tasks_units(),
            ReminderDurationUnit::Days
        );
        assert!(!settings.hide_cancelled_tasks());
        assert!(!settings.hide_cancelled_events());

        // 2. Cutoff calculation when disabled
        assert_eq!(
            settings.hide_completed_tasks_cutoff_timestamp(1_700_000_000),
            None
        );

        // 3. Mutate typed settings
        settings.set_use_default_reminder(true);
        assert!(settings.use_default_reminder());

        settings.set_default_reminder_interval(2);
        settings.set_default_reminder_units(ReminderDurationUnit::Hours);
        assert_eq!(settings.default_reminder_interval(), 2);
        assert_eq!(
            settings.default_reminder_units(),
            ReminderDurationUnit::Hours
        );
        assert_eq!(settings.default_reminder_total_minutes(), 120);
        assert_eq!(settings.calendar_default_reminder_minutes(), 120);

        settings.set_use_ba_reminder(true);
        settings.set_ba_reminder_interval(3);
        settings.set_ba_reminder_units(ReminderDurationUnit::Days);
        assert!(settings.use_ba_reminder());
        assert_eq!(settings.ba_reminder_interval(), 3);
        assert_eq!(settings.ba_reminder_units(), ReminderDurationUnit::Days);

        settings.set_hide_completed_tasks(true);
        settings.set_hide_completed_tasks_value(2);
        settings.set_hide_completed_tasks_units(ReminderDurationUnit::Days);
        assert!(settings.hide_completed_tasks());
        assert_eq!(settings.hide_completed_tasks_value(), 2);
        assert_eq!(
            settings.hide_completed_tasks_units(),
            ReminderDurationUnit::Days
        );

        // Cutoff calculation with 2 days
        let now = 1_700_000_000;
        let expected_cutoff = now - 2 * 86_400;
        assert_eq!(
            settings.hide_completed_tasks_cutoff_timestamp(now),
            Some(expected_cutoff)
        );

        // Cutoff calculation with 0 (immediate)
        settings.set_hide_completed_tasks_value(0);
        assert_eq!(
            settings.hide_completed_tasks_cutoff_timestamp(now),
            Some(now)
        );

        settings.set_hide_cancelled_tasks(true);
        assert!(settings.hide_cancelled_tasks());
        settings.set_hide_cancelled_events(true);
        assert!(settings.hide_cancelled_events());

        // 4. Test apply_migrated_setting
        settings
            .apply_migrated_setting("use-default-reminder", "false")
            .unwrap();
        assert!(!settings.use_default_reminder());

        settings
            .apply_migrated_setting("default-reminder-interval", "45")
            .unwrap();
        assert_eq!(settings.default_reminder_interval(), 45);

        settings
            .apply_migrated_setting("default-reminder-units", "minutes")
            .unwrap();
        assert_eq!(
            settings.default_reminder_units(),
            ReminderDurationUnit::Minutes
        );

        settings
            .apply_migrated_setting("hide-completed-tasks", "false")
            .unwrap();
        assert!(!settings.hide_completed_tasks());

        settings
            .apply_migrated_setting("hide-completed-tasks-value", "10")
            .unwrap();
        assert_eq!(settings.hide_completed_tasks_value(), 10);

        settings
            .apply_migrated_setting("hide-completed-tasks-units", "hours")
            .unwrap();
        assert_eq!(
            settings.hide_completed_tasks_units(),
            ReminderDurationUnit::Hours
        );

        settings
            .apply_migrated_setting("hide-cancelled-tasks", "false")
            .unwrap();
        assert!(!settings.hide_cancelled_tasks());

        settings
            .apply_migrated_setting("hide-cancelled-events", "false")
            .unwrap();
        assert!(!settings.hide_cancelled_events());

        // 5. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert!(!reset.use_default_reminder());
        assert_eq!(reset.default_reminder_interval(), 15);
        assert_eq!(
            reset.default_reminder_units(),
            ReminderDurationUnit::Minutes
        );
        assert!(!reset.use_ba_reminder());
        assert_eq!(reset.ba_reminder_interval(), 15);
        assert_eq!(reset.ba_reminder_units(), ReminderDurationUnit::Minutes);
        assert!(!reset.hide_completed_tasks());
        assert_eq!(reset.hide_completed_tasks_value(), 1);
        assert_eq!(
            reset.hide_completed_tasks_units(),
            ReminderDurationUnit::Days
        );
        assert!(!reset.hide_cancelled_tasks());
        assert!(!reset.hide_cancelled_events());
    }

    #[test]
    fn contact_settings_roundtrip_defaults_and_map_uri() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Defaults
        assert_eq!(settings.contact_completion_minimum_query_length(), 3);
        assert!(!settings.contact_completion_show_address());
        assert!(settings.contact_completion_book_sources().is_empty());
        assert!(!settings.contact_address_formatting());
        assert!(!settings.contact_preview_show_maps());
        assert!(!settings.contact_preview_home_before_work());
        assert_eq!(settings.contact_open_map_target(), "openstreetmap");
        assert!(settings.contact_open_map_prefer_local());
        assert_eq!(settings.contact_layout(), "classic");
        assert!(settings.contact_preview_visible());
        assert_eq!(settings.contact_hpane_position(), 200);
        assert_eq!(settings.contact_vpane_position(), 400);
        assert!(settings.contact_editor_show_phone());
        assert!(settings.contact_editor_show_sip());
        assert!(settings.contact_editor_show_im());
        assert!(settings.contact_editor_show_home_address());
        assert!(settings.contact_editor_show_work_address());
        assert!(settings.contact_editor_show_other_address());
        assert!(settings.contact_editor_show_web());
        assert!(settings.contact_editor_show_job());
        assert!(settings.contact_editor_show_dates());
        assert!(settings.contact_editor_show_misc());
        assert!(settings.contact_editor_show_notes());
        assert!(settings.contact_editor_show_certs());

        // 2. Setters & clamping / normalization
        settings.set_contact_completion_minimum_query_length(5);
        assert_eq!(settings.contact_completion_minimum_query_length(), 5);
        settings.set_contact_completion_minimum_query_length(0);
        assert_eq!(settings.contact_completion_minimum_query_length(), 1);

        settings.set_contact_completion_show_address(true);
        assert!(settings.contact_completion_show_address());

        settings.set_contact_completion_book_sources(" book-b , book-a , book-b ");
        assert_eq!(settings.contact_completion_book_sources(), "book-a,book-b");

        settings.set_contact_address_formatting(true);
        assert!(settings.contact_address_formatting());

        settings.set_contact_preview_show_maps(true);
        assert!(settings.contact_preview_show_maps());

        settings.set_contact_preview_home_before_work(true);
        assert!(settings.contact_preview_home_before_work());

        settings.set_contact_open_map_target("google");
        assert_eq!(settings.contact_open_map_target(), "google");
        settings.set_contact_open_map_target("unknown");
        assert_eq!(settings.contact_open_map_target(), "openstreetmap");

        settings.set_contact_open_map_prefer_local(false);
        assert!(!settings.contact_open_map_prefer_local());

        settings.set_contact_layout("vertical");
        assert_eq!(settings.contact_layout(), "vertical");
        settings.set_contact_layout("other");
        assert_eq!(settings.contact_layout(), "classic");

        settings.set_contact_preview_visible(false);
        assert!(!settings.contact_preview_visible());

        settings.set_contact_hpane_position(350);
        assert_eq!(settings.contact_hpane_position(), 350);
        settings.set_contact_hpane_position(50);
        assert_eq!(settings.contact_hpane_position(), 80);

        settings.set_contact_vpane_position(550);
        assert_eq!(settings.contact_vpane_position(), 550);

        settings.set_contact_editor_show_phone(false);
        assert!(!settings.contact_editor_show_phone());

        // 3. Map URI generation
        let (primary, fallback) =
            build_contact_map_uri("Alexanderplatz 1, Berlin", "openstreetmap", true);
        assert_eq!(primary, "geo:0,0?q=Alexanderplatz+1++Berlin");
        assert_eq!(
            fallback,
            Some("https://www.openstreetmap.org/search?query=Alexanderplatz+1++Berlin".to_string())
        );

        let (primary_google, fallback_google) =
            build_contact_map_uri("1600 Amphitheatre Pkwy", "google", false);
        assert_eq!(
            primary_google,
            "https://www.google.com/maps/search/?api=1&query=1600+Amphitheatre+Pkwy"
        );
        assert_eq!(fallback_google, None);

        // 4. Migration import
        settings
            .apply_migrated_setting("contact-layout", "vertical")
            .unwrap();
        assert_eq!(settings.contact_layout(), "vertical");
        settings
            .apply_migrated_setting("contact-open-map-target", "google")
            .unwrap();
        assert_eq!(settings.contact_open_map_target(), "google");

        // 5. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.contact_completion_minimum_query_length(), 3);
        assert!(!reset.contact_completion_show_address());
        assert_eq!(reset.contact_open_map_target(), "openstreetmap");
        assert!(reset.contact_open_map_prefer_local());
        assert_eq!(reset.contact_layout(), "classic");
        assert!(reset.contact_preview_visible());
        assert!(reset.contact_editor_show_phone());
    }

    #[test]
    fn filter_logging_and_flush_outbox_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Defaults
        assert!(!settings.filters_log_actions());
        assert_eq!(settings.filters_log_file(), "");
        assert!(settings.flush_outbox());

        // 2. Setters
        settings.set_filters_log_actions(true);
        assert!(settings.filters_log_actions());

        settings.set_filters_log_file(" /tmp/filter.log ");
        assert_eq!(settings.filters_log_file(), "/tmp/filter.log");

        settings.set_flush_outbox(false);
        assert!(!settings.flush_outbox());

        // 3. Migration import
        settings
            .apply_migrated_setting("filters-log-actions", "false")
            .unwrap();
        assert!(!settings.filters_log_actions());

        settings
            .apply_migrated_setting("filters-log-file", "/var/log/evolution-filters.log")
            .unwrap();
        assert_eq!(settings.filters_log_file(), "/var/log/evolution-filters.log");

        settings
            .apply_migrated_setting("flush-outbox", "true")
            .unwrap();
        assert!(settings.flush_outbox());

        // 4. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert!(!reset.filters_log_actions());
        assert_eq!(reset.filters_log_file(), "");
        assert!(reset.flush_outbox());
    }

    #[test]
    fn mail_labels_and_junk_settings_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Defaults
        let default_labels = settings.mail_labels();
        assert_eq!(default_labels.len(), 5);
        assert_eq!(default_labels[0].name, "Important");
        assert_eq!(default_labels[0].color, "#EF2929");
        assert_eq!(default_labels[0].tag, "$Labelimportant");

        assert!(settings.junk_check_incoming());
        assert!(!settings.junk_empty_on_exit());
        assert_eq!(settings.junk_empty_on_exit_days(), 0);
        assert!(settings.junk_lookup_addressbook());
        assert!(!settings.junk_lookup_addressbook_local_only());
        assert_eq!(settings.junk_default_plugin(), "Bogofilter");
        assert_eq!(settings.junk_custom_header(), "X-Spam-Flag");
        assert_eq!(settings.junk_custom_header_value(), "YES");

        // 2. Setters & Label definition manipulation
        let custom_labels = vec![
            MailLabelDefinition::new("Urgent", "#FF0000", None),
            MailLabelDefinition::new("Client A", "#00FF00", Some("client-a".to_string())),
        ];
        assert!(settings.set_mail_labels(&custom_labels));
        let loaded = settings.mail_labels();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Urgent");
        assert_eq!(loaded[0].color, "#FF0000");
        assert_eq!(loaded[1].name, "Client A");
        assert_eq!(loaded[1].tag, "client-a");

        settings.set_junk_check_incoming(false);
        assert!(!settings.junk_check_incoming());
        settings.set_junk_empty_on_exit(true);
        assert!(settings.junk_empty_on_exit());
        settings.set_junk_empty_on_exit_days(7);
        assert_eq!(settings.junk_empty_on_exit_days(), 7);
        settings.set_junk_lookup_addressbook(false);
        assert!(!settings.junk_lookup_addressbook());
        settings.set_junk_lookup_addressbook_local_only(true);
        assert!(settings.junk_lookup_addressbook_local_only());
        settings.set_junk_default_plugin("SpamAssassin");
        assert_eq!(settings.junk_default_plugin(), "SpamAssassin");
        settings.set_junk_custom_header("X-Spam-Status");
        assert_eq!(settings.junk_custom_header(), "X-Spam-Status");
        settings.set_junk_custom_header_value("Yes");
        assert_eq!(settings.junk_custom_header_value(), "Yes");

        // 3. Migration
        settings
            .apply_migrated_setting(
                "mail-labels",
                "['Work:#F57900|$Labelwork', 'Billing:#112233|billing']",
            )
            .unwrap();
        let migrated_labels = settings.mail_labels();
        assert_eq!(migrated_labels.len(), 2);
        assert_eq!(migrated_labels[0].name, "Work");
        assert_eq!(migrated_labels[1].name, "Billing");
        assert_eq!(migrated_labels[1].color, "#112233");

        settings
            .apply_migrated_setting("junk-check-incoming", "true")
            .unwrap();
        assert!(settings.junk_check_incoming());
        settings
            .apply_migrated_setting("junk-default-plugin", "Bogofilter")
            .unwrap();
        assert_eq!(settings.junk_default_plugin(), "Bogofilter");
        assert!(
            settings
                .apply_migrated_setting("junk-default-plugin", "InvalidPlugin")
                .is_err()
        );

        // 4. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.mail_labels().len(), 5);
        assert!(reset.junk_check_incoming());
        assert!(!reset.junk_empty_on_exit());
        assert_eq!(reset.junk_empty_on_exit_days(), 0);
        assert_eq!(reset.junk_default_plugin(), "Bogofilter");
    }

    #[test]
    fn composer_header_toggles_and_options_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Defaults
        assert!(settings.composer_show_from());
        assert!(settings.composer_show_cc());
        assert!(!settings.composer_show_bcc());
        assert!(!settings.composer_show_reply_to());
        assert!(settings.composer_prompt_empty_subject());
        assert!(settings.composer_prompt_only_bcc());
        assert!(settings.composer_inline_spelling());
        assert_eq!(settings.composer_reply_style(), "quoted");
        assert_eq!(settings.composer_forward_style(), "attached");
        assert!(!settings.composer_top_signature());
        assert!(settings.composer_signature_outline());

        // 2. Setters & Roundtrip
        settings.set_composer_show_from(false);
        assert!(!settings.composer_show_from());
        settings.set_composer_show_bcc(true);
        assert!(settings.composer_show_bcc());
        settings.set_composer_show_reply_to(true);
        assert!(settings.composer_show_reply_to());
        settings.set_composer_prompt_empty_subject(false);
        assert!(!settings.composer_prompt_empty_subject());
        settings.set_composer_prompt_only_bcc(false);
        assert!(!settings.composer_prompt_only_bcc());
        settings.set_composer_inline_spelling(false);
        assert!(!settings.composer_inline_spelling());
        settings.set_composer_reply_style("inline");
        assert_eq!(settings.composer_reply_style(), "inline");
        settings.set_composer_forward_style("quoted");
        assert_eq!(settings.composer_forward_style(), "quoted");
        settings.set_composer_top_signature(true);
        assert!(settings.composer_top_signature());
        settings.set_composer_signature_outline(false);
        assert!(!settings.composer_signature_outline());

        // 3. Normalization
        assert_eq!(normalize_composer_reply_style("ATTACH"), Some("attach"));
        assert_eq!(normalize_composer_reply_style("invalid"), None);
        assert_eq!(normalize_composer_forward_style("inline"), Some("inline"));
        assert_eq!(normalize_composer_forward_style("invalid"), None);

        // 4. Migration
        settings
            .apply_migrated_setting("composer-show-bcc", "true")
            .unwrap();
        assert!(settings.composer_show_bcc());
        settings
            .apply_migrated_setting("composer-reply-style", "attach")
            .unwrap();
        assert_eq!(settings.composer_reply_style(), "attach");
        assert!(
            settings
                .apply_migrated_setting("composer-reply-style", "nonexistent")
                .is_err()
        );

        // 5. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert!(reset.composer_show_from());
        assert!(reset.composer_show_cc());
        assert!(!reset.composer_show_bcc());
        assert!(!reset.composer_show_reply_to());
        assert!(reset.composer_prompt_empty_subject());
        assert_eq!(reset.composer_reply_style(), "quoted");
        assert_eq!(reset.composer_forward_style(), "attached");
        assert!(!reset.composer_top_signature());
        assert!(reset.composer_signature_outline());
    }

    #[test]
    fn mail_reading_policies_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Defaults
        assert_eq!(settings.mail_image_loading_policy(), "never");
        assert!(settings.mail_mark_seen());
        assert_eq!(settings.mail_mark_seen_timeout(), 1500);
        assert!(!settings.mail_caret_mode());
        assert!(settings.mail_magic_spacebar());
        assert!(settings.mail_citation_highlighting());
        assert_eq!(settings.mail_citation_color_1(), "#729FCF");
        assert_eq!(settings.mail_citation_color_2(), "#AD7FA8");
        assert_eq!(settings.mail_citation_color_3(), "#8AE234");
        assert_eq!(settings.mail_citation_color_4(), "#FCAF3E");
        assert_eq!(settings.mail_citation_color_5(), "#E9B96E");
        assert_eq!(
            settings.mail_citation_colors(),
            [
                "#729FCF".to_string(),
                "#AD7FA8".to_string(),
                "#8AE234".to_string(),
                "#FCAF3E".to_string(),
                "#E9B96E".to_string(),
            ]
        );
        assert!(!settings.mail_headers_collapsed());
        assert!(settings.mail_show_real_date());

        // 2. Setters & Hex normalization
        settings.set_mail_image_loading_policy("sometimes");
        assert_eq!(settings.mail_image_loading_policy(), "sometimes");
        assert!(!settings.auto_load_remote_images());

        settings.set_mail_image_loading_policy("always");
        assert_eq!(settings.mail_image_loading_policy(), "always");
        assert!(settings.auto_load_remote_images());

        settings.set_mail_mark_seen(false);
        assert!(!settings.mail_mark_seen());
        assert!(!settings.mark_read_on_selection());

        settings.set_mail_mark_seen_timeout(3000);
        assert_eq!(settings.mail_mark_seen_timeout(), 3000);
        assert_eq!(settings.mark_read_timeout(), 3000);

        settings.set_mail_caret_mode(true);
        assert!(settings.mail_caret_mode());

        settings.set_mail_magic_spacebar(false);
        assert!(!settings.mail_magic_spacebar());

        settings.set_mail_citation_highlighting(false);
        assert!(!settings.mail_citation_highlighting());

        settings.set_mail_citation_color_1("#112233");
        assert_eq!(settings.mail_citation_color_1(), "#112233");

        settings.set_mail_citation_color_2("#aabbcc");
        assert_eq!(settings.mail_citation_color_2(), "#AABBCC");

        settings.set_mail_citation_color_3("#123");
        assert_eq!(settings.mail_citation_color_3(), "#112233");

        settings.set_mail_citation_color_4("#445566");
        assert_eq!(settings.mail_citation_color_4(), "#445566");

        settings.set_mail_citation_color_5("#778899");
        assert_eq!(settings.mail_citation_color_5(), "#778899");

        settings.set_mail_headers_collapsed(true);
        assert!(settings.mail_headers_collapsed());

        settings.set_mail_show_real_date(false);
        assert!(!settings.mail_show_real_date());

        // 3. Normalization
        assert_eq!(normalize_image_loading_policy("ALWAYS"), Some("always"));
        assert_eq!(normalize_image_loading_policy("contacts"), Some("sometimes"));
        assert_eq!(normalize_image_loading_policy("never"), Some("never"));
        assert_eq!(normalize_image_loading_policy("invalid"), None);

        // 4. Migration
        settings
            .apply_migrated_setting("mail-image-loading-policy", "sometimes")
            .unwrap();
        assert_eq!(settings.mail_image_loading_policy(), "sometimes");
        settings
            .apply_migrated_setting("mail-mark-seen", "true")
            .unwrap();
        assert!(settings.mail_mark_seen());
        settings
            .apply_migrated_setting("mail-mark-seen-timeout", "4500")
            .unwrap();
        assert_eq!(settings.mail_mark_seen_timeout(), 4500);
        settings
            .apply_migrated_setting("mail-caret-mode", "true")
            .unwrap();
        assert!(settings.mail_caret_mode());
        settings
            .apply_migrated_setting("mail-magic-spacebar", "true")
            .unwrap();
        assert!(settings.mail_magic_spacebar());
        settings
            .apply_migrated_setting("mail-citation-highlighting", "true")
            .unwrap();
        assert!(settings.mail_citation_highlighting());
        settings
            .apply_migrated_setting("mail-citation-color-1", "#123456")
            .unwrap();
        assert_eq!(settings.mail_citation_color_1(), "#123456");
        settings
            .apply_migrated_setting("mail-headers-collapsed", "false")
            .unwrap();
        assert!(!settings.mail_headers_collapsed());
        settings
            .apply_migrated_setting("mail-show-real-date", "true")
            .unwrap();
        assert!(settings.mail_show_real_date());

        assert!(
            settings
                .apply_migrated_setting("mail-image-loading-policy", "bad_policy")
                .is_err()
        );

        // 5. Reset to defaults
        settings.reset_to_defaults();
        let reset = settings_with_backend(&backend);
        assert_eq!(reset.mail_image_loading_policy(), "never");
        assert!(reset.mail_mark_seen());
        assert_eq!(reset.mail_mark_seen_timeout(), 1500);
        assert!(!reset.mail_caret_mode());
        assert!(reset.mail_magic_spacebar());
        assert!(reset.mail_citation_highlighting());
        assert_eq!(reset.mail_citation_color_1(), "#729FCF");
        assert_eq!(reset.mail_citation_color_2(), "#AD7FA8");
        assert_eq!(reset.mail_citation_color_3(), "#8AE234");
        assert_eq!(reset.mail_citation_color_4(), "#FCAF3E");
        assert_eq!(reset.mail_citation_color_5(), "#E9B96E");
        assert!(!reset.mail_headers_collapsed());
        assert!(reset.mail_show_real_date());
    }

    #[test]
    fn reset_to_defaults_keys_have_perfect_schema_parity() {
        let schema = settings_schema();

        // 1. None of the 12 legacy keys may exist in org.gnome.Evolution4 schema
        const LEGACY_KEYS: &[&str] = &[
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

        for &legacy in LEGACY_KEYS {
            assert!(
                !schema.has_key(legacy),
                "Legacy key '{legacy}' must NOT exist in schema"
            );
        }

        // 2. The previously missing schema keys must exist in schema
        assert!(
            schema.has_key("debug-console-enabled"),
            "debug-console-enabled must exist in schema"
        );
        assert!(
            schema.has_key("mail-show-mark-read-actions"),
            "mail-show-mark-read-actions must exist in schema"
        );
    }

    #[test]
    fn reset_to_defaults_resets_mutated_values_cleanly() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // Mutate diverse settings across distinct domains and types
        settings.set_mail_show_statusbar(false);
        settings.set_offline_mode(true);
        settings.set_mail_sync_interval_seconds(600);
        settings.set_color_scheme("dark");
        settings.set_debug_console_enabled(true);
        settings.set_mail_show_mark_read_actions(true);
        settings.set_calendar_week_start("sunday");
        settings.set_mark_read_timeout(3000);
        settings.set_composer_html(false);

        // Verify mutations were applied
        assert_eq!(settings.mail_show_statusbar(), false);
        assert_eq!(settings.offline_mode(), true);
        assert_eq!(settings.mail_sync_interval_seconds(), 600);
        assert_eq!(settings.color_scheme(), "dark");
        assert_eq!(settings.debug_console_enabled(), true);
        assert_eq!(settings.mail_show_mark_read_actions(), true);
        assert_eq!(settings.calendar_week_start(), "sunday");
        assert_eq!(settings.mark_read_timeout(), 3000);
        assert_eq!(settings.composer_html(), false);

        // Reset all settings to defaults
        settings.reset_to_defaults();

        // Verify that values reverted to canonical schema defaults
        assert_eq!(settings.mail_show_statusbar(), true);
        assert_eq!(settings.offline_mode(), false);
        assert_eq!(settings.mail_sync_interval_seconds(), 300);
        assert_eq!(settings.color_scheme(), "system");
        assert_eq!(settings.debug_console_enabled(), false);
        assert_eq!(settings.mail_show_mark_read_actions(), false);
        assert_eq!(settings.calendar_week_start(), "monday");
        assert_eq!(settings.mark_read_timeout(), 1500);
        assert_eq!(settings.composer_html(), true);
    }

    #[test]
    fn test_color_scheme_parsing_and_mapping() {
        assert_eq!(AppSettings::parse_adw_color_scheme("light"), adw::ColorScheme::ForceLight);
        assert_eq!(AppSettings::parse_adw_color_scheme("dark"), adw::ColorScheme::ForceDark);
        assert_eq!(AppSettings::parse_adw_color_scheme("system"), adw::ColorScheme::Default);
        assert_eq!(AppSettings::parse_adw_color_scheme("unknown-value"), adw::ColorScheme::Default);

        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        settings.set_color_scheme("light");
        assert_eq!(settings.adw_color_scheme(), adw::ColorScheme::ForceLight);
        settings.set_color_scheme("dark");
        assert_eq!(settings.adw_color_scheme(), adw::ColorScheme::ForceDark);
        settings.set_color_scheme("system");
        assert_eq!(settings.adw_color_scheme(), adw::ColorScheme::Default);
    }

    #[test]
    fn test_spellcheck_languages_sanitization_accepts_underscores() {
        assert_eq!(
            sanitize_spellcheck_languages("en_US,de_DE"),
            "en_US,de_DE"
        );
        assert_eq!(
            sanitize_spellcheck_languages("en_US,de_DE,en_US"),
            "en_US,de_DE"
        );
        assert_eq!(
            sanitize_spellcheck_languages(""),
            ""
        );
        assert_eq!(
            sanitize_spellcheck_languages(" invalid!tag , fr_FR "),
            "fr_FR"
        );
    }

    #[test]
    fn test_composer_reply_style_normalization_and_persistence() {
        assert_eq!(normalize_composer_reply_style("quoted"), Some("quoted"));
        assert_eq!(normalize_composer_reply_style("inline"), Some("inline"));
        assert_eq!(normalize_composer_reply_style("attach"), Some("attach"));
        assert_eq!(normalize_composer_reply_style("attached"), Some("attach"));
        assert_eq!(normalize_composer_reply_style("do-not-quote"), Some("do-not-quote"));
        assert_eq!(normalize_composer_reply_style("none"), Some("do-not-quote"));
        assert_eq!(normalize_composer_reply_style("invalid"), None);

        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        settings.set_composer_reply_style("do-not-quote");
        assert_eq!(settings.composer_reply_style(), "do-not-quote");
        settings.set_composer_reply_style("none");
        assert_eq!(settings.composer_reply_style(), "do-not-quote");
        settings.set_composer_reply_style("attach");
        assert_eq!(settings.composer_reply_style(), "attach");
        settings.set_composer_reply_style("attached");
        assert_eq!(settings.composer_reply_style(), "attach");
        settings.set_composer_reply_style("quoted");
        assert_eq!(settings.composer_reply_style(), "quoted");
    }

    #[test]
    fn test_feature_19_reply_and_forward_style_permutations_and_boundary_stress() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Reply style permutations: casing and whitespace
        let reply_quoted_variants = ["quoted", "QUOTED", "Quoted", " quoted ", "\tquoted\n", "  QUOTED  "];
        for v in reply_quoted_variants {
            assert_eq!(normalize_composer_reply_style(v), Some("quoted"), "Failed for {v}");
            settings.set_composer_reply_style(v);
            assert_eq!(settings.composer_reply_style(), "quoted", "Failed persistence for {v}");
        }

        let reply_inline_variants = ["inline", "INLINE", "Inline", " inline ", "\tinline\n", "  INLINE  "];
        for v in reply_inline_variants {
            assert_eq!(normalize_composer_reply_style(v), Some("inline"), "Failed for {v}");
            settings.set_composer_reply_style(v);
            assert_eq!(settings.composer_reply_style(), "inline", "Failed persistence for {v}");
        }

        let reply_attach_variants = [
            "attach", "ATTACH", "Attach", " attach ", "\tattach\n",
            "attached", "ATTACHED", "Attached", " attached ", "\tattached\n"
        ];
        for v in reply_attach_variants {
            assert_eq!(normalize_composer_reply_style(v), Some("attach"), "Failed for {v}");
            settings.set_composer_reply_style(v);
            assert_eq!(settings.composer_reply_style(), "attach", "Failed persistence for {v}");
        }

        let reply_do_not_quote_variants = [
            "do-not-quote", "DO-NOT-QUOTE", "Do-Not-Quote", " do-not-quote ", "\tdo-not-quote\n",
            "none", "NONE", "None", " none ", "\tnone\n"
        ];
        for v in reply_do_not_quote_variants {
            assert_eq!(normalize_composer_reply_style(v), Some("do-not-quote"), "Failed for {v}");
            settings.set_composer_reply_style(v);
            assert_eq!(settings.composer_reply_style(), "do-not-quote", "Failed persistence for {v}");
        }

        // Invalid reply styles fallback safely to quoted
        let invalid_reply_variants = ["", "   ", "xyz", "attachment", "quote", "default", "other"];
        for v in invalid_reply_variants {
            assert_eq!(normalize_composer_reply_style(v), None, "Expected None for invalid {v}");
            settings.set_composer_reply_style(v);
            assert_eq!(settings.composer_reply_style(), "quoted", "Expected default fallback to quoted for {v}");
        }

        // 2. Forward style permutations: casing and whitespace
        let forward_attached_variants = [
            "attached", "ATTACHED", "Attached", " attached ", "\tattached\n",
            "attach", "ATTACH", "Attach", " attach ", "\tattach\n"
        ];
        for v in forward_attached_variants {
            assert_eq!(normalize_composer_forward_style(v), Some("attached"), "Failed for {v}");
            settings.set_composer_forward_style(v);
            assert_eq!(settings.composer_forward_style(), "attached", "Failed persistence for {v}");
        }

        let forward_inline_variants = ["inline", "INLINE", "Inline", " inline ", "\tinline\n", "  INLINE  "];
        for v in forward_inline_variants {
            assert_eq!(normalize_composer_forward_style(v), Some("inline"), "Failed for {v}");
            settings.set_composer_forward_style(v);
            assert_eq!(settings.composer_forward_style(), "inline", "Failed persistence for {v}");
        }

        let forward_quoted_variants = ["quoted", "QUOTED", "Quoted", " quoted ", "\tquoted\n", "  QUOTED  "];
        for v in forward_quoted_variants {
            assert_eq!(normalize_composer_forward_style(v), Some("quoted"), "Failed for {v}");
            settings.set_composer_forward_style(v);
            assert_eq!(settings.composer_forward_style(), "quoted", "Failed persistence for {v}");
        }

        // Invalid forward styles fallback safely to attached
        let invalid_forward_variants = ["", "   ", "xyz", "none", "do-not-quote", "default", "prefix"];
        for v in invalid_forward_variants {
            assert_eq!(normalize_composer_forward_style(v), None, "Expected None for invalid {v}");
            settings.set_composer_forward_style(v);
            assert_eq!(settings.composer_forward_style(), "attached", "Expected default fallback to attached for {v}");
        }

        // 3. Draft autosave interval boundary stress (0, 1, 30, 3600, negative, overflow)
        settings.set_draft_autosave_interval_seconds(0);
        assert_eq!(settings.draft_autosave_interval_seconds(), 0);

        settings.set_draft_autosave_interval_seconds(1);
        assert_eq!(settings.draft_autosave_interval_seconds(), 1);

        settings.set_draft_autosave_interval_seconds(30);
        assert_eq!(settings.draft_autosave_interval_seconds(), 30);

        settings.set_draft_autosave_interval_seconds(3600);
        assert_eq!(settings.draft_autosave_interval_seconds(), 3600);

        // Clamping on high values via setter
        settings.set_draft_autosave_interval_seconds(3601);
        assert_eq!(settings.draft_autosave_interval_seconds(), 3600);
        settings.set_draft_autosave_interval_seconds(10000);
        assert_eq!(settings.draft_autosave_interval_seconds(), 3600);
        settings.set_draft_autosave_interval_seconds(u32::MAX);
        assert_eq!(settings.draft_autosave_interval_seconds(), 3600);

        // Direct GSettings negative value injection
        let backend = gio::memory_settings_backend_new();
        let mem_settings = settings_with_backend(&backend);
        mem_settings.set_int("draft-autosave-interval", -1);
        assert_eq!(mem_settings.draft_autosave_interval_seconds(), 0, "Negative interval must clamp to 0 (disabled)");

        mem_settings.set_int("draft-autosave-interval", -500);
        assert_eq!(mem_settings.draft_autosave_interval_seconds(), 0, "Negative interval must clamp to 0 (disabled)");

        mem_settings.set_int("draft-autosave-interval", i32::MIN);
        assert_eq!(mem_settings.draft_autosave_interval_seconds(), 0, "i32::MIN must clamp to 0 (disabled)");

        mem_settings.set_int("draft-autosave-interval", 999999);
        assert_eq!(mem_settings.draft_autosave_interval_seconds(), 3600, "Large interval must clamp to 3600");

        // Clean reset
        settings.set_composer_reply_style("quoted");
        settings.set_composer_forward_style("attached");
        settings.set_draft_autosave_interval_seconds(30);
    }

    #[test]
    fn test_adversarial_stress_color_scheme_and_spellcheck_languages() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // 1. Color scheme 50-cycle rapid mutation across valid and invalid inputs
        let scheme_cases = [
            ("light", adw::ColorScheme::ForceLight, "light"),
            ("dark", adw::ColorScheme::ForceDark, "dark"),
            ("system", adw::ColorScheme::Default, "system"),
            ("LIGHT", adw::ColorScheme::Default, "system"),
            ("DARK", adw::ColorScheme::Default, "system"),
            ("", adw::ColorScheme::Default, "system"),
            ("   ", adw::ColorScheme::Default, "system"),
            ("invalid_scheme", adw::ColorScheme::Default, "system"),
            ("system\0null", adw::ColorScheme::Default, "system"),
            ("force-dark", adw::ColorScheme::Default, "system"),
            ("'; DROP TABLE--", adw::ColorScheme::Default, "system"),
            ("☀️🌙", adw::ColorScheme::Default, "system"),
        ];

        let initial_scheme = settings.color_scheme();
        for cycle in 0..50 {
            let (input, expected_adw, expected_persisted) = scheme_cases[cycle % scheme_cases.len()];
            assert_eq!(
                AppSettings::parse_adw_color_scheme(input),
                expected_adw,
                "Cycle {cycle} parse mismatch for {input:?}"
            );
            settings.set_color_scheme(input);
            assert_eq!(
                settings.color_scheme(),
                expected_persisted,
                "Cycle {cycle} persistence mismatch for {input:?}"
            );
            assert_eq!(
                settings.adw_color_scheme(),
                expected_adw,
                "Cycle {cycle} adw scheme mismatch for {input:?}"
            );
        }
        settings.set_color_scheme(&initial_scheme);

        // 2. Spellcheck languages adversarial sanitization & mutation across 50 cycles
        let lang_cases = [
            ("en_US", "en_US"),
            ("de_DE", "de_DE"),
            ("en_US,de_DE", "en_US,de_DE"),
            ("de_DE,en_US,fr_FR", "de_DE,en_US,fr_FR"),
            ("en-US", "en-US"),
            ("de-DE,en-US", "de-DE,en-US"),
            ("  en_US  ,  de_DE  ", "en_US,de_DE"),
            ("en_US,de_DE,en_US,de_DE", "en_US,de_DE"),
            ("en_US,en_US,en_US", "en_US"),
            ("", ""),
            ("   ", ""),
            (",", ""),
            (",,,,", ""),
            ("  ,  ,  ", ""),
            ("en!US", ""),
            ("de.DE", ""),
            ("fr/FR", ""),
            ("ru#RU,en_US", "en_US"),
            ("es$ES,it%IT,de_DE", "de_DE"),
            ("en@US, de_DE , fr;FR", "de_DE"),
            ("abcdefghijklmnopqrstuvwxyz123456789", "abcdefghijklmnopqrstuvwxyz123456789"),
            ("abcdefghijklmnopqrstuvwxyz1234567890", ""),
            ("🇬🇧,🇩🇪", ""),
            ("日本語,en_US", "en_US"),
        ];

        let initial_langs = settings.spellcheck_languages();
        for cycle in 0..50 {
            let (input, expected_sanitized) = lang_cases[cycle % lang_cases.len()];
            assert_eq!(
                sanitize_spellcheck_languages(input),
                expected_sanitized,
                "Cycle {cycle} sanitization mismatch for {input:?}"
            );
            settings.set_spellcheck_languages(input);
            assert_eq!(
                settings.spellcheck_languages(),
                expected_sanitized,
                "Cycle {cycle} persistence mismatch for {input:?}"
            );
        }
        settings.set_spellcheck_languages(&initial_langs);
    }

    #[test]
    fn test_mail_expanded_folders_nul_byte_safe() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);
        let test_folders = vec![
            "account-1".to_string(),
            "account-1\0INBOX".to_string(),
            "account-1\0Projects/2026".to_string(),
        ];
        settings.set_mail_expanded_folders(&test_folders);
        let loaded = settings.mail_expanded_folders();
        assert_eq!(loaded, test_folders);
    }

    #[test]
    fn test_date_and_time_format_settings() {
        let backend = gio::memory_settings_backend_new();
        let settings = settings_with_backend(&backend);

        // Default should be locale
        assert_eq!(settings.date_format(), "locale");
        assert_eq!(settings.time_format(), "locale");

        let resolved_df = settings.resolved_date_format();
        assert!(["german", "us", "iso"].contains(&resolved_df.as_str()));

        let resolved_tf = settings.resolved_time_format();
        assert!(["12h", "24h"].contains(&resolved_tf.as_str()));

        // Set custom date formats
        settings.set_date_format("german");
        assert_eq!(settings.date_format(), "german");
        assert_eq!(settings.resolved_date_format(), "german");

        settings.set_date_format("us");
        assert_eq!(settings.date_format(), "us");
        assert_eq!(settings.resolved_date_format(), "us");

        settings.set_date_format("iso");
        assert_eq!(settings.date_format(), "iso");
        assert_eq!(settings.resolved_date_format(), "iso");

        // Set custom time formats
        settings.set_time_format("12h");
        assert_eq!(settings.time_format(), "12h");
        assert_eq!(settings.resolved_time_format(), "12h");

        settings.set_time_format("24h");
        assert_eq!(settings.time_format(), "24h");
        assert_eq!(settings.resolved_time_format(), "24h");
    }
}

