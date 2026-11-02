// SPDX-License-Identifier: GPL-3.0-or-later
//
// Local iCalendar store for standalone .ics/.vcs sources.  ECal remains the
// authoritative synchronized backend; explicitly selected external folders
// stay read-only, while the app-owned fallback can persist local components.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ical_time_service::{property_local_display, property_local_timestamp};
use super::reminder_service::{Alarm, parse_alarms, replace_component_alarms};

static ATOMIC_WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static CALENDAR_MUTATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEventRecord {
    pub uid: String,
    pub source_uid: String,
    pub source_display_name: String,
    pub component: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub location: String,
    pub description: String,
    pub all_day: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub uid: String,
    pub source_uid: String,
    pub source_display_name: String,
    pub component: String,
    pub summary: String,
    pub due: String,
    pub priority: u8,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoRecord {
    pub uid: String,
    pub source_uid: String,
    pub source_display_name: String,
    pub component: String,
    pub summary: String,
    pub description: String,
    pub updated: String,
}

/// One RFC5545 ATTACH value as a GTK-free value object.
///
/// URI values and inline values are deliberately kept as text.  The portable
/// editor never follows a URI, opens a file, decodes an inline payload, or
/// otherwise interprets untrusted attachment content.  `unknown_parameters`
/// contains parameters other than the two fields exposed by the editor;
/// their source order is retained internally for lossless serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoAttachment {
    pub uri: Option<String>,
    pub inline_content: Option<String>,
    pub filename: Option<String>,
    pub fmttype: Option<String>,
    pub unknown_parameters: Vec<MemoAttachmentParameter>,
    raw_property: Option<String>,
    raw_property_lines: Vec<String>,
    raw_parameters: Vec<String>,
    source_signature: Option<MemoAttachmentSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoAttachmentParameter {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoAttachmentSignature {
    uri: Option<String>,
    inline_content: Option<String>,
    filename: Option<String>,
    fmttype: Option<String>,
    unknown_parameters: Vec<MemoAttachmentParameter>,
}

impl MemoAttachment {
    pub fn uri(value: impl Into<String>) -> Self {
        Self {
            uri: Some(value.into()),
            inline_content: None,
            filename: None,
            fmttype: None,
            unknown_parameters: Vec::new(),
            raw_property: None,
            raw_property_lines: Vec::new(),
            raw_parameters: Vec::new(),
            source_signature: None,
        }
    }

    pub fn inline(value: impl Into<String>) -> Self {
        Self {
            uri: None,
            inline_content: Some(value.into()),
            filename: None,
            fmttype: None,
            unknown_parameters: Vec::new(),
            raw_property: None,
            raw_property_lines: Vec::new(),
            raw_parameters: Vec::new(),
            source_signature: None,
        }
    }

    pub fn is_inline(&self) -> bool {
        self.inline_content.is_some()
    }

    pub fn is_uri(&self) -> bool {
        self.uri.is_some()
    }

    fn signature(&self) -> MemoAttachmentSignature {
        MemoAttachmentSignature {
            uri: self.uri.clone(),
            inline_content: self.inline_content.clone(),
            filename: self.filename.clone(),
            fmttype: self.fmttype.clone(),
            unknown_parameters: self.unknown_parameters.clone(),
        }
    }

    fn is_unchanged(&self) -> bool {
        self.raw_property.is_some()
            && self
                .source_signature
                .as_ref()
                .is_some_and(|signature| *signature == self.signature())
    }
}

/// GTK-free, staged VJOURNAL editor state.
///
/// The original component is copied on construction.  `apply` starts from
/// that copy and changes only the managed fields whose typed values changed;
/// unknown top-level properties, X-properties, their parameters and nested
/// VALARM blocks therefore remain untouched unless the caller explicitly
/// changes the corresponding attachment/alarm collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoEditorDraft {
    pub uid: String,
    pub summary: String,
    pub dtstart: Option<String>,
    pub class: Option<String>,
    pub status: Option<String>,
    pub url: Option<String>,
    pub categories: Vec<String>,
    pub description: String,
    pub attachments: Vec<MemoAttachment>,
    pub alarms: Vec<Alarm>,
    original_component: String,
    original_summary: Option<String>,
    original_dtstart: Option<String>,
    original_class: Option<String>,
    original_status: Option<String>,
    original_url: Option<String>,
    original_categories: Vec<String>,
    original_description: Option<String>,
    original_attachments: Vec<MemoAttachment>,
    original_alarms: Vec<Alarm>,
}

impl MemoEditorDraft {
    pub fn parse(component: &str) -> Result<Self, String> {
        Self::from_component(component)
    }

    pub fn from_component(component: &str) -> Result<Self, String> {
        if component_kind(component) != Some("VJOURNAL") {
            return Err("Die Komponente ist kein VJOURNAL".to_string());
        }

        let properties = top_level_properties(component);
        let first = |name: &str| {
            properties
                .iter()
                .find(|(property, _, _)| property.eq_ignore_ascii_case(name))
                .map(|(_, value, _)| unescape(value))
        };
        let categories = properties
            .iter()
            .filter(|(property, _, _)| property.eq_ignore_ascii_case("CATEGORIES"))
            .flat_map(|(_, value, _)| split_ical_list(value))
            .collect::<Vec<_>>();
        let attachments = top_level_attachment_properties(component)
            .into_iter()
            .filter_map(|line| parse_memo_attachment(&line))
            .collect::<Vec<_>>();
        let memo_component = first_primary_component(component);
        let alarms = memo_component
            .as_deref()
            .map(parse_alarms)
            .unwrap_or_default();
        let summary = first("SUMMARY").unwrap_or_default();
        let dtstart = first("DTSTART");
        let class = first("CLASS");
        let status = first("STATUS");
        let url = first("URL");
        let description = first("DESCRIPTION").unwrap_or_default();
        let uid = first("UID").unwrap_or_default();

        Ok(Self {
            uid,
            summary: summary.clone(),
            dtstart: dtstart.clone(),
            class: class.clone(),
            status: status.clone(),
            url: url.clone(),
            categories: categories.clone(),
            description: description.clone(),
            attachments: attachments.clone(),
            alarms: alarms.clone(),
            original_component: component.to_string(),
            original_summary: first("SUMMARY"),
            original_dtstart: dtstart,
            original_class: class,
            original_status: status,
            original_url: url,
            original_categories: categories,
            original_description: first("DESCRIPTION"),
            original_attachments: attachments,
            original_alarms: alarms,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err("Eine Notiz benötigt einen Titel".to_string());
        }
        validate_single_line("Summary", &self.summary)?;
        for (label, value) in [
            ("DTSTART", self.dtstart.as_deref()),
            ("CLASS", self.class.as_deref()),
            ("STATUS", self.status.as_deref()),
            ("URL", self.url.as_deref()),
        ] {
            if let Some(value) = value {
                validate_single_line(label, value)?;
                if label == "DTSTART" && !value.trim().is_empty() {
                    validate_dtstart_value(value)?;
                }
            }
        }
        for category in &self.categories {
            validate_single_line("Kategorie", category)?;
        }
        for attachment in &self.attachments {
            if attachment.uri.is_some() == attachment.inline_content.is_some() {
                return Err(
                    "Jeder Anhang muss genau eine URI oder einen Inline-Inhalt besitzen"
                        .to_string(),
                );
            }
            if let Some(uri) = attachment.uri.as_deref() {
                validate_single_line("Anhang-URI", uri)?;
            }
            if let Some(content) = attachment.inline_content.as_deref() {
                validate_single_line("Inline-Anhang", content)?;
            }
            if let Some(filename) = attachment.filename.as_deref() {
                validate_single_line("Anhang-Dateiname", filename)?;
            }
            if let Some(fmttype) = attachment.fmttype.as_deref() {
                validate_single_line("Anhang-FMTTYPE", fmttype)?;
            }
            for parameter in &attachment.unknown_parameters {
                validate_ical_token("Anhang-Parametername", &parameter.name)?;
                if let Some(value) = parameter.value.as_deref() {
                    validate_single_line("Anhang-Parameterwert", value)?;
                }
            }
        }
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.summary != self.original_summary.clone().unwrap_or_default()
            || self.dtstart != self.original_dtstart
            || self.class != self.original_class
            || self.status != self.original_status
            || self.url != self.original_url
            || self.categories != self.original_categories
            || self.description != self.original_description.clone().unwrap_or_default()
            || self.attachments != self.original_attachments
            || self.alarms != self.original_alarms
    }

    pub fn apply(&self) -> Result<String, String> {
        self.validate()?;
        if !self.is_dirty() {
            return Ok(self.original_component.clone());
        }

        let mut component = self.original_component.clone();
        if self.summary != self.original_summary.clone().unwrap_or_default() {
            component = update_ical_property(
                &component,
                "SUMMARY",
                Some(&escape_ical_text_value(&self.summary)),
            );
        }
        if self.dtstart != self.original_dtstart {
            let dtstart = self
                .dtstart
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(strip_line_breaks);
            if let Some(dtstart) = dtstart.as_deref() {
                let original_prefix = top_level_properties(&component)
                    .into_iter()
                    .find(|(property, _, _)| property.eq_ignore_ascii_case("DTSTART"))
                    .map(|(_, _, prefix)| prefix);
                let original_is_date = original_prefix
                    .as_deref()
                    .is_some_and(|prefix| property_has_parameter(prefix, "VALUE", "DATE"));
                if original_is_date != is_ical_date(dtstart) {
                    return Err(
                        "DTSTART darf beim Bearbeiten nicht zwischen DATE und Datum/Zeit wechseln"
                            .to_string(),
                    );
                }
                if !original_is_date && !is_ical_date_time(dtstart) {
                    return Err("DTSTART benötigt einen Datum/Zeit-Wert".to_string());
                }
            }
            component = update_ical_property(&component, "DTSTART", dtstart.as_deref());
        }
        if self.class != self.original_class {
            let class = self
                .class
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(escape_ical_text_value);
            component = update_ical_property(&component, "CLASS", class.as_deref());
        }
        if self.status != self.original_status {
            let status = self
                .status
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(escape_ical_text_value);
            component = update_ical_property(&component, "STATUS", status.as_deref());
        }
        if self.url != self.original_url {
            let url = self
                .url
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(strip_line_breaks);
            component = update_ical_property(&component, "URL;VALUE=URI", url.as_deref());
        }
        if self.categories != self.original_categories {
            let categories = serialize_ical_categories(&self.categories);
            component = update_ical_property(
                &component,
                "CATEGORIES",
                (!categories.is_empty()).then_some(categories.as_str()),
            );
        }
        if self.description != self.original_description.clone().unwrap_or_default() {
            let description = (!self.description.trim().is_empty())
                .then(|| escape_ical_text_value(&self.description));
            component = update_ical_property(&component, "DESCRIPTION", description.as_deref());
        }
        if self.attachments != self.original_attachments {
            component = replace_ical_memo_attachments(&component, &self.attachments);
        }
        if self.alarms != self.original_alarms {
            component = replace_memo_component_alarms(&component, &self.alarms);
        }
        Ok(component)
    }

    pub fn apply_to_component(&self) -> Result<String, String> {
        self.apply()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSnapshot {
    pub events: Vec<CalendarEventRecord>,
    pub tasks: Vec<TaskRecord>,
    pub memos: Vec<MemoRecord>,
}

impl CalendarSnapshot {
    /// Return all event records that overlap with the specified date range.
    /// Dates can be YYYY-MM-DD or RFC3339/ISO prefixes.
    pub fn events_in_range<'a>(&'a self, start_date: &str, end_date: &str) -> Vec<&'a CalendarEventRecord> {
        self.events
            .iter()
            .filter(|ev| {
                if !ev.start.is_empty() && !end_date.is_empty() && ev.start.as_str() > end_date {
                    return false;
                }
                if !ev.end.is_empty() && !start_date.is_empty() && ev.end.as_str() < start_date {
                    return false;
                }
                true
            })
            .collect()
    }

    /// Return all event records matching a specific source UID without cloning the entire snapshot.
    pub fn events_for_source<'a>(&'a self, source_uid: &str) -> Vec<&'a CalendarEventRecord> {
        self.events
            .iter()
            .filter(|ev| ev.source_uid == source_uid)
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarPurgeResult {
    pub removed_count: usize,
    pub skipped_recurring_count: usize,
    pub skipped_invalid_count: usize,
}

#[derive(Debug)]
pub struct CalendarPurgeFailure {
    pub confirmed: CalendarPurgeResult,
    pub error: io::Error,
}

impl std::fmt::Display for CalendarPurgeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for CalendarPurgeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarPurgeDisposition {
    Remove,
    Keep,
    Recurring,
    Invalid,
}

#[derive(Debug, Clone)]
pub struct CalendarStoreService {
    calendar_dir: PathBuf,
    writable: bool,
}

impl CalendarStoreService {
    pub fn new() -> Self {
        let (calendar_dir, writable) =
            if let Some(path) = std::env::var_os("MERIDIAN_ICAL_DIR").or_else(|| std::env::var_os("EVOLUTION_GTK4_ICAL_DIR")).map(PathBuf::from) {
                (path, false)
            } else if let Some(path) = crate::services::settings_service::AppSettings::try_new()
                .map(|settings| settings.local_calendar_directory())
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
                .map(|path| {
                    let writable = crate::services::is_app_owned_store_path(&path);
                    (path, writable)
                })
            {
                path
            } else {
                (crate::services::app_data_dir().join("ical"), true)
            };
        Self {
            calendar_dir,
            writable,
        }
    }

    #[allow(dead_code)]
    pub fn with_calendar_dir(calendar_dir: impl Into<PathBuf>) -> Self {
        Self {
            calendar_dir: calendar_dir.into(),
            writable: false,
        }
    }

    pub fn with_writable_calendar_dir(calendar_dir: impl Into<PathBuf>) -> Self {
        Self {
            calendar_dir: calendar_dir.into(),
            writable: true,
        }
    }

    #[allow(dead_code)]
    pub fn calendar_dir(&self) -> &Path {
        &self.calendar_dir
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn parse_ical_file(path: &Path) -> CalendarSnapshot {
        let mut snapshot = CalendarSnapshot::default();
        if let Ok(raw) = fs::read_to_string(path) {
            parse_ical(&raw, path, "local-calendar", "Lokaler Kalender", &mut snapshot);
            sort_snapshot(&mut snapshot);
        }
        snapshot
    }

    /// Copy user-selected iCalendar files into the configured local source.
    /// Existing files are never replaced, so repeating an import is safe.
    pub fn import_files(&self, files: &[PathBuf]) -> io::Result<usize> {
        fs::create_dir_all(&self.calendar_dir)?;
        let existing_snapshot = self.load_snapshot();
        let mut existing_uids: std::collections::HashSet<String> = existing_snapshot
            .events
            .iter()
            .map(|e| e.uid.clone())
            .chain(existing_snapshot.tasks.iter().map(|t| t.uid.clone()))
            .chain(existing_snapshot.memos.iter().map(|m| m.uid.clone()))
            .filter(|uid| !uid.is_empty())
            .collect();
        let mut imported = 0;
        for source in files {
            if !source.is_file()
                || !source
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "ics" | "ical" | "ifb" | "vcs"
                        )
                    })
            {
                continue;
            }
            let parsed = Self::parse_ical_file(source);
            let file_uids: Vec<String> = parsed
                .events
                .iter()
                .map(|e| e.uid.clone())
                .chain(parsed.tasks.iter().map(|t| t.uid.clone()))
                .chain(parsed.memos.iter().map(|m| m.uid.clone()))
                .filter(|uid| !uid.is_empty())
                .collect();
            if !file_uids.is_empty() && file_uids.iter().all(|uid| existing_uids.contains(uid)) {
                continue;
            }
            let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let destination = unique_destination(&self.calendar_dir, file_name);
            fs::copy(source, destination)?;
            existing_uids.extend(file_uids);
            imported += 1;
        }
        Ok(imported)
    }

    pub fn load_snapshot(&self) -> CalendarSnapshot {
        let mut paths = Vec::new();
        collect_calendar_files(&self.calendar_dir, &mut paths);
        paths.sort();

        let mut snapshot = CalendarSnapshot::default();
        for path in paths {
            let Ok(raw) = fs::read_to_string(&path) else {
                tracing::warn!(path = %path.display(), "Could not read local iCalendar file");
                continue;
            };
            parse_ical(
                &raw,
                &path,
                if self.writable { "local-calendar" } else { "" },
                if self.writable {
                    "Lokaler Kalender"
                } else {
                    ""
                },
                &mut snapshot,
            );
        }
        sort_snapshot(&mut snapshot);
        snapshot
    }

    pub fn create_component(&self, component: &str) -> io::Result<(String, String)> {
        self.ensure_writable()?;
        let _mutation_guard = lock_calendar_mutations()?;
        let kind = component_kind(component).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültige iCalendar-Komponente",
            )
        })?;
        let (uid, component) = ensure_component_uid(component, kind, None);
        let path = self.calendar_file_path();
        let _file_lock = CalendarFileLock::acquire(&path)?;
        let mut calendar = read_calendar_file(&path)?;
        insert_calendar_component(&mut calendar, &component)?;
        write_atomic_locked(&path, calendar.as_bytes())?;
        Ok((uid, component))
    }

    pub fn modify_component(&self, uid: &str, component: &str) -> io::Result<(String, String)> {
        self.ensure_writable()?;
        let _mutation_guard = lock_calendar_mutations()?;
        let kind = component_kind(component).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültige iCalendar-Komponente",
            )
        })?;
        let (component_uid, component) = ensure_component_uid(component, kind, Some(uid));
        if component_uid != uid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Komponenten-UID stimmt nicht mit dem Ziel überein",
            ));
        }
        for path in self.mutable_calendar_files() {
            let _file_lock = CalendarFileLock::acquire(&path)?;
            let Ok(calendar) = fs::read_to_string(&path) else {
                continue;
            };
            if !calendar_contains_component(&calendar, &path, uid) {
                continue;
            }
            let updated = replace_calendar_component(&calendar, &path, uid, &component)?;
            write_atomic_locked(&path, updated.as_bytes())?;
            return Ok((component_uid, component));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("iCalendar-Komponente nicht gefunden: {uid}"),
        ))
    }

    pub fn remove_component(&self, uid: &str) -> io::Result<String> {
        self.ensure_writable()?;
        let _mutation_guard = lock_calendar_mutations()?;
        for path in self.mutable_calendar_files() {
            let _file_lock = CalendarFileLock::acquire(&path)?;
            let Ok(calendar) = fs::read_to_string(&path) else {
                continue;
            };
            if !calendar_contains_component(&calendar, &path, uid) {
                continue;
            }
            let updated = remove_calendar_component(&calendar, &path, uid)?;
            write_atomic_locked(&path, updated.as_bytes())?;
            return Ok(uid.to_string());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("iCalendar-Komponente nicht gefunden: {uid}"),
        ))
    }

    /// Remove only app-owned, non-recurring VEVENTs which are proven to end
    /// before `cutoff`.  Each backing file is rewritten once, so cancellation
    /// or validation failure before that rewrite leaves the complete file
    /// unchanged.  Recurrence masters and detached instances deliberately
    /// fail closed until the portable store can address UID + RECURRENCE-ID
    /// and evaluate the complete recurrence set like ECal does.
    pub fn purge_events_before(
        &self,
        cutoff_timestamp: i64,
        cancellation: &AtomicBool,
    ) -> Result<CalendarPurgeResult, CalendarPurgeFailure> {
        let mut confirmed = CalendarPurgeResult::default();
        self.ensure_writable()
            .map_err(|error| CalendarPurgeFailure {
                confirmed: confirmed.clone(),
                error,
            })?;
        let _mutation_guard = lock_calendar_mutations().map_err(|error| CalendarPurgeFailure {
            confirmed: confirmed.clone(),
            error,
        })?;

        let paths = self.mutable_calendar_files();
        let mut file_locks = Vec::with_capacity(paths.len());
        for path in &paths {
            if cancellation.load(Ordering::Relaxed) {
                return Err(calendar_purge_cancelled(confirmed));
            }
            file_locks.push(CalendarFileLock::acquire(path).map_err(|error| {
                CalendarPurgeFailure {
                    confirmed: confirmed.clone(),
                    error,
                }
            })?);
        }

        let mut files = Vec::with_capacity(paths.len());
        let mut uid_counts = HashMap::<String, usize>::new();
        for path in paths {
            if cancellation.load(Ordering::Relaxed) {
                return Err(calendar_purge_cancelled(confirmed));
            }
            let calendar = fs::read_to_string(&path).map_err(|error| CalendarPurgeFailure {
                confirmed: confirmed.clone(),
                error,
            })?;
            let mut snapshot = CalendarSnapshot::default();
            parse_ical(
                &calendar,
                &path,
                "local-calendar",
                "Lokaler Kalender",
                &mut snapshot,
            );
            for event in &snapshot.events {
                *uid_counts.entry(event.uid.clone()).or_insert(0) += 1;
            }
            files.push((path, calendar, snapshot));
        }

        for (path, calendar, snapshot) in files {
            let mut removable_uids = Vec::new();
            for event in &snapshot.events {
                if cancellation.load(Ordering::Relaxed) {
                    return Err(calendar_purge_cancelled(confirmed));
                }
                if uid_counts.get(&event.uid).copied().unwrap_or(0) != 1 {
                    confirmed.skipped_recurring_count += 1;
                    continue;
                }
                match calendar_event_purge_disposition(&event.component, cutoff_timestamp) {
                    CalendarPurgeDisposition::Remove => removable_uids.push(event.uid.clone()),
                    CalendarPurgeDisposition::Keep => {}
                    CalendarPurgeDisposition::Recurring => {
                        confirmed.skipped_recurring_count += 1;
                    }
                    CalendarPurgeDisposition::Invalid => {
                        confirmed.skipped_invalid_count += 1;
                    }
                }
            }

            if removable_uids.is_empty() {
                continue;
            }
            let mut updated = calendar;
            for uid in &removable_uids {
                if cancellation.load(Ordering::Relaxed) {
                    return Err(calendar_purge_cancelled(confirmed));
                }
                updated = remove_calendar_component(&updated, &path, uid).map_err(|error| {
                    CalendarPurgeFailure {
                        confirmed: confirmed.clone(),
                        error,
                    }
                })?;
            }
            write_atomic_locked(&path, updated.as_bytes()).map_err(|error| {
                CalendarPurgeFailure {
                    confirmed: confirmed.clone(),
                    error,
                }
            })?;
            confirmed.removed_count += removable_uids.len();
            if cancellation.load(Ordering::Relaxed) {
                return Err(calendar_purge_cancelled(confirmed));
            }
        }

        drop(file_locks);

        Ok(confirmed)
    }

    fn ensure_writable(&self) -> io::Result<()> {
        if self.writable {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Der ausgewählte iCalendar-Ordner ist schreibgeschützt",
            ))
        }
    }

    fn calendar_file_path(&self) -> PathBuf {
        let meridian_path = self.calendar_dir.join("meridian-local.ics");
        if meridian_path.is_file() || !self.calendar_dir.join("evolution-gtk4-local.ics").is_file() {
            meridian_path
        } else {
            self.calendar_dir.join("evolution-gtk4-local.ics")
        }
    }

    fn mutable_calendar_files(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        collect_calendar_files(&self.calendar_dir, &mut paths);
        paths.sort();
        paths
    }
}

fn calendar_purge_cancelled(confirmed: CalendarPurgeResult) -> CalendarPurgeFailure {
    CalendarPurgeFailure {
        confirmed,
        error: io::Error::new(
            io::ErrorKind::Interrupted,
            "Kalenderbereinigung abgebrochen",
        ),
    }
}

fn calendar_event_purge_disposition(
    component: &str,
    cutoff_timestamp: i64,
) -> CalendarPurgeDisposition {
    let properties = top_level_properties(component);
    if properties
        .iter()
        .any(|(name, _, _)| matches!(name.as_str(), "RRULE" | "RDATE" | "RECURRENCE-ID"))
    {
        return CalendarPurgeDisposition::Recurring;
    }
    if properties.iter().any(|(name, _, _)| name == "DURATION") {
        return CalendarPurgeDisposition::Invalid;
    }

    let end_properties = properties
        .iter()
        .filter(|(name, _, _)| name == "DTEND")
        .collect::<Vec<_>>();
    let start_properties = properties
        .iter()
        .filter(|(name, _, _)| name == "DTSTART")
        .collect::<Vec<_>>();
    if start_properties.len() != 1 || end_properties.len() > 1 {
        return CalendarPurgeDisposition::Invalid;
    }
    let start = start_properties[0];
    let Some(start_is_date) = ical_property_date_kind(&start.2, &start.1) else {
        return CalendarPurgeDisposition::Invalid;
    };
    let Some(start_timestamp) = property_local_timestamp(&start.2, &start.1, 0) else {
        return CalendarPurgeDisposition::Invalid;
    };
    let end_timestamp = if let Some(end) = end_properties.first() {
        let Some(end_is_date) = ical_property_date_kind(&end.2, &end.1) else {
            return CalendarPurgeDisposition::Invalid;
        };
        if end_is_date != start_is_date {
            return CalendarPurgeDisposition::Invalid;
        }
        let Some(end_timestamp) = property_local_timestamp(&end.2, &end.1, 0) else {
            return CalendarPurgeDisposition::Invalid;
        };
        if end_timestamp < start_timestamp || (start_is_date && end_timestamp == start_timestamp) {
            return CalendarPurgeDisposition::Invalid;
        }
        end_timestamp
    } else if start_is_date {
        // RFC 5545 gives an all-day VEVENT without DTEND a one-day duration.
        // Keep it until that default is represented explicitly and safely.
        return CalendarPurgeDisposition::Invalid;
    } else {
        start_timestamp
    };

    if end_timestamp < cutoff_timestamp {
        CalendarPurgeDisposition::Remove
    } else {
        CalendarPurgeDisposition::Keep
    }
}

fn ical_property_date_kind(property: &str, value: &str) -> Option<bool> {
    let bytes = value.as_bytes();
    let value_is_date = bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit);
    let value_is_datetime = matches!(bytes.len(), 15 | 16)
        && bytes[8] == b'T'
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && (bytes.len() == 15 || bytes[15] == b'Z');
    if !value_is_date && !value_is_datetime {
        return None;
    }

    let mut declared_value = None;
    let mut tzid = None;
    for parameter in property.split(';').skip(1) {
        let (name, value) = parameter.split_once('=')?;
        let name = name.trim();
        let value = value.trim().trim_matches('"');
        if name.eq_ignore_ascii_case("VALUE") {
            if declared_value.replace(value).is_some() {
                return None;
            }
        } else if name.eq_ignore_ascii_case("TZID") {
            if value.is_empty() || tzid.replace(value).is_some() {
                return None;
            }
        }
    }
    if tzid.is_some() && (value_is_date || value.ends_with('Z')) {
        return None;
    }

    match declared_value {
        Some(value) if value.eq_ignore_ascii_case("DATE") => value_is_date.then_some(true),
        Some(value) if value.eq_ignore_ascii_case("DATE-TIME") => {
            value_is_datetime.then_some(false)
        }
        Some(_) => None,
        None => Some(value_is_date),
    }
}

impl Default for CalendarStoreService {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn sort_snapshot(snapshot: &mut CalendarSnapshot) {
    snapshot.events.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.uid.cmp(&right.uid))
    });
    snapshot.tasks.sort_by(|left, right| {
        left.due
            .cmp(&right.due)
            .then_with(|| left.uid.cmp(&right.uid))
    });
    snapshot.memos.sort_by(|left, right| {
        left.updated
            .cmp(&right.updated)
            .then_with(|| left.uid.cmp(&right.uid))
    });
}

fn collect_calendar_files(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !file_type.is_symlink() {
                collect_calendar_files(&path, paths);
            }
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(extension.to_ascii_lowercase().as_str(), "ics" | "vcs")
                })
        {
            paths.push(path);
        }
    }
}

fn unique_destination(root: &Path, file_name: &str) -> PathBuf {
    let original = root.join(file_name);
    if !original.exists() {
        return original;
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("calendar");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let mut suffix = 1;
    loop {
        let candidate = root.join(format!("{stem}-{suffix}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

fn parse_ical(
    raw: &str,
    path: &Path,
    source_uid: &str,
    source_display_name: &str,
    snapshot: &mut CalendarSnapshot,
) {
    parse_ical_with_source(raw, path, source_uid, source_display_name, snapshot);
}

fn component_kind(raw: &str) -> Option<&'static str> {
    unfold_lines(raw).into_iter().find_map(|line| {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            Some("VEVENT")
        } else if line.eq_ignore_ascii_case("BEGIN:VTODO") {
            Some("VTODO")
        } else if line.eq_ignore_ascii_case("BEGIN:VJOURNAL") {
            Some("VJOURNAL")
        } else {
            None
        }
    })
}

fn first_primary_component(raw: &str) -> Option<String> {
    let lines = logical_lines(raw);
    let start = lines
        .iter()
        .position(|line| is_primary_component_begin(&line.unfolded))?;
    let kind = primary_component_kind(&lines[start].unfolded)?;
    let mut nested_depth = 0usize;
    let mut end = None;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if line
            .unfolded
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth = nested_depth.saturating_add(1);
        } else if line
            .unfolded
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            if nested_depth == 0 && line.unfolded.eq_ignore_ascii_case(&format!("END:{kind}")) {
                end = Some(index);
                break;
            }
            nested_depth = nested_depth.saturating_sub(1);
        }
    }
    let end = end?;
    Some(finish_physical_lines(
        lines[start..=end]
            .iter()
            .flat_map(|line| line.physical.iter().cloned())
            .collect(),
    ))
}

fn ensure_component_uid(raw: &str, kind: &str, fallback_uid: Option<&str>) -> (String, String) {
    let mut lines = unfold_lines(raw);
    let uid = component_uid_from_lines(&lines)
        .or_else(|| fallback_uid.map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("meridian-{}", uuid::Uuid::new_v4()));
    if component_uid_from_lines(&lines).is_none() {
        let insertion = lines
            .iter()
            .position(|line| line.eq_ignore_ascii_case(&format!("BEGIN:{kind}")))
            .map(|index| index + 1)
            .unwrap_or(0);
        lines.insert(insertion, format!("UID:{uid}"));
    }
    let mut component = lines.join("\r\n");
    component.push_str("\r\n");
    (uid, component)
}

fn read_calendar_file(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(raw),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(concat!(
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "PRODID:-//Meridian//EN\r\n",
            "END:VCALENDAR\r\n"
        )
        .to_string()),
        Err(error) => Err(error),
    }
}

fn insert_calendar_component(calendar: &mut String, component: &str) -> io::Result<()> {
    let mut lines = unfold_lines(calendar);
    let Some(insertion) = lines
        .iter()
        .position(|line| line.eq_ignore_ascii_case("END:VCALENDAR"))
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "iCalendar-Datei enthält kein END:VCALENDAR",
        ));
    };
    lines.splice(insertion..insertion, unfold_lines(component));
    *calendar = lines.join("\r\n");
    calendar.push_str("\r\n");
    Ok(())
}

fn calendar_contains_component(calendar: &str, path: &Path, uid: &str) -> bool {
    find_calendar_component_range_for_path(calendar, path, uid).is_some()
}

fn replace_calendar_component(
    calendar: &str,
    path: &Path,
    uid: &str,
    replacement: &str,
) -> io::Result<String> {
    let mut lines = unfold_lines(calendar);
    let Some((start, end)) = find_calendar_component_range_in_lines_for_path(&lines, path, uid)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("iCalendar-Komponente nicht gefunden: {uid}"),
        ));
    };
    lines.splice(start..=end, unfold_lines(replacement));
    let mut result = lines.join("\r\n");
    result.push_str("\r\n");
    Ok(result)
}

fn remove_calendar_component(calendar: &str, path: &Path, uid: &str) -> io::Result<String> {
    let mut lines = unfold_lines(calendar);
    let Some((start, end)) = find_calendar_component_range_in_lines_for_path(&lines, path, uid)
    else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("iCalendar-Komponente nicht gefunden: {uid}"),
        ));
    };
    lines.drain(start..=end);
    let mut result = lines.join("\r\n");
    result.push_str("\r\n");
    Ok(result)
}

fn find_calendar_component_range_for_path(
    calendar: &str,
    path: &Path,
    uid: &str,
) -> Option<(usize, usize)> {
    let lines = unfold_lines(calendar);
    find_calendar_component_range_in_lines_for_path(&lines, path, uid)
}

fn find_calendar_component_range_in_lines_for_path(
    lines: &[String],
    path: &Path,
    uid: &str,
) -> Option<(usize, usize)> {
    let mut index = 0;
    let mut component_index = 0;
    while index < lines.len() {
        let is_component = lines[index].eq_ignore_ascii_case("BEGIN:VEVENT")
            || lines[index].eq_ignore_ascii_case("BEGIN:VTODO")
            || lines[index].eq_ignore_ascii_case("BEGIN:VJOURNAL");
        if !is_component {
            index += 1;
            continue;
        }
        component_index += 1;
        let start = index;
        let mut depth = 1usize;
        index += 1;
        while index < lines.len() && depth > 0 {
            if lines[index]
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
            {
                depth += 1;
            } else if lines[index]
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
            {
                depth = depth.saturating_sub(1);
            }
            index += 1;
        }
        let end = index.saturating_sub(1);
        let component_id = component_uid_from_lines(&lines[start..=end]).or_else(|| {
            Some(format!(
                "local:{}#{component_index}",
                path.to_string_lossy()
            ))
        });
        if depth == 0 && end < lines.len() && component_id.as_deref() == Some(uid) {
            return Some((start, end));
        }
    }
    None
}

fn component_uid_from_lines(lines: &[String]) -> Option<String> {
    let mut depth = 0usize;
    for line in lines {
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            depth += 1;
            continue;
        }
        if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth != 1 {
            continue;
        }
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        if property
            .split(';')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("UID"))
        {
            return Some(unescape(value));
        }
    }
    None
}

struct CalendarFileLock {
    path: PathBuf,
}

impl CalendarFileLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let basename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("calendar");
        let lock_path = parent.join(format!(".{basename}.lock"));
        for _ in 0..200 {
            match fs::create_dir(&lock_path) {
                Ok(()) => return Ok(Self { path: lock_path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "iCalendar-Datei wird bereits bearbeitet: {}",
                path.display()
            ),
        ))
    }
}

impl Drop for CalendarFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn lock_calendar_mutations() -> io::Result<MutexGuard<'static, ()>> {
    CALENDAR_MUTATION_LOCK
        .lock()
        .map_err(|_| io::Error::other("Kalender-Mutationssperre ist vergiftet"))
}

#[cfg(test)]
fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let _mutation_guard = lock_calendar_mutations()?;
    let _file_lock = CalendarFileLock::acquire(path)?;
    write_atomic_locked(path, contents)
}

fn write_atomic_locked(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let previous_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("calendar");
    let mut temporary = None;
    let mut temporary_file = None;
    for _ in 0..32 {
        let sequence = ATOMIC_WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{basename}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some(candidate);
                temporary_file = Some(file);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let Some(temporary) = temporary else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Kein eindeutiger temporärer iCalendar-Pfad verfügbar",
        ));
    };
    let mut temporary_file = Some(temporary_file.expect("temporary file exists with its path"));
    let mut renamed = false;
    let result = (|| {
        if let Some(permissions) = previous_permissions {
            fs::set_permissions(&temporary, permissions)?;
        }
        {
            let file = temporary_file
                .as_mut()
                .expect("temporary file exists until it is closed");
            file.write_all(contents)?;
            file.flush()?;
            file.sync_all()?;
        }
        temporary_file.take();
        fs::rename(&temporary, path)?;
        renamed = true;

        // Directory fsync is a durability best effort on platforms which
        // expose directory handles.  The atomic replacement has already
        // succeeded; a platform that rejects opening the directory must not
        // turn a successful replacement into a reported write failure.
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if !renamed {
        temporary_file.take();
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn parse_ical_component(
    raw: &str,
    source_uid: &str,
    source_display_name: &str,
    snapshot: &mut CalendarSnapshot,
) {
    parse_ical_with_source(
        raw,
        Path::new("<eds-calendar-component>"),
        source_uid,
        source_display_name,
        snapshot,
    );
}

fn parse_ical_with_source(
    raw: &str,
    path: &Path,
    source_uid: &str,
    source_display_name: &str,
    snapshot: &mut CalendarSnapshot,
) {
    let lines = unfold_lines(raw);
    let mut current: Option<CurrentComponent> = None;
    let mut component_index = 0;

    for line in lines {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            current = Some(CurrentComponent::new("VEVENT"));
            continue;
        }
        if line.eq_ignore_ascii_case("BEGIN:VTODO") {
            current = Some(CurrentComponent::new("VTODO"));
            continue;
        }
        if line.eq_ignore_ascii_case("BEGIN:VJOURNAL") {
            current = Some(CurrentComponent::new("VJOURNAL"));
            continue;
        }
        if line.starts_with("BEGIN:") {
            if let Some(component) = current.as_mut() {
                component.raw_lines.push(line.clone());
                component.nested_depth = component.nested_depth.saturating_add(1);
            }
            continue;
        }
        if line.starts_with("END:") {
            if line.eq_ignore_ascii_case("END:VCALENDAR") {
                continue;
            }
            if let Some(component) = current.as_mut() {
                if component.nested_depth > 0 {
                    component.raw_lines.push(line.clone());
                    component.nested_depth -= 1;
                    continue;
                }
            }
            if line.eq_ignore_ascii_case("END:VEVENT")
                || line.eq_ignore_ascii_case("END:VTODO")
                || line.eq_ignore_ascii_case("END:VJOURNAL")
            {
                if let Some(component) = current.take() {
                    let mut component = component;
                    component.raw_lines.push(line.clone());
                    component_index += 1;
                    append_component(
                        component,
                        path,
                        component_index,
                        source_uid,
                        source_display_name,
                        snapshot,
                    );
                }
            }
            continue;
        }
        if let Some(component) = current.as_mut() {
            component.raw_lines.push(line.clone());
            if component.nested_depth > 0 {
                continue;
            }
            let Some((property, value)) = line.split_once(':') else {
                continue;
            };
            let property_name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            component.properties.insert(property_name, unescape(value));
            if property.to_ascii_uppercase().contains("VALUE=DATE") {
                component.date_properties.insert(
                    property
                        .split(';')
                        .next()
                        .unwrap_or(property)
                        .to_ascii_uppercase(),
                );
            }
        }
    }
}

#[derive(Debug, Default)]
struct CurrentComponent {
    kind: String,
    properties: std::collections::HashMap<String, String>,
    date_properties: std::collections::HashSet<String>,
    raw_lines: Vec<String>,
    nested_depth: usize,
}

impl CurrentComponent {
    fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            raw_lines: vec![format!("BEGIN:{kind}")],
            ..Self::default()
        }
    }

    fn raw_string(&self) -> String {
        let mut raw = self.raw_lines.join("\r\n");
        raw.push_str("\r\n");
        raw
    }
}

fn append_component(
    component: CurrentComponent,
    path: &Path,
    index: usize,
    source_uid: &str,
    source_display_name: &str,
    snapshot: &mut CalendarSnapshot,
) {
    let raw_component = component.raw_string();
    let uid = component
        .properties
        .get("UID")
        .cloned()
        .unwrap_or_else(|| format!("local:{}#{index}", path.to_string_lossy()));
    let summary = component
        .properties
        .get("SUMMARY")
        .cloned()
        .unwrap_or_else(|| "(Ohne Titel)".to_string());

    let resolved_source_name = component
        .properties
        .get("X-EVOLUTION-SOURCE-NAME")
        .or_else(|| {
            if source_display_name == "Lokaler Kalender" || source_display_name.is_empty() {
                component.properties.get("CATEGORIES")
            } else {
                None
            }
        })
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| source_display_name.to_string());

    let resolved_source_uid = component
        .properties
        .get("X-EVOLUTION-SOURCE-UID")
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| {
            if (source_uid == "local-calendar" || source_uid.is_empty())
                && !resolved_source_name.is_empty()
                && resolved_source_name != "Lokaler Kalender"
            {
                format!("calendar-{}", resolved_source_name)
            } else {
                source_uid.to_string()
            }
        });

    match component.kind.as_str() {
        "VEVENT" => {
            let all_day = component.date_properties.contains("DTSTART");
            let start = component
                .properties
                .get("DTSTART")
                .and_then(|_| property_local_display(&raw_component, "DTSTART", all_day))
                .or_else(|| {
                    component
                        .properties
                        .get("DTSTART")
                        .map(|value| format_ical_datetime(value))
                })
                .unwrap_or_default();
            let end = component
                .properties
                .get("DTEND")
                .and_then(|_| property_local_display(&raw_component, "DTEND", all_day))
                .or_else(|| {
                    component
                        .properties
                        .get("DTEND")
                        .map(|value| format_ical_datetime(value))
                })
                .unwrap_or_default();
            snapshot.events.push(CalendarEventRecord {
                uid,
                source_uid: resolved_source_uid,
                source_display_name: resolved_source_name,
                component: raw_component.clone(),
                summary,
                start,
                end,
                location: component
                    .properties
                    .get("LOCATION")
                    .cloned()
                    .unwrap_or_default(),
                description: component
                    .properties
                    .get("DESCRIPTION")
                    .cloned()
                    .unwrap_or_default(),
                all_day,
            });
        }
        "VTODO" => {
            let priority = component
                .properties
                .get("PRIORITY")
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(0);
            let completed = component
                .properties
                .get("STATUS")
                .is_some_and(|value| value.eq_ignore_ascii_case("COMPLETED"));
            snapshot.tasks.push(TaskRecord {
                uid,
                source_uid: resolved_source_uid,
                source_display_name: resolved_source_name,
                component: raw_component.clone(),
                summary,
                due: component
                    .properties
                    .get("DUE")
                    .and_then(|_| {
                        property_local_display(
                            &raw_component,
                            "DUE",
                            component.date_properties.contains("DUE"),
                        )
                    })
                    .or_else(|| {
                        component
                            .properties
                            .get("DUE")
                            .map(|value| format_ical_datetime(value))
                    })
                    .unwrap_or_default(),
                priority,
                completed,
            });
        }
        "VJOURNAL" => {
            snapshot.memos.push(MemoRecord {
                uid,
                source_uid: resolved_source_uid,
                source_display_name: resolved_source_name,
                component: raw_component.clone(),
                summary,
                description: component
                    .properties
                    .get("DESCRIPTION")
                    .cloned()
                    .unwrap_or_default(),
                updated: property_local_display(&raw_component, "LAST-MODIFIED", false)
                    .or_else(|| property_local_display(&raw_component, "DTSTART", false))
                    .or_else(|| {
                        component
                            .properties
                            .get("LAST-MODIFIED")
                            .or_else(|| component.properties.get("DTSTART"))
                            .map(|value| format_ical_datetime(value))
                    })
                    .unwrap_or_default(),
            });
        }
        _ => {}
    }
}

#[derive(Debug, Clone)]
struct IcalLogicalLine {
    unfolded: String,
    physical: Vec<String>,
}

fn logical_lines(raw: &str) -> Vec<IcalLogicalLine> {
    let mut lines: Vec<IcalLogicalLine> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.unfolded.push_str(&raw_line[1..]);
                previous.physical.push(raw_line.to_string());
            }
        } else {
            lines.push(IcalLogicalLine {
                unfolded: raw_line.to_string(),
                physical: vec![raw_line.to_string()],
            });
        }
    }
    lines
}

fn unfold_lines(raw: &str) -> Vec<String> {
    logical_lines(raw)
        .into_iter()
        .map(|line| line.unfolded)
        .collect()
}

fn fold_ical_line(line: &str) -> Vec<String> {
    const MAX_OCTETS: usize = 75;
    if line.len() <= MAX_OCTETS {
        return vec![line.to_string()];
    }

    let mut folded = Vec::new();
    let mut remaining = line;
    let mut first = true;
    while !remaining.is_empty() {
        let budget = if first { MAX_OCTETS } else { MAX_OCTETS - 1 };
        let mut end = remaining.len().min(budget);
        while end > 0 && !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            break;
        }
        let (part, rest) = remaining.split_at(end);
        folded.push(if first {
            part.to_string()
        } else {
            format!(" {part}")
        });
        remaining = rest;
        first = false;
    }
    folded
}

fn append_folded_line(lines: &mut Vec<String>, prefix: &str, value: &str) {
    lines.extend(fold_ical_line(&format!("{prefix}:{value}")));
}

fn finish_physical_lines(lines: Vec<String>) -> String {
    let mut result = lines.join("\r\n");
    result.push_str("\r\n");
    result
}

fn unescape(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn escape_ical_text_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn strip_line_breaks(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '\r' && *character != '\n')
        .collect()
}

fn validate_single_line(label: &str, value: &str) -> Result<(), String> {
    if value.contains('\r') || value.contains('\n') {
        Err(format!("{label} darf keine Zeilenumbrüche enthalten"))
    } else {
        Ok(())
    }
}

fn validate_dtstart_value(value: &str) -> Result<(), String> {
    let value = value.trim();
    if is_ical_date(value) || is_ical_date_time(value) {
        Ok(())
    } else {
        Err("DTSTART muss ein iCalendar-Datum oder ein Datum/Zeit-Wert sein".to_string())
    }
}

fn is_ical_date(value: &str) -> bool {
    value.len() == 8 && value.chars().all(|character| character.is_ascii_digit())
}

fn is_ical_date_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    (bytes.len() == 15 || bytes.len() == 16)
        && bytes.get(8) == Some(&b'T')
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && (value.len() == 15 || value.ends_with('Z'))
}

fn property_has_parameter(prefix: &str, expected_name: &str, expected_value: &str) -> bool {
    split_ical_parameters(prefix)
        .into_iter()
        .skip(1)
        .any(|parameter| {
            let (name, value) = parameter_name_value(parameter);
            name.eq_ignore_ascii_case(expected_name)
                && value
                    .map(unquote_parameter_value)
                    .is_some_and(|value| value.eq_ignore_ascii_case(expected_value))
        })
}

fn validate_ical_token(label: &str, value: &str) -> Result<(), String> {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        Ok(())
    } else {
        Err(format!("{label} enthält einen ungültigen iCalendar-Namen"))
    }
}

fn split_ical_property_line(line: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
        } else if character == ':' && !quoted {
            return Some((&line[..index], &line[index + character.len_utf8()..]));
        }
    }
    None
}

fn split_ical_parameters(prefix: &str) -> Vec<&str> {
    let mut parameters = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in prefix.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
        } else if character == ';' && !quoted {
            parameters.push(&prefix[start..index]);
            start = index + character.len_utf8();
        }
    }
    parameters.push(&prefix[start..]);
    parameters
}

fn property_name(prefix: &str) -> &str {
    split_ical_parameters(prefix)
        .first()
        .copied()
        .unwrap_or(prefix)
        .trim()
}

fn parameter_name_value(parameter: &str) -> (&str, Option<&str>) {
    parameter
        .split_once('=')
        .map(|(name, value)| (name.trim(), Some(value.trim())))
        .unwrap_or((parameter.trim(), None))
}

fn unquote_parameter_value(value: &str) -> String {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    value.replace("\\\"", "\"").replace("\\\\", "\\")
}

fn top_level_properties(raw: &str) -> Vec<(String, String, String)> {
    let mut properties = Vec::new();
    let mut depth = 0usize;
    let mut target_depth = None;
    for logical in logical_lines(raw) {
        let line = logical.unfolded;
        if is_primary_component_begin(&line) {
            if target_depth.is_none() {
                target_depth = Some(depth + 1);
            }
            depth = depth.saturating_add(1);
            continue;
        }
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            depth = depth.saturating_add(1);
            continue;
        }
        if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            if target_depth == Some(depth) {
                break;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        if target_depth != Some(depth) {
            continue;
        }
        let Some((prefix, value)) = split_ical_property_line(&line) else {
            continue;
        };
        properties.push((
            property_name(prefix).to_ascii_uppercase(),
            value.to_string(),
            prefix.to_string(),
        ));
    }
    properties
}

fn is_primary_component_begin(line: &str) -> bool {
    primary_component_kind(line).is_some()
}

fn primary_component_kind(line: &str) -> Option<&'static str> {
    if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
        Some("VEVENT")
    } else if line.eq_ignore_ascii_case("BEGIN:VTODO") {
        Some("VTODO")
    } else if line.eq_ignore_ascii_case("BEGIN:VJOURNAL") {
        Some("VJOURNAL")
    } else {
        None
    }
}

fn top_level_attachment_properties(raw: &str) -> Vec<String> {
    let mut attachments = Vec::new();
    let mut depth = 0usize;
    let mut target_depth = None;
    for logical in logical_lines(raw) {
        let line = &logical.unfolded;
        if is_primary_component_begin(line) {
            if target_depth.is_none() {
                target_depth = Some(depth + 1);
            }
            depth = depth.saturating_add(1);
            continue;
        }
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            depth = depth.saturating_add(1);
            continue;
        }
        if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            if target_depth == Some(depth) {
                break;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        if target_depth != Some(depth) {
            continue;
        }
        let Some((prefix, _)) = split_ical_property_line(line) else {
            continue;
        };
        if property_name(prefix).eq_ignore_ascii_case("ATTACH") {
            attachments.push(logical.physical.join("\r\n"));
        }
    }
    attachments
}

fn split_ical_list(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push('\\');
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ',' {
            if !current.trim().is_empty() {
                values.push(unescape(current.trim()));
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.trim().is_empty() {
        values.push(unescape(current.trim()));
    }
    values
}

fn serialize_ical_categories(categories: &[String]) -> String {
    categories
        .iter()
        .map(|category| escape_ical_text_value(category.trim()))
        .filter(|category| !category.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_memo_attachment(line: &str) -> Option<MemoAttachment> {
    let logical = logical_lines(line).into_iter().next()?;
    let line = logical.unfolded;
    let (prefix, value) = split_ical_property_line(&line)?;
    if !property_name(prefix).eq_ignore_ascii_case("ATTACH") {
        return None;
    }

    let raw_parameters = split_ical_parameters(prefix)
        .into_iter()
        .skip(1)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut filename = None;
    let mut fmttype = None;
    let mut unknown_parameters = Vec::new();
    let mut value_kind = None;
    let mut encoding = None;
    for parameter in &raw_parameters {
        let (name, parameter_value) = parameter_name_value(parameter);
        let normalized = name.to_ascii_uppercase();
        let parsed_value = parameter_value.map(unquote_parameter_value);
        match normalized.as_str() {
            "FILENAME" => filename = parsed_value,
            "FMTTYPE" => fmttype = parsed_value,
            "VALUE" => value_kind = parsed_value,
            "ENCODING" => encoding = parsed_value,
            _ => unknown_parameters.push(MemoAttachmentParameter {
                name: name.to_string(),
                value: parsed_value,
            }),
        }
    }

    let is_inline = value_kind
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("BINARY"))
        || encoding
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case("NONE"));
    let (uri, inline_content) = if is_inline {
        (None, Some(value.to_string()))
    } else {
        (Some(unescape(value)), None)
    };
    let mut attachment = MemoAttachment {
        uri,
        inline_content,
        filename,
        fmttype,
        unknown_parameters,
        raw_property: Some(line),
        raw_property_lines: logical.physical,
        raw_parameters,
        source_signature: None,
    };
    attachment.source_signature = Some(attachment.signature());
    Some(attachment)
}

fn render_parameter_value(value: &str) -> String {
    let value = strip_line_breaks(value);
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._/".contains(character))
    {
        value
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn render_parameter(name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!("{name}={}", render_parameter_value(value)),
        None => name.to_string(),
    }
}

fn render_memo_attachment(attachment: &MemoAttachment) -> String {
    if attachment.is_unchanged() {
        return attachment.raw_property.clone().unwrap_or_default();
    }

    let mut parameters = Vec::new();
    let source_filename = attachment
        .source_signature
        .as_ref()
        .and_then(|signature| signature.filename.as_deref());
    let source_fmttype = attachment
        .source_signature
        .as_ref()
        .and_then(|signature| signature.fmttype.as_deref());
    let source_unknown_parameters = attachment
        .source_signature
        .as_ref()
        .map(|signature| signature.unknown_parameters.as_slice())
        .unwrap_or(&[]);
    let mut used_unknown_parameters = vec![false; attachment.unknown_parameters.len()];
    let mut used_source_unknown_parameters = vec![false; source_unknown_parameters.len()];
    let mut filename_written = false;
    let mut fmttype_written = false;
    let mut value_written = false;
    let mut encoding_written = false;
    for raw_parameter in &attachment.raw_parameters {
        let (name, raw_value) = parameter_name_value(raw_parameter);
        if name.eq_ignore_ascii_case("FILENAME") {
            if !filename_written {
                if let Some(filename) = attachment.filename.as_deref() {
                    if source_filename == Some(filename) {
                        parameters.push(raw_parameter.clone());
                    } else {
                        parameters.push(render_parameter(name, Some(filename)));
                    }
                    filename_written = true;
                }
            }
        } else if name.eq_ignore_ascii_case("FMTTYPE") {
            if !fmttype_written {
                if let Some(fmttype) = attachment.fmttype.as_deref() {
                    if source_fmttype == Some(fmttype) {
                        parameters.push(raw_parameter.clone());
                    } else {
                        parameters.push(render_parameter(name, Some(fmttype)));
                    }
                    fmttype_written = true;
                }
            }
        } else if name.eq_ignore_ascii_case("VALUE") {
            if !value_written {
                let wanted = if attachment.inline_content.is_some() {
                    "BINARY"
                } else {
                    "URI"
                };
                if raw_value
                    .map(unquote_parameter_value)
                    .is_some_and(|value| value.eq_ignore_ascii_case(wanted))
                {
                    parameters.push(raw_parameter.clone());
                } else {
                    parameters.push(render_parameter(name, Some(wanted)));
                }
                value_written = true;
            }
        } else if name.eq_ignore_ascii_case("ENCODING") {
            if !encoding_written {
                // ENCODING is meaningful for inline content.  When the
                // value changes to a URI, stale BINARY/8BIT metadata must
                // not remain attached to it.
                if attachment.inline_content.is_some() {
                    parameters.push(raw_parameter.clone());
                }
                encoding_written = true;
            }
        } else if let Some((index, parameter)) = attachment
            .unknown_parameters
            .iter()
            .enumerate()
            .find(|(index, parameter)| {
                !used_unknown_parameters[*index] && parameter.name.eq_ignore_ascii_case(name)
            })
        {
            used_unknown_parameters[index] = true;
            let source_parameter =
                source_unknown_parameters
                    .iter()
                    .enumerate()
                    .find(|(index, parameter)| {
                        !used_source_unknown_parameters[*index]
                            && parameter.name.eq_ignore_ascii_case(name)
                    });
            if let Some((source_index, source_parameter)) = source_parameter {
                used_source_unknown_parameters[source_index] = true;
                if source_parameter == parameter {
                    parameters.push(raw_parameter.clone());
                    continue;
                }
            }
            parameters.push(render_parameter(name, parameter.value.as_deref()));
        }
    }
    if !filename_written {
        if let Some(filename) = attachment.filename.as_deref() {
            parameters.push(render_parameter("FILENAME", Some(filename)));
        }
    }
    if !fmttype_written {
        if let Some(fmttype) = attachment.fmttype.as_deref() {
            parameters.push(render_parameter("FMTTYPE", Some(fmttype)));
        }
    }
    parameters.extend(
        attachment
            .unknown_parameters
            .iter()
            .enumerate()
            .filter(|(index, _)| !used_unknown_parameters[*index])
            .map(|(_, parameter)| parameter)
            .map(|parameter| render_parameter(&parameter.name, parameter.value.as_deref())),
    );
    if !value_written && !attachment.raw_parameters.is_empty() {
        parameters.push(render_parameter(
            "VALUE",
            Some(if attachment.inline_content.is_some() {
                "BINARY"
            } else {
                "URI"
            }),
        ));
    }
    if attachment.raw_parameters.is_empty() {
        if attachment.inline_content.is_some() {
            parameters.push("VALUE=BINARY".to_string());
        } else {
            parameters.push("VALUE=URI".to_string());
        }
    }
    let prefix = if parameters.is_empty() {
        "ATTACH".to_string()
    } else {
        format!("ATTACH;{}", parameters.join(";"))
    };
    let value = attachment
        .inline_content
        .as_deref()
        .or(attachment.uri.as_deref())
        .unwrap_or_default();
    let serialized_value = if attachment.inline_content.is_some() {
        strip_line_breaks(value)
    } else {
        serialize_memo_uri_value(value)
    };
    format!("{prefix}:{serialized_value}")
}

fn render_memo_attachment_lines(attachment: &MemoAttachment) -> Vec<String> {
    if attachment.is_unchanged() && !attachment.raw_property_lines.is_empty() {
        attachment.raw_property_lines.clone()
    } else {
        fold_ical_line(&render_memo_attachment(attachment))
    }
}

/// Replace or remove a top-level iCalendar property while preserving the rest
/// of the component, including nested alarms and vendor-specific fields.
pub(crate) fn update_ical_property(
    raw: &str,
    requested_property: &str,
    value: Option<&str>,
) -> String {
    let target = requested_property
        .split(';')
        .next()
        .unwrap_or(requested_property)
        .to_ascii_uppercase();
    let mut lines = Vec::new();
    let mut replaced = false;
    let mut in_primary = false;
    let mut nested_depth = 0usize;
    let mut primary_kind = None;
    let mut primary_seen = false;

    for logical in logical_lines(raw) {
        let line = &logical.unfolded;
        if !in_primary {
            if !primary_seen {
                if let Some(kind) = primary_component_kind(line) {
                    in_primary = true;
                    primary_seen = true;
                    primary_kind = Some(kind.to_string());
                }
            }
            lines.extend(logical.physical);
            continue;
        }

        let prefix = split_ical_property_line(line)
            .map(|(prefix, _)| prefix)
            .unwrap_or(line);
        let property = property_name(prefix).to_ascii_uppercase();
        if property == target && nested_depth == 0 {
            if !replaced {
                if let Some(value) = value {
                    // Keep the complete original parameter set, including
                    // VALUE=DATE/VALUE=URI and vendor parameters.  The
                    // requested prefix is used only when the property is new.
                    append_folded_line(&mut lines, prefix, value);
                }
                replaced = true;
            }
            continue;
        }

        if nested_depth == 0
            && primary_kind
                .as_deref()
                .is_some_and(|kind| line.eq_ignore_ascii_case(&format!("END:{kind}")))
        {
            if value.is_some() && !replaced {
                append_folded_line(&mut lines, requested_property, value.unwrap_or_default());
                replaced = true;
            }
            lines.extend(logical.physical);
            in_primary = false;
            primary_kind = None;
            continue;
        }

        lines.extend(logical.physical);
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth = nested_depth.saturating_add(1);
        } else if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            nested_depth = nested_depth.saturating_sub(1);
        }
    }

    if value.is_some() && !replaced {
        append_folded_line(&mut lines, requested_property, value.unwrap_or_default());
    }
    finish_physical_lines(lines)
}

/// Replace top-level ATTACH properties while retaining parameters for URIs that
/// remain attached to the component.
pub(crate) fn replace_ical_attachments(raw: &str, values: &[String]) -> String {
    let existing = top_level_attachment_properties(raw)
        .into_iter()
        .filter_map(|line| parse_memo_attachment(&line))
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut replaced = false;
    let mut in_primary = false;
    let mut primary_seen = false;
    let mut nested_depth = 0usize;
    let mut primary_kind = None;
    let mut used_existing = vec![false; existing.len()];
    for logical in logical_lines(raw) {
        let line = &logical.unfolded;
        if !in_primary {
            if !primary_seen {
                if let Some(kind) = primary_component_kind(line) {
                    in_primary = true;
                    primary_seen = true;
                    primary_kind = Some(kind);
                }
            }
            lines.extend(logical.physical);
            continue;
        }
        let prefix = split_ical_property_line(line)
            .map(|(prefix, _)| prefix)
            .unwrap_or(line);
        let property = property_name(prefix).to_ascii_uppercase();
        if property == "ATTACH" && nested_depth == 0 {
            if !replaced {
                for value in values {
                    let matching_existing = existing.iter().enumerate().find(|(index, current)| {
                        !used_existing[*index]
                            && current
                                .uri
                                .as_ref()
                                .is_some_and(|uri| uri.eq_ignore_ascii_case(value))
                    });
                    if let Some((index, current)) = matching_existing {
                        used_existing[index] = true;
                        lines.extend(if current.raw_property_lines.is_empty() {
                            vec![format!(
                                "ATTACH;VALUE=URI:{}",
                                serialize_ical_attachment_value(value)
                            )]
                        } else {
                            current.raw_property_lines.clone()
                        });
                    } else {
                        lines.push(format!(
                            "ATTACH;VALUE=URI:{}",
                            serialize_ical_attachment_value(value)
                        ));
                    }
                }
                replaced = true;
            }
            continue;
        }
        if nested_depth == 0
            && primary_kind
                .as_deref()
                .is_some_and(|kind| line.eq_ignore_ascii_case(&format!("END:{kind}")))
        {
            if !replaced && !values.is_empty() {
                lines.extend(values.iter().map(|value| {
                    format!(
                        "ATTACH;VALUE=URI:{}",
                        serialize_ical_attachment_value(value)
                    )
                }));
                replaced = true;
            }
            lines.extend(logical.physical);
            in_primary = false;
            primary_kind = None;
            continue;
        }
        lines.extend(logical.physical);
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth = nested_depth.saturating_add(1);
        } else if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            nested_depth = nested_depth.saturating_sub(1);
        }
    }
    if !replaced && !values.is_empty() {
        lines.extend(values.iter().map(|value| {
            format!(
                "ATTACH;VALUE=URI:{}",
                serialize_ical_attachment_value(value)
            )
        }));
    }
    if !replaced && values.is_empty() {
        return raw.to_string();
    }
    finish_physical_lines(lines)
}

pub(crate) fn replace_ical_memo_attachments(raw: &str, attachments: &[MemoAttachment]) -> String {
    let existing = top_level_attachment_properties(raw)
        .into_iter()
        .filter_map(|line| parse_memo_attachment(&line))
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut replaced = false;
    let mut in_primary = false;
    let mut nested_depth = 0usize;
    let mut primary_kind = None;
    let mut primary_seen = false;
    let mut used_existing = vec![false; existing.len()];
    for logical in logical_lines(raw) {
        let line = &logical.unfolded;
        if !in_primary {
            if !primary_seen {
                if let Some(kind) = primary_component_kind(line) {
                    in_primary = true;
                    primary_seen = true;
                    primary_kind = Some(kind.to_string());
                }
            }
            lines.extend(logical.physical);
            continue;
        }

        let prefix = split_ical_property_line(line)
            .map(|(prefix, _)| prefix)
            .unwrap_or(line);
        let property = property_name(prefix).to_ascii_uppercase();
        if property == "ATTACH" && nested_depth == 0 {
            if !replaced {
                for attachment in attachments {
                    let matching_existing = existing.iter().enumerate().find(|(index, current)| {
                        !used_existing[*index]
                            && attachment.uri.is_some()
                            && attachment.inline_content.is_none()
                            && current.uri.as_ref().is_some_and(|uri| {
                                attachment
                                    .uri
                                    .as_ref()
                                    .is_some_and(|value| uri.eq_ignore_ascii_case(value))
                            })
                    });
                    if let Some((index, current)) = matching_existing {
                        used_existing[index] = true;
                        if attachment.raw_property.is_none()
                            && attachment.filename.is_none()
                            && attachment.fmttype.is_none()
                            && attachment.unknown_parameters.is_empty()
                        {
                            lines.extend(if current.raw_property_lines.is_empty() {
                                render_memo_attachment_lines(attachment)
                            } else {
                                current.raw_property_lines.clone()
                            });
                            continue;
                        }
                    }
                    lines.extend(render_memo_attachment_lines(attachment));
                }
                replaced = true;
            }
            continue;
        }

        if nested_depth == 0
            && primary_kind
                .as_deref()
                .is_some_and(|kind| line.eq_ignore_ascii_case(&format!("END:{kind}")))
        {
            if !replaced && !attachments.is_empty() {
                for attachment in attachments {
                    lines.extend(render_memo_attachment_lines(attachment));
                }
                replaced = true;
            }
            lines.extend(logical.physical);
            in_primary = false;
            primary_kind = None;
            continue;
        }

        lines.extend(logical.physical);
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth = nested_depth.saturating_add(1);
        } else if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            nested_depth = nested_depth.saturating_sub(1);
        }
    }
    if !replaced && !attachments.is_empty() {
        for attachment in attachments {
            lines.extend(render_memo_attachment_lines(attachment));
        }
    }
    if !replaced && attachments.is_empty() {
        return raw.to_string();
    }
    finish_physical_lines(lines)
}

fn replace_memo_component_alarms(raw: &str, alarms: &[Alarm]) -> String {
    let source = logical_lines(raw);
    let Some(start) = source
        .iter()
        .position(|line| is_primary_component_begin(&line.unfolded))
    else {
        return raw.to_string();
    };
    let Some(kind) = primary_component_kind(&source[start].unfolded) else {
        return raw.to_string();
    };
    let mut nested_depth = 0usize;
    let Some(end) = source
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| {
            if line
                .unfolded
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
            {
                nested_depth = nested_depth.saturating_add(1);
                return None;
            }
            if line
                .unfolded
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
            {
                if nested_depth == 0 && line.unfolded.eq_ignore_ascii_case(&format!("END:{kind}")) {
                    return Some(index);
                }
                nested_depth = nested_depth.saturating_sub(1);
            }
            None
        })
    else {
        return raw.to_string();
    };

    let component = finish_physical_lines(
        source[start..=end]
            .iter()
            .flat_map(|line| line.physical.iter().cloned())
            .collect(),
    );
    let original_alarms = parse_alarms(&component);
    let original_blocks = top_level_alarm_blocks(&component);
    let generated = replace_component_alarms(&component, alarms);
    let generated_blocks = top_level_alarm_blocks(&generated);
    let mut used_original = vec![false; original_alarms.len()];
    let replacement_blocks = alarms
        .iter()
        .enumerate()
        .map(|(index, alarm)| {
            let matching_original = original_alarms
                .iter()
                .enumerate()
                .find(|(old_index, old)| !used_original[*old_index] && *old == alarm);
            if let Some((old_index, _)) = matching_original {
                used_original[old_index] = true;
                original_blocks
                    .get(old_index)
                    .cloned()
                    .or_else(|| generated_blocks.get(index).cloned())
                    .unwrap_or_default()
            } else {
                generated_blocks.get(index).cloned().unwrap_or_default()
            }
        })
        .collect::<Vec<_>>();

    let mut output = source[..start]
        .iter()
        .flat_map(|line| line.physical.iter().cloned())
        .collect::<Vec<_>>();
    let mut alarm_index = 0usize;
    let mut index = start;
    let mut primary_nested_depth = 0usize;
    while index <= end {
        if index == start {
            output.extend(source[index].physical.iter().cloned());
            index += 1;
            continue;
        }
        let line = &source[index].unfolded;
        if line.eq_ignore_ascii_case("BEGIN:VALARM") && primary_nested_depth == 0 {
            let Some(alarm_end) = source
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, candidate)| candidate.unfolded.eq_ignore_ascii_case("END:VALARM"))
                .map(|(position, _)| position)
            else {
                output.extend(source[index].physical.iter().cloned());
                index += 1;
                continue;
            };
            if let Some(block) = replacement_blocks.get(alarm_index) {
                output.extend(block.iter().cloned());
                alarm_index += 1;
            }
            index = alarm_end + 1;
            continue;
        }
        if primary_nested_depth == 0 && line.eq_ignore_ascii_case(&format!("END:{kind}")) {
            output.extend(
                replacement_blocks
                    .iter()
                    .skip(alarm_index)
                    .flat_map(|block| block.iter().cloned()),
            );
            alarm_index = replacement_blocks.len();
            output.extend(source[index].physical.iter().cloned());
            index += 1;
            continue;
        }
        output.extend(source[index].physical.iter().cloned());
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            primary_nested_depth = primary_nested_depth.saturating_add(1);
        } else if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            primary_nested_depth = primary_nested_depth.saturating_sub(1);
        }
        index += 1;
    }
    output.extend(
        source[end + 1..]
            .iter()
            .flat_map(|line| line.physical.iter().cloned()),
    );
    finish_physical_lines(output)
}

fn top_level_alarm_blocks(raw: &str) -> Vec<Vec<String>> {
    let lines = logical_lines(raw);
    let Some(start) = lines
        .iter()
        .position(|line| is_primary_component_begin(&line.unfolded))
    else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    let mut index = start + 1;
    while index < lines.len() {
        if lines[index].unfolded.eq_ignore_ascii_case("BEGIN:VALARM") {
            let Some(end) = lines
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, line)| line.unfolded.eq_ignore_ascii_case("END:VALARM"))
                .map(|(position, _)| position)
            else {
                break;
            };
            blocks.push(
                lines[index..=end]
                    .iter()
                    .flat_map(|line| line.physical.iter().cloned())
                    .collect(),
            );
            index = end + 1;
        } else {
            index += 1;
        }
    }
    blocks
}

pub(crate) fn serialize_ical_attachment_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '\r' && *character != '\n')
        .collect()
}

fn serialize_memo_uri_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '\r' && *character != '\n')
        .fold(String::new(), |mut escaped, character| {
            match character {
                '\\' => escaped.push_str("\\\\"),
                ';' => escaped.push_str("\\;"),
                ',' => escaped.push_str("\\,"),
                _ => escaped.push(character),
            }
            escaped
        })
}

/// Return a standards-shaped iCalendar document for a single component.
///
/// ECal and the local parser work with bare VEVENT/VTODO/VJOURNAL components,
/// but exported files should contain a VCALENDAR envelope so they can be
/// opened by Evolution and other calendar applications without heuristics.
pub(crate) fn export_ical_component(raw: &str) -> String {
    let logical = logical_lines(raw);
    let lines = logical
        .iter()
        .flat_map(|line| line.physical.iter().cloned())
        .collect::<Vec<_>>();
    if logical
        .first()
        .is_some_and(|line| line.unfolded.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return finish_physical_lines(lines);
    }

    let mut result =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Meridian//EN\r\n");
    result.push_str(&lines.join("\r\n"));
    result.push_str("\r\nEND:VCALENDAR\r\n");
    result
}

fn format_ical_datetime(value: &str) -> String {
    let value = value.trim_end_matches('Z');
    if value.len() == 8 && value.chars().all(|character| character.is_ascii_digit()) {
        return format!("{}-{}-{}", &value[..4], &value[4..6], &value[6..8]);
    }
    if value.len() >= 15 && value.as_bytes().get(8) == Some(&b'T') {
        return format!(
            "{}-{}-{} {}:{}",
            &value[..4],
            &value[4..6],
            &value[6..8],
            &value[9..11],
            &value[11..13]
        );
    }
    value.to_string()
}

pub fn normalize_mid_uri(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let without_scheme = if trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("mid:") {
        &trimmed[4..]
    } else {
        trimmed
    };
    let cleaned = without_scheme
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if cleaned.is_empty()
        || cleaned
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '<' | '>'))
    {
        return None;
    }
    Some(format!("mid:{cleaned}"))
}

pub fn memo_linked_mid(component: &str) -> Option<String> {
    let properties = top_level_properties(component);
    for (prop, val, _) in &properties {
        if prop.eq_ignore_ascii_case("URL") {
            let unescaped = unescape(val);
            let trimmed = unescaped.trim();
            if trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("mid:") {
                if let Some(norm) = normalize_mid_uri(trimmed) {
                    return Some(norm);
                }
            }
        }
    }
    for (prop, val, _) in &properties {
        if prop.eq_ignore_ascii_case("RELATED-TO")
            || prop.eq_ignore_ascii_case("X-MERIDIAN-MESSAGE-ID")
        {
            let unescaped = unescape(val);
            if let Some(norm) = normalize_mid_uri(&unescaped) {
                return Some(norm);
            }
        }
    }
    None
}

pub fn memo_is_team_shared(memo: &MemoRecord) -> bool {
    let properties = top_level_properties(&memo.component);
    for (prop, val, _) in &properties {
        if prop.eq_ignore_ascii_case("CLASS") {
            let cls = unescape(val);
            let cls_trimmed = cls.trim();
            if cls_trimmed.eq_ignore_ascii_case("PUBLIC") {
                return true;
            }
            if cls_trimmed.eq_ignore_ascii_case("PRIVATE")
                || cls_trimmed.eq_ignore_ascii_case("CONFIDENTIAL")
            {
                return false;
            }
        }
    }
    !memo.source_uid.is_empty()
        && memo.source_uid != "local-calendar"
        && memo.source_uid != "system-memo-list"
}

pub fn build_mail_linked_memo_component(
    uid: &str,
    summary: &str,
    description: &str,
    mid_uri: &str,
    is_team: bool,
) -> String {
    let normalized_mid = normalize_mid_uri(mid_uri).unwrap_or_else(|| format!("mid:{}", mid_uri.trim()));
    let raw_mid = normalized_mid.strip_prefix("mid:").unwrap_or(&normalized_mid);
    let now_stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let today_date = chrono::Local::now().format("%Y%m%d").to_string();
    let class_val = if is_team { "PUBLIC" } else { "PRIVATE" };

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Meridian//Mail Linked Memo//EN".to_string(),
        "BEGIN:VJOURNAL".to_string(),
    ];
    append_folded_line(&mut lines, "UID", &strip_line_breaks(uid));
    append_folded_line(&mut lines, "DTSTAMP", &now_stamp);
    append_folded_line(&mut lines, "LAST-MODIFIED", &now_stamp);
    append_folded_line(&mut lines, "DTSTART;VALUE=DATE", &today_date);
    append_folded_line(&mut lines, "SUMMARY", &escape_ical_text_value(summary));
    append_folded_line(&mut lines, "DESCRIPTION", &escape_ical_text_value(description));
    append_folded_line(&mut lines, "CLASS", class_val);
    append_folded_line(&mut lines, "URL", &serialize_memo_uri_value(&normalized_mid));
    append_folded_line(&mut lines, "RELATED-TO", &escape_ical_text_value(raw_mid));
    lines.push("END:VJOURNAL".to_string());
    lines.push("END:VCALENDAR".to_string());
    finish_physical_lines(lines)
}

pub fn update_mail_linked_memo_component(
    existing_component: &str,
    summary: &str,
    description: &str,
    mid_uri: &str,
    is_team: bool,
) -> String {
    let normalized_mid = normalize_mid_uri(mid_uri).unwrap_or_else(|| format!("mid:{}", mid_uri.trim()));
    let raw_mid = normalized_mid.strip_prefix("mid:").unwrap_or(&normalized_mid);
    let now_stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let class_val = if is_team { "PUBLIC" } else { "PRIVATE" };

    let mut updated = update_ical_property(
        existing_component,
        "SUMMARY",
        Some(&escape_ical_text_value(summary)),
    );
    updated = update_ical_property(
        &updated,
        "DESCRIPTION",
        Some(&escape_ical_text_value(description)),
    );
    updated = update_ical_property(&updated, "CLASS", Some(class_val));
    updated = update_ical_property(
        &updated,
        "URL",
        Some(&serialize_memo_uri_value(&normalized_mid)),
    );
    updated = update_ical_property(
        &updated,
        "RELATED-TO",
        Some(&escape_ical_text_value(raw_mid)),
    );
    updated = update_ical_property(&updated, "LAST-MODIFIED", Some(&now_stamp));
    export_ical_component(&updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn parses_events_tasks_and_memos_from_one_calendar_file() {
        let root = temporary_directory("ical-roundtrip");
        let path = root.join("calendar.ics");
        fs::write(
            &path,
            concat!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
                "BEGIN:VEVENT\r\nUID:event-1\r\nSUMMARY:Team meeting\r\n",
                "DTSTART:20260810T120000Z\r\nDTEND:20260810T130000Z\r\n",
                "LOCATION:Room 1\r\nEND:VEVENT\r\n",
                "BEGIN:VTODO\r\nUID:task-1\r\nSUMMARY:Prepare report\r\n",
                "DUE;VALUE=DATE:20260811\r\nPRIORITY:1\r\nEND:VTODO\r\n",
                "BEGIN:VJOURNAL\r\nUID:memo-1\r\nSUMMARY:Note\r\n",
                "DESCRIPTION:Line one\\nLine two\r\nEND:VJOURNAL\r\n",
                "END:VCALENDAR\r\n"
            ),
        )
        .expect("calendar should be written");

        let snapshot = CalendarStoreService::with_calendar_dir(&root).load_snapshot();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(
            snapshot.events[0].start,
            property_local_display(
                "BEGIN:VEVENT\r\nDTSTART:20260810T120000Z\r\nEND:VEVENT\r\n",
                "DTSTART",
                false,
            )
            .unwrap()
        );
        assert_eq!(snapshot.tasks[0].due, "2026-08-11");
        assert_eq!(snapshot.tasks[0].priority, 1);
        assert_eq!(snapshot.memos[0].description, "Line one\nLine two");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_calendar_directory_is_empty_and_does_not_create_data() {
        let root = std::env::temp_dir().join(format!(
            "meridian-missing-ical-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            CalendarStoreService::with_calendar_dir(&root).load_snapshot(),
            CalendarSnapshot::default()
        );
    }

    #[test]
    fn imports_ical_files_without_overwriting_existing_files() {
        let source_root = temporary_directory("ical-import-source");
        let destination_root = temporary_directory("ical-import-destination");
        let source = source_root.join("calendar.ics");
        fs::write(&source, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
            .expect("source calendar should be written");
        fs::write(destination_root.join("calendar.ics"), "existing calendar\n")
            .expect("existing calendar should be written");

        let service = CalendarStoreService::with_calendar_dir(&destination_root);
        assert_eq!(
            service.import_files(std::slice::from_ref(&source)).unwrap(),
            1
        );
        assert_eq!(
            fs::read_to_string(destination_root.join("calendar.ics")).unwrap(),
            "existing calendar\n"
        );
        assert_eq!(
            fs::read_to_string(destination_root.join("calendar-1.ics")).unwrap(),
            "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"
        );

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(destination_root);
    }

    #[test]
    fn writable_local_store_persists_create_modify_and_remove() {
        let root = temporary_directory("ical-mutations");
        let service = CalendarStoreService::with_writable_calendar_dir(&root);
        let component = concat!(
            "BEGIN:VEVENT\r\nUID:event-local-1\r\nSUMMARY:Initial\r\n",
            "DTSTART:20260810T120000Z\r\nDTEND:20260810T130000Z\r\nEND:VEVENT\r\n"
        );
        let (uid, _) = service
            .create_component(component)
            .expect("event should be created");
        assert_eq!(uid, "event-local-1");
        assert_eq!(
            service.load_snapshot().events[0].source_uid,
            "local-calendar"
        );
        assert_eq!(service.load_snapshot().events[0].summary, "Initial");
        assert_eq!(
            service
                .modify_component(
                    &uid,
                    &component.replace("SUMMARY:Initial", "SUMMARY:Updated")
                )
                .expect("event should be modified")
                .0,
            uid
        );
        assert_eq!(service.load_snapshot().events[0].summary, "Updated");
        assert_eq!(
            service
                .remove_component(&uid)
                .expect("event should be removed"),
            uid
        );
        assert!(service.load_snapshot().events.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn purge_removes_only_proven_old_single_events_and_preserves_other_components() {
        let root = temporary_directory("ical-purge-safe");
        let path = root.join("calendar.ics");
        fs::write(
            &path,
            concat!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
                "BEGIN:VEVENT\r\nUID:old\r\nDTSTART:20260801T100000\r\n",
                "DTEND:20260801T110000\r\nEND:VEVENT\r\n",
                "BEGIN:VEVENT\r\nUID:boundary\r\nDTSTART:20260810T110000\r\n",
                "DTEND:20260810T120000\r\nEND:VEVENT\r\n",
                "BEGIN:VEVENT\r\nUID:future\r\nDTSTART:20260811T100000\r\n",
                "DTEND:20260811T110000\r\nEND:VEVENT\r\n",
                "BEGIN:VEVENT\r\nUID:series\r\nDTSTART:20260701T100000\r\n",
                "RRULE:FREQ=DAILY\r\nEND:VEVENT\r\n",
                "BEGIN:VEVENT\r\nUID:detached\r\nRECURRENCE-ID:20260702T100000\r\n",
                "DTSTART:20260702T100000\r\nEND:VEVENT\r\n",
                "BEGIN:VEVENT\r\nUID:duration\r\nDTSTART:20260703T100000\r\n",
                "DURATION:PT1H\r\nEND:VEVENT\r\n",
                "BEGIN:VTODO\r\nUID:task\r\nDTSTART:20260701T100000\r\nEND:VTODO\r\n",
                "BEGIN:VJOURNAL\r\nUID:memo\r\nDTSTART:20260701T100000\r\nEND:VJOURNAL\r\n",
                "END:VCALENDAR\r\n"
            ),
        )
        .expect("purge fixture should be written");
        let service = CalendarStoreService::with_writable_calendar_dir(&root);
        let cutoff =
            property_local_timestamp("DTEND", "20260810T120000", 0).expect("cutoff should parse");

        let result = service
            .purge_events_before(cutoff, &AtomicBool::new(false))
            .expect("safe purge should succeed");

        assert_eq!(result.removed_count, 1);
        assert_eq!(result.skipped_recurring_count, 2);
        assert_eq!(result.skipped_invalid_count, 1);
        let snapshot = service.load_snapshot();
        assert_eq!(
            snapshot
                .events
                .iter()
                .map(|event| event.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["series", "detached", "duration", "boundary", "future"]
        );
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.memos.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn purge_uses_absolute_boundaries_and_rejects_implicit_or_invalid_ends() {
        let cutoff = property_local_timestamp("DTEND", "20260810T100000Z", 0).unwrap();
        let event = |start: &str, end: Option<&str>| {
            format!(
                "BEGIN:VEVENT\r\nUID:event\r\nDTSTART:{start}\r\n{}END:VEVENT\r\n",
                end.map(|end| format!("DTEND:{end}\r\n"))
                    .unwrap_or_default()
            )
        };

        assert_eq!(
            calendar_event_purge_disposition(
                &event("20260810T090000Z", Some("20260810T100000Z")),
                cutoff,
            ),
            CalendarPurgeDisposition::Keep
        );
        assert_eq!(
            calendar_event_purge_disposition(
                &event("20260810T090000Z", Some("20260810T095959Z")),
                cutoff,
            ),
            CalendarPurgeDisposition::Remove
        );
        assert_eq!(
            calendar_event_purge_disposition(
                "BEGIN:VEVENT\r\nUID:all-day\r\nDTSTART;VALUE=DATE:20260810\r\nEND:VEVENT\r\n",
                cutoff,
            ),
            CalendarPurgeDisposition::Invalid
        );
        assert_eq!(
            calendar_event_purge_disposition(
                &event("20260810T110000Z", Some("20260810T100000Z")),
                cutoff,
            ),
            CalendarPurgeDisposition::Invalid
        );
        assert_eq!(
            calendar_event_purge_disposition(
                "BEGIN:VEVENT\r\nUID:mixed\r\nDTSTART;VALUE=DATE:20260801\r\nDTEND:20260802T100000Z\r\nEND:VEVENT\r\n",
                cutoff,
            ),
            CalendarPurgeDisposition::Invalid
        );
        assert_eq!(
            calendar_event_purge_disposition(
                &event("20260801T090000Z", Some("20260801T100000Zunexpected"),),
                cutoff,
            ),
            CalendarPurgeDisposition::Invalid
        );
        assert_eq!(
            calendar_event_purge_disposition(
                "BEGIN:VEVENT\r\nUID:invalid-tzid\r\nDTSTART;TZID=Europe/Berlin:20260801T090000Z\r\nDTEND;TZID=Europe/Berlin:20260801T100000Z\r\nEND:VEVENT\r\n",
                cutoff,
            ),
            CalendarPurgeDisposition::Invalid
        );
    }

    #[test]
    fn purge_skips_duplicate_uids_across_the_complete_local_source() {
        let root = temporary_directory("ical-purge-duplicate-source");
        let first = root.join("first.ics");
        let second = root.join("second.ics");
        let calendar = |summary: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:duplicate\r\nSUMMARY:{summary}\r\nDTSTART:20260701T100000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        let first_original = calendar("First");
        let second_original = calendar("Second");
        fs::write(&first, &first_original).unwrap();
        fs::write(&second, &second_original).unwrap();
        let cutoff = property_local_timestamp("DTEND", "20260810T120000", 0).unwrap();

        let result = CalendarStoreService::with_writable_calendar_dir(&root)
            .purge_events_before(cutoff, &AtomicBool::new(false))
            .expect("ambiguous UIDs must be skipped without failing the run");

        assert_eq!(result.removed_count, 0);
        assert_eq!(result.skipped_recurring_count, 2);
        assert_eq!(fs::read_to_string(first).unwrap(), first_original);
        assert_eq!(fs::read_to_string(second).unwrap(), second_original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn purge_cancellation_and_readonly_store_leave_existing_data_unchanged() {
        let root = temporary_directory("ical-purge-cancel");
        let path = root.join("calendar.ics");
        let original = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
            "BEGIN:VEVENT\r\nUID:old\r\nDTSTART:20260701T100000\r\nEND:VEVENT\r\n",
            "END:VCALENDAR\r\n"
        );
        fs::write(&path, original).expect("purge fixture should be written");
        let cutoff =
            property_local_timestamp("DTEND", "20260810T120000", 0).expect("cutoff should parse");
        let cancelled = AtomicBool::new(true);

        let failure = CalendarStoreService::with_writable_calendar_dir(&root)
            .purge_events_before(cutoff, &cancelled)
            .expect_err("cancelled purge must stop before writing");
        assert_eq!(failure.error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(failure.confirmed.removed_count, 0);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let failure = CalendarStoreService::with_calendar_dir(&root)
            .purge_events_before(cutoff, &AtomicBool::new(false))
            .expect_err("read-only store must reject purge");
        assert_eq!(failure.error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writable_local_store_persists_vtodo_status_and_vjournal_updates() {
        let root = temporary_directory("ical-task-memo-mutations");
        let service = CalendarStoreService::with_writable_calendar_dir(&root);
        let task = concat!(
            "BEGIN:VTODO\r\nUID:task-local-1\r\nSUMMARY:Open task\r\n",
            "STATUS:NEEDS-ACTION\r\nPERCENT-COMPLETE:0\r\n",
            "DUE;VALUE=DATE:20260812\r\nEND:VTODO\r\n"
        );
        let memo = concat!(
            "BEGIN:VJOURNAL\r\nUID:memo-local-1\r\nSUMMARY:First note\r\n",
            "DESCRIPTION:Initial body\r\nEND:VJOURNAL\r\n"
        );
        service
            .create_component(task)
            .expect("local task should be created");
        service
            .create_component(memo)
            .expect("local memo should be created");

        let completed_task = task
            .replace("STATUS:NEEDS-ACTION", "STATUS:COMPLETED")
            .replace("PERCENT-COMPLETE:0", "PERCENT-COMPLETE:100")
            .replace("END:VTODO", "COMPLETED:20260811T100000Z\r\nEND:VTODO");
        service
            .modify_component("task-local-1", &completed_task)
            .expect("local task should be modified");
        let updated_memo = memo
            .replace("SUMMARY:First note", "SUMMARY:Updated note")
            .replace("DESCRIPTION:Initial body", "DESCRIPTION:Updated body");
        service
            .modify_component("memo-local-1", &updated_memo)
            .expect("local memo should be modified");

        let snapshot = service.load_snapshot();
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].source_uid, "local-calendar");
        assert!(snapshot.tasks[0].completed);
        assert_eq!(snapshot.tasks[0].summary, "Open task");
        assert_eq!(snapshot.memos[0].summary, "Updated note");
        assert_eq!(snapshot.memos[0].description, "Updated body");

        service
            .remove_component("task-local-1")
            .expect("local task should be removed");
        service
            .remove_component("memo-local-1")
            .expect("local memo should be removed");
        assert!(service.load_snapshot().tasks.is_empty());
        assert!(service.load_snapshot().memos.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imported_calendar_store_rejects_mutations() {
        let root = temporary_directory("ical-read-only");
        let service = CalendarStoreService::with_calendar_dir(&root);
        assert!(
            service
                .create_component("BEGIN:VEVENT\r\nEND:VEVENT\r\n")
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writable_imported_component_without_uid_can_be_modified_and_removed() {
        let root = temporary_directory("ical-uidless-mutations");
        let path = root.join("imported.ics");
        let component = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
            "BEGIN:VEVENT\r\nSUMMARY:Imported\r\n",
            "DTSTART:20260810T120000Z\r\nDTEND:20260810T130000Z\r\n",
            "END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        fs::write(&path, component).expect("uid-less calendar should be written");
        let service = CalendarStoreService::with_writable_calendar_dir(&root);
        let uid = service.load_snapshot().events[0].uid.clone();
        assert!(uid.starts_with("local:"));

        let updated = component.replace("SUMMARY:Imported", "SUMMARY:Updated");
        service
            .modify_component(&uid, &updated)
            .expect("uid-less component should be modified");
        assert_eq!(service.load_snapshot().events[0].summary, "Updated");
        service
            .remove_component(&uid)
            .expect("uid-less component should be removed");
        assert!(service.load_snapshot().events.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_native_eds_component_with_source_identity() {
        let mut snapshot = CalendarSnapshot::default();
        parse_ical_component(
            concat!(
                "BEGIN:VEVENT\r\nUID:eds-event-1\r\nSUMMARY:Remote meeting\r\n",
                "DTSTART:20260810T120000Z\r\nDTEND:20260810T130000Z\r\n",
                "END:VEVENT\r\n"
            ),
            "source-uid",
            "Remote calendar",
            &mut snapshot,
        );

        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].source_uid, "source-uid");
        assert_eq!(snapshot.events[0].source_display_name, "Remote calendar");
        assert_eq!(snapshot.events[0].summary, "Remote meeting");
    }

    #[test]
    fn nested_alarm_properties_do_not_replace_event_properties() {
        let mut snapshot = CalendarSnapshot::default();
        parse_ical_component(
            concat!(
                "BEGIN:VEVENT\r\nUID:eds-event-2\r\nSUMMARY:Remote meeting\r\n",
                "DESCRIPTION:Event description\r\n",
                "BEGIN:VALARM\r\nDESCRIPTION:Alarm description\r\n",
                "END:VALARM\r\nEND:VEVENT\r\n"
            ),
            "source-uid",
            "Remote calendar",
            &mut snapshot,
        );

        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].description, "Event description");
    }

    #[test]
    fn component_properties_can_be_updated_without_losing_nested_data() {
        let raw = concat!(
            "BEGIN:VTODO\r\nUID:task-1\r\nSUMMARY:Keep this\r\n",
            "DESCRIPTION:Top-level description\r\n",
            "BEGIN:VALARM\r\nTRIGGER:-PT10M\r\n",
            "DESCRIPTION:Alarm description\r\nEND:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let updated = update_ical_property(raw, "STATUS", Some("COMPLETED"));
        let updated = update_ical_property(&updated, "PERCENT-COMPLETE", Some("100"));
        assert!(updated.contains("STATUS:COMPLETED\r\n"));
        assert!(updated.contains("PERCENT-COMPLETE:100\r\n"));
        assert!(updated.contains(
            "BEGIN:VALARM\r\nTRIGGER:-PT10M\r\nDESCRIPTION:Alarm description\r\nEND:VALARM"
        ));

        let removed = update_ical_property(&updated, "STATUS", None);
        assert!(!removed.contains("STATUS:"));
        assert!(removed.ends_with("END:VTODO\r\n"));
    }

    #[test]
    fn exports_bare_components_inside_a_vcalendar_envelope() {
        let event = "BEGIN:VEVENT\nUID:event-1\nSUMMARY:Meeting\nEND:VEVENT\n";
        let exported = export_ical_component(event);
        assert!(exported.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(exported.contains("BEGIN:VEVENT\r\nUID:event-1\r\n"));
        assert!(exported.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));

        let already_wrapped = export_ical_component(&exported);
        assert_eq!(already_wrapped.matches("BEGIN:VCALENDAR").count(), 1);

        let folded = concat!(
            "BEGIN:VJOURNAL\r\nUID:folded-export\r\n",
            "X-UNKNOWN:first part\r\n second part\r\nEND:VJOURNAL\r\n"
        );
        let folded_export = export_ical_component(folded);
        assert!(folded_export.contains("X-UNKNOWN:first part\r\n second part\r\n"));
    }

    #[test]
    fn memo_draft_roundtrips_uri_inline_unknown_data_and_managed_parameters() {
        let raw = concat!(
            "BEGIN:VJOURNAL\r\n",
            "UID:memo-editor-1\r\n",
            "SUMMARY:Original note\r\n",
            "DTSTART;VALUE=DATE;X-DATE=keep:20260822\r\n",
            "CLASS;X-CLASS=keep:PRIVATE\r\n",
            "STATUS;X-STATUS=keep:DRAFT\r\n",
            "URL;VALUE=URI;X-URL=keep:https://example.test/note\r\n",
            "CATEGORIES:Work\\, urgent,Personal\r\n",
            "DESCRIPTION:Original body\\nsecond line\r\n",
            "ATTACH;VALUE=URI;FILENAME=note.txt;FMTTYPE=text/plain;X-ATTACH=uri:https://example.test/note.txt\r\n",
            "ATTACH;VALUE=BINARY;ENCODING=BASE64;FMTTYPE=image/png;X-ATTACH=inline:UE5H\r\n",
            "X-MEMO-UNKNOWN;X-PARAM=keep:preserve me\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "TRIGGER:-PT10M\r\n",
            "X-ALARM-UNKNOWN:preserve\r\n",
            "END:VALARM\r\n",
            "END:VJOURNAL\r\n"
        );

        let mut draft = MemoEditorDraft::from_component(raw).expect("VJOURNAL should parse");
        assert_eq!(draft.summary, "Original note");
        assert_eq!(draft.dtstart.as_deref(), Some("20260822"));
        assert_eq!(draft.url.as_deref(), Some("https://example.test/note"));
        assert_eq!(draft.categories, ["Work, urgent", "Personal"]);
        assert_eq!(draft.attachments.len(), 2);
        assert_eq!(
            draft.attachments[0].uri.as_deref(),
            Some("https://example.test/note.txt")
        );
        assert_eq!(draft.attachments[0].filename.as_deref(), Some("note.txt"));
        assert_eq!(draft.attachments[0].fmttype.as_deref(), Some("text/plain"));
        assert_eq!(draft.attachments[0].unknown_parameters[0].name, "X-ATTACH");
        assert_eq!(draft.attachments[1].inline_content.as_deref(), Some("UE5H"));
        assert!(!draft.is_dirty());
        assert_eq!(
            draft.apply().expect("unchanged draft applies losslessly"),
            raw
        );

        draft.summary = "Changed note".to_string();
        let changed = draft.apply().expect("changed draft should apply");
        assert!(changed.contains("SUMMARY:Changed note\r\n"));
        assert!(changed.contains("DTSTART;VALUE=DATE;X-DATE=keep:20260822\r\n"));
        assert!(changed.contains("URL;VALUE=URI;X-URL=keep:https://example.test/note\r\n"));
        assert!(changed.contains("X-MEMO-UNKNOWN;X-PARAM=keep:preserve me\r\n"));
        assert!(changed.contains("X-ALARM-UNKNOWN:preserve\r\n"));
        assert!(changed.contains(
            "ATTACH;VALUE=BINARY;ENCODING=BASE64;FMTTYPE=image/png;X-ATTACH=inline:UE5H\r\n"
        ));
    }

    #[test]
    fn memo_draft_applies_all_managed_vjournal_properties() {
        let raw = concat!(
            "BEGIN:VJOURNAL\r\n",
            "UID:memo-managed-fields\r\n",
            "SUMMARY:Old summary\r\n",
            "DTSTART;VALUE=DATE;X-DATE=keep:20260822\r\n",
            "CLASS;X-CLASS=keep:PUBLIC\r\n",
            "STATUS;X-STATUS=keep:DRAFT\r\n",
            "URL;VALUE=URI;X-URL=keep:https://example.test/old\r\n",
            "CATEGORIES:Old\r\n",
            "DESCRIPTION:Old description\r\n",
            "X-UNKNOWN;X-PARAM=keep:untouched\r\n",
            "END:VJOURNAL\r\n"
        );
        let mut draft = MemoEditorDraft::from_component(raw).expect("memo should parse");
        draft.summary = "New, summary".to_string();
        draft.dtstart = Some("20260823".to_string());
        draft.class = Some("PRIVATE".to_string());
        draft.status = Some("COMPLETED".to_string());
        draft.url = Some("https://example.test/new".to_string());
        draft.categories = vec!["Work, urgent".to_string(), "Personal".to_string()];
        draft.description = "New description\nsecond line".to_string();

        let updated = draft.apply().expect("all managed fields should apply");
        assert!(updated.contains("SUMMARY:New\\, summary\r\n"));
        assert!(updated.contains("DTSTART;VALUE=DATE;X-DATE=keep:20260823\r\n"));
        assert!(updated.contains("CLASS;X-CLASS=keep:PRIVATE\r\n"));
        assert!(updated.contains("STATUS;X-STATUS=keep:COMPLETED\r\n"));
        assert!(updated.contains("URL;VALUE=URI;X-URL=keep:https://example.test/new\r\n"));
        assert!(updated.contains("CATEGORIES:Work\\, urgent,Personal\r\n"));
        assert!(updated.contains("DESCRIPTION:New description\\nsecond line\r\n"));
        assert!(updated.contains("X-UNKNOWN;X-PARAM=keep:untouched\r\n"));
    }

    #[test]
    fn memo_apply_updates_only_the_first_vjournal_inside_a_vcalendar() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
            "X-CALENDAR-UNKNOWN:keep\r\n",
            "BEGIN:VJOURNAL\r\nUID:memo-wrapper\r\nSUMMARY:Old\r\n",
            "ATTACH;VALUE=URI;FILENAME=old.txt;X-UNKNOWN=keep:https://example.test/a\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
            "END:VJOURNAL\r\n",
            "BEGIN:VEVENT\r\nUID:other-component\r\nSUMMARY:Other\r\n",
            "ATTACH;VALUE=URI:https://example.test/other\r\n",
            "END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let mut draft = MemoEditorDraft::from_component(raw).expect("wrapped memo should parse");
        draft.summary = "Changed".to_string();
        draft.attachments[0].filename = Some("new.txt".to_string());
        let updated = draft.apply().expect("wrapped memo should apply");

        assert_eq!(updated.matches("SUMMARY:Changed\r\n").count(), 1);
        assert_eq!(updated.matches("ATTACH;").count(), 2);
        assert!(updated.contains("X-CALENDAR-UNKNOWN:keep\r\n"));
        assert!(updated.contains("ATTACH;VALUE=URI;FILENAME=new.txt;X-UNKNOWN=keep"));
        assert!(updated.contains("ATTACH;VALUE=URI:https://example.test/other\r\n"));
        assert!(updated.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\n"));
    }

    #[test]
    fn memo_apply_preserves_unmanaged_folding_and_is_idempotent() {
        let raw = concat!(
            "BEGIN:VJOURNAL\r\nUID:memo-folding\r\nSUMMARY:Old\r\n",
            "X-UNMANAGED;X-PARAM=keep:first physical part\r\n",
            " continuation physical part\r\n",
            "END:VJOURNAL\r\n"
        );
        let mut draft = MemoEditorDraft::from_component(raw).expect("memo should parse");
        draft.summary = "A deliberately long summary that must be folded at the RFC5545 line boundary without changing any unrelated property".to_string();
        let updated = draft.apply().expect("long summary should apply");
        assert!(updated.contains(
            "X-UNMANAGED;X-PARAM=keep:first physical part\r\n continuation physical part\r\n"
        ));
        assert!(
            unfold_lines(&updated)
                .iter()
                .any(|line| { line.starts_with("SUMMARY:A deliberately long summary") })
        );
        assert!(
            updated
                .lines()
                .filter(|line| !line.is_empty())
                .all(|line| line.len() <= 75)
        );
        assert_eq!(
            draft.apply().expect("repeated apply should be stable"),
            updated
        );
        let reparsed = MemoEditorDraft::from_component(&updated).expect("folded memo should parse");
        assert_eq!(
            reparsed
                .apply()
                .expect("unchanged folded memo should roundtrip"),
            updated
        );
    }

    #[test]
    fn memo_attach_switches_uri_and_inline_semantics_without_stale_controls() {
        let raw = concat!(
            "BEGIN:VJOURNAL\r\nUID:memo-attach-switch\r\nSUMMARY:Attachments\r\n",
            "ATTACH;VALUE=URI;FILENAME=first.txt;FMTTYPE=text/plain;X-DUP=one;X-DUP=two:https://example.test/a\r\n",
            "ATTACH;VALUE=BINARY;ENCODING=8BIT;FILENAME=second.txt;FMTTYPE=text/plain;X-KEEP=yes:literal\r\n",
            "END:VJOURNAL\r\n"
        );
        let mut draft = MemoEditorDraft::from_component(raw).expect("memo should parse");
        draft.attachments[0].uri = None;
        draft.attachments[0].inline_content = Some("A,B;C\\D".to_string());
        draft.attachments[1].uri = Some("file:///tmp/a,b;c\\d".to_string());
        draft.attachments[1].inline_content = None;
        let updated = draft.apply().expect("attachment kind changes should apply");
        let lines = unfold_lines(&updated);
        let first = lines
            .iter()
            .find(|line| line.starts_with("ATTACH;VALUE=BINARY"))
            .expect("inline attachment should be present");
        let second = lines
            .iter()
            .find(|line| line.contains("FILENAME=second.txt"))
            .expect("URI attachment should be present");
        assert!(first.contains("X-DUP=one;X-DUP=two:A,B;C\\D"));
        assert!(
            second
                .starts_with("ATTACH;VALUE=URI;FILENAME=second.txt;FMTTYPE=text/plain;X-KEEP=yes:")
        );
        assert!(!second.contains("ENCODING="));
        assert!(second.ends_with("file:///tmp/a\\,b\\;c\\\\d"));
        let reparsed =
            MemoEditorDraft::from_component(&updated).expect("switched memo should parse");
        assert_eq!(
            reparsed.attachments[0].inline_content.as_deref(),
            Some("A,B;C\\D")
        );
        assert_eq!(
            reparsed.attachments[1].uri.as_deref(),
            Some("file:///tmp/a,b;c\\d")
        );

        let mut removed_parameter_draft =
            MemoEditorDraft::from_component(raw).expect("memo should parse for parameter removal");
        removed_parameter_draft.attachments[0]
            .unknown_parameters
            .remove(0);
        let removed_parameter = removed_parameter_draft
            .apply()
            .expect("unknown parameter removal should apply");
        let removed_line = unfold_lines(&removed_parameter)
            .into_iter()
            .find(|line| line.starts_with("ATTACH;VALUE=URI"))
            .expect("URI attachment should remain");
        assert!(!removed_line.contains("X-DUP=one"));
        assert!(removed_line.contains("X-DUP=two:"));
    }

    #[test]
    fn memo_dtstart_preserves_date_and_tzid_semantics() {
        let date_raw = concat!(
            "BEGIN:VJOURNAL\r\nUID:memo-date\r\nSUMMARY:Old\r\n",
            "DTSTART;VALUE=DATE;X-DATE=keep:20260822\r\nEND:VJOURNAL\r\n"
        );
        let mut date_draft = MemoEditorDraft::from_component(date_raw).expect("date memo parses");
        date_draft.dtstart = Some("20260823".to_string());
        assert!(
            date_draft
                .apply()
                .expect("date-only DTSTART should apply")
                .contains("DTSTART;VALUE=DATE;X-DATE=keep:20260823\r\n")
        );
        date_draft.dtstart = Some("20260823T120000Z".to_string());
        assert!(date_draft.apply().is_err());

        let timed_raw = concat!(
            "BEGIN:VJOURNAL\r\nUID:memo-time\r\nSUMMARY:Old\r\n",
            "DTSTART;TZID=Europe/Berlin;X-TZ=keep:20260822T120000\r\nEND:VJOURNAL\r\n"
        );
        let mut timed_draft =
            MemoEditorDraft::from_component(timed_raw).expect("timed memo parses");
        timed_draft.dtstart = Some("20260823T130000".to_string());
        assert!(
            timed_draft
                .apply()
                .expect("TZID DTSTART should apply")
                .contains("DTSTART;TZID=Europe/Berlin;X-TZ=keep:20260823T130000\r\n")
        );
    }

    #[test]
    fn memo_alarm_replace_is_scoped_and_keeps_other_component_alarms() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
            "BEGIN:VJOURNAL\r\nUID:memo-alarm-scope\r\nSUMMARY:Memo\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
            "END:VJOURNAL\r\n",
            "BEGIN:VEVENT\r\nUID:event-alarm-scope\r\nSUMMARY:Event\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\n",
            "END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let mut draft = MemoEditorDraft::from_component(raw).expect("wrapped memo parses");
        draft.alarms.clear();
        let updated = draft.apply().expect("memo alarm removal should apply");
        assert_eq!(updated.matches("BEGIN:VALARM").count(), 1);
        assert!(updated.contains("TRIGGER:-PT10M\r\n"));
        assert!(!updated.contains("TRIGGER:-PT5M\r\n"));
    }

    #[test]
    fn memo_attachment_edits_keep_unknown_parameters_and_order() {
        let raw = concat!(
            "BEGIN:VJOURNAL\r\nUID:memo-attachments\r\nSUMMARY:Attachments\r\n",
            "ATTACH;VALUE=URI;FILENAME=old.txt;FMTTYPE=text/plain;X-ONE=1;X-TWO=2:https://example.test/a\r\n",
            "ATTACH;VALUE=BINARY;ENCODING=8BIT;FMTTYPE=text/plain;X-INLINE=yes:literal\r\n",
            "END:VJOURNAL\r\n"
        );
        let mut draft = MemoEditorDraft::from_component(raw).expect("memo should parse");
        draft.attachments[0].filename = Some("new.txt".to_string());
        draft.attachments[1].inline_content = Some("changed-inline".to_string());
        let updated = draft.apply().expect("attachment edits should apply");
        let unfolded = unfold_lines(&updated);
        assert!(unfolded.contains(
            &"ATTACH;VALUE=URI;FILENAME=new.txt;FMTTYPE=text/plain;X-ONE=1;X-TWO=2:https://example.test/a"
                .to_string()
        ));
        assert!(
            unfolded.contains(
                &"ATTACH;VALUE=BINARY;ENCODING=8BIT;FMTTYPE=text/plain;X-INLINE=yes:changed-inline"
                    .to_string()
            )
        );
        let reparsed = MemoEditorDraft::from_component(&updated).expect("updated memo parses");
        assert_eq!(reparsed.attachments.len(), 2);
        assert_eq!(reparsed.attachments[0].filename.as_deref(), Some("new.txt"));
        assert_eq!(
            reparsed.attachments[1].inline_content.as_deref(),
            Some("changed-inline")
        );

        draft.attachments[0].unknown_parameters[1].value = Some("updated".to_string());
        let updated_parameter = draft
            .apply()
            .expect("changing an unknown parameter should apply");
        assert!(unfold_lines(&updated_parameter).contains(
            &"ATTACH;VALUE=URI;FILENAME=new.txt;FMTTYPE=text/plain;X-ONE=1;X-TWO=updated:https://example.test/a"
                .to_string()
        ));
    }

    #[test]
    fn atomic_write_flushes_successfully_and_cleans_failed_temporary_files() {
        let root = temporary_directory("ical-atomic-write");
        let path = root.join("calendar.ics");
        let old = b"old bytes\r\n";
        let new = b"new bytes\r\n";
        write_atomic(&path, old).expect("initial atomic write should succeed");
        assert_eq!(fs::read(&path).expect("old file should exist"), old);
        write_atomic(&path, new).expect("replacement atomic write should succeed");
        assert_eq!(fs::read(&path).expect("new file should exist"), new);
        assert!(
            !fs::read_dir(&root)
                .expect("atomic directory should be readable")
                .flatten()
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".calendar.ics.")
                    && entry.file_name().to_string_lossy().ends_with(".tmp"))
                && !fs::read_dir(&root)
                    .expect("atomic directory should be readable")
                    .flatten()
                    .any(|entry| entry.file_name().to_string_lossy() == ".calendar.ics.lock")
        );

        let target_directory = root.join("target.ics");
        fs::create_dir(&target_directory).expect("failure target directory should exist");
        let sentinel = target_directory.join("sentinel");
        fs::write(&sentinel, b"unchanged").expect("sentinel should exist");
        assert!(write_atomic(&target_directory, b"must fail").is_err());
        assert_eq!(
            fs::read(&sentinel).expect("sentinel should remain"),
            b"unchanged"
        );
        assert!(
            !fs::read_dir(&root)
                .expect("atomic directory should be readable after failure")
                .flatten()
                .any(
                    |entry| entry.file_name().to_string_lossy().contains(".target.ics.")
                        && entry.file_name().to_string_lossy().ends_with(".tmp")
                )
                && !fs::read_dir(&root)
                    .expect("atomic directory should be readable after failure")
                    .flatten()
                    .any(|entry| entry.file_name().to_string_lossy() == ".target.ics.lock")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replacement_preserves_existing_file_mode() {
        let root = temporary_directory("ical-atomic-mode");
        let path = root.join("calendar.ics");
        write_atomic(&path, b"old\r\n").expect("initial write should succeed");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("test mode should be set");
        write_atomic(&path, b"new\r\n").expect("replacement should succeed");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read(&path).unwrap(), b"new\r\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_store_serializes_concurrent_read_modify_writers() {
        let root = temporary_directory("ical-concurrent-writers");
        let service = Arc::new(CalendarStoreService::with_writable_calendar_dir(&root));
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let service = Arc::clone(&service);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let component = format!(
                        "BEGIN:VJOURNAL\r\nUID:concurrent-{index}\r\nSUMMARY:Memo {index}\r\nEND:VJOURNAL\r\n"
                    );
                    service
                        .create_component(&component)
                        .expect("concurrent memo creation should succeed");
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("writer thread should finish");
        }
        assert_eq!(service.load_snapshot().memos.len(), 8);
        assert!(!fs::read_dir(&root).unwrap().flatten().any(|entry| {
            let name = entry.file_name();
            name.to_string_lossy().ends_with(".tmp") || name.to_string_lossy().ends_with(".lock")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_parse_ical_resolves_source_metadata_from_component() {
        let ics = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:cal-ev-1\r\n\
DTSTART:20260915T090000\r\n\
DTEND:20260915T100000\r\n\
SUMMARY:Team Sync\r\n\
X-EVOLUTION-SOURCE-NAME:Arbeit\r\n\
X-EVOLUTION-SOURCE-UID:macos:calendar:12:GoogleAcc:work-cal-id\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:cal-ev-2\r\n\
DTSTART:20260915T120000\r\n\
DTEND:20260915T130000\r\n\
SUMMARY:Mittagessen\r\n\
CATEGORIES:Privat\r\n\
END:VEVENT\r\n\
BEGIN:VTODO\r\n\
UID:cal-todo-1\r\n\
SUMMARY:Einkaufen\r\n\
X-EVOLUTION-SOURCE-NAME:Einkaufsliste\r\n\
END:VTODO\r\n\
END:VCALENDAR\r\n";

        let mut snapshot = CalendarSnapshot::default();
        parse_ical(ics, Path::new("test.ics"), "local-calendar", "Lokaler Kalender", &mut snapshot);

        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.events[0].source_display_name, "Arbeit");
        assert_eq!(snapshot.events[0].source_uid, "macos:calendar:12:GoogleAcc:work-cal-id");

        assert_eq!(snapshot.events[1].source_display_name, "Privat");
        assert_eq!(snapshot.events[1].source_uid, "calendar-Privat");

        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].source_display_name, "Einkaufsliste");
    }

    #[test]
    fn test_calendar_snapshot_range_and_source_filtering() {
        let mut snapshot = CalendarSnapshot::default();
        snapshot.events.push(CalendarEventRecord {
            uid: "ev-1".to_string(),
            source_uid: "src-work".to_string(),
            source_display_name: "Work".to_string(),
            component: String::new(),
            summary: "Sprint Planning".to_string(),
            start: "2026-09-20T09:00:00".to_string(),
            end: "2026-09-20T10:00:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        });
        snapshot.events.push(CalendarEventRecord {
            uid: "ev-2".to_string(),
            source_uid: "src-home".to_string(),
            source_display_name: "Home".to_string(),
            component: String::new(),
            summary: "Dinner".to_string(),
            start: "2026-09-25T19:00:00".to_string(),
            end: "2026-09-25T21:00:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        });

        // Test range filtering
        let sep25 = snapshot.events_in_range("2026-09-25", "2026-09-25T23:59:59");
        assert_eq!(sep25.len(), 1);
        assert_eq!(sep25[0].uid, "ev-2");

        let all = snapshot.events_in_range("2026-09-01", "2026-09-30");
        assert_eq!(all.len(), 2);

        let none = snapshot.events_in_range("2026-10-01", "2026-10-31");
        assert_eq!(none.len(), 0);

        // Test source filtering
        let work = snapshot.events_for_source("src-work");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].uid, "ev-1");

        let home = snapshot.events_for_source("src-home");
        assert_eq!(home.len(), 1);
        assert_eq!(home[0].uid, "ev-2");
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("meridian-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
