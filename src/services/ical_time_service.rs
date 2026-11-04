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

/// Return the configured default calendar IANA timezone identifier (e.g. "Europe/Berlin").
pub(crate) fn default_calendar_timezone_id() -> String {
    let configured = AppSettings::try_new()
        .map(|settings| settings.calendar_time_zone())
        .unwrap_or_else(|| "system".to_string());
    if !configured.trim().is_empty() && !configured.eq_ignore_ascii_case("system") {
        return configured.trim().to_string();
    }
    let local_id = TimeZone::local().identifier().to_string();
    if !local_id.trim().is_empty() && local_id.contains('/') {
        local_id
    } else {
        "Europe/Berlin".to_string()
    }
}

/// Return the explicit `TZID` parameter on an iCalendar property if present.
pub(crate) fn property_explicit_tzid(component: &str, target: &str) -> Option<String> {
    let (property, _) = property_line(component, target)?;
    if property_has_parameter(&property, "VALUE", "DATE") {
        return None;
    }
    property_parameter(&property, "TZID")
        .map(|s| s.trim_matches('"').trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Return the wall-clock `YYYY-MM-DD HH:MM` (or `YYYY-MM-DD`) stored in the property
/// without converting to the local display timezone. Used when editing an event that
/// carries an explicit `TZID` so the editor displays the time in the event's timezone.
pub(crate) fn property_wallclock_display(
    component: &str,
    target: &str,
    all_day: bool,
) -> Option<String> {
    let (property, value) = property_line(component, target)?;
    let date_only = all_day || property_has_parameter(&property, "VALUE", "DATE");
    let value = value.trim();
    if value.len() >= 8 && value[..8].chars().all(|c| c.is_ascii_digit()) {
        let date = NaiveDate::parse_from_str(&value[..8], "%Y%m%d").ok()?;
        if date_only {
            return Some(date.format("%Y-%m-%d").to_string());
        }
        if value.len() >= 15 && value.as_bytes().get(8) == Some(&b'T') {
            let dt = NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()?;
            return Some(dt.format("%Y-%m-%d %H:%M").to_string());
        }
    }
    None
}

/// Format the first matching property for the calendar UI after local-zone
/// conversion.  Date-only properties remain date-only.
pub(crate) fn property_local_display(
    component: &str,
    target: &str,
    all_day: bool,
) -> Option<String> {
    let (property, value) = property_line(component, target)?;
    local_display_from_property(&property, &value, all_day)
}

/// Format an already-extracted `(property, value)` pair for the calendar UI
/// after local-zone conversion without re-scanning the raw component string.
#[inline]
pub(crate) fn local_display_from_property(
    property: &str,
    value: &str,
    all_day: bool,
) -> Option<String> {
    let date_only = all_day || property_has_parameter(property, "VALUE", "DATE");
    let datetime = local_datetime_from_property(property, value, all_day)?;
    Some(if date_only {
        format_ymd_fast(datetime.date())
    } else {
        format_ymd_hm_fast(datetime)
    })
}

#[inline]
pub(crate) fn parse_ical_date_fast(value: &str) -> Option<NaiveDate> {
    let b = value.as_bytes();
    if b.len() < 8 {
        return None;
    }
    let y0 = b[0].wrapping_sub(b'0');
    let y1 = b[1].wrapping_sub(b'0');
    let y2 = b[2].wrapping_sub(b'0');
    let y3 = b[3].wrapping_sub(b'0');
    let m0 = b[4].wrapping_sub(b'0');
    let m1 = b[5].wrapping_sub(b'0');
    let d0 = b[6].wrapping_sub(b'0');
    let d1 = b[7].wrapping_sub(b'0');
    if (y0 | y1 | y2 | y3 | m0 | m1 | d0 | d1) > 9 {
        return None;
    }
    let year = (y0 as i32) * 1000 + (y1 as i32) * 100 + (y2 as i32) * 10 + (y3 as i32);
    let month = (m0 as u32) * 10 + (m1 as u32);
    let day = (d0 as u32) * 10 + (d1 as u32);
    NaiveDate::from_ymd_opt(year, month, day)
}

#[inline]
pub(crate) fn parse_ical_datetime_fast(value: &str) -> Option<NaiveDateTime> {
    let b = value.as_bytes();
    if b.len() < 15 || b[8] != b'T' {
        return None;
    }
    let date = parse_ical_date_fast(&value[..8])?;
    let h0 = b[9].wrapping_sub(b'0');
    let h1 = b[10].wrapping_sub(b'0');
    let min0 = b[11].wrapping_sub(b'0');
    let min1 = b[12].wrapping_sub(b'0');
    let s0 = b[13].wrapping_sub(b'0');
    let s1 = b[14].wrapping_sub(b'0');
    if (h0 | h1 | min0 | min1 | s0 | s1) > 9 {
        return None;
    }
    let hour = (h0 as u32) * 10 + (h1 as u32);
    let minute = (min0 as u32) * 10 + (min1 as u32);
    let second = (s0 as u32) * 10 + (s1 as u32);
    date.and_hms_opt(hour, minute, second)
}

#[inline]
pub(crate) fn format_ymd_fast(date: NaiveDate) -> String {
    let y = date.year();
    if !(0..=9999).contains(&y) {
        return date.format("%Y-%m-%d").to_string();
    }
    let y = y as u32;
    let m = date.month();
    let d = date.day();
    let buf: [u8; 10] = [
        b'0' + ((y / 1000) % 10) as u8,
        b'0' + ((y / 100) % 10) as u8,
        b'0' + ((y / 10) % 10) as u8,
        b'0' + (y % 10) as u8,
        b'-',
        b'0' + (m / 10) as u8,
        b'0' + (m % 10) as u8,
        b'-',
        b'0' + (d / 10) as u8,
        b'0' + (d % 10) as u8,
    ];
    String::from_utf8_lossy(&buf).into_owned()
}

#[inline]
pub(crate) fn format_ymd_hm_fast(dt: NaiveDateTime) -> String {
    let date = dt.date();
    let y = date.year();
    if !(0..=9999).contains(&y) {
        return dt.format("%Y-%m-%d %H:%M").to_string();
    }
    let y = y as u32;
    let m = date.month();
    let d = date.day();
    let h = dt.time().hour();
    let min = dt.time().minute();
    let buf: [u8; 16] = [
        b'0' + ((y / 1000) % 10) as u8,
        b'0' + ((y / 100) % 10) as u8,
        b'0' + ((y / 10) % 10) as u8,
        b'0' + (y % 10) as u8,
        b'-',
        b'0' + (m / 10) as u8,
        b'0' + (m % 10) as u8,
        b'-',
        b'0' + (d / 10) as u8,
        b'0' + (d % 10) as u8,
        b' ',
        b'0' + (h / 10) as u8,
        b'0' + (h % 10) as u8,
        b':',
        b'0' + (min / 10) as u8,
        b'0' + (min % 10) as u8,
    ];
    String::from_utf8_lossy(&buf).into_owned()
}

/// Return all local calendar dates represented by a repeated property such as
/// RDATE or EXDATE.  Comma-separated values share the property parameters.
pub(crate) fn property_local_dates(component: &str, target: &str) -> Vec<NaiveDate> {
    let target = target.trim();
    matching_unfolded_lines(component, target)
        .into_iter()
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
    let target = target.trim();
    let mut current_match: Option<String> = None;
    for raw_line in iter_physical_lines(component) {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(prev) = current_match.as_mut() {
                prev.push_str(raw_line.trim_start());
            }
            continue;
        }
        if let Some(unfolded) = current_match.take() {
            if let Some((property, value)) = unfolded.split_once(':') {
                let name = property.split(';').next().unwrap_or(property).trim();
                if name.eq_ignore_ascii_case(target) {
                    return Some((property.to_string(), value.to_string()));
                }
            }
        }
        let first_token = raw_line.split([':', ';']).next().unwrap_or(raw_line).trim();
        let can_match = first_token.eq_ignore_ascii_case(target)
            || (!raw_line.contains([':', ';'])
                && target
                    .get(..first_token.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(first_token)));
        if can_match {
            current_match = Some(raw_line.to_string());
        }
    }
    if let Some(unfolded) = current_match.take() {
        if let Some((property, value)) = unfolded.split_once(':') {
            let name = property.split(';').next().unwrap_or(property).trim();
            if name.eq_ignore_ascii_case(target) {
                return Some((property.to_string(), value.to_string()));
            }
        }
    }
    None
}

fn matching_unfolded_lines(component: &str, target: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut current_match: Option<String> = None;
    for raw_line in iter_physical_lines(component) {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(prev) = current_match.as_mut() {
                prev.push_str(raw_line.trim_start());
            }
            continue;
        }
        if let Some(unfolded) = current_match.take() {
            if let Some((property, value)) = unfolded.split_once(':') {
                let name = property.split(';').next().unwrap_or(property).trim();
                if name.eq_ignore_ascii_case(target) {
                    results.push((property.to_string(), value.to_string()));
                }
            }
        }
        let first_token = raw_line.split([':', ';']).next().unwrap_or(raw_line).trim();
        let can_match = first_token.eq_ignore_ascii_case(target)
            || (!raw_line.contains([':', ';'])
                && target
                    .get(..first_token.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(first_token)));
        if can_match {
            current_match = Some(raw_line.to_string());
        }
    }
    if let Some(unfolded) = current_match.take() {
        if let Some((property, value)) = unfolded.split_once(':') {
            let name = property.split(';').next().unwrap_or(property).trim();
            if name.eq_ignore_ascii_case(target) {
                results.push((property.to_string(), value.to_string()));
            }
        }
    }
    results
}

fn iter_physical_lines(raw: &str) -> impl Iterator<Item = &str> {
    raw.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line))
}

pub(crate) fn local_datetime_from_property(
    property: &str,
    value: &str,
    all_day: bool,
) -> Option<NaiveDateTime> {
    let value = value.trim();
    if value.len() == 8 {
        return parse_ical_date_fast(value)?.and_hms_opt(0, 0, 0);
    }
    let naive = parse_ical_datetime_fast(value)?;
    if all_day {
        return Some(naive);
    }

    let is_utc = value.as_bytes().get(15) == Some(&b'Z');
    let timezone = if is_utc || !property.contains(';') {
        None
    } else {
        property_parameter(property, "TZID")
    };
    if !is_utc && timezone.is_none() {
        // Floating times are intentionally interpreted in the user's local
        // zone and therefore retain their wall-clock representation.
        return Some(naive);
    }

    let source_timezone = if is_utc {
        cached_utc_timezone()
    } else {
        let identifier = timezone.as_deref()?.trim_matches('"');
        cached_timezone_from_identifier(identifier)?
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
    if !property.contains(';') {
        return None;
    }
    property.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case(target.trim())
            .then(|| value.trim().to_string())
    })
}

fn property_has_parameter(property: &str, name: &str, expected: &str) -> bool {
    if !property.contains(';') {
        return false;
    }
    property.split(';').skip(1).any(|parameter| {
        let Some((p_name, p_val)) = parameter.split_once('=') else {
            return false;
        };
        p_name.trim().eq_ignore_ascii_case(name)
            && p_val.trim().trim_matches('"').eq_ignore_ascii_case(expected)
    })
}

thread_local! {
    static DISPLAY_TZ_CACHE: std::cell::RefCell<Option<(std::time::Instant, TimeZone)>> =
        const { std::cell::RefCell::new(None) };
    static UTC_TZ_CACHE: std::cell::OnceCell<TimeZone> = const { std::cell::OnceCell::new() };
    static NAMED_TZ_CACHE: std::cell::RefCell<std::collections::HashMap<String, Option<TimeZone>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn cached_utc_timezone() -> TimeZone {
    UTC_TZ_CACHE.with(|cell| cell.get_or_init(TimeZone::utc).clone())
}

fn cached_timezone_from_identifier(identifier: &str) -> Option<TimeZone> {
    NAMED_TZ_CACHE.with(|cache| {
        if let Some(cached) = cache.borrow().get(identifier) {
            return cached.clone();
        }
        let tz = TimeZone::from_identifier(Some(identifier));
        cache.borrow_mut().insert(identifier.to_string(), tz.clone());
        tz
    })
}

fn display_timezone() -> TimeZone {
    DISPLAY_TZ_CACHE.with(|cache| {
        let now = std::time::Instant::now();
        if let Some((checked_at, ref tz)) = *cache.borrow() {
            if now.duration_since(checked_at) < std::time::Duration::from_secs(2) {
                return tz.clone();
            }
        }
        let identifier = AppSettings::try_new()
            .map(|settings| settings.calendar_time_zone())
            .unwrap_or_else(|| "system".to_string());
        let tz = if identifier.eq_ignore_ascii_case("system") {
            TimeZone::local()
        } else if identifier.eq_ignore_ascii_case("utc") {
            cached_utc_timezone()
        } else {
            cached_timezone_from_identifier(&identifier).unwrap_or_else(TimeZone::local)
        };
        *cache.borrow_mut() = Some((now, tz.clone()));
        tz
    })
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
