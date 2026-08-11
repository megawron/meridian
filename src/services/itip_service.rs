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
            Self::Accepted => "Zusagen",
            Self::Tentative => "Mit Vorbehalt",
            Self::Declined => "Ablehnen",
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
pub struct ItipInvitation {
    pub method: ItipMethod,
    pub event: CalendarEventRecord,
    pub organizer: String,
    pub attendees: Vec<String>,
    pub sequence: Option<u32>,
}

/// Build a standards-shaped iTIP REPLY without contacting an account
/// provider.  The result is ready to become a `text/calendar; method=REPLY`
/// MIME part or a portable response draft.  Camel remains responsible for
/// transport and server-side attendee state on Linux.
pub fn build_reply_component(
    invitation: &ItipInvitation,
    attendee: &str,
    status: ItipReplyStatus,
) -> Result<String, String> {
    if !matches!(invitation.method, ItipMethod::Request | ItipMethod::Add) {
        return Err("Für diese iTIP-Nachricht ist keine Antwort vorgesehen".to_string());
    }
    let attendee = normalize_attendee_address(attendee).ok_or_else(|| {
        "Für die Antwort wird eine gültige eigene E-Mail-Adresse benötigt".to_string()
    })?;
    let uid = top_level_property(&invitation.event.component, "UID")
        .map(|(_, value)| value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| invitation.event.uid.clone());
    if uid.trim().is_empty() {
        return Err("Die Einladung besitzt keine UID".to_string());
    }

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Evolution GTK4//iTIP Reply//EN".to_string(),
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
    let value = value.strip_prefix("mailto:").unwrap_or(value).trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) || !value.contains('@') {
        return None;
    }
    Some(format!("mailto:{value}"))
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
    let organizer = ical_property(raw, "ORGANIZER").unwrap_or_default();
    let attendees = ical_properties(raw, "ATTENDEE");
    let sequence = ical_property(raw, "SEQUENCE").and_then(|value| value.parse::<u32>().ok());
    Some(ItipInvitation {
        method,
        event,
        organizer,
        attendees,
        sequence,
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

fn unescape_ical_text(value: &str) -> String {
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
        )
        .expect("reply should build");
        assert!(reply.contains("METHOD:REPLY\r\n"));
        assert!(reply.contains("UID:meeting-1\r\n"));
        assert!(reply.contains("DTSTART;TZID=Europe/Berlin:20260810T120000\r\n"));
        assert!(reply.contains("ATTENDEE;CN=Person;PARTSTAT=ACCEPTED:mailto:person@example.test"));
    }

    #[test]
    fn reply_requires_a_valid_local_attendee() {
        let invitation = parse_invitation(
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .expect("invitation should parse");
        assert!(
            build_reply_component(&invitation, "not-an-address", ItipReplyStatus::Declined)
                .is_err()
        );
    }
}
