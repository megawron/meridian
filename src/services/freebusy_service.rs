// SPDX-License-Identifier: GPL-3.0-or-later
//
// FreeBusyService — RFC 5545 VFREEBUSY generation, parsing, publishing and meeting scheduling.

use chrono::{Duration, NaiveDate, NaiveDateTime, Timelike};
use std::cmp::Ordering;

use super::calendar_store_service::CalendarEventRecord;

/// Classification of Free/Busy period types according to RFC 5545 section 3.8.2.6 (FBTYPE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreeBusyType {
    /// Busy period (default when FBTYPE is omitted).
    Busy,
    /// Busy period marked as tentative.
    BusyTentative,
    /// Busy period marked as unavailable / out of office.
    BusyUnavailable,
    /// Explicitly free period.
    Free,
}

impl FreeBusyType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Busy => "BUSY",
            Self::BusyTentative => "BUSY-TENTATIVE",
            Self::BusyUnavailable => "BUSY-UNAVAILABLE",
            Self::Free => "FREE",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "BUSY-TENTATIVE" | "TENTATIVE" => Self::BusyTentative,
            "BUSY-UNAVAILABLE" | "UNAVAILABLE" | "OOF" => Self::BusyUnavailable,
            "FREE" => Self::Free,
            _ => Self::Busy,
        }
    }
}

/// A specific time interval with an associated Free/Busy status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeBusyInterval {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub fb_type: FreeBusyType,
    pub summary: Option<String>,
}

impl FreeBusyInterval {
    pub fn new(start: NaiveDateTime, end: NaiveDateTime, fb_type: FreeBusyType) -> Self {
        Self {
            start,
            end,
            fb_type,
            summary: None,
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn duration_minutes(&self) -> i64 {
        self.end.signed_duration_since(self.start).num_minutes()
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Structured RFC 5545 VFREEBUSY component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VFreeBusyDocument {
    pub uid: String,
    pub dtstamp: Option<NaiveDateTime>,
    pub dtstart: NaiveDateTime,
    pub dtend: NaiveDateTime,
    pub organizer: Option<String>,
    pub attendee: Option<String>,
    pub url: Option<String>,
    pub comment: Option<String>,
    pub periods: Vec<FreeBusyInterval>,
}

impl VFreeBusyDocument {
    pub fn new(uid: impl Into<String>, dtstart: NaiveDateTime, dtend: NaiveDateTime) -> Self {
        Self {
            uid: uid.into(),
            dtstamp: None,
            dtstart,
            dtend,
            organizer: None,
            attendee: None,
            url: None,
            comment: None,
            periods: Vec::new(),
        }
    }

    /// Serialize this component into standard RFC 5545 VCALENDAR / VFREEBUSY text.
    pub fn to_ical(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("BEGIN:VCALENDAR\r\n");
        out.push_str("VERSION:2.0\r\n");
        out.push_str("PRODID:-//Meridian//FreeBusy Service//EN\r\n");
        out.push_str("METHOD:PUBLISH\r\n");
        out.push_str("BEGIN:VFREEBUSY\r\n");
        out.push_str(&format!("UID:{}\r\n", self.uid));

        let dtstamp = self.dtstamp.unwrap_or_else(|| self.dtstart);
        out.push_str(&format!(
            "DTSTAMP:{}\r\n",
            format_ical_datetime(dtstamp)
        ));
        out.push_str(&format!(
            "DTSTART:{}\r\n",
            format_ical_datetime(self.dtstart)
        ));
        out.push_str(&format!(
            "DTEND:{}\r\n",
            format_ical_datetime(self.dtend)
        ));

        if let Some(org) = &self.organizer {
            if org.starts_with("ORGANIZER") {
                out.push_str(&format!("{}\r\n", org));
            } else if org.contains(':') {
                out.push_str(&format!("ORGANIZER;{}\r\n", org));
            } else {
                out.push_str(&format!("ORGANIZER:mailto:{}\r\n", org));
            }
        }

        if let Some(att) = &self.attendee {
            if att.starts_with("ATTENDEE") {
                out.push_str(&format!("{}\r\n", att));
            } else if att.contains(':') {
                out.push_str(&format!("ATTENDEE;{}\r\n", att));
            } else {
                out.push_str(&format!("ATTENDEE:mailto:{}\r\n", att));
            }
        }

        if let Some(url) = &self.url {
            out.push_str(&format!("URL:{}\r\n", url));
        }

        if let Some(comment) = &self.comment {
            out.push_str(&format!("COMMENT:{}\r\n", comment));
        }

        // Group intervals by type for clean RFC output
        for period in &self.periods {
            if period.end <= period.start {
                continue;
            }
            let fb_param = if period.fb_type == FreeBusyType::Busy {
                String::new()
            } else {
                format!(";FBTYPE={}", period.fb_type.as_str())
            };
            out.push_str(&format!(
                "FREEBUSY{}:{}/{}\r\n",
                fb_param,
                format_ical_datetime(period.start),
                format_ical_datetime(period.end)
            ));
        }

        out.push_str("END:VFREEBUSY\r\n");
        out.push_str("END:VCALENDAR\r\n");
        out
    }

    /// Parse an RFC 5545 VFREEBUSY component from a raw iCalendar stream.
    pub fn parse_ical(raw: &str) -> Result<Self, String> {
        let unfolded = unfold_ical_lines(raw);
        let mut in_vfreebusy = false;
        let mut uid = String::new();
        let mut dtstamp = None;
        let mut dtstart = None;
        let mut dtend = None;
        let mut organizer = None;
        let mut attendee = None;
        let mut url = None;
        let mut comment = None;
        let mut periods = Vec::new();

        for line in unfolded.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.eq_ignore_ascii_case("BEGIN:VFREEBUSY") {
                in_vfreebusy = true;
                continue;
            }
            if line.eq_ignore_ascii_case("END:VFREEBUSY") {
                in_vfreebusy = false;
                continue;
            }
            if !in_vfreebusy {
                continue;
            }

            let (key_part, val_part) = match line.split_once(':') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };

            let key_upper = key_part.to_ascii_uppercase();
            if key_upper == "UID" {
                uid = val_part.to_string();
            } else if key_upper.starts_with("DTSTAMP") {
                dtstamp = parse_ical_datetime(val_part);
            } else if key_upper.starts_with("DTSTART") {
                dtstart = parse_ical_datetime(val_part);
            } else if key_upper.starts_with("DTEND") {
                dtend = parse_ical_datetime(val_part);
            } else if key_upper.starts_with("ORGANIZER") {
                organizer = Some(val_part.to_string());
            } else if key_upper.starts_with("ATTENDEE") {
                attendee = Some(val_part.to_string());
            } else if key_upper == "URL" {
                url = Some(val_part.to_string());
            } else if key_upper == "COMMENT" {
                comment = Some(val_part.to_string());
            } else if key_upper.starts_with("FREEBUSY") {
                let fb_type = extract_fbtype(key_part);
                // Val part can contain comma-separated periods
                for period_str in val_part.split(',') {
                    if let Some(period) = parse_freebusy_period(period_str.trim(), fb_type) {
                        periods.push(period);
                    }
                }
            }
        }

        let start = dtstart.ok_or_else(|| "Missing DTSTART in VFREEBUSY".to_string())?;
        let end = dtend.unwrap_or_else(|| start + Duration::days(1));
        if uid.is_empty() {
            uid = uuid::Uuid::new_v4().to_string();
        }

        Ok(Self {
            uid,
            dtstamp,
            dtstart: start,
            dtend: end,
            organizer,
            attendee,
            url,
            comment,
            periods: merge_freebusy_intervals(&periods),
        })
    }
}

/// Extract FBTYPE parameter from a property key like `FREEBUSY;FBTYPE=BUSY-TENTATIVE`.
fn extract_fbtype(key: &str) -> FreeBusyType {
    for part in key.split(';') {
        let part = part.trim();
        if let Some((param, val)) = part.split_once('=') {
            if param.trim().eq_ignore_ascii_case("FBTYPE") {
                return FreeBusyType::from_str(val.trim());
            }
        }
    }
    FreeBusyType::Busy
}

/// Parse a single period string like `19980314T233000Z/19980315T003000Z` or `19980314T233000Z/PT1H`.
fn parse_freebusy_period(period_str: &str, fb_type: FreeBusyType) -> Option<FreeBusyInterval> {
    let (start_str, end_or_dur_str) = period_str.split_once('/')?;
    let start = parse_ical_datetime(start_str.trim())?;

    let end = if end_or_dur_str.trim().starts_with('P') || end_or_dur_str.trim().starts_with("-P") {
        let duration = parse_iso_duration(end_or_dur_str.trim())?;
        start.checked_add_signed(duration)?
    } else {
        parse_ical_datetime(end_or_dur_str.trim())?
    };

    if end <= start {
        return None;
    }

    Some(FreeBusyInterval::new(start, end, fb_type))
}

/// Parse simple ISO 8601 / RFC 5545 duration strings like `PT1H`, `PT30M`, `P1D`.
fn parse_iso_duration(s: &str) -> Option<Duration> {
    let mut total_seconds = 0i64;
    let s = s.trim();
    let is_neg = s.starts_with('-');
    let s = s.trim_start_matches('-').trim_start_matches('+');

    if !s.starts_with('P') {
        return None;
    }
    let body = &s[1..];
    let (date_part, time_part) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };

    // Parse date part (e.g. 1W, 2D)
    let mut num_buf = String::new();
    for ch in date_part.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            let n: i64 = num_buf.parse().ok()?;
            num_buf.clear();
            match ch {
                'W' => total_seconds += n * 7 * 86400,
                'D' => total_seconds += n * 86400,
                _ => {}
            }
        }
    }

    // Parse time part (e.g. 1H, 30M, 45S)
    if let Some(t_str) = time_part {
        let mut num_buf = String::new();
        for ch in t_str.chars() {
            if ch.is_ascii_digit() {
                num_buf.push(ch);
            } else {
                let n: i64 = num_buf.parse().ok()?;
                num_buf.clear();
                match ch {
                    'H' => total_seconds += n * 3600,
                    'M' => total_seconds += n * 60,
                    'S' => total_seconds += n,
                    _ => {}
                }
            }
        }
    }

    let dur = Duration::seconds(total_seconds);
    if is_neg { Some(-dur) } else { Some(dur) }
}

/// Generate a VFreeBusyDocument from a list of CalendarEventRecords within the specified window.
pub fn generate_freebusy_for_events(
    events: &[CalendarEventRecord],
    range_start: NaiveDateTime,
    range_end: NaiveDateTime,
    organizer: Option<&str>,
    attendee: Option<&str>,
) -> VFreeBusyDocument {
    let mut doc = VFreeBusyDocument::new(uuid::Uuid::new_v4().to_string(), range_start, range_end);
    doc.organizer = organizer.map(str::to_string);
    doc.attendee = attendee.map(str::to_string);

    let mut intervals = Vec::new();

    for event in events {
        // Skip cancelled events
        if event.component.contains("STATUS:CANCELLED") {
            continue;
        }

        // Check transparency (TRANSP:TRANSPARENT means free/available)
        if event.component.contains("TRANSP:TRANSPARENT") {
            continue;
        }

        let fb_type = if event.component.contains("STATUS:TENTATIVE") {
            FreeBusyType::BusyTentative
        } else if event.component.contains("X-MICROSOFT-CDO-BUSYSTATUS:OOF")
            || event.component.contains("FBTYPE=BUSY-UNAVAILABLE")
        {
            FreeBusyType::BusyUnavailable
        } else {
            FreeBusyType::Busy
        };

        let (ev_start, ev_end) = if event.all_day {
            let start_date = parse_date_str(&event.start).unwrap_or_else(|| range_start.date());
            let end_date = parse_date_str(&event.end).unwrap_or(start_date);
            let s = start_date.and_hms_opt(0, 0, 0).unwrap_or(range_start);
            let e = if end_date > start_date {
                end_date.and_hms_opt(0, 0, 0).unwrap_or(range_end)
            } else {
                s + Duration::days(1)
            };
            (s, e)
        } else {
            let s = parse_datetime_str(&event.start).unwrap_or(range_start);
            let e = parse_datetime_str(&event.end).unwrap_or_else(|| s + Duration::hours(1));
            (s, e)
        };

        // Clip to requested range
        if ev_end <= range_start || ev_start >= range_end || ev_end <= ev_start {
            continue;
        }

        let clipped_start = ev_start.max(range_start);
        let clipped_end = ev_end.min(range_end);

        intervals.push(
            FreeBusyInterval::new(clipped_start, clipped_end, fb_type)
                .with_summary(&event.summary),
        );
    }

    doc.periods = merge_freebusy_intervals(&intervals);
    doc
}

/// Merge adjacent and overlapping intervals with the same FBTYPE, and sort chronologically.
pub fn merge_freebusy_intervals(intervals: &[FreeBusyInterval]) -> Vec<FreeBusyInterval> {
    if intervals.is_empty() {
        return Vec::new();
    }

    let mut sorted = intervals.to_vec();
    sorted.sort_by(|a, b| match a.start.cmp(&b.start) {
        Ordering::Equal => a.end.cmp(&b.end),
        other => other,
    });

    let mut merged: Vec<FreeBusyInterval> = Vec::with_capacity(sorted.len());
    for interval in sorted {
        if interval.end <= interval.start {
            continue;
        }
        if let Some(prev) = merged.last_mut() {
            if prev.fb_type == interval.fb_type && interval.start <= prev.end {
                prev.end = prev.end.max(interval.end);
                continue;
            }
        }
        merged.push(interval);
    }

    merged
}

/// Result of meeting slot scheduling analysis across multiple attendees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingFreeSlot {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_attendees_available: bool,
    pub conflicting_attendees: Vec<String>,
}

impl MeetingFreeSlot {
    pub fn duration_minutes(&self) -> i64 {
        self.end.signed_duration_since(self.start).num_minutes()
    }
}

/// Finds common free slots within normal working hours across multiple attendees.
pub fn find_common_free_slots(
    attendees_schedules: &[(&str, &[FreeBusyInterval])],
    range_start: NaiveDateTime,
    range_end: NaiveDateTime,
    workday_start_minutes: u32,
    workday_end_minutes: u32,
    duration_minutes: u32,
) -> Vec<MeetingFreeSlot> {
    if range_end <= range_start || duration_minutes == 0 {
        return Vec::new();
    }

    let start_date = range_start.date();
    let end_date = range_end.date();
    let min_dur = i64::from(duration_minutes);

    let mut result = Vec::new();
    let mut current_date = start_date;

    while current_date <= end_date {
        let day_work_start = current_date
            .and_hms_opt(workday_start_minutes / 60, workday_start_minutes % 60, 0)
            .unwrap_or(range_start);
        let day_work_end = current_date
            .and_hms_opt(workday_end_minutes / 60, workday_end_minutes % 60, 0)
            .unwrap_or(range_end);

        let window_start = day_work_start.max(range_start);
        let window_end = day_work_end.min(range_end);

        if window_end > window_start
            && window_end.signed_duration_since(window_start).num_minutes() >= min_dur
        {
            // Collect all busy periods for this day across all attendees
            let mut day_busy_by_attendee: Vec<(&str, Vec<FreeBusyInterval>)> = Vec::new();
            let mut all_busy_intervals = Vec::new();

            for &(att_name, periods) in attendees_schedules {
                let att_busy: Vec<_> = periods
                    .iter()
                    .filter(|p| p.fb_type != FreeBusyType::Free)
                    .filter_map(|p| {
                        if p.end <= window_start || p.start >= window_end {
                            None
                        } else {
                            Some(FreeBusyInterval::new(
                                p.start.max(window_start),
                                p.end.min(window_end),
                                p.fb_type,
                            ))
                        }
                    })
                    .collect();

                all_busy_intervals.extend(att_busy.clone());
                day_busy_by_attendee.push((att_name, att_busy));
            }

            // Calculate universal free slots (where NO ONE is busy)
            let merged_all_busy = merge_freebusy_intervals(&all_busy_intervals);
            let mut cursor = window_start;

            for busy in merged_all_busy {
                if busy.start > cursor
                    && busy.start.signed_duration_since(cursor).num_minutes() >= min_dur
                {
                    result.push(MeetingFreeSlot {
                        start: cursor,
                        end: busy.start,
                        all_attendees_available: true,
                        conflicting_attendees: Vec::new(),
                    });
                }
                cursor = cursor.max(busy.end);
            }

            if window_end > cursor
                && window_end.signed_duration_since(cursor).num_minutes() >= min_dur
            {
                result.push(MeetingFreeSlot {
                    start: cursor,
                    end: window_end,
                    all_attendees_available: true,
                    conflicting_attendees: Vec::new(),
                });
            }
        }

        current_date = match current_date.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }

    result
}

// Helpers

fn format_ical_datetime(dt: NaiveDateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

fn parse_ical_datetime(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().trim_end_matches('Z');
    if s.len() >= 15 {
        NaiveDateTime::parse_from_str(&s[..15], "%Y%m%dT%H%M%S").ok()
    } else if s.len() >= 8 {
        let date = NaiveDate::parse_from_str(&s[..8], "%Y%m%d").ok()?;
        date.and_hms_opt(0, 0, 0)
    } else {
        None
    }
}

fn parse_date_str(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.len() >= 10 {
        NaiveDate::parse_from_str(&s[..10], "%Y-%m-%d").ok()
    } else if s.len() >= 8 {
        NaiveDate::parse_from_str(&s[..8], "%Y%m%d").ok()
    } else {
        None
    }
}

fn parse_datetime_str(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().trim_end_matches('Z');
    if s.len() >= 16 {
        NaiveDateTime::parse_from_str(&s[..16], "%Y-%m-%d %H:%M").ok()
    } else if s.len() >= 15 {
        NaiveDateTime::parse_from_str(&s[..15], "%Y%m%dT%H%M%S").ok()
    } else {
        parse_date_str(s)?.and_hms_opt(0, 0, 0)
    }
}

fn unfold_ical_lines(raw: &str) -> String {
    let mut unfolded = String::with_capacity(raw.len());
    for line in raw.lines() {
        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            unfolded.push_str(&line[1..]);
        } else {
            if !unfolded.is_empty() {
                unfolded.push('\n');
            }
            unfolded.push_str(line);
        }
    }
    unfolded
}

trait DateExt {
    fn year(&self) -> i32;
    fn month(&self) -> u32;
    fn day(&self) -> u32;
}

impl DateExt for NaiveDateTime {
    fn year(&self) -> i32 {
        chrono::Datelike::year(&self.date())
    }
    fn month(&self) -> u32 {
        chrono::Datelike::month(&self.date())
    }
    fn day(&self) -> u32 {
        chrono::Datelike::day(&self.date())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    #[test]
    fn parses_and_serializes_vfreebusy_document() {
        let ical_sample = r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Test//EN
METHOD:PUBLISH
BEGIN:VFREEBUSY
UID:fb-test-123
DTSTAMP:20260811T090000Z
DTSTART:20260811T000000Z
DTEND:20260812T000000Z
ORGANIZER:mailto:organizer@example.org
ATTENDEE:mailto:user@example.org
FREEBUSY:20260811T090000Z/20260811T103000Z
FREEBUSY;FBTYPE=BUSY-TENTATIVE:20260811T130000Z/20260811T140000Z
FREEBUSY;FBTYPE=BUSY-UNAVAILABLE:20260811T160000Z/PT1H
END:VFREEBUSY
END:VCALENDAR"#;

        let parsed = VFreeBusyDocument::parse_ical(ical_sample).expect("valid VFREEBUSY");
        assert_eq!(parsed.uid, "fb-test-123");
        assert_eq!(parsed.dtstart, dt(2026, 8, 11, 0, 0));
        assert_eq!(parsed.dtend, dt(2026, 8, 12, 0, 0));
        assert_eq!(parsed.periods.len(), 3);
        assert_eq!(parsed.periods[0].fb_type, FreeBusyType::Busy);
        assert_eq!(parsed.periods[0].start, dt(2026, 8, 11, 9, 0));
        assert_eq!(parsed.periods[0].end, dt(2026, 8, 11, 10, 30));

        assert_eq!(parsed.periods[1].fb_type, FreeBusyType::BusyTentative);
        assert_eq!(parsed.periods[2].fb_type, FreeBusyType::BusyUnavailable);
        assert_eq!(parsed.periods[2].end, dt(2026, 8, 11, 17, 0));

        let reserialized = parsed.to_ical();
        assert!(reserialized.contains("BEGIN:VFREEBUSY"));
        assert!(reserialized.contains("UID:fb-test-123"));
        assert!(reserialized.contains("FREEBUSY:20260811T090000Z/20260811T103000Z"));
        assert!(reserialized.contains("FREEBUSY;FBTYPE=BUSY-TENTATIVE:20260811T130000Z/20260811T140000Z"));
        assert!(reserialized.contains("FREEBUSY;FBTYPE=BUSY-UNAVAILABLE:20260811T160000Z/20260811T170000Z"));
    }

    #[test]
    fn generates_freebusy_from_calendar_event_records() {
        let events = vec![
            CalendarEventRecord {
                uid: "ev-1".to_string(),
                source_uid: "local".to_string(),
                source_display_name: "Local".to_string(),
                component: "BEGIN:VEVENT\r\nSTATUS:CONFIRMED\r\nEND:VEVENT".to_string(),
                summary: "Standard Busy".to_string(),
                start: "2026-08-11 09:00".to_string(),
                end: "2026-08-11 10:00".to_string(),
                location: String::new(),
                description: String::new(),
                all_day: false,
            },
            CalendarEventRecord {
                uid: "ev-2".to_string(),
                source_uid: "local".to_string(),
                source_display_name: "Local".to_string(),
                component: "BEGIN:VEVENT\r\nSTATUS:TENTATIVE\r\nEND:VEVENT".to_string(),
                summary: "Tentative Meeting".to_string(),
                start: "2026-08-11 11:00".to_string(),
                end: "2026-08-11 12:00".to_string(),
                location: String::new(),
                description: String::new(),
                all_day: false,
            },
            CalendarEventRecord {
                uid: "ev-3".to_string(),
                source_uid: "local".to_string(),
                source_display_name: "Local".to_string(),
                component: "BEGIN:VEVENT\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT".to_string(),
                summary: "Transparent Event".to_string(),
                start: "2026-08-11 14:00".to_string(),
                end: "2026-08-11 15:00".to_string(),
                location: String::new(),
                description: String::new(),
                all_day: false,
            },
            CalendarEventRecord {
                uid: "ev-4".to_string(),
                source_uid: "local".to_string(),
                source_display_name: "Local".to_string(),
                component: "BEGIN:VEVENT\r\nSTATUS:CANCELLED\r\nEND:VEVENT".to_string(),
                summary: "Cancelled Event".to_string(),
                start: "2026-08-11 15:00".to_string(),
                end: "2026-08-11 16:00".to_string(),
                location: String::new(),
                description: String::new(),
                all_day: false,
            },
        ];

        let doc = generate_freebusy_for_events(
            &events,
            dt(2026, 8, 11, 0, 0),
            dt(2026, 8, 12, 0, 0),
            Some("boss@example.org"),
            Some("me@example.org"),
        );

        assert_eq!(doc.periods.len(), 2);
        assert_eq!(doc.periods[0].fb_type, FreeBusyType::Busy);
        assert_eq!(doc.periods[0].start, dt(2026, 8, 11, 9, 0));
        assert_eq!(doc.periods[0].end, dt(2026, 8, 11, 10, 0));

        assert_eq!(doc.periods[1].fb_type, FreeBusyType::BusyTentative);
        assert_eq!(doc.periods[1].start, dt(2026, 8, 11, 11, 0));
        assert_eq!(doc.periods[1].end, dt(2026, 8, 11, 12, 0));
    }

    #[test]
    fn finds_common_free_slots_across_attendees() {
        let alice_periods = vec![
            FreeBusyInterval::new(dt(2026, 8, 11, 9, 30), dt(2026, 8, 11, 10, 30), FreeBusyType::Busy),
            FreeBusyInterval::new(dt(2026, 8, 11, 14, 0), dt(2026, 8, 11, 15, 0), FreeBusyType::Busy),
        ];
        let bob_periods = vec![
            FreeBusyInterval::new(dt(2026, 8, 11, 10, 0), dt(2026, 8, 11, 11, 0), FreeBusyType::Busy),
            FreeBusyInterval::new(dt(2026, 8, 11, 13, 0), dt(2026, 8, 11, 14, 0), FreeBusyType::Busy),
        ];

        let schedules = vec![
            ("alice@example.org", alice_periods.as_slice()),
            ("bob@example.org", bob_periods.as_slice()),
        ];

        let slots = find_common_free_slots(
            &schedules,
            dt(2026, 8, 11, 0, 0),
            dt(2026, 8, 11, 23, 59),
            9 * 60,  // 09:00
            17 * 60, // 17:00
            30,      // min 30m
        );

        assert_eq!(
            slots,
            vec![
                MeetingFreeSlot {
                    start: dt(2026, 8, 11, 9, 0),
                    end: dt(2026, 8, 11, 9, 30),
                    all_attendees_available: true,
                    conflicting_attendees: Vec::new(),
                },
                MeetingFreeSlot {
                    start: dt(2026, 8, 11, 11, 0),
                    end: dt(2026, 8, 11, 13, 0),
                    all_attendees_available: true,
                    conflicting_attendees: Vec::new(),
                },
                MeetingFreeSlot {
                    start: dt(2026, 8, 11, 15, 0),
                    end: dt(2026, 8, 11, 17, 0),
                    all_attendees_available: true,
                    conflicting_attendees: Vec::new(),
                },
            ]
        );
    }
}
