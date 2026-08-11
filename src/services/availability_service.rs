// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable free/busy calculation for the application-owned calendar.
//
// This module intentionally works on already loaded local records.  A native
// ECal free/busy query, provider time zones, and server-side attendee
// availability remain Linux/EDS work; the same interval calculation can be
// reused once those records are supplied by the native adapter.

use chrono::{Duration, NaiveDate, NaiveDateTime};

use super::calendar_store_service::{CalendarEventRecord, CalendarSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyInterval {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSlot {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

impl FreeSlot {
    pub fn duration_minutes(self) -> i64 {
        self.end.signed_duration_since(self.start).num_minutes()
    }
}

/// Calculate contiguous free blocks inside the configured workday.
///
/// Busy intervals are clipped to the day, sorted, and merged before the free
/// gaps are returned.  A gap shorter than `minimum_duration_minutes` is not
/// useful for scheduling and is omitted.
pub fn find_free_slots(
    date: NaiveDate,
    busy_intervals: &[BusyInterval],
    workday_start_minutes: u32,
    workday_end_minutes: u32,
    minimum_duration_minutes: u32,
) -> Vec<FreeSlot> {
    let start_minutes = workday_start_minutes.min(1_439);
    let end_minutes = workday_end_minutes.min(1_440);
    if end_minutes <= start_minutes {
        return Vec::new();
    }
    let Some(window_start) = minutes_on_date(date, start_minutes) else {
        return Vec::new();
    };
    let Some(window_end) = (if end_minutes == 1_440 {
        date.checked_add_signed(Duration::days(1))
            .and_then(|next_day| next_day.and_hms_opt(0, 0, 0))
    } else {
        minutes_on_date(date, end_minutes)
    }) else {
        return Vec::new();
    };

    let mut clipped = busy_intervals
        .iter()
        .filter_map(|interval| {
            if interval.end <= interval.start
                || interval.end <= window_start
                || interval.start >= window_end
            {
                return None;
            }
            Some(BusyInterval {
                start: interval.start.max(window_start),
                end: interval.end.min(window_end),
            })
        })
        .collect::<Vec<_>>();
    clipped.sort_by_key(|interval| (interval.start, interval.end));

    let mut merged: Vec<BusyInterval> = Vec::with_capacity(clipped.len());
    for interval in clipped {
        if let Some(previous) = merged.last_mut() {
            if interval.start <= previous.end {
                previous.end = previous.end.max(interval.end);
                continue;
            }
        }
        merged.push(interval);
    }

    let minimum = i64::from(minimum_duration_minutes);
    let mut cursor = window_start;
    let mut free = Vec::new();
    for busy in merged {
        if busy.start > cursor && busy.start.signed_duration_since(cursor).num_minutes() >= minimum
        {
            free.push(FreeSlot {
                start: cursor,
                end: busy.start,
            });
        }
        cursor = cursor.max(busy.end);
    }
    if window_end > cursor && window_end.signed_duration_since(cursor).num_minutes() >= minimum {
        free.push(FreeSlot {
            start: cursor,
            end: window_end,
        });
    }
    free
}

/// Convert the local snapshot into busy intervals for one date.  This is the
/// portable fallback used by the UI; native ECal records can be converted to
/// the same `BusyInterval` type by the Linux adapter.
pub fn busy_intervals_for_snapshot(
    snapshot: &CalendarSnapshot,
    date: NaiveDate,
) -> Vec<BusyInterval> {
    snapshot
        .events
        .iter()
        .filter_map(|event| busy_interval_for_event(event, date))
        .collect()
}

fn busy_interval_for_event(event: &CalendarEventRecord, date: NaiveDate) -> Option<BusyInterval> {
    if event.all_day {
        let start_date = parse_date(&event.start)?;
        let end_date = parse_date(&event.end).unwrap_or(start_date);
        if date < start_date || date > end_date || (date == end_date && end_date > start_date) {
            return None;
        }
        let start = date.and_hms_opt(0, 0, 0)?;
        return Some(BusyInterval {
            start,
            end: start.checked_add_signed(Duration::days(1))?,
        });
    }

    let start = parse_datetime(&event.start)?;
    let end =
        parse_datetime(&event.end).or_else(|| start.checked_add_signed(Duration::hours(1)))?;
    if end <= date.and_hms_opt(0, 0, 0)?
        || start
            >= date
                .checked_add_signed(Duration::days(1))?
                .and_hms_opt(0, 0, 0)?
    {
        return None;
    }
    Some(BusyInterval { start, end })
}

fn minutes_on_date(date: NaiveDate, minutes: u32) -> Option<NaiveDateTime> {
    date.and_hms_opt(minutes / 60, minutes % 60, 0)
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if value.len() >= 10 {
        NaiveDate::parse_from_str(&value[..10], "%Y-%m-%d").ok()
    } else if value.len() >= 8 {
        NaiveDate::parse_from_str(&value[..8], "%Y%m%d").ok()
    } else {
        None
    }
}

fn parse_datetime(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim().trim_end_matches('Z');
    if value.len() >= 16 {
        NaiveDateTime::parse_from_str(&value[..16], "%Y-%m-%d %H:%M").ok()
    } else if value.len() >= 15 {
        NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()
    } else {
        parse_date(value)?.and_hms_opt(0, 0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 11).expect("valid test date")
    }

    fn time(hour: u32, minute: u32) -> NaiveDateTime {
        date()
            .and_hms_opt(hour, minute, 0)
            .expect("valid test time")
    }

    #[test]
    fn merges_overlapping_busy_intervals_and_returns_gaps() {
        let slots = find_free_slots(
            date(),
            &[
                BusyInterval {
                    start: time(9, 30),
                    end: time(10, 30),
                },
                BusyInterval {
                    start: time(10, 0),
                    end: time(11, 0),
                },
                BusyInterval {
                    start: time(13, 0),
                    end: time(13, 30),
                },
            ],
            9 * 60,
            17 * 60,
            30,
        );
        assert_eq!(
            slots,
            vec![
                FreeSlot {
                    start: time(9, 0),
                    end: time(9, 30),
                },
                FreeSlot {
                    start: time(11, 0),
                    end: time(13, 0),
                },
                FreeSlot {
                    start: time(13, 30),
                    end: time(17, 0),
                },
            ]
        );
        assert_eq!(slots[1].duration_minutes(), 120);
    }

    #[test]
    fn clips_busy_intervals_to_workday_and_applies_minimum_duration() {
        let slots = find_free_slots(
            date(),
            &[BusyInterval {
                start: time(8, 0),
                end: time(16, 45),
            }],
            9 * 60,
            17 * 60,
            15,
        );
        assert_eq!(
            slots,
            vec![FreeSlot {
                start: time(16, 45),
                end: time(17, 0),
            }]
        );
        assert!(find_free_slots(date(), &[], 16 * 60 + 45, 17 * 60, 30).is_empty());
        assert!(find_free_slots(date(), &[], 17 * 60, 9 * 60, 15).is_empty());
    }

    #[test]
    fn converts_all_day_and_timed_snapshot_events() {
        let snapshot = CalendarSnapshot {
            events: vec![
                CalendarEventRecord {
                    uid: "all-day".to_string(),
                    source_uid: "local".to_string(),
                    source_display_name: "Local".to_string(),
                    component: String::new(),
                    summary: "Holiday".to_string(),
                    start: "2026-08-11".to_string(),
                    end: "2026-08-12".to_string(),
                    location: String::new(),
                    description: String::new(),
                    all_day: true,
                },
                CalendarEventRecord {
                    uid: "timed".to_string(),
                    source_uid: "local".to_string(),
                    source_display_name: "Local".to_string(),
                    component: String::new(),
                    summary: "Meeting".to_string(),
                    start: "2026-08-11 10:00".to_string(),
                    end: "2026-08-11 11:00".to_string(),
                    location: String::new(),
                    description: String::new(),
                    all_day: false,
                },
            ],
            ..CalendarSnapshot::default()
        };
        let intervals = busy_intervals_for_snapshot(&snapshot, date());
        assert_eq!(intervals.len(), 2);
        assert!(
            intervals
                .iter()
                .any(|interval| interval.start == time(10, 0))
        );
        assert!(
            intervals
                .iter()
                .any(|interval| interval.start == date().and_hms_opt(0, 0, 0).unwrap())
        );
    }
}
