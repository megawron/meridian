// SPDX-License-Identifier: GPL-3.0-or-later
//
// Local iCalendar store for standalone .ics/.vcs sources.  ECal remains the
// authoritative synchronized backend; explicitly selected external folders
// stay read-only, while the app-owned fallback can persist local components.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoRecord {
    pub uid: String,
    pub source_uid: String,
    pub source_display_name: String,
    pub component: String,
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
    writable: bool,
}

impl CalendarStoreService {
    pub fn new() -> Self {
        let (calendar_dir, writable) =
            if let Some(path) = std::env::var_os("EVOLUTION_GTK4_ICAL_DIR").map(PathBuf::from) {
                (path, false)
            } else if let Some(path) = crate::services::settings_service::AppSettings::try_new()
                .map(|settings| settings.local_calendar_directory())
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
            {
                (path, false)
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

    pub fn calendar_dir(&self) -> &Path {
        &self.calendar_dir
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Copy user-selected iCalendar files into the configured local source.
    /// Existing files are never replaced, so repeating an import is safe.
    pub fn import_files(&self, files: &[PathBuf]) -> io::Result<usize> {
        fs::create_dir_all(&self.calendar_dir)?;
        let mut imported = 0;
        for source in files {
            if !source.is_file()
                || !source
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(extension.to_ascii_lowercase().as_str(), "ics" | "vcs")
                    })
            {
                continue;
            }
            let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let destination = unique_destination(&self.calendar_dir, file_name);
            fs::copy(source, destination)?;
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
        let kind = component_kind(component).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültige iCalendar-Komponente",
            )
        })?;
        let (uid, component) = ensure_component_uid(component, kind, None);
        let path = self.calendar_file_path();
        let mut calendar = read_calendar_file(&path)?;
        insert_calendar_component(&mut calendar, &component)?;
        write_atomic(&path, calendar.as_bytes())?;
        Ok((uid, component))
    }

    pub fn modify_component(&self, uid: &str, component: &str) -> io::Result<(String, String)> {
        self.ensure_writable()?;
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
            let Ok(calendar) = fs::read_to_string(&path) else {
                continue;
            };
            if !calendar_contains_component(&calendar, &path, uid) {
                continue;
            }
            let updated = replace_calendar_component(&calendar, &path, uid, &component)?;
            write_atomic(&path, updated.as_bytes())?;
            return Ok((component_uid, component));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("iCalendar-Komponente nicht gefunden: {uid}"),
        ))
    }

    pub fn remove_component(&self, uid: &str) -> io::Result<String> {
        self.ensure_writable()?;
        for path in self.mutable_calendar_files() {
            let Ok(calendar) = fs::read_to_string(&path) else {
                continue;
            };
            if !calendar_contains_component(&calendar, &path, uid) {
                continue;
            }
            let updated = remove_calendar_component(&calendar, &path, uid)?;
            write_atomic(&path, updated.as_bytes())?;
            return Ok(uid.to_string());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("iCalendar-Komponente nicht gefunden: {uid}"),
        ))
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
        self.calendar_dir.join("evolution-gtk4-local.ics")
    }

    fn mutable_calendar_files(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        collect_calendar_files(&self.calendar_dir, &mut paths);
        paths.sort();
        paths
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

fn ensure_component_uid(raw: &str, kind: &str, fallback_uid: Option<&str>) -> (String, String) {
    let mut lines = unfold_lines(raw);
    let uid = component_uid_from_lines(&lines)
        .or_else(|| fallback_uid.map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("evolution-gtk4-{}", uuid::Uuid::new_v4()));
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
            "PRODID:-//Evolution GTK4//EN\r\n",
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

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("ics.tmp");
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
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
                source_uid: source_uid.to_string(),
                source_display_name: source_display_name.to_string(),
                component: raw_component,
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
                source_uid: source_uid.to_string(),
                source_display_name: source_display_name.to_string(),
                component: raw_component,
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
                source_uid: source_uid.to_string(),
                source_display_name: source_display_name.to_string(),
                component: raw_component,
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

/// Replace or remove a top-level iCalendar property while preserving the rest
/// of the component, including nested alarms and vendor-specific fields.
pub(crate) fn update_ical_property(raw: &str, property_name: &str, value: Option<&str>) -> String {
    let target = property_name
        .split(';')
        .next()
        .unwrap_or(property_name)
        .to_ascii_uppercase();
    let mut lines = Vec::new();
    let mut replaced = false;
    let mut component_depth = 0usize;

    for line in unfold_lines(raw) {
        let property = line
            .split_once(':')
            .map(|(property, _)| property)
            .unwrap_or(line.as_str())
            .split(';')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if property == target && component_depth <= 1 {
            if !replaced {
                if let Some(value) = value {
                    lines.push(format!("{property_name}:{value}"));
                }
                replaced = true;
            }
            continue;
        }
        lines.push(line);

        if property == "BEGIN" {
            component_depth = component_depth.saturating_add(1);
        } else if property == "END" {
            component_depth = component_depth.saturating_sub(1);
        }
    }

    if value.is_some() && !replaced {
        let insertion = lines
            .iter()
            .position(|line| line.eq_ignore_ascii_case("END:VEVENT"))
            .or_else(|| {
                lines
                    .iter()
                    .position(|line| line.eq_ignore_ascii_case("END:VTODO"))
            })
            .or_else(|| {
                lines
                    .iter()
                    .position(|line| line.eq_ignore_ascii_case("END:VJOURNAL"))
            })
            .unwrap_or(lines.len());
        lines.insert(
            insertion,
            format!("{property_name}:{}", value.unwrap_or_default()),
        );
    }

    let mut result = lines.join("\r\n");
    result.push_str("\r\n");
    result
}

/// Return a standards-shaped iCalendar document for a single component.
///
/// ECal and the local parser work with bare VEVENT/VTODO/VJOURNAL components,
/// but exported files should contain a VCALENDAR envelope so they can be
/// opened by Evolution and other calendar applications without heuristics.
pub(crate) fn export_ical_component(raw: &str) -> String {
    let lines = unfold_lines(raw);
    if lines
        .first()
        .is_some_and(|line| line.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        let mut result = lines.join("\r\n");
        result.push_str("\r\n");
        return result;
    }

    let mut result =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Evolution GTK4//EN\r\n");
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
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
