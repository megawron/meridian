// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailPage — Three-column mail view with dynamic message selection, real-time search filtering, and WebKitGTK 6.0 rendering.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Duration, Local, NaiveDate, Utc};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::models::eds_source::EdsSourceObject;
use crate::models::folder::FolderObject;
use crate::models::mail_message::MailMessageObject;
use crate::services::AppSettings;
use crate::services::account_config_service::AccountConfigStore;
use crate::services::camel_mail_service::{self, CamelMailLoad};
use crate::services::crypto_service::CryptoService;
use crate::services::draft_store_service::{DraftMessage, DraftStoreService};
use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::eds_service;
use crate::services::inline_content_service::{InlineImage, resolve_cid_images};
use crate::services::itip_service::{
    ItipInvitation, ItipMethod, ItipReplyStatus, build_reply_component, normalize_attendee_address,
    parse_invitation,
};
use crate::services::mail_composer_service::{self, ExportAttachment, ExportMessage};
use crate::services::mail_operations_service::{
    MailCommand, MailCommandStack, MailOperationError, MailOperationsService,
};
use crate::services::mail_store_service::ParsedMail;
use crate::services::mail_thread_service::{ThreadIdentity, group_messages};
use crate::services::search_folder_service::SearchFolderService;
use crate::views::MailViewer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageFilter {
    All,
    Unread,
    Flagged,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MailSortField {
    #[default]
    Date,
    Subject,
    Sender,
    Unread,
    Flagged,
    Attachments,
}

impl MailSortField {
    fn from_setting(value: &str) -> Self {
        match value {
            "subject" => Self::Subject,
            "sender" => Self::Sender,
            "unread" | "status" => Self::Unread,
            "flagged" | "starred" | "important" => Self::Flagged,
            "attachments" | "attachment" => Self::Attachments,
            _ => Self::Date,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Subject => "subject",
            Self::Sender => "sender",
            Self::Unread => "unread",
            Self::Flagged => "flagged",
            Self::Attachments => "attachments",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MailLayout {
    #[default]
    Modern,
    Classic,
}

impl MailLayout {
    fn from_setting(value: &str) -> Self {
        if value == "classic" {
            Self::Classic
        } else {
            Self::Modern
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Classic => "classic",
        }
    }

    fn orientation(self) -> gtk::Orientation {
        match self {
            Self::Modern => gtk::Orientation::Horizontal,
            Self::Classic => gtk::Orientation::Vertical,
        }
    }

    fn default_position(self) -> i32 {
        match self {
            Self::Modern => 420,
            Self::Classic => 360,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FolderSelection {
    folder_id: String,
    source_uid: Option<String>,
    folder_name: Option<String>,
}

impl FolderSelection {
    fn all() -> Self {
        Self {
            folder_id: "all".to_string(),
            source_uid: None,
            folder_name: None,
        }
    }

    fn is_remote(&self) -> bool {
        self.source_uid.is_some() && self.folder_name.is_some()
    }

    fn matches(&self, message: &MailMessageObject) -> bool {
        if let (Some(source_uid), Some(folder_name)) =
            (self.source_uid.as_deref(), self.folder_name.as_deref())
        {
            return message
                .backend_location()
                .map(|location| {
                    location.source_uid == source_uid && location.folder_name == folder_name
                })
                .unwrap_or(false);
        }

        match self.folder_id.as_str() {
            "all" => true,
            "spam" => message.folder_id() == "junk",
            folder_id => message.folder_id() == folder_id,
        }
    }
}

impl Default for FolderSelection {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct MessageSearchQuery {
    text_terms: Vec<String>,
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    bcc: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    label: Option<String>,
    filename: Option<String>,
    unread: Option<bool>,
    flagged: Option<bool>,
    has_attachment: Option<bool>,
    folder: Option<String>,
    date_after: Option<NaiveDate>,
    date_before: Option<NaiveDate>,
}

impl MessageSearchQuery {
    fn parse(input: &str) -> Self {
        let mut query = Self::default();
        for token in search_tokens(input) {
            let Some((key, value)) = token.split_once(':') else {
                query.text_terms.push(token.to_lowercase());
                continue;
            };
            let value = value.trim().to_lowercase();
            if value.is_empty() {
                query.text_terms.push(token.to_lowercase());
                continue;
            }
            match key.to_ascii_lowercase().as_str() {
                "from" => query.from = Some(value),
                "to" => query.to = Some(value),
                "cc" => query.cc = Some(value),
                "bcc" => query.bcc = Some(value),
                "subject" => query.subject = Some(value),
                "body" | "content" => query.body = Some(value),
                "label" | "tag" | "keyword" => query.label = Some(value),
                "filename" | "name" => query.filename = Some(value),
                "is" => match value.as_str() {
                    "unread" => query.unread = Some(true),
                    "read" => query.unread = Some(false),
                    "starred" | "flagged" | "important" => query.flagged = Some(true),
                    "inbox" | "sent" | "sent-mail" | "draft" | "drafts" | "trash" | "deleted"
                    | "junk" | "spam" | "archive" | "all-mail" => query.folder = Some(value),
                    _ => query.text_terms.push(token.to_lowercase()),
                },
                "has" => match value.as_str() {
                    "attachment" | "attachments" => query.has_attachment = Some(true),
                    _ => query.text_terms.push(token.to_lowercase()),
                },
                "no" => match value.as_str() {
                    "attachment" | "attachments" => query.has_attachment = Some(false),
                    _ => query.text_terms.push(token.to_lowercase()),
                },
                "after" | "newer" => match parse_search_date(&value, false) {
                    Some(date) => query.date_after = Some(date),
                    None => query.text_terms.push(token.to_lowercase()),
                },
                "before" | "older" => match parse_search_date(&value, true) {
                    Some(date) => query.date_before = Some(date),
                    None => query.text_terms.push(token.to_lowercase()),
                },
                "on" => match parse_search_date(&value, false) {
                    Some(date) => {
                        query.date_after = Some(date);
                        query.date_before = date.checked_add_signed(Duration::days(1));
                    }
                    None => query.text_terms.push(token.to_lowercase()),
                },
                "in" | "folder" => query.folder = Some(value),
                _ => query.text_terms.push(token.to_lowercase()),
            }
        }
        query
    }

    fn is_empty(&self) -> bool {
        self.text_terms.is_empty()
            && self.from.is_none()
            && self.to.is_none()
            && self.cc.is_none()
            && self.bcc.is_none()
            && self.subject.is_none()
            && self.body.is_none()
            && self.label.is_none()
            && self.filename.is_none()
            && self.unread.is_none()
            && self.flagged.is_none()
            && self.has_attachment.is_none()
            && self.folder.is_none()
            && self.date_after.is_none()
            && self.date_before.is_none()
    }
}

fn search_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn parse_search_date(value: &str, inclusive_next_day: bool) -> Option<NaiveDate> {
    if let Some(days) = value
        .strip_suffix('d')
        .and_then(|value| value.parse::<i64>().ok())
    {
        return Some(Local::now().date_naive() - Duration::days(days.max(0)));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    if inclusive_next_day {
        date.checked_add_signed(Duration::days(1))
    } else {
        Some(date)
    }
}

fn message_date(message: &MailMessageObject) -> Option<NaiveDate> {
    let value = message.date_str();
    chrono::DateTime::parse_from_rfc2822(&value)
        .ok()
        .map(|date| date.date_naive())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(&value)
                .ok()
                .map(|date| date.date_naive())
        })
        .or_else(|| NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok())
}

fn format_message_date(value: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .or_else(|| chrono::DateTime::parse_from_rfc3339(value).ok());
    let Some(parsed) = parsed else {
        return value.to_string();
    };
    let settings = AppSettings::try_new();
    let date_format = settings
        .as_ref()
        .map(|settings| settings.date_format())
        .unwrap_or_else(|| "german".to_string());
    let time_format = settings
        .as_ref()
        .map(|settings| settings.time_format())
        .unwrap_or_else(|| "24h".to_string());
    let pattern = match (date_format.as_str(), time_format.as_str()) {
        ("iso", "12h") => "%Y-%m-%d %I:%M %p",
        ("iso", _) => "%Y-%m-%d %H:%M",
        ("us", "12h") => "%m/%d/%Y %I:%M %p",
        ("us", _) => "%m/%d/%Y %H:%M",
        ("german", "12h") => "%d.%m.%Y %I:%M %p",
        _ => "%d.%m.%Y %H:%M",
    };
    parsed.format(pattern).to_string()
}

fn mail_row_density_metrics(value: &str) -> (i32, i32) {
    match value {
        "compact" => (2, 4),
        "spacious" => (7, 14),
        _ => (4, 8),
    }
}

fn message_matches_filter(
    message: &MailMessageObject,
    query: &MessageSearchQuery,
    filter: MessageFilter,
    folder_selection: &FolderSelection,
) -> bool {
    let subject = message.subject().to_lowercase();
    let sender_name = message.sender_name().to_lowercase();
    let sender_email = message.sender_email().to_lowercase();
    let recipient_to = message.recipient_to().to_lowercase();
    let recipient_cc = message.recipient_cc().to_lowercase();
    let recipient_bcc = message.recipient_bcc().to_lowercase();
    let preview = message.preview_snippet().to_lowercase();
    let body = message.html_body().to_lowercase();
    let label_values = message.labels();
    let labels = label_values
        .iter()
        .map(|label| label.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let filenames = message
        .attachments()
        .into_iter()
        .map(|attachment| attachment.filename.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let searchable_text = [
        subject.as_str(),
        sender_name.as_str(),
        sender_email.as_str(),
        recipient_to.as_str(),
        recipient_cc.as_str(),
        recipient_bcc.as_str(),
        preview.as_str(),
        body.as_str(),
        labels.as_str(),
        filenames.as_str(),
    ]
    .join(" ");
    let text_matches = query
        .text_terms
        .iter()
        .all(|term| searchable_text.contains(term));
    let field_matches = query
        .from
        .as_deref()
        .map(|value| sender_name.contains(value) || sender_email.contains(value))
        .unwrap_or(true)
        && query
            .to
            .as_deref()
            .map(|value| {
                recipient_to.contains(value)
                    || recipient_cc.contains(value)
                    || recipient_bcc.contains(value)
            })
            .unwrap_or(true)
        && query
            .cc
            .as_deref()
            .map(|value| recipient_cc.contains(value))
            .unwrap_or(true)
        && query
            .bcc
            .as_deref()
            .map(|value| recipient_bcc.contains(value))
            .unwrap_or(true)
        && query
            .subject
            .as_deref()
            .map(|value| subject.contains(value))
            .unwrap_or(true)
        && query
            .body
            .as_deref()
            .map(|value| body.contains(value) || preview.contains(value))
            .unwrap_or(true)
        && query
            .label
            .as_deref()
            .map(|value| {
                label_values
                    .iter()
                    .any(|label| label.to_lowercase().contains(value))
            })
            .unwrap_or(true)
        && query
            .filename
            .as_deref()
            .map(|value| filenames.contains(value))
            .unwrap_or(true)
        && query
            .unread
            .map(|unread| message.is_unread() == unread)
            .unwrap_or(true)
        && query
            .flagged
            .map(|flagged| message.is_flagged() == flagged)
            .unwrap_or(true)
        && query
            .has_attachment
            .map(|has_attachment| message.attachments().is_empty() != has_attachment)
            .unwrap_or(true)
        && query
            .folder
            .as_deref()
            .map(|folder| message_folder_matches(message, folder))
            .unwrap_or(true)
        && query
            .date_after
            .map(|date| {
                message_date(message)
                    .map(|value| value >= date)
                    .unwrap_or(false)
            })
            .unwrap_or(true)
        && query
            .date_before
            .map(|date| {
                message_date(message)
                    .map(|value| value < date)
                    .unwrap_or(false)
            })
            .unwrap_or(true);
    let state_matches = match filter {
        MessageFilter::All => true,
        MessageFilter::Unread => message.is_unread(),
        MessageFilter::Flagged => message.is_flagged(),
    };

    (query.is_empty() || (text_matches && field_matches))
        && state_matches
        && folder_selection.matches(message)
}

fn message_folder_matches(message: &MailMessageObject, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    let matches_alias = |folder: &str| match query.as_str() {
        "inbox" => folder == "inbox",
        "sent" | "sent-mail" => folder == "sent",
        "draft" | "drafts" => folder == "drafts",
        "trash" | "deleted" => folder == "trash",
        "junk" | "spam" => folder == "junk",
        "archive" | "all-mail" => folder == "archive",
        value => folder == value || folder.trim_start_matches("custom:") == value,
    };
    if matches_alias(&message.folder_id().to_ascii_lowercase()) {
        return true;
    }
    message
        .backend_location()
        .is_some_and(|location| location.folder_name.to_ascii_lowercase().contains(&query))
}

fn sender_header(message: &MailMessageObject) -> String {
    let name = message.sender_name();
    let email = message.sender_email();
    if name.trim().is_empty() || name.trim().eq_ignore_ascii_case(email.trim()) {
        email
    } else if email.trim().is_empty() {
        name
    } else {
        format!("{} <{}>", name.trim(), email.trim())
    }
}

fn export_filename(subject: &str) -> String {
    let sanitized = subject
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.');
    let stem = if sanitized.is_empty() {
        "nachricht"
    } else {
        sanitized
    };
    format!("{}.eml", stem.chars().take(80).collect::<String>())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FolderRowData {
    folder_id: String,
    source_uid: Option<String>,
    folder_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MoveDestination {
    source_uid: String,
    folder_name: String,
    label: String,
}

impl FolderRowData {
    fn selection(&self) -> FolderSelection {
        FolderSelection {
            folder_id: self.folder_id.clone(),
            source_uid: self.source_uid.clone(),
            folder_name: self.folder_name.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct DynamicFolderNode {
    source_uid: String,
    source_display_name: String,
    folder_name: String,
    display_name: String,
    icon_name: String,
    unread_count: u32,
    total_count: u32,
    selectable: bool,
    parent_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MailLoadRequest {
    source_uid: Option<String>,
    folder_name: Option<String>,
    offset: usize,
    max_messages_per_folder: i32,
}

impl MailLoadRequest {
    fn initial() -> Self {
        Self {
            source_uid: None,
            folder_name: None,
            offset: 0,
            max_messages_per_folder: camel_mail_service::default_page_limit(),
        }
    }

    fn is_remote_or_paged(&self) -> bool {
        self.source_uid.is_some() || self.folder_name.is_some() || self.offset > 0
    }
}

enum MailPageLoadResult {
    Camel(CamelMailLoad),
    Local {
        reason: String,
        messages: Vec<ParsedMail>,
    },
    Error(String),
}

fn load_mail_backend(
    request: &MailLoadRequest,
    cancelled: &AtomicBool,
    progress_sender: Option<&async_channel::Sender<camel_mail_service::CamelSyncProgress>>,
) -> MailPageLoadResult {
    let camel_result = camel_mail_service::load_messages_page_with_progress_and_cancellation(
        request.source_uid.as_deref(),
        request.folder_name.as_deref(),
        request.offset,
        request.max_messages_per_folder,
        Some(cancelled),
        progress_sender,
    );

    if cancelled.load(Ordering::Relaxed) {
        return MailPageLoadResult::Error("Mail-Ladevorgang wurde abgebrochen.".to_string());
    }

    if request.is_remote_or_paged() {
        return match camel_result {
            Ok(camel) => MailPageLoadResult::Camel(camel),
            Err(error) => MailPageLoadResult::Error(error),
        };
    }

    match camel_result {
        Ok(camel)
            if camel.configured_sources > 0
                || !camel.messages.is_empty()
                || !camel.folders.is_empty() =>
        {
            MailPageLoadResult::Camel(camel)
        }
        Ok(_) => MailPageLoadResult::Local {
            reason: "Keine aktiven EDS-Mailkonten; lokaler Evolution-Speicher".to_string(),
            messages: crate::services::MailStoreService::new()
                .load_local_records_cancelable(cancelled),
        },
        Err(error) => MailPageLoadResult::Local {
            reason: format!("EDS/Camel nicht verfügbar ({error}); lokaler Evolution-Speicher"),
            messages: crate::services::MailStoreService::new()
                .load_local_records_cancelable(cancelled),
        },
    }
}

fn format_camel_status(camel: &CamelMailLoad) -> String {
    let account_label = if camel.configured_sources == 1 {
        "Konto"
    } else {
        "Konten"
    };
    let message_label = if camel.messages.len() == 1 {
        "Nachricht"
    } else {
        "Nachrichten"
    };
    let sync_status = if camel.failed_sources == 0 {
        format!(
            "{} {account_label} synchronisiert",
            camel.successful_sources
        )
    } else if camel.successful_sources > 0 {
        format!(
            "{}/{} {account_label} synchronisiert, {} mit Fehler",
            camel.successful_sources, camel.configured_sources, camel.failed_sources
        )
    } else {
        format!(
            "{} {account_label} nicht synchronisiert",
            camel.failed_sources
        )
    };
    let warning = camel
        .warning
        .as_deref()
        .map(|warning| format!(" · Hinweis: {warning}"))
        .unwrap_or_default();

    format!(
        "EDS/Camel: {sync_status}, {} {message_label} geladen{warning}",
        camel.messages.len()
    )
}

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MailPage {
        pub outer_paned: gtk::Paned,
        pub inner_paned: gtk::Paned,
        pub preview_widget: RefCell<Option<gtk::Widget>>,
        pub message_selection: RefCell<Option<gtk::SingleSelection>>,
        pub message_store: RefCell<Option<gio::ListStore>>,
        pub message_filter: RefCell<Option<gtk::CustomFilter>>,
        pub thread_selection: RefCell<Option<gtk::SingleSelection>>,
        pub thread_root_store: RefCell<Option<gio::ListStore>>,
        pub thread_filter: RefCell<Option<gtk::CustomFilter>>,
        pub thread_members: RefCell<Option<Rc<RefCell<HashMap<String, Vec<MailMessageObject>>>>>>,
        pub thread_children: RefCell<Option<Rc<RefCell<HashMap<String, gio::ListStore>>>>>,
        pub thread_selection_syncing: Cell<bool>,
        pub(super) folder_selection: RefCell<Option<Rc<RefCell<FolderSelection>>>>,
        pub folder_list: RefCell<Option<gtk::ListBox>>,
        pub dynamic_folder_store: RefCell<Option<gio::ListStore>>,
        pub dynamic_folder_selection: RefCell<Option<gtk::SingleSelection>>,
        pub dynamic_folder_children: RefCell<Option<Rc<RefCell<HashMap<String, gio::ListStore>>>>>,
        pub dynamic_folder_index: RefCell<Option<Rc<RefCell<HashMap<String, FolderObject>>>>>,
        pub folder_section: RefCell<Option<gtk::Box>>,
        pub search_list: RefCell<Option<gtk::ListBox>>,
        pub message_state: RefCell<Option<gtk::Label>>,
        pub refresh_button: RefCell<Option<gtk::Button>>,
        pub load_more_button: RefCell<Option<gtk::Button>>,
        pub batch_action_bar: RefCell<Option<gtk::Box>>,
        pub batch_action_label: RefCell<Option<gtk::Label>>,
        pub(crate) batch_selection: RefCell<HashSet<String>>,
        pub(crate) batch_binding: Cell<bool>,
        pub(super) active_request: RefCell<Option<MailLoadRequest>>,
        pub load_generation: Cell<u64>,
        pub mail_loading: Cell<bool>,
        pub mail_change_pending: Cell<bool>,
        pub mail_incremental_loading: Cell<bool>,
        pub updating_folder_rows: Cell<bool>,
        pub mail_sync_source: RefCell<Option<glib::SourceId>>,
        pub mail_load_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub mail_incremental_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub mail_change_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub(crate) mail_commands: RefCell<MailCommandStack>,
        pub preview_settings: RefCell<Option<AppSettings>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub(super) mail_sort_field: Cell<MailSortField>,
        pub mail_sort_descending: Cell<bool>,
        pub mail_threading: Cell<bool>,
        pub(super) mail_layout: Cell<MailLayout>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailPage {
        const NAME: &'static str = "EvolutionMailPage";
        type Type = super::MailPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for MailPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.mail_commands.replace(MailCommandStack::persistent());
            let preview_position = if let Some(settings) = AppSettings::try_new() {
                self.mail_sort_field
                    .set(MailSortField::from_setting(&settings.mail_sort_field()));
                self.mail_sort_descending
                    .set(settings.mail_sort_descending());
                self.mail_threading.set(settings.mail_threading());
                let layout = MailLayout::from_setting(&settings.mail_layout());
                self.mail_layout.set(layout);
                settings.mail_preview_position(layout.setting_value())
            } else {
                self.mail_sort_descending.set(true);
                self.mail_threading.set(true);
                self.mail_layout.set(MailLayout::Modern);
                self.mail_layout.get().default_position()
            };
            self.inner_paned
                .set_orientation(self.mail_layout.get().orientation());
            self.inner_paned.set_wide_handle(true);
            self.inner_paned.set_resize_start_child(true);
            self.inner_paned.set_shrink_start_child(true);
            self.inner_paned.set_resize_end_child(true);
            self.inner_paned.set_shrink_end_child(true);
            self.inner_paned.set_position(preview_position);
            let obj = self.obj();

            let (sidebar, source_store, source_state, folder_section, folder_list, search_list) =
                obj.build_sidebar();
            let (
                message_container,
                message_state,
                list_store,
                selection,
                custom_filter,
                thread_filter,
                message_filter,
                folder_selection,
                search_query,
            ) = obj.build_message_list();
            let (
                detail_view,
                mail_viewer,
                lbl_subject,
                lbl_from,
                lbl_to,
                lbl_cc,
                lbl_date,
                lbl_crypto,
                attachment_bar,
                thread_context,
            ) = obj.build_detail_view();

            obj.connect_selection_handler(
                &selection,
                &list_store,
                &custom_filter,
                &thread_filter,
                &mail_viewer,
                &lbl_subject,
                &lbl_from,
                &lbl_to,
                &lbl_cc,
                &lbl_date,
                &lbl_crypto,
                &attachment_bar,
                &thread_context,
            );
            obj.connect_folder_filters(
                &folder_list,
                &search_list,
                &custom_filter,
                &thread_filter,
                &folder_selection,
                &message_filter,
                &search_query,
            );

            self.message_selection.replace(Some(selection.clone()));
            self.message_store.replace(Some(list_store.clone()));
            self.message_filter.replace(Some(custom_filter.clone()));
            self.thread_filter.replace(Some(thread_filter.clone()));
            self.folder_selection
                .replace(Some(folder_selection.clone()));
            self.folder_list.replace(Some(folder_list.clone()));
            self.folder_section.replace(Some(folder_section.clone()));
            self.search_list.replace(Some(search_list.clone()));
            self.message_state.replace(Some(message_state.clone()));

            obj.assemble_layout(sidebar, message_container, detail_view);
            obj.bind_preview_setting();
            obj.load_initial_data(&source_store, &source_state, &folder_section);
        }

        fn dispose(&self) {
            if let Some(source) = self.mail_sync_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(cancelled) = self.mail_load_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(cancelled) = self.mail_incremental_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(cancelled) = self.mail_change_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            self.outer_paned.unparent();
        }
    }

    impl WidgetImpl for MailPage {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct MailPage(ObjectSubclass<imp::MailPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MailPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Returns the outer paned widget (folder sidebar | message area).
    pub fn outer_paned(&self) -> &gtk::Paned {
        &self.imp().outer_paned
    }

    /// Returns the message-list/preview divider.  Its orientation is
    /// horizontal for the modern layout and vertical for the classic
    /// Evolution-compatible layout.
    pub fn inner_paned(&self) -> &gtk::Paned {
        &self.imp().inner_paned
    }

    pub fn preview_widget(&self) -> Option<gtk::Widget> {
        self.imp().preview_widget.borrow().clone()
    }

    pub fn set_sidebar_visible(&self, visible: bool) {
        // Show/hide the folder sidebar by collapsing the paned to 0 or restoring
        let paned = &self.imp().outer_paned;
        if visible {
            let saved_position = AppSettings::try_new()
                .map(|settings| settings.mail_sidebar_position())
                .unwrap_or(220);
            if paned.position() < 10 {
                paned.set_position(saved_position.max(10));
            }
        } else {
            paned.set_position(0);
        }
    }

    fn bind_preview_setting(&self) {
        let settings = AppSettings::try_new();
        let show_preview = settings
            .as_ref()
            .map(AppSettings::show_message_preview)
            .unwrap_or(true);
        self.set_preview_visible(show_preview);
        if let Some(settings) = settings {
            let page_weak_for_preview = self.downgrade();
            settings.connect_changed(Some("show-message-preview"), move |settings, _| {
                if let Some(page) = page_weak_for_preview.upgrade() {
                    page.set_preview_visible(settings.boolean("show-message-preview"));
                }
            });
            let page_weak_for_density = self.downgrade();
            settings.connect_changed(Some("mail-row-density"), move |_, _| {
                if let Some(page) = page_weak_for_density.upgrade() {
                    page.refresh_mail();
                }
            });
            for key in [
                "mail-show-avatar",
                "mail-show-sender",
                "mail-show-date",
                "mail-show-preview-text",
                "mail-show-labels",
            ] {
                let page_weak_for_columns = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(page) = page_weak_for_columns.upgrade() {
                        page.refresh_mail();
                    }
                });
            }
            let page_weak_for_sort_field = self.downgrade();
            settings.connect_changed(Some("mail-sort-field"), move |_, _| {
                if let Some(page) = page_weak_for_sort_field.upgrade() {
                    page.reload_sort_preferences();
                }
            });
            let page_weak_for_sort_direction = self.downgrade();
            settings.connect_changed(Some("mail-sort-descending"), move |_, _| {
                if let Some(page) = page_weak_for_sort_direction.upgrade() {
                    page.reload_sort_preferences();
                }
            });
            let page_weak_for_threading = self.downgrade();
            settings.connect_changed(Some("mail-threading"), move |_, _| {
                if let Some(page) = page_weak_for_threading.upgrade() {
                    page.reload_sort_preferences();
                }
            });
            let page_weak_for_sync_interval = self.downgrade();
            settings.connect_changed(Some("mail-sync-interval"), move |_, _| {
                if let Some(page) = page_weak_for_sync_interval.upgrade() {
                    page.restart_mail_polling();
                }
            });
            let page_weak_for_layout = self.downgrade();
            settings.connect_changed(Some("mail-layout"), move |settings, _| {
                if let Some(page) = page_weak_for_layout.upgrade() {
                    page.apply_mail_layout(MailLayout::from_setting(
                        &settings.string("mail-layout"),
                    ));
                }
            });
            let settings_for_position = settings.clone();
            let page_weak_for_position = self.downgrade();
            self.imp()
                .inner_paned
                .connect_position_notify(move |paned| {
                    if let Some(page) = page_weak_for_position.upgrade() {
                        settings_for_position.set_mail_preview_position(
                            page.imp().mail_layout.get().setting_value(),
                            paned.position(),
                        );
                    }
                });
            self.imp().preview_settings.replace(Some(settings));
        }
    }

    fn apply_mail_layout(&self, layout: MailLayout) {
        let imp = self.imp();
        let previous_layout = imp.mail_layout.get();
        if previous_layout == layout {
            return;
        }
        let next_position = AppSettings::try_new()
            .map(|settings| {
                settings.set_mail_preview_position(
                    previous_layout.setting_value(),
                    imp.inner_paned.position(),
                );
                settings.mail_preview_position(layout.setting_value())
            })
            .unwrap_or_else(|| layout.default_position());
        imp.mail_layout.set(layout);
        imp.inner_paned.set_orientation(layout.orientation());
        imp.inner_paned.set_position(next_position);
    }

    fn set_preview_visible(&self, visible: bool) {
        if let Some(preview) = self.imp().preview_widget.borrow().as_ref() {
            preview.set_visible(visible);
        }
    }

    fn reload_sort_preferences(&self) {
        if let Some(settings) = AppSettings::try_new() {
            self.imp()
                .mail_sort_field
                .set(MailSortField::from_setting(&settings.mail_sort_field()));
            self.imp()
                .mail_sort_descending
                .set(settings.mail_sort_descending());
            self.imp().mail_threading.set(settings.mail_threading());
        }
        if let Some(store) = self.imp().message_store.borrow().as_ref().cloned() {
            self.refresh_thread_metadata(&store);
        }
    }

    pub fn focus_search(&self) {
        if let Some(search_entry) = self.imp().search_entry.borrow().as_ref() {
            search_entry.grab_focus();
        }
    }

    /// Open a reply composer for the currently selected message.  Keeping the
    /// selection lookup here makes buttons and keyboard actions share exactly
    /// the same message context.
    pub fn reply_selected(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        let sender = sender_header(&message);
        let dialog = crate::dialogs::ComposeDialog::reply_to_message(
            &sender,
            &message.reply_to(),
            &message.subject(),
            &message.html_body(),
        );
        self.present_composer(dialog)
    }

    pub fn reply_all_selected(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        let sender = sender_header(&message);
        let dialog = crate::dialogs::ComposeDialog::reply_all_message(
            &sender,
            &message.reply_to(),
            &message.recipient_to(),
            &message.recipient_cc(),
            &message.subject(),
            &message.html_body(),
        );
        self.present_composer(dialog)
    }

    pub fn forward_selected(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        let dialog = crate::dialogs::ComposeDialog::forward_message(
            &message.subject(),
            &sender_header(&message),
            &message.recipient_to(),
            &message.recipient_cc(),
            &message.date_str(),
            &message.html_body(),
        );
        self.present_composer(dialog)
    }

    /// Export the selected message as a complete RFC822/EML file.  This is a
    /// local, loss-minimizing operation: the original EDS/Camel message is
    /// never modified and the parsed thread and attachment metadata are
    /// retained in the exported headers and MIME parts.
    pub fn export_selected(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        let export = ExportMessage {
            from: sender_header(&message),
            reply_to: message.reply_to(),
            to: message.recipient_to(),
            cc: message.recipient_cc(),
            bcc: message.recipient_bcc(),
            subject: message.subject(),
            date: message.date_str(),
            message_id: message.message_id(),
            in_reply_to: message.in_reply_to(),
            references: message.references(),
            html_body: message.html_body(),
            attachments: message
                .attachments()
                .into_iter()
                .map(|attachment| ExportAttachment {
                    filename: attachment.filename,
                    mime_type: attachment.mime_type,
                    content: attachment.content,
                })
                .collect(),
        };
        let raw = match mail_composer_service::build_export_rfc822(&export) {
            Ok(raw) => raw,
            Err(error) => {
                self.show_toast(&format!("E-Mail konnte nicht exportiert werden: {error}"));
                return true;
            }
        };
        let Some(root) = self.root().and_downcast::<gtk::Window>() else {
            self.show_toast("Kein Fenster für den E-Mail-Export verfügbar");
            return true;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("E-Mail als EML speichern")
            .initial_name(export_filename(&export.subject))
            .modal(true)
            .build();
        let page = self.downgrade();
        file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let save_result = file.replace_contents(
                &raw,
                None,
                true,
                gio::FileCreateFlags::NONE,
                None::<&gio::Cancellable>,
            );
            if let Some(page) = page.upgrade() {
                match save_result {
                    Ok(_) => page.show_toast("E-Mail als EML exportiert"),
                    Err(error) => {
                        page.show_toast(&format!("EML konnte nicht gespeichert werden: {error}"))
                    }
                }
            }
        });
        true
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Mail page result");
    }

    fn refresh_search_folders(&self) {
        if let Some(search_list) = self.imp().search_list.borrow().as_ref() {
            Self::populate_search_folders(search_list);
        }
    }

    fn selected_message(&self) -> Option<MailMessageObject> {
        self.imp()
            .message_selection
            .borrow()
            .as_ref()
            .and_then(|selection| selection.selected_item())
            .and_then(|item| item.downcast::<MailMessageObject>().ok())
    }

    pub fn has_selected_message(&self) -> bool {
        self.selected_message().is_some()
    }

    fn present_composer(&self, dialog: crate::dialogs::ComposeDialog) -> bool {
        let Some(root) = self.root().and_downcast::<gtk::Window>() else {
            tracing::warn!("Cannot open composer without a parent window");
            return false;
        };
        dialog.set_transient_for(Some(&root));
        dialog.present();
        true
    }

    pub fn select_relative_message(&self, offset: i32) {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return;
        };
        let count = selection.n_items();
        if count == 0 {
            return;
        }

        let current = selection.selected();
        let current = if current == gtk::INVALID_LIST_POSITION {
            0
        } else {
            current as i32
        };
        let next = (current + offset).clamp(0, count.saturating_sub(1) as i32) as u32;
        selection.set_selected(next);
    }

    /// Route destructive actions through the backend service.  Local imports
    /// remain untouched; local deletion is represented by the app-owned
    /// sidecar until a writable Camel backend is available.
    pub fn delete_selected(&self) -> Result<bool, MailOperationError> {
        let selected = self
            .imp()
            .message_selection
            .borrow()
            .as_ref()
            .and_then(|selection| selection.selected_item())
            .and_then(|item| item.downcast::<MailMessageObject>().ok());

        let Some(message) = selected else {
            return Ok(false);
        };

        let result = match message.backend_location() {
            Some(location) => {
                self.execute_mail_command(MailCommand::delete(message.id(), location))
            }
            None => self.execute_mail_command(MailCommand::delete_local(message.id())),
        };
        result.map(|()| {
            self.refresh_mail();
            true
        })
    }

    pub fn undo(&self) -> Result<bool, MailOperationError> {
        let operations = MailOperationsService::new();
        let command = self.imp().mail_commands.borrow_mut().undo(&operations)?;
        if let Some(command) = command {
            tracing::info!(operation = command.description(), "Mail command undone");
            self.refresh_mail();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn redo(&self) -> Result<bool, MailOperationError> {
        let operations = MailOperationsService::new();
        let command = self.imp().mail_commands.borrow_mut().redo(&operations)?;
        if let Some(command) = command {
            tracing::info!(operation = command.description(), "Mail command redone");
            self.refresh_mail();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn mark_selected_unread(&self) -> Result<bool, MailOperationError> {
        let selected = self
            .imp()
            .message_selection
            .borrow()
            .as_ref()
            .and_then(|selection| selection.selected_item())
            .and_then(|item| item.downcast::<MailMessageObject>().ok());

        let Some(message) = selected else {
            return Ok(false);
        };

        let command = MailCommand::set_message_state(
            message.id(),
            message.backend_location(),
            message.is_unread(),
            message.is_flagged(),
            true,
            message.is_flagged(),
        );
        self.execute_mail_command(command)?;
        message.set_unread(true);
        if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
        Ok(true)
    }

    fn execute_mail_command(&self, command: MailCommand) -> Result<(), MailOperationError> {
        let operations = MailOperationsService::new();
        self.imp()
            .mail_commands
            .borrow_mut()
            .execute(&operations, command)
    }

    fn set_batch_message_selected(&self, id: &str, selected: bool) {
        if selected {
            self.imp()
                .batch_selection
                .borrow_mut()
                .insert(id.to_string());
        } else {
            self.imp().batch_selection.borrow_mut().remove(id);
        }
        self.update_batch_action_bar();
    }

    fn selected_batch_messages(&self) -> Vec<MailMessageObject> {
        let selected_ids = self.imp().batch_selection.borrow().clone();
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return Vec::new();
        };
        (0..store.n_items())
            .filter_map(|index| store.item(index).and_downcast::<MailMessageObject>())
            .filter(|message| selected_ids.contains(&message.id()))
            .collect()
    }

    fn update_batch_action_bar(&self) {
        let count = self.imp().batch_selection.borrow().len();
        if let Some(label) = self.imp().batch_action_label.borrow().as_ref() {
            label.set_label(&format!("{count} ausgewählt"));
        }
        if let Some(bar) = self.imp().batch_action_bar.borrow().as_ref() {
            bar.set_visible(count > 0);
        }
    }

    fn clear_operation_error(&self) {
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.remove_css_class("error");
        }
    }

    fn report_operation_error(&self, action: &str, error: &MailOperationError) {
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.add_css_class("error");
            state.set_label(&format!("{action}: {error}"));
        }
    }

    fn clear_batch_selection(&self) {
        self.imp().batch_selection.borrow_mut().clear();
        if let Some(store) = self.imp().message_store.borrow().as_ref() {
            let count = store.n_items();
            if count > 0 {
                store.items_changed(0, count, count);
            }
        }
        self.update_batch_action_bar();
    }

    fn mark_batch_read(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        let commands = messages
            .iter()
            .filter(|message| message.is_unread())
            .map(|message| {
                MailCommand::set_message_state(
                    message.id(),
                    message.backend_location(),
                    true,
                    message.is_flagged(),
                    false,
                    message.is_flagged(),
                )
            })
            .collect::<Vec<_>>();
        let count = commands.len();
        if count > 0 {
            let operations = MailOperationsService::new();
            self.imp()
                .mail_commands
                .borrow_mut()
                .execute_batch(&operations, commands)?;
            for message in messages {
                message.set_unread(false);
            }
            if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
        }
        self.clear_batch_selection();
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{count} Nachricht(en) als gelesen markiert"));
        }
        Ok(())
    }

    fn archive_batch(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        let commands = messages
            .iter()
            .map(|message| {
                message
                    .backend_location()
                    .map(|location| MailCommand::archive(message.id(), location))
                    .unwrap_or_else(|| {
                        MailCommand::archive_local(message.id(), message.folder_id())
                    })
            })
            .collect::<Vec<_>>();
        let operations = MailOperationsService::new();
        let archived = self
            .imp()
            .mail_commands
            .borrow_mut()
            .execute_batch(&operations, commands)?;
        self.clear_batch_selection();
        if archived > 0 {
            self.refresh_mail();
        }
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{archived} Nachricht(en) archiviert"));
        }
        Ok(())
    }

    fn mark_batch_junk(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        let operations = MailOperationsService::new();
        let commands = messages
            .iter()
            .map(|message| match message.backend_location() {
                Some(location) => MailCommand::set_junk(
                    message.id(),
                    location,
                    message.folder_id() == "junk",
                    true,
                ),
                None => MailCommand::set_junk_local(message.id(), message.folder_id()),
            })
            .collect::<Vec<_>>();
        let marked = self
            .imp()
            .mail_commands
            .borrow_mut()
            .execute_batch(&operations, commands)?;
        self.clear_batch_selection();
        if marked > 0 {
            self.refresh_mail();
        }
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{marked} Nachricht(en) als Spam markiert"));
        }
        Ok(())
    }

    fn delete_batch(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        let commands = messages
            .iter()
            .map(|message| {
                message
                    .backend_location()
                    .map(|location| MailCommand::delete(message.id(), location))
                    .unwrap_or_else(|| MailCommand::delete_local(message.id()))
            })
            .collect::<Vec<_>>();
        let operations = MailOperationsService::new();
        let deleted = self
            .imp()
            .mail_commands
            .borrow_mut()
            .execute_batch(&operations, commands)?;
        self.clear_batch_selection();
        if deleted > 0 {
            self.refresh_mail();
        }
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{deleted} Nachricht(en) gelöscht"));
        }
        Ok(())
    }

    fn folder_destinations(&self) -> Vec<MoveDestination> {
        let mut destinations = Vec::new();
        if let Some(folder_index) = self.imp().dynamic_folder_index.borrow().as_ref().cloned() {
            for folder in folder_index.borrow().values() {
                let (Some(source_uid), Some(folder_name)) =
                    (folder.source_uid(), folder.folder_name())
                else {
                    continue;
                };
                if !folder.selectable() {
                    continue;
                }
                destinations.push(MoveDestination {
                    source_uid,
                    folder_name,
                    label: folder.name(),
                });
            }
        }

        let has_local_messages = self
            .selected_batch_messages()
            .iter()
            .any(|message| message.backend_location().is_none());
        if has_local_messages {
            for (folder_name, label) in [
                ("inbox", "Lokaler Posteingang"),
                ("sent", "Lokal · Gesendet"),
                ("drafts", "Lokal · Entwürfe"),
                ("trash", "Lokaler Papierkorb"),
                ("archive", "Lokales Archiv"),
                ("junk", "Lokaler Spam"),
            ] {
                destinations.push(MoveDestination {
                    source_uid: "local".to_string(),
                    folder_name: folder_name.to_string(),
                    label: label.to_string(),
                });
            }
        }

        destinations.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| left.source_uid.cmp(&right.source_uid))
                .then_with(|| left.folder_name.cmp(&right.folder_name))
        });
        destinations.dedup_by(|left, right| {
            left.source_uid == right.source_uid && left.folder_name == right.folder_name
        });
        destinations
    }

    fn move_batch_to(&self, destination_source: &str, destination_folder: &str) {
        let messages = self.selected_batch_messages();
        let has_local = messages
            .iter()
            .any(|message| message.backend_location().is_none());
        let has_remote = messages
            .iter()
            .any(|message| message.backend_location().is_some());
        if has_local && has_remote {
            self.report_operation_error(
                "Verschieben nicht möglich",
                &MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Move,
                    backend: "gemischte lokale/EDS-Auswahl",
                },
            );
            return;
        }

        let commands = if destination_source == "local" {
            messages
                .iter()
                .filter(|message| message.backend_location().is_none())
                .map(|message| {
                    MailCommand::move_local(
                        message.id(),
                        message.folder_id(),
                        destination_folder.to_string(),
                    )
                })
                .collect::<Vec<_>>()
        } else {
            messages
                .iter()
                .filter_map(|message| {
                    let location = message.backend_location()?;
                    (location.source_uid == destination_source).then(|| {
                        MailCommand::move_to(message.id(), location, destination_folder.to_string())
                    })
                })
                .collect::<Vec<_>>()
        };

        if commands.is_empty() {
            self.report_operation_error(
                "Verschieben nicht möglich",
                &MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Move,
                    backend: "kein gemeinsames EDS-Konto",
                },
            );
            return;
        }

        let operations = MailOperationsService::new();
        let result = self
            .imp()
            .mail_commands
            .borrow_mut()
            .execute_batch(&operations, commands);
        match result {
            Ok(count) => {
                self.clear_operation_error();
                self.clear_batch_selection();
                self.refresh_mail();
                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    state.set_label(&format!("{count} Nachricht(en) verschoben"));
                }
            }
            Err(error) => {
                self.report_operation_error("Verschieben nicht möglich", &error);
            }
        }
    }

    // ── Private: Build Sidebar (Folder Tree) ────────────────────────────

    fn build_sidebar(
        &self,
    ) -> (
        adw::NavigationPage,
        gio::ListStore,
        gtk::Label,
        gtk::Box,
        gtk::ListBox,
        gtk::ListBox,
    ) {
        let source_store = gio::ListStore::new::<EdsSourceObject>();
        let source_selection = gtk::SingleSelection::new(Some(source_store.clone()));
        source_selection.set_autoselect(false);
        source_selection.set_can_unselect(true);
        let source_factory = gtk::SignalListItemFactory::new();

        source_factory.connect_setup(|_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");

            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .margin_start(8)
                .margin_end(8)
                .margin_top(6)
                .margin_bottom(6)
                .build();

            let icon = gtk::Image::from_icon_name("mail-account-symbolic");
            icon.add_css_class("dim-label");

            let details = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .hexpand(true)
                .build();
            let title = gtk::Label::builder()
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            let subtitle = gtk::Label::builder()
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(vec!["caption", "dim-label"])
                .build();

            details.append(&title);
            details.append(&subtitle);
            row.append(&icon);
            row.append(&details);
            list_item.set_child(Some(&row));
        });

        source_factory.connect_bind(|_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");
            let source = list_item
                .item()
                .and_downcast::<EdsSourceObject>()
                .expect("Expected EdsSourceObject");
            let row = list_item
                .child()
                .and_downcast::<gtk::Box>()
                .expect("Expected source row");

            if let Some(details) = row.last_child().and_downcast::<gtk::Box>() {
                if let Some(title) = details.first_child().and_downcast::<gtk::Label>() {
                    title.set_label(&source.display_name());
                }
                if let Some(subtitle) = details.last_child().and_downcast::<gtk::Label>() {
                    subtitle.set_label(&format!(
                        "{} · {} · {}",
                        source.backend_name(),
                        if source.enabled() {
                            "aktiv"
                        } else {
                            "deaktiviert / Backend ausstehend"
                        },
                        source.origin()
                    ));
                }
            }
            row.set_sensitive(source.enabled());
        });

        let source_view = gtk::ListView::builder()
            .model(&source_selection)
            .factory(&source_factory)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        let page_for_source = self.downgrade();
        source_selection.connect_selected_item_notify(move |selection| {
            if let Some(source) = selection.selected_item().and_downcast::<EdsSourceObject>() {
                tracing::debug!(
                    uid = %source.uid(),
                    display_name = %source.display_name(),
                    "EDS mail source selected"
                );
                if let Some(page) = page_for_source.upgrade() {
                    if let Some(folder_selection) = page.imp().folder_selection.borrow().as_ref() {
                        folder_selection.replace(FolderSelection::all());
                    }
                    page.select_folder_row(&FolderSelection::all());
                    if let Some(filter) = page.imp().message_filter.borrow().as_ref() {
                        filter.changed(gtk::FilterChange::Different);
                    }
                    page.start_mail_load(
                        MailLoadRequest {
                            source_uid: Some(source.uid()),
                            folder_name: None,
                            offset: 0,
                            max_messages_per_folder: camel_mail_service::default_page_limit(),
                        },
                        true,
                    );
                }
            }
        });

        let folder_list = gtk::ListBox::new();
        folder_list.set_selection_mode(gtk::SelectionMode::Single);
        folder_list.add_css_class("navigation-sidebar");

        let account_header = gtk::Label::builder()
            .label("Konten (EDS)")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_top(8)
            .margin_bottom(4)
            .build();

        let folders = [
            ("all", "mail-symbolic", "Alle Nachrichten", ""),
            ("inbox", "mail-inbox-symbolic", "Posteingang", ""),
            ("sent", "mail-sent-symbolic", "Gesendet", ""),
            ("drafts", "mail-drafts-symbolic", "Entwürfe", ""),
            ("trash", "edit-delete-symbolic", "Papierkorb", ""),
            ("archive", "mail-archive-symbolic", "Archiv", ""),
            ("spam", "folder-symbolic", "Spam", ""),
        ];

        for (id, icon, label, badge) in folders {
            let row = Self::create_folder_row(id, icon, label, badge);
            folder_list.append(&row);
        }

        let dynamic_folder_store = gio::ListStore::new::<FolderObject>();
        let dynamic_folder_children =
            Rc::new(RefCell::new(HashMap::<String, gio::ListStore>::new()));
        let dynamic_folder_index = Rc::new(RefCell::new(HashMap::<String, FolderObject>::new()));
        let children_for_dynamic_model = Rc::clone(&dynamic_folder_children);
        let dynamic_tree_model =
            gtk::TreeListModel::new(dynamic_folder_store.clone(), false, false, move |item| {
                let folder = item.downcast_ref::<FolderObject>()?;
                children_for_dynamic_model
                    .borrow()
                    .get(&folder.id())
                    .cloned()
                    .map(|store| store.upcast::<gio::ListModel>())
            });
        let dynamic_folder_selection = gtk::SingleSelection::new(Some(dynamic_tree_model));
        dynamic_folder_selection.set_autoselect(false);
        dynamic_folder_selection.set_can_unselect(true);

        let dynamic_factory = gtk::SignalListItemFactory::new();
        dynamic_factory.connect_setup(|_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");
            let expander = gtk::TreeExpander::new();
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .margin_start(8)
                .margin_end(8)
                .margin_top(5)
                .margin_bottom(5)
                .build();
            let icon = gtk::Image::new();
            icon.add_css_class("dim-label");
            let label = gtk::Label::builder()
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            let badge = gtk::Label::builder()
                .css_classes(vec!["badge", "numeric"])
                .halign(gtk::Align::End)
                .build();
            row.append(&icon);
            row.append(&label);
            row.append(&badge);
            expander.set_child(Some(&row));
            list_item.set_child(Some(&expander));
        });
        dynamic_factory.connect_bind(|_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");
            let tree_row = list_item
                .item()
                .and_downcast::<gtk::TreeListRow>()
                .expect("Expected TreeListRow");
            let folder = tree_row
                .item()
                .and_downcast::<FolderObject>()
                .expect("Expected FolderObject");
            let expander = list_item
                .child()
                .and_downcast::<gtk::TreeExpander>()
                .expect("Expected TreeExpander");
            expander.set_list_row(Some(&tree_row));
            let row = expander
                .child()
                .and_downcast::<gtk::Box>()
                .expect("Expected folder row");
            if let Some(icon) = row.first_child().and_downcast::<gtk::Image>() {
                icon.set_icon_name(Some(&folder.icon_name()));
            }
            if let Some(label) = row
                .first_child()
                .and_then(|child| child.next_sibling())
                .and_downcast::<gtk::Label>()
            {
                label.set_label(&folder.name());
                label.set_sensitive(folder.selectable());
            }
            if let Some(badge) = row.last_child().and_downcast::<gtk::Label>() {
                let count = if folder.unread_count() > 0 {
                    folder.unread_count()
                } else {
                    folder.total_count()
                };
                badge.set_label(&count.to_string());
                badge.set_visible(count > 0);
            }
        });
        dynamic_factory.connect_unbind(|_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");
            if let Some(expander) = list_item.child().and_downcast::<gtk::TreeExpander>() {
                expander.set_list_row(None);
            }
        });
        let dynamic_folder_view = gtk::ListView::builder()
            .model(&dynamic_folder_selection)
            .factory(&dynamic_factory)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        self.imp()
            .dynamic_folder_store
            .replace(Some(dynamic_folder_store));
        self.imp()
            .dynamic_folder_selection
            .replace(Some(dynamic_folder_selection.clone()));
        self.imp()
            .dynamic_folder_children
            .replace(Some(Rc::clone(&dynamic_folder_children)));
        self.imp()
            .dynamic_folder_index
            .replace(Some(Rc::clone(&dynamic_folder_index)));

        let page_for_dynamic_selection = self.downgrade();
        dynamic_folder_selection.connect_selected_item_notify(move |selection| {
            let Some(page) = page_for_dynamic_selection.upgrade() else {
                return;
            };
            if page.imp().updating_folder_rows.get() {
                return;
            }
            let Some(row) = selection.selected_item().and_downcast::<gtk::TreeListRow>() else {
                return;
            };
            let Some(folder) = row.item().and_downcast::<FolderObject>() else {
                return;
            };
            if !folder.selectable() {
                return;
            }
            let (Some(source_uid), Some(folder_name)) = (folder.source_uid(), folder.folder_name())
            else {
                return;
            };
            let folder_selection = FolderSelection {
                folder_id: folder.id(),
                source_uid: Some(source_uid.clone()),
                folder_name: Some(folder_name.clone()),
            };
            if let Some(current) = page.imp().folder_selection.borrow().as_ref() {
                current.replace(folder_selection);
            }
            page.start_mail_load(
                MailLoadRequest {
                    source_uid: Some(source_uid),
                    folder_name: Some(folder_name),
                    offset: 0,
                    max_messages_per_folder: camel_mail_service::default_page_limit(),
                },
                true,
            );
        });

        let search_header = gtk::Label::builder()
            .label("Suchordner")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_top(16)
            .margin_bottom(4)
            .build();

        let search_list = gtk::ListBox::new();
        search_list.set_selection_mode(gtk::SelectionMode::Single);
        search_list.add_css_class("navigation-sidebar");
        Self::populate_search_folders(&search_list);

        let folder_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .visible(false)
            .build();
        folder_section.append(&folder_list);
        let dynamic_header = gtk::Label::builder()
            .label("EDS-Ordner")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_top(16)
            .margin_bottom(4)
            .build();
        folder_section.append(&dynamic_header);
        folder_section.append(&dynamic_folder_view);
        folder_section.append(&search_header);
        folder_section.append(&search_list);

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        sidebar_box.append(&account_header);
        let source_state = gtk::Label::builder()
            .label("EDS-Konten werden geladen …")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::Start)
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        sidebar_box.append(&source_state);
        sidebar_box.append(&source_view);
        sidebar_box.append(&folder_section);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&sidebar_box)
            .width_request(260)
            .build();

        let page = adw::NavigationPage::builder()
            .title("Ordner")
            .child(&sidebar_scroll)
            .build();

        (
            page,
            source_store,
            source_state,
            folder_section,
            folder_list,
            search_list,
        )
    }

    fn create_folder_row(
        folder_id: &str,
        icon_name: &str,
        label_text: &str,
        badge_text: &str,
    ) -> gtk::ListBoxRow {
        Self::create_folder_row_with_backend(
            folder_id, icon_name, label_text, badge_text, None, None, false,
        )
    }

    fn create_search_folder_row(
        folder_id: &str,
        icon_name: &str,
        label_text: &str,
        badge_text: &str,
        query: &str,
    ) -> gtk::ListBoxRow {
        let row = Self::create_folder_row(folder_id, icon_name, label_text, badge_text);
        unsafe {
            row.set_data("search-query", query.to_string());
        }
        row
    }

    fn populate_search_folders(search_list: &gtk::ListBox) {
        while let Some(row) = search_list.row_at_index(0) {
            search_list.remove(&row);
        }
        for folder in SearchFolderService::default_vfolders() {
            search_list.append(&Self::create_search_folder_row(
                &folder.id(),
                &folder.icon_name(),
                &folder.name(),
                "",
                &folder.query_expression(),
            ));
        }
        for search in SearchFolderService::load_saved() {
            let row = Self::create_search_folder_row(
                &format!("saved-search:{}", search.id),
                "edit-find-symbolic",
                &search.name,
                "",
                &search.query,
            );
            let edit_button = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Gespeicherten Suchordner bearbeiten")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();
            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Gespeicherten Suchordner löschen")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();
            if let Some(box_container) = row.child().and_downcast::<gtk::Box>() {
                box_container.append(&edit_button);
                box_container.append(&delete_button);
            }
            let search_for_edit = search.clone();
            let search_list_for_edit = search_list.clone();
            edit_button.connect_clicked(move |button| {
                let Some(root) = button.root().and_downcast::<gtk::Window>() else {
                    tracing::warn!("Could not find a parent window for saved search editing");
                    return;
                };
                let dialog = crate::dialogs::SearchFolderDialog::new_for_edit(&search_for_edit);
                dialog.set_transient_for(Some(&root));
                let search_list_for_close = search_list_for_edit.clone();
                dialog.connect_close_request(move |_| {
                    Self::populate_search_folders(&search_list_for_close);
                    glib::Propagation::Proceed
                });
                dialog.present();
            });
            let row_for_delete = row.downgrade();
            let search_list_for_delete = search_list.clone();
            let search_id = search.id.clone();
            delete_button.connect_clicked(move |button| {
                let row_for_action = row_for_delete.clone();
                let search_list_for_action = search_list_for_delete.clone();
                let search_id_for_action = search_id.clone();
                crate::dialogs::run_destructive_action(
                    button,
                    "Gespeicherten Suchordner löschen?",
                    "Die lokale Suchdefinition wird dauerhaft entfernt.",
                    "Löschen",
                    move || match SearchFolderService::delete_saved(&search_id_for_action) {
                        Ok(true) => {
                            if let Some(row) = row_for_action.upgrade() {
                                search_list_for_action.remove(&row);
                            }
                        }
                        Ok(false) => {}
                        Err(error) => tracing::warn!(%error, "Could not delete saved search"),
                    },
                );
            });
            search_list.append(&row);
        }
    }

    fn create_folder_row_with_backend(
        folder_id: &str,
        icon_name: &str,
        label_text: &str,
        badge_text: &str,
        source_uid: Option<&str>,
        folder_name: Option<&str>,
        dynamic: bool,
    ) -> gtk::ListBoxRow {
        let hbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let icon = gtk::Image::from_icon_name(icon_name);
        icon.add_css_class("dim-label");

        let label = gtk::Label::builder()
            .label(label_text)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();

        hbox.append(&icon);
        hbox.append(&label);

        let badge = gtk::Label::builder()
            .label(badge_text)
            .css_classes(vec!["badge", "numeric"])
            .halign(gtk::Align::End)
            .visible(!badge_text.is_empty())
            .build();
        hbox.append(&badge);

        let row = gtk::ListBoxRow::builder().child(&hbox).build();
        unsafe {
            row.set_data("folder-id", folder_id.to_string());
            row.set_data("folder-badge", badge);
            row.set_data(
                "folder-row-data",
                FolderRowData {
                    folder_id: folder_id.to_string(),
                    source_uid: source_uid.map(str::to_string),
                    folder_name: folder_name.map(str::to_string),
                },
            );
            row.set_data("folder-dynamic", dynamic);
        }
        row
    }

    fn update_folder_counts(folder_list: &gtk::ListBox, counts: &HashMap<String, (u32, u32)>) {
        for index in 0..folder_list.observe_children().n_items() {
            let Some(row) = folder_list
                .row_at_index(index as i32)
                .and_downcast::<gtk::ListBoxRow>()
            else {
                continue;
            };
            let Some(folder_id) = Self::folder_id(&row) else {
                continue;
            };
            let key = if folder_id == "spam" {
                "junk"
            } else {
                folder_id.as_str()
            };
            let (unread, total) = if folder_id == "all" {
                counts
                    .values()
                    .fold((0_u32, 0_u32), |(unread, total), value| {
                        (
                            unread.saturating_add(value.0),
                            total.saturating_add(value.1),
                        )
                    })
            } else {
                let Some((unread, total)) = counts.get(key) else {
                    continue;
                };
                (*unread, *total)
            };
            let Some(badge) = (unsafe {
                row.data::<gtk::Label>("folder-badge")
                    .map(|pointer| pointer.as_ref())
            }) else {
                continue;
            };
            badge.set_label(&if unread > 0 {
                format!("{unread}")
            } else {
                format!("{total}")
            });
            badge.set_visible(unread > 0 || total > 0);
        }
    }

    fn rebuild_camel_folder_rows(
        &self,
        folder_list: &gtk::ListBox,
        folders: &[camel_mail_service::CamelFolderSummary],
    ) {
        let was_updating = self.imp().updating_folder_rows.replace(true);
        let mut dynamic_rows = Vec::new();
        for index in 0..folder_list.observe_children().n_items() {
            let Some(row) = folder_list
                .row_at_index(index as i32)
                .and_downcast::<gtk::ListBoxRow>()
            else {
                continue;
            };
            let is_dynamic = unsafe {
                row.data::<bool>("folder-dynamic")
                    .map(|value| *value.as_ref())
                    .unwrap_or(false)
            };
            if is_dynamic {
                dynamic_rows.push(row);
            }
        }
        for row in dynamic_rows {
            folder_list.remove(&row);
        }

        let Some(root_store) = self.imp().dynamic_folder_store.borrow().as_ref().cloned() else {
            self.imp().updating_folder_rows.set(was_updating);
            return;
        };
        let Some(children) = self
            .imp()
            .dynamic_folder_children
            .borrow()
            .as_ref()
            .cloned()
        else {
            self.imp().updating_folder_rows.set(was_updating);
            return;
        };
        let Some(index) = self.imp().dynamic_folder_index.borrow().as_ref().cloned() else {
            self.imp().updating_folder_rows.set(was_updating);
            return;
        };

        let mut nodes = Vec::<DynamicFolderNode>::new();
        for folder in folders {
            let parent_name = Self::parent_folder_name(&folder.folder_name);
            nodes.push(DynamicFolderNode {
                source_uid: folder.source_uid.clone(),
                source_display_name: folder.source_display_name.clone(),
                folder_name: folder.folder_name.clone(),
                display_name: folder.display_name.clone(),
                icon_name: Self::folder_icon_name(&folder.folder_id).to_string(),
                unread_count: folder.unread_count,
                total_count: folder.total_count,
                selectable: folder.is_selectable,
                parent_name,
            });
        }

        let mut known_nodes = nodes
            .iter()
            .map(|node| Self::dynamic_folder_key(&node.source_uid, &node.folder_name))
            .collect::<HashSet<_>>();
        let mut synthetic_nodes = Vec::new();
        for node in &nodes {
            let mut parent_name = node.parent_name.clone();
            while let Some(parent) = parent_name {
                let key = Self::dynamic_folder_key(&node.source_uid, &parent);
                if known_nodes.insert(key) {
                    synthetic_nodes.push(DynamicFolderNode {
                        source_uid: node.source_uid.clone(),
                        source_display_name: node.source_display_name.clone(),
                        folder_name: parent.clone(),
                        display_name: parent
                            .rsplit('/')
                            .next()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(parent.as_str())
                            .to_string(),
                        icon_name: "folder-symbolic".to_string(),
                        unread_count: 0,
                        total_count: 0,
                        selectable: false,
                        parent_name: Self::parent_folder_name(&parent),
                    });
                }
                parent_name = Self::parent_folder_name(&parent);
            }
        }
        nodes.extend(synthetic_nodes);
        nodes.sort_by(|left, right| {
            left.source_display_name
                .cmp(&right.source_display_name)
                .then_with(|| left.folder_name.cmp(&right.folder_name))
        });

        let mut roots = Vec::new();
        let mut child_stores = HashMap::<String, gio::ListStore>::new();
        let mut folder_index = HashMap::<String, FolderObject>::new();
        for node in nodes {
            let id = Self::dynamic_folder_key(&node.source_uid, &node.folder_name);
            let parent_id = node
                .parent_name
                .as_deref()
                .map(|name| Self::dynamic_folder_key(&node.source_uid, name));
            let label = if node.parent_name.is_none() && !node.source_display_name.is_empty() {
                format!("{} · {}", node.source_display_name, node.display_name)
            } else {
                node.display_name.clone()
            };
            let folder = FolderObject::new_backend(
                &id,
                &label,
                &node.icon_name,
                node.unread_count,
                node.total_count,
                &node.source_uid,
                &node.folder_name,
                parent_id.as_deref(),
                node.selectable,
            );
            if let Some(parent_id) = parent_id {
                child_stores
                    .entry(parent_id)
                    .or_insert_with(|| gio::ListStore::new::<FolderObject>())
                    .append(&folder);
            } else {
                roots.push(folder.clone());
            }
            folder_index.insert(id, folder);
        }

        root_store.remove_all();
        children.replace(child_stores);
        index.replace(folder_index);
        for root in roots {
            root_store.append(&root);
        }
        self.imp().updating_folder_rows.set(was_updating);
    }

    fn dynamic_folder_key(source_uid: &str, folder_name: &str) -> String {
        format!("{source_uid}\u{0}{folder_name}")
    }

    fn parent_folder_name(folder_name: &str) -> Option<String> {
        folder_name
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .filter(|parent| !parent.is_empty())
    }

    fn folder_icon_name(folder_id: &str) -> &'static str {
        match folder_id {
            "inbox" => "mail-inbox-symbolic",
            "sent" => "mail-sent-symbolic",
            "drafts" => "mail-drafts-symbolic",
            "trash" => "edit-delete-symbolic",
            "archive" => "mail-archive-symbolic",
            "junk" => "dialog-warning-symbolic",
            _ => "folder-symbolic",
        }
    }

    fn select_folder_row(&self, selection: &FolderSelection) {
        let Some(folder_list) = self.imp().folder_list.borrow().as_ref().cloned() else {
            return;
        };

        self.imp().updating_folder_rows.set(true);
        let mut matching_row = None;
        for index in 0..folder_list.observe_children().n_items() {
            let Some(row) = folder_list
                .row_at_index(index as i32)
                .and_downcast::<gtk::ListBoxRow>()
            else {
                continue;
            };
            if Self::folder_row_data(&row)
                .map(|data| data.selection() == *selection)
                .unwrap_or(false)
            {
                matching_row = Some(row);
                break;
            }
        }
        folder_list.select_row(matching_row.as_ref());
        if let Some(dynamic_selection) = self
            .imp()
            .dynamic_folder_selection
            .borrow()
            .as_ref()
            .cloned()
        {
            if matching_row.is_some() {
                dynamic_selection.set_selected(gtk::INVALID_LIST_POSITION);
            } else {
                for index in 0..dynamic_selection.n_items() {
                    let Some(row) = dynamic_selection
                        .item(index)
                        .and_downcast::<gtk::TreeListRow>()
                    else {
                        continue;
                    };
                    let Some(folder) = row.item().and_downcast::<FolderObject>() else {
                        continue;
                    };
                    let current = FolderSelection {
                        folder_id: folder.id(),
                        source_uid: folder.source_uid(),
                        folder_name: folder.folder_name(),
                    };
                    if current == *selection {
                        dynamic_selection.set_selected(index);
                        break;
                    }
                }
            }
        }
        self.imp().updating_folder_rows.set(false);
    }

    // ── Private: Build Message List with GListStore & Search Filter ───────

    fn build_message_list(
        &self,
    ) -> (
        gtk::Box,
        gtk::Label,
        gio::ListStore,
        gtk::SingleSelection,
        gtk::CustomFilter,
        gtk::CustomFilter,
        Rc<Cell<MessageFilter>>,
        Rc<RefCell<FolderSelection>>,
        Rc<RefCell<MessageSearchQuery>>,
    ) {
        let store = gio::ListStore::new::<MailMessageObject>();

        // Custom filter for real-time search
        let query = Rc::new(RefCell::new(MessageSearchQuery::default()));
        let message_filter = Rc::new(Cell::new(MessageFilter::All));
        let folder_selection = Rc::new(RefCell::new(FolderSelection::all()));
        let query_for_filter = Rc::clone(&query);
        let message_filter_for_filter = Rc::clone(&message_filter);
        let folder_selection_for_filter = Rc::clone(&folder_selection);
        let custom_filter = gtk::CustomFilter::new(move |obj| {
            let Some(msg) = obj.downcast_ref::<MailMessageObject>() else {
                return false;
            };

            let query = query_for_filter.borrow();
            let selected_folder = folder_selection_for_filter.borrow();
            message_matches_filter(
                msg,
                &query,
                message_filter_for_filter.get(),
                &selected_folder,
            )
        });
        let filter_model =
            gtk::FilterListModel::new(Some(store.clone()), Some(custom_filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filter_model));

        let thread_root_store = gio::ListStore::new::<MailMessageObject>();
        let thread_members = Rc::new(RefCell::new(
            HashMap::<String, Vec<MailMessageObject>>::new(),
        ));
        let query_for_thread_filter = Rc::clone(&query);
        let message_filter_for_thread_filter = Rc::clone(&message_filter);
        let folder_selection_for_thread_filter = Rc::clone(&folder_selection);
        let thread_members_for_filter = Rc::clone(&thread_members);
        let thread_filter = gtk::CustomFilter::new(move |obj| {
            let Some(root) = obj.downcast_ref::<MailMessageObject>() else {
                return false;
            };
            let query = query_for_thread_filter.borrow();
            let selected_folder = folder_selection_for_thread_filter.borrow();
            let members = thread_members_for_filter.borrow();
            members
                .get(&root.thread_id())
                .map(|messages| {
                    messages.iter().any(|message| {
                        message_matches_filter(
                            message,
                            &query,
                            message_filter_for_thread_filter.get(),
                            &selected_folder,
                        )
                    })
                })
                .unwrap_or_else(|| {
                    message_matches_filter(
                        root,
                        &query,
                        message_filter_for_thread_filter.get(),
                        &selected_folder,
                    )
                })
        });
        let thread_children = Rc::new(RefCell::new(HashMap::<String, gio::ListStore>::new()));
        let thread_children_for_model = Rc::clone(&thread_children);
        let thread_root_filter_model =
            gtk::FilterListModel::new(Some(thread_root_store.clone()), Some(thread_filter.clone()));
        let thread_tree_model =
            gtk::TreeListModel::new(thread_root_filter_model, false, false, move |item| {
                let message = item.downcast_ref::<MailMessageObject>()?;
                thread_children_for_model
                    .borrow()
                    .get(&message.thread_id())
                    .cloned()
                    .map(|store| store.upcast::<gio::ListModel>())
            });
        let thread_selection = gtk::SingleSelection::new(Some(thread_tree_model));

        self.imp()
            .thread_root_store
            .replace(Some(thread_root_store.clone()));
        self.imp()
            .thread_members
            .replace(Some(Rc::clone(&thread_members)));
        self.imp()
            .thread_children
            .replace(Some(Rc::clone(&thread_children)));
        self.imp()
            .thread_selection
            .replace(Some(thread_selection.clone()));

        let flat_selection_for_tree = selection.clone();
        let page_for_tree_selection = self.downgrade();
        thread_selection.connect_selected_item_notify(move |selection| {
            if page_for_tree_selection
                .upgrade()
                .map(|page| page.imp().thread_selection_syncing.get())
                .unwrap_or(false)
            {
                return;
            }
            let Some(row) = selection.selected_item().and_downcast::<gtk::TreeListRow>() else {
                return;
            };
            let Some(message) = row.item().and_downcast::<MailMessageObject>() else {
                return;
            };
            for index in 0..flat_selection_for_tree.n_items() {
                let Some(candidate) = flat_selection_for_tree
                    .item(index)
                    .and_downcast::<MailMessageObject>()
                else {
                    continue;
                };
                if candidate.id() == message.id() {
                    flat_selection_for_tree.set_selected(index);
                    break;
                }
            }
        });

        let factory = gtk::SignalListItemFactory::new();

        let page_for_batch = self.downgrade();
        factory.connect_setup(move |_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .margin_start(12)
                .margin_end(12)
                .margin_top(8)
                .margin_bottom(8)
                .build();

            let header_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();

            let batch_check = gtk::CheckButton::builder()
                .valign(gtk::Align::Center)
                .tooltip_text("Nachricht für eine Mehrfachaktion auswählen")
                .build();
            let list_item_for_batch = list_item.downgrade();
            let page_for_batch = page_for_batch.clone();
            batch_check.connect_toggled(move |check| {
                let Some(page) = page_for_batch.upgrade() else {
                    return;
                };
                if page.imp().batch_binding.get() {
                    return;
                }
                let Some(list_item) = list_item_for_batch.upgrade() else {
                    return;
                };
                let Some(row) = list_item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                let Some(message) = row.item().and_downcast::<MailMessageObject>() else {
                    return;
                };
                page.set_batch_message_selected(&message.id(), check.is_active());
            });

            let avatar = adw::Avatar::builder().size(32).show_initials(true).build();

            let sender_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .hexpand(true)
                .build();

            let sender = gtk::Label::builder()
                .css_classes(vec!["heading"])
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .build();

            let date = gtk::Label::builder()
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::End)
                .build();

            let btn_star = gtk::Button::builder()
                .icon_name("non-starred-symbolic")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .tooltip_text("Markieren / Sternchhen setzen")
                .build();

            sender_box.append(&sender);
            header_box.append(&batch_check);
            header_box.append(&avatar);
            header_box.append(&sender_box);
            header_box.append(&date);
            header_box.append(&btn_star);

            let subject = gtk::Label::builder()
                .css_classes(vec!["body"])
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .build();

            let preview = gtk::Label::builder()
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(60)
                .build();

            vbox.append(&header_box);
            vbox.append(&subject);
            vbox.append(&preview);

            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            separator.set_margin_top(4);

            let outer = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            outer.append(&vbox);
            outer.append(&separator);

            let expander = gtk::TreeExpander::new();
            expander.set_child(Some(&outer));
            list_item.set_child(Some(&expander));
        });

        let filter_for_bind = custom_filter.clone();
        let thread_filter_for_bind = thread_filter.clone();
        let page_for_bind = self.downgrade();
        factory.connect_bind(move |_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");

            let tree_row = list_item
                .item()
                .and_downcast::<gtk::TreeListRow>()
                .expect("Expected TreeListRow");
            let msg = tree_row
                .item()
                .and_downcast::<MailMessageObject>()
                .expect("Expected MailMessageObject");

            let expander = list_item
                .child()
                .and_downcast::<gtk::TreeExpander>()
                .expect("Expected TreeExpander");
            expander.set_list_row(Some(&tree_row));
            let outer = expander
                .child()
                .and_downcast::<gtk::Box>()
                .expect("Expected message Box");

            let vbox = outer
                .first_child()
                .and_downcast::<gtk::Box>()
                .expect("Expected inner Box");

            let density = AppSettings::try_new()
                .map(|settings| settings.mail_row_density())
                .unwrap_or_else(|| "comfortable".to_string());
            let (row_spacing, row_margin) = mail_row_density_metrics(&density);
            vbox.set_spacing(row_spacing);
            vbox.set_margin_top(row_margin);
            vbox.set_margin_bottom(row_margin);

            let settings = AppSettings::try_new();
            let show_avatar = settings
                .as_ref()
                .map(AppSettings::mail_show_avatar)
                .unwrap_or(true);
            let show_sender = settings
                .as_ref()
                .map(AppSettings::mail_show_sender)
                .unwrap_or(true);
            let show_date = settings
                .as_ref()
                .map(AppSettings::mail_show_date)
                .unwrap_or(true);
            let show_preview_text = settings
                .as_ref()
                .map(AppSettings::mail_show_preview_text)
                .unwrap_or(true);
            let show_labels = settings
                .as_ref()
                .map(AppSettings::mail_show_labels)
                .unwrap_or(true);

            let header_box = vbox
                .first_child()
                .and_downcast::<gtk::Box>()
                .expect("Expected header Box");

            if let Some(page) = page_for_bind.upgrade() {
                if let Some(batch_check) = header_box
                    .first_child()
                    .and_downcast::<gtk::CheckButton>()
                {
                    page.imp().batch_binding.set(true);
                    batch_check.set_active(
                        page.imp()
                            .batch_selection
                            .borrow()
                            .contains(&msg.id()),
                    );
                    page.imp().batch_binding.set(false);
                }
            }

            if let Some(avatar) = header_box
                .first_child()
                .and_then(|child| child.next_sibling())
                .and_downcast::<adw::Avatar>()
            {
                avatar.set_visible(show_avatar);
                avatar.set_text(Some(&msg.sender_name()));
            }

            if let Some(sender_box) = header_box
                .first_child()
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.next_sibling())
                .and_downcast::<gtk::Box>()
            {
                sender_box.set_visible(show_sender);
                if let Some(sender) = sender_box.first_child().and_downcast::<gtk::Label>() {
                    sender.set_label(&msg.sender_name());
                    if msg.is_unread() {
                        sender.add_css_class("title-4");
                    } else {
                        sender.remove_css_class("title-4");
                    }
                }
            }

            if let Some(date) = header_box
                .first_child()
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.next_sibling())
                .and_downcast::<gtk::Label>()
            {
                date.set_visible(show_date);
                date.set_label(&format_message_date(&msg.date_str()));
            }

            if let Some(btn_star) = header_box.last_child().and_downcast::<gtk::Button>() {
                if msg.is_flagged() {
                    btn_star.set_icon_name("starred-symbolic");
                } else {
                    btn_star.set_icon_name("non-starred-symbolic");
                }
                let msg_clone = msg.clone();
                let filter_for_star = filter_for_bind.clone();
                let thread_filter_for_star = thread_filter_for_bind.clone();
                let page_for_star = page_for_bind.clone();
                btn_star.connect_clicked(move |btn| {
                    let current = msg_clone.is_flagged();
                    let next_flagged = !current;
                    let command = MailCommand::set_message_state(
                        msg_clone.id(),
                        msg_clone.backend_location(),
                        msg_clone.is_unread(),
                        current,
                        msg_clone.is_unread(),
                        next_flagged,
                    );
                    let result = page_for_star
                        .upgrade()
                        .map(|page| page.execute_mail_command(command))
                        .unwrap_or_else(|| {
                            Err(MailOperationError::Backend {
                                operation:
                                    crate::services::mail_operations_service::MailOperationKind::SetMessageState,
                                message: "Mailansicht ist nicht mehr verfügbar".to_string(),
                            })
                        });
                    if let Err(error) = result {
                        tracing::error!(%error, "Could not persist message flag");
                        return;
                    }
                    msg_clone.set_flagged(next_flagged);
                    filter_for_star.changed(gtk::FilterChange::Different);
                    thread_filter_for_star.changed(gtk::FilterChange::Different);
                    if next_flagged {
                        btn.set_icon_name("starred-symbolic");
                    } else {
                        btn.set_icon_name("non-starred-symbolic");
                    }
                });
            }

            if let Some(subject) = vbox
                .first_child()
                .and_then(|w| w.next_sibling())
                .and_downcast::<gtk::Label>()
            {
                let mut subject_text = if tree_row.depth() == 0 && msg.thread_size() > 1 {
                    format!("{}  ·  {} Nachrichten", msg.subject(), msg.thread_size())
                } else {
                    msg.subject()
                };
                let labels = msg.labels();
                if show_labels && !labels.is_empty() {
                    subject_text.push_str(&format!("  ·  [{}]", labels.join(", ")));
                }
                subject.set_label(&subject_text);
            }

            if let Some(preview) = vbox.last_child().and_downcast::<gtk::Label>() {
                preview.set_visible(show_preview_text);
                preview.set_label(&msg.preview_snippet());
            }
        });

        factory.connect_unbind(move |_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");
            if let Some(expander) = list_item.child().and_downcast::<gtk::TreeExpander>() {
                expander.set_list_row(None);
            }
        });

        let list_view = gtk::ListView::builder()
            .model(&thread_selection)
            .factory(&factory)
            .css_classes(vec!["message-list"])
            .build();

        // Search entry container
        let search_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .spacing(6)
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Mails durchsuchen (from:, in:sent, on:2026-08-11, is:unread)…")
            .hexpand(true)
            .build();

        let save_search_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Aktuelle Suche als Suchordner speichern")
            .css_classes(vec!["flat", "circular"])
            .build();
        let page_for_save_search = self.downgrade();
        let search_entry_for_save = search_entry.clone();
        save_search_button.connect_clicked(move |_| {
            let Some(page) = page_for_save_search.upgrade() else {
                return;
            };
            let query = search_entry_for_save.text().trim().to_string();
            if query.is_empty() {
                page.show_toast("Eine leere Suche kann nicht gespeichert werden");
                return;
            }
            let dialog = crate::dialogs::SearchFolderDialog::new(&query);
            if let Some(root) = page.root().and_downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&root));
            }
            let page_for_close = page.downgrade();
            dialog.connect_close_request(move |_| {
                if let Some(page) = page_for_close.upgrade() {
                    page.refresh_search_folders();
                }
                glib::Propagation::Proceed
            });
            dialog.present();
        });

        let query_for_search = Rc::clone(&query);
        let filter_for_search = custom_filter.clone();
        let thread_filter_for_search = thread_filter.clone();
        search_entry.connect_search_changed(move |entry| {
            *query_for_search.borrow_mut() = MessageSearchQuery::parse(entry.text().as_str());
            filter_for_search.changed(gtk::FilterChange::Different);
            thread_filter_for_search.changed(gtk::FilterChange::Different);
        });

        let sort_model = gtk::StringList::new(&[
            "Datum",
            "Betreff",
            "Absender",
            "Ungelesen-Status",
            "Markierung",
            "Anhänge",
        ]);
        let sort_selector = gtk::DropDown::new(Some(sort_model), None::<&gtk::Expression>);
        sort_selector.set_tooltip_text(Some("Mails sortieren"));
        sort_selector.set_selected(match self.imp().mail_sort_field.get() {
            MailSortField::Subject => 1,
            MailSortField::Sender => 2,
            MailSortField::Unread => 3,
            MailSortField::Flagged => 4,
            MailSortField::Attachments => 5,
            MailSortField::Date => 0,
        });
        let direction_button = gtk::Button::builder()
            .css_classes(vec!["flat", "circular"])
            .build();
        let set_direction_button = |button: &gtk::Button, descending: bool| {
            button.set_icon_name(if descending {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            button.set_tooltip_text(Some(if descending {
                "Absteigend sortiert"
            } else {
                "Aufsteigend sortiert"
            }));
        };
        set_direction_button(&direction_button, self.imp().mail_sort_descending.get());

        let page_for_sort = self.downgrade();
        sort_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_for_sort.upgrade() else {
                return;
            };
            let field = match selector.selected() {
                1 => MailSortField::Subject,
                2 => MailSortField::Sender,
                3 => MailSortField::Unread,
                4 => MailSortField::Flagged,
                5 => MailSortField::Attachments,
                _ => MailSortField::Date,
            };
            page.imp().mail_sort_field.set(field);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_sort_field(field.setting_value());
            }
            if let Some(store) = page.imp().message_store.borrow().as_ref().cloned() {
                page.refresh_thread_metadata(&store);
            }
        });

        let page_for_direction = self.downgrade();
        let direction_button_for_signal = direction_button.clone();
        direction_button.connect_clicked(move |_| {
            let Some(page) = page_for_direction.upgrade() else {
                return;
            };
            let descending = !page.imp().mail_sort_descending.get();
            page.imp().mail_sort_descending.set(descending);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_sort_descending(descending);
            }
            set_direction_button(&direction_button_for_signal, descending);
            if let Some(store) = page.imp().message_store.borrow().as_ref().cloned() {
                page.refresh_thread_metadata(&store);
            }
        });

        search_box.append(&search_entry);
        search_box.append(&sort_selector);
        search_box.append(&direction_button);
        search_box.append(&save_search_button);
        self.imp().search_entry.replace(Some(search_entry.clone()));

        let list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        let message_state = gtk::Label::builder()
            .label("Nachrichten werden geladen …")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .wrap(true)
            .build();

        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Nachrichten aktualisieren (F5)")
            .css_classes(vec!["flat", "circular"])
            .build();
        let page_for_refresh = self.downgrade();
        refresh_button.connect_clicked(move |_| {
            if let Some(page) = page_for_refresh.upgrade() {
                page.refresh_mail();
            }
        });

        let load_more_button = gtk::Button::builder()
            .label("Mehr laden")
            .tooltip_text("Weitere Nachrichten laden")
            .css_classes(vec!["flat"])
            .visible(false)
            .build();
        let page_for_more = self.downgrade();
        load_more_button.connect_clicked(move |_| {
            if let Some(page) = page_for_more.upgrade() {
                page.load_more_mail();
            }
        });

        let message_state_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        message_state.set_hexpand(true);
        message_state_box.append(&message_state);
        message_state_box.append(&load_more_button);
        message_state_box.append(&refresh_button);

        let batch_action_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(6)
            .visible(false)
            .build();
        let batch_action_label = gtk::Label::builder()
            .css_classes(vec!["caption", "dim-label"])
            .hexpand(true)
            .halign(gtk::Align::Start)
            .build();
        batch_action_bar.append(&batch_action_label);

        let batch_read_button = gtk::Button::builder()
            .label("Gelesen")
            .tooltip_text("Ausgewählte Nachrichten als gelesen markieren")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_read = self.downgrade();
        batch_read_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_read.upgrade() {
                if let Err(error) = page.mark_batch_read() {
                    page.report_operation_error("Gelesen-Markierung nicht möglich", &error);
                    tracing::warn!(%error, "Batch read operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_read_button);

        let batch_archive_button = gtk::Button::builder()
            .label("Archivieren")
            .tooltip_text("Ausgewählte Nachrichten archivieren")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_archive = self.downgrade();
        batch_archive_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_archive.upgrade() {
                if let Err(error) = page.archive_batch() {
                    page.report_operation_error("Archivieren nicht möglich", &error);
                    tracing::warn!(%error, "Batch archive operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_archive_button);

        let batch_junk_button = gtk::Button::builder()
            .label("Spam")
            .tooltip_text("Ausgewählte Nachrichten als Spam markieren")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_junk = self.downgrade();
        batch_junk_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_junk.upgrade() {
                if let Err(error) = page.mark_batch_junk() {
                    page.report_operation_error("Spam-Markierung nicht möglich", &error);
                    tracing::warn!(%error, "Batch junk operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_junk_button);

        let move_popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let move_popover = gtk::Popover::builder().child(&move_popover_content).build();
        let page_for_move_menu = self.downgrade();
        let move_popover_weak = move_popover.downgrade();
        move_popover.connect_show(move |_| {
            while let Some(child) = move_popover_content.first_child() {
                move_popover_content.remove(&child);
            }
            let Some(page) = page_for_move_menu.upgrade() else {
                return;
            };
            let destinations = page.folder_destinations();
            if destinations.is_empty() {
                move_popover_content.append(
                    &gtk::Label::builder()
                        .label("Keine verfügbaren Zielordner")
                        .css_classes(vec!["caption", "dim-label"])
                        .build(),
                );
                return;
            }
            for destination in destinations {
                let button = gtk::Button::builder()
                    .label(&destination.label)
                    .halign(gtk::Align::Fill)
                    .css_classes(vec!["flat"])
                    .build();
                let page_for_destination = page.downgrade();
                let popover_for_destination = move_popover_weak.clone();
                button.connect_clicked(move |_| {
                    if let Some(page) = page_for_destination.upgrade() {
                        page.move_batch_to(&destination.source_uid, &destination.folder_name);
                    }
                    if let Some(popover) = popover_for_destination.upgrade() {
                        popover.popdown();
                    }
                });
                move_popover_content.append(&button);
            }
        });
        let move_button = gtk::MenuButton::builder()
            .label("Verschieben")
            .tooltip_text("Ausgewählte Nachrichten in einen EDS- oder lokalen Ordner verschieben")
            .popover(&move_popover)
            .css_classes(vec!["flat"])
            .build();
        batch_action_bar.append(&move_button);

        let batch_delete_button = gtk::Button::builder()
            .label("Löschen")
            .tooltip_text("Ausgewählte Nachrichten löschen")
            .css_classes(vec!["flat", "destructive-action"])
            .build();
        let page_for_batch_delete = self.downgrade();
        batch_delete_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_delete.upgrade() {
                let page_for_action = page.clone();
                crate::dialogs::run_destructive_action(
                    &page,
                    "Nachrichten löschen?",
                    "Die ausgewählten Nachrichten werden dauerhaft gelöscht bzw. in den Papierkorb verschoben.",
                    "Löschen",
                    move || {
                        if let Err(error) = page_for_action.delete_batch() {
                            page_for_action.report_operation_error("Löschen nicht möglich", &error);
                            tracing::warn!(%error, "Batch delete operation was not applied");
                        }
                    },
                );
            }
        });
        batch_action_bar.append(&batch_delete_button);

        list_container.append(&message_state_box);
        list_container.append(&batch_action_bar);
        list_container.append(&search_box);
        list_container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&list_view)
            .vexpand(true)
            .width_request(340)
            .build();

        list_container.append(&list_scroll);

        self.imp().refresh_button.replace(Some(refresh_button));
        self.imp().load_more_button.replace(Some(load_more_button));
        self.imp().batch_action_bar.replace(Some(batch_action_bar));
        self.imp()
            .batch_action_label
            .replace(Some(batch_action_label));

        (
            list_container,
            message_state,
            store,
            selection,
            custom_filter,
            thread_filter,
            message_filter,
            folder_selection,
            query,
        )
    }

    fn connect_folder_filters(
        &self,
        folder_list: &gtk::ListBox,
        search_list: &gtk::ListBox,
        custom_filter: &gtk::CustomFilter,
        thread_filter: &gtk::CustomFilter,
        folder_selection: &Rc<RefCell<FolderSelection>>,
        message_filter: &Rc<Cell<MessageFilter>>,
        query: &Rc<RefCell<MessageSearchQuery>>,
    ) {
        let folder_selection_for_rows = Rc::clone(folder_selection);
        let message_filter_for_rows = Rc::clone(message_filter);
        let custom_filter_for_rows = custom_filter.clone();
        let thread_filter_for_rows = thread_filter.clone();
        let search_list_for_rows = search_list.clone();
        let query_for_folder = Rc::clone(query);
        let page_for_rows = self.downgrade();
        folder_list.connect_row_selected(move |_, row| {
            if page_for_rows
                .upgrade()
                .map(|page| page.imp().updating_folder_rows.get())
                .unwrap_or(false)
            {
                return;
            }

            let selection = row
                .and_then(Self::folder_row_data)
                .map(|data| data.selection())
                .unwrap_or_default();
            folder_selection_for_rows.replace(selection.clone());
            message_filter_for_rows.set(MessageFilter::All);
            query_for_folder.replace(MessageSearchQuery::default());
            if row.is_some() {
                search_list_for_rows.unselect_all();
                if let Some(page) = page_for_rows.upgrade() {
                    if let Some(search_entry) = page.imp().search_entry.borrow().as_ref() {
                        search_entry.set_text("");
                    }
                }
            }
            custom_filter_for_rows.changed(gtk::FilterChange::Different);
            thread_filter_for_rows.changed(gtk::FilterChange::Different);

            if let Some(page) = page_for_rows.upgrade() {
                if selection.is_remote() {
                    page.start_mail_load(
                        MailLoadRequest {
                            source_uid: selection.source_uid.clone(),
                            folder_name: selection.folder_name.clone(),
                            offset: 0,
                            max_messages_per_folder: camel_mail_service::default_page_limit(),
                        },
                        true,
                    );
                } else {
                    let needs_all_mail_reload = page
                        .imp()
                        .active_request
                        .borrow()
                        .as_ref()
                        .map(|request| {
                            request.source_uid.is_some() || request.folder_name.is_some()
                        })
                        .unwrap_or(false);
                    if needs_all_mail_reload {
                        page.start_mail_load(MailLoadRequest::initial(), true);
                    }
                }
            }
        });

        let message_filter = Rc::clone(message_filter);
        let folder_selection = Rc::clone(folder_selection);
        let custom_filter = custom_filter.clone();
        let thread_filter = thread_filter.clone();
        let query = Rc::clone(query);
        let folder_list_for_search = folder_list.clone();
        let page_for_search = self.downgrade();
        search_list.connect_row_selected(move |_, row| {
            let folder_id = row.and_then(Self::folder_id).unwrap_or_default();
            let filter = match folder_id.as_str() {
                "vfolder-unread" => MessageFilter::Unread,
                "vfolder-starred" => MessageFilter::Flagged,
                _ => MessageFilter::All,
            };
            message_filter.set(filter);
            if row.is_some() {
                let saved_query = unsafe {
                    row.and_then(|row| {
                        row.data::<String>("search-query")
                            .map(|value| value.as_ref().clone())
                    })
                };
                query.replace(
                    saved_query
                        .as_deref()
                        .map(MessageSearchQuery::parse)
                        .unwrap_or_default(),
                );
                folder_selection.replace(FolderSelection::all());
                folder_list_for_search.unselect_all();
                if let Some(page) = page_for_search.upgrade() {
                    if let (Some(search_entry), Some(saved_query)) = (
                        page.imp().search_entry.borrow().as_ref(),
                        saved_query.as_deref(),
                    ) {
                        search_entry.set_text(saved_query);
                    }
                    let needs_all_mail_reload = page
                        .imp()
                        .active_request
                        .borrow()
                        .as_ref()
                        .map(|request| {
                            request.source_uid.is_some() || request.folder_name.is_some()
                        })
                        .unwrap_or(false);
                    if needs_all_mail_reload {
                        page.start_mail_load(MailLoadRequest::initial(), true);
                    }
                }
            }
            custom_filter.changed(gtk::FilterChange::Different);
            thread_filter.changed(gtk::FilterChange::Different);
        });
    }

    fn folder_id(row: &gtk::ListBoxRow) -> Option<String> {
        unsafe {
            row.data::<FolderRowData>("folder-row-data")
                .map(|value| value.as_ref().folder_id.clone())
        }
    }

    fn folder_row_data(row: &gtk::ListBoxRow) -> Option<FolderRowData> {
        unsafe {
            row.data::<FolderRowData>("folder-row-data")
                .map(|value| value.as_ref().clone())
        }
    }

    // ── Private: Build Detail View ──────────────────────────────────────

    #[allow(clippy::type_complexity)]
    fn build_detail_view(
        &self,
    ) -> (
        gtk::Box,
        MailViewer,
        gtk::Label,
        gtk::Label,
        gtk::Label,
        gtk::Label,
        gtk::Label,
        gtk::Label,
        gtk::Box,
        gtk::ListBox,
    ) {
        let detail_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(12)
            .build();

        let lbl_subject = gtk::Label::builder()
            .label("E-Mail auswählen")
            .css_classes(vec!["title-2"])
            .halign(gtk::Align::Start)
            .wrap(true)
            .build();

        let meta_grid = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(12)
            .margin_top(8)
            .build();

        let from_label = gtk::Label::builder()
            .label("Von:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let lbl_from = gtk::Label::builder()
            .label("-")
            .halign(gtk::Align::Start)
            .selectable(true)
            .build();

        let to_label = gtk::Label::builder()
            .label("An:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let lbl_to = gtk::Label::builder()
            .label("-")
            .halign(gtk::Align::Start)
            .selectable(true)
            .build();

        let cc_label = gtk::Label::builder()
            .label("Cc:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let lbl_cc = gtk::Label::builder()
            .label("-")
            .halign(gtk::Align::Start)
            .selectable(true)
            .build();

        let date_label = gtk::Label::builder()
            .label("Datum:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let lbl_date = gtk::Label::builder()
            .label("-")
            .halign(gtk::Align::Start)
            .build();

        let crypto_label = gtk::Label::builder()
            .label("Sicherheit:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();

        let crypto_chip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .css_classes(vec!["badge"])
            .halign(gtk::Align::Start)
            .build();
        let icon_lock = gtk::Image::from_icon_name("dialog-information-symbolic");
        let lbl_crypto = gtk::Label::builder()
            .label("Keine OpenPGP-/S/MIME-Sicherung erkannt")
            .css_classes(vec!["caption"])
            .build();
        crypto_chip.append(&icon_lock);
        crypto_chip.append(&lbl_crypto);

        meta_grid.attach(&from_label, 0, 0, 1, 1);
        meta_grid.attach(&lbl_from, 1, 0, 1, 1);
        meta_grid.attach(&to_label, 0, 1, 1, 1);
        meta_grid.attach(&lbl_to, 1, 1, 1, 1);
        meta_grid.attach(&cc_label, 0, 2, 1, 1);
        meta_grid.attach(&lbl_cc, 1, 2, 1, 1);
        meta_grid.attach(&date_label, 0, 3, 1, 1);
        meta_grid.attach(&lbl_date, 1, 3, 1, 1);
        meta_grid.attach(&crypto_label, 0, 4, 1, 1);
        meta_grid.attach(&crypto_chip, 1, 4, 1, 1);

        let action_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(12)
            .build();

        let reply_btn = gtk::Button::builder()
            .icon_name("mail-reply-sender-symbolic")
            .tooltip_text("Antworten (Ctrl+R)")
            .css_classes(vec!["flat"])
            .build();

        let reply_all_btn = gtk::Button::builder()
            .icon_name("mail-reply-all-symbolic")
            .tooltip_text("Allen antworten (Ctrl+Shift+R)")
            .css_classes(vec!["flat"])
            .build();

        let forward_btn = gtk::Button::builder()
            .icon_name("mail-forward-symbolic")
            .tooltip_text("Weiterleiten (Ctrl+L)")
            .css_classes(vec!["flat"])
            .build();

        let export_btn = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("E-Mail als EML exportieren")
            .css_classes(vec!["flat"])
            .build();

        let delete_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Löschen (Delete)")
            .action_name("win.delete")
            .css_classes(vec!["flat"])
            .build();

        let unread_btn = gtk::Button::builder()
            .icon_name("mail-unread-symbolic")
            .tooltip_text("Als ungelesen markieren")
            .action_name("win.mark-unread")
            .css_classes(vec!["flat"])
            .build();

        reply_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                if !page.reply_selected() {
                    tracing::warn!("Reply requested without a selected message");
                }
            }
        ));

        reply_all_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                if !page.reply_all_selected() {
                    tracing::warn!("Reply-all requested without a selected message");
                }
            }
        ));

        forward_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                if !page.forward_selected() {
                    tracing::warn!("Forward requested without a selected message");
                }
            }
        ));

        export_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                if !page.export_selected() {
                    tracing::warn!("E-Mail export requested without a selected message");
                }
            }
        ));

        action_bar.append(&reply_btn);
        action_bar.append(&reply_all_btn);
        action_bar.append(&forward_btn);
        action_bar.append(&export_btn);

        let spacer = gtk::Box::builder().hexpand(true).build();
        action_bar.append(&spacer);
        action_bar.append(&unread_btn);
        action_bar.append(&delete_btn);

        header_box.append(&lbl_subject);
        header_box.append(&meta_grid);
        header_box.append(&action_bar);

        let header_separator = gtk::Separator::new(gtk::Orientation::Horizontal);

        let thread_context = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["thread-context"])
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .visible(false)
            .build();

        let mail_viewer = MailViewer::new();

        let attachment_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(16)
            .margin_end(16)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let attachment_separator = gtk::Separator::new(gtk::Orientation::Horizontal);

        mail_viewer.set_vexpand(true);
        detail_view.set_vexpand(true);

        detail_view.append(&header_box);
        detail_view.append(&header_separator);
        detail_view.append(&thread_context);
        detail_view.append(&mail_viewer);
        detail_view.append(&attachment_separator);
        detail_view.append(&attachment_bar);

        (
            detail_view,
            mail_viewer,
            lbl_subject,
            lbl_from,
            lbl_to,
            lbl_cc,
            lbl_date,
            lbl_crypto,
            attachment_bar,
            thread_context,
        )
    }

    // ── Private: Connect Selection Changed Handler ──────────────────────

    #[allow(clippy::too_many_arguments)]
    fn connect_selection_handler(
        &self,
        selection: &gtk::SingleSelection,
        store: &gio::ListStore,
        custom_filter: &gtk::CustomFilter,
        thread_filter: &gtk::CustomFilter,
        mail_viewer: &MailViewer,
        lbl_subject: &gtk::Label,
        lbl_from: &gtk::Label,
        lbl_to: &gtk::Label,
        lbl_cc: &gtk::Label,
        lbl_date: &gtk::Label,
        lbl_crypto: &gtk::Label,
        attachment_bar: &gtk::Box,
        thread_context: &gtk::ListBox,
    ) {
        let operations = MailOperationsService::new();
        let filter_for_selection = custom_filter.clone();
        let thread_filter_for_selection = thread_filter.clone();
        let store_for_thread_context = store.clone();
        let selection_for_thread_context = selection.clone();
        selection.connect_selected_item_notify(glib::clone!(
            #[strong]
            operations,
            #[strong]
            filter_for_selection,
            #[weak]
            mail_viewer,
            #[weak]
            lbl_subject,
            #[weak]
            lbl_from,
            #[weak]
            lbl_to,
            #[weak]
            lbl_cc,
            #[weak]
            lbl_date,
            #[weak]
            lbl_crypto,
            #[weak]
            attachment_bar,
            #[strong]
            store_for_thread_context,
            #[strong]
            selection_for_thread_context,
            #[weak]
            thread_context,
            move |sel| {
                if let Some(item) = sel.selected_item() {
                    if let Ok(msg) = item.downcast::<MailMessageObject>() {
                        let was_unread = msg.is_unread();
                        let mark_read_on_selection = AppSettings::try_new()
                            .map(|settings| settings.mark_read_on_selection())
                            .unwrap_or(true);
                        if was_unread && mark_read_on_selection {
                            let result = match msg.backend_location() {
                                Some(location) => operations.mark_read_for_location(
                                    &msg.id(),
                                    &location,
                                    msg.is_flagged(),
                                ),
                                None => operations.mark_read(&msg.id(), msg.is_flagged()),
                            };
                            if let Err(error) = result {
                                tracing::error!(%error, "Could not persist read state");
                            } else {
                                msg.set_unread(false);
                                filter_for_selection.changed(gtk::FilterChange::Different);
                                thread_filter_for_selection.changed(gtk::FilterChange::Different);
                            }
                        }
                        lbl_subject.set_label(&msg.subject());
                        lbl_from.set_label(&format!(
                            "{} <{}>",
                            msg.sender_name(),
                            msg.sender_email()
                        ));
                        lbl_to.set_label(&msg.recipient_to());
                        let recipient_cc = msg.recipient_cc();
                        lbl_cc.set_label(if recipient_cc.trim().is_empty() {
                            "-"
                        } else {
                            &recipient_cc
                        });
                        lbl_date.set_label(&format_message_date(&msg.date_str()));

                        Self::render_thread_context(
                            &store_for_thread_context,
                            &selection_for_thread_context,
                            &msg,
                            &thread_context,
                        );

                        while let Some(child) = attachment_bar.first_child() {
                            attachment_bar.remove(&child);
                        }

                        let attachments = msg.attachments();
                        let mut crypto_probe = msg.html_body().into_bytes();
                        for attachment in &attachments {
                            crypto_probe.extend_from_slice(b"\n");
                            crypto_probe.extend_from_slice(attachment.mime_type.as_bytes());
                            crypto_probe.extend_from_slice(b"\n");
                            crypto_probe.extend_from_slice(&attachment.content);
                        }
                        let crypto_details = CryptoService::verify_signature(&crypto_probe);
                        lbl_crypto.set_label(&crypto_details.display_text());
                        for attachment in &attachments {
                            let looks_like_calendar =
                                attachment.mime_type.split(';').next().is_some_and(|mime| {
                                    mime.trim().eq_ignore_ascii_case("text/calendar")
                                }) || attachment.filename.to_ascii_lowercase().ends_with(".ics");
                            if looks_like_calendar {
                                if let Some(invitation) =
                                    parse_invitation(&String::from_utf8_lossy(&attachment.content))
                                {
                                    append_itip_invitation(&attachment_bar, invitation);
                                }
                            }
                        }
                        let inline_images = attachments
                            .iter()
                            .filter_map(|attachment| {
                                attachment.content_id.as_ref().map(|content_id| {
                                    InlineImage::new(
                                        content_id.clone(),
                                        attachment.mime_type.clone(),
                                        attachment.content.clone(),
                                    )
                                })
                            })
                            .collect::<Vec<_>>();
                        let rendered_html = resolve_cid_images(&msg.html_body(), &inline_images);
                        mail_viewer.load_html_for_message(Some(&msg.id()), &rendered_html, None);
                        if attachments.is_empty() {
                            attachment_bar.set_visible(false);
                        } else {
                            attachment_bar.set_visible(true);
                            for att in attachments {
                                let chip = gtk::Box::builder()
                                    .orientation(gtk::Orientation::Horizontal)
                                    .spacing(6)
                                    .css_classes(vec!["card", "attachment-chip"])
                                    .margin_top(4)
                                    .margin_bottom(4)
                                    .build();

                                let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
                                let text = gtk::Label::builder()
                                    .label(&format!("{} ({})", att.filename, att.size_formatted))
                                    .css_classes(vec!["caption"])
                                    .build();

                                let btn_save = gtk::Button::builder()
                                    .icon_name("document-save-symbolic")
                                    .tooltip_text("Anhang speichern unter…")
                                    .css_classes(vec!["flat", "circular"])
                                    .valign(gtk::Align::Center)
                                    .build();
                                let filename = att.filename.clone();
                                let content = att.content.clone();
                                let parent = attachment_bar.root().and_downcast::<gtk::Window>();
                                btn_save.connect_clicked(move |_| {
                                    let Some(parent) = parent.as_ref() else {
                                        tracing::warn!(
                                            "Cannot save attachment without a parent window"
                                        );
                                        return;
                                    };

                                    let file_dialog = gtk::FileDialog::builder()
                                        .title("Anhang speichern")
                                        .modal(true)
                                        .initial_name(&filename)
                                        .build();
                                    let filename = filename.clone();
                                    let content = content.clone();
                                    file_dialog.save(
                                        Some(parent),
                                        None::<&gio::Cancellable>,
                                        move |result| match result {
                                            Ok(file) => {
                                                let Some(path) = file.path() else {
                                                    tracing::warn!(
                                                        "Cannot save attachment to a non-local URI"
                                                    );
                                                    return;
                                                };
                                                if let Err(error) = fs::write(&path, &content) {
                                                    tracing::error!(
                                                        path = %path.display(),
                                                        %error,
                                                        "Could not save attachment"
                                                    );
                                                } else {
                                                    tracing::info!(
                                                        path = %path.display(),
                                                        filename = %filename,
                                                        "Attachment saved"
                                                    );
                                                }
                                            }
                                            Err(error) => {
                                                tracing::debug!(%error, "Attachment save cancelled")
                                            }
                                        },
                                    );
                                });

                                chip.append(&icon);
                                chip.append(&text);
                                chip.append(&btn_save);
                                attachment_bar.append(&chip);
                            }
                        }
                    }
                }
            }
        ));

        if store.n_items() > 0 {
            selection.set_selected(0);
        }
    }

    fn render_thread_context(
        store: &gio::ListStore,
        selection: &gtk::SingleSelection,
        selected: &MailMessageObject,
        thread_context: &gtk::ListBox,
    ) {
        while let Some(child) = thread_context.first_child() {
            thread_context.remove(&child);
        }

        let thread_id = selected.thread_id();
        if thread_id.is_empty() || selected.thread_size() <= 1 {
            thread_context.set_visible(false);
            return;
        }

        let mut messages = Vec::new();
        for index in 0..store.n_items() {
            let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                continue;
            };
            if message.thread_id() == thread_id {
                messages.push(message);
            }
        }
        messages.sort_by_key(|message| (message.thread_position(), message.id()));

        let header = gtk::Label::builder()
            .label(&format!("Konversation · {} Nachrichten", messages.len()))
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_bottom(4)
            .build();
        let header_row = gtk::ListBoxRow::builder()
            .child(&header)
            .activatable(false)
            .build();
        thread_context.append(&header_row);

        let message_count = messages.len();
        for message in &messages {
            let label = gtk::Label::builder()
                .label(&format!(
                    "{} · {}\n{}",
                    message.sender_name(),
                    format_message_date(&message.date_str()),
                    message.subject()
                ))
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(2)
                .xalign(0.0)
                .hexpand(true)
                .build();
            let button = gtk::Button::builder()
                .child(&label)
                .css_classes(vec!["flat", "thread-message"])
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .build();
            if message.id() == selected.id() {
                button.add_css_class("accent");
                button.set_sensitive(false);
            } else {
                let message_id = message.id();
                let selection = selection.clone();
                button.connect_clicked(move |_| {
                    for index in 0..selection.n_items() {
                        let Some(candidate) =
                            selection.item(index).and_downcast::<MailMessageObject>()
                        else {
                            continue;
                        };
                        if candidate.id() == message_id {
                            selection.set_selected(index);
                            break;
                        }
                    }
                });
            }
            let row = gtk::ListBoxRow::builder().child(&button).build();
            thread_context.append(&row);
        }
        thread_context.set_visible(message_count > 1);
    }

    // ── Private: Assemble the split layout ──────────────────────────────

    fn assemble_layout(
        &self,
        sidebar: adw::NavigationPage,
        message_container: gtk::Box,
        detail_view: gtk::Box,
    ) {
        let imp = self.imp();

        // Outer gtk::Paned: folder sidebar | message+preview area
        // Gives the user a draggable divider between the folder list and messages.
        let sidebar_child: gtk::Widget = sidebar.upcast();
        imp.outer_paned.set_start_child(Some(&sidebar_child));
        imp.outer_paned.set_resize_start_child(false);
        imp.outer_paned.set_shrink_start_child(false);

        let list_page = adw::NavigationPage::builder()
            .title("Posteingang")
            .child(&message_container)
            .build();

        let detail_page = adw::NavigationPage::builder()
            .title("Nachricht")
            .child(&detail_view)
            .build();

        let list_widget: gtk::Widget = list_page.upcast();
        let detail_widget: gtk::Widget = detail_page.upcast();
        imp.inner_paned.set_start_child(Some(&list_widget));
        imp.inner_paned.set_end_child(Some(&detail_widget));
        imp.preview_widget.replace(Some(detail_widget));

        let inner_paned_widget: gtk::Widget = imp.inner_paned.clone().upcast();
        imp.outer_paned.set_end_child(Some(&inner_paned_widget));
        imp.outer_paned.set_resize_end_child(true);
        imp.outer_paned.set_shrink_end_child(false);
        let sidebar_position = AppSettings::try_new()
            .map(|settings| settings.mail_sidebar_position())
            .unwrap_or(220);
        imp.outer_paned.set_position(sidebar_position);
        if let Some(settings) = AppSettings::try_new() {
            imp.outer_paned.connect_position_notify(move |paned| {
                let position = paned.position();
                if position > 10 {
                    settings.set_mail_sidebar_position(position);
                }
            });
        }

        imp.outer_paned.set_parent(&*self.imp().obj());
    }

    fn load_initial_data(
        &self,
        source_store: &gio::ListStore,
        source_state: &gtk::Label,
        folder_section: &gtk::Box,
    ) {
        self.select_folder_row(&FolderSelection::all());
        self.start_mail_load(MailLoadRequest::initial(), true);
        self.setup_mail_polling();
        self.setup_camel_change_monitor();

        let source_store_for_load = source_store.clone();
        let source_state_for_load = source_state.clone();
        let folder_section_for_load = folder_section.clone();
        let configured_accounts = AccountConfigStore::default().load();
        for profile in &configured_accounts {
            source_store.append(&EdsSourceObject::from_account_profile(profile));
        }
        let has_configured_accounts = !configured_accounts.is_empty();
        glib::spawn_future_local(async move {
            match eds_service::list_mail_sources().await {
                Ok(sources) => {
                    tracing::info!("Loaded {} mail sources from EDS", sources.len());
                    let has_eds_sources = !sources.is_empty();
                    let has_enabled_source = sources.iter().any(|source| source.enabled);
                    for source in sources {
                        tracing::debug!("  Source: {} ({})", source.display_name, source.uid);
                        source_store_for_load.append(&EdsSourceObject::from_source(&source));
                    }
                    if !has_eds_sources && has_configured_accounts {
                        source_state_for_load.set_label("Gespeicherte Kontoprofile vorhanden; EDS/Camel-Synchronisation ist noch nicht verbunden.");
                    } else if !has_eds_sources {
                        source_state_for_load.set_label("Keine EDS-/GOA-Mailkonten gefunden. Das Konto-Setup ist noch nicht mit EDS verbunden.");
                    } else if !has_enabled_source {
                        source_state_for_load.set_label(if has_configured_accounts {
                            "Alle gefundenen EDS-/GOA-Mailkonten sind deaktiviert; gespeicherte Kontoprofile warten auf die Backend-Anbindung."
                        } else {
                            "Alle gefundenen EDS-/GOA-Mailkonten sind deaktiviert."
                        });
                    } else {
                        source_state_for_load.set_visible(false);
                        folder_section_for_load.set_visible(true);
                    }
                }
                Err(err) => {
                    tracing::warn!("Failed to load EDS sources: {}", err);
                    source_state_for_load.set_label(&format!("EDS ist nicht erreichbar: {}", err));
                }
            }
        });
    }

    fn selected_message_id(&self) -> Option<String> {
        self.imp()
            .message_selection
            .borrow()
            .as_ref()
            .and_then(|selection| selection.selected_item())
            .and_then(|item| item.downcast::<MailMessageObject>().ok())
            .map(|message| message.id())
    }

    fn select_message_after_reload(&self, previous_id: Option<&str>) {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return;
        };
        if selection.n_items() == 0 {
            return;
        }
        if let Some(previous_id) = previous_id {
            for index in 0..selection.n_items() {
                let Some(message) = selection.item(index).and_downcast::<MailMessageObject>()
                else {
                    continue;
                };
                if message.id() == previous_id {
                    selection.set_selected(index);
                    self.sync_thread_selection(Some(previous_id));
                    return;
                }
            }
        }
        selection.set_selected(0);
        let selected_id = selection
            .selected_item()
            .and_downcast::<MailMessageObject>()
            .map(|message| message.id());
        self.sync_thread_selection(selected_id.as_deref());
    }

    fn sync_thread_selection(&self, message_id: Option<&str>) {
        let Some(thread_selection) = self.imp().thread_selection.borrow().as_ref().cloned() else {
            return;
        };
        if thread_selection.n_items() == 0 {
            return;
        }

        let target_thread_id = message_id.and_then(|message_id| {
            self.imp()
                .message_store
                .borrow()
                .as_ref()
                .and_then(|store| {
                    (0..store.n_items()).find_map(|index| {
                        store
                            .item(index)
                            .and_downcast::<MailMessageObject>()
                            .filter(|message| message.id() == message_id)
                            .map(|message| message.thread_id())
                    })
                })
        });
        let mut selected_index = None;
        for index in 0..thread_selection.n_items() {
            let Some(row) = thread_selection
                .item(index)
                .and_downcast::<gtk::TreeListRow>()
            else {
                continue;
            };
            let Some(message) = row.item().and_downcast::<MailMessageObject>() else {
                continue;
            };
            if message_id
                .map(|message_id| message.id() == message_id)
                .unwrap_or(false)
            {
                selected_index = Some(index);
                break;
            }
            if selected_index.is_none()
                && target_thread_id
                    .as_deref()
                    .map(|thread_id| message.thread_id() == thread_id)
                    .unwrap_or(false)
            {
                selected_index = Some(index);
            }
        }

        let index = selected_index.unwrap_or(0);
        self.imp().thread_selection_syncing.set(true);
        thread_selection.set_selected(index);
        self.imp().thread_selection_syncing.set(false);
    }

    fn setup_mail_polling(&self) {
        let Some(settings) = AppSettings::try_new() else {
            return;
        };
        let interval = settings.mail_sync_interval_seconds();
        if interval == 0 {
            return;
        }

        let page = self.downgrade();
        let source = glib::timeout_add_seconds_local(interval, move || {
            let Some(page) = page.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !page.imp().mail_loading.get() {
                page.refresh_mail();
            }
            glib::ControlFlow::Continue
        });
        self.imp().mail_sync_source.replace(Some(source));
    }

    fn restart_mail_polling(&self) {
        if let Some(source) = self.imp().mail_sync_source.borrow_mut().take() {
            source.remove();
        }
        self.setup_mail_polling();
    }

    fn setup_camel_change_monitor(&self) {
        let Some((cancellation, receiver)) = camel_mail_service::start_change_monitor() else {
            return;
        };

        self.imp()
            .mail_change_cancellation
            .replace(Some(cancellation));
        let page = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                let Some(page) = page.upgrade() else {
                    break;
                };
                page.handle_camel_sync_event(event);
            }
        });
    }

    fn handle_camel_sync_event(&self, event: camel_mail_service::CamelSyncEvent) {
        match event {
            camel_mail_service::CamelSyncEvent::Ready(status) => {
                if status.failed_sources > 0 && !self.imp().mail_loading.get() {
                    if let Some(state) = self.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!(
                            "EDS/Camel-Signalmonitor aktiv · {}/{} Konten verfügbar{}",
                            status.successful_sources,
                            status.configured_sources,
                            status
                                .warning
                                .as_deref()
                                .map(|warning| format!(" · {warning}"))
                                .unwrap_or_default()
                        ));
                    }
                }
            }
            camel_mail_service::CamelSyncEvent::Change(change) => {
                if self.imp().mail_loading.get() || self.imp().mail_incremental_loading.get() {
                    self.imp().mail_change_pending.set(true);
                    return;
                }

                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    let kind = match change.kind {
                        camel_mail_service::CamelChangeKind::Changed => "geändert",
                        camel_mail_service::CamelChangeKind::FolderDeleted => "gelöscht",
                        camel_mail_service::CamelChangeKind::FolderRenamed => "umbenannt",
                    };
                    let rename_hint = change
                        .old_folder_name
                        .as_deref()
                        .map(|old_name| format!(" von {old_name}"))
                        .unwrap_or_default();
                    state.set_label(&format!(
                        "EDS/Camel: Ordner {} in {} wurde {}{} (＋{} / −{} / ~{} / neu {}) · Aktualisierung …",
                        change.folder_name,
                        change.source_uid,
                        kind,
                        rename_hint,
                        change.added_count,
                        change.removed_count,
                        change.changed_count,
                        change.recent_count
                    ));
                }
                self.start_incremental_camel_update(change);
            }
            camel_mail_service::CamelSyncEvent::Error(error) => {
                tracing::warn!(%error, "Native Camel change monitor stopped");
                if !self.imp().mail_loading.get() {
                    if let Some(state) = self.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!(
                            "EDS/Camel-Signalmonitor nicht verfügbar: {error}"
                        ));
                    }
                }
            }
        }
    }

    fn start_incremental_camel_update(&self, change: camel_mail_service::CamelChangeEvent) {
        if change.kind != camel_mail_service::CamelChangeKind::Changed {
            self.refresh_mail();
            return;
        }

        let mut message_uids = Vec::new();
        for uid in change
            .added_uids
            .iter()
            .chain(change.changed_uids.iter())
            .chain(change.recent_uids.iter())
        {
            if !message_uids.contains(uid) {
                message_uids.push(uid.clone());
            }
        }
        let removed_ids = change
            .removed_uids
            .iter()
            .map(|uid| format!("eds:{}:{}:{}", change.source_uid, change.folder_name, uid))
            .collect::<std::collections::HashSet<_>>();
        let replaced_ids = message_uids
            .iter()
            .map(|uid| format!("eds:{}:{}:{}", change.source_uid, change.folder_name, uid))
            .collect::<std::collections::HashSet<_>>();

        let store = self.imp().message_store.borrow().clone();
        if let Some(store) = store.as_ref() {
            Self::remove_message_ids(store, &removed_ids);
            self.refresh_thread_metadata(store);
        }
        self.imp()
            .batch_selection
            .borrow_mut()
            .retain(|id| !removed_ids.contains(id));
        self.update_batch_action_bar();

        if message_uids.is_empty() {
            if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label(&format!(
                    "EDS/Camel: Änderungen in {} inkrementell übernommen",
                    change.folder_name
                ));
            }
            return;
        }

        let generation = self.imp().load_generation.get();
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .mail_incremental_cancellation
            .replace(Some(cancellation.clone()));
        self.imp().mail_incremental_loading.set(true);
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!(
                "EDS/Camel: {} Nachrichten in {} werden inkrementell aktualisiert …",
                message_uids.len(),
                change.folder_name
            ));
        }

        let source_uid = change.source_uid.clone();
        let folder_name = change.folder_name.clone();
        let worker_folder_name = folder_name.clone();
        let (sender, receiver) = async_channel::bounded(1);
        let worker_cancellation = cancellation.clone();
        std::thread::spawn(move || {
            let result = camel_mail_service::load_messages_for_uids(
                &source_uid,
                &worker_folder_name,
                &message_uids,
                &worker_cancellation,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Incremental Camel result was dropped");
            }
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err("Inkrementelles Camel-Laden wurde abgebrochen".to_string())
            });
            let Some(page) = page.upgrade() else {
                return;
            };
            if page.imp().load_generation.get() != generation {
                return;
            }
            page.imp().mail_incremental_loading.set(false);
            match result {
                Ok(messages) => {
                    let previous_selected_id = page.selected_message_id();
                    if let Some(store) = page.imp().message_store.borrow().as_ref() {
                        Self::remove_message_ids(store, &removed_ids);
                        Self::remove_message_ids(store, &replaced_ids);
                        page.replace_or_append_messages(store, messages);
                    }
                    if let Some(filter) = page.imp().message_filter.borrow().as_ref() {
                        filter.changed(gtk::FilterChange::Different);
                    }
                    page.select_message_after_reload(previous_selected_id.as_deref());
                    if let Some(state) = page.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!(
                            "EDS/Camel: Änderungen in {} inkrementell übernommen",
                            folder_name
                        ));
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Incremental Camel update failed");
                    if let Some(state) = page.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!(
                            "EDS/Camel-Inkrementalsynchronisation fehlgeschlagen: {error}"
                        ));
                    }
                }
            }
            if page.imp().mail_change_pending.replace(false) {
                page.refresh_mail();
            }
        });
    }

    fn update_camel_sync_progress(&self, progress: &camel_mail_service::CamelSyncProgress) {
        let message_state = self.imp().message_state.borrow();
        let Some(state) = message_state.as_ref() else {
            return;
        };

        let account = progress
            .source_display_name
            .as_deref()
            .or(progress.source_uid.as_deref())
            .unwrap_or("EDS-Konto");
        let account_progress = if progress.configured_sources > 0 {
            format!(
                " ({}/{} Konten, {} erfolgreich, {} Fehler, {} Ordner, {} Nachrichten)",
                progress.processed_sources,
                progress.configured_sources,
                progress.successful_sources,
                progress.failed_sources,
                progress.folders_seen,
                progress.messages_loaded
            )
        } else {
            String::new()
        };
        let label = match progress.phase {
            camel_mail_service::CamelSyncPhase::Discovering => {
                "EDS/Camel: Mailkonten werden gesucht …".to_string()
            }
            camel_mail_service::CamelSyncPhase::Connecting => {
                format!("EDS/Camel: Verbinde mit {account}{account_progress} …")
            }
            camel_mail_service::CamelSyncPhase::LoadingFolders => {
                format!("EDS/Camel: Lade Ordner von {account}{account_progress} …")
            }
            camel_mail_service::CamelSyncPhase::LoadingMessages => format!(
                "EDS/Camel: Lade {} von {account}{account_progress} …",
                progress.folder_name.as_deref().unwrap_or("Nachrichten")
            ),
            camel_mail_service::CamelSyncPhase::SourceComplete => {
                format!("EDS/Camel: {account} synchronisiert{account_progress}")
            }
            camel_mail_service::CamelSyncPhase::Complete => {
                format!("EDS/Camel: Synchronisation abgeschlossen{account_progress}")
            }
            camel_mail_service::CamelSyncPhase::Failed => {
                format!("EDS/Camel: Synchronisation fehlgeschlagen{account_progress}")
            }
            camel_mail_service::CamelSyncPhase::Retrying => format!(
                "EDS/Camel: vorübergehender Fehler · neuer Versuch {}/{} …",
                progress.attempt, progress.max_attempts
            ),
        };
        state.set_label(&label);
    }

    pub fn refresh_mail(&self) {
        let mut request = self
            .imp()
            .active_request
            .borrow()
            .clone()
            .unwrap_or_else(MailLoadRequest::initial);
        request.offset = 0;
        self.start_mail_load(request, true);
    }

    pub fn load_more_mail(&self) {
        if self.imp().mail_loading.get() {
            return;
        }

        let Some(mut request) = self.imp().active_request.borrow().clone() else {
            return;
        };
        request.offset = request
            .offset
            .saturating_add(request.max_messages_per_folder.max(1) as usize);
        self.start_mail_load(request, false);
    }

    fn start_mail_load(&self, request: MailLoadRequest, replace: bool) {
        if let Some(cancelled) = self.imp().mail_load_cancellation.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(cancelled) = self.imp().mail_incremental_cancellation.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.imp().mail_incremental_loading.set(false);

        let generation = self.imp().load_generation.get().wrapping_add(1);
        self.imp().load_generation.set(generation);
        self.imp().mail_loading.set(true);
        if let Some(button) = self.imp().refresh_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
            button.set_visible(false);
            button.set_sensitive(false);
        }
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(if replace {
                "Nachrichten werden aktualisiert …"
            } else {
                "Weitere Nachrichten werden geladen …"
            });
        }

        let (sender, receiver) = async_channel::bounded(1);
        let (progress_sender, progress_receiver) = async_channel::bounded(32);
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .mail_load_cancellation
            .replace(Some(cancellation.clone()));
        let request_for_worker = request.clone();
        let progress_sender_for_worker = progress_sender.clone();
        std::thread::spawn(move || {
            let result = load_mail_backend(
                &request_for_worker,
                &cancellation,
                Some(&progress_sender_for_worker),
            );
            drop(progress_sender_for_worker);
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Mail result was dropped before delivery");
            }
        });

        let page_for_progress = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(progress) = progress_receiver.recv().await {
                let Some(page) = page_for_progress.upgrade() else {
                    break;
                };
                if page.imp().load_generation.get() == generation {
                    page.update_camel_sync_progress(&progress);
                }
            }
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                MailPageLoadResult::Error("Mail-Ladevorgang wurde abgebrochen.".to_string())
            });
            let Some(page) = page.upgrade() else {
                return;
            };
            if page.imp().load_generation.get() != generation {
                return;
            }
            page.apply_mail_load(request, result, replace);
        });
    }

    fn apply_mail_load(&self, request: MailLoadRequest, result: MailPageLoadResult, replace: bool) {
        let previous_selected_id = replace.then(|| self.selected_message_id()).flatten();
        if replace {
            self.clear_batch_selection();
        }
        self.imp().mail_loading.set(false);
        if let Some(button) = self.imp().refresh_button.borrow().as_ref() {
            button.set_sensitive(true);
        }

        match result {
            MailPageLoadResult::Camel(camel) => {
                let status = format_camel_status(&camel);
                let has_more = camel.has_more_for(
                    request.source_uid.as_deref(),
                    request.folder_name.as_deref(),
                    request.offset,
                    request.max_messages_per_folder,
                );
                let folder_list = self.imp().folder_list.borrow().clone();
                if let Some(folder_list) = folder_list.as_ref() {
                    if replace {
                        self.rebuild_camel_folder_rows(folder_list, &camel.folders);
                    }
                    self.update_camel_folder_counts(folder_list, &camel.folders);
                }

                let appended = if let Some(store) = self.imp().message_store.borrow().as_ref() {
                    if replace {
                        store.remove_all();
                    }
                    self.append_unique_messages(store, camel.messages)
                } else {
                    0
                };

                self.imp().active_request.replace(Some(request.clone()));
                if let Some(section) = self.imp().folder_section.borrow().as_ref() {
                    section.set_visible(true);
                }
                if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                    button.set_visible(has_more);
                    button.set_sensitive(true);
                }
                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    state.set_label(&format!(
                        "{status} · {appended} Nachricht(en) in dieser Seite"
                    ));
                }

                if replace {
                    self.select_message_after_reload(previous_selected_id.as_deref());
                    let selected_folder = self
                        .imp()
                        .folder_selection
                        .borrow()
                        .as_ref()
                        .map(|selection| selection.borrow().clone());
                    if let Some(selected_folder) = selected_folder {
                        if selected_folder.is_remote() {
                            self.select_folder_row(&selected_folder);
                        }
                    }
                }
            }
            MailPageLoadResult::Local { reason, messages } => {
                let folder_list = self.imp().folder_list.borrow().clone();
                if let Some(folder_list) = folder_list.as_ref() {
                    self.rebuild_camel_folder_rows(folder_list, &[]);
                }
                let mut loaded_count = 0;
                let mut folder_counts = HashMap::new();
                if let Some(store) = self.imp().message_store.borrow().as_ref() {
                    if replace {
                        store.remove_all();
                    }
                    for record in messages {
                        loaded_count += 1;
                        let folder_counts_entry = folder_counts
                            .entry(record.folder_id.clone())
                            .or_insert((0_u32, 0_u32));
                        folder_counts_entry.1 += 1;
                        if record.is_unread {
                            folder_counts_entry.0 += 1;
                        }
                        store.append(&record.into_object());
                    }
                    self.refresh_thread_metadata(store);
                }
                if let Some(folder_list) = folder_list.as_ref() {
                    Self::update_folder_counts(folder_list, &folder_counts);
                }
                self.imp().active_request.replace(None);
                if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                    button.set_visible(false);
                    button.set_sensitive(true);
                }
                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    state.set_label(&format!(
                        "{reason} · {loaded_count} lokale Nachricht(en) geladen"
                    ));
                }
                if let Some(selection) = self.imp().folder_selection.borrow().as_ref() {
                    selection.replace(FolderSelection::all());
                }
                self.select_folder_row(&FolderSelection::all());
                if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
                    filter.changed(gtk::FilterChange::Different);
                }
                if loaded_count > 0 {
                    if let Some(section) = self.imp().folder_section.borrow().as_ref() {
                        section.set_visible(true);
                    }
                    self.select_message_after_reload(None);
                }
            }
            MailPageLoadResult::Error(error) => {
                if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                    button.set_visible(false);
                    button.set_sensitive(true);
                }
                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    state.set_label(&format!("EDS/Camel-Laden nicht möglich: {error}"));
                }
                tracing::warn!(%error, "Mail backend load failed");
            }
        }

        if self.imp().mail_change_pending.replace(false) && !self.imp().mail_loading.get() {
            self.refresh_mail();
        }
    }

    fn append_unique_messages(&self, store: &gio::ListStore, messages: Vec<ParsedMail>) -> usize {
        let mut existing_ids = std::collections::HashSet::new();
        for index in 0..store.n_items() {
            if let Some(message) = store.item(index).and_downcast::<MailMessageObject>() {
                existing_ids.insert(message.id());
            }
        }

        let mut appended = 0;
        for record in messages {
            if existing_ids.insert(record.id.clone()) {
                store.append(&record.into_object());
                appended += 1;
            }
        }
        self.refresh_thread_metadata(store);
        appended
    }

    fn remove_message_ids(store: &gio::ListStore, ids: &std::collections::HashSet<String>) {
        if ids.is_empty() {
            return;
        }

        let mut indices = Vec::new();
        for index in 0..store.n_items() {
            if let Some(message) = store.item(index).and_downcast::<MailMessageObject>() {
                if ids.contains(&message.id()) {
                    indices.push(index);
                }
            }
        }
        for index in indices.into_iter().rev() {
            store.remove(index);
        }
    }

    fn replace_or_append_messages(&self, store: &gio::ListStore, messages: Vec<ParsedMail>) {
        let ids = messages
            .iter()
            .map(|message| message.id.clone())
            .collect::<std::collections::HashSet<_>>();
        Self::remove_message_ids(store, &ids);
        for message in messages {
            store.append(&message.into_object());
        }
        self.refresh_thread_metadata(store);
    }

    fn refresh_thread_metadata(&self, store: &gio::ListStore) {
        let messages = (0..store.n_items())
            .filter_map(|index| store.item(index).and_downcast::<MailMessageObject>())
            .collect::<Vec<_>>();
        let message_ids = messages
            .iter()
            .map(MailMessageObject::message_id)
            .collect::<Vec<_>>();
        let in_reply_to = messages
            .iter()
            .map(MailMessageObject::in_reply_to)
            .collect::<Vec<_>>();
        let references = messages
            .iter()
            .map(MailMessageObject::references)
            .collect::<Vec<_>>();
        let subjects = messages
            .iter()
            .map(MailMessageObject::subject)
            .collect::<Vec<_>>();
        let inputs = (0..messages.len())
            .map(|index| ThreadIdentity {
                message_id: message_ids[index].as_deref(),
                in_reply_to: &in_reply_to[index],
                references: &references[index],
                subject: &subjects[index],
            })
            .collect::<Vec<_>>();
        let memberships = group_messages(&inputs);
        for (message, membership) in messages.iter().zip(memberships) {
            message.set_thread_metadata(
                &membership.thread_id,
                membership.size,
                membership.position,
            );
        }

        if !self.imp().mail_threading.get() {
            let mut roots = Vec::with_capacity(messages.len());
            let mut thread_members = HashMap::<String, Vec<MailMessageObject>>::new();
            let thread_children = HashMap::<String, gio::ListStore>::new();
            for message in messages {
                let message_id = message.id();
                message.set_thread_metadata(&message_id, 1, 0);
                thread_members.insert(message_id, vec![message.clone()]);
                roots.push(message);
            }
            self.sort_mail_roots(&mut roots);
            self.replace_thread_models(store, roots, thread_members, thread_children);
            return;
        }

        let mut thread_order = Vec::new();
        let mut grouped_messages = HashMap::<String, Vec<MailMessageObject>>::new();
        for message in &messages {
            let thread_id = message.thread_id();
            if !grouped_messages.contains_key(&thread_id) {
                thread_order.push(thread_id.clone());
            }
            grouped_messages
                .entry(thread_id)
                .or_default()
                .push(message.clone());
        }

        let mut roots = Vec::new();
        let mut thread_members = HashMap::<String, Vec<MailMessageObject>>::new();
        let mut thread_children = HashMap::<String, gio::ListStore>::new();
        for thread_id in thread_order {
            let Some(mut members) = grouped_messages.remove(&thread_id) else {
                continue;
            };
            members.sort_by_key(|message| (message.thread_position(), message.id()));
            let Some(root) = members.first().cloned() else {
                continue;
            };

            let children = gio::ListStore::new::<MailMessageObject>();
            for child in members.iter().skip(1) {
                children.append(child);
            }
            roots.push(root);
            thread_members.insert(thread_id.clone(), members);
            thread_children.insert(thread_id, children);
        }
        self.sort_mail_roots(&mut roots);
        self.replace_thread_models(store, roots, thread_members, thread_children);
    }

    fn sort_mail_roots(&self, roots: &mut [MailMessageObject]) {
        let sort_field = self.imp().mail_sort_field.get();
        let descending = self.imp().mail_sort_descending.get();
        roots.sort_by(|left, right| {
            let ordering = match sort_field {
                MailSortField::Date => left.date_str().cmp(&right.date_str()),
                MailSortField::Subject => left.subject().cmp(&right.subject()),
                MailSortField::Sender => left
                    .sender_name()
                    .cmp(&right.sender_name())
                    .then_with(|| left.sender_email().cmp(&right.sender_email())),
                MailSortField::Unread => left.is_unread().cmp(&right.is_unread()),
                MailSortField::Flagged => left.is_flagged().cmp(&right.is_flagged()),
                MailSortField::Attachments => {
                    left.attachments().len().cmp(&right.attachments().len())
                }
            }
            .then_with(|| left.id().cmp(&right.id()));
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
    }

    fn replace_thread_models(
        &self,
        store: &gio::ListStore,
        roots: Vec<MailMessageObject>,
        thread_members: HashMap<String, Vec<MailMessageObject>>,
        thread_children: HashMap<String, gio::ListStore>,
    ) {
        let root_store = self.imp().thread_root_store.borrow().clone();
        let members_for_model = self.imp().thread_members.borrow().as_ref().cloned();
        let children_for_model = self.imp().thread_children.borrow().as_ref().cloned();
        if let Some(root_store) = root_store {
            root_store.remove_all();
            if let Some(members_for_model) = members_for_model {
                members_for_model.replace(thread_members);
            }
            if let Some(children_for_model) = children_for_model {
                children_for_model.replace(thread_children);
            }
            for root in roots {
                root_store.append(&root);
            }
            if let Some(filter) = self.imp().thread_filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
        }

        if store.n_items() > 0 {
            let count = store.n_items();
            store.items_changed(0, count, count);
        }
    }

    fn update_camel_folder_counts(
        &self,
        folder_list: &gtk::ListBox,
        folders: &[camel_mail_service::CamelFolderSummary],
    ) {
        for index in 0..folder_list.observe_children().n_items() {
            let Some(row) = folder_list
                .row_at_index(index as i32)
                .and_downcast::<gtk::ListBoxRow>()
            else {
                continue;
            };
            let Some(data) = Self::folder_row_data(&row) else {
                continue;
            };
            let (unread, total) = if let (Some(source_uid), Some(folder_name)) =
                (data.source_uid.as_deref(), data.folder_name.as_deref())
            {
                folders
                    .iter()
                    .find(|folder| {
                        folder.source_uid == source_uid && folder.folder_name == folder_name
                    })
                    .map(|folder| (folder.unread_count, folder.total_count))
                    .unwrap_or((0, 0))
            } else {
                folders
                    .iter()
                    .filter(|folder| {
                        data.folder_id == "all"
                            || folder.folder_id
                                == if data.folder_id == "spam" {
                                    "junk"
                                } else {
                                    data.folder_id.as_str()
                                }
                    })
                    .fold((0_u32, 0_u32), |(unread, total), folder| {
                        (
                            unread.saturating_add(folder.unread_count),
                            total.saturating_add(folder.total_count),
                        )
                    })
            };
            let Some(badge) = (unsafe {
                row.data::<gtk::Label>("folder-badge")
                    .map(|pointer| pointer.as_ref())
            }) else {
                continue;
            };
            badge.set_label(&if unread > 0 {
                unread.to_string()
            } else {
                total.to_string()
            });
            badge.set_visible(unread > 0 || total > 0);
        }

        if let Some(index) = self.imp().dynamic_folder_index.borrow().as_ref().cloned() {
            let index = index.borrow();
            for folder in folders {
                let id = Self::dynamic_folder_key(&folder.source_uid, &folder.folder_name);
                if let Some(object) = index.get(&id) {
                    object.set_counts(folder.unread_count, folder.total_count);
                }
            }
        }
        if let Some(root_store) = self.imp().dynamic_folder_store.borrow().as_ref() {
            let count = root_store.n_items();
            if count > 0 {
                root_store.items_changed(0, count, count);
            }
        }
        if let Some(children) = self
            .imp()
            .dynamic_folder_children
            .borrow()
            .as_ref()
            .cloned()
        {
            for store in children.borrow().values() {
                let count = store.n_items();
                if count > 0 {
                    store.items_changed(0, count, count);
                }
            }
        }
    }
}

impl Default for MailPage {
    fn default() -> Self {
        Self::new()
    }
}

fn append_itip_invitation(attachment_bar: &gtk::Box, invitation: ItipInvitation) {
    let event = invitation.event.clone();
    let method = invitation.method;
    let title = if event.summary.trim().is_empty() {
        "Kalendereinladung"
    } else {
        event.summary.as_str()
    };
    let mut details = vec![format!("{} · {}", invitation.method.as_str(), event.start)];
    if !event.end.trim().is_empty() {
        details.push(event.end.clone());
    }
    if !invitation.organizer.trim().is_empty() {
        details.push(format!("Organisator: {}", invitation.organizer));
    }
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(&details.join(" · "))
        .css_classes(vec!["card"])
        .build();

    if matches!(
        method,
        ItipMethod::Request | ItipMethod::Publish | ItipMethod::Add
    ) {
        let import_button = gtk::Button::builder()
            .label("Lokal übernehmen")
            .tooltip_text("Diese Einladung in den app-eigenen Kalender übernehmen")
            .css_classes(vec!["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&import_button);

        let component = event.component.clone();
        let row_for_result = row.clone();
        import_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            let component = component.clone();
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let result = ECalService::new().create_component(
                    "local-calendar",
                    ECalComponentType::Event,
                    &component,
                );
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "iTIP local import result was dropped");
                }
            });
            let button_weak = button.downgrade();
            let row_for_future = row_for_result.clone();
            glib::spawn_future_local(async move {
                match receiver.recv().await {
                    Ok(Ok(result)) => {
                        if let Some(button) = button_weak.upgrade() {
                            button.set_label("Übernommen");
                        }
                        show_mail_toast_for_widget(
                            &row_for_future,
                            &format!("Termin lokal übernommen ({})", result.uid),
                        );
                    }
                    Ok(Err(error)) => {
                        if let Some(button) = button_weak.upgrade() {
                            button.set_sensitive(true);
                        }
                        show_mail_toast_for_widget(
                            &row_for_future,
                            &format!("Einladung konnte nicht übernommen werden: {error}"),
                        );
                    }
                    Err(error) => {
                        if let Some(button) = button_weak.upgrade() {
                            button.set_sensitive(true);
                        }
                        tracing::debug!(%error, "iTIP import channel closed");
                    }
                }
            });
        });

        if matches!(method, ItipMethod::Request | ItipMethod::Add) {
            for (label, status) in [
                ("Zusagen", ItipReplyStatus::Accepted),
                ("Vorbehalt", ItipReplyStatus::Tentative),
                ("Ablehnen", ItipReplyStatus::Declined),
            ] {
                let reply_button = gtk::Button::builder()
                    .label(label)
                    .tooltip_text(&format!("iTIP-Antwort: {}", status.as_str()))
                    .css_classes(if status == ItipReplyStatus::Accepted {
                        vec!["suggested-action"]
                    } else {
                        vec!["flat"]
                    })
                    .valign(gtk::Align::Center)
                    .build();
                row.add_suffix(&reply_button);
                connect_itip_reply_button(&reply_button, &row, invitation.clone(), status);
            }
        }
    } else {
        let status = gtk::Label::builder()
            .label("EDS-Antwort erforderlich")
            .css_classes(vec!["dim-label", "caption"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&status);
    }
    attachment_bar.append(&row);
}

fn connect_itip_reply_button(
    button: &gtk::Button,
    row: &adw::ActionRow,
    invitation: ItipInvitation,
    status: ItipReplyStatus,
) {
    let button_for_signal = button.clone();
    let row_for_signal = row.clone();
    button.connect_clicked(move |_| {
        button_for_signal.set_sensitive(false);
        let invitation = invitation.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = save_itip_reply_draft(&invitation, status);
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "iTIP response draft result was dropped");
            }
        });
        let button_weak = button_for_signal.downgrade();
        let row_for_future = row_for_signal.clone();
        glib::spawn_future_local(async move {
            match receiver.recv().await {
                Ok(Ok(draft_id)) => {
                    if let Some(button) = button_weak.upgrade() {
                        button.set_label("Entwurf gespeichert");
                    }
                    show_mail_toast_for_widget(
                        &row_for_future,
                        &format!("iTIP-Antwort als Entwurf gespeichert ({draft_id})"),
                    );
                }
                Ok(Err(error)) => {
                    if let Some(button) = button_weak.upgrade() {
                        button.set_sensitive(true);
                    }
                    show_mail_toast_for_widget(
                        &row_for_future,
                        &format!("iTIP-Antwort konnte nicht gespeichert werden: {error}"),
                    );
                }
                Err(error) => {
                    if let Some(button) = button_weak.upgrade() {
                        button.set_sensitive(true);
                    }
                    tracing::debug!(%error, "iTIP response draft channel closed");
                }
            }
        });
    });
}

fn save_itip_reply_draft(
    invitation: &ItipInvitation,
    status: ItipReplyStatus,
) -> Result<String, String> {
    let profiles = AccountConfigStore::default().load();
    let preferred = AppSettings::try_new()
        .map(|settings| settings.default_account_id())
        .filter(|id| !id.trim().is_empty());
    let profile = preferred
        .as_deref()
        .and_then(|id| {
            profiles
                .iter()
                .find(|profile| profile.id == id && profile.enabled)
        })
        .or_else(|| profiles.iter().find(|profile| profile.enabled))
        .ok_or_else(|| {
            "Kein aktiviertes lokales Kontoprofil für die Antwort vorhanden".to_string()
        })?;
    let attendee = normalize_attendee_address(&profile.email).ok_or_else(|| {
        "Das ausgewählte Kontoprofil besitzt keine gültige E-Mail-Adresse".to_string()
    })?;
    let organizer = normalize_attendee_address(&invitation.organizer)
        .ok_or_else(|| "Die Einladung besitzt keinen gültigen Organisator".to_string())?;
    let component = build_reply_component(invitation, &attendee, status)?;
    let organizer_mailbox = organizer.trim_start_matches("mailto:").to_string();
    let summary = if invitation.event.summary.trim().is_empty() {
        "Kalendereinladung"
    } else {
        invitation.event.summary.trim()
    };
    let draft_id = format!(
        "itip-reply-{}-{}",
        invitation.event.uid,
        attendee.trim_start_matches("mailto:")
    );
    let draft = DraftMessage {
        id: draft_id.clone(),
        from: profile.email.clone(),
        source_uid: profile.id.clone(),
        to: organizer_mailbox,
        cc: String::new(),
        bcc: String::new(),
        subject: format!("Re: {summary}"),
        html_body: format!(
            "{}: {}\n\nDie iTIP-Antwort wurde als text/calendar-Entwurf vorbereitet.",
            status.as_str(),
            summary
        ),
        plain_text: true,
        updated_at: Utc::now().to_rfc3339(),
        attachments: Vec::new(),
        calendar_method: Some("REPLY".to_string()),
        calendar_component: Some(component),
    };
    DraftStoreService::default()
        .save(draft)
        .map_err(|error| format!("{error}"))?;
    Ok(draft_id)
}

fn show_mail_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Mail page result");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_status_keeps_failed_account_warning_visible() {
        let status = format_camel_status(&CamelMailLoad {
            messages: Vec::new(),
            folders: Vec::new(),
            configured_sources: 1,
            successful_sources: 0,
            failed_sources: 1,
            warning: Some("OAuth-Anmeldung fehlgeschlagen".to_string()),
        });

        assert!(status.contains("1 Konto nicht synchronisiert"));
        assert!(status.contains("OAuth-Anmeldung fehlgeschlagen"));
    }

    #[test]
    fn message_search_query_parses_field_and_state_operators() {
        let query = MessageSearchQuery::parse(
            "from:alice@example.test subject:Roadmap is:unread has:attachment deploy",
        );

        assert_eq!(query.from.as_deref(), Some("alice@example.test"));
        assert_eq!(query.subject.as_deref(), Some("roadmap"));
        assert_eq!(query.unread, Some(true));
        assert_eq!(query.has_attachment, Some(true));
        assert_eq!(query.text_terms, vec!["deploy"]);
    }

    #[test]
    fn message_search_query_parses_labels_and_state_operators() {
        let query = MessageSearchQuery::parse("label:important is:read");

        assert_eq!(query.unread, Some(false));
        assert_eq!(query.label.as_deref(), Some("important"));
        assert!(query.text_terms.is_empty());
    }

    #[test]
    fn message_search_query_supports_quotes_recipients_and_dates() {
        let query = MessageSearchQuery::parse(
            "subject:\"Quarterly report\" cc:team@example.test filename:report.pdf after:2026-08-01 no:attachment",
        );

        assert_eq!(query.subject.as_deref(), Some("quarterly report"));
        assert_eq!(query.cc.as_deref(), Some("team@example.test"));
        assert_eq!(query.filename.as_deref(), Some("report.pdf"));
        assert_eq!(query.has_attachment, Some(false));
        assert_eq!(query.date_after, NaiveDate::from_ymd_opt(2026, 8, 1));
        assert!(query.text_terms.is_empty());
    }

    #[test]
    fn message_search_query_supports_folder_and_exact_date_aliases() {
        let query = MessageSearchQuery::parse("in:sent on:2026-08-11");

        assert_eq!(query.folder.as_deref(), Some("sent"));
        assert_eq!(query.date_after, NaiveDate::from_ymd_opt(2026, 8, 11));
        assert_eq!(query.date_before, NaiveDate::from_ymd_opt(2026, 8, 12));
    }

    #[test]
    fn message_search_query_supports_older_newer_and_folder_states() {
        let newer = MessageSearchQuery::parse("newer:7d");
        assert!(newer.date_after.is_some());
        assert!(newer.date_before.is_none());

        let older = MessageSearchQuery::parse("older:2026-08-01");
        assert!(older.date_after.is_none());
        assert_eq!(older.date_before, NaiveDate::from_ymd_opt(2026, 8, 2));

        let sent = MessageSearchQuery::parse("is:sent");
        assert_eq!(sent.folder.as_deref(), Some("sent"));
    }

    #[test]
    fn message_date_formatting_has_a_safe_raw_value_fallback() {
        assert_eq!(
            format_message_date("Tue, 11 Aug 2026 14:30:00 +0000"),
            "11.08.2026 14:30"
        );
        assert_eq!(format_message_date("not-a-date"), "not-a-date");
    }

    #[test]
    fn mail_row_density_has_conservative_fallbacks() {
        assert_eq!(mail_row_density_metrics("compact"), (2, 4));
        assert_eq!(mail_row_density_metrics("spacious"), (7, 14));
        assert_eq!(mail_row_density_metrics("unknown"), (4, 8));
    }

    #[test]
    fn mail_layout_preserves_modern_and_classic_workflows() {
        assert_eq!(MailLayout::from_setting("modern"), MailLayout::Modern);
        assert_eq!(MailLayout::from_setting("classic"), MailLayout::Classic);
        assert_eq!(MailLayout::from_setting("unknown"), MailLayout::Modern);
        assert_eq!(
            MailLayout::Modern.orientation(),
            gtk::Orientation::Horizontal
        );
        assert_eq!(
            MailLayout::Classic.orientation(),
            gtk::Orientation::Vertical
        );
        assert_eq!(MailLayout::Classic.default_position(), 360);
    }
}
