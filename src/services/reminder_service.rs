// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable calendar/task reminders.
//
// The iCalendar component remains the source of truth for the alarm itself.
// This service only stores delivery/snooze state owned by this application so
// that a reminder is not shown again on every polling pass.  EDS-backed
// calendars can use the same API once the native Linux watcher is connected;
// the portable fallback currently consumes the local CalendarSnapshot.

use chrono::{Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicI64, Ordering},
};

use super::calendar_store_service::{
    CalendarEventRecord, CalendarSnapshot, CalendarStoreService, TaskRecord,
};
use super::ical_time_service::{property_local_datetime, property_local_timestamp};
use super::recurrence_service::{
    parse_ical_date as parse_shared_ical_date, parse_recurrence_rule, recurrence_occurs_on,
};

const CURRENT_VERSION: u32 = 2;
const DEFAULT_ALL_DAY_HOUR: u32 = 9;
const MAX_LATE_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_COMPONENT_SCAN_LINES: usize = 20_000;
const MAX_ALARM_REPETITIONS: u32 = 4_096;
const MAX_REMINDER_URI_BYTES: usize = 2_048;
const MAX_REMINDER_UID_BYTES: usize = 256;
const MAX_REMINDER_KEY_BYTES: usize = 4_096;
const MAX_ACTION_TARGET_BYTES: usize = 8_192;
const MAX_CLAIM_TOKEN_BYTES: usize = 128;
const MAX_CLAIM_SECONDS: i64 = 5 * 60;

static REMINDER_STATE_LOCK: Mutex<()> = Mutex::new(());
static REMINDER_SEQUENCE: AtomicI64 = AtomicI64::new(0);
static REMINDER_PROCESS_NONCE: OnceLock<u128> = OnceLock::new();

fn reminder_process_nonce() -> u128 {
    *REMINDER_PROCESS_NONCE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    })
}

/// Capture settings on the GTK thread, then construct the calendar backend
/// from plain values inside a worker.  `CalendarStoreService::new()` resolves
/// GSettings for legacy callers; reminder runtime workers must not do that.
pub(crate) fn calendar_store_for_worker(
    local_calendar_directory: Option<String>,
) -> CalendarStoreService {
    if let Some(path) = std::env::var_os("MERIDIAN_ICAL_DIR").or_else(|| std::env::var_os("EVOLUTION_GTK4_ICAL_DIR")) {
        return CalendarStoreService::with_calendar_dir(path);
    }
    if let Some(path) = local_calendar_directory
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
    {
        return if super::is_app_owned_store_path(&path) {
            CalendarStoreService::with_writable_calendar_dir(path)
        } else {
            CalendarStoreService::with_calendar_dir(path)
        };
    }
    CalendarStoreService::with_writable_calendar_dir(super::app_data_dir().join("ical"))
}

/// Helper to construct a contact store for reminder workers without reading GSettings off-thread.
pub(crate) fn contact_store_for_worker(
    local_contact_directory: Option<String>,
) -> super::contact_store_service::ContactStoreService {
    if let Some(path) = std::env::var_os("MERIDIAN_VCARD_DIR").or_else(|| std::env::var_os("EVOLUTION_GTK4_VCARD_DIR")) {
        return super::contact_store_service::ContactStoreService::with_contact_dir(path);
    }
    if let Some(path) = local_contact_directory
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
    {
        return if super::is_app_owned_store_path(&path) {
            super::contact_store_service::ContactStoreService::with_writable_contact_dir(path)
        } else {
            super::contact_store_service::ContactStoreService::with_contact_dir(path)
        };
    }
    super::contact_store_service::ContactStoreService::with_writable_contact_dir(
        super::app_data_dir().join("vcards"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderKind {
    Event,
    Task,
    Memo,
    Contact,
}

/// The component kind is kept separately from `ReminderKind` so the URI
/// contract can distinguish the Evolution component types even when a
/// caller only needs the notification category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderComponentKind {
    Event,
    Task,
    Memo,
    Contact,
}

impl ReminderComponentKind {
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Event => "calendar",
            Self::Task => "task",
            Self::Memo => "memo",
            Self::Contact => "contact",
        }
    }

    fn from_scheme(value: &str) -> Option<Self> {
        match value {
            "calendar" => Some(Self::Event),
            "task" => Some(Self::Task),
            "memo" => Some(Self::Memo),
            "contact" => Some(Self::Contact),
            _ => None,
        }
    }
}

impl From<ReminderComponentKind> for ReminderKind {
    fn from(value: ReminderComponentKind) -> Self {
        match value {
            ReminderComponentKind::Event => Self::Event,
            ReminderComponentKind::Task => Self::Task,
            ReminderComponentKind::Memo => Self::Memo,
            ReminderComponentKind::Contact => Self::Contact,
        }
    }
}

/// A validated, source-scoped component identity.  It is deliberately a
/// value type so notification actions, command-line opening and page routing
/// all consume exactly the same contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderIdentity {
    pub kind: ReminderComponentKind,
    pub source_uid: String,
    pub component_uid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderIdentityError {
    MalformedUri,
    UnknownScheme,
    MissingParameter,
    DuplicateParameter,
    UnknownParameter,
    EmptyValue,
    InvalidPercentEncoding,
    InvalidUtf8,
    NonCanonicalEncoding,
    ControlCharacter,
    Traversal,
    Oversized,
    InvalidActionTarget,
    InvalidReminderKey,
    StaleAction,
}

impl std::fmt::Display for ReminderIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MalformedUri => "invalid reminder URI",
            Self::UnknownScheme => "unknown reminder scheme",
            Self::MissingParameter => "required URI parameter missing",
            Self::DuplicateParameter => "duplicate URI parameter",
            Self::UnknownParameter => "unknown URI parameter",
            Self::EmptyValue => "empty reminder identity",
            Self::InvalidPercentEncoding => "invalid percent encoding",
            Self::InvalidUtf8 => "invalid UTF-8 encoding",
            Self::NonCanonicalEncoding => "non-canonical percent encoding",
            Self::ControlCharacter => "control character in reminder identity",
            Self::Traversal => "traversal in reminder identity",
            Self::Oversized => "reminder identity is oversized",
            Self::InvalidActionTarget => "invalid reminder action target",
            Self::InvalidReminderKey => "invalid reminder key",
            Self::StaleAction => "stale reminder action",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ReminderIdentityError {}

impl ReminderIdentity {
    pub fn new(
        kind: ReminderComponentKind,
        source_uid: impl Into<String>,
        component_uid: impl Into<String>,
    ) -> Result<Self, ReminderIdentityError> {
        let source_uid = source_uid.into();
        let component_uid = component_uid.into();
        validate_reminder_uid(&source_uid)?;
        validate_reminder_uid(&component_uid)?;
        Ok(Self {
            kind,
            source_uid,
            component_uid,
        })
    }

    pub fn parse(uri: &str) -> Result<Self, ReminderIdentityError> {
        parse_reminder_deep_link(uri)
    }

    pub fn to_uri(&self) -> String {
        let uri = format!(
            "{}:///?source-uid={}&comp-uid={}",
            self.kind.scheme(),
            percent_encode(&self.source_uid),
            percent_encode(&self.component_uid),
        );
        debug_assert!(uri.len() <= MAX_REMINDER_URI_BYTES);
        uri
    }
}

fn validate_reminder_uid(value: &str) -> Result<(), ReminderIdentityError> {
    if value.is_empty() {
        return Err(ReminderIdentityError::EmptyValue);
    }
    if value.len() > MAX_REMINDER_UID_BYTES {
        return Err(ReminderIdentityError::Oversized);
    }
    if value != value.trim()
        || value
            .chars()
            .any(|character| character.is_control() || character == '\u{7f}')
    {
        return Err(ReminderIdentityError::ControlCharacter);
    }
    if value
        .split(['/', '\\'])
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(ReminderIdentityError::Traversal);
    }
    Ok(())
}

fn validate_claim_token(value: &str) -> Result<(), ReminderIdentityError> {
    if value.is_empty() || value.len() > MAX_CLAIM_TOKEN_BYTES {
        return Err(ReminderIdentityError::InvalidActionTarget);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(ReminderIdentityError::InvalidActionTarget);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmRelation {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmAction {
    Display,
    Audio,
    Email,
    Procedure,
    Unknown(String),
}

impl AlarmAction {
    pub fn from_value(value: &str) -> Self {
        let trimmed = value.trim();
        match trimmed.to_ascii_uppercase().as_str() {
            "DISPLAY" => Self::Display,
            "AUDIO" => Self::Audio,
            "EMAIL" => Self::Email,
            "PROCEDURE" => Self::Procedure,
            _ => Self::Unknown(trimmed.to_string()),
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Display => "DISPLAY",
            Self::Audio => "AUDIO",
            Self::Email => "EMAIL",
            Self::Procedure => "PROCEDURE",
            Self::Unknown(value) => value,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Display => "Display".to_string(),
            Self::Audio => "Sound".to_string(),
            Self::Email => "Email".to_string(),
            Self::Procedure => "Procedure".to_string(),
            Self::Unknown(value) if value.is_empty() => "Unknown action".to_string(),
            Self::Unknown(value) => format!("Unknown action ({value})"),
        }
    }

    /// Imported AUDIO/EMAIL/PROCEDURE alarms are data only.  The portable
    /// client deliberately has one executable notification path: DISPLAY.
    pub fn is_safe_to_notify(&self) -> bool {
        matches!(self, Self::Display)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlarmDuration {
    pub seconds: i64,
    raw: Option<String>,
    raw_seconds: Option<i64>,
}

impl AlarmDuration {
    pub fn new(seconds: i64) -> Self {
        Self {
            seconds,
            raw: None,
            raw_seconds: None,
        }
    }

    fn from_ical(value: &str) -> Option<Self> {
        let seconds = parse_ical_duration(value)?;
        Some(Self {
            seconds,
            raw: Some(value.trim().to_string()),
            raw_seconds: Some(seconds),
        })
    }

    fn to_ical(&self) -> String {
        if self.raw_seconds == Some(self.seconds) {
            if let Some(raw) = self.raw.as_deref() {
                return raw.to_string();
            }
        }
        format_ical_duration(self.seconds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmTrigger {
    Relative {
        relation: AlarmRelation,
        offset_seconds: i64,
        property_prefix: String,
    },
    Absolute {
        property_prefix: String,
        value: String,
    },
    Unknown {
        property_prefix: String,
        value: String,
    },
}

impl AlarmTrigger {
    pub fn relative(relation: AlarmRelation, offset_seconds: i64) -> Self {
        Self::relative_with_property_prefix(
            relation,
            offset_seconds,
            if relation == AlarmRelation::End {
                "TRIGGER;RELATED=END"
            } else {
                "TRIGGER"
            },
        )
    }

    pub fn relative_with_property_prefix(
        relation: AlarmRelation,
        offset_seconds: i64,
        property_prefix: impl Into<String>,
    ) -> Self {
        Self::Relative {
            relation,
            offset_seconds,
            property_prefix: property_prefix.into(),
        }
    }

    pub fn absolute(value: impl Into<String>) -> Self {
        Self::absolute_with_property_prefix("TRIGGER;VALUE=DATE-TIME", value)
    }

    pub fn absolute_with_property_prefix(
        property_prefix: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self::Absolute {
            property_prefix: property_prefix.into(),
            value: value.into(),
        }
    }

    pub fn relation(&self) -> Option<AlarmRelation> {
        match self {
            Self::Relative { relation, .. } => Some(*relation),
            _ => None,
        }
    }

    pub fn offset_seconds(&self) -> Option<i64> {
        match self {
            Self::Relative { offset_seconds, .. } => Some(*offset_seconds),
            _ => None,
        }
    }

    pub fn absolute_value(&self) -> Option<&str> {
        match self {
            Self::Absolute { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn property_prefix(&self) -> Option<&str> {
        match self {
            Self::Relative {
                property_prefix, ..
            }
            | Self::Absolute {
                property_prefix, ..
            }
            | Self::Unknown {
                property_prefix, ..
            } => Some(property_prefix),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Relative {
                relation,
                offset_seconds,
                ..
            } => {
                let seconds = offset_seconds.unsigned_abs();
                let direction = if *offset_seconds < 0 { "before" } else { "after" };
                let anchor = match relation {
                    AlarmRelation::Start => "start",
                    AlarmRelation::End => "end",
                };
                format!(
                    "{} {} {}",
                    format_duration_for_display(seconds),
                    direction,
                    anchor
                )
            }
            Self::Absolute { value, .. } => format!("absolute {value}"),
            Self::Unknown { .. } => "unknown trigger".to_string(),
        }
    }

    fn to_line(&self) -> String {
        match self {
            Self::Relative {
                relation,
                offset_seconds,
                property_prefix,
            } => {
                let mut property_prefix = property_prefix.clone();
                if property_prefix.split(';').skip(1).any(|parameter| {
                    parameter.split_once('=').is_some_and(|(name, value)| {
                        name.trim().eq_ignore_ascii_case("VALUE")
                            && value
                                .trim()
                                .trim_matches('"')
                                .eq_ignore_ascii_case("DATE-TIME")
                    })
                }) {
                    property_prefix = remove_trigger_parameter(&property_prefix, "VALUE");
                }
                let existing_relation = property_prefix.split(';').skip(1).find_map(|parameter| {
                    let (name, value) = parameter.split_once('=')?;
                    name.trim().eq_ignore_ascii_case("RELATED").then(|| {
                        if value.trim().trim_matches('"').eq_ignore_ascii_case("END") {
                            AlarmRelation::End
                        } else {
                            AlarmRelation::Start
                        }
                    })
                });
                let prefix = if existing_relation == Some(*relation)
                    || (existing_relation.is_none() && *relation == AlarmRelation::Start)
                {
                    property_prefix
                } else {
                    let prefix = remove_trigger_parameter(&property_prefix, "RELATED");
                    if *relation == AlarmRelation::End {
                        format!("{prefix};RELATED=END")
                    } else {
                        prefix
                    }
                };
                format!("{prefix}:{}", format_ical_duration(*offset_seconds))
            }
            Self::Absolute {
                property_prefix,
                value,
            }
            | Self::Unknown {
                property_prefix,
                value,
            } => format!("{property_prefix}:{value}"),
        }
    }
}

/// A portable, ordered VALARM.  `properties` contains every unfolded
/// property from the source alarm.  Known properties are rendered from the
/// typed fields while unrecognized properties stay in their original order.
/// This is intentionally a value type so editors can stage changes and only
/// replace the component after a successful save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub action: AlarmAction,
    pub trigger: AlarmTrigger,
    pub repeat: Option<u32>,
    pub duration: Option<AlarmDuration>,
    pub message: Option<String>,
    pub attachments: Vec<String>,
    pub attendees: Vec<String>,
    raw_properties: Vec<String>,
    raw_attachment_values: Vec<String>,
    raw_attendee_values: Vec<String>,
    original_action: Option<AlarmAction>,
    original_trigger: Option<AlarmTrigger>,
    original_repeat: Option<Option<u32>>,
    original_duration: Option<Option<AlarmDuration>>,
    original_message: Option<Option<String>>,
}

impl Alarm {
    pub fn new_display_relative(
        relation: AlarmRelation,
        offset_seconds: i64,
        message: Option<String>,
    ) -> Self {
        Self {
            action: AlarmAction::Display,
            trigger: AlarmTrigger::relative(relation, offset_seconds),
            repeat: None,
            duration: None,
            message,
            attachments: Vec::new(),
            attendees: Vec::new(),
            raw_properties: Vec::new(),
            raw_attachment_values: Vec::new(),
            raw_attendee_values: Vec::new(),
            original_action: None,
            original_trigger: None,
            original_repeat: None,
            original_duration: None,
            original_message: None,
        }
    }

    pub fn is_safe_to_notify(&self) -> bool {
        self.action.is_safe_to_notify()
    }

    pub fn label(&self) -> String {
        format!("{} {}", self.action.label(), self.trigger.label())
    }

    pub(crate) fn apply_default_display_message(&mut self, summary: &str) {
        if self.raw_properties.is_empty()
            && matches!(self.action, AlarmAction::Display)
            && self
                .message
                .as_deref()
                .is_none_or(|message| message.trim().is_empty())
            && !summary.trim().is_empty()
        {
            self.message = Some(summary.trim().to_string());
        }
    }

    fn to_ical_lines(&self) -> Vec<String> {
        let mut lines = vec!["BEGIN:VALARM".to_string()];
        let mut action_written = false;
        let mut trigger_written = false;
        let mut repeat_written = false;
        let mut duration_written = false;
        let mut message_written = false;
        let mut attachments_written = false;
        let mut attendees_written = false;

        let action_changed = self.original_action.as_ref() != Some(&self.action);
        let trigger_changed = self.original_trigger.as_ref() != Some(&self.trigger);
        let repeat_changed = self.original_repeat.as_ref() != Some(&self.repeat);
        let duration_changed = self.original_duration.as_ref() != Some(&self.duration);
        let message_changed = self.original_message.as_ref() != Some(&self.message);
        let attachments_changed = self.attachments != self.raw_attachment_values;
        let attendees_changed = self.attendees != self.raw_attendee_values;

        for raw in &self.raw_properties {
            let name = property_name(raw);
            match name.as_str() {
                "ACTION" => {
                    if !action_changed {
                        lines.push(raw.clone());
                        action_written = true;
                    } else if !action_written {
                        if !self.action.value().is_empty() {
                            lines.push(format!("ACTION:{}", self.action.value()));
                        }
                        action_written = true;
                    }
                }
                "TRIGGER" => {
                    if !trigger_changed {
                        lines.push(raw.clone());
                        trigger_written = true;
                    } else if !trigger_written {
                        if !trigger_is_empty(&self.trigger) {
                            lines.push(self.trigger.to_line());
                        }
                        trigger_written = true;
                    }
                }
                "REPEAT" => {
                    if !repeat_changed {
                        lines.push(raw.clone());
                        repeat_written = true;
                    } else if !repeat_written {
                        if let Some(repeat) = self.repeat {
                            lines.push(format!("REPEAT:{repeat}"));
                        }
                        repeat_written = true;
                    }
                }
                "DURATION" => {
                    if !duration_changed {
                        lines.push(raw.clone());
                        duration_written = true;
                    } else if !duration_written {
                        if let Some(duration) = &self.duration {
                            lines.push(format!("DURATION:{}", duration.to_ical()));
                        }
                        duration_written = true;
                    }
                }
                "DESCRIPTION" => {
                    if !message_changed {
                        lines.push(raw.clone());
                        message_written = true;
                    } else if !message_written {
                        if let Some(message) =
                            self.message.as_deref().filter(|value| !value.is_empty())
                        {
                            lines.push(format!("DESCRIPTION:{}", escape_ical_text(message)));
                        }
                        message_written = true;
                    }
                }
                "ATTACH" if !attachments_changed => {
                    lines.push(raw.clone());
                    attachments_written = true;
                }
                "ATTACH" if !attachments_written => {
                    lines.extend(self.render_attachment_values());
                    attachments_written = true;
                }
                "ATTACH" => {}
                "ATTENDEE" if !attendees_changed => {
                    lines.push(raw.clone());
                    attendees_written = true;
                }
                "ATTENDEE" if !attendees_written => {
                    lines.extend(self.render_attendee_values());
                    attendees_written = true;
                }
                "ATTENDEE" => {}
                _ => lines.push(raw.clone()),
            }
        }

        if !action_written && !self.action.value().is_empty() {
            lines.push(format!("ACTION:{}", self.action.value()));
        }
        if !trigger_written && !trigger_is_empty(&self.trigger) {
            lines.push(self.trigger.to_line());
        }
        if !repeat_written {
            if let Some(repeat) = self.repeat {
                lines.push(format!("REPEAT:{repeat}"));
            }
        }
        if !duration_written {
            if let Some(duration) = &self.duration {
                lines.push(format!("DURATION:{}", duration.to_ical()));
            }
        }
        if !message_written {
            if let Some(message) = self.message.as_deref().filter(|value| !value.is_empty()) {
                lines.push(format!("DESCRIPTION:{}", escape_ical_text(message)));
            }
        }
        if !attachments_written && !self.attachments.is_empty() {
            lines.extend(self.render_attachment_values());
        }
        if !attendees_written && !self.attendees.is_empty() {
            lines.extend(self.render_attendee_values());
        }
        lines.push("END:VALARM".to_string());
        lines
    }

    fn render_attachment_values(&self) -> Vec<String> {
        let raw_properties = self
            .raw_properties
            .iter()
            .filter(|line| property_name(line) == "ATTACH")
            .collect::<Vec<_>>();
        let mut used = vec![false; self.raw_attachment_values.len()];
        self.attachments
            .iter()
            .map(|value| {
                let matching = self
                    .raw_attachment_values
                    .iter()
                    .enumerate()
                    .find(|(index, candidate)| !used[*index] && *candidate == value);
                if let Some((index, _)) = matching {
                    used[index] = true;
                    raw_properties
                        .get(index)
                        .map(|line| (*line).clone())
                        .unwrap_or_else(|| {
                            format!("ATTACH;VALUE=URI:{}", serialize_attachment_value(value))
                        })
                } else {
                    format!("ATTACH;VALUE=URI:{}", serialize_attachment_value(value))
                }
            })
            .collect()
    }

    fn render_attendee_values(&self) -> Vec<String> {
        let raw_properties = self
            .raw_properties
            .iter()
            .filter(|line| property_name(line) == "ATTENDEE")
            .collect::<Vec<_>>();
        let mut used = vec![false; self.raw_attendee_values.len()];
        self.attendees
            .iter()
            .map(|value| {
                let matching = self
                    .raw_attendee_values
                    .iter()
                    .enumerate()
                    .find(|(index, candidate)| !used[*index] && *candidate == value);
                if let Some((index, _)) = matching {
                    used[index] = true;
                    raw_properties
                        .get(index)
                        .map(|line| (*line).clone())
                        .unwrap_or_else(|| format!("ATTENDEE:{value}"))
                } else {
                    format!("ATTENDEE:{value}")
                }
            })
            .collect()
    }
}

/// Parse all top-level VALARM components in source order.  The parser does
/// not filter on ACTION: unsupported actions remain available to the editor
/// and are serialized again, but the scheduler only consumes DISPLAY.
pub fn parse_alarms(component: &str) -> Vec<Alarm> {
    let lines = unfold_lines(component);
    let mut component_depth = 0usize;
    let mut current: Option<Vec<String>> = None;
    let mut alarms = Vec::new();

    for line in lines.into_iter().take(MAX_COMPONENT_SCAN_LINES) {
        if is_calendar_component_begin(&line) {
            component_depth = component_depth.saturating_add(1);
            continue;
        }
        if line.eq_ignore_ascii_case("BEGIN:VALARM") && component_depth == 1 {
            current = Some(Vec::new());
            continue;
        }
        if line.eq_ignore_ascii_case("END:VALARM") && current.is_some() {
            if let Some(properties) = current.take() {
                alarms.push(parse_alarm_properties(properties));
            }
            continue;
        }
        if let Some(properties) = current.as_mut() {
            properties.push(line.clone());
            continue;
        }
        if is_calendar_component_end(&line) {
            component_depth = component_depth.saturating_sub(1);
        }
    }
    alarms
}

/// Replace only the ordered VALARM blocks in a component.  All component
/// properties, including unknown top-level fields, remain untouched.  The
/// function is also used for newly-created components, so it accepts an empty
/// alarm list as the explicit remove-all operation.
pub fn replace_component_alarms(component: &str, alarms: &[Alarm]) -> String {
    let source = unfold_lines(component);
    let mut lines = Vec::with_capacity(source.len() + alarms.len() * 6);
    let mut component_depth = 0usize;
    let mut index = 0usize;
    let mut inserted = false;

    while index < source.len() {
        let line = &source[index];
        if line.eq_ignore_ascii_case("BEGIN:VALARM") && component_depth == 1 {
            let Some(end) = source
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, candidate)| candidate.eq_ignore_ascii_case("END:VALARM"))
                .map(|(position, _)| position)
            else {
                lines.push(line.clone());
                index += 1;
                continue;
            };
            index = end + 1;
            continue;
        }

        if is_calendar_component_end(line) && component_depth == 1 && !inserted {
            for alarm in alarms {
                lines.extend(alarm.to_ical_lines());
            }
            inserted = true;
        }

        lines.push(line.clone());
        if is_calendar_component_begin(line) {
            component_depth = component_depth.saturating_add(1);
        } else if is_calendar_component_end(line) {
            component_depth = component_depth.saturating_sub(1);
        }
        index += 1;
    }

    if !inserted && !alarms.is_empty() {
        lines.extend(alarms.iter().flat_map(Alarm::to_ical_lines));
    }
    let folded = lines
        .iter()
        .flat_map(|line| fold_ical_line(line))
        .collect::<Vec<_>>();
    let mut result = folded.join("\r\n");
    if !result.is_empty() {
        result.push_str("\r\n");
    }
    result
}

fn parse_alarm_properties(properties: Vec<String>) -> Alarm {
    let mut action = AlarmAction::Unknown(String::new());
    let mut trigger = AlarmTrigger::Unknown {
        property_prefix: "TRIGGER".to_string(),
        value: String::new(),
    };
    let mut repeat = None;
    let mut duration = None;
    let mut message = None;
    let mut attachments = Vec::new();
    let mut attendees = Vec::new();
    let mut action_present = false;
    let mut trigger_present = false;
    let mut repeat_present = false;
    let mut duration_present = false;
    let mut message_present = false;

    for line in &properties {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        match property_name(property).as_str() {
            "ACTION" => {
                action_present = true;
                action = AlarmAction::from_value(value);
            }
            "TRIGGER" => {
                trigger_present = true;
                trigger = parse_alarm_trigger(property, value);
            }
            "REPEAT" => {
                repeat_present = true;
                repeat = value.trim().parse::<u32>().ok();
            }
            "DURATION" => {
                duration_present = true;
                duration = AlarmDuration::from_ical(value);
            }
            "DESCRIPTION" => {
                message_present = true;
                message = Some(unescape_ical_text(value));
            }
            "ATTACH" => attachments.push(unescape_ical_text(value)),
            "ATTENDEE" => attendees.push(unescape_ical_text(value)),
            _ => {}
        }
    }

    Alarm {
        action: action.clone(),
        trigger: trigger.clone(),
        repeat,
        duration: duration.clone(),
        message: message.clone(),
        raw_attachment_values: attachments.clone(),
        raw_attendee_values: attendees.clone(),
        attachments,
        attendees,
        raw_properties: properties,
        original_action: action_present.then_some(action.clone()),
        original_trigger: trigger_present.then_some(trigger.clone()),
        original_repeat: repeat_present.then_some(repeat),
        original_duration: duration_present.then_some(duration.clone()),
        original_message: message_present.then_some(message.clone()),
    }
}

fn parse_alarm_trigger(property: &str, value: &str) -> AlarmTrigger {
    if let Some(offset_seconds) = parse_ical_duration(value) {
        let relation = property
            .split(';')
            .skip(1)
            .find(|parameter| {
                parameter.split_once('=').is_some_and(|(name, value)| {
                    name.trim().eq_ignore_ascii_case("RELATED")
                        && value.trim().trim_matches('"').eq_ignore_ascii_case("END")
                })
            })
            .map(|_| AlarmRelation::End)
            .unwrap_or(AlarmRelation::Start);
        AlarmTrigger::Relative {
            relation,
            offset_seconds,
            property_prefix: property.to_string(),
        }
    } else if property.split(';').skip(1).any(|parameter| {
        parameter.split_once('=').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("VALUE")
                && value
                    .trim()
                    .trim_matches('"')
                    .eq_ignore_ascii_case("DATE-TIME")
        })
    }) || looks_like_ical_datetime(value)
    {
        AlarmTrigger::Absolute {
            property_prefix: property.to_string(),
            value: value.trim().to_string(),
        }
    } else {
        AlarmTrigger::Unknown {
            property_prefix: property.to_string(),
            value: value.to_string(),
        }
    }
}

fn property_name(line: &str) -> String {
    line.split_once(':')
        .map(|(property, _)| property)
        .unwrap_or(line)
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase()
}

fn is_calendar_component_begin(line: &str) -> bool {
    line.eq_ignore_ascii_case("BEGIN:VEVENT")
        || line.eq_ignore_ascii_case("BEGIN:VTODO")
        || line.eq_ignore_ascii_case("BEGIN:VJOURNAL")
}

fn is_calendar_component_end(line: &str) -> bool {
    line.eq_ignore_ascii_case("END:VEVENT")
        || line.eq_ignore_ascii_case("END:VTODO")
        || line.eq_ignore_ascii_case("END:VJOURNAL")
}

fn remove_trigger_parameter(property: &str, target: &str) -> String {
    let mut parts = property.split(';');
    let base = parts.next().unwrap_or(property);
    let parameters = parts
        .filter(|parameter| {
            parameter
                .split_once('=')
                .map(|(name, _)| !name.trim().eq_ignore_ascii_case(target))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if parameters.is_empty() {
        base.to_string()
    } else {
        format!("{base};{}", parameters.join(";"))
    }
}

fn trigger_is_empty(trigger: &AlarmTrigger) -> bool {
    matches!(
        trigger,
        AlarmTrigger::Unknown { property_prefix, value }
            if property_prefix.eq_ignore_ascii_case("TRIGGER") && value.is_empty()
    )
}

fn looks_like_ical_datetime(value: &str) -> bool {
    let bytes = value.trim().as_bytes();
    bytes.len() >= 15
        && bytes.get(8) == Some(&b'T')
        && bytes[..8].iter().all(|byte| byte.is_ascii_digit())
}

fn format_ical_duration(seconds: i64) -> String {
    let sign = if seconds < 0 { "-" } else { "" };
    let mut remaining = seconds.unsigned_abs();
    let week_seconds = 7 * 24 * 60 * 60;
    let use_weeks = remaining > 0 && remaining % week_seconds == 0;
    let weeks = if use_weeks {
        remaining / week_seconds
    } else {
        0
    };
    if use_weeks {
        return format!("{sign}P{weeks}W");
    }
    let days = remaining / (24 * 60 * 60);
    remaining %= 24 * 60 * 60;
    let hours = remaining / (60 * 60);
    remaining %= 60 * 60;
    let minutes = remaining / 60;
    let secs = remaining % 60;
    let mut value = format!("{sign}P");
    if weeks > 0 {
        value.push_str(&format!("{weeks}W"));
    } else {
        if days > 0 {
            value.push_str(&format!("{days}D"));
        }
        if hours > 0 || minutes > 0 || secs > 0 {
            value.push('T');
            if hours > 0 {
                value.push_str(&format!("{hours}H"));
            }
            if minutes > 0 {
                value.push_str(&format!("{minutes}M"));
            }
            if secs > 0 || (days == 0 && hours == 0 && minutes == 0) {
                value.push_str(&format!("{secs}S"));
            }
        }
    }
    if value == format!("{sign}P") {
        value.push_str("T0S");
    }
    value
}

fn format_duration_for_display(seconds: u64) -> String {
    if seconds % (24 * 60 * 60) == 0 {
        let days = seconds / (24 * 60 * 60);
        if days == 1 {
            "1 day".to_string()
        } else {
            format!("{days} days")
        }
    } else if seconds % (60 * 60) == 0 {
        let hours = seconds / (60 * 60);
        if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{hours} hours")
        }
    } else if seconds % 60 == 0 {
        let minutes = seconds / 60;
        if minutes == 1 {
            "1 minute".to_string()
        } else {
            format!("{minutes} minutes")
        }
    } else if seconds == 1 {
        "1 second".to_string()
    } else {
        format!("{seconds} seconds")
    }
}

fn serialize_attachment_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '\r' && *character != '\n')
        .collect()
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn unescape_ical_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            match character {
                'n' | 'N' => result.push('\n'),
                ',' | ';' | '\\' => result.push(character),
                other => {
                    result.push('\\');
                    result.push(other);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

fn fold_ical_line(line: &str) -> Vec<String> {
    if line.len() <= 75 {
        return vec![line.to_string()];
    }

    let mut result = Vec::new();
    let mut remaining = line;
    let mut first = true;
    while !remaining.is_empty() {
        let limit = if first { 75 } else { 74 };
        let mut end = 0;
        for (index, character) in remaining.char_indices() {
            if index + character.len_utf8() > limit {
                break;
            }
            end = index + character.len_utf8();
        }
        if end == 0 {
            end = remaining.chars().next().map(char::len_utf8).unwrap_or(0);
        }
        let prefix = if first { "" } else { " " };
        result.push(format!("{prefix}{}", &remaining[..end]));
        remaining = &remaining[end..];
        first = false;
    }
    result
}

/// Return a canonical deep link when the input is a valid identity.
///
/// The string-returning form is kept for existing editor/test callers.  New
/// runtime code uses `try_reminder_deep_link` so malformed identities are
/// rejected without manufacturing a target or panicking on untrusted input.
pub fn reminder_deep_link(
    kind: ReminderComponentKind,
    source_uid: &str,
    component_uid: &str,
) -> String {
    try_reminder_deep_link(kind, source_uid, component_uid).unwrap_or_default()
}

pub fn memo_deep_link(source_uid: &str, component_uid: &str) -> String {
    reminder_deep_link(ReminderComponentKind::Memo, source_uid, component_uid)
}

pub fn try_reminder_deep_link(
    kind: ReminderComponentKind,
    source_uid: &str,
    component_uid: &str,
) -> Result<String, ReminderIdentityError> {
    ReminderIdentity::new(kind, source_uid, component_uid).map(|identity| identity.to_uri())
}

pub fn parse_reminder_deep_link(uri: &str) -> Result<ReminderIdentity, ReminderIdentityError> {
    if uri.len() > MAX_REMINDER_URI_BYTES {
        return Err(ReminderIdentityError::Oversized);
    }
    let Some((scheme, query)) = uri.split_once(":///?") else {
        return Err(ReminderIdentityError::MalformedUri);
    };
    let Some(kind) = ReminderComponentKind::from_scheme(scheme) else {
        return if scheme.is_empty() {
            Err(ReminderIdentityError::MalformedUri)
        } else {
            Err(ReminderIdentityError::UnknownScheme)
        };
    };
    let mut source_uid = None;
    let mut component_uid = None;
    let parameters = query.split('&').collect::<Vec<_>>();
    if parameters.len() != 2 {
        return Err(ReminderIdentityError::MalformedUri);
    }
    for parameter in parameters {
        let Some((name, value)) = parameter.split_once('=') else {
            return Err(ReminderIdentityError::MalformedUri);
        };
        if value.contains('=') {
            return Err(ReminderIdentityError::MalformedUri);
        }
        let target = match name {
            "source-uid" => &mut source_uid,
            "comp-uid" => &mut component_uid,
            _ if name.is_empty() => return Err(ReminderIdentityError::MalformedUri),
            _ => return Err(ReminderIdentityError::UnknownParameter),
        };
        if target.is_some() {
            return Err(ReminderIdentityError::DuplicateParameter);
        }
        *target = Some(percent_decode_uid(value)?);
    }
    let identity = ReminderIdentity::new(
        kind,
        source_uid.ok_or(ReminderIdentityError::MissingParameter)?,
        component_uid.ok_or(ReminderIdentityError::MissingParameter)?,
    )?;
    if identity.to_uri() != uri {
        return Err(ReminderIdentityError::NonCanonicalEncoding);
    }
    Ok(identity)
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode_value(value: &str) -> Result<String, ReminderIdentityError> {
    if value.is_empty() {
        return Err(ReminderIdentityError::EmptyValue);
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err(ReminderIdentityError::InvalidPercentEncoding);
                }
                let high = hex_value(bytes[index + 1])
                    .ok_or(ReminderIdentityError::InvalidPercentEncoding)?;
                let low = hex_value(bytes[index + 2])
                    .ok_or(ReminderIdentityError::InvalidPercentEncoding)?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') => {
                decoded.push(byte);
                index += 1;
            }
            _ => return Err(ReminderIdentityError::InvalidPercentEncoding),
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ReminderIdentityError::InvalidUtf8)?;
    if percent_encode(&decoded) != value {
        return Err(ReminderIdentityError::NonCanonicalEncoding);
    }
    Ok(decoded)
}

fn percent_decode_uid(value: &str) -> Result<String, ReminderIdentityError> {
    let decoded = percent_decode_value(value)?;
    validate_reminder_uid(&decoded)?;
    Ok(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderComponentPresence {
    Found,
    MissingSource,
    MissingComponent,
}

/// Resolve an identity against an immutable snapshot without touching GTK or
/// any backend connection.  The UI uses the same distinction for visible
/// stale-source/component errors after an asynchronous page load misses.
pub fn component_presence(
    snapshot: &CalendarSnapshot,
    identity: &ReminderIdentity,
) -> ReminderComponentPresence {
    if identity.kind == ReminderComponentKind::Contact {
        return ReminderComponentPresence::Found;
    }
    let found = match identity.kind {
        ReminderComponentKind::Event => snapshot.events.iter().any(|event| {
            event.source_uid == identity.source_uid && event.uid == identity.component_uid
        }),
        ReminderComponentKind::Task => snapshot.tasks.iter().any(|task| {
            task.source_uid == identity.source_uid && task.uid == identity.component_uid
        }),
        ReminderComponentKind::Memo => snapshot.memos.iter().any(|memo| {
            memo.source_uid == identity.source_uid && memo.uid == identity.component_uid
        }),
        ReminderComponentKind::Contact => true,
    };
    let source_exists = match identity.kind {
        ReminderComponentKind::Event => snapshot
            .events
            .iter()
            .any(|event| event.source_uid == identity.source_uid),
        ReminderComponentKind::Task => snapshot
            .tasks
            .iter()
            .any(|task| task.source_uid == identity.source_uid),
        ReminderComponentKind::Memo => snapshot
            .memos
            .iter()
            .any(|memo| memo.source_uid == identity.source_uid),
        ReminderComponentKind::Contact => true,
    };
    if found {
        ReminderComponentPresence::Found
    } else if source_exists {
        ReminderComponentPresence::MissingComponent
    } else {
        ReminderComponentPresence::MissingSource
    }
}

fn reminder_key(
    identity: &ReminderIdentity,
    occurrence_id: &str,
    alarm_index: usize,
    repeat_index: u32,
    due_at: i64,
) -> String {
    format!(
        "v1:{}:{}:{}:{}:{}:{}:{}",
        identity.kind.scheme(),
        percent_encode(&identity.source_uid),
        percent_encode(&identity.component_uid),
        percent_encode(occurrence_id),
        alarm_index,
        repeat_index,
        due_at,
    )
}

fn identity_from_reminder_key(key: &str) -> Result<ReminderIdentity, ReminderIdentityError> {
    if key.len() > MAX_REMINDER_KEY_BYTES {
        return Err(ReminderIdentityError::Oversized);
    }
    let parts = key.split(':').collect::<Vec<_>>();
    if parts.len() != 8 || parts[0] != "v1" {
        return Err(ReminderIdentityError::InvalidReminderKey);
    }
    let Some(kind) = ReminderComponentKind::from_scheme(parts[1]) else {
        return Err(ReminderIdentityError::InvalidReminderKey);
    };
    let source_uid = percent_decode_uid(parts[2])?;
    let component_uid = percent_decode_uid(parts[3])?;
    let occurrence_id = percent_decode_uid(parts[4])?;
    let Ok(alarm_index) = parts[5].parse::<usize>() else {
        return Err(ReminderIdentityError::InvalidReminderKey);
    };
    let Ok(repeat_index) = parts[6].parse::<u32>() else {
        return Err(ReminderIdentityError::InvalidReminderKey);
    };
    let Ok(due_at) = parts[7].parse::<i64>() else {
        return Err(ReminderIdentityError::InvalidReminderKey);
    };
    if occurrence_id.is_empty()
        || parts[5] != alarm_index.to_string()
        || parts[6] != repeat_index.to_string()
        || parts[7] != due_at.to_string()
    {
        return Err(ReminderIdentityError::InvalidReminderKey);
    }
    ReminderIdentity::new(kind, source_uid, component_uid)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderCandidate {
    pub key: String,
    pub title: String,
    pub due_at: i64,
    pub kind: ReminderKind,
}

impl ReminderCandidate {
    pub fn identity(&self) -> Result<ReminderIdentity, ReminderIdentityError> {
        let identity = identity_from_reminder_key(&self.key)?;
        if self.kind != identity.kind.into() {
            return Err(ReminderIdentityError::InvalidReminderKey);
        }
        Ok(identity)
    }

    pub fn deep_link(&self) -> Result<String, ReminderIdentityError> {
        self.identity().map(|identity| identity.to_uri())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderDelivery {
    pub candidate: ReminderCandidate,
    pub claim_token: String,
}

impl ReminderDelivery {
    pub fn identity(&self) -> Result<ReminderIdentity, ReminderIdentityError> {
        self.candidate.identity()
    }

    pub fn action_target(&self) -> Result<String, ReminderIdentityError> {
        let identity = self.identity()?;
        validate_claim_token(&self.claim_token)?;
        let target = format!(
            "v1|{}|{}|{}",
            identity.to_uri(),
            percent_encode(&self.candidate.key),
            percent_encode(&self.claim_token),
        );
        if target.len() > MAX_ACTION_TARGET_BYTES {
            return Err(ReminderIdentityError::Oversized);
        }
        Ok(target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderActionTarget {
    pub identity: ReminderIdentity,
    pub key: String,
    pub claim_token: String,
}

pub fn parse_reminder_action_target(
    target: &str,
) -> Result<ReminderActionTarget, ReminderIdentityError> {
    if target.len() > MAX_ACTION_TARGET_BYTES {
        return Err(ReminderIdentityError::Oversized);
    }
    let parts = target.split('|').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "v1" {
        return Err(ReminderIdentityError::InvalidActionTarget);
    }
    let identity = parse_reminder_deep_link(parts[1])?;
    let key = percent_decode_value(parts[2])?;
    if key.len() > MAX_REMINDER_KEY_BYTES {
        return Err(ReminderIdentityError::Oversized);
    }
    let claim_token = percent_decode_value(parts[3])?;
    validate_claim_token(&claim_token)?;
    let key_identity = identity_from_reminder_key(&key)?;
    if key_identity != identity {
        return Err(ReminderIdentityError::InvalidActionTarget);
    }
    Ok(ReminderActionTarget {
        identity,
        key,
        claim_token,
    })
}

pub fn deduplicate_deliveries(deliveries: &[ReminderDelivery]) -> Vec<ReminderDelivery> {
    let mut seen = BTreeSet::new();
    deliveries
        .iter()
        .filter(|delivery| seen.insert(delivery.candidate.key.clone()))
        .cloned()
        .collect()
}

pub trait ReminderClock: Clone {
    fn now(&self) -> i64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl ReminderClock for SystemClock {
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

/// A deterministic clock for scheduler tests and portable replay.  Production
/// callers can keep using the explicit `now` argument on the candidate API.
#[derive(Debug, Clone)]
pub struct FakeClock {
    now: Arc<AtomicI64>,
}

impl FakeClock {
    pub fn new(now: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(now)),
        }
    }

    pub fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }

    pub fn advance(&self, seconds: i64) {
        self.set(self.now().saturating_add(seconds));
    }
}

impl ReminderClock for FakeClock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub struct ReminderScheduler<C: ReminderClock> {
    clock: C,
}

impl<C: ReminderClock> ReminderScheduler<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    pub fn due_reminders(
        &self,
        store: &mut ReminderStore,
        candidates: &[ReminderCandidate],
    ) -> Vec<ReminderCandidate> {
        store.due_reminders(candidates, self.clock.now())
    }

    pub fn claim_due_reminders(
        &self,
        store: &mut ReminderStore,
        candidates: &[ReminderCandidate],
    ) -> io::Result<Vec<ReminderDelivery>> {
        store.claim_due_reminders(candidates, self.clock.now())
    }

    pub fn snooze_for(&self, store: &mut ReminderStore, key: &str, seconds: i64) -> io::Result<()> {
        store.snooze_until(key, self.clock.now().saturating_add(seconds.max(0)))
    }

    pub fn dismiss(&self, store: &mut ReminderStore, key: &str) -> io::Result<()> {
        store.dismiss(key)
    }

    pub fn restart(&self, store: &mut ReminderStore, key: &str) -> io::Result<()> {
        store.restart(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReminderState {
    version: u32,
    #[serde(default)]
    snoozed_until: BTreeMap<String, i64>,
    #[serde(default)]
    delivered: BTreeSet<String>,
    #[serde(default)]
    pending: BTreeMap<String, PendingDeliveryState>,
    #[serde(default)]
    delivery_tokens: BTreeMap<String, String>,
    #[serde(default)]
    delivery_completed_at: BTreeMap<String, i64>,
    #[serde(default)]
    delivery_actions: BTreeMap<String, DeliveryAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingDeliveryState {
    token: String,
    claimed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum DeliveryAction {
    Delivered,
    Snoozed,
    Dismissed,
}

fn current_reminder_time() -> i64 {
    chrono::Utc::now().timestamp()
}

fn claim_is_current(claim: &PendingDeliveryState, token: &str, now: i64) -> bool {
    claim.token == token && claim.claimed_at > now.saturating_sub(MAX_CLAIM_SECONDS)
}

fn completed_delivery_is_current(state: &ReminderState, key: &str, token: &str, now: i64) -> bool {
    state
        .delivery_tokens
        .get(key)
        .is_some_and(|stored| stored == token)
        && state
            .delivery_completed_at
            .get(key)
            // State written by the pre-lease implementation has no
            // completion timestamp.  It is deliberately not actionable:
            // without an age there is no safe way to distinguish a live
            // notification from a stale one after restart.
            .is_some_and(|completed_at| *completed_at >= now.saturating_sub(MAX_LATE_SECONDS))
}

fn completed_delivery_action(state: &ReminderState, key: &str) -> Option<DeliveryAction> {
    state.delivery_actions.get(key).copied().or_else(|| {
        // Older state files did not record which action completed a claim.
        // Treat their delivered bit as a notification handoff so the valid
        // snooze button remains usable during the compatibility window.
        if state.snoozed_until.contains_key(key) {
            Some(DeliveryAction::Snoozed)
        } else if state.delivered.contains(key) {
            Some(DeliveryAction::Delivered)
        } else {
            None
        }
    })
}

impl Default for ReminderState {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            snoozed_until: BTreeMap::new(),
            delivered: BTreeSet::new(),
            pending: BTreeMap::new(),
            delivery_tokens: BTreeMap::new(),
            delivery_completed_at: BTreeMap::new(),
            delivery_actions: BTreeMap::new(),
        }
    }
}

fn load_reminder_state(path: &Path) -> io::Result<ReminderState> {
    match fs::read(path) {
        Ok(raw) => {
            let state = serde_json::from_slice::<ReminderState>(&raw)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if state.version == 0 || state.version > CURRENT_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Nicht unterstützte Reminder-Version: {} (erwartet {})",
                        state.version, CURRENT_VERSION
                    ),
                ));
            }
            Ok(ReminderState {
                version: CURRENT_VERSION,
                ..state
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ReminderState::default()),
        Err(error) => Err(error),
    }
}

fn save_reminder_state(path: &Path, state: &ReminderState) -> io::Result<()> {
    let raw = serde_json::to_vec_pretty(state)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("reminders.json");
    let sequence = REMINDER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn due_reminders_from_state(
    state: &ReminderState,
    candidates: &[ReminderCandidate],
    now: i64,
) -> Vec<ReminderCandidate> {
    let mut due = candidates
        .iter()
        .filter(|candidate| {
            candidate.identity().is_ok()
                && candidate.due_at <= now
                && candidate.due_at >= now - MAX_LATE_SECONDS
                && !state.delivered.contains(&candidate.key)
                && !state
                    .pending
                    .get(&candidate.key)
                    .is_some_and(|claim| claim.claimed_at > now.saturating_sub(MAX_CLAIM_SECONDS))
                && state
                    .snoozed_until
                    .get(&candidate.key)
                    .is_none_or(|until| *until <= now)
        })
        .cloned()
        .collect::<Vec<_>>();
    due.sort_by(|left, right| {
        left.due_at
            .cmp(&right.due_at)
            .then_with(|| left.key.cmp(&right.key))
    });
    due.dedup_by(|left, right| left.key == right.key);
    due
}

#[derive(Debug, Clone)]
pub struct ReminderStore {
    path: PathBuf,
    state: ReminderState,
}

impl ReminderStore {
    pub fn new() -> io::Result<Self> {
        let path = std::env::var_os("MERIDIAN_REMINDER_FILE").or_else(|| std::env::var_os("EVOLUTION_GTK4_REMINDER_FILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| super::app_data_dir().join("reminders.json"));
        Self::with_path(path)
    }

    pub fn with_path(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let state = load_reminder_state(&path)?;
        Ok(Self { path, state })
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return reminders that are due now and have not already been delivered.
    /// Reminders older than one week are ignored to avoid a notification storm
    /// after a long period in which the application was not running.
    pub fn due_reminders(
        &mut self,
        candidates: &[ReminderCandidate],
        now: i64,
    ) -> Vec<ReminderCandidate> {
        self.state
            .snoozed_until
            .retain(|_, until| *until > now - MAX_LATE_SECONDS);
        due_reminders_from_state(&self.state, candidates, now)
    }

    /// Atomically claim each due, valid candidate before handing it to a
    /// notification adapter.  A claim is persisted separately from delivery;
    /// an interrupted adapter can therefore reclaim it after a short lease.
    pub fn claim_due_reminders(
        &mut self,
        candidates: &[ReminderCandidate],
        now: i64,
    ) -> io::Result<Vec<ReminderDelivery>> {
        self.update_state(|state| {
            let before = state.clone();
            state
                .snoozed_until
                .retain(|_, until| *until > now - MAX_LATE_SECONDS);
            state
                .pending
                .retain(|_, claim| claim.claimed_at > now.saturating_sub(MAX_CLAIM_SECONDS));
            state
                .delivery_completed_at
                .retain(|_, completed_at| *completed_at > now.saturating_sub(MAX_LATE_SECONDS));
            state
                .delivery_tokens
                .retain(|key, _| state.delivery_completed_at.contains_key(key));
            state
                .delivery_actions
                .retain(|key, _| state.delivery_completed_at.contains_key(key));
            let mut deliveries = Vec::new();
            for candidate in due_reminders_from_state(state, candidates, now) {
                if candidate.identity().is_err() {
                    continue;
                }
                let sequence = REMINDER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let claim_token = format!(
                    "{}-{}-{}-{sequence}",
                    now,
                    std::process::id(),
                    reminder_process_nonce(),
                );
                state.pending.insert(
                    candidate.key.clone(),
                    PendingDeliveryState {
                        token: claim_token.clone(),
                        claimed_at: now,
                    },
                );
                state.delivered.remove(&candidate.key);
                state.delivery_tokens.remove(&candidate.key);
                state.delivery_completed_at.remove(&candidate.key);
                state.delivery_actions.remove(&candidate.key);
                deliveries.push(ReminderDelivery {
                    candidate,
                    claim_token,
                });
            }
            Ok((state != &before, deliveries))
        })
    }

    pub fn mark_delivered(&mut self, key: &str) -> io::Result<()> {
        self.update_state(|state| {
            let mut changed = false;
            changed |= state.snoozed_until.remove(key).is_some();
            changed |= state.pending.remove(key).is_some();
            changed |= state.delivered.insert(key.to_string());
            changed |= state.delivery_tokens.remove(key).is_some();
            changed |= state.delivery_completed_at.remove(key).is_some();
            changed |= state.delivery_actions.remove(key).is_some();
            Ok((changed, ()))
        })
    }

    pub fn snooze_until(&mut self, key: &str, until: i64) -> io::Result<()> {
        self.update_state(|state| {
            let mut changed = false;
            changed |= state.delivered.remove(key);
            changed |= state.pending.remove(key).is_some();
            changed |= state.snoozed_until.insert(key.to_string(), until) != Some(until);
            changed |= state.delivery_tokens.remove(key).is_some();
            changed |= state.delivery_completed_at.remove(key).is_some();
            changed |= state.delivery_actions.remove(key).is_some();
            Ok((changed, ()))
        })
    }

    pub fn dismiss(&mut self, key: &str) -> io::Result<()> {
        self.mark_delivered(key)
    }

    pub fn snooze_if_current(
        &mut self,
        key: &str,
        claim_token: &str,
        until: i64,
    ) -> io::Result<bool> {
        self.snooze_if_current_at(key, claim_token, until, current_reminder_time())
    }

    pub fn snooze_if_current_at(
        &mut self,
        key: &str,
        claim_token: &str,
        until: i64,
        now: i64,
    ) -> io::Result<bool> {
        if identity_from_reminder_key(key).is_err() || validate_claim_token(claim_token).is_err() {
            return Ok(false);
        }
        self.update_state(|state| {
            let pending_matches = state
                .pending
                .get(key)
                .is_some_and(|claim| claim_is_current(claim, claim_token, now));
            let completed_matches = completed_delivery_is_current(state, key, claim_token, now);
            if !pending_matches && !completed_matches {
                return Ok((false, false));
            }
            if !pending_matches {
                match completed_delivery_action(state, key) {
                    Some(DeliveryAction::Snoozed) => return Ok((false, true)),
                    Some(DeliveryAction::Delivered) => {}
                    Some(DeliveryAction::Dismissed) | None => return Ok((false, false)),
                }
            }
            state.pending.remove(key);
            state.delivered.remove(key);
            state.snoozed_until.insert(key.to_string(), until);
            state
                .delivery_tokens
                .insert(key.to_string(), claim_token.to_string());
            state.delivery_completed_at.insert(key.to_string(), now);
            state
                .delivery_actions
                .insert(key.to_string(), DeliveryAction::Snoozed);
            Ok((true, true))
        })
    }

    pub fn dismiss_if_current(&mut self, key: &str, claim_token: &str) -> io::Result<bool> {
        self.dismiss_if_current_at(key, claim_token, current_reminder_time())
    }

    pub fn dismiss_if_current_at(
        &mut self,
        key: &str,
        claim_token: &str,
        now: i64,
    ) -> io::Result<bool> {
        if identity_from_reminder_key(key).is_err() || validate_claim_token(claim_token).is_err() {
            return Ok(false);
        }
        self.update_state(|state| {
            let pending_matches = state
                .pending
                .get(key)
                .is_some_and(|claim| claim_is_current(claim, claim_token, now));
            let completed_matches = completed_delivery_is_current(state, key, claim_token, now);
            if !pending_matches && !completed_matches {
                return Ok((false, false));
            }
            if !pending_matches {
                match completed_delivery_action(state, key) {
                    Some(DeliveryAction::Dismissed) => return Ok((false, true)),
                    Some(DeliveryAction::Delivered) => {}
                    Some(DeliveryAction::Snoozed) | None => return Ok((false, false)),
                }
            }
            state.pending.remove(key);
            state.snoozed_until.remove(key);
            state.delivered.insert(key.to_string());
            state
                .delivery_tokens
                .insert(key.to_string(), claim_token.to_string());
            state.delivery_completed_at.insert(key.to_string(), now);
            state
                .delivery_actions
                .insert(key.to_string(), DeliveryAction::Dismissed);
            Ok((true, true))
        })
    }

    pub fn mark_delivered_if_claimed(&mut self, key: &str, claim_token: &str) -> io::Result<bool> {
        self.mark_delivered_if_claimed_at(key, claim_token, current_reminder_time())
    }

    pub fn mark_delivered_if_claimed_at(
        &mut self,
        key: &str,
        claim_token: &str,
        now: i64,
    ) -> io::Result<bool> {
        if identity_from_reminder_key(key).is_err() || validate_claim_token(claim_token).is_err() {
            return Ok(false);
        }
        self.update_state(|state| {
            if !state
                .pending
                .get(key)
                .is_some_and(|claim| claim_is_current(claim, claim_token, now))
            {
                return Ok((false, false));
            }
            state.pending.remove(key);
            state.snoozed_until.remove(key);
            state.delivered.insert(key.to_string());
            state
                .delivery_tokens
                .insert(key.to_string(), claim_token.to_string());
            state.delivery_completed_at.insert(key.to_string(), now);
            state
                .delivery_actions
                .insert(key.to_string(), DeliveryAction::Delivered);
            Ok((true, true))
        })
    }

    /// Make a previously delivered alarm eligible again.  This is the
    /// explicit restart operation used by deterministic scheduler tests and
    /// is persisted so a process restart cannot silently change the state.
    pub fn restart(&mut self, key: &str) -> io::Result<()> {
        self.update_state(|state| {
            let mut changed = false;
            changed |= state.snoozed_until.remove(key).is_some();
            changed |= state.pending.remove(key).is_some();
            changed |= state.delivered.remove(key);
            changed |= state.delivery_tokens.remove(key).is_some();
            changed |= state.delivery_completed_at.remove(key).is_some();
            changed |= state.delivery_actions.remove(key).is_some();
            Ok((changed, ()))
        })
    }

    #[allow(dead_code)]
    pub fn is_delivered(&self, key: &str) -> bool {
        self.state.delivered.contains(key)
    }

    fn update_state<T, F>(&mut self, update: F) -> io::Result<T>
    where
        F: FnOnce(&mut ReminderState) -> io::Result<(bool, T)>,
    {
        let _guard = REMINDER_STATE_LOCK
            .lock()
            .map_err(|_| io::Error::other("Reminder-State-Sperre ist beschädigt"))?;
        let mut state = load_reminder_state(&self.path)?;
        let (changed, result) = update(&mut state)?;
        if changed {
            save_reminder_state(&self.path, &state)?;
        }
        self.state = state;
        Ok(result)
    }
}

/// Extract all portable reminder candidates from the current local snapshot.
/// The candidate key contains the component UID, alarm index and due time so
/// changing an event or a recurring occurrence creates a new delivery key.
pub fn candidates_from_snapshot(snapshot: &CalendarSnapshot, now: i64) -> Vec<ReminderCandidate> {
    let mut candidates = snapshot
        .events
        .iter()
        .flat_map(|event| event_reminder_candidates(event, now))
        .chain(
            snapshot
                .tasks
                .iter()
                .filter(|task| !task.completed)
                .flat_map(task_reminder_candidates),
        )
        .chain(snapshot.memos.iter().flat_map(memo_reminder_candidates))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.due_at
            .cmp(&right.due_at)
            .then_with(|| left.key.cmp(&right.key))
    });
    candidates.dedup_by(|left, right| left.key == right.key);
    candidates
}

/// Parse flexible vCard date representations into (original_year, month, day).
pub(crate) fn parse_contact_date(value: &str) -> Option<(Option<i32>, u32, u32)> {
    let s = value.trim();
    if s.is_empty() {
        return None;
    }
    // Formats: "--MM-DD" or "--MMDD" (without year)
    if let Some(rest) = s.strip_prefix("--") {
        let clean = rest.replace('-', "");
        if clean.len() == 4 && clean.bytes().all(|b| b.is_ascii_digit()) {
            let month = clean[..2].parse::<u32>().ok()?;
            let day = clean[2..].parse::<u32>().ok()?;
            if (1..=12).contains(&month) && (1..=31).contains(&day) {
                return Some((None, month, day));
            }
        }
    }
    // Formats: "YYYY-MM-DD" or "YYYY-MM-DDTHH:MM:SS"
    if s.len() >= 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.as_bytes()[..4].iter().all(|b| b.is_ascii_digit())
        && s.as_bytes()[5..7].iter().all(|b| b.is_ascii_digit())
        && s.as_bytes()[8..10].iter().all(|b| b.is_ascii_digit())
    {
        let year = s[..4].parse::<i32>().ok()?;
        let month = s[5..7].parse::<u32>().ok()?;
        let day = s[8..10].parse::<u32>().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some((Some(year), month, day));
        }
    }
    // Formats: "YYYYMMDD" or "YYYYMMDDTHHMMSS"
    if s.len() >= 8 && s.as_bytes()[..8].iter().all(|b| b.is_ascii_digit()) {
        let year = s[..4].parse::<i32>().ok()?;
        let month = s[4..6].parse::<u32>().ok()?;
        let day = s[6..8].parse::<u32>().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some((Some(year), month, day));
        }
    }
    None
}

/// Extract birthday and anniversary reminder candidates from contact records.
pub fn contact_birthday_candidates(
    contacts: &[super::contact_store_service::LocalContactRecord],
    now: i64,
    offset_seconds: i64,
) -> Vec<ReminderCandidate> {
    let now_date = match Local.timestamp_opt(now, 0) {
        LocalResult::Single(dt) => dt.date_naive(),
        _ => return Vec::new(),
    };
    let current_year = now_date.year();
    let mut candidates = Vec::new();

    for record in contacts {
        if record.id.trim().is_empty() {
            continue;
        }
        let name = if !record.contact.fn_name.trim().is_empty() {
            record.contact.fn_name.trim()
        } else {
            "Contact"
        };

        let dates_to_check = [
            ("Birthday", &record.contact.birthday, "bday"),
            ("Anniversary", &record.contact.anniversary, "anni"),
        ];

        for (label, date_str, kind_prefix) in dates_to_check {
            let Some((orig_year, month, day)) = parse_contact_date(date_str) else {
                continue;
            };

            // Evaluate occurrences in [current_year - 1, current_year, current_year + 1]
            for year in [current_year - 1, current_year, current_year + 1] {
                let occurrence_date = NaiveDate::from_ymd_opt(year, month, day)
                    .or_else(|| {
                        // Leap year fallback: Feb 29 -> Feb 28
                        if month == 2 && day == 29 {
                            NaiveDate::from_ymd_opt(year, 2, 28)
                        } else {
                            None
                        }
                    });
                let Some(occ_date) = occurrence_date else {
                    continue;
                };

                let occ_datetime = occ_date.and_hms_opt(DEFAULT_ALL_DAY_HOUR, 0, 0);
                let Some(occ_dt) = occ_datetime else { continue; };
                let occ_timestamp = match Local.from_local_datetime(&occ_dt) {
                    LocalResult::Single(dt) => dt.timestamp(),
                    LocalResult::Ambiguous(dt1, _) => dt1.timestamp(),
                    LocalResult::None => continue,
                };

                let due_at = occ_timestamp.saturating_sub(offset_seconds);
                let Ok(identity) = ReminderIdentity::new(
                    ReminderComponentKind::Contact,
                    "contacts",
                    &record.id,
                ) else {
                    continue;
                };

                let occurrence_id = format!(
                    "{}-{:04}{:02}{:02}",
                    kind_prefix,
                    occ_date.year(),
                    occ_date.month(),
                    occ_date.day()
                );
                let title = match orig_year {
                    Some(y) if year > y => format!("{name}: {}. {label}", year - y),
                    _ => format!("{name}: {label}"),
                };

                candidates.push(ReminderCandidate {
                    key: reminder_key(&identity, &occurrence_id, 0, 0, due_at),
                    title,
                    due_at,
                    kind: ReminderKind::Contact,
                });
            }
        }
    }

    candidates
}

fn event_reminder_candidates(event: &CalendarEventRecord, now: i64) -> Vec<ReminderCandidate> {
    let Some(start) = safe_property_local_datetime(&event.component, "DTSTART", event.all_day)
        .or_else(|| parse_display_naive(&event.start, event.all_day))
    else {
        return Vec::new();
    };
    let end = safe_property_local_datetime(&event.component, "DTEND", event.all_day)
        .or_else(|| parse_display_naive(&event.end, event.all_day))
        .unwrap_or(start);
    let start_property = ical_property_line(&event.component, "DTSTART");
    let end_property = ical_property_line(&event.component, "DTEND");
    let initial_date = start_property
        .as_ref()
        .and_then(|(_, value)| parse_ical_date(value))
        .unwrap_or_else(|| start.date());
    let initial_start_timestamp = start_property
        .as_ref()
        .and_then(|(property, value)| {
            value
                .is_ascii()
                .then(|| property_local_timestamp(property, value, DEFAULT_ALL_DAY_HOUR))
                .flatten()
        })
        .or_else(|| local_timestamp(start));
    let initial_end_timestamp = end_property
        .as_ref()
        .and_then(|(property, value)| {
            value
                .is_ascii()
                .then(|| property_local_timestamp(property, value, DEFAULT_ALL_DAY_HOUR))
                .flatten()
        })
        .or_else(|| local_timestamp(end));
    let duration_seconds = initial_start_timestamp
        .zip(initial_end_timestamp)
        .map(|(start, end)| end.saturating_sub(start));
    let dates = recurrence_dates(&event.component, initial_date, now);
    let alarms = parse_alarms(&event.component);
    let mut candidates = Vec::new();
    for date in dates {
        let occurrence_start_timestamp = start_property
            .as_ref()
            .and_then(|(property, value)| {
                occurrence_timestamp(property, value, date, event.all_day)
            })
            .or_else(|| local_timestamp(date.and_time(start.time())));
        let occurrence_end_timestamp = end_property
            .as_ref()
            .and_then(|(property, value)| {
                occurrence_timestamp(property, value, date, event.all_day)
            })
            .or_else(|| {
                occurrence_start_timestamp.and_then(|value| {
                    duration_seconds.map(|duration| value.saturating_add(duration))
                })
            });
        candidates.extend(alarm_candidates_from_specs(
            &alarms,
            occurrence_start_timestamp,
            occurrence_end_timestamp,
            ReminderComponentKind::Event,
            &event.uid,
            &event.source_uid,
            &event.summary,
            &date.format("%Y%m%d").to_string(),
            date == initial_date,
        ));
    }
    candidates
}

fn task_reminder_candidates(task: &TaskRecord) -> Vec<ReminderCandidate> {
    let due = ical_property_timestamp(&task.component, "DUE", task.due.len() == 10)
        .or_else(|| parse_display_datetime(&task.due, task.due.len() == 10));
    let start = ical_property_timestamp(&task.component, "DTSTART", false).or(due);
    let end = due.or(start);
    alarm_candidates_from_specs(
        &parse_alarms(&task.component),
        start,
        end,
        ReminderComponentKind::Task,
        &task.uid,
        &task.source_uid,
        &task.summary,
        "single",
        true,
    )
}

fn memo_reminder_candidates(
    memo: &super::calendar_store_service::MemoRecord,
) -> Vec<ReminderCandidate> {
    let anchor = ical_property_timestamp(&memo.component, "DTSTART", false)
        .or_else(|| ical_property_timestamp(&memo.component, "DTSTAMP", false))
        .or_else(|| ical_property_timestamp(&memo.component, "LAST-MODIFIED", false));
    alarm_candidates_from_specs(
        &parse_alarms(&memo.component),
        anchor,
        anchor,
        ReminderComponentKind::Memo,
        &memo.uid,
        &memo.source_uid,
        &memo.summary,
        "single",
        true,
    )
}

fn alarm_candidates_from_specs(
    alarms: &[Alarm],
    start: Option<i64>,
    end: Option<i64>,
    component_kind: ReminderComponentKind,
    uid: &str,
    source_uid: &str,
    title: &str,
    occurrence_id: &str,
    include_absolute: bool,
) -> Vec<ReminderCandidate> {
    let Ok(identity) = ReminderIdentity::new(component_kind, source_uid, uid) else {
        return Vec::new();
    };
    alarms
        .iter()
        .into_iter()
        .enumerate()
        .flat_map(move |(index, alarm)| {
            let identity = identity.clone();
            if !alarm.is_safe_to_notify()
                || (!include_absolute && !matches!(alarm.trigger, AlarmTrigger::Relative { .. }))
            {
                return Vec::new();
            }
            let Some(initial_due_at) = alarm_due_at(alarm, start, end) else {
                return Vec::new();
            };
            let repetitions = alarm.repeat.unwrap_or(0).min(MAX_ALARM_REPETITIONS);
            let repeat_step = (repetitions > 0)
                .then(|| alarm.duration.as_ref().map(|duration| duration.seconds))
                .flatten()
                .filter(|seconds| *seconds > 0);
            let repetitions = if repetitions > 0 && repeat_step.is_none() {
                0
            } else {
                repetitions
            };
            (0..=repetitions)
                .filter_map(move |repeat_index| {
                    let due_at = if repeat_index == 0 {
                        initial_due_at
                    } else {
                        let step = repeat_step?;
                        initial_due_at.checked_add(step.checked_mul(i64::from(repeat_index))?)?
                    };
                    let kind = identity.kind.into();
                    Some(ReminderCandidate {
                        key: reminder_key(&identity, occurrence_id, index, repeat_index, due_at),
                        title: alarm
                            .message
                            .as_deref()
                            .filter(|message| !message.trim().is_empty())
                            .unwrap_or(title)
                            .to_string(),
                        due_at,
                        kind,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn alarm_due_at(alarm: &Alarm, start: Option<i64>, end: Option<i64>) -> Option<i64> {
    match &alarm.trigger {
        AlarmTrigger::Relative {
            relation,
            offset_seconds,
            ..
        } => match relation {
            AlarmRelation::Start => start?.checked_add(*offset_seconds),
            AlarmRelation::End => end.or(start)?.checked_add(*offset_seconds),
        },
        AlarmTrigger::Absolute {
            property_prefix,
            value,
        } => value
            .is_ascii()
            .then(|| property_local_timestamp(property_prefix, value, DEFAULT_ALL_DAY_HOUR))
            .flatten(),
        AlarmTrigger::Unknown { .. } => None,
    }
}

fn ical_property_line(raw: &str, target: &str) -> Option<(String, String)> {
    unfold_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        (property_name(property) == target.to_ascii_uppercase())
            .then(|| (property.to_string(), value.to_string()))
    })
}

fn safe_property_local_datetime(raw: &str, target: &str, all_day: bool) -> Option<NaiveDateTime> {
    let (_, value) = ical_property_line(raw, target)?;
    value
        .is_ascii()
        .then(|| property_local_datetime(raw, target, all_day))
        .flatten()
}

fn ical_property_timestamp(raw: &str, target: &str, all_day: bool) -> Option<i64> {
    let (property, value) = ical_property_line(raw, target)?;
    value
        .is_ascii()
        .then(|| property_local_timestamp(&property, &value, DEFAULT_ALL_DAY_HOUR))
        .flatten()
        .or_else(|| safe_property_local_datetime(raw, target, all_day).and_then(local_timestamp))
}

fn occurrence_timestamp(
    property: &str,
    value: &str,
    date: NaiveDate,
    all_day: bool,
) -> Option<i64> {
    if !value.is_ascii() {
        return None;
    }
    if all_day || value.len() == 8 {
        return property_local_timestamp(
            property,
            &date.format("%Y%m%d").to_string(),
            DEFAULT_ALL_DAY_HOUR,
        );
    }
    let suffix = value.get(8..)?;
    let occurrence_value = format!("{}{}", date.format("%Y%m%d"), suffix);
    property_local_timestamp(property, &occurrence_value, DEFAULT_ALL_DAY_HOUR)
}

fn parse_ical_duration(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.len() < 2 || !raw.is_ascii() {
        return None;
    }
    let (sign, value) = match raw.as_bytes()[0] {
        b'-' => (-1_i64, &raw[1..]),
        b'+' => (1_i64, &raw[1..]),
        _ => (1_i64, raw),
    };
    if !value.starts_with('P') || value.len() == 1 {
        return None;
    }
    let mut number = 0_i64;
    let mut has_number = false;
    let mut in_time = false;
    let mut seconds = 0_i64;
    let mut found_date = false;
    let mut found_time = false;
    let mut found_week = false;
    let mut last_time_unit = 0_u8;
    for character in value[1..].bytes() {
        if character == b'T' {
            if in_time || has_number || found_time || found_week {
                return None;
            }
            in_time = true;
            continue;
        }
        if character.is_ascii_digit() {
            number = number
                .checked_mul(10)?
                .checked_add(i64::from(character - b'0'))?;
            has_number = true;
            continue;
        }
        if !has_number {
            return None;
        }
        let (unit_seconds, time_order) = match (in_time, character) {
            (false, b'W') if !found_date && !found_week => {
                found_week = true;
                (7 * 24 * 60 * 60, 0)
            }
            (false, b'D') if !found_date && !found_week => {
                found_date = true;
                (24 * 60 * 60, 0)
            }
            (true, b'H') if last_time_unit < 1 => (60 * 60, 1),
            (true, b'M') if last_time_unit < 2 => (60, 2),
            (true, b'S') if last_time_unit < 3 => (1, 3),
            _ => return None,
        };
        seconds = seconds.checked_add(number.checked_mul(unit_seconds)?)?;
        number = 0;
        has_number = false;
        if in_time {
            last_time_unit = time_order;
            found_time = true;
        }
    }
    if has_number
        || in_time && !found_time
        || (!found_date && !found_week && !found_time)
        || found_week && found_time
    {
        return None;
    }
    seconds.checked_mul(sign)
}

fn recurrence_dates(component: &str, initial: NaiveDate, now: i64) -> Vec<NaiveDate> {
    let horizon = Local
        .timestamp_opt(now, 0)
        .single()
        .map(|value| value.date_naive() + Duration::days(370))
        .unwrap_or_else(|| initial + Duration::days(370));

    let excluded_dates = ical_properties(component, "EXDATE")
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .filter_map(parse_ical_date)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut dates = Vec::new();
    if let Some(raw_rule) = ical_property(component, "RRULE") {
        if let Some(rule) = parse_recurrence_rule(&raw_rule) {
            if initial <= horizon {
                let mut cursor = initial;
                while cursor <= horizon && dates.len() < 4_096 {
                    if recurrence_occurs_on(&rule, initial, cursor, &excluded_dates) {
                        dates.push(cursor);
                    }
                    let Some(next) = cursor.checked_add_signed(Duration::days(1)) else {
                        break;
                    };
                    cursor = next;
                }
            }
        } else {
            dates.push(initial);
        }
    } else {
        dates.push(initial);
    }

    if initial <= horizon {
        for value in ical_properties(component, "RDATE") {
            for date in value.split(',').filter_map(parse_ical_date) {
                if date <= horizon && !excluded_dates.contains(&date) {
                    dates.push(date);
                }
            }
        }
    }
    dates.sort_unstable();
    dates.dedup();
    dates
}

fn parse_ical_date(value: &str) -> Option<NaiveDate> {
    parse_shared_ical_date(value)
}

fn has_exdate(component: &str, date: NaiveDate) -> bool {
    ical_properties(component, "EXDATE")
        .into_iter()
        .flat_map(|value| value.split(',').map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|value| parse_ical_date(&value))
        .any(|excluded| excluded == date)
}

fn ical_property(raw: &str, target: &str) -> Option<String> {
    unfold_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (name == target.to_ascii_uppercase()).then(|| value.to_string())
    })
}

fn ical_properties(raw: &str, target: &str) -> Vec<String> {
    unfold_lines(raw)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            (name == target.to_ascii_uppercase()).then(|| value.to_string())
        })
        .collect()
}

fn parse_display_datetime(value: &str, all_day: bool) -> Option<i64> {
    local_timestamp(parse_display_naive(value, all_day)?)
}

fn parse_display_naive(value: &str, all_day: bool) -> Option<NaiveDateTime> {
    let value = value.trim();
    if !value.is_ascii() {
        return None;
    }
    if all_day || value.len() == 10 {
        let date = NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()?;
        return date.and_hms_opt(DEFAULT_ALL_DAY_HOUR, 0, 0);
    }
    let normalized = value.get(..16)?;
    NaiveDateTime::parse_from_str(normalized, "%Y-%m-%d %H:%M").ok()
}

fn local_timestamp(naive: NaiveDateTime) -> Option<i64> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value.timestamp()),
        LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
        LocalResult::None => (1..=180).find_map(|minutes| {
            let shifted = naive.checked_add_signed(Duration::minutes(minutes))?;
            match Local.from_local_datetime(&shifted) {
                LocalResult::Single(value) => Some(value.timestamp()),
                LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
                LocalResult::None => None,
            }
        }),
    }
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split_terminator('\n')
    {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.get(1..).unwrap_or_default());
            }
        } else if !raw_line.is_empty() || !lines.is_empty() {
            lines.push(raw_line.to_string());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_relative_and_absolute_display_alarms() {
        let component = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "DTSTART:20260811T100000Z\r\nDTEND:20260811T110000Z\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20260811T120000Z\r\nEND:VALARM\r\n",
            "END:VEVENT\r\n"
        );
        let event = CalendarEventRecord {
            uid: "event-1".to_string(),
            source_uid: "local".to_string(),
            source_display_name: "Local".to_string(),
            component: component.to_string(),
            summary: "Meeting".to_string(),
            start: "2026-08-11 10:00".to_string(),
            end: "2026-08-11 11:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        let candidates = event_reminder_candidates(&event, 1_786_437_600);
        assert_eq!(candidates.len(), 2);
        let event_start =
            property_local_timestamp("DTSTART", "20260811T100000Z", DEFAULT_ALL_DAY_HOUR).unwrap();
        assert_eq!(
            candidates[0].due_at,
            event_start - chrono::Duration::minutes(15).num_seconds()
        );
        assert_eq!(
            candidates[1].due_at,
            property_local_timestamp(
                "TRIGGER;VALUE=DATE-TIME",
                "20260811T120000Z",
                DEFAULT_ALL_DAY_HOUR,
            )
            .unwrap()
        );
    }

    #[test]
    fn respects_related_end_and_skips_non_display_alarms() {
        let component = concat!(
            "BEGIN:VEVENT\r\nUID:event-2\r\n",
            "BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT10M\r\nEND:VALARM\r\n",
            "END:VEVENT\r\n"
        );
        let event = CalendarEventRecord {
            uid: "event-2".to_string(),
            source_uid: "local".to_string(),
            source_display_name: "Local".to_string(),
            component: component.to_string(),
            summary: "Meeting".to_string(),
            start: "2026-08-11 10:00".to_string(),
            end: "2026-08-11 11:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        let candidates = event_reminder_candidates(&event, 1_786_437_600);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].due_at,
            local_timestamp(
                NaiveDate::from_ymd_opt(2026, 8, 11)
                    .unwrap()
                    .and_hms_opt(10, 50, 0)
                    .unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn expands_recurring_event_alarms_with_count_and_exdate() {
        let component = concat!(
            "BEGIN:VEVENT\r\nUID:event-recurring\r\n",
            "DTSTART:20260811T100000Z\r\nDTEND:20260811T110000Z\r\n",
            "RRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20260812T100000Z\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n",
            "END:VEVENT\r\n"
        );
        let event = CalendarEventRecord {
            uid: "event-recurring".to_string(),
            source_uid: "local".to_string(),
            source_display_name: "Local".to_string(),
            component: component.to_string(),
            summary: "Daily meeting".to_string(),
            start: "2026-08-11 10:00".to_string(),
            end: "2026-08-11 11:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        let now = local_timestamp(
            NaiveDate::from_ymd_opt(2026, 8, 11)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        )
        .unwrap();
        assert!(has_exdate(
            &component,
            NaiveDate::from_ymd_opt(2026, 8, 12).unwrap()
        ));
        let snapshot = CalendarSnapshot {
            events: vec![event],
            ..CalendarSnapshot::default()
        };
        let candidates = candidates_from_snapshot(&snapshot, now);
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].key.contains("20260811"));
        assert!(candidates[1].key.contains("20260813"));
    }

    #[test]
    fn reminder_candidates_include_rdate_occurrences() {
        let component = concat!(
            "BEGIN:VEVENT\r\nUID:event-rdate\r\n",
            "DTSTART:20260811T100000Z\r\nDTEND:20260811T110000Z\r\n",
            "RDATE:20260812T100000Z,20260813T100000Z\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n",
            "END:VEVENT\r\n"
        );
        let event = CalendarEventRecord {
            uid: "event-rdate".to_string(),
            source_uid: "local".to_string(),
            source_display_name: "Local".to_string(),
            component: component.to_string(),
            summary: "Additional meetings".to_string(),
            start: "2026-08-11 10:00".to_string(),
            end: "2026-08-11 11:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        let now = local_timestamp(
            NaiveDate::from_ymd_opt(2026, 8, 11)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        )
        .unwrap();
        let snapshot = CalendarSnapshot {
            events: vec![event],
            ..CalendarSnapshot::default()
        };
        let candidates = candidates_from_snapshot(&snapshot, now);
        assert_eq!(candidates.len(), 3);
        assert!(candidates[1].key.contains("20260812"));
        assert!(candidates[2].key.contains("20260813"));
    }

    #[test]
    fn reminder_store_persists_delivery_and_snooze_state() {
        let path = temporary_path("reminders");
        let mut store = ReminderStore::with_path(&path).expect("store should load");
        let candidate = ReminderCandidate {
            key: test_candidate(ReminderComponentKind::Event, 100).key,
            title: "Meeting".to_string(),
            due_at: 100,
            kind: ReminderKind::Event,
        };
        assert_eq!(
            store.due_reminders(std::slice::from_ref(&candidate), 100),
            vec![candidate.clone()]
        );
        store
            .snooze_until(&candidate.key, 200)
            .expect("snooze should save");
        assert!(
            store
                .due_reminders(std::slice::from_ref(&candidate), 199)
                .is_empty()
        );
        drop(store);

        let mut reopened = ReminderStore::with_path(&path).expect("store should reopen");
        assert_eq!(
            reopened.due_reminders(std::slice::from_ref(&candidate), 200),
            vec![candidate.clone()]
        );
        reopened
            .mark_delivered(&candidate.key)
            .expect("delivery should save");
        drop(reopened);

        let mut final_store = ReminderStore::with_path(&path).expect("store should reopen again");
        assert!(
            final_store
                .due_reminders(std::slice::from_ref(&candidate), 300)
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reminder_identity_roundtrips_all_types_and_reserved_values() {
        for kind in [
            ReminderComponentKind::Event,
            ReminderComponentKind::Task,
            ReminderComponentKind::Memo,
        ] {
            let identity =
                ReminderIdentity::new(kind, "source uid:ä?&/calendar", "component uid:ä?&=")
                    .expect("identity should validate");
            let uri = identity.to_uri();
            assert!(uri.contains("%20"));
            assert!(uri.contains("%2F"));
            assert_eq!(ReminderIdentity::parse(&uri), Ok(identity));
        }
    }

    #[test]
    fn reminder_identity_rejects_malformed_unknown_empty_control_traversal_and_oversize() {
        let invalid = [
            "calendar:///?source-uid=a&comp-uid=b&extra=c",
            "calendar:///?source-uid=a&source-uid=b",
            "calendar:///?source-uid=a&unknown=b",
            "calendar:///?source-uid=%&comp-uid=b",
            "calendar:///?source-uid=%2f&comp-uid=b",
            "calendar:///?source-uid=a%00b&comp-uid=b",
            "calendar:///?source-uid=a%2F..%2Fb&comp-uid=b",
            "http:///?source-uid=a&comp-uid=b",
            "calendar:///?source-uid=&comp-uid=b",
            "calendar:///?source-uid=a&comp-uid=.",
            "calendar:/?source-uid=a&comp-uid=b",
        ];
        for uri in invalid {
            assert!(
                ReminderIdentity::parse(uri).is_err(),
                "URI should be rejected: {uri}"
            );
        }
        let oversized = format!(
            "calendar:///?source-uid={}&comp-uid=b",
            "x".repeat(MAX_REMINDER_UID_BYTES + 1)
        );
        assert_eq!(
            ReminderIdentity::parse(&oversized),
            Err(ReminderIdentityError::Oversized)
        );
        assert!(ReminderIdentity::new(ReminderComponentKind::Event, "a\n", "b").is_err());
        assert!(ReminderIdentity::new(ReminderComponentKind::Event, "..", "b").is_err());
    }

    #[test]
    fn reminder_uri_is_canonical_and_decodes_percent_once() {
        let identity = ReminderIdentity::new(
            ReminderComponentKind::Event,
            "literal %2F and unicode ä",
            "component",
        )
        .unwrap();
        let uri = identity.to_uri();
        assert_eq!(ReminderIdentity::parse(&uri), Ok(identity.clone()));

        let double_encoded = "calendar:///?source-uid=a%252Fb&comp-uid=b";
        let parsed = ReminderIdentity::parse(double_encoded).unwrap();
        assert_eq!(parsed.source_uid, "a%2Fb");
        assert_eq!(parsed.to_uri(), double_encoded);

        for uri in [
            "calendar:///?source-uid=%c3%a4&comp-uid=b",
            "calendar:///?source-uid=%C3%28&comp-uid=b",
            "calendar:///?source-uid=%C3&comp-uid=b",
            "calendar:///?source-uid=%00&comp-uid=b",
            "calendar:///?source-uid=%2E%2E%2Fentry&comp-uid=b",
            "calendar:///?source-uid=a&comp-uid=b&source-uid=c",
        ] {
            assert!(
                ReminderIdentity::parse(uri).is_err(),
                "non-canonical or unsafe URI should be rejected: {uri}"
            );
        }
    }

    #[test]
    fn component_presence_distinguishes_source_component_and_type() {
        let snapshot = CalendarSnapshot {
            events: vec![CalendarEventRecord {
                uid: "event-1".to_string(),
                source_uid: "source-1".to_string(),
                source_display_name: "Source".to_string(),
                component: "BEGIN:VEVENT\r\nEND:VEVENT\r\n".to_string(),
                summary: "Event".to_string(),
                start: String::new(),
                end: String::new(),
                location: String::new(),
                description: String::new(),
                all_day: false,
            }],
            tasks: vec![TaskRecord {
                uid: "task-1".to_string(),
                source_uid: "source-1".to_string(),
                source_display_name: "Source".to_string(),
                component: "BEGIN:VTODO\r\nEND:VTODO\r\n".to_string(),
                summary: "Task".to_string(),
                due: String::new(),
                priority: 0,
                completed: false,
            }],
            memos: vec![super::super::calendar_store_service::MemoRecord {
                uid: "memo-1".to_string(),
                source_uid: "source-1".to_string(),
                source_display_name: "Source".to_string(),
                component: "BEGIN:VJOURNAL\r\nEND:VJOURNAL\r\n".to_string(),
                summary: "Memo".to_string(),
                description: String::new(),
                updated: String::new(),
            }],
        };
        assert_eq!(
            component_presence(
                &snapshot,
                &ReminderIdentity::new(ReminderComponentKind::Event, "source-1", "event-1")
                    .unwrap()
            ),
            ReminderComponentPresence::Found
        );
        assert_eq!(
            component_presence(
                &snapshot,
                &ReminderIdentity::new(ReminderComponentKind::Memo, "source-1", "memo-1").unwrap()
            ),
            ReminderComponentPresence::Found
        );
        assert_eq!(
            component_presence(
                &snapshot,
                &ReminderIdentity::new(ReminderComponentKind::Event, "source-1", "missing")
                    .unwrap()
            ),
            ReminderComponentPresence::MissingComponent
        );
        assert_eq!(
            component_presence(
                &snapshot,
                &ReminderIdentity::new(ReminderComponentKind::Memo, "missing", "memo-1").unwrap()
            ),
            ReminderComponentPresence::MissingSource
        );

        let task_only = CalendarSnapshot {
            tasks: snapshot.tasks.clone(),
            ..CalendarSnapshot::default()
        };
        assert_eq!(
            component_presence(
                &task_only,
                &ReminderIdentity::new(ReminderComponentKind::Event, "source-1", "event-1")
                    .unwrap()
            ),
            ReminderComponentPresence::MissingSource
        );
    }

    #[test]
    fn reminder_action_targets_roundtrip_and_deduplicate_repeated_activation() {
        let candidate = test_candidate(ReminderComponentKind::Task, 100);
        let delivery = ReminderDelivery {
            candidate,
            claim_token: "100-42-1".to_string(),
        };
        let target = delivery.action_target().expect("target should validate");
        let parsed = parse_reminder_action_target(&target).expect("target should parse");
        assert_eq!(parsed.identity, delivery.identity().unwrap());
        assert_eq!(parsed.key, delivery.candidate.key);
        assert_eq!(
            deduplicate_deliveries(&[delivery.clone(), delivery.clone()]),
            vec![delivery.clone()]
        );
        assert!(parse_reminder_action_target("v1|calendar:///bad|bad|bad").is_err());
        assert!(parse_reminder_action_target(&target.replacen("%3A", "%3a", 1)).is_err());
        assert!(
            ReminderDelivery {
                candidate: delivery.candidate.clone(),
                claim_token: "token/with-slash".to_string(),
            }
            .action_target()
            .is_err()
        );

        let long_identity = ReminderIdentity::new(
            ReminderComponentKind::Event,
            "s".repeat(MAX_REMINDER_UID_BYTES),
            "c".repeat(MAX_REMINDER_UID_BYTES),
        )
        .unwrap();
        let long_candidate = ReminderCandidate {
            key: reminder_key(
                &long_identity,
                &"o".repeat(MAX_REMINDER_UID_BYTES),
                0,
                0,
                100,
            ),
            title: "Long reminder".to_string(),
            due_at: 100,
            kind: ReminderKind::Event,
        };
        let long_delivery = ReminderDelivery {
            candidate: long_candidate,
            claim_token: "100-42-1".to_string(),
        };
        let long_target = long_delivery.action_target().unwrap();
        assert_eq!(
            parse_reminder_action_target(&long_target).unwrap().identity,
            long_identity
        );
    }

    #[test]
    fn reminder_claim_delivery_is_exactly_once_and_stale_claim_is_ignored() {
        let path = temporary_path("claim");
        let candidate = test_candidate(ReminderComponentKind::Event, 100);
        let mut first = ReminderStore::with_path(&path).unwrap();
        let claimed = first
            .claim_due_reminders(std::slice::from_ref(&candidate), 100)
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let token = claimed[0].claim_token.clone();

        let mut second = ReminderStore::with_path(&path).unwrap();
        assert!(
            second
                .claim_due_reminders(std::slice::from_ref(&candidate), 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            second
                .mark_delivered_if_claimed_at(&candidate.key, "stale-token", 100)
                .unwrap()
                == false
        );
        assert!(
            second
                .mark_delivered_if_claimed_at(&candidate.key, &token, 100)
                .unwrap()
        );
        assert!(
            second
                .snooze_if_current_at(&candidate.key, &token, 200, 101)
                .unwrap()
        );
        assert!(
            !second
                .dismiss_if_current_at(&candidate.key, &token, 101)
                .unwrap()
        );
        drop(second);

        let mut reopened = ReminderStore::with_path(&path).unwrap();
        assert!(
            reopened
                .claim_due_reminders(std::slice::from_ref(&candidate), 101)
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn concurrent_claimers_produce_one_delivery() {
        let path = temporary_path("concurrent-claim");
        let candidate = test_candidate(ReminderComponentKind::Event, 100);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let candidate = candidate.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                ReminderStore::with_path(path)
                    .unwrap()
                    .claim_due_reminders(std::slice::from_ref(&candidate), 100)
                    .unwrap()
                    .len()
            }));
        }
        let claimed: usize = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum();
        assert_eq!(claimed, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reminder_scheduler_claim_uses_deterministic_clock_and_store_contract() {
        let path = temporary_path("scheduler-claim");
        let clock = FakeClock::new(100);
        let scheduler = ReminderScheduler::new(clock.clone());
        let candidate = test_candidate(ReminderComponentKind::Task, 100);
        let mut store = ReminderStore::with_path(&path).unwrap();

        assert_eq!(
            scheduler
                .claim_due_reminders(&mut store, std::slice::from_ref(&candidate))
                .unwrap()
                .len(),
            1
        );
        assert!(
            scheduler
                .claim_due_reminders(&mut store, std::slice::from_ref(&candidate))
                .unwrap()
                .is_empty()
        );
        clock.advance(MAX_CLAIM_SECONDS);
        assert_eq!(
            scheduler
                .claim_due_reminders(&mut store, std::slice::from_ref(&candidate))
                .unwrap()
                .len(),
            1
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reminder_snooze_dismiss_and_restart_survive_restart() {
        let path = temporary_path("restart");
        let candidate = test_candidate(ReminderComponentKind::Memo, 100);
        let mut store = ReminderStore::with_path(&path).unwrap();
        let delivery = store
            .claim_due_reminders(std::slice::from_ref(&candidate), 100)
            .unwrap()
            .pop()
            .unwrap();
        assert!(
            store
                .snooze_if_current_at(&candidate.key, &delivery.claim_token, 200, 100)
                .unwrap()
        );
        drop(store);

        let mut snoozed = ReminderStore::with_path(&path).unwrap();
        assert!(
            snoozed
                .claim_due_reminders(std::slice::from_ref(&candidate), 199)
                .unwrap()
                .is_empty()
        );
        let replacement = snoozed
            .claim_due_reminders(std::slice::from_ref(&candidate), 200)
            .unwrap()
            .pop()
            .unwrap();
        assert!(
            !snoozed
                .dismiss_if_current_at(&candidate.key, &delivery.claim_token, 200)
                .unwrap()
        );
        assert!(
            snoozed
                .dismiss_if_current_at(&candidate.key, &replacement.claim_token, 200)
                .unwrap()
        );
        drop(snoozed);

        let mut dismissed = ReminderStore::with_path(&path).unwrap();
        assert!(
            dismissed
                .claim_due_reminders(std::slice::from_ref(&candidate), 300)
                .unwrap()
                .is_empty()
        );
        dismissed.restart(&candidate.key).unwrap();
        assert_eq!(
            dismissed
                .claim_due_reminders(std::slice::from_ref(&candidate), 300)
                .unwrap()
                .len(),
            1
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn expired_and_crossed_reminder_actions_are_rejected() {
        let path = temporary_path("stale-action");
        let candidate = test_candidate(ReminderComponentKind::Event, 100);
        let mut store = ReminderStore::with_path(&path).unwrap();
        let first = store
            .claim_due_reminders(std::slice::from_ref(&candidate), 100)
            .unwrap()
            .pop()
            .unwrap();

        assert!(
            !store
                .dismiss_if_current_at(&candidate.key, &first.claim_token, 400)
                .unwrap()
        );
        let replacement = store
            .claim_due_reminders(std::slice::from_ref(&candidate), 400)
            .unwrap()
            .pop()
            .unwrap();
        assert_ne!(first.claim_token, replacement.claim_token);
        assert!(
            !store
                .dismiss_if_current_at(&candidate.key, &first.claim_token, 400)
                .unwrap()
        );

        assert!(
            store
                .snooze_if_current_at(&candidate.key, &replacement.claim_token, 500, 400)
                .unwrap()
        );
        // A retry of the same snooze is idempotent, while a different stale
        // operation cannot turn the snooze into a dismissal.
        assert!(
            store
                .snooze_if_current_at(&candidate.key, &replacement.claim_token, 500, 401)
                .unwrap()
        );
        assert!(
            !store
                .dismiss_if_current_at(&candidate.key, &replacement.claim_token, 401)
                .unwrap()
        );
        drop(store);

        let mut reopened = ReminderStore::with_path(&path).unwrap();
        assert!(
            reopened
                .claim_due_reminders(std::slice::from_ref(&candidate), 499)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened
                .claim_due_reminders(std::slice::from_ref(&candidate), 500)
                .unwrap()
                .len(),
            1
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_completion_without_timestamp_is_not_actionable() {
        let path = temporary_path("legacy-completion");
        let candidate = test_candidate(ReminderComponentKind::Event, 100);
        let token = "100-legacy-claim".to_string();
        let mut state = ReminderState::default();
        state.delivered.insert(candidate.key.clone());
        state
            .delivery_tokens
            .insert(candidate.key.clone(), token.clone());
        save_reminder_state(&path, &state).unwrap();

        let mut store = ReminderStore::with_path(&path).unwrap();
        assert!(
            !store
                .dismiss_if_current_at(&candidate.key, &token, 400)
                .unwrap()
        );
        assert!(
            store
                .claim_due_reminders(std::slice::from_ref(&candidate), 400)
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn failed_state_write_keeps_previous_state_and_does_not_claim() {
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::temp_dir().join(format!(
            "meridian-failed-write-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir(&parent).unwrap();
        let path = parent.join("reminders.json");
        let candidate = test_candidate(ReminderComponentKind::Event, 100);
        let mut store = ReminderStore::with_path(&path).unwrap();
        store.snooze_until(&candidate.key, 200).unwrap();
        let before = fs::read(&path).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
        let result = store.restart(&candidate.key);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut reopened = ReminderStore::with_path(&path).unwrap();
        assert!(
            reopened
                .claim_due_reminders(std::slice::from_ref(&candidate), 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened
                .claim_due_reminders(std::slice::from_ref(&candidate), 200)
                .unwrap()
                .len(),
            1
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn failed_atomic_save_cleans_temp_and_preserves_destination() {
        let parent = std::env::temp_dir().join(format!(
            "meridian-reminder-atomic-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir(&parent).unwrap();
        let destination = parent.join("occupied.json");
        fs::create_dir(&destination).unwrap();

        assert!(save_reminder_state(&destination, &ReminderState::default()).is_err());
        assert!(destination.is_dir());
        let prefix = ".occupied.json.tmp-";
        assert!(
            !fs::read_dir(&parent)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        );
        let _ = fs::remove_dir_all(parent);
    }

    fn test_candidate(kind: ReminderComponentKind, due_at: i64) -> ReminderCandidate {
        let identity = ReminderIdentity::new(kind, "source-1", "component-1").unwrap();
        ReminderCandidate {
            key: reminder_key(&identity, "single", 0, 0, due_at),
            title: "Test reminder".to_string(),
            due_at,
            kind: kind.into(),
        }
    }

    #[test]
    fn parses_duration_units_and_rejects_malformed_values() {
        assert_eq!(
            parse_ical_duration("-P1DT2H30M"),
            Some(-(24 * 60 * 60 + 2 * 60 * 60 + 30 * 60))
        );
        assert_eq!(parse_ical_duration("P2W"), Some(14 * 24 * 60 * 60));
        assert_eq!(parse_ical_duration("PT0S"), Some(0));
        assert_eq!(parse_ical_duration("P"), None);
        assert_eq!(parse_ical_duration("-garbage"), None);
    }

    fn temporary_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "meridian-{label}-{}-{unique}.json",
            std::process::id()
        ))
    }

    #[test]
    fn test_parse_contact_date() {
        assert_eq!(parse_contact_date("1990-05-15"), Some((Some(1990), 5, 15)));
        assert_eq!(
            parse_contact_date("1990-05-15T09:30:00"),
            Some((Some(1990), 5, 15))
        );
        assert_eq!(parse_contact_date("19900515"), Some((Some(1990), 5, 15)));
        assert_eq!(parse_contact_date("--05-15"), Some((None, 5, 15)));
        assert_eq!(parse_contact_date("--0515"), Some((None, 5, 15)));
        assert_eq!(parse_contact_date(""), None);
        assert_eq!(parse_contact_date("not-a-date"), None);
        assert_eq!(parse_contact_date("1990-13-01"), None);
        assert_eq!(parse_contact_date("1990-00-10"), None);
        assert_eq!(parse_contact_date("1990-05-32"), None);
    }

    #[test]
    fn test_birthday_candidate() {
        use crate::services::contact_store_service::LocalContactRecord;
        use crate::services::vcard_service::VCardContact;

        let vcard_str = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Alice Smith\r\nBDAY:1990-06-15\r\nANNIVERSARY:2015-10-20\r\nEND:VCARD\r\n";
        let contact = VCardContact::parse_vcard_string(vcard_str).expect("parse contact");
        let record = LocalContactRecord {
            id: "contact-alice-1".to_string(),
            contact,
            vcard: vcard_str.to_string(),
        };

        // Reference now: 2026-06-01 12:00:00 UTC
        let now_dt = chrono::NaiveDate::from_ymd_opt(2026, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let now = chrono::Local
            .from_local_datetime(&now_dt)
            .unwrap()
            .timestamp();
        let offset = 900; // 15 minutes before

        let candidates = contact_birthday_candidates(&[record], now, offset);
        assert!(!candidates.is_empty());

        // Find birthday candidate for 2026
        let bday_2026 = candidates.iter().find(|c| c.key.contains("bday-20260615"));
        assert!(bday_2026.is_some(), "should find 2026 birthday candidate");
        let bday = bday_2026.unwrap();
        assert_eq!(bday.title, "Alice Smith: 36. Birthday");
        assert_eq!(bday.kind, ReminderKind::Contact);

        // Check offset applied: due_at should be 9:00 - 900s (8:45) local time on 2026-06-15
        let expected_bday_dt = chrono::NaiveDate::from_ymd_opt(2026, 6, 15)
            .unwrap()
            .and_hms_opt(DEFAULT_ALL_DAY_HOUR, 0, 0)
            .unwrap();
        let expected_ts = chrono::Local
            .from_local_datetime(&expected_bday_dt)
            .unwrap()
            .timestamp()
            - offset;
        assert_eq!(bday.due_at, expected_ts);

        // Find anniversary candidate for 2026
        let anni_2026 = candidates.iter().find(|c| c.key.contains("anni-20261020"));
        assert!(anni_2026.is_some(), "should find 2026 anniversary candidate");
        let anni = anni_2026.unwrap();
        assert_eq!(anni.title, "Alice Smith: 11. Anniversary");

        // Leap year birthday: Feb 29 contact evaluated in non-leap year (e.g. 2025 or 2026)
        let leap_vcard = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Leap Baby\r\nBDAY:2004-02-29\r\nEND:VCARD\r\n";
        let leap_contact = VCardContact::parse_vcard_string(leap_vcard).unwrap();
        let leap_record = LocalContactRecord {
            id: "contact-leap-1".to_string(),
            contact: leap_contact,
            vcard: leap_vcard.to_string(),
        };
        let leap_candidates = contact_birthday_candidates(&[leap_record], now, 0);
        // Non-leap year 2026 should fall back to Feb 28
        let leap_2026 = leap_candidates.iter().find(|c| c.key.contains("20260228"));
        assert!(leap_2026.is_some(), "leap day should fall back to Feb 28 in non-leap year");

        // Test unnamed contact fallback
        let mut noname_contact = VCardContact::parse_vcard_string(vcard_str).unwrap();
        noname_contact.fn_name = "   ".to_string();
        let noname_record = LocalContactRecord {
            id: "contact-noname-1".to_string(),
            contact: noname_contact,
            vcard: vcard_str.to_string(),
        };
        let noname_candidates = contact_birthday_candidates(&[noname_record], now, 0);
        let noname_candidate = noname_candidates.iter().find(|c| c.key.contains("20260615"));
        assert!(noname_candidate.is_some());
        assert!(noname_candidate.unwrap().title.starts_with("Contact:"));
    }

    #[test]
    fn test_adversarial_contact_date_parsing_resilience() {
        // 1. Valid standard ISO representations
        assert_eq!(parse_contact_date("1990-05-15"), Some((Some(1990), 5, 15)));
        assert_eq!(
            parse_contact_date("1990-05-15T09:30:00"),
            Some((Some(1990), 5, 15))
        );
        assert_eq!(
            parse_contact_date("1990-05-15T09:30:00Z"),
            Some((Some(1990), 5, 15))
        );
        assert_eq!(
            parse_contact_date("1990-05-15T09:30:00+02:00"),
            Some((Some(1990), 5, 15))
        );

        // 2. Valid compact YYYYMMDD
        assert_eq!(parse_contact_date("19900515"), Some((Some(1990), 5, 15)));
        assert_eq!(
            parse_contact_date("19900515T120000"),
            Some((Some(1990), 5, 15))
        );

        // 3. Valid yearless representations
        assert_eq!(parse_contact_date("--05-15"), Some((None, 5, 15)));
        assert_eq!(parse_contact_date("--0515"), Some((None, 5, 15)));

        // 4. Leap year dates
        assert_eq!(parse_contact_date("2000-02-29"), Some((Some(2000), 2, 29)));
        assert_eq!(parse_contact_date("--02-29"), Some((None, 2, 29)));

        // 5. Whitespace resilience
        assert_eq!(
            parse_contact_date("  1990-05-15  "),
            Some((Some(1990), 5, 15))
        );
        assert_eq!(parse_contact_date(" --05-15 "), Some((None, 5, 15)));
        assert_eq!(parse_contact_date(""), None);
        assert_eq!(parse_contact_date("   "), None);
        assert_eq!(parse_contact_date("\t\n\r"), None);

        // 6. Out-of-range month / day values
        assert_eq!(parse_contact_date("1990-13-01"), None);
        assert_eq!(parse_contact_date("1990-00-10"), None);
        assert_eq!(parse_contact_date("1990-05-32"), None);
        assert_eq!(parse_contact_date("1990-05-00"), None);
        assert_eq!(parse_contact_date("--13-01"), None);
        assert_eq!(parse_contact_date("--00-10"), None);
        assert_eq!(parse_contact_date("--05-32"), None);
        assert_eq!(parse_contact_date("--05-00"), None);

        // 7. Structural malformations & invalid text
        assert_eq!(parse_contact_date("not-a-date"), None);
        assert_eq!(parse_contact_date("1990/05/15"), None);
        assert_eq!(parse_contact_date("1990-5-15"), None);
        assert_eq!(parse_contact_date("1990-05-5"), None);
        assert_eq!(parse_contact_date("1990051"), None);
        assert_eq!(parse_contact_date("--"), None);
        assert_eq!(parse_contact_date("---"), None);
        assert_eq!(parse_contact_date("--12"), None);
        assert_eq!(parse_contact_date("--12345"), None);

        // 8. UTF-8 Multi-byte and Emoji Resilience (Zero Panics, graceful None fallback)
        // 8a. Yearless emojis and currency symbols
        assert_eq!(parse_contact_date("--🎉"), None);
        assert_eq!(parse_contact_date("--🎂"), None);
        assert_eq!(parse_contact_date("--€1"), None);
        assert_eq!(parse_contact_date("--1€"), None);
        assert_eq!(parse_contact_date("--\u{FFFD}"), None);
        assert_eq!(parse_contact_date("--\u{00A0}"), None);
        assert_eq!(parse_contact_date("--月-日"), None);
        assert_eq!(parse_contact_date("--🎉-15"), None);
        assert_eq!(parse_contact_date("--05-🎉"), None);

        // 8b. ISO emojis and malformed structures
        assert_eq!(parse_contact_date("2020-05-🎉"), None);
        assert_eq!(parse_contact_date("2020-05-€"), None);
        assert_eq!(parse_contact_date("2020-🎉-15"), None);
        assert_eq!(parse_contact_date("🎉-05-15"), None);
        assert_eq!(parse_contact_date("🎉"), None);
        // Valid ISO date prefix followed by time separator and emoji safely extracts date without panic
        assert_eq!(
            parse_contact_date("2020-05-15T🎉"),
            Some((Some(2020), 5, 15))
        );

        // 8c. Compact emojis
        assert_eq!(parse_contact_date("2020🎉15"), None);
        assert_eq!(parse_contact_date("🎉0515"), None);
        assert_eq!(parse_contact_date("202005🎉"), None);
        assert_eq!(parse_contact_date("🎉-🎉-🎉"), None);

        // 8d. Overflow, boundary, and malformed dates
        assert_eq!(parse_contact_date("99999-99-99"), None);
        assert_eq!(parse_contact_date("-0001-01-01"), None);
        assert_eq!(parse_contact_date(""), None);
        assert_eq!(parse_contact_date("  "), None);
        assert_eq!(parse_contact_date("--"), None);
        assert_eq!(parse_contact_date("--00-00"), None);
        assert_eq!(parse_contact_date("--13-01"), None);
        assert_eq!(parse_contact_date("2020-00-15"), None);
        assert_eq!(parse_contact_date("2020-13-15"), None);
        assert_eq!(parse_contact_date("2020-05-00"), None);
        assert_eq!(parse_contact_date("2020-05-32"), None);
        assert_eq!(parse_contact_date("20200015"), None);
        assert_eq!(parse_contact_date("20201315"), None);
        assert_eq!(parse_contact_date("20200500"), None);
        assert_eq!(parse_contact_date("20200532"), None);

        // 8e. Multi-byte combining accents, BiDi overrides, RTL, and null bytes
        assert_eq!(parse_contact_date("2020-05-1e\u{0301}"), None);
        assert_eq!(parse_contact_date("\u{200F}2020-05-15"), None);
        assert_eq!(parse_contact_date("\u{202E}2020-05-15"), None);
        assert_eq!(parse_contact_date("2020\0-05-15"), None);
        assert_eq!(parse_contact_date("2020-05-15\0"), Some((Some(2020), 5, 15)));
        assert_eq!(parse_contact_date("٢٠٢٠-٠٥-١٥"), None);
        assert_eq!(parse_contact_date("２０２０-０５-１５"), None);

        // 8f. Empirical Stress & Fuzz Generator: 1,000 adversarial payloads
        let mut rng_state: u64 = 0xDEADBEEFCAFEBABE;
        let adversarial_tokens = [
            "", "-", "--", "---", " ", "\t", "\n", "\0",
            "🎉", "🎂", "€", "$", "¥", "£",
            "\u{FFFD}", "\u{0301}", "\u{200F}", "\u{202E}", "日", "月", "年",
            "2020", "2026", "9999", "0000", "-001", "12345",
            "01", "05", "12", "13", "00", "31", "32", "99",
        ];
        for _ in 0..1000 {
            // Simple deterministic LCG pseudo-random string generator
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let token_count = ((rng_state >> 32) % 6 + 1) as usize;
            let mut payload = String::new();
            for i in 0..token_count {
                let idx = ((rng_state.wrapping_shr((i * 8) as u32)) % (adversarial_tokens.len() as u64)) as usize;
                payload.push_str(adversarial_tokens[idx]);
            }
            // CRITICAL INVARIANT: parse_contact_date MUST NEVER panic on any payload
            let res = parse_contact_date(&payload);
            if let Some((year_opt, month, day)) = res {
                assert!((1..=12).contains(&month), "month {month} must be in 1..=12 for payload {payload:?}");
                assert!((1..=31).contains(&day), "day {day} must be in 1..=31 for payload {payload:?}");
                if let Some(y) = year_opt {
                    assert!(y >= 0, "year {y} must be non-negative for payload {payload:?}");
                }
            }
        }

        // 8g. Contact birthday candidate generation with None fallback
        use crate::services::contact_store_service::LocalContactRecord;
        use crate::services::vcard_service::VCardContact;
        let malformed_payloads = [
            "--🎉", "--🎂", "--€1", "--1€", "--\u{FFFD}",
            "2020-05-🎉", "2020-🎉-15", "🎉-05-15",
            "2020🎉15", "🎉0515", "99999-99-99", "-0001-01-01",
            "", "  ", "--", "--00-00", "--13-01",
        ];
        for (i, bad_date) in malformed_payloads.iter().enumerate() {
            let vcard = format!("BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Bad Date {i}\r\nBDAY:{bad_date}\r\nANNIVERSARY:{bad_date}\r\nEND:VCARD\r\n");
            let contact = VCardContact::parse_vcard_string(&vcard).unwrap();
            let record = LocalContactRecord {
                id: format!("contact-bad-{i}"),
                contact,
                vcard,
            };
            // Candidate evaluation must handle None gracefully without panic and produce 0 candidates
            let cands = contact_birthday_candidates(&[record], 1772452800, 0);
            assert!(
                cands.is_empty(),
                "malformed date {bad_date:?} must yield zero candidates and never panic"
            );
        }
    }

    #[test]
    fn test_adversarial_contact_birthday_leap_year_and_candidate_generation() {
        use crate::services::contact_store_service::LocalContactRecord;
        use crate::services::vcard_service::VCardContact;

        // Reference now: 2026-06-01 12:00:00 (non-leap year)
        let now_2026_dt = chrono::NaiveDate::from_ymd_opt(2026, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let now_2026 = chrono::Local
            .from_local_datetime(&now_2026_dt)
            .unwrap()
            .timestamp();

        // Reference now: 2028-06-01 12:00:00 (leap year)
        let now_2028_dt = chrono::NaiveDate::from_ymd_opt(2028, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let now_2028 = chrono::Local
            .from_local_datetime(&now_2028_dt)
            .unwrap()
            .timestamp();

        // 1. Leap year birth date: 2004-02-29
        let leap_vcard = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Leap Baby\r\nBDAY:2004-02-29\r\nEND:VCARD\r\n";
        let leap_contact = VCardContact::parse_vcard_string(leap_vcard).unwrap();
        let leap_record = LocalContactRecord {
            id: "contact-leap-1".to_string(),
            contact: leap_contact,
            vcard: leap_vcard.to_string(),
        };

        // In non-leap year 2026: must safely fall back to Feb 28 without panicking
        let cands_2026 = contact_birthday_candidates(&[leap_record.clone()], now_2026, 0);
        let cand_2026 = cands_2026
            .iter()
            .find(|c| c.key.contains("20260228"))
            .expect("should find Feb 28 fallback candidate in non-leap year");
        assert_eq!(cand_2026.title, "Leap Baby: 22. Birthday");
        assert!(
            !cands_2026.iter().any(|c| c.key.contains("20260229")),
            "must never generate Feb 29 candidate in non-leap year"
        );

        // In leap year 2028: must generate candidate on Feb 29
        let cands_2028 = contact_birthday_candidates(&[leap_record.clone()], now_2028, 0);
        let cand_2028 = cands_2028
            .iter()
            .find(|c| c.key.contains("20280229"))
            .expect("should find Feb 29 candidate in leap year");
        assert_eq!(cand_2028.title, "Leap Baby: 24. Birthday");

        // 2. Yearless leap day: --02-29
        let yearless_vcard = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Yearless Baby\r\nBDAY:--02-29\r\nEND:VCARD\r\n";
        let yearless_contact = VCardContact::parse_vcard_string(yearless_vcard).unwrap();
        let yearless_record = LocalContactRecord {
            id: "contact-yearless-1".to_string(),
            contact: yearless_contact,
            vcard: yearless_vcard.to_string(),
        };

        // In non-leap year 2026: falls back to Feb 28 without year prefix
        let yearless_cands_2026 =
            contact_birthday_candidates(&[yearless_record.clone()], now_2026, 0);
        let yearless_2026 = yearless_cands_2026
            .iter()
            .find(|c| c.key.contains("20260228"))
            .expect("should fall back to Feb 28 for yearless leap day");
        assert_eq!(yearless_2026.title, "Yearless Baby: Birthday");

        // In leap year 2028: generates Feb 29 without year prefix
        let yearless_cands_2028 = contact_birthday_candidates(&[yearless_record], now_2028, 0);
        let yearless_2028 = yearless_cands_2028
            .iter()
            .find(|c| c.key.contains("20280229"))
            .expect("should evaluate to Feb 29 in leap year for yearless leap day");
        assert_eq!(yearless_2028.title, "Yearless Baby: Birthday");

        // 3. Invalid Feb 30 and April 31: safely omitted without panic
        let invalid_vcard = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Invalid Dates\r\nBDAY:2000-02-30\r\nANNIVERSARY:2010-04-31\r\nEND:VCARD\r\n";
        let invalid_contact = VCardContact::parse_vcard_string(invalid_vcard).unwrap();
        let invalid_record = LocalContactRecord {
            id: "contact-invalid-1".to_string(),
            contact: invalid_contact,
            vcard: invalid_vcard.to_string(),
        };
        let invalid_cands = contact_birthday_candidates(&[invalid_record], now_2026, 0);
        assert!(
            invalid_cands.is_empty(),
            "invalid calendar dates must yield zero candidates"
        );

        // 4. Future birth date & same-year birth date
        let future_vcard = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Future Person\r\nBDAY:2030-05-15\r\nEND:VCARD\r\n";
        let future_contact = VCardContact::parse_vcard_string(future_vcard).unwrap();
        let future_record = LocalContactRecord {
            id: "contact-future-1".to_string(),
            contact: future_contact,
            vcard: future_vcard.to_string(),
        };
        let future_cands = contact_birthday_candidates(&[future_record], now_2026, 0);
        let future_2026 = future_cands
            .iter()
            .find(|c| c.key.contains("20260515"))
            .unwrap();
        assert_eq!(
            future_2026.title, "Future Person: Birthday",
            "future birth year must not cause negative underflow"
        );

        // 5. Empty record ID is safely skipped
        let mut empty_id_record = leap_record;
        empty_id_record.id = "   ".to_string();
        let empty_id_cands = contact_birthday_candidates(&[empty_id_record], now_2026, 0);
        assert!(
            empty_id_cands.is_empty(),
            "records with empty ID must be skipped"
        );
    }

    #[test]
    fn test_adversarial_contact_reminder_lead_time_calculations() {
        use crate::services::contact_store_service::LocalContactRecord;
        use crate::services::settings_service::ReminderDurationUnit;
        use crate::services::vcard_service::VCardContact;

        // 1. Unit conversions
        assert_eq!(ReminderDurationUnit::Minutes.to_seconds(15), 900);
        assert_eq!(ReminderDurationUnit::Minutes.to_minutes(15), 15);

        assert_eq!(ReminderDurationUnit::Hours.to_seconds(2), 7200);
        assert_eq!(ReminderDurationUnit::Hours.to_minutes(2), 120);

        assert_eq!(ReminderDurationUnit::Days.to_seconds(3), 259200);
        assert_eq!(ReminderDurationUnit::Days.to_minutes(3), 4320);

        // Zero intervals
        assert_eq!(ReminderDurationUnit::Minutes.to_seconds(0), 0);
        assert_eq!(ReminderDurationUnit::Hours.to_seconds(0), 0);
        assert_eq!(ReminderDurationUnit::Days.to_seconds(0), 0);

        // Unit string parsing variations
        assert_eq!(
            ReminderDurationUnit::from_str("minutes"),
            ReminderDurationUnit::Minutes
        );
        assert_eq!(
            ReminderDurationUnit::from_str("Minutes"),
            ReminderDurationUnit::Minutes
        );
        assert_eq!(
            ReminderDurationUnit::from_str("hours"),
            ReminderDurationUnit::Hours
        );
        assert_eq!(
            ReminderDurationUnit::from_str("HOURS"),
            ReminderDurationUnit::Hours
        );
        assert_eq!(
            ReminderDurationUnit::from_str("hour"),
            ReminderDurationUnit::Hours
        );
        assert_eq!(
            ReminderDurationUnit::from_str("1"),
            ReminderDurationUnit::Hours
        );
        assert_eq!(
            ReminderDurationUnit::from_str("days"),
            ReminderDurationUnit::Days
        );
        assert_eq!(
            ReminderDurationUnit::from_str("DAYS"),
            ReminderDurationUnit::Days
        );
        assert_eq!(
            ReminderDurationUnit::from_str("day"),
            ReminderDurationUnit::Days
        );
        assert_eq!(
            ReminderDurationUnit::from_str("2"),
            ReminderDurationUnit::Days
        );
        assert_eq!(
            ReminderDurationUnit::from_str("invalid"),
            ReminderDurationUnit::Minutes
        );

        // 2. Candidate generation with different lead-time offsets
        let vcard_str = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Offset Test\r\nBDAY:1995-07-20\r\nEND:VCARD\r\n";
        let contact = VCardContact::parse_vcard_string(vcard_str).unwrap();
        let record = LocalContactRecord {
            id: "contact-offset-1".to_string(),
            contact,
            vcard: vcard_str.to_string(),
        };

        let now_dt = chrono::NaiveDate::from_ymd_opt(2026, 7, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let now = chrono::Local
            .from_local_datetime(&now_dt)
            .unwrap()
            .timestamp();

        let bday_dt = chrono::NaiveDate::from_ymd_opt(2026, 7, 20)
            .unwrap()
            .and_hms_opt(DEFAULT_ALL_DAY_HOUR, 0, 0)
            .unwrap();
        let bday_base_ts = chrono::Local
            .from_local_datetime(&bday_dt)
            .unwrap()
            .timestamp();

        // 15 minutes lead time (900 seconds)
        let cands_15m = contact_birthday_candidates(&[record.clone()], now, 900);
        let cand_15m = cands_15m
            .iter()
            .find(|c| c.key.contains("20260720"))
            .unwrap();
        assert_eq!(cand_15m.due_at, bday_base_ts - 900);

        // 2 hours lead time (7200 seconds)
        let cands_2h = contact_birthday_candidates(&[record.clone()], now, 7200);
        let cand_2h = cands_2h.iter().find(|c| c.key.contains("20260720")).unwrap();
        assert_eq!(cand_2h.due_at, bday_base_ts - 7200);

        // 1 day lead time (86400 seconds)
        let cands_1d = contact_birthday_candidates(&[record], now, 86400);
        let cand_1d = cands_1d.iter().find(|c| c.key.contains("20260720")).unwrap();
        assert_eq!(cand_1d.due_at, bday_base_ts - 86400);
    }

    #[test]
    fn test_adversarial_contact_reminder_store_snooze_dismiss_lifecycle() {
        use crate::services::contact_store_service::LocalContactRecord;
        use crate::services::vcard_service::VCardContact;

        let temp_dir = std::env::temp_dir().join(format!(
            "evo_test_reminders_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let reminder_file = temp_dir.join("reminders.json");
        let mut store = ReminderStore::with_path(&reminder_file).expect("create test ReminderStore");

        let vcard_str = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Store Test\r\nBDAY:1992-09-15\r\nEND:VCARD\r\n";
        let contact = VCardContact::parse_vcard_string(vcard_str).unwrap();
        let record = LocalContactRecord {
            id: "contact-store-1".to_string(),
            contact,
            vcard: vcard_str.to_string(),
        };

        let now_dt = chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let now = chrono::Local
            .from_local_datetime(&now_dt)
            .unwrap()
            .timestamp();

        let candidates = contact_birthday_candidates(&[record], now, 0);
        let cand = candidates
            .iter()
            .find(|c| c.key.contains("20260915"))
            .unwrap()
            .clone();

        // Verify contact reminder identity contract
        let identity = cand.identity().expect("contact candidate must have valid identity");
        assert_eq!(identity.kind, ReminderComponentKind::Contact);
        assert_eq!(identity.source_uid, "contacts");
        assert_eq!(identity.component_uid, "contact-store-1");
        assert_eq!(
            cand.deep_link().unwrap(),
            "contact:///?source-uid=contacts&comp-uid=contact-store-1"
        );

        let due_at = cand.due_at;

        // 1. Before due_at: not due
        let before_due = store.due_reminders(&[cand.clone()], due_at - 100);
        assert!(before_due.is_empty(), "must not be due before due_at");

        // 2. At due_at: due
        let at_due = store.due_reminders(&[cand.clone()], due_at);
        assert_eq!(at_due.len(), 1);
        assert_eq!(at_due[0].key, cand.key);

        // 3. Claim due reminder
        let deliveries = store
            .claim_due_reminders(&[cand.clone()], due_at)
            .expect("claim due reminders");
        assert_eq!(deliveries.len(), 1);
        let delivery = &deliveries[0];
        assert_eq!(delivery.candidate.key, cand.key);
        let claim_token = delivery.claim_token.clone();
        assert!(!claim_token.is_empty());

        // 4. Snooze reminder until due_at + 3600
        let snooze_until = due_at + 3600;
        let snooze_result = store
            .snooze_if_current_at(&cand.key, &claim_token, snooze_until, due_at)
            .expect("snooze contact reminder");
        assert!(snooze_result, "snooze should succeed with valid claim");

        // While snoozed: not due
        let while_snoozed = store.due_reminders(&[cand.clone()], due_at + 1800);
        assert!(while_snoozed.is_empty(), "must not be due while snoozed");

        // When snooze expires: due again
        let after_snooze = store.due_reminders(&[cand.clone()], snooze_until + 1);
        assert_eq!(
            after_snooze.len(),
            1,
            "must be due again once snooze expires"
        );

        // 5. Reclaim after snooze
        let re_deliveries = store
            .claim_due_reminders(&[cand.clone()], snooze_until + 1)
            .expect("reclaim due reminders");
        assert_eq!(re_deliveries.len(), 1);
        let re_token = re_deliveries[0].claim_token.clone();

        // 6. Dismiss reminder
        let dismiss_result = store
            .dismiss_if_current_at(&cand.key, &re_token, snooze_until + 1)
            .expect("dismiss contact reminder");
        assert!(dismiss_result, "dismiss should succeed with valid claim");

        // After dismissal: never due again
        let after_dismiss = store.due_reminders(&[cand.clone()], snooze_until + 10000);
        assert!(
            after_dismiss.is_empty(),
            "must not be due after being dismissed"
        );

        // 7. Security / adversarial rejection
        let invalid_token_snooze = store
            .snooze_if_current_at(&cand.key, "fake-token", snooze_until + 20000, due_at)
            .expect("invalid token check");
        assert!(!invalid_token_snooze, "must reject invalid claim token");

        let invalid_key_dismiss = store
            .dismiss_if_current_at("not-a-valid-reminder-key", &claim_token, due_at)
            .expect("invalid key check");
        assert!(!invalid_key_dismiss, "must reject invalid reminder key");

        // Cleanup tempdir
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn delivered_reminders_remain_actionable_beyond_claim_lease_timeout() {
        let path = temporary_path("delivered-actionable-timeout");
        let candidate1 = test_candidate(ReminderComponentKind::Event, 1000);
        let candidate2 = test_candidate(ReminderComponentKind::Task, 1000);
        let mut store = ReminderStore::with_path(&path).unwrap();

        // 1. Claim reminders at t = 1000
        let deliveries = store
            .claim_due_reminders(&[candidate1.clone(), candidate2.clone()], 1000)
            .unwrap();
        assert_eq!(deliveries.len(), 2);
        let token1 = deliveries[0].claim_token.clone();
        let token2 = deliveries[1].claim_token.clone();

        // 2. Mark both delivered at t = 1000
        assert!(store.mark_delivered_if_claimed_at(&candidate1.key, &token1, 1000).unwrap());
        assert!(store.mark_delivered_if_claimed_at(&candidate2.key, &token2, 1000).unwrap());

        // 3. Past MAX_CLAIM_SECONDS (e.g. 10 minutes later = t 1600):
        // Delivered reminders must NOT be considered stale; user can dismiss and snooze
        let now_later = 1000 + 600;
        assert!(store.dismiss_if_current_at(&candidate1.key, &token1, now_later).unwrap());
        assert!(store.snooze_if_current_at(&candidate2.key, &token2, 3000, now_later).unwrap());

        // 4. Repeated actions are idempotent
        assert!(store.dismiss_if_current_at(&candidate1.key, &token1, now_later + 1).unwrap());
        assert!(store.snooze_if_current_at(&candidate2.key, &token2, 3000, now_later + 1).unwrap());

        // 5. Cross-action rejection: dismissed reminder cannot be snoozed, and snoozed reminder cannot be dismissed
        assert!(!store.snooze_if_current_at(&candidate1.key, &token1, 3500, now_later + 2).unwrap());
        assert!(!store.dismiss_if_current_at(&candidate2.key, &token2, now_later + 2).unwrap());

        // 6. Beyond MAX_LATE_SECONDS (e.g. 8 days later): delivery action is stale
        let candidate3 = test_candidate(ReminderComponentKind::Memo, 2000);
        let deliveries3 = store
            .claim_due_reminders(&[candidate3.clone()], 2000)
            .unwrap();
        let token3 = deliveries3[0].claim_token.clone();
        assert!(store.mark_delivered_if_claimed_at(&candidate3.key, &token3, 2000).unwrap());

        let ancient_time = 2000 + MAX_LATE_SECONDS + 1;
        assert!(!store.dismiss_if_current_at(&candidate3.key, &token3, ancient_time).unwrap());

        let _ = fs::remove_file(path);
    }
}


