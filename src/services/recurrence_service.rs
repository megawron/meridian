// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable RFC 5545 recurrence helpers shared by calendar and task views.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecurrenceByDay {
    pub(crate) weekday: Weekday,
    pub(crate) ordinal: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecurrenceRule {
    pub(crate) frequency: String,
    pub(crate) interval: i64,
    pub(crate) count: Option<usize>,
    pub(crate) until: Option<NaiveDate>,
    pub(crate) week_start: Weekday,
    pub(crate) by_days: Vec<RecurrenceByDay>,
    pub(crate) by_month_days: Vec<i32>,
    pub(crate) by_months: Vec<u32>,
    pub(crate) by_set_positions: Vec<i32>,
}

pub(crate) fn parse_recurrence_rule(raw: &str) -> Option<RecurrenceRule> {
    let mut frequency = None;
    let mut interval = 1_i64;
    let mut count = None;
    let mut until = None;
    let mut invalid_count = false;
    let mut invalid_until = false;
    let mut week_start = Weekday::Mon;
    let mut by_days = Vec::new();
    let mut by_month_days = Vec::new();
    let mut by_months = Vec::new();
    let mut by_set_positions = Vec::new();

    for part in raw.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => frequency = Some(value.trim().to_ascii_uppercase()),
            "INTERVAL" => {
                interval = value
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or(1)
            }
            "COUNT" => {
                count = value
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|value| *value > 0);
                invalid_count |= count.is_none();
            }
            "UNTIL" => {
                until = parse_ical_date(value);
                invalid_until |= until.is_none();
            }
            "WKST" => {
                if let Some(value) = parse_weekday_token(value) {
                    week_start = value;
                }
            }
            "BYDAY" => by_days.extend(value.split(',').filter_map(parse_extended_weekday_token)),
            "BYMONTHDAY" => by_month_days.extend(value.split(',').filter_map(|value| {
                let day = value.trim().parse::<i32>().ok()?;
                (day != 0 && (-31..=31).contains(&day)).then_some(day)
            })),
            "BYMONTH" => by_months.extend(value.split(',').filter_map(|value| {
                let month = value.trim().parse::<u32>().ok()?;
                (1..=12).contains(&month).then_some(month)
            })),
            "BYSETPOS" => by_set_positions.extend(value.split(',').filter_map(|value| {
                let position = value.trim().parse::<i32>().ok()?;
                (position != 0 && (-366..=366).contains(&position)).then_some(position)
            })),
            _ => {}
        }
    }

    if invalid_count || invalid_until {
        return None;
    }

    Some(RecurrenceRule {
        frequency: frequency?,
        interval,
        count,
        until,
        week_start,
        by_days,
        by_month_days,
        by_months,
        by_set_positions,
    })
}

/// Returns whether a date belongs to the rule before COUNT and EXDATE are applied.
pub(crate) fn recurrence_matches_without_count(
    rule: &RecurrenceRule,
    initial_start: NaiveDate,
    date: NaiveDate,
) -> bool {
    if !recurrence_candidate_matches(rule, initial_start, date) {
        return false;
    }
    if rule.by_set_positions.is_empty() {
        return true;
    }

    let (period_start, period_end) = recurrence_period_bounds(rule, date);
    let candidates = (0..=(period_end - period_start).num_days())
        .map(|offset| period_start + Duration::days(offset))
        .filter(|candidate| recurrence_candidate_matches(rule, initial_start, *candidate))
        .collect::<Vec<_>>();
    let Some(index) = candidates.iter().position(|candidate| *candidate == date) else {
        return false;
    };
    rule.by_set_positions.iter().any(|position| {
        if *position > 0 {
            usize::try_from(*position - 1).ok() == Some(index)
        } else {
            usize::try_from(position.unsigned_abs() - 1).ok()
                == Some(candidates.len().saturating_sub(index + 1))
        }
    })
}

/// Evaluates one rule occurrence, including COUNT and an optional EXDATE set.
pub(crate) fn recurrence_occurs_on(
    rule: &RecurrenceRule,
    initial_start: NaiveDate,
    date: NaiveDate,
    excluded_dates: &[NaiveDate],
) -> bool {
    if date < initial_start
        || rule.until.is_some_and(|until| date > until)
        || excluded_dates.contains(&date)
    {
        return false;
    }
    if !is_generated_occurrence(rule, initial_start, date) {
        return false;
    }

    let Some(count) = rule.count else {
        return true;
    };
    let mut occurrences = 0_usize;
    let mut cursor = initial_start;
    while cursor <= date {
        if is_generated_occurrence(rule, initial_start, cursor) {
            occurrences = occurrences.saturating_add(1);
            if occurrences > count {
                return false;
            }
        }
        let Some(next) = cursor.checked_add_signed(Duration::days(1)) else {
            break;
        };
        cursor = next;
    }
    occurrences > 0
}

/// Finds the next generated occurrence on or after `from`, excluding EXDATE values.
pub(crate) fn next_recurrence_date(
    rule: &RecurrenceRule,
    initial_start: NaiveDate,
    from: NaiveDate,
    excluded_dates: &[NaiveDate],
) -> Option<NaiveDate> {
    let mut occurrences = 0_usize;
    let mut cursor = initial_start;
    loop {
        if is_generated_occurrence(rule, initial_start, cursor) {
            occurrences = occurrences.saturating_add(1);
            if rule.count.is_some_and(|count| occurrences > count) {
                return None;
            }
            if cursor >= from && !excluded_dates.contains(&cursor) {
                return Some(cursor);
            }
        }
        if rule.until.is_some_and(|until| cursor >= until) {
            return None;
        }
        cursor = cursor.checked_add_signed(Duration::days(1))?;
    }
}

fn is_generated_occurrence(
    rule: &RecurrenceRule,
    initial_start: NaiveDate,
    date: NaiveDate,
) -> bool {
    date == initial_start || recurrence_matches_without_count(rule, initial_start, date)
}

fn recurrence_candidate_matches(
    rule: &RecurrenceRule,
    initial_start: NaiveDate,
    date: NaiveDate,
) -> bool {
    if date < initial_start || rule.until.is_some_and(|until| date > until) {
        return false;
    }

    let period_matches = match rule.frequency.as_str() {
        "DAILY" => (date - initial_start).num_days() % rule.interval == 0,
        "WEEKLY" => {
            let initial_week = week_start(initial_start, rule.week_start);
            let weeks = (week_start(date, rule.week_start) - initial_week)
                .num_days()
                .div_euclid(7);
            weeks >= 0 && weeks % rule.interval == 0
        }
        "MONTHLY" => {
            let months = month_distance(initial_start, date);
            months >= 0 && months % rule.interval == 0
        }
        "YEARLY" => {
            let years = i64::from(date.year() - initial_start.year());
            years >= 0 && years % rule.interval == 0
        }
        _ => false,
    };
    if !period_matches {
        return false;
    }

    if !rule.by_months.is_empty() && !rule.by_months.contains(&date.month()) {
        return false;
    }
    if !rule.by_month_days.is_empty()
        && !rule
            .by_month_days
            .iter()
            .any(|day| month_day_matches(*day, date))
    {
        return false;
    }
    if !rule.by_days.is_empty()
        && !rule.by_days.iter().any(|by_day| {
            if by_day.weekday != date.weekday() {
                return false;
            }
            match rule.frequency.as_str() {
                "MONTHLY" => by_day_ordinal_matches_in_month(*by_day, date),
                "YEARLY" if !rule.by_months.is_empty() => {
                    by_day_ordinal_matches_in_month(*by_day, date)
                }
                "YEARLY" => by_day_ordinal_matches_in_year(*by_day, date),
                _ => true,
            }
        })
    {
        return false;
    }

    match rule.frequency.as_str() {
        "WEEKLY" if rule.by_days.is_empty() => date.weekday() == initial_start.weekday(),
        "MONTHLY" if rule.by_month_days.is_empty() && rule.by_days.is_empty() => {
            date.day() == initial_start.day()
        }
        "YEARLY" if rule.by_month_days.is_empty() && rule.by_days.is_empty() => {
            date.month() == initial_start.month() && date.day() == initial_start.day()
        }
        _ => true,
    }
}

fn recurrence_period_bounds(rule: &RecurrenceRule, date: NaiveDate) -> (NaiveDate, NaiveDate) {
    match rule.frequency.as_str() {
        "DAILY" => (date, date),
        "WEEKLY" => {
            let start = week_start(date, rule.week_start);
            (start, start + Duration::days(6))
        }
        "MONTHLY" => {
            let start = date.with_day(1).unwrap_or(date);
            let end = next_month(start)
                .map(|value| value - Duration::days(1))
                .unwrap_or(date);
            (start, end)
        }
        "YEARLY" => {
            let start = NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap_or(date);
            let end = NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
                .map(|value| value - Duration::days(1))
                .unwrap_or(date);
            (start, end)
        }
        _ => (date, date),
    }
}

fn week_start(date: NaiveDate, first_weekday: Weekday) -> NaiveDate {
    let current = i64::from(date.weekday().num_days_from_monday());
    let first = i64::from(first_weekday.num_days_from_monday());
    date - Duration::days((current - first).rem_euclid(7))
}

fn month_distance(start: NaiveDate, date: NaiveDate) -> i64 {
    i64::from(date.year() - start.year()) * 12
        + i64::from(date.month() as i32 - start.month() as i32)
}

fn next_month(date: NaiveDate) -> Option<NaiveDate> {
    if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    }
}

fn days_in_month(date: NaiveDate) -> u32 {
    next_month(date)
        .and_then(|value| value.pred_opt())
        .map(|value| value.day())
        .unwrap_or(31)
}

fn month_day_matches(day: i32, date: NaiveDate) -> bool {
    if day > 0 {
        date.day() == day as u32
    } else {
        let offset = day.unsigned_abs();
        offset <= days_in_month(date) && date.day() == days_in_month(date) - offset + 1
    }
}

fn by_day_ordinal_matches_in_month(by_day: RecurrenceByDay, date: NaiveDate) -> bool {
    let Some(ordinal) = by_day.ordinal else {
        return true;
    };
    let positive = ((date.day() - 1) / 7 + 1) as i32;
    if ordinal > 0 {
        positive == ordinal
    } else {
        let negative = -(((days_in_month(date) - date.day()) / 7 + 1) as i32);
        negative == ordinal
    }
}

fn by_day_ordinal_matches_in_year(by_day: RecurrenceByDay, date: NaiveDate) -> bool {
    let Some(ordinal) = by_day.ordinal else {
        return true;
    };
    let start = NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap_or(date);
    let end = NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
        .and_then(|value| value.pred_opt())
        .unwrap_or(date);
    let matching = (0..=(end - start).num_days())
        .map(|offset| start + Duration::days(offset))
        .filter(|candidate| candidate.weekday() == by_day.weekday)
        .collect::<Vec<_>>();
    let Some(index) = matching.iter().position(|candidate| *candidate == date) else {
        return false;
    };
    if ordinal > 0 {
        usize::try_from(ordinal - 1).ok() == Some(index)
    } else {
        usize::try_from(ordinal.unsigned_abs() - 1).ok()
            == Some(matching.len().saturating_sub(index + 1))
    }
}

pub(crate) fn parse_extended_weekday_token(value: &str) -> Option<RecurrenceByDay> {
    let value = value.trim().to_ascii_uppercase();
    if value.len() < 2 {
        return None;
    }
    let weekday = parse_weekday_token(value.get(value.len() - 2..)?)?;
    let prefix = value.get(..value.len() - 2).unwrap_or_default();
    let ordinal = if prefix.is_empty() {
        None
    } else {
        let ordinal = prefix.parse::<i32>().ok()?;
        (ordinal != 0 && (-53..=53).contains(&ordinal)).then_some(ordinal)
    };
    Some(RecurrenceByDay { weekday, ordinal })
}

fn parse_weekday_token(value: &str) -> Option<Weekday> {
    match value.trim().to_ascii_uppercase().as_str() {
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

pub(crate) fn parse_ical_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if value.len() < 8 {
        return None;
    }
    NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").expect("test date should parse")
    }

    #[test]
    fn next_daily_occurrence_honors_count_and_exdate() {
        let rule = parse_recurrence_rule("FREQ=DAILY;COUNT=4").expect("rule should parse");
        let excluded = [date("2026-08-02")];

        assert_eq!(
            next_recurrence_date(&rule, date("2026-08-01"), date("2026-08-01"), &excluded),
            Some(date("2026-08-01"))
        );
        assert_eq!(
            next_recurrence_date(&rule, date("2026-08-01"), date("2026-08-02"), &excluded),
            Some(date("2026-08-03"))
        );
        assert_eq!(
            next_recurrence_date(&rule, date("2026-08-01"), date("2026-08-05"), &excluded),
            None
        );
    }

    #[test]
    fn count_is_not_extended_by_an_excluded_occurrence() {
        let rule = parse_recurrence_rule("FREQ=DAILY;COUNT=3").expect("rule should parse");
        let excluded = [date("2026-08-02")];

        assert_eq!(
            next_recurrence_date(&rule, date("2026-08-01"), date("2026-08-03"), &excluded),
            Some(date("2026-08-03"))
        );
        assert_eq!(
            next_recurrence_date(&rule, date("2026-08-01"), date("2026-08-04"), &excluded),
            None
        );
    }

    #[test]
    fn weekly_and_until_rules_find_the_expected_series_dates() {
        let rule = parse_recurrence_rule("FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20260812").unwrap();
        let start = date("2026-08-03");

        assert_eq!(
            next_recurrence_date(&rule, start, date("2026-08-04"), &[]),
            Some(date("2026-08-05"))
        );
        assert_eq!(
            next_recurrence_date(&rule, start, date("2026-08-13"), &[]),
            None
        );
    }

    #[test]
    fn weekly_interval_honors_rrule_week_start() {
        let monday_rule = parse_recurrence_rule("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO;WKST=MO").unwrap();
        let sunday_rule = parse_recurrence_rule("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO;WKST=SU").unwrap();
        let start = date("2026-08-09");

        assert_eq!(monday_rule.week_start, Weekday::Mon);
        assert_eq!(sunday_rule.week_start, Weekday::Sun);
        assert!(!recurrence_occurs_on(
            &monday_rule,
            start,
            date("2026-08-10"),
            &[]
        ));
        assert!(recurrence_occurs_on(
            &sunday_rule,
            start,
            date("2026-08-10"),
            &[]
        ));
        assert!(recurrence_occurs_on(
            &monday_rule,
            start,
            date("2026-08-17"),
            &[]
        ));
        assert!(!recurrence_occurs_on(
            &sunday_rule,
            start,
            date("2026-08-17"),
            &[]
        ));
    }

    #[test]
    fn weekly_set_position_uses_rrule_week_start() {
        let monday_rule =
            parse_recurrence_rule("FREQ=WEEKLY;BYDAY=SU,MO;BYSETPOS=1;WKST=MO").unwrap();
        let sunday_rule =
            parse_recurrence_rule("FREQ=WEEKLY;BYDAY=SU,MO;BYSETPOS=1;WKST=SU").unwrap();
        let start = date("2026-08-02");

        assert!(recurrence_matches_without_count(
            &monday_rule,
            start,
            date("2026-08-03")
        ));
        assert!(!recurrence_matches_without_count(
            &monday_rule,
            start,
            date("2026-08-09")
        ));
        assert!(!recurrence_matches_without_count(
            &sunday_rule,
            start,
            date("2026-08-03")
        ));
        assert!(recurrence_matches_without_count(
            &sunday_rule,
            start,
            date("2026-08-09")
        ));
    }

    #[test]
    fn monthly_count_skips_months_without_the_start_day() {
        let rule = parse_recurrence_rule("FREQ=MONTHLY;COUNT=4").unwrap();
        let start = date("2026-01-31");

        assert!(recurrence_occurs_on(&rule, start, date("2026-01-31"), &[]));
        assert!(!recurrence_occurs_on(&rule, start, date("2026-02-28"), &[]));
        assert!(recurrence_occurs_on(&rule, start, date("2026-03-31"), &[]));
        assert!(!recurrence_occurs_on(&rule, start, date("2026-04-30"), &[]));
        assert!(recurrence_occurs_on(&rule, start, date("2026-05-31"), &[]));
        assert_eq!(
            next_recurrence_date(&rule, start, date("2026-02-01"), &[]),
            Some(date("2026-03-31"))
        );
    }

    #[test]
    fn yearly_count_skips_non_leap_years_for_february_29() {
        let rule = parse_recurrence_rule("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29;COUNT=3").unwrap();
        let start = date("2024-02-29");

        assert!(recurrence_occurs_on(&rule, start, date("2024-02-29"), &[]));
        assert!(!recurrence_occurs_on(&rule, start, date("2025-02-28"), &[]));
        assert!(recurrence_occurs_on(&rule, start, date("2028-02-29"), &[]));
        assert!(recurrence_occurs_on(&rule, start, date("2032-02-29"), &[]));
        assert_eq!(
            next_recurrence_date(&rule, start, date("2025-01-01"), &[]),
            Some(date("2028-02-29"))
        );
    }

    #[test]
    fn until_date_time_is_inclusive_at_date_precision() {
        let rule = parse_recurrence_rule("FREQ=DAILY;UNTIL=20260803T235959Z").unwrap();
        let start = date("2026-08-01");

        assert_eq!(rule.until, Some(date("2026-08-03")));
        assert!(recurrence_occurs_on(&rule, start, date("2026-08-03"), &[]));
        assert!(!recurrence_occurs_on(&rule, start, date("2026-08-04"), &[]));
        assert_eq!(
            next_recurrence_date(&rule, start, date("2026-08-04"), &[]),
            None
        );
    }

    #[test]
    fn parse_ical_date_accepts_date_time_values_without_panicking() {
        assert_eq!(
            parse_ical_date("20260803T235959Z"),
            Some(date("2026-08-03"))
        );
        assert_eq!(parse_ical_date("1234567ä"), None);
    }

    #[test]
    fn invalid_count_and_until_do_not_create_unbounded_rules() {
        assert!(parse_recurrence_rule("FREQ=DAILY;COUNT=0").is_none());
        assert!(parse_recurrence_rule("FREQ=DAILY;COUNT=-1").is_none());
        assert!(parse_recurrence_rule("FREQ=DAILY;UNTIL=not-a-date").is_none());
        assert!(parse_recurrence_rule("FREQ=DAILY").is_some());
    }
}
