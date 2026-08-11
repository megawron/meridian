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
                    .filter(|value| *value > 0)
            }
            "UNTIL" => until = parse_ical_date(value),
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

    Some(RecurrenceRule {
        frequency: frequency?,
        interval,
        count,
        until,
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

    let (period_start, period_end) = recurrence_period_bounds(&rule.frequency, date);
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
            let weeks = (week_start(date) - week_start(initial_start))
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

fn recurrence_period_bounds(frequency: &str, date: NaiveDate) -> (NaiveDate, NaiveDate) {
    match frequency {
        "DAILY" => (date, date),
        "WEEKLY" => {
            let start = week_start(date);
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

fn week_start(date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
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
    let weekday = match value.get(value.len() - 2..)? {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    };
    let prefix = value.get(..value.len() - 2).unwrap_or_default();
    let ordinal = if prefix.is_empty() {
        None
    } else {
        let ordinal = prefix.parse::<i32>().ok()?;
        (ordinal != 0 && (-53..=53).contains(&ordinal)).then_some(ordinal)
    };
    Some(RecurrenceByDay { weekday, ordinal })
}

pub(crate) fn parse_ical_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if value.len() < 8 {
        return None;
    }
    NaiveDate::parse_from_str(&value[..8], "%Y%m%d").ok()
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
}
