// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable iTIP/iCalendar invitation inspection and response construction.
//
// Parsing/importing invitation data and constructing a standards-shaped REPLY
// are portable operations.  Sending ACCEPTED/TENTATIVE/DECLINED replies
// through an account transport remains an EDS/Camel operation and is
// intentionally kept in the Linux roadmap until the native account path is
// verified.

use crate::services::calendar_store_service::{CalendarEventRecord, CalendarSnapshot};
use chrono::Utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItipMethod {
    Request,
    Reply,
    Cancel,
    Publish,
    Add,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItipReplyStatus {
    Accepted,
    Tentative,
    Declined,
}

impl ItipReplyStatus {
    pub fn as_ical(self) -> &'static str {
        match self {
            Self::Accepted => "ACCEPTED",
            Self::Tentative => "TENTATIVE",
            Self::Declined => "DECLINED",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "Accept",
            Self::Tentative => "Tentative",
            Self::Declined => "Decline",
        }
    }
}

impl ItipMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "REQUEST",
            Self::Reply => "REPLY",
            Self::Cancel => "CANCEL",
            Self::Publish => "PUBLISH",
            Self::Add => "ADD",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItipPerson {
    pub name: Option<String>,
    pub email: String,
}

impl ItipPerson {
    pub fn display_label(&self) -> String {
        if let Some(name) = &self.name {
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }
        if !self.email.is_empty() {
            self.email.clone()
        } else {
            String::new()
        }
    }

    pub fn full_display(&self) -> String {
        if let Some(name) = &self.name {
            if !name.trim().is_empty() && !self.email.is_empty() {
                return format!("{} <{}>", name.trim(), self.email.trim());
            }
        }
        if !self.email.is_empty() {
            self.email.clone()
        } else {
            self.name.clone().unwrap_or_default()
        }
    }
}

pub fn strip_mailto_prefix(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 7 && s[..7].eq_ignore_ascii_case("mailto:") {
        s[7..].trim()
    } else {
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItipInvitation {
    pub method: ItipMethod,
    pub event: CalendarEventRecord,
    pub organizer: String,
    pub attendees: Vec<String>,
    pub organizer_person: Option<ItipPerson>,
    pub attendee_persons: Vec<ItipPerson>,
    pub sequence: Option<u32>,
    pub recurrence: Option<String>,
    pub raw_component: String,
}

/// Build a standards-shaped iTIP REPLY without contacting an account
/// provider.  The result is ready to become a `text/calendar; method=REPLY`
/// MIME part or a portable response draft.  Camel remains responsible for
/// transport and server-side attendee state on Linux.
pub fn build_reply_component(
    invitation: &ItipInvitation,
    attendee: &str,
    status: ItipReplyStatus,
    comment: Option<&str>,
) -> Result<String, String> {
    if !matches!(invitation.method, ItipMethod::Request | ItipMethod::Add) {
        return Err("No reply is intended for this iTIP message".to_string());
    }
    let attendee = normalize_attendee_address(attendee).ok_or_else(|| {
        "A valid own email address is required to reply".to_string()
    })?;
    let uid = top_level_property(&invitation.event.component, "UID")
        .map(|(_, value)| value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| invitation.event.uid.clone());
    if uid.trim().is_empty() {
        return Err("The invitation has no UID".to_string());
    }

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Meridian//iTIP Reply//EN".to_string(),
        "METHOD:REPLY".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
    ];
    if let Some((_, value)) = top_level_property(&invitation.event.component, "DTSTAMP") {
        lines.push(format!("DTSTAMP:{value}"));
    } else {
        lines.push(format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
    }
    for property in ["SEQUENCE", "DTSTART", "DTEND", "RECURRENCE-ID"] {
        if let Some((prefix, value)) = top_level_property(&invitation.event.component, property) {
            lines.push(format!("{prefix}:{value}"));
        }
    }
    if let Some((prefix, value)) = top_level_property(&invitation.event.component, "ORGANIZER")
        .or_else(|| {
            let organizer = invitation.organizer.trim();
            (!organizer.is_empty()).then(|| ("ORGANIZER".to_string(), organizer.to_string()))
        })
    {
        lines.push(format!("{prefix}:{value}"));
    }
    lines.push(reply_attendee_line(
        &invitation.event.component,
        &attendee,
        status,
    ));
    if let Some(comment_text) = comment {
        let clean = comment_text.trim().replace("\r\n", " ").replace('\n', " ");
        if !clean.is_empty() {
            lines.push(format!("COMMENT:{clean}"));
        }
    }
    lines.extend(["END:VEVENT".to_string(), "END:VCALENDAR".to_string()]);
    Ok(format!("{}\r\n", lines.join("\r\n")))
}

pub fn normalize_attendee_address(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value
        .rsplit_once('<')
        .and_then(|(_, value)| value.strip_suffix('>'))
        .unwrap_or(value)
        .trim();
    let value = strip_mailto_prefix(value);
    if value.is_empty() || value.chars().any(char::is_whitespace) || !value.contains('@') {
        return None;
    }
    Some(format!("mailto:{value}"))
}

pub fn parse_ical_person(line: &str) -> ItipPerson {
    let line = line.trim();
    let (property_and_params, val_part) = line.split_once(':').unwrap_or(("", line));

    let mut cn = None;
    for param in property_and_params.split(';') {
        let param = param.trim();
        if param.len() >= 3 && param[..3].eq_ignore_ascii_case("CN=") {
            let val = param[3..].trim_matches('"').trim();
            if !val.is_empty() {
                cn = Some(unescape_ical_text(val));
                break;
            }
        }
    }

    let clean_val = strip_mailto_prefix(val_part).trim();
    let (name, email) = if let Some(start) = clean_val.rfind('<') {
        if let Some(end_offset) = clean_val[start..].find('>') {
            let end = start + end_offset;
            let mail = clean_val[start + 1..end].trim();
            let parsed_name = clean_val[..start].trim().trim_matches('"').trim();
            let resolved_name = cn.or_else(|| (!parsed_name.is_empty()).then(|| parsed_name.to_string()));
            (resolved_name, strip_mailto_prefix(mail).to_string())
        } else {
            (cn, clean_val.to_string())
        }
    } else {
        (cn, clean_val.to_string())
    };

    ItipPerson { name, email }
}

/// Parse a `text/calendar` payload if it contains a supported VEVENT and a
/// meaningful iTIP method.  The returned event remains a loss-minimizing raw
/// component so importing it into the local calendar does not discard fields
/// unknown to the portable UI.
pub fn parse_invitation(raw: &str) -> Option<ItipInvitation> {
    let method = parse_method(raw);
    if method == ItipMethod::Unknown {
        return None;
    }

    let mut snapshot = CalendarSnapshot::default();
    crate::services::calendar_store_service::parse_ical_component(raw, "", "", &mut snapshot);
    let event = snapshot.events.into_iter().next()?;
    let organizer_prop = top_level_property(raw, "ORGANIZER");
    let organizer_person = organizer_prop
        .as_ref()
        .map(|(prop, val)| parse_ical_person(&format!("{prop}:{val}")));
    let organizer = organizer_prop
        .map(|(_, val)| unescape_ical_text(&val))
        .unwrap_or_default();

    let attendee_props = top_level_properties(raw, "ATTENDEE");
    let attendee_persons: Vec<ItipPerson> = attendee_props
        .iter()
        .map(|(prop, val)| parse_ical_person(&format!("{prop}:{val}")))
        .collect();
    let attendees: Vec<String> = attendee_props
        .into_iter()
        .map(|(_, val)| unescape_ical_text(&val))
        .collect();
    let sequence = top_level_property(raw, "SEQUENCE")
        .and_then(|(_, value)| unescape_ical_text(&value).parse::<u32>().ok());
    let recurrence =
        top_level_property(raw, "RRULE").map(|(_, value)| unescape_ical_text(&value));
    Some(ItipInvitation {
        method,
        event,
        organizer,
        attendees,
        organizer_person,
        attendee_persons,
        sequence,
        recurrence,
        raw_component: raw.to_string(),
    })
}

fn parse_method(raw: &str) -> ItipMethod {
    match ical_property(raw, "METHOD")
        .unwrap_or_default()
        .to_ascii_uppercase()
        .as_str()
    {
        "REQUEST" => ItipMethod::Request,
        "REPLY" => ItipMethod::Reply,
        "CANCEL" => ItipMethod::Cancel,
        "PUBLISH" => ItipMethod::Publish,
        "ADD" => ItipMethod::Add,
        _ => ItipMethod::Unknown,
    }
}

fn ical_property(raw: &str, target: &str) -> Option<String> {
    unfold_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .eq_ignore_ascii_case(target);
        name.then(|| unescape_ical_text(value))
    })
}

fn ical_properties(raw: &str, target: &str) -> Vec<String> {
    unfold_lines(raw)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            property
                .split(';')
                .next()
                .unwrap_or(property)
                .eq_ignore_ascii_case(target)
                .then(|| unescape_ical_text(value))
        })
        .collect()
}

fn ical_property_line(raw: &str, target: &str) -> Option<String> {
    unfold_lines(raw).into_iter().find(|line| {
        if let Some((property, _)) = line.split_once(':') {
            let name = property.split(';').next().unwrap_or(property);
            if name.eq_ignore_ascii_case(target) {
                return true;
            }
        }
        false
    })
}

fn ical_property_lines(raw: &str, target: &str) -> Vec<String> {
    unfold_lines(raw)
        .into_iter()
        .filter(|line| {
            if let Some((property, _)) = line.split_once(':') {
                let name = property.split(';').next().unwrap_or(property);
                if name.eq_ignore_ascii_case(target) {
                    return true;
                }
            }
            false
        })
        .collect()
}

pub fn unfold_lines(raw: &str) -> Vec<String> {
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

pub fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn top_level_property(raw: &str, target: &str) -> Option<(String, String)> {
    let mut component_depth = 0usize;
    for line in unfold_lines(raw) {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        if property_name == "BEGIN" {
            if value.eq_ignore_ascii_case("VEVENT") {
                component_depth = 1;
            } else if component_depth > 0 {
                component_depth += 1;
            }
            continue;
        }
        if property_name == "END" {
            if component_depth > 0 {
                component_depth = component_depth.saturating_sub(1);
            }
            continue;
        }
        if component_depth == 1 && property_name == target.to_ascii_uppercase() {
            return Some((property.to_string(), value.to_string()));
        }
    }
    None
}

fn reply_attendee_line(raw: &str, attendee: &str, status: ItipReplyStatus) -> String {
    let existing = top_level_properties(raw, "ATTENDEE")
        .into_iter()
        .find(|(_, value)| {
            normalize_attendee_address(&unescape_ical_text(value)) == Some(attendee.to_string())
        });
    let prefix = existing
        .as_ref()
        .map(|(property, _)| {
            let mut parameters = property
                .split(';')
                .skip(1)
                .filter(|parameter| {
                    !parameter
                        .split_once('=')
                        .map(|(name, _)| name.eq_ignore_ascii_case("PARTSTAT"))
                        .unwrap_or(false)
                })
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            parameters.push(format!("PARTSTAT={}", status.as_ical()));
            if parameters.is_empty() {
                "ATTENDEE".to_string()
            } else {
                format!("ATTENDEE;{}", parameters.join(";"))
            }
        })
        .unwrap_or_else(|| format!("ATTENDEE;PARTSTAT={}", status.as_ical()));
    let value = existing
        .map(|(_, value)| value)
        .unwrap_or_else(|| attendee.to_string());
    format!("{prefix}:{value}")
}

fn top_level_properties(raw: &str, target: &str) -> Vec<(String, String)> {
    let mut properties = Vec::new();
    let mut component_depth = 0usize;
    for line in unfold_lines(raw) {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        if property_name == "BEGIN" {
            if value.eq_ignore_ascii_case("VEVENT") {
                component_depth = 1;
            } else if component_depth > 0 {
                component_depth += 1;
            }
        } else if property_name == "END" {
            if component_depth > 0 {
                component_depth = component_depth.saturating_sub(1);
            }
        } else if component_depth == 1 && property_name == target.to_ascii_uppercase() {
            properties.push((property.to_string(), value.to_string()));
        }
    }
    properties
}

/// Formats an RFC 5545 RRULE into a human-readable summary string.
pub fn format_recurrence_summary(rrule: &str) -> String {
    let mut parts = Vec::new();
    let mut freq = String::new();
    let mut interval = 1u32;
    let mut by_days = Vec::new();
    let mut count = None;
    let mut until = None;

    for param in rrule.split(';') {
        let (k, v) = match param.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_uppercase(), v.trim()),
            None => continue,
        };
        match k.as_str() {
            "FREQ" => freq = v.to_ascii_uppercase(),
            "INTERVAL" => interval = v.parse::<u32>().unwrap_or(1),
            "BYDAY" => {
                for day in v.split(',') {
                    let day_upper = day.trim().to_ascii_uppercase();
                    let d = match day_upper.as_str() {
                        "MO" => "Mon".to_string(),
                        "TU" => "Tue".to_string(),
                        "WE" => "Wed".to_string(),
                        "TH" => "Thu".to_string(),
                        "FR" => "Fri".to_string(),
                        "SA" => "Sat".to_string(),
                        "SU" => "Sun".to_string(),
                        other => other.to_string(),
                    };
                    by_days.push(d);
                }
            }
            "COUNT" => count = Some(v.to_string()),
            "UNTIL" => until = Some(v.to_string()),
            _ => {}
        }
    }

    match freq.as_str() {
        "DAILY" => {
            if interval == 1 {
                parts.push("Daily".to_string());
            } else {
                parts.push(format!("Every {interval} days"));
            }
        }
        "WEEKLY" => {
            if interval == 1 {
                parts.push("Weekly".to_string());
            } else {
                parts.push(format!("Every {interval} weeks"));
            }
            if !by_days.is_empty() {
                parts.push(format!("on {}", by_days.join(", ")));
            }
        }
        "MONTHLY" => {
            if interval == 1 {
                parts.push("Monthly".to_string());
            } else {
                parts.push(format!("Every {interval} months"));
            }
        }
        "YEARLY" => {
            if interval == 1 {
                parts.push("Yearly".to_string());
            } else {
                parts.push(format!("Every {interval} years"));
            }
        }
        _ => {
            if !rrule.trim().is_empty() {
                parts.push(format!("Recurrence ({rrule})"));
            }
        }
    }

    if let Some(c) = count {
        parts.push(format!("({c} times)"));
    } else if let Some(u) = until {
        parts.push(format!("(until {u})"));
    }

    if parts.is_empty() {
        rrule.to_string()
    } else {
        parts.join(" ")
    }
}

/// Extract the user's participation status (PARTSTAT) for a given attendee address from an iCalendar component.
pub fn attendee_partstat(raw: &str, attendee_email: &str) -> Option<ItipReplyStatus> {
    let target = normalize_attendee_address(attendee_email)?;
    for (prop, val) in top_level_properties(raw, "ATTENDEE") {
        if normalize_attendee_address(&unescape_ical_text(&val)) == Some(target.clone()) {
            for param in prop.split(';').skip(1) {
                if let Some((k, v)) = param.split_once('=') {
                    if k.trim().eq_ignore_ascii_case("PARTSTAT") {
                        match v.trim().to_ascii_uppercase().as_str() {
                            "ACCEPTED" => return Some(ItipReplyStatus::Accepted),
                            "TENTATIVE" => return Some(ItipReplyStatus::Tentative),
                            "DECLINED" => return Some(ItipReplyStatus::Declined),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    None
}

/// Find the user's status across any matching candidate email addresses (e.g. configured account profiles).
pub fn find_user_status(raw: &str, candidate_emails: &[&str]) -> Option<ItipReplyStatus> {
    for email in candidate_emails {
        if let Some(status) = attendee_partstat(raw, email) {
            return Some(status);
        }
    }
    None
}

/// Update or insert an ATTENDEE line with the specified PARTSTAT in a VEVENT component.
pub fn update_attendee_partstat(
    component: &str,
    attendee_email: &str,
    status: ItipReplyStatus,
) -> String {
    let target = normalize_attendee_address(attendee_email).unwrap_or_else(|| {
        if attendee_email.starts_with("mailto:") {
            attendee_email.to_string()
        } else {
            format!("mailto:{attendee_email}")
        }
    });

    let mut lines = Vec::new();
    let mut attendee_updated = false;

    for line in unfold_lines(component) {
        let Some((prop_with_params, val)) = line.split_once(':') else {
            lines.push(line);
            continue;
        };

        let prop_name = prop_with_params
            .split(';')
            .next()
            .unwrap_or(prop_with_params)
            .trim();

        if prop_name.eq_ignore_ascii_case("ATTENDEE") {
            let normalized_val = normalize_attendee_address(&unescape_ical_text(val));
            if normalized_val == Some(target.clone()) {
                let mut params = prop_with_params
                    .split(';')
                    .skip(1)
                    .filter(|p| {
                        !p.split_once('=')
                            .map(|(k, _)| k.trim().eq_ignore_ascii_case("PARTSTAT"))
                            .unwrap_or(false)
                    })
                    .map(|p| p.trim().to_string())
                    .collect::<Vec<_>>();
                params.push(format!("PARTSTAT={}", status.as_ical()));
                lines.push(format!("ATTENDEE;{}:{val}", params.join(";")));
                attendee_updated = true;
                continue;
            }
        } else if prop_name.eq_ignore_ascii_case("END") && val.trim().eq_ignore_ascii_case("VEVENT") {
            if !attendee_updated {
                lines.push(format!("ATTENDEE;PARTSTAT={}:{target}", status.as_ical()));
                attendee_updated = true;
            }
        }

        lines.push(line);
    }

    format!("{}\r\n", lines.join("\r\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_and_preserves_event_component() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\nUID:meeting-1\r\nSUMMARY:Team Sync\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T130000\r\n",
            "ORGANIZER:mailto:host@example.test\r\n",
            "ATTENDEE;CN=Person:mailto:person@example.test\r\n",
            "SEQUENCE:3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let invitation = parse_invitation(raw).expect("request should parse");
        assert_eq!(invitation.method, ItipMethod::Request);
        assert_eq!(invitation.event.uid, "meeting-1");
        assert_eq!(invitation.event.summary, "Team Sync");
        assert_eq!(invitation.organizer, "mailto:host@example.test");
        assert_eq!(invitation.attendees, vec!["mailto:person@example.test"]);
        assert_eq!(invitation.sequence, Some(3));
        assert!(
            invitation
                .event
                .component
                .contains("DTSTART;TZID=Europe/Berlin:20260810T120000")
        );
    }

    #[test]
    fn ignores_calendar_payload_without_itip_method_or_event() {
        assert!(parse_invitation("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_none());
        assert!(
            parse_invitation("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n").is_none()
        );
        assert!(
            parse_invitation("BEGIN:VCALENDAR\r\nMETHOD:UNKNOWN\r\nEND:VCALENDAR\r\n").is_none()
        );
    }

    #[test]
    fn accepts_cancel_and_publish_methods() {
        for method in ["CANCEL", "PUBLISH", "REPLY", "ADD"] {
            let raw = format!(
                "BEGIN:VCALENDAR\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:X\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            assert!(parse_invitation(&raw).is_some(), "{method} should parse");
        }
    }

    #[test]
    fn builds_reply_with_attendee_partstat_and_original_timezone() {
        let invitation = parse_invitation(concat!(
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\nUID:meeting-1\r\nDTSTAMP:20260801T090000Z\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T130000\r\n",
            "ORGANIZER:mailto:host@example.test\r\n",
            "ATTENDEE;CN=Person:mailto:person@example.test\r\n",
            "END:VEVENT\r\nEND:VCALENDAR\r\n"
        ))
        .expect("invitation should parse");
        let reply = build_reply_component(
            &invitation,
            "Person <person@example.test>",
            ItipReplyStatus::Accepted,
            Some("Will be 5 minutes late"),
        )
        .expect("reply should build");
        assert!(reply.contains("METHOD:REPLY\r\n"));
        assert!(reply.contains("UID:meeting-1\r\n"));
        assert!(reply.contains("DTSTART;TZID=Europe/Berlin:20260810T120000\r\n"));
        assert!(reply.contains("ATTENDEE;CN=Person;PARTSTAT=ACCEPTED:mailto:person@example.test"));
        assert!(reply.contains("COMMENT:Will be 5 minutes late\r\n"));
    }

    #[test]
    fn reply_requires_a_valid_local_attendee() {
        let invitation = parse_invitation(
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .expect("invitation should parse");
        assert!(
            build_reply_component(
                &invitation,
                "not-an-address",
                ItipReplyStatus::Declined,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn formats_recurrence_summary_properly() {
        assert_eq!(
            format_recurrence_summary("FREQ=WEEKLY;BYDAY=MO,WE,FR"),
            "Weekly on Mon, Wed, Fri"
        );
        assert_eq!(
            format_recurrence_summary("FREQ=DAILY;INTERVAL=2;COUNT=5"),
            "Every 2 days (5 times)"
        );
        assert_eq!(
            format_recurrence_summary("FREQ=MONTHLY;UNTIL=20261231T235959Z"),
            "Monthly (until 20261231T235959Z)"
        );
        assert_eq!(
            format_recurrence_summary("FREQ=YEARLY"),
            "Yearly"
        );
    }

    #[test]
    fn updates_and_reads_attendee_partstat() {
        let component = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:event-123\r\n",
            "SUMMARY:Sync\r\n",
            "ATTENDEE;CN=Host:mailto:host@example.org\r\n",
            "ATTENDEE;CN=Guest;PARTSTAT=NEEDS-ACTION:mailto:guest@example.org\r\n",
            "END:VEVENT\r\n"
        );

        assert_eq!(
            attendee_partstat(component, "guest@example.org"),
            None
        );

        let updated = update_attendee_partstat(component, "guest@example.org", ItipReplyStatus::Accepted);
        assert_eq!(
            attendee_partstat(&updated, "guest@example.org"),
            Some(ItipReplyStatus::Accepted)
        );
        assert!(updated.contains("PARTSTAT=ACCEPTED"));

        let candidate_emails = ["other@example.org", "guest@example.org"];
        assert_eq!(
            find_user_status(&updated, &candidate_emails),
            Some(ItipReplyStatus::Accepted)
        );

        let updated_declined = update_attendee_partstat(&updated, "guest@example.org", ItipReplyStatus::Declined);
        assert_eq!(
            attendee_partstat(&updated_declined, "guest@example.org"),
            Some(ItipReplyStatus::Declined)
        );

        // Also test inserting a new attendee if not in list
        let updated_new = update_attendee_partstat(component, "newuser@example.org", ItipReplyStatus::Tentative);
        assert_eq!(
            attendee_partstat(&updated_new, "newuser@example.org"),
            Some(ItipReplyStatus::Tentative)
        );
    }

    #[test]
    fn test_parse_ical_person_and_organizer_attendees() {
        let p1 = parse_ical_person("ORGANIZER;CN=\"Gregory Davis\":MAILTO:gregory@acme-corp.example");
        assert_eq!(p1.name.as_deref(), Some("Gregory Davis"));
        assert_eq!(p1.email, "gregory@acme-corp.example");
        assert_eq!(p1.display_label(), "Gregory Davis");
        assert_eq!(p1.full_display(), "Gregory Davis <gregory@acme-corp.example>");

        let p2 = parse_ical_person("ATTENDEE;ROLE=REQ-PARTICIPANT;CN=Max Jensen;RSVP=TRUE:mailto:MJensen@acme-corp.example");
        assert_eq!(p2.name.as_deref(), Some("Max Jensen"));
        assert_eq!(p2.email, "MJensen@acme-corp.example");
        assert_eq!(p2.display_label(), "Max Jensen");

        let p3 = parse_ical_person("ORGANIZER:MAILTO:noreply@example.com");
        assert_eq!(p3.name, None);
        assert_eq!(p3.email, "noreply@example.com");
        assert_eq!(p3.display_label(), "noreply@example.com");

        let raw = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\nUID:meeting-custom\r\nSUMMARY:Team Event\r\n",
            "DTSTART;TZID=Europe/Berlin:20260905T180000\r\n",
            "DTEND;TZID=Europe/Berlin:20260905T230000\r\n",
            "ORGANIZER;CN=\"Gregory Davis\":MAILTO:gregory@acme-corp.example\r\n",
            "ATTENDEE;CN=Max Jensen:mailto:MJensen@acme-corp.example\r\n",
            "ATTENDEE;CN=Max Mustermann:mailto:max@example.com\r\n",
            "SEQUENCE:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let invitation = parse_invitation(raw).expect("should parse");
        let org = invitation.organizer_person.expect("organizer person");
        assert_eq!(org.name.as_deref(), Some("Gregory Davis"));
        assert_eq!(org.email, "gregory@acme-corp.example");
        assert_eq!(invitation.attendee_persons.len(), 2);
        assert_eq!(invitation.attendee_persons[0].display_label(), "Max Jensen");
        assert_eq!(invitation.attendee_persons[1].display_label(), "Max Mustermann");
    }

    #[test]
    fn test_vtimezone_rrule_does_not_leak_into_vevent_recurrence() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "VERSION:2.0\r\n",
            "BEGIN:VTIMEZONE\r\n",
            "TZID:W. Europe Standard Time\r\n",
            "BEGIN:STANDARD\r\n",
            "DTSTART:16010101T030000\r\n",
            "TZOFFSETFROM:+0200\r\n",
            "TZOFFSETTO:+0100\r\n",
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\n",
            "END:STANDARD\r\n",
            "BEGIN:DAYLIGHT\r\n",
            "DTSTART:16010101T020000\r\n",
            "TZOFFSETFROM:+0100\r\n",
            "TZOFFSETTO:+0200\r\n",
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\n",
            "END:DAYLIGHT\r\n",
            "END:VTIMEZONE\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:single-teams-meeting\r\n",
            "SUMMARY:Teams Meeting\r\n",
            "DTSTART;TZID=\"W. Europe Standard Time\":20261005T153000\r\n",
            "DTEND;TZID=\"W. Europe Standard Time\":20261005T160000\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n"
        );
        let invitation = parse_invitation(raw).expect("should parse invitation with VTIMEZONE");
        assert_eq!(
            invitation.recurrence, None,
            "VTIMEZONE DST RRULE must not leak into VEVENT recurrence"
        );
    }
}

