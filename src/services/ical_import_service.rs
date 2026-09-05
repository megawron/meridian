// SPDX-License-Identifier: GPL-3.0-or-later
//
// ical_import_service.rs — Upstream parity parser, preview planner, and batch execution for iCalendar (.ics) and vCalendar (.vcs) data.

use std::collections::HashSet;
use std::io;

use crate::services::calendar_store_service::CalendarStoreService;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IcalComponentKind {
    Event,
    Task,
    Memo,
    Timezone,
    Unknown,
}

impl IcalComponentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Event => "VEVENT",
            Self::Task => "VTODO",
            Self::Memo => "VJOURNAL",
            Self::Timezone => "VTIMEZONE",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Event => "Termin",
            Self::Task => "Aufgabe",
            Self::Memo => "Notiz",
            Self::Timezone => "Zeitzone",
            Self::Unknown => "Unbekannt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcalImportItem {
    pub uid: String,
    pub kind: IcalComponentKind,
    pub summary: String,
    pub dtstart: Option<String>,
    pub dtend_or_due: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub is_recurring: bool,
    pub alarm_count: usize,
    pub raw_component: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IcalDuplicatePolicy {
    #[default]
    KeepBothNewUid,
    OverwriteExisting,
    SkipExisting,
}

impl IcalDuplicatePolicy {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::KeepBothNewUid => "Beide behalten (neue Kennung vergeben)",
            Self::OverwriteExisting => "Bestehende Einträge überschreiben",
            Self::SkipExisting => "Bereits vorhandene Einträge überspringen",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcalImportPlan {
    pub items: Vec<IcalImportItem>,
    pub event_count: usize,
    pub task_count: usize,
    pub memo_count: usize,
    pub duplicate_uids: Vec<String>,
    pub policy: IcalDuplicatePolicy,
}

impl IcalImportPlan {
    pub fn summary_label(&self) -> String {
        let mut parts = Vec::new();
        if self.event_count > 0 {
            parts.push(format!(
                "{} {}",
                self.event_count,
                if self.event_count == 1 {
                    "Termin"
                } else {
                    "Termine"
                }
            ));
        }
        if self.task_count > 0 {
            parts.push(format!(
                "{} {}",
                self.task_count,
                if self.task_count == 1 {
                    "Aufgabe"
                } else {
                    "Aufgaben"
                }
            ));
        }
        if self.memo_count > 0 {
            parts.push(format!(
                "{} {}",
                self.memo_count,
                if self.memo_count == 1 {
                    "Notiz"
                } else {
                    "Notizen"
                }
            ));
        }
        if parts.is_empty() {
            "Keine importierbaren Einträge gefunden".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IcalImportResult {
    pub imported_count: usize,
    pub overwritten_count: usize,
    pub skipped_count: usize,
    pub created_uids: Vec<String>,
}

pub fn unfold_ical_lines(content: &str) -> Vec<String> {
    let mut unfolded: Vec<String> = Vec::new();
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.lines() {
        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            let last = unfolded.last_mut().expect("checked not empty");
            last.push_str(&line[1..]);
        } else if !line.trim().is_empty() {
            unfolded.push(line.to_string());
        }
    }
    unfolded
}

pub fn parse_ical_stream(content: &str) -> Vec<IcalImportItem> {
    let lines = unfold_ical_lines(content);
    let mut items = Vec::new();
    let mut current_block: Option<(IcalComponentKind, Vec<String>)> = None;

    for line in lines {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();

        if upper.starts_with("BEGIN:") {
            let block_type = &upper["BEGIN:".len()..];
            let kind = match block_type {
                "VEVENT" => Some(IcalComponentKind::Event),
                "VTODO" => Some(IcalComponentKind::Task),
                "VJOURNAL" => Some(IcalComponentKind::Memo),
                "VTIMEZONE" => Some(IcalComponentKind::Timezone),
                _ => None,
            };

            if let Some(k) = kind {
                if current_block.is_none() {
                    current_block = Some((k, vec![line]));
                    continue;
                }
            }
        }

        if let Some((ref kind, ref mut block_lines)) = current_block {
            block_lines.push(line.clone());
            let end_tag = format!("END:{}", kind.as_str());
            if upper.starts_with(&end_tag) {
                let finished_kind = *kind;
                let finished_lines = std::mem::take(block_lines);
                current_block = None;

                if finished_kind != IcalComponentKind::Timezone {
                    if let Some(item) = parse_component_lines(finished_kind, &finished_lines) {
                        items.push(item);
                    }
                }
            }
        }
    }

    items
}

fn parse_component_lines(kind: IcalComponentKind, lines: &[String]) -> Option<IcalImportItem> {
    let mut uid = String::new();
    let mut summary = String::new();
    let mut dtstart = None;
    let mut dtend_or_due = None;
    let mut description = None;
    let mut location = None;
    let mut is_recurring = false;
    let mut alarm_count = 0usize;
    let mut in_sub_component = false;

    for line in lines {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();

        if upper.starts_with("BEGIN:") {
            let sub = &upper["BEGIN:".len()..];
            if sub != kind.as_str() {
                in_sub_component = true;
                if sub == "VALARM" {
                    alarm_count += 1;
                }
            }
            continue;
        }

        if upper.starts_with("END:") {
            let sub = &upper["END:".len()..];
            if sub != kind.as_str() {
                in_sub_component = false;
            }
            continue;
        }

        if in_sub_component {
            continue;
        }

        if upper.starts_with("UID:") || upper.starts_with("UID;") {
            if let Some(idx) = trimmed.find(':') {
                uid = trimmed[idx + 1..].trim().to_string();
            }
        } else if upper.starts_with("SUMMARY:") || upper.starts_with("SUMMARY;") {
            if let Some(idx) = trimmed.find(':') {
                summary = unescape_ical_text(trimmed[idx + 1..].trim());
            }
        } else if upper.starts_with("DTSTART:") || upper.starts_with("DTSTART;") {
            if let Some(idx) = trimmed.find(':') {
                dtstart = Some(trimmed[idx + 1..].trim().to_string());
            }
        } else if upper.starts_with("DTEND:")
            || upper.starts_with("DTEND;")
            || upper.starts_with("DUE:")
            || upper.starts_with("DUE;")
        {
            if let Some(idx) = trimmed.find(':') {
                dtend_or_due = Some(trimmed[idx + 1..].trim().to_string());
            }
        } else if upper.starts_with("DESCRIPTION:") || upper.starts_with("DESCRIPTION;") {
            if let Some(idx) = trimmed.find(':') {
                description = Some(unescape_ical_text(trimmed[idx + 1..].trim()));
            }
        } else if upper.starts_with("LOCATION:") || upper.starts_with("LOCATION;") {
            if let Some(idx) = trimmed.find(':') {
                location = Some(unescape_ical_text(trimmed[idx + 1..].trim()));
            }
        } else if upper.starts_with("RRULE:")
            || upper.starts_with("RDATE:")
            || upper.starts_with("EXDATE:")
        {
            is_recurring = true;
        }
    }

    if uid.is_empty() {
        uid = generate_import_uid();
    }

    if summary.is_empty() {
        summary = match kind {
            IcalComponentKind::Event => "Unbenannter Termin".to_string(),
            IcalComponentKind::Task => "Unbenannte Aufgabe".to_string(),
            IcalComponentKind::Memo => "Unbenannte Notiz".to_string(),
            _ => "Unbenannter Eintrag".to_string(),
        };
    }

    let raw_component = lines.join("\r\n");

    Some(IcalImportItem {
        uid,
        kind,
        summary,
        dtstart,
        dtend_or_due,
        description,
        location,
        is_recurring,
        alarm_count,
        raw_component,
    })
}

pub fn unescape_ical_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                match next {
                    'n' | 'N' => {
                        chars.next();
                        out.push('\n');
                    }
                    '\\' => {
                        chars.next();
                        out.push('\\');
                    }
                    ';' => {
                        chars.next();
                        out.push(';');
                    }
                    ',' => {
                        chars.next();
                        out.push(',');
                    }
                    _ => {
                        out.push(c);
                    }
                }
            } else {
                out.push(c);
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn generate_import_uid() -> String {
    format!("import-{}", uuid::Uuid::new_v4())
}

pub fn replace_component_uid(raw_component: &str, new_uid: &str) -> String {
    let mut lines: Vec<String> = raw_component.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    for line in &mut lines {
        let upper = line.trim().to_ascii_uppercase();
        if upper.starts_with("UID:") || upper.starts_with("UID;") {
            *line = format!("UID:{new_uid}");
            found = true;
            break;
        }
    }
    if !found {
        if lines.len() > 1 {
            lines.insert(1, format!("UID:{new_uid}"));
        } else {
            lines.push(format!("UID:{new_uid}"));
        }
    }
    lines.join("\r\n")
}

pub fn wrap_in_vcalendar_envelope(component: &str) -> String {
    let trimmed = component.trim();
    if trimmed.to_ascii_uppercase().starts_with("BEGIN:VCALENDAR") {
        return format!("{trimmed}\r\n");
    }
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Evolution GTK4 Importer//EN\r\n{trimmed}\r\nEND:VCALENDAR\r\n"
    )
}

pub fn plan_ical_import(
    content: &str,
    existing_uids: &HashSet<String>,
    policy: IcalDuplicatePolicy,
) -> IcalImportPlan {
    let items = parse_ical_stream(content);
    let mut event_count = 0usize;
    let mut task_count = 0usize;
    let mut memo_count = 0usize;
    let mut duplicate_uids = Vec::new();

    for item in &items {
        match item.kind {
            IcalComponentKind::Event => event_count += 1,
            IcalComponentKind::Task => task_count += 1,
            IcalComponentKind::Memo => memo_count += 1,
            _ => {}
        }
        if existing_uids.contains(&item.uid) {
            duplicate_uids.push(item.uid.clone());
        }
    }

    IcalImportPlan {
        items,
        event_count,
        task_count,
        memo_count,
        duplicate_uids,
        policy,
    }
}

pub fn execute_ical_import(
    plan: &IcalImportPlan,
    store: &CalendarStoreService,
) -> io::Result<IcalImportResult> {
    let mut result = IcalImportResult::default();
    let snapshot = store.load_snapshot();
    let mut existing_uids = HashSet::new();
    for event in snapshot.events {
        existing_uids.insert(event.uid);
    }
    for task in snapshot.tasks {
        existing_uids.insert(task.uid);
    }
    for memo in snapshot.memos {
        existing_uids.insert(memo.uid);
    }

    for item in &plan.items {
        let is_duplicate = existing_uids.contains(&item.uid);

        match plan.policy {
            IcalDuplicatePolicy::SkipExisting if is_duplicate => {
                result.skipped_count += 1;
                continue;
            }
            IcalDuplicatePolicy::OverwriteExisting if is_duplicate => {
                match store.modify_component(&item.uid, &item.raw_component) {
                    Ok(_) => {
                        result.overwritten_count += 1;
                    }
                    Err(err) => {
                        rollback_created_components(store, &result.created_uids);
                        return Err(err);
                    }
                }
            }
            IcalDuplicatePolicy::KeepBothNewUid if is_duplicate => {
                let fresh_uid = generate_import_uid();
                let updated_component = replace_component_uid(&item.raw_component, &fresh_uid);
                match store.create_component(&updated_component) {
                    Ok((created_uid, _)) => {
                        result.created_uids.push(created_uid);
                        result.imported_count += 1;
                    }
                    Err(err) => {
                        rollback_created_components(store, &result.created_uids);
                        return Err(err);
                    }
                }
            }
            _ => {
                // New component
                match store.create_component(&item.raw_component) {
                    Ok((created_uid, _)) => {
                        result.created_uids.push(created_uid);
                        result.imported_count += 1;
                    }
                    Err(err) => {
                        rollback_created_components(store, &result.created_uids);
                        return Err(err);
                    }
                }
            }
        }
    }

    Ok(result)
}

fn rollback_created_components(store: &CalendarStoreService, created_uids: &[String]) {
    for uid in created_uids {
        let _ = store.remove_component(uid);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const SAMPLE_MULTI_ICS: &str = r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Example Corp.//EN
BEGIN:VEVENT
UID:event-1@example.com
SUMMARY:Projekt-Kickoff
DTSTART:20260910T090000Z
DTEND:20260910T100000Z
DESCRIPTION:Besprechung der Meilensteine
LOCATION:Raum 404
BEGIN:VALARM
TRIGGER:-PT15M
ACTION:DISPLAY
DESCRIPTION:Erinnerung
END:VALARM
END:VEVENT
BEGIN:VEVENT
UID:event-2@example.com
SUMMARY:Wöchentliches Sync-Meeting
DTSTART;TZID=Europe/Berlin:20260911T140000
DTEND;TZID=Europe/Berlin:20260911T150000
RRULE:FREQ=WEEKLY;BYDAY=FR
END:VEVENT
BEGIN:VTODO
UID:task-1@example.com
SUMMARY:Dokumentation schreiben
DUE:20260915T180000Z
STATUS:IN-PROCESS
END:VTODO
BEGIN:VJOURNAL
UID:memo-1@example.com
SUMMARY:Notizen zur Architektur
DESCRIPTION:Erste Gedanken zum Design
END:VJOURNAL
END:VCALENDAR
"#;

    #[test]
    fn parse_multi_component_stream_extracts_all_items_accurately() {
        let items = parse_ical_stream(SAMPLE_MULTI_ICS);
        assert_eq!(items.len(), 4);

        let event1 = &items[0];
        assert_eq!(event1.kind, IcalComponentKind::Event);
        assert_eq!(event1.uid, "event-1@example.com");
        assert_eq!(event1.summary, "Projekt-Kickoff");
        assert_eq!(event1.dtstart.as_deref(), Some("20260910T090000Z"));
        assert_eq!(event1.dtend_or_due.as_deref(), Some("20260910T100000Z"));
        assert_eq!(
            event1.description.as_deref(),
            Some("Besprechung der Meilensteine")
        );
        assert_eq!(event1.location.as_deref(), Some("Raum 404"));
        assert_eq!(event1.alarm_count, 1);
        assert!(!event1.is_recurring);

        let event2 = &items[1];
        assert_eq!(event2.kind, IcalComponentKind::Event);
        assert_eq!(event2.uid, "event-2@example.com");
        assert!(event2.is_recurring);

        let task = &items[2];
        assert_eq!(task.kind, IcalComponentKind::Task);
        assert_eq!(task.uid, "task-1@example.com");
        assert_eq!(task.summary, "Dokumentation schreiben");
        assert_eq!(task.dtend_or_due.as_deref(), Some("20260915T180000Z"));

        let memo = &items[3];
        assert_eq!(memo.kind, IcalComponentKind::Memo);
        assert_eq!(memo.uid, "memo-1@example.com");
        assert_eq!(memo.summary, "Notizen zur Architektur");
    }

    #[test]
    fn plan_ical_import_identifies_duplicates_and_summarizes() {
        let mut existing = HashSet::new();
        existing.insert("event-1@example.com".to_string());
        existing.insert("other-event@example.com".to_string());

        let plan = plan_ical_import(
            SAMPLE_MULTI_ICS,
            &existing,
            IcalDuplicatePolicy::KeepBothNewUid,
        );
        assert_eq!(plan.event_count, 2);
        assert_eq!(plan.task_count, 1);
        assert_eq!(plan.memo_count, 1);
        assert_eq!(plan.duplicate_uids, vec!["event-1@example.com"]);
        assert_eq!(plan.summary_label(), "2 Termine, 1 Aufgabe, 1 Notiz");
    }

    #[test]
    fn replace_component_uid_preserves_content_and_modifies_uid_line() {
        let original = "BEGIN:VEVENT\r\nUID:old-id\r\nSUMMARY:Test\r\nEND:VEVENT";
        let replaced = replace_component_uid(original, "new-id");
        assert!(replaced.contains("UID:new-id"));
        assert!(!replaced.contains("UID:old-id"));
        assert!(replaced.contains("SUMMARY:Test"));
    }

    #[test]
    fn unescape_ical_text_replaces_newlines_and_escapes() {
        let escaped = r"Line 1\nLine 2\, with comma\; and semicolon\\";
        let unescaped = unescape_ical_text(escaped);
        assert_eq!(unescaped, "Line 1\nLine 2, with comma; and semicolon\\");
    }

    #[test]
    fn duplicate_policies_behave_deterministically_with_store() {
        let unique_suffix = uuid::Uuid::new_v4();
        let temp_path = std::env::temp_dir().join(format!("evolution-ical-test-{unique_suffix}"));
        std::fs::create_dir_all(&temp_path).unwrap();
        let store = CalendarStoreService::with_writable_calendar_dir(&temp_path);

        // Create an initial component in the store
        let initial_ics =
            "BEGIN:VEVENT\r\nUID:event-1@example.com\r\nSUMMARY:Original\r\nEND:VEVENT\r\n";
        store.create_component(initial_ics).unwrap();

        // 1. SkipDuplicates policy
        let mut existing = HashSet::new();
        existing.insert("event-1@example.com".to_string());
        let plan_skip = plan_ical_import(
            SAMPLE_MULTI_ICS,
            &existing,
            IcalDuplicatePolicy::SkipExisting,
        );
        let res_skip = execute_ical_import(&plan_skip, &store).unwrap();
        assert_eq!(res_skip.skipped_count, 1);
        assert_eq!(res_skip.imported_count, 3); // 3 other items imported

        // 2. OverwriteExisting policy
        let plan_overwrite = plan_ical_import(
            SAMPLE_MULTI_ICS,
            &existing,
            IcalDuplicatePolicy::OverwriteExisting,
        );
        let res_overwrite = execute_ical_import(&plan_overwrite, &store).unwrap();
        assert_eq!(res_overwrite.overwritten_count, 4); // all 4 items present in store are overwritten

        let _ = std::fs::remove_dir_all(&temp_path);
    }
}
