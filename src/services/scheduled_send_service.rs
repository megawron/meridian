// SPDX-License-Identifier: GPL-3.0-or-later
//
// ScheduledSendService — Persistent "Send Later" queue and background dispatcher.

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

use super::mail_composer_service::OutgoingMessage;

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScheduledSendEntry {
    pub id: String,
    pub deliver_at_utc: DateTime<Utc>,
    pub created_at_utc: DateTime<Utc>,
    pub outgoing: OutgoingMessage,
    #[serde(default)]
    pub preferred_sent_folder: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduledSendFile {
    version: u32,
    #[serde(default)]
    entries: Vec<ScheduledSendEntry>,
}

#[derive(Debug, Clone)]
pub struct ScheduledSendStore {
    path: PathBuf,
}

impl Default for ScheduledSendStore {
    fn default() -> Self {
        let path = std::env::var_os("MERIDIAN_SCHEDULED_SEND_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| super::app_data_dir().join("scheduled-send.json"));
        Self { path }
    }
}

impl ScheduledSendStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn load(&self) -> Vec<ScheduledSendEntry> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(parsed) = serde_json::from_slice::<ScheduledSendFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring unreadable scheduled-send file");
            return Vec::new();
        };
        if parsed.version != CURRENT_VERSION {
            return Vec::new();
        }
        parsed.entries
    }

    fn save(&self, entries: &[ScheduledSendEntry]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_vec_pretty(&ScheduledSendFile {
            version: CURRENT_VERSION,
            entries: entries.to_vec(),
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, payload)?;
        fs::rename(tmp, &self.path)
    }

    pub(crate) fn enqueue(
        &self,
        outgoing: OutgoingMessage,
        deliver_at_utc: DateTime<Utc>,
        preferred_sent_folder: Option<String>,
    ) -> io::Result<ScheduledSendEntry> {
        let mut entries = self.load();
        let entry = ScheduledSendEntry {
            id: uuid::Uuid::new_v4().to_string(),
            deliver_at_utc,
            created_at_utc: Utc::now(),
            outgoing,
            preferred_sent_folder,
            attempts: 0,
            last_error: None,
        };
        entries.push(entry.clone());
        entries.sort_by_key(|e| e.deliver_at_utc);
        self.save(&entries)?;
        Ok(entry)
    }

    pub(crate) fn cancel(&self, id: &str) -> io::Result<Option<ScheduledSendEntry>> {
        let mut entries = self.load();
        let pos = entries.iter().position(|e| e.id == id);
        let removed = pos.map(|idx| entries.remove(idx));
        if removed.is_some() {
            self.save(&entries)?;
        }
        Ok(removed)
    }

    pub(crate) fn due_entries(&self, now_utc: DateTime<Utc>) -> Vec<ScheduledSendEntry> {
        self.load()
            .into_iter()
            .filter(|e| e.deliver_at_utc <= now_utc && e.attempts < 5)
            .collect()
    }

    pub fn mark_delivered(&self, id: &str) -> io::Result<()> {
        let _ = self.cancel(id)?;
        Ok(())
    }

    pub fn mark_failed(&self, id: &str, error: String) -> io::Result<()> {
        let mut entries = self.load();
        if let Some(item) = entries.iter_mut().find(|e| e.id == id) {
            item.attempts = item.attempts.saturating_add(1);
            item.last_error = Some(error);
            self.save(&entries)?;
        }
        Ok(())
    }
}

/// Convenient human presets for "Schedule Send…".
pub fn schedule_presets_utc() -> Vec<(String, DateTime<Utc>)> {
    let now_local = Local::now();
    let today = now_local.date_naive();
    let tomorrow = today + Duration::days(1);

    let date_fmt_kind = crate::services::settings_service::AppSettings::try_new()
        .map(|s| s.resolved_date_format())
        .unwrap_or_else(|| {
            crate::services::settings_service::AppSettings::system_locale_date_format()
                .to_string()
        });
    let prefers_12h = crate::services::settings_service::AppSettings::try_new()
        .map(|s| s.resolved_time_format() == "12h")
        .unwrap_or_else(crate::services::settings_service::AppSettings::system_prefers_12h);
    let time_fmt = if prefers_12h { "%-I:%M %p" } else { "%H:%M" };
    let date_fmt = match date_fmt_kind.as_str() {
        "german" => "%d.%m.%Y",
        "us" => "%m/%d/%Y",
        _ => "%Y-%m-%d",
    };

    let mut presets = Vec::new();

    if now_local.time() < NaiveTime::from_hms_opt(16, 30, 0).unwrap() {
        if let Some(dt) = today
            .and_hms_opt(17, 0, 0)
            .and_then(|ndt| Local.from_local_datetime(&ndt).single())
        {
            presets.push((
                format!(
                    "{} ({})",
                    crate::i18n::gettext("Later Today"),
                    dt.format(time_fmt)
                ),
                dt.with_timezone(&Utc),
            ));
        }
    }

    if let Some(dt) = tomorrow
        .and_hms_opt(8, 0, 0)
        .and_then(|ndt| Local.from_local_datetime(&ndt).single())
    {
        presets.push((
            format!(
                "{} ({}, {})",
                crate::i18n::gettext("Tomorrow Morning"),
                dt.format(date_fmt),
                dt.format(time_fmt)
            ),
            dt.with_timezone(&Utc),
        ));
    }

    if let Some(dt) = tomorrow
        .and_hms_opt(13, 0, 0)
        .and_then(|ndt| Local.from_local_datetime(&ndt).single())
    {
        presets.push((
            format!(
                "{} ({}, {})",
                crate::i18n::gettext("Tomorrow Afternoon"),
                dt.format(date_fmt),
                dt.format(time_fmt)
            ),
            dt.with_timezone(&Utc),
        ));
    }

    let days_until_monday = match today.weekday() {
        Weekday::Mon => 7,
        Weekday::Tue => 6,
        Weekday::Wed => 5,
        Weekday::Thu => 4,
        Weekday::Fri => 3,
        Weekday::Sat => 2,
        Weekday::Sun => 1,
    };
    let next_monday = today + Duration::days(days_until_monday);
    if let Some(dt) = next_monday
        .and_hms_opt(8, 0, 0)
        .and_then(|ndt| Local.from_local_datetime(&ndt).single())
    {
        presets.push((
            format!(
                "{} ({}, {})",
                crate::i18n::gettext("Monday Morning"),
                dt.format(date_fmt),
                dt.format(time_fmt)
            ),
            dt.with_timezone(&Utc),
        ));
    }

    presets
}

/// Worker-thread helper: dispatches due scheduled emails through the configured
/// account transport and returns subjects of successfully sent messages.
pub fn dispatch_due_scheduled_messages(store: &ScheduledSendStore) -> Vec<String> {
    let due = store.due_entries(Utc::now());
    let mut delivered_subjects = Vec::new();
    for item in due {
        let send_result = deliver_outgoing(&item.outgoing, item.preferred_sent_folder.as_deref());
        match send_result {
            Ok(()) => {
                let _ = store.mark_delivered(&item.id);
                delivered_subjects.push(item.outgoing.subject.clone());
            }
            Err(err) => {
                tracing::warn!(id = %item.id, error = %err, "Scheduled send attempt failed");
                let _ = store.mark_failed(&item.id, err);
            }
        }
    }
    delivered_subjects
}

fn deliver_outgoing(
    outgoing: &OutgoingMessage,
    _preferred_sent_folder: Option<&str>,
) -> Result<(), String> {
    let raw = super::mail_composer_service::build_rfc822(outgoing)?;
    super::camel_mail_service::send_message(&outgoing.source_uid, &raw).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_outgoing() -> OutgoingMessage {
        OutgoingMessage {
            source_uid: "local".to_string(),
            from: "sender@example.org".to_string(),
            reply_to: String::new(),
            request_read_receipt: false,
            to: "recipient@example.org".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Scheduled Hello".to_string(),
            html_body: "<p>Hi</p>".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        }
    }

    #[test]
    fn scheduled_messages_enqueue_due_and_cancel_cleanly() {
        let path =
            std::env::temp_dir().join(format!("meridian-sched-{}.json", uuid::Uuid::new_v4()));
        let store = ScheduledSendStore::new(&path);
        let past = Utc::now() - Duration::minutes(2);
        let future = Utc::now() + Duration::hours(4);

        let due_entry = store.enqueue(sample_outgoing(), past, None).unwrap();
        let future_entry = store.enqueue(sample_outgoing(), future, None).unwrap();

        let due_now = store.due_entries(Utc::now());
        assert_eq!(due_now.len(), 1);
        assert_eq!(due_now[0].id, due_entry.id);

        store.mark_delivered(&due_entry.id).unwrap();
        assert!(store.due_entries(Utc::now()).is_empty());

        let cancelled = store.cancel(&future_entry.id).unwrap();
        assert!(cancelled.is_some());
        assert!(store.load().is_empty());
        let _ = fs::remove_file(path);
    }
}
