// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable calendar/task reminders.
//
// The iCalendar component remains the source of truth for the alarm itself.
// This service only stores delivery/snooze state owned by this application so
// that a reminder is not shown again on every polling pass.  EDS-backed
// calendars can use the same API once the native Linux watcher is connected;
// the portable fallback currently consumes the local CalendarSnapshot.

use chrono::{Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::calendar_store_service::{CalendarEventRecord, CalendarSnapshot, TaskRecord};
use super::ical_time_service::{property_local_datetime, property_local_timestamp};

const CURRENT_VERSION: u32 = 1;
const DEFAULT_ALL_DAY_HOUR: u32 = 9;
const MAX_LATE_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_COMPONENT_SCAN_LINES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderKind {
    Event,
    Task,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderCandidate {
    pub key: String,
    pub title: String,
    pub due_at: i64,
    pub kind: ReminderKind,
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
    let Some(start) = property_local_datetime(&event.component, "DTSTART", event.all_day)
        .or_else(|| parse_display_naive(&event.start, event.all_day))
    else {
        return Vec::new();
    };
    let end = property_local_datetime(&event.component, "DTEND", event.all_day)
        .or_else(|| parse_display_naive(&event.end, event.all_day))
        .unwrap_or(start);
    let duration = end.signed_duration_since(start);
    let dates = recurrence_dates(&event.component, start.date(), now);
    let alarms = alarm_specs(&event.component);
    let mut candidates = Vec::new();
    for (occurrence_index, date) in dates.into_iter().enumerate() {
        let occurrence_start = date.and_time(start.time());
        let occurrence_end = occurrence_start
            .checked_add_signed(duration)
            .unwrap_or(occurrence_start);
        let Some(occurrence_start_timestamp) = local_timestamp(occurrence_start) else {
            continue;
        };
        let Some(occurrence_end_timestamp) = local_timestamp(occurrence_end) else {
            continue;
        };
        candidates.extend(alarm_candidates_from_specs(
            &alarms,
            occurrence_start_timestamp,
            occurrence_end_timestamp,
            ReminderKind::Event,
            &event.uid,
            &event.summary,
            &date.format("%Y%m%d").to_string(),
            occurrence_index == 0,
        ));
    }
    candidates
}

fn task_reminder_candidates(task: &TaskRecord) -> Vec<ReminderCandidate> {
    let Some(due) = property_local_datetime(&task.component, "DUE", task.due.len() == 10)
        .and_then(local_timestamp)
        .or_else(|| parse_display_datetime(&task.due, task.due.len() == 10))
    else {
        return Vec::new();
    };
    alarm_candidates_from_specs(
        &alarm_specs(&task.component),
        due,
        due,
        ReminderKind::Task,
        &task.uid,
        &task.summary,
        "single",
        true,
    )
}

fn alarm_candidates_from_specs(
    alarms: &[AlarmSpec],
    start: i64,
    end: i64,
    kind: ReminderKind,
    uid: &str,
    title: &str,
    occurrence_id: &str,
    include_absolute: bool,
) -> Vec<ReminderCandidate> {
    alarms
        .iter()
        .into_iter()
        .enumerate()
        .filter_map(|(index, alarm)| {
            if !include_absolute && parse_ical_duration(&alarm.trigger).is_none() {
                return None;
            }
            let due_at = alarm_due_at(&alarm, start, end)?;
            Some(ReminderCandidate {
                key: format!(
                    "{}:{}:{}:{}:{}",
                    match kind {
                        ReminderKind::Event => "event",
                        ReminderKind::Task => "task",
                    },
                    uid,
                    occurrence_id,
                    index,
                    due_at
                ),
                title: if title.trim().is_empty() {
                    "Erinnerung".to_string()
                } else {
                    title.to_string()
                },
                due_at,
                kind,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AlarmSpec {
    trigger: String,
    trigger_property: String,
    related_end: bool,
    action: String,
}

fn alarm_specs(component: &str) -> Vec<AlarmSpec> {
    let mut alarms = Vec::new();
    let mut current: Option<AlarmSpec> = None;
    for line in unfold_lines(component)
        .into_iter()
        .take(MAX_COMPONENT_SCAN_LINES)
    {
        if line.eq_ignore_ascii_case("BEGIN:VALARM") {
            current = Some(AlarmSpec {
                trigger: String::new(),
                trigger_property: String::new(),
                related_end: false,
                action: String::new(),
            });
            continue;
        }
        if line.eq_ignore_ascii_case("END:VALARM") {
            if let Some(alarm) = current.take() {
                if !alarm.trigger.trim().is_empty()
                    && (alarm.action.is_empty() || alarm.action.eq_ignore_ascii_case("DISPLAY"))
                {
                    alarms.push(alarm);
                }
            }
            continue;
        }
        let Some(alarm) = current.as_mut() else {
            continue;
        };
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        match property_name.as_str() {
            "ACTION" => alarm.action = value.trim().to_string(),
            "TRIGGER" => {
                alarm.trigger = value.trim().to_string();
                alarm.trigger_property = property.to_string();
                alarm.related_end = property
                    .split(';')
                    .skip(1)
                    .any(|parameter| parameter.eq_ignore_ascii_case("RELATED=END"));
            }
            _ => {}
        }
    }
    alarms
}

fn alarm_due_at(alarm: &AlarmSpec, start: i64, end: i64) -> Option<i64> {
    if let Some(duration) = parse_ical_duration(&alarm.trigger) {
        return (if alarm.related_end { end } else { start }).checked_add(duration);
    }
    property_local_timestamp(
        &alarm.trigger_property,
        &alarm.trigger,
        DEFAULT_ALL_DAY_HOUR,
    )
}

fn parse_ical_duration(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.len() < 2 {
        return None;
    }
    let (sign, value) = match raw.as_bytes()[0] {
        b'-' => (-1_i64, &raw[1..]),
        b'+' => (1_i64, &raw[1..]),
        _ => (1_i64, raw),
    };
    if !value.starts_with('P') {
        return None;
    }
    let mut number = String::new();
    let mut in_time = false;
    let mut seconds = 0_i64;
    let mut found = false;
    for character in value[1..].chars() {
        if character == 'T' {
            if in_time || !number.is_empty() {
                return None;
            }
            in_time = true;
            continue;
        }
        if character.is_ascii_digit() {
            number.push(character);
            continue;
        }
        if number.is_empty() {
            return None;
        }
        let amount = number.parse::<i64>().ok()?;
        number.clear();
        let unit_seconds = match (in_time, character) {
            (false, 'W') => 7 * 24 * 60 * 60,
            (false, 'D') => 24 * 60 * 60,
            (true, 'H') => 60 * 60,
            (true, 'M') => 60,
            (true, 'S') => 1,
            _ => return None,
        };
        seconds = seconds.checked_add(amount.checked_mul(unit_seconds)?)?;
        found = true;
    }
    if !number.is_empty() || !found {
        return None;
    }
    seconds.checked_mul(sign)
}

#[derive(Debug, Default)]
struct RecurrenceRule {
    frequency: Option<String>,
    interval: i64,
    count: Option<usize>,
    until: Option<NaiveDate>,
    by_days: Vec<Weekday>,
    by_month_days: Vec<i32>,
}

fn recurrence_dates(component: &str, initial: NaiveDate, now: i64) -> Vec<NaiveDate> {
    let horizon = Local
        .timestamp_opt(now, 0)
        .single()
        .map(|value| value.date_naive() + Duration::days(370))
        .unwrap_or_else(|| initial + Duration::days(370));

    let mut dates = Vec::new();
    if let Some(raw_rule) = ical_property(component, "RRULE") {
        let rule = parse_recurrence_rule(&raw_rule);
        if let Some(frequency) = rule.frequency.as_deref() {
            if initial <= horizon {
                let mut cursor = initial;
                let mut occurrence_count = 0usize;
                while cursor <= horizon && dates.len() < 4_096 {
                    if recurrence_matches(&rule, frequency, initial, cursor) {
                        occurrence_count += 1;
                        if rule.count.is_some_and(|count| occurrence_count > count) {
                            break;
                        }
                        if !has_exdate(component, cursor) {
                            dates.push(cursor);
                        }
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
                if date <= horizon && !has_exdate(component, date) {
                    dates.push(date);
                }
            }
        }
    }
    dates.sort_unstable();
    dates.dedup();
    dates
}

fn parse_recurrence_rule(raw: &str) -> RecurrenceRule {
    let mut rule = RecurrenceRule {
        interval: 1,
        ..RecurrenceRule::default()
    };
    for part in raw.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => rule.frequency = Some(value.trim().to_ascii_uppercase()),
            "INTERVAL" => {
                rule.interval = value
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or(1);
            }
            "COUNT" => rule.count = value.trim().parse::<usize>().ok(),
            "UNTIL" => rule.until = parse_ical_date(value),
            "BYDAY" => rule
                .by_days
                .extend(value.split(',').filter_map(parse_weekday)),
            "BYMONTHDAY" => rule.by_month_days.extend(
                value
                    .split(',')
                    .filter_map(|value| value.trim().parse::<i32>().ok()),
            ),
            _ => {}
        }
    }
    rule
}

fn recurrence_matches(
    rule: &RecurrenceRule,
    frequency: &str,
    initial: NaiveDate,
    date: NaiveDate,
) -> bool {
    if date < initial || rule.until.is_some_and(|until| date > until) {
        return false;
    }
    match frequency {
        "DAILY" => (date - initial).num_days() % rule.interval == 0,
        "WEEKLY" => {
            let days = (date - initial).num_days();
            let week = days.div_euclid(7);
            week % rule.interval == 0
                && if rule.by_days.is_empty() {
                    date.weekday() == initial.weekday()
                } else {
                    rule.by_days.contains(&date.weekday())
                }
        }
        "MONTHLY" => {
            let months = (date.year() - initial.year()) as i64 * 12
                + i64::from(date.month() as i32 - initial.month() as i32);
            months >= 0
                && months % rule.interval == 0
                && if rule.by_month_days.is_empty() {
                    date.day() == initial.day()
                } else {
                    rule.by_month_days
                        .iter()
                        .any(|day| *day > 0 && date.day() == *day as u32)
                }
        }
        "YEARLY" => {
            let years = i64::from(date.year() - initial.year());
            years >= 0
                && years % rule.interval == 0
                && date.month() == initial.month()
                && date.day() == initial.day()
        }
        _ => false,
    }
}

fn parse_ical_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d").ok()
}

fn parse_weekday(value: &str) -> Option<Weekday> {
    match value.trim().get(value.trim().len().saturating_sub(2)..)? {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
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
        LocalResult::None => None,
    }
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.trim_start());
            }
        } else {
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
        assert_eq!(
            candidates[0].due_at,
            local_timestamp(
                NaiveDate::from_ymd_opt(2026, 8, 11)
                    .unwrap()
                    .and_hms_opt(9, 45, 0)
                    .unwrap()
            )
            .unwrap()
        );
        assert_eq!(candidates[1].due_at, 1_786_449_600);
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
