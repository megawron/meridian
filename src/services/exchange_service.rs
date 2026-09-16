// SPDX-License-Identifier: GPL-3.0-or-later
//
// exchange_service — Microsoft Graph transport for Exchange/Microsoft 365.
//
// The native Linux EDS/EWS adapter remains authoritative when an EDS source is
// available.  This service is the portable Graph path used by the account
// fallback and by tests.  Access tokens are injected by the caller and are
// intentionally never serialized, logged, or placed in an AccountProfile.

use chrono::Datelike;
use reqwest::Client;
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::mail_metadata_service::{MailFollowUpMetadata, MailFollowUpState};
use super::out_of_office_service::OutOfOfficeSettings;

pub use super::mail_metadata_service::MailFollowUpState as ExchangeMailFollowUpState;

const GRAPH_DEFAULT_URL: &str = "https://graph.microsoft.com/v1.0";
const GRAPH_USER_AGENT: &str = "EvolutionGTK4/0.1";
const MAX_QUERY_LENGTH: usize = 256;
const MAX_GRAPH_ITEMS: usize = 5_000;
const MAX_GRAPH_PAGES: usize = 256;
const GRAPH_PAGE_SIZE: usize = 1_000;
const MAX_GAL_RESULTS: usize = 250;
const MAX_GRAPH_SEARCH_ITEMS: usize = 1_000;
const GRAPH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const GRAPH_CANCELLATION_POLL: Duration = Duration::from_millis(10);
const GRAPH_CANCELLATION_ACK_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalContact {
    pub display_name: String,
    pub email_address: String,
    pub job_title: String,
    pub department: String,
    pub office_location: String,
}

/// A portable address used by Graph mail, calendar and contact resources.
///
/// The native EDS/Camel/EBook adapters keep their provider-specific objects;
/// this small value type is the common boundary for the Graph fallback and is
/// deliberately free of GTK and credential state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeEmailAddress {
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeDateTime {
    pub date_time: String,
    pub time_zone: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeMailFolder {
    pub id: String,
    pub display_name: String,
    pub parent_folder_id: Option<String>,
    pub child_folder_count: usize,
    pub total_item_count: usize,
    pub unread_item_count: usize,
}

fn follow_up_state_from_graph_flag_status(status: &str) -> MailFollowUpState {
    if status.eq_ignore_ascii_case("flagged") {
        MailFollowUpState::FollowUp
    } else if status.eq_ignore_ascii_case("complete") {
        MailFollowUpState::Completed
    } else {
        // Graph currently documents notFlagged, flagged and complete. Treat
        // an omitted or future status conservatively as no active follow-up.
        MailFollowUpState::None
    }
}

const fn graph_flag_status_for_follow_up_state(state: MailFollowUpState) -> &'static str {
    match state {
        MailFollowUpState::None => "notFlagged",
        MailFollowUpState::FollowUp => "flagged",
        MailFollowUpState::Completed => "complete",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeMailMessage {
    pub id: String,
    pub folder_id: String,
    pub internet_message_id: Option<String>,
    pub subject: String,
    pub sender: ExchangeEmailAddress,
    pub from: ExchangeEmailAddress,
    pub to_recipients: Vec<ExchangeEmailAddress>,
    pub cc_recipients: Vec<ExchangeEmailAddress>,
    pub bcc_recipients: Vec<ExchangeEmailAddress>,
    pub reply_to: Vec<ExchangeEmailAddress>,
    pub received_date_time: String,
    pub sent_date_time: String,
    #[serde(default)]
    pub size: usize,
    pub body_preview: String,
    pub body_html: String,
    pub body_text: String,
    pub has_attachments: bool,
    pub is_read: bool,
    pub is_flagged: bool,
    #[serde(default)]
    pub follow_up_state: MailFollowUpState,
    pub is_important: bool,
    pub categories: Vec<String>,
    #[serde(default)]
    pub internet_message_headers: Vec<(String, String)>,
    pub attachments: Vec<ExchangeMailAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeMailAttachment {
    pub name: String,
    pub content_type: String,
    pub size: usize,
    pub content_id: Option<String>,
    pub is_inline: bool,
    pub content: Vec<u8>,
}

/// Result of one bounded Graph mail search.
///
/// Graph may return a continuation link even after the caller's result limit
/// has been reached.  Keeping that fact beside the messages prevents callers
/// from presenting a truncated result set as a complete search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeMailSearchResult {
    pub messages: Vec<ExchangeMailMessage>,
    pub is_complete: bool,
    /// The opaque, origin-validated Graph continuation URL for the next
    /// search page, if Graph reported a safe continuation.  Callers must pass
    /// this value back unchanged to a cursor-aware search request; query and
    /// folder arguments are intentionally not re-applied when following it.
    pub next_cursor: Option<String>,
}

impl ExchangeMailMessage {
    /// Convert a Graph message into the same parser output used by local
    /// maildir/mbox and native Camel.  This keeps search, threading, labels
    /// and the existing mail list independent from the remote transport.
    pub(crate) fn into_parsed_mail(
        self,
        source_uid: &str,
        folder_name: &str,
    ) -> crate::services::mail_store_service::ParsedMail {
        let raw_headers = graph_header_block(&self.internet_message_headers);
        let sender = if self.sender.address.trim().is_empty() {
            self.from.clone()
        } else {
            self.sender.clone()
        };
        let body_html = if self.body_html.trim().is_empty() {
            format!("<pre>{}</pre>", escape_html(self.body_text.trim()))
        } else {
            let sanitized = crate::services::html_sanitizer::sanitize_html(&self.body_html);
            if strip_html(&sanitized).trim().is_empty() && !self.body_text.trim().is_empty() {
                format!("<pre>{}</pre>", escape_html(self.body_text.trim()))
            } else {
                sanitized
            }
        };
        let mut preview = if !self.body_preview.trim().is_empty() {
            crate::services::html_sanitizer::clean_mail_snippet(&self.body_preview)
        } else {
            String::new()
        };
        if preview.is_empty() && !self.body_text.trim().is_empty() {
            preview = crate::services::html_sanitizer::clean_mail_snippet(&self.body_text);
        }
        if preview.is_empty() && !self.body_html.trim().is_empty() {
            preview = crate::services::html_sanitizer::clean_mail_snippet_from_html(&self.body_html);
        }
        let message_id = self.internet_message_id.clone();
        let folder_id = normalized_mail_folder_id(folder_name);
        crate::services::mail_store_service::ParsedMail {
            id: self.id.clone(),
            folder_id: folder_id.clone(),
            folder_name: folder_name.to_string(),
            backend_location: Some(crate::services::mail_store_service::MailBackendLocation {
                source_uid: source_uid.to_string(),
                folder_name: folder_name.to_string(),
                message_uid: self.id,
                message_id: message_id.clone(),
            }),
            sender_name: sender.name,
            sender_email: sender.address,
            recipient_to: format_addresses(&self.to_recipients),
            recipient_cc: format_addresses(&self.cc_recipients),
            recipient_bcc: format_addresses(&self.bcc_recipients),
            reply_to: format_addresses(&self.reply_to),
            subject: if self.subject.trim().is_empty() {
                "(Kein Betreff)".to_string()
            } else {
                self.subject
            },
            message_id,
            in_reply_to: Vec::new(),
            references: Vec::new(),
            thread_id: String::new(),
            thread_size: 1,
            thread_position: 0,
            date_str: if self.received_date_time.trim().is_empty() {
                self.sent_date_time
            } else {
                self.received_date_time
            },
            message_size: self.size,
            preview_snippet: preview,
            html_body: body_html,
            raw_headers,
            raw_mime: None,
            is_unread: !self.is_read,
            is_flagged: self.is_flagged,
            is_important: self.is_important,
            is_replied: false,
            is_forwarded: false,
            follow_up_state: self.follow_up_state,
            follow_up: MailFollowUpMetadata {
                state: self.follow_up_state,
                ..MailFollowUpMetadata::default()
            }
            .normalized(),
            labels: self.categories,
            attachments: self
                .attachments
                .into_iter()
                .map(|attachment| crate::models::mail_message::AttachmentInfo {
                    filename: if attachment.name.trim().is_empty() {
                        "Anhang".to_string()
                    } else {
                        attachment.name
                    },
                    size_formatted: format_attachment_size(attachment.size),
                    mime_type: if attachment.content_type.trim().is_empty() {
                        "application/octet-stream".to_string()
                    } else {
                        attachment.content_type
                    },
                    content_id: attachment.content_id,
                    content: attachment.content.into(),
                })
                .collect(),
            is_preview_only: false,
        }
    }
}

fn graph_header_block(headers: &[(String, String)]) -> Option<Vec<u8>> {
    let mut encoded = Vec::new();
    for (name, value) in headers {
        let name = name.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b':')
        {
            continue;
        }
        let value = value.replace(['\r', '\n'], " ");
        encoded.extend_from_slice(name.as_bytes());
        encoded.extend_from_slice(b": ");
        encoded.extend_from_slice(value.trim().as_bytes());
        encoded.extend_from_slice(b"\r\n");
    }
    (!encoded.is_empty()).then_some(encoded)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangePostalAddress {
    pub street: String,
    pub city: String,
    pub state: String,
    pub country_or_region: String,
    pub postal_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeContact {
    pub id: String,
    pub display_name: String,
    pub given_name: String,
    pub surname: String,
    pub middle_name: String,
    pub nickname: String,
    pub title: String,
    pub company_name: String,
    pub department: String,
    pub job_title: String,
    pub office_location: String,
    pub birthday: String,
    pub personal_notes: String,
    pub email_addresses: Vec<ExchangeEmailAddress>,
    pub business_phones: Vec<String>,
    pub home_phones: Vec<String>,
    pub mobile_phone: String,
    pub business_address: Option<ExchangePostalAddress>,
    pub home_address: Option<ExchangePostalAddress>,
    pub categories: Vec<String>,
}

impl ExchangeContact {
    /// Export Graph's contact shape to the app's existing vCard contract.
    /// The Graph id is retained as an X-property so a later write path can
    /// address the same remote contact without putting credentials in the
    /// local address-book file.
    pub(crate) fn to_vcard(&self) -> String {
        let primary_email = self
            .email_addresses
            .first()
            .map(|email| email.address.clone())
            .unwrap_or_default();
        let mut contact = crate::services::vcard_service::VCardContact::new(
            &self.display_name,
            &primary_email,
            &self.company_name,
            &self.job_title,
        );
        contact.name = crate::services::vcard_service::VCardName {
            family: self.surname.clone(),
            given: self.given_name.clone(),
            additional: self.middle_name.clone(),
            prefix: self.title.clone(),
            suffix: String::new(),
        };
        contact.emails = self
            .email_addresses
            .iter()
            .map(|email| email.address.clone())
            .filter(|email| !email.trim().is_empty())
            .collect();
        contact.email = contact.emails.first().cloned().unwrap_or_default();
        contact.phones = self
            .business_phones
            .iter()
            .chain(self.home_phones.iter())
            .chain(std::iter::once(&self.mobile_phone))
            .filter(|phone| !phone.trim().is_empty())
            .cloned()
            .collect();
        contact.tel = contact.phones.first().cloned().unwrap_or_default();
        contact.nickname = self.nickname.clone();
        contact.birthday = self.birthday.clone();
        contact.note = self.personal_notes.clone();
        contact.categories = self.categories.clone();
        if let Some(address) = self.business_address.as_ref() {
            contact.addresses.push(format_address(address));
        }
        if let Some(address) = self.home_address.as_ref() {
            contact.addresses.push(format_address(address));
        }
        contact
            .extensions
            .push(crate::services::vcard_service::VCardExtension {
                property: "X-EVOLUTION-GRAPH-ID".to_string(),
                value: self.id.clone(),
            });
        contact.to_vcard_string()
    }

    pub(crate) fn from_vcard(
        id: &str,
        contact: &crate::services::vcard_service::VCardContact,
    ) -> Self {
        let email_addresses = if contact.emails.is_empty() {
            (!contact.email.trim().is_empty())
                .then(|| ExchangeEmailAddress {
                    name: contact.fn_name.clone(),
                    address: contact.email.clone(),
                })
                .into_iter()
                .collect()
        } else {
            contact
                .emails
                .iter()
                .filter(|email| !email.trim().is_empty())
                .map(|email| ExchangeEmailAddress {
                    name: contact.fn_name.clone(),
                    address: email.clone(),
                })
                .collect()
        };
        let phones = if contact.phones.is_empty() {
            (!contact.tel.trim().is_empty())
                .then(|| contact.tel.clone())
                .into_iter()
                .collect()
        } else {
            contact.phones.clone()
        };
        Self {
            id: id.to_string(),
            display_name: contact.fn_name.clone(),
            given_name: contact.name.given.clone(),
            surname: contact.name.family.clone(),
            middle_name: contact.name.additional.clone(),
            nickname: contact.nickname.clone(),
            title: contact.name.prefix.clone(),
            company_name: contact.org.clone(),
            department: String::new(),
            job_title: contact.title.clone(),
            office_location: String::new(),
            birthday: contact.birthday.clone(),
            personal_notes: contact.note.clone(),
            email_addresses,
            business_phones: phones,
            home_phones: Vec::new(),
            mobile_phone: String::new(),
            business_address: None,
            home_address: None,
            categories: contact.categories.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeCalendarEvent {
    pub id: String,
    pub subject: String,
    pub body_html: String,
    pub body_text: String,
    pub start: ExchangeDateTime,
    pub end: ExchangeDateTime,
    pub location: String,
    pub is_all_day: bool,
    pub organizer: ExchangeEmailAddress,
    pub attendees: Vec<ExchangeEmailAddress>,
    pub show_as: String,
    pub sensitivity: String,
    pub web_link: String,
    pub is_cancelled: bool,
    pub categories: Vec<String>,
    pub is_recurring: bool,
    /// RFC 5545 recurrence rule reconstructed from Graph's patterned
    /// recurrence object when the server returns one.
    pub recurrence_rule: Option<String>,
}

impl ExchangeCalendarEvent {
    /// Build a standards-shaped VEVENT and let the existing calendar parser
    /// create its `CalendarEventRecord`.  This preserves the component used
    /// by the calendar editor/exporter instead of introducing a second row
    /// model just for Graph.
    pub(crate) fn into_calendar_event(
        &self,
        source_uid: &str,
        source_display_name: &str,
    ) -> Option<crate::services::calendar_store_service::CalendarEventRecord> {
        let mut component = String::from("BEGIN:VEVENT\r\n");
        component.push_str(&format!("UID:{}\r\n", ical_escape(&self.id)));
        component.push_str(&format!("SUMMARY:{}\r\n", ical_escape(&self.subject)));
        component.push_str(&format!(
            "DTSTART{}:{}\r\n",
            if self.is_all_day { ";VALUE=DATE" } else { "" },
            ical_datetime(&self.start.date_time, self.is_all_day)
        ));
        component.push_str(&format!(
            "DTEND{}:{}\r\n",
            if self.is_all_day { ";VALUE=DATE" } else { "" },
            ical_datetime(&self.end.date_time, self.is_all_day)
        ));
        if !self.location.trim().is_empty() {
            component.push_str(&format!("LOCATION:{}\r\n", ical_escape(&self.location)));
        }
        let description = if self.body_text.trim().is_empty() {
            strip_html(&self.body_html)
        } else {
            self.body_text.clone()
        };
        if !description.trim().is_empty() {
            component.push_str(&format!("DESCRIPTION:{}\r\n", ical_escape(&description)));
        }
        if !self.organizer.address.trim().is_empty() {
            component.push_str(&format!(
                "ORGANIZER;CN={}:mailto:{}\r\n",
                ical_escape(&self.organizer.name),
                ical_escape(&self.organizer.address)
            ));
        }
        for attendee in &self.attendees {
            if !attendee.address.trim().is_empty() {
                component.push_str(&format!(
                    "ATTENDEE;CN={}:mailto:{}\r\n",
                    ical_escape(&attendee.name),
                    ical_escape(&attendee.address)
                ));
            }
        }
        if self.is_cancelled {
            component.push_str("STATUS:CANCELLED\r\n");
        }
        if let Some(rule) = self.recurrence_rule.as_deref() {
            // RRULE is already a structured semicolon-separated value; it
            // must not go through the text-property escaping routine.
            component.push_str(&format!("RRULE:{}\r\n", rule));
        } else if self.is_recurring {
            component.push_str("X-EVOLUTION-GRAPH-RECURRING:TRUE\r\n");
        }
        component.push_str("END:VEVENT\r\n");

        let mut snapshot = crate::services::calendar_store_service::CalendarSnapshot::default();
        crate::services::calendar_store_service::parse_ical_component(
            &component,
            source_uid,
            source_display_name,
            &mut snapshot,
        );
        snapshot.events.into_iter().next()
    }
}

fn format_addresses(addresses: &[ExchangeEmailAddress]) -> String {
    addresses
        .iter()
        .filter(|address| !address.address.trim().is_empty())
        .map(|address| {
            if address.name.trim().is_empty() {
                address.address.clone()
            } else {
                format!("{} <{}>", address.name, address.address)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn graph_recipients(addresses: &[ExchangeEmailAddress]) -> Vec<serde_json::Value> {
    addresses
        .iter()
        .filter(|address| !address.address.trim().is_empty())
        .map(|address| {
            serde_json::json!({
                "emailAddress": {
                    "name": address.name,
                    "address": address.address
                }
            })
        })
        .collect()
}

fn graph_contact_email_addresses(addresses: &[ExchangeEmailAddress]) -> Vec<serde_json::Value> {
    addresses
        .iter()
        .filter(|address| !address.address.trim().is_empty())
        .map(|address| {
            serde_json::json!({
                "name": address.name,
                "address": address.address
            })
        })
        .collect()
}

fn contact_payload(contact: &ExchangeContact) -> serde_json::Value {
    serde_json::json!({
        "displayName": contact.display_name,
        "givenName": contact.given_name,
        "surname": contact.surname,
        "middleName": contact.middle_name,
        "nickName": contact.nickname,
        "title": contact.title,
        "companyName": contact.company_name,
        "department": contact.department,
        "jobTitle": contact.job_title,
        "officeLocation": contact.office_location,
        "birthday": contact.birthday,
        "personalNotes": contact.personal_notes,
        "emailAddresses": graph_contact_email_addresses(&contact.email_addresses),
        "businessPhones": contact.business_phones,
        "homePhones": contact.home_phones,
        "mobilePhone": contact.mobile_phone,
        "businessAddress": contact.business_address.as_ref().map(postal_address_payload),
        "homeAddress": contact.home_address.as_ref().map(postal_address_payload),
        "categories": contact.categories
    })
}

fn postal_address_payload(address: &ExchangePostalAddress) -> serde_json::Value {
    serde_json::json!({
        "street": address.street,
        "city": address.city,
        "state": address.state,
        "countryOrRegion": address.country_or_region,
        "postalCode": address.postal_code
    })
}

fn folder_aliases(value: &str) -> Vec<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "inbox" => vec!["Inbox"],
        "sent" | "sent items" | "sent-mail" => vec!["Sent Items", "Sent"],
        "draft" | "drafts" => vec!["Drafts"],
        "trash" | "deleted" | "deleted items" => vec!["Deleted Items", "Trash"],
        "junk" | "spam" | "junk email" => vec!["Junk Email", "Junk", "Spam"],
        "archive" | "all mail" | "all-mail" => vec!["Archive"],
        _ => Vec::new(),
    }
}

#[allow(dead_code)]
#[cfg(test)]
fn parse_address_list(value: &str) -> Vec<ExchangeEmailAddress> {
    value
        .split([',', ';'])
        .filter_map(|raw| {
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            if let Some((name, address)) = raw.rsplit_once('<') {
                let address = address.trim_end_matches('>').trim();
                if address.contains('@') {
                    return Some(ExchangeEmailAddress {
                        name: name.trim().trim_matches('"').to_string(),
                        address: address.to_string(),
                    });
                }
            }
            Some(ExchangeEmailAddress {
                name: String::new(),
                address: raw.to_string(),
            })
        })
        .collect()
}

fn calendar_event_payload(
    event: &crate::services::calendar_store_service::CalendarEventRecord,
) -> serde_json::Value {
    let start = graph_event_datetime_from_ical(&event.start, event.all_day);
    let end = graph_event_datetime_from_ical(&event.end, event.all_day);
    let component = event.component.as_str();
    let attendees = ical_properties(component, "ATTENDEE")
        .into_iter()
        .filter_map(|value| parse_ical_attendee(&value))
        .map(|address| {
            serde_json::json!({
                "emailAddress": {
                    "name": address.name,
                    "address": address.address
                },
                "type": "required"
            })
        })
        .collect::<Vec<_>>();
    let recurrence = ical_property(component, "RRULE")
        .and_then(|rule| graph_recurrence_payload(rule, &event.start));
    serde_json::json!({
        "subject": event.summary,
        "body": {"contentType":"Text", "content": event.description},
        "start": start,
        "end": end,
        "location": {"displayName": event.location},
        "isAllDay": event.all_day,
        "attendees": attendees,
        "recurrence": recurrence
    })
}

fn ical_property(raw: &str, wanted: &str) -> Option<String> {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter_map(|line| line.split_once(':'))
        .find_map(|(property, value)| {
            property
                .split(';')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
                .then(|| value.to_string())
        })
}

fn ical_properties(raw: &str, wanted: &str) -> Vec<String> {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter_map(|line| line.split_once(':'))
        .filter(|(property, _)| {
            property
                .split(';')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
        })
        .map(|(_, value)| value.to_string())
        .collect()
}

fn parse_ical_attendee(value: &str) -> Option<ExchangeEmailAddress> {
    let address = value
        .trim()
        .strip_prefix("mailto:")
        .unwrap_or(value.trim())
        .trim();
    (!address.is_empty()).then(|| ExchangeEmailAddress {
        name: String::new(),
        address: address.to_string(),
    })
}

fn graph_recurrence_payload(rule: String, event_start: &str) -> Option<serde_json::Value> {
    let mut frequency = None;
    let mut interval = 1_u32;
    let mut days = Vec::new();
    let mut count = None;
    let mut until = None;
    for part in rule.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => frequency = Some(value.to_ascii_uppercase()),
            "INTERVAL" => interval = value.parse().unwrap_or(1).max(1),
            "BYDAY" => days.extend(value.split(',').filter_map(graph_weekday_name)),
            "COUNT" => count = value.parse::<u32>().ok().filter(|value| *value > 0),
            "UNTIL" => until = graph_date_only_from_ical(value),
            _ => {}
        }
    }
    let recurrence_pattern_type = match frequency.as_deref()? {
        "DAILY" => "daily",
        "WEEKLY" => "weekly",
        "MONTHLY" => "absoluteMonthly",
        "YEARLY" => "absoluteYearly",
        _ => return None,
    };
    let mut pattern = serde_json::json!({
        "type": recurrence_pattern_type,
        "interval": interval
    });
    match recurrence_pattern_type {
        "weekly" => {
            if days.is_empty() {
                days.push(graph_weekday_from_ical(event_start)?);
            }
            pattern["daysOfWeek"] = serde_json::json!(days);
            pattern["firstDayOfWeek"] = serde_json::json!("monday");
        }
        "absoluteMonthly" => {
            pattern["dayOfMonth"] = serde_json::json!(graph_day_of_month_from_ical(event_start)?);
        }
        "absoluteYearly" => {
            pattern["dayOfMonth"] = serde_json::json!(graph_day_of_month_from_ical(event_start)?);
            pattern["month"] = serde_json::json!(graph_month_from_ical(event_start)?);
        }
        _ => {}
    }
    // Graph requires startDate for every recurrence range.  Without it the
    // otherwise valid pattern is rejected by the event endpoint.
    let start_date = graph_date_only_from_ical(event_start)?;
    let range = if let Some(count) = count {
        serde_json::json!({
            "type":"numbered",
            "numberOfOccurrences":count,
            "startDate":start_date
        })
    } else if let Some(end_date) = until {
        serde_json::json!({"type":"endDate", "endDate":end_date, "startDate":start_date})
    } else {
        serde_json::json!({"type":"noEnd", "startDate":start_date})
    };
    Some(serde_json::json!({"pattern": pattern, "range": range}))
}

fn graph_weekday_name(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_uppercase().as_str() {
        "MO" => Some("monday"),
        "TU" => Some("tuesday"),
        "WE" => Some("wednesday"),
        "TH" => Some("thursday"),
        "FR" => Some("friday"),
        "SA" => Some("saturday"),
        "SU" => Some("sunday"),
        _ => None,
    }
}

fn graph_weekday_from_ical(value: &str) -> Option<&'static str> {
    let digits = value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    if digits.len() < 8 {
        return None;
    }
    let year = digits.get(..4)?.parse::<i32>().ok()?;
    let month = digits.get(4..6)?.parse::<u32>().ok()?;
    let day = digits.get(6..8)?.parse::<u32>().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day).map(|date| match date.weekday() {
        chrono::Weekday::Mon => "monday",
        chrono::Weekday::Tue => "tuesday",
        chrono::Weekday::Wed => "wednesday",
        chrono::Weekday::Thu => "thursday",
        chrono::Weekday::Fri => "friday",
        chrono::Weekday::Sat => "saturday",
        chrono::Weekday::Sun => "sunday",
    })
}

fn graph_date_only_from_ical(value: &str) -> Option<String> {
    let digits = value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    (digits.len() >= 8).then(|| format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..8]))
}

fn graph_day_of_month_from_ical(value: &str) -> Option<u32> {
    let date = graph_date_only_from_ical(value)?;
    date.get(8..)?.parse().ok()
}

fn graph_month_from_ical(value: &str) -> Option<u32> {
    let date = graph_date_only_from_ical(value)?;
    date.get(5..7)?.parse().ok()
}

fn graph_recurrence_rule(value: &serde_json::Value) -> Option<String> {
    let pattern = value.get("pattern")?;
    let range = value.get("range")?;
    let pattern_type = pattern.get("type")?.as_str()?;
    let interval = pattern.get("interval").and_then(|value| value.as_u64());
    let mut parts = Vec::new();
    match pattern_type {
        "daily" => parts.push("FREQ=DAILY".to_string()),
        "weekly" => {
            parts.push("FREQ=WEEKLY".to_string());
            if let Some(days) = pattern.get("daysOfWeek").and_then(|value| value.as_array()) {
                let days = days
                    .iter()
                    .filter_map(|day| day.as_str().and_then(ical_weekday_name))
                    .collect::<Vec<_>>();
                if !days.is_empty() {
                    parts.push(format!("BYDAY={}", days.join(",")));
                }
            }
        }
        "absoluteMonthly" => {
            parts.push("FREQ=MONTHLY".to_string());
            let day = pattern.get("dayOfMonth")?.as_u64()?;
            parts.push(format!("BYMONTHDAY={day}"));
        }
        "absoluteYearly" => {
            parts.push("FREQ=YEARLY".to_string());
            let day = pattern.get("dayOfMonth")?.as_u64()?;
            let month = pattern.get("month")?.as_u64()?;
            parts.push(format!("BYMONTHDAY={day}"));
            parts.push(format!("BYMONTH={month}"));
        }
        _ => return None,
    }
    if let Some(interval) = interval.filter(|value| *value > 1) {
        parts.push(format!("INTERVAL={interval}"));
    }
    match range.get("type").and_then(|value| value.as_str()) {
        Some("numbered") => {
            let count = range.get("numberOfOccurrences")?.as_u64()?;
            parts.push(format!("COUNT={count}"));
        }
        Some("endDate") => {
            let date = range.get("endDate")?.as_str()?;
            parts.push(format!("UNTIL={}", date.replace('-', "")));
        }
        Some("noEnd") | None => {}
        _ => return None,
    }
    Some(parts.join(";"))
}

fn ical_weekday_name(value: &str) -> Option<&'static str> {
    match value {
        "monday" => Some("MO"),
        "tuesday" => Some("TU"),
        "wednesday" => Some("WE"),
        "thursday" => Some("TH"),
        "friday" => Some("FR"),
        "saturday" => Some("SA"),
        "sunday" => Some("SU"),
        _ => None,
    }
}

fn graph_event_datetime_from_ical(value: &str, all_day: bool) -> serde_json::Value {
    let digits = value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    if all_day && digits.len() >= 8 {
        serde_json::json!({"dateTime": format!("{}-{}-{}T00:00:00", &digits[..4], &digits[4..6], &digits[6..8]), "timeZone":"UTC"})
    } else if digits.len() >= 14 {
        serde_json::json!({
            "dateTime": format!("{}-{}-{}T{}:{}:{}", &digits[..4], &digits[4..6], &digits[6..8], &digits[8..10], &digits[10..12], &digits[12..14]),
            "timeZone":"UTC"
        })
    } else {
        serde_json::json!({"dateTime": value, "timeZone":"UTC"})
    }
}

fn format_attachment_size(size: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn normalized_mail_folder_id(folder_name: &str) -> String {
    match folder_name.trim().to_ascii_lowercase().as_str() {
        "inbox" => "inbox".to_string(),
        "sent" | "sent items" | "sent-mail" => "sent".to_string(),
        "draft" | "drafts" => "drafts".to_string(),
        "trash" | "deleted" | "deleted items" => "trash".to_string(),
        "junk" | "spam" | "junk email" => "junk".to_string(),
        "archive" | "all mail" | "all-mail" => "archive".to_string(),
        other => format!("custom:{other}"),
    }
}

fn format_address(address: &ExchangePostalAddress) -> String {
    [
        address.street.as_str(),
        address.city.as_str(),
        address.state.as_str(),
        address.country_or_region.as_str(),
        address.postal_code.as_str(),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join(", ")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(
            TABLE[(((first & 0x03) << 4) | second.map_or(0, |value| value >> 4)) as usize] as char,
        );
        encoded.push(match second {
            Some(value) => {
                TABLE[(((value & 0x0f) << 2) | third.map_or(0, |item| item >> 6)) as usize] as char
            }
            None => '=',
        });
        encoded.push(match third {
            Some(value) => TABLE[(value & 0x3f) as usize] as char,
            None => '=',
        });
    }
    encoded
}

fn strip_html(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                plain.push(' ');
            }
            _ if !in_tag => plain.push(character),
            _ => {}
        }
    }
    plain
}

fn ical_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn ical_datetime(value: &str, all_day: bool) -> String {
    let value = value.trim();
    let digits = value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    if all_day {
        if digits.len() >= 8 {
            return digits[..8].to_string();
        }
        return value.to_string();
    }
    if digits.len() >= 14 {
        let mut result = format!(
            "{}{}{}T{}{}{}",
            &digits[..4],
            &digits[4..6],
            &digits[6..8],
            &digits[8..10],
            &digits[10..12],
            &digits[12..14]
        );
        if value.ends_with('Z') {
            result.push('Z');
        }
        result
    } else {
        value.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ExchangeAuthStatus {
    Authenticated,
    TokenExpired,
    NeedOAuthConsent,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    InvalidUrl(String),
    InvalidQuery(String),
    MissingAccessToken,
    Cancelled,
    Transport(String),
    Http {
        status: u16,
        code: Option<String>,
        message: String,
    },
    Decode(String),
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl(message) => {
                write!(formatter, "Ungültige Microsoft-Graph-URL: {message}")
            }
            Self::InvalidQuery(message) => write!(formatter, "Ungültige Graph-Suche: {message}"),
            Self::MissingAccessToken => {
                write!(
                    formatter,
                    "Microsoft-Graph-Zugriff benötigt ein gültiges OAuth2-Token"
                )
            }
            Self::Cancelled => write!(formatter, "Microsoft-Graph-Anfrage wurde abgebrochen"),
            Self::Transport(message) => {
                write!(formatter, "Microsoft-Graph-Netzwerkfehler: {message}")
            }
            Self::Http {
                status,
                code,
                message,
            } => {
                if let Some(code) = code {
                    write!(
                        formatter,
                        "Microsoft-Graph-Fehler {status} ({code}): {message}"
                    )
                } else {
                    write!(formatter, "Microsoft-Graph-Fehler {status}: {message}")
                }
            }
            Self::Decode(message) => {
                write!(formatter, "Ungültige Microsoft-Graph-Antwort: {message}")
            }
        }
    }
}

impl std::error::Error for ExchangeError {}

/// Cooperative cancellation shared by a synchronous Graph operation and its
/// owner.  The token is one-shot: create one token per operation or service
/// clone.  Cancelling it drops the async HTTP future even while Graph is still
/// waiting for headers or response bytes; it does not retry the request.
#[derive(Clone, Debug)]
struct ExchangeCancellationToken {
    requested: Arc<AtomicBool>,
}

impl Default for ExchangeCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl ExchangeCancellationToken {
    fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Adopt the existing cancellation representation used by the mail and
    /// Camel workers.  No flag is copied, so a later store by the owner is
    /// observed by the Graph transport immediately.
    fn from_flag(requested: Arc<AtomicBool>) -> Self {
        Self { requested }
    }

    fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeOutOfOffice {
    pub enabled: bool,
    pub use_schedule: bool,
    pub start_date: String,
    pub end_date: String,
    pub internal_message: String,
    pub external_message: String,
}

impl From<ExchangeOutOfOffice> for OutOfOfficeSettings {
    fn from(value: ExchangeOutOfOffice) -> Self {
        Self {
            enabled: value.enabled,
            use_schedule: value.use_schedule,
            start_date: value.start_date,
            end_date: value.end_date,
            internal_message: value.internal_message,
            external_message: value.external_message,
        }
    }
}

enum GraphHttpBody {
    Json(serde_json::Value),
    Raw(Vec<u8>),
}

struct GraphHttpRequest {
    method: Method,
    url: Url,
    params: Vec<(String, String)>,
    body: Option<GraphHttpBody>,
    accept: &'static str,
}

struct GraphHttpResponse {
    status: StatusCode,
    body: Vec<u8>,
}

#[derive(Clone)]
pub struct ExchangeService {
    pub server_url: String,
    pub username: String,
    pub auth_status: ExchangeAuthStatus,
    access_token: Option<String>,
    client: Client,
    cancellation: ExchangeCancellationToken,
}

impl std::fmt::Debug for ExchangeService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExchangeService")
            .field("server_url", &self.server_url)
            .field("username", &self.username)
            .field("auth_status", &self.auth_status)
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .finish_non_exhaustive()
    }
}

impl ExchangeService {
    pub fn new(server_url: &str, username: &str) -> Self {
        Self {
            server_url: if server_url.trim().is_empty() {
                GRAPH_DEFAULT_URL.to_string()
            } else {
                server_url.trim().trim_end_matches('/').to_string()
            },
            username: username.trim().to_string(),
            auth_status: ExchangeAuthStatus::Disconnected,
            access_token: None,
            client: Client::builder()
                .user_agent(GRAPH_USER_AGENT)
                .connect_timeout(GRAPH_REQUEST_TIMEOUT)
                .timeout(GRAPH_REQUEST_TIMEOUT)
                .build()
                .expect("reqwest async client should build"),
            cancellation: ExchangeCancellationToken::new(),
        }
    }

    pub fn graph(username: &str, access_token: &str) -> Self {
        let mut service = Self::new(GRAPH_DEFAULT_URL, username);
        service.set_access_token(access_token);
        service
    }

    /// Build the explicit portable/integration Graph path from an environment
    /// token.  The token remains process-local and is never persisted or
    /// included in diagnostics.  Linux EDS/GOA remains the production
    /// credential provider and does not depend on this override.
    pub fn from_environment(username: &str) -> Option<Self> {
        let token = std::env::var("EVOLUTION_GTK4_GRAPH_ACCESS_TOKEN").ok()?;
        let token = token.trim();
        (!token.is_empty()).then(|| Self::graph(username, token))
    }

    /// Inject an OAuth2 bearer token for the current process only.
    ///
    /// The token is kept in memory and is never included in `Debug`, logs,
    /// account profiles, backups, or error messages.  EDS/GOA or a future
    /// platform credential provider can supply this value.
    pub fn set_access_token(&mut self, access_token: &str) {
        let token = access_token.trim();
        self.access_token = (!token.is_empty()).then(|| token.to_string());
        self.auth_status = if self.access_token.is_some() {
            ExchangeAuthStatus::Authenticated
        } else {
            ExchangeAuthStatus::Disconnected
        };
    }

    #[allow(dead_code)]
    pub fn clear_access_token(&mut self) {
        self.access_token = None;
        self.auth_status = ExchangeAuthStatus::Disconnected;
    }

    #[allow(dead_code)]
    pub fn has_access_token(&self) -> bool {
        self.access_token.is_some()
    }

    /// Create a service clone bound to an existing mail/Camel cancellation
    /// flag.  The flag is shared rather than copied, so the established
    /// `Arc<AtomicBool>` cancellation API can stop an in-flight Graph request.
    pub fn with_cancellation_flag(&self, requested: Arc<AtomicBool>) -> Self {
        let mut service = self.clone();
        service.cancellation = ExchangeCancellationToken::from_flag(requested);
        service
    }

    /// Search the Global Address List through Microsoft Graph.
    ///
    /// This async compatibility wrapper keeps the original service API.  UI
    /// callers should use `search_gal_sync` from a worker thread because the
    /// portable fallback deliberately exposes a synchronous service boundary.
    #[allow(dead_code)]
    pub async fn search_gal(&self, query: &str) -> Result<Vec<GalContact>, String> {
        self.search_gal_sync(query)
            .map_err(|error| error.to_string())
    }

    pub fn search_gal_sync(&self, query: &str) -> Result<Vec<GalContact>, ExchangeError> {
        let query = validate_query(query)?;
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let escaped = escape_odata_string(&query);
        let filter = format!(
            "startswith(displayName,'{escaped}') or startswith(mail,'{escaped}') or startswith(userPrincipalName,'{escaped}')"
        );
        let params = vec![
            ("$filter".to_string(), filter),
            (
                "$select".to_string(),
                "displayName,mail,userPrincipalName,jobTitle,department,officeLocation".to_string(),
            ),
            ("$top".to_string(), "50".to_string()),
        ];
        let users: Vec<GraphUser> = self.request_collection("users", &params, MAX_GAL_RESULTS)?;
        Ok(users
            .into_iter()
            .filter_map(|user| {
                let email = user
                    .mail
                    .or(user.user_principal_name)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                (!email.is_empty()).then(|| GalContact {
                    display_name: user.display_name.unwrap_or_default(),
                    email_address: email,
                    job_title: user.job_title.unwrap_or_default(),
                    department: user.department.unwrap_or_default(),
                    office_location: user.office_location.unwrap_or_default(),
                })
            })
            .collect())
    }

    /// Load the complete visible mailbox hierarchy, following Graph's
    /// continuation links and each folder's child-folder collection.
    pub fn list_mail_folders_sync(&self) -> Result<Vec<ExchangeMailFolder>, ExchangeError> {
        let params = vec![
            (
                "$select".to_string(),
                "id,displayName,parentFolderId,childFolderCount,totalItemCount,unreadItemCount"
                    .to_string(),
            ),
            ("$top".to_string(), GRAPH_PAGE_SIZE.to_string()),
        ];
        let roots: Vec<GraphMailFolder> =
            self.request_collection("me/mailFolders", &params, MAX_GRAPH_ITEMS)?;
        let mut folders = Vec::new();
        let mut visited = HashSet::new();
        for folder in roots {
            self.append_mail_folder_tree(folder, &params, &mut folders, &mut visited, 0)?;
        }
        Ok(folders)
    }

    /// Load messages from one Graph mail folder.  The limit is applied after
    /// continuation pages have been followed, so a caller never has to
    /// reason about Graph's page size or opaque `@odata.nextLink` values.
    #[allow(dead_code)]
    pub fn list_messages_sync(
        &self,
        folder_id: &str,
        max_items: usize,
    ) -> Result<Vec<ExchangeMailMessage>, ExchangeError> {
        let max_items = validate_limit(max_items)?;
        let folder_id = encode_path_segment(folder_id)?;
        let params = vec![
            (
                "$select".to_string(),
                "id,parentFolderId,internetMessageId,subject,sender,from,toRecipients,ccRecipients,bccRecipients,replyTo,receivedDateTime,sentDateTime,size,body,bodyPreview,hasAttachments,isRead,flag,importance,categories,internetMessageHeaders"
                    .to_string(),
            ),
            ("$orderby".to_string(), "receivedDateTime desc".to_string()),
            ("$top".to_string(), page_size(max_items).to_string()),
        ];
        let messages: Vec<GraphMessage> = self.request_collection(
            &format!("me/mailFolders/{folder_id}/messages"),
            &params,
            max_items,
        )?;
        self.into_public_messages(messages, folder_id)
    }

    /// Download the exact RFC822/MIME representation exposed by Microsoft
    /// Graph's `$value` endpoint.
    pub(crate) fn message_mime_sync(&self, message_id: &str) -> Result<Vec<u8>, ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        let token = self
            .access_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or(ExchangeError::MissingAccessToken)?;
        let response = self.request_payload(
            token,
            GraphHttpRequest {
                method: Method::GET,
                url: self.endpoint(&format!("me/messages/{message_id}/$value"))?,
                params: Vec::new(),
                body: None,
                accept: "message/rfc822",
            },
        )?;
        if !response.status.is_success() {
            return Err(parse_http_error(
                response.status,
                &String::from_utf8_lossy(&response.body),
            ));
        }
        if response.body.is_empty() {
            return Err(ExchangeError::Decode(
                "Microsoft Graph lieferte eine leere Nachrichtenquelle".to_string(),
            ));
        }
        Ok(response.body)
    }

    /// Load exactly one Graph collection page and return the validated opaque
    /// continuation URL. Keeping this cursor avoids position-based `$skip`
    /// drift when new messages arrive between UI paging requests.
    pub fn list_messages_cursor_sync(
        &self,
        folder_id: &str,
        cursor: Option<&str>,
        max_items: usize,
    ) -> Result<(Vec<ExchangeMailMessage>, Option<String>), ExchangeError> {
        let max_items = validate_limit(max_items)?;
        let encoded_folder_id = encode_path_segment(folder_id)?;
        let params = vec![
            (
                "$select".to_string(),
                "id,parentFolderId,internetMessageId,subject,sender,from,toRecipients,ccRecipients,bccRecipients,replyTo,receivedDateTime,sentDateTime,size,body,bodyPreview,hasAttachments,isRead,flag,importance,categories,internetMessageHeaders"
                    .to_string(),
            ),
            ("$orderby".to_string(), "receivedDateTime desc".to_string()),
            ("$top".to_string(), page_size(max_items).to_string()),
        ];
        let (url, query) = match cursor {
            Some(cursor) => (self.validate_next_link(cursor)?, &[][..]),
            None => (
                self.endpoint(&format!("me/mailFolders/{encoded_folder_id}/messages"))?,
                params.as_slice(),
            ),
        };
        let page: GraphCollection<GraphMessage> =
            self.request_json_url(Method::GET, url, query, None)?;
        let next_link = page
            .next_link
            .map(|link| self.validate_next_link(&link).map(|url| url.to_string()))
            .transpose()?;
        let messages = page.value.into_iter().take(max_items).collect::<Vec<_>>();
        Ok((
            self.into_public_messages(messages, encoded_folder_id)?,
            next_link,
        ))
    }

    /// Search the signed-in mailbox using Outlook/Graph KQL.  The Graph mail
    /// search endpoint returns at most 1,000 results; this method follows
    /// continuation links until the caller's bounded result limit is met, and
    /// reports when that limit may have truncated the search result.
    pub fn search_messages_sync(
        &self,
        folder_id: Option<&str>,
        folder_name: Option<&str>,
        search: &str,
        max_items: usize,
    ) -> Result<ExchangeMailSearchResult, ExchangeError> {
        let max_items = validate_search_limit(max_items)?;
        let search = validate_query(search)?;
        if search.is_empty() {
            return Err(ExchangeError::InvalidQuery(
                "Die Graph-Mail-Suche darf nicht leer sein".to_string(),
            ));
        }
        let path = folder_id
            .map(|folder_id| {
                encode_path_segment(folder_id)
                    .map(|folder_id| format!("me/mailFolders/{folder_id}/messages"))
            })
            .transpose()?;
        let path = path.unwrap_or_else(|| "me/messages".to_string());
        let params = vec![
            (
                "$select".to_string(),
                "id,parentFolderId,internetMessageId,subject,sender,from,toRecipients,ccRecipients,bccRecipients,replyTo,receivedDateTime,sentDateTime,size,body,bodyPreview,hasAttachments,isRead,flag,importance,categories,internetMessageHeaders"
                    .to_string(),
            ),
            ("$search".to_string(), search),
            ("$top".to_string(), page_size(max_items).to_string()),
        ];
        let (messages, is_complete, next_cursor) =
            self.request_collection_with_completeness(&path, &params, max_items)?;
        let messages =
            self.into_public_messages(messages, folder_name.unwrap_or_default().to_string())?;
        Ok(ExchangeMailSearchResult {
            messages,
            is_complete,
            next_cursor,
        })
    }

    /// Load one bounded Graph mail-search page.
    ///
    /// With `cursor == None`, this starts a new KQL search using `folder_id`,
    /// `folder_name` and `search`.  With a cursor, the cursor is first
    /// validated against this service's configured origin and then requested
    /// verbatim.  In particular, the caller's query and folder are not
    /// reinterpreted or appended to a continuation URL.  This keeps Graph's
    /// opaque paging state authoritative and lets UI paging cross the
    /// service's ordinary search result limit one page at a time.
    pub fn search_messages_cursor_sync(
        &self,
        folder_id: Option<&str>,
        folder_name: Option<&str>,
        search: &str,
        cursor: Option<&str>,
        max_items: usize,
    ) -> Result<ExchangeMailSearchResult, ExchangeError> {
        let max_items = validate_search_limit(max_items)?;
        let is_continuation = cursor.is_some();
        let (url, params) = match cursor {
            Some(cursor) => (self.validate_next_link(cursor)?, Vec::new()),
            None => {
                let search = validate_query(search)?;
                if search.is_empty() {
                    return Err(ExchangeError::InvalidQuery(
                        "Die Graph-Mail-Suche darf nicht leer sein".to_string(),
                    ));
                }
                let path = folder_id
                    .map(|folder_id| {
                        encode_path_segment(folder_id)
                            .map(|folder_id| format!("me/mailFolders/{folder_id}/messages"))
                    })
                    .transpose()?;
                let path = path.unwrap_or_else(|| "me/messages".to_string());
                let params = vec![
                    (
                        "$select".to_string(),
                        "id,parentFolderId,internetMessageId,subject,sender,from,toRecipients,ccRecipients,bccRecipients,replyTo,receivedDateTime,sentDateTime,size,body,bodyPreview,hasAttachments,isRead,flag,importance,categories,internetMessageHeaders"
                            .to_string(),
                    ),
                    ("$search".to_string(), search),
                    ("$top".to_string(), page_size(max_items).to_string()),
                ];
                (self.endpoint(&path)?, params)
            }
        };

        let page: GraphCollection<GraphMessage> =
            self.request_json_url(Method::GET, url, &params, None)?;
        let GraphCollection {
            value: page_values,
            next_link,
        } = page;
        let next_cursor = next_link
            .map(|link| {
                self.validate_next_link(&link)?;
                Ok::<String, ExchangeError>(link)
            })
            .transpose()?;
        let page_was_truncated = page_values.len() > max_items;
        // A Graph continuation advances past the complete server page.  If
        // the server violated `$top` and we dropped values from that page,
        // exposing its cursor would silently skip those values on retry.
        let next_cursor = if page_was_truncated {
            None
        } else {
            next_cursor
        };
        let messages = page_values.into_iter().take(max_items).collect::<Vec<_>>();
        let fallback_folder_id = if is_continuation {
            // A cursor owns the search context.  Do not let a caller-supplied
            // folder name invent an identity when Graph omits parentFolderId.
            String::new()
        } else {
            folder_name.unwrap_or_default().to_string()
        };
        let messages = self.into_public_messages(messages, fallback_folder_id)?;
        Ok(ExchangeMailSearchResult {
            messages,
            is_complete: !page_was_truncated && next_cursor.is_none(),
            next_cursor,
        })
    }

    fn list_message_attachments_sync(
        &self,
        message_id: &str,
    ) -> Result<Vec<ExchangeMailAttachment>, ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        let params = vec![
            (
                "$select".to_string(),
                "id,name,contentType,size,isInline,contentId,contentBytes".to_string(),
            ),
            ("$top".to_string(), "50".to_string()),
        ];
        let attachments: Vec<GraphAttachment> = self.request_collection(
            &format!("me/messages/{message_id}/attachments"),
            &params,
            50,
        )?;
        Ok(attachments
            .into_iter()
            .filter_map(GraphAttachment::into_public)
            .collect())
    }

    fn into_public_messages(
        &self,
        messages: Vec<GraphMessage>,
        fallback_folder_id: String,
    ) -> Result<Vec<ExchangeMailMessage>, ExchangeError> {
        messages
            .into_iter()
            .map(|message| {
                let message_id = message.id.clone();
                let has_attachments = message.has_attachments;
                let mut message = message.into_public(fallback_folder_id.clone());
                if has_attachments {
                    message.attachments = self
                        .list_message_attachments_sync(&message_id)
                        .unwrap_or_else(|error| {
                            tracing::warn!(
                                %error,
                                message_id = %message_id,
                                "Graph message attachments could not be loaded"
                            );
                            Vec::new()
                        });
                }
                Ok(message)
            })
            .collect::<Result<Vec<_>, ExchangeError>>()
    }

    /// Load a calendar view for the inclusive time window used by Evolution's
    /// calendar page.  Graph expands recurring instances in this endpoint.
    pub fn list_calendar_view_sync(
        &self,
        start: &str,
        end: &str,
        max_items: usize,
    ) -> Result<Vec<ExchangeCalendarEvent>, ExchangeError> {
        let max_items = validate_limit(max_items)?;
        let start = validate_datetime(start, "Startzeitpunkt")?;
        let end = validate_datetime(end, "Endzeitpunkt")?;
        let params = vec![
            ("startDateTime".to_string(), start),
            ("endDateTime".to_string(), end),
            (
                "$select".to_string(),
                "id,subject,body,start,end,location,isAllDay,organizer,attendees,showAs,sensitivity,webLink,isCancelled,categories,recurrence"
                    .to_string(),
            ),
            ("$orderby".to_string(), "start/dateTime".to_string()),
            ("$top".to_string(), page_size(max_items).to_string()),
        ];
        let events: Vec<GraphEvent> =
            self.request_collection("me/calendarView", &params, max_items)?;
        Ok(events.into_iter().map(GraphEvent::into_public).collect())
    }

    /// Load contacts from the signed-in user's default Outlook contacts.
    /// Graph's contact filtering is intentionally kept to display names here;
    /// a full local search index can still search all returned fields.
    pub fn list_contacts_sync(
        &self,
        query: &str,
        max_items: usize,
    ) -> Result<Vec<ExchangeContact>, ExchangeError> {
        let max_items = validate_limit(max_items)?;
        let query = validate_query(query)?;
        let mut params = vec![
            (
                "$select".to_string(),
                "id,displayName,givenName,surname,middleName,nickName,title,companyName,department,jobTitle,officeLocation,birthday,personalNotes,emailAddresses,businessPhones,homePhones,mobilePhone,businessAddress,homeAddress,categories"
                    .to_string(),
            ),
            ("$orderby".to_string(), "displayName".to_string()),
            ("$top".to_string(), page_size(max_items).to_string()),
        ];
        if !query.is_empty() {
            params.push((
                "$filter".to_string(),
                format!("startswith(displayName,'{}')", escape_odata_string(&query)),
            ));
        }
        let contacts: Vec<GraphContact> =
            self.request_collection("me/contacts", &params, max_items)?;
        Ok(contacts
            .into_iter()
            .map(GraphContact::into_public)
            .collect())
    }

    #[allow(dead_code)]
    pub fn send_mail_sync(
        &self,
        from: &str,
        to: &[ExchangeEmailAddress],
        cc: &[ExchangeEmailAddress],
        bcc: &[ExchangeEmailAddress],
        subject: &str,
        body_html: &str,
        body_text: &str,
    ) -> Result<(), ExchangeError> {
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err(ExchangeError::InvalidQuery(
                "Mindestens ein Empfänger ist erforderlich".to_string(),
            ));
        }
        let body = if body_html.trim().is_empty() {
            serde_json::json!({"contentType":"Text", "content": body_text})
        } else {
            serde_json::json!({"contentType":"HTML", "content": body_html})
        };
        let message = serde_json::json!({
            "subject": subject,
            "body": body,
            "from": {"emailAddress": {"address": from}},
            "toRecipients": graph_recipients(to),
            "ccRecipients": graph_recipients(cc),
            "bccRecipients": graph_recipients(bcc)
        });
        let payload = serde_json::json!({"message": message, "saveToSentItems": true});
        self.request_empty(Method::POST, "me/sendMail", &[], Some(payload))
    }

    #[allow(dead_code)]
    pub fn set_message_state_sync(
        &self,
        message_id: &str,
        is_read: bool,
        is_flagged: bool,
        is_important: Option<bool>,
    ) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        let flag_status = if is_flagged { "flagged" } else { "notFlagged" };
        let mut payload = serde_json::json!({
            "isRead": is_read,
            "flag": {"flagStatus": flag_status}
        });
        if let Some(is_important) = is_important {
            payload["importance"] = serde_json::json!(if is_important { "high" } else { "normal" });
        }
        self.request_empty(
            Method::PATCH,
            &format!("me/messages/{message_id}"),
            &[],
            Some(payload),
        )
    }

    /// Update only the read bit without changing Outlook's independent
    /// follow-up flag. Graph has no Evolution-style `CAMEL_MESSAGE_FLAGGED`
    /// star, so read/unread actions must not accidentally clear `message.flag`.
    pub fn set_message_read_state_sync(
        &self,
        message_id: &str,
        is_read: bool,
    ) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        self.request_empty(
            Method::PATCH,
            &format!("me/messages/{message_id}"),
            &[],
            Some(serde_json::json!({"isRead": is_read})),
        )
    }

    /// Set the complete Microsoft Graph follow-up state for one message.
    ///
    /// Graph represents this state as `message.flag.flagStatus` and requires
    /// a PATCH to the message resource.  The request deliberately goes
    /// through the shared empty-response transport so token validation,
    /// cancellation and structured Graph errors have exactly the same
    /// behavior as the other message mutations.
    pub fn set_message_follow_up_state_sync(
        &self,
        message_id: &str,
        state: ExchangeMailFollowUpState,
    ) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        self.request_empty(
            Method::PATCH,
            &format!("me/messages/{message_id}"),
            &[],
            Some(serde_json::json!({
                "flag": {"flagStatus": graph_flag_status_for_follow_up_state(state)}
            })),
        )
    }

    pub fn set_message_importance_sync(
        &self,
        message_id: &str,
        is_important: bool,
    ) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        self.request_empty(
            Method::PATCH,
            &format!("me/messages/{message_id}"),
            &[],
            Some(serde_json::json!({
                "importance": if is_important { "high" } else { "normal" }
            })),
        )
    }

    pub fn set_message_label_sync(
        &self,
        message_id: &str,
        label: &str,
        labeled: bool,
    ) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 128 {
            return Err(ExchangeError::InvalidQuery(
                "Das Graph-Kategorie-Label ist ungültig".to_string(),
            ));
        }
        let existing: GraphMessage = self.request_json(
            Method::GET,
            &format!("me/messages/{message_id}"),
            &[("$select".to_string(), "categories".to_string())],
            None,
        )?;
        let mut categories = existing.categories;
        if labeled {
            if !categories
                .iter()
                .any(|value| value.eq_ignore_ascii_case(label))
            {
                categories.push(label.to_string());
            }
        } else {
            categories.retain(|value| !value.eq_ignore_ascii_case(label));
        }
        self.request_empty(
            Method::PATCH,
            &format!("me/messages/{message_id}"),
            &[],
            Some(serde_json::json!({"categories": categories})),
        )
    }

    pub fn set_message_junk_sync(
        &self,
        message_id: &str,
        current_folder_name: &str,
        is_junk: bool,
    ) -> Result<(), ExchangeError> {
        let target = if is_junk {
            folder_aliases("junk")
                .first()
                .copied()
                .unwrap_or("Junk Email")
        } else {
            current_folder_name
        };
        self.move_message_to_folder_name_sync(message_id, target)
    }

    pub fn move_message_to_folder_name_sync(
        &self,
        message_id: &str,
        folder_name: &str,
    ) -> Result<(), ExchangeError> {
        let folder_id = self.resolve_mail_folder_id_sync(folder_name)?;
        self.move_message_sync(message_id, &folder_id)
    }

    pub fn resolve_mail_folder_id_sync(&self, folder_name: &str) -> Result<String, ExchangeError> {
        let wanted = folder_name.trim();
        if wanted.is_empty() {
            return Err(ExchangeError::InvalidQuery(
                "Der Zielordner darf nicht leer sein".to_string(),
            ));
        }
        let folders = self.list_mail_folders_sync()?;
        let aliases = folder_aliases(wanted);
        folders
            .into_iter()
            .find(|folder| {
                folder.display_name.eq_ignore_ascii_case(wanted)
                    || aliases
                        .iter()
                        .any(|alias| folder.display_name.eq_ignore_ascii_case(alias))
            })
            .map(|folder| folder.id)
            .ok_or_else(|| {
                ExchangeError::InvalidQuery(format!("Graph-Mailordner nicht gefunden: {wanted}"))
            })
    }

    pub fn move_message_sync(
        &self,
        message_id: &str,
        destination_folder_id: &str,
    ) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        let destination_folder_id = encode_path_segment(destination_folder_id)?;
        self.request_empty(
            Method::POST,
            &format!("me/messages/{message_id}/move"),
            &[],
            Some(serde_json::json!({"destinationId": destination_folder_id})),
        )
    }

    /// Copy one message inside the signed-in mailbox and return the newly
    /// created Graph resource.  Graph assigns a new immutable message ID to
    /// the copy; callers must retain that ID if they want a safe undo path.
    pub fn copy_message_to_folder_sync(
        &self,
        message_id: &str,
        destination_folder_id: &str,
    ) -> Result<ExchangeMailMessage, ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        let destination_folder_id = validate_graph_value(destination_folder_id)?;
        let copied: GraphMessage = self.request_json(
            Method::POST,
            &format!("me/messages/{message_id}/copy"),
            &[],
            Some(serde_json::json!({"destinationId": destination_folder_id})),
        )?;
        self.into_public_messages(vec![copied], destination_folder_id)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ExchangeError::Decode("Graph lieferte keine kopierte Nachricht zurück".to_string())
            })
    }

    pub fn delete_message_sync(&self, message_id: &str) -> Result<(), ExchangeError> {
        let message_id = encode_path_segment(message_id)?;
        self.request_empty(
            Method::DELETE,
            &format!("me/messages/{message_id}"),
            &[],
            None,
        )
    }

    pub fn create_calendar_event_sync(
        &self,
        event: &crate::services::calendar_store_service::CalendarEventRecord,
    ) -> Result<ExchangeCalendarEvent, ExchangeError> {
        self.request_json(
            Method::POST,
            "me/events",
            &[],
            Some(calendar_event_payload(event)),
        )
    }

    pub(crate) fn send_raw_message_sync(
        &self,
        raw_message: &[u8],
        save_as_draft: bool,
    ) -> Result<bool, ExchangeError> {
        let path = if save_as_draft {
            "me/messages"
        } else {
            "me/sendMail"
        };
        // Microsoft Graph's MIME endpoints use a text/plain request body, but
        // the complete RFC822 payload itself must be Base64 encoded.  Keep
        // the original MIME bytes intact so attachments and S/MIME data are
        // not lost while adapting the transport boundary.
        let encoded = base64_encode(raw_message);
        self.request_raw_empty(Method::POST, path, encoded.as_bytes())?;
        Ok(!save_as_draft)
    }

    pub fn update_calendar_event_sync(
        &self,
        event_id: &str,
        event: &crate::services::calendar_store_service::CalendarEventRecord,
    ) -> Result<ExchangeCalendarEvent, ExchangeError> {
        let event_id = encode_path_segment(event_id)?;
        self.request_json(
            Method::PATCH,
            &format!("me/events/{event_id}"),
            &[],
            Some(calendar_event_payload(event)),
        )
    }

    pub fn delete_calendar_event_sync(&self, event_id: &str) -> Result<(), ExchangeError> {
        let event_id = encode_path_segment(event_id)?;
        self.request_empty(Method::DELETE, &format!("me/events/{event_id}"), &[], None)
    }

    pub fn create_contact_sync(
        &self,
        contact: &ExchangeContact,
    ) -> Result<ExchangeContact, ExchangeError> {
        self.request_json(
            Method::POST,
            "me/contacts",
            &[],
            Some(contact_payload(contact)),
        )
    }

    pub fn update_contact_sync(
        &self,
        contact: &ExchangeContact,
    ) -> Result<ExchangeContact, ExchangeError> {
        let id = encode_path_segment(&contact.id)?;
        self.request_json(
            Method::PATCH,
            &format!("me/contacts/{id}"),
            &[],
            Some(contact_payload(contact)),
        )
    }

    pub fn delete_contact_sync(&self, contact_id: &str) -> Result<(), ExchangeError> {
        let id = encode_path_segment(contact_id)?;
        self.request_empty(Method::DELETE, &format!("me/contacts/{id}"), &[], None)
    }

    pub fn get_out_of_office_sync(&self) -> Result<OutOfOfficeSettings, ExchangeError> {
        let response: GraphAutomaticRepliesSetting = self.request_json(
            Method::GET,
            "me/mailboxSettings/automaticRepliesSetting",
            &[],
            None,
        )?;
        Ok(ExchangeOutOfOffice::from_graph(response).into())
    }

    pub fn set_out_of_office_sync(
        &self,
        settings: &OutOfOfficeSettings,
    ) -> Result<OutOfOfficeSettings, ExchangeError> {
        settings.validate().map_err(ExchangeError::InvalidQuery)?;
        let status = if !settings.enabled {
            "disabled"
        } else if settings.use_schedule {
            "scheduled"
        } else {
            "alwaysEnabled"
        };
        if settings.enabled
            && settings.use_schedule
            && (settings.start_date.trim().is_empty() || settings.end_date.trim().is_empty())
        {
            return Err(ExchangeError::InvalidQuery(
                "Ein geplanter Exchange-Autoresponder benötigt Start- und Enddatum".to_string(),
            ));
        }
        let mut body = serde_json::json!({
            "status": status,
            "internalReplyMessage": settings.internal_message,
            "externalReplyMessage": settings.external_message
        });
        if settings.enabled && settings.use_schedule {
            body["scheduledStartDateTime"] = serde_json::json!({
                "dateTime": graph_date_time(&settings.start_date),
                "timeZone": "UTC"
            });
            body["scheduledEndDateTime"] = serde_json::json!({
                "dateTime": graph_date_time(&settings.end_date),
                "timeZone": "UTC"
            });
        }
        let response: GraphAutomaticRepliesSetting = self.request_json(
            Method::PATCH,
            "me/mailboxSettings/automaticRepliesSetting",
            &[],
            Some(body),
        )?;
        Ok(ExchangeOutOfOffice::from_graph(response).into())
    }

    fn request_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        params: &[(String, String)],
        body: Option<serde_json::Value>,
    ) -> Result<T, ExchangeError> {
        let url = self.endpoint(path)?;
        self.request_json_url(method, url, params, body)
    }

    fn request_json_url<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        params: &[(String, String)],
        body: Option<serde_json::Value>,
    ) -> Result<T, ExchangeError> {
        let token = self
            .access_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or(ExchangeError::MissingAccessToken)?;
        let response = self.request_payload(
            token,
            GraphHttpRequest {
                method,
                url,
                params: params.to_vec(),
                body: body.map(GraphHttpBody::Json),
                accept: "application/json",
            },
        )?;
        if !response.status.is_success() {
            return Err(parse_http_error(
                response.status,
                &String::from_utf8_lossy(&response.body),
            ));
        }
        serde_json::from_slice(&response.body)
            .map_err(|error| ExchangeError::Decode(error.to_string()))
    }

    fn request_empty(
        &self,
        method: Method,
        path: &str,
        params: &[(String, String)],
        body: Option<serde_json::Value>,
    ) -> Result<(), ExchangeError> {
        let token = self
            .access_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or(ExchangeError::MissingAccessToken)?;
        let url = self.endpoint(path)?;
        let response = self.request_payload(
            token,
            GraphHttpRequest {
                method,
                url,
                params: params.to_vec(),
                body: body.map(GraphHttpBody::Json),
                accept: "application/json",
            },
        )?;
        if !response.status.is_success() {
            return Err(parse_http_error(
                response.status,
                &String::from_utf8_lossy(&response.body),
            ));
        }
        Ok(())
    }

    fn request_raw_empty(
        &self,
        method: Method,
        path: &str,
        body: &[u8],
    ) -> Result<(), ExchangeError> {
        if body.is_empty() {
            return Err(ExchangeError::InvalidQuery(
                "Die RFC822-Nachricht ist leer".to_string(),
            ));
        }
        let token = self
            .access_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or(ExchangeError::MissingAccessToken)?;
        let url = self.endpoint(path)?;
        let response = self.request_payload(
            token,
            GraphHttpRequest {
                method,
                url,
                params: Vec::new(),
                body: Some(GraphHttpBody::Raw(body.to_vec())),
                accept: "application/json",
            },
        )?;
        if !response.status.is_success() {
            return Err(parse_http_error(
                response.status,
                &String::from_utf8_lossy(&response.body),
            ));
        }
        Ok(())
    }

    fn request_payload(
        &self,
        token: &str,
        request: GraphHttpRequest,
    ) -> Result<GraphHttpResponse, ExchangeError> {
        if self.cancellation.is_cancelled() {
            return Err(ExchangeError::Cancelled);
        }

        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        let (cancel_sender, cancel_receiver) = tokio::sync::oneshot::channel();
        let client = self.client.clone();
        let token = token.to_string();
        std::thread::spawn(move || {
            let result = run_graph_http_request(client, token, request, cancel_receiver);
            let _ = result_sender.send(result);
        });

        loop {
            match result_receiver.recv_timeout(GRAPH_CANCELLATION_POLL) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ExchangeError::Transport(
                        "der Graph-HTTP-Arbeiter wurde unerwartet beendet".to_string(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !self.cancellation.is_cancelled() {
                        continue;
                    }

                    // Dropping the request future is the cancellation point.
                    // Wait briefly for the worker to acknowledge that drop so
                    // normal tests and callers can rely on the socket being
                    // released before the operation returns.  The bounded
                    // fallback keeps cancellation prompt even if a resolver
                    // or platform TLS backend does not unwind immediately.
                    let _ = cancel_sender.send(());
                    let _ = result_receiver.recv_timeout(GRAPH_CANCELLATION_ACK_TIMEOUT);
                    return Err(ExchangeError::Cancelled);
                }
            }
        }
    }

    fn request_collection<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(String, String)],
        max_items: usize,
    ) -> Result<Vec<T>, ExchangeError> {
        let max_items = validate_limit(max_items)?;
        let mut next_url = Some(self.endpoint(path)?);
        let mut first_page = true;
        let mut pages = 0;
        let mut values = Vec::new();

        while let Some(url) = next_url.take() {
            pages += 1;
            if pages > MAX_GRAPH_PAGES {
                return Err(ExchangeError::Decode(
                    "Microsoft-Graph lieferte zu viele Seiten ohne Abschluss".to_string(),
                ));
            }
            let response: GraphCollection<T> = self.request_json_url(
                Method::GET,
                url,
                if first_page { params } else { &[] },
                None,
            )?;
            first_page = false;
            let remaining = max_items.saturating_sub(values.len());
            values.extend(response.value.into_iter().take(remaining));
            if values.len() >= max_items {
                break;
            }
            next_url = response
                .next_link
                .map(|link| self.validate_next_link(&link))
                .transpose()?;
        }
        Ok(values)
    }

    /// Collect a bounded collection while preserving whether the bound cut
    /// off a page or a validated continuation link.  Ordinary collection
    /// callers intentionally keep using `request_collection`; this stricter
    /// boundary is reserved for mail search, where a truncated result must be
    /// surfaced instead of looking complete to the caller.
    fn request_collection_with_completeness<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(String, String)],
        max_items: usize,
    ) -> Result<(Vec<T>, bool, Option<String>), ExchangeError> {
        let max_items = validate_limit(max_items)?;
        let mut next_url = Some(self.endpoint(path)?);
        let mut first_page = true;
        let mut pages = 0;
        let mut values = Vec::new();

        while let Some(url) = next_url.take() {
            pages += 1;
            if pages > MAX_GRAPH_PAGES {
                return Err(ExchangeError::Decode(
                    "Microsoft-Graph lieferte zu viele Seiten ohne Abschluss".to_string(),
                ));
            }
            let response: GraphCollection<T> = self.request_json_url(
                Method::GET,
                url,
                if first_page { params } else { &[] },
                None,
            )?;
            first_page = false;

            let GraphCollection {
                value: page_values,
                next_link,
            } = response;
            let remaining = max_items.saturating_sub(values.len());
            let page_was_truncated = page_values.len() > remaining;
            values.extend(page_values.into_iter().take(remaining));
            let validated_next_url = next_link
                .map(|link| {
                    let url = self.validate_next_link(&link)?;
                    Ok::<(Url, String), ExchangeError>((url, link))
                })
                .transpose()?;

            if page_was_truncated {
                // The continuation points after the whole server page, not
                // after the prefix returned to the caller.  Do not expose it
                // when `$top` was violated and values were dropped.
                return Ok((values, false, None));
            }
            if values.len() >= max_items {
                let next_cursor = validated_next_url.map(|(_, cursor)| cursor);
                return Ok((values, next_cursor.is_none(), next_cursor));
            }
            next_url = validated_next_url.map(|(url, _)| url);
        }

        Ok((values, true, None))
    }

    fn append_mail_folder_tree(
        &self,
        folder: GraphMailFolder,
        params: &[(String, String)],
        output: &mut Vec<ExchangeMailFolder>,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> Result<(), ExchangeError> {
        if depth > 32 || output.len() >= MAX_GRAPH_ITEMS || !visited.insert(folder.id.clone()) {
            return Ok(());
        }
        let folder_id = encode_path_segment(&folder.id)?;
        let child_count = folder.child_folder_count;
        output.push(folder.into_public());
        if child_count == 0 {
            return Ok(());
        }
        let children: Vec<GraphMailFolder> = self.request_collection(
            &format!("me/mailFolders/{folder_id}/childFolders"),
            params,
            MAX_GRAPH_ITEMS,
        )?;
        for child in children {
            self.append_mail_folder_tree(child, params, output, visited, depth + 1)?;
            if output.len() >= MAX_GRAPH_ITEMS {
                break;
            }
        }
        Ok(())
    }

    fn validate_next_link(&self, link: &str) -> Result<Url, ExchangeError> {
        let next =
            Url::parse(link).map_err(|error| ExchangeError::InvalidUrl(error.to_string()))?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err(ExchangeError::InvalidUrl(
                "Graph-Fortsetzungslinks müssen http oder https verwenden".to_string(),
            ));
        }
        let base = Url::parse(self.server_url.trim_end_matches('/'))
            .map_err(|error| ExchangeError::InvalidUrl(error.to_string()))?;
        if next.scheme() != base.scheme()
            || next.host_str() != base.host_str()
            || next.port_or_known_default() != base.port_or_known_default()
            || !next.username().is_empty()
            || next.password().is_some()
        {
            return Err(ExchangeError::InvalidUrl(
                "Graph-Fortsetzungslink verwendet eine fremde Herkunft".to_string(),
            ));
        }
        Ok(next)
    }

    fn endpoint(&self, path: &str) -> Result<Url, ExchangeError> {
        let mut base = Url::parse(self.server_url.trim_end_matches('/'))
            .map_err(|error| ExchangeError::InvalidUrl(error.to_string()))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(ExchangeError::InvalidUrl(
                "nur http und https werden unterstützt".to_string(),
            ));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(ExchangeError::InvalidUrl(
                "die Basis-URL darf keine Query oder Fragment-Komponente enthalten".to_string(),
            ));
        }

        let is_graph_host = base
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("graph.microsoft.com"));
        let current_path = base.path().trim_end_matches('/');
        if is_graph_host && !current_path.ends_with("/v1.0") {
            base.set_path(&format!("{current_path}/v1.0"));
        }
        let joined = format!(
            "{}/{}",
            base.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        Url::parse(&joined).map_err(|error| ExchangeError::InvalidUrl(error.to_string()))
    }
}

fn run_graph_http_request(
    client: Client,
    token: String,
    request: GraphHttpRequest,
    cancel_receiver: tokio::sync::oneshot::Receiver<()>,
) -> Result<GraphHttpResponse, ExchangeError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ExchangeError::Transport(format!("Tokio-HTTP-Arbeiter: {error}")))?;

    runtime.block_on(async move {
        let request_future = async move {
            let GraphHttpRequest {
                method,
                url,
                params,
                body,
                accept,
            } = request;
            let mut builder = client
                .request(method, url)
                .bearer_auth(token)
                .header(reqwest::header::ACCEPT, accept)
                .query(&params);
            if let Some(body) = body {
                builder = match body {
                    GraphHttpBody::Json(body) => builder
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .json(&body),
                    GraphHttpBody::Raw(body) => builder
                        .header(reqwest::header::CONTENT_TYPE, "text/plain")
                        .body(body),
                };
            }
            let response = builder
                .send()
                .await
                .map_err(|error| ExchangeError::Transport(sanitize_transport_error(error)))?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|error| ExchangeError::Transport(sanitize_transport_error(error)))?
                .to_vec();
            Ok(GraphHttpResponse { status, body })
        };

        tokio::select! {
            result = request_future => result,
            _ = cancel_receiver => Err(ExchangeError::Cancelled),
        }
    })
}

/// Select the configured Exchange profile used by the portable development
/// and fallback path.  The OAuth token is still process-local and must be
/// supplied through the platform credential integration or the explicit
/// development environment override.
pub(crate) fn graph_service_for_configured_account() -> Option<(String, ExchangeService)> {
    use crate::services::account_config_service::{AccountConfigStore, AccountProtocol};
    use crate::services::settings_service::AppSettings;

    let accounts = AccountConfigStore::default().load();
    let preferred_id = AppSettings::try_new()
        .map(|settings| settings.default_account_id())
        .filter(|id| !id.trim().is_empty());
    graph_service_for_accounts(
        &accounts,
        preferred_id.as_deref(),
        AccountProtocol::Exchange,
    )
}

pub(crate) fn graph_service_for_source(source_uid: &str) -> Option<ExchangeService> {
    let (configured_source_uid, service) = graph_service_for_configured_account()?;
    (configured_source_uid == source_uid).then_some(service)
}

/// Resolve a Graph account without consulting GSettings.  Provider workers
/// must remain framework-neutral: selecting a configured account is file/env
/// work, while `AppSettings` is a GObject-backed UI boundary.  The normal
/// UI-facing selector above still honors the persisted default account.
pub(crate) fn graph_service_for_configured_account_worker() -> Option<(String, ExchangeService)> {
    use crate::services::account_config_service::{AccountConfigStore, AccountProtocol};

    let accounts = AccountConfigStore::default().load();
    graph_service_for_accounts(&accounts, None, AccountProtocol::Exchange)
}

/// Resolve the exact Graph profile named by a provider worker.  This avoids
/// falling back to the first Exchange account when a send belongs to another
/// configured source, and it never touches GTK/GSettings.
pub(crate) fn graph_service_for_source_worker(source_uid: &str) -> Option<ExchangeService> {
    use crate::services::account_config_service::{AccountConfigStore, AccountProtocol};

    let accounts = AccountConfigStore::default().load();
    let profile = accounts.iter().find(|profile| {
        profile.enabled && profile.protocol == AccountProtocol::Exchange && profile.id == source_uid
    })?;
    ExchangeService::from_environment(&profile.email)
}

fn graph_service_for_accounts(
    accounts: &[crate::services::account_config_service::AccountProfile],
    preferred_id: Option<&str>,
    protocol: crate::services::account_config_service::AccountProtocol,
) -> Option<(String, ExchangeService)> {
    let is_matching_profile =
        |profile: &&crate::services::account_config_service::AccountProfile| {
            profile.enabled && profile.protocol == protocol
        };
    let profile = preferred_id
        .and_then(|id| {
            accounts
                .iter()
                .find(|profile| is_matching_profile(profile) && profile.id == id)
        })
        .or_else(|| accounts.iter().find(is_matching_profile))?;
    let service = ExchangeService::from_environment(&profile.email)?;
    Some((profile.id.clone(), service))
}

#[derive(Debug, Deserialize)]
struct GraphCollection<T> {
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    #[serde(default)]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphUser {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    mail: Option<String>,
    #[serde(rename = "jobTitle")]
    job_title: Option<String>,
    department: Option<String>,
    #[serde(rename = "officeLocation")]
    office_location: Option<String>,
    #[serde(rename = "userPrincipalName")]
    user_principal_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct GraphMailFolder {
    #[serde(default)]
    id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "parentFolderId")]
    parent_folder_id: Option<String>,
    #[serde(rename = "childFolderCount", default)]
    child_folder_count: usize,
    #[serde(rename = "totalItemCount", default)]
    total_item_count: usize,
    #[serde(rename = "unreadItemCount", default)]
    unread_item_count: usize,
}

impl GraphMailFolder {
    fn into_public(self) -> ExchangeMailFolder {
        ExchangeMailFolder {
            id: self.id,
            display_name: self.display_name,
            parent_folder_id: self.parent_folder_id,
            child_folder_count: self.child_folder_count,
            total_item_count: self.total_item_count,
            unread_item_count: self.unread_item_count,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphRecipient {
    #[serde(rename = "emailAddress", default)]
    email_address: GraphEmailAddress,
}

#[derive(Debug, Deserialize, Default)]
struct GraphEmailAddress {
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
}

impl GraphEmailAddress {
    fn into_public(self) -> ExchangeEmailAddress {
        ExchangeEmailAddress {
            name: self.name,
            address: self.address,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphItemBody {
    #[serde(default)]
    content: String,
    #[serde(rename = "contentType", default)]
    content_type: String,
}

impl GraphItemBody {
    fn into_parts(self) -> (String, String) {
        if self.content_type.eq_ignore_ascii_case("html") {
            (self.content, String::new())
        } else {
            (String::new(), self.content)
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphFollowupFlag {
    #[serde(rename = "flagStatus", default)]
    flag_status: String,
}

impl GraphFollowupFlag {
    fn into_public_state(self) -> ExchangeMailFollowUpState {
        follow_up_state_from_graph_flag_status(&self.flag_status)
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphMessage {
    #[serde(default)]
    id: String,
    #[serde(rename = "parentFolderId")]
    parent_folder_id: Option<String>,
    #[serde(rename = "internetMessageId")]
    internet_message_id: Option<String>,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    sender: GraphRecipient,
    #[serde(default)]
    from: GraphRecipient,
    #[serde(rename = "toRecipients", default)]
    to_recipients: Vec<GraphRecipient>,
    #[serde(rename = "ccRecipients", default)]
    cc_recipients: Vec<GraphRecipient>,
    #[serde(rename = "bccRecipients", default)]
    bcc_recipients: Vec<GraphRecipient>,
    #[serde(rename = "replyTo", default)]
    reply_to: Vec<GraphRecipient>,
    #[serde(rename = "receivedDateTime", default)]
    received_date_time: String,
    #[serde(rename = "sentDateTime", default)]
    sent_date_time: String,
    #[serde(default)]
    size: usize,
    #[serde(default)]
    body: GraphItemBody,
    #[serde(rename = "bodyPreview", default)]
    body_preview: String,
    #[serde(rename = "hasAttachments", default)]
    has_attachments: bool,
    #[serde(rename = "isRead", default)]
    is_read: bool,
    #[serde(default)]
    flag: GraphFollowupFlag,
    #[serde(default)]
    importance: String,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(rename = "internetMessageHeaders", default)]
    internet_message_headers: Vec<GraphInternetMessageHeader>,
}

#[derive(Debug, Deserialize, Default)]
struct GraphInternetMessageHeader {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
}

impl GraphMessage {
    fn into_public(self, folder_id: String) -> ExchangeMailMessage {
        let (body_html, body_text) = self.body.into_parts();
        let follow_up_state = self.flag.into_public_state();
        ExchangeMailMessage {
            id: self.id,
            folder_id: self.parent_folder_id.unwrap_or(folder_id),
            internet_message_id: self.internet_message_id,
            subject: self.subject,
            sender: self.sender.email_address.into_public(),
            from: self.from.email_address.into_public(),
            to_recipients: self
                .to_recipients
                .into_iter()
                .map(|recipient| recipient.email_address.into_public())
                .collect(),
            cc_recipients: self
                .cc_recipients
                .into_iter()
                .map(|recipient| recipient.email_address.into_public())
                .collect(),
            bcc_recipients: self
                .bcc_recipients
                .into_iter()
                .map(|recipient| recipient.email_address.into_public())
                .collect(),
            reply_to: self
                .reply_to
                .into_iter()
                .map(|recipient| recipient.email_address.into_public())
                .collect(),
            received_date_time: self.received_date_time,
            sent_date_time: self.sent_date_time,
            size: self.size,
            body_preview: self.body_preview,
            body_html,
            body_text,
            has_attachments: self.has_attachments,
            is_read: self.is_read,
            // Graph's message.flag is Outlook follow-up, not Evolution's
            // separate starred/flagged bit. Keep the two UI columns distinct.
            is_flagged: false,
            follow_up_state,
            is_important: self.importance.eq_ignore_ascii_case("high"),
            categories: self.categories,
            internet_message_headers: self
                .internet_message_headers
                .into_iter()
                .map(|header| (header.name, header.value))
                .collect(),
            attachments: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphAttachment {
    #[serde(default)]
    name: String,
    #[serde(rename = "contentType", default)]
    content_type: String,
    #[serde(default)]
    size: usize,
    #[serde(rename = "contentId")]
    content_id: Option<String>,
    #[serde(rename = "isInline", default)]
    is_inline: bool,
    #[serde(rename = "contentBytes")]
    content_bytes: Option<String>,
}

impl GraphAttachment {
    fn into_public(self) -> Option<ExchangeMailAttachment> {
        let content = self
            .content_bytes
            .as_deref()
            .and_then(decode_base64)
            .unwrap_or_default();
        Some(ExchangeMailAttachment {
            name: self.name,
            content_type: self.content_type,
            size: self.size,
            content_id: self.content_id,
            is_inline: self.is_inline,
            content,
        })
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphPostalAddress {
    #[serde(default)]
    street: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    state: String,
    #[serde(rename = "countryOrRegion", default)]
    country_or_region: String,
    #[serde(rename = "postalCode", default)]
    postal_code: String,
}

impl GraphPostalAddress {
    fn into_public(self) -> ExchangePostalAddress {
        ExchangePostalAddress {
            street: self.street,
            city: self.city,
            state: self.state,
            country_or_region: self.country_or_region,
            postal_code: self.postal_code,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphContact {
    #[serde(default)]
    id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "givenName", default)]
    given_name: String,
    #[serde(rename = "surname", default)]
    surname: String,
    #[serde(rename = "middleName", default)]
    middle_name: String,
    #[serde(rename = "nickName", default)]
    nickname: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "companyName", default)]
    company_name: String,
    #[serde(default)]
    department: String,
    #[serde(rename = "jobTitle", default)]
    job_title: String,
    #[serde(rename = "officeLocation", default)]
    office_location: String,
    #[serde(default)]
    birthday: String,
    #[serde(rename = "personalNotes", default)]
    personal_notes: String,
    #[serde(rename = "emailAddresses", default)]
    email_addresses: Vec<GraphEmailAddress>,
    #[serde(rename = "businessPhones", default)]
    business_phones: Vec<String>,
    #[serde(rename = "homePhones", default)]
    home_phones: Vec<String>,
    #[serde(rename = "mobilePhone", default)]
    mobile_phone: String,
    #[serde(rename = "businessAddress")]
    business_address: Option<GraphPostalAddress>,
    #[serde(rename = "homeAddress")]
    home_address: Option<GraphPostalAddress>,
    #[serde(default)]
    categories: Vec<String>,
}

impl GraphContact {
    fn into_public(self) -> ExchangeContact {
        ExchangeContact {
            id: self.id,
            display_name: self.display_name,
            given_name: self.given_name,
            surname: self.surname,
            middle_name: self.middle_name,
            nickname: self.nickname,
            title: self.title,
            company_name: self.company_name,
            department: self.department,
            job_title: self.job_title,
            office_location: self.office_location,
            birthday: self.birthday,
            personal_notes: self.personal_notes,
            email_addresses: self
                .email_addresses
                .into_iter()
                .map(GraphEmailAddress::into_public)
                .collect(),
            business_phones: self.business_phones,
            home_phones: self.home_phones,
            mobile_phone: self.mobile_phone,
            business_address: self.business_address.map(GraphPostalAddress::into_public),
            home_address: self.home_address.map(GraphPostalAddress::into_public),
            categories: self.categories,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    body: GraphItemBody,
    #[serde(default)]
    start: GraphDateTime,
    #[serde(default)]
    end: GraphDateTime,
    #[serde(default)]
    location: GraphLocation,
    #[serde(rename = "isAllDay", default)]
    is_all_day: bool,
    #[serde(default)]
    organizer: GraphRecipient,
    #[serde(default)]
    attendees: Vec<GraphRecipient>,
    #[serde(rename = "showAs", default)]
    show_as: String,
    #[serde(default)]
    sensitivity: String,
    #[serde(rename = "webLink", default)]
    web_link: String,
    #[serde(rename = "isCancelled", default)]
    is_cancelled: bool,
    #[serde(default)]
    categories: Vec<String>,
    recurrence: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct GraphLocation {
    #[serde(rename = "displayName", default)]
    display_name: String,
}

impl GraphEvent {
    fn into_public(self) -> ExchangeCalendarEvent {
        let (body_html, body_text) = self.body.into_parts();
        let is_recurring = self.recurrence.is_some();
        let recurrence_rule = self.recurrence.as_ref().and_then(graph_recurrence_rule);
        ExchangeCalendarEvent {
            id: self.id,
            subject: self.subject,
            body_html,
            body_text,
            start: self.start.into_public(),
            end: self.end.into_public(),
            location: self.location.display_name,
            is_all_day: self.is_all_day,
            organizer: self.organizer.email_address.into_public(),
            attendees: self
                .attendees
                .into_iter()
                .map(|recipient| recipient.email_address.into_public())
                .collect(),
            show_as: self.show_as,
            sensitivity: self.sensitivity,
            web_link: self.web_link,
            is_cancelled: self.is_cancelled,
            categories: self.categories,
            is_recurring,
            recurrence_rule,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GraphAutomaticRepliesSetting {
    status: String,
    #[serde(rename = "scheduledStartDateTime")]
    scheduled_start: Option<GraphDateTime>,
    #[serde(rename = "scheduledEndDateTime")]
    scheduled_end: Option<GraphDateTime>,
    #[serde(rename = "internalReplyMessage", default)]
    internal_message: String,
    #[serde(rename = "externalReplyMessage", default)]
    external_message: String,
}

#[derive(Debug, Deserialize, Default)]
struct GraphDateTime {
    #[serde(rename = "dateTime", default)]
    date_time: String,
    #[serde(rename = "timeZone", default)]
    time_zone: String,
}

impl GraphDateTime {
    fn into_public(self) -> ExchangeDateTime {
        ExchangeDateTime {
            date_time: self.date_time,
            time_zone: self.time_zone,
        }
    }
}

impl ExchangeOutOfOffice {
    fn from_graph(value: GraphAutomaticRepliesSetting) -> Self {
        Self {
            enabled: value.status != "disabled",
            use_schedule: value.status == "scheduled",
            start_date: graph_date(&value.scheduled_start),
            end_date: graph_date(&value.scheduled_end),
            internal_message: value.internal_message,
            external_message: value.external_message,
        }
    }
}

fn graph_date(value: &Option<GraphDateTime>) -> String {
    value
        .as_ref()
        .and_then(|value| value.date_time.get(..10))
        .unwrap_or_default()
        .to_string()
}

fn graph_date_time(value: &str) -> String {
    let date = value.trim();
    if date.is_empty() {
        "1970-01-01T00:00:00".to_string()
    } else {
        format!("{date}T00:00:00")
    }
}

fn validate_limit(limit: usize) -> Result<usize, ExchangeError> {
    if limit == 0 {
        return Err(ExchangeError::InvalidQuery(
            "Die maximale Anzahl muss größer als null sein".to_string(),
        ));
    }
    Ok(limit.min(MAX_GRAPH_ITEMS))
}

fn validate_search_limit(limit: usize) -> Result<usize, ExchangeError> {
    Ok(validate_limit(limit)?.min(MAX_GRAPH_SEARCH_ITEMS))
}

fn page_size(limit: usize) -> usize {
    limit.min(GRAPH_PAGE_SIZE).max(1)
}

fn validate_datetime(value: &str, label: &str) -> Result<String, ExchangeError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 || value.contains(['\r', '\n']) {
        return Err(ExchangeError::InvalidQuery(format!("{label} ist ungültig")));
    }
    Ok(value.to_string())
}

fn encode_path_segment(value: &str) -> Result<String, ExchangeError> {
    let value = validate_graph_value(value)?;
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    Ok(encoded)
}

fn validate_graph_value(value: &str) -> Result<String, ExchangeError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 512 || value.contains(['\r', '\n']) {
        return Err(ExchangeError::InvalidQuery(
            "Graph-ID enthält ungültige Zeichen".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + value - 10) as char,
        _ => unreachable!(),
    }
}

fn validate_query(query: &str) -> Result<String, ExchangeError> {
    let query = query.trim();
    if query.chars().count() > MAX_QUERY_LENGTH {
        return Err(ExchangeError::InvalidQuery(format!(
            "Suchbegriffe dürfen höchstens {MAX_QUERY_LENGTH} Zeichen enthalten"
        )));
    }
    if query
        .chars()
        .any(|character| character == '\r' || character == '\n')
    {
        return Err(ExchangeError::InvalidQuery(
            "Suchbegriffe dürfen keine Zeilenumbrüche enthalten".to_string(),
        ));
    }
    Ok(query.to_string())
}

fn escape_odata_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn sanitize_transport_error(error: reqwest::Error) -> String {
    error.without_url().to_string().chars().take(512).collect()
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let bytes = value
        .as_bytes()
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let first = base64_value(chunk[0])?;
        let second = base64_value(chunk[1])?;
        let third = if chunk[2] == b'=' {
            None
        } else {
            Some(base64_value(chunk[2])?)
        };
        let fourth = if chunk[3] == b'=' {
            None
        } else {
            Some(base64_value(chunk[3])?)
        };
        output.push((first << 2) | (second >> 4));
        if let Some(third) = third {
            output.push((second << 4) | (third >> 2));
            if let Some(fourth) = fourth {
                output.push((third << 6) | fourth);
            }
        }
    }
    Some(output)
}

fn base64_value(value: u8) -> Option<u8> {
    match value {
        b'A'..=b'Z' => Some(value - b'A'),
        b'a'..=b'z' => Some(value - b'a' + 26),
        b'0'..=b'9' => Some(value - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn parse_http_error(status: StatusCode, body: &str) -> ExchangeError {
    #[derive(Deserialize)]
    struct Envelope {
        error: Option<BodyError>,
    }
    #[derive(Deserialize)]
    struct BodyError {
        code: Option<String>,
        message: Option<String>,
    }

    let parsed = serde_json::from_str::<Envelope>(body).ok();
    let (code, message) = parsed
        .and_then(|envelope| envelope.error)
        .map(|error| {
            (
                error.code,
                error
                    .message
                    .unwrap_or_else(|| "unbekannter Graph-Fehler".to_string()),
            )
        })
        .unwrap_or_else(|| (None, "unbekannter Graph-Fehler".to_string()));
    ExchangeError::Http {
        status: status.as_u16(),
        code,
        message: message.chars().take(512).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    fn mock_server(
        response: &'static str,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener
            .local_addr()
            .expect("mock server address should exist");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client should connect");
            let request = read_request(&mut stream);
            sender.send(request).expect("request should be observed");
            stream
                .write_all(response.as_bytes())
                .expect("mock response should be written");
        });
        (format!("http://{address}/v1.0"), receiver, handle)
    }

    fn mock_server_sequence(
        responses: Vec<String>,
    ) -> (String, mpsc::Receiver<Vec<String>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener
            .local_addr()
            .expect("mock server address should exist");
        let (sender, receiver) = mpsc::channel();
        let base_url = format!("http://{address}/v1.0");
        let response_base_url = base_url.clone();
        let handle = thread::spawn(move || {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut stream, _) = listener.accept().expect("client should connect");
                requests.push(read_request(&mut stream));
                let response = response.replace("__BASE__", &response_base_url);
                stream
                    .write_all(response.as_bytes())
                    .expect("mock response should be written");
            }
            sender.send(requests).expect("requests should be observed");
        });
        (base_url, receiver, handle)
    }

    fn slow_mock_server() -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener
            .local_addr()
            .expect("mock server address should exist");
        let (seen_sender, seen_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client should connect");
            let _request = read_request(&mut stream);
            seen_sender.send(()).expect("request should be observed");
            release_receiver
                .recv()
                .expect("test should release the slow response");
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[]}",
            );
        });
        (
            format!("http://{address}/v1.0"),
            seen_receiver,
            release_sender,
            handle,
        )
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut target_length = None;
        loop {
            let count = stream
                .read(&mut buffer)
                .expect("request should be readable");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if target_length.is_none() {
                let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let header_end = header_end + 4;
                let content_length = String::from_utf8_lossy(&bytes[..header_end])
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Content-Length:")
                            .or_else(|| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                target_length = Some(header_end + content_length);
            }
            if target_length.is_some_and(|length| bytes.len() >= length) {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn graph_gal_search_uses_bearer_token_and_escapes_query() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"value\":[{\"displayName\":\"O'Connor\",\"mail\":\"oconnor@example.org\",\"jobTitle\":\"Lead\",\"department\":\"IT\",\"officeLocation\":\"Berlin\"}]}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService::new(&base_url, "user@example.org");
        let mut service = service;
        service.set_access_token("test-token");
        let contacts = service
            .search_gal_sync("O'Connor")
            .expect("GAL response should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        let request_lower = request.to_ascii_lowercase();
        assert!(request.contains("GET /v1.0/users?"));
        assert!(request_lower.contains("authorization: bearer test-token"));
        assert!(request.contains("%24filter="));
        assert_eq!(contacts[0].email_address, "oconnor@example.org");
        assert_eq!(contacts[0].office_location, "Berlin");
    }

    #[test]
    fn graph_gal_search_follows_pagination_and_uses_user_principal_name() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener
            .local_addr()
            .expect("mock server address should exist");
        let (sender, receiver) = mpsc::channel();
        let responses = vec![
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{\"value\":[{{\"displayName\":\"Ada Example\",\"mail\":null,\"userPrincipalName\":\"ada@example.org\"}}],\"@odata.nextLink\":\"http://{address}/v1.0/users?skip=1\"}}"
            ),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"displayName\":\"Grace Example\",\"mail\":\"grace@example.org\"}]}".to_string(),
        ];
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("client should connect");
                requests.push(read_request(&mut stream));
                stream
                    .write_all(response.as_bytes())
                    .expect("mock response should be written");
            }
            sender.send(requests).expect("requests should be observed");
        });
        let service = ExchangeService {
            server_url: format!("http://{address}/v1.0"),
            ..ExchangeService::graph("user@example.org", "token")
        };
        let contacts = service
            .search_gal_sync("Example")
            .expect("paginated GAL response should parse");
        let requests = receiver.recv().expect("requests should be captured");
        handle.join().expect("mock server should finish");
        assert_eq!(contacts.len(), 2);
        assert_eq!(contacts[0].email_address, "ada@example.org");
        assert_eq!(contacts[1].email_address, "grace@example.org");
        assert!(requests[0].contains("GET /v1.0/users?"));
        assert!(requests[1].contains("GET /v1.0/users?skip=1"));
    }

    #[test]
    fn graph_oof_response_maps_scheduled_dates() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"status\":\"scheduled\",\"scheduledStartDateTime\":{\"dateTime\":\"2026-08-11T00:00:00.0000000\",\"timeZone\":\"UTC\"},\"scheduledEndDateTime\":{\"dateTime\":\"2026-08-20T00:00:00.0000000\",\"timeZone\":\"UTC\"},\"internalReplyMessage\":\"Bin im Urlaub\",\"externalReplyMessage\":\"Out of office\"}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService::graph("user@example.org", "token");
        let service = ExchangeService {
            server_url: base_url,
            ..service
        };
        let settings = service
            .get_out_of_office_sync()
            .expect("OOF response should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        assert!(request.contains("GET /v1.0/me/mailboxSettings/automaticRepliesSetting"));
        assert_eq!(settings.start_date, "2026-08-11");
        assert_eq!(settings.end_date, "2026-08-20");
        assert!(settings.enabled);
        assert!(settings.use_schedule);
    }

    #[test]
    fn graph_calls_require_a_token_and_redact_it_from_debug() {
        let service = ExchangeService::new("https://graph.microsoft.com/v1.0", "user");
        assert_eq!(
            service.search_gal_sync("person"),
            Err(ExchangeError::MissingAccessToken)
        );
        assert_eq!(
            service.set_message_follow_up_state_sync(
                "message-id",
                ExchangeMailFollowUpState::FollowUp,
            ),
            Err(ExchangeError::MissingAccessToken)
        );
        let mut service = service;
        service.set_access_token("secret-token");
        let debug = format!("{service:?}");
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn graph_http_cancellation_drops_a_slow_in_flight_request_promptly() {
        let (base_url, request_seen, release_server, server_handle) = slow_mock_server();
        let requested = Arc::new(AtomicBool::new(false));
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        }
        .with_cancellation_flag(Arc::clone(&requested));
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = service.list_messages_cursor_sync("folder-id", None, 20);
            result_sender
                .send(result)
                .expect("request result should be observed");
        });

        request_seen
            .recv_timeout(Duration::from_secs(1))
            .expect("slow server should observe the request");
        let cancellation_started = Instant::now();
        requested.store(true, Ordering::Release);
        let result = result_receiver.recv_timeout(Duration::from_secs(1));
        release_server
            .send(())
            .expect("slow server should be released");
        worker.join().expect("Graph worker should finish");
        server_handle.join().expect("slow server should finish");

        assert_eq!(
            result.expect("cancellation should finish promptly"),
            Err(ExchangeError::Cancelled)
        );
        assert!(
            cancellation_started.elapsed() < Duration::from_secs(1),
            "cancellation took too long: {:?}",
            cancellation_started.elapsed()
        );
    }

    #[test]
    fn graph_mail_follow_up_flags_map_all_three_graph_states() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"value\":[",
            "{\"id\":\"none\",\"flag\":{\"flagStatus\":\"notFlagged\"}},",
            "{\"id\":\"follow-up\",\"flag\":{\"flagStatus\":\"flagged\"}},",
            "{\"id\":\"completed\",\"flag\":{\"flagStatus\":\"complete\"}}]}",
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let messages = service
            .list_messages_sync("folder-id", 20)
            .expect("Graph message flags should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(request.contains("GET /v1.0/me/mailFolders/folder-id/messages?"));
        assert!(request.contains("flag"));
        assert_eq!(
            messages
                .iter()
                .map(|message| message.follow_up_state)
                .collect::<Vec<_>>(),
            vec![
                ExchangeMailFollowUpState::None,
                ExchangeMailFollowUpState::FollowUp,
                ExchangeMailFollowUpState::Completed,
            ]
        );
        assert!(!messages[0].is_flagged);
        assert!(!messages[1].is_flagged);
        assert!(!messages[2].is_flagged);
    }

    #[test]
    fn graph_message_headers_form_a_safe_reusable_rfc822_block() {
        let block = graph_header_block(&[
            ("Subject".to_string(), "Roadmap".to_string()),
            ("X-Custom".to_string(), "line one\r\nline two".to_string()),
            ("Bad:Name".to_string(), "ignored".to_string()),
        ])
        .expect("valid Graph headers should encode");

        assert_eq!(
            String::from_utf8(block).expect("headers should remain UTF-8"),
            "Subject: Roadmap\r\nX-Custom: line one  line two\r\n"
        );
    }

    #[test]
    fn graph_selected_headers_reach_the_shared_mail_record() {
        let message: GraphMessage = serde_json::from_value(serde_json::json!({
            "id": "message-id",
            "subject": "Roadmap",
            "size": 4096,
            "internetMessageHeaders": [
                {"name": "Subject", "value": "Roadmap"},
                {"name": "X-Project", "value": "Evolution GTK4"}
            ]
        }))
        .expect("Graph message shape should parse");
        let parsed = message
            .into_public("folder-id".to_string())
            .into_parsed_mail("source-id", "Inbox");

        assert_eq!(
            parsed.raw_headers.as_deref(),
            Some(b"Subject: Roadmap\r\nX-Project: Evolution GTK4\r\n".as_slice())
        );
        assert_eq!(parsed.message_size, 4096);
    }

    #[test]
    fn graph_follow_up_patch_sends_payload_and_accepts_success() {
        let response = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        service
            .set_message_follow_up_state_sync("message/id", ExchangeMailFollowUpState::Completed)
            .expect("Graph follow-up PATCH should succeed");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(request.contains("PATCH /v1.0/me/messages/message%2Fid HTTP"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer token")
        );
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("request should contain a body separator");
        let body: serde_json::Value =
            serde_json::from_str(body).expect("follow-up PATCH body should be JSON");
        assert_eq!(
            body,
            serde_json::json!({"flag": {"flagStatus": "complete"}})
        );
    }

    #[test]
    fn graph_read_patch_does_not_overwrite_follow_up() {
        let response = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        service
            .set_message_read_state_sync("message-id", true)
            .expect("Graph read PATCH should succeed");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("request should contain a body separator");
        let body: serde_json::Value =
            serde_json::from_str(body).expect("read PATCH body should be JSON");

        assert_eq!(body, serde_json::json!({"isRead": true}));
        assert!(body.get("flag").is_none());
    }

    #[test]
    fn graph_follow_up_patch_preserves_structured_graph_errors() {
        let response = concat!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"error\":{\"code\":\"ErrorInvalidFlag\",\"message\":\"invalid status\"}}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let error = service
            .set_message_follow_up_state_sync("message-id", ExchangeMailFollowUpState::FollowUp)
            .expect_err("Graph follow-up PATCH should surface an HTTP error");
        requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert_eq!(
            error,
            ExchangeError::Http {
                status: 400,
                code: Some("ErrorInvalidFlag".to_string()),
                message: "invalid status".to_string(),
            }
        );
    }

    #[test]
    fn graph_follow_up_patch_honors_in_flight_cancellation() {
        let (base_url, request_seen, release_server, server_handle) = slow_mock_server();
        let requested = Arc::new(AtomicBool::new(false));
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        }
        .with_cancellation_flag(Arc::clone(&requested));
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = service.set_message_follow_up_state_sync(
                "message-id",
                ExchangeMailFollowUpState::FollowUp,
            );
            result_sender
                .send(result)
                .expect("request result should be observed");
        });

        request_seen
            .recv_timeout(Duration::from_secs(1))
            .expect("slow server should observe the PATCH request");
        requested.store(true, Ordering::Release);
        let result = result_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation should finish promptly");
        release_server
            .send(())
            .expect("slow server should be released");
        worker.join().expect("Graph worker should finish");
        server_handle.join().expect("slow server should finish");

        assert_eq!(result, Err(ExchangeError::Cancelled));
    }

    #[test]
    fn graph_http_cancellation_before_start_skips_transport() {
        let requested = Arc::new(AtomicBool::new(true));
        let service =
            ExchangeService::graph("user@example.org", "token").with_cancellation_flag(requested);

        assert_eq!(
            service.list_messages_cursor_sync("folder-id", None, 20),
            Err(ExchangeError::Cancelled)
        );
    }

    #[test]
    fn graph_http_errors_keep_code_and_drop_raw_body() {
        let response = concat!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"error\":{\"code\":\"InvalidAuthenticationToken\",\"message\":\"expired\"}}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService::graph("user", "token");
        let service = ExchangeService {
            server_url: base_url,
            ..service
        };
        let error = service
            .search_gal_sync("person")
            .expect_err("unauthorized response should fail");
        requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        assert_eq!(
            error,
            ExchangeError::Http {
                status: 401,
                code: Some("InvalidAuthenticationToken".to_string()),
                message: "expired".to_string(),
            }
        );
    }

    #[test]
    fn graph_oof_update_sends_only_fields_required_by_the_selected_mode() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"status\":\"alwaysEnabled\",\"internalReplyMessage\":\"Bin im Urlaub\",\"externalReplyMessage\":\"Out of office\"}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService::graph("user", "token");
        let service = ExchangeService {
            server_url: base_url,
            ..service
        };
        let settings = OutOfOfficeSettings {
            enabled: true,
            use_schedule: false,
            internal_message: "Bin im Urlaub".to_string(),
            external_message: "Out of office".to_string(),
            ..OutOfOfficeSettings::default()
        };
        let returned = service
            .set_out_of_office_sync(&settings)
            .expect("OOF update should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        assert!(request.contains("PATCH /v1.0/me/mailboxSettings/automaticRepliesSetting"));
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("request should contain a body separator");
        let body: serde_json::Value = serde_json::from_str(body).expect("request body is JSON");
        assert_eq!(body["status"], "alwaysEnabled");
        assert_eq!(body["internalReplyMessage"], "Bin im Urlaub");
        assert!(body.get("scheduledStartDateTime").is_none());
        assert!(body.get("scheduledEndDateTime").is_none());
        assert!(returned.enabled);
        assert!(!returned.use_schedule);
    }

    #[test]
    fn graph_mime_requests_base64_encode_the_complete_rfc822_payload() {
        let response = "HTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\n";
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };
        let raw = b"From: user@example.org\r\nSubject: Test\r\n\r\nHello";
        assert!(
            service
                .send_raw_message_sync(raw, false)
                .expect("Graph MIME request should succeed")
        );
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        assert!(request.contains("POST /v1.0/me/sendMail"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: text/plain")
        );
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("request should contain a body separator");
        assert_eq!(body, base64_encode(raw));
    }

    #[test]
    fn graph_message_source_uses_the_rfc822_value_endpoint() {
        let raw = "From: sender@example.org\r\nSubject: Original\r\n\r\nBody";
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: message/rfc822\r\n",
            "Connection: close\r\n\r\n",
            "From: sender@example.org\r\nSubject: Original\r\n\r\nBody"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let source = service
            .message_mime_sync("message/id")
            .expect("Graph should return the original MIME source");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert_eq!(source, raw.as_bytes());
        assert!(request.contains("GET /v1.0/me/messages/message%2Fid/$value"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("accept: message/rfc822")
        );
    }

    #[test]
    fn graph_contact_payload_uses_the_contact_email_shape() {
        let contact = ExchangeContact {
            id: String::new(),
            display_name: "Ada Example".to_string(),
            given_name: "Ada".to_string(),
            surname: "Example".to_string(),
            middle_name: String::new(),
            nickname: String::new(),
            title: String::new(),
            company_name: String::new(),
            department: String::new(),
            job_title: String::new(),
            office_location: String::new(),
            birthday: String::new(),
            personal_notes: String::new(),
            email_addresses: vec![ExchangeEmailAddress {
                name: "Ada Example".to_string(),
                address: "ada@example.org".to_string(),
            }],
            business_phones: Vec::new(),
            home_phones: Vec::new(),
            mobile_phone: String::new(),
            business_address: None,
            home_address: None,
            categories: Vec::new(),
        };
        let payload = contact_payload(&contact);
        assert_eq!(payload["emailAddresses"][0]["address"], "ada@example.org");
        assert!(payload["emailAddresses"][0].get("emailAddress").is_none());
    }

    #[test]
    fn graph_base_url_adds_v1_0_only_for_microsoft_host() {
        let graph = ExchangeService::new("https://graph.microsoft.com", "user");
        assert_eq!(
            graph.endpoint("users").unwrap().as_str(),
            "https://graph.microsoft.com/v1.0/users"
        );
        let custom = ExchangeService::new("https://exchange.example.org/api", "user");
        assert_eq!(
            custom.endpoint("users").unwrap().as_str(),
            "https://exchange.example.org/api/users"
        );
    }

    #[test]
    fn graph_mail_folder_tree_follows_pagination_and_child_folders() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener
            .local_addr()
            .expect("mock server address should exist");
        let (sender, receiver) = mpsc::channel();
        let responses = vec![
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{\"value\":[{{\"id\":\"inbox-id\",\"displayName\":\"Inbox\",\"childFolderCount\":1,\"totalItemCount\":4,\"unreadItemCount\":2}}],\"@odata.nextLink\":\"http://{address}/v1.0/me/mailFolders?skip=1\"}}"
            ),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"sent-id\",\"displayName\":\"Sent Items\",\"childFolderCount\":0,\"totalItemCount\":1,\"unreadItemCount\":0}]}".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"child-id\",\"displayName\":\"Receipts\",\"parentFolderId\":\"inbox-id\",\"childFolderCount\":0,\"totalItemCount\":1,\"unreadItemCount\":0}]}".to_string(),
        ];
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("client should connect");
                requests.push(read_request(&mut stream));
                stream
                    .write_all(response.as_bytes())
                    .expect("mock response should be written");
            }
            sender.send(requests).expect("requests should be observed");
        });
        let service = ExchangeService {
            server_url: format!("http://{address}/v1.0"),
            ..ExchangeService::graph("user@example.org", "token")
        };
        let folders = service
            .list_mail_folders_sync()
            .expect("folder hierarchy should load");
        let requests = receiver.recv().expect("requests should be captured");
        handle.join().expect("mock server should finish");
        assert_eq!(
            folders
                .iter()
                .map(|folder| folder.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Inbox", "Receipts", "Sent Items"]
        );
        assert!(requests[0].contains("GET /v1.0/me/mailFolders?"));
        assert!(requests[1].contains("GET /v1.0/me/mailFolders?skip=1"));
        assert!(requests[2].contains("GET /v1.0/me/mailFolders/inbox-id/childFolders?"));
    }

    #[test]
    fn graph_mail_calendar_and_contact_shapes_convert_to_existing_models() {
        let mail_response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"value\":[{\"id\":\"message-id\",\"parentFolderId\":\"folder-id\",\"internetMessageId\":\"<m@example.org>\",\"subject\":\"Hello\",\"sender\":{\"emailAddress\":{\"name\":\"Ada\",\"address\":\"ada@example.org\"}},\"from\":{\"emailAddress\":{\"name\":\"Ada\",\"address\":\"ada@example.org\"}},\"toRecipients\":[{\"emailAddress\":{\"name\":\"User\",\"address\":\"user@example.org\"}}],\"size\":8192,\"body\":{\"contentType\":\"html\",\"content\":\"<p>Safe</p><script>bad()</script>\"},\"bodyPreview\":\"Safe\",\"isRead\":false,\"flag\":{\"flagStatus\":\"flagged\"},\"importance\":\"high\",\"categories\":[\"Work\"]}]}"
        );
        let (base_url, requests, handle) = mock_server(mail_response);
        let service = ExchangeService::graph("user@example.org", "token");
        let service = ExchangeService {
            server_url: base_url,
            ..service
        };
        let messages = service
            .list_messages_sync("folder-id", 20)
            .expect("message response should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");
        let parsed = messages
            .into_iter()
            .next()
            .expect("one Graph message expected");
        assert!(request.contains("GET /v1.0/me/mailFolders/folder-id/messages?"));
        assert!(request.contains("internetMessageHeaders"));
        assert!(request.contains("size"));
        assert_eq!(parsed.folder_id, "folder-id");
        let parsed_mail = parsed.into_parsed_mail("account-id", "Inbox");
        assert_eq!(parsed_mail.sender_email, "ada@example.org");
        assert!(parsed_mail.html_body.contains("<p>Safe</p>"));
        assert!(!parsed_mail.html_body.contains("script"));
        assert!(parsed_mail.is_unread);
        assert!(!parsed_mail.is_flagged);
        assert_eq!(
            parsed_mail.follow_up_state,
            ExchangeMailFollowUpState::FollowUp
        );
        assert!(parsed_mail.is_important);
        assert_eq!(parsed_mail.message_size, 8192);

        let contact = ExchangeContact {
            id: "contact-id".to_string(),
            display_name: "Ada Example".to_string(),
            given_name: "Ada".to_string(),
            surname: "Example".to_string(),
            middle_name: String::new(),
            nickname: "A".to_string(),
            title: String::new(),
            company_name: "Example Ltd".to_string(),
            department: "Engineering".to_string(),
            job_title: "Lead".to_string(),
            office_location: String::new(),
            birthday: "1990-01-02".to_string(),
            personal_notes: "Note".to_string(),
            email_addresses: vec![ExchangeEmailAddress {
                name: "Ada Example".to_string(),
                address: "ada@example.org".to_string(),
            }],
            business_phones: vec!["+49 30".to_string()],
            home_phones: Vec::new(),
            mobile_phone: String::new(),
            business_address: None,
            home_address: None,
            categories: vec!["Work".to_string()],
        };
        let vcard = contact.to_vcard();
        let reparsed = crate::services::vcard_service::VCardContact::parse_vcard_string(&vcard)
            .expect("Graph contact should become a valid vCard");
        assert_eq!(reparsed.fn_name, "Ada Example");
        assert_eq!(
            reparsed.extension_values("X-EVOLUTION-GRAPH-ID"),
            vec!["contact-id"]
        );

        let event = ExchangeCalendarEvent {
            id: "event-id".to_string(),
            subject: "Planning".to_string(),
            body_html: "<p>Discuss</p>".to_string(),
            body_text: String::new(),
            start: ExchangeDateTime {
                date_time: "2026-08-12T09:00:00.0000000".to_string(),
                time_zone: "UTC".to_string(),
            },
            end: ExchangeDateTime {
                date_time: "2026-08-12T10:00:00.0000000".to_string(),
                time_zone: "UTC".to_string(),
            },
            location: "Berlin".to_string(),
            is_all_day: false,
            organizer: ExchangeEmailAddress {
                name: "Ada".to_string(),
                address: "ada@example.org".to_string(),
            },
            attendees: Vec::new(),
            show_as: "busy".to_string(),
            sensitivity: "normal".to_string(),
            web_link: String::new(),
            is_cancelled: false,
            categories: Vec::new(),
            is_recurring: false,
            recurrence_rule: None,
        };
        let record = event
            .into_calendar_event("account-id", "Microsoft 365")
            .expect("Graph event should become a calendar record");
        assert_eq!(record.uid, "event-id");
        assert_eq!(record.summary, "Planning");
        assert_eq!(record.location, "Berlin");
    }

    #[test]
    fn graph_next_link_must_keep_the_configured_origin() {
        let service = ExchangeService::graph("user@example.org", "token");
        assert!(
            service
                .validate_next_link("https://graph.microsoft.com/v1.0/me/messages?$skiptoken=abc")
                .is_ok()
        );
        assert!(
            service
                .validate_next_link("http://graph.microsoft.com/v1.0/me/messages?$skiptoken=abc")
                .is_err()
        );
        assert!(
            service
                .validate_next_link("https://attacker@example.com/v1.0/me/messages")
                .is_err()
        );
    }

    #[test]
    fn graph_mail_cursor_uses_validated_next_link_instead_of_numeric_skip() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener.local_addr().expect("mock address should exist");
        let (sender, receiver) = mpsc::channel();
        let first = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{\"value\":[],\"@odata.nextLink\":\"http://{address}/v1.0/messages-next?page=2\"}}"
        );
        let second = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[]}";
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [first.as_str(), second] {
                let (mut stream, _) = listener.accept().expect("client should connect");
                requests.push(read_request(&mut stream));
                stream
                    .write_all(response.as_bytes())
                    .expect("mock response should be written");
            }
            sender.send(requests).expect("requests should be observed");
        });
        let service = ExchangeService {
            server_url: format!("http://{address}/v1.0"),
            ..ExchangeService::graph("user@example.org", "token")
        };
        let (_, cursor) = service
            .list_messages_cursor_sync("folder-id", None, 20)
            .expect("first cursor page should load");
        let cursor = cursor.expect("next link should be retained");
        let (_, next) = service
            .list_messages_cursor_sync("folder-id", Some(&cursor), 20)
            .expect("cursor page should load");
        assert!(next.is_none());
        let requests = receiver.recv().expect("requests should be captured");
        handle.join().expect("mock server should finish");
        assert!(requests[0].contains("/v1.0/me/mailFolders/folder-id/messages?"));
        assert!(requests[1].contains("/v1.0/messages-next?page=2"));
        assert!(!requests[1].contains("skip"));
    }

    #[test]
    fn graph_message_search_uses_kql_and_preserves_folder_identity() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"value\":[{\"id\":\"message-id\",\"parentFolderId\":\"folder-id\",\"subject\":\"Roadmap\",\"sender\":{\"emailAddress\":{\"name\":\"Alice\",\"address\":\"alice@example.org\"}},\"from\":{\"emailAddress\":{\"name\":\"Alice\",\"address\":\"alice@example.org\"}},\"receivedDateTime\":\"2026-08-12T10:00:00Z\",\"isRead\":false}]}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };
        let result = service
            .search_messages_sync(
                Some("folder-id"),
                Some("Inbox"),
                "\"subject:roadmap\" AND \"from:alice@example.org\"",
                20,
            )
            .expect("Graph message search should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(request.contains("GET /v1.0/me/mailFolders/folder-id/messages?"));
        assert!(request.contains("%24search="));
        assert!(request.contains("subject%3Aroadmap"));
        assert!(result.is_complete);
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].folder_id, "folder-id");
        assert_eq!(result.messages[0].subject, "Roadmap");
    }

    #[test]
    fn graph_message_search_follows_next_link_within_limit_and_is_complete() {
        let responses = vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"first\"}],\"@odata.nextLink\":\"__BASE__/search-page-2\"}".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"second\"}]}".to_string(),
        ];
        let (base_url, requests, handle) = mock_server_sequence(responses);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let result = service
            .search_messages_sync(None, None, "\"roadmap\"", 5)
            .expect("Graph search should follow its continuation");
        let requests = requests.recv().expect("requests should be captured");
        handle.join().expect("mock server should finish");

        assert!(result.is_complete);
        assert!(result.next_cursor.is_none());
        assert_eq!(
            result
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains("GET /v1.0/search-page-2"));
    }

    #[test]
    fn graph_message_search_cursor_loads_first_and_following_pages() {
        let responses = vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"first\"}],\"@odata.nextLink\":\"__BASE__/search-page-2?skiptoken=opaque%2Bcursor\"}".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"second\",\"parentFolderId\":\"cursor-folder-id\"}]}".to_string(),
        ];
        let (base_url, requests, handle) = mock_server_sequence(responses);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let first = service
            .search_messages_cursor_sync(Some("folder-id"), Some("Inbox"), "\"roadmap\"", None, 1)
            .expect("first Graph search cursor page should load");
        assert!(!first.is_complete);
        let cursor = first
            .next_cursor
            .as_deref()
            .expect("first search page should expose its continuation cursor")
            .to_string();

        // A continuation is opaque: these values must not be validated,
        // folded into the URL, or used to rebuild the original search.
        let second = service
            .search_messages_cursor_sync(
                Some("folder with\ninvalid id"),
                Some("Not the original folder"),
                "\n",
                Some(&cursor),
                1,
            )
            .expect("continuation Graph search cursor page should load");
        assert!(second.is_complete);
        assert!(second.next_cursor.is_none());
        assert_eq!(first.messages[0].id, "first");
        assert_eq!(second.messages[0].id, "second");
        assert_eq!(second.messages[0].folder_id, "cursor-folder-id");

        let requests = requests.recv().expect("requests should be captured");
        handle.join().expect("mock server should finish");
        assert!(requests[0].contains("GET /v1.0/me/mailFolders/folder-id/messages?"));
        assert!(requests[0].contains("%24search="));
        assert!(requests[1].contains("GET /v1.0/search-page-2?skiptoken=opaque%2Bcursor"));
        assert!(!requests[1].contains("mailFolders"));
        assert!(!requests[1].contains("%24search"));
    }

    #[test]
    fn graph_message_search_cursor_rejects_a_foreign_origin_before_transport() {
        let service = ExchangeService::graph("user@example.org", "token");

        let error = service
            .search_messages_cursor_sync(
                Some("folder\nnot-used"),
                Some("not-used"),
                "\n",
                Some("https://attacker.example/v1.0/me/messages?skiptoken=opaque"),
                20,
            )
            .expect_err("foreign Graph cursor origin must be rejected");
        assert!(matches!(error, ExchangeError::InvalidUrl(_)));
    }

    #[test]
    fn graph_message_search_cursor_rejects_a_foreign_origin_from_graph() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"first\"}],\"@odata.nextLink\":\"https://attacker.example/v1.0/me/messages?skiptoken=opaque\"}";
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let error = service
            .search_messages_cursor_sync(None, None, "\"roadmap\"", None, 20)
            .expect_err("foreign Graph continuation origin must be rejected");
        requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(matches!(error, ExchangeError::InvalidUrl(_)));
    }

    #[test]
    fn graph_message_search_cursor_drops_next_cursor_on_page_truncation() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"first\"},{\"id\":\"second\"}],\"@odata.nextLink\":\"__BASE__/search-page-2\"}".to_string();
        let (base_url, requests, handle) = mock_server_sequence(vec![response]);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let result = service
            .search_messages_cursor_sync(None, None, "\"roadmap\"", None, 1)
            .expect("Graph search should return the bounded page");
        requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(!result.is_complete);
        assert!(result.next_cursor.is_none());
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].id, "first");
    }

    #[test]
    fn graph_message_search_reports_an_incomplete_limit_with_remaining_next_link() {
        let responses = vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"first\"}],\"@odata.nextLink\":\"__BASE__/search-page-2\"}".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"second\"}],\"@odata.nextLink\":\"__BASE__/search-page-3\"}".to_string(),
        ];
        let (base_url, requests, handle) = mock_server_sequence(responses);
        let expected_cursor = format!("{base_url}/search-page-3");
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let result = service
            .search_messages_sync(None, None, "\"roadmap\"", 2)
            .expect("Graph search should return its bounded result");
        let requests = requests.recv().expect("requests should be captured");
        handle.join().expect("mock server should finish");

        assert!(!result.is_complete);
        assert_eq!(
            result.next_cursor.as_deref(),
            Some(expected_cursor.as_str())
        );
        assert_eq!(result.messages.len(), 2);
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn graph_message_search_reports_page_truncation_without_next_link() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"value\":[{\"id\":\"first\"},{\"id\":\"second\"},{\"id\":\"third\"}]}";
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let result = service
            .search_messages_sync(None, None, "\"roadmap\"", 2)
            .expect("Graph search should return the bounded page");
        requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(!result.is_complete);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[1].id, "second");
    }

    #[test]
    fn graph_message_search_rejects_an_empty_query_before_transport() {
        let service = ExchangeService::graph("user@example.org", "token");

        assert!(matches!(
            service.search_messages_sync(None, None, "  ", 20),
            Err(ExchangeError::InvalidQuery(_))
        ));
    }

    #[test]
    fn graph_message_copy_posts_destination_and_returns_new_message_id() {
        let response = concat!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\n",
            "Connection: close\r\n\r\n",
            "{\"id\":\"copied-message-id\",\"parentFolderId\":\"archive-id\",\"internetMessageId\":\"<copy@example.org>\",\"subject\":\"Copied\"}"
        );
        let (base_url, requests, handle) = mock_server(response);
        let service = ExchangeService {
            server_url: base_url,
            ..ExchangeService::graph("user@example.org", "token")
        };

        let copied = service
            .copy_message_to_folder_sync("original/message", "archive-id")
            .expect("Graph message copy should parse");
        let request = requests.recv().expect("request should be captured");
        handle.join().expect("mock server should finish");

        assert!(request.contains("POST /v1.0/me/messages/original%2Fmessage/copy"));
        assert!(request.contains("\"destinationId\":\"archive-id\""));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer token")
        );
        assert_eq!(copied.id, "copied-message-id");
        assert_eq!(copied.folder_id, "archive-id");
        assert_eq!(
            copied.internet_message_id.as_deref(),
            Some("<copy@example.org>")
        );
    }

    #[test]
    fn graph_recurrence_payload_contains_required_range_and_pattern_fields() {
        let payload = graph_recurrence_payload(
            "FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4".to_string(),
            "20260812T090000",
        )
        .expect("weekly recurrence should convert");
        assert_eq!(payload["pattern"]["type"], "weekly");
        assert_eq!(payload["pattern"]["daysOfWeek"][0], "monday");
        assert_eq!(payload["pattern"]["firstDayOfWeek"], "monday");
        assert_eq!(payload["range"]["type"], "numbered");
        assert_eq!(payload["range"]["startDate"], "2026-08-12");
        assert_eq!(payload["range"]["numberOfOccurrences"], 4);
    }

    #[test]
    fn graph_recurrence_shape_roundtrips_to_rfc5545_rule() {
        let recurrence = serde_json::json!({
            "pattern": {
                "type": "absoluteMonthly",
                "interval": 2,
                "dayOfMonth": 15
            },
            "range": {
                "type": "endDate",
                "startDate": "2026-01-15",
                "endDate": "2026-12-31"
            }
        });
        assert_eq!(
            graph_recurrence_rule(&recurrence).as_deref(),
            Some("FREQ=MONTHLY;BYMONTHDAY=15;INTERVAL=2;UNTIL=20261231")
        );
    }
}
