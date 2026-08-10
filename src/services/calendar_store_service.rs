// SPDX-License-Identifier: GPL-3.0-or-later
//
// Small read-only iCalendar importer for local files.  ECal remains the
// authoritative synchronized backend; this importer gives the GTK models a
// real, deterministic source for standalone .ics/.vcs exports meanwhile.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEventRecord {
    pub uid: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub location: String,
    pub description: String,
    pub all_day: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub uid: String,
    pub summary: String,
    pub due: String,
    pub priority: u8,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoRecord {
    pub uid: String,
    pub summary: String,
    pub description: String,
    pub updated: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarSnapshot {
    pub events: Vec<CalendarEventRecord>,
    pub tasks: Vec<TaskRecord>,
    pub memos: Vec<MemoRecord>,
}

#[derive(Debug, Clone)]
pub struct CalendarStoreService {
    calendar_dir: PathBuf,
}

impl CalendarStoreService {
    pub fn new() -> Self {
        let calendar_dir = std::env::var_os("EVOLUTION_GTK4_ICAL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4/ical"));
        Self { calendar_dir }
    }

    #[allow(dead_code)]
    pub fn with_calendar_dir(calendar_dir: impl Into<PathBuf>) -> Self {
        Self {
            calendar_dir: calendar_dir.into(),
        }
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
            parse_ical(&raw, &path, &mut snapshot);
        }
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
        snapshot
    }
}

impl Default for CalendarStoreService {
    fn default() -> Self {
        Self::new()
    }
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

fn parse_ical(raw: &str, path: &Path, snapshot: &mut CalendarSnapshot) {
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
        if line.starts_with("BEGIN:") || line.eq_ignore_ascii_case("END:VCALENDAR") {
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT")
            || line.eq_ignore_ascii_case("END:VTODO")
            || line.eq_ignore_ascii_case("END:VJOURNAL")
        {
            if let Some(component) = current.take() {
                component_index += 1;
                append_component(component, path, component_index, snapshot);
            }
            continue;
        }
        if let Some(component) = current.as_mut() {
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
}

impl CurrentComponent {
    fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            ..Self::default()
        }
    }
}

fn append_component(
    component: CurrentComponent,
    path: &Path,
    index: usize,
    snapshot: &mut CalendarSnapshot,
) {
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

    match component.kind.as_str() {
        "VEVENT" => {
            let start = component
                .properties
                .get("DTSTART")
                .map(|value| format_ical_datetime(value))
                .unwrap_or_default();
            let end = component
                .properties
                .get("DTEND")
                .map(|value| format_ical_datetime(value))
                .unwrap_or_default();
            snapshot.events.push(CalendarEventRecord {
                uid,
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
                all_day: component.date_properties.contains("DTSTART"),
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
                summary,
                due: component
                    .properties
                    .get("DUE")
                    .map(|value| format_ical_datetime(value))
                    .unwrap_or_default(),
                priority,
                completed,
            });
        }
        "VJOURNAL" => {
            snapshot.memos.push(MemoRecord {
                uid,
                summary,
                description: component
                    .properties
                    .get("DESCRIPTION")
                    .cloned()
                    .unwrap_or_default(),
                updated: component
                    .properties
                    .get("LAST-MODIFIED")
                    .or_else(|| component.properties.get("DTSTART"))
                    .map(|value| format_ical_datetime(value))
                    .unwrap_or_default(),
            });
        }
        _ => {}
    }
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.trim_start());
            }
        } else {
            lines.push(raw_line.to_string());
        }
    }
    lines
}

fn unescape(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(snapshot.events[0].start, "2026-08-10 12:00");
        assert_eq!(snapshot.tasks[0].due, "2026-08-11");
        assert_eq!(snapshot.tasks[0].priority, 1);
        assert_eq!(snapshot.memos[0].description, "Line one\nLine two");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_calendar_directory_is_empty_and_does_not_create_data() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-missing-ical-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            CalendarStoreService::with_calendar_dir(&root).load_snapshot(),
            CalendarSnapshot::default()
        );
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
