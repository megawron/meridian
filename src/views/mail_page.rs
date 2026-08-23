// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailPage — Three-column mail view with dynamic message selection, real-time search filtering, and WebKitGTK 6.0 rendering.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, Utc};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use crate::models::eds_source::EdsSourceObject;
use crate::models::folder::FolderObject;
use crate::models::mail_message::{MailMessageIdentity, MailMessageObject};
use crate::services::AppSettings;
use crate::services::account_config_service::AccountConfigStore;
use crate::services::camel_mail_service::{self, CamelFolderSummary, CamelMailLoad};
use crate::services::crypto_service::{self, CryptoProtocol, EncryptionStatus, SignatureStatus};
use crate::services::draft_store_service::{DraftMessage, DraftStoreService};
use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::eds_service::{self, EdsSourceDiscovery, EdsSourceKind, EdsSourceOrigin};
use crate::services::eds_source_cache_service::EdsSourceCache;
use crate::services::html_sanitizer::html_to_plain_text;
use crate::services::inline_content_service::InlineImage;
use crate::services::itip_service::{
    ItipInvitation, ItipMethod, ItipReplyStatus, build_reply_component, normalize_attendee_address,
    parse_invitation,
};
use crate::services::mail_composer_service::{self, ExportAttachment, ExportMessage};
use crate::services::mail_filter_service::MailFilterStore;
use crate::services::mail_metadata_service::{
    DEFAULT_FOLLOW_UP_TEXT, MailFollowUpMetadata, MailFollowUpState, display_follow_up_due,
    parse_follow_up_due_input,
};
use crate::services::mail_operations_service::{
    MailCommand, MailCommandStack, MailOperationCancellation, MailOperationContext,
    MailOperationCoordinator, MailOperationError, MailOperationsService,
};
use crate::services::mail_print_service::{
    self, MailPrintAction, MailPrintAttachment, MailPrintJobState, MailPrintSnapshot,
};
use crate::services::mail_store_service::{
    MailBackendLocation, MailStoreService, MailStoreWorkerConfig, ParsedMail, parse_rfc822_bytes,
    parse_rfc822_header_fields,
};
use crate::services::search_folder_service::SearchFolderService;
use crate::views::accessibility::set_accessible_label;
use crate::views::{MailViewer, MessageSearchResult, MessageSearchStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageFilter {
    All,
    Unread,
    Flagged,
    Important,
    HasAttachments,
}

impl MessageFilter {
    fn from_selector_index(index: u32) -> Self {
        match index {
            1 => Self::Unread,
            2 => Self::Flagged,
            3 => Self::Important,
            4 => Self::HasAttachments,
            _ => Self::All,
        }
    }

    fn selector_index(self) -> u32 {
        match self {
            Self::All => 0,
            Self::Unread => 1,
            Self::Flagged => 2,
            Self::Important => 3,
            Self::HasAttachments => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MailSortField {
    #[default]
    Date,
    Subject,
    Sender,
    Unread,
    Flagged,
    FollowUp,
    Important,
    Attachments,
}

fn is_graph_backend_location(location: Option<&MailBackendLocation>) -> bool {
    location.is_some_and(|location| {
        crate::services::exchange_service::graph_service_for_source(&location.source_uid).is_some()
    })
}

impl MailSortField {
    fn from_setting(value: &str) -> Self {
        match value {
            "subject" => Self::Subject,
            "sender" => Self::Sender,
            "unread" | "status" => Self::Unread,
            "flagged" | "starred" => Self::Flagged,
            "follow_up" | "follow-up" => Self::FollowUp,
            "important" => Self::Important,
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
            Self::FollowUp => "follow-up",
            Self::Important => "important",
            Self::Attachments => "attachments",
        }
    }

    fn selector_index(self) -> u32 {
        match self {
            Self::Date => 0,
            Self::Subject => 1,
            Self::Sender => 2,
            Self::Unread => 3,
            Self::Flagged => 4,
            Self::FollowUp => 5,
            Self::Important => 6,
            Self::Attachments => 7,
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
        match value {
            "classic" => Self::Classic,
            _ => Self::Modern,
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

    fn from_folder_id(folder_id: &str) -> Self {
        let folder_id = match folder_id {
            "all" | "inbox" | "sent" | "drafts" | "trash" | "archive" | "spam" => folder_id,
            _ => "inbox",
        };
        Self {
            folder_id: folder_id.to_string(),
            source_uid: None,
            folder_name: None,
        }
    }

    fn configured_initial() -> Self {
        let folder_id = AppSettings::try_new()
            .map(|settings| settings.mail_default_folder())
            .unwrap_or_else(|| "all".to_string());
        Self::from_folder_id(&folder_id)
    }

    fn is_remote(&self) -> bool {
        self.source_uid.is_some()
    }

    fn matches(&self, message: &MailMessageObject) -> bool {
        if let Some(source_uid) = self.source_uid.as_deref() {
            return message
                .backend_location()
                .map(|location| {
                    location.source_uid == source_uid
                        && self
                            .folder_name
                            .as_deref()
                            .is_none_or(|folder_name| location.folder_name == folder_name)
                })
                .unwrap_or(false);
        }

        match self.folder_id.as_str() {
            "all" => true,
            "spam" => message.folder_id() == "junk",
            folder_id => {
                message.folder_id() == folder_id
                    || (folder_id.starts_with("custom:")
                        && message
                            .folder_id()
                            .strip_prefix(folder_id)
                            .is_some_and(|rest| rest.starts_with('/')))
            }
        }
    }

    fn title_and_icon(&self) -> (&str, &str) {
        if let Some(ref name) = self.folder_name {
            (name.as_str(), "folder-symbolic")
        } else {
            match self.folder_id.as_str() {
                "inbox" => ("Posteingang", "mail-inbox-symbolic"),
                "sent" => ("Gesendet", "mail-send-symbolic"),
                "drafts" => ("Entwürfe", "document-edit-symbolic"),
                "trash" => ("Papierkorb", "user-trash-symbolic"),
                "archive" => ("Archiv", "mail-archive-symbolic"),
                "spam" => ("Junk", "mail-mark-junk-symbolic"),
                _ => ("Posteingang", "mail-inbox-symbolic"),
            }
        }
    }
}

impl Default for FolderSelection {
    fn default() -> Self {
        Self::all()
    }
}

fn folder_path_is_within(folder_name: &str, parent_name: &str) -> bool {
    folder_name == parent_name
        || folder_name
            .strip_prefix(parent_name)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn renamed_folder_path(folder_name: &str, old_name: &str, new_name: &str) -> Option<String> {
    if folder_name == old_name {
        return Some(new_name.to_string());
    }
    folder_name
        .strip_prefix(old_name)
        .filter(|suffix| suffix.starts_with('/'))
        .map(|suffix| format!("{new_name}{suffix}"))
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum QuickSearchField {
    #[default]
    AllFields,
    Subject,
    Sender,
    Recipient,
    Body,
}

impl QuickSearchField {
    fn from_selector_index(index: u32) -> Self {
        match index {
            1 => Self::Subject,
            2 => Self::Sender,
            3 => Self::Recipient,
            4 => Self::Body,
            _ => Self::AllFields,
        }
    }

    fn selector_index(self) -> u32 {
        match self {
            Self::AllFields => 0,
            Self::Subject => 1,
            Self::Sender => 2,
            Self::Recipient => 3,
            Self::Body => 4,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum QuickSearchScope {
    #[default]
    CurrentFolder,
    CurrentAccount,
    AllAccounts,
}

impl QuickSearchScope {
    fn from_selector_index(index: u32) -> Self {
        match index {
            1 => Self::CurrentAccount,
            2 => Self::AllAccounts,
            _ => Self::CurrentFolder,
        }
    }

    fn selector_index(self) -> u32 {
        match self {
            Self::CurrentFolder => 0,
            Self::CurrentAccount => 1,
            Self::AllAccounts => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageSearchField {
    From,
    To,
    Cc,
    Bcc,
    Subject,
    Body,
    Label,
    Filename,
    Folder,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct MessageSearchQuery {
    text_terms: Vec<String>,
    negative_text_terms: Vec<String>,
    negative_fields: Vec<(MessageSearchField, String)>,
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
    important: Option<bool>,
    junk: Option<bool>,
    has_attachment: Option<bool>,
    folder: Option<String>,
    date_after: Option<NaiveDate>,
    date_before: Option<NaiveDate>,
    date_not_after: Option<NaiveDate>,
    date_not_before: Option<NaiveDate>,
    date_not_on: Option<NaiveDate>,
}

impl MessageSearchQuery {
    fn parse(input: &str) -> Self {
        Self::parse_with_field_scope(input, QuickSearchField::AllFields)
    }

    fn parse_with_field_scope(input: &str, field_scope: QuickSearchField) -> Self {
        let mut query = Self::default();
        for raw_token in search_tokens(input) {
            let (negated, token) = split_search_negation(&raw_token);
            let Some((raw_key, raw_value)) = token.split_once(':') else {
                match field_scope {
                    QuickSearchField::Subject => {
                        query.push_field(MessageSearchField::Subject, token.to_lowercase(), negated);
                    }
                    QuickSearchField::Sender => {
                        query.push_field(MessageSearchField::From, token.to_lowercase(), negated);
                    }
                    QuickSearchField::Recipient => {
                        query.push_field(MessageSearchField::To, token.to_lowercase(), negated);
                    }
                    QuickSearchField::Body => {
                        query.push_field(MessageSearchField::Body, token.to_lowercase(), negated);
                    }
                    QuickSearchField::AllFields => {
                        query.push_text(token, negated);
                    }
                }
                continue;
            };
            let raw_value = raw_value.trim();
            let value = raw_value.to_lowercase();
            if value.is_empty() {
                query.push_text(token, negated);
                continue;
            }
            let raw_key = raw_key.to_ascii_lowercase();
            let (key, key_negated) = raw_key
                .strip_prefix("not-")
                .map(|key| (key, true))
                .unwrap_or((raw_key.as_str(), false));
            let negated = negated ^ key_negated;
            match key {
                "from" | "sender" => query.push_field(MessageSearchField::From, value, negated),
                "to" | "recipient" => query.push_field(MessageSearchField::To, value, negated),
                "cc" => query.push_field(MessageSearchField::Cc, value, negated),
                "bcc" => query.push_field(MessageSearchField::Bcc, value, negated),
                "subject" => query.push_field(MessageSearchField::Subject, value, negated),
                "body" | "content" => query.push_field(MessageSearchField::Body, value, negated),
                "label" | "tag" | "keyword" => {
                    query.push_field(MessageSearchField::Label, value, negated)
                }
                "filename" | "name" => {
                    query.push_field(MessageSearchField::Filename, value, negated)
                }
                "is" => query.parse_is_value(&value, negated),
                "has" | "no" => {
                    if matches!(value.as_str(), "attachment" | "attachments") {
                        let expected = key == "has";
                        query.has_attachment = Some(expected ^ negated);
                    } else {
                        query.push_text(token, negated);
                    }
                }
                "after" | "newer" => match parse_search_date(raw_value, false) {
                    Some(date) if negated => query.date_not_after = Some(date),
                    Some(date) => query.date_after = Some(date),
                    None => query.push_text(token, negated),
                },
                "before" | "older" => match parse_search_date(raw_value, true) {
                    Some(date) if negated => query.date_not_before = Some(date),
                    Some(date) => query.date_before = Some(date),
                    None => query.push_text(token, negated),
                },
                "on" => match parse_search_date(raw_value, false) {
                    Some(date) if negated => query.date_not_on = Some(date),
                    Some(date) => {
                        query.date_after = Some(date);
                        query.date_before = date.checked_add_signed(Duration::days(1));
                    }
                    None => query.push_text(token, negated),
                },
                "in" | "folder" => query.push_field(MessageSearchField::Folder, value, negated),
                "not" => query.push_text(&value, !negated),
                _ => query.push_text(token, negated),
            }
        }
        query
    }

    fn is_empty(&self) -> bool {
        self.text_terms.is_empty()
            && self.negative_text_terms.is_empty()
            && self.negative_fields.is_empty()
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
            && self.important.is_none()
            && self.junk.is_none()
            && self.has_attachment.is_none()
            && self.folder.is_none()
            && self.date_after.is_none()
            && self.date_before.is_none()
            && self.date_not_after.is_none()
            && self.date_not_before.is_none()
            && self.date_not_on.is_none()
    }

    fn push_text(&mut self, value: &str, negated: bool) {
        if negated {
            self.negative_text_terms.push(value.to_lowercase());
        } else {
            self.text_terms.push(value.to_lowercase());
        }
    }

    fn push_field(&mut self, field: MessageSearchField, value: String, negated: bool) {
        if negated {
            self.negative_fields.push((field, value));
            return;
        }
        match field {
            MessageSearchField::From => self.from = Some(value),
            MessageSearchField::To => self.to = Some(value),
            MessageSearchField::Cc => self.cc = Some(value),
            MessageSearchField::Bcc => self.bcc = Some(value),
            MessageSearchField::Subject => self.subject = Some(value),
            MessageSearchField::Body => self.body = Some(value),
            MessageSearchField::Label => self.label = Some(value),
            MessageSearchField::Filename => self.filename = Some(value),
            MessageSearchField::Folder => self.folder = Some(value),
        }
    }

    fn parse_is_value(&mut self, value: &str, negated: bool) {
        let (value, value_negated) = value
            .strip_prefix("not-")
            .map(|value| (value, true))
            .unwrap_or((value, false));
        let negated = negated ^ value_negated;
        match value {
            "unread" => self.unread = Some(true ^ negated),
            "read" => self.unread = Some(false ^ negated),
            "starred" | "flagged" => self.flagged = Some(true ^ negated),
            "important" => self.important = Some(true ^ negated),
            "junk" | "spam" => self.junk = Some(true ^ negated),
            "attachment" | "attachments" => self.has_attachment = Some(true ^ negated),
            "inbox" | "sent" | "sent-mail" | "draft" | "drafts" | "trash" | "deleted"
            | "archive" => self.push_field(MessageSearchField::Folder, value.to_string(), negated),
            "all" | "all-mail" if !negated => {}
            _ => self.push_text(&format!("is:{value}"), negated),
        }
    }
}

/// Translate only the positive parts of the Evolution-style query that have
/// an equivalent in Outlook/Graph KQL.  The complete parsed query still runs
/// through `message_matches_filter` locally, which preserves negative terms,
/// labels, dates, flags and folder predicates whose semantics are not the
/// same on every Graph mailbox.
fn graph_search_expression(query: &MessageSearchQuery) -> Option<String> {
    let mut clauses = Vec::new();

    for term in &query.text_terms {
        // A free-text token containing `:` could be interpreted as an
        // unsupported KQL property by Graph.  Keep it in the local filter
        // instead of turning a valid local search into a remote 400 response.
        if !term.contains(':') {
            if let Some(value) = escape_graph_search_value(term) {
                clauses.push(format!("\"{value}\""));
            }
        }
    }
    for (field, value) in [
        ("from", query.from.as_deref()),
        ("recipients", query.to.as_deref()),
        ("cc", query.cc.as_deref()),
        ("bcc", query.bcc.as_deref()),
        ("subject", query.subject.as_deref()),
        ("body", query.body.as_deref()),
        ("attachment", query.filename.as_deref()),
    ] {
        let Some(value) = value.and_then(escape_graph_search_value) else {
            continue;
        };
        clauses.push(format!("\"{field}:{value}\""));
    }
    if let Some(has_attachment) = query.has_attachment {
        clauses.push(format!("\"hasAttachments:{has_attachment}\""));
    }

    (!clauses.is_empty()).then(|| clauses.join(" AND "))
}

fn escape_graph_search_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 || value.contains(['\r', '\n']) {
        return None;
    }
    Some(value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn split_search_negation(token: &str) -> (bool, &str) {
    token
        .strip_prefix('-')
        .filter(|value| !value.is_empty())
        .map(|value| (true, value))
        .unwrap_or((false, token))
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
    let value = value.trim();
    if let Some(days) = value
        .strip_suffix('d')
        .and_then(|value| value.parse::<i64>().ok())
    {
        return Some(Local::now().date_naive() - Duration::days(days.max(0)));
    }
    let relative = match value.to_ascii_lowercase().as_str() {
        "today" | "heute" => Some(Local::now().date_naive()),
        "yesterday" | "gestern" => Some(Local::now().date_naive() - Duration::days(1)),
        "tomorrow" | "morgen" => Some(Local::now().date_naive() + Duration::days(1)),
        _ => None,
    };
    let date = relative
        .or_else(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .or_else(|| NaiveDate::parse_from_str(value, "%Y%m%d").ok())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|date| date.date_naive())
        })
        .or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|date| date.date_naive())
        })
        .or_else(|| {
            ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y%m%dT%H%M%S"]
                .into_iter()
                .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
                .map(|date| date.date())
        })?;
    if inclusive_next_day {
        date.checked_add_signed(Duration::days(1))
    } else {
        Some(date)
    }
}

fn message_date(message: &MailMessageObject) -> Option<NaiveDate> {
    let value = message.date_str();
    parse_message_date(&value)
}

fn parse_message_date(value: &str) -> Option<NaiveDate> {
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
        "compact" => (0, 1),
        "spacious" => (7, 14),
        _ => (3, 6),
    }
}

fn mail_row_status_icon(
    is_unread: bool,
    is_replied: bool,
    is_forwarded: bool,
) -> (&'static str, &'static str) {
    if is_replied {
        ("mail-replied-symbolic", "Beantwortet")
    } else if is_forwarded {
        ("mail-forwarded-symbolic", "Weitergeleitet")
    } else if is_unread {
        ("mail-unread-symbolic", "Ungelesen")
    } else {
        ("mail-read-symbolic", "Gelesen")
    }
}

fn mail_row_attachment_info(
    attachments: &[crate::models::mail_message::AttachmentInfo],
) -> Option<(&'static str, &'static str)> {
    if attachments.is_empty() {
        return None;
    }
    for att in attachments {
        let mime = att.mime_type.to_ascii_lowercase();
        let filename = att.filename.to_ascii_lowercase();
        if mime.contains("calendar") || filename.ends_with(".ics") {
            return Some(("x-office-calendar-symbolic", "Termineinladung (.ics)"));
        }
        if mime.contains("vcard") || filename.ends_with(".vcf") {
            return Some(("x-office-address-book-symbolic", "Kontaktkarte (.vcf)"));
        }
    }
    Some(("mail-attachment-symbolic", "Anhang"))
}

fn themed_icon_name<W: IsA<gtk::Widget>>(widget: &W, preferred: &str, fallback: &str) -> String {
    let theme = gtk::IconTheme::for_display(&widget.display());
    if theme.has_icon(preferred) {
        preferred.to_string()
    } else if theme.has_icon(fallback) {
        fallback.to_string()
    } else {
        "image-missing-symbolic".to_string()
    }
}

fn set_image_icon_with_fallback(image: &gtk::Image, preferred: &str, fallback: &str) {
    image.set_icon_name(Some(&themed_icon_name(image, preferred, fallback)));
}

fn set_button_icon_with_fallback(button: &gtk::Button, preferred: &str, fallback: &str) {
    button.set_icon_name(&themed_icon_name(button, preferred, fallback));
}

fn mail_row_expander_visible(thread_size: usize, depth: u32) -> bool {
    depth == 0 && thread_size > 1
}

fn mail_row_is_shaded(position: u32) -> bool {
    position % 2 == 0
}

fn visible_thread_size(threading: bool, stored_size: usize) -> usize {
    if threading { stored_size.max(1) } else { 1 }
}

fn compact_folder_indent(depth: usize) -> i32 {
    1 + (depth as i32).saturating_mul(12)
}

const MAIL_THREAD_COLUMN_WIDTH: i32 = 18;
const MAIL_INDICATOR_COLUMN_WIDTH: i32 = 22;
const MAIL_AVATAR_COLUMN_WIDTH: i32 = 28;
const MAIL_PREVIEW_COLUMN_WIDTH: i32 = 160;
const MAIL_DATE_COLUMN_WIDTH: i32 = 126;

fn unread_badge_state(unread: u32) -> (String, bool) {
    (
        (unread > 0).then(|| unread.to_string()).unwrap_or_default(),
        unread > 0,
    )
}

fn relative_message_index(
    item_count: usize,
    selected: Option<usize>,
    offset: i32,
) -> Option<usize> {
    if item_count == 0 || offset == 0 {
        return None;
    }
    let Some(selected) = selected else {
        return Some(if offset > 0 { 0 } else { item_count - 1 });
    };
    let target = selected as i64 + i64::from(offset);
    (target >= 0 && target < item_count as i64).then_some(target as usize)
}

fn matching_message_index(
    matches: &[bool],
    selected: Option<usize>,
    forward: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if selected.is_none() {
        return if forward {
            matches.iter().position(|matches| *matches)
        } else {
            matches.iter().rposition(|matches| *matches)
        };
    }

    let selected = selected.expect("checked above").min(matches.len() - 1);
    for distance in 1..=matches.len() {
        let index = if forward {
            (selected + distance) % matches.len()
        } else {
            (selected + matches.len() - (distance % matches.len())) % matches.len()
        };
        if matches[index] {
            return Some(index);
        }
    }
    None
}

fn crypto_details_are_visible(details: &crypto_service::MailCryptoDetails) -> bool {
    details.protocol != CryptoProtocol::None
        || details.signature_status != SignatureStatus::None
        || details.encryption_status != EncryptionStatus::NotEncrypted
}

fn crypto_status_visible(show_routine_details: bool, routine_status: bool) -> bool {
    show_routine_details || !routine_status
}

fn raw_mime_may_contain_crypto(raw_mime: &[u8]) -> bool {
    // MIME protocol declarations live in the envelope/header area. Limiting
    // this cheap preflight avoids copying multi-megabyte message bodies on the
    // GTK thread merely to discover that an ordinary mail is not encrypted.
    const PREFLIGHT_BYTES: usize = 128 * 1024;
    let prefix = &raw_mime[..raw_mime.len().min(PREFLIGHT_BYTES)];
    let mime = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    [
        "application/pgp-",
        "application/pkcs7-",
        "application/x-pkcs7-",
        "begin pgp signed message",
        "begin pgp message",
    ]
    .iter()
    .any(|marker| mime.contains(marker))
}

fn selected_mail_message(selection: &gtk::SingleSelection) -> Option<MailMessageObject> {
    selection.selected_item().and_then(|item| {
        item.clone()
            .downcast::<MailMessageObject>()
            .ok()
            .or_else(|| {
                item.downcast::<gtk::TreeListRow>()
                    .ok()
                    .and_then(|row| row.item())
                    .and_then(|object| object.downcast::<MailMessageObject>().ok())
            })
    })
}

fn message_matches_filter(
    message: &MailMessageObject,
    query: &MessageSearchQuery,
    filter: MessageFilter,
    folder_selection: &FolderSelection,
    search_scope: QuickSearchScope,
) -> bool {
    let state_matches = match filter {
        MessageFilter::All => true,
        MessageFilter::Unread => message.is_unread(),
        MessageFilter::Flagged => message.is_flagged(),
        MessageFilter::Important => message.is_important(),
        MessageFilter::HasAttachments => !message.attachments().is_empty(),
    };
    if query.is_empty() {
        return state_matches && folder_selection.matches(message);
    }

    let scope_matches = match search_scope {
        QuickSearchScope::AllAccounts => true,
        QuickSearchScope::CurrentAccount => {
            if let Some(source_uid) = folder_selection.source_uid.as_deref() {
                message
                    .backend_location()
                    .map(|loc| loc.source_uid == source_uid)
                    .unwrap_or(false)
            } else {
                message.backend_location().is_none()
            }
        }
        QuickSearchScope::CurrentFolder => folder_selection.matches(message),
    };

    if !scope_matches || !state_matches {
        return false;
    }

    let subject = message.subject().to_lowercase();
    let sender_name = message.sender_name().to_lowercase();
    let sender_email = message.sender_email().to_lowercase();
    let recipient_to = message.recipient_to().to_lowercase();
    let recipient_cc = message.recipient_cc().to_lowercase();
    let recipient_bcc = message.recipient_bcc().to_lowercase();
    let preview = message.preview_snippet().to_lowercase();
    let searchable_body_text = search_query_needs_body(query)
        .then(|| message.searchable_body_text())
        .flatten();
    let body = searchable_body_text.as_deref().unwrap_or("");
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
        body.as_ref(),
        labels.as_str(),
        filenames.as_str(),
    ]
    .join(" ");
    let text_matches = query
        .text_terms
        .iter()
        .all(|term| searchable_text.contains(term));
    let negative_text_matches = query
        .negative_text_terms
        .iter()
        .all(|term| !searchable_text.contains(term));
    let negative_field_matches = query
        .negative_fields
        .iter()
        .all(|(field, value)| !message_search_field_matches(message, *field, value));
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
            .important
            .map(|important| message.is_important() == important)
            .unwrap_or(true)
        && query
            .junk
            .map(|junk| message_is_junk(message) == junk)
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
            .unwrap_or(true)
        && query
            .date_not_after
            .map(|date| {
                message_date(message)
                    .map(|value| value < date)
                    .unwrap_or(false)
            })
            .unwrap_or(true)
        && query
            .date_not_before
            .map(|date| {
                message_date(message)
                    .map(|value| value >= date)
                    .unwrap_or(false)
            })
            .unwrap_or(true)
        && query
            .date_not_on
            .map(|date| {
                message_date(message)
                    .map(|value| value != date)
                    .unwrap_or(false)
            })
            .unwrap_or(true);
    (query.is_empty()
        || (text_matches && negative_text_matches && negative_field_matches && field_matches))
        && state_matches
        && folder_selection.matches(message)
}

fn search_query_needs_body(query: &MessageSearchQuery) -> bool {
    !query.text_terms.is_empty()
        || !query.negative_text_terms.is_empty()
        || query.body.is_some()
        || query
            .negative_fields
            .iter()
            .any(|(field, _)| *field == MessageSearchField::Body)
}

fn message_search_field_matches(
    message: &MailMessageObject,
    field: MessageSearchField,
    value: &str,
) -> bool {
    let value = value.to_ascii_lowercase();
    match field {
        MessageSearchField::From => {
            message.sender_name().to_ascii_lowercase().contains(&value)
                || message.sender_email().to_ascii_lowercase().contains(&value)
        }
        MessageSearchField::To => [
            message.recipient_to(),
            message.recipient_cc(),
            message.recipient_bcc(),
        ]
        .into_iter()
        .any(|recipient| recipient.to_ascii_lowercase().contains(&value)),
        MessageSearchField::Cc => message.recipient_cc().to_ascii_lowercase().contains(&value),
        MessageSearchField::Bcc => message
            .recipient_bcc()
            .to_ascii_lowercase()
            .contains(&value),
        MessageSearchField::Subject => message.subject().to_ascii_lowercase().contains(&value),
        MessageSearchField::Body => {
            message
                .searchable_body_text()
                .is_some_and(|body| body.contains(&value))
                || message
                    .preview_snippet()
                    .to_ascii_lowercase()
                    .contains(&value)
        }
        MessageSearchField::Label => message
            .labels()
            .iter()
            .any(|label| label.to_ascii_lowercase().contains(&value)),
        MessageSearchField::Filename => message
            .attachments()
            .into_iter()
            .any(|attachment| attachment.filename.to_ascii_lowercase().contains(&value)),
        MessageSearchField::Folder => message_folder_matches(message, &value),
    }
}

fn message_is_junk(message: &MailMessageObject) -> bool {
    let folder_id = message.folder_id().to_ascii_lowercase();
    folder_id == "junk"
        || folder_id == "spam"
        || folder_id.starts_with("junk/")
        || message_folder_matches(message, "junk")
}

fn message_is_trash(message: &MailMessageObject) -> bool {
    let folder_id = message.folder_id().to_ascii_lowercase();
    folder_id == "trash"
        || folder_id == "deleted"
        || folder_id.starts_with("trash/")
        || message_folder_matches(message, "trash")
        || message.backend_location().is_some_and(|location| {
            let folder_name = location.folder_name.to_ascii_lowercase();
            folder_name.contains("trash")
                || folder_name.contains("deleted")
                || folder_name.contains("mülleimer")
                || folder_name.contains("papierkorb")
        })
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
        "archive" => folder == "archive",
        "all" | "all-mail" => true,
        value => {
            let local_path = folder.trim_start_matches("custom:");
            folder == value
                || local_path == value
                || local_path
                    .strip_prefix(value)
                    .is_some_and(|rest| rest.starts_with('/'))
        }
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

fn export_message_from(message: &MailMessageObject) -> ExportMessage {
    ExportMessage {
        from: sender_header(message),
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
    }
}

fn print_snapshot_from(message: &MailMessageObject) -> MailPrintSnapshot {
    let message_attachments = message.attachments();
    let attachments = message_attachments
        .iter()
        .filter(|attachment| attachment.content_id.is_none())
        .map(|attachment| MailPrintAttachment {
            filename: attachment.filename.clone(),
            mime_type: attachment.mime_type.clone(),
            size_formatted: attachment.size_formatted.clone(),
        })
        .collect::<Vec<_>>();
    let inline_images = message_attachments
        .iter()
        .filter_map(|attachment| {
            attachment.content_id.clone().map(|content_id| {
                InlineImage::new(
                    content_id,
                    attachment.mime_type.clone(),
                    attachment.content.clone(),
                )
            })
        })
        .collect::<Vec<_>>();

    MailPrintSnapshot {
        message_key: message.id(),
        sender: sender_header(message),
        reply_to: message.reply_to(),
        to: message.recipient_to(),
        cc: message.recipient_cc(),
        bcc: message.recipient_bcc(),
        subject: message.subject(),
        date: message.date_str(),
        html_body: message.html_body(),
        fallback_text: message.preview_snippet(),
        inline_images,
        attachments,
    }
}

fn rfc822_header_block(raw: &[u8]) -> &[u8] {
    for separator in [
        b"\r\n\r\n".as_slice(),
        b"\n\n".as_slice(),
        b"\r\r".as_slice(),
    ] {
        if let Some(offset) = raw
            .windows(separator.len())
            .position(|window| window == separator)
        {
            return &raw[..offset];
        }
    }
    raw
}

fn all_headers_request_is_current(
    cancelled: bool,
    expected_generation: u64,
    current_generation: u64,
    visible: bool,
    same_message: bool,
) -> bool {
    !cancelled && expected_generation == current_generation && visible && same_message
}

fn message_header_fields(message: &MailMessageObject) -> Vec<(String, String)> {
    let headers = if let Some(headers) = message.raw_headers() {
        headers
    } else {
        // The header fallback must stay cheap: constructing the complete export
        // would otherwise clone and Base64-encode every attachment on selection.
        let mut envelope = export_message_from(message);
        envelope.html_body.clear();
        envelope.attachments.clear();
        let Ok(source) = mail_composer_service::build_export_rfc822(&envelope) else {
            return Vec::new();
        };
        Arc::<[u8]>::from(rfc822_header_block(&source))
    };
    parse_rfc822_header_fields(&headers)
}

fn normal_header_display_name(name: &str) -> &str {
    match name.to_ascii_lowercase().as_str() {
        "from" => "Von",
        "reply-to" => "Antwort an",
        "to" => "An",
        "cc" => "Cc",
        "bcc" => "Bcc",
        "subject" => "Betreff",
        "date" => "Datum",
        "newsgroups" => "Newsgroups",
        "face" => "Face",
        "x-evolution-mailer" => "Mailer",
        _ => name,
    }
}

fn default_normal_header_configuration() -> Vec<(String, bool)> {
    [
        ("From", true),
        ("Reply-To", false),
        ("To", true),
        ("Cc", true),
        ("Bcc", true),
        ("Subject", false),
        ("Date", true),
        ("Newsgroups", true),
        ("Face", true),
        ("x-evolution-mailer", false),
    ]
    .into_iter()
    .map(|(name, enabled)| (name.to_string(), enabled))
    .collect()
}

fn configured_normal_header_fields(
    fields: &[(String, String)],
    configuration: &[(String, bool)],
) -> Vec<(String, String)> {
    let mut selected = Vec::new();
    let from_value = fields
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("from"))
        .map(|(_, val)| val.trim().to_lowercase());

    for (configured_name, enabled) in configuration {
        if !enabled {
            continue;
        }
        let configured_name = configured_name.trim();
        let is_reply_to = configured_name.eq_ignore_ascii_case("reply-to");
        let mailer_pseudo_header = configured_name.eq_ignore_ascii_case("x-evolution-mailer");
        for (actual_name, value) in fields {
            let matches = actual_name.eq_ignore_ascii_case(configured_name)
                || mailer_pseudo_header
                    && ["x-mailer", "user-agent", "x-newsreader"]
                        .iter()
                        .any(|name| actual_name.eq_ignore_ascii_case(name));
            if matches && !value.trim().is_empty() {
                if is_reply_to {
                    let reply_to_val = value.trim().to_lowercase();
                    if let Some(ref from_val) = from_value {
                        if reply_to_val == *from_val || from_val.contains(&reply_to_val) {
                            continue;
                        }
                    }
                }
                let value = if configured_name.eq_ignore_ascii_case("date") {
                    format_message_date(value)
                } else {
                    value.clone()
                };
                selected.push((
                    normal_header_display_name(configured_name).to_string(),
                    value,
                ));
            }
        }
    }
    selected
}

fn replace_normal_header_rows(grid: &gtk::Grid, fields: &[(String, String)]) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    for (row, (name, value)) in fields.iter().enumerate() {
        let name_label = gtk::Label::builder()
            .label(format!("{name}:"))
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .xalign(1.0)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        grid.attach(&name_label, 0, row as i32, 1, 1);
        if name == "Face" {
            let face = crate::services::vcard_service::decode_base64(value)
                .map(glib::Bytes::from_owned)
                .and_then(|bytes| gtk::gdk::Texture::from_bytes(&bytes).ok());
            if let Some(texture) = face {
                let avatar = adw::Avatar::builder()
                    .size(48)
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Center)
                    .build();
                avatar.set_custom_image(Some(&texture));
                grid.attach(&avatar, 1, row as i32, 1, 1);
            } else {
                let invalid = gtk::Label::builder()
                    .label("Absenderbild konnte nicht angezeigt werden")
                    .halign(gtk::Align::Start)
                    .css_classes(vec!["caption", "dim-label"])
                    .build();
                grid.attach(&invalid, 1, row as i32, 1, 1);
            }
            continue;
        }
        let mut classes = vec!["mail-normal-header-value"];
        if name == "Betreff" {
            classes.push("title-4");
        }
        let value_label = gtk::Label::builder()
            .label(value)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Center)
            .xalign(0.0)
            .hexpand(true)
            .selectable(true)
            .ellipsize(pango::EllipsizeMode::End)
            .lines(1)
            .css_classes(classes)
            .build();
        grid.attach(&value_label, 1, row as i32, 1, 1);
    }
}

fn replace_formatted_header_rows(grid: &gtk::Grid, fields: &[(String, String)]) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    for (row, (name, value)) in fields.iter().enumerate() {
        let name = gtk::Label::builder()
            .label(format!("{name}:"))
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .xalign(1.0)
            .selectable(true)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        let value = gtk::Label::builder()
            .label(value)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .selectable(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .build();
        grid.attach(&name, 0, row as i32, 1, 1);
        grid.attach(&value, 1, row as i32, 1, 1);
    }
}

fn replace_formatted_header_status(grid: &gtk::Grid, status: &str) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    let label = gtk::Label::builder()
        .label(status)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .css_classes(vec!["caption", "dim-label"])
        .build();
    grid.attach(&label, 0, 0, 2, 1);
}

fn displayed_message_search_status(result: &MessageSearchResult) -> String {
    match result.status {
        MessageSearchStatus::EmptyQuery => String::new(),
        MessageSearchStatus::NoDisplayedMessage => "Keine Nachricht angezeigt".to_string(),
        MessageSearchStatus::Pending => "Suche …".to_string(),
        MessageSearchStatus::Found | MessageSearchStatus::NotFound => {
            format!("Treffer: {}", result.match_count)
        }
    }
}

fn displayed_message_search_has_matches(result: &MessageSearchResult) -> bool {
    result.status == MessageSearchStatus::Found && result.match_count > 0
}

fn update_displayed_message_search_status(
    label: &gtk::Label,
    find_previous: &gtk::Button,
    find_next: &gtk::Button,
    result: &MessageSearchResult,
) {
    label.set_label(&displayed_message_search_status(result));
    let sensitive = displayed_message_search_has_matches(result);
    find_previous.set_sensitive(sensitive);
    find_next.set_sensitive(sensitive);
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

#[derive(Debug)]
struct MessageSourceWindowState {
    window: adw::Window,
    stack: gtk::Stack,
    source_buffer: gtk::TextBuffer,
    warning_label: gtk::Label,
    error_page: adw::StatusPage,
    copy_button: gtk::Button,
    cancellation: Arc<AtomicBool>,
}

type MessageSourceLoadResult = Result<(Arc<[u8]>, Option<String>), String>;

fn message_source_window_key(message: &MailMessageObject) -> String {
    if let Some(location) = message.backend_location() {
        format!(
            "backend\u{1f}{}\u{1f}{}\u{1f}{}",
            location.source_uid, location.folder_name, location.message_uid
        )
    } else {
        format!(
            "portable\u{1f}{}\u{1f}{}",
            message.folder_id(),
            message.id()
        )
    }
}

fn message_source_result_is_current(
    cancelled: bool,
    window_is_open: bool,
    same_request: bool,
) -> bool {
    !cancelled && window_is_open && same_request
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
    graph_cursors: HashMap<String, String>,
}

fn configured_mail_page_limit() -> i32 {
    AppSettings::try_new()
        .map(|settings| settings.mail_sync_message_limit() as i32)
        .unwrap_or_else(camel_mail_service::default_page_limit)
}

impl MailLoadRequest {
    fn initial() -> Self {
        Self {
            source_uid: None,
            folder_name: None,
            offset: 0,
            max_messages_per_folder: configured_mail_page_limit(),
            graph_cursors: HashMap::new(),
        }
    }

    fn is_remote_or_paged(&self) -> bool {
        self.source_uid.is_some() || self.folder_name.is_some() || self.offset > 0
    }
}

/// Immutable UI identity captured before a mail-side worker starts.  A
/// completed operation may still commit confirmed backend/history state, but
/// it may only refresh or report through the UI while this folder/load
/// identity is still current.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MailWorkerUiScope {
    load_generation: u64,
    folder_selection: FolderSelection,
    active_request: Option<MailLoadRequest>,
}

fn mail_worker_scope_is_current(
    expected: &MailWorkerUiScope,
    current_generation: u64,
    current_folder_selection: &FolderSelection,
    current_request: Option<&MailLoadRequest>,
) -> bool {
    expected.load_generation == current_generation
        && expected.folder_selection == *current_folder_selection
        && expected.active_request.as_ref() == current_request
}

/// Owned, framework-neutral input for one mail load worker.
///
/// This value is captured on the GTK thread before the worker is spawned.  It
/// deliberately contains only the request and settings values needed by the
/// backend; no `AppSettings`, GSettings, GTK, or GObject handle may cross this
/// boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MailLoadWorkerInput {
    request: MailLoadRequest,
    offline_mode: bool,
    store_config: MailStoreWorkerConfig,
}

/// The small settings value needed by a mail load is converted to owned Rust
/// data before the store/environment snapshot is captured.  Keeping this
/// separate makes it explicit that no GObject-backed settings handle is part
/// of a worker input.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MailLoadSettingsSnapshot {
    offline_mode: bool,
    local_mail_directory: Option<String>,
}

impl MailLoadSettingsSnapshot {
    fn from_main_thread() -> Self {
        let settings = AppSettings::try_new();
        let (offline_mode, local_mail_directory) = settings
            .map(|settings| {
                let offline_mode = settings.offline_mode();
                let local_mail_directory = settings.local_mail_directory();
                (offline_mode, Some(local_mail_directory))
            })
            .unwrap_or((false, None));

        Self {
            offline_mode,
            local_mail_directory,
        }
    }
}

impl MailLoadWorkerInput {
    fn from_main_thread(request: MailLoadRequest) -> Self {
        Self::from_settings_snapshot(request, MailLoadSettingsSnapshot::from_main_thread())
    }

    fn from_settings_snapshot(
        request: MailLoadRequest,
        settings: MailLoadSettingsSnapshot,
    ) -> Self {
        Self {
            request,
            offline_mode: settings.offline_mode,
            store_config: MailStoreWorkerConfig::capture(settings.local_mail_directory),
        }
    }
}

/// Capture the complete store policy on the GTK/main thread before a caller
/// starts a worker.  The returned value is plain owned data; worker closures
/// must pass it to `MailStoreService::new_worker_safe` and never resolve the
/// settings-backed constructor themselves.
fn mail_store_worker_config_from_main_thread() -> MailStoreWorkerConfig {
    let local_mail_directory =
        AppSettings::try_new().map(|settings| settings.local_mail_directory());
    MailStoreWorkerConfig::capture(local_mail_directory)
}

enum MailPageLoadResult {
    Camel {
        camel: CamelMailLoad,
        searchable_body_texts: SearchableBodyTextCache,
    },
    Combined {
        camel: CamelMailLoad,
        local_messages: Vec<ParsedMail>,
        local_folders: HashSet<String>,
        searchable_body_texts: SearchableBodyTextCache,
    },
    Local {
        reason: String,
        messages: Vec<ParsedMail>,
        local_folders: HashSet<String>,
        searchable_body_texts: SearchableBodyTextCache,
    },
    Error(String),
}

type SearchableBodyTextCache = HashMap<MailMessageIdentity, Arc<str>>;

/// Prepare the body index while the backend result is still owned by a
/// worker.  The GTK filter only consumes the resulting immutable Arc values;
/// it must never turn a message's HTML into plaintext on demand.
fn prepare_searchable_body_text(html_body: &str) -> Arc<str> {
    html_to_plain_text(html_body).to_ascii_lowercase().into()
}

fn parsed_mail_identity(message: &ParsedMail) -> MailMessageIdentity {
    MailMessageIdentity::from_parts(
        &message.id,
        &message.folder_id,
        message.message_id.as_deref(),
        message.backend_location.as_ref(),
    )
}

fn insert_searchable_body_text_first(
    cache: &mut SearchableBodyTextCache,
    identity: MailMessageIdentity,
    html_body: &str,
) {
    cache
        .entry(identity)
        .or_insert_with(|| prepare_searchable_body_text(html_body));
}

fn prepare_searchable_body_texts(
    messages: &[ParsedMail],
    cancelled: &AtomicBool,
) -> Result<SearchableBodyTextCache, String> {
    let mut texts = HashMap::with_capacity(messages.len());
    for message in messages {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Mail-Suchindex wurde abgebrochen".to_string());
        }
        if !message.id.trim().is_empty() {
            insert_searchable_body_text_first(
                &mut texts,
                parsed_mail_identity(message),
                &message.html_body,
            );
        }
    }
    Ok(texts)
}

/// Merge the authoritative Camel cache before supplemental local records.
/// Exact identity collisions are first-wins; a local body can never replace
/// the body prepared for the Camel row that will be displayed for that ID.
fn merge_authoritative_searchable_body_texts(
    mut authoritative: SearchableBodyTextCache,
    supplemental: SearchableBodyTextCache,
) -> SearchableBodyTextCache {
    for (identity, text) in supplemental {
        authoritative.entry(identity).or_insert(text);
    }
    authoritative
}

fn prepared_camel_result(camel: CamelMailLoad, cancelled: &AtomicBool) -> MailPageLoadResult {
    match prepare_searchable_body_texts(&camel.messages, cancelled) {
        Ok(searchable_body_texts) => MailPageLoadResult::Camel {
            camel,
            searchable_body_texts,
        },
        Err(error) => MailPageLoadResult::Error(error),
    }
}

const MAIL_MODEL_CHUNK_SIZE: usize = 32;
const MAIL_SORT_WORKER_THRESHOLD: usize = 128;
const MESSAGE_CONTEXT_ACTIONS: &[(&str, &str)] = &[
    ("Antworten", "win.reply"),
    ("Allen antworten", "win.reply-all"),
    ("Weiterleiten", "win.forward"),
    ("Als EML speichern …", "win.export"),
    ("Drucken …", "win.mail-print"),
    ("Druckvorschau …", "win.mail-print-preview"),
    ("Nachrichtenquelle anzeigen", "win.view-message-source"),
    ("Als gelesen markieren", "win.mark-read"),
    ("Als ungelesen markieren", "win.mark-unread"),
    ("Markierung umschalten", "win.toggle-flag"),
    ("Wichtigkeitsmarkierung umschalten", "win.toggle-important"),
    ("Labels bearbeiten …", "win.edit-labels"),
    ("In Ordner verschieben …", "win.move-message"),
    ("In Ordner kopieren …", "win.copy-message"),
    ("Archivieren", "win.archive"),
    ("Als Spam markieren", "win.mark-junk"),
    ("Als Nicht-Spam markieren", "win.mark-not-junk"),
    ("Löschen", "win.delete"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingMailApplyStage {
    Camel,
    Local,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingThreadModelStage {
    ReadStore,
    CollectMessages,
    CollectThreadSortKeys,
    AwaitThreadSort,
    ApplyThreadSort,
    CollectRootSortKeys,
    AwaitRootSort,
    ApplyRootSort,
    AppendRoots,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum MailSortKey {
    Date(Option<NaiveDate>, String),
    Text(String, String),
    Sender(String, String, String),
    State(bool),
    Count(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MailRootSortRecord {
    key: MailSortKey,
    id: String,
    index: usize,
}

impl Ord for MailRootSortRecord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| self.id.cmp(&other.id))
            .then_with(|| self.index.cmp(&other.index))
    }
}

impl PartialOrd for MailRootSortRecord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ThreadMemberSortRecord {
    position: usize,
    id: String,
    index: usize,
}

struct PendingThreadBuild {
    thread_id: String,
    members: Vec<MailMessageObject>,
    sort_records: Vec<ThreadMemberSortRecord>,
    sort_order: Vec<usize>,
    sorted_members: Vec<MailMessageObject>,
    next_member: usize,
    children: gio::ListStore,
}

impl PendingThreadBuild {
    fn new(thread_id: String, members: Vec<MailMessageObject>) -> Self {
        let capacity = members.len();
        Self {
            thread_id,
            members,
            sort_records: Vec::with_capacity(capacity),
            sort_order: Vec::new(),
            sorted_members: Vec::with_capacity(capacity),
            next_member: 0,
            children: gio::ListStore::new::<MailMessageObject>(),
        }
    }
}

fn sorted_record_indices<T>(mut records: Vec<T>, descending: bool) -> Vec<usize>
where
    T: Ord + RecordIndex,
{
    records.sort();
    if descending {
        records.reverse();
    }
    records.into_iter().map(|record| record.index()).collect()
}

trait RecordIndex {
    fn index(&self) -> usize;
}

impl RecordIndex for MailRootSortRecord {
    fn index(&self) -> usize {
        self.index
    }
}

impl RecordIndex for ThreadMemberSortRecord {
    fn index(&self) -> usize {
        self.index
    }
}

fn spawn_record_sort<T>(records: Vec<T>, descending: bool) -> Receiver<Vec<usize>>
where
    T: Ord + RecordIndex + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(sorted_record_indices(records, descending));
    });
    receiver
}

fn mail_root_sort_record(
    message: &MailMessageObject,
    field: MailSortField,
    index: usize,
) -> MailRootSortRecord {
    let key = match field {
        MailSortField::Date => {
            let raw = message.date_str();
            MailSortKey::Date(parse_message_date(&raw), raw)
        }
        MailSortField::Subject => {
            let value = message.subject();
            MailSortKey::Text(value.to_lowercase(), value)
        }
        MailSortField::Sender => {
            let name = message.sender_name();
            let email = message.sender_email();
            MailSortKey::Sender(name.to_lowercase(), email.to_lowercase(), name)
        }
        MailSortField::Unread => MailSortKey::State(message.is_unread()),
        MailSortField::Flagged => MailSortKey::State(message.is_flagged()),
        MailSortField::FollowUp => {
            use crate::services::mail_metadata_service::MailFollowUpState;
            MailSortKey::Count(match message.follow_up_state() {
                MailFollowUpState::Completed => 2,
                MailFollowUpState::FollowUp => 1,
                MailFollowUpState::None => 0,
            })
        }
        MailSortField::Important => MailSortKey::State(message.is_important()),
        MailSortField::Attachments => MailSortKey::Count(message.attachment_count()),
    };
    MailRootSortRecord {
        key,
        id: message.id(),
        index,
    }
}

/// State for applying a backend result to GTK models without monopolising the
/// main context.  EDS/Camel already runs in a worker; this is the second half
/// of the same boundary: creating GObjects, list-model entries and thread
/// models must happen on GTK's thread, but it must yield between small chunks.
struct PendingMailApply {
    started: Instant,
    generation: u64,
    request: MailLoadRequest,
    replace: bool,
    previous_selection: MailSelectionAnchor,
    pending_search: Option<String>,
    result_kind: &'static str,
    stage: PendingMailApplyStage,
    active_messages: Vec<ParsedMail>,
    local_messages: Option<Vec<ParsedMail>>,
    searchable_body_texts: SearchableBodyTextCache,
    combined: bool,
    local_folders: HashSet<String>,
    local_counts: HashMap<String, (u32, u32)>,
    existing_ids: HashSet<String>,
    appended: usize,
    camel_folders: Vec<CamelFolderSummary>,
    camel_status: Option<String>,
    camel_has_more: bool,
    camel_warning_visible: bool,
    camel_is_graph: bool,
    camel_search_applied: bool,
    local_reason: Option<String>,
}

struct PendingMailCompletion {
    generation: u64,
    started: Instant,
    replace: bool,
    previous_selection: MailSelectionAnchor,
    is_local: bool,
    appended: usize,
    has_visible_local_content: bool,
    pending_search: Option<String>,
    result_kind: &'static str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct MailSelectionAnchor {
    id: Option<String>,
    index: Option<u32>,
}

fn fallback_selection_index(item_count: u32, previous_index: Option<u32>) -> Option<u32> {
    if item_count == 0 {
        return None;
    }
    Some(previous_index.unwrap_or(0).min(item_count - 1))
}

fn generation_is_current(current: u64, result_generation: u64) -> bool {
    current == result_generation
}

fn advance_generation(current: u64) -> u64 {
    current.wrapping_add(1).max(1)
}

fn crypto_result_is_current(
    active_generation: u64,
    result_generation: u64,
    active_identity: &MailMessageIdentity,
    result_identity: &MailMessageIdentity,
) -> bool {
    active_generation == result_generation && active_identity == result_identity
}

fn camel_folder_counter_delta(
    change: &camel_mail_service::CamelChangeEvent,
    previous_unread: &HashMap<String, bool>,
    replacements: &[ParsedMail],
) -> (i64, i64) {
    let added = change
        .added_uids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let removed = change
        .removed_uids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let total_delta = added.len() as i64 - removed.len() as i64;
    let mut unread_delta = 0i64;
    for uid in removed {
        if previous_unread.get(uid).copied() == Some(true) {
            unread_delta -= 1;
        }
    }
    let mut seen_replacements = HashSet::new();
    for message in replacements {
        let Some(location) = message.backend_location.as_ref() else {
            continue;
        };
        if !seen_replacements.insert(location.message_uid.as_str()) {
            continue;
        }
        let before = previous_unread.get(&location.message_uid).copied();
        match (before, message.is_unread) {
            (Some(true), false) => unread_delta -= 1,
            (Some(false), true) => unread_delta += 1,
            (None, true) if added.contains(location.message_uid.as_str()) => unread_delta += 1,
            _ => {}
        }
    }
    (unread_delta, total_delta)
}

fn apply_count_delta(value: u32, delta: i64) -> u32 {
    if delta >= 0 {
        value.saturating_add(delta.min(u32::MAX as i64) as u32)
    } else {
        value.saturating_sub(delta.unsigned_abs().min(u32::MAX as u64) as u32)
    }
}

struct PendingThreadModelApply {
    generation: u64,
    store: gio::ListStore,
    messages: Vec<MailMessageObject>,
    next_message: usize,
    thread_order: Vec<String>,
    grouped_messages: HashMap<String, Vec<MailMessageObject>>,
    next_thread: usize,
    current_thread: Option<PendingThreadBuild>,
    thread_sort_receiver: Option<Receiver<Vec<usize>>>,
    roots: Vec<MailMessageObject>,
    root_sort_records: Vec<MailRootSortRecord>,
    root_sort_order: Vec<usize>,
    sorted_roots: Vec<MailMessageObject>,
    next_root: usize,
    root_sort_receiver: Option<Receiver<Vec<usize>>>,
    thread_members: HashMap<String, Vec<MailMessageObject>>,
    thread_children: HashMap<String, gio::ListStore>,
    threading: bool,
    stage: PendingThreadModelStage,
    root_store: Option<gio::ListStore>,
    root_index: usize,
    completion: PendingMailCompletion,
}

impl PendingThreadModelApply {
    fn from_store(
        generation: u64,
        store: gio::ListStore,
        threading: bool,
        completion: PendingMailCompletion,
    ) -> Self {
        Self {
            generation,
            store,
            messages: Vec::new(),
            next_message: 0,
            thread_order: Vec::new(),
            grouped_messages: HashMap::new(),
            next_thread: 0,
            current_thread: None,
            thread_sort_receiver: None,
            roots: Vec::new(),
            root_sort_records: Vec::new(),
            root_sort_order: Vec::new(),
            sorted_roots: Vec::new(),
            next_root: 0,
            root_sort_receiver: None,
            thread_members: HashMap::new(),
            thread_children: HashMap::new(),
            threading,
            stage: PendingThreadModelStage::ReadStore,
            root_store: None,
            root_index: 0,
            completion,
        }
    }
}

impl PendingMailApply {
    fn from_camel(
        mut request: MailLoadRequest,
        mut camel: CamelMailLoad,
        local_messages: Option<Vec<ParsedMail>>,
        searchable_body_texts: SearchableBodyTextCache,
        local_folders: HashSet<String>,
        replace: bool,
        previous_selection: MailSelectionAnchor,
        pending_search: Option<String>,
        generation: u64,
    ) -> Self {
        let status = format_camel_status(&camel);
        let has_more = camel.search_query.is_none()
            && if camel.is_graph {
                !camel.graph_next_links.is_empty()
            } else {
                camel.has_more_for(
                    request.source_uid.as_deref(),
                    request.folder_name.as_deref(),
                    request.offset,
                    request.max_messages_per_folder,
                )
            };
        request.graph_cursors = camel.graph_next_links.clone();
        let camel_warning_visible =
            camel.warning.is_some() || !camel.folders.iter().any(|folder| folder.is_selectable);
        let messages = std::mem::take(&mut camel.messages);
        let camel_folders = camel.folders;

        let combined = local_messages.is_some();
        Self {
            started: Instant::now(),
            generation,
            request,
            replace,
            previous_selection,
            pending_search,
            result_kind: if combined { "combined" } else { "camel" },
            stage: PendingMailApplyStage::Camel,
            active_messages: messages,
            combined,
            local_messages,
            searchable_body_texts,
            local_folders,
            local_counts: HashMap::new(),
            existing_ids: HashSet::new(),
            appended: 0,
            camel_folders,
            camel_status: Some(status),
            camel_has_more: has_more,
            camel_warning_visible,
            camel_is_graph: camel.is_graph,
            camel_search_applied: camel.search_query.is_some(),
            local_reason: None,
        }
    }

    fn from_local(
        request: MailLoadRequest,
        reason: String,
        messages: Vec<ParsedMail>,
        searchable_body_texts: SearchableBodyTextCache,
        local_folders: HashSet<String>,
        replace: bool,
        previous_selection: MailSelectionAnchor,
        generation: u64,
    ) -> Self {
        Self {
            started: Instant::now(),
            generation,
            request,
            replace,
            previous_selection,
            pending_search: None,
            result_kind: "local",
            stage: PendingMailApplyStage::Local,
            active_messages: messages,
            combined: false,
            local_messages: None,
            searchable_body_texts,
            local_folders,
            local_counts: HashMap::new(),
            existing_ids: HashSet::new(),
            appended: 0,
            camel_folders: Vec::new(),
            camel_status: None,
            camel_has_more: false,
            camel_warning_visible: false,
            camel_is_graph: false,
            camel_search_applied: false,
            local_reason: Some(reason),
        }
    }
}

fn load_mail_backend(
    input: MailLoadWorkerInput,
    cancelled: &Arc<AtomicBool>,
    progress_sender: Option<&async_channel::Sender<camel_mail_service::CamelSyncProgress>>,
) -> MailPageLoadResult {
    let MailLoadWorkerInput {
        request,
        offline_mode,
        store_config,
    } = input;
    let started = Instant::now();
    tracing::debug!(
        source_uid = ?request.source_uid,
        folder_name = ?request.folder_name,
        offset = request.offset,
        limit = request.max_messages_per_folder,
        "Mail backend load started"
    );
    if offline_mode {
        let store = crate::services::MailStoreService::new_worker_safe(store_config.clone());
        let messages = store.load_local_records_cancelable(cancelled);
        let searchable_body_texts = match prepare_searchable_body_texts(&messages, cancelled) {
            Ok(texts) => texts,
            Err(error) => return MailPageLoadResult::Error(error),
        };
        return MailPageLoadResult::Local {
            reason: "Offline-Modus: lokaler Evolution-Speicher".to_string(),
            messages,
            local_folders: store.local_folder_definitions(),
            searchable_body_texts,
        };
    }
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Starting synchronous EDS/Camel work on mail worker"
    );
    let camel_result = camel_mail_service::load_messages_page_with_progress_and_cancellation(
        request.source_uid.as_deref(),
        request.folder_name.as_deref(),
        request.offset,
        request.max_messages_per_folder,
        Some(cancelled),
        progress_sender,
    );
    if let Ok(ref camel) = camel_result {
        if !camel.messages.is_empty() || !camel.folders.is_empty() {
            camel_mail_service::save_mail_load_cache(camel);
        }
    }
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        result = if camel_result.is_ok() { "ok" } else { "error" },
        "Mail backend returned from EDS/Camel"
    );

    if cancelled.load(Ordering::Relaxed) {
        return MailPageLoadResult::Error("Mail-Ladevorgang wurde abgebrochen.".to_string());
    }

    if request.is_remote_or_paged() {
        return match camel_result {
            Ok(camel) => prepared_camel_result(camel, cancelled),
            Err(error) => match camel_mail_service::load_messages_graph(
                request.source_uid.as_deref(),
                request.folder_name.as_deref(),
                request.offset,
                request.max_messages_per_folder,
                &request.graph_cursors,
                cancelled,
            ) {
                Ok(graph) => prepared_camel_result(graph, cancelled),
                Err(graph_error) => MailPageLoadResult::Error(format!(
                    "Mailkonto-Synchronisation nicht verfügbar ({error}); Online-Suche ebenfalls nicht verfügbar ({graph_error})"
                )),
            },
        };
    }

    match camel_result {
        Ok(camel)
            if camel.configured_sources > 0
                || !camel.messages.is_empty()
                || !camel.folders.is_empty() =>
        {
            let store = crate::services::MailStoreService::new_worker_safe(store_config.clone());
            let local_folders = store.local_folder_definitions();
            let local_messages = store.load_local_records_cancelable(cancelled);
            let mut searchable_body_texts =
                match prepare_searchable_body_texts(&camel.messages, cancelled) {
                    Ok(texts) => texts,
                    Err(error) => return MailPageLoadResult::Error(error),
                };
            let local_searchable_body_texts =
                match prepare_searchable_body_texts(&local_messages, cancelled) {
                    Ok(texts) => texts,
                    Err(error) => return MailPageLoadResult::Error(error),
                };
            searchable_body_texts = merge_authoritative_searchable_body_texts(
                searchable_body_texts,
                local_searchable_body_texts,
            );
            if local_messages.is_empty() && local_folders.is_empty() {
                MailPageLoadResult::Camel {
                    camel,
                    searchable_body_texts,
                }
            } else {
                MailPageLoadResult::Combined {
                    camel,
                    local_messages,
                    local_folders,
                    searchable_body_texts,
                }
            }
        }
        Ok(_) => {
            if let Ok(graph) = camel_mail_service::load_messages_graph(
                request.source_uid.as_deref(),
                request.folder_name.as_deref(),
                request.offset,
                request.max_messages_per_folder,
                &request.graph_cursors,
                cancelled,
            ) {
                return prepared_camel_result(graph, cancelled);
            }
            let store = crate::services::MailStoreService::new_worker_safe(store_config.clone());
            let messages = store.load_local_records_cancelable(cancelled);
            let searchable_body_texts = match prepare_searchable_body_texts(&messages, cancelled) {
                Ok(texts) => texts,
                Err(error) => return MailPageLoadResult::Error(error),
            };
            MailPageLoadResult::Local {
                reason: "Keine aktiven Mailkonten; lokaler Evolution-Speicher".to_string(),
                messages,
                local_folders: store.local_folder_definitions(),
                searchable_body_texts,
            }
        }
        Err(error) => {
            if let Ok(graph) = camel_mail_service::load_messages_graph(
                request.source_uid.as_deref(),
                request.folder_name.as_deref(),
                request.offset,
                request.max_messages_per_folder,
                &request.graph_cursors,
                cancelled,
            ) {
                return prepared_camel_result(graph, cancelled);
            }
            let store = crate::services::MailStoreService::new_worker_safe(store_config);
            let messages = store.load_local_records_cancelable(cancelled);
            let searchable_body_texts = match prepare_searchable_body_texts(&messages, cancelled) {
                Ok(texts) => texts,
                Err(search_error) => {
                    return MailPageLoadResult::Error(search_error);
                }
            };
            MailPageLoadResult::Local {
                reason: format!(
                    "Mailkonto-Synchronisation nicht verfügbar ({error}); lokaler Evolution-Speicher"
                ),
                messages,
                local_folders: store.local_folder_definitions(),
                searchable_body_texts,
            }
        }
    }
}

fn format_camel_status(camel: &CamelMailLoad) -> String {
    let backend_label = if camel.search_query.is_some() {
        "Graph-Suche"
    } else if camel.is_graph {
        "Microsoft Graph"
    } else {
        "Mailkonto"
    };
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
    let folder_warning = (camel.configured_sources > 0
        && camel.successful_sources > 0
        && !camel.folders.iter().any(|folder| folder.is_selectable))
    .then_some(" · Keine lesbaren Mailordner gefunden")
    .unwrap_or_default();

    format!(
        "{backend_label}: {sync_status}, {} {message_label} geladen{warning}{folder_warning}",
        camel.messages.len(),
    )
}

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MailPage {
        pub root_box: gtk::Box,
        pub outer_paned: gtk::Paned,
        pub inner_paned: gtk::Paned,
        pub preview_widget: RefCell<Option<gtk::Widget>>,
        pub message_selection: RefCell<Option<gtk::SingleSelection>>,
        pub message_store: RefCell<Option<gio::ListStore>>,
        pub message_filter: RefCell<Option<gtk::CustomFilter>>,
        pub(super) message_filter_state: RefCell<Option<Rc<Cell<MessageFilter>>>>,
        pub thread_selection: RefCell<Option<gtk::SingleSelection>>,
        pub thread_root_store: RefCell<Option<gio::ListStore>>,
        pub thread_filter: RefCell<Option<gtk::CustomFilter>>,
        pub thread_members: RefCell<Option<Rc<RefCell<HashMap<String, Vec<MailMessageObject>>>>>>,
        pub thread_children: RefCell<Option<Rc<RefCell<HashMap<String, gio::ListStore>>>>>,
        pub thread_selection_syncing: Cell<bool>,
        pub(super) folder_selection: RefCell<Option<Rc<RefCell<FolderSelection>>>>,
        pub source_selection: RefCell<Option<gtk::SingleSelection>>,
        pub folder_list: RefCell<Option<gtk::ListBox>>,
        pub local_folder_ids: RefCell<HashSet<String>>,
        pub local_folder_collapsed: RefCell<HashSet<String>>,
        pub dynamic_folder_store: RefCell<Option<gio::ListStore>>,
        pub dynamic_folder_selection: RefCell<Option<gtk::SingleSelection>>,
        pub dynamic_folder_children: RefCell<Option<Rc<RefCell<HashMap<String, gio::ListStore>>>>>,
        pub dynamic_folder_index: RefCell<Option<Rc<RefCell<HashMap<String, FolderObject>>>>>,
        pub(crate) mail_folder_summaries: RefCell<Vec<CamelFolderSummary>>,
        pub crypto_status_caption: RefCell<Option<gtk::Label>>,
        pub crypto_status_chip: RefCell<Option<gtk::Box>>,
        pub crypto_status_routine: Cell<bool>,
        pub crypto_request_generation: Cell<u64>,
        pub all_headers_revealer: RefCell<Option<gtk::Revealer>>,
        pub all_headers_grid: RefCell<Option<gtk::Grid>>,
        pub normal_headers_grid: RefCell<Option<gtk::Grid>>,
        pub all_headers_generation: Cell<u64>,
        pub all_headers_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub(super) message_source_windows: RefCell<HashMap<String, MessageSourceWindowState>>,
        pub mail_viewer: RefCell<Option<MailViewer>>,
        pub print_status_area: RefCell<Option<gtk::Box>>,
        pub print_status_label: RefCell<Option<gtk::Label>>,
        pub print_cancel_button: RefCell<Option<gtk::Button>>,
        pub print_preview_window: RefCell<Option<gtk::Window>>,
        pub print_viewer: RefCell<Option<MailViewer>>,
        pub print_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub print_generation: Cell<u64>,
        pub print_state: Cell<MailPrintJobState>,
        pub message_find_revealer: RefCell<Option<gtk::Revealer>>,
        pub message_find_entry: RefCell<Option<gtk::SearchEntry>>,
        pub folder_section: RefCell<Option<gtk::Box>>,
        pub local_folder_section: RefCell<Option<gtk::Box>>,
        pub source_state: RefCell<Option<gtk::Label>>,
        pub source_status_area: RefCell<Option<gtk::Box>>,
        pub search_list: RefCell<Option<gtk::ListBox>>,
        pub search_field_scope: Cell<QuickSearchField>,
        pub search_scope: Cell<QuickSearchScope>,
        pub search_field_selector: RefCell<Option<gtk::DropDown>>,
        pub search_scope_selector: RefCell<Option<gtk::DropDown>>,
        pub quick_filter: RefCell<Option<gtk::DropDown>>,
        pub message_state: RefCell<Option<gtk::Label>>,
        pub message_status_area: RefCell<Option<gtk::Box>>,
        pub refresh_button: RefCell<Option<gtk::Button>>,
        pub load_more_button: RefCell<Option<gtk::Button>>,
        pub batch_action_bar: RefCell<Option<gtk::Box>>,
        pub batch_action_label: RefCell<Option<gtk::Label>>,
        pub(crate) batch_selection: RefCell<HashSet<String>>,
        pub(super) active_request: RefCell<Option<MailLoadRequest>>,
        pub load_generation: Cell<u64>,
        pub mail_loading: Cell<bool>,
        pub mail_change_pending: Cell<bool>,
        pub mail_incremental_loading: Cell<bool>,
        pub updating_folder_rows: Cell<bool>,
        pub mail_sync_source: RefCell<Option<glib::SourceId>>,
        pub mail_startup_source: RefCell<Option<glib::SourceId>>,
        pub mail_apply_source: RefCell<Option<glib::SourceId>>,
        pub mail_thread_apply_source: RefCell<Option<glib::SourceId>>,
        pub mail_load_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub mail_search_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub search_debounce_source: RefCell<Option<glib::SourceId>>,
        pub mark_read_timeout_source: RefCell<Option<glib::SourceId>>,
        pub search_generation: Cell<u64>,
        pub graph_backend_active: Cell<bool>,
        pub remote_search_applied: Cell<bool>,
        pub mail_incremental_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub mail_change_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub camel_monitor_started: Cell<bool>,
        pub(crate) mail_commands: RefCell<MailCommandStack>,
        pub mail_history_load_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub mail_history_generation: Cell<u64>,
        pub mail_history_persist_sequence: Cell<u64>,
        pub mail_history_persisted_sequence: Arc<Mutex<u64>>,
        pub(crate) mail_operation_coordinator: MailOperationCoordinator,
        pub(crate) mail_operation_cancellations: RefCell<HashMap<u64, MailOperationCancellation>>,
        pub mail_operations_active: Cell<usize>,
        pub mail_batch_running: Cell<bool>,
        pub preview_settings: RefCell<Option<AppSettings>>,
        pub classic_toolbar: RefCell<Option<gtk::Box>>,
        pub mail_column_header: RefCell<Option<gtk::Box>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub(super) search_query: RefCell<Option<Rc<RefCell<MessageSearchQuery>>>>,
        pub(super) mail_sort_field: Cell<MailSortField>,
        pub mail_sort_descending: Cell<bool>,
        pub mail_threading: Cell<bool>,
        pub(super) mail_layout: Cell<MailLayout>,
        pub mail_sort_selector: RefCell<Option<gtk::DropDown>>,
        pub mail_sort_direction_button: RefCell<Option<gtk::Button>>,
        pub drawer_header_title: RefCell<Option<gtk::Label>>,
        pub drawer_header_icon: RefCell<Option<gtk::Image>>,
        pub status_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub attachment_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub follow_up_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub star_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub important_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub sender_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub preview_size_group: RefCell<Option<gtk::SizeGroup>>,
        pub date_size_group: RefCell<Option<gtk::SizeGroup>>,
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
            let started = Instant::now();
            tracing::debug!("MailPage construction started");
            self.mail_commands
                .replace(MailCommandStack::empty_persistent());
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
                self.mail_layout.set(MailLayout::Classic);
                self.mail_layout.get().default_position()
            };
            self.inner_paned
                .set_orientation(self.mail_layout.get().orientation());
            self.inner_paned.set_wide_handle(true);
            self.inner_paned.set_resize_start_child(true);
            self.inner_paned.set_shrink_start_child(false);
            self.inner_paned.set_resize_end_child(true);
            self.inner_paned.set_shrink_end_child(false);
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
                crypto_label,
                lbl_crypto,
                crypto_chip,
                attachment_bar,
                thread_context,
                all_headers_revealer,
                all_headers_grid,
                message_find_revealer,
                message_find_entry,
            ) = obj.build_detail_view();
            self.crypto_status_caption
                .replace(Some(crypto_label.clone()));
            self.crypto_status_chip.replace(Some(crypto_chip.clone()));
            self.crypto_status_routine.set(true);
            self.all_headers_revealer
                .replace(Some(all_headers_revealer));
            self.all_headers_grid.replace(Some(all_headers_grid));
            self.mail_viewer.replace(Some(mail_viewer.clone()));
            self.message_find_revealer
                .replace(Some(message_find_revealer));
            self.message_find_entry.replace(Some(message_find_entry));

            obj.connect_selection_handler(
                &selection,
                &list_store,
                &custom_filter,
                &thread_filter,
                &mail_viewer,
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
            self.message_filter_state
                .replace(Some(message_filter.clone()));
            self.thread_filter.replace(Some(thread_filter.clone()));
            self.folder_selection
                .replace(Some(folder_selection.clone()));
            self.folder_list.replace(Some(folder_list.clone()));
            self.folder_section.replace(Some(folder_section.clone()));
            self.source_state.replace(Some(source_state.clone()));
            self.search_list.replace(Some(search_list.clone()));
            self.search_query.replace(Some(search_query.clone()));
            self.message_state.replace(Some(message_state.clone()));

            obj.assemble_layout(sidebar, message_container, detail_view);
            obj.bind_preview_setting();
            obj.load_mail_history_async();
            obj.load_initial_data(&source_store, &source_state);
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "MailPage construction finished"
            );
        }

        fn dispose(&self) {
            if let Some(source) = self.mail_sync_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = self.mail_startup_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = self.mail_apply_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = self.mail_thread_apply_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(cancelled) = self.mail_load_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(cancelled) = self.mail_search_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(source) = self.search_debounce_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = self.mark_read_timeout_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(cancelled) = self.mail_incremental_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(cancelled) = self.mail_change_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(cancelled) = self.mail_history_load_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            if let Some(cancelled) = self.all_headers_cancellation.borrow().as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            let source_windows = std::mem::take(&mut *self.message_source_windows.borrow_mut());
            for state in source_windows.into_values() {
                state.cancellation.store(true, Ordering::Relaxed);
                state.window.close();
            }
            self.obj().shutdown_print();
            for cancellation in self.mail_operation_cancellations.borrow().values() {
                cancellation.cancel();
            }
            self.mail_operation_cancellations.borrow_mut().clear();
            self.root_box.unparent();
        }
    }

    impl WidgetImpl for MailPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
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

    pub fn select_previous_message(&self) -> bool {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return false;
        };
        let current = selection.selected();
        if current > 0 && current != gtk::INVALID_LIST_POSITION {
            selection.set_selected(current - 1);
            true
        } else {
            false
        }
    }

    pub fn select_next_message(&self) -> bool {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return false;
        };
        let current = selection.selected();
        if current != gtk::INVALID_LIST_POSITION && current + 1 < selection.n_items() {
            selection.set_selected(current + 1);
            true
        } else {
            false
        }
    }

    fn begin_crypto_request(&self) -> u64 {
        let generation = advance_generation(self.imp().crypto_request_generation.get());
        self.imp().crypto_request_generation.set(generation);
        generation
    }

    fn invalidate_message_filters(&self) {
        if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
        if let Some(filter) = self.imp().thread_filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
    }

    fn set_crypto_status_routine(&self, routine: bool) {
        self.imp().crypto_status_routine.set(routine);
        self.refresh_crypto_status_visibility();
    }

    fn refresh_crypto_status_visibility(&self) {
        let show_routine = AppSettings::try_new()
            .map(|settings| settings.mail_show_security_details())
            .unwrap_or(false);
        let visible = crypto_status_visible(show_routine, self.imp().crypto_status_routine.get());
        if let Some(caption) = self.imp().crypto_status_caption.borrow().as_ref() {
            caption.set_visible(visible);
        }
        if let Some(chip) = self.imp().crypto_status_chip.borrow().as_ref() {
            chip.set_visible(visible);
        }
    }

    pub(crate) fn set_all_headers_visible(&self, visible: bool) {
        let selected = self.selected_message();
        self.refresh_normal_headers_for_message(selected.as_ref());
        if visible {
            self.refresh_all_headers_for_message(selected.as_ref());
        } else {
            self.cancel_all_headers_request();
            if let Some(grid) = self.imp().all_headers_grid.borrow().as_ref() {
                replace_formatted_header_rows(grid, &[]);
            }
            if let Some(revealer) = self.imp().all_headers_revealer.borrow().as_ref() {
                revealer.set_reveal_child(false);
            }
        }
    }

    fn refresh_normal_headers_for_message(&self, message: Option<&MailMessageObject>) {
        let Some(grid) = self.imp().normal_headers_grid.borrow().as_ref().cloned() else {
            return;
        };
        let show_all = AppSettings::try_new()
            .map(|settings| settings.mail_show_all_headers())
            .unwrap_or(false);
        grid.set_visible(message.is_some() && !show_all);
        let Some(message) = message.filter(|_| !show_all) else {
            replace_normal_header_rows(&grid, &[]);
            return;
        };
        let configuration = AppSettings::try_new()
            .map(|settings| settings.mail_normal_headers())
            .unwrap_or_else(default_normal_header_configuration);
        let fields =
            configured_normal_header_fields(&message_header_fields(message), &configuration);
        replace_normal_header_rows(&grid, &fields);
    }

    fn set_mail_status_areas_visible(&self, _visible: bool) {
        if let Some(area) = self.imp().source_status_area.borrow().as_ref() {
            area.set_visible(false);
        }
        if let Some(area) = self.imp().message_status_area.borrow().as_ref() {
            area.set_visible(false);
        }
    }

    fn refresh_all_headers_for_message(&self, message: Option<&MailMessageObject>) {
        let generation = self.cancel_all_headers_request();
        let visible = AppSettings::try_new()
            .map(|settings| settings.mail_show_all_headers())
            .unwrap_or(false);
        if let Some(revealer) = self.imp().all_headers_revealer.borrow().as_ref() {
            revealer.set_reveal_child(visible && message.is_some());
        }
        let Some(message) = message.filter(|_| visible) else {
            if let Some(grid) = self.imp().all_headers_grid.borrow().as_ref() {
                replace_formatted_header_rows(grid, &[]);
            }
            return;
        };
        if message.raw_headers().is_some() {
            if let Some(grid) = self.imp().all_headers_grid.borrow().as_ref() {
                replace_formatted_header_rows(grid, &message_header_fields(message));
            }
            return;
        }
        let Some(location) = message.backend_location() else {
            if let Some(grid) = self.imp().all_headers_grid.borrow().as_ref() {
                replace_formatted_header_rows(grid, &message_header_fields(message));
            }
            return;
        };

        if let Some(grid) = self.imp().all_headers_grid.borrow().as_ref() {
            replace_formatted_header_status(grid, "Kopfzeilen werden geladen …");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.imp()
            .all_headers_cancellation
            .replace(Some(cancelled.clone()));
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled_for_worker = cancelled.clone();
        let requested_location = location.clone();
        std::thread::spawn(move || {
            let result =
                camel_mail_service::load_raw_message(&requested_location, &cancelled_for_worker)
                    .and_then(|source| {
                        let headers = rfc822_header_block(&source);
                        (!headers.is_empty())
                            .then(|| headers.to_vec())
                            .ok_or_else(|| {
                                "Die Nachrichtenquelle enthält keine Kopfzeilen".to_string()
                            })
                    });
            let _ = sender.send_blocking(result);
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page.upgrade() else {
                return;
            };
            let visible = AppSettings::try_new()
                .map(|settings| settings.mail_show_all_headers())
                .unwrap_or(false);
            let Some(selected) = page.selected_message() else {
                return;
            };
            if !all_headers_request_is_current(
                cancelled.load(Ordering::Relaxed),
                generation,
                page.imp().all_headers_generation.get(),
                visible,
                selected.backend_location().as_ref() == Some(&location),
            ) {
                return;
            }
            match result {
                Ok(headers) => {
                    selected.set_raw_headers(Some(headers));
                    if let Some(grid) = page.imp().all_headers_grid.borrow().as_ref() {
                        replace_formatted_header_rows(grid, &message_header_fields(&selected));
                    }
                }
                Err(error) => {
                    if let Some(grid) = page.imp().all_headers_grid.borrow().as_ref() {
                        replace_formatted_header_rows(grid, &message_header_fields(&selected));
                    }
                    page.show_toast(&format!("Originale Kopfzeilen nicht verfügbar: {error}"));
                }
            }
        });
    }

    fn cancel_all_headers_request(&self) -> u64 {
        if let Some(cancelled) = self.imp().all_headers_cancellation.borrow_mut().take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        let generation = self.imp().all_headers_generation.get().wrapping_add(1);
        self.imp().all_headers_generation.set(generation);
        generation
    }

    fn bind_preview_setting(&self) {
        let settings = AppSettings::try_new();
        let show_preview = settings
            .as_ref()
            .map(AppSettings::show_message_preview)
            .unwrap_or(true);
        self.set_preview_visible(show_preview);
        let show_all_headers = settings
            .as_ref()
            .map(AppSettings::mail_show_all_headers)
            .unwrap_or(false);
        self.set_all_headers_visible(show_all_headers);
        let show_statusbar = settings
            .as_ref()
            .map(AppSettings::mail_show_statusbar)
            .unwrap_or(true);
        self.set_mail_status_areas_visible(show_statusbar);
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
            let page_weak_for_security = self.downgrade();
            settings.connect_changed(Some("mail-show-security-details"), move |_, _| {
                if let Some(page) = page_weak_for_security.upgrade() {
                    page.refresh_crypto_status_visibility();
                }
            });
            let page_weak_for_headers = self.downgrade();
            settings.connect_changed(Some("mail-show-all-headers"), move |settings, _| {
                if let Some(page) = page_weak_for_headers.upgrade() {
                    page.set_all_headers_visible(settings.boolean("mail-show-all-headers"));
                }
            });
            let page_weak_for_normal_headers = self.downgrade();
            settings.connect_changed(Some("mail-normal-headers"), move |_, _| {
                if let Some(page) = page_weak_for_normal_headers.upgrade() {
                    let selected = page.selected_message();
                    page.refresh_normal_headers_for_message(selected.as_ref());
                }
            });
            let page_weak_for_statusbar = self.downgrade();
            settings.connect_changed(Some("mail-show-statusbar"), move |settings, _| {
                if let Some(page) = page_weak_for_statusbar.upgrade() {
                    page.set_mail_status_areas_visible(settings.boolean("mail-show-statusbar"));
                }
            });
            for key in [
                "mail-show-avatar",
                "mail-show-sender",
                "mail-show-recipient",
                "mail-show-date",
                "mail-show-attachment",
                "mail-show-preview-text",
                "mail-show-labels",
            ] {
                let page_weak_for_columns = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(page) = page_weak_for_columns.upgrade() {
                        page.refresh_mail_column_header();
                        page.refresh_mail();
                    }
                });
            }
            let page_weak_for_col_width = self.downgrade();
            settings.connect_changed(Some("mail-sender-column-width"), move |_, _| {
                if let Some(page) = page_weak_for_col_width.upgrade() {
                    page.refresh_mail_column_header();
                }
            });
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
            let page_weak_for_sync_limit = self.downgrade();
            settings.connect_changed(Some("mail-sync-message-limit"), move |_, _| {
                if let Some(page) = page_weak_for_sync_limit.upgrade() {
                    page.refresh_mail();
                }
            });
            let page_weak_for_offline_mode = self.downgrade();
            settings.connect_changed(Some("offline-mode"), move |settings, _| {
                if let Some(page) = page_weak_for_offline_mode.upgrade() {
                    page.apply_offline_mode(settings.boolean("offline-mode"));
                    page.refresh_mail();
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
            let page_weak_for_toolbar = self.downgrade();
            settings.connect_changed(Some("mail-show-toolbar"), move |settings, _| {
                if let Some(page) = page_weak_for_toolbar.upgrade() {
                    if let Some(toolbar) = page.imp().classic_toolbar.borrow().as_ref() {
                        toolbar.set_visible(settings.boolean("mail-show-toolbar"));
                    }
                }
            });
            let page_weak_for_sidebar = self.downgrade();
            settings.connect_changed(Some("mail-show-sidebar"), move |settings, _| {
                if let Some(page) = page_weak_for_sidebar.upgrade() {
                    page.set_sidebar_visible(settings.boolean("mail-show-sidebar"));
                }
            });
            let page_weak_for_default_folder = self.downgrade();
            settings.connect_changed(Some("mail-default-folder"), move |settings, _| {
                if let Some(page) = page_weak_for_default_folder.upgrade() {
                    page.apply_default_folder(&settings.string("mail-default-folder"));
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
        let current_pos = imp.inner_paned.position();
        let total_width = imp.inner_paned.width();
        let next_position = AppSettings::try_new()
            .map(|settings| {
                if current_pos > 50 {
                    settings
                        .set_mail_preview_position(previous_layout.setting_value(), current_pos);
                }
                let saved = settings.mail_preview_position(layout.setting_value());
                if saved < 100 {
                    layout.default_position()
                } else {
                    saved
                }
            })
            .unwrap_or_else(|| layout.default_position());

        let target_pos = if layout == MailLayout::Modern && total_width > 650 {
            next_position.clamp(280, (total_width - 350).max(280))
        } else if layout == MailLayout::Modern {
            (total_width / 2).clamp(280, 500)
        } else {
            next_position.clamp(180, 600)
        };

        imp.mail_layout.set(layout);
        imp.inner_paned.set_orientation(layout.orientation());
        imp.inner_paned.set_position(target_pos);
        if let Some(end_child) = imp.inner_paned.end_child() {
            end_child.set_visible(true);
        }
    }

    fn apply_default_folder(&self, folder_id: &str) {
        let selection = FolderSelection::from_folder_id(folder_id);
        if let Some(current) = self.imp().folder_selection.borrow().as_ref() {
            current.replace(selection.clone());
        }
        self.select_folder_row(&selection);

        if let Some(search_list) = self.imp().search_list.borrow().as_ref() {
            search_list.unselect_all();
        }
        if let Some(search_entry) = self.imp().search_entry.borrow().as_ref() {
            search_entry.set_text("");
        }
        if let Some(message_filter) = self.imp().message_filter_state.borrow().as_ref() {
            message_filter.set(MessageFilter::All);
        }
        if let Some(query) = self.imp().search_query.borrow().as_ref() {
            query.replace(MessageSearchQuery::default());
        }
        if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
        if let Some(filter) = self.imp().thread_filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }

        // If an account is selected, retain that account while switching the
        // built-in folder.  The native loader returns all selectable folders
        // for the account and the portable folder selection filters the result
        // by its normalized folder id.  Without an account selection, reload
        // the normal aggregate request.
        let mut request = self
            .imp()
            .active_request
            .borrow()
            .clone()
            .unwrap_or_else(MailLoadRequest::initial);
        request.folder_name = None;
        request.offset = 0;
        self.start_mail_load(request, true);
    }

    fn set_preview_visible(&self, visible: bool) {
        if let Some(preview) = self.imp().preview_widget.borrow().as_ref() {
            preview.set_visible(visible);
        }
    }

    fn mail_sort_selector_index(field: MailSortField) -> u32 {
        field.selector_index()
    }

    fn mail_sort_field_from_selector_index(index: u32) -> MailSortField {
        match index {
            1 => MailSortField::Subject,
            2 => MailSortField::Sender,
            3 => MailSortField::Unread,
            4 => MailSortField::Flagged,
            5 => MailSortField::FollowUp,
            6 => MailSortField::Important,
            7 => MailSortField::Attachments,
            _ => MailSortField::Date,
        }
    }

    fn set_mail_sort_direction_button(button: &gtk::Button, descending: bool) {
        button.set_icon_name(if descending {
            "view-sort-descending-symbolic"
        } else {
            "view-sort-ascending-symbolic"
        });
        let label = if descending {
            "Sortierung absteigend; Sortierrichtung umschalten"
        } else {
            "Sortierung aufsteigend; Sortierrichtung umschalten"
        };
        button.set_tooltip_text(Some(label));
        set_accessible_label(button, label);
    }

    fn refresh_mail_sort_controls(&self) {
        let field = self.imp().mail_sort_field.get();
        if let Some(selector) = self.imp().mail_sort_selector.borrow().as_ref() {
            selector.set_selected(Self::mail_sort_selector_index(field));
        }
        if let Some(button) = self.imp().mail_sort_direction_button.borrow().as_ref() {
            Self::set_mail_sort_direction_button(button, self.imp().mail_sort_descending.get());
        }
    }

    fn mail_header_sort_label(&self, label: &str, field: MailSortField) -> String {
        if self.imp().mail_sort_field.get() != field {
            return label.to_string();
        }
        let direction = if self.imp().mail_sort_descending.get() {
            "↓"
        } else {
            "↑"
        };
        format!("{label} {direction}")
    }

    fn set_mail_sort_from_header(&self, field: MailSortField) {
        let descending = if self.imp().mail_sort_field.get() == field {
            !self.imp().mail_sort_descending.get()
        } else {
            matches!(
                field,
                MailSortField::Date
                    | MailSortField::Unread
                    | MailSortField::Flagged
                    | MailSortField::FollowUp
                    | MailSortField::Important
                    | MailSortField::Attachments
            )
        };
        self.imp().mail_sort_field.set(field);
        self.imp().mail_sort_descending.set(descending);
        if let Some(settings) = AppSettings::try_new() {
            settings.set_mail_sort_field(field.setting_value());
            settings.set_mail_sort_descending(descending);
        }
        self.refresh_mail_sort_controls();
        self.resort_mail_preserving_selection();
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
        self.refresh_mail_sort_controls();
        self.resort_mail_preserving_selection();
    }

    fn resort_mail_preserving_selection(&self) {
        let previous_selection = self.selected_message_anchor();
        if let Some(store) = self.imp().message_store.borrow().as_ref().cloned() {
            self.begin_incremental_thread_refresh(store, previous_selection);
        }
        self.refresh_mail_column_header();
    }

    pub fn focus_search(&self) {
        if let Some(search_entry) = self.imp().search_entry.borrow().as_ref() {
            search_entry.grab_focus();
        }
    }

    pub(crate) fn find_in_displayed_message(&self) -> bool {
        if self.selected_message().is_none()
            || !AppSettings::try_new()
                .map(|settings| settings.show_message_preview())
                .unwrap_or(true)
        {
            return false;
        }
        if let Some(revealer) = self.imp().message_find_revealer.borrow().as_ref() {
            revealer.set_reveal_child(true);
        }
        if let Some(entry) = self.imp().message_find_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
        true
    }

    fn reset_displayed_message_search_for_selection(&self) {
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            viewer.finish_displayed_message_search();
        }
        if let Some(entry) = self.imp().message_find_entry.borrow().as_ref()
            && !entry.text().is_empty()
        {
            entry.set_text("");
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(search_entry) = self.imp().search_entry.borrow().as_ref() {
            search_entry.set_text(query);
            search_entry.grab_focus();
        }
    }

    pub(crate) fn select_search_result(&self, message_id: &str) -> bool {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return false;
        };
        for index in 0..selection.n_items() {
            let Some(message) = selection.item(index).and_downcast::<MailMessageObject>() else {
                continue;
            };
            if message.id() == message_id {
                selection.set_selected(index);
                self.sync_thread_selection(Some(message_id));
                return true;
            }
        }
        false
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

    pub(crate) fn print_action_enabled(&self, action: MailPrintAction) -> bool {
        if matches!(action, MailPrintAction::Print) && !cfg!(target_os = "linux") {
            return false;
        }
        self.imp()
            .print_state
            .get()
            .action_enabled(self.selected_message().is_some())
    }

    pub(crate) fn print_job_active(&self) -> bool {
        self.imp().print_state.get().is_running()
    }

    pub(crate) fn print_cancel_enabled(&self) -> bool {
        self.imp().print_state.get().is_cancellable()
    }

    /// Start either the direct print action or the deterministic in-app
    /// preview for the currently selected row/reader message.  The snapshot
    /// is intentionally captured before entering the worker and contains no
    /// GTK object or MIME operation.
    pub(crate) fn start_print(&self, action: MailPrintAction) -> bool {
        if self.print_job_active() {
            self.show_toast("Ein Druckvorgang läuft bereits");
            return true;
        }
        let Some(message) = self.selected_message() else {
            self.set_print_state(
                MailPrintJobState::Error,
                "Drucken nicht möglich: keine Nachricht ausgewählt",
            );
            return false;
        };
        let snapshot = print_snapshot_from(&message);
        let generation = advance_generation(self.imp().print_generation.get());
        self.imp().print_generation.set(generation);
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .print_cancellation
            .replace(Some(cancellation.clone()));
        self.set_print_state(
            MailPrintJobState::Preparing,
            if action.is_preview() {
                "Druckvorschau wird vorbereitet …"
            } else {
                "Druck wird vorbereitet …"
            },
        );

        let (sender, receiver) = async_channel::bounded(1);
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::Builder::new()
            .name("mail-print-preparation".to_string())
            .spawn(move || {
                let result =
                    mail_print_service::prepare_print_document(&snapshot, &worker_cancellation);
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Print document result was dropped");
                }
            });
        if let Err(error) = worker {
            drop(receiver);
            self.finish_print_error(
                generation,
                format!("Druckvorbereitung konnte nicht gestartet werden: {error}"),
            );
            return true;
        }

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err(mail_print_service::MailPrintDocumentError::WorkerDisconnected)
            });
            let Some(page) = page.upgrade() else {
                return;
            };
            if page.imp().print_generation.get() != generation {
                return;
            }
            match result {
                Ok(document) if action.is_preview() => {
                    page.set_print_state(
                        MailPrintJobState::Previewing,
                        "Druckvorschau wird geöffnet …",
                    );
                    if let Err(error) = page.present_print_preview(document) {
                        page.finish_print_error(generation, error);
                    } else {
                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Idle,
                            "Druckvorschau geöffnet",
                        );
                    }
                }
                Ok(document) => page.start_native_print(generation, document),
                Err(mail_print_service::MailPrintDocumentError::Cancelled) => {
                    page.imp().print_cancellation.borrow_mut().take();
                    page.finish_print_state(
                        generation,
                        MailPrintJobState::Cancelled,
                        "Druckvorbereitung abgebrochen",
                    );
                }
                Err(error) => page.finish_print_error(generation, error.to_string()),
            }
        });
        true
    }

    pub(crate) fn cancel_print(&self) -> bool {
        if !self.imp().print_state.get().is_cancellable() {
            return false;
        }
        let Some(cancellation) = self.imp().print_cancellation.borrow_mut().take() else {
            return false;
        };
        cancellation.store(true, Ordering::Release);
        let generation = advance_generation(self.imp().print_generation.get());
        self.imp().print_generation.set(generation);
        self.set_print_state(MailPrintJobState::Cancelled, "Druckvorgang abgebrochen");
        true
    }

    /// Invalidate every print callback before the window or page is torn down.
    /// The native WebKit dialog has its own cancellation control; advancing
    /// the generation is still required so a late `finished`/`failed` signal
    /// cannot update a closing page.
    pub(crate) fn shutdown_print(&self) {
        let generation = advance_generation(self.imp().print_generation.get());
        self.imp().print_generation.set(generation);
        if let Some(cancellation) = self.imp().print_cancellation.borrow_mut().take() {
            cancellation.store(true, Ordering::Release);
        }
        if let Some(window) = self.imp().print_preview_window.borrow_mut().take() {
            window.close();
        }
        self.imp().print_viewer.borrow_mut().take();
        self.imp().print_state.set(MailPrintJobState::Cancelled);
        if let Some(area) = self.imp().print_status_area.borrow().as_ref() {
            area.set_visible(false);
        }
        if let Some(cancel_button) = self.imp().print_cancel_button.borrow().as_ref() {
            cancel_button.set_visible(false);
        }
    }

    fn set_print_state(&self, state: MailPrintJobState, message: &str) {
        self.imp().print_state.set(state);
        if let Some(label) = self.imp().print_status_label.borrow().as_ref() {
            label.set_label(message);
        }
        if let Some(area) = self.imp().print_status_area.borrow().as_ref() {
            area.set_visible(!matches!(state, MailPrintJobState::Idle));
        }
        if let Some(cancel_button) = self.imp().print_cancel_button.borrow().as_ref() {
            cancel_button.set_visible(state.is_cancellable());
        }
        if let Some(root) = self.root().and_downcast::<crate::window::EvolutionWindow>() {
            root.update_mail_print_actions();
        }
    }

    fn finish_print_state(&self, generation: u64, state: MailPrintJobState, message: &str) {
        if self.imp().print_generation.get() != generation {
            return;
        }
        self.imp().print_cancellation.borrow_mut().take();
        self.set_print_state(state, message);
    }

    fn finish_print_error(&self, generation: u64, error: impl Into<String>) {
        if self.imp().print_generation.get() != generation {
            return;
        }
        let error = error.into();
        self.imp().print_cancellation.borrow_mut().take();
        self.set_print_state(
            MailPrintJobState::Error,
            &format!("Drucken nicht möglich: {error}"),
        );
        self.show_toast(&format!("Drucken nicht möglich: {error}"));
    }

    fn present_print_preview(
        &self,
        document: mail_print_service::MailPrintDocument,
    ) -> Result<(), String> {
        let parent = self
            .root()
            .and_downcast::<gtk::Window>()
            .ok_or_else(|| "Kein Fenster für die Druckvorschau verfügbar".to_string())?;
        if let Some(window) = self.imp().print_preview_window.borrow_mut().take() {
            window.close();
        }

        let preview_viewer = MailViewer::new();
        preview_viewer.load_print_document(&document)?;
        let preview_window = gtk::Window::builder()
            .title("Druckvorschau")
            .default_width(900)
            .default_height(700)
            .transient_for(&parent)
            .build();
        let header = gtk::HeaderBar::new();
        let close_button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Druckvorschau schließen")
            .build();
        set_accessible_label(&close_button, "Druckvorschau schließen");
        let preview_window_for_close = preview_window.downgrade();
        close_button.connect_clicked(move |_| {
            if let Some(window) = preview_window_for_close.upgrade() {
                window.close();
            }
        });
        header.pack_end(&close_button);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&header);
        content.append(&preview_viewer);
        preview_window.set_child(Some(&content));
        let page = self.downgrade();
        preview_window.connect_close_request(move |window| {
            if let Some(page) = page.upgrade()
                && page
                    .imp()
                    .print_preview_window
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| current == window)
            {
                page.imp().print_preview_window.borrow_mut().take();
            }
            glib::Propagation::Proceed
        });
        self.imp()
            .print_preview_window
            .replace(Some(preview_window.clone()));
        preview_window.present();
        Ok(())
    }

    fn start_native_print(&self, generation: u64, document: mail_print_service::MailPrintDocument) {
        if self.imp().print_generation.get() != generation {
            return;
        }
        // The preparation worker has already returned its immutable
        // document.  From here on the native dialog owns cancellation, so a
        // stale preparation token must not make the UI promise a cancellation
        // that WebKitGTK cannot perform.
        self.imp().print_cancellation.borrow_mut().take();
        self.set_print_state(MailPrintJobState::Printing, "Druckdialog wird geöffnet …");

        #[cfg(target_os = "linux")]
        {
            let Some(parent) = self.root().and_downcast::<gtk::Window>() else {
                self.finish_print_error(generation, "Kein Fenster für den Druckdialog verfügbar");
                return;
            };
            let print_viewer = MailViewer::new();
            self.imp().print_viewer.replace(Some(print_viewer.clone()));
            let page = self.downgrade();
            print_viewer.print_document(&document, &parent, move |result| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                page.imp().print_viewer.borrow_mut().take();
                match result {
                    Ok(crate::views::mail_viewer::MailPrintDialogResult::Printed) => {
                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Idle,
                            "Druckauftrag wurde an das System übergeben",
                        );
                    }
                    Ok(crate::views::mail_viewer::MailPrintDialogResult::Cancelled) => {
                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Cancelled,
                            "Druckdialog abgebrochen",
                        );
                    }
                    Err(error) => page.finish_print_error(generation, error),
                }
            });
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = document;
            self.finish_print_error(generation, MailViewer::native_print_unavailable());
        }
    }

    /// Export the selected message as a complete RFC822/EML file.  This is a
    /// local, loss-minimizing operation: the original EDS/Camel message is
    /// never modified and the parsed thread and attachment metadata are
    /// retained in the exported headers and MIME parts.
    pub fn export_selected(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        let subject = message.subject();
        let location = message.backend_location();
        let raw_mime = message.raw_mime();
        let export = export_message_from(&message);

        let root = self.root().and_downcast::<gtk::Window>();
        let file_dialog = gtk::FileDialog::builder()
            .title("E-Mail als EML speichern")
            .initial_name(export_filename(&subject))
            .modal(true)
            .build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("EML-Dateien (*.eml)"));
        filter.add_pattern("*.eml");
        filter.add_mime_type("message/rfc822");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        file_dialog.set_filters(Some(&filters));

        let page = self.downgrade();
        file_dialog.save(root.as_ref(), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let (sender, receiver) = async_channel::bounded(1);
            let location_clone = location.clone();
            let raw_mime_clone = raw_mime.clone();
            std::thread::spawn(move || {
                let bytes = if let Some(raw) = raw_mime_clone {
                    raw.to_vec()
                } else if let Some(loc) = location_clone.as_ref() {
                    let cancelled = Arc::new(AtomicBool::new(false));
                    camel_mail_service::load_raw_message(loc, &cancelled).unwrap_or_else(|_| {
                        mail_composer_service::build_export_rfc822(&export).unwrap_or_default()
                    })
                } else {
                    mail_composer_service::build_export_rfc822(&export).unwrap_or_default()
                };
                let _ = sender.send_blocking(bytes);
            });
            let page_weak = page.clone();
            glib::spawn_future_local(async move {
                let Ok(raw_bytes) = receiver.recv().await else {
                    return;
                };
                let save_result = file.replace_contents(
                    &raw_bytes,
                    None,
                    true,
                    gio::FileCreateFlags::NONE,
                    None::<&gio::Cancellable>,
                );
                if let Some(page) = page_weak.upgrade() {
                    match save_result {
                        Ok(_) => page.show_toast("E-Mail als EML exportiert"),
                        Err(error) => page
                            .show_toast(&format!("EML konnte nicht gespeichert werden: {error}")),
                    }
                }
            });
        });
        true
    }

    pub(crate) fn present_message_source(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        let window_key = message_source_window_key(&message);
        if let Some(state) = self.imp().message_source_windows.borrow().get(&window_key) {
            state.window.present();
            return true;
        }
        let Some(parent) = self.root().and_downcast::<gtk::Window>() else {
            self.show_toast("Kein Fenster für die Nachrichtenquelle verfügbar");
            return true;
        };

        let subject = message.subject();
        let cancellation = Arc::new(AtomicBool::new(false));
        let state = self.build_message_source_window(&parent, &subject, cancellation.clone());
        let page = self.downgrade();
        let key_for_close = window_key.clone();
        let cancellation_for_close = cancellation.clone();
        state.window.connect_close_request(move |_| {
            cancellation_for_close.store(true, Ordering::Relaxed);
            if let Some(page) = page.upgrade() {
                let mut windows = page.imp().message_source_windows.borrow_mut();
                let is_this_window = windows
                    .get(&key_for_close)
                    .is_some_and(|state| Arc::ptr_eq(&state.cancellation, &cancellation_for_close));
                if is_this_window {
                    windows.remove(&key_for_close);
                }
            }
            glib::Propagation::Proceed
        });
        let window = state.window.clone();
        self.imp()
            .message_source_windows
            .borrow_mut()
            .insert(window_key.clone(), state);
        window.present();

        if let Some(source) = message.raw_mime() {
            self.apply_message_source_result(&window_key, &cancellation, Ok((source, None)));
            return true;
        }

        let location = message.backend_location();
        let export = export_message_from(&message);
        let (sender, receiver) = async_channel::bounded(1);
        let cancellation_for_worker = cancellation.clone();
        std::thread::spawn(move || {
            let exact = location
                .as_ref()
                .map(|location| {
                    camel_mail_service::load_raw_message(location, &cancellation_for_worker)
                })
                .transpose();
            let result: MessageSourceLoadResult = if cancellation_for_worker.load(Ordering::Relaxed)
            {
                Err("Abruf abgebrochen".to_string())
            } else {
                match exact {
                    Ok(Some(source)) => Ok((source.into(), None)),
                    Ok(None) => mail_composer_service::build_export_rfc822(&export)
                        .map(|source| {
                            (
                                source.into(),
                                Some("Der Nachrichtenspeicher besitzt keine abrufbare originale Rohquelle. Angezeigt wird eine verlustarme RFC822-Rekonstruktion der geladenen Nachricht.".to_string()),
                            )
                        })
                        .map_err(|error| error.to_string()),
                    Err(error) => {
                        tracing::warn!(%error, "Original message source retrieval failed");
                        mail_composer_service::build_export_rfc822(&export).map(|source| {
                        (
                            source.into(),
                            Some("Die originale Nachrichtenquelle konnte nicht geladen werden. Angezeigt wird eine verlustarme RFC822-Rekonstruktion.".to_string()),
                        )
                        }).map_err(|error| error.to_string())
                    }
                }
            };
            let _ = sender.send_blocking(result);
        });
        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page.upgrade() else {
                return;
            };
            page.apply_message_source_result(&window_key, &cancellation, result);
        });
        true
    }

    fn build_message_source_window(
        &self,
        parent: &gtk::Window,
        subject: &str,
        cancellation: Arc<AtomicBool>,
    ) -> MessageSourceWindowState {
        let window = adw::Window::builder()
            .title(format!("Nachrichtenquelle — {subject}"))
            .transient_for(parent)
            .modal(false)
            .default_width(900)
            .default_height(700)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Gesamte Nachrichtenquelle kopieren")
            .sensitive(false)
            .build();
        set_accessible_label(&copy_button, "Gesamte Nachrichtenquelle kopieren");
        let source_buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        let buffer_for_copy = source_buffer.clone();
        copy_button.connect_clicked(move |button| {
            let text = buffer_for_copy.text(
                &buffer_for_copy.start_iter(),
                &buffer_for_copy.end_iter(),
                false,
            );
            button.clipboard().set_text(&text);
        });
        header.pack_end(&copy_button);
        toolbar.add_top_bar(&header);

        let source_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let warning_label = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .css_classes(vec!["warning", "caption"])
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .visible(false)
            .build();
        source_content.append(&warning_label);
        let source_view = gtk::TextView::builder()
            .buffer(&source_buffer)
            .editable(false)
            .cursor_visible(true)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::None)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&source_view)
            .build();
        source_content.append(&scrolled);

        let loading_page = adw::StatusPage::builder()
            .title("Nachrichtenquelle wird geladen …")
            .description("Die originale RFC822-Nachricht wird abgerufen.")
            .icon_name("content-loading-symbolic")
            .build();
        let error_page = adw::StatusPage::builder()
            .title("Nachrichtenquelle konnte nicht geladen werden")
            .icon_name("dialog-error-symbolic")
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&loading_page, Some("loading"));
        stack.add_named(&source_content, Some("source"));
        stack.add_named(&error_page, Some("error"));
        stack.set_visible_child_name("loading");
        toolbar.set_content(Some(&stack));
        window.set_content(Some(&toolbar));

        MessageSourceWindowState {
            window,
            stack,
            source_buffer,
            warning_label,
            error_page,
            copy_button,
            cancellation,
        }
    }

    fn apply_message_source_result(
        &self,
        window_key: &str,
        request_cancellation: &Arc<AtomicBool>,
        result: MessageSourceLoadResult,
    ) {
        let windows = self.imp().message_source_windows.borrow();
        let Some(state) = windows.get(window_key) else {
            return;
        };
        if !message_source_result_is_current(
            request_cancellation.load(Ordering::Relaxed),
            true,
            Arc::ptr_eq(&state.cancellation, request_cancellation),
        ) {
            return;
        }
        match result {
            Ok((source, warning)) => {
                state
                    .source_buffer
                    .set_text(&String::from_utf8_lossy(source.as_ref()));
                state
                    .warning_label
                    .set_label(warning.as_deref().unwrap_or(""));
                state.warning_label.set_visible(warning.is_some());
                state.copy_button.set_sensitive(true);
                state.stack.set_visible_child_name("source");
            }
            Err(error) => {
                state.copy_button.set_sensitive(false);
                state.error_page.set_description(Some(&error));
                state.stack.set_visible_child_name("error");
            }
        }
    }

    fn present_local_folder_dialog(&self, current_folder: Option<&str>) {
        let Some(root) = self.root().and_downcast::<gtk::Window>() else {
            self.show_toast("Kein Fenster für die lokalen Ordner verfügbar");
            return;
        };

        let is_rename = current_folder.is_some();
        let initial_name = current_folder
            .and_then(|folder_id| folder_id.strip_prefix("custom:"))
            .unwrap_or_default();
        let entry = gtk::Entry::builder()
            .text(initial_name)
            .placeholder_text("z. B. Projekte/2026")
            .hexpand(true)
            .activates_default(true)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .build();
        let hint = gtk::Label::builder()
            .label("Unterordner können mit / getrennt werden. Die importierten Maildateien bleiben unverändert.")
            .wrap(true)
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&entry);
        content.append(&hint);

        let heading = if is_rename {
            "Lokalen Ordner umbenennen"
        } else {
            "Lokalen Ordner erstellen"
        };
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body("Der Ordner wird ausschließlich im app-eigenen lokalen Mailbestand angelegt.")
            .extra_child(&content)
            .default_response("save")
            .focus_widget(&entry)
            .build();
        dialog.add_responses(&[("cancel", "Abbrechen"), ("save", "Speichern")]);

        let page = self.downgrade();
        let current_folder = current_folder.map(str::to_string);
        let entry_for_response = entry.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response != "save" {
                return;
            }
            let name = entry_for_response.text().to_string();
            let result = if let Some(from) = current_folder.as_deref() {
                crate::services::MailStoreService::new().rename_local_folder(from, &name)
            } else {
                crate::services::MailStoreService::new().create_local_folder(&name)
            };
            match result {
                Ok(_) => {
                    dialog.close();
                    if let Some(page) = page.upgrade() {
                        page.refresh_mail();
                    }
                }
                Err(error) => {
                    if let Some(page) = page.upgrade() {
                        page.show_toast(&format!(
                            "Lokaler Ordner konnte nicht gespeichert werden: {error}"
                        ));
                    }
                }
            }
        });
        dialog.present(Some(&root));
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
            self.refresh_search_folder_counts();
        }
    }

    fn refresh_search_folder_counts(&self) {
        let Some(search_list) = self.imp().search_list.borrow().as_ref().cloned() else {
            return;
        };
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return;
        };
        let messages = (0..store.n_items())
            .filter_map(|index| store.item(index).and_downcast::<MailMessageObject>())
            .collect::<Vec<_>>();
        let all_messages = FolderSelection::all();

        for index in 0..search_list.observe_children().n_items() {
            let Some(row) = search_list
                .row_at_index(index as i32)
                .and_downcast::<gtk::ListBoxRow>()
            else {
                continue;
            };
            let Some(query) = (unsafe {
                row.data::<String>("search-query")
                    .map(|value| value.as_ref().clone())
            }) else {
                continue;
            };
            let query = MessageSearchQuery::parse(&query);
            let mut unread = 0u32;
            for message in &messages {
                if message_matches_filter(
                    message,
                    &query,
                    MessageFilter::All,
                    &all_messages,
                    QuickSearchScope::CurrentFolder,
                ) {
                    if message.is_unread() {
                        unread = unread.saturating_add(1);
                    }
                }
            }
            let Some(badge) = (unsafe {
                row.data::<gtk::Label>("folder-badge")
                    .map(|value| value.as_ref().clone())
            }) else {
                continue;
            };
            let (label, visible) = unread_badge_state(unread);
            badge.set_label(&label);
            badge.set_visible(visible);
        }
    }

    fn current_local_folder_counts(&self) -> HashMap<String, (u32, u32)> {
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return HashMap::new();
        };
        let mut counts = HashMap::<String, (u32, u32)>::new();
        for index in 0..store.n_items() {
            let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                continue;
            };
            if message.backend_location().is_some() {
                continue;
            }
            let entry = counts.entry(message.folder_id()).or_insert((0, 0));
            entry.1 = entry.1.saturating_add(1);
            if message.is_unread() {
                entry.0 = entry.0.saturating_add(1);
            }
        }
        counts
    }

    fn refresh_unread_counters(&self) {
        let Some(folder_list) = self.imp().folder_list.borrow().as_ref().cloned() else {
            self.refresh_search_folder_counts();
            return;
        };
        let local_counts = self.current_local_folder_counts();
        let camel_folders = self.imp().mail_folder_summaries.borrow().clone();
        if camel_folders.is_empty() {
            Self::update_folder_counts(&folder_list, &local_counts);
        } else {
            self.update_camel_folder_counts(&folder_list, &camel_folders, Some(&local_counts));
        }
        self.refresh_search_folder_counts();
    }

    fn selected_message(&self) -> Option<MailMessageObject> {
        self.imp()
            .message_selection
            .borrow()
            .as_ref()
            .and_then(selected_mail_message)
            .or_else(|| {
                self.imp()
                    .thread_selection
                    .borrow()
                    .as_ref()
                    .and_then(selected_mail_message)
            })
    }

    fn mail_worker_ui_scope(&self) -> MailWorkerUiScope {
        let folder_selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        MailWorkerUiScope {
            load_generation: self.imp().load_generation.get(),
            folder_selection,
            active_request: self.imp().active_request.borrow().clone(),
        }
    }

    fn mail_worker_ui_scope_is_current(&self, expected: &MailWorkerUiScope) -> bool {
        let current_folder_selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        mail_worker_scope_is_current(
            expected,
            self.imp().load_generation.get(),
            &current_folder_selection,
            self.imp().active_request.borrow().as_ref(),
        )
    }

    pub fn has_selected_message(&self) -> bool {
        self.selected_message().is_some()
    }

    /// Notify a caller whenever the primary mail selection becomes usable for
    /// message actions.  The window uses this to mirror Evolution's action
    /// sensitivity in both the main menu and the message context menu.
    pub(crate) fn connect_selected_message_notify<F>(&self, callback: F)
    where
        F: Fn(bool) + 'static,
    {
        let cb = Rc::new(callback);
        let page = self.downgrade();

        let notify = {
            let cb = Rc::clone(&cb);
            let page = page.clone();
            move || {
                if let Some(page) = page.upgrade() {
                    let has_sel = page.has_selected_message();
                    cb(has_sel);
                }
            }
        };

        notify();

        if let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() {
            let notify = notify.clone();
            selection.connect_selected_item_notify(move |_| {
                notify();
            });
        }

        if let Some(thread_selection) = self.imp().thread_selection.borrow().as_ref().cloned() {
            let notify = notify.clone();
            thread_selection.connect_selected_item_notify(move |_| {
                notify();
            });
        }
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

    pub(crate) fn has_messages(&self) -> bool {
        self.imp()
            .message_selection
            .borrow()
            .as_ref()
            .is_some_and(|selection| selection.n_items() > 0)
    }

    pub(crate) fn can_select_relative_message(&self, offset: i32) -> bool {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return false;
        };
        let selected = (selection.selected() != gtk::INVALID_LIST_POSITION)
            .then_some(selection.selected() as usize);
        relative_message_index(selection.n_items() as usize, selected, offset).is_some()
    }

    pub fn select_relative_message(&self, offset: i32) -> bool {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return false;
        };
        let selected = (selection.selected() != gtk::INVALID_LIST_POSITION)
            .then_some(selection.selected() as usize);
        let Some(target) = relative_message_index(selection.n_items() as usize, selected, offset)
        else {
            return false;
        };
        selection.set_selected(target as u32);
        true
    }

    pub fn select_relative_unread_message(&self, forward: bool) -> bool {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return false;
        };
        let unread = (0..selection.n_items())
            .map(|index| {
                selection
                    .item(index)
                    .and_downcast::<MailMessageObject>()
                    .is_some_and(|message| message.is_unread())
            })
            .collect::<Vec<_>>();
        let selected = (selection.selected() != gtk::INVALID_LIST_POSITION)
            .then_some(selection.selected() as usize);
        let Some(target) = matching_message_index(&unread, selected, forward) else {
            return false;
        };
        selection.set_selected(target as u32);
        true
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
        result.map(|()| true)
    }

    /// Increase zoom level in the message viewer.
    pub fn zoom_in(&self) {
        if let Some(ref viewer) = *self.imp().mail_viewer.borrow() {
            viewer.zoom_in();
        }
    }

    /// Decrease zoom level in the message viewer.
    pub fn zoom_out(&self) {
        if let Some(ref viewer) = *self.imp().mail_viewer.borrow() {
            viewer.zoom_out();
        }
    }

    /// Reset zoom level in the message viewer.
    pub fn zoom_reset(&self) {
        if let Some(ref viewer) = *self.imp().mail_viewer.borrow() {
            viewer.zoom_reset();
        }
    }

    /// Expand all message threads in the thread tree.
    pub fn expand_all_threads(&self) {
        let Some(selection) = self.imp().thread_selection.borrow().as_ref().cloned() else {
            return;
        };
        for index in 0..selection.n_items() {
            if let Some(row) = selection.item(index).and_downcast::<gtk::TreeListRow>() {
                if row.is_expandable() {
                    row.set_expanded(true);
                }
            }
        }
    }

    /// Collapse all message threads in the thread tree.
    pub fn collapse_all_threads(&self) {
        let Some(selection) = self.imp().thread_selection.borrow().as_ref().cloned() else {
            return;
        };
        for index in 0..selection.n_items() {
            if let Some(row) = selection.item(index).and_downcast::<gtk::TreeListRow>() {
                if row.is_expandable() {
                    row.set_expanded(false);
                }
            }
        }
    }

    /// Mark all messages belonging to the currently selected thread as read.
    pub fn mark_selected_thread_read(&self) -> Result<usize, MailOperationError> {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return Ok(0);
        };
        let Some(selected_msg) = selected_mail_message(&selection) else {
            return Ok(0);
        };
        let thread_id = selected_msg.thread_id();
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return Ok(0);
        };

        let mut commands = Vec::new();
        for index in 0..store.n_items() {
            if let Some(msg) = store.item(index).and_downcast::<MailMessageObject>() {
                if msg.thread_id() == thread_id && msg.is_unread() {
                    commands.push(MailCommand::set_message_state(
                        msg.id(),
                        msg.backend_location(),
                        true,
                        msg.is_flagged(),
                        false,
                        msg.is_flagged(),
                    ));
                }
            }
        }

        if commands.is_empty() {
            return Ok(0);
        }

        let count = commands.len();
        self.execute_mail_batch(commands, "Konversation als gelesen markieren")?;
        Ok(count)
    }

    /// Delete all messages belonging to the currently selected thread.
    pub fn delete_selected_thread(&self) -> Result<usize, MailOperationError> {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return Ok(0);
        };
        let Some(selected_msg) = selected_mail_message(&selection) else {
            return Ok(0);
        };
        let thread_id = selected_msg.thread_id();
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return Ok(0);
        };

        let mut commands = Vec::new();
        for index in 0..store.n_items() {
            if let Some(msg) = store.item(index).and_downcast::<MailMessageObject>() {
                if msg.thread_id() == thread_id {
                    let cmd = match msg.backend_location() {
                        Some(loc) => MailCommand::delete(msg.id(), loc),
                        None => MailCommand::delete_local(msg.id()),
                    };
                    commands.push(cmd);
                }
            }
        }

        if commands.is_empty() {
            return Ok(0);
        }

        let count = commands.len();
        self.execute_mail_batch(commands, "Konversation löschen")?;
        Ok(count)
    }

    /// Open the same local-folder editor used by the sidebar's folder button.
    /// The window action uses this entry point so the Evolution-style File →
    /// New → Folder action and the sidebar affordance share one behavior.
    pub(crate) fn present_new_local_folder_dialog(&self) {
        self.present_local_folder_dialog(None);
    }

    /// Present a dialog to rename the currently selected folder.
    pub(crate) fn present_rename_selected_folder(&self) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        if selection.folder_id.starts_with("custom:") {
            self.present_local_folder_dialog(Some(&selection.folder_id));
        } else if selection.is_remote() {
            self.show_toast(
                "Das Umbenennen von IMAP-/Exchange-Ordnern wird über den Linux-Camel-Pfad synchronisiert.",
            );
        } else {
            self.show_toast("Standard-Ordner wie Posteingang oder Papierkorb können nicht umbenannt werden.");
        }
    }

    /// Delete the currently selected folder with confirmation if valid and empty.
    pub(crate) fn present_delete_selected_folder(&self) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        if selection.folder_id.starts_with("custom:") {
            let folder_id = selection.folder_id.clone();
            let folder_name = selection.folder_name.clone().unwrap_or_else(|| {
                folder_id.strip_prefix("custom:").unwrap_or(&folder_id).to_string()
            });
            let page = self.downgrade();
            let heading = format!("Ordner „{}“ löschen?", folder_name);
            let body = "Der lokale Ordner und seine Zuordnung werden entfernt. Der Ordner muss leer sein.";
            if let Some(root) = self.root().and_downcast::<gtk::Window>() {
                crate::dialogs::run_destructive_action(
                    &root,
                    &heading,
                    body,
                    "Löschen",
                    move || {
                        let result = crate::services::MailStoreService::new().delete_local_folder(&folder_id);
                        if let Some(page) = page.upgrade() {
                            match result {
                                Ok(()) => {
                                    page.show_toast(&format!("Ordner „{}“ gelöscht", folder_name));
                                    page.refresh_mail();
                                }
                                Err(error) => {
                                    page.show_toast(&format!("Ordner konnte nicht gelöscht werden: {error}"));
                                }
                            }
                        }
                    },
                );
            }
        } else if selection.is_remote() {
            self.show_toast(
                "Das Löschen von synchronisierten IMAP-/Exchange-Ordnern wird über den Linux-Camel-Pfad ausgeführt.",
            );
        } else {
            self.show_toast("Standard-Ordner können nicht gelöscht werden.");
        }
    }

    /// Mark all unread messages in the currently selected folder as read.
    pub fn mark_all_read_in_selected_folder(&self) -> Result<usize, MailOperationError> {
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return Ok(0);
        };
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|s| s.borrow().clone())
            .unwrap_or_default();
        
        let mut commands = Vec::new();
        for index in 0..store.n_items() {
            let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                continue;
            };
            if selection.matches(&message) && message.is_unread() {
                commands.push(MailCommand::set_message_state(
                    message.id(),
                    message.backend_location(),
                    true,
                    message.is_flagged(),
                    false,
                    message.is_flagged(),
                ));
            }
        }

        if commands.is_empty() {
            return Ok(0);
        }

        let count = commands.len();
        self.execute_mail_batch(commands, "Alle als gelesen markieren")?;
        Ok(count)
    }

    /// Select the next folder containing unread messages.
    pub fn select_next_unread_folder(&self) -> bool {
        // First check dynamic folder selection
        if let Some(selection) = self.imp().dynamic_folder_selection.borrow().as_ref() {
            let n_items = selection.model().map(|m| m.n_items()).unwrap_or(0);
            let current = selection.selected();
            for offset in 1..=n_items {
                let index = (current.saturating_add(offset)) % n_items;
                if let Some(row) = selection.model().and_then(|m| m.item(index)).and_downcast::<gtk::TreeListRow>() {
                    if let Some(folder) = row.item().and_downcast::<FolderObject>() {
                        if folder.selectable() && folder.unread_count() > 0 {
                            selection.set_selected(index);
                            return true;
                        }
                    }
                }
            }
        }
        
        // Then check local folder counts
        let local_counts = self.current_local_folder_counts();
        for (folder_id, (unread, _)) in &local_counts {
            if *unread > 0 {
                self.select_folder_row(&FolderSelection::from_folder_id(folder_id));
                return true;
            }
        }
        
        false
    }

    /// Present the folder properties dialog for the currently selected folder.
    pub(crate) fn present_folder_properties_dialog(&self) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|s| s.borrow().clone())
            .unwrap_or_default();
        
        let (folder_name, account_name, folder_path, is_remote) = if let Some(ref source_uid) = selection.source_uid {
            let name = selection.folder_name.clone().unwrap_or_else(|| "Posteingang".to_string());
            let account = self.imp().source_selection.borrow().as_ref().and_then(|sel| {
                sel.model().and_then(|model| {
                    (0..model.n_items()).find_map(|idx| {
                        model.item(idx).and_downcast::<EdsSourceObject>().filter(|s| s.uid() == *source_uid).map(|s| s.display_name())
                    })
                })
            }).unwrap_or_else(|| "Synchronisiertes Konto".to_string());
            let path = format!("camel://{source_uid}/{name}");
            (name, account, path, true)
        } else {
            let name = match selection.folder_id.as_str() {
                "all" => "Alle Nachrichten".to_string(),
                "inbox" => "Posteingang".to_string(),
                "sent" => "Gesendet".to_string(),
                "drafts" => "Entwürfe".to_string(),
                "trash" => "Papierkorb".to_string(),
                "archive" => "Archiv".to_string(),
                "spam" => "Spam".to_string(),
                custom if custom.starts_with("custom:") => custom.strip_prefix("custom:").unwrap_or(custom).to_string(),
                other => other.to_string(),
            };
            let path = format!("local://{}", selection.folder_id);
            (name, "Lokale Ordner".to_string(), path, false)
        };

        let counts = self.current_local_folder_counts();
        let (unread_count, total_count) = counts.get(&selection.folder_id).copied().unwrap_or((0, 0));

        let dialog = crate::dialogs::FolderPropertiesDialog::new(
            &folder_name,
            &account_name,
            &folder_path,
            total_count,
            unread_count,
            is_remote,
        );
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }
        dialog.present();
    }

    /// Present the folder subscriptions dialog for remote folders.
    pub(crate) fn present_folder_subscriptions_dialog(&self) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|s| s.borrow().clone())
            .unwrap_or_default();
        
        let (account_name, source_uid) = if let Some(ref source_uid) = selection.source_uid {
            let account = self.imp().source_selection.borrow().as_ref().and_then(|sel| {
                sel.model().and_then(|model| {
                    (0..model.n_items()).find_map(|idx| {
                        model.item(idx).and_downcast::<EdsSourceObject>().filter(|s| s.uid() == *source_uid).map(|s| s.display_name())
                    })
                })
            }).unwrap_or_else(|| "Konto".to_string());
            (account, Some(source_uid.as_str()))
        } else {
            ("Lokale Ordner".to_string(), None)
        };

        let mut available_folders = Vec::new();
        if let Some(store) = self.imp().dynamic_folder_store.borrow().as_ref() {
            for idx in 0..store.n_items() {
                if let Some(folder) = store.item(idx).and_downcast::<FolderObject>() {
                    if folder.selectable() {
                        available_folders.push(crate::dialogs::SubscribedFolderItem {
                            name: folder.name(),
                            path: folder.folder_name().unwrap_or_else(|| folder.id()),
                            is_subscribed: true,
                        });
                    }
                }
            }
        }

        if available_folders.is_empty() {
            for (id, _, label, _) in [
                ("inbox", "", "Posteingang", ""),
                ("sent", "", "Gesendet", ""),
                ("drafts", "", "Entwürfe", ""),
                ("trash", "", "Papierkorb", ""),
                ("archive", "", "Archiv", ""),
                ("spam", "", "Spam", ""),
            ] {
                available_folders.push(crate::dialogs::SubscribedFolderItem {
                    name: label.to_string(),
                    path: id.to_string(),
                    is_subscribed: true,
                });
            }
        }

        let dialog = crate::dialogs::FolderSubscriptionsDialog::new(
            &account_name,
            source_uid,
            available_folders,
        );
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }
        dialog.present();
    }

    /// Empty the portable app-owned trash without blocking GTK.  Imported
    /// source files are never rewritten.  A native EDS/Camel trash still
    /// needs its backend expunge operation and is reported explicitly instead
    /// of being silently treated as a local sidecar mutation.
    pub(crate) fn empty_trash_async(&self, success_label: &str) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        let active_request = self.imp().active_request.borrow().clone();
        let has_remote_selection = selection.is_remote()
            || active_request
                .as_ref()
                .is_some_and(|request| request.source_uid.is_some());
        let has_loaded_remote_trash =
            self.imp()
                .message_store
                .borrow()
                .as_ref()
                .is_some_and(|store| {
                    (0..store.n_items()).any(|index| {
                        store
                            .item(index)
                            .and_downcast::<MailMessageObject>()
                            .is_some_and(|message| {
                                message.backend_location().is_some() && message_is_trash(&message)
                            })
                    })
                });
        if has_remote_selection || has_loaded_remote_trash {
            self.show_toast(
                "Dieser Kontoordner benötigt zum endgültigen Leeren den Linux-Expunge-Pfad; der lokale Bestand wurde nicht verändert.",
            );
            return;
        }

        let worker_scope = self.mail_worker_ui_scope();
        let store_config = mail_store_worker_config_from_main_thread();
        let success_label = success_label.to_string();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = MailStoreService::new_worker_safe(store_config)
                .empty_local_trash()
                .map_err(MailOperationError::from);
            let _ = sender.send_blocking(result);
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err(MailOperationError::Backend {
                    operation: crate::services::mail_operations_service::MailOperationKind::Delete,
                    message: "Papierkorb-Vorgang wurde abgebrochen".to_string(),
                })
            });
            let Some(page) = page.upgrade() else {
                return;
            };
            if !page.mail_worker_ui_scope_is_current(&worker_scope) {
                tracing::debug!("Ignoring stale empty-trash result for a replaced mail scope");
                return;
            }
            match result {
                Ok(count) => {
                    page.clear_batch_selection();
                    if count > 0 {
                        page.refresh_mail();
                        if let Some(state) = page.imp().message_state.borrow().as_ref() {
                            state.set_label(&format!(
                                "{count} lokale Nachricht(en) aus dem Papierkorb entfernt"
                            ));
                        }
                        page.show_toast(&format!("{success_label}: {count} Nachricht(en)"));
                    } else {
                        page.show_toast("Papierkorb ist bereits leer");
                    }
                }
                Err(error) => {
                    page.show_toast(&format!("Papierkorb konnte nicht geleert werden: {error}"))
                }
            }
        });
    }

    pub(crate) fn empty_junk_async(&self, success_label: &str) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        let active_request = self.imp().active_request.borrow().clone();
        let has_remote_selection = selection.is_remote()
            || active_request
                .as_ref()
                .is_some_and(|request| request.source_uid.is_some());
        let has_loaded_remote_junk =
            self.imp()
                .message_store
                .borrow()
                .as_ref()
                .is_some_and(|store| {
                    (0..store.n_items()).any(|index| {
                        store
                            .item(index)
                            .and_downcast::<MailMessageObject>()
                            .is_some_and(|message| {
                                message.backend_location().is_some() && message_is_junk(&message)
                            })
                    })
                });
        if has_remote_selection || has_loaded_remote_junk {
            self.show_toast(
                "Dieser Kontoordner benötigt zum endgültigen Leeren den Linux-Expunge-Pfad; der lokale Bestand wurde nicht verändert.",
            );
            return;
        }

        let worker_scope = self.mail_worker_ui_scope();
        let store_config = mail_store_worker_config_from_main_thread();
        let success_label = success_label.to_string();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = MailStoreService::new_worker_safe(store_config)
                .empty_local_junk()
                .map_err(MailOperationError::from);
            let _ = sender.send_blocking(result);
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err(MailOperationError::Backend {
                    operation: crate::services::mail_operations_service::MailOperationKind::Delete,
                    message: "Spam-Vorgang wurde abgebrochen".to_string(),
                })
            });
            let Some(page) = page.upgrade() else {
                return;
            };
            if !page.mail_worker_ui_scope_is_current(&worker_scope) {
                tracing::debug!("Ignoring stale empty-junk result for a replaced mail scope");
                return;
            }
            match result {
                Ok(count) => {
                    page.clear_batch_selection();
                    if count > 0 {
                        page.refresh_mail();
                        if let Some(state) = page.imp().message_state.borrow().as_ref() {
                            state.set_label(&format!(
                                "{count} lokale Nachricht(en) aus dem Spam-Ordner entfernt"
                            ));
                        }
                        page.show_toast(&format!("{success_label}: {count} Nachricht(en)"));
                    } else {
                        page.show_toast("Spam-Ordner ist bereits leer");
                    }
                }
                Err(error) => {
                    page.show_toast(&format!("Spam-Ordner konnte nicht geleert werden: {error}"))
                }
            }
        });
    }

    fn load_mail_history_async(&self) {
        let generation = self.imp().mail_history_generation.get();
        let history_path = MailCommandStack::capture_persistent_path();
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .mail_history_load_cancellation
            .replace(Some(cancellation.clone()));

        let (sender, receiver) = async_channel::bounded(1);
        let worker_cancellation = cancellation.clone();
        std::thread::spawn(move || {
            let history = MailCommandStack::load_persistent_from_path(history_path);
            if !worker_cancellation.load(Ordering::Relaxed) {
                let _ = sender.send_blocking(history);
            }
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let history = receiver.recv().await.ok();
            let Some(page) = page.upgrade() else {
                return;
            };
            let is_current = page
                .imp()
                .mail_history_load_cancellation
                .borrow()
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancellation));
            if !is_current {
                return;
            }
            page.imp()
                .mail_history_load_cancellation
                .borrow_mut()
                .take();

            if let Some(history) = history
                && !cancellation.load(Ordering::Relaxed)
                && page.imp().mail_history_generation.get() == generation
            {
                page.imp().mail_commands.replace(history);
            }
        });
    }

    fn persist_mail_history_async(&self) {
        let sequence = self
            .imp()
            .mail_history_persist_sequence
            .get()
            .saturating_add(1);
        self.imp().mail_history_persist_sequence.set(sequence);
        let snapshot = self.imp().mail_commands.borrow().clone();
        let persisted_sequence = Arc::clone(&self.imp().mail_history_persisted_sequence);
        std::thread::spawn(move || {
            let mut last_persisted = persisted_sequence
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if sequence <= *last_persisted {
                return;
            }
            snapshot.persist_snapshot();
            *last_persisted = sequence;
        });
    }

    pub fn undo(&self) -> Result<bool, MailOperationError> {
        let Some(operation) = self.imp().mail_commands.borrow().undo_operation_kind() else {
            return Ok(false);
        };
        self.execute_history_action(true, operation)?;
        Ok(true)
    }

    pub fn redo(&self) -> Result<bool, MailOperationError> {
        let Some(operation) = self.imp().mail_commands.borrow().redo_operation_kind() else {
            return Ok(false);
        };
        self.execute_history_action(false, operation)?;
        Ok(true)
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
        Ok(true)
    }

    pub fn mark_selected_read(&self) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        let command = MailCommand::set_message_state(
            message.id(),
            message.backend_location(),
            message.is_unread(),
            message.is_flagged(),
            false,
            message.is_flagged(),
        );
        self.execute_mail_command(command)?;
        Ok(true)
    }

    pub fn toggle_selected_flagged(&self) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        if message
            .backend_location()
            .as_ref()
            .is_some_and(|location| is_graph_backend_location(Some(location)))
        {
            return Err(MailOperationError::Unsupported {
                operation:
                    crate::services::mail_operations_service::MailOperationKind::SetMessageState,
                backend: "Graph (bitte die separate Nachverfolgung verwenden)",
            });
        }
        let next_flagged = !message.is_flagged();
        let command = MailCommand::set_message_state(
            message.id(),
            message.backend_location(),
            message.is_unread(),
            message.is_flagged(),
            message.is_unread(),
            next_flagged,
        );
        self.execute_mail_command(command)?;
        Ok(true)
    }

    fn set_message_follow_up(
        &self,
        message: &MailMessageObject,
        metadata: MailFollowUpMetadata,
    ) -> Result<(), MailOperationError> {
        let command = MailCommand::set_follow_up(
            message.id(),
            message.backend_location(),
            message.follow_up(),
            metadata,
        );
        self.execute_mail_command(command)
    }

    /// Present Evolution's explicit follow-up editor for the selected
    /// message. The ordinary star remains a separate action and column.
    pub fn edit_selected_follow_up(&self) -> bool {
        let Some(message) = self.selected_message() else {
            return false;
        };
        self.present_follow_up_editor(&message);
        true
    }

    fn present_follow_up_editor(&self, message: &MailMessageObject) {
        let dialog = adw::Window::builder()
            .title("Nachverfolgung")
            .modal(true)
            .default_width(420)
            .build();
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label("Abbrechen");
        let save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        let group = adw::PreferencesGroup::builder()
            .title("Nachverfolgung")
            .build();
        let text = adw::EntryRow::builder().title("Kennzeichnung").build();
        let due = adw::EntryRow::builder()
            .title("Fällig (YYYY-MM-DD HH:MM, optional)")
            .build();
        let current = message.follow_up();
        text.set_text(current.text.as_deref().unwrap_or(DEFAULT_FOLLOW_UP_TEXT));
        due.set_text(&display_follow_up_due(current.due_by.as_deref()));
        group.add(&text);
        group.add(&due);
        content.append(&group);

        let is_graph = message
            .backend_location()
            .as_ref()
            .is_some_and(|location| is_graph_backend_location(Some(location)));
        if is_graph {
            due.set_sensitive(false);
            let warning = gtk::Label::builder()
                .label("Microsoft Graph unterstützt keine Follow-up-Fälligkeit.")
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(vec!["warning", "dim-label"])
                .build();
            content.append(&warning);
        }

        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        let clear = gtk::Button::with_label("Entfernen");
        let complete = gtk::Button::with_label("Als erledigt markieren");
        complete.set_sensitive(current.state != MailFollowUpState::None);
        actions.append(&clear);
        actions.append(&complete);
        content.append(&actions);
        toolbar.set_content(Some(&content));
        dialog.set_content(Some(&toolbar));

        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let page = self.downgrade();
        let message_for_save = message.clone();
        let dialog_for_save = dialog.clone();
        save.connect_clicked(move |_| {
            let due_by = match parse_follow_up_due_input(&due.text()) {
                Ok(due_by) => due_by,
                Err(error) => {
                    if let Some(page) = page.upgrade() {
                        page.show_toast(&error);
                    }
                    return;
                }
            };
            let metadata = MailFollowUpMetadata::pending(Some(text.text().to_string()), due_by);
            if let Some(page) = page.upgrade() {
                match page.set_message_follow_up(&message_for_save, metadata) {
                    Ok(()) => dialog_for_save.close(),
                    Err(error) => {
                        page.show_toast(&format!("Nachverfolgung nicht möglich: {error}"))
                    }
                }
            }
        });
        let page = self.downgrade();
        let message_for_clear = message.clone();
        let dialog_for_clear = dialog.clone();
        clear.connect_clicked(move |_| {
            if let Some(page) = page.upgrade() {
                match page.set_message_follow_up(&message_for_clear, MailFollowUpMetadata::none()) {
                    Ok(()) => dialog_for_clear.close(),
                    Err(error) => {
                        page.show_toast(&format!("Nachverfolgung nicht möglich: {error}"))
                    }
                }
            }
        });
        let page = self.downgrade();
        let message_for_complete = message.clone();
        let dialog_for_complete = dialog.clone();
        complete.connect_clicked(move |_| {
            let metadata = MailFollowUpMetadata::completed_from(
                &message_for_complete.follow_up(),
                Utc::now().to_rfc2822(),
            );
            if let Some(page) = page.upgrade() {
                match page.set_message_follow_up(&message_for_complete, metadata) {
                    Ok(()) => dialog_for_complete.close(),
                    Err(error) => {
                        page.show_toast(&format!("Nachverfolgung nicht möglich: {error}"))
                    }
                }
            }
        });
        dialog.present();
    }

    pub fn toggle_selected_important(&self) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        if message
            .backend_location()
            .as_ref()
            .is_some_and(|location| !is_graph_backend_location(Some(location)))
        {
            return Err(MailOperationError::Unsupported {
                operation: if message.is_important() {
                    crate::services::mail_operations_service::MailOperationKind::MarkNotImportant
                } else {
                    crate::services::mail_operations_service::MailOperationKind::MarkImportant
                },
                backend: "Wichtigkeitsmarkierung dieses Kontos (Linux-Abnahme ausstehend)",
            });
        }
        let next_important = !message.is_important();
        let command = message
            .backend_location()
            .map(|location| {
                MailCommand::set_important(
                    message.id(),
                    location,
                    message.is_important(),
                    next_important,
                )
            })
            .unwrap_or_else(|| {
                MailCommand::set_important_local(
                    message.id(),
                    message.is_important(),
                    next_important,
                )
            });
        self.execute_mail_command(command)?;
        Ok(true)
    }

    fn set_selected_label(&self, label: &str, labeled: bool) -> Result<bool, MailOperationError> {
        let label = label.trim();
        if label.is_empty() {
            return Ok(false);
        }
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        if message
            .backend_location()
            .as_ref()
            .is_some_and(|location| !is_graph_backend_location(Some(location)))
        {
            return Err(MailOperationError::Unsupported {
                operation: crate::services::mail_operations_service::MailOperationKind::SetLabel,
                backend: "Mehrfach-Labels dieses Kontos",
            });
        }

        let before = message.has_label(label);
        if before == labeled {
            return Ok(true);
        }
        let command = message
            .backend_location()
            .map(|location| {
                MailCommand::set_label_for_location(message.id(), location, label, before, labeled)
            })
            .unwrap_or_else(|| MailCommand::set_label_local(message.id(), label, before, labeled));
        self.execute_mail_command(command)?;
        Ok(true)
    }

    fn set_batch_label(&self, label: &str, labeled: bool) -> Result<usize, MailOperationError> {
        let label = label.trim();
        if label.is_empty() {
            return Ok(0);
        }
        let messages = self.selected_batch_messages();
        if messages.is_empty() {
            return Ok(0);
        }
        if messages.iter().any(|message| {
            message
                .backend_location()
                .as_ref()
                .is_some_and(|location| !is_graph_backend_location(Some(location)))
        }) {
            return Err(MailOperationError::Unsupported {
                operation: crate::services::mail_operations_service::MailOperationKind::SetLabel,
                backend: "Mehrfach-Labels dieses Kontos",
            });
        }

        let commands = messages
            .iter()
            .filter_map(|message| {
                let before = message.has_label(label);
                (before != labeled).then(|| {
                    message
                        .backend_location()
                        .map(|location| {
                            MailCommand::set_label_for_location(
                                message.id(),
                                location,
                                label,
                                before,
                                labeled,
                            )
                        })
                        .unwrap_or_else(|| {
                            MailCommand::set_label_local(message.id(), label, before, labeled)
                        })
                })
            })
            .collect::<Vec<_>>();
        let count = commands.len();
        if count > 0 {
            self.execute_mail_batch(commands, "Labels ändern")?;
        }
        Ok(count)
    }

    pub(crate) fn present_label_editor(&self) {
        let has_batch = !self.imp().batch_selection.borrow().is_empty();
        if !has_batch && self.selected_message().is_none() {
            self.show_toast("Keine Nachricht ausgewählt");
            return;
        }
        let Some(parent) = self.root().and_downcast::<gtk::Window>() else {
            tracing::warn!("Cannot open label editor without a parent window");
            return;
        };

        let dialog = adw::Window::builder()
            .title(if has_batch {
                "Label für Auswahl"
            } else {
                "Label der Nachricht"
            })
            .modal(true)
            .default_width(420)
            .default_height(180)
            .build();
        dialog.set_transient_for(Some(&parent));

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let set_button = gtk::Button::builder()
            .label("Setzen")
            .css_classes(vec!["suggested-action"])
            .build();
        let remove_button = gtk::Button::builder()
            .label("Entfernen")
            .css_classes(vec!["destructive-action"])
            .build();
        header.pack_start(&remove_button);
        header.pack_end(&set_button);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(20)
            .margin_end(20)
            .margin_top(20)
            .margin_bottom(20)
            .build();
        let label_entry = adw::EntryRow::builder()
            .title("Label")
            .show_apply_button(false)
            .build();
        label_entry.set_tooltip_text(Some("z. B. Arbeit oder Rechnungen"));
        content.append(&label_entry);
        content.append(
            &gtk::Label::builder()
                .label("Labels werden bei lokalen/importierten Nachrichten im app-eigenen Sidecar gespeichert.")
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(vec!["caption", "dim-label"])
                .build(),
        );
        toolbar.set_content(Some(&content));
        dialog.set_content(Some(&toolbar));

        let page_for_set = self.downgrade();
        let dialog_for_set = dialog.clone();
        let entry_for_set = label_entry.clone();
        set_button.connect_clicked(move |_| {
            let label = entry_for_set.text().trim().to_string();
            let Some(page) = page_for_set.upgrade() else {
                return;
            };
            if label.is_empty() {
                page.show_toast("Bitte einen Labelnamen eingeben");
                return;
            }
            let result = if has_batch {
                page.set_batch_label(&label, true)
                    .map(|count| format!("{count} Nachricht(en) mit Label versehen"))
            } else {
                page.set_selected_label(&label, true)
                    .map(|_| "Label gesetzt".to_string())
            };
            match result {
                Ok(message) => {
                    page.show_toast(&message);
                    dialog_for_set.close();
                }
                Err(error) => {
                    page.show_toast(&format!("Label konnte nicht gesetzt werden: {error}"))
                }
            }
        });

        let page_for_remove = self.downgrade();
        let dialog_for_remove = dialog.clone();
        let entry_for_remove = label_entry.clone();
        remove_button.connect_clicked(move |_| {
            let label = entry_for_remove.text().trim().to_string();
            let Some(page) = page_for_remove.upgrade() else {
                return;
            };
            if label.is_empty() {
                page.show_toast("Bitte einen Labelnamen eingeben");
                return;
            }
            let result = if has_batch {
                page.set_batch_label(&label, false)
                    .map(|count| format!("Label von {count} Nachricht(en) entfernt"))
            } else {
                page.set_selected_label(&label, false)
                    .map(|_| "Label entfernt".to_string())
            };
            match result {
                Ok(message) => {
                    page.show_toast(&message);
                    dialog_for_remove.close();
                }
                Err(error) => {
                    page.show_toast(&format!("Label konnte nicht entfernt werden: {error}"))
                }
            }
        });

        dialog.present();
    }

    pub(crate) fn present_folder_destination_dialog(&self, copy: bool) {
        let Some(message) = self.selected_message() else {
            self.show_toast("Keine Nachricht ausgewählt");
            return;
        };
        let destinations = if copy {
            self.copy_folder_destinations_for_messages(std::slice::from_ref(&message))
        } else {
            self.folder_destinations_for_messages(std::slice::from_ref(&message))
        };
        if destinations.is_empty() {
            self.show_toast("Keine verfügbaren Zielordner");
            return;
        }
        let Some(parent) = self.root().and_downcast::<gtk::Window>() else {
            tracing::warn!("Cannot open folder destination dialog without a parent window");
            return;
        };

        let title = if copy {
            "Nachricht kopieren"
        } else {
            "Nachricht verschieben"
        };
        let dialog = adw::Window::builder()
            .title(title)
            .modal(true)
            .default_width(420)
            .default_height(520)
            .build();
        dialog.set_transient_for(Some(&parent));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        for destination in destinations {
            let button = gtk::Button::builder()
                .label(&destination.label)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .css_classes(vec!["flat"])
                .build();
            let page = self.downgrade();
            let dialog_weak = dialog.downgrade();
            let source_uid = destination.source_uid;
            let folder_name = destination.folder_name;
            button.connect_clicked(move |_| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                let result = if copy {
                    page.copy_selected_to(&source_uid, &folder_name)
                } else {
                    page.move_selected_to(&source_uid, &folder_name)
                };
                match result {
                    Ok(true) => {
                        page.show_toast(if copy {
                            "Nachricht wird kopiert"
                        } else {
                            "Nachricht wird verschoben"
                        });
                        if let Some(dialog) = dialog_weak.upgrade() {
                            dialog.close();
                        }
                    }
                    Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                    Err(error) => page.show_toast(&format!("{title} nicht möglich: {error}")),
                }
            });
            list.append(&button);
        }
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        content.append(&list);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content)
            .build();
        toolbar.set_content(Some(&scrolled));
        dialog.set_content(Some(&toolbar));
        dialog.present();
    }

    pub fn archive_selected(&self) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        let command = message
            .backend_location()
            .map(|location| MailCommand::archive(message.id(), location))
            .unwrap_or_else(|| MailCommand::archive_local(message.id(), message.folder_id()));
        self.execute_mail_command(command)?;
        Ok(true)
    }

    pub fn mark_selected_junk(&self) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        let command = message
            .backend_location()
            .map(|location| {
                MailCommand::set_junk(message.id(), location, message_is_junk(&message), true)
            })
            .unwrap_or_else(|| MailCommand::set_junk_local(message.id(), message.folder_id()));
        self.execute_mail_command(command)?;
        Ok(true)
    }

    pub fn mark_selected_not_junk(&self) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        if !message_is_junk(&message) {
            return Ok(true);
        }
        let command = message
            .backend_location()
            .map(|location| MailCommand::set_junk(message.id(), location, true, false))
            .unwrap_or_else(|| MailCommand::clear_junk_local(message.id()));
        self.execute_mail_command(command)?;
        Ok(true)
    }

    pub fn apply_filters(&self) {
        let worker_scope = self.mail_worker_ui_scope();
        let store_config = mail_store_worker_config_from_main_thread();
        let filter_store = MailFilterStore::default();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = MailStoreService::new_worker_safe(store_config)
                .run_all_active_filters_with_store(&filter_store);
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Apply filters result channel dropped");
            }
        });

        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !page.mail_worker_ui_scope_is_current(&worker_scope) {
                tracing::debug!("Ignoring stale apply-filters result for a replaced mail scope");
                return;
            }
            match result {
                Ok(Ok(report)) => {
                    page.refresh_mail();
                    if report.matched_messages == 0 {
                        page.show_toast("Keine passenden Nachrichten für aktive Filterregeln");
                    } else {
                        page.show_toast(&format!(
                            "Filter angewendet: {} Treffer · {} verschoben · {} gelöscht",
                            report.matched_messages, report.moved_messages, report.deleted_messages,
                        ));
                    }
                }
                Ok(Err(error)) => {
                    page.show_toast(&format!("Filterlauf fehlgeschlagen: {error}"));
                }
                Err(error) => {
                    page.show_toast(&format!("Filterlauf abgebrochen: {error}"));
                }
            }
        });
    }

    fn move_selected_to(
        &self,
        destination_source: &str,
        destination_folder: &str,
    ) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };

        let command = if destination_source == "local" {
            if message.backend_location().is_some() {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Move,
                    backend: "Nachricht aus einem Konto in den lokalen Import",
                });
            }
            MailCommand::move_local(
                message.id(),
                message.folder_id(),
                destination_folder.to_string(),
            )
        } else {
            let Some(location) = message.backend_location() else {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Move,
                    backend: "lokaler Import in ein Konto",
                });
            };
            if location.source_uid != destination_source {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Move,
                    backend: "anderes Konto",
                });
            }
            MailCommand::move_to(message.id(), location, destination_folder.to_string())
        };

        self.execute_mail_command(command)?;
        Ok(true)
    }

    fn copy_selected_to(
        &self,
        destination_source: &str,
        destination_folder: &str,
    ) -> Result<bool, MailOperationError> {
        let Some(message) = self.selected_message() else {
            return Ok(false);
        };
        let command = if destination_source == "local" {
            if message.backend_location().is_some() {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Copy,
                    backend: "Online-Nachricht in den lokalen Import",
                });
            }
            if message.id().contains("::copy::") {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Copy,
                    backend: "lokale Kopie einer Kopie",
                });
            }
            if message.folder_id().eq_ignore_ascii_case(destination_folder) {
                return Ok(true);
            }
            MailCommand::copy_local(message.id(), destination_folder.to_string())
        } else {
            let Some(location) = message.backend_location() else {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Copy,
                    backend: "lokaler Import in ein Graph-Konto",
                });
            };
            if location.source_uid != destination_source {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Copy,
                    backend: "anderes Online-Konto",
                });
            }
            if !is_graph_backend_location(Some(&location)) {
                return Err(MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Copy,
                    backend: "Kopieren für dieses Konto (Linux-Abnahme ausstehend)",
                });
            }
            // A Graph copy intentionally remains allowed in the same folder;
            // Outlook creates a distinct message resource in that case.
            MailCommand::copy_remote(message.id(), location, destination_folder.to_string())
        };

        self.execute_mail_command(command)?;
        Ok(true)
    }

    fn execute_mail_command(&self, command: MailCommand) -> Result<(), MailOperationError> {
        if self.imp().mail_batch_running.get() {
            return Err(MailOperationError::Busy {
                operation: command.operation_kind(),
                message: "laufender Mehrfachauswahl".to_string(),
            });
        }
        let description = command.description().to_string();
        let prepared = self.imp().mail_operation_coordinator.prepare(command)?;
        let context = prepared.context();
        let operation_id = context.id().get();
        self.imp()
            .mail_operation_cancellations
            .borrow_mut()
            .insert(operation_id, prepared.cancellation());
        self.imp()
            .mail_operations_active
            .set(self.imp().mail_operations_active.get().saturating_add(1));

        self.clear_operation_error();
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{description} …"));
        }

        let worker_scope = self.mail_worker_ui_scope();
        let (sender, receiver) = async_channel::bounded(1);
        let store_config = mail_store_worker_config_from_main_thread();
        std::thread::spawn(move || {
            let operations = MailOperationsService::new_worker_safe(store_config);
            let result = prepared.run(&operations);
            let _ = sender.send_blocking(result);
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page.upgrade() else {
                return;
            };
            page.imp()
                .mail_operation_cancellations
                .borrow_mut()
                .remove(&operation_id);
            page.imp()
                .mail_operations_active
                .set(page.imp().mail_operations_active.get().saturating_sub(1));

            let scope_is_current = page.mail_worker_ui_scope_is_current(&worker_scope);

            match result {
                Ok(Ok(completed)) => {
                    let cancellation_requested = completed.cancellation_requested();
                    completed.commit(&mut page.imp().mail_commands.borrow_mut());
                    page.imp()
                        .mail_history_generation
                        .set(page.imp().mail_history_generation.get().saturating_add(1));
                    page.persist_mail_history_async();
                    if scope_is_current {
                        page.clear_operation_error();
                        page.refresh_mail();
                        if let Some(state) = page.imp().message_state.borrow().as_ref() {
                            let status = if cancellation_requested {
                                "Vorgang wurde bereits serverseitig abgeschlossen".to_string()
                            } else {
                                format!("{description} abgeschlossen")
                            };
                            state.set_label(&status);
                        }
                    }
                }
                Ok(Err(error)) => {
                    if scope_is_current {
                        page.report_operation_error(&description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));
                        // Existing callers may still apply a short-lived local UI
                        // preview. Reloading here reconciles it after any failure.
                        page.refresh_mail();
                    }
                }
                Err(_) => {
                    let error = MailOperationError::Io(std::io::Error::other(
                        "Mail-Worker wurde unerwartet beendet",
                    ));
                    if scope_is_current {
                        page.report_operation_error(&description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));
                        page.refresh_mail();
                    }
                }
            }
        });
        Ok(())
    }

    fn execute_mail_batch(
        &self,
        commands: Vec<MailCommand>,
        description: &str,
    ) -> Result<usize, MailOperationError> {
        let Some(first) = commands.first() else {
            return Ok(0);
        };
        let operation = first.operation_kind();
        if self.imp().mail_batch_running.get() || self.imp().mail_operations_active.get() > 0 {
            return Err(MailOperationError::Busy {
                operation,
                message: "anderer Mailaktion".to_string(),
            });
        }

        let count = commands.len();
        let context = MailOperationContext::new();
        let operation_id = context.id().get();
        self.imp().mail_batch_running.set(true);
        self.imp()
            .mail_operation_cancellations
            .borrow_mut()
            .insert(operation_id, context.cancellation());
        self.clear_operation_error();
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{description} …"));
        }

        let worker_scope = self.mail_worker_ui_scope();
        let (sender, receiver) = async_channel::bounded(1);
        let store_config = mail_store_worker_config_from_main_thread();
        std::thread::spawn(move || {
            let operations = MailOperationsService::new_worker_safe(store_config);
            let result =
                MailCommandStack::execute_batch_for_worker(&operations, commands, &context);
            let _ = sender.send_blocking(result);
        });

        let page = self.downgrade();
        let description = description.to_string();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page.upgrade() else {
                return;
            };
            page.imp().mail_batch_running.set(false);
            page.imp()
                .mail_operation_cancellations
                .borrow_mut()
                .remove(&operation_id);

            let scope_is_current = page.mail_worker_ui_scope_is_current(&worker_scope);

            match result {
                Ok(Ok(completed)) => {
                    let applied = completed.len();
                    completed.commit(&mut page.imp().mail_commands.borrow_mut());
                    page.imp()
                        .mail_history_generation
                        .set(page.imp().mail_history_generation.get().saturating_add(1));
                    page.persist_mail_history_async();
                    if scope_is_current {
                        page.clear_operation_error();
                        page.clear_batch_selection();
                        page.refresh_mail();
                        if let Some(state) = page.imp().message_state.borrow().as_ref() {
                            state.set_label(&format!("{description}: {applied} abgeschlossen"));
                        }
                    }
                }
                Ok(Err(error)) => {
                    if scope_is_current {
                        page.report_operation_error(&description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));
                        page.refresh_mail();
                    }
                }
                Err(_) => {
                    let error = MailOperationError::Io(std::io::Error::other(
                        "Mail-Batch-Worker wurde unerwartet beendet",
                    ));
                    if scope_is_current {
                        page.report_operation_error(&description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));
                        page.refresh_mail();
                    }
                }
            }
        });
        Ok(count)
    }

    fn execute_history_action(
        &self,
        undo: bool,
        operation: crate::services::mail_operations_service::MailOperationKind,
    ) -> Result<(), MailOperationError> {
        if self.imp().mail_batch_running.get() || self.imp().mail_operations_active.get() > 0 {
            return Err(MailOperationError::Busy {
                operation,
                message: "anderer Mailaktion".to_string(),
            });
        }

        let description = if undo { "Rückgängig" } else { "Wiederholen" };
        let history = self.imp().mail_commands.borrow().clone();
        let context = MailOperationContext::new();
        let operation_id = context.id().get();
        self.imp().mail_batch_running.set(true);
        self.imp()
            .mail_operation_cancellations
            .borrow_mut()
            .insert(operation_id, context.cancellation());
        self.clear_operation_error();
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label(&format!("{description} …"));
        }

        let worker_scope = self.mail_worker_ui_scope();
        let (sender, receiver) = async_channel::bounded(1);
        let store_config = mail_store_worker_config_from_main_thread();
        std::thread::spawn(move || {
            let operations = MailOperationsService::new_worker_safe(store_config);
            let result = MailCommandStack::execute_history_action_for_worker(
                history,
                &operations,
                undo,
                operation,
                &context,
            )
            .map(|completed| {
                let (history, command) = completed.into_parts();
                history.persist_snapshot();
                (history, command)
            });
            let _ = sender.send_blocking(result);
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page.upgrade() else {
                return;
            };
            page.imp().mail_batch_running.set(false);
            page.imp()
                .mail_operation_cancellations
                .borrow_mut()
                .remove(&operation_id);

            let scope_is_current = page.mail_worker_ui_scope_is_current(&worker_scope);

            match result {
                Ok(Ok((history, Some(command)))) => {
                    page.imp().mail_commands.replace(history);
                    page.imp()
                        .mail_history_generation
                        .set(page.imp().mail_history_generation.get().saturating_add(1));
                    tracing::info!(
                        operation = command.description(),
                        undo,
                        "Mail history action confirmed"
                    );
                    if scope_is_current {
                        page.clear_operation_error();
                        page.refresh_mail();
                        if let Some(state) = page.imp().message_state.borrow().as_ref() {
                            state.set_label(&format!("{description} abgeschlossen"));
                        }
                    }
                }
                Ok(Ok((history, None))) => {
                    page.imp().mail_commands.replace(history);
                    if scope_is_current {
                        if let Some(state) = page.imp().message_state.borrow().as_ref() {
                            state.set_label(&format!("Nichts zum {description}"));
                        }
                    }
                }
                Ok(Err(error)) => {
                    if scope_is_current {
                        page.report_operation_error(description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));
                        page.refresh_mail();
                    }
                }
                Err(_) => {
                    let error = MailOperationError::Io(std::io::Error::other(
                        "Mail-Historien-Worker wurde unerwartet beendet",
                    ));
                    if scope_is_current {
                        page.report_operation_error(description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));
                        page.refresh_mail();
                    }
                }
            }
        });
        Ok(())
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

    pub fn select_all_visible_messages(&self) -> usize {
        let Some(store) = self.imp().message_store.borrow().as_ref().cloned() else {
            return 0;
        };
        let query = self
            .imp()
            .search_query
            .borrow()
            .as_ref()
            .map(|query| query.borrow().clone())
            .unwrap_or_default();
        let folder_selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        let message_filter = self
            .imp()
            .message_filter_state
            .borrow()
            .as_ref()
            .map(|filter| filter.get())
            .unwrap_or(MessageFilter::All);

        let search_scope = self.imp().search_scope.get();
        let mut batch_selection = self.imp().batch_selection.borrow_mut();
        for index in 0..store.n_items() {
            let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                continue;
            };
            if message_matches_filter(
                &message,
                &query,
                message_filter,
                &folder_selection,
                search_scope,
            ) {
                batch_selection.insert(message.id());
            }
        }
        let count = batch_selection.len();
        drop(batch_selection);

        let item_count = store.n_items();
        if item_count > 0 {
            store.items_changed(0, item_count, item_count);
        }
        self.update_batch_action_bar();
        count
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
            self.execute_mail_batch(commands, "Als gelesen markieren")?;
        }
        Ok(())
    }

    fn mark_batch_unread(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        let commands = messages
            .iter()
            .filter(|message| !message.is_unread())
            .map(|message| {
                MailCommand::set_message_state(
                    message.id(),
                    message.backend_location(),
                    message.is_unread(),
                    message.is_flagged(),
                    true,
                    message.is_flagged(),
                )
            })
            .collect::<Vec<_>>();
        let count = commands.len();
        if count > 0 {
            self.execute_mail_batch(commands, "Als ungelesen markieren")?;
        }
        Ok(())
    }

    fn toggle_batch_flagged(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        if messages.iter().any(|message| {
            message
                .backend_location()
                .as_ref()
                .is_some_and(|location| is_graph_backend_location(Some(location)))
        }) {
            return Err(MailOperationError::Unsupported {
                operation:
                    crate::services::mail_operations_service::MailOperationKind::SetMessageState,
                backend: "Graph (bitte die separate Nachverfolgung verwenden)",
            });
        }
        let next_flagged = messages.iter().any(|message| !message.is_flagged());
        let commands = messages
            .iter()
            .filter(|message| message.is_flagged() != next_flagged)
            .map(|message| {
                MailCommand::set_message_state(
                    message.id(),
                    message.backend_location(),
                    message.is_unread(),
                    message.is_flagged(),
                    message.is_unread(),
                    next_flagged,
                )
            })
            .collect::<Vec<_>>();
        let count = commands.len();
        if count > 0 {
            self.execute_mail_batch(commands, "Markierung ändern")?;
        }
        Ok(())
    }

    fn toggle_batch_important(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        if messages.iter().any(|message| {
            message
                .backend_location()
                .as_ref()
                .is_some_and(|location| !is_graph_backend_location(Some(location)))
        }) {
            return Err(MailOperationError::Unsupported {
                operation:
                    crate::services::mail_operations_service::MailOperationKind::MarkImportant,
                backend: "Wichtigkeitsmarkierung dieses Kontos (Linux-Abnahme ausstehend)",
            });
        }
        let next_important = messages.iter().any(|message| !message.is_important());
        let commands = messages
            .iter()
            .filter(|message| message.is_important() != next_important)
            .map(|message| {
                message
                    .backend_location()
                    .map(|location| {
                        MailCommand::set_important(
                            message.id(),
                            location,
                            message.is_important(),
                            next_important,
                        )
                    })
                    .unwrap_or_else(|| {
                        MailCommand::set_important_local(
                            message.id(),
                            message.is_important(),
                            next_important,
                        )
                    })
            })
            .collect::<Vec<_>>();
        let count = commands.len();
        if count > 0 {
            self.execute_mail_batch(commands, "Wichtigkeit ändern")?;
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
        self.execute_mail_batch(commands, "Nachrichten archivieren")?;
        Ok(())
    }

    fn mark_batch_junk(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self.selected_batch_messages();
        let commands = messages
            .iter()
            .map(|message| match message.backend_location() {
                Some(location) => {
                    MailCommand::set_junk(message.id(), location, message_is_junk(message), true)
                }
                None => MailCommand::set_junk_local(message.id(), message.folder_id()),
            })
            .collect::<Vec<_>>();
        self.execute_mail_batch(commands, "Als Spam markieren")?;
        Ok(())
    }

    fn mark_batch_not_junk(&self) -> Result<(), MailOperationError> {
        self.clear_operation_error();
        let messages = self
            .selected_batch_messages()
            .into_iter()
            .filter(message_is_junk)
            .collect::<Vec<_>>();
        let commands = messages
            .iter()
            .map(|message| {
                message
                    .backend_location()
                    .map(|location| MailCommand::set_junk(message.id(), location, true, false))
                    .unwrap_or_else(|| MailCommand::clear_junk_local(message.id()))
            })
            .collect::<Vec<_>>();
        self.execute_mail_batch(commands, "Spam-Markierung entfernen")?;
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
        self.execute_mail_batch(commands, "Nachrichten löschen")?;
        Ok(())
    }

    fn folder_destinations(&self) -> Vec<MoveDestination> {
        let messages = self.selected_batch_messages();
        self.folder_destinations_for_messages(&messages)
    }

    fn folder_destinations_for_messages(
        &self,
        messages: &[MailMessageObject],
    ) -> Vec<MoveDestination> {
        let mut destinations = Vec::new();
        let single_local_message = messages.len() == 1 && messages[0].backend_location().is_none();
        let single_remote_source = (messages.len() == 1).then(|| {
            messages[0]
                .backend_location()
                .map(|location| location.source_uid)
        });
        if let Some(folder_index) = self.imp().dynamic_folder_index.borrow().as_ref().cloned() {
            for folder in folder_index.borrow().values() {
                let (Some(source_uid), Some(folder_name)) =
                    (folder.source_uid(), folder.folder_name())
                else {
                    continue;
                };
                if single_local_message
                    || single_remote_source
                        .as_ref()
                        .is_some_and(|source| source.as_deref() != Some(source_uid.as_str()))
                {
                    continue;
                }
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

        let has_local_messages = messages
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
            for folder_id in self
                .imp()
                .local_folder_ids
                .borrow()
                .iter()
                .filter(|folder_id| folder_id.starts_with("custom:"))
            {
                destinations.push(MoveDestination {
                    source_uid: "local".to_string(),
                    folder_name: folder_id.clone(),
                    label: format!("Lokal · {}", Self::local_folder_label(folder_id)),
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

    fn copy_folder_destinations(&self) -> Vec<MoveDestination> {
        let messages = self.selected_batch_messages();
        self.copy_folder_destinations_for_messages(&messages)
    }

    fn graph_copy_source_for_messages(&self, messages: &[MailMessageObject]) -> Option<String> {
        let mut source_uid = None;
        for message in messages {
            let location = message.backend_location()?;
            if !is_graph_backend_location(Some(&location)) {
                return None;
            }
            match source_uid.as_deref() {
                Some(existing) if existing != location.source_uid => return None,
                None => source_uid = Some(location.source_uid),
                _ => {}
            }
        }
        source_uid
    }

    fn copy_folder_destinations_for_messages(
        &self,
        messages: &[MailMessageObject],
    ) -> Vec<MoveDestination> {
        let graph_source = self.graph_copy_source_for_messages(messages);
        self.folder_destinations_for_messages(messages)
            .into_iter()
            .filter(|destination| {
                destination.source_uid == "local"
                    || graph_source.as_deref() == Some(destination.source_uid.as_str())
            })
            .collect()
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
                    backend: "gemischte lokale/Konto-Auswahl",
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
                    backend: "kein gemeinsames Konto",
                },
            );
            return;
        }

        let result = self.execute_mail_batch(commands, "Nachrichten verschieben");
        match result {
            Ok(_) => {}
            Err(error) => {
                self.report_operation_error("Verschieben nicht möglich", &error);
            }
        }
    }

    fn copy_batch_to(&self, destination_source: &str, destination_folder: &str) {
        let messages = self.selected_batch_messages();
        let has_local = messages
            .iter()
            .any(|message| message.backend_location().is_none());
        let has_remote = messages
            .iter()
            .any(|message| message.backend_location().is_some());
        if has_local && has_remote {
            self.report_operation_error(
                "Kopieren nicht möglich",
                &MailOperationError::Unsupported {
                    operation: crate::services::mail_operations_service::MailOperationKind::Copy,
                    backend: "gemischte lokale/Remote-Auswahl",
                },
            );
            return;
        }

        let commands = if destination_source == "local" {
            if has_remote {
                self.report_operation_error(
                    "Kopieren nicht möglich",
                    &MailOperationError::Unsupported {
                        operation:
                            crate::services::mail_operations_service::MailOperationKind::Copy,
                        backend: "Online-Nachricht in den lokalen Import",
                    },
                );
                return;
            }
            if messages
                .iter()
                .any(|message| message.id().contains("::copy::"))
            {
                self.report_operation_error(
                    "Kopieren nicht möglich",
                    &MailOperationError::Unsupported {
                        operation:
                            crate::services::mail_operations_service::MailOperationKind::Copy,
                        backend: "lokale Kopie einer Kopie",
                    },
                );
                return;
            }
            messages
                .iter()
                .filter(|message| !message.folder_id().eq_ignore_ascii_case(destination_folder))
                .map(|message| {
                    MailCommand::copy_local(message.id(), destination_folder.to_string())
                })
                .collect::<Vec<_>>()
        } else {
            let Some(source_uid) = self.graph_copy_source_for_messages(&messages) else {
                self.report_operation_error(
                    "Kopieren nicht möglich",
                    &MailOperationError::Unsupported {
                        operation:
                            crate::services::mail_operations_service::MailOperationKind::Copy,
                        backend: if has_remote {
                            "Kopieren für dieses Konto (Linux-Abnahme ausstehend)"
                        } else {
                            "lokaler Import in ein Graph-Konto"
                        },
                    },
                );
                return;
            };
            if source_uid != destination_source {
                self.report_operation_error(
                    "Kopieren nicht möglich",
                    &MailOperationError::Unsupported {
                        operation:
                            crate::services::mail_operations_service::MailOperationKind::Copy,
                        backend: "anderes Online-Konto",
                    },
                );
                return;
            }
            messages
                .iter()
                .filter_map(|message| {
                    message.backend_location().map(|location| {
                        MailCommand::copy_remote(
                            message.id(),
                            location,
                            destination_folder.to_string(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        if commands.is_empty() {
            self.clear_batch_selection();
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label("Keine neue Kopie erforderlich");
            }
            return;
        }

        let result = self.execute_mail_batch(commands, "Nachrichten kopieren");
        match result {
            Ok(_) => {}
            Err(error) => {
                self.report_operation_error("Kopieren nicht möglich", &error);
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
                .spacing(4)
                .margin_start(4)
                .margin_end(4)
                .margin_top(0)
                .margin_bottom(0)
                .css_classes(vec!["mail-sidebar-row"])
                .build();

            let title = gtk::Label::builder()
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            row.append(&title);
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

            if let Some(title) = row.last_child().and_downcast::<gtk::Label>() {
                let display_name = Self::localize_source_name(&source.display_name());
                title.set_label(&display_name);
                title.set_tooltip_text(Some(&format!(
                    "{} · {}",
                    display_name,
                    if source.enabled() {
                        "aktiv"
                    } else {
                        "Konto nicht verfügbar"
                    }
                )));
            }
            row.set_sensitive(source.enabled());
        });

        let source_view = gtk::ListView::builder()
            .model(&source_selection)
            .factory(&source_factory)
            .css_classes(vec!["navigation-sidebar"])
            .visible(false)
            .build();

        let page_for_source = self.downgrade();
        source_selection.connect_selected_item_notify(move |selection| {
            if let Some(source) = selection.selected_item().and_downcast::<EdsSourceObject>() {
                if !source.enabled() {
                    tracing::debug!(
                        uid = %source.uid(),
                        display_name = %source.display_name(),
                        "Ignoring selection of a source without an active backend"
                    );
                    return;
                }
                tracing::debug!(
                    uid = %source.uid(),
                    display_name = %source.display_name(),
                    "EDS mail source selected"
                );
                if let Some(page) = page_for_source.upgrade() {
                    let initial_folder = FolderSelection::configured_initial();
                    if let Some(folder_selection) = page.imp().folder_selection.borrow().as_ref() {
                        folder_selection.replace(initial_folder.clone());
                    }
                    page.select_folder_row(&initial_folder);
                    if let Some(filter) = page.imp().message_filter.borrow().as_ref() {
                        filter.changed(gtk::FilterChange::Different);
                    }
                    page.start_mail_load(
                        MailLoadRequest {
                            source_uid: Some(source.uid()),
                            folder_name: None,
                            offset: 0,
                            max_messages_per_folder: configured_mail_page_limit(),
                            graph_cursors: HashMap::new(),
                        },
                        true,
                    );
                }
            }
        });
        self.imp()
            .source_selection
            .replace(Some(source_selection.clone()));

        let folder_list = gtk::ListBox::new();
        folder_list.set_selection_mode(gtk::SelectionMode::Single);
        folder_list.add_css_class("navigation-sidebar");
        folder_list.set_margin_start(2);

        let folders = [
            ("all", "mail-unread-symbolic", "Alle Nachrichten", ""),
            ("inbox", "folder-symbolic", "Posteingang", ""),
            ("sent", "mail-send-symbolic", "Gesendet", ""),
            ("drafts", "document-edit-symbolic", "Entwürfe", ""),
            ("trash", "user-trash-symbolic", "Papierkorb", ""),
            ("archive", "folder-symbolic", "Archiv", ""),
            ("spam", "mail-mark-junk-symbolic", "Spam", ""),
        ];

        for (id, icon, label, badge) in folders {
            let row = Self::create_folder_row(id, icon, label, badge);
            folder_list.append(&row);
        }

        let local_folder_click = gtk::GestureClick::builder().button(3).build();
        let list_weak = folder_list.downgrade();
        local_folder_click.connect_pressed(move |_, _, x, y| {
            let Some(list) = list_weak.upgrade() else {
                return;
            };
            let folder_menu = gio::Menu::new();
            let new_section = gio::Menu::new();
            new_section.append(Some("Neuer Ordner..."), Some("win.new-folder"));
            new_section.append(Some("Ordner umbenennen..."), Some("win.rename-folder"));
            new_section.append(Some("Ordner löschen..."), Some("win.delete-folder"));
            folder_menu.append_section(None, &new_section);

            let action_section = gio::Menu::new();
            action_section.append(Some("Alle als gelesen markieren"), Some("win.mark-all-read"));
            action_section.append(Some("Nächster ungelesener Ordner"), Some("win.next-unread-folder"));
            action_section.append(Some("Ordner aktualisieren"), Some("win.refresh-mail"));
            action_section.append(Some("Papierkorb leeren"), Some("win.empty-trash"));
            action_section.append(Some("Spam-Ordner leeren"), Some("win.empty-junk"));
            action_section.append(Some("Endgültig löschen (Expunge)"), Some("win.expunge"));
            folder_menu.append_section(None, &action_section);

            let props_section = gio::Menu::new();
            props_section.append(Some("Abonnements verwalten..."), Some("win.folder-subscriptions"));
            props_section.append(Some("Eigenschaften..."), Some("win.folder-properties"));
            folder_menu.append_section(None, &props_section);

            let popover = gtk::PopoverMenu::builder()
                .menu_model(&folder_menu)
                .has_arrow(false)
                .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
                .build();
            popover.set_parent(&list);
            let popover_weak = popover.downgrade();
            popover.connect_closed(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.unparent();
                }
            });
            popover.popup();
        });
        folder_list.add_controller(local_folder_click);

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
            expander.set_indent_for_icon(false);
            expander.set_indent_for_depth(false);
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(4)
                .margin_start(4)
                .margin_end(4)
                .margin_top(0)
                .margin_bottom(0)
                .css_classes(vec!["mail-sidebar-row"])
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
            row.set_margin_start(compact_folder_indent(tree_row.depth() as usize));
            if let Some(icon) = row.first_child().and_downcast::<gtk::Image>() {
                let is_account_root = folder
                    .folder_name()
                    .is_some_and(|folder_name| folder_name.trim().is_empty());
                icon.set_visible(!is_account_root);
                if !is_account_root {
                    set_image_icon_with_fallback(&icon, &folder.icon_name(), "folder-symbolic");
                } else {
                    icon.set_icon_name(None);
                }
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
                let count = folder.unread_count();
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
        dynamic_folder_view.set_margin_start(2);

        let folder_click = gtk::GestureClick::builder().button(3).build();
        let view_weak = dynamic_folder_view.downgrade();
        folder_click.connect_pressed(move |_, _, x, y| {
            let Some(view) = view_weak.upgrade() else {
                return;
            };
            let folder_menu = gio::Menu::new();
            let new_section = gio::Menu::new();
            new_section.append(Some("Neuer Ordner..."), Some("win.new-folder"));
            new_section.append(Some("Ordner umbenennen..."), Some("win.rename-folder"));
            new_section.append(Some("Ordner löschen..."), Some("win.delete-folder"));
            folder_menu.append_section(None, &new_section);

            let action_section = gio::Menu::new();
            action_section.append(Some("Alle als gelesen markieren"), Some("win.mark-all-read"));
            action_section.append(Some("Nächster ungelesener Ordner"), Some("win.next-unread-folder"));
            action_section.append(Some("Ordner aktualisieren"), Some("win.refresh-mail"));
            action_section.append(Some("Papierkorb leeren"), Some("win.empty-trash"));
            action_section.append(Some("Spam-Ordner leeren"), Some("win.empty-junk"));
            action_section.append(Some("Endgültig löschen (Expunge)"), Some("win.expunge"));
            folder_menu.append_section(None, &action_section);

            let props_section = gio::Menu::new();
            props_section.append(Some("Abonnements verwalten..."), Some("win.folder-subscriptions"));
            props_section.append(Some("Eigenschaften..."), Some("win.folder-properties"));
            folder_menu.append_section(None, &props_section);

            let popover = gtk::PopoverMenu::builder()
                .menu_model(&folder_menu)
                .has_arrow(false)
                .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
                .build();
            popover.set_parent(&view);
            let popover_weak = popover.downgrade();
            popover.connect_closed(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.unparent();
                }
            });
            popover.popup();
        });
        dynamic_folder_view.add_controller(folder_click);

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
            let Some(source_uid) = folder.source_uid() else {
                return;
            };
            let folder_name = folder
                .folder_name()
                .filter(|folder_name| !folder_name.trim().is_empty());
            let folder_selection = FolderSelection {
                folder_id: folder.id(),
                source_uid: Some(source_uid.clone()),
                folder_name: folder_name.clone(),
            };
            if let Some(current) = page.imp().folder_selection.borrow().as_ref() {
                current.replace(folder_selection);
            }
            if let Some(folder_list) = page.imp().folder_list.borrow().as_ref() {
                folder_list.unselect_all();
            }
            if let Some(search_list) = page.imp().search_list.borrow().as_ref() {
                search_list.unselect_all();
            }
            if let Some(search_entry) = page.imp().search_entry.borrow().as_ref() {
                search_entry.set_text("");
            }
            if let Some(filter_state) = page.imp().message_filter_state.borrow().as_ref() {
                filter_state.set(MessageFilter::All);
            }
            if let Some(query) = page.imp().search_query.borrow().as_ref() {
                query.replace(MessageSearchQuery::default());
            }
            if let Some(quick_filter) = page.imp().quick_filter.borrow().as_ref() {
                quick_filter.set_selected(0);
            }
            if let Some(filter) = page.imp().message_filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
            if let Some(filter) = page.imp().thread_filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
            page.start_mail_load(
                MailLoadRequest {
                    source_uid: Some(source_uid),
                    folder_name,
                    offset: 0,
                    max_messages_per_folder: configured_mail_page_limit(),
                    graph_cursors: HashMap::new(),
                },
                true,
            );
        });

        let search_header = gtk::Label::builder()
            .label("Suchordner")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_top(3)
            .margin_bottom(2)
            .build();

        let search_list = gtk::ListBox::new();
        search_list.set_selection_mode(gtk::SelectionMode::Single);
        search_list.add_css_class("navigation-sidebar");
        search_list.set_margin_start(2);
        Self::populate_search_folders(&search_list);

        let local_folder_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(4)
            .margin_end(4)
            .margin_top(3)
            .margin_bottom(1)
            .build();
        let local_folder_header = gtk::Label::builder()
            .label("Auf diesem Rechner")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        let new_local_folder_button = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text("Lokalen Ordner erstellen")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        local_folder_toolbar.append(&local_folder_header);
        local_folder_toolbar.append(&new_local_folder_button);

        let folder_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .visible(false)
            .build();
        let page_for_new_local_folder = self.downgrade();
        new_local_folder_button.connect_clicked(move |_| {
            if let Some(page) = page_for_new_local_folder.upgrade() {
                page.present_local_folder_dialog(None);
            }
        });
        let local_folder_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .visible(false)
            .build();
        local_folder_section.append(&local_folder_toolbar);
        local_folder_section.append(&folder_list);
        self.imp()
            .local_folder_section
            .replace(Some(local_folder_section.clone()));
        folder_section.append(&local_folder_section);
        let dynamic_header = gtk::Label::builder()
            .label("Postfächer")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_top(3)
            .margin_bottom(2)
            .build();
        folder_section.append(&dynamic_header);
        folder_section.append(&dynamic_folder_view);
        folder_section.append(&search_header);
        folder_section.append(&search_list);

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .css_classes(vec!["mail-sidebar"])
            .build();

        let source_state = gtk::Label::builder()
            .label("Konten werden geladen …")
            .css_classes(vec!["dim-label"])
            .visible(false)
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&source_view);
        sidebar_box.append(&folder_section);

        let source_status_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["mail-pane-status"])
            .build();
        source_status_area.append(&source_state);
        source_status_area.set_visible(false);
        let source_status_area_for_visibility = source_status_area.clone();
        source_state.connect_visible_notify(move |state| {
            let statusbar_enabled = AppSettings::try_new()
                .map(|settings| settings.mail_show_statusbar())
                .unwrap_or(true);
            source_status_area_for_visibility.set_visible(statusbar_enabled && state.is_visible());
        });
        self.imp()
            .source_status_area
            .replace(Some(source_status_area.clone()));

        let drawer_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(vec!["sidebar-drawer-header"])
            .build();
        let drawer_icon = gtk::Image::from_icon_name("mail-inbox-symbolic");
        let drawer_title = gtk::Label::builder()
            .label("Posteingang")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);
        self.imp().drawer_header_title.replace(Some(drawer_title));
        self.imp().drawer_header_icon.replace(Some(drawer_icon));

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&sidebar_box)
            .width_request(200)
            .build();
        let sidebar_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();
        sidebar_scroll.set_vexpand(true);
        sidebar_content.append(&drawer_header);
        sidebar_content.append(&sidebar_scroll);
        sidebar_content.append(&source_status_area);

        sidebar_content.append(&crate::views::build_sidebar_module_switcher(
            "mail",
            &sidebar_content,
        ));

        let page = adw::NavigationPage::builder()
            .title("Ordner")
            .child(&sidebar_content)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
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
        for search in SearchFolderService::default().load_saved() {
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
                    move || match SearchFolderService::default().delete_saved(&search_id_for_action)
                    {
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
            .spacing(4)
            .margin_start(4)
            .margin_end(4)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["mail-sidebar-row"])
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
            let (unread, _total) = if folder_id == "all" {
                counts
                    .values()
                    .fold((0_u32, 0_u32), |(unread, total), value| {
                        (
                            unread.saturating_add(value.0),
                            total.saturating_add(value.1),
                        )
                    })
            } else if folder_id.starts_with("custom:") {
                counts
                    .iter()
                    .filter(|(candidate, _)| {
                        let candidate = candidate.as_str();
                        candidate == folder_id
                            || candidate
                                .strip_prefix(&folder_id)
                                .is_some_and(|rest| rest.starts_with('/'))
                    })
                    .fold((0_u32, 0_u32), |(unread, total), (_, value)| {
                        (
                            unread.saturating_add(value.0),
                            total.saturating_add(value.1),
                        )
                    })
            } else {
                counts.get(key).copied().unwrap_or((0, 0))
            };
            let Some(badge) = (unsafe {
                row.data::<gtk::Label>("folder-badge")
                    .map(|pointer| pointer.as_ref())
            }) else {
                continue;
            };
            badge.set_label(&unread.to_string());
            badge.set_visible(unread > 0);
        }
    }

    fn rebuild_local_folder_rows(
        &self,
        folder_list: &gtk::ListBox,
        counts: &HashMap<String, (u32, u32)>,
        definitions: &HashSet<String>,
    ) {
        let was_updating = self.imp().updating_folder_rows.replace(true);
        let mut stale_rows = Vec::new();
        for index in 0..folder_list.observe_children().n_items() {
            let Some(row) = folder_list
                .row_at_index(index as i32)
                .and_downcast::<gtk::ListBoxRow>()
            else {
                continue;
            };
            let is_local_dynamic = unsafe {
                row.data::<bool>("folder-local-dynamic")
                    .map(|value| *value.as_ref())
                    .unwrap_or(false)
            };
            if is_local_dynamic {
                stale_rows.push(row);
            }
        }
        for row in stale_rows {
            folder_list.remove(&row);
        }

        let mut local_folders = HashSet::new();
        for folder_id in counts.keys().chain(definitions.iter()) {
            if folder_id.starts_with("custom:") {
                Self::add_local_folder_tree_ids(&mut local_folders, folder_id);
            }
        }
        let mut ordered_folders = local_folders.iter().cloned().collect::<Vec<_>>();
        ordered_folders.sort_by(|left, right| {
            Self::local_folder_label(left)
                .to_ascii_lowercase()
                .cmp(&Self::local_folder_label(right).to_ascii_lowercase())
                .then_with(|| left.cmp(right))
        });

        let collapsed = self.imp().local_folder_collapsed.borrow().clone();
        for folder_id in ordered_folders {
            if Self::local_folder_has_collapsed_parent(&folder_id, &collapsed) {
                continue;
            }
            let has_children = local_folders.iter().any(|candidate| {
                candidate != &folder_id
                    && candidate
                        .strip_prefix(&format!("{folder_id}/"))
                        .is_some_and(|rest| !rest.is_empty())
            });
            let row = Self::create_folder_row_with_backend(
                &folder_id,
                "folder-symbolic",
                &Self::local_folder_display_label(&folder_id),
                "",
                None,
                None,
                true,
            );
            if let Some(container) = row.child().and_downcast::<gtk::Box>() {
                container
                    .set_margin_start(compact_folder_indent(Self::local_folder_depth(&folder_id)));
            }
            unsafe {
                row.set_data("folder-local-dynamic", true);
            }
            self.add_local_folder_expander(&row, &folder_id, has_children);
            self.add_local_folder_actions(&row, &folder_id);
            folder_list.append(&row);
        }
        Self::update_folder_counts(folder_list, counts);
        self.imp().updating_folder_rows.set(was_updating);
    }

    fn add_local_folder_tree_ids(folder_ids: &mut HashSet<String>, folder_id: &str) {
        let Some(path) = folder_id.strip_prefix("custom:") else {
            return;
        };
        let mut prefix = String::new();
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            folder_ids.insert(format!("custom:{prefix}"));
        }
    }

    fn local_folder_depth(folder_id: &str) -> usize {
        folder_id
            .strip_prefix("custom:")
            .map(|path| {
                path.split('/')
                    .filter(|segment| !segment.is_empty())
                    .count()
            })
            .unwrap_or(1)
            .saturating_sub(1)
    }

    fn local_folder_display_label(folder_id: &str) -> String {
        folder_id
            .strip_prefix("custom:")
            .unwrap_or(folder_id)
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(folder_id)
            .to_string()
    }

    fn local_folder_has_collapsed_parent(folder_id: &str, collapsed: &HashSet<String>) -> bool {
        let Some(path) = folder_id.strip_prefix("custom:") else {
            return false;
        };
        let segments = path.split('/').filter(|segment| !segment.is_empty());
        let mut prefix = String::new();
        for (index, segment) in segments.enumerate() {
            if index + 1 == path.split('/').filter(|value| !value.is_empty()).count() {
                break;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            if collapsed.contains(&format!("custom:{prefix}")) {
                return true;
            }
        }
        false
    }

    fn add_local_folder_expander(
        &self,
        row: &gtk::ListBoxRow,
        folder_id: &str,
        has_children: bool,
    ) {
        if !has_children {
            return;
        }
        let collapsed = self
            .imp()
            .local_folder_collapsed
            .borrow()
            .contains(folder_id);
        let button = gtk::Button::builder()
            .icon_name(if collapsed {
                "pan-end-symbolic"
            } else {
                "pan-down-symbolic"
            })
            .tooltip_text(if collapsed {
                "Unterordner einblenden"
            } else {
                "Unterordner ausblenden"
            })
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        if let Some(container) = row.child().and_downcast::<gtk::Box>() {
            container.prepend(&button);
        }
        let folder_id = folder_id.to_string();
        let page = self.downgrade();
        button.connect_clicked(move |_| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let mut collapsed = page.imp().local_folder_collapsed.borrow_mut();
            if !collapsed.insert(folder_id.clone()) {
                collapsed.remove(&folder_id);
            }
            drop(collapsed);
            page.refresh_mail();
        });
    }

    fn add_local_folder_actions(&self, row: &gtk::ListBoxRow, folder_id: &str) {
        let Some(container) = row.child().and_downcast::<gtk::Box>() else {
            return;
        };

        let rename_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Lokalen Ordner umbenennen")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Leeren lokalen Ordner löschen")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        container.append(&rename_button);
        container.append(&delete_button);

        let folder_id_for_rename = folder_id.to_string();
        let page_for_rename = self.downgrade();
        rename_button.connect_clicked(move |_| {
            if let Some(page) = page_for_rename.upgrade() {
                page.present_local_folder_dialog(Some(&folder_id_for_rename));
            }
        });

        let folder_id_for_delete = folder_id.to_string();
        let page_for_delete = self.downgrade();
        delete_button.connect_clicked(move |button| {
            let folder_id = folder_id_for_delete.clone();
            let page = page_for_delete.clone();
            crate::dialogs::run_destructive_action(
                button,
                "Lokalen Ordner löschen?",
                "Nur leere app-eigene Ordner können gelöscht werden. Importierte Maildateien bleiben unverändert.",
                "Löschen",
                move || {
                    let result = crate::services::MailStoreService::new()
                        .delete_local_folder(&folder_id);
                    if let Some(page) = page.upgrade() {
                        match result {
                            Ok(()) => page.refresh_mail(),
                            Err(error) => page.show_toast(&format!(
                                "Lokaler Ordner konnte nicht gelöscht werden: {error}"
                            )),
                        }
                    }
                },
            );
        });
    }
    fn localize_folder_name(name: &str) -> String {
        let clean = name.trim();
        match clean.to_ascii_lowercase().as_str() {
            "inbox" | "posteingang" => "Posteingang".to_string(),
            "drafts" | "entwürfe" | "entwuerfe" => "Entwürfe".to_string(),
            "sent" | "sent items" | "gesendet" => "Gesendet".to_string(),
            "trash" | "mülleimer" | "papierkorb" | "deleted items" => "Mülleimer".to_string(),
            "junk" | "junk email" | "spam" => "Spam".to_string(),
            "archive" | "archiv" => "Archiv".to_string(),
            "outbox" | "postausgang" => "Postausgang".to_string(),
            _ => clean.to_string(),
        }
    }

    fn localize_source_name(name: &str) -> String {
        let clean = name.trim();
        let normalized = clean.to_ascii_lowercase();
        if normalized.contains("eds")
            || normalized.contains("camel")
            || normalized.contains("gnome online")
            || normalized == "goa"
        {
            return "Mailkonto".to_string();
        }
        match normalized.as_str() {
            "on this computer" | "local" => "Auf diesem Computer".to_string(),
            "search folders" | "vfolder" => "Suchordner".to_string(),
            "news and blogs" | "rss" => "News & Blogs".to_string(),
            _ => clean.to_string(),
        }
    }

    fn rebuild_camel_folder_rows(
        &self,
        folder_list: &gtk::ListBox,
        folders: &[camel_mail_service::CamelFolderSummary],
    ) {
        let started = Instant::now();
        tracing::debug!(
            folder_count = folders.len(),
            "Rebuilding EDS/Camel folder rows"
        );
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

        let mut source_roots = HashMap::<String, FolderObject>::new();
        let mut source_counts = HashMap::<String, (u32, u32)>::new();
        for folder in folders {
            let source_root_id = Self::dynamic_folder_key(&folder.source_uid, "");
            source_roots
                .entry(folder.source_uid.clone())
                .or_insert_with(|| {
                    let source_name = Self::localize_source_name(&folder.source_display_name);
                    FolderObject::new_backend(
                        &source_root_id,
                        if source_name.is_empty() {
                            "Mailkonto"
                        } else {
                            &source_name
                        },
                        "",
                        0,
                        0,
                        &folder.source_uid,
                        "",
                        None,
                        true,
                    )
                });
            let counts = source_counts.entry(folder.source_uid.clone()).or_default();
            counts.0 = counts.0.saturating_add(folder.unread_count);
            counts.1 = counts.1.saturating_add(folder.total_count);
        }

        let mut roots = source_roots.values().cloned().collect::<Vec<_>>();
        roots.sort_by_key(FolderObject::name);
        for root in &roots {
            if let Some((unread, total)) = source_counts.get(&root.source_uid().unwrap_or_default())
            {
                root.set_counts(*unread, *total);
            }
        }
        let mut child_stores = HashMap::<String, gio::ListStore>::new();
        let mut folder_index = HashMap::<String, FolderObject>::new();
        for root in &roots {
            folder_index.insert(root.id(), root.clone());
        }
        for node in nodes {
            let id = Self::dynamic_folder_key(&node.source_uid, &node.folder_name);
            let parent_id = node
                .parent_name
                .as_deref()
                .map(|name| Self::dynamic_folder_key(&node.source_uid, name))
                .unwrap_or_else(|| Self::dynamic_folder_key(&node.source_uid, ""));
            let localized_display = Self::localize_folder_name(&node.display_name);
            let label = localized_display;
            let folder = FolderObject::new_backend(
                &id,
                &label,
                &node.icon_name,
                node.unread_count,
                node.total_count,
                &node.source_uid,
                &node.folder_name,
                Some(parent_id.as_str()),
                node.selectable,
            );
            child_stores
                .entry(parent_id)
                .or_insert_with(|| gio::ListStore::new::<FolderObject>())
                .append(&folder);
            folder_index.insert(id, folder);
        }

        root_store.remove_all();
        children.replace(child_stores);
        index.replace(folder_index);
        for root in roots {
            root_store.append(&root);
        }
        self.imp().updating_folder_rows.set(was_updating);
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            folder_count = folders.len(),
            "EDS/Camel folder rows rebuilt"
        );
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
        let clean = folder_id.trim().to_ascii_lowercase();
        if clean.contains("inbox") || clean.contains("posteingang") {
            "folder-symbolic"
        } else if clean.contains("sent") || clean.contains("gesendet") {
            "mail-send-symbolic"
        } else if clean.contains("draft") || clean.contains("entwurf") || clean.contains("entwürfe")
        {
            "document-edit-symbolic"
        } else if clean.contains("trash")
            || clean.contains("mülleimer")
            || clean.contains("papierkorb")
            || clean.contains("deleted")
        {
            "user-trash-symbolic"
        } else if clean.contains("junk") || clean.contains("spam") {
            "mail-mark-junk-symbolic"
        } else if clean.contains("archive") || clean.contains("archiv") {
            "folder-symbolic"
        } else if clean.contains("outbox") || clean.contains("postausgang") {
            "mail-send-symbolic"
        } else {
            "folder-symbolic"
        }
    }

    fn local_folder_label(folder_id: &str) -> String {
        folder_id
            .strip_prefix("custom:")
            .unwrap_or(folder_id)
            .replace('/', " / ")
    }

    #[cfg(test)]
    fn is_builtin_folder_id(folder_id: &str) -> bool {
        matches!(
            folder_id,
            "all" | "inbox" | "sent" | "drafts" | "trash" | "archive" | "junk" | "spam"
        )
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
                if let Some(source_uid) = selection.source_uid.as_deref() {
                    let mut ancestors = vec![String::new()];
                    if let Some(folder_name) = selection.folder_name.as_deref() {
                        let mut path = String::new();
                        let mut components = folder_name.split('/').peekable();
                        while let Some(component) = components.next() {
                            if components.peek().is_none() {
                                break;
                            }
                            if !path.is_empty() {
                                path.push('/');
                            }
                            path.push_str(component);
                            ancestors.push(path.clone());
                        }
                    }
                    for ancestor in ancestors {
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
                            let folder_name = folder.folder_name().unwrap_or_default();
                            if folder.source_uid().as_deref() == Some(source_uid)
                                && folder_name == ancestor
                            {
                                row.set_expanded(true);
                                break;
                            }
                        }
                    }
                }
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
                        folder_name: folder
                            .folder_name()
                            .filter(|folder_name| !folder_name.trim().is_empty()),
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

    fn build_classic_mail_toolbar(&self) -> gtk::Box {
        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(2)
            .css_classes(vec!["toolbar"])
            .build();

        let button = |icon_name: &str, tooltip: &str, action_name: &str| {
            let button = gtk::Button::builder()
                .icon_name(icon_name)
                .tooltip_text(tooltip)
                .action_name(action_name)
                .css_classes(vec!["flat"])
                .build();
            let fallback = match icon_name {
                "mail-message-new-symbolic" => "document-new-symbolic",
                "mail-reply-sender-symbolic" | "mail-reply-all-symbolic" => "go-previous-symbolic",
                "mail-forward-symbolic" => "go-next-symbolic",
                "document-save-symbolic" => "document-save-symbolic",
                "document-print-symbolic" => "document-save-symbolic",
                "document-print-preview-symbolic" => "document-print-symbolic",
                "mail-read-symbolic" | "mail-unread-symbolic" => "mail-message-new-symbolic",
                "starred-symbolic" => "bookmark-new-symbolic",
                "mail-mark-important-symbolic" => "dialog-warning-symbolic",
                "mail-mark-junk-symbolic" => "dialog-warning-symbolic",
                "folder-symbolic" => "folder-symbolic",
                "edit-copy-symbolic" => "edit-copy-symbolic",
                _ => "image-missing-symbolic",
            };
            set_button_icon_with_fallback(&button, icon_name, fallback);
            set_accessible_label(&button, tooltip);
            button
        };

        for (icon_name, tooltip, action_name) in [
            (
                "mail-message-new-symbolic",
                "Neue E-Mail (Ctrl+N)",
                "app.compose",
            ),
            (
                "mail-reply-sender-symbolic",
                "Antworten (Ctrl+R)",
                "win.reply",
            ),
            (
                "mail-reply-all-symbolic",
                "Allen antworten (Ctrl+Shift+R)",
                "win.reply-all",
            ),
            (
                "mail-forward-symbolic",
                "Weiterleiten (Ctrl+L)",
                "win.forward",
            ),
            (
                "document-save-symbolic",
                "Als EML speichern (Ctrl+Shift+S)",
                "win.export",
            ),
            (
                "document-print-symbolic",
                "Drucken (Ctrl+P)",
                "win.mail-print",
            ),
            (
                "document-print-preview-symbolic",
                "Druckvorschau",
                "win.mail-print-preview",
            ),
        ] {
            toolbar.append(&button(icon_name, tooltip, action_name));
        }

        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

        for (icon_name, tooltip, action_name) in [
            (
                "mail-read-symbolic",
                "Als gelesen markieren",
                "win.mark-read",
            ),
            (
                "mail-unread-symbolic",
                "Als ungelesen markieren",
                "win.mark-unread",
            ),
            (
                "starred-symbolic",
                "Markierung umschalten",
                "win.toggle-flag",
            ),
            (
                "mail-mark-important-symbolic",
                "Wichtigkeitsmarkierung umschalten",
                "win.toggle-important",
            ),
            ("folder-symbolic", "Archivieren", "win.archive"),
            (
                "folder-symbolic",
                "In Ordner verschieben (Ctrl+Shift+V)",
                "win.move-message",
            ),
            (
                "edit-copy-symbolic",
                "In Ordner kopieren (Ctrl+Shift+C)",
                "win.copy-message",
            ),
            (
                "mail-mark-junk-symbolic",
                "Als Spam markieren",
                "win.mark-junk",
            ),
            ("user-trash-symbolic", "Löschen", "win.delete"),
        ] {
            toolbar.append(&button(icon_name, tooltip, action_name));
        }

        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&button(
            "view-refresh-symbolic",
            "Mail aktualisieren (F5)",
            "win.refresh-mail",
        ));
        toolbar.append(&button("edit-undo-symbolic", "Rückgängig", "win.undo"));
        toolbar.append(&button("edit-redo-symbolic", "Wiederholen", "win.redo"));
        toolbar
    }

    fn message_context_menu() -> gio::Menu {
        let menu = gio::Menu::new();
        for (label, action) in MESSAGE_CONTEXT_ACTIONS {
            menu.append(Some(label), Some(action));
        }
        menu
    }

    // ── Private: Build Message List with GListStore & Search Filter ───────

    fn mail_header_child(header: &gtk::Box, index: usize) -> Option<gtk::Widget> {
        let mut child = header.first_child();
        for _ in 0..index {
            child = child.and_then(|child| child.next_sibling());
        }
        child
    }

    fn cell_label(cell: &gtk::Widget) -> Option<gtk::Label> {
        if let Some(lbl) = cell.downcast_ref::<gtk::Label>() {
            Some(lbl.clone())
        } else if let Some(bx) = cell.downcast_ref::<gtk::Box>() {
            bx.first_child().and_downcast::<gtk::Label>()
        } else {
            None
        }
    }

    fn refresh_mail_column_header(&self) {
        let Some(header) = self.imp().mail_column_header.borrow().as_ref().cloned() else {
            return;
        };
        let settings = AppSettings::try_new();
        let show_avatar = settings
            .as_ref()
            .map(AppSettings::mail_show_avatar)
            .unwrap_or(false);
        let show_sender = settings
            .as_ref()
            .map(AppSettings::mail_show_sender)
            .unwrap_or(true);
        let show_recipient = settings
            .as_ref()
            .map(AppSettings::mail_show_recipient)
            .unwrap_or(false);
        let show_date = settings
            .as_ref()
            .map(AppSettings::mail_show_date)
            .unwrap_or(true);
        let show_attachment = settings
            .as_ref()
            .map(AppSettings::mail_show_attachment)
            .unwrap_or(true);
        let show_preview = settings
            .as_ref()
            .map(AppSettings::mail_show_preview_text)
            .unwrap_or(false);
        let sender_width = settings
            .as_ref()
            .map(AppSettings::mail_sender_column_width)
            .unwrap_or(180);

        if let Some(avatar) = Self::mail_header_child(&header, 6) {
            avatar.set_visible(show_avatar);
        }
        if let Some(attachment) = Self::mail_header_child(&header, 2) {
            attachment.set_visible(show_attachment);
            if let Some(lbl) = Self::cell_label(&attachment) {
                lbl.set_label(&self.mail_header_sort_label("📎", MailSortField::Attachments));
            }
        }
        if let Some(status) = Self::mail_header_child(&header, 1) {
            if let Some(lbl) = Self::cell_label(&status) {
                lbl.set_label(&self.mail_header_sort_label("●", MailSortField::Unread));
            }
        }
        if let Some(follow_up) = Self::mail_header_child(&header, 3) {
            if let Some(lbl) = Self::cell_label(&follow_up) {
                lbl.set_label(&self.mail_header_sort_label("⚑", MailSortField::FollowUp));
            }
        }
        if let Some(star) = Self::mail_header_child(&header, 4) {
            if let Some(lbl) = Self::cell_label(&star) {
                lbl.set_label(&self.mail_header_sort_label("★", MailSortField::Flagged));
            }
        }
        if let Some(important) = Self::mail_header_child(&header, 5) {
            if let Some(lbl) = Self::cell_label(&important) {
                lbl.set_label(&self.mail_header_sort_label("!", MailSortField::Important));
            }
        }
        if let Some(sender_column) = Self::mail_header_child(&header, 7) {
            sender_column.set_visible(show_sender || show_recipient);
            sender_column.set_width_request(sender_width);
            if let Some(sender_label) = Self::cell_label(&sender_column) {
                let label = match (show_sender, show_recipient) {
                    (true, true) => "Von / An",
                    (true, false) => "Von",
                    (false, true) => "An",
                    (false, false) => "Von",
                };
                sender_label.set_label(&self.mail_header_sort_label(label, MailSortField::Sender));
            }
        }
        if let Some(subject) = Self::mail_header_child(&header, 8) {
            if let Some(lbl) = Self::cell_label(&subject) {
                lbl.set_label(&self.mail_header_sort_label("Betreff", MailSortField::Subject));
            }
        }
        if let Some(preview) = Self::mail_header_child(&header, 9) {
            preview.set_visible(show_preview);
        }
        if let Some(date) = Self::mail_header_child(&header, 10) {
            date.set_visible(show_date);
            if let Some(lbl) = Self::cell_label(&date) {
                lbl.set_label(&self.mail_header_sort_label("Datum", MailSortField::Date));
            }
        }
    }

    fn ensure_size_groups(&self) {
        let imp = self.imp();
        if imp.status_size_group.borrow().is_none() {
            imp.status_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.attachment_size_group.borrow().is_none() {
            imp.attachment_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.follow_up_size_group.borrow().is_none() {
            imp.follow_up_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.star_size_group.borrow().is_none() {
            imp.star_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.important_size_group.borrow().is_none() {
            imp.important_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.sender_size_group.borrow().is_none() {
            imp.sender_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.preview_size_group.borrow().is_none() {
            imp.preview_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
        if imp.date_size_group.borrow().is_none() {
            imp.date_size_group
                .replace(Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)));
        }
    }

    fn build_mail_column_header(&self) -> gtk::Box {
        self.ensure_size_groups();
        let settings = AppSettings::try_new();
        let sender_width = settings
            .as_ref()
            .map(AppSettings::mail_sender_column_width)
            .unwrap_or(180);

        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .margin_start(4)
            .margin_end(4)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["mail-column-header"])
            .build();

        let thread_spacer = gtk::Box::builder()
            .width_request(MAIL_THREAD_COLUMN_WIDTH)
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();

        let status_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
            .halign(gtk::Align::Fill)
            .hexpand(false)
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();
        let status_label = gtk::Label::builder()
            .label("●")
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.5)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        status_column.append(&status_label);

        let avatar_spacer = gtk::Box::builder()
            .width_request(MAIL_AVATAR_COLUMN_WIDTH)
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();

        let sender_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .width_request(sender_width)
            .hexpand(false)
            .halign(gtk::Align::Start)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(vec!["mail-column-header-cell"])
            .build();
        let sender_label = gtk::Label::builder()
            .label("Von")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        sender_column.append(&sender_label);

        let date_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_DATE_COLUMN_WIDTH)
            .halign(gtk::Align::Fill)
            .css_classes(vec!["mail-column-header-cell"])
            .build();
        let date_label = gtk::Label::builder()
            .label("Datum")
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .max_width_chars(16)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        date_column.append(&date_label);

        let attachment_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
            .halign(gtk::Align::Fill)
            .hexpand(false)
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();
        let attachment_label = gtk::Label::builder()
            .label("📎")
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.5)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        attachment_column.append(&attachment_label);

        let star_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
            .halign(gtk::Align::Fill)
            .hexpand(false)
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();
        let star_label = gtk::Label::builder()
            .label("★")
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.5)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        star_column.append(&star_label);

        let follow_up_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
            .halign(gtk::Align::Fill)
            .hexpand(false)
            .tooltip_text("Nach Nachverfolgung sortieren")
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();
        let follow_up_sort_gesture = gtk::GestureClick::new();
        follow_up_sort_gesture.set_button(1);
        let page_for_follow_up_sort = self.downgrade();
        follow_up_sort_gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page_for_follow_up_sort.upgrade() {
                page.set_mail_sort_from_header(MailSortField::FollowUp);
            }
        });
        follow_up_column.add_controller(follow_up_sort_gesture);
        let follow_up_label = gtk::Label::builder()
            .label("⚑")
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.5)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        follow_up_column.append(&follow_up_label);

        let important_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
            .halign(gtk::Align::Fill)
            .hexpand(false)
            .css_classes(vec!["mail-column-header-cell", "icon-cell"])
            .build();
        let important_label = gtk::Label::builder()
            .label("!")
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.5)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        important_column.append(&important_label);

        let subject_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .css_classes(vec!["mail-column-header-cell"])
            .build();
        let subject_label = gtk::Label::builder()
            .label("Betreff")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        subject_column.append(&subject_label);

        let preview_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .width_request(MAIL_PREVIEW_COLUMN_WIDTH)
            .halign(gtk::Align::Start)
            .css_classes(vec!["mail-column-header-cell"])
            .build();
        let preview_label = gtk::Label::builder()
            .label("Vorschau")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .max_width_chars(20)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        preview_column.append(&preview_label);

        if let Some(group) = self.imp().status_size_group.borrow().as_ref() {
            group.add_widget(&status_column);
        }
        if let Some(group) = self.imp().attachment_size_group.borrow().as_ref() {
            group.add_widget(&attachment_column);
        }
        if let Some(group) = self.imp().follow_up_size_group.borrow().as_ref() {
            group.add_widget(&follow_up_column);
        }
        if let Some(group) = self.imp().star_size_group.borrow().as_ref() {
            group.add_widget(&star_column);
        }
        if let Some(group) = self.imp().important_size_group.borrow().as_ref() {
            group.add_widget(&important_column);
        }
        if let Some(group) = self.imp().sender_size_group.borrow().as_ref() {
            group.add_widget(&sender_column);
        }
        if let Some(group) = self.imp().preview_size_group.borrow().as_ref() {
            group.add_widget(&preview_column);
        }
        if let Some(group) = self.imp().date_size_group.borrow().as_ref() {
            group.add_widget(&date_column);
        }

        header.append(&thread_spacer);
        header.append(&status_column);
        header.append(&attachment_column);
        header.append(&follow_up_column);
        header.append(&star_column);
        header.append(&important_column);
        header.append(&avatar_spacer);
        header.append(&sender_column);
        header.append(&subject_column);
        header.append(&preview_column);
        header.append(&date_column);

        // Sorting when clicking inside the column body (away from right border)
        let sender_sort_gesture = gtk::GestureClick::new();
        sender_sort_gesture.set_button(1);
        let page_for_sender_sort = self.downgrade();
        let sender_col_for_sort = sender_column.clone();
        sender_sort_gesture.connect_pressed(move |gesture, _, x, _| {
            let width = sender_col_for_sort.width();
            if x < (width - 8) as f64 {
                if let Some(page) = page_for_sender_sort.upgrade() {
                    page.set_mail_sort_from_header(MailSortField::Sender);
                }
            } else {
                gesture.set_state(gtk::EventSequenceState::Denied);
            }
        });
        sender_column.set_tooltip_text(Some("Nach Absender sortieren"));
        sender_column.add_controller(sender_sort_gesture);

        // Edge cursor motion controller: show col-resize cursor when hovering right border
        let motion = gtk::EventControllerMotion::new();
        let sender_col_for_motion = sender_column.clone();
        motion.connect_motion(move |_, x, _| {
            let width = sender_col_for_motion.width();
            if x >= (width - 8) as f64 {
                sender_col_for_motion
                    .set_cursor(gdk::Cursor::from_name("col-resize", None).as_ref());
            } else {
                sender_col_for_motion.set_cursor(None);
            }
        });
        let sender_col_for_leave = sender_column.clone();
        motion.connect_leave(move |_| {
            sender_col_for_leave.set_cursor(None);
        });
        sender_column.add_controller(motion);

        // Edge drag resizing: dragging from right border resizes the column smoothly
        let is_resizing = Rc::new(Cell::new(false));
        let width_at_start = Rc::new(Cell::new(sender_width));
        let current_width = Rc::new(Cell::new(sender_width));
        let drag_gesture = gtk::GestureDrag::new();
        drag_gesture.set_button(1);

        let sender_col_for_begin = sender_column.clone();
        let width_at_start_for_begin = Rc::clone(&width_at_start);
        let current_width_for_begin = Rc::clone(&current_width);
        let is_resizing_for_begin = Rc::clone(&is_resizing);
        drag_gesture.connect_drag_begin(move |gesture, start_x, _| {
            let width = sender_col_for_begin.width_request().clamp(80, 450);
            if start_x >= (sender_col_for_begin.width() - 8) as f64 {
                is_resizing_for_begin.set(true);
                width_at_start_for_begin.set(width);
                current_width_for_begin.set(width);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else {
                is_resizing_for_begin.set(false);
            }
        });

        let sender_col_for_update = sender_column.clone();
        let width_at_start_for_update = Rc::clone(&width_at_start);
        let current_width_for_update = Rc::clone(&current_width);
        let is_resizing_for_update = Rc::clone(&is_resizing);
        drag_gesture.connect_drag_update(move |gesture, offset_x, _| {
            if is_resizing_for_update.get() {
                let width = (width_at_start_for_update.get() as f64 + offset_x)
                    .round()
                    .clamp(80.0, 450.0) as i32;
                current_width_for_update.set(width);
                sender_col_for_update.set_width_request(width);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });

        let settings_for_resize = settings.clone();
        let current_width_for_end = Rc::clone(&current_width);
        let is_resizing_for_end = Rc::clone(&is_resizing);
        drag_gesture.connect_drag_end(move |_, _, _| {
            if is_resizing_for_end.get() {
                is_resizing_for_end.set(false);
                let width = current_width_for_end.get().clamp(80, 450);
                if let Some(settings) = settings_for_resize.as_ref() {
                    settings.set_mail_sender_column_width(width);
                }
            }
        });
        sender_column.add_controller(drag_gesture);

        let status_sort_gesture = gtk::GestureClick::new();
        status_sort_gesture.set_button(1);
        let page_for_status_sort = self.downgrade();
        status_sort_gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page_for_status_sort.upgrade() {
                page.set_mail_sort_from_header(MailSortField::Unread);
            }
        });
        status_column.set_tooltip_text(Some("Nach Lesestatus sortieren"));
        status_column.add_controller(status_sort_gesture);

        let date_motion = gtk::EventControllerMotion::new();
        let date_col_for_motion = date_column.clone();
        date_motion.connect_motion(move |_, x, _| {
            if x <= 8.0 {
                date_col_for_motion.set_cursor(gdk::Cursor::from_name("col-resize", None).as_ref());
            } else {
                date_col_for_motion.set_cursor(None);
            }
        });
        let date_col_for_leave = date_column.clone();
        date_motion.connect_leave(move |_| {
            date_col_for_leave.set_cursor(None);
        });
        date_column.add_controller(date_motion);

        let is_date_resizing = Rc::new(Cell::new(false));
        let date_width_at_start = Rc::new(Cell::new(MAIL_DATE_COLUMN_WIDTH));
        let date_current_width = Rc::new(Cell::new(MAIL_DATE_COLUMN_WIDTH));
        let date_drag_gesture = gtk::GestureDrag::new();
        date_drag_gesture.set_button(1);

        let date_col_for_begin = date_column.clone();
        let date_width_at_start_for_begin = Rc::clone(&date_width_at_start);
        let date_current_width_for_begin = Rc::clone(&date_current_width);
        let is_date_resizing_for_begin = Rc::clone(&is_date_resizing);
        date_drag_gesture.connect_drag_begin(move |gesture, start_x, _| {
            let width = date_col_for_begin.width_request().clamp(60, 300);
            if start_x <= 8.0 {
                is_date_resizing_for_begin.set(true);
                date_width_at_start_for_begin.set(width);
                date_current_width_for_begin.set(width);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else {
                is_date_resizing_for_begin.set(false);
            }
        });

        let date_col_for_update = date_column.clone();
        let date_width_at_start_for_update = Rc::clone(&date_width_at_start);
        let date_current_width_for_update = Rc::clone(&date_current_width);
        let is_date_resizing_for_update = Rc::clone(&is_date_resizing);
        date_drag_gesture.connect_drag_update(move |gesture, offset_x, _| {
            if is_date_resizing_for_update.get() {
                let width = (date_width_at_start_for_update.get() as f64 - offset_x)
                    .round()
                    .clamp(60.0, 300.0) as i32;
                date_current_width_for_update.set(width);
                date_col_for_update.set_width_request(width);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });

        let is_date_resizing_for_end = Rc::clone(&is_date_resizing);
        date_drag_gesture.connect_drag_end(move |_, _, _| {
            is_date_resizing_for_end.set(false);
        });
        date_column.add_controller(date_drag_gesture);

        let date_sort_gesture = gtk::GestureClick::new();
        date_sort_gesture.set_button(1);
        let page_for_date_sort = self.downgrade();
        date_sort_gesture.connect_pressed(move |gesture, _, x, _| {
            if x > 8.0 {
                if let Some(page) = page_for_date_sort.upgrade() {
                    page.set_mail_sort_from_header(MailSortField::Date);
                }
            } else {
                gesture.set_state(gtk::EventSequenceState::Denied);
            }
        });
        date_column.set_tooltip_text(Some("Nach Datum sortieren"));
        date_column.add_controller(date_sort_gesture);

        let attachment_sort_gesture = gtk::GestureClick::new();
        attachment_sort_gesture.set_button(1);
        let page_for_attachment_sort = self.downgrade();
        attachment_sort_gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page_for_attachment_sort.upgrade() {
                page.set_mail_sort_from_header(MailSortField::Attachments);
            }
        });
        attachment_column.set_tooltip_text(Some("Nach Anhängen sortieren"));
        attachment_column.add_controller(attachment_sort_gesture);

        let star_sort_gesture = gtk::GestureClick::new();
        star_sort_gesture.set_button(1);
        let page_for_star_sort = self.downgrade();
        star_sort_gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page_for_star_sort.upgrade() {
                page.set_mail_sort_from_header(MailSortField::Flagged);
            }
        });
        star_column.set_tooltip_text(Some("Nach Markierung sortieren"));
        star_column.add_controller(star_sort_gesture);

        let important_sort_gesture = gtk::GestureClick::new();
        important_sort_gesture.set_button(1);
        let page_for_important_sort = self.downgrade();
        important_sort_gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page_for_important_sort.upgrade() {
                page.set_mail_sort_from_header(MailSortField::Important);
            }
        });
        important_column.set_tooltip_text(Some("Nach Wichtigkeit sortieren"));
        important_column.add_controller(important_sort_gesture);

        let subject_sort_gesture = gtk::GestureClick::new();
        subject_sort_gesture.set_button(1);
        let page_for_subject_sort = self.downgrade();
        subject_sort_gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page_for_subject_sort.upgrade() {
                page.set_mail_sort_from_header(MailSortField::Subject);
            }
        });
        subject_column.set_tooltip_text(Some("Nach Betreff sortieren"));
        subject_column.add_controller(subject_sort_gesture);

        self.imp().mail_column_header.replace(Some(header.clone()));
        self.refresh_mail_column_header();
        header
    }

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
        let search_scope = Rc::new(Cell::new(QuickSearchScope::CurrentFolder));
        let query_for_filter = Rc::clone(&query);
        let message_filter_for_filter = Rc::clone(&message_filter);
        let folder_selection_for_filter = Rc::clone(&folder_selection);
        let search_scope_for_filter = Rc::clone(&search_scope);
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
                search_scope_for_filter.get(),
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
        let search_scope_for_thread_filter = Rc::clone(&search_scope);
        let thread_members_for_filter = Rc::clone(&thread_members);
        let thread_filter = gtk::CustomFilter::new(move |obj| {
            let Some(root) = obj.downcast_ref::<MailMessageObject>() else {
                return false;
            };
            let query = query_for_thread_filter.borrow();
            let selected_folder = folder_selection_for_thread_filter.borrow();
            let members = thread_members_for_filter.borrow();
            let scope = search_scope_for_thread_filter.get();
            members
                .get(&root.thread_id())
                .map(|messages| {
                    messages.iter().any(|message| {
                        message_matches_filter(
                            message,
                            &query,
                            message_filter_for_thread_filter.get(),
                            &selected_folder,
                            scope,
                        )
                    })
                })
                .unwrap_or_else(|| {
                    message_matches_filter(
                        root,
                        &query,
                        message_filter_for_thread_filter.get(),
                        &selected_folder,
                        scope,
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
                    .filter(|store| store.n_items() > 0)
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

        let page_for_row_actions = self.downgrade();
        factory.connect_setup(move |_, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("Expected ListItem");

            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(0)
                .margin_start(4)
                .margin_end(4)
                .margin_top(0)
                .margin_bottom(0)
                .hexpand(true)
                .css_classes(vec!["message-line"])
                .build();

            let header_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(0)
                .hexpand(true)
                .build();

            let thread_button = gtk::Button::builder()
                .width_request(MAIL_THREAD_COLUMN_WIDTH)
                .css_classes(vec!["flat", "mail-thread-count"])
                .valign(gtk::Align::Center)
                .build();
            set_accessible_label(&thread_button, "Konversation auf- oder zuklappen");
            let list_item_for_thread = list_item.downgrade();
            thread_button.connect_clicked(move |_| {
                let Some(list_item) = list_item_for_thread.upgrade() else {
                    return;
                };
                let Some(tree_row) = list_item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                if tree_row.depth() == 0 && tree_row.is_expandable() {
                    tree_row.set_expanded(!tree_row.is_expanded());
                }
            });

            let avatar = adw::Avatar::builder().size(24).show_initials(true).build();
            avatar.set_width_request(MAIL_AVATAR_COLUMN_WIDTH);

            let sender_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(4)
                .width_request(
                    AppSettings::try_new()
                        .map(|settings| settings.mail_sender_column_width())
                        .unwrap_or(180),
                )
                .hexpand(false)
                .halign(gtk::Align::Start)
                .overflow(gtk::Overflow::Hidden)
                .build();

            let sender = gtk::Label::builder()
                .css_classes(vec!["heading"])
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(1)
                .width_chars(1)
                .build();

            let recipient = gtk::Label::builder()
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(1)
                .width_chars(1)
                .build();

            let date = gtk::Label::builder()
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .width_request(MAIL_DATE_COLUMN_WIDTH)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(1)
                .width_chars(1)
                .max_width_chars(16)
                .build();

            let status = gtk::Image::new();
            status.set_width_request(MAIL_INDICATOR_COLUMN_WIDTH);
            let (init_status_icon, _) = mail_row_status_icon(false, false, false);
            set_image_icon_with_fallback(
                &status,
                init_status_icon,
                "mail-message-new-symbolic",
            );
            status.add_css_class("dim-label");
            status.set_tooltip_text(Some("Lesestatus"));
            status.set_valign(gtk::Align::Center);

            let btn_star = gtk::Button::builder()
                .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
                .icon_name("non-starred-symbolic")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .tooltip_text("Markieren / Sternchen setzen")
                .build();
            set_button_icon_with_fallback(
                &btn_star,
                "non-starred-symbolic",
                "bookmark-new-symbolic",
            );
            set_accessible_label(&btn_star, "Nachricht markieren");

            let btn_follow_up = gtk::Button::builder()
                .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
                .icon_name("flag-symbolic")
                .css_classes(vec!["flat", "circular", "mail-follow-up"])
                .valign(gtk::Align::Center)
                .tooltip_text("Nachverfolgung setzen")
                .build();
            set_button_icon_with_fallback(
                &btn_follow_up,
                "flag-symbolic",
                "bookmark-new-symbolic",
            );
            set_accessible_label(&btn_follow_up, "Nachverfolgung setzen");

            let btn_important = gtk::Button::builder()
                .width_request(MAIL_INDICATOR_COLUMN_WIDTH)
                .icon_name("mail-mark-important-symbolic")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .tooltip_text("Als wichtig markieren")
                .build();
            set_button_icon_with_fallback(
                &btn_important,
                "mail-mark-important-symbolic",
                "dialog-warning-symbolic",
            );
            set_accessible_label(&btn_important, "Nachricht als wichtig markieren");

            let list_item_for_star = list_item.downgrade();
            let page_for_star = page_for_row_actions.clone();
            btn_star.connect_clicked(move |_| {
                let Some(list_item) = list_item_for_star.upgrade() else {
                    return;
                };
                let Some(tree_row) = list_item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                let Some(message) = tree_row.item().and_downcast::<MailMessageObject>() else {
                    return;
                };
                let current = message.is_flagged();
                let next_flagged = !current;
                let command = MailCommand::set_message_state(
                    message.id(),
                    message.backend_location(),
                    message.is_unread(),
                    current,
                    message.is_unread(),
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
                }
            });

            let list_item_for_follow_up = list_item.downgrade();
            let page_for_follow_up = page_for_row_actions.clone();
            btn_follow_up.connect_clicked(move |_| {
                let Some(list_item) = list_item_for_follow_up.upgrade() else {
                    return;
                };
                let Some(tree_row) = list_item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                let Some(message) = tree_row.item().and_downcast::<MailMessageObject>() else {
                    return;
                };
                let Some(page) = page_for_follow_up.upgrade() else {
                    return;
                };
                page.present_follow_up_editor(&message);
            });

            let list_item_for_important = list_item.downgrade();
            let page_for_important = page_for_row_actions.clone();
            btn_important.connect_clicked(move |_| {
                let Some(list_item) = list_item_for_important.upgrade() else {
                    return;
                };
                let Some(tree_row) = list_item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                let Some(message) = tree_row.item().and_downcast::<MailMessageObject>() else {
                    return;
                };
                let current = message.is_important();
                let Some(page) = page_for_important.upgrade() else {
                    return;
                };
                let result = if message
                    .backend_location()
                    .as_ref()
                    .is_some_and(|location| !is_graph_backend_location(Some(location)))
                {
                    Err(MailOperationError::Unsupported {
                        operation: if current {
                            crate::services::mail_operations_service::MailOperationKind::MarkNotImportant
                        } else {
                            crate::services::mail_operations_service::MailOperationKind::MarkImportant
                        },
                        backend: "Wichtigkeitsmarkierung dieses Kontos (Linux-Abnahme ausstehend)",
                    })
                } else {
                    let command = message
                        .backend_location()
                        .map(|location| {
                            MailCommand::set_important(
                                message.id(),
                                location,
                                current,
                                !current,
                            )
                        })
                        .unwrap_or_else(|| {
                            MailCommand::set_important_local(message.id(), current, !current)
                        });
                    page.execute_mail_command(command)
                };
                if let Err(error) = result {
                    page.show_toast(&format!("Wichtig-Markierung nicht möglich: {error}"));
                }
            });

            let attachment = gtk::Image::new();
            attachment.set_width_request(MAIL_INDICATOR_COLUMN_WIDTH);
            set_image_icon_with_fallback(
                &attachment,
                "mail-attachment-symbolic",
                "document-open-symbolic",
            );
            attachment.set_tooltip_text(Some("Anhänge"));
            attachment.set_valign(gtk::Align::Center);

            sender_box.append(&sender);
            sender_box.append(&recipient);
            header_box.append(&thread_button);
            header_box.append(&status);
            header_box.append(&attachment);
            header_box.append(&btn_follow_up);
            header_box.append(&btn_star);
            header_box.append(&btn_important);
            header_box.append(&avatar);
            header_box.append(&sender_box);

            let subject = gtk::Label::builder()
                .css_classes(vec!["body"])
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(1)
                .width_chars(1)
                .build();

            let preview = gtk::Label::builder()
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::Start)
                .width_request(MAIL_PREVIEW_COLUMN_WIDTH)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(1)
                .width_chars(1)
                .max_width_chars(20)
                .build();

            if let Some(page) = page_for_row_actions.upgrade() {
                page.ensure_size_groups();
                if let Some(group) = page.imp().status_size_group.borrow().as_ref() {
                    group.add_widget(&status);
                }
                if let Some(group) = page.imp().attachment_size_group.borrow().as_ref() {
                    group.add_widget(&attachment);
                }
                if let Some(group) = page.imp().follow_up_size_group.borrow().as_ref() {
                    group.add_widget(&btn_follow_up);
                }
                if let Some(group) = page.imp().star_size_group.borrow().as_ref() {
                    group.add_widget(&btn_star);
                }
                if let Some(group) = page.imp().important_size_group.borrow().as_ref() {
                    group.add_widget(&btn_important);
                }
                if let Some(group) = page.imp().sender_size_group.borrow().as_ref() {
                    group.add_widget(&sender_box);
                }
                if let Some(group) = page.imp().preview_size_group.borrow().as_ref() {
                    group.add_widget(&preview);
                }
                if let Some(group) = page.imp().date_size_group.borrow().as_ref() {
                    group.add_widget(&date);
                }
            }

            // Match Evolution's table scan order: status/attachment columns
            // lead, then sender and subject, with the date at the far edge.
            header_box.append(&subject);
            header_box.append(&preview);
            header_box.append(&date);
            vbox.append(&header_box);

            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            separator.set_margin_top(0);

            let outer = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .hexpand(true)
                .build();
            outer.append(&vbox);
            outer.append(&separator);

            let expander = gtk::TreeExpander::new();
            expander.set_hexpand(true);
            expander.set_indent_for_icon(false);
            expander.set_indent_for_depth(false);
            expander.set_hide_expander(true);
            expander.set_child(Some(&outer));
            list_item.set_child(Some(&expander));

            let right_click = gtk::GestureClick::builder().button(3).build();
            let list_item_for_menu = list_item.downgrade();
            let outer_for_menu = outer.downgrade();
            let page_for_menu = page_for_row_actions.clone();
            right_click.connect_pressed(move |gesture, _, x, y| {
                let (Some(page), Some(list_item), Some(outer)) = (
                    page_for_menu.upgrade(),
                    list_item_for_menu.upgrade(),
                    outer_for_menu.upgrade(),
                ) else {
                    return;
                };
                let position = list_item.position();
                if position != gtk::INVALID_LIST_POSITION {
                    if let Some(selection) = page.imp().thread_selection.borrow().as_ref() {
                        selection.set_selected(position);
                    }
                    if let Some(tree_row) = list_item.item().and_downcast::<gtk::TreeListRow>() {
                        if let Some(msg) = tree_row.item().and_downcast::<MailMessageObject>() {
                            if let Some(flat_sel) = page.imp().message_selection.borrow().as_ref() {
                                for i in 0..flat_sel.n_items() {
                                    if let Some(cand) = flat_sel.item(i).and_downcast::<MailMessageObject>() {
                                        if cand.id() == msg.id() {
                                            flat_sel.set_selected(i);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);

                let popover = gtk::PopoverMenu::builder()
                    .menu_model(&Self::message_context_menu())
                    .has_arrow(false)
                    .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
                    .build();
                popover.set_parent(&outer);
                let popover_weak = popover.downgrade();
                popover.connect_closed(move |_| {
                    if let Some(popover) = popover_weak.upgrade() {
                        popover.unparent();
                    }
                });
                popover.popup();
            });
            outer.add_controller(right_click);
        });

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
            let threading = page_for_bind
                .upgrade()
                .is_some_and(|page| page.imp().mail_threading.get());

            let visible_thread_size = visible_thread_size(threading, msg.thread_size());
            let thread_label = if visible_thread_size > 1 {
                format!("{} Nachrichten", visible_thread_size)
            } else {
                "Einzelne Nachricht".to_string()
            };
            list_item.set_accessible_label(&format!(
                "{} von {} · {} · {}",
                msg.subject(),
                sender_header(&msg),
                format_message_date(&msg.date_str()),
                thread_label
            ));
            list_item.set_accessible_description(&format!(
                "{}{}{}",
                if msg.is_unread() {
                    "Ungelesen. "
                } else {
                    "Gelesen. "
                },
                if msg.is_flagged() { "Markiert. " } else { "" },
                if msg.attachments().is_empty() {
                    "Keine Anhänge."
                } else {
                    "Anhänge vorhanden."
                }
            ));

            let expander = list_item
                .child()
                .and_downcast::<gtk::TreeExpander>()
                .expect("Expected TreeExpander");
            expander.set_list_row(Some(&tree_row));
            expander.set_hide_expander(true);
            expander.set_indent_for_depth(false);
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
                .unwrap_or_else(|| "compact".to_string());
            let (row_spacing, row_margin) = mail_row_density_metrics(&density);
            vbox.set_spacing(row_spacing);
            vbox.set_margin_top(row_margin);
            vbox.set_margin_bottom(row_margin);

            let settings = AppSettings::try_new();
            let show_avatar = settings
                .as_ref()
                .map(AppSettings::mail_show_avatar)
                .unwrap_or(false);
            let show_sender = settings
                .as_ref()
                .map(AppSettings::mail_show_sender)
                .unwrap_or(true);
            let show_recipient = settings
                .as_ref()
                .map(AppSettings::mail_show_recipient)
                .unwrap_or(false);
            let show_date = settings
                .as_ref()
                .map(AppSettings::mail_show_date)
                .unwrap_or(true);
            let show_attachment = settings
                .as_ref()
                .map(AppSettings::mail_show_attachment)
                .unwrap_or(true);
            let show_preview_text = settings
                .as_ref()
                .map(AppSettings::mail_show_preview_text)
                .unwrap_or(false);
            let show_labels = settings
                .as_ref()
                .map(AppSettings::mail_show_labels)
                .unwrap_or(true);

            let header_box = vbox
                .first_child()
                .and_downcast::<gtk::Box>()
                .expect("Expected header Box");

            if let Some(thread_button) =
                Self::mail_header_child(&header_box, 0).and_downcast::<gtk::Button>()
            {
                let is_conversation =
                    mail_row_expander_visible(visible_thread_size, tree_row.depth());
                thread_button.set_label("");
                if is_conversation {
                    set_button_icon_with_fallback(
                        &thread_button,
                        if tree_row.is_expanded() {
                            "pan-down-symbolic"
                        } else {
                            "pan-end-symbolic"
                        },
                        "go-next-symbolic",
                    );
                    thread_button.set_opacity(1.0);
                    thread_button.set_sensitive(true);
                } else {
                    thread_button.set_icon_name("");
                    thread_button.set_opacity(0.0);
                    thread_button.set_sensitive(false);
                }
                thread_button.set_tooltip_text(is_conversation.then_some(
                    if tree_row.is_expanded() {
                        "Konversation zuklappen"
                    } else {
                        "Konversation aufklappen"
                    },
                ));
            }

            if let Some(status) =
                Self::mail_header_child(&header_box, 1).and_downcast::<gtk::Image>()
            {
                let (icon_name, tooltip) = mail_row_status_icon(msg.is_unread(), false, false);
                set_image_icon_with_fallback(&status, icon_name, "mail-message-new-symbolic");
                status.set_tooltip_text(Some(tooltip));
            }

            if let Some(avatar) =
                Self::mail_header_child(&header_box, 6).and_downcast::<adw::Avatar>()
            {
                avatar.set_visible(show_avatar);
                avatar.set_text(Some(&msg.sender_name()));
            }

            if let Some(sender_box) =
                Self::mail_header_child(&header_box, 7).and_downcast::<gtk::Box>()
            {
                sender_box.set_width_request(
                    settings
                        .as_ref()
                        .map(AppSettings::mail_sender_column_width)
                        .unwrap_or(180),
                );
                sender_box.set_visible(show_sender || show_recipient);
                if let Some(sender) = sender_box.first_child().and_downcast::<gtk::Label>() {
                    sender.set_visible(show_sender);
                    sender.set_label(&msg.sender_name());
                    if msg.is_unread() {
                        sender.add_css_class("unread-heading");
                    } else {
                        sender.remove_css_class("unread-heading");
                    }
                }
                if let Some(recipient) = sender_box
                    .first_child()
                    .and_then(|child| child.next_sibling())
                    .and_downcast::<gtk::Label>()
                {
                    recipient.set_visible(show_recipient);
                    let recipient_text = if !msg.recipient_to().trim().is_empty() {
                        format!("An: {}", msg.recipient_to())
                    } else if !msg.recipient_cc().trim().is_empty() {
                        format!("Cc: {}", msg.recipient_cc())
                    } else {
                        String::new()
                    };
                    recipient.set_label(&recipient_text);
                }
            }

            if let Some(date) =
                Self::mail_header_child(&header_box, 10).and_downcast::<gtk::Label>()
            {
                date.set_visible(show_date);
                date.set_label(&format_message_date(&msg.date_str()));
            }

            if let Some(attachment) =
                Self::mail_header_child(&header_box, 2).and_downcast::<gtk::Image>()
            {
                attachment.set_visible(show_attachment);
                if let Some((icon_name, tooltip)) = mail_row_attachment_info(&msg.attachments()) {
                    set_image_icon_with_fallback(
                        &attachment,
                        icon_name,
                        "mail-attachment-symbolic",
                    );
                    attachment.set_opacity(1.0);
                    attachment.set_tooltip_text(Some(tooltip));
                } else {
                    attachment.set_opacity(0.0);
                    attachment.set_tooltip_text(None);
                }
            }

            if let Some(button) =
                Self::mail_header_child(&header_box, 3).and_downcast::<gtk::Button>()
            {
                button.remove_css_class("follow-up-pending");
                button.remove_css_class("follow-up-completed");
                match msg.follow_up_state() {
                    MailFollowUpState::None => {
                        set_button_icon_with_fallback(
                            &button,
                            "flag-symbolic",
                            "bookmark-new-symbolic",
                        );
                        button.set_opacity(0.24);
                        button.set_tooltip_text(Some("Nachverfolgung setzen"));
                        set_accessible_label(&button, "Nachverfolgung setzen");
                    }
                    MailFollowUpState::FollowUp => {
                        set_button_icon_with_fallback(
                            &button,
                            "flag-symbolic",
                            "bookmark-new-symbolic",
                        );
                        button.set_opacity(1.0);
                        button.add_css_class("follow-up-pending");
                        button.set_tooltip_text(Some("Als erledigt markieren"));
                        set_accessible_label(&button, "Nachverfolgung als erledigt markieren");
                    }
                    MailFollowUpState::Completed => {
                        set_button_icon_with_fallback(
                            &button,
                            "emblem-ok-symbolic",
                            "object-select-symbolic",
                        );
                        button.set_opacity(1.0);
                        button.add_css_class("follow-up-completed");
                        button.set_tooltip_text(Some("Nachverfolgung entfernen"));
                        set_accessible_label(&button, "Nachverfolgung entfernen");
                    }
                }
            }

            if let Some(btn_star) =
                Self::mail_header_child(&header_box, 4).and_downcast::<gtk::Button>()
            {
                let graph_follow_up_only = msg
                    .backend_location()
                    .as_ref()
                    .is_some_and(|location| is_graph_backend_location(Some(location)));
                btn_star.set_sensitive(!graph_follow_up_only);
                if graph_follow_up_only {
                    btn_star.set_tooltip_text(Some(
                        "Dieses Konto verwendet die separate Nachverfolgungsspalte",
                    ));
                } else {
                    btn_star.set_tooltip_text(Some("Markieren / Sternchen setzen"));
                }
                if msg.is_flagged() {
                    set_button_icon_with_fallback(
                        &btn_star,
                        "starred-symbolic",
                        "bookmark-new-symbolic",
                    );
                    set_accessible_label(&btn_star, "Markierung entfernen");
                } else {
                    set_button_icon_with_fallback(
                        &btn_star,
                        "non-starred-symbolic",
                        "bookmark-new-symbolic",
                    );
                    set_accessible_label(&btn_star, "Nachricht markieren");
                }
                if graph_follow_up_only {
                    set_accessible_label(
                        &btn_star,
                        "Sternmarkierung nicht verfügbar; Nachverfolgungsspalte verwenden",
                    );
                }
            }

            if let Some(btn_important) =
                Self::mail_header_child(&header_box, 5).and_downcast::<gtk::Button>()
            {
                set_button_icon_with_fallback(
                    &btn_important,
                    "mail-mark-important-symbolic",
                    "dialog-warning-symbolic",
                );
                btn_important.set_opacity(if msg.is_important() { 1.0 } else { 0.28 });
                set_accessible_label(
                    &btn_important,
                    if msg.is_important() {
                        "Wichtigkeitsmarkierung entfernen"
                    } else {
                        "Nachricht als wichtig markieren"
                    },
                );
            }

            if let Some(subject) =
                Self::mail_header_child(&header_box, 8).and_downcast::<gtk::Label>()
            {
                let mut subject_text = msg.subject();
                let labels = msg.labels();
                if show_labels && !labels.is_empty() {
                    subject_text.push_str(&format!("  ·  [{}]", labels.join(", ")));
                }
                subject.set_label(&subject_text);
            }

            if let Some(preview) =
                Self::mail_header_child(&header_box, 9).and_downcast::<gtk::Label>()
            {
                preview.set_visible(show_preview_text);
                preview.set_label(&msg.preview_snippet());
            }

            if let Some(page) = page_for_bind.upgrade() {
                let is_batch_selected = page.imp().batch_selection.borrow().contains(&msg.id());
                if is_batch_selected {
                    outer.add_css_class("batch-selected");
                } else {
                    outer.remove_css_class("batch-selected");
                }
            }
            if mail_row_is_shaded(list_item.position()) {
                outer.add_css_class("mail-row-even");
            } else {
                outer.remove_css_class("mail-row-even");
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
            .css_classes(vec!["mail-search-bar"])
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

        let quick_filter_model =
            gtk::StringList::new(&["Alle", "Ungelesen", "Markiert", "Wichtig", "Mit Anhang"]);
        let quick_filter = gtk::DropDown::new(Some(quick_filter_model), None::<&gtk::Expression>);
        quick_filter.set_tooltip_text(Some("Nachrichtenfilter"));
        set_accessible_label(&quick_filter, "Nachrichtenfilter");
        quick_filter.set_selected(message_filter.get().selector_index());
        let message_filter_for_quick_filter = Rc::clone(&message_filter);
        let filter_for_quick_filter = custom_filter.clone();
        let thread_filter_for_quick_filter = thread_filter.clone();
        let page_for_quick_filter = self.downgrade();
        quick_filter.connect_selected_notify(move |selector| {
            let filter = MessageFilter::from_selector_index(selector.selected());
            message_filter_for_quick_filter.set(filter);
            filter_for_quick_filter.changed(gtk::FilterChange::Different);
            thread_filter_for_quick_filter.changed(gtk::FilterChange::Different);
            if let Some(page) = page_for_quick_filter.upgrade() {
                // A filtered-out selection otherwise leaves the reader on a
                // message that is no longer visible, which makes the
                // dropdown appear inert. Wait one main-loop turn for the
                // filter model to apply, then select the first visible result.
                let page = page.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(page) = page.upgrade() {
                        page.select_message_after_reload(&MailSelectionAnchor::default());
                    }
                });
            }
        });
        self.imp().quick_filter.replace(Some(quick_filter.clone()));

        let save_search_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Aktuelle Suche als Suchordner speichern")
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(
            &save_search_button,
            "Aktuelle Suche als Suchordner speichern",
        );
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

        let search_field_model = gtk::StringList::new(&[
            "Alle Felder",
            "Betreff",
            "Absender",
            "Empfänger",
            "Nachrichtentext",
        ]);
        let search_field_selector =
            gtk::DropDown::new(Some(search_field_model), None::<&gtk::Expression>);
        search_field_selector.set_tooltip_text(Some("Zu durchsuchendes Feld"));
        set_accessible_label(&search_field_selector, "Suchfeld");
        search_field_selector.set_selected(self.imp().search_field_scope.get().selector_index());
        self.imp()
            .search_field_selector
            .replace(Some(search_field_selector.clone()));

        let search_scope_model = gtk::StringList::new(&[
            "Aktueller Ordner",
            "Aktuelles Konto",
            "Alle Konten",
        ]);
        let search_scope_selector =
            gtk::DropDown::new(Some(search_scope_model), None::<&gtk::Expression>);
        search_scope_selector.set_tooltip_text(Some("Suchbereich"));
        set_accessible_label(&search_scope_selector, "Suchbereich");
        search_scope_selector.set_selected(self.imp().search_scope.get().selector_index());
        self.imp()
            .search_scope_selector
            .replace(Some(search_scope_selector.clone()));

        let search_scope_for_scope_change = Rc::clone(&search_scope);
        let filter_for_scope_change = custom_filter.clone();
        let thread_filter_for_scope_change = thread_filter.clone();
        let page_for_scope_change = self.downgrade();
        search_scope_selector.connect_selected_notify(move |selector| {
            let scope = QuickSearchScope::from_selector_index(selector.selected());
            search_scope_for_scope_change.set(scope);
            filter_for_scope_change.changed(gtk::FilterChange::Different);
            thread_filter_for_scope_change.changed(gtk::FilterChange::Different);
            if let Some(page) = page_for_scope_change.upgrade() {
                page.imp().search_scope.set(scope);
            }
        });

        let search_field_for_field_change = self.downgrade();
        let query_for_field_change = Rc::clone(&query);
        let filter_for_field_change = custom_filter.clone();
        let thread_filter_for_field_change = thread_filter.clone();
        let search_entry_for_field_change = search_entry.clone();
        search_field_selector.connect_selected_notify(move |selector| {
            let field_scope = QuickSearchField::from_selector_index(selector.selected());
            let text = search_entry_for_field_change.text();
            *query_for_field_change.borrow_mut() =
                MessageSearchQuery::parse_with_field_scope(text.as_str(), field_scope);
            filter_for_field_change.changed(gtk::FilterChange::Different);
            thread_filter_for_field_change.changed(gtk::FilterChange::Different);
            if let Some(page) = search_field_for_field_change.upgrade() {
                page.imp().search_field_scope.set(field_scope);
                page.handle_search_changed(text.as_str());
            }
        });

        let query_for_search = Rc::clone(&query);
        let filter_for_search = custom_filter.clone();
        let thread_filter_for_search = thread_filter.clone();
        let page_for_search = self.downgrade();
        search_entry.connect_search_changed(move |entry| {
            let field_scope = page_for_search
                .upgrade()
                .map(|p| p.imp().search_field_scope.get())
                .unwrap_or_default();
            *query_for_search.borrow_mut() =
                MessageSearchQuery::parse_with_field_scope(entry.text().as_str(), field_scope);
            filter_for_search.changed(gtk::FilterChange::Different);
            thread_filter_for_search.changed(gtk::FilterChange::Different);
            if let Some(page) = page_for_search.upgrade() {
                page.handle_search_changed(entry.text().as_str());
            }
        });

        let sort_model = gtk::StringList::new(&[
            "Datum",
            "Betreff",
            "Absender",
            "Ungelesen-Status",
            "Markierung",
            "Wichtigkeit",
            "Anhänge",
        ]);
        let sort_selector = gtk::DropDown::new(Some(sort_model), None::<&gtk::Expression>);
        sort_selector.set_tooltip_text(Some("Mails sortieren"));
        set_accessible_label(&sort_selector, "Nachrichten sortieren");
        sort_selector.set_selected(Self::mail_sort_selector_index(
            self.imp().mail_sort_field.get(),
        ));
        self.imp()
            .mail_sort_selector
            .replace(Some(sort_selector.clone()));
        let direction_button = gtk::Button::builder()
            .css_classes(vec!["flat", "circular"])
            .build();
        let set_direction_button = |button: &gtk::Button, descending: bool| {
            button.set_icon_name(if descending {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            let label = if descending {
                "Sortierung absteigend; Sortierrichtung umschalten"
            } else {
                "Sortierung aufsteigend; Sortierrichtung umschalten"
            };
            button.set_tooltip_text(Some(label));
            set_accessible_label(button, label);
        };
        set_direction_button(&direction_button, self.imp().mail_sort_descending.get());
        self.imp()
            .mail_sort_direction_button
            .replace(Some(direction_button.clone()));

        let page_for_sort = self.downgrade();
        sort_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_for_sort.upgrade() else {
                return;
            };
            let field = MailPage::mail_sort_field_from_selector_index(selector.selected());
            page.imp().mail_sort_field.set(field);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_sort_field(field.setting_value());
            }
            page.resort_mail_preserving_selection();
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
            page.resort_mail_preserving_selection();
        });

        let columns_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .margin_bottom(8)
            .width_request(230)
            .build();
        columns_popover_box.append(
            &gtk::Label::builder()
                .label("Sichtbare Spalten")
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .margin_bottom(4)
                .build(),
        );
        let column_settings = [
            ("mail-show-avatar", "Avatare"),
            ("mail-show-sender", "Absender"),
            ("mail-show-recipient", "Empfänger"),
            ("mail-show-date", "Datum"),
            ("mail-show-attachment", "Anhänge"),
            ("mail-show-preview-text", "Vorschautext"),
            ("mail-show-labels", "Labels"),
        ];
        if let Some(settings) = AppSettings::try_new() {
            for (key, label) in column_settings {
                let active = match key {
                    "mail-show-avatar" => settings.mail_show_avatar(),
                    "mail-show-sender" => settings.mail_show_sender(),
                    "mail-show-recipient" => settings.mail_show_recipient(),
                    "mail-show-date" => settings.mail_show_date(),
                    "mail-show-attachment" => settings.mail_show_attachment(),
                    "mail-show-preview-text" => settings.mail_show_preview_text(),
                    "mail-show-labels" => settings.mail_show_labels(),
                    _ => false,
                };
                let check = gtk::CheckButton::builder()
                    .label(label)
                    .active(active)
                    .build();
                let settings_for_check = settings.clone();
                check.connect_toggled(move |check| match key {
                    "mail-show-avatar" => {
                        settings_for_check.set_mail_show_avatar(check.is_active())
                    }
                    "mail-show-sender" => {
                        settings_for_check.set_mail_show_sender(check.is_active())
                    }
                    "mail-show-recipient" => {
                        settings_for_check.set_mail_show_recipient(check.is_active())
                    }
                    "mail-show-date" => settings_for_check.set_mail_show_date(check.is_active()),
                    "mail-show-attachment" => {
                        settings_for_check.set_mail_show_attachment(check.is_active())
                    }
                    "mail-show-preview-text" => {
                        settings_for_check.set_mail_show_preview_text(check.is_active())
                    }
                    "mail-show-labels" => {
                        settings_for_check.set_mail_show_labels(check.is_active())
                    }
                    _ => {}
                });
                columns_popover_box.append(&check);
            }

            columns_popover_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            columns_popover_box.append(
                &gtk::Label::builder()
                    .label("Absenderbreite")
                    .halign(gtk::Align::Start)
                    .css_classes(vec!["caption", "dim-label"])
                    .margin_top(4)
                    .build(),
            );
            let width_adjustment = gtk::Adjustment::new(
                settings.mail_sender_column_width() as f64,
                80.0,
                360.0,
                10.0,
                40.0,
                0.0,
            );
            let width_scale =
                gtk::Scale::new(gtk::Orientation::Horizontal, Some(&width_adjustment));
            width_scale.set_digits(0);
            width_scale.set_draw_value(true);
            width_scale.set_hexpand(true);
            let settings_for_width = settings.clone();
            width_scale.connect_value_changed(move |scale| {
                settings_for_width.set_mail_sender_column_width(scale.value() as i32);
            });
            columns_popover_box.append(&width_scale);
        } else {
            columns_popover_box.append(
                &gtk::Label::builder()
                    .label("Einstellungen sind in diesem Entwicklungsmodus nicht verfügbar.")
                    .wrap(true)
                    .css_classes(vec!["caption", "dim-label"])
                    .build(),
            );
        }
        let columns_popover = gtk::Popover::builder().child(&columns_popover_box).build();
        let columns_button = gtk::MenuButton::builder()
            .label("Spalten")
            .icon_name("view-list-symbolic")
            .tooltip_text("Sichtbare Mailspalten und Absenderbreite")
            .popover(&columns_popover)
            .css_classes(vec!["flat"])
            .build();
        columns_button.set_icon_name(&themed_icon_name(
            &columns_button,
            "view-list-symbolic",
            "open-menu-symbolic",
        ));
        let syntax_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .margin_bottom(8)
            .width_request(300)
            .build();
        syntax_popover_box.append(
            &gtk::Label::builder()
                .label("Suchsyntax-Operatoren")
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .margin_bottom(4)
                .build(),
        );
        let syntax_items = [
            ("from:", "Absender (z.B. from:alice)"),
            ("to:", "Empfänger (z.B. to:bob)"),
            ("subject:", "Betreffzeile"),
            ("body:", "Nachrichtentext"),
            ("is:unread", "Ungelesene Mails"),
            ("is:flagged", "Markierte Mails"),
            ("has:attachment", "Mails mit Anhang"),
            ("in:", "Ordner (z.B. in:sent, in:drafts)"),
            ("on:", "Datum (z.B. on:2026-08-11)"),
            ("after:", "Nach Datum (after:2026-01-01)"),
            ("before:", "Vor Datum (before:2026-12-31)"),
            ("not:", "Begriff ausschließen"),
        ];
        let search_entry_for_syntax = search_entry.clone();
        for (token, desc) in syntax_items {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .build();
            let btn_token = gtk::Button::builder()
                .label(token)
                .css_classes(vec!["flat", "caption"])
                .build();
            let lbl_desc = gtk::Label::builder()
                .label(desc)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .css_classes(vec!["caption", "dim-label"])
                .build();
            let search_entry_clone = search_entry_for_syntax.clone();
            let token_str = token.to_string();
            btn_token.connect_clicked(move |_| {
                let current = search_entry_clone.text();
                let new_text = if current.trim().is_empty() {
                    token_str.clone()
                } else {
                    format!("{} {}", current.trim(), token_str)
                };
                search_entry_clone.set_text(&new_text);
                search_entry_clone.set_position(-1);
            });
            row.append(&btn_token);
            row.append(&lbl_desc);
            syntax_popover_box.append(&row);
        }
        let syntax_popover = gtk::Popover::builder().child(&syntax_popover_box).build();
        let syntax_help_button = gtk::MenuButton::builder()
            .icon_name("help-about-symbolic")
            .tooltip_text("Suchsyntax-Operatoren anzeigen")
            .popover(&syntax_popover)
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(&syntax_help_button, "Suchsyntax-Hilfe");

        search_box.append(&search_entry);
        search_box.append(&search_field_selector);
        search_box.append(&search_scope_selector);
        search_box.append(&quick_filter);
        search_box.append(&columns_button);
        search_box.append(&save_search_button);
        search_box.append(&syntax_help_button);
        self.imp().search_entry.replace(Some(search_entry.clone()));

        let list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        let classic_toolbar = self.build_classic_mail_toolbar();
        classic_toolbar.set_visible(
            AppSettings::try_new()
                .map(|settings| settings.mail_show_toolbar())
                .unwrap_or(true),
        );
        self.imp()
            .classic_toolbar
            .replace(Some(classic_toolbar.clone()));
        let message_state = gtk::Label::builder()
            .label("Nachrichten werden geladen …")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();

        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Nachrichten aktualisieren (F5)")
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(&refresh_button, "Nachrichten aktualisieren");
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
        set_accessible_label(&load_more_button, "Weitere Nachrichten laden");
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

        let batch_unread_button = gtk::Button::builder()
            .label("Ungelesen")
            .tooltip_text("Ausgewählte Nachrichten als ungelesen markieren")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_unread = self.downgrade();
        batch_unread_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_unread.upgrade() {
                if let Err(error) = page.mark_batch_unread() {
                    page.report_operation_error("Ungelesen-Markierung nicht möglich", &error);
                    tracing::warn!(%error, "Batch unread operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_unread_button);

        let batch_flag_button = gtk::Button::builder()
            .label("Markierung")
            .tooltip_text("Markierung der ausgewählten Nachrichten umschalten")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_flag = self.downgrade();
        batch_flag_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_flag.upgrade() {
                if let Err(error) = page.toggle_batch_flagged() {
                    page.report_operation_error("Markierung nicht möglich", &error);
                    tracing::warn!(%error, "Batch flag operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_flag_button);

        let batch_important_button = gtk::Button::builder()
            .label("Wichtig")
            .tooltip_text("Wichtigkeit der ausgewählten Nachrichten umschalten")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_important = self.downgrade();
        batch_important_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_important.upgrade() {
                if let Err(error) = page.toggle_batch_important() {
                    page.report_operation_error("Wichtig-Markierung nicht möglich", &error);
                    tracing::warn!(%error, "Batch important operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_important_button);

        let batch_label_button = gtk::Button::builder()
            .label("Label")
            .tooltip_text("Label der ausgewählten Nachrichten setzen oder entfernen")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_label = self.downgrade();
        batch_label_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_label.upgrade() {
                page.present_label_editor();
            }
        });
        batch_action_bar.append(&batch_label_button);

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

        let batch_not_junk_button = gtk::Button::builder()
            .label("Nicht Spam")
            .tooltip_text("Ausgewählten Nachrichten die Spam-Markierung entziehen")
            .css_classes(vec!["flat"])
            .build();
        let page_for_batch_not_junk = self.downgrade();
        batch_not_junk_button.connect_clicked(move |_| {
            if let Some(page) = page_for_batch_not_junk.upgrade() {
                if let Err(error) = page.mark_batch_not_junk() {
                    page.report_operation_error("Nicht-Spam-Markierung nicht möglich", &error);
                    tracing::warn!(%error, "Batch not-junk operation was not applied");
                }
            }
        });
        batch_action_bar.append(&batch_not_junk_button);

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
            .tooltip_text("Ausgewählte Nachrichten in einen Konto- oder lokalen Ordner verschieben")
            .popover(&move_popover)
            .css_classes(vec!["flat"])
            .build();
        batch_action_bar.append(&move_button);

        let copy_popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let copy_popover = gtk::Popover::builder().child(&copy_popover_content).build();
        let page_for_copy_menu = self.downgrade();
        let copy_popover_weak = copy_popover.downgrade();
        copy_popover.connect_show(move |_| {
            while let Some(child) = copy_popover_content.first_child() {
                copy_popover_content.remove(&child);
            }
            let Some(page) = page_for_copy_menu.upgrade() else {
                return;
            };
            let destinations = page.copy_folder_destinations();
            if destinations.is_empty() {
                copy_popover_content.append(
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
                let popover_for_destination = copy_popover_weak.clone();
                button.connect_clicked(move |_| {
                    if let Some(page) = page_for_destination.upgrade() {
                        page.copy_batch_to(&destination.source_uid, &destination.folder_name);
                    }
                    if let Some(popover) = popover_for_destination.upgrade() {
                        popover.popdown();
                    }
                });
                copy_popover_content.append(&button);
            }
        });
        let copy_button = gtk::MenuButton::builder()
            .label("Kopieren")
            .tooltip_text("Ausgewählte Nachrichten in einen weiteren Ordner kopieren")
            .popover(&copy_popover)
            .css_classes(vec!["flat"])
            .build();
        batch_action_bar.append(&copy_button);

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

        list_container.append(&batch_action_bar);
        list_container.append(&search_box);
        list_container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let column_header = self.build_mail_column_header();
        list_container.append(&column_header);

        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Always)
            .child(&list_view)
            .vexpand(true)
            .width_request(340)
            .build();

        list_container.append(&list_scroll);
        message_state_box.add_css_class("mail-pane-status");
        message_state_box.set_visible(false);
        self.imp()
            .message_status_area
            .replace(Some(message_state_box.clone()));

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
            let (folder_title, folder_icon) = selection.title_and_icon();
            if let Some(page) = page_for_rows.upgrade() {
                if let Some(quick_filter) = page.imp().quick_filter.borrow().as_ref() {
                    quick_filter.set_selected(0);
                }
                if let Some(title) = page.imp().drawer_header_title.borrow().as_ref() {
                    title.set_label(folder_title);
                }
                if let Some(icon) = page.imp().drawer_header_icon.borrow().as_ref() {
                    icon.set_icon_name(Some(folder_icon));
                }
            }
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
                            max_messages_per_folder: configured_mail_page_limit(),
                            graph_cursors: HashMap::new(),
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
                        .unwrap_or(false)
                        || page.imp().graph_backend_active.get();
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
                "vfolder-flagged" => MessageFilter::Flagged,
                "vfolder-important" => MessageFilter::Important,
                _ => MessageFilter::All,
            };
            message_filter.set(filter);
            if let Some(page) = page_for_search.upgrade() {
                let selected = filter.selector_index();
                if let Some(quick_filter) = page.imp().quick_filter.borrow().as_ref() {
                    quick_filter.set_selected(selected);
                }
            }
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
                        .unwrap_or(false)
                        || page.imp().graph_backend_active.get();
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
        gtk::Box,
        gtk::Box,
        gtk::ScrolledWindow,
        gtk::Revealer,
        gtk::Grid,
        gtk::Revealer,
        gtk::SearchEntry,
    ) {
        let detail_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["mail-detail"])
            .spacing(0)
            .build();

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["mail-detail-header"])
            .spacing(2)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(3)
            .build();

        let meta_grid = gtk::Grid::builder()
            .row_spacing(0)
            .column_spacing(6)
            .build();

        let crypto_label = gtk::Label::builder()
            .label("Sicherheit:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .visible(false)
            .build();

        let crypto_chip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .css_classes(vec!["badge"])
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        let icon_lock = gtk::Image::new();
        set_image_icon_with_fallback(
            &icon_lock,
            "security-high-symbolic",
            "dialog-information-symbolic",
        );
        let lbl_crypto = gtk::Label::builder()
            .label("Keine OpenPGP-/S/MIME-Sicherung erkannt")
            .css_classes(vec!["caption"])
            .build();
        crypto_chip.append(&icon_lock);
        crypto_chip.append(&lbl_crypto);

        meta_grid.attach(&crypto_label, 0, 0, 1, 1);
        meta_grid.attach(&crypto_chip, 1, 0, 1, 1);

        let normal_headers_grid = gtk::Grid::builder()
            .row_spacing(0)
            .column_spacing(6)
            .hexpand(true)
            .css_classes(vec!["mail-normal-headers"])
            .build();
        self.imp()
            .normal_headers_grid
            .replace(Some(normal_headers_grid.clone()));

        let all_headers_grid = gtk::Grid::builder()
            .row_spacing(3)
            .column_spacing(8)
            .margin_start(6)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["mail-all-headers"])
            .build();
        let all_headers_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(72)
            .max_content_height(200)
            .propagate_natural_height(true)
            .child(&all_headers_grid)
            .build();
        let all_headers_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(120)
            .reveal_child(false)
            .child(&all_headers_scroll)
            .build();

        let action_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["mail-detail-actions"])
            .spacing(2)
            .margin_top(2)
            .build();

        let reply_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let reply_icon = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
        let reply_label = gtk::Label::new(Some("Antworten"));
        reply_box.append(&reply_icon);
        reply_box.append(&reply_label);
        let reply_btn = gtk::Button::builder()
            .child(&reply_box)
            .tooltip_text("Antworten (Strg+R)")
            .css_classes(vec!["flat"])
            .build();

        let reply_all_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let reply_all_icon = gtk::Image::from_icon_name("mail-reply-all-symbolic");
        let reply_all_label = gtk::Label::new(Some("Allen antworten"));
        let reply_all_arrow = gtk::Image::from_icon_name("pan-down-symbolic");
        reply_all_box.append(&reply_all_icon);
        reply_all_box.append(&reply_all_label);
        reply_all_box.append(&reply_all_arrow);
        let reply_all_btn = gtk::Button::builder()
            .child(&reply_all_box)
            .tooltip_text("Allen antworten (Strg+Umschalt+R)")
            .css_classes(vec!["flat"])
            .build();

        let forward_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let forward_icon = gtk::Image::from_icon_name("mail-forward-symbolic");
        let forward_label = gtk::Label::new(Some("Weiterleiten"));
        let forward_arrow = gtk::Image::from_icon_name("pan-down-symbolic");
        forward_box.append(&forward_icon);
        forward_box.append(&forward_label);
        forward_box.append(&forward_arrow);
        let forward_btn = gtk::Button::builder()
            .child(&forward_box)
            .tooltip_text("Weiterleiten (Strg+L)")
            .css_classes(vec!["flat"])
            .build();

        let export_btn = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("E-Mail als EML exportieren")
            .css_classes(vec!["flat"])
            .build();

        let print_btn = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text("E-Mail drucken (Ctrl+P)")
            .action_name("win.mail-print")
            .css_classes(vec!["flat"])
            .build();

        let print_preview_btn = gtk::Button::builder()
            .icon_name("document-print-preview-symbolic")
            .tooltip_text("Druckvorschau anzeigen")
            .action_name("win.mail-print-preview")
            .css_classes(vec!["flat"])
            .build();

        let label_btn = gtk::Button::builder()
            .icon_name("bookmark-new-symbolic")
            .tooltip_text("Label setzen oder entfernen")
            .css_classes(vec!["flat"])
            .build();

        let delete_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Löschen (Entf)")
            .action_name("win.delete")
            .css_classes(vec!["flat"])
            .build();

        let unread_btn = gtk::Button::builder()
            .icon_name("mail-unread-symbolic")
            .tooltip_text("Als ungelesen markieren")
            .action_name("win.mark-unread")
            .css_classes(vec!["flat"])
            .build();

        let read_btn = gtk::Button::builder()
            .icon_name("mail-read-symbolic")
            .tooltip_text("Als gelesen markieren")
            .action_name("win.mark-read")
            .css_classes(vec!["flat"])
            .build();

        let flag_btn = gtk::Button::builder()
            .icon_name("starred-symbolic")
            .tooltip_text("Markierung umschalten")
            .css_classes(vec!["flat"])
            .build();

        let important_btn = gtk::Button::builder()
            .icon_name("mail-mark-important-symbolic")
            .tooltip_text("Wichtig-Markierung umschalten")
            .css_classes(vec!["flat"])
            .build();

        let archive_btn = gtk::Button::builder()
            .icon_name("folder-symbolic")
            .tooltip_text("Archivieren")
            .css_classes(vec!["flat"])
            .build();

        let junk_btn = gtk::Button::builder()
            .icon_name("mail-mark-junk-symbolic")
            .tooltip_text("Als Spam markieren")
            .css_classes(vec!["flat"])
            .build();

        let not_junk_btn = gtk::Button::builder()
            .icon_name("mail-mark-notjunk-symbolic")
            .tooltip_text("Als Nicht-Spam markieren")
            .css_classes(vec!["flat"])
            .build();

        let prev_message_btn = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Vorherige Nachricht in der Liste (k)")
            .css_classes(vec!["flat"])
            .build();

        let next_message_btn = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Nächste Nachricht in der Liste (j)")
            .css_classes(vec!["flat"])
            .build();

        let page_for_prev = self.downgrade();
        prev_message_btn.connect_clicked(move |_| {
            if let Some(page) = page_for_prev.upgrade() {
                page.select_previous_message();
            }
        });

        let page_for_next = self.downgrade();
        next_message_btn.connect_clicked(move |_| {
            if let Some(page) = page_for_next.upgrade() {
                page.select_next_message();
            }
        });

        for (button, preferred, fallback) in [
            (
                &reply_btn,
                "mail-reply-sender-symbolic",
                "go-previous-symbolic",
            ),
            (
                &reply_all_btn,
                "mail-reply-all-symbolic",
                "go-previous-symbolic",
            ),
            (&forward_btn, "mail-forward-symbolic", "go-next-symbolic"),
            (
                &print_btn,
                "document-print-symbolic",
                "document-save-symbolic",
            ),
            (
                &print_preview_btn,
                "document-print-preview-symbolic",
                "document-print-symbolic",
            ),
            (
                &unread_btn,
                "mail-unread-symbolic",
                "mail-message-new-symbolic",
            ),
            (&read_btn, "mail-read-symbolic", "mail-message-new-symbolic"),
            (&flag_btn, "starred-symbolic", "bookmark-new-symbolic"),
            (
                &important_btn,
                "mail-mark-important-symbolic",
                "dialog-warning-symbolic",
            ),
            (&archive_btn, "folder-symbolic", "folder-symbolic"),
            (
                &junk_btn,
                "mail-mark-junk-symbolic",
                "dialog-warning-symbolic",
            ),
            (
                &not_junk_btn,
                "mail-mark-notjunk-symbolic",
                "folder-symbolic",
            ),
        ] {
            set_button_icon_with_fallback(button, preferred, fallback);
        }

        for (button, label) in [
            (&reply_btn, "Auf Nachricht antworten"),
            (&reply_all_btn, "Allen Empfängern antworten"),
            (&forward_btn, "Nachricht weiterleiten"),
            (&export_btn, "Nachricht als EML exportieren"),
            (&print_btn, "Nachricht drucken"),
            (&print_preview_btn, "Druckvorschau der Nachricht anzeigen"),
            (&label_btn, "Label bearbeiten"),
            (&delete_btn, "Nachricht löschen"),
            (&unread_btn, "Nachricht als ungelesen markieren"),
            (&read_btn, "Nachricht als gelesen markieren"),
            (&flag_btn, "Markierung umschalten"),
            (&important_btn, "Wichtigkeitsmarkierung umschalten"),
            (&archive_btn, "Nachricht archivieren"),
            (&junk_btn, "Nachricht als Spam markieren"),
            (&not_junk_btn, "Nachricht als Nicht-Spam markieren"),
        ] {
            set_accessible_label(button, label);
        }

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

        label_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.present_label_editor()
        ));

        flag_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| match page.toggle_selected_flagged() {
                Ok(true) => {}
                Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                Err(error) => page.show_toast(&format!("Markierung nicht möglich: {error}")),
            }
        ));

        important_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| match page.toggle_selected_important() {
                Ok(true) => {}
                Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                Err(error) =>
                    page.show_toast(&format!("Wichtig-Markierung nicht möglich: {error}")),
            }
        ));

        archive_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| match page.archive_selected() {
                Ok(true) => {}
                Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                Err(error) => page.show_toast(&format!("Archivieren nicht möglich: {error}")),
            }
        ));

        junk_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| match page.mark_selected_junk() {
                Ok(true) => {}
                Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                Err(error) => page.show_toast(&format!("Spam-Markierung nicht möglich: {error}")),
            }
        ));

        not_junk_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| match page.mark_selected_not_junk() {
                Ok(true) => {}
                Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                Err(error) =>
                    page.show_toast(&format!("Nicht-Spam-Markierung nicht möglich: {error}")),
            }
        ));

        let action_spacer = gtk::Box::builder().hexpand(true).build();
        action_bar.append(&action_spacer);
        action_bar.append(&reply_btn);
        action_bar.append(&reply_all_btn);
        action_bar.append(&forward_btn);
        action_bar.append(&export_btn);
        action_bar.append(&print_btn);
        action_bar.append(&print_preview_btn);

        let move_popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let move_popover = gtk::Popover::builder().child(&move_popover_content).build();
        let page_for_single_move = self.downgrade();
        let move_popover_weak = move_popover.downgrade();
        move_popover.connect_show(move |_| {
            while let Some(child) = move_popover_content.first_child() {
                move_popover_content.remove(&child);
            }
            let Some(page) = page_for_single_move.upgrade() else {
                return;
            };
            let Some(message) = page.selected_message() else {
                move_popover_content.append(
                    &gtk::Label::builder()
                        .label("Keine Nachricht ausgewählt")
                        .css_classes(vec!["caption", "dim-label"])
                        .build(),
                );
                return;
            };
            let destinations =
                page.folder_destinations_for_messages(std::slice::from_ref(&message));
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
                let destination_source = destination.source_uid.clone();
                let destination_folder = destination.folder_name.clone();
                button.connect_clicked(move |_| {
                    if let Some(page) = page_for_destination.upgrade() {
                        match page.move_selected_to(&destination_source, &destination_folder) {
                            Ok(true) => {}
                            Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                            Err(error) => {
                                page.show_toast(&format!("Verschieben nicht möglich: {error}"))
                            }
                        }
                    }
                    if let Some(popover) = popover_for_destination.upgrade() {
                        popover.popdown();
                    }
                });
                move_popover_content.append(&button);
            }
        });
        let move_button = gtk::MenuButton::builder()
            .icon_name("folder-symbolic")
            .tooltip_text("Nachricht verschieben")
            .popover(&move_popover)
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&move_button, "Nachricht in einen Ordner verschieben");

        let copy_popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let copy_popover = gtk::Popover::builder().child(&copy_popover_content).build();
        let page_for_single_copy = self.downgrade();
        let copy_popover_weak = copy_popover.downgrade();
        copy_popover.connect_show(move |_| {
            while let Some(child) = copy_popover_content.first_child() {
                copy_popover_content.remove(&child);
            }
            let Some(page) = page_for_single_copy.upgrade() else {
                return;
            };
            let Some(message) = page.selected_message() else {
                copy_popover_content.append(
                    &gtk::Label::builder()
                        .label("Keine Nachricht ausgewählt")
                        .css_classes(vec!["caption", "dim-label"])
                        .build(),
                );
                return;
            };
            let destinations =
                page.copy_folder_destinations_for_messages(std::slice::from_ref(&message));
            if destinations.is_empty() {
                copy_popover_content.append(
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
                let popover_for_destination = copy_popover_weak.clone();
                let destination_source = destination.source_uid.clone();
                let destination_folder = destination.folder_name.clone();
                button.connect_clicked(move |_| {
                    if let Some(page) = page_for_destination.upgrade() {
                        match page.copy_selected_to(&destination_source, &destination_folder) {
                            Ok(true) => {}
                            Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                            Err(error) => {
                                page.show_toast(&format!("Kopieren nicht möglich: {error}"))
                            }
                        }
                    }
                    if let Some(popover) = popover_for_destination.upgrade() {
                        popover.popdown();
                    }
                });
                copy_popover_content.append(&button);
            }
        });
        let copy_button = gtk::MenuButton::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Nachricht kopieren")
            .popover(&copy_popover)
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&copy_button, "Nachricht in einen weiteren Ordner kopieren");

        header_box.append(&action_bar);
        header_box.append(&normal_headers_grid);
        header_box.append(&meta_grid);
        header_box.append(&all_headers_revealer);

        let header_separator = gtk::Separator::new(gtk::Orientation::Horizontal);

        let thread_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["thread-context"])
            .build();
        let thread_context = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .min_content_height(0)
            .max_content_height(36)
            .height_request(36)
            .vexpand(false)
            .valign(gtk::Align::Start)
            .child(&thread_list)
            .margin_start(8)
            .margin_end(8)
            .margin_top(1)
            .margin_bottom(1)
            .visible(false)
            .build();

        let mail_viewer = MailViewer::new();

        let print_status_label = gtk::Label::builder()
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let print_cancel_button = gtk::Button::builder()
            .label("Abbrechen")
            .tooltip_text("Druckvorbereitung abbrechen")
            .action_name("win.mail-print-cancel")
            .css_classes(vec!["flat"])
            .visible(false)
            .build();
        set_accessible_label(&print_cancel_button, "Druckvorgang abbrechen");
        let print_status_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_top(3)
            .margin_bottom(3)
            .visible(false)
            .build();
        print_status_area.append(&print_status_label);
        print_status_area.append(&print_cancel_button);
        self.imp()
            .print_status_area
            .replace(Some(print_status_area.clone()));
        self.imp()
            .print_status_label
            .replace(Some(print_status_label.clone()));
        self.imp()
            .print_cancel_button
            .replace(Some(print_cancel_button.clone()));

        let message_find_entry = gtk::SearchEntry::builder()
            .placeholder_text("In Nachricht suchen")
            .hexpand(true)
            .build();
        let message_find_status = gtk::Label::builder()
            .css_classes(vec!["caption", "dim-label"])
            .width_chars(14)
            .xalign(0.0)
            .build();
        let find_previous = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Vorheriger Treffer")
            .css_classes(vec!["flat"])
            .build();
        let find_next = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Nächster Treffer")
            .css_classes(vec!["flat"])
            .build();
        let find_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Suche schließen")
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&find_previous, "Vorheriger Treffer");
        set_accessible_label(&find_next, "Nächster Treffer");
        set_accessible_label(&find_close, "Suche in Nachricht schließen");
        find_previous.set_sensitive(false);
        find_next.set_sensitive(false);
        let message_find_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(3)
            .margin_bottom(3)
            .build();
        message_find_bar.append(&message_find_entry);
        message_find_bar.append(&message_find_status);
        message_find_bar.append(&find_previous);
        message_find_bar.append(&find_next);
        message_find_bar.append(&find_close);
        let message_find_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(100)
            .reveal_child(false)
            .child(&message_find_bar)
            .build();

        let viewer_for_find = mail_viewer.clone();
        let status_for_find = message_find_status.clone();
        let previous_for_find = find_previous.clone();
        let next_for_find = find_next.clone();
        message_find_entry.connect_search_changed(move |entry| {
            let result = viewer_for_find.search_displayed_message(entry.text().as_str());
            update_displayed_message_search_status(
                &status_for_find,
                &previous_for_find,
                &next_for_find,
                &result,
            );
            if result.status != MessageSearchStatus::Pending {
                return;
            }
            let viewer = viewer_for_find.downgrade();
            let status = status_for_find.downgrade();
            let previous = previous_for_find.downgrade();
            let next = next_for_find.downgrade();
            let generation = result.generation;
            let attempts = Rc::new(Cell::new(0u16));
            glib::timeout_add_local(StdDuration::from_millis(40), move || {
                let (Some(viewer), Some(status), Some(previous), Some(next)) = (
                    viewer.upgrade(),
                    status.upgrade(),
                    previous.upgrade(),
                    next.upgrade(),
                ) else {
                    return glib::ControlFlow::Break;
                };
                let attempt = attempts.get().saturating_add(1);
                attempts.set(attempt);
                if attempt >= 250 {
                    status.set_label("Suche nicht verfügbar");
                    previous.set_sensitive(false);
                    next.set_sensitive(false);
                    return glib::ControlFlow::Break;
                }
                let Some(result) = viewer.displayed_message_search_result() else {
                    return glib::ControlFlow::Break;
                };
                if result.generation != generation {
                    return glib::ControlFlow::Break;
                }
                update_displayed_message_search_status(&status, &previous, &next, &result);
                if result.status == MessageSearchStatus::Pending {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
        });
        let viewer_for_previous = mail_viewer.clone();
        let status_for_previous = message_find_status.clone();
        let previous_for_previous = find_previous.clone();
        let next_for_previous = find_next.clone();
        find_previous.connect_clicked(move |_| {
            let result = viewer_for_previous.search_displayed_message_previous();
            update_displayed_message_search_status(
                &status_for_previous,
                &previous_for_previous,
                &next_for_previous,
                &result,
            );
        });
        let viewer_for_next = mail_viewer.clone();
        let status_for_next = message_find_status.clone();
        let previous_for_next = find_previous.clone();
        let next_for_next = find_next.clone();
        find_next.connect_clicked(move |_| {
            let result = viewer_for_next.search_displayed_message_next();
            update_displayed_message_search_status(
                &status_for_next,
                &previous_for_next,
                &next_for_next,
                &result,
            );
        });
        let viewer_for_close = mail_viewer.clone();
        let entry_for_close = message_find_entry.clone();
        let revealer_for_close = message_find_revealer.clone();
        find_close.connect_clicked(move |_| {
            viewer_for_close.finish_displayed_message_search();
            entry_for_close.set_text("");
            revealer_for_close.set_reveal_child(false);
        });
        let escape_controller = gtk::EventControllerKey::new();
        let viewer_for_escape = mail_viewer.clone();
        let entry_for_escape = message_find_entry.clone();
        let revealer_for_escape = message_find_revealer.clone();
        escape_controller.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            viewer_for_escape.finish_displayed_message_search();
            entry_for_escape.set_text("");
            revealer_for_escape.set_reveal_child(false);
            glib::Propagation::Stop
        });
        message_find_entry.add_controller(escape_controller);

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

        let page_for_trash_restore = self.downgrade();
        mail_viewer.connect_trash_restore(move || {
            if let Some(page) = page_for_trash_restore.upgrade() {
                if let Some(message) = page.selected_message() {
                    let source_uid = message
                        .backend_location()
                        .map(|loc| loc.source_uid)
                        .unwrap_or_default();
                    match page.move_selected_to(&source_uid, "inbox") {
                        Ok(true) => {
                            page.show_toast("Nachricht in den Posteingang wiederhergestellt");
                            if let Some(viewer) = page.imp().mail_viewer.borrow().as_ref() {
                                viewer.set_trash_banner_visible(false);
                            }
                        }
                        Ok(false) => {}
                        Err(err) => {
                            page.show_toast(&format!("Wiederherstellen fehlgeschlagen: {err}"))
                        }
                    }
                }
            }
        });

        let page_for_junk_restore = self.downgrade();
        mail_viewer.connect_junk_restore(move || {
            if let Some(page) = page_for_junk_restore.upgrade() {
                match page.mark_selected_not_junk() {
                    Ok(true) => {
                        page.show_toast("Nachricht als kein Spam markiert");
                        if let Some(viewer) = page.imp().mail_viewer.borrow().as_ref() {
                            viewer.set_junk_banner_visible(false);
                        }
                    }
                    Ok(false) => {}
                    Err(err) => page.show_toast(&format!("Aktion fehlgeschlagen: {err}")),
                }
            }
        });

        detail_view.append(&header_box);
        detail_view.append(&header_separator);
        detail_view.append(&thread_context);
        detail_view.append(&message_find_revealer);
        detail_view.append(&mail_viewer);
        detail_view.append(&attachment_separator);
        detail_view.append(&attachment_bar);
        detail_view.append(&print_status_area);

        (
            detail_view,
            mail_viewer,
            crypto_label,
            lbl_crypto,
            crypto_chip,
            attachment_bar,
            thread_context,
            all_headers_revealer,
            all_headers_grid,
            message_find_revealer,
            message_find_entry,
        )
    }

    // ── Private: Connect Selection Changed Handler ──────────────────────

    #[allow(clippy::too_many_arguments)]
    fn connect_selection_handler(
        &self,
        selection: &gtk::SingleSelection,
        store: &gio::ListStore,
        _custom_filter: &gtk::CustomFilter,
        _thread_filter: &gtk::CustomFilter,
        mail_viewer: &MailViewer,
        lbl_crypto: &gtk::Label,
        attachment_bar: &gtk::Box,
        thread_context: &gtk::ScrolledWindow,
    ) {
        let selection_for_async_crypto = selection.clone();
        let store_for_thread_context = store.clone();
        let selection_for_thread_context = selection.clone();
        let page_for_read = self.downgrade();
        selection.connect_selected_item_notify(glib::clone!(
            #[strong]
            selection_for_async_crypto,
            #[strong]
            page_for_read,
            #[weak]
            mail_viewer,
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
                let crypto_request_generation = page_for_read
                    .upgrade()
                    .map(|page| page.begin_crypto_request())
                    .unwrap_or_default();
                if let Some(page) = page_for_read.upgrade() {
                    page.reset_displayed_message_search_for_selection();
                }
                if let Some(source) = page_for_read.upgrade().and_then(|p| p.imp().mark_read_timeout_source.borrow_mut().take()) {
                    source.remove();
                }
                let msg_option = selected_mail_message(sel);
                if let Some(msg) = msg_option {
                    let was_unread = msg.is_unread();
                    let settings = AppSettings::try_new();
                    let mark_read_on_selection = settings
                        .as_ref()
                        .map(|s| s.mark_read_on_selection())
                        .unwrap_or(true);
                    let timeout_ms = settings
                        .as_ref()
                        .map(|s| s.mark_read_timeout())
                        .unwrap_or(1500);

                    if was_unread && mark_read_on_selection {
                        let msg_id = msg.id().to_string();
                        let backend_location = msg.backend_location();
                        let is_flagged = msg.is_flagged();

                        if timeout_ms <= 0 {
                            let command = MailCommand::set_message_state(
                                &msg_id,
                                backend_location,
                                true,
                                is_flagged,
                                false,
                                is_flagged,
                            );
                            if let Some(page) = page_for_read.upgrade() {
                                if let Err(error) = page.execute_mail_command(command) {
                                    tracing::error!(%error, "Could not queue read-state mutation");
                                }
                            }
                        } else if let Some(page) = page_for_read.upgrade() {
                            let page_weak = page.downgrade();
                            let timeout_source = glib::timeout_add_local_once(
                                std::time::Duration::from_millis(timeout_ms as u64),
                                move || {
                                    if let Some(page) = page_weak.upgrade() {
                                        page.imp().mark_read_timeout_source.replace(None);
                                        if let Some(sel) = page.imp().message_selection.borrow().as_ref() {
                                            if let Some(current_msg) = selected_mail_message(sel) {
                                                if current_msg.id() == msg_id && current_msg.is_unread() {
                                                    let command = MailCommand::set_message_state(
                                                        &msg_id,
                                                        backend_location,
                                                        true,
                                                        is_flagged,
                                                        false,
                                                        is_flagged,
                                                    );
                                                    if let Err(error) = page.execute_mail_command(command) {
                                                        tracing::error!(%error, "Could not queue delayed read-state mutation");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                },
                            );
                            page.imp().mark_read_timeout_source.replace(Some(timeout_source));
                        }
                    }
                    if let Some(page) = page_for_read.upgrade() {
                        page.refresh_normal_headers_for_message(Some(&msg));
                        page.refresh_all_headers_for_message(Some(&msg));
                    }

                    Self::render_thread_context(
                        &store_for_thread_context,
                        &selection_for_thread_context,
                        &msg,
                        &thread_context,
                    );

                    let raw_mime = msg.raw_mime();
                    let has_crypto_envelope =
                        raw_mime.as_deref().is_some_and(raw_mime_may_contain_crypto);
                    let message_identity = msg.message_identity();
                    if let Some(page) = page_for_read.upgrade() {
                        page.set_crypto_status_routine(!has_crypto_envelope);
                    }
                    lbl_crypto.set_label(if has_crypto_envelope {
                        "Kryptografie: Prüfung läuft …"
                    } else {
                        "Keine OpenPGP-/S/MIME-Sicherung erkannt"
                    });
                    if let Some(raw_mime) = raw_mime.filter(|_| has_crypto_envelope) {
                        let message_id = msg.id();
                        let folder_id = msg.folder_id();
                        let unread = msg.is_unread();
                        let selection_for_crypto = selection_for_async_crypto.clone();
                        let label_for_crypto = lbl_crypto.downgrade();
                        let page_for_crypto = page_for_read.clone();
                        let viewer_for_crypto = mail_viewer.clone();
                        let attachment_bar_for_crypto = attachment_bar.clone();
                        let (crypto_sender, crypto_receiver) = async_channel::bounded(1);
                        let message_id_for_parse = message_id.clone();
                        let message_identity_for_worker = message_identity.clone();
                        let smime_options = AppSettings::try_new()
                            .map(|settings| settings.smime_options(false, false))
                            .unwrap_or_default();
                        std::thread::spawn(move || {
                            let result = crypto_service::inspect_crypto_mime(
                                &raw_mime,
                                &smime_options,
                            )
                            .map(|inspection| {
                                let decrypted = inspection.decrypted_mime.as_deref().and_then(
                                    |decrypted_mime| {
                                        parse_rfc822_bytes(
                                            &message_id_for_parse,
                                            &folder_id,
                                            decrypted_mime,
                                            unread,
                                        )
                                    },
                                );
                                let searchable_body_text = decrypted
                                    .as_ref()
                                    .map(|parsed| prepare_searchable_body_text(&parsed.html_body));
                                (inspection, decrypted, searchable_body_text)
                            });
                            if let Err(error) = crypto_sender.send_blocking((
                                crypto_request_generation,
                                message_identity_for_worker,
                                result,
                            )) {
                                tracing::debug!(%error, "OpenPGP inspection result was dropped");
                            }
                        });
                        glib::spawn_future_local(async move {
                            let Ok((result_generation, result_identity, inspection)) =
                                crypto_receiver.recv().await
                            else {
                                return;
                            };
                            let Some(page) = page_for_crypto.upgrade() else {
                                return;
                            };
                            let Some(current) = selected_mail_message(&selection_for_crypto) else {
                                return;
                            };
                            if !crypto_result_is_current(
                                page.imp().crypto_request_generation.get(),
                                result_generation,
                                &current.message_identity(),
                                &result_identity,
                            ) {
                                return;
                            }
                            let Some(label) = label_for_crypto.upgrade() else {
                                return;
                            };
                            match inspection {
                                Ok((inspection, decrypted, searchable_body_text)) => {
                                    let label_text = inspection.details.display_text();
                                    let crypto_visible =
                                        crypto_details_are_visible(&inspection.details);
                                    if let Some(parsed) = decrypted {
                                        current.replace_rendered_content(
                                            parsed.html_body,
                                            parsed.preview_snippet,
                                            parsed.attachments,
                                            searchable_body_text,
                                        );
                                        page.invalidate_message_filters();
                                        Self::render_message_content(
                                            &current,
                                            &viewer_for_crypto,
                                            &attachment_bar_for_crypto,
                                        );
                                    }
                                    label.set_label(&label_text);
                                    page.set_crypto_status_routine(!crypto_visible);
                                }
                                Err(error) => {
                                    label.set_label(&format!(
                                        "Kryptografie-Prüfung fehlgeschlagen: {error}"
                                    ));
                                    page.set_crypto_status_routine(false);
                                }
                            }
                        });
                    }
                    Self::render_message_content(&msg, &mail_viewer, &attachment_bar);
                } else {
                    lbl_crypto.set_label("");
                    thread_context.set_visible(false);
                    while let Some(child) = attachment_bar.first_child() {
                        attachment_bar.remove(&child);
                    }
                    attachment_bar.set_visible(false);
                    mail_viewer.set_trash_banner_visible(false);
                    mail_viewer.set_junk_banner_visible(false);
                    mail_viewer.load_html_for_message(None, "", None);
                    if let Some(page) = page_for_read.upgrade() {
                        page.set_crypto_status_routine(true);
                        page.refresh_normal_headers_for_message(None);
                        page.refresh_all_headers_for_message(None);
                    }
                }
            }
        ));

        if store.n_items() > 0 {
            selection.set_selected(0);
        }
    }

    fn render_message_content(
        message: &MailMessageObject,
        mail_viewer: &MailViewer,
        attachment_bar: &gtk::Box,
    ) {
        while let Some(child) = attachment_bar.first_child() {
            attachment_bar.remove(&child);
        }

        let attachments = message.attachments();
        for attachment in &attachments {
            let looks_like_calendar = attachment
                .mime_type
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/calendar"))
                || attachment.filename.to_ascii_lowercase().ends_with(".ics");
            if looks_like_calendar
                && let Some(invitation) =
                    parse_invitation(&String::from_utf8_lossy(&attachment.content))
            {
                append_itip_invitation(attachment_bar, invitation);
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
        let html_body = message.html_body();
        let preview = message.preview_snippet();
        mail_viewer.load_message_content_with_fallback(
            Some(&message.id()),
            &html_body,
            &inline_images,
            None,
            Some(&preview),
        );
        let is_trash = message_is_trash(message);
        let is_junk = message_is_junk(message);
        mail_viewer.set_trash_banner_visible(is_trash);
        mail_viewer.set_junk_banner_visible(!is_trash && is_junk);
        if attachments.is_empty() {
            attachment_bar.set_visible(false);
        } else {
            attachment_bar.set_visible(true);
            if attachments.len() > 1 {
                let btn_save_all = gtk::Button::builder()
                    .label("Alle speichern…")
                    .icon_name("document-save-symbolic")
                    .css_classes(vec!["flat", "suggested-action"])
                    .valign(gtk::Align::Center)
                    .build();
                let attachments_for_all = attachments.clone();
                let parent = attachment_bar.root().and_downcast::<gtk::Window>();
                btn_save_all.connect_clicked(move |_| {
                    let Some(parent) = parent.as_ref() else {
                        return;
                    };
                    let folder_dialog = gtk::FileDialog::builder()
                        .title("Ordner für alle Anhänge auswählen")
                        .modal(true)
                        .build();
                    let attachments_to_save = attachments_for_all.clone();
                    folder_dialog.select_folder(Some(parent), None::<&gio::Cancellable>, move |result| {
                        if let Ok(folder) = result {
                            if let Some(folder_path) = folder.path() {
                                for att in &attachments_to_save {
                                    let target_path = folder_path.join(&att.filename);
                                    let _ = fs::write(&target_path, att.content.as_ref());
                                }
                            }
                        }
                    });
                });
                attachment_bar.append(&btn_save_all);
            }

            for attachment in attachments {
                let chip = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .css_classes(vec!["card", "attachment-chip"])
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();

                let icon_name = match attachment.mime_type.as_str() {
                    m if m.starts_with("image/") => "image-x-generic-symbolic",
                    m if m.contains("pdf") => "x-office-document-symbolic",
                    m if m.contains("zip") || m.contains("tar") || m.contains("compressed") => {
                        "package-x-generic-symbolic"
                    }
                    m if m.starts_with("audio/") => "audio-x-generic-symbolic",
                    m if m.starts_with("video/") => "video-x-generic-symbolic",
                    m if m.contains("text/") => "text-x-generic-symbolic",
                    _ => "mail-attachment-symbolic",
                };
                let icon = gtk::Image::new();
                set_image_icon_with_fallback(&icon, icon_name, "mail-attachment-symbolic");
                let text = gtk::Label::builder()
                    .label(&format!(
                        "{} ({})",
                        attachment.filename, attachment.size_formatted
                    ))
                    .css_classes(vec!["caption"])
                    .build();

                let btn_open = gtk::Button::builder()
                    .icon_name("document-open-symbolic")
                    .tooltip_text("Anhang öffnen")
                    .css_classes(vec!["flat", "circular"])
                    .valign(gtk::Align::Center)
                    .build();
                let filename_for_open = attachment.filename.clone();
                let content_for_open = attachment.content.clone();
                btn_open.connect_clicked(move |_| {
                    let temp_dir = std::env::temp_dir().join("evolution4-attachments");
                    let _ = fs::create_dir_all(&temp_dir);
                    let temp_path = temp_dir.join(&filename_for_open);
                    if fs::write(&temp_path, content_for_open.as_ref()).is_ok() {
                        let uri = format!("file://{}", temp_path.to_string_lossy());
                        let _ = gio::AppInfo::launch_default_for_uri(
                            &uri,
                            None::<&gio::AppLaunchContext>,
                        );
                    }
                });

                let btn_save = gtk::Button::builder()
                    .icon_name("document-save-symbolic")
                    .tooltip_text("Anhang speichern unter…")
                    .css_classes(vec!["flat", "circular"])
                    .valign(gtk::Align::Center)
                    .build();
                let filename = attachment.filename.clone();
                let content = attachment.content.clone();
                let parent = attachment_bar.root().and_downcast::<gtk::Window>();
                btn_save.connect_clicked(move |_| {
                    let Some(parent) = parent.as_ref() else {
                        tracing::warn!("Cannot save attachment without a parent window");
                        return;
                    };

                    let file_dialog = gtk::FileDialog::builder()
                        .title("Anhang speichern")
                        .modal(true)
                        .initial_name(&filename)
                        .build();
                    let filename = filename.clone();
                    let content = content.clone();
                    file_dialog.save(Some(parent), None::<&gio::Cancellable>, move |result| {
                        match result {
                            Ok(file) => {
                                let Some(path) = file.path() else {
                                    tracing::warn!("Cannot save attachment to a non-local URI");
                                    return;
                                };
                                if let Err(error) = fs::write(&path, content.as_ref()) {
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
                        }
                    });
                });

                chip.append(&icon);
                chip.append(&text);
                chip.append(&btn_open);
                chip.append(&btn_save);
                attachment_bar.append(&chip);
            }
        }
    }

    fn render_thread_context(
        store: &gio::ListStore,
        selection: &gtk::SingleSelection,
        selected: &MailMessageObject,
        thread_context: &gtk::ScrolledWindow,
    ) {
        let Some(thread_list) = thread_context.child().and_downcast::<gtk::ListBox>() else {
            thread_context.set_visible(false);
            return;
        };
        while let Some(child) = thread_list.first_child() {
            thread_list.remove(&child);
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
            .margin_start(4)
            .margin_bottom(2)
            .build();
        let header_row = gtk::ListBoxRow::builder()
            .child(&header)
            .activatable(false)
            .build();
        thread_list.append(&header_row);

        let message_count = messages.len();
        const MAX_VISIBLE_THREAD_MESSAGES: usize = 1;
        let selected_index = messages
            .iter()
            .position(|message| message.id() == selected.id())
            .unwrap_or(0);
        let mut visible_indices = if selected_index == 0 {
            (0..messages.len().min(MAX_VISIBLE_THREAD_MESSAGES)).collect::<Vec<_>>()
        } else {
            vec![0, selected_index]
        };
        visible_indices.sort_unstable();
        visible_indices.dedup();
        for index in &visible_indices {
            let message = &messages[*index];
            let label = gtk::Label::builder()
                .label(&format!(
                    "{} · {} · {}",
                    message.sender_name(),
                    format_message_date(&message.date_str()),
                    message.subject()
                ))
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(1)
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
            thread_list.append(&row);
        }
        if visible_indices.len() < messages.len() {
            let remaining = messages.len() - visible_indices.len();
            let more = gtk::Label::builder()
                .label(&format!("… {remaining} weitere Nachrichten"))
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::Start)
                .margin_start(4)
                .build();
            thread_list.append(&gtk::ListBoxRow::builder().child(&more).build());
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
        sidebar_child.set_width_request(240);
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

        imp.inner_paned.set_vexpand(true);
        imp.inner_paned.set_hexpand(true);
        imp.outer_paned.set_end_child(Some(&imp.inner_paned));
        imp.outer_paned.set_resize_end_child(true);
        imp.outer_paned.set_shrink_end_child(false);
        let sidebar_visible = AppSettings::try_new()
            .map(|settings| settings.mail_show_sidebar())
            .unwrap_or(true);
        imp.outer_paned
            .set_position(if sidebar_visible { 240 } else { 0 });
        if let Some(settings) = AppSettings::try_new() {
            imp.outer_paned.connect_position_notify(move |paned| {
                let position = paned.position();
                if position > 10 {
                    settings.set_mail_sidebar_position(position);
                }
            });
        }

        imp.outer_paned.set_vexpand(true);
        imp.outer_paned.set_hexpand(true);
        imp.root_box.set_orientation(gtk::Orientation::Vertical);
        imp.root_box.set_vexpand(true);
        imp.root_box.set_hexpand(true);
        if let Some(toolbar) = imp.classic_toolbar.borrow().as_ref().cloned() {
            imp.root_box.append(&toolbar);
        }
        imp.root_box.append(&imp.outer_paned);
        imp.root_box.set_parent(&*self.imp().obj());
    }

    fn load_initial_data(&self, source_store: &gio::ListStore, source_state: &gtk::Label) {
        let startup_started = Instant::now();
        tracing::debug!("MailPage initial data setup started");
        let initial_folder = FolderSelection::configured_initial();
        if let Some(selection) = self.imp().folder_selection.borrow().as_ref() {
            selection.replace(initial_folder.clone());
        }
        self.select_folder_row(&initial_folder);

        // Let GTK present the already-built shell before the first backend
        // result can schedule model work. EDS/Camel runs in a worker, but the
        // resulting GObjects and list-model changes must be applied on the
        // GTK thread. A short delay prevents a cold Linux EDS session from
        // making the freshly presented window appear unresponsive.
        let page = self.downgrade();
        let startup_source =
            glib::timeout_add_local_once(StdDuration::from_millis(100), move || {
                let Some(page) = page.upgrade() else {
                    return;
                };
                page.imp().mail_startup_source.borrow_mut().take();
                page.start_mail_load(MailLoadRequest::initial(), true);
                page.setup_mail_polling();
            });
        self.imp().mail_startup_source.replace(Some(startup_source));
        tracing::debug!(
            elapsed_ms = startup_started.elapsed().as_millis() as u64,
            "MailPage initial mail load deferred until after first frame"
        );

        let source_store_for_load = source_store.clone();
        let source_selection_for_load = self.imp().source_selection.borrow().clone();
        let source_state_for_load = source_state.clone();
        let offline_mode = AppSettings::try_new()
            .map(|settings| settings.offline_mode())
            .unwrap_or(false);
        let configured_accounts = AccountConfigStore::default().load();
        let preferred_account_id = AppSettings::try_new()
            .map(|settings| settings.default_account_id())
            .filter(|id| !id.trim().is_empty());
        for profile in &configured_accounts {
            source_store.append(&EdsSourceObject::from_account_profile(profile));
        }
        let has_configured_accounts = !configured_accounts.is_empty();
        let (discovery_sender, discovery_receiver) = async_channel::bounded(1);
        if offline_mode {
            let discovery = EdsSourceDiscovery {
                sources: EdsSourceCache::new().load(EdsSourceKind::Mail),
                origin: EdsSourceOrigin::Cache,
                error: Some("Offline-Modus aktiviert".to_string()),
            };
            let _ = discovery_sender.send_blocking(discovery);
        } else {
            std::thread::spawn(move || {
                let discovery_started = Instant::now();
                let discovery = eds_service::discover_sources_blocking(EdsSourceKind::Mail);
                tracing::info!(
                    sources = discovery.sources.len(),
                    origin = ?discovery.origin,
                    elapsed_ms = discovery_started.elapsed().as_millis() as u64,
                    "Loaded mail sources"
                );
                if let Some(error) = discovery.error.as_deref() {
                    tracing::warn!(%error, "Live EDS mail source discovery failed");
                }
                if let Err(error) = discovery_sender.send_blocking(discovery) {
                    tracing::debug!(%error, "Mail source discovery result was dropped");
                }
            });
        }
        glib::spawn_future_local(async move {
            let Ok(discovery) = discovery_receiver.recv().await else {
                source_state_for_load.set_label("Kontenerkennung wurde abgebrochen.");
                source_state_for_load.set_visible(true);
                return;
            };

            let has_eds_sources = !discovery.sources.is_empty();
            let has_enabled_source = discovery.sources.iter().any(|source| source.enabled);
            let source_origin = match discovery.origin {
                EdsSourceOrigin::Live => "Konten",
                EdsSourceOrigin::Cache => "Offline-Cache",
                EdsSourceOrigin::Empty => "Keine Konten verfügbar",
            };
            for source in &discovery.sources {
                tracing::debug!("  Source: {} ({})", source.display_name, source.uid);
                source_store_for_load.append(&EdsSourceObject::from_source_with_origin(
                    source,
                    source_origin,
                ));
            }

            if let (Some(selection), Some(preferred_account_id)) = (
                source_selection_for_load.as_ref(),
                preferred_account_id.as_deref(),
            ) {
                let preferred_uid = preferred_account_id
                    .strip_prefix("evolution-source:")
                    .unwrap_or(preferred_account_id);
                let preferred_index = (0..source_store_for_load.n_items()).find(|index| {
                    source_store_for_load
                        .item(*index)
                        .and_downcast::<EdsSourceObject>()
                        .is_some_and(|source| {
                            source.enabled()
                                && (source.uid() == preferred_account_id
                                    || source.uid() == preferred_uid)
                        })
                });
                if let Some(index) = preferred_index {
                    tracing::info!(
                        preferred_account_id,
                        preferred_uid,
                        index,
                        "Selecting configured default mail account"
                    );
                    selection.set_selected(index);
                }
            }

            if discovery.origin == EdsSourceOrigin::Cache {
                source_state_for_load.set_label(&format!(
                    "{} Die Konten stammen aus dem Offline-Cache; Ordner und Nachrichten werden bei bestehender Verbindung geladen.",
                    discovery.status_text()
                ));
                source_state_for_load.set_visible(true);
            } else if !has_eds_sources && has_configured_accounts {
                source_state_for_load.set_label("Gespeicherte Kontoprofile vorhanden; die Synchronisation ist noch nicht verbunden.");
            } else if !has_eds_sources {
                source_state_for_load
                    .set_label("Keine Mailkonten gefunden. Richte zuerst ein Konto ein.");
            } else if !has_enabled_source {
                source_state_for_load.set_label(if has_configured_accounts {
                    "Alle gefundenen Mailkonten sind deaktiviert."
                } else {
                    "Alle Mailkonten sind deaktiviert."
                });
            } else {
                // Once a live account is available the account row is the
                // useful status indicator; keep the sidebar free of backend
                // terminology and reclaim the extra status line.
                source_state_for_load.set_visible(false);
            }
        });
    }

    fn selected_message_anchor(&self) -> MailSelectionAnchor {
        let selection = self.imp().message_selection.borrow();
        let Some(selection) = selection.as_ref() else {
            return MailSelectionAnchor::default();
        };
        let index =
            (selection.selected() != gtk::INVALID_LIST_POSITION).then_some(selection.selected());
        MailSelectionAnchor {
            id: selection
                .selected_item()
                .and_downcast::<MailMessageObject>()
                .map(|message| message.id()),
            index,
        }
    }

    fn select_message_after_reload(&self, previous: &MailSelectionAnchor) {
        let Some(selection) = self.imp().message_selection.borrow().as_ref().cloned() else {
            return;
        };
        if selection.n_items() == 0 {
            selection.set_selected(gtk::INVALID_LIST_POSITION);
            return;
        }
        if let Some(previous_id) = previous.id.as_deref() {
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
        let Some(index) = fallback_selection_index(selection.n_items(), previous.index) else {
            return;
        };
        selection.set_selected(index);
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
        if settings.offline_mode() {
            return;
        }
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

    fn apply_offline_mode(&self, offline: bool) {
        if !offline {
            self.restart_mail_polling();
            return;
        }

        if let Some(source) = self.imp().mail_sync_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(cancelled) = self.imp().mail_change_cancellation.borrow_mut().take() {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(cancelled) = self.imp().mail_incremental_cancellation.borrow_mut().take() {
            cancelled.store(true, Ordering::Release);
        }
        self.imp().camel_monitor_started.set(false);
        self.imp().mail_change_pending.set(false);
    }

    fn setup_camel_change_monitor(&self) {
        tracing::debug!("Starting deferred Camel change monitor");
        let Some((cancellation, receiver)) = camel_mail_service::start_change_monitor() else {
            tracing::debug!("Camel change monitor is unavailable in this build");
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
                            "Kontosynchronisation aktiv · {}/{} Konten verfügbar{}",
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
                        "Ordner {} in {} wurde {}{} (＋{} / −{} / ~{} / neu {}) · Aktualisierung …",
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
            camel_mail_service::CamelSyncEvent::ResyncRequired(reason) => {
                tracing::warn!(%reason, "Camel change stream requested a full resync");
                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    state.set_label("Änderungsstrom wird neu synchronisiert …");
                }
                self.refresh_mail();
            }
            camel_mail_service::CamelSyncEvent::Error(error) => {
                tracing::warn!(%error, "Native Camel change monitor stopped");
                if !self.imp().mail_loading.get() {
                    if let Some(state) = self.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!("Kontosynchronisation nicht verfügbar: {error}"));
                    }
                }
            }
        }
    }

    fn start_incremental_camel_update(&self, change: camel_mail_service::CamelChangeEvent) {
        match change.kind {
            camel_mail_service::CamelChangeKind::FolderDeleted => {
                self.apply_camel_folder_deleted(&change);
                return;
            }
            camel_mail_service::CamelChangeKind::FolderRenamed => {
                self.apply_camel_folder_renamed(&change);
                return;
            }
            camel_mail_service::CamelChangeKind::Changed => {}
        }

        let mut seen_uids = std::collections::HashSet::new();
        let message_uids = change
            .added_uids
            .iter()
            .chain(change.changed_uids.iter())
            .chain(change.recent_uids.iter())
            .filter(|uid| seen_uids.insert((*uid).clone()))
            .cloned()
            .collect::<Vec<_>>();
        let removed_ids = change
            .removed_uids
            .iter()
            .map(|uid| format!("eds:{}:{}:{}", change.source_uid, change.folder_name, uid))
            .collect::<std::collections::HashSet<_>>();
        let replaced_ids = message_uids
            .iter()
            .map(|uid| format!("eds:{}:{}:{}", change.source_uid, change.folder_name, uid))
            .collect::<std::collections::HashSet<_>>();

        let previous_selection = self.selected_message_anchor();
        let store = self.imp().message_store.borrow().clone();
        let mut previous_unread = HashMap::new();
        if let Some(store) = store.as_ref() {
            for index in 0..store.n_items() {
                let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                    continue;
                };
                let Some(location) = message.backend_location() else {
                    continue;
                };
                if location.source_uid == change.source_uid
                    && location.folder_name == change.folder_name
                {
                    previous_unread.insert(location.message_uid, message.is_unread());
                }
            }
            Self::remove_message_ids(store, &removed_ids);
        }
        self.imp()
            .batch_selection
            .borrow_mut()
            .retain(|id| !removed_ids.contains(id));
        self.update_batch_action_bar();

        if message_uids.is_empty() {
            let (unread_delta, total_delta) =
                camel_folder_counter_delta(&change, &previous_unread, &[]);
            self.apply_camel_folder_counter_delta(
                &change.source_uid,
                &change.folder_name,
                unread_delta,
                total_delta,
            );
            if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label(&format!("Änderungen in {} übernommen", change.folder_name));
            }
            if let Some(store) = store {
                self.begin_incremental_thread_refresh(store, previous_selection);
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
                "{} Nachrichten in {} werden aktualisiert …",
                message_uids.len(),
                change.folder_name
            ));
        }

        let source_uid = change.source_uid.clone();
        let folder_name = change.folder_name.clone();
        let change_for_result = change.clone();
        let worker_folder_name = folder_name.clone();
        let (sender, receiver) = async_channel::bounded(1);
        let worker_cancellation = cancellation.clone();
        std::thread::spawn(move || {
            let result = camel_mail_service::load_messages_for_uids(
                &source_uid,
                &worker_folder_name,
                &message_uids,
                &worker_cancellation,
            )
            .and_then(|messages| {
                prepare_searchable_body_texts(&messages, &worker_cancellation)
                    .map(|searchable_body_texts| (messages, searchable_body_texts))
            });
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Incremental Camel result was dropped");
            }
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err("Inkrementelle Kontosynchronisation wurde abgebrochen".to_string())
            });
            let Some(page) = page.upgrade() else {
                return;
            };
            if !generation_is_current(page.imp().load_generation.get(), generation) {
                return;
            }
            page.imp().mail_incremental_loading.set(false);
            match result {
                Ok((messages, searchable_body_texts)) => {
                    let (unread_delta, total_delta) =
                        camel_folder_counter_delta(&change_for_result, &previous_unread, &messages);
                    page.apply_camel_folder_counter_delta(
                        &change_for_result.source_uid,
                        &change_for_result.folder_name,
                        unread_delta,
                        total_delta,
                    );
                    if let Some(store) = page.imp().message_store.borrow().as_ref() {
                        Self::remove_message_ids(store, &removed_ids);
                        Self::remove_message_ids(store, &replaced_ids);
                        page.replace_or_append_messages(store, messages, searchable_body_texts);
                    }
                    if let Some(filter) = page.imp().message_filter.borrow().as_ref() {
                        filter.changed(gtk::FilterChange::Different);
                    }
                    if let Some(store) = page.imp().message_store.borrow().as_ref().cloned() {
                        page.begin_incremental_thread_refresh(store, previous_selection);
                    }
                    if let Some(state) = page.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!("Änderungen in {} übernommen", folder_name));
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Incremental Camel update failed");
                    if let Some(state) = page.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!("Synchronisation fehlgeschlagen: {error}"));
                    }
                    page.imp().mail_change_pending.set(false);
                    page.refresh_mail();
                    return;
                }
            }
            if page.imp().mail_change_pending.replace(false) {
                page.refresh_mail();
            }
        });
    }

    fn apply_camel_folder_counter_delta(
        &self,
        source_uid: &str,
        folder_name: &str,
        unread_delta: i64,
        total_delta: i64,
    ) {
        let mut folders = self.imp().mail_folder_summaries.borrow_mut();
        if let Some(folder) = folders
            .iter_mut()
            .find(|folder| folder.source_uid == source_uid && folder.folder_name == folder_name)
        {
            folder.unread_count = apply_count_delta(folder.unread_count, unread_delta);
            folder.total_count = apply_count_delta(folder.total_count, total_delta);
        }
    }

    fn apply_camel_folder_deleted(&self, change: &camel_mail_service::CamelChangeEvent) {
        let previous_selection = self.selected_message_anchor();
        let mut removed_ids = std::collections::HashSet::new();
        if let Some(store) = self.imp().message_store.borrow().as_ref() {
            for index in 0..store.n_items() {
                let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                    continue;
                };
                if message.backend_location().is_some_and(|location| {
                    location.source_uid == change.source_uid
                        && folder_path_is_within(&location.folder_name, &change.folder_name)
                }) {
                    removed_ids.insert(message.id());
                }
            }
            Self::remove_message_ids(store, &removed_ids);
        }
        self.imp()
            .batch_selection
            .borrow_mut()
            .retain(|id| !removed_ids.contains(id));
        self.update_batch_action_bar();

        let folders = {
            let mut folders = self.imp().mail_folder_summaries.borrow_mut();
            folders.retain(|folder| {
                folder.source_uid != change.source_uid
                    || !folder_path_is_within(&folder.folder_name, &change.folder_name)
            });
            folders.clone()
        };
        let selected_folder_deleted =
            self.imp()
                .folder_selection
                .borrow()
                .as_ref()
                .is_some_and(|selection| {
                    let selection = selection.borrow();
                    selection.source_uid.as_deref() == Some(change.source_uid.as_str())
                        && selection.folder_name.as_deref().is_some_and(|folder| {
                            folder_path_is_within(folder, &change.folder_name)
                        })
                });
        let fallback_folder = selected_folder_deleted.then(|| FolderSelection {
            folder_id: Self::dynamic_folder_key(&change.source_uid, ""),
            source_uid: Some(change.source_uid.clone()),
            folder_name: None,
        });
        if let Some(selection) = fallback_folder.as_ref() {
            if let Some(current) = self.imp().folder_selection.borrow().as_ref() {
                current.replace(selection.clone());
            }
        }
        if let Some(folder_list) = self.imp().folder_list.borrow().as_ref() {
            self.rebuild_camel_folder_rows(folder_list, &folders);
        }
        if let Some(selection) = fallback_folder.as_ref() {
            self.select_folder_row(selection);
            self.start_mail_load(
                MailLoadRequest {
                    source_uid: Some(change.source_uid.clone()),
                    folder_name: None,
                    offset: 0,
                    max_messages_per_folder: configured_mail_page_limit(),
                    graph_cursors: HashMap::new(),
                },
                true,
            );
            return;
        }
        if let Some(store) = self.imp().message_store.borrow().as_ref().cloned() {
            self.begin_incremental_thread_refresh(store, previous_selection);
        }
    }

    fn apply_camel_folder_renamed(&self, change: &camel_mail_service::CamelChangeEvent) {
        let Some(old_folder_name) = change.old_folder_name.as_deref() else {
            self.refresh_mail();
            return;
        };
        let mut previous_selection = self.selected_message_anchor();
        let mut renamed_message_ids = Vec::new();
        if let Some(store) = self.imp().message_store.borrow().as_ref() {
            for index in 0..store.n_items() {
                let Some(message) = store.item(index).and_downcast::<MailMessageObject>() else {
                    continue;
                };
                let Some(location) = message.backend_location() else {
                    continue;
                };
                if location.source_uid != change.source_uid {
                    continue;
                }
                let Some(renamed) = renamed_folder_path(
                    &location.folder_name,
                    old_folder_name,
                    &change.folder_name,
                ) else {
                    continue;
                };
                if let Some(ids) = message.rename_backend_folder(
                    &change.source_uid,
                    &location.folder_name,
                    &renamed,
                ) {
                    renamed_message_ids.push(ids);
                }
            }
        }
        if !renamed_message_ids.is_empty() {
            let mut batch_selection = self.imp().batch_selection.borrow_mut();
            for (old_id, new_id) in &renamed_message_ids {
                if batch_selection.remove(old_id) {
                    batch_selection.insert(new_id.clone());
                }
                if previous_selection.id.as_deref() == Some(old_id.as_str()) {
                    previous_selection.id = Some(new_id.clone());
                }
            }
            drop(batch_selection);
            self.update_batch_action_bar();
        }

        let folders = {
            let mut folders = self.imp().mail_folder_summaries.borrow_mut();
            for folder in folders
                .iter_mut()
                .filter(|folder| folder.source_uid == change.source_uid)
            {
                let is_renamed_root = folder.folder_name == old_folder_name;
                if let Some(renamed) =
                    renamed_folder_path(&folder.folder_name, old_folder_name, &change.folder_name)
                {
                    folder.folder_name = renamed;
                    if is_renamed_root {
                        folder.display_name = change
                            .folder_name
                            .rsplit('/')
                            .next()
                            .unwrap_or(change.folder_name.as_str())
                            .to_string();
                    }
                }
            }
            folders.clone()
        };
        if let Some(request) = self.imp().active_request.borrow_mut().as_mut()
            && request.source_uid.as_deref() == Some(change.source_uid.as_str())
            && let Some(folder) = request.folder_name.as_deref()
            && let Some(renamed) = renamed_folder_path(folder, old_folder_name, &change.folder_name)
        {
            request.folder_name = Some(renamed);
        }
        if let Some(selection) = self.imp().folder_selection.borrow().as_ref() {
            let mut selection = selection.borrow_mut();
            if selection.source_uid.as_deref() == Some(change.source_uid.as_str())
                && let Some(folder) = selection.folder_name.as_deref()
                && let Some(renamed) =
                    renamed_folder_path(folder, old_folder_name, &change.folder_name)
            {
                selection.folder_id = Self::dynamic_folder_key(&change.source_uid, &renamed);
                selection.folder_name = Some(renamed);
            }
        }
        if let Some(folder_list) = self.imp().folder_list.borrow().as_ref() {
            self.rebuild_camel_folder_rows(folder_list, &folders);
        }
        let selected_folder = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone());
        if let Some(selection) = selected_folder.as_ref() {
            self.select_folder_row(selection);
        }
        if let Some(store) = self.imp().message_store.borrow().as_ref().cloned() {
            self.begin_incremental_thread_refresh(store, previous_selection);
        }
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
            .unwrap_or("Mailkonto");
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
                "Konten werden gesucht …".to_string()
            }
            camel_mail_service::CamelSyncPhase::Connecting => {
                format!("Verbinde mit {account}{account_progress} …")
            }
            camel_mail_service::CamelSyncPhase::LoadingFolders => {
                format!("Lade Ordner von {account}{account_progress} …")
            }
            camel_mail_service::CamelSyncPhase::LoadingMessages => format!(
                "Lade {} von {account}{account_progress} …",
                progress.folder_name.as_deref().unwrap_or("Nachrichten")
            ),
            camel_mail_service::CamelSyncPhase::SourceComplete => {
                format!("{account} synchronisiert{account_progress}")
            }
            camel_mail_service::CamelSyncPhase::Complete => {
                format!("Synchronisation abgeschlossen{account_progress}")
            }
            camel_mail_service::CamelSyncPhase::Failed => {
                format!("Synchronisation fehlgeschlagen{account_progress}")
            }
            camel_mail_service::CamelSyncPhase::Retrying => format!(
                "Vorübergehender Fehler · neuer Versuch {}/{} …",
                progress.attempt, progress.max_attempts
            ),
        };
        state.set_label(&label);
        if let Some(window) = self.root().and_downcast::<crate::window::EvolutionWindow>() {
            window.update_mail_status(&label);
        }
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

    fn cancel_pending_mail_apply(&self) {
        if let Some(source) = self.imp().mail_apply_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.imp().mail_thread_apply_source.borrow_mut().take() {
            source.remove();
        }
    }

    fn start_mail_load(&self, request: MailLoadRequest, replace: bool) {
        if let Some(source) = self.imp().mail_startup_source.borrow_mut().take() {
            source.remove();
            self.setup_mail_polling();
        }
        tracing::debug!(
            source_uid = ?request.source_uid,
            folder_name = ?request.folder_name,
            offset = request.offset,
            replace,
            "Scheduling mail load"
        );
        if let Some(cancelled) = self.imp().mail_search_cancellation.borrow_mut().take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(source) = self.imp().search_debounce_source.borrow_mut().take() {
            source.remove();
        }
        self.imp()
            .search_generation
            .set(self.imp().search_generation.get().wrapping_add(1));
        self.imp().remote_search_applied.set(false);
        if let Some(cancelled) = self.imp().mail_load_cancellation.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(cancelled) = self.imp().mail_incremental_cancellation.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.imp().mail_incremental_loading.set(false);
        self.cancel_pending_mail_apply();

        let generation = self.imp().load_generation.get().wrapping_add(1);
        self.imp().load_generation.set(generation);
        // A replacing request represents the user's current target before its
        // worker returns, so refresh cannot jump back to the old folder. A
        // paging cursor, however, advances only after successful apply; a
        // failed "load more" must not skip a page on retry.
        if replace {
            self.imp().active_request.replace(Some(request.clone()));
        }
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
        let progress_sender_for_worker = progress_sender.clone();
        let worker_input = MailLoadWorkerInput::from_main_thread(request.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = load_mail_backend(
                worker_input,
                &cancellation,
                Some(&progress_sender_for_worker),
            );
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Mail load worker completed"
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
                if generation_is_current(page.imp().load_generation.get(), generation) {
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
            if !generation_is_current(page.imp().load_generation.get(), generation) {
                return;
            }
            page.apply_mail_load(request, result, replace);
        });
    }

    fn handle_search_changed(&self, input: &str) {
        let cancelled_search =
            if let Some(cancelled) = self.imp().mail_search_cancellation.borrow_mut().take() {
                cancelled.store(true, Ordering::Relaxed);
                true
            } else {
                false
            };
        if let Some(source) = self.imp().search_debounce_source.borrow_mut().take() {
            source.remove();
        }
        let generation = self.imp().search_generation.get().wrapping_add(1);
        self.imp().search_generation.set(generation);

        let input = input.trim().to_string();
        if input.is_empty() {
            if self.imp().remote_search_applied.replace(false) {
                let mut request = self
                    .imp()
                    .active_request
                    .borrow()
                    .clone()
                    .unwrap_or_else(MailLoadRequest::initial);
                request.offset = 0;
                self.start_mail_load(request, true);
            } else if cancelled_search {
                self.restore_after_graph_search(false);
            }
            return;
        }

        if !self.imp().graph_backend_active.get() {
            if cancelled_search {
                self.restore_after_graph_search(false);
            }
            return;
        }
        let query = MessageSearchQuery::parse(&input);
        let Some(graph_query) = graph_search_expression(&query) else {
            // State-only, label-only and date-only predicates remain fully
            // local.  If a previous remote search was displayed, restore the
            // normal Graph page first so the local predicate sees the normal
            // mailbox window rather than an old remote result set.
            if self.imp().remote_search_applied.replace(false) {
                let mut request = self
                    .imp()
                    .active_request
                    .borrow()
                    .clone()
                    .unwrap_or_else(MailLoadRequest::initial);
                request.offset = 0;
                self.start_mail_load(request, true);
            } else if cancelled_search {
                self.restore_after_graph_search(false);
            }
            return;
        };

        let page = self.downgrade();
        let source = glib::timeout_add_local_once(StdDuration::from_millis(250), move || {
            let Some(page) = page.upgrade() else {
                return;
            };
            page.imp().search_debounce_source.borrow_mut().take();
            page.start_graph_search(input, graph_query, generation);
        });
        self.imp().search_debounce_source.replace(Some(source));
    }

    fn graph_search_scope(&self) -> (Option<String>, Option<String>) {
        let selection = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone())
            .unwrap_or_default();
        let active_request = self.imp().active_request.borrow().clone();
        let source_uid = selection.source_uid.clone().or_else(|| {
            active_request
                .as_ref()
                .and_then(|request| request.source_uid.clone())
        });
        let folder_name = if selection.is_remote() {
            selection.folder_name.clone()
        } else {
            match selection.folder_id.as_str() {
                "inbox" => Some("Inbox".to_string()),
                "sent" => Some("Sent Items".to_string()),
                "drafts" => Some("Drafts".to_string()),
                "trash" => Some("Deleted Items".to_string()),
                "spam" => Some("Junk Email".to_string()),
                "archive" => Some("Archive".to_string()),
                _ => None,
            }
        };
        (source_uid, folder_name)
    }

    fn start_graph_search(&self, input: String, graph_query: String, generation: u64) {
        if generation != self.imp().search_generation.get()
            || !self.imp().graph_backend_active.get()
        {
            return;
        }
        if let Some(cancelled) = self.imp().mail_load_cancellation.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(cancelled) = self.imp().mail_incremental_cancellation.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.imp().mail_incremental_loading.set(false);
        let load_generation = self.imp().load_generation.get().wrapping_add(1);
        self.imp().load_generation.set(load_generation);
        self.cancel_pending_mail_apply();
        let (source_uid, folder_name) = self.graph_search_scope();
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .mail_search_cancellation
            .replace(Some(cancellation.clone()));
        self.imp().mail_loading.set(true);
        if let Some(button) = self.imp().refresh_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
            button.set_visible(false);
            button.set_sensitive(false);
        }
        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            state.set_label("Graph-Suche läuft …");
        }

        let request = MailLoadRequest {
            source_uid: source_uid.clone(),
            folder_name: folder_name.clone(),
            offset: 0,
            max_messages_per_folder: configured_mail_page_limit(),
            graph_cursors: HashMap::new(),
        };
        let search_limit = request.max_messages_per_folder;
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = camel_mail_service::search_messages_graph(
                source_uid.as_deref(),
                folder_name.as_deref(),
                &graph_query,
                search_limit,
                &cancellation,
            )
            .and_then(|load| {
                prepare_searchable_body_texts(&load.messages, &cancellation)
                    .map(|searchable_body_texts| (load, searchable_body_texts))
            });
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Graph search result was dropped before delivery");
            }
        });

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page.upgrade() else {
                return;
            };
            if page.imp().search_generation.get() != generation
                || !generation_is_current(page.imp().load_generation.get(), load_generation)
            {
                return;
            }
            page.imp().mail_search_cancellation.borrow_mut().take();
            page.imp().mail_loading.set(false);
            if let Some(button) = page.imp().refresh_button.borrow().as_ref() {
                button.set_sensitive(true);
            }
            match result {
                Ok(Ok((load, searchable_body_texts))) => {
                    page.apply_mail_load(
                        request,
                        MailPageLoadResult::Camel {
                            camel: load,
                            searchable_body_texts,
                        },
                        true,
                    );
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, query = %input, "Graph mail search failed");
                    page.restore_after_graph_search(true);
                }
                Err(_) => {
                    page.restore_after_graph_search(true);
                }
            }
        });
    }

    fn restore_after_graph_search(&self, clear_stale_models: bool) {
        self.imp().remote_search_applied.set(false);
        if clear_stale_models {
            if let Some(store) = self.imp().message_store.borrow().as_ref() {
                store.remove_all();
            }
            if let Some(store) = self.imp().thread_root_store.borrow().as_ref() {
                store.remove_all();
            }
            if let Some(members) = self.imp().thread_members.borrow().as_ref() {
                members.borrow_mut().clear();
            }
            if let Some(children) = self.imp().thread_children.borrow().as_ref() {
                children.borrow_mut().clear();
            }
        }
        let mut request = self
            .imp()
            .active_request
            .borrow()
            .clone()
            .unwrap_or_else(MailLoadRequest::initial);
        request.offset = 0;
        self.start_mail_load(request, true);
    }

    fn apply_mail_load(&self, request: MailLoadRequest, result: MailPageLoadResult, replace: bool) {
        let pending_search = match &result {
            MailPageLoadResult::Camel { camel, .. }
            | MailPageLoadResult::Combined { camel, .. }
                if camel.is_graph && camel.search_query.is_none() =>
            {
                self.imp()
                    .search_entry
                    .borrow()
                    .as_ref()
                    .map(|entry| entry.text().trim().to_string())
                    .filter(|query| !query.is_empty())
            }
            _ => None,
        };
        let result_kind = match &result {
            MailPageLoadResult::Camel { .. } => "camel",
            MailPageLoadResult::Combined { .. } => "combined",
            MailPageLoadResult::Local { .. } => "local",
            MailPageLoadResult::Error(_) => "error",
        };
        tracing::debug!(
            result = result_kind,
            replace,
            "Applying mail load to GTK models"
        );
        let previous_selection = self.selected_message_anchor();
        if replace {
            self.clear_batch_selection();
        }

        match result {
            MailPageLoadResult::Combined {
                camel,
                local_messages,
                searchable_body_texts,
                local_folders,
            } => {
                self.begin_pending_mail_apply(PendingMailApply::from_camel(
                    request,
                    camel,
                    Some(local_messages),
                    searchable_body_texts,
                    local_folders,
                    replace,
                    previous_selection,
                    pending_search,
                    self.imp().load_generation.get(),
                ));
            }
            MailPageLoadResult::Camel {
                camel,
                searchable_body_texts,
            } => {
                tracing::debug!(
                    folders = camel.folders.len(),
                    messages = camel.messages.len(),
                    "Building GTK mail models from backend result"
                );
                self.begin_pending_mail_apply(PendingMailApply::from_camel(
                    request,
                    camel,
                    None,
                    searchable_body_texts,
                    HashSet::new(),
                    replace,
                    previous_selection,
                    pending_search,
                    self.imp().load_generation.get(),
                ));
            }
            MailPageLoadResult::Local {
                reason,
                messages,
                searchable_body_texts,
                local_folders,
            } => {
                self.begin_pending_mail_apply(PendingMailApply::from_local(
                    request,
                    reason,
                    messages,
                    searchable_body_texts,
                    local_folders,
                    replace,
                    previous_selection,
                    self.imp().load_generation.get(),
                ));
            }
            MailPageLoadResult::Error(error) => {
                self.imp().mail_loading.set(false);
                if replace {
                    if let Some(store) = self.imp().message_store.borrow().as_ref() {
                        store.remove_all();
                    }
                    if let Some(store) = self.imp().thread_root_store.borrow().as_ref() {
                        store.remove_all();
                    }
                    if let Some(members) = self.imp().thread_members.borrow().as_ref() {
                        members.borrow_mut().clear();
                    }
                    if let Some(children) = self.imp().thread_children.borrow().as_ref() {
                        children.borrow_mut().clear();
                    }
                }
                if let Some(button) = self.imp().refresh_button.borrow().as_ref() {
                    button.set_sensitive(true);
                }
                if let Some(state) = self.imp().source_state.borrow().as_ref() {
                    state.set_label(&format!("Kontosynchronisation nicht möglich: {error}"));
                    state.set_visible(true);
                }
                if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                    button.set_visible(false);
                    button.set_sensitive(true);
                }
                if let Some(state) = self.imp().message_state.borrow().as_ref() {
                    state.set_label(&format!("Kontosynchronisation nicht möglich: {error}"));
                }
                tracing::warn!(%error, "Mail backend load failed");
            }
        }
    }

    fn begin_pending_mail_apply(&self, mut pending: PendingMailApply) {
        let store = self.imp().message_store.borrow().clone();
        if let Some(store) = store.as_ref() {
            if pending.replace {
                store.remove_all();
            }
            for index in 0..store.n_items() {
                if let Some(message) = store.item(index).and_downcast::<MailMessageObject>() {
                    pending.existing_ids.insert(message.id());
                }
            }
        }

        if pending.result_kind == "local" {
            self.imp().mail_folder_summaries.replace(Vec::new());
        } else {
            self.imp()
                .mail_folder_summaries
                .replace(pending.camel_folders.clone());
        }

        self.imp().mail_loading.set(true);
        if let Some(button) = self.imp().refresh_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
            button.set_sensitive(false);
        }

        if pending.result_kind != "local" {
            if pending.replace {
                self.imp().local_folder_ids.replace(HashSet::new());
            }
            self.imp().graph_backend_active.set(pending.camel_is_graph);
            self.imp()
                .remote_search_applied
                .set(pending.camel_search_applied);
            self.imp()
                .active_request
                .replace(Some(pending.request.clone()));

            if let Some(folder_list) = self.imp().folder_list.borrow().as_ref() {
                if pending.replace {
                    self.rebuild_camel_folder_rows(folder_list, &pending.camel_folders);
                }
                self.update_camel_folder_counts(folder_list, &pending.camel_folders, None);
            }
            if let Some(section) = self.imp().folder_section.borrow().as_ref() {
                section.set_visible(true);
            }
            if let Some(state) = self.imp().source_state.borrow().as_ref() {
                if pending.camel_warning_visible {
                    state.set_label(pending.camel_status.as_deref().unwrap_or_default());
                    state.set_visible(true);
                } else {
                    state.set_visible(false);
                }
            }
            if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                button.set_visible(pending.camel_has_more);
            }
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label(&format!(
                    "{} · Nachrichten werden aufgebaut …",
                    pending.camel_status.as_deref().unwrap_or("Mailbackend")
                ));
            }
        } else {
            self.imp().graph_backend_active.set(false);
            self.imp().remote_search_applied.set(false);
            if let Some(folder_list) = self.imp().folder_list.borrow().as_ref() {
                self.rebuild_camel_folder_rows(folder_list, &[]);
            }
            if let Some(state) = self.imp().source_state.borrow().as_ref() {
                state.set_label(pending.local_reason.as_deref().unwrap_or_default());
                state.set_visible(true);
            }
            if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                button.set_visible(false);
            }
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label("Lokale Nachrichten werden aufgebaut …");
            }
        }

        tracing::debug!(
            stage = ?pending.stage,
            messages = pending.active_messages.len(),
            combined = pending.combined,
            "Scheduling incremental GTK mail model application"
        );
        let state = Rc::new(RefCell::new(pending));
        let page = self.downgrade();
        let state_for_source = state.clone();
        let source = glib::idle_add_local_full(glib::Priority::LOW, move || {
            let Some(page) = page.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !generation_is_current(
                page.imp().load_generation.get(),
                state_for_source.borrow().generation,
            ) {
                page.imp().mail_apply_source.borrow_mut().take();
                return glib::ControlFlow::Break;
            }

            let done = {
                let mut pending = state_for_source.borrow_mut();
                page.process_pending_mail_apply(&mut pending)
            };
            if !done {
                return glib::ControlFlow::Continue;
            }

            page.imp().mail_apply_source.borrow_mut().take();
            let pending = state_for_source.borrow();
            page.finish_pending_mail_apply(&pending);
            glib::ControlFlow::Break
        });
        self.imp().mail_apply_source.replace(Some(source));
    }

    fn process_pending_mail_apply(&self, pending: &mut PendingMailApply) -> bool {
        if pending.active_messages.is_empty() {
            if pending.stage == PendingMailApplyStage::Camel {
                if let Some(local_messages) = pending.local_messages.take() {
                    pending.stage = PendingMailApplyStage::Local;
                    pending.active_messages = local_messages;
                    if let Some(state) = self.imp().message_state.borrow().as_ref() {
                        state.set_label(&format!(
                            "{} · Lokale Nachrichten werden aufgebaut …",
                            pending.camel_status.as_deref().unwrap_or("Mailbackend")
                        ));
                    }
                    return false;
                }
            }
            return true;
        }

        let chunk_len = pending.active_messages.len().min(MAIL_MODEL_CHUNK_SIZE);
        let records = pending
            .active_messages
            .drain(..chunk_len)
            .collect::<Vec<_>>();
        let mut objects = Vec::with_capacity(records.len());
        for record in records {
            if pending.stage == PendingMailApplyStage::Local {
                let counts = pending
                    .local_counts
                    .entry(record.folder_id.clone())
                    .or_insert((0, 0));
                counts.1 = counts.1.saturating_add(1);
                if record.is_unread {
                    counts.0 = counts.0.saturating_add(1);
                }
            }
            if pending.existing_ids.insert(record.id.clone()) {
                let searchable_body_text = pending
                    .searchable_body_texts
                    .remove(&parsed_mail_identity(&record));
                let object = record.into_object();
                if let Some(searchable_body_text) = searchable_body_text {
                    object.set_searchable_body_text(searchable_body_text);
                }
                objects.push(object.upcast::<glib::Object>());
                pending.appended = pending.appended.saturating_add(1);
            }
        }
        if let Some(store) = self.imp().message_store.borrow().as_ref() {
            if !objects.is_empty() {
                store.splice(store.n_items(), 0, &objects);
            }
        }

        if let Some(state) = self.imp().message_state.borrow().as_ref() {
            let prefix = pending
                .camel_status
                .as_deref()
                .or(pending.local_reason.as_deref())
                .unwrap_or("Nachrichten");
            state.set_label(&format!(
                "{prefix} · {} Nachricht(en) aufgebaut …",
                pending.appended
            ));
        }
        pending.active_messages.is_empty()
            && !(pending.stage == PendingMailApplyStage::Camel && pending.local_messages.is_some())
    }

    fn finish_pending_mail_apply(&self, pending: &PendingMailApply) {
        let is_local = pending.result_kind == "local";
        let mut visible_local_folders = pending.local_folders.clone();
        visible_local_folders.extend(
            pending
                .local_counts
                .keys()
                .filter(|folder_id| folder_id.starts_with("custom:"))
                .cloned(),
        );
        if let Some(section) = self.imp().local_folder_section.borrow().as_ref() {
            section.set_visible(
                is_local || !visible_local_folders.is_empty() || !pending.local_counts.is_empty(),
            );
        }

        if is_local {
            if let Some(folder_list) = self.imp().folder_list.borrow().as_ref() {
                self.rebuild_local_folder_rows(
                    folder_list,
                    &pending.local_counts,
                    &visible_local_folders,
                );
            }
            self.imp()
                .local_folder_ids
                .replace(visible_local_folders.clone());
            self.imp().active_request.replace(None);
            if let Some(state) = self.imp().source_state.borrow().as_ref() {
                state.set_label(pending.local_reason.as_deref().unwrap_or_default());
                state.set_visible(true);
            }
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label(&format!(
                    "{} · {} lokale Nachricht(en) geladen",
                    pending
                        .local_reason
                        .as_deref()
                        .unwrap_or("Lokaler Speicher"),
                    pending.appended
                ));
            }
        } else {
            if pending.combined {
                if let Some(folder_list) = self.imp().folder_list.borrow().as_ref() {
                    self.rebuild_local_folder_rows(
                        folder_list,
                        &pending.local_counts,
                        &visible_local_folders,
                    );
                    self.update_camel_folder_counts(
                        folder_list,
                        &pending.camel_folders,
                        Some(&pending.local_counts),
                    );
                }
                self.imp()
                    .local_folder_ids
                    .replace(visible_local_folders.clone());
            }
            if let Some(state) = self.imp().message_state.borrow().as_ref() {
                state.set_label(&format!(
                    "{} · {} Nachricht(en) in dieser Seite",
                    pending.camel_status.as_deref().unwrap_or("Mailbackend"),
                    pending.appended
                ));
            }
            if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
                button.set_visible(pending.camel_has_more);
            }
        }

        let completion = PendingMailCompletion {
            generation: pending.generation,
            started: pending.started,
            replace: pending.replace,
            previous_selection: pending.previous_selection.clone(),
            is_local,
            appended: pending.appended,
            has_visible_local_content: pending.appended > 0 || !visible_local_folders.is_empty(),
            pending_search: pending.pending_search.clone(),
            result_kind: pending.result_kind,
        };
        if let Some(store) = self.imp().message_store.borrow().as_ref().cloned() {
            self.begin_pending_thread_models(store, completion);
        } else {
            self.finalize_pending_mail_apply(&completion);
        }
    }

    fn begin_pending_thread_models(
        &self,
        store: gio::ListStore,
        completion: PendingMailCompletion,
    ) {
        let state = Rc::new(RefCell::new(PendingThreadModelApply::from_store(
            completion.generation,
            store,
            self.imp().mail_threading.get(),
            completion,
        )));
        let page = self.downgrade();
        let state_for_source = state.clone();
        let source = glib::timeout_add_local_full(
            StdDuration::from_millis(1),
            glib::Priority::LOW,
            move || {
                let Some(page) = page.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !generation_is_current(
                    page.imp().load_generation.get(),
                    state_for_source.borrow().generation,
                ) {
                    page.imp().mail_thread_apply_source.borrow_mut().take();
                    return glib::ControlFlow::Break;
                }

                let done = {
                    let mut state = state_for_source.borrow_mut();
                    page.process_pending_thread_models(&mut state)
                };
                if !done {
                    return glib::ControlFlow::Continue;
                }

                page.imp().mail_thread_apply_source.borrow_mut().take();
                let state = state_for_source.borrow();
                page.finalize_pending_mail_apply(&state.completion);
                glib::ControlFlow::Break
            },
        );
        self.imp().mail_thread_apply_source.replace(Some(source));
    }

    fn process_pending_thread_models(&self, state: &mut PendingThreadModelApply) -> bool {
        match state.stage {
            PendingThreadModelStage::ReadStore => {
                let end = (state.next_message + MAIL_MODEL_CHUNK_SIZE)
                    .min(state.store.n_items() as usize);
                for index in state.next_message..end {
                    if let Some(message) = state
                        .store
                        .item(index as u32)
                        .and_downcast::<MailMessageObject>()
                    {
                        state.messages.push(message);
                    }
                }
                state.next_message = end;
                if state.next_message < state.store.n_items() as usize {
                    return false;
                }
                state.next_message = 0;
                state.stage = PendingThreadModelStage::CollectMessages;
                false
            }
            PendingThreadModelStage::CollectMessages => {
                let end = (state.next_message + MAIL_MODEL_CHUNK_SIZE).min(state.messages.len());
                if state.threading {
                    for message in &state.messages[state.next_message..end] {
                        let thread_id = message.thread_id();
                        if !state.grouped_messages.contains_key(&thread_id) {
                            state.thread_order.push(thread_id.clone());
                        }
                        state
                            .grouped_messages
                            .entry(thread_id)
                            .or_default()
                            .push(message.clone());
                    }
                } else {
                    for message in &state.messages[state.next_message..end] {
                        let message_id = message.id();
                        state
                            .thread_members
                            .insert(message_id.clone(), vec![message.clone()]);
                        state.roots.push(message.clone());
                    }
                }
                state.next_message = end;
                if state.next_message < state.messages.len() {
                    return false;
                }
                if state.threading {
                    state.stage = PendingThreadModelStage::CollectThreadSortKeys;
                } else {
                    state.stage = PendingThreadModelStage::CollectRootSortKeys;
                }
                false
            }
            PendingThreadModelStage::CollectThreadSortKeys => {
                if state.current_thread.is_none() {
                    let Some(thread_id) = state.thread_order.get(state.next_thread).cloned() else {
                        state.stage = PendingThreadModelStage::CollectRootSortKeys;
                        return false;
                    };
                    state.next_thread += 1;
                    let Some(members) = state.grouped_messages.remove(&thread_id) else {
                        return false;
                    };
                    state.current_thread = Some(PendingThreadBuild::new(thread_id, members));
                }

                let current = state
                    .current_thread
                    .as_mut()
                    .expect("current thread initialized");
                let end = (current.next_member + MAIL_MODEL_CHUNK_SIZE).min(current.members.len());
                for index in current.next_member..end {
                    let message = &current.members[index];
                    current.sort_records.push(ThreadMemberSortRecord {
                        position: message.thread_position(),
                        id: message.id(),
                        index,
                    });
                }
                current.next_member = end;
                if current.next_member < current.members.len() {
                    return false;
                }
                current.next_member = 0;
                let records = std::mem::take(&mut current.sort_records);
                if records.len() >= MAIL_SORT_WORKER_THRESHOLD {
                    state.thread_sort_receiver = Some(spawn_record_sort(records, false));
                    state.stage = PendingThreadModelStage::AwaitThreadSort;
                } else {
                    current.sort_order = sorted_record_indices(records, false);
                    state.stage = PendingThreadModelStage::ApplyThreadSort;
                }
                false
            }
            PendingThreadModelStage::AwaitThreadSort => {
                let result = state
                    .thread_sort_receiver
                    .as_ref()
                    .map(Receiver::try_recv)
                    .unwrap_or(Err(TryRecvError::Disconnected));
                match result {
                    Ok(indices) => {
                        state.thread_sort_receiver = None;
                        if let Some(current) = state.current_thread.as_mut() {
                            current.sort_order = indices;
                        }
                        state.stage = PendingThreadModelStage::ApplyThreadSort;
                    }
                    Err(TryRecvError::Empty) => return false,
                    Err(TryRecvError::Disconnected) => {
                        state.thread_sort_receiver = None;
                        if let Some(current) = state.current_thread.as_mut() {
                            current.sort_order = (0..current.members.len()).collect();
                        }
                        state.stage = PendingThreadModelStage::ApplyThreadSort;
                    }
                }
                false
            }
            PendingThreadModelStage::ApplyThreadSort => {
                let current = state
                    .current_thread
                    .as_mut()
                    .expect("sorted thread initialized");
                let member_count = current.sort_order.len();
                let end = (current.next_member + MAIL_MODEL_CHUNK_SIZE).min(member_count);
                for position in current.next_member..end {
                    let Some(member) = current
                        .sort_order
                        .get(position)
                        .and_then(|index| current.members.get(*index))
                        .cloned()
                    else {
                        continue;
                    };
                    member.set_thread_metadata(&current.thread_id, member_count, position);
                    if position > 0 {
                        current.children.append(&member);
                    }
                    current.sorted_members.push(member);
                }
                current.next_member = end;
                if current.next_member < member_count {
                    return false;
                }

                let current = state
                    .current_thread
                    .take()
                    .expect("completed thread initialized");
                if let Some(root) = current.sorted_members.first().cloned() {
                    state.roots.push(root);
                    state
                        .thread_members
                        .insert(current.thread_id.clone(), current.sorted_members);
                    state
                        .thread_children
                        .insert(current.thread_id, current.children);
                }
                state.stage = PendingThreadModelStage::CollectThreadSortKeys;
                false
            }
            PendingThreadModelStage::CollectRootSortKeys => {
                let end = (state.next_root + MAIL_MODEL_CHUNK_SIZE).min(state.roots.len());
                let sort_field = self.imp().mail_sort_field.get();
                for index in state.next_root..end {
                    state.root_sort_records.push(mail_root_sort_record(
                        &state.roots[index],
                        sort_field,
                        index,
                    ));
                }
                state.next_root = end;
                if state.next_root < state.roots.len() {
                    return false;
                }
                state.next_root = 0;
                let records = std::mem::take(&mut state.root_sort_records);
                let descending = self.imp().mail_sort_descending.get();
                if records.len() >= MAIL_SORT_WORKER_THRESHOLD {
                    state.root_sort_receiver = Some(spawn_record_sort(records, descending));
                    state.stage = PendingThreadModelStage::AwaitRootSort;
                } else {
                    state.root_sort_order = sorted_record_indices(records, descending);
                    state.stage = PendingThreadModelStage::ApplyRootSort;
                }
                false
            }
            PendingThreadModelStage::AwaitRootSort => {
                let result = state
                    .root_sort_receiver
                    .as_ref()
                    .map(Receiver::try_recv)
                    .unwrap_or(Err(TryRecvError::Disconnected));
                match result {
                    Ok(indices) => {
                        state.root_sort_receiver = None;
                        state.root_sort_order = indices;
                        state.stage = PendingThreadModelStage::ApplyRootSort;
                    }
                    Err(TryRecvError::Empty) => return false,
                    Err(TryRecvError::Disconnected) => {
                        state.root_sort_receiver = None;
                        state.root_sort_order = (0..state.roots.len()).collect();
                        state.stage = PendingThreadModelStage::ApplyRootSort;
                    }
                }
                false
            }
            PendingThreadModelStage::ApplyRootSort => {
                let end =
                    (state.next_root + MAIL_MODEL_CHUNK_SIZE).min(state.root_sort_order.len());
                for position in state.next_root..end {
                    let Some(root) = state
                        .root_sort_order
                        .get(position)
                        .and_then(|index| state.roots.get(*index))
                        .cloned()
                    else {
                        continue;
                    };
                    state.sorted_roots.push(root);
                }
                state.next_root = end;
                if state.next_root < state.root_sort_order.len() {
                    return false;
                }
                state.roots = std::mem::take(&mut state.sorted_roots);
                state.root_sort_order.clear();
                state.next_root = 0;
                state.stage = PendingThreadModelStage::AppendRoots;
                false
            }
            PendingThreadModelStage::AppendRoots => {
                let Some(root_store) = state
                    .root_store
                    .clone()
                    .or_else(|| self.imp().thread_root_store.borrow().clone())
                else {
                    return true;
                };
                if state.root_store.is_none() {
                    root_store.remove_all();
                    if let Some(members_for_model) = self.imp().thread_members.borrow().as_ref() {
                        members_for_model.replace(state.thread_members.clone());
                    }
                    if let Some(children_for_model) = self.imp().thread_children.borrow().as_ref() {
                        children_for_model.replace(state.thread_children.clone());
                    }
                    state.root_store = Some(root_store.clone());
                }
                let end = (state.root_index + MAIL_MODEL_CHUNK_SIZE).min(state.roots.len());
                for root in &state.roots[state.root_index..end] {
                    root_store.append(root);
                }
                state.root_index = end;
                if state.root_index < state.roots.len() {
                    return false;
                }
                if let Some(filter) = self.imp().thread_filter.borrow().as_ref() {
                    filter.changed(gtk::FilterChange::Different);
                }
                if state.store.n_items() > 0 {
                    let count = state.store.n_items();
                    state.store.items_changed(0, count, count);
                }
                true
            }
        }
    }

    fn finalize_pending_mail_apply(&self, completion: &PendingMailCompletion) {
        if let Some(filter) = self.imp().message_filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }

        let selected_folder = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().clone());
        if completion.replace {
            if completion.is_local {
                let selected_folder = selected_folder
                    .filter(|selection| !selection.is_remote())
                    .unwrap_or_else(FolderSelection::configured_initial);
                if let Some(selection) = self.imp().folder_selection.borrow().as_ref() {
                    selection.replace(selected_folder.clone());
                }
                self.select_folder_row(&selected_folder);
                if completion.has_visible_local_content {
                    if let Some(section) = self.imp().folder_section.borrow().as_ref() {
                        section.set_visible(true);
                    }
                }
                self.select_message_after_reload(&MailSelectionAnchor::default());
            } else if let Some(selected_folder) = selected_folder {
                if selected_folder.is_remote() {
                    self.select_folder_row(&selected_folder);
                }
                self.select_message_after_reload(&completion.previous_selection);
            } else {
                self.select_message_after_reload(&completion.previous_selection);
            }
        } else {
            self.select_message_after_reload(&completion.previous_selection);
        }

        self.imp().mail_loading.set(false);
        if let Some(button) = self.imp().refresh_button.borrow().as_ref() {
            button.set_sensitive(true);
        }
        if let Some(button) = self.imp().load_more_button.borrow().as_ref() {
            button.set_sensitive(true);
        }

        if completion.replace
            && !self.imp().graph_backend_active.get()
            && !AppSettings::try_new()
                .map(|settings| settings.offline_mode())
                .unwrap_or(false)
            && !self.imp().camel_monitor_started.replace(true)
        {
            let page = self.downgrade();
            glib::timeout_add_local_once(StdDuration::from_millis(1_500), move || {
                let Some(page) = page.upgrade() else {
                    return;
                };
                if !page.imp().graph_backend_active.get() {
                    page.setup_camel_change_monitor();
                }
            });
        }

        self.refresh_unread_counters();
        if self.imp().mail_change_pending.replace(false) && !self.imp().mail_loading.get() {
            self.refresh_mail();
        }
        if let Some(pending_search) = completion.pending_search.as_deref() {
            self.handle_search_changed(pending_search);
        }
        let stored_messages = self
            .imp()
            .message_store
            .borrow()
            .as_ref()
            .map(gtk::prelude::ListModelExt::n_items)
            .unwrap_or(0);
        let visible_messages = self
            .imp()
            .message_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.n_items())
            .unwrap_or(0);
        let visible_threads = self
            .imp()
            .thread_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.n_items())
            .unwrap_or(0);
        let selected_folder = self
            .imp()
            .folder_selection
            .borrow()
            .as_ref()
            .map(|selection| selection.borrow().folder_id.clone())
            .unwrap_or_else(|| "all".to_string());
        tracing::debug!(
            elapsed_ms = completion.started.elapsed().as_millis() as u64,
            result = completion.result_kind,
            appended = completion.appended,
            stored_messages,
            visible_messages,
            visible_threads,
            selected_folder = %selected_folder,
            "Mail load applied to GTK models"
        );
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

    fn replace_or_append_messages(
        &self,
        store: &gio::ListStore,
        messages: Vec<ParsedMail>,
        mut searchable_body_texts: SearchableBodyTextCache,
    ) {
        let mut unique_messages = Vec::with_capacity(messages.len());
        let mut ids = std::collections::HashSet::new();
        for message in messages {
            let id = message.id.clone();
            if !id.trim().is_empty() && ids.insert(id.clone()) {
                let searchable_body_text =
                    searchable_body_texts.remove(&parsed_mail_identity(&message));
                unique_messages.push((message, searchable_body_text));
            }
        }
        Self::remove_message_ids(store, &ids);
        for (message, searchable_body_text) in unique_messages {
            let object = message.into_object();
            if let Some(searchable_body_text) = searchable_body_text {
                object.set_searchable_body_text(searchable_body_text);
            }
            store.append(&object);
        }
    }

    fn begin_incremental_thread_refresh(
        &self,
        store: gio::ListStore,
        previous_selection: MailSelectionAnchor,
    ) {
        self.cancel_pending_mail_apply();
        self.begin_pending_thread_models(
            store,
            PendingMailCompletion {
                generation: self.imp().load_generation.get(),
                started: Instant::now(),
                replace: false,
                previous_selection,
                is_local: false,
                appended: 0,
                has_visible_local_content: false,
                pending_search: None,
                result_kind: "incremental",
            },
        );
    }

    fn update_camel_folder_counts(
        &self,
        folder_list: &gtk::ListBox,
        folders: &[camel_mail_service::CamelFolderSummary],
        local_counts: Option<&HashMap<String, (u32, u32)>>,
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
            if data.source_uid.is_none() && data.folder_id.starts_with("custom:") {
                continue;
            }
            let (unread, _total) = if let (Some(source_uid), Some(folder_name)) =
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
                let local_key = if data.folder_id == "spam" {
                    "junk"
                } else {
                    data.folder_id.as_str()
                };
                let local = if data.folder_id == "all" {
                    local_counts
                        .map(|counts| {
                            counts
                                .values()
                                .fold((0_u32, 0_u32), |(unread, total), value| {
                                    (
                                        unread.saturating_add(value.0),
                                        total.saturating_add(value.1),
                                    )
                                })
                        })
                        .unwrap_or((0, 0))
                } else {
                    local_counts
                        .and_then(|counts| counts.get(local_key).copied())
                        .unwrap_or((0, 0))
                };
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
                    .fold(local, |(unread, total), folder| {
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
            badge.set_label(&unread.to_string());
            badge.set_visible(unread > 0);
        }

        if let Some(index) = self.imp().dynamic_folder_index.borrow().as_ref().cloned() {
            let index = index.borrow();
            let mut source_counts = HashMap::<String, (u32, u32)>::new();
            for folder in folders {
                let id = Self::dynamic_folder_key(&folder.source_uid, &folder.folder_name);
                if let Some(object) = index.get(&id) {
                    object.set_counts(folder.unread_count, folder.total_count);
                }
                let counts = source_counts.entry(folder.source_uid.clone()).or_default();
                counts.0 = counts.0.saturating_add(folder.unread_count);
                counts.1 = counts.1.saturating_add(folder.total_count);
            }
            for (source_uid, (unread, total)) in source_counts {
                let root_id = Self::dynamic_folder_key(&source_uid, "");
                if let Some(root) = index.get(&root_id) {
                    root.set_counts(unread, total);
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
            .label("Kontosynchronisation erforderlich")
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
        pgp_sign: false,
        pgp_encrypt: false,
        pgp_key: String::new(),
        smime_options: None,
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
    use crate::services::crypto_service::CryptoService;

    fn skip_rust_trivia(source: &str, mut index: usize) -> usize {
        let bytes = source.as_bytes();
        loop {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'/' {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
                index += 2;
                let mut depth = 1;
                while index < bytes.len() && depth > 0 {
                    if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
                        depth += 1;
                        index += 2;
                    } else if index + 1 < bytes.len()
                        && bytes[index] == b'*'
                        && bytes[index + 1] == b'/'
                    {
                        depth -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
                continue;
            }
            return index;
        }
    }

    fn skip_rust_literal(source: &str, index: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        if index >= bytes.len() {
            return None;
        }

        if bytes[index] == b'"' {
            let mut cursor = index + 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor = (cursor + 2).min(bytes.len());
                } else if bytes[cursor] == b'"' {
                    return Some(cursor + 1);
                } else {
                    cursor += 1;
                }
            }
            return Some(bytes.len());
        }

        if bytes[index] == b'r' {
            let mut cursor = index + 1;
            while cursor < bytes.len() && bytes[cursor] == b'#' {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'"' {
                let hashes = cursor - (index + 1);
                let mut end = cursor + 1;
                while end < bytes.len() {
                    if bytes[end] == b'"'
                        && end + 1 + hashes <= bytes.len()
                        && bytes[end + 1..end + 1 + hashes]
                            .iter()
                            .all(|byte| *byte == b'#')
                    {
                        return Some(end + 1 + hashes);
                    }
                    end += 1;
                }
                return Some(bytes.len());
            }
        }

        // Treat character literals as literals, but leave Rust lifetimes
        // (`'static`, `'a`) in the token stream.
        if bytes[index] == b'\'' {
            if index + 2 < bytes.len() && bytes[index + 2] == b'\'' {
                return Some(index + 3);
            }
            if index + 1 < bytes.len() && bytes[index + 1] == b'\\' {
                let mut cursor = index + 2;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\\' {
                        cursor = (cursor + 2).min(bytes.len());
                    } else if bytes[cursor] == b'\'' {
                        return Some(cursor + 1);
                    } else {
                        cursor += 1;
                    }
                }
                return Some(bytes.len());
            }
        }

        None
    }

    fn rust_identifier_start(byte: u8) -> bool {
        byte == b'_' || byte.is_ascii_alphabetic()
    }

    fn rust_identifier_continue(byte: u8) -> bool {
        rust_identifier_start(byte) || byte.is_ascii_digit()
    }

    fn next_rust_identifier(source: &str, mut index: usize) -> Option<(usize, usize)> {
        let bytes = source.as_bytes();
        while index < bytes.len() {
            index = skip_rust_trivia(source, index);
            if index >= bytes.len() {
                return None;
            }
            if let Some(next) = skip_rust_literal(source, index) {
                index = next;
                continue;
            }
            if rust_identifier_start(bytes[index]) {
                let start = index;
                index += 1;
                while index < bytes.len() && rust_identifier_continue(bytes[index]) {
                    index += 1;
                }
                return Some((start, index));
            }
            index += 1;
        }
        None
    }

    fn next_rust_char(source: &str, mut index: usize, wanted: u8) -> Option<usize> {
        let bytes = source.as_bytes();
        while index < bytes.len() {
            index = skip_rust_trivia(source, index);
            if index >= bytes.len() {
                return None;
            }
            if let Some(next) = skip_rust_literal(source, index) {
                index = next;
                continue;
            }
            if bytes[index] == wanted {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    fn matching_rust_brace(source: &str, open: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        let mut index = open;
        let mut depth = 0usize;
        while index < bytes.len() {
            index = skip_rust_trivia(source, index);
            if index >= bytes.len() {
                return None;
            }
            if let Some(next) = skip_rust_literal(source, index) {
                index = next;
                continue;
            }
            match bytes[index] {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn rust_function_body<'a>(source: &'a str, name: &str) -> &'a str {
        let mut cursor = 0;
        while let Some((token_start, token_end)) = next_rust_identifier(source, cursor) {
            if &source[token_start..token_end] == "fn" {
                let Some((name_start, name_end)) = next_rust_identifier(source, token_end) else {
                    break;
                };
                if &source[name_start..name_end] == name {
                    let open = next_rust_char(source, name_end, b'{')
                        .expect("function should have a body");
                    let close = matching_rust_brace(source, open)
                        .expect("function body should have balanced braces");
                    return &source[token_start..=close];
                }
                cursor = name_end;
            } else {
                cursor = token_end;
            }
        }
        panic!("function {name} should exist in source");
    }

    fn spawned_closure_body<'a>(source: &'a str, call_name: &str) -> &'a str {
        let call_start = source
            .find(call_name)
            .expect("worker call should exist in spawn closure");
        let spawn_start = source[..call_start]
            .rfind("std::thread::spawn")
            .expect("worker call should be owned by a thread spawn");
        let open =
            next_rust_char(source, spawn_start, b'{').expect("spawn closure should have a body");
        let close =
            matching_rust_brace(source, open).expect("spawn closure should have balanced braces");
        &source[spawn_start..=close]
    }

    fn compact_rust(source: &str) -> String {
        source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    #[test]
    fn removed_selection_uses_next_item_then_previous_item() {
        assert_eq!(fallback_selection_index(4, Some(2)), Some(2));
        assert_eq!(fallback_selection_index(2, Some(2)), Some(1));
        assert_eq!(fallback_selection_index(1, Some(0)), Some(0));
        assert_eq!(fallback_selection_index(0, Some(0)), None);
        assert_eq!(fallback_selection_index(3, None), Some(0));
    }

    #[test]
    fn only_the_current_request_generation_may_mutate_models() {
        assert!(generation_is_current(7, 7));
        assert!(!generation_is_current(8, 7));
        assert!(!generation_is_current(7, 8));
    }

    #[test]
    fn mail_load_worker_input_is_owned_send_sync_plain_rust_and_job_local() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_static<T: 'static>() {}

        assert_send_sync::<MailLoadWorkerInput>();
        assert_send_sync::<MailLoadSettingsSnapshot>();
        assert_send_sync::<MailStoreWorkerConfig>();
        assert_send_sync::<MailWorkerUiScope>();
        assert_static::<MailLoadWorkerInput>();
        assert_static::<MailLoadSettingsSnapshot>();
        assert_static::<MailStoreWorkerConfig>();
        assert_static::<MailWorkerUiScope>();

        let request = MailLoadRequest {
            source_uid: Some("account-a".to_string()),
            folder_name: Some("INBOX".to_string()),
            offset: 32,
            max_messages_per_folder: 64,
            graph_cursors: HashMap::from([("INBOX".to_string(), "cursor-a".to_string())]),
        };
        let first_job = MailLoadWorkerInput::from_settings_snapshot(
            request.clone(),
            MailLoadSettingsSnapshot {
                offline_mode: true,
                local_mail_directory: Some("/first/mail".to_string()),
            },
        );
        // Model a settings change before the next job is created.  The first
        // input must retain the old values even after this second snapshot.
        let second_job = MailLoadWorkerInput::from_settings_snapshot(
            request,
            MailLoadSettingsSnapshot {
                offline_mode: false,
                local_mail_directory: Some("/second/mail".to_string()),
            },
        );

        let first_returned = std::thread::spawn(move || first_job)
            .join()
            .expect("owned worker input should cross the thread boundary");
        assert!(first_returned.offline_mode);
        assert_eq!(
            first_returned.store_config.local_mail_directory.as_deref(),
            Some("/first/mail")
        );
        assert!(!second_job.offline_mode);
        assert_eq!(
            second_job.store_config.local_mail_directory.as_deref(),
            Some("/second/mail")
        );
        assert_ne!(first_returned, second_job);
    }

    #[test]
    fn mail_worker_scope_rejects_stale_generation_folder_and_request() {
        let first_folder = FolderSelection {
            folder_id: "account-a/inbox".to_string(),
            source_uid: Some("account-a".to_string()),
            folder_name: Some("Inbox".to_string()),
        };
        let second_folder = FolderSelection {
            folder_id: "account-b/inbox".to_string(),
            source_uid: Some("account-b".to_string()),
            folder_name: Some("Inbox".to_string()),
        };
        let first_request = MailLoadRequest {
            source_uid: first_folder.source_uid.clone(),
            folder_name: first_folder.folder_name.clone(),
            offset: 0,
            max_messages_per_folder: 32,
            graph_cursors: HashMap::new(),
        };
        let second_request = MailLoadRequest {
            source_uid: second_folder.source_uid.clone(),
            folder_name: second_folder.folder_name.clone(),
            ..first_request.clone()
        };
        let scope = MailWorkerUiScope {
            load_generation: 7,
            folder_selection: first_folder.clone(),
            active_request: Some(first_request.clone()),
        };

        assert!(mail_worker_scope_is_current(
            &scope,
            7,
            &first_folder,
            Some(&first_request)
        ));
        assert!(!mail_worker_scope_is_current(
            &scope,
            8,
            &first_folder,
            Some(&first_request)
        ));
        assert!(!mail_worker_scope_is_current(
            &scope,
            7,
            &second_folder,
            Some(&second_request)
        ));
        assert!(!mail_worker_scope_is_current(
            &scope,
            7,
            &first_folder,
            Some(&second_request)
        ));
    }

    #[test]
    fn mail_load_worker_boundary_has_no_settings_or_ui_handles() {
        let source = include_str!("mail_page.rs");
        let worker_source = rust_function_body(source, "load_mail_backend");

        for forbidden in [
            "AppSettings",
            "gio::",
            "gtk::",
            "glib::",
            "adw::",
            "GObject",
            "MainContext",
            "thread_default",
        ] {
            assert!(
                !worker_source.contains(forbidden),
                "load worker must stay framework-neutral: found {forbidden}"
            );
        }
        assert!(
            !compact_rust(worker_source).contains("MailStoreService::new("),
            "normal load callgraph must not resolve settings through MailStoreService::new"
        );

        let start_source = rust_function_body(source, "start_mail_load");
        let input_capture = compact_rust(start_source)
            .find("worker_input=MailLoadWorkerInput::from_main_thread(request.clone())")
            .expect("load worker input must be captured by the main-thread start path");
        let spawn_source = spawned_closure_body(start_source, "load_mail_backend");
        assert!(
            input_capture
                < compact_rust(start_source)
                    .find("std::thread::spawn")
                    .expect("load worker should be spawned after input capture"),
            "worker input must be captured before spawning the worker"
        );
        for forbidden in [
            "AppSettings",
            "gio::",
            "gtk::",
            "glib::",
            "adw::",
            "GObject",
            "MainContext",
            "thread_default",
        ] {
            assert!(
                !spawn_source.contains(forbidden),
                "load worker closure must not capture framework handles: found {forbidden}"
            );
        }
        assert!(
            !compact_rust(spawn_source).contains("MailStoreService::new("),
            "load worker closure must not call the settings-backed constructor"
        );

        let store_source = include_str!("../services/mail_store_service.rs");
        let compact_store_source = compact_rust(store_source);
        assert!(
            compact_store_source.contains("pub(crate)fnnew_worker_safe("),
            "the worker-safe constructor must remain crate-local"
        );
        assert!(
            !compact_store_source.contains("pubfnnew_worker_safe("),
            "the worker-safe constructor must not become a global API"
        );
        for constructor in ["new_worker_safe", "new_worker_safe_with_paths"] {
            let constructor_source = rust_function_body(store_source, constructor);
            for forbidden in [
                "AppSettings",
                "gio::",
                "gtk::",
                "glib::",
                "adw::",
                "GObject",
                "std::env::",
                "app_data_dir(",
            ] {
                assert!(
                    !constructor_source.contains(forbidden),
                    "worker-safe store constructor must not resolve framework/global state: {}",
                    forbidden
                );
            }
        }
    }

    #[test]
    fn mail_store_operation_workers_capture_plain_config_before_spawn() {
        let source = include_str!("mail_page.rs");
        let workers = [
            ("empty_trash_async", "MailStoreService::new_worker_safe"),
            ("apply_filters", "MailStoreService::new_worker_safe"),
            (
                "execute_mail_command",
                "MailOperationsService::new_worker_safe",
            ),
            (
                "execute_mail_batch",
                "MailOperationsService::new_worker_safe",
            ),
            (
                "execute_history_action",
                "MailOperationsService::new_worker_safe",
            ),
        ];

        for (function_name, constructor) in workers {
            let function_source = rust_function_body(source, function_name);
            let compact = compact_rust(function_source);
            let config_capture = compact
                .find("mail_store_worker_config_from_main_thread()")
                .expect("mail worker must capture a store config on the main thread");
            let spawn = compact
                .find("std::thread::spawn")
                .expect("mail worker should be spawned");
            assert!(
                config_capture < spawn,
                "{function_name} must capture its config before spawning"
            );

            let worker_source = compact_rust(spawned_closure_body(function_source, constructor));
            assert!(
                worker_source.contains("new_worker_safe(store_config)"),
                "{function_name} must use the worker-safe constructor"
            );
            for forbidden in [
                "AppSettings",
                "GSettings",
                "std::env::",
                "app_data_dir(",
                "AppData",
                "gio::",
                "gtk::",
                "glib::",
                "adw::",
                "GObject",
                "MainContext",
                "thread_default",
            ] {
                assert!(
                    !worker_source.contains(forbidden),
                    "{function_name} worker must stay framework/global-state free: found {forbidden}"
                );
            }
            assert!(!worker_source.contains("MailStoreService::new("));
            assert!(!worker_source.contains("MailOperationsService::new()"));

            let scope_capture = compact
                .find("mail_worker_ui_scope()")
                .expect("mail worker must capture its UI scope before spawning");
            assert!(
                scope_capture < spawn,
                "{function_name} must capture its UI scope before spawning"
            );
            assert!(
                compact.contains("mail_worker_ui_scope_is_current(&worker_scope)"),
                "{function_name} must gate UI delivery by the captured scope"
            );

            if constructor == "MailOperationsService::new_worker_safe" {
                let confirmed_commit = if function_name == "execute_history_action" {
                    compact
                        .find("page.imp().mail_commands.replace(history)")
                        .expect("confirmed history action must still replace its history")
                } else {
                    compact
                        .find("completed.commit")
                        .expect("confirmed operation must still commit its result")
                };
                let ui_gate = compact
                    .find("ifscope_is_current")
                    .expect("confirmed operation must gate UI delivery");
                assert!(
                    confirmed_commit < ui_gate,
                    "{function_name} must commit confirmed success before stale UI gating"
                );
            }
        }

        let filter_source = compact_rust(rust_function_body(source, "apply_filters"));
        let filter_capture = filter_source
            .find("MailFilterStore::default()")
            .expect("Apply Filters must capture its filter store before spawning");
        let filter_spawn = filter_source
            .find("std::thread::spawn")
            .expect("Apply Filters should be spawned");
        assert!(
            filter_capture < filter_spawn,
            "Apply Filters must capture MailFilterStore before spawning"
        );
        assert!(filter_source.contains("run_all_active_filters_with_store(&filter_store)"));
        assert!(!filter_source.contains("run_all_active_filters()"));
        let filter_worker = compact_rust(spawned_closure_body(
            rust_function_body(source, "apply_filters"),
            "MailStoreService::new_worker_safe",
        ));
        assert!(!filter_worker.contains("MailFilterStore::default()"));
    }

    #[test]
    fn history_workers_capture_path_before_spawn_and_keep_global_state_out() {
        let source = include_str!("mail_page.rs");
        let history_source = rust_function_body(source, "load_mail_history_async");
        let compact_history = compact_rust(history_source);
        let path_capture = compact_history
            .find("MailCommandStack::capture_persistent_path()")
            .expect("history path must be captured before spawning");
        let spawn = compact_history
            .find("std::thread::spawn")
            .expect("history load should be spawned");
        assert!(path_capture < spawn);

        let history_worker = compact_rust(spawned_closure_body(
            history_source,
            "MailCommandStack::load_persistent_from_path",
        ));
        for forbidden in [
            "AppSettings",
            "GSettings",
            "std::env::",
            "app_data_dir(",
            "AppData",
            "gio::",
            "gtk::",
            "glib::",
            "adw::",
            "GObject",
            "MainContext",
            "thread_default",
        ] {
            assert!(
                !history_worker.contains(forbidden),
                "history load worker must stay global-state free: found {forbidden}"
            );
        }
        assert!(!history_worker.contains("MailCommandStack::persistent()"));

        let persist_source = rust_function_body(source, "persist_mail_history_async");
        let persist_worker = compact_rust(spawned_closure_body(persist_source, "persist_snapshot"));
        assert!(!persist_worker.contains("std::env::"));
        assert!(!persist_worker.contains("app_data_dir("));
    }

    #[test]
    fn replacing_load_claims_folder_before_worker_and_stale_status_is_rejected() {
        let source = include_str!("mail_page.rs");
        let start_source = compact_rust(rust_function_body(source, "start_mail_load"));
        let request_claim = start_source
            .find("self.imp().active_request.replace(Some(request.clone()))")
            .expect("a replacing load must claim the requested folder immediately");
        let spawn = start_source
            .find("std::thread::spawn")
            .expect("mail load should be spawned");
        assert!(request_claim < spawn);

        for (function_name, status_fragment) in [
            ("empty_trash_async", "page.refresh_mail()"),
            ("apply_filters", "page.refresh_mail()"),
            ("execute_mail_command", "page.refresh_mail()"),
            ("execute_mail_batch", "page.refresh_mail()"),
            ("execute_history_action", "page.refresh_mail()"),
        ] {
            let function_source = compact_rust(rust_function_body(source, function_name));
            assert!(
                function_source.contains(status_fragment),
                "{function_name} must reconcile the current folder after completion"
            );
            assert!(
                function_source.contains("mail_worker_ui_scope_is_current"),
                "{function_name} must reject stale folder/generation results before UI mutation"
            );
        }

        let camel_status = compact_rust(rust_function_body(source, "format_camel_status"));
        assert!(camel_status.contains("successful_sources"));
        assert!(camel_status.contains("failed_sources"));
        assert!(camel_status.contains("warning"));
    }

    #[test]
    fn stale_mail_progress_and_result_apply_are_rejected_by_generation_gate() {
        for (current, result, accepted) in [(7, 7, true), (8, 7, false), (7, 8, false)] {
            assert_eq!(
                generation_is_current(current, result),
                accepted,
                "progress and result gates must share latest-request semantics"
            );
        }

        let source = include_str!("mail_page.rs");
        let start_source = compact_rust(rust_function_body(source, "start_mail_load"));
        let gate = "generation_is_current(page.imp().load_generation.get(),generation)";
        assert_eq!(
            start_source.matches(gate).count(),
            2,
            "the current-generation gate must protect both progress and result delivery"
        );
        assert!(
            start_source.contains(
                "ifgeneration_is_current(page.imp().load_generation.get(),generation){page.update_camel_sync_progress(&progress);"
            ),
            "stale progress must be rejected before the GTK status update"
        );
        assert!(
            start_source.contains(
                "if!generation_is_current(page.imp().load_generation.get(),generation){return;}page.apply_mail_load(request,result,replace);"
            ),
            "stale results must be rejected before the GTK model apply"
        );
    }

    #[test]
    fn worker_sort_plans_preserve_mail_order_and_direction_without_gobjects() {
        let records = vec![
            MailRootSortRecord {
                key: MailSortKey::Text("zebra".to_string(), "Zebra".to_string()),
                id: "message-c".to_string(),
                index: 2,
            },
            MailRootSortRecord {
                key: MailSortKey::Text("alpha".to_string(), "alpha".to_string()),
                id: "message-b".to_string(),
                index: 1,
            },
            MailRootSortRecord {
                key: MailSortKey::Text("alpha".to_string(), "Alpha".to_string()),
                id: "message-a".to_string(),
                index: 0,
            },
        ];

        assert_eq!(sorted_record_indices(records.clone(), false), [0, 1, 2]);
        assert_eq!(sorted_record_indices(records, true), [2, 1, 0]);
    }

    #[test]
    fn thread_sort_plan_orders_positions_before_ids() {
        let records = vec![
            ThreadMemberSortRecord {
                position: 2,
                id: "c".to_string(),
                index: 0,
            },
            ThreadMemberSortRecord {
                position: 0,
                id: "a".to_string(),
                index: 1,
            },
            ThreadMemberSortRecord {
                position: 1,
                id: "b".to_string(),
                index: 2,
            },
        ];

        assert_eq!(sorted_record_indices(records, false), [1, 2, 0]);
    }

    #[test]
    fn camel_counter_deltas_remove_known_unread_and_never_underflow() {
        let change = camel_mail_service::CamelChangeEvent {
            kind: camel_mail_service::CamelChangeKind::Changed,
            source_uid: "account-a".to_string(),
            folder_name: "INBOX".to_string(),
            old_folder_name: None,
            added_count: 2,
            removed_count: 1,
            changed_count: 0,
            recent_count: 0,
            added_uids: vec![
                "new-1".to_string(),
                "new-1".to_string(),
                "new-2".to_string(),
            ],
            removed_uids: vec!["old".to_string(), "old".to_string()],
            changed_uids: Vec::new(),
            recent_uids: Vec::new(),
        };
        let previous = HashMap::from([("old".to_string(), true)]);
        assert_eq!(camel_folder_counter_delta(&change, &previous, &[]), (-1, 1));
        assert_eq!(apply_count_delta(0, -2), 0);
        assert_eq!(apply_count_delta(u32::MAX, 2), u32::MAX);
    }

    #[test]
    fn folder_rename_and_delete_include_descendants_without_prefix_collisions() {
        assert!(folder_path_is_within("Projects/2026", "Projects"));
        assert!(!folder_path_is_within("Projects-old", "Projects"));
        assert_eq!(
            renamed_folder_path("Projects/2026/Q3", "Projects", "Archive"),
            Some("Archive/2026/Q3".to_string())
        );
        assert_eq!(
            renamed_folder_path("Projects-old", "Projects", "Archive"),
            None
        );
    }

    #[test]
    fn camel_status_keeps_failed_account_warning_visible() {
        let status = format_camel_status(&CamelMailLoad {
            messages: Vec::new(),
            folders: Vec::new(),
            configured_sources: 1,
            successful_sources: 0,
            failed_sources: 1,
            warning: Some("OAuth-Anmeldung fehlgeschlagen".to_string()),
            is_graph: false,
            search_query: None,
            graph_next_links: HashMap::new(),
        });

        assert!(status.contains("1 Konto nicht synchronisiert"));
        assert!(status.contains("OAuth-Anmeldung fehlgeschlagen"));
    }

    #[test]
    fn camel_status_explains_successful_account_without_readable_folders() {
        let status = format_camel_status(&CamelMailLoad {
            messages: Vec::new(),
            folders: Vec::new(),
            configured_sources: 1,
            successful_sources: 1,
            failed_sources: 0,
            warning: None,
            is_graph: false,
            search_query: None,
            graph_next_links: HashMap::new(),
        });

        assert!(status.contains("1 Konto synchronisiert"));
        assert!(status.contains("Keine lesbaren Mailordner gefunden"));
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
    fn message_search_query_keeps_important_separate_from_starred() {
        let important = MessageSearchQuery::parse("is:important");
        assert_eq!(important.important, Some(true));
        assert_eq!(important.flagged, None);

        let not_important = MessageSearchQuery::parse("is:not-important");
        assert_eq!(not_important.important, Some(false));
        assert!(!not_important.is_empty());
    }

    #[test]
    fn mail_sort_settings_roundtrip_important_status() {
        assert_eq!(
            MailSortField::from_setting("important"),
            MailSortField::Important
        );
        assert_eq!(MailSortField::Important.setting_value(), "important");
        assert_eq!(
            MailSortField::from_setting("starred"),
            MailSortField::Flagged
        );
    }

    #[test]
    fn mail_list_dropdown_indices_roundtrip_to_their_actions() {
        for (filter, index) in [
            (MessageFilter::All, 0),
            (MessageFilter::Unread, 1),
            (MessageFilter::Flagged, 2),
            (MessageFilter::Important, 3),
            (MessageFilter::HasAttachments, 4),
        ] {
            assert_eq!(MessageFilter::from_selector_index(index), filter);
            assert_eq!(filter.selector_index(), index);
        }

        for (field, index) in [
            (MailSortField::Date, 0),
            (MailSortField::Subject, 1),
            (MailSortField::Sender, 2),
            (MailSortField::Unread, 3),
            (MailSortField::Flagged, 4),
            (MailSortField::FollowUp, 5),
            (MailSortField::Important, 6),
            (MailSortField::Attachments, 7),
        ] {
            assert_eq!(MailPage::mail_sort_field_from_selector_index(index), field);
            assert_eq!(MailPage::mail_sort_selector_index(field), index);
        }
    }

    #[test]
    fn message_context_menu_exposes_daily_state_and_label_actions() {
        let actions = MESSAGE_CONTEXT_ACTIONS
            .iter()
            .map(|(_, action)| *action)
            .collect::<HashSet<_>>();

        for required in [
            "win.reply",
            "win.export",
            "win.mail-print",
            "win.mail-print-preview",
            "win.view-message-source",
            "win.edit-labels",
            "win.move-message",
            "win.copy-message",
            "win.mark-junk",
            "win.mark-not-junk",
            "win.archive",
            "win.delete",
        ] {
            assert!(actions.contains(required), "missing {required}");
        }
    }

    #[test]
    fn rfc822_header_block_accepts_common_line_endings() {
        assert_eq!(
            rfc822_header_block(b"From: a\r\nSubject: b\r\n\r\nBody"),
            b"From: a\r\nSubject: b"
        );
        assert_eq!(
            rfc822_header_block(b"From: a\nSubject: b\n\nBody"),
            b"From: a\nSubject: b"
        );
        assert_eq!(rfc822_header_block(b"From: a"), b"From: a");
    }

    #[test]
    fn configured_normal_headers_preserve_configuration_and_repeated_field_order() {
        let fields = vec![
            ("Received".to_string(), "first".to_string()),
            ("Subject".to_string(), "Roadmap".to_string()),
            ("received".to_string(), "second".to_string()),
            ("X-Mailer".to_string(), "Evolution".to_string()),
            ("Cc".to_string(), String::new()),
        ];
        let configuration = vec![
            ("Subject".to_string(), true),
            ("Received".to_string(), true),
            ("Cc".to_string(), true),
            ("x-evolution-mailer".to_string(), true),
            ("From".to_string(), false),
        ];

        assert_eq!(
            configured_normal_header_fields(&fields, &configuration),
            vec![
                ("Betreff".to_string(), "Roadmap".to_string()),
                ("Received".to_string(), "first".to_string()),
                ("Received".to_string(), "second".to_string()),
                ("Mailer".to_string(), "Evolution".to_string()),
            ]
        );
    }

    #[test]
    fn all_headers_request_rejects_cancelled_stale_hidden_and_wrong_message_results() {
        assert!(all_headers_request_is_current(false, 7, 7, true, true));
        assert!(!all_headers_request_is_current(true, 7, 7, true, true));
        assert!(!all_headers_request_is_current(false, 7, 8, true, true));
        assert!(!all_headers_request_is_current(false, 7, 7, false, true));
        assert!(!all_headers_request_is_current(false, 7, 7, true, false));
    }

    #[test]
    fn message_source_result_rejects_cancelled_closed_and_replaced_windows() {
        assert!(message_source_result_is_current(false, true, true));
        assert!(!message_source_result_is_current(true, true, true));
        assert!(!message_source_result_is_current(false, false, true));
        assert!(!message_source_result_is_current(false, true, false));
    }

    #[test]
    fn displayed_message_search_status_matches_evolution_find_bar_states() {
        let result = MessageSearchResult {
            generation: 3,
            backend: None,
            status: MessageSearchStatus::Found,
            query: "evolution".to_string(),
            match_count: 4,
            current_match: Some(2),
        };
        assert_eq!(displayed_message_search_status(&result), "Treffer: 4");
        assert!(displayed_message_search_has_matches(&result));

        let not_found = MessageSearchResult {
            status: MessageSearchStatus::NotFound,
            match_count: 0,
            current_match: None,
            ..result
        };
        assert_eq!(displayed_message_search_status(&not_found), "Treffer: 0");
        assert!(!displayed_message_search_has_matches(&not_found));
    }

    #[test]
    fn searchable_body_cache_is_camel_first_and_first_wins_for_duplicate_ids() {
        let camel_identity = MailMessageIdentity::from_parts(
            "same-id",
            "inbox",
            Some("<same@example.test>"),
            Some(&MailBackendLocation {
                source_uid: "account-a".to_string(),
                folder_name: "INBOX".to_string(),
                message_uid: "uid-1".to_string(),
                message_id: Some("<same@example.test>".to_string()),
            }),
        );
        let local_identity =
            MailMessageIdentity::from_parts("same-id", "inbox", Some("<same@example.test>"), None);
        let mut camel = HashMap::new();
        insert_searchable_body_text_first(&mut camel, camel_identity.clone(), "<p>Camel body</p>");
        let mut local = HashMap::new();
        insert_searchable_body_text_first(&mut local, local_identity.clone(), "<p>Local body</p>");
        insert_searchable_body_text_first(
            &mut local,
            local_identity.clone(),
            "<p>Later duplicate</p>",
        );
        local.insert(camel_identity.clone(), Arc::from("local collision"));

        let merged = merge_authoritative_searchable_body_texts(camel, local);
        assert_eq!(
            merged.get(&camel_identity).map(Arc::as_ref),
            Some("camel body")
        );
        assert_eq!(
            merged.get(&local_identity).map(Arc::as_ref),
            Some("local body")
        );
    }

    #[test]
    fn stale_crypto_result_is_rejected_even_when_message_id_is_unchanged() {
        let identity = MailMessageIdentity::from_parts("same-id", "inbox", None, None);
        let other_identity = MailMessageIdentity::from_parts("same-id", "archive", None, None);

        assert!(crypto_result_is_current(7, 7, &identity, &identity));
        assert!(!crypto_result_is_current(8, 7, &identity, &identity));
        assert!(!crypto_result_is_current(7, 7, &identity, &other_identity));
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
    fn message_search_query_supports_negated_evolution_predicates() {
        let query = MessageSearchQuery::parse(
            "-from:blocked@example.test -body:secret is:not-unread is:not-starred is:not-important is:junk -has:attachment after:2026-08-01",
        );

        assert_eq!(query.unread, Some(false));
        assert_eq!(query.flagged, Some(false));
        assert_eq!(query.important, Some(false));
        assert_eq!(query.junk, Some(true));
        assert_eq!(query.has_attachment, Some(false));
        assert!(
            query
                .negative_fields
                .contains(&(MessageSearchField::From, "blocked@example.test".to_string()))
        );
        assert!(
            query
                .negative_fields
                .contains(&(MessageSearchField::Body, "secret".to_string()))
        );
        assert!(!query.is_empty());

        let date_query = MessageSearchQuery::parse("on:2026-08-11T14:30:00Z");
        assert_eq!(date_query.date_after, NaiveDate::from_ymd_opt(2026, 8, 11));
        assert_eq!(date_query.date_before, NaiveDate::from_ymd_opt(2026, 8, 12));
    }

    #[test]
    fn graph_search_expression_keeps_only_positive_graph_predicates() {
        let query = MessageSearchQuery::parse(
            "from:alice@example.test subject:Roadmap has:attachment is:unread -spam label:team after:2026-01-01",
        );
        let expression = graph_search_expression(&query).expect("Graph terms should be present");

        assert!(expression.contains("\"from:alice@example.test\""));
        assert!(expression.contains("\"subject:roadmap\""));
        assert!(expression.contains("\"hasAttachments:true\""));
        assert!(!expression.contains("spam"));
        assert!(!expression.contains("team"));
        assert!(!expression.contains("2026-01-01"));
    }

    #[test]
    fn graph_search_expression_is_empty_for_local_only_predicates() {
        let query = MessageSearchQuery::parse("is:unread label:important after:today");

        assert_eq!(graph_search_expression(&query), None);
        assert!(!search_query_needs_body(&query));

        assert!(search_query_needs_body(&MessageSearchQuery::parse(
            "deploy"
        )));
        assert!(search_query_needs_body(&MessageSearchQuery::parse(
            "body:release"
        )));
        assert!(search_query_needs_body(&MessageSearchQuery::parse(
            "-body:secret"
        )));
    }

    #[test]
    fn searchable_body_text_is_prepared_as_lowercase_plaintext_off_ui_thread() {
        let prepared = std::thread::spawn(|| {
            prepare_searchable_body_text("<p>Body <strong>TERM</strong></p>")
        })
        .join()
        .expect("worker preparation should finish");

        assert_eq!(&*prepared, "body term");
    }

    #[test]
    fn message_body_filter_uses_only_prepared_worker_text() {
        let message = MailMessageObject::new(
            "message-body",
            "inbox",
            "Alice",
            "alice@example.test",
            "me@example.test",
            "",
            "",
            "",
            "Body search",
            None,
            Vec::new(),
            Vec::new(),
            "message-body",
            1,
            0,
            "Tue, 11 Aug 2026 14:30:00 +0000",
            "",
            "<p>Prepared later</p>",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            Vec::new(),
            None,
            Vec::new(),
        );
        let query = MessageSearchQuery::parse("body:prepared");

        assert!(!message_matches_filter(
            &message,
            &query,
            MessageFilter::All,
            &FolderSelection::all(),
            QuickSearchScope::CurrentFolder,
        ));

        message.set_searchable_body_text(Arc::from("prepared later"));
        assert!(message_matches_filter(
            &message,
            &query,
            MessageFilter::All,
            &FolderSelection::all(),
            QuickSearchScope::CurrentFolder,
        ));
    }

    #[test]
    fn message_search_filter_evaluates_negated_fields_and_junk_state() {
        let message = MailMessageObject::new(
            "message-1",
            "junk",
            "Alice",
            "alice@example.test",
            "me@example.test",
            "",
            "",
            "",
            "Roadmap",
            None,
            Vec::new(),
            Vec::new(),
            "message-1",
            1,
            0,
            "Tue, 11 Aug 2026 14:30:00 +0000",
            "The project is ready",
            "<p>The project is ready</p>",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            Vec::new(),
            None,
            Vec::new(),
        );
        let query = MessageSearchQuery::parse(
            "-from:blocked@example.test -body:secret is:not-unread is:not-starred is:not-important is:junk -has:attachment after:2026-08-01",
        );

        assert!(message_matches_filter(
            &message,
            &query,
            MessageFilter::All,
            &FolderSelection::all(),
            QuickSearchScope::CurrentFolder,
        ));
    }

    #[test]
    fn message_search_query_supports_quick_search_field_and_scope() {
        let subject_query =
            MessageSearchQuery::parse_with_field_scope("Meeting", QuickSearchField::Subject);
        assert_eq!(subject_query.subject.as_deref(), Some("meeting"));
        assert!(subject_query.text_terms.is_empty());

        let sender_query =
            MessageSearchQuery::parse_with_field_scope("Alice", QuickSearchField::Sender);
        assert_eq!(sender_query.from.as_deref(), Some("alice"));
        assert!(sender_query.text_terms.is_empty());

        let recipient_query =
            MessageSearchQuery::parse_with_field_scope("Bob", QuickSearchField::Recipient);
        assert_eq!(recipient_query.to.as_deref(), Some("bob"));
        assert!(recipient_query.text_terms.is_empty());

        let body_query =
            MessageSearchQuery::parse_with_field_scope("Secret", QuickSearchField::Body);
        assert_eq!(body_query.body.as_deref(), Some("secret"));
        assert!(body_query.text_terms.is_empty());

        assert_eq!(QuickSearchField::from_selector_index(0), QuickSearchField::AllFields);
        assert_eq!(QuickSearchField::from_selector_index(1), QuickSearchField::Subject);
        assert_eq!(QuickSearchField::from_selector_index(2), QuickSearchField::Sender);
        assert_eq!(QuickSearchField::from_selector_index(3), QuickSearchField::Recipient);
        assert_eq!(QuickSearchField::from_selector_index(4), QuickSearchField::Body);

        assert_eq!(QuickSearchScope::from_selector_index(0), QuickSearchScope::CurrentFolder);
        assert_eq!(QuickSearchScope::from_selector_index(1), QuickSearchScope::CurrentAccount);
        assert_eq!(QuickSearchScope::from_selector_index(2), QuickSearchScope::AllAccounts);
    }

    #[test]
    fn trash_predicate_matches_builtin_and_provider_deleted_folders() {
        let local = MailMessageObject::new(
            "trash-local",
            "trash",
            "Alice",
            "alice@example.test",
            "me@example.test",
            "",
            "",
            "",
            "Deleted",
            None,
            Vec::new(),
            Vec::new(),
            "trash-local",
            1,
            0,
            "Tue, 11 Aug 2026 14:30:00 +0000",
            "",
            "",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            Vec::new(),
            None,
            Vec::new(),
        );
        assert!(message_is_trash(&local));

        let provider = MailMessageObject::new(
            "trash-provider",
            "Deleted Items",
            "Alice",
            "alice@example.test",
            "me@example.test",
            "",
            "",
            "",
            "Deleted",
            None,
            Vec::new(),
            Vec::new(),
            "trash-provider",
            1,
            0,
            "Tue, 11 Aug 2026 14:30:00 +0000",
            "",
            "",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            Vec::new(),
            Some(MailBackendLocation {
                source_uid: "source-1".to_string(),
                folder_name: "Deleted Items".to_string(),
                message_uid: "uid-1".to_string(),
                message_id: None,
            }),
            Vec::new(),
        );
        assert!(message_is_trash(&provider));
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
        assert_eq!(mail_row_density_metrics("compact"), (0, 1));
        assert_eq!(mail_row_density_metrics("spacious"), (7, 14));
        assert_eq!(mail_row_density_metrics("unknown"), (3, 6));
    }

    #[test]
    fn mail_row_indicators_conversation_counts_and_zebra_order_stay_compact() {
        assert_eq!(
            mail_row_status_icon(true, false, false),
            ("mail-unread-symbolic", "Ungelesen")
        );
        assert_eq!(
            mail_row_status_icon(false, false, false),
            ("mail-read-symbolic", "Gelesen")
        );
        assert_eq!(
            mail_row_status_icon(false, true, false),
            ("mail-replied-symbolic", "Beantwortet")
        );
        assert_eq!(
            mail_row_status_icon(false, false, true),
            ("mail-forwarded-symbolic", "Weitergeleitet")
        );
        assert!(mail_row_expander_visible(2, 0));
        assert!(!mail_row_expander_visible(1, 0));
        assert!(!mail_row_expander_visible(2, 1));
        assert_eq!(visible_thread_size(true, 4), 4);
        assert_eq!(visible_thread_size(false, 4), 1);
        assert!(mail_row_is_shaded(0));
        assert!(!mail_row_is_shaded(1));
        assert!(mail_row_is_shaded(2));

        let cal_attachment = crate::models::mail_message::AttachmentInfo {
            filename: "meeting.ics".to_string(),
            size_formatted: "1 KB".to_string(),
            mime_type: "text/calendar".to_string(),
            content_id: None,
            content: std::sync::Arc::from([]),
        };
        assert_eq!(
            mail_row_attachment_info(&[cal_attachment]),
            Some(("x-office-calendar-symbolic", "Termineinladung (.ics)"))
        );

        let vcard_attachment = crate::models::mail_message::AttachmentInfo {
            filename: "contact.vcf".to_string(),
            size_formatted: "500 B".to_string(),
            mime_type: "text/vcard".to_string(),
            content_id: None,
            content: std::sync::Arc::from([]),
        };
        assert_eq!(
            mail_row_attachment_info(&[vcard_attachment]),
            Some(("x-office-address-book-symbolic", "Kontaktkarte (.vcf)"))
        );

        let file_attachment = crate::models::mail_message::AttachmentInfo {
            filename: "document.pdf".to_string(),
            size_formatted: "2 MB".to_string(),
            mime_type: "application/pdf".to_string(),
            content_id: None,
            content: std::sync::Arc::from([]),
        };
        assert_eq!(
            mail_row_attachment_info(&[file_attachment]),
            Some(("mail-attachment-symbolic", "Anhang"))
        );
        assert_eq!(mail_row_attachment_info(&[]), None);
    }

    #[test]
    fn folder_tree_indent_is_reduced_without_flattening_depth() {
        assert_eq!(compact_folder_indent(0), 1);
        assert_eq!(compact_folder_indent(1), 13);
        assert_eq!(compact_folder_indent(2), 25);
    }

    #[test]
    fn unread_badges_never_fall_back_to_total_counts() {
        assert_eq!(unread_badge_state(0), (String::new(), false));
        assert_eq!(unread_badge_state(7), ("7".to_string(), true));
    }

    #[test]
    fn relative_navigation_handles_empty_unselected_and_boundaries() {
        assert_eq!(relative_message_index(0, None, 1), None);
        assert_eq!(relative_message_index(3, None, 1), Some(0));
        assert_eq!(relative_message_index(3, None, -1), Some(2));
        assert_eq!(relative_message_index(3, Some(0), -1), None);
        assert_eq!(relative_message_index(3, Some(2), 1), None);
        assert_eq!(relative_message_index(3, Some(1), 1), Some(2));
        assert_eq!(relative_message_index(3, Some(1), -1), Some(0));
    }

    #[test]
    fn unread_navigation_wraps_and_never_selects_a_read_row() {
        let unread = [false, true, false, true];
        assert_eq!(matching_message_index(&unread, None, true), Some(1));
        assert_eq!(matching_message_index(&unread, None, false), Some(3));
        assert_eq!(matching_message_index(&unread, Some(1), true), Some(3));
        assert_eq!(matching_message_index(&unread, Some(3), true), Some(1));
        assert_eq!(matching_message_index(&unread, Some(1), false), Some(3));
        assert_eq!(matching_message_index(&[false, false], Some(0), true), None);
        assert_eq!(matching_message_index(&[], None, true), None);
    }

    #[test]
    fn routine_security_line_is_hidden_but_crypto_status_is_visible() {
        let ordinary = CryptoService::verify_signature(b"ordinary message");
        assert!(!crypto_details_are_visible(&ordinary));

        let valid_signature = crypto_service::MailCryptoDetails {
            protocol: CryptoProtocol::OpenPgp,
            signature_status: SignatureStatus::ValidGood,
            encryption_status: EncryptionStatus::NotEncrypted,
            signer_identity: Some("Alice <alice@example.test>".to_string()),
            key_fingerprint: None,
        };
        assert!(crypto_details_are_visible(&valid_signature));
        assert!(!crypto_status_visible(false, true));
        assert!(crypto_status_visible(true, true));
        assert!(crypto_status_visible(false, false));
    }

    #[test]
    fn crypto_preflight_skips_normal_mail_and_detects_supported_envelopes() {
        assert!(!raw_mime_may_contain_crypto(
            b"Content-Type: text/html\r\n\r\n<p>ordinary message</p>"
        ));
        assert!(raw_mime_may_contain_crypto(
            b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\""
        ));
        assert!(raw_mime_may_contain_crypto(
            b"Content-Type: application/pkcs7-mime; smime-type=enveloped-data"
        ));
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

    #[test]
    fn folder_selection_normalizes_supported_default_folders() {
        assert_eq!(FolderSelection::from_folder_id("inbox").folder_id, "inbox");
        assert_eq!(FolderSelection::from_folder_id("all").folder_id, "all");
        assert_eq!(FolderSelection::from_folder_id("spam").folder_id, "spam");
        assert_eq!(
            FolderSelection::from_folder_id("provider-specific").folder_id,
            "inbox"
        );
    }

    #[test]
    fn folder_selection_can_target_an_entire_mail_account() {
        let selection = FolderSelection {
            folder_id: "account:source-1".to_string(),
            source_uid: Some("source-1".to_string()),
            folder_name: None,
        };

        assert!(selection.is_remote());
        assert_eq!(selection.source_uid.as_deref(), Some("source-1"));
        assert!(selection.folder_name.is_none());
    }

    #[test]
    fn visible_source_labels_never_expose_backend_implementation_names() {
        assert_eq!(MailPage::localize_source_name("EDS"), "Mailkonto");
        assert_eq!(MailPage::localize_source_name("Camel"), "Mailkonto");
        assert_eq!(
            MailPage::localize_source_name("alice@example.test"),
            "alice@example.test"
        );
    }

    #[test]
    fn local_folder_rows_keep_custom_folder_names_readable() {
        assert_eq!(
            MailPage::local_folder_label("custom:Projects/2026"),
            "Projects / 2026"
        );
        assert_eq!(MailPage::local_folder_label("archive"), "archive");
        assert!(MailPage::is_builtin_folder_id("junk"));
        assert!(!MailPage::is_builtin_folder_id("custom:Projects"));
    }

    #[test]
    fn local_folder_tree_adds_parents_and_honors_collapsed_ancestors() {
        let mut ids = HashSet::new();
        MailPage::add_local_folder_tree_ids(&mut ids, "custom:Projects/2026/Invoices");

        assert!(ids.contains("custom:Projects"));
        assert!(ids.contains("custom:Projects/2026"));
        assert!(ids.contains("custom:Projects/2026/Invoices"));
        assert_eq!(
            MailPage::local_folder_display_label("custom:Projects/2026/Invoices"),
            "Invoices"
        );
        assert_eq!(
            MailPage::local_folder_depth("custom:Projects/2026/Invoices"),
            2
        );

        let collapsed = HashSet::from(["custom:Projects".to_string()]);
        assert!(MailPage::local_folder_has_collapsed_parent(
            "custom:Projects/2026/Invoices",
            &collapsed
        ));
        assert!(!MailPage::local_folder_has_collapsed_parent(
            "custom:Projects",
            &collapsed
        ));
    }
}
