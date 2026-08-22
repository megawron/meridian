// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable calendar/task reminders.
//
// The iCalendar component remains the source of truth for the alarm itself.
// This service only stores delivery/snooze state owned by this application so
// that a reminder is not shown again on every polling pass.  EDS-backed
// calendars can use the same API once the native Linux watcher is connected;
// the portable fallback currently consumes the local CalendarSnapshot.

use chrono::{Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use super::calendar_store_service::{CalendarEventRecord, CalendarSnapshot, TaskRecord};
use super::ical_time_service::{property_local_datetime, property_local_timestamp};
use super::recurrence_service::{
    parse_ical_date as parse_shared_ical_date, parse_recurrence_rule, recurrence_occurs_on,
};

const CURRENT_VERSION: u32 = 1;
const DEFAULT_ALL_DAY_HOUR: u32 = 9;
const MAX_LATE_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_COMPONENT_SCAN_LINES: usize = 20_000;
const MAX_ALARM_REPETITIONS: u32 = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderKind {
    Event,
    Task,
}

/// The component kind is kept separately from `ReminderKind` for backwards
/// compatibility with the application notification action.  Memos use the
/// same safe desktop notification path until that action can accept a third
/// visible body label, while their deep link remains unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderComponentKind {
    Event,
    Task,
    Memo,
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
            Self::Display => "Anzeige".to_string(),
            Self::Audio => "Ton".to_string(),
            Self::Email => "E-Mail".to_string(),
            Self::Procedure => "Prozedur".to_string(),
            Self::Unknown(value) if value.is_empty() => "Unbekannte Aktion".to_string(),
            Self::Unknown(value) => format!("Unbekannte Aktion ({value})"),
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
                let direction = if *offset_seconds < 0 { "vor" } else { "nach" };
                let anchor = match relation {
                    AlarmRelation::Start => "Beginn",
                    AlarmRelation::End => "Ende",
                };
                format!(
                    "{} {} {}",
                    format_duration_for_display(seconds),
                    direction,
                    anchor
                )
            }
            Self::Absolute { value, .. } => format!("absolut {value}"),
            Self::Unknown { .. } => "unbekannter Trigger".to_string(),
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
        format!("{} Tage", seconds / (24 * 60 * 60))
    } else if seconds % (60 * 60) == 0 {
        format!("{} Stunden", seconds / (60 * 60))
    } else if seconds % 60 == 0 {
        format!("{} Minuten", seconds / 60)
    } else {
        format!("{seconds} Sekunden")
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

/// Stable `memo:///` deep-link contract shared by reminder tests and future
/// notification activation code.  Only URI-unreserved characters stay raw.
pub fn reminder_deep_link(
    kind: ReminderComponentKind,
    source_uid: &str,
    component_uid: &str,
) -> String {
    let scheme = match kind {
        ReminderComponentKind::Event => "calendar",
        ReminderComponentKind::Task => "task",
        ReminderComponentKind::Memo => "memo",
    };
    format!(
        "{scheme}:///?source-uid={}&comp-uid={}",
        percent_encode(source_uid),
        percent_encode(component_uid)
    )
}

pub fn memo_deep_link(source_uid: &str, component_uid: &str) -> String {
    reminder_deep_link(ReminderComponentKind::Memo, source_uid, component_uid)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderCandidate {
    pub key: String,
    pub title: String,
    pub due_at: i64,
    pub kind: ReminderKind,
}

pub trait ReminderClock: Clone {
    fn now(&self) -> i64;
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
}

impl Default for ReminderState {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            snoozed_until: BTreeMap::new(),
            delivered: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReminderStore {
    path: PathBuf,
    state: ReminderState,
}

impl ReminderStore {
    pub fn new() -> io::Result<Self> {
        let path = std::env::var_os("EVOLUTION_GTK4_REMINDER_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| super::app_data_dir().join("reminders.json"));
        Self::with_path(path)
    }

    pub fn with_path(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let state = match fs::read(&path) {
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
                ReminderState {
                    version: CURRENT_VERSION,
                    ..state
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => ReminderState::default(),
            Err(error) => return Err(error),
        };
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

        let mut due = candidates
            .iter()
            .filter(|candidate| {
                candidate.due_at <= now
                    && candidate.due_at >= now - MAX_LATE_SECONDS
                    && !self.state.delivered.contains(&candidate.key)
                    && self
                        .state
                        .snoozed_until
                        .get(&candidate.key)
                        .map_or(true, |until| *until <= now)
            })
            .cloned()
            .collect::<Vec<_>>();
        due.sort_by(|left, right| {
            left.due_at
                .cmp(&right.due_at)
                .then_with(|| left.key.cmp(&right.key))
        });
        due
    }

    pub fn mark_delivered(&mut self, key: &str) -> io::Result<()> {
        self.state.snoozed_until.remove(key);
        self.state.delivered.insert(key.to_string());
        self.save()
    }

    pub fn snooze_until(&mut self, key: &str, until: i64) -> io::Result<()> {
        self.state.delivered.remove(key);
        self.state.snoozed_until.insert(key.to_string(), until);
        self.save()
    }

    pub fn dismiss(&mut self, key: &str) -> io::Result<()> {
        self.mark_delivered(key)
    }

    /// Make a previously delivered alarm eligible again.  This is the
    /// explicit restart operation used by deterministic scheduler tests and
    /// is persisted so a process restart cannot silently change the state.
    pub fn restart(&mut self, key: &str) -> io::Result<()> {
        self.state.snoozed_until.remove(key);
        self.state.delivered.remove(key);
        self.save()
    }

    #[allow(dead_code)]
    pub fn is_delivered(&self, key: &str) -> bool {
        self.state.delivered.contains(key)
    }

    fn save(&self) -> io::Result<()> {
        let raw = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, raw)?;
        fs::rename(temporary, &self.path)
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
    alarms
        .iter()
        .into_iter()
        .enumerate()
        .flat_map(|(index, alarm)| {
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
                    let kind = match component_kind {
                        ReminderComponentKind::Task => ReminderKind::Task,
                        ReminderComponentKind::Event | ReminderComponentKind::Memo => {
                            ReminderKind::Event
                        }
                    };
                    Some(ReminderCandidate {
                        key: format!(
                            "{}:{}:{}:{}:{}:{}:{}",
                            match component_kind {
                                ReminderComponentKind::Event => "event",
                                ReminderComponentKind::Task => "task",
                                ReminderComponentKind::Memo => "memo",
                            },
                            source_uid,
                            uid,
                            occurrence_id,
                            index,
                            repeat_index,
                            due_at
                        ),
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
            key: "event:event-1:0:100".to_string(),
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
            "evolution-gtk4-{label}-{}-{unique}.json",
            std::process::id()
        ))
    }
}
