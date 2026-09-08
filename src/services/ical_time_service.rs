// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable iCalendar time handling.  iCalendar stores floating local times,
// UTC values and named TZID values in the same component.  GLib already ships
// the platform IANA time-zone database, so the GTK4 client can convert those
// values without maintaining a second, incomplete zone database.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use glib::{DateTime, TimeZone};

use super::settings_service::AppSettings;

/// Return the local date represented by an iCalendar property value.
pub(crate) fn property_local_date(
    component: &str,
    target: &str,
    all_day: bool,
) -> Option<NaiveDate> {
    property_local_datetime(component, target, all_day).map(|datetime| datetime.date())
}

/// Return the first matching iCalendar property converted to the local zone.
pub(crate) fn property_local_datetime(
    component: &str,
    target: &str,
    all_day: bool,
) -> Option<NaiveDateTime> {
    let (property, value) = property_line(component, target)?;
    local_datetime_from_property(&property, &value, all_day)
}

/// Parse an RFC 5545 duration string such as "PT1H", "PT30M", "P1D", "PT1H30M".
pub(crate) fn parse_ical_duration(value: &str) -> Option<chrono::Duration> {
    let value = value.trim();
    let (is_negative, rest) = if let Some(stripped) = value.strip_prefix('-') {
        (true, stripped)
    } else if let Some(stripped) = value.strip_prefix('+') {
        (false, stripped)
    } else {
        (false, value)
    };
    let rest = rest.strip_prefix('P')?;
    let mut days = 0i64;
    let mut hours = 0i64;
    let mut minutes = 0i64;
    let mut seconds = 0i64;

    let (date_part, time_part) = if let Some((d, t)) = rest.split_once('T') {
        (d, Some(t))
    } else {
        (rest, None)
    };

    let mut current_num = String::new();
    for ch in date_part.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else if ch == 'W' {
            let weeks = current_num.parse::<i64>().ok()?;
            days += weeks * 7;
            current_num.clear();
        } else if ch == 'D' {
            let d = current_num.parse::<i64>().ok()?;
            days += d;
            current_num.clear();
        }
    }

    if let Some(time_part) = time_part {
        current_num.clear();
        for ch in time_part.chars() {
            if ch.is_ascii_digit() {
                current_num.push(ch);
            } else if ch == 'H' {
                let h = current_num.parse::<i64>().ok()?;
                hours += h;
                current_num.clear();
            } else if ch == 'M' {
                let m = current_num.parse::<i64>().ok()?;
                minutes += m;
                current_num.clear();
            } else if ch == 'S' {
                let s = current_num.parse::<i64>().ok()?;
                seconds += s;
                current_num.clear();
            }
        }
    }

    let total_seconds = days * 86400 + hours * 3600 + minutes * 60 + seconds;
    let duration = chrono::Duration::seconds(if is_negative { -total_seconds } else { total_seconds });
    Some(duration)
}

/// Return the duration specified by DURATION property in an iCalendar component.
pub(crate) fn property_local_duration(component: &str) -> Option<chrono::Duration> {
    let (_, value) = property_line(component, "DURATION")?;
    parse_ical_duration(&value)
}

/// Convert an iCalendar date-time property to a Unix timestamp.  This is used
/// for absolute VALARM triggers where the original TZID parameter must be
/// honored instead of first going through a display string.
pub(crate) fn property_local_timestamp(
    property: &str,
    value: &str,
    all_day_hour: u32,
) -> Option<i64> {
    let value = value.trim();
    let (year, month, day, hour, minute, second) =
        if value.len() == 8 && value.chars().all(|character| character.is_ascii_digit()) {
            let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
            (date.year(), date.month(), date.day(), all_day_hour, 0, 0)
        } else {
            if value.len() < 15 || value.as_bytes().get(8) != Some(&b'T') {
                return None;
            }
            let datetime = NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()?;
            (
                datetime.date().year(),
                datetime.date().month(),
                datetime.date().day(),
                datetime.time().hour(),
                datetime.time().minute(),
                datetime.time().second(),
            )
        };

    let source_timezone = if value.len() > 15 && value[15..].starts_with('Z') {
        TimeZone::utc()
    } else if let Some(identifier) = property_parameter(property, "TZID") {
        TimeZone::from_identifier(Some(identifier.trim_matches('"')))?
    } else {
        display_timezone()
    };
    DateTime::new(
        &source_timezone,
        year,
        month as i32,
        day as i32,
        hour as i32,
        minute as i32,
        second as f64,
    )
    .ok()
    .map(|datetime| datetime.to_unix())
}

/// Format the first matching property for the calendar UI after local-zone
/// conversion.  Date-only properties remain date-only.
pub(crate) fn property_local_display(
    component: &str,
    target: &str,
    all_day: bool,
) -> Option<String> {
    let (property, value) = property_line(component, target)?;
    let date_only = all_day || property_has_parameter(&property, "VALUE", "DATE");
    let datetime = local_datetime_from_property(&property, &value, all_day)?;
    Some(if date_only {
        datetime.date().format("%Y-%m-%d").to_string()
    } else {
        datetime.format("%Y-%m-%d %H:%M").to_string()
    })
}

/// Return all local calendar dates represented by a repeated property such as
/// RDATE or EXDATE.  Comma-separated values share the property parameters.
pub(crate) fn property_local_dates(component: &str, target: &str) -> Vec<NaiveDate> {
    let target = target.trim().to_ascii_uppercase();
    unfold_lines(component)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .trim()
                .to_ascii_uppercase();
            (name == target).then_some((property.to_string(), value.to_string()))
        })
        .flat_map(|(property, values)| {
            let all_day = property_has_parameter(&property, "VALUE", "DATE");
            values
                .split(',')
                .filter_map(move |value| {
                    local_datetime_from_property(&property, value, all_day)
                        .map(|datetime| datetime.date())
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn property_line(component: &str, target: &str) -> Option<(String, String)> {
    let target = target.trim().to_ascii_uppercase();
    unfold_lines(component).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .trim()
            .to_ascii_uppercase();
        (name == target).then_some((property.to_string(), value.to_string()))
    })
}

fn local_datetime_from_property(
    property: &str,
    value: &str,
    all_day: bool,
) -> Option<NaiveDateTime> {
    let value = value.trim();
    if value.len() == 8 && value.chars().all(|character| character.is_ascii_digit()) {
        return NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()?
            .and_hms_opt(0, 0, 0);
    }
    if value.len() < 15 || value.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()?;
    if all_day {
        return Some(naive);
    }

    let timezone = property_parameter(property, "TZID");
    if !value[15..].starts_with('Z') && timezone.is_none() {
        // Floating times are intentionally interpreted in the user's local
        // zone and therefore retain their wall-clock representation.
        return Some(naive);
    }

    let source_timezone = if value[15..].starts_with('Z') {
        TimeZone::utc()
    } else {
        let identifier = timezone.as_deref()?.trim_matches('"');
        TimeZone::from_identifier(Some(identifier))?
    };
    let source = DateTime::new(
        &source_timezone,
        naive.date().year(),
        naive.date().month() as i32,
        naive.date().day() as i32,
        naive.time().hour() as i32,
        naive.time().minute() as i32,
        naive.time().second() as f64,
    )
    .ok()?;
    let local = source.to_timezone(&display_timezone()).ok()?;
    NaiveDate::from_ymd_opt(
        local.year(),
        local.month() as u32,
        local.day_of_month() as u32,
    )?
    .and_hms_opt(
        local.hour() as u32,
        local.minute() as u32,
        local.second() as u32,
    )
}

fn property_parameter(property: &str, target: &str) -> Option<String> {
    let target = target.trim().to_ascii_uppercase();
    property.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        (name.trim().to_ascii_uppercase() == target).then(|| value.trim().to_string())
    })
}

fn property_has_parameter(property: &str, name: &str, expected: &str) -> bool {
    property_parameter(property, name)
        .is_some_and(|value| value.trim_matches('"').eq_ignore_ascii_case(expected))
}

fn display_timezone() -> TimeZone {
    let identifier = AppSettings::try_new()
        .map(|settings| settings.calendar_time_zone())
        .unwrap_or_else(|| "system".to_string());
    if identifier.eq_ignore_ascii_case("system") {
        TimeZone::local()
    } else if identifier.eq_ignore_ascii_case("utc") {
        TimeZone::utc()
    } else {
        TimeZone::from_identifier(Some(&identifier)).unwrap_or_else(TimeZone::local)
    }
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(line.trim_start());
            }
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_and_date_values_keep_their_wall_clock_value() {
        let component = concat!(
            "BEGIN:VEVENT\r\n",
            "DTSTART:20260810T120000\r\n",
            "EXDATE;VALUE=DATE:20260811\r\n",
            "END:VEVENT\r\n"
        );
        assert_eq!(
            property_local_display(component, "DTSTART", false).as_deref(),
            Some("2026-08-10 12:00")
        );
        assert_eq!(
            property_local_dates(component, "EXDATE"),
            vec![NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()]
        );
    }

    #[test]
    fn utc_values_convert_using_the_platform_local_zone() {
        let component = "DTSTART:20260810T120000Z\r\n";
        let local = property_local_datetime(component, "DTSTART", false).unwrap();
        let expected = DateTime::from_utc(2026, 8, 10, 12, 0, 0.0)
            .unwrap()
            .to_timezone(&display_timezone())
            .unwrap();
        assert_eq!(local.date().year(), expected.year());
        assert_eq!(local.date().month(), expected.month() as u32);
        assert_eq!(local.date().day(), expected.day_of_month() as u32);
        assert_eq!(local.time().hour(), expected.hour() as u32);
        assert_eq!(local.time().minute(), expected.minute() as u32);
    }

    #[test]
    fn named_timezone_values_are_accepted() {
        let component = "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n";
        assert!(property_local_datetime(component, "DTSTART", false).is_some());
    }

    #[test]
    fn ical_duration_parsing() {
        assert_eq!(parse_ical_duration("PT1H").unwrap(), chrono::Duration::hours(1));
        assert_eq!(parse_ical_duration("PT30M").unwrap(), chrono::Duration::minutes(30));
        assert_eq!(parse_ical_duration("PT1H30M").unwrap(), chrono::Duration::minutes(90));
        assert_eq!(parse_ical_duration("P1D").unwrap(), chrono::Duration::days(1));
        assert_eq!(parse_ical_duration("P1DT2H").unwrap(), chrono::Duration::hours(26));
        let comp = "BEGIN:VEVENT\r\nDURATION:PT2H\r\nEND:VEVENT\r\n";
        assert_eq!(property_local_duration(comp).unwrap(), chrono::Duration::hours(2));
    }
}
