// SPDX-License-Identifier: GPL-3.0-or-later
//
// TasksPage — Dedicated Task and Todo (VTODO) manager view.
//
// Features:
// - Category list sidebar ("Alle Aufgaben", "Heute fällig", "Wichtig", "Erledigt")
// - Dynamic Task list with checkable boxes, priority chips, and due dates
// - "+ Neue Aufgabe" task creation dialog connection

use chrono::{Duration, Local, NaiveDate, NaiveDateTime, Utc};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::dialogs::event_dialog::AlarmEditor;
use crate::models::task_item::TaskItemObject;
use crate::services::calendar_category_service::{
    CalendarCategory, CalendarCategoryStore, category_names_from_ical_component,
    category_names_from_ical_value,
};
use crate::services::calendar_store_service::{
    CalendarStoreService, TaskRecord, export_ical_component, serialize_ical_attachment_value,
    update_ical_property,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, CalendarLoadResult, ECalChangeKind, ECalComponentType,
    ECalLocalSourceCreateStatus, ECalLocalSourceProperties, ECalLocalSourcePropertiesUpdate,
    ECalRecord, ECalRefreshOutcome, ECalService, ECalServiceError, ECalSourceCapability,
    ECalSourceCopyStatus, ECalSyncEvent, load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::ical_time_service::{
    property_local_date, property_local_dates, property_local_datetime, property_local_display,
    property_local_timestamp,
};
use crate::services::recurrence_service::{
    next_recurrence_date, parse_ical_date, parse_recurrence_rule,
};
use crate::services::reminder_service::{Alarm, parse_alarms, replace_component_alarms};
use crate::services::settings_service::{AppSettings, normalized_task_category_filter};
use crate::views::accessibility::set_accessible_label;
use adw::prelude::*;
use adw::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct TasksPage {
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub(super) task_filter: Cell<TaskFilter>,
        pub(super) task_search_mode: Cell<TaskSearchMode>,
        pub(super) task_source_filter: RefCell<Option<String>>,
        pub(super) task_sort: Cell<TaskSort>,
        pub(super) task_sort_descending: Cell<bool>,
        pub task_query: RefCell<String>,
        pub(super) task_category_filter: RefCell<Option<String>>,
        pub(super) task_category_metadata: RefCell<Option<Vec<CalendarCategory>>>,
        pub(super) task_category_metadata_generation: Cell<u64>,
        pub(super) task_category_model: RefCell<Option<gtk::StringList>>,
        pub(super) task_category_selector: RefCell<Option<gtk::DropDown>>,
        pub task_list: RefCell<Option<gtk::ListBox>>,
        pub task_store: RefCell<Option<gio::ListStore>>,
        pub task_empty_state: RefCell<Option<gtk::Label>>,
        pub(super) task_selection: RefCell<Vec<TaskSelectionIdentity>>,
        pub(super) task_selection_restoring: Cell<bool>,
        pub(super) task_selection_status: RefCell<Option<gtk::Label>>,
        pub(super) task_forward_button: RefCell<Option<gtk::Button>>,
        pub(super) task_assign_button: RefCell<Option<gtk::Button>>,
        pub(super) task_open_url_button: RefCell<Option<gtk::Button>>,
        pub(super) task_print_button: RefCell<Option<gtk::Button>>,
        pub(super) task_copy_button: RefCell<Option<gtk::Button>>,
        pub(super) task_cut_button: RefCell<Option<gtk::Button>>,
        pub(super) task_paste_button: RefCell<Option<gtk::Button>>,
        pub(super) task_select_all_button: RefCell<Option<gtk::Button>>,
        pub(super) task_bulk_edit_button: RefCell<Option<gtk::Button>>,
        pub(super) task_preview_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub(super) task_preview_orientation: RefCell<Option<gtk::DropDown>>,
        pub(super) task_preview_paned: RefCell<Option<gtk::Paned>>,
        pub(super) task_preview_box: RefCell<Option<gtk::Box>>,
        pub(super) task_preview_content: RefCell<Option<gtk::TextView>>,
        pub(super) task_preview_find_bar: RefCell<Option<gtk::Box>>,
        pub(super) task_preview_find_entry: RefCell<Option<gtk::SearchEntry>>,
        pub(super) task_preview_find_status: RefCell<Option<gtk::Label>>,
        pub(super) task_preview_identity: RefCell<Option<TaskSelectionIdentity>>,
        pub(super) task_preview_text: RefCell<String>,
        pub(super) task_preview_find: RefCell<TaskPreviewFindState>,
        pub(super) task_preview_position_restoring: Cell<bool>,
        pub(super) task_export_button: RefCell<Option<gtk::Button>>,
        pub(super) task_complete_button: RefCell<Option<gtk::Button>>,
        pub(super) task_incomplete_button: RefCell<Option<gtk::Button>>,
        pub(super) task_delete_selection_button: RefCell<Option<gtk::Button>>,
        pub(super) task_purge_button: RefCell<Option<gtk::Button>>,
        pub(super) task_refresh_button: RefCell<Option<gtk::Button>>,
        pub(super) task_refresh_running: Cell<bool>,
        pub(super) task_refresh_generation: Cell<u64>,
        pub(super) task_refresh_unsupported_sources: RefCell<HashSet<String>>,
        pub(super) task_source_capabilities: RefCell<HashMap<String, TaskSourceCapability>>,
        pub(super) task_source_metadata: RefCell<HashMap<String, ECalSourceCapability>>,
        pub(super) task_source_model: RefCell<Option<gtk::StringList>>,
        pub(super) task_source_selector: RefCell<Option<gtk::DropDown>>,
        pub(super) task_source_list: RefCell<Option<gtk::ListBox>>,
        pub(super) task_source_discovery_generation: Cell<u64>,
        pub(super) task_source_management_running: Cell<bool>,
        pub(super) task_source_management_generation: Cell<u64>,
        pub(super) task_source_new_button: RefCell<Option<gtk::Button>>,
        pub(super) task_source_copy_button: RefCell<Option<gtk::Button>>,
        pub(super) task_source_rename_button: RefCell<Option<gtk::Button>>,
        pub(super) task_source_properties_button: RefCell<Option<gtk::Button>>,
        pub(super) task_source_delete_button: RefCell<Option<gtk::Button>>,
        pub(super) task_assignment_running: Cell<bool>,
        pub(super) task_assignment_generation: Cell<u64>,
        pub(super) task_lifecycle_batch_running: Cell<bool>,
        pub(super) task_lifecycle_batch_generation: Cell<u64>,
        pub(super) task_delete_batch_running: Cell<bool>,
        pub(super) task_delete_batch_generation: Cell<u64>,
        pub(super) task_paste_batch_running: Cell<bool>,
        pub(super) task_paste_batch_generation: Cell<u64>,
        pub(super) task_bulk_edit_running: Cell<bool>,
        pub(super) task_bulk_edit_generation: Cell<u64>,
        pub(super) task_source_uids: RefCell<Vec<String>>,
        pub new_task_button: RefCell<Option<gtk::Button>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TasksPage {
        const NAME: &'static str = "EvolutionTasksPage";
        type Type = super::TasksPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for TasksPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(cancel) = self.ecal_monitor_cancel.borrow_mut().take() {
                use std::sync::atomic::Ordering;
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TasksPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
}

glib::wrapper! {
    pub struct TasksPage(ObjectSubclass<imp::TasksPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TaskFilter {
    #[default]
    All,
    Today,
    Next7,
    Next30,
    Next365,
    Important,
    Overdue,
    Scheduled,
    NoDue,
    WithAttachments,
    Started,
    WithoutCategory,
    Uncompleted,
    InProgress,
    Cancelled,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TaskSearchMode {
    #[default]
    AnyField,
    Summary,
    Description,
    Advanced,
}

const TASK_FILTER_OPTIONS: [(TaskFilter, &str, &str); 16] = [
    (TaskFilter::All, "view-list-symbolic", "Alle Aufgaben"),
    (TaskFilter::Today, "alarm-symbolic", "Heute fällig"),
    (
        TaskFilter::Next7,
        "appointment-soon-symbolic",
        "Nächste 7 Tage",
    ),
    (
        TaskFilter::Next30,
        "appointment-soon-symbolic",
        "Nächste 30 Tage",
    ),
    (
        TaskFilter::Next365,
        "appointment-soon-symbolic",
        "Aktiv · nächste 365 Tage",
    ),
    (
        TaskFilter::Overdue,
        "appointment-missed-symbolic",
        "Überfällig",
    ),
    (
        TaskFilter::Scheduled,
        "x-office-calendar-symbolic",
        "Mit Fälligkeit",
    ),
    (TaskFilter::NoDue, "view-list-symbolic", "Ohne Fälligkeit"),
    (
        TaskFilter::WithAttachments,
        "mail-attachment-symbolic",
        "Mit Anhängen",
    ),
    (
        TaskFilter::Started,
        "media-playback-start-symbolic",
        "Begonnen",
    ),
    (
        TaskFilter::WithoutCategory,
        "tag-symbolic",
        "Ohne Kategorie",
    ),
    (
        TaskFilter::Uncompleted,
        "view-list-symbolic",
        "Offene Aufgaben",
    ),
    (
        TaskFilter::Important,
        "emblem-important-symbolic",
        "Wichtig",
    ),
    (
        TaskFilter::InProgress,
        "media-playback-start-symbolic",
        "In Bearbeitung",
    ),
    (
        TaskFilter::Cancelled,
        "window-close-symbolic",
        "Abgebrochen",
    ),
    (TaskFilter::Completed, "emblem-ok-symbolic", "Erledigt"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TaskSort {
    #[default]
    DueDate,
    Title,
    Priority,
}

impl TaskFilter {
    fn from_setting(value: &str) -> Self {
        match value {
            "today" => Self::Today,
            "next-7" => Self::Next7,
            "next-30" => Self::Next30,
            "next-365" | "active" => Self::Next365,
            "important" => Self::Important,
            "overdue" => Self::Overdue,
            "scheduled" => Self::Scheduled,
            "no-due" => Self::NoDue,
            "with-attachments" => Self::WithAttachments,
            "started" => Self::Started,
            "without-category" => Self::WithoutCategory,
            "uncompleted" => Self::Uncompleted,
            "in-progress" => Self::InProgress,
            "cancelled" => Self::Cancelled,
            "completed" => Self::Completed,
            _ => Self::All,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Today => "today",
            Self::Next7 => "next-7",
            Self::Next30 => "next-30",
            Self::Next365 => "next-365",
            Self::Important => "important",
            Self::Overdue => "overdue",
            Self::Scheduled => "scheduled",
            Self::NoDue => "no-due",
            Self::WithAttachments => "with-attachments",
            Self::Started => "started",
            Self::WithoutCategory => "without-category",
            Self::Uncompleted => "uncompleted",
            Self::InProgress => "in-progress",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
        }
    }
}

fn task_filter_index(filter: TaskFilter) -> i32 {
    TASK_FILTER_OPTIONS
        .iter()
        .position(|(candidate, _, _)| *candidate == filter)
        .unwrap_or(0) as i32
}

fn task_filter_from_index(index: i32) -> TaskFilter {
    index
        .try_into()
        .ok()
        .and_then(|index: usize| TASK_FILTER_OPTIONS.get(index))
        .map(|(filter, _, _)| *filter)
        .unwrap_or(TaskFilter::All)
}

impl TaskSearchMode {
    fn from_setting(value: &str) -> Self {
        match value {
            "summary" => Self::Summary,
            "description" => Self::Description,
            "advanced" => Self::Advanced,
            _ => Self::AnyField,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::AnyField => "any-field",
            Self::Summary => "summary",
            Self::Description => "description",
            Self::Advanced => "advanced",
        }
    }

    fn index(self) -> u32 {
        match self {
            Self::AnyField => 0,
            Self::Summary => 1,
            Self::Description => 2,
            Self::Advanced => 3,
        }
    }

    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Summary,
            2 => Self::Description,
            3 => Self::Advanced,
            _ => Self::AnyField,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskFilterContext {
    now: NaiveDateTime,
    now_timestamp: i64,
    today: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TaskSelectionIdentity {
    source_uid: String,
    task_uid: String,
}

impl TaskSelectionIdentity {
    fn from_task(task: &TaskItemObject) -> Self {
        Self {
            source_uid: task.source_uid(),
            task_uid: task.id(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskSelectionActionState {
    export_enabled: bool,
    single_task_actions_enabled: bool,
    forward_enabled: bool,
    open_url_enabled: bool,
    print_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskPreviewOrientation {
    Classic,
    Vertical,
}

impl TaskPreviewOrientation {
    fn from_setting(value: &str) -> Self {
        match value {
            "vertical" => Self::Vertical,
            _ => Self::Classic,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Vertical => "vertical",
        }
    }

    fn paned_orientation(self) -> gtk::Orientation {
        match self {
            // Evolution's classic task view places preview below the list.
            Self::Classic => gtk::Orientation::Vertical,
            // Its vertical task view places preview beside the list.
            Self::Vertical => gtk::Orientation::Horizontal,
        }
    }

    fn index(self) -> u32 {
        match self {
            Self::Classic => 0,
            Self::Vertical => 1,
        }
    }

    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Vertical,
            _ => Self::Classic,
        }
    }

    fn default_position(self) -> i32 {
        match self {
            Self::Classic => 400,
            Self::Vertical => 200,
        }
    }
}

fn apply_task_preview_paned_layout(page: &TasksPage, orientation: TaskPreviewOrientation) {
    let Some(paned) = page.imp().task_preview_paned.borrow().as_ref().cloned() else {
        return;
    };
    let position = AppSettings::try_new()
        .map(|settings| settings.task_preview_position(orientation.setting_value()))
        .unwrap_or_else(|| orientation.default_position());
    page.imp().task_preview_position_restoring.set(true);
    paned.set_orientation(orientation.paned_orientation());
    paned.set_position(position);
    page.imp().task_preview_position_restoring.set(false);
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TaskPreviewFindState {
    query: String,
    matches: Vec<(usize, usize)>,
    current: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPreviewDocument {
    identity: TaskSelectionIdentity,
    text: String,
}

fn task_selection_action_state(
    selection_count: usize,
    has_exact_forward_payload: bool,
    has_valid_url: bool,
    has_print_document: bool,
) -> TaskSelectionActionState {
    TaskSelectionActionState {
        export_enabled: selection_count > 0,
        single_task_actions_enabled: selection_count == 1,
        forward_enabled: selection_count == 1 && has_exact_forward_payload,
        open_url_enabled: selection_count == 1 && has_valid_url,
        print_enabled: selection_count == 1 && has_print_document,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskExportCandidate {
    identity: TaskSelectionIdentity,
    component: String,
}

fn task_export_candidates(task_store: &gio::ListStore) -> Vec<TaskExportCandidate> {
    (0..task_store.n_items())
        .filter_map(|index| task_store.item(index).and_downcast::<TaskItemObject>())
        .map(|task| TaskExportCandidate {
            identity: TaskSelectionIdentity::from_task(&task),
            component: task.component(),
        })
        .collect()
}

fn selected_task_candidate<'a>(
    selection: &[TaskSelectionIdentity],
    candidates: &'a [TaskExportCandidate],
) -> Result<&'a TaskExportCandidate, String> {
    let [identity] = selection else {
        return Err("Diese Aktion ist nur für genau eine Aufgabe verfügbar".to_string());
    };
    let matches = candidates
        .iter()
        .filter(|candidate| candidate.identity == *identity)
        .collect::<Vec<_>>();
    let [candidate] = matches.as_slice() else {
        return Err("Die ausgewählte Aufgabe ist nicht mehr verfügbar".to_string());
    };
    Ok(candidate)
}

fn validated_task_vtodo_component(
    candidate: &TaskExportCandidate,
    expected_identity: &TaskSelectionIdentity,
) -> Result<String, String> {
    let lines = candidate
        .component
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>();
    let uids = ical_properties(&candidate.component, "UID");
    if lines
        .first()
        .is_none_or(|line| !line.eq_ignore_ascii_case("BEGIN:VTODO"))
        || lines
            .last()
            .is_none_or(|line| !line.eq_ignore_ascii_case("END:VTODO"))
        || lines
            .iter()
            .filter(|line| line.eq_ignore_ascii_case("BEGIN:VTODO"))
            .count()
            != 1
        || lines
            .iter()
            .filter(|line| line.eq_ignore_ascii_case("END:VTODO"))
            .count()
            != 1
        || uids.len() != 1
        || uids.first().map(String::as_str) != Some(expected_identity.task_uid.as_str())
    {
        return Err("Eine ausgewählte Aufgabe enthält keine gültige VTODO-Komponente".to_string());
    }
    Ok(lines.join("\r\n"))
}

fn task_preview_document(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
) -> Result<TaskPreviewDocument, String> {
    let candidate = selected_task_candidate(selection, candidates)?;
    let component = validated_task_vtodo_component(candidate, &candidate.identity)?;
    let mut fields = Vec::<(String, String)>::new();
    let first = |name| ical_property(&component, name).filter(|value| !value.trim().is_empty());
    let all = |name| {
        ical_properties(&component, name)
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
    };
    let push = |fields: &mut Vec<(String, String)>, label: &str, value: Option<String>| {
        if let Some(value) = value {
            fields.push((label.to_string(), value));
        }
    };

    push(
        &mut fields,
        "Zusammenfassung",
        Some(first("SUMMARY").unwrap_or_else(|| "Unbenannt".to_string())),
    );
    push(&mut fields, "Ort", first("LOCATION"));
    push(&mut fields, "GEO", first("GEO"));
    push(&mut fields, "Kommentar", first("COMMENT"));
    push(&mut fields, "Beginn", first("DTSTART"));
    push(&mut fields, "Wiederholung", first("RRULE"));
    push(&mut fields, "Fällig", first("DUE"));
    push(&mut fields, "Geschätzte Dauer", first("ESTIMATED-DURATION"));
    push(&mut fields, "Status", first("STATUS"));
    push(&mut fields, "Kategorien", first("CATEGORIES"));
    push(&mut fields, "Priorität", first("PRIORITY"));
    push(&mut fields, "Sichtbarkeit", first("CLASS"));
    push(&mut fields, "Organisator", first("ORGANIZER"));
    let attendees = all("ATTENDEE");
    if !attendees.is_empty() {
        fields.push(("Teilnehmende".to_string(), attendees.join("\n")));
    }
    push(&mut fields, "URL", first("URL"));
    push(&mut fields, "Beschreibung", first("DESCRIPTION"));
    push(&mut fields, "Alternativbeschreibung", first("X-ALT-DESC"));
    let attachments = task_top_level_attachment_lines(&component)
        .into_iter()
        .map(|attachment| match attachment.kind {
            TaskAttachmentValueKind::Uri => format!("URI (nicht geöffnet): {}", attachment.value),
            TaskAttachmentValueKind::Binary => "Binäranhang".to_string(),
            TaskAttachmentValueKind::Other => format!("Anhang: {}", attachment.value),
        })
        .collect::<Vec<_>>();
    if !attachments.is_empty() {
        fields.push(("Anhänge".to_string(), attachments.join("\n")));
    }

    if fields.is_empty() {
        return Err("Die ausgewählte Aufgabe enthält keine darstellbaren Felder".to_string());
    }
    let text = fields
        .into_iter()
        .map(|(label, value)| format!("{label}\n{value}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(TaskPreviewDocument {
        identity: candidate.identity.clone(),
        text,
    })
}

fn task_preview_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    // Lowercasing can expand one Unicode scalar (for example `İ` becomes
    // `i` plus a combining dot). Keep an explicit map back to the original
    // GtkTextBuffer character offsets instead of applying folded offsets to
    // the original buffer.
    let mut folded_text = String::new();
    let mut folded_to_original = Vec::new();
    for (original_offset, character) in text.chars().enumerate() {
        for folded_character in character.to_lowercase() {
            folded_text.push(folded_character);
            folded_to_original.push((original_offset, original_offset + 1));
        }
    }
    let folded_query = query.to_lowercase();
    if folded_query.is_empty() {
        return Vec::new();
    }
    folded_text
        .match_indices(&folded_query)
        .filter_map(|(start, matched)| {
            let folded_start = folded_text[..start].chars().count();
            let folded_end = folded_start + matched.chars().count();
            let original_start = folded_to_original.get(folded_start)?.0;
            let original_end = folded_to_original.get(folded_end.checked_sub(1)?)?.1;
            Some((original_start, original_end))
        })
        .collect()
}

fn task_preview_find_state(
    text: &str,
    query: &str,
    current: Option<usize>,
) -> TaskPreviewFindState {
    let matches = task_preview_match_ranges(text, query);
    let current = (!matches.is_empty()).then(|| current.unwrap_or(0) % matches.len());
    TaskPreviewFindState {
        query: query.trim().to_string(),
        matches,
        current,
    }
}

fn task_preview_find_step(state: &TaskPreviewFindState, forward: bool) -> TaskPreviewFindState {
    let mut result = state.clone();
    let Some(current) = state.current else {
        return result;
    };
    result.current = Some(if forward {
        (current + 1) % state.matches.len()
    } else {
        (current + state.matches.len() - 1) % state.matches.len()
    });
    result
}

fn strip_task_valarms(component: &str) -> Result<String, String> {
    let mut result = Vec::new();
    let mut alarm_depth = 0usize;
    for line in component.lines() {
        if line.eq_ignore_ascii_case("BEGIN:VALARM") {
            alarm_depth += 1;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VALARM") {
            if alarm_depth == 0 {
                return Err("Die ausgewählte Aufgabe enthält einen ungültigen Alarm".to_string());
            }
            alarm_depth -= 1;
            continue;
        }
        if alarm_depth == 0 {
            result.push(line);
        }
    }
    if alarm_depth != 0 {
        return Err("Die ausgewählte Aufgabe enthält einen unvollständigen Alarm".to_string());
    }
    Ok(result.join("\r\n"))
}

fn build_task_forward_payload(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
) -> Result<String, String> {
    let candidate = selected_task_candidate(selection, candidates)?;
    let identity = selection
        .first()
        .expect("selected_task_candidate validated one identity");
    let component = strip_task_valarms(&validated_task_vtodo_component(candidate, identity)?)?;
    Ok(format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Evolution GTK4//EN\r\nMETHOD:PUBLISH\r\n{component}\r\nEND:VCALENDAR\r\n"
    ))
}

fn validate_task_open_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    let Some((scheme, _)) = value.split_once(':') else {
        return Err("Die Aufgaben-URL benötigt http oder https".to_string());
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err("Nur http- und https-Aufgaben-URLs werden unterstützt".to_string());
    }
    // Reuse the hardened URI authority/port policy already shared by the memo
    // surface, while narrowing this task action to upstream's web-page intent.
    crate::services::memo_surface_service::validate_open_url(value)
        .map_err(|_| "Die Aufgaben-URL ist ungültig".to_string())
}

fn selected_task_open_url(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
) -> Result<String, String> {
    let candidate = selected_task_candidate(selection, candidates)?;
    let identity = selection
        .first()
        .expect("selected_task_candidate validated one identity");
    let component = validated_task_vtodo_component(candidate, identity)?;
    let url = ical_property(&component, "URL")
        .ok_or_else(|| "Die ausgewählte Aufgabe hat keine URL".to_string())?;
    validate_task_open_url(&url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPrintDocument {
    html: String,
    plain_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskListPrintDocument {
    html: String,
    plain_text: String,
    row_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskListPrintAction {
    Print,
    Preview,
}

impl TaskListPrintAction {
    fn preparing_label(self) -> &'static str {
        match self {
            Self::Print => "Druckliste wird vorbereitet …",
            Self::Preview => "Druckvorschau wird vorbereitet …",
        }
    }

    fn preparation_label(self) -> &'static str {
        match self {
            Self::Print => "Druckdokument vorbereitet",
            Self::Preview => "Druckvorschau vorbereitet",
        }
    }
}

fn task_print_field(label: &str, value: &str) -> (String, String) {
    (
        format!(
            "<dt>{}</dt><dd>{}</dd>",
            glib::markup_escape_text(label),
            glib::markup_escape_text(value)
        ),
        format!("{label}: {value}"),
    )
}

fn build_task_print_document(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
) -> Result<TaskPrintDocument, String> {
    let candidate = selected_task_candidate(selection, candidates)?;
    let identity = selection
        .first()
        .expect("selected_task_candidate validated one identity");
    let component = validated_task_vtodo_component(candidate, identity)?;
    let title = ical_property(&component, "SUMMARY").unwrap_or_default();
    let status = task_status_label(&component);
    let due = property_local_display(&component, "DUE", false).unwrap_or_default();
    let priority = ical_property(&component, "PRIORITY").unwrap_or_default();
    let categories = ical_property(&component, "CATEGORIES").unwrap_or_default();
    let description = ical_property(&component, "DESCRIPTION").unwrap_or_default();

    let mut html_fields = Vec::new();
    let mut text_fields = Vec::new();
    for (label, value) in [
        ("Status", status.to_string()),
        ("Fällig", due),
        ("Priorität", priority),
        ("Kategorien", categories),
    ] {
        if !value.trim().is_empty() {
            let (html, text) = task_print_field(label, &value);
            html_fields.push(html);
            text_fields.push(text);
        }
    }
    if !description.trim().is_empty() {
        html_fields.push(format!(
            "<h2>Beschreibung</h2><pre>{}</pre>",
            glib::markup_escape_text(&description)
        ));
        text_fields.push(format!("Beschreibung:\n{description}"));
    }

    Ok(TaskPrintDocument {
        html: format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><dl>{}</dl></body></html>",
            glib::markup_escape_text(&title),
            glib::markup_escape_text(&title),
            html_fields.join("")
        ),
        plain_text: std::iter::once(title)
            .chain(text_fields)
            .collect::<Vec<_>>()
            .join("\n\n"),
    })
}

fn build_task_list_print_document(
    visible: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
) -> Result<TaskListPrintDocument, String> {
    if visible.is_empty() {
        return Err("Es sind keine sichtbaren Aufgaben zum Drucken vorhanden".to_string());
    }
    let mut seen = HashSet::new();
    let mut html_rows = Vec::with_capacity(visible.len());
    let mut text_rows = Vec::with_capacity(visible.len());
    for identity in visible {
        if !seen.insert(identity.clone()) {
            return Err("Die sichtbare Aufgabenliste enthält eine doppelte Identität".to_string());
        }
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err("Eine sichtbare Aufgabe ist nicht mehr eindeutig verfügbar".to_string());
        };
        let component = validated_task_vtodo_component(candidate, identity)?;
        let completion = if task_component_is_completed_for_purge(&component) {
            "✓"
        } else {
            ""
        };
        let status = task_status_label(&component);
        let summary =
            ical_property(&component, "SUMMARY").unwrap_or_else(|| "Unbenannt".to_string());
        let due = property_local_display(&component, "DUE", false).unwrap_or_default();
        let priority = ical_property(&component, "PRIORITY").unwrap_or_default();
        let categories = category_names_from_ical_component(&component).join(", ");
        let cells = [
            completion.to_string(),
            status.to_string(),
            summary.clone(),
            due.clone(),
            priority.clone(),
            identity.source_uid.clone(),
            categories.clone(),
        ];
        html_rows.push(format!(
            "<tr>{}</tr>",
            cells
                .iter()
                .map(|value| format!("<td>{}</td>", glib::markup_escape_text(value)))
                .collect::<Vec<_>>()
                .join("")
        ));
        text_rows.push(format!(
            "{} | {} | {} | {} | {} | {} | {}",
            completion, status, summary, due, priority, identity.source_uid, categories
        ));
    }
    let headings = [
        "Erledigt",
        "Status",
        "Zusammenfassung",
        "Fällig",
        "Priorität",
        "Quelle",
        "Kategorien",
    ];
    Ok(TaskListPrintDocument {
        html: format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Print Tasks</title></head><body><h1>Aufgaben</h1><table><thead><tr>{}</tr></thead><tbody>{}</tbody></table></body></html>",
            headings
                .iter()
                .map(|heading| format!("<th>{}</th>", glib::markup_escape_text(heading)))
                .collect::<Vec<_>>()
                .join(""),
            html_rows.join("")
        ),
        plain_text: std::iter::once(headings.join(" | "))
            .chain(text_rows)
            .collect::<Vec<_>>()
            .join("\n"),
        row_count: visible.len(),
    })
}

fn prepare_task_list_print(
    page: &TasksPage,
    button: &gtk::Button,
    task_list: &gtk::ListBox,
    task_store: &gio::ListStore,
    action: TaskListPrintAction,
) {
    show_toast_for_widget(button, action.preparing_label());
    // Snapshot only the rows currently displayed by the list.  This is
    // deliberately independent of selection and does not consult a backend.
    let visible = match task_visible_identities(task_list) {
        Ok(visible) => visible,
        Err(error) => {
            show_toast_for_widget(
                button,
                &format!("Druckliste konnte nicht vorbereitet werden: {error}"),
            );
            return;
        }
    };
    let document = build_task_list_print_document(&visible, &task_export_candidates(task_store));
    match document {
        Ok(document) => page.show_toast(&format!(
            "{} ({} Aufgaben, {} Zeichen); GTK-Druckdialog/Portal ist Linux-Handoff",
            action.preparation_label(),
            document.row_count,
            document.plain_text.chars().count()
        )),
        Err(error) => show_toast_for_widget(
            button,
            &format!("Druckliste konnte nicht vorbereitet werden: {error}"),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskLifecycleBatchOperation {
    Complete,
    Incomplete,
}

impl TaskLifecycleBatchOperation {
    fn completed(self) -> bool {
        matches!(self, Self::Complete)
    }

    fn progress_label(self) -> &'static str {
        match self {
            Self::Complete => "Aufgaben werden als erledigt markiert …",
            Self::Incomplete => "Aufgaben werden als offen markiert …",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskLifecycleMutation {
    identity: TaskSelectionIdentity,
    component: String,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskLifecycleBatchPlan {
    operation: TaskLifecycleBatchOperation,
    mutations: Vec<TaskLifecycleMutation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TaskLifecycleActionState {
    complete_enabled: bool,
    incomplete_enabled: bool,
}

fn task_has_top_level_property(component: &str, property_name: &str) -> bool {
    let mut in_vtodo = false;
    let mut nested_depth = 0usize;
    for line in unfold_ical_lines(component) {
        if !in_vtodo {
            in_vtodo = line.eq_ignore_ascii_case("BEGIN:VTODO");
            continue;
        }
        if nested_depth == 0 && line.eq_ignore_ascii_case("END:VTODO") {
            break;
        }
        if nested_depth == 0
            && line.split_once(':').is_some_and(|(property, _)| {
                property
                    .split(';')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(property_name))
            })
        {
            return true;
        }
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth += 1;
        } else if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            nested_depth = nested_depth.saturating_sub(1);
        }
    }
    false
}

fn task_lifecycle_component_is_complete(component: &str) -> bool {
    task_has_top_level_property(component, "COMPLETED")
}

fn task_component_is_completed_for_purge(component: &str) -> bool {
    task_lifecycle_component_is_complete(component)
        || task_status_value(component) == "COMPLETED"
        || parse_task_percent(component) == 100
}

fn resolve_task_lifecycle_selection(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
) -> Result<Vec<TaskLifecycleMutation>, String> {
    if selection.is_empty() {
        return Err("Keine Aufgaben ausgewählt".to_string());
    }
    let mut resolved = Vec::with_capacity(selection.len());
    let mut seen = HashSet::with_capacity(selection.len());
    for identity in selection {
        if !seen.insert(identity.clone()) {
            return Err("Eine ausgewählte Aufgabe wurde mehrfach aufgelöst".to_string());
        }
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err("Die ausgewählte Aufgabe ist nicht mehr eindeutig verfügbar".to_string());
        };
        let component = validated_task_vtodo_component(candidate, identity)?;
        if task_source_capability(&identity.source_uid, capabilities, local_writable)
            != TaskSourceCapability::Writable
        {
            return Err(
                "Mindestens eine ausgewählte Aufgabenliste ist nicht schreibfähig oder nicht verfügbar"
                    .to_string(),
            );
        }
        resolved.push(TaskLifecycleMutation {
            identity: identity.clone(),
            completed: task_lifecycle_component_is_complete(&component),
            component,
        });
    }
    Ok(resolved)
}

fn task_lifecycle_action_state(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
    batch_running: bool,
) -> TaskLifecycleActionState {
    if batch_running {
        return TaskLifecycleActionState::default();
    }
    let Ok(resolved) =
        resolve_task_lifecycle_selection(selection, candidates, capabilities, local_writable)
    else {
        return TaskLifecycleActionState::default();
    };
    TaskLifecycleActionState {
        complete_enabled: resolved.iter().any(|mutation| !mutation.completed),
        incomplete_enabled: resolved.iter().any(|mutation| mutation.completed),
    }
}

fn transform_task_lifecycle_component(
    component: &str,
    operation: TaskLifecycleBatchOperation,
    completed_timestamp: &str,
) -> Result<String, String> {
    if matches!(operation, TaskLifecycleBatchOperation::Complete)
        && NaiveDateTime::parse_from_str(completed_timestamp, "%Y%m%dT%H%M%SZ").is_err()
    {
        return Err("Der Abschlusszeitpunkt ist ungültig".to_string());
    }
    let component = match operation {
        TaskLifecycleBatchOperation::Complete => {
            let component = update_ical_property(component, "STATUS", Some("COMPLETED"));
            let component = update_ical_property(&component, "PERCENT-COMPLETE", Some("100"));
            if task_lifecycle_component_is_complete(&component) {
                component
            } else {
                update_ical_property(&component, "COMPLETED", Some(completed_timestamp))
            }
        }
        TaskLifecycleBatchOperation::Incomplete => {
            let component = update_ical_property(component, "STATUS", Some("NEEDS-ACTION"));
            let component = update_ical_property(&component, "PERCENT-COMPLETE", None);
            update_ical_property(&component, "COMPLETED", None)
        }
    };
    Ok(component)
}

fn build_task_lifecycle_batch_plan(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
    operation: TaskLifecycleBatchOperation,
    completed_timestamp: &str,
) -> Result<TaskLifecycleBatchPlan, String> {
    let resolved =
        resolve_task_lifecycle_selection(selection, candidates, capabilities, local_writable)?;
    let should_run = match operation {
        TaskLifecycleBatchOperation::Complete => {
            resolved.iter().any(|mutation| !mutation.completed)
        }
        TaskLifecycleBatchOperation::Incomplete => {
            resolved.iter().any(|mutation| mutation.completed)
        }
    };
    if !should_run {
        return Err("Für diese Stapelaktion gibt es keine passenden Aufgaben".to_string());
    }
    let mutations = resolved
        .into_iter()
        .map(|mutation| {
            let component = transform_task_lifecycle_component(
                &mutation.component,
                operation,
                completed_timestamp,
            )?;
            Ok(TaskLifecycleMutation {
                completed: operation.completed(),
                component,
                ..mutation
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(TaskLifecycleBatchPlan {
        operation,
        mutations,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskLifecycleDispatchResult {
    Success(TaskLifecycleMutation),
    Failure {
        identity: TaskSelectionIdentity,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TaskLifecycleBatchReduction {
    successes: Vec<TaskLifecycleMutation>,
    failures: Vec<(TaskSelectionIdentity, String)>,
}

fn reduce_task_lifecycle_batch_results(
    plan: &TaskLifecycleBatchPlan,
    results: &[TaskLifecycleDispatchResult],
) -> TaskLifecycleBatchReduction {
    let mut reduction = TaskLifecycleBatchReduction::default();
    for mutation in &plan.mutations {
        let matching = results
            .iter()
            .filter(|result| match result {
                TaskLifecycleDispatchResult::Success(success) => {
                    success.identity == mutation.identity
                }
                TaskLifecycleDispatchResult::Failure { identity, .. } => {
                    *identity == mutation.identity
                }
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [TaskLifecycleDispatchResult::Success(success)] => {
                reduction.successes.push(success.clone())
            }
            [TaskLifecycleDispatchResult::Failure { error, .. }] => {
                reduction
                    .failures
                    .push((mutation.identity.clone(), error.clone()));
            }
            [] => reduction.failures.push((
                mutation.identity.clone(),
                "Das Backend hat kein Ergebnis für diese Aufgabe geliefert".to_string(),
            )),
            _ => reduction.failures.push((
                mutation.identity.clone(),
                "Das Backend hat mehrdeutige Ergebnisse für diese Aufgabe geliefert".to_string(),
            )),
        }
    }
    reduction
}

fn task_lifecycle_result_is_current(expected_generation: u64, result_generation: u64) -> bool {
    expected_generation == result_generation
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TaskBulkEditDraft {
    dtstart: Option<String>,
    due: Option<String>,
    completed: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    percent: Option<String>,
    class: Option<String>,
    estimated_duration: Option<String>,
    timezone: Option<String>,
    categories_add: Vec<String>,
    categories_remove: Vec<String>,
}

impl TaskBulkEditDraft {
    fn has_changes(&self) -> bool {
        self.dtstart.is_some()
            || self.due.is_some()
            || self.completed.is_some()
            || self.status.is_some()
            || self.priority.is_some()
            || self.percent.is_some()
            || self.class.is_some()
            || self.estimated_duration.is_some()
            || self.timezone.is_some()
            || !self.categories_add.is_empty()
            || !self.categories_remove.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskBulkEditMutation {
    identity: TaskSelectionIdentity,
    component: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskBulkEditPlan {
    mutations: Vec<TaskBulkEditMutation>,
}

fn parse_task_bulk_categories(value: &str) -> Vec<String> {
    category_names_from_ical_value(value)
        .into_iter()
        .map(|category| category.trim().to_string())
        .filter(|category| !category.is_empty())
        .collect()
}

fn validate_task_bulk_duration(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let Some(rest) = value.strip_prefix('P') else {
        return Err("ESTIMATED-DURATION muss eine ISO-8601-Dauer wie PT1H sein".to_string());
    };
    let mut in_time = false;
    let mut digits = String::new();
    let mut last_order = 0u8;
    let mut saw_unit = false;
    let mut saw_week = false;
    for character in rest.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        if character == 'T' {
            if in_time || saw_week || !digits.is_empty() {
                return Err("ESTIMATED-DURATION muss eine ISO-8601-Dauer wie PT1H sein".to_string());
            }
            in_time = true;
            continue;
        }
        if digits.is_empty() {
            return Err("ESTIMATED-DURATION muss eine ISO-8601-Dauer wie PT1H sein".to_string());
        }
        let order = match (in_time, character) {
            (false, 'W') => 1,
            (false, 'D') => 2,
            (true, 'H') => 3,
            (true, 'M') => 4,
            (true, 'S') => 5,
            _ => {
                return Err("ESTIMATED-DURATION muss eine ISO-8601-Dauer wie PT1H sein".to_string());
            }
        };
        if order <= last_order || (character == 'W' && saw_unit) || (saw_week && character != 'W') {
            return Err("ESTIMATED-DURATION muss eine ISO-8601-Dauer wie PT1H sein".to_string());
        }
        last_order = order;
        saw_unit = true;
        saw_week |= character == 'W';
        digits.clear();
    }
    if !saw_unit || !digits.is_empty() {
        return Err("ESTIMATED-DURATION muss eine ISO-8601-Dauer wie PT1H sein".to_string());
    }
    Ok(Some(value.to_string()))
}

fn transform_task_bulk_edit_component(
    component: &str,
    draft: &TaskBulkEditDraft,
    completed_timestamp: &str,
) -> Result<String, String> {
    if !draft.has_changes() {
        return Ok(component.to_string());
    }
    if draft
        .timezone
        .as_deref()
        .is_some_and(|timezone| !timezone.trim().is_empty())
    {
        return Err(
            "Zeitzonen-Stapelbearbeitung benötigt native VTIMEZONE-Unterstützung (Linux-Handoff)"
                .to_string(),
        );
    }
    let editor = task_editor_draft_from_component(component);
    let dtstart_mutation = draft
        .dtstart
        .as_deref()
        .map(|value| task_date_mutation(&editor.dtstart, value, "DTSTART"))
        .transpose()?
        .map_or(TaskDateMutation::Preserve, |(mutation, _)| mutation);
    let due_mutation = draft
        .due
        .as_deref()
        .map(|value| task_date_mutation(&editor.due, value, "DUE"))
        .transpose()?
        .map_or(TaskDateMutation::Preserve, |(mutation, _)| mutation);
    let start = task_date_mutation_date(dtstart_mutation).or(editor.dtstart.original_date);
    let due = task_date_mutation_date(due_mutation).or(editor.due.original_date);
    if (task_date_mutation_changed(dtstart_mutation) || task_date_mutation_changed(due_mutation))
        && let (Some(start), Some(due)) = (start, due)
        && due <= start
    {
        return Err("Das Fälligkeitsdatum muss nach dem Beginn liegen".to_string());
    }
    let mut result = apply_task_date_pair_mutations(
        component,
        &editor.dtstart,
        &editor.due,
        dtstart_mutation,
        due_mutation,
    );

    let lifecycle_changed =
        draft.status.is_some() || draft.percent.is_some() || draft.completed.is_some();
    if lifecycle_changed {
        let mut status = draft
            .status
            .as_deref()
            .map(normalize_task_status)
            .transpose()?
            .unwrap_or_else(|| task_status_value(component));
        let mut percent = draft
            .percent
            .as_deref()
            .map(|value| {
                value
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|value| *value <= 100)
                    .ok_or_else(|| "PERCENT-COMPLETE muss zwischen 0 und 100 liegen".to_string())
            })
            .transpose()?
            .unwrap_or_else(|| parse_task_percent(component));
        let completed = draft
            .completed
            .as_deref()
            .map(|value| task_date_mutation(&editor.completed, value, "COMPLETED"))
            .transpose()?;
        if draft.status.is_some() {
            match status {
                "COMPLETED" => percent = 100,
                "IN-PROCESS" if percent == 0 || percent == 100 => percent = 50,
                "NEEDS-ACTION" => percent = 0,
                _ => {}
            }
        }
        if draft.percent.is_some() {
            status = match percent {
                100 => "COMPLETED",
                0 => "NEEDS-ACTION",
                _ => "IN-PROCESS",
            };
        }
        if let Some((mutation, _)) = completed {
            match mutation {
                TaskDateMutation::Remove => {
                    if status == "COMPLETED" {
                        status = "NEEDS-ACTION";
                        percent = 0;
                    }
                }
                TaskDateMutation::Set(_) => {
                    status = "COMPLETED";
                    percent = 100;
                }
                TaskDateMutation::Preserve => {}
            }
        }
        result = update_ical_property(&result, "STATUS", Some(status));
        result = update_ical_property(&result, "PERCENT-COMPLETE", Some(&percent.to_string()));
        result = match status {
            "COMPLETED" => match completed.map(|(mutation, _)| mutation) {
                Some(TaskDateMutation::Set(date)) => set_task_date_property(
                    &result,
                    "COMPLETED",
                    date,
                    TaskDateValueKind::DateOnly,
                    Some(&editor.completed),
                ),
                Some(TaskDateMutation::Remove) => unreachable!("completed status was normalized"),
                _ if editor.completed.original_date.is_some() => result,
                _ => update_ical_property(&result, "COMPLETED", Some(completed_timestamp)),
            },
            _ => update_ical_property(&result, "COMPLETED", None),
        };
    }
    if let Some(priority) = draft.priority.as_deref() {
        let priority = priority.trim();
        if priority.is_empty() {
            result = update_ical_property(&result, "PRIORITY", None);
        } else {
            let priority = priority
                .parse::<u8>()
                .ok()
                .filter(|value| *value <= 9)
                .map(|value| value.to_string())
                .ok_or_else(|| "PRIORITY muss leer oder zwischen 0 und 9 liegen".to_string())?;
            result = update_ical_property(&result, "PRIORITY", Some(&priority));
        }
    }
    if let Some(class) = draft.class.as_deref() {
        let class = class.trim().to_ascii_uppercase();
        if !class.is_empty() && !matches!(class.as_str(), "PUBLIC" | "PRIVATE" | "CONFIDENTIAL") {
            return Err("CLASS muss PUBLIC, PRIVATE, CONFIDENTIAL oder leer sein".to_string());
        }
        result = update_ical_property(
            &result,
            "CLASS",
            (!class.is_empty()).then_some(class.as_str()),
        );
    }
    if let Some(duration) = draft.estimated_duration.as_deref() {
        let duration = validate_task_bulk_duration(duration)?;
        result = update_ical_property(&result, "ESTIMATED-DURATION", duration.as_deref());
    }
    if !draft.categories_add.is_empty() || !draft.categories_remove.is_empty() {
        let mut categories = category_names_from_ical_component(&result);
        categories.retain(|category| {
            !draft
                .categories_remove
                .iter()
                .any(|remove| category.eq_ignore_ascii_case(remove))
        });
        for add in &draft.categories_add {
            if !categories
                .iter()
                .any(|category| category.eq_ignore_ascii_case(add))
            {
                categories.push(add.clone());
            }
        }
        let categories = categories
            .iter()
            .map(|category| escape_ical_text(category))
            .collect::<Vec<_>>()
            .join(",");
        result = update_ical_property(
            &result,
            "CATEGORIES",
            (!categories.is_empty()).then_some(categories.as_str()),
        );
    }
    Ok(result)
}

fn build_task_bulk_edit_plan(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
    draft: &TaskBulkEditDraft,
    completed_timestamp: &str,
) -> Result<TaskBulkEditPlan, String> {
    if !draft.has_changes() {
        return Err("Für die Stapelbearbeitung wurde kein Feld ausgewählt".to_string());
    }
    let resolved =
        resolve_task_lifecycle_selection(selection, candidates, capabilities, local_writable)?;
    let mutations = resolved
        .into_iter()
        .map(|mutation| {
            let component = transform_task_bulk_edit_component(
                &mutation.component,
                draft,
                completed_timestamp,
            )?;
            validated_task_vtodo_component(
                &TaskExportCandidate {
                    identity: mutation.identity.clone(),
                    component: component.clone(),
                },
                &mutation.identity,
            )?;
            Ok(TaskBulkEditMutation {
                identity: mutation.identity,
                component,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(TaskBulkEditPlan { mutations })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskBulkEditWorkerResult {
    successes: Vec<TaskBulkEditMutation>,
    failure: Option<(TaskSelectionIdentity, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TaskBulkEditReduction {
    successes: Vec<TaskBulkEditMutation>,
    failure: Option<(TaskSelectionIdentity, String)>,
    unattempted: usize,
    invalid_result: bool,
}

fn reduce_task_bulk_edit_result(
    plan: &TaskBulkEditPlan,
    result: &TaskBulkEditWorkerResult,
) -> TaskBulkEditReduction {
    let valid = result.successes.len() <= plan.mutations.len()
        && result
            .successes
            .iter()
            .zip(&plan.mutations)
            .all(|(success, expected)| success.identity == expected.identity)
        && result.failure.as_ref().is_none_or(|(identity, _)| {
            plan.mutations
                .get(result.successes.len())
                .is_some_and(|expected| expected.identity == *identity)
        });
    if !valid {
        return TaskBulkEditReduction {
            failure: Some((
                plan.mutations
                    .first()
                    .map(|mutation| mutation.identity.clone())
                    .unwrap_or(TaskSelectionIdentity {
                        source_uid: String::new(),
                        task_uid: String::new(),
                    }),
                "Das Backend lieferte ein veraltetes oder mehrdeutiges Stapelergebnis".to_string(),
            )),
            unattempted: plan.mutations.len(),
            invalid_result: true,
            ..TaskBulkEditReduction::default()
        };
    }
    let unattempted = plan
        .mutations
        .len()
        .saturating_sub(result.successes.len() + usize::from(result.failure.is_some()));
    TaskBulkEditReduction {
        successes: result.successes.clone(),
        failure: result.failure.clone(),
        unattempted,
        ..TaskBulkEditReduction::default()
    }
}

fn task_bulk_edit_result_is_current(expected_generation: u64, result_generation: u64) -> bool {
    expected_generation == result_generation
}

fn reload_task_snapshot_after_bulk_edit_uncertainty(
    page: &TasksPage,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    let generation = page.imp().task_bulk_edit_generation.get();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let snapshot = load_snapshot_prefer_eds().snapshot;
        if let Err(error) = sender.send_blocking(snapshot) {
            tracing::debug!(%error, "Task bulk-edit recovery snapshot was dropped");
        }
    });
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    glib::spawn_future_local(async move {
        let Ok(snapshot) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_bulk_edit_result_is_current(page.imp().task_bulk_edit_generation.get(), generation)
        {
            return;
        }
        task_store.remove_all();
        for task in snapshot.tasks {
            let priority = if task.priority == 0 {
                "normal".to_string()
            } else {
                task.priority.to_string()
            };
            task_store.append(&TaskItemObject::new(
                &task.source_uid,
                &task.uid,
                &task.summary,
                &task.due,
                &priority,
                task.completed,
                &task.component,
            ));
        }
        render_task_rows(&page, &task_list, &task_store, &empty_tasks);
    });
}

fn task_bulk_edit_field(
    content: &gtk::Box,
    label: &str,
    placeholder: &str,
) -> (gtk::CheckButton, gtk::Entry) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let check = gtk::CheckButton::with_label(label);
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .sensitive(false)
        .build();
    let entry_for_toggle = entry.clone();
    check.connect_toggled(move |check| entry_for_toggle.set_sensitive(check.is_active()));
    row.append(&check);
    row.append(&entry);
    content.append(&row);
    (check, entry)
}

fn present_task_bulk_edit_dialog(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    // Preflight on opening makes unavailable sources explicit, while the
    // toolbar remains enabled for any non-empty upstream selection.
    if let Err(error) = resolve_task_lifecycle_selection(
        &page.imp().task_selection.borrow(),
        &task_export_candidates(task_store),
        &page.imp().task_source_capabilities.borrow(),
        CalendarStoreService::new().is_writable(),
    ) {
        show_toast_for_widget(button, &error);
        return;
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(6)
        .build();
    let hint = gtk::Label::builder()
        .label("Nur aktivierte Felder werden geändert. Leere Datums-, Prioritäts-, Klasse- oder Dauerwerte entfernen die jeweilige Eigenschaft.")
        .wrap(true)
        .halign(gtk::Align::Start)
        .css_classes(vec!["dim-label"])
        .build();
    content.append(&hint);
    let (dtstart_check, dtstart) = task_bulk_edit_field(&content, "Beginn", "YYYY-MM-DD");
    let (due_check, due) = task_bulk_edit_field(&content, "Fälligkeit", "YYYY-MM-DD");
    let (completed_check, completed) =
        task_bulk_edit_field(&content, "Abgeschlossen", "YYYY-MM-DD");
    let (status_check, status) = task_bulk_edit_field(
        &content,
        "Status",
        "NEEDS-ACTION | IN-PROCESS | COMPLETED | CANCELLED",
    );
    let (priority_check, priority) = task_bulk_edit_field(&content, "Priorität", "0 bis 9");
    let (percent_check, percent) = task_bulk_edit_field(&content, "Fortschritt", "0 bis 100");
    let (class_check, class) = task_bulk_edit_field(
        &content,
        "Klassifizierung",
        "PUBLIC | PRIVATE | CONFIDENTIAL",
    );
    let (duration_check, duration) = task_bulk_edit_field(&content, "Geschätzte Dauer", "PT1H");
    let (categories_add_check, categories_add) =
        task_bulk_edit_field(&content, "Kategorien hinzufügen", "Kommagetrennt");
    let (categories_remove_check, categories_remove) =
        task_bulk_edit_field(&content, "Kategorien entfernen", "Kommagetrennt");
    let timezone =
        gtk::CheckButton::with_label("Zeitzone (native VTIMEZONE-Unterstützung erforderlich)");
    timezone.set_sensitive(false);
    content.append(&timezone);
    let dialog = adw::AlertDialog::builder()
        .heading("Aufgaben stapelweise bearbeiten")
        .body("Die Änderungen werden nacheinander auf die ausgewählten Aufgaben angewendet.")
        .extra_child(&content)
        .default_response("apply")
        .build();
    dialog.add_responses(&[("cancel", "Abbrechen"), ("apply", "Anwenden")]);
    let page_weak = page.downgrade();
    let button_for_result = button.clone();
    let dialog_parent = button.root();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response != "apply" {
            dialog.close();
            return;
        }
        let selected = |check: &gtk::CheckButton, entry: &gtk::Entry| {
            check.is_active().then(|| entry.text().to_string())
        };
        let draft = TaskBulkEditDraft {
            dtstart: selected(&dtstart_check, &dtstart),
            due: selected(&due_check, &due),
            completed: selected(&completed_check, &completed),
            status: selected(&status_check, &status),
            priority: selected(&priority_check, &priority),
            percent: selected(&percent_check, &percent),
            class: selected(&class_check, &class),
            estimated_duration: selected(&duration_check, &duration),
            timezone: timezone.is_active().then_some("native".to_string()),
            categories_add: categories_add_check
                .is_active()
                .then(|| parse_task_bulk_categories(&categories_add.text()))
                .unwrap_or_default(),
            categories_remove: categories_remove_check
                .is_active()
                .then(|| parse_task_bulk_categories(&categories_remove.text()))
                .unwrap_or_default(),
        };
        if !draft.has_changes() {
            show_toast_for_widget(
                &button_for_result,
                "Für die Stapelbearbeitung wurde kein Feld ausgewählt",
            );
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        dialog.close();
        start_task_bulk_edit(
            &page,
            &button_for_result,
            &task_store,
            &task_list,
            &empty_tasks,
            draft,
        );
    });
    dialog.present(dialog_parent.as_ref());
}

fn start_task_bulk_edit(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    draft: TaskBulkEditDraft,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    let selection = page.imp().task_selection.borrow().clone();
    let capabilities = page.imp().task_source_capabilities.borrow().clone();
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let plan = match build_task_bulk_edit_plan(
        &selection,
        &task_export_candidates(task_store),
        &capabilities,
        CalendarStoreService::new().is_writable(),
        &draft,
        &stamp,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            show_toast_for_widget(button, &error);
            return;
        }
    };
    let generation = page.imp().task_bulk_edit_generation.get().wrapping_add(1);
    page.imp().task_bulk_edit_generation.set(generation);
    page.imp().task_bulk_edit_running.set(true);
    task_list.set_sensitive(false);
    update_task_selection_controls(page);
    let (sender, receiver) = async_channel::bounded(1);
    let plan_for_worker = plan.clone();
    std::thread::spawn(move || {
        let service = ECalService::new();
        let local_writable = CalendarStoreService::new().is_writable();
        let mut result = TaskBulkEditWorkerResult {
            successes: Vec::new(),
            failure: None,
        };
        for mutation in plan_for_worker.mutations {
            if task_source_capability(&mutation.identity.source_uid, &capabilities, local_writable)
                != TaskSourceCapability::Writable
            {
                result.failure = Some((
                    mutation.identity,
                    "Eine Aufgabenliste ist nicht mehr schreibfähig".to_string(),
                ));
                break;
            }
            match service.modify_component(
                &mutation.identity.source_uid,
                ECalComponentType::Task,
                &mutation.identity.task_uid,
                &mutation.component,
            ) {
                Ok(mut confirmed) => {
                    if confirmed
                        .component
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                    {
                        confirmed.component = load_snapshot_prefer_eds()
                            .snapshot
                            .tasks
                            .into_iter()
                            .find(|task| {
                                task.source_uid == mutation.identity.source_uid
                                    && task.uid == mutation.identity.task_uid
                            })
                            .map(|task| task.component);
                    }
                    match task_authoritative_component(
                        &mutation.identity.task_uid,
                        &confirmed.uid,
                        confirmed.component,
                    )
                    .and_then(|component| {
                        validated_task_vtodo_component(
                            &TaskExportCandidate {
                                identity: mutation.identity.clone(),
                                component,
                            },
                            &mutation.identity,
                        )
                    }) {
                        Ok(component) => result.successes.push(TaskBulkEditMutation {
                            component,
                            ..mutation
                        }),
                        Err(error) => {
                            result.failure = Some((mutation.identity, error));
                            break;
                        }
                    }
                }
                Err(error) => {
                    result.failure = Some((mutation.identity, error.to_string()));
                    break;
                }
            }
        }
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Task bulk-edit result was dropped");
        }
    });
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    let button_for_result = button.clone();
    glib::spawn_future_local(async move {
        let received = receiver.recv().await;
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_bulk_edit_result_is_current(page.imp().task_bulk_edit_generation.get(), generation)
        {
            return;
        }
        page.imp().task_bulk_edit_running.set(false);
        task_list.set_sensitive(true);
        let result = match received {
            Ok(result) => result,
            Err(error) => {
                tracing::debug!(%error, "Task bulk-edit channel closed");
                update_task_selection_controls(&page);
                reload_task_snapshot_after_bulk_edit_uncertainty(
                    &page,
                    &task_store,
                    &task_list,
                    &empty_tasks,
                );
                show_toast_for_widget(
                    &button_for_result,
                    "Stapelbearbeitungsergebnis ging verloren; autoritative Daten werden neu geladen",
                );
                return;
            }
        };
        let reduction = reduce_task_bulk_edit_result(&plan, &result);
        let successes = reduction.successes;
        let failure = reduction.failure;
        let unattempted = reduction.unattempted;
        let mut applied = 0usize;
        let mut recovery_required = reduction.invalid_result;
        for success in &successes {
            let matches = (0..task_store.n_items())
                .filter(|index| {
                    task_store
                        .item(*index)
                        .and_downcast::<TaskItemObject>()
                        .is_some_and(|task| {
                            task.source_uid() == success.identity.source_uid
                                && task.id() == success.identity.task_uid
                        })
                })
                .collect::<Vec<_>>();
            if let [index] = matches.as_slice() {
                match task_item_from_confirmed_component(
                    &success.identity.source_uid,
                    &success.identity.task_uid,
                    &success.component,
                ) {
                    Ok(item) => {
                        task_store.remove(*index);
                        task_store.insert(*index, &item);
                        applied += 1;
                    }
                    Err(_) => recovery_required = true,
                }
            } else {
                recovery_required = true;
            }
        }
        render_task_rows(&page, &task_list, &task_store, &empty_tasks);
        if recovery_required {
            reload_task_snapshot_after_bulk_edit_uncertainty(
                &page,
                &task_store,
                &task_list,
                &empty_tasks,
            );
        }
        match (applied, failure.is_some() || recovery_required, unattempted) {
            (count, false, 0) => page.show_toast(&format!(
                "{count} Aufgabe{} bearbeitet",
                if count == 1 { "" } else { "n" }
            )),
            (count, true, pending) => page.show_toast(&format!(
                "{count} Aufgabe{} bearbeitet; Fehler, {pending} nicht versucht",
                if count == 1 { "" } else { "n" }
            )),
            _ => page.show_toast("Keine Aufgabe bearbeitet"),
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDeleteMutation {
    identity: TaskSelectionIdentity,
    summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDeletePlan {
    mutations: Vec<TaskDeleteMutation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDeleteResolved {
    identity: TaskSelectionIdentity,
    component: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDeleteConfirmation {
    title: String,
    description: String,
}

fn resolve_task_delete_selection(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
) -> Result<Vec<TaskDeleteResolved>, String> {
    if selection.is_empty() {
        return Err("Keine Aufgaben ausgewählt".to_string());
    }
    let mut seen = HashSet::with_capacity(selection.len());
    let mut resolved = Vec::with_capacity(selection.len());
    for identity in selection {
        if !seen.insert(identity.clone()) {
            return Err("Eine ausgewählte Aufgabe wurde mehrfach aufgelöst".to_string());
        }
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err("Die ausgewählte Aufgabe ist nicht mehr eindeutig verfügbar".to_string());
        };
        let component = validated_task_vtodo_component(candidate, identity)?;
        if task_source_capability(&identity.source_uid, capabilities, local_writable)
            != TaskSourceCapability::Writable
        {
            return Err(
                "Mindestens eine ausgewählte Aufgabenliste ist nicht schreibfähig oder nicht verfügbar"
                    .to_string(),
            );
        }
        resolved.push(TaskDeleteResolved {
            identity: identity.clone(),
            component,
        });
    }
    Ok(resolved)
}

fn build_task_delete_plan(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
) -> Result<TaskDeletePlan, String> {
    let resolved =
        resolve_task_delete_selection(selection, candidates, capabilities, local_writable)?;
    let mut mutations = Vec::with_capacity(resolved.len());
    for resolved in resolved {
        let TaskDeleteResolved {
            identity,
            component,
        } = resolved;
        if task_has_top_level_property(&component, "RECURRENCE-ID") {
            return Err(
                "Serienaufgaben mit RECURRENCE-ID können ohne MOD_THIS-Unterstützung nicht sicher gelöscht werden"
                    .to_string(),
            );
        }
        // Upstream offers an iTIP cancellation/retract prompt only for the
        // single selected task.  The portable ECal bridge cannot send that
        // notice, so fail closed for exactly that case while preserving
        // upstream's multi-selection deletion behavior.
        if selection.len() == 1
            && (task_has_top_level_property(&component, "ORGANIZER")
                || task_has_top_level_property(&component, "ATTENDEE"))
        {
            return Err(
                "Diese zugewiesene Aufgabe benötigt eine iTIP-Stornierungsbenachrichtigung (Linux-Handoff)"
                    .to_string(),
            );
        }
        mutations.push(TaskDeleteMutation {
            identity,
            summary: ical_property(&component, "SUMMARY").unwrap_or_default(),
        });
    }
    Ok(TaskDeletePlan { mutations })
}

fn build_task_purge_plan(
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
) -> Result<TaskDeletePlan, String> {
    let mut seen = HashSet::new();
    let mut mutations = Vec::new();
    for candidate in candidates {
        if !task_component_is_completed_for_purge(&candidate.component) {
            continue;
        }
        if task_source_capability(&candidate.identity.source_uid, capabilities, local_writable)
            != TaskSourceCapability::Writable
        {
            continue;
        }
        if !seen.insert(candidate.identity.clone()) {
            return Err("Eine erledigte Aufgabe ist nicht mehr eindeutig verfügbar".to_string());
        }
        let component = validated_task_vtodo_component(candidate, &candidate.identity)?;
        if task_has_top_level_property(&component, "RECURRENCE-ID") {
            return Err(
                "Eine erledigte Serieninstanz benötigt native MOD_THIS-Unterstützung (Linux-Handoff)"
                    .to_string(),
            );
        }
        mutations.push(TaskDeleteMutation {
            identity: candidate.identity.clone(),
            summary: ical_property(&component, "SUMMARY").unwrap_or_default(),
        });
    }
    if mutations.is_empty() {
        return Err("Keine erledigten Aufgaben in schreibfähigen Aufgabenlisten".to_string());
    }
    Ok(TaskDeletePlan { mutations })
}

fn task_delete_confirmation(plan: &TaskDeletePlan) -> TaskDeleteConfirmation {
    let count = plan.mutations.len();
    if let [mutation] = plan.mutations.as_slice()
        && !mutation.summary.trim().is_empty()
    {
        let summary = glib::markup_escape_text(&mutation.summary);
        return TaskDeleteConfirmation {
            title: format!("Aufgabe „{summary}“ löschen?"),
            description: "Alle Informationen zu dieser Aufgabe werden unwiderruflich gelöscht."
                .to_string(),
        };
    }
    TaskDeleteConfirmation {
        title: if count == 1 {
            "Aufgabe löschen?".to_string()
        } else {
            format!("{count} Aufgaben löschen?")
        },
        description: if count == 1 {
            "Alle Informationen zu dieser Aufgabe werden unwiderruflich gelöscht.".to_string()
        } else {
            format!("Alle Informationen zu diesen {count} Aufgaben werden unwiderruflich gelöscht.")
        },
    }
}

fn task_delete_action_enabled(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
    batch_running: bool,
) -> bool {
    !batch_running
        && resolve_task_delete_selection(selection, candidates, capabilities, local_writable)
            .is_ok()
}

fn task_delete_can_dispatch(confirmed: bool) -> bool {
    confirmed
}

fn task_delete_plan_matches_selection(
    plan: &TaskDeletePlan,
    selection: &[TaskSelectionIdentity],
) -> bool {
    plan.mutations
        .iter()
        .map(|mutation| &mutation.identity)
        .eq(selection.iter())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDeleteWorkerResult {
    removed: Vec<TaskSelectionIdentity>,
    failure: Option<(TaskSelectionIdentity, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TaskDeleteBatchReduction {
    removed: Vec<TaskSelectionIdentity>,
    failure: Option<(TaskSelectionIdentity, String)>,
    unattempted: Vec<TaskSelectionIdentity>,
}

fn reduce_task_delete_batch_result(
    plan: &TaskDeletePlan,
    result: &TaskDeleteWorkerResult,
) -> TaskDeleteBatchReduction {
    let mut reduction = TaskDeleteBatchReduction::default();
    let expected = plan
        .mutations
        .iter()
        .map(|mutation| mutation.identity.clone())
        .collect::<Vec<_>>();
    let removed_valid = result.removed.len() <= expected.len()
        && result
            .removed
            .iter()
            .zip(expected.iter())
            .all(|(removed, expected)| removed == expected)
        && result.removed.iter().collect::<HashSet<_>>().len() == result.removed.len();
    let failure_valid = result.failure.as_ref().is_none_or(|(identity, _)| {
        expected
            .get(result.removed.len())
            .is_some_and(|expected| expected == identity)
    });
    if !removed_valid || !failure_valid {
        reduction.failure = Some((
            expected.first().cloned().unwrap_or(TaskSelectionIdentity {
                source_uid: String::new(),
                task_uid: String::new(),
            }),
            "Das Backend lieferte ein veraltetes oder mehrdeutiges Lösch-Ergebnis".to_string(),
        ));
        reduction.unattempted = expected;
        return reduction;
    }
    reduction.removed = result.removed.clone();
    reduction.failure = result.failure.clone();
    let attempted = result.removed.len() + usize::from(result.failure.is_some());
    reduction.unattempted = expected.into_iter().skip(attempted).collect();
    reduction
}

fn task_delete_result_is_current(expected_generation: u64, result_generation: u64) -> bool {
    expected_generation == result_generation
}

fn build_selected_task_export(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
) -> Result<String, String> {
    if selection.is_empty() {
        return Err("Keine Aufgaben ausgewählt".to_string());
    }

    let mut components = Vec::with_capacity(selection.len());
    for identity in selection {
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err("Die ausgewählte Aufgabe ist nicht mehr verfügbar".to_string());
        };
        components.push(validated_task_vtodo_component(candidate, identity)?);
    }

    Ok(format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Evolution GTK4//EN\r\n{}\r\nEND:VCALENDAR\r\n",
        components.join("\r\n")
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TaskClipboardActionState {
    copy_enabled: bool,
    cut_enabled: bool,
    paste_enabled: bool,
    select_all_enabled: bool,
}

fn task_clipboard_action_state(
    selection_count: usize,
    copy_payload_valid: bool,
    all_selected_writable: bool,
    destination_writable: bool,
    calendar_clipboard_available: bool,
    editing_or_mutating: bool,
) -> TaskClipboardActionState {
    if editing_or_mutating {
        return TaskClipboardActionState {
            // Upstream keeps Select All available even while editing.  A
            // running mutation still makes the list itself insensitive.
            select_all_enabled: true,
            ..TaskClipboardActionState::default()
        };
    }
    TaskClipboardActionState {
        copy_enabled: selection_count > 0 && copy_payload_valid,
        cut_enabled: selection_count > 0 && copy_payload_valid && all_selected_writable,
        paste_enabled: destination_writable && calendar_clipboard_available,
        // Mirrors ETaskTable: this is available even for an empty visible
        // table, unless a mutation has made the list insensitive.
        select_all_enabled: true,
    }
}

fn task_paste_destination(
    source_uids: &[String],
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
    preferred_source_uid: Option<&str>,
) -> Result<String, String> {
    let Some(index) = task_default_source_index(
        source_uids,
        preferred_source_uid,
        None,
        capabilities,
        local_writable,
    ) else {
        return Err(
            "Keine schreibfähige Standard-Aufgabenliste für Einfügen verfügbar".to_string(),
        );
    };
    let Some(source_uid) = source_uids.get(index) else {
        return Err("Die Ziel-Aufgabenliste ist nicht eindeutig verfügbar".to_string());
    };
    if task_source_capability(source_uid, capabilities, local_writable)
        != TaskSourceCapability::Writable
    {
        return Err(
            "Die Ziel-Aufgabenliste ist nicht schreibfähig oder nicht verfügbar".to_string(),
        );
    }
    Ok(source_uid.clone())
}

fn task_component_requires_vtimezone(component: &str) -> bool {
    unfold_ical_lines(component).iter().any(|line| {
        line.split_once(':').is_some_and(|(property, _)| {
            property.split(';').skip(1).any(|parameter| {
                parameter.eq_ignore_ascii_case("TZID")
                    || parameter
                        .get(..5)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("TZID="))
            })
        })
    })
}

fn parse_task_clipboard_payload(payload: &str) -> Result<Vec<String>, String> {
    let lines = payload.lines().map(str::trim_end).collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| line.eq_ignore_ascii_case("BEGIN:VTODO"))
        && lines
            .last()
            .is_some_and(|line| line.eq_ignore_ascii_case("END:VTODO"))
    {
        return Ok(vec![lines.join("\r\n")]);
    }
    if lines
        .first()
        .is_none_or(|line| !line.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
        || lines
            .last()
            .is_none_or(|line| !line.eq_ignore_ascii_case("END:VCALENDAR"))
    {
        return Err(
            "Die Zwischenablage enthält keine gültige VCALENDAR-Aufgabenauswahl".to_string(),
        );
    }
    let mut components = Vec::new();
    let mut component = Vec::new();
    let mut depth = 0usize;
    for line in &lines[1..lines.len() - 1] {
        if component.is_empty() {
            if line.eq_ignore_ascii_case("BEGIN:VTODO") {
                component.push(*line);
                depth = 1;
            } else if line
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
            {
                return Err(
                    "Die Zwischenablage enthält keine ausschließlich portablen VTODO-Komponenten"
                        .to_string(),
                );
            }
            continue;
        }
        component.push(*line);
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            depth += 1;
        } else if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                if !line.eq_ignore_ascii_case("END:VTODO") {
                    return Err(
                        "Die Zwischenablage enthält eine ungültig verschachtelte Aufgabe"
                            .to_string(),
                    );
                }
                components.push(component.join("\r\n"));
                component.clear();
            }
        }
    }
    if !component.is_empty() || components.is_empty() {
        return Err("Die Zwischenablage enthält keine vollständige VTODO-Aufgabe".to_string());
    }
    Ok(components)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPasteMutation {
    identity: TaskSelectionIdentity,
    component: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPastePlan {
    destination_uid: String,
    mutations: Vec<TaskPasteMutation>,
}

fn plan_task_clipboard_paste_with_uids(
    payload: &str,
    destination_uid: &str,
    existing: &[TaskSelectionIdentity],
    fresh_uids: &[String],
) -> Result<TaskPastePlan, String> {
    if destination_uid.trim().is_empty() {
        return Err("Die Ziel-Aufgabenliste ist ungültig".to_string());
    }
    let components = parse_task_clipboard_payload(payload)?;
    if components.len() != fresh_uids.len() {
        return Err("Die Einfügeplanung enthält keine eindeutigen neuen Aufgaben-UIDs".to_string());
    }
    let mut identities = existing.iter().cloned().collect::<HashSet<_>>();
    let mut mutations = Vec::with_capacity(components.len());
    for (component, fresh_uid) in components.into_iter().zip(fresh_uids) {
        if task_component_requires_vtimezone(&component) {
            return Err("Aufgaben mit TZID benötigen VTIMEZONE-Unterstützung und werden nicht unsicher eingefügt".to_string());
        }
        let original_uid = ical_properties(&component, "UID");
        let [_original_uid] = original_uid.as_slice() else {
            return Err("Eine Zwischenablage-Aufgabe hat keine eindeutige UID".to_string());
        };
        if fresh_uid.trim().is_empty() {
            return Err("Die Zwischenablage-Aufgaben sind nicht eindeutig".to_string());
        }
        let identity = TaskSelectionIdentity {
            source_uid: destination_uid.to_string(),
            task_uid: fresh_uid.clone(),
        };
        if !identities.insert(identity.clone()) {
            return Err("Eine neue Aufgaben-UID kollidiert bereits in der Zielquelle".to_string());
        }
        let component = update_ical_property(&component, "UID", Some(fresh_uid));
        let component = validated_task_vtodo_component(
            &TaskExportCandidate {
                identity: identity.clone(),
                component,
            },
            &identity,
        )?;
        mutations.push(TaskPasteMutation {
            identity,
            component,
        });
    }
    Ok(TaskPastePlan {
        destination_uid: destination_uid.to_string(),
        mutations,
    })
}

fn current_task_filter_context() -> TaskFilterContext {
    let timezone = AppSettings::try_new()
        .map(|settings| settings.calendar_time_zone())
        .and_then(|identifier| {
            if identifier.eq_ignore_ascii_case("system") {
                Some(glib::TimeZone::local())
            } else if identifier.eq_ignore_ascii_case("utc") {
                Some(glib::TimeZone::utc())
            } else {
                glib::TimeZone::from_identifier(Some(&identifier))
            }
        })
        .unwrap_or_else(glib::TimeZone::local);
    let current = glib::DateTime::now(&timezone).ok();
    let now = current
        .as_ref()
        .and_then(|datetime| {
            NaiveDate::from_ymd_opt(
                datetime.year(),
                datetime.month() as u32,
                datetime.day_of_month() as u32,
            )?
            .and_hms_opt(
                datetime.hour() as u32,
                datetime.minute() as u32,
                datetime.second() as u32,
            )
        })
        .unwrap_or_else(|| Local::now().naive_local());
    TaskFilterContext {
        today: now.date(),
        now,
        now_timestamp: current
            .map(|datetime| datetime.to_unix())
            .unwrap_or_else(|| Utc::now().timestamp()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskTextField {
    Any,
    Summary,
    Description,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskDateConstraint {
    None,
    Today,
    Next(i64),
    Overdue,
    Scheduled,
    Started,
    Range(NaiveDate, NaiveDate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskQueryPredicate {
    Category(Option<String>),
    Status(String),
    Due(TaskDateConstraint),
    Start(TaskDateConstraint),
    HasAttachments(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TaskQuery {
    text: Vec<(TaskTextField, String)>,
    predicates: Vec<TaskQueryPredicate>,
}

impl TaskQuery {
    fn parse(mode: TaskSearchMode, raw: &str) -> Self {
        let mut query = Self::default();
        match mode {
            TaskSearchMode::AnyField | TaskSearchMode::Summary | TaskSearchMode::Description => {
                let field = match mode {
                    TaskSearchMode::AnyField => TaskTextField::Any,
                    TaskSearchMode::Summary => TaskTextField::Summary,
                    TaskSearchMode::Description => TaskTextField::Description,
                    TaskSearchMode::Advanced => unreachable!(),
                };
                query.text.extend(
                    raw.split_whitespace()
                        .map(str::trim)
                        .filter(|term| !term.is_empty())
                        .map(|term| (field, term.to_lowercase())),
                );
            }
            TaskSearchMode::Advanced => {
                for token in advanced_query_tokens(raw) {
                    let Some((field, value)) = token.split_once(':') else {
                        if !token.trim().is_empty() {
                            query.text.push((TaskTextField::Any, token.to_lowercase()));
                        }
                        continue;
                    };
                    let field = field.trim().to_ascii_lowercase();
                    let value = value.trim();
                    if value.is_empty() {
                        continue;
                    }
                    let value_lower = value.to_lowercase();
                    if (field == "category" || field == "categories")
                        && value.eq_ignore_ascii_case("any")
                    {
                        continue;
                    }
                    let predicate = match field.as_str() {
                        "category" | "categories" => Some(TaskQueryPredicate::Category(
                            (!value.eq_ignore_ascii_case("none")).then_some(value.to_string()),
                        )),
                        "status" => normalize_query_status(value)
                            .map(|status| TaskQueryPredicate::Status(status.to_string())),
                        "due" => parse_task_date_constraint(value).map(TaskQueryPredicate::Due),
                        "start" | "starts" => {
                            parse_task_date_constraint(value).map(TaskQueryPredicate::Start)
                        }
                        "has"
                            if value.eq_ignore_ascii_case("attachment")
                                || value.eq_ignore_ascii_case("attachments") =>
                        {
                            Some(TaskQueryPredicate::HasAttachments(true))
                        }
                        "has"
                            if value.eq_ignore_ascii_case("no-attachment")
                                || value.eq_ignore_ascii_case("no-attachments") =>
                        {
                            Some(TaskQueryPredicate::HasAttachments(false))
                        }
                        _ => None,
                    };
                    if let Some(predicate) = predicate {
                        query.predicates.push(predicate);
                    } else {
                        query.text.push((
                            if field == "summary" {
                                TaskTextField::Summary
                            } else if field == "description" {
                                TaskTextField::Description
                            } else {
                                TaskTextField::Any
                            },
                            if field == "summary" || field == "description" {
                                value_lower
                            } else {
                                token.to_lowercase()
                            },
                        ));
                    }
                }
            }
        }
        query
    }

    fn matches(&self, task: &TaskItemObject, context: TaskFilterContext) -> bool {
        self.text.iter().all(|(field, term)| {
            let haystack = match field {
                TaskTextField::Any => task_search_haystack(task),
                TaskTextField::Summary => task.title().to_lowercase(),
                TaskTextField::Description => ical_property(&task.component(), "DESCRIPTION")
                    .unwrap_or_default()
                    .to_lowercase(),
            };
            haystack.contains(term)
        }) && self
            .predicates
            .iter()
            .all(|predicate| task_query_predicate_matches(task, predicate, context))
    }
}

fn advanced_query_tokens(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn normalize_query_status(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "open" | "needs-action" | "todo" => Some("NEEDS-ACTION"),
        "in-progress" | "inprogress" | "started" => Some("IN-PROCESS"),
        "completed" | "complete" | "done" => Some("COMPLETED"),
        "cancelled" | "canceled" => Some("CANCELLED"),
        _ => None,
    }
}

fn parse_task_date_constraint(value: &str) -> Option<TaskDateConstraint> {
    let value = value.trim();
    match value.to_ascii_lowercase().as_str() {
        "none" | "no-due" | "no-start" => return Some(TaskDateConstraint::None),
        "today" => return Some(TaskDateConstraint::Today),
        "next7" | "next-7" => return Some(TaskDateConstraint::Next(7)),
        "next30" | "next-30" => return Some(TaskDateConstraint::Next(30)),
        "next365" | "next-365" => return Some(TaskDateConstraint::Next(365)),
        "overdue" => return Some(TaskDateConstraint::Overdue),
        "scheduled" => return Some(TaskDateConstraint::Scheduled),
        "started" => return Some(TaskDateConstraint::Started),
        _ => {}
    }
    let (start, end) = value.split_once("..")?;
    let start = parse_task_query_date(start)?;
    let end = parse_task_query_date(end)?;
    (start <= end).then_some(TaskDateConstraint::Range(start, end))
}

fn parse_task_query_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value.trim(), "%Y%m%d"))
        .ok()
}

impl TaskSort {
    fn from_setting(value: &str) -> Self {
        match value {
            "title" => Self::Title,
            "priority" => Self::Priority,
            _ => Self::DueDate,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::DueDate => "due",
            Self::Title => "title",
            Self::Priority => "priority",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskQuickEditValues {
    due: String,
    priority: u8,
    status: &'static str,
    percent: u8,
    categories: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskDateValueKind {
    DateOnly,
    DateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskAttachmentValueKind {
    Uri,
    Binary,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskAttachmentLine {
    /// The unfolded logical line, including every parameter and the original
    /// value.  Keeping this separate from the editor text prevents a no-op
    /// edit from going through a URI-only serializer.
    raw: String,
    property: String,
    value: String,
    kind: TaskAttachmentValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskAttachmentDraft {
    lines: Vec<TaskAttachmentLine>,
    editor_text: String,
    attachments_changed: bool,
}

impl TaskAttachmentDraft {
    fn empty() -> Self {
        Self {
            lines: Vec::new(),
            editor_text: String::new(),
            attachments_changed: false,
        }
    }

    fn from_component(component: &str) -> Self {
        let lines = task_top_level_attachment_lines(component);
        let editor_text = lines
            .iter()
            .map(|line| line.raw.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            lines,
            editor_text,
            attachments_changed: false,
        }
    }

    fn with_editor_text(&self, editor_text: &str) -> Self {
        let mut draft = self.clone();
        draft.attachments_changed = editor_text != self.editor_text;
        draft.editor_text = editor_text.to_string();
        draft
    }
}

/// The editor displays date-only input, but keeps the original logical
/// property around so an imported date-time is not rewritten merely because
/// the task was opened and saved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDateDraft {
    input: String,
    original_line: Option<String>,
    original_date: Option<NaiveDate>,
    original_kind: Option<TaskDateValueKind>,
}

impl TaskDateDraft {
    fn empty() -> Self {
        Self {
            input: String::new(),
            original_line: None,
            original_date: None,
            original_kind: None,
        }
    }

    fn from_component(component: &str, property_name: &str) -> Self {
        let Some((property, value)) = ical_property_with_parameters(component, property_name)
        else {
            return Self::empty();
        };

        let parsed = parse_task_date_property(&property, &value).ok();
        let (input, original_kind, original_date) = match parsed {
            Some((kind, date)) => {
                let display_date = if kind == TaskDateValueKind::DateTime {
                    property_local_date(component, property_name, false).unwrap_or(date)
                } else {
                    date
                };
                (
                    display_date.format("%Y-%m-%d").to_string(),
                    Some(kind),
                    Some(display_date),
                )
            }
            None => (value.clone(), None, None),
        };

        Self {
            input,
            original_line: Some(format!("{property}:{value}")),
            original_date,
            original_kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskDateMutation {
    Preserve,
    Remove,
    Set(NaiveDate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskLifecycleValues {
    status: &'static str,
    percent: u8,
    completed: Option<NaiveDate>,
    completion_present: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TaskLifecycleEdits {
    status: bool,
    percent: bool,
    completed: bool,
}

#[derive(Debug, Clone)]
struct TaskEditorDraft {
    uid: String,
    summary: String,
    dtstart: TaskDateDraft,
    due: TaskDateDraft,
    completed: TaskDateDraft,
    recurrence: String,
    rdates: String,
    exdates: String,
    priority: u8,
    categories: String,
    description: String,
    attachments: String,
    attachment_lines: Vec<TaskAttachmentLine>,
    attachments_changed: bool,
    status: &'static str,
    status_raw: Option<String>,
    unsupported_status: Option<String>,
    percent: u8,
    percent_raw: Option<String>,
    unsupported_percent: Option<String>,
}

impl TaskEditorDraft {
    fn default_for_new() -> Self {
        Self {
            uid: String::new(),
            summary: String::new(),
            dtstart: TaskDateDraft::empty(),
            due: TaskDateDraft::empty(),
            completed: TaskDateDraft::empty(),
            recurrence: String::new(),
            rdates: String::new(),
            exdates: String::new(),
            priority: 0,
            categories: String::new(),
            description: String::new(),
            attachments: String::new(),
            attachment_lines: Vec::new(),
            attachments_changed: false,
            status: "NEEDS-ACTION",
            status_raw: None,
            unsupported_status: None,
            percent: 0,
            percent_raw: None,
            unsupported_percent: None,
        }
    }
}

fn task_editor_draft_from_component(component: &str) -> TaskEditorDraft {
    let status_raw = ical_property(component, "STATUS");
    let percent_raw = ical_property(component, "PERCENT-COMPLETE");
    let attachment_draft = TaskAttachmentDraft::from_component(component);
    TaskEditorDraft {
        uid: ical_property(component, "UID").unwrap_or_default(),
        summary: ical_property(component, "SUMMARY").unwrap_or_default(),
        dtstart: TaskDateDraft::from_component(component, "DTSTART"),
        due: TaskDateDraft::from_component(component, "DUE"),
        completed: TaskDateDraft::from_component(component, "COMPLETED"),
        recurrence: ical_property(component, "RRULE").unwrap_or_default(),
        rdates: ical_properties(component, "RDATE").join(","),
        exdates: ical_properties(component, "EXDATE").join(","),
        priority: task_priority_value(&ical_property(component, "PRIORITY").unwrap_or_default()),
        categories: ical_property(component, "CATEGORIES").unwrap_or_default(),
        description: ical_property(component, "DESCRIPTION").unwrap_or_default(),
        attachments: attachment_draft.editor_text,
        attachment_lines: attachment_draft.lines,
        attachments_changed: attachment_draft.attachments_changed,
        status: task_status_value(component),
        unsupported_status: status_raw
            .as_deref()
            .filter(|value| normalize_task_status(value).is_err())
            .map(ToOwned::to_owned),
        status_raw,
        percent: parse_task_percent(component),
        unsupported_percent: percent_raw
            .as_deref()
            .filter(|value| parse_task_percent_value(value).is_none())
            .map(ToOwned::to_owned),
        percent_raw,
    }
}

fn task_lifecycle_edits_inferred(
    draft: &TaskEditorDraft,
    status: &str,
    percent: u8,
    completed: &str,
) -> TaskLifecycleEdits {
    TaskLifecycleEdits {
        // This helper is used by non-interactive callers such as quick edit.
        // Compare against the displayed draft values rather than property
        // presence, otherwise changing the title of a task without a
        // PERCENT-COMPLETE property would manufacture one on save.
        status: !draft.status.eq_ignore_ascii_case(status),
        percent: draft.percent != percent,
        completed: completed.trim() != draft.completed.input.trim(),
    }
}

fn parse_task_date_property(
    property: &str,
    value: &str,
) -> Result<(TaskDateValueKind, NaiveDate), String> {
    let value = value.trim();
    let declared_type = property.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("VALUE")
            .then(|| value.trim().trim_matches('"').to_ascii_uppercase())
    });

    match declared_type.as_deref() {
        Some("DATE") => {
            if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("Der iCalendar-Wert ist kein gültiges Datum".to_string());
            }
            NaiveDate::parse_from_str(value, "%Y%m%d")
                .map(|date| (TaskDateValueKind::DateOnly, date))
                .map_err(|_| "Der iCalendar-Wert ist kein gültiges Datum".to_string())
        }
        Some("DATE-TIME") => parse_task_datetime_date(value)
            .map(|date| (TaskDateValueKind::DateTime, date))
            .ok_or_else(|| "Der iCalendar-Wert ist kein gültiger Zeitstempel".to_string()),
        Some(_) => Err("Der iCalendar-Wert hat einen nicht unterstützten Typ".to_string()),
        None if value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            NaiveDate::parse_from_str(value, "%Y%m%d")
                .map(|date| (TaskDateValueKind::DateOnly, date))
                .map_err(|_| "Der iCalendar-Wert ist kein gültiges Datum".to_string())
        }
        None => parse_task_datetime_date(value)
            .map(|date| (TaskDateValueKind::DateTime, date))
            .ok_or_else(|| {
                "Der iCalendar-Wert ist kein gültiges Datum oder Zeitstempel".to_string()
            }),
    }
}

fn parse_task_datetime_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    let value_without_utc = value.strip_suffix('Z').unwrap_or(value);
    (value_without_utc.len() == 15 && value_without_utc.as_bytes().get(8) == Some(&b'T'))
        .then_some(())?;
    NaiveDateTime::parse_from_str(value_without_utc, "%Y%m%dT%H%M%S")
        .ok()
        .map(|datetime| datetime.date())
}

fn parse_task_date_only_input(value: &str, field: &str) -> Result<NaiveDate, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} darf nicht leer sein"));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(format!("{field} enthält ungültige Steuerzeichen"));
    }
    if value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
    {
        return Err(format!("{field} muss YYYY-MM-DD entsprechen"));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("{field} muss ein gültiges Datum enthalten"))
}

fn task_date_mutation(
    draft: &TaskDateDraft,
    input: &str,
    field: &str,
) -> Result<(TaskDateMutation, Option<NaiveDate>), String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok((TaskDateMutation::Remove, None));
    }

    if draft.original_line.is_some() && input == draft.input.trim() {
        return Ok((TaskDateMutation::Preserve, draft.original_date));
    }

    let date = parse_task_date_only_input(input, field)?;
    Ok((TaskDateMutation::Set(date), Some(date)))
}

fn task_lifecycle_for_date(
    status: &str,
    percent: u8,
    completed: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<TaskLifecycleValues, String> {
    task_lifecycle_for_date_with_presence(status, percent, completed, completed.is_some(), today)
}

fn task_lifecycle_for_date_with_presence(
    status: &str,
    percent: u8,
    completed: Option<NaiveDate>,
    completion_present: bool,
    today: NaiveDate,
) -> Result<TaskLifecycleValues, String> {
    let status = normalize_task_status(status)?;
    if completed.is_some_and(|date| date > today) {
        return Err("Das Abschlussdatum darf nicht in der Zukunft liegen".to_string());
    }

    match status {
        "NEEDS-ACTION" => Ok(TaskLifecycleValues {
            status,
            percent: 0,
            completed: None,
            completion_present: false,
        }),
        "IN-PROCESS" => Ok(TaskLifecycleValues {
            status,
            percent: if (1..=99).contains(&percent) {
                percent
            } else {
                50
            },
            completed: None,
            completion_present: false,
        }),
        "COMPLETED" => Ok(TaskLifecycleValues {
            status,
            percent: 100,
            completed: if completion_present {
                completed
            } else {
                return Err("Erledigte Aufgaben benötigen ein Abschlussdatum".to_string());
            },
            completion_present: true,
        }),
        // CANCELLED is retained for imported tasks.  It is not one of the
        // GTK3 lifecycle triplet, but a cancelled task must not retain a
        // contradictory completion marker.
        "CANCELLED" => Ok(TaskLifecycleValues {
            status,
            percent: 0,
            completed: None,
            completion_present: false,
        }),
        _ => unreachable!("normalize_task_status returned an unknown value"),
    }
}

fn task_initial_editor_lifecycle(draft: &TaskEditorDraft, today: NaiveDate) -> (u8, String) {
    // Unsupported imported values are deliberately displayed as-is (with a
    // warning in the dialog) until the user explicitly changes the field.
    if draft.unsupported_status.is_some() || draft.unsupported_percent.is_some() {
        return (draft.percent, draft.completed.input.clone());
    }
    match draft.status {
        "NEEDS-ACTION" | "CANCELLED" => (0, String::new()),
        "IN-PROCESS" => (
            if (1..=99).contains(&draft.percent) {
                draft.percent
            } else {
                50
            },
            String::new(),
        ),
        "COMPLETED" => (
            100,
            if draft.completed.input.trim().is_empty() {
                today.format("%Y-%m-%d").to_string()
            } else {
                draft.completed.input.clone()
            },
        ),
        _ => (draft.percent, draft.completed.input.clone()),
    }
}

fn task_recurrence_is_unchanged(existing: &str, recurrence: &str) -> bool {
    ical_property(existing, "RRULE").is_some_and(|value| value.trim() == recurrence.trim())
}

fn validate_task_recurrence(
    existing: &str,
    recurrence: &str,
    has_start: bool,
) -> Result<(), String> {
    let recurrence = recurrence.trim();
    if recurrence.is_empty() {
        return Ok(());
    }
    if !has_start {
        return Err("Wiederkehrende Aufgaben benötigen ein DTSTART".to_string());
    }
    if recurrence
        .chars()
        .any(|character| character == '\r' || character == '\n' || character.is_control())
    {
        return Err("Die Wiederholungsregel enthält ungültige Zeichen".to_string());
    }
    if task_recurrence_is_unchanged(existing, recurrence) {
        return Ok(());
    }

    let mut frequency = None;
    for part in recurrence.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            return Err("Die Wiederholungsregel hat eine ungültige Syntax".to_string());
        };
        if key.trim().is_empty() || value.trim().is_empty() {
            return Err("Die Wiederholungsregel hat eine ungültige Syntax".to_string());
        }
        if key.trim().eq_ignore_ascii_case("FREQ") {
            frequency = Some(value.trim().to_ascii_uppercase());
        }
    }
    let Some(frequency) = frequency else {
        return Err("Die Wiederholungsregel benötigt FREQ".to_string());
    };
    if !matches!(
        frequency.as_str(),
        "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY"
    ) || parse_recurrence_rule(recurrence).is_none()
    {
        return Err("Die Wiederholungsregel ist ungültig".to_string());
    }
    Ok(())
}

fn apply_task_date_mutation(
    component: &str,
    property_name: &str,
    mutation: TaskDateMutation,
) -> String {
    match mutation {
        TaskDateMutation::Preserve => component.to_string(),
        TaskDateMutation::Remove => update_ical_property(component, property_name, None),
        TaskDateMutation::Set(date) => {
            let component = update_ical_property(component, property_name, None);
            update_ical_property(
                &component,
                &format!("{property_name};VALUE=DATE"),
                Some(&date.format("%Y%m%d").to_string()),
            )
        }
    }
}

fn task_date_mutation_changed(mutation: TaskDateMutation) -> bool {
    !matches!(mutation, TaskDateMutation::Preserve)
}

fn task_date_mutation_date(mutation: TaskDateMutation) -> Option<NaiveDate> {
    match mutation {
        TaskDateMutation::Set(date) => Some(date),
        TaskDateMutation::Preserve | TaskDateMutation::Remove => None,
    }
}

/// DTSTART and DUE follow Evolution's GTK3 policy: once both values exist,
/// editing one establishes the value type for the other.  If a field is being
/// added, its type is derived from the existing peer; editing both date-only
/// fields keeps the portable editor's date-only representation.
fn task_date_pair_kind(
    dtstart: &TaskDateDraft,
    due: &TaskDateDraft,
    dtstart_mutation: TaskDateMutation,
    due_mutation: TaskDateMutation,
) -> Option<TaskDateValueKind> {
    let dtstart_changed = task_date_mutation_changed(dtstart_mutation);
    let due_changed = task_date_mutation_changed(due_mutation);
    if !dtstart_changed && !due_changed {
        return None;
    }

    if dtstart_changed && !due_changed {
        return Some(
            dtstart
                .original_kind
                .or(due.original_kind)
                .unwrap_or(TaskDateValueKind::DateOnly),
        );
    }
    if due_changed && !dtstart_changed {
        return Some(
            due.original_kind
                .or(dtstart.original_kind)
                .unwrap_or(TaskDateValueKind::DateOnly),
        );
    }

    match (dtstart.original_kind, due.original_kind) {
        (Some(left), Some(right)) if left == right => Some(left),
        _ => Some(TaskDateValueKind::DateOnly),
    }
}

fn task_date_property_prefix(
    property_name: &str,
    kind: TaskDateValueKind,
    template: Option<&TaskDateDraft>,
) -> String {
    let Some(original_line) = template.and_then(|draft| draft.original_line.as_deref()) else {
        return match kind {
            TaskDateValueKind::DateOnly => format!("{property_name};VALUE=DATE"),
            TaskDateValueKind::DateTime => property_name.to_string(),
        };
    };
    let Some((property, _)) = original_line.split_once(':') else {
        return property_name.to_string();
    };
    let mut parameters = property
        .split(';')
        .skip(1)
        .filter(|parameter| {
            let name = parameter
                .split_once('=')
                .map_or(*parameter, |(name, _)| name);
            !name.eq_ignore_ascii_case("VALUE")
                && !(kind == TaskDateValueKind::DateOnly && name.eq_ignore_ascii_case("TZID"))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    let value_parameter = match kind {
        TaskDateValueKind::DateOnly => Some("VALUE=DATE".to_string()),
        TaskDateValueKind::DateTime => None,
    };
    if let Some(value_parameter) = value_parameter {
        parameters.insert(0, value_parameter);
    }
    if parameters.is_empty() {
        property_name.to_string()
    } else {
        format!("{property_name};{}", parameters.join(";"))
    }
}

fn task_date_datetime_value(date: NaiveDate, template: Option<&TaskDateDraft>) -> String {
    let Some(original_line) = template.and_then(|draft| draft.original_line.as_deref()) else {
        return format!("{}T000000", date.format("%Y%m%d"));
    };
    let Some((_, value)) = original_line.split_once(':') else {
        return format!("{}T000000", date.format("%Y%m%d"));
    };
    let value = value.trim();
    let suffix = if value.len() >= 15 && value.as_bytes().get(8) == Some(&b'T') {
        &value[8..]
    } else {
        "T000000"
    };
    format!("{}{}", date.format("%Y%m%d"), suffix)
}

fn set_task_date_property(
    component: &str,
    property_name: &str,
    date: NaiveDate,
    kind: TaskDateValueKind,
    template: Option<&TaskDateDraft>,
) -> String {
    let prefix = task_date_property_prefix(property_name, kind, template);
    let value = match kind {
        TaskDateValueKind::DateOnly => date.format("%Y%m%d").to_string(),
        TaskDateValueKind::DateTime => task_date_datetime_value(date, template),
    };
    let component = update_ical_property(component, property_name, None);
    update_ical_property(&component, &prefix, Some(&value))
}

fn apply_task_date_pair_mutations(
    existing: &str,
    dtstart: &TaskDateDraft,
    due: &TaskDateDraft,
    dtstart_mutation: TaskDateMutation,
    due_mutation: TaskDateMutation,
) -> String {
    let Some(pair_kind) = task_date_pair_kind(dtstart, due, dtstart_mutation, due_mutation) else {
        return existing.to_string();
    };

    let dtstart_date = task_date_mutation_date(dtstart_mutation).or(dtstart.original_date);
    let due_date = task_date_mutation_date(due_mutation).or(due.original_date);
    let mut component = match dtstart_mutation {
        TaskDateMutation::Remove => update_ical_property(existing, "DTSTART", None),
        TaskDateMutation::Set(date) => {
            set_task_date_property(existing, "DTSTART", date, pair_kind, Some(dtstart))
        }
        TaskDateMutation::Preserve if dtstart.original_kind == Some(pair_kind) => {
            existing.to_string()
        }
        TaskDateMutation::Preserve => dtstart_date.map_or_else(
            || existing.to_string(),
            |date| set_task_date_property(existing, "DTSTART", date, pair_kind, Some(dtstart)),
        ),
    };
    component = match due_mutation {
        TaskDateMutation::Remove => update_ical_property(&component, "DUE", None),
        TaskDateMutation::Set(date) => {
            set_task_date_property(&component, "DUE", date, pair_kind, Some(due))
        }
        TaskDateMutation::Preserve if due.original_kind == Some(pair_kind) => component,
        TaskDateMutation::Preserve => {
            if let Some(date) = due_date {
                set_task_date_property(&component, "DUE", date, pair_kind, Some(due))
            } else {
                component
            }
        }
    };
    component
}

fn task_priority_value(raw: &str) -> u8 {
    raw.trim().parse::<u8>().unwrap_or(0).min(9)
}

fn task_is_completed(task: &TaskItemObject) -> bool {
    task.is_completed() || task_status_value(&task.component()) == "COMPLETED"
}

fn task_quick_edit_values(task: &TaskItemObject) -> TaskQuickEditValues {
    let component = task.component();
    let draft = task_editor_draft_from_component(&component);
    TaskQuickEditValues {
        due: draft.due.input,
        priority: task_priority_value(&task.priority()),
        status: task_status_value(&component),
        percent: parse_task_percent(&component),
        categories: ical_property(&component, "CATEGORIES").unwrap_or_default(),
    }
}

fn update_task_quick_fields(
    task: &TaskItemObject,
    due: &str,
    priority: u8,
    categories: &str,
    status: &str,
    percent: u8,
) -> Result<String, String> {
    let component = task.component();
    let draft = task_editor_draft_from_component(&component);
    let completed = if status == "COMPLETED" && draft.completed.input.trim().is_empty() {
        Local::now().date_naive().format("%Y-%m-%d").to_string()
    } else {
        draft.completed.input.clone()
    };
    update_task_component_with_dates(
        &component,
        &task.title(),
        &draft.dtstart.input,
        due,
        &completed,
        &ical_property(&component, "RRULE").unwrap_or_default(),
        &ical_properties(&component, "RDATE").join(","),
        &ical_properties(&component, "EXDATE").join(","),
        priority,
        categories,
        &parse_alarms(&component),
        &ical_property(&component, "DESCRIPTION").unwrap_or_default(),
        &draft.attachments,
        status,
        percent,
    )
}

#[cfg(test)]
fn normalize_task_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn task_search_haystack(task: &TaskItemObject) -> String {
    let component = task.component();
    let categories = ical_properties(&component, "CATEGORIES").join(" ");
    let description = ical_property(&component, "DESCRIPTION").unwrap_or_default();
    format!(
        "{} {} {} {} {} {} {}",
        task.title(),
        task.due_date(),
        task.priority(),
        task_status_label(&component),
        categories,
        description,
        component,
    )
    .to_lowercase()
}

#[cfg(test)]
fn task_query_matches(task: &TaskItemObject, query: &str) -> bool {
    TaskQuery::parse(TaskSearchMode::AnyField, query).matches(task, current_task_filter_context())
}

impl TasksPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Trigger the same editor as the page-local “Neue Aufgabe” button.
    /// The main Evolution toolbar uses this when the Aufgaben module is
    /// active, so both entry points preserve one creation workflow.
    pub fn present_new_task(&self) {
        if let Some(button) = self.imp().new_task_button.borrow().as_ref() {
            button.activate();
        }
    }

    pub(crate) fn focus_search(&self) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn open_search_result(&self, source_uid: &str, uid: &str) -> bool {
        let (Some(task_list), Some(task_store), Some(empty_tasks)) = (
            self.imp().task_list.borrow().as_ref().cloned(),
            self.imp().task_store.borrow().as_ref().cloned(),
            self.imp().task_empty_state.borrow().as_ref().cloned(),
        ) else {
            return false;
        };
        let Some(task) = (0..task_store.n_items())
            .filter_map(|index| task_store.item(index).and_downcast::<TaskItemObject>())
            .find(|task| task.source_uid() == source_uid && task.id() == uid)
        else {
            return false;
        };
        let mut child = task_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                continue;
            };
            let Some(task_row) = row
                .child()
                .and_then(|child| child.downcast::<adw::ActionRow>().ok())
            else {
                continue;
            };
            let matches = unsafe {
                task_row
                    .data::<String>("task-source-uid")
                    .is_some_and(|value| value.as_ref() == source_uid)
                    && task_row
                        .data::<String>("task-id")
                        .is_some_and(|value| value.as_ref() == uid)
            };
            if matches {
                task_list.select_row(Some(&row));
                if task_source_capability(
                    &source_uid,
                    &self.imp().task_source_capabilities.borrow(),
                    CalendarStoreService::new().is_writable(),
                ) != TaskSourceCapability::Writable
                {
                    self.show_toast("Diese Aufgabenliste ist nicht als schreibfähig bestätigt");
                    return true;
                }
                self.open_task_dialog(task_store, task_list, empty_tasks, Some(task));
                return true;
            }
        }
        false
    }

    /// Re-render category names and colored task rows after the presentation-
    /// only category store changed in Preferences.
    pub fn refresh_category_presentation(&self) {
        self.reload_task_category_metadata();
    }

    fn reload_task_category_metadata(&self) {
        let generation = self
            .imp()
            .task_category_metadata_generation
            .get()
            .wrapping_add(1);
        self.imp().task_category_metadata_generation.set(generation);

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let categories = CalendarCategoryStore::default().load();
            if let Err(error) = sender.send_blocking(categories) {
                tracing::debug!(%error, "Task category metadata was dropped before delivery");
            }
        });

        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(categories) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if page.imp().task_category_metadata_generation.get() != generation {
                return;
            }
            page.imp().task_category_metadata.replace(Some(categories));
            let (Some(task_list), Some(task_store), Some(empty_state)) = (
                page.imp().task_list.borrow().as_ref().cloned(),
                page.imp().task_store.borrow().as_ref().cloned(),
                page.imp().task_empty_state.borrow().as_ref().cloned(),
            ) else {
                return;
            };
            render_task_rows(&page, &task_list, &task_store, &empty_state);
        });
    }

    fn build_ui(&self) {
        if let Some(settings) = AppSettings::try_new() {
            self.imp()
                .task_filter
                .set(TaskFilter::from_setting(&settings.task_filter()));
            self.imp()
                .task_search_mode
                .set(TaskSearchMode::from_setting(&settings.task_search_mode()));
            let source_filter = settings.task_source_filter();
            self.imp()
                .task_source_filter
                .replace((!source_filter.trim().is_empty()).then_some(source_filter));
            let category_filter = settings.task_category_filter();
            self.imp()
                .task_category_filter
                .replace((!category_filter.trim().is_empty()).then_some(category_filter));
            self.imp()
                .task_sort
                .set(TaskSort::from_setting(&settings.task_sort_field()));
            self.imp()
                .task_sort_descending
                .set(settings.task_sort_descending());
        }
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(240.0)
            .max_sidebar_width(240.0)
            .sidebar_width_fraction(0.0)
            .build();

        // ── Sidebar (Categories) ────────────────────────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let header = gtk::Label::builder()
            .label("Aufgabenlisten")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        let source_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .build();
        source_header.append(&header);
        let new_task_source_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Neue lokale Aufgabenliste")
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(&new_task_source_button, "Neue lokale Aufgabenliste");
        let copy_task_source_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Ausgewählte Aufgabenliste kopieren")
            .css_classes(vec!["flat", "circular"])
            .sensitive(false)
            .build();
        set_accessible_label(
            &copy_task_source_button,
            "Ausgewählte Aufgabenliste kopieren",
        );
        let rename_task_source_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Ausgewählte Aufgabenliste umbenennen")
            .css_classes(vec!["flat", "circular"])
            .sensitive(false)
            .build();
        set_accessible_label(
            &rename_task_source_button,
            "Ausgewählte Aufgabenliste umbenennen",
        );
        let properties_task_source_button = gtk::Button::builder()
            .icon_name("document-properties-symbolic")
            .tooltip_text("Eigenschaften der ausgewählten Aufgabenliste")
            .css_classes(vec!["flat", "circular"])
            .sensitive(false)
            .build();
        set_accessible_label(
            &properties_task_source_button,
            "Eigenschaften der ausgewählten Aufgabenliste",
        );
        let delete_task_source_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Ausgewählte Aufgabenliste löschen")
            .css_classes(vec!["flat", "circular", "destructive-action"])
            .sensitive(false)
            .build();
        set_accessible_label(
            &delete_task_source_button,
            "Ausgewählte Aufgabenliste löschen",
        );
        source_header.append(&new_task_source_button);
        sidebar_box.append(&source_header);
        let source_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::End)
            .build();
        source_actions.append(&copy_task_source_button);
        source_actions.append(&rename_task_source_button);
        source_actions.append(&properties_task_source_button);
        source_actions.append(&delete_task_source_button);
        sidebar_box.append(&source_actions);

        let task_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        sidebar_box.append(&task_source_list);
        self.imp()
            .task_source_list
            .replace(Some(task_source_list.clone()));
        self.imp()
            .task_source_new_button
            .replace(Some(new_task_source_button.clone()));
        self.imp()
            .task_source_copy_button
            .replace(Some(copy_task_source_button.clone()));
        self.imp()
            .task_source_rename_button
            .replace(Some(rename_task_source_button.clone()));
        self.imp()
            .task_source_properties_button
            .replace(Some(properties_task_source_button.clone()));
        self.imp()
            .task_source_delete_button
            .replace(Some(delete_task_source_button.clone()));

        let category_header = gtk::Label::builder()
            .label("Ansichten")
            .css_classes(vec!["title-5"])
            .halign(gtk::Align::Start)
            .margin_top(8)
            .margin_bottom(2)
            .build();
        sidebar_box.append(&category_header);

        let cat_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        for (_, icon, label) in TASK_FILTER_OPTIONS {
            let row = adw::ActionRow::builder().title(label).build();
            let img = gtk::Image::from_icon_name(icon);
            row.add_prefix(&img);
            cat_list.append(&row);
        }

        sidebar_box.append(&cat_list);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar_box)
            .vexpand(true)
            .build();

        let drawer_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(vec!["sidebar-drawer-header"])
            .build();
        let drawer_icon = gtk::Image::from_icon_name("checkbox-checked-symbolic");
        let drawer_title = gtk::Label::builder()
            .label("Aufgaben")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();
        sidebar_outer_box.append(&drawer_header);
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "tasks",
            &sidebar_outer_box,
        ));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Aufgaben")
            .child(&sidebar_outer_box)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();

        split.set_sidebar(Some(&sidebar_page));

        // ── Main Content (Task List) ────────────────────────────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let search_mode_model = gtk::StringList::new(&[
            "Alle Felder",
            "Zusammenfassung",
            "Beschreibung",
            "Erweitert",
        ]);
        let search_mode_selector =
            gtk::DropDown::new(Some(search_mode_model), None::<&gtk::Expression>);
        search_mode_selector.set_selected(self.imp().task_search_mode.get().index());
        search_mode_selector.set_tooltip_text(Some(
            "Suchfeld wählen; Erweitert unterstützt summary:, description:, category:, due:, start:, status: und has:attachment",
        ));

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Aufgaben durchsuchen…")
            .hexpand(true)
            .build();
        search_entry.set_tooltip_text(Some(
            "Mehrere Begriffe werden kombiniert. Im Modus Erweitert: category:Work due:next7 has:attachment",
        ));
        self.imp().search_entry.replace(Some(search_entry.clone()));

        let sort_model = gtk::StringList::new(&["Fälligkeit", "Titel", "Priorität"]);
        let sort_selector = gtk::DropDown::new(Some(sort_model), None::<&gtk::Expression>);
        sort_selector.set_tooltip_text(Some("Aufgaben sortieren"));
        sort_selector.set_selected(match self.imp().task_sort.get() {
            TaskSort::Title => 1,
            TaskSort::Priority => 2,
            TaskSort::DueDate => 0,
        });

        let direction_button = gtk::Button::builder()
            .css_classes(vec!["flat", "circular"])
            .build();
        let set_direction_button = |button: &gtk::Button, descending: bool| {
            button.set_icon_name(if descending {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            button.set_tooltip_text(Some(if descending {
                "Absteigend sortiert"
            } else {
                "Aufsteigend sortiert"
            }));
            set_accessible_label(
                button,
                if descending {
                    "Sortierung aufsteigend ändern"
                } else {
                    "Sortierung absteigend ändern"
                },
            );
        };
        set_direction_button(&direction_button, self.imp().task_sort_descending.get());

        let source_model = gtk::StringList::new(&["Alle Aufgabenlisten"]);
        let source_selector =
            gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        source_selector.set_tooltip_text(Some("Aufgabenliste auswählen"));
        self.imp()
            .task_source_model
            .replace(Some(source_model.clone()));
        self.imp()
            .task_source_selector
            .replace(Some(source_selector.clone()));
        let source_selector_for_sidebar = source_selector.clone();
        let split_for_source_sidebar = split.clone();
        task_source_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                source_selector_for_sidebar.set_selected(row.index() as u32);
                split_for_source_sidebar.set_show_content(true);
            }
        });
        let category_model = gtk::StringList::new(&["Alle Kategorien"]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some("Aufgabenkategorie auswählen"));
        self.imp()
            .task_category_model
            .replace(Some(category_model.clone()));
        self.imp()
            .task_category_selector
            .replace(Some(category_selector.clone()));

        let btn_add_task = gtk::Button::builder()
            .label("Neue Aufgabe")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["suggested-action"])
            .build();
        self.imp()
            .new_task_button
            .replace(Some(btn_add_task.clone()));
        let selection_status = gtk::Label::builder()
            .label("Keine Auswahl")
            .css_classes(vec!["dim-label"])
            .build();
        let forward_selection_button = gtk::Button::builder()
            .icon_name("mail-forward-symbolic")
            .tooltip_text("Ausgewählte Aufgabe als iCalendar weiterleiten")
            .sensitive(false)
            .build();
        set_accessible_label(
            &forward_selection_button,
            "Ausgewählte Aufgabe als iCalendar weiterleiten",
        );
        let assign_selection_button = gtk::Button::builder()
            .icon_name("avatar-default-symbolic")
            .tooltip_text("Ausgewählte Aufgabe zuweisen")
            .sensitive(false)
            .build();
        set_accessible_label(&assign_selection_button, "Ausgewählte Aufgabe zuweisen");
        let open_url_selection_button = gtk::Button::builder()
            .icon_name("external-link-symbolic")
            .tooltip_text("URL der ausgewählten Aufgabe öffnen")
            .sensitive(false)
            .build();
        set_accessible_label(
            &open_url_selection_button,
            "URL der ausgewählten Aufgabe öffnen",
        );
        let print_selection_button = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text("Ausgewählte Aufgabe drucken")
            .sensitive(false)
            .build();
        set_accessible_label(&print_selection_button, "Ausgewählte Aufgabe drucken");
        let print_list_button = gtk::Button::builder()
            .label("Liste drucken")
            .icon_name("document-print-symbolic")
            .tooltip_text("Sichtbare Aufgabenliste drucken (Strg+P)")
            .build();
        set_accessible_label(&print_list_button, "Sichtbare Aufgabenliste drucken");
        let print_list_preview_button = gtk::Button::builder()
            .label("Druckvorschau")
            .icon_name("document-print-preview-symbolic")
            .tooltip_text("Druckvorschau der sichtbaren Aufgabenliste vorbereiten")
            .build();
        set_accessible_label(
            &print_list_preview_button,
            "Druckvorschau der sichtbaren Aufgabenliste vorbereiten",
        );
        let refresh_source_button = gtk::Button::builder()
            .label("Aktualisieren")
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Ausgewählte Aufgabenliste beim Anbieter aktualisieren")
            .sensitive(false)
            .build();
        set_accessible_label(
            &refresh_source_button,
            "Ausgewählte Aufgabenliste aktualisieren",
        );
        let copy_selection_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Ausgewählte Aufgaben in die Zwischenablage kopieren")
            .sensitive(false)
            .build();
        set_accessible_label(
            &copy_selection_button,
            "Ausgewählte Aufgaben in die Zwischenablage kopieren",
        );
        let cut_selection_button = gtk::Button::builder()
            .icon_name("edit-cut-symbolic")
            .tooltip_text("Ausgewählte Aufgaben ausschneiden")
            .sensitive(false)
            .build();
        set_accessible_label(&cut_selection_button, "Ausgewählte Aufgaben ausschneiden");
        let paste_selection_button = gtk::Button::builder()
            .icon_name("edit-paste-symbolic")
            .tooltip_text("Aufgaben aus der Zwischenablage einfügen")
            .sensitive(false)
            .build();
        set_accessible_label(
            &paste_selection_button,
            "Aufgaben aus der Zwischenablage einfügen",
        );
        let select_all_tasks_button = gtk::Button::builder()
            .icon_name("edit-select-all-symbolic")
            .tooltip_text("Alle sichtbaren Aufgaben auswählen")
            .build();
        set_accessible_label(
            &select_all_tasks_button,
            "Alle sichtbaren Aufgaben auswählen",
        );
        let bulk_edit_button = gtk::Button::builder()
            .label("Stapel bearbeiten")
            .icon_name("document-edit-symbolic")
            .tooltip_text("Ausgewählte Aufgaben gemeinsam bearbeiten")
            .sensitive(false)
            .build();
        set_accessible_label(
            &bulk_edit_button,
            "Ausgewählte Aufgaben gemeinsam bearbeiten",
        );
        let complete_selection_button = gtk::Button::builder()
            .label("Erledigt")
            .icon_name("emblem-ok-symbolic")
            .tooltip_text("Ausgewählte Aufgaben als erledigt markieren")
            .sensitive(false)
            .build();
        set_accessible_label(
            &complete_selection_button,
            "Ausgewählte Aufgaben als erledigt markieren",
        );
        let incomplete_selection_button = gtk::Button::builder()
            .label("Offen")
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Ausgewählte Aufgaben als offen markieren")
            .sensitive(false)
            .build();
        set_accessible_label(
            &incomplete_selection_button,
            "Ausgewählte Aufgaben als offen markieren",
        );
        let delete_selection_button = gtk::Button::builder()
            .label("Auswahl löschen")
            .icon_name("user-trash-symbolic")
            .tooltip_text("Ausgewählte Aufgaben löschen")
            .css_classes(vec!["destructive-action"])
            .sensitive(false)
            .build();
        set_accessible_label(&delete_selection_button, "Ausgewählte Aufgaben löschen");
        let purge_button = gtk::Button::builder()
            .label("Erledigte löschen")
            .icon_name("edit-clear-all-symbolic")
            .tooltip_text("Alle erledigten Aufgaben dauerhaft löschen (Strg+E)")
            .css_classes(vec!["destructive-action"])
            .sensitive(false)
            .build();
        set_accessible_label(&purge_button, "Alle erledigten Aufgaben dauerhaft löschen");
        let export_selection_button = gtk::Button::builder()
            .label("Auswahl exportieren")
            .icon_name("document-save-symbolic")
            .tooltip_text("Ausgewählte Aufgaben gemeinsam als iCalendar exportieren")
            .sensitive(false)
            .build();
        set_accessible_label(
            &export_selection_button,
            "Ausgewählte Aufgaben gemeinsam als iCalendar exportieren",
        );
        let preview_visible = AppSettings::try_new()
            .map(|settings| settings.task_preview_visible())
            .unwrap_or(true);
        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("view-dual-symbolic")
            .tooltip_text("Aufgabenvorschau ein-/ausblenden (Strg+M)")
            .active(preview_visible)
            .build();
        set_accessible_label(&preview_toggle, "Aufgabenvorschau ein- oder ausblenden");
        let preview_orientation_model =
            gtk::StringList::new(&["Vorschau unten", "Vorschau daneben"]);
        let preview_orientation_selector =
            gtk::DropDown::new(Some(preview_orientation_model), None::<&gtk::Expression>);
        let preview_orientation = AppSettings::try_new()
            .map(|settings| {
                TaskPreviewOrientation::from_setting(&settings.task_preview_orientation())
            })
            .unwrap_or(TaskPreviewOrientation::Classic);
        preview_orientation_selector.set_selected(preview_orientation.index());
        preview_orientation_selector.set_tooltip_text(Some("Anordnung der Aufgabenvorschau"));
        self.imp()
            .task_selection_status
            .replace(Some(selection_status.clone()));
        self.imp()
            .task_forward_button
            .replace(Some(forward_selection_button.clone()));
        self.imp()
            .task_assign_button
            .replace(Some(assign_selection_button.clone()));
        self.imp()
            .task_open_url_button
            .replace(Some(open_url_selection_button.clone()));
        self.imp()
            .task_print_button
            .replace(Some(print_selection_button.clone()));
        self.imp()
            .task_copy_button
            .replace(Some(copy_selection_button.clone()));
        self.imp()
            .task_cut_button
            .replace(Some(cut_selection_button.clone()));
        self.imp()
            .task_paste_button
            .replace(Some(paste_selection_button.clone()));
        self.imp()
            .task_select_all_button
            .replace(Some(select_all_tasks_button.clone()));
        self.imp()
            .task_bulk_edit_button
            .replace(Some(bulk_edit_button.clone()));
        self.imp()
            .task_complete_button
            .replace(Some(complete_selection_button.clone()));
        self.imp()
            .task_incomplete_button
            .replace(Some(incomplete_selection_button.clone()));
        self.imp()
            .task_delete_selection_button
            .replace(Some(delete_selection_button.clone()));
        self.imp()
            .task_purge_button
            .replace(Some(purge_button.clone()));
        self.imp()
            .task_refresh_button
            .replace(Some(refresh_source_button.clone()));
        self.imp()
            .task_export_button
            .replace(Some(export_selection_button.clone()));
        self.imp()
            .task_preview_toggle
            .replace(Some(preview_toggle.clone()));
        self.imp()
            .task_preview_orientation
            .replace(Some(preview_orientation_selector.clone()));

        toolbar.append(&search_mode_selector);
        toolbar.append(&search_entry);
        toolbar.append(&sort_selector);
        toolbar.append(&direction_button);
        toolbar.append(&source_selector);
        toolbar.append(&refresh_source_button);
        toolbar.append(&category_selector);
        toolbar.append(&selection_status);
        toolbar.append(&assign_selection_button);
        toolbar.append(&forward_selection_button);
        toolbar.append(&open_url_selection_button);
        toolbar.append(&print_selection_button);
        toolbar.append(&print_list_button);
        toolbar.append(&print_list_preview_button);
        toolbar.append(&copy_selection_button);
        toolbar.append(&cut_selection_button);
        toolbar.append(&paste_selection_button);
        toolbar.append(&select_all_tasks_button);
        toolbar.append(&bulk_edit_button);
        toolbar.append(&complete_selection_button);
        toolbar.append(&incomplete_selection_button);
        toolbar.append(&delete_selection_button);
        toolbar.append(&purge_button);
        toolbar.append(&export_selection_button);
        toolbar.append(&preview_toggle);
        toolbar.append(&preview_orientation_selector);
        toolbar.append(&btn_add_task);

        content_box.append(&toolbar);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let task_store = gio::ListStore::new::<TaskItemObject>();

        let task_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Multiple)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(vec!["boxed-list"])
            .build();

        let empty_tasks = gtk::Label::builder()
            .label("Aufgaben werden geladen…")
            .css_classes(vec!["dim-label"])
            .margin_top(24)
            .build();
        self.imp().task_list.replace(Some(task_list.clone()));
        self.imp().task_store.replace(Some(task_store.clone()));
        self.imp()
            .task_empty_state
            .replace(Some(empty_tasks.clone()));

        let preview_find_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .visible(false)
            .build();
        let preview_find_entry = gtk::SearchEntry::builder()
            .placeholder_text("In Vorschau suchen")
            .hexpand(true)
            .sensitive(false)
            .build();
        let preview_find_previous = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Vorheriger Treffer")
            .build();
        let preview_find_next = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Nächster Treffer")
            .build();
        let preview_find_status = gtk::Label::new(None);
        let preview_find_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Vorschau-Suche schließen")
            .build();
        preview_find_bar.append(&preview_find_entry);
        preview_find_bar.append(&preview_find_previous);
        preview_find_bar.append(&preview_find_next);
        preview_find_bar.append(&preview_find_status);
        preview_find_bar.append(&preview_find_close);
        let preview_content = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(12)
            .right_margin(12)
            .top_margin(12)
            .bottom_margin(12)
            .build();
        let preview_scroll = gtk::ScrolledWindow::builder()
            .child(&preview_content)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let preview_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["background"])
            .visible(preview_visible)
            .build();
        preview_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        preview_box.append(&preview_find_bar);
        preview_box.append(&preview_scroll);
        self.imp()
            .task_preview_box
            .replace(Some(preview_box.clone()));
        self.imp()
            .task_preview_content
            .replace(Some(preview_content.clone()));
        self.imp()
            .task_preview_find_bar
            .replace(Some(preview_find_bar.clone()));
        self.imp()
            .task_preview_find_entry
            .replace(Some(preview_find_entry.clone()));
        self.imp()
            .task_preview_find_status
            .replace(Some(preview_find_status.clone()));

        let page_weak_for_preview_find = self.downgrade();
        preview_find_entry.connect_search_changed(move |entry| {
            if let Some(page) = page_weak_for_preview_find.upgrade() {
                let text = page.imp().task_preview_text.borrow().clone();
                apply_task_preview_find(
                    &page,
                    task_preview_find_state(&text, entry.text().as_str(), None),
                );
            }
        });
        for (button, forward) in [
            (preview_find_previous.clone(), false),
            (preview_find_next.clone(), true),
        ] {
            let page_weak = self.downgrade();
            button.connect_clicked(move |_| {
                if let Some(page) = page_weak.upgrade() {
                    let state = page.imp().task_preview_find.borrow().clone();
                    apply_task_preview_find(&page, task_preview_find_step(&state, forward));
                }
            });
        }
        let preview_find_entry_for_close = preview_find_entry.clone();
        let preview_find_bar_for_close = preview_find_bar.clone();
        preview_find_close.connect_clicked(move |_| {
            preview_find_entry_for_close.set_text("");
            preview_find_bar_for_close.set_visible(false);
        });

        let page_weak_for_preview_toggle = self.downgrade();
        preview_toggle.connect_toggled(move |toggle| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_preview_visible(toggle.is_active());
            }
            if let Some(page) = page_weak_for_preview_toggle.upgrade() {
                refresh_task_preview(&page);
            }
        });
        let page_weak_for_preview_orientation = self.downgrade();
        preview_orientation_selector.connect_selected_notify(move |selector| {
            let orientation = TaskPreviewOrientation::from_index(selector.selected());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_preview_orientation(orientation.setting_value());
            }
            if let Some(page) = page_weak_for_preview_orientation.upgrade() {
                apply_task_preview_paned_layout(&page, orientation);
            }
        });
        content_box.append(&empty_tasks);

        let page_weak_for_selection = self.downgrade();
        task_list.connect_selected_rows_changed(move |list| {
            let Some(page) = page_weak_for_selection.upgrade() else {
                return;
            };
            if page.imp().task_selection_restoring.get() {
                return;
            }
            page.imp()
                .task_selection
                .replace(task_selection_from_list(list));
            update_task_selection_controls(&page);
        });

        let page_weak_for_copy = self.downgrade();
        let task_store_for_copy = task_store.clone();
        copy_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_copy.upgrade() else {
                return;
            };
            if task_mutation_batch_running(&page) {
                return;
            }
            let selection = page.imp().task_selection.borrow().clone();
            match build_selected_task_export(
                &selection,
                &task_export_candidates(&task_store_for_copy),
            ) {
                Ok(payload) => {
                    button.display().clipboard().set_text(&payload);
                    show_toast_for_widget(button, "Aufgabe(n) in die Zwischenablage kopiert");
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let page_weak_for_cut = self.downgrade();
        let task_store_for_cut = task_store.clone();
        let task_list_for_cut = task_list.clone();
        let empty_tasks_for_cut = empty_tasks.clone();
        cut_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_cut.upgrade() else {
                return;
            };
            if task_mutation_batch_running(&page) {
                return;
            }
            let selection = page.imp().task_selection.borrow().clone();
            let candidates = task_export_candidates(&task_store_for_cut);
            if !task_delete_action_enabled(
                &selection,
                &candidates,
                &page.imp().task_source_capabilities.borrow(),
                CalendarStoreService::new().is_writable(),
                false,
            ) {
                show_toast_for_widget(button, "Ausschneiden ist für diese Auswahl nicht verfügbar");
                return;
            }
            let payload = match build_selected_task_export(&selection, &candidates) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            // GTK's set_text copies this immutable value into the clipboard
            // synchronously.  Only then enter the existing guarded deletion
            // confirmation; a failed copy never removes source tasks.
            button.display().clipboard().set_text(&payload);
            present_task_delete_confirmation(
                &page,
                button,
                &task_store_for_cut,
                &task_list_for_cut,
                &empty_tasks_for_cut,
            );
        });

        let page_weak_for_paste = self.downgrade();
        let task_store_for_paste = task_store.clone();
        let task_list_for_paste = task_list.clone();
        let empty_tasks_for_paste = empty_tasks.clone();
        paste_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_paste.upgrade() else {
                return;
            };
            start_task_clipboard_paste(
                &page,
                button,
                &task_store_for_paste,
                &task_list_for_paste,
                &empty_tasks_for_paste,
            );
        });

        let task_list_for_select_all = task_list.clone();
        select_all_tasks_button.connect_clicked(move |_| task_list_for_select_all.select_all());

        let copy_for_keys = copy_selection_button.clone();
        let cut_for_keys = cut_selection_button.clone();
        let paste_for_keys = paste_selection_button.clone();
        let select_all_for_keys = select_all_tasks_button.clone();
        let bulk_edit_for_keys = bulk_edit_button.clone();
        let print_list_for_keys = print_list_button.clone();
        let purge_for_keys = purge_button.clone();
        let preview_toggle_for_keys = preview_toggle.clone();
        let preview_find_bar_for_keys = preview_find_bar.clone();
        let preview_find_entry_for_keys = preview_find_entry.clone();
        let clipboard_keys = gtk::EventControllerKey::new();
        clipboard_keys.connect_key_pressed(move |_, key, _, modifiers| {
            if !modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            if matches!(key, gtk::gdk::Key::f | gtk::gdk::Key::F)
                && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK)
            {
                // This controller belongs only to the task list.  Require an
                // exact current single selection before exposing preview find.
                if preview_toggle_for_keys.is_active() && preview_find_entry_for_keys.is_sensitive()
                {
                    preview_find_bar_for_keys.set_visible(true);
                    preview_find_entry_for_keys.grab_focus();
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
            if matches!(key, gtk::gdk::Key::m | gtk::gdk::Key::M) {
                preview_toggle_for_keys.set_active(!preview_toggle_for_keys.is_active());
                return glib::Propagation::Stop;
            }
            if matches!(key, gtk::gdk::Key::p | gtk::gdk::Key::P)
                && !modifiers.intersects(
                    gtk::gdk::ModifierType::SHIFT_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK
                        | gtk::gdk::ModifierType::META_MASK,
                )
            {
                print_list_for_keys.activate();
                return glib::Propagation::Stop;
            }
            let action = match key {
                gtk::gdk::Key::c | gtk::gdk::Key::C => Some(&copy_for_keys),
                gtk::gdk::Key::x | gtk::gdk::Key::X => Some(&cut_for_keys),
                gtk::gdk::Key::v | gtk::gdk::Key::V => Some(&paste_for_keys),
                gtk::gdk::Key::a | gtk::gdk::Key::A => Some(&select_all_for_keys),
                gtk::gdk::Key::b | gtk::gdk::Key::B => Some(&bulk_edit_for_keys),
                gtk::gdk::Key::e | gtk::gdk::Key::E => Some(&purge_for_keys),
                _ => None,
            };
            if let Some(action) = action.filter(|action| action.is_sensitive()) {
                action.activate();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        // This controller is attached only to the task list, so editable
        // search and task-editor widgets retain their standard shortcuts.
        task_list.add_controller(clipboard_keys);

        let page_weak_for_bulk_edit = self.downgrade();
        let task_store_for_bulk_edit = task_store.clone();
        let task_list_for_bulk_edit = task_list.clone();
        let empty_tasks_for_bulk_edit = empty_tasks.clone();
        bulk_edit_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_bulk_edit.upgrade() else {
                return;
            };
            present_task_bulk_edit_dialog(
                &page,
                button,
                &task_store_for_bulk_edit,
                &task_list_for_bulk_edit,
                &empty_tasks_for_bulk_edit,
            );
        });

        let page_weak_for_clipboard_formats = self.downgrade();
        paste_selection_button
            .display()
            .clipboard()
            .connect_changed(move |_| {
                if let Some(page) = page_weak_for_clipboard_formats.upgrade() {
                    update_task_selection_controls(&page);
                }
            });

        let page_weak_for_export = self.downgrade();
        let task_store_for_export = task_store.clone();
        export_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_export.upgrade() else {
                return;
            };
            let selection = page.imp().task_selection.borrow().clone();
            let candidates = task_export_candidates(&task_store_for_export);
            // Validate every identity and component before allowing the file
            // chooser to expose an existing target.
            let payload = match build_selected_task_export(&selection, &candidates) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<gtk::Window>() else {
                return;
            };
            let file_dialog = gtk::FileDialog::builder()
                .title("Aufgabenauswahl als iCalendar speichern")
                .initial_name("aufgaben.ics")
                .modal(true)
                .build();
            let button_for_result = button.clone();
            file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else {
                    return;
                };
                // GIO's replace operation is atomic: a failed replacement
                // leaves an existing destination intact.
                let save_result = file.replace_contents(
                    payload.as_bytes(),
                    None,
                    true,
                    gio::FileCreateFlags::NONE,
                    None::<&gio::Cancellable>,
                );
                match save_result {
                    Ok(_) => show_toast_for_widget(
                        &button_for_result,
                        "Aufgabenauswahl als iCalendar exportiert",
                    ),
                    Err(error) => show_toast_for_widget(
                        &button_for_result,
                        &format!("iCalendar konnte nicht gespeichert werden: {error}"),
                    ),
                }
            });
        });

        let page_weak_for_forward = self.downgrade();
        let task_store_for_forward = task_store.clone();
        forward_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_forward.upgrade() else {
                return;
            };
            let selection = page.imp().task_selection.borrow().clone();
            match build_task_forward_payload(
                &selection,
                &task_export_candidates(&task_store_for_forward),
            ) {
                Ok(payload) => {
                    button.display().clipboard().set_text(&payload);
                    show_toast_for_widget(
                        button,
                        "METHOD:PUBLISH bereitgestellt; echtes iTIP-Weiterleiten ist Linux-Handoff",
                    );
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let page_weak_for_assign = self.downgrade();
        let task_store_for_assign = task_store.clone();
        let task_list_for_assign = task_list.clone();
        let empty_tasks_for_assign = empty_tasks.clone();
        assign_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_assign.upgrade() else {
                return;
            };
            present_task_assignment_dialog(
                &page,
                button,
                &task_store_for_assign,
                &task_list_for_assign,
                &empty_tasks_for_assign,
            );
        });

        let page_weak_for_open_url = self.downgrade();
        let task_store_for_open_url = task_store.clone();
        open_url_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_open_url.upgrade() else {
                return;
            };
            let selection = page.imp().task_selection.borrow().clone();
            let url = match selected_task_open_url(
                &selection,
                &task_export_candidates(&task_store_for_open_url),
            ) {
                Ok(url) => url,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let button_for_result = button.clone();
            gio::AppInfo::launch_default_for_uri_async(
                &url,
                None::<&gio::AppLaunchContext>,
                None::<&gio::Cancellable>,
                move |result| match result {
                    Ok(()) => show_toast_for_widget(&button_for_result, "URL geöffnet"),
                    Err(error) => show_toast_for_widget(
                        &button_for_result,
                        &format!("URL konnte nicht geöffnet werden: {error}"),
                    ),
                },
            );
        });

        let page_weak_for_print = self.downgrade();
        let task_store_for_print = task_store.clone();
        print_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_print.upgrade() else {
                return;
            };
            show_toast_for_widget(button, "Druckdokument wird vorbereitet …");
            let selection = page.imp().task_selection.borrow().clone();
            match build_task_print_document(
                &selection,
                &task_export_candidates(&task_store_for_print),
            ) {
                Ok(document) => show_toast_for_widget(
                    button,
                    &format!(
                        "Druckdokument vorbereitet ({} Zeichen); GTK-Druckdialog/Portal ist Linux-Handoff",
                        document.plain_text.chars().count()
                    ),
                ),
                Err(error) => show_toast_for_widget(
                    button,
                    &format!("Druckdokument konnte nicht vorbereitet werden: {error}"),
                ),
            }
        });

        let page_weak_for_list_print = self.downgrade();
        let task_store_for_list_print = task_store.clone();
        let task_list_for_list_print = task_list.clone();
        print_list_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_list_print.upgrade() {
                prepare_task_list_print(
                    &page,
                    button,
                    &task_list_for_list_print,
                    &task_store_for_list_print,
                    TaskListPrintAction::Print,
                );
            }
        });

        let page_weak_for_list_preview = self.downgrade();
        let task_store_for_list_preview = task_store.clone();
        let task_list_for_list_preview = task_list.clone();
        print_list_preview_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_list_preview.upgrade() {
                prepare_task_list_print(
                    &page,
                    button,
                    &task_list_for_list_preview,
                    &task_store_for_list_preview,
                    TaskListPrintAction::Preview,
                );
            }
        });

        let page_weak_for_complete = self.downgrade();
        let task_store_for_complete = task_store.clone();
        let task_list_for_complete = task_list.clone();
        let empty_tasks_for_complete = empty_tasks.clone();
        complete_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_complete.upgrade() else {
                return;
            };
            start_task_lifecycle_batch(
                &page,
                button,
                &task_store_for_complete,
                &task_list_for_complete,
                &empty_tasks_for_complete,
                TaskLifecycleBatchOperation::Complete,
            );
        });

        let page_weak_for_incomplete = self.downgrade();
        let task_store_for_incomplete = task_store.clone();
        let task_list_for_incomplete = task_list.clone();
        let empty_tasks_for_incomplete = empty_tasks.clone();
        incomplete_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_incomplete.upgrade() else {
                return;
            };
            start_task_lifecycle_batch(
                &page,
                button,
                &task_store_for_incomplete,
                &task_list_for_incomplete,
                &empty_tasks_for_incomplete,
                TaskLifecycleBatchOperation::Incomplete,
            );
        });

        let page_weak_for_delete_selection = self.downgrade();
        let task_store_for_delete_selection = task_store.clone();
        let task_list_for_delete_selection = task_list.clone();
        let empty_tasks_for_delete_selection = empty_tasks.clone();
        delete_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_delete_selection.upgrade() else {
                return;
            };
            present_task_delete_confirmation(
                &page,
                button,
                &task_store_for_delete_selection,
                &task_list_for_delete_selection,
                &empty_tasks_for_delete_selection,
            );
        });

        let page_weak_for_purge = self.downgrade();
        let task_store_for_purge = task_store.clone();
        let task_list_for_purge = task_list.clone();
        let empty_tasks_for_purge = empty_tasks.clone();
        purge_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_purge.upgrade() else {
                return;
            };
            present_task_purge_confirmation(
                &page,
                button,
                &task_store_for_purge,
                &task_list_for_purge,
                &empty_tasks_for_purge,
            );
        });

        let page_weak_for_refresh = self.downgrade();
        refresh_source_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_refresh.upgrade() {
                start_task_source_refresh(&page, button);
            }
        });

        let page_weak_for_source_rename = self.downgrade();
        rename_task_source_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_source_rename.upgrade() {
                present_task_source_rename(&page, button);
            }
        });

        let page_weak_for_source_new = self.downgrade();
        new_task_source_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_source_new.upgrade() {
                present_new_local_task_source(&page, button);
            }
        });

        let page_weak_for_source_properties = self.downgrade();
        properties_task_source_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_source_properties.upgrade() {
                present_task_source_properties(&page, button);
            }
        });

        let page_weak_for_source_copy = self.downgrade();
        copy_task_source_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_source_copy.upgrade() {
                present_task_source_copy(&page, button);
            }
        });

        let page_weak_for_source_delete = self.downgrade();
        delete_task_source_button.connect_clicked(move |button| {
            if let Some(page) = page_weak_for_source_delete.upgrade() {
                present_task_source_delete_confirmation(&page, button);
            }
        });

        let page_weak_for_activation = self.downgrade();
        let task_store_for_activation = task_store.clone();
        let task_list_for_activation = task_list.clone();
        let empty_tasks_for_activation = empty_tasks.clone();
        task_list.connect_row_activated(move |_, row| {
            let Some(task_row) = row
                .child()
                .and_then(|child| child.downcast::<adw::ActionRow>().ok())
            else {
                return;
            };
            let read_data = |key: &str| unsafe {
                task_row
                    .data::<String>(key)
                    .map(|value| value.as_ref().clone())
            };
            let source_uid = read_data("task-source-uid").unwrap_or_default();
            let task_uid = read_data("task-id").unwrap_or_default();
            let Some(task) = (0..task_store_for_activation.n_items())
                .filter_map(|index| {
                    task_store_for_activation
                        .item(index)
                        .and_downcast::<TaskItemObject>()
                })
                .find(|task| task.source_uid() == source_uid && task.id() == task_uid)
            else {
                return;
            };
            let Some(page) = page_weak_for_activation.upgrade() else {
                return;
            };
            if task_source_capability(
                &source_uid,
                &page.imp().task_source_capabilities.borrow(),
                CalendarStoreService::new().is_writable(),
            ) != TaskSourceCapability::Writable
            {
                page.show_toast("Diese Aufgabenliste ist nicht als schreibfähig bestätigt");
                return;
            }
            page.open_task_dialog(
                task_store_for_activation.clone(),
                task_list_for_activation.clone(),
                empty_tasks_for_activation.clone(),
                Some(task),
            );
        });

        let page_weak_for_search = self.downgrade();
        let task_list_for_search = task_list.clone();
        let task_store_for_search = task_store.clone();
        let empty_tasks_for_search = empty_tasks.clone();
        search_entry.connect_search_changed(move |entry| {
            let Some(page) = page_weak_for_search.upgrade() else {
                return;
            };
            page.imp().task_query.replace(entry.text().to_string());
            render_task_rows(
                &page,
                &task_list_for_search,
                &task_store_for_search,
                &empty_tasks_for_search,
            );
        });

        let page_weak_for_search_mode = self.downgrade();
        let task_list_for_search_mode = task_list.clone();
        let task_store_for_search_mode = task_store.clone();
        let empty_tasks_for_search_mode = empty_tasks.clone();
        search_mode_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_search_mode.upgrade() else {
                return;
            };
            let mode = TaskSearchMode::from_index(selector.selected());
            page.imp().task_search_mode.set(mode);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_search_mode(mode.setting_value());
            }
            render_task_rows(
                &page,
                &task_list_for_search_mode,
                &task_store_for_search_mode,
                &empty_tasks_for_search_mode,
            );
        });

        let page_weak_for_sort = self.downgrade();
        let task_list_for_sort = task_list.clone();
        let task_store_for_sort = task_store.clone();
        let empty_tasks_for_sort = empty_tasks.clone();
        sort_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_sort.upgrade() else {
                return;
            };
            page.imp().task_sort.set(match selector.selected() {
                1 => TaskSort::Title,
                2 => TaskSort::Priority,
                _ => TaskSort::DueDate,
            });
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_sort_field(page.imp().task_sort.get().setting_value());
            }
            render_task_rows(
                &page,
                &task_list_for_sort,
                &task_store_for_sort,
                &empty_tasks_for_sort,
            );
        });

        let page_weak_for_direction = self.downgrade();
        let direction_button_for_signal = direction_button.clone();
        let task_list_for_direction = task_list.clone();
        let task_store_for_direction = task_store.clone();
        let empty_tasks_for_direction = empty_tasks.clone();
        direction_button.connect_clicked(move |_| {
            let Some(page) = page_weak_for_direction.upgrade() else {
                return;
            };
            let descending = !page.imp().task_sort_descending.get();
            page.imp().task_sort_descending.set(descending);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_sort_descending(descending);
            }
            set_direction_button(&direction_button_for_signal, descending);
            render_task_rows(
                &page,
                &task_list_for_direction,
                &task_store_for_direction,
                &empty_tasks_for_direction,
            );
        });

        let page_weak_for_source = self.downgrade();
        let task_source_list_for_selector = task_source_list.clone();
        let task_list_for_source = task_list.clone();
        let task_store_for_source = task_store.clone();
        let empty_tasks_for_source = empty_tasks.clone();
        source_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_source.upgrade() else {
                return;
            };
            let source_filter = if selector.selected() == 0 {
                None
            } else {
                page.imp()
                    .task_source_uids
                    .borrow()
                    .get(selector.selected() as usize - 1)
                    .cloned()
            };
            page.imp().task_source_filter.replace(source_filter.clone());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_source_filter(source_filter.as_deref().unwrap_or_default());
            }
            if let Some(row) =
                task_source_list_for_selector.row_at_index(selector.selected() as i32)
            {
                task_source_list_for_selector.select_row(Some(&row));
            }
            render_task_rows(
                &page,
                &task_list_for_source,
                &task_store_for_source,
                &empty_tasks_for_source,
            );
        });

        let page_weak_for_category = self.downgrade();
        let task_list_for_category = task_list.clone();
        let task_store_for_category = task_store.clone();
        let empty_tasks_for_category = empty_tasks.clone();
        let category_model_for_selector = category_model.clone();
        let split_for_category_selection = split.clone();
        category_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_category.upgrade() else {
                return;
            };
            let category = if selector.selected() == 0 {
                None
            } else {
                category_model_for_selector
                    .string(selector.selected())
                    .map(|value| value.to_string())
            };
            page.imp().task_category_filter.replace(category);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_category_filter(
                    page.imp()
                        .task_category_filter
                        .borrow()
                        .as_deref()
                        .unwrap_or_default(),
                );
            }
            render_task_rows(
                &page,
                &task_list_for_category,
                &task_store_for_category,
                &empty_tasks_for_category,
            );
            split_for_category_selection.set_show_content(true);
        });

        let page_weak_for_categories = self.downgrade();
        let task_list_for_categories = task_list.clone();
        let task_store_for_categories = task_store.clone();
        let empty_tasks_for_categories = empty_tasks.clone();
        let split_for_category_sidebar = split.clone();
        cat_list.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                return;
            };
            let Some(page) = page_weak_for_categories.upgrade() else {
                return;
            };
            let filter = task_filter_from_index(row.index());
            page.imp().task_filter.set(filter);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_filter(filter.setting_value());
            }
            render_task_rows(
                &page,
                &task_list_for_categories,
                &task_store_for_categories,
                &empty_tasks_for_categories,
            );
            split_for_category_sidebar.set_show_content(true);
        });
        let initial_filter_index = task_filter_index(self.imp().task_filter.get());
        if let Some(row) = cat_list.row_at_index(initial_filter_index) {
            cat_list.select_row(Some(&row));
        }

        if let Some(settings) = AppSettings::try_new() {
            let page_weak = self.downgrade();
            let cat_list_for_settings = cat_list.clone();
            let task_list_for_settings = task_list.clone();
            let task_store_for_settings = task_store.clone();
            let empty_tasks_for_settings = empty_tasks.clone();
            settings.connect_changed(Some("task-filter"), move |settings, _| {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let filter = TaskFilter::from_setting(settings.string("task-filter").as_str());
                page.imp().task_filter.set(filter);
                let index = task_filter_index(filter);
                if let Some(row) = cat_list_for_settings.row_at_index(index) {
                    cat_list_for_settings.select_row(Some(&row));
                }
                render_task_rows(
                    &page,
                    &task_list_for_settings,
                    &task_store_for_settings,
                    &empty_tasks_for_settings,
                );
            });

            let page_weak = self.downgrade();
            let task_list_for_category_settings = task_list.clone();
            let task_store_for_category_settings = task_store.clone();
            let empty_tasks_for_category_settings = empty_tasks.clone();
            settings.connect_changed(Some("task-category-filter"), move |settings, _| {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let raw_value = settings.string("task-category-filter");
                let value = normalized_task_category_filter(&raw_value);
                page.imp()
                    .task_category_filter
                    .replace((!value.trim().is_empty()).then_some(value));
                render_task_rows(
                    &page,
                    &task_list_for_category_settings,
                    &task_store_for_category_settings,
                    &empty_tasks_for_category_settings,
                );
            });

            let page_weak = self.downgrade();
            let task_list_for_search_mode_settings = task_list.clone();
            let task_store_for_search_mode_settings = task_store.clone();
            let empty_tasks_for_search_mode_settings = empty_tasks.clone();
            let search_mode_selector_for_settings = search_mode_selector.clone();
            settings.connect_changed(Some("task-search-mode"), move |settings, _| {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let mode =
                    TaskSearchMode::from_setting(settings.string("task-search-mode").as_str());
                page.imp().task_search_mode.set(mode);
                search_mode_selector_for_settings.set_selected(mode.index());
                render_task_rows(
                    &page,
                    &task_list_for_search_mode_settings,
                    &task_store_for_search_mode_settings,
                    &empty_tasks_for_search_mode_settings,
                );
            });

            let page_weak = self.downgrade();
            let task_list_for_source_settings = task_list.clone();
            let task_store_for_source_settings = task_store.clone();
            let empty_tasks_for_source_settings = empty_tasks.clone();
            let source_selector_for_settings = source_selector.clone();
            settings.connect_changed(Some("task-source-filter"), move |settings, _| {
                let value = settings.string("task-source-filter");
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                let selected = if value.trim().is_empty() {
                    Some(0)
                } else {
                    page.imp()
                        .task_source_uids
                        .borrow()
                        .iter()
                        .position(|uid| uid == value.as_str())
                        .map(|index| index as u32 + 1)
                };
                page.imp()
                    .task_source_filter
                    .replace(if value.trim().is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    });
                if let Some(selected) = selected {
                    source_selector_for_settings.set_selected(selected);
                }
                render_task_rows(
                    &page,
                    &task_list_for_source_settings,
                    &task_store_for_source_settings,
                    &empty_tasks_for_source_settings,
                );
            });

            let page_weak = self.downgrade();
            let task_list_for_sort_settings = task_list.clone();
            let task_store_for_sort_settings = task_store.clone();
            let empty_tasks_for_sort_settings = empty_tasks.clone();
            let sort_selector_for_settings = sort_selector.clone();
            settings.connect_changed(Some("task-sort-field"), move |settings, _| {
                let selected = match settings.string("task-sort-field").as_str() {
                    "title" => 1,
                    "priority" => 2,
                    _ => 0,
                };
                if let Some(page) = page_weak.upgrade() {
                    page.imp().task_sort.set(match selected {
                        1 => TaskSort::Title,
                        2 => TaskSort::Priority,
                        _ => TaskSort::DueDate,
                    });
                }
                sort_selector_for_settings.set_selected(selected);
                if let Some(page) = page_weak.upgrade() {
                    render_task_rows(
                        &page,
                        &task_list_for_sort_settings,
                        &task_store_for_sort_settings,
                        &empty_tasks_for_sort_settings,
                    );
                }
            });

            let page_weak = self.downgrade();
            let task_list_for_direction_settings = task_list.clone();
            let task_store_for_direction_settings = task_store.clone();
            let empty_tasks_for_direction_settings = empty_tasks.clone();
            let direction_button_for_settings = direction_button.clone();
            settings.connect_changed(Some("task-sort-descending"), move |settings, _| {
                let descending = settings.boolean("task-sort-descending");
                if let Some(page) = page_weak.upgrade() {
                    page.imp().task_sort_descending.set(descending);
                }
                direction_button_for_settings.set_icon_name(if descending {
                    "view-sort-descending-symbolic"
                } else {
                    "view-sort-ascending-symbolic"
                });
                direction_button_for_settings.set_tooltip_text(Some(if descending {
                    "Absteigend sortiert"
                } else {
                    "Aufsteigend sortiert"
                }));
                set_accessible_label(
                    &direction_button_for_settings,
                    if descending {
                        "Sortierung aufsteigend ändern"
                    } else {
                        "Sortierung absteigend ändern"
                    },
                );
                if let Some(page) = page_weak.upgrade() {
                    render_task_rows(
                        &page,
                        &task_list_for_direction_settings,
                        &task_store_for_direction_settings,
                        &empty_tasks_for_direction_settings,
                    );
                }
            });

            let page_weak = self.downgrade();
            let preview_toggle_for_settings = preview_toggle.clone();
            settings.connect_changed(Some("task-preview-visible"), move |settings, _| {
                let visible = settings.boolean("task-preview-visible");
                preview_toggle_for_settings.set_active(visible);
                if let Some(page) = page_weak.upgrade() {
                    refresh_task_preview(&page);
                }
            });

            let page_weak = self.downgrade();
            let preview_orientation_for_settings = preview_orientation_selector.clone();
            settings.connect_changed(Some("task-preview-orientation"), move |settings, _| {
                let orientation = TaskPreviewOrientation::from_setting(
                    settings.string("task-preview-orientation").as_str(),
                );
                preview_orientation_for_settings.set_selected(orientation.index());
                if let Some(page) = page_weak.upgrade() {
                    apply_task_preview_paned_layout(&page, orientation);
                }
            });

            for key in ["task-hpane-position", "task-vpane-position"] {
                let page_weak = self.downgrade();
                settings.connect_changed(Some(key), move |settings, _| {
                    let orientation = TaskPreviewOrientation::from_setting(
                        settings.string("task-preview-orientation").as_str(),
                    );
                    if let Some(page) = page_weak.upgrade() {
                        apply_task_preview_paned_layout(&page, orientation);
                    }
                });
            }
        }

        let page_weak = self.downgrade();
        let task_store_for_dialog = task_store.clone();
        let task_list_for_dialog = task_list.clone();
        let empty_tasks_for_dialog = empty_tasks.clone();
        btn_add_task.connect_clicked(move |_| {
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            page.open_task_dialog(
                task_store_for_dialog.clone(),
                task_list_for_dialog.clone(),
                empty_tasks_for_dialog.clone(),
                None,
            );
        });

        let task_store_for_load = task_store.clone();
        let task_list_for_load = task_list.clone();
        let empty_tasks_for_load = empty_tasks.clone();
        let page_weak_for_load = self.downgrade();
        let load_generation = self.imp().task_refresh_generation.get().wrapping_add(1);
        self.imp().task_refresh_generation.set(load_generation);
        let (task_sender, task_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = load_snapshot_prefer_eds();
            if let Err(error) = task_sender.send_blocking(result) {
                tracing::debug!(%error, "Task snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match task_receiver.recv().await {
                Ok(result) => {
                    let Some(page) = page_weak_for_load.upgrade() else {
                        return;
                    };
                    if page.imp().task_refresh_generation.get() != load_generation {
                        return;
                    }
                    if let Some(warning) = result.warning {
                        tracing::warn!(%warning, "Task data loaded with a backend warning");
                    }
                    if result.snapshot.tasks.is_empty() {
                        empty_tasks_for_load.set_label(match result.origin {
                            CalendarDataOrigin::Eds => "Keine synchronisierten Aufgaben geladen",
                            CalendarDataOrigin::Graph => {
                                "Keine Aufgaben aus Microsoft Graph geladen"
                            }
                            CalendarDataOrigin::Local => "Keine lokalen Aufgaben geladen",
                        });
                        empty_tasks_for_load.set_visible(true);
                    } else {
                        empty_tasks_for_load.set_visible(false);
                        for task in result.snapshot.tasks {
                            upsert_task_store(&task_list_for_load, &task_store_for_load, task);
                        }
                        render_task_rows(
                            &page,
                            &task_list_for_load,
                            &task_store_for_load,
                            &empty_tasks_for_load,
                        );
                    }
                }
                Err(error) => {
                    let Some(page) = page_weak_for_load.upgrade() else {
                        return;
                    };
                    if page.imp().task_refresh_generation.get() != load_generation {
                        return;
                    }
                    empty_tasks_for_load.set_label("Aufgaben konnten nicht geladen werden");
                    empty_tasks_for_load.set_visible(true);
                    tracing::debug!(%error, "Task snapshot unavailable");
                }
            }
        });

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let task_store_for_monitor = task_store.clone();
            let task_list_for_monitor = task_list.clone();
            let empty_tasks_for_monitor = empty_tasks.clone();
            let page_weak_for_monitor = self.downgrade();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        ECalSyncEvent::Ready(status) => {
                            if let Some(warning) = status.warning {
                                tracing::warn!(
                                    configured = status.configured_sources,
                                    active = status.active_sources,
                                    failed = status.failed_sources,
                                    %warning,
                                    "ECal task signal monitor started with source warnings"
                                );
                            }
                        }
                        ECalSyncEvent::Change(change) => match change.kind {
                            ECalChangeKind::Removed => {
                                let Some(page) = page_weak_for_monitor.upgrade() else {
                                    break;
                                };
                                remove_task_record(
                                    &page,
                                    &task_store_for_monitor,
                                    &task_list_for_monitor,
                                    &empty_tasks_for_monitor,
                                    &change.source_uid,
                                    &change.uid,
                                );
                            }
                            ECalChangeKind::Added | ECalChangeKind::Modified => {
                                let Some(page) = page_weak_for_monitor.upgrade() else {
                                    break;
                                };
                                if let Some(ECalRecord::Task(task)) = parse_change_record(&change) {
                                    upsert_task_row(
                                        &page,
                                        &task_list_for_monitor,
                                        &task_store_for_monitor,
                                        &empty_tasks_for_monitor,
                                        task,
                                    );
                                }
                            }
                        },
                        ECalSyncEvent::Error(error) => {
                            tracing::warn!(%error, "ECal task signal monitor stopped");
                            break;
                        }
                    }
                }
            });
        }

        let task_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&task_list)
            .vexpand(true)
            .build();
        let preview_paned = gtk::Paned::builder()
            .orientation(preview_orientation.paned_orientation())
            .wide_handle(true)
            .vexpand(true)
            .build();
        preview_paned.set_start_child(Some(&task_scroll));
        preview_paned.set_end_child(Some(&preview_box));
        preview_paned.set_resize_start_child(true);
        preview_paned.set_resize_end_child(true);
        preview_paned.set_position(
            AppSettings::try_new()
                .map(|settings| settings.task_preview_position(preview_orientation.setting_value()))
                .unwrap_or_else(|| preview_orientation.default_position()),
        );
        self.imp()
            .task_preview_paned
            .replace(Some(preview_paned.clone()));
        let page_weak_for_preview_position = self.downgrade();
        preview_paned.connect_position_notify(move |paned| {
            let Some(page) = page_weak_for_preview_position.upgrade() else {
                return;
            };
            if page.imp().task_preview_position_restoring.get() {
                return;
            }
            let orientation = if paned.orientation() == gtk::Orientation::Horizontal {
                TaskPreviewOrientation::Vertical
            } else {
                TaskPreviewOrientation::Classic
            };
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_preview_position(orientation.setting_value(), paned.position());
            }
        });
        content_box.append(&preview_paned);

        let content_page = adw::NavigationPage::builder()
            .title("Aufgaben-Übersicht")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        self.reload_task_category_metadata();
        let split_for_navigation = split.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            let is_back = key == gtk::gdk::Key::Escape
                || (key == gtk::gdk::Key::Left
                    && modifiers.contains(gtk::gdk::ModifierType::ALT_MASK));
            if is_back
                && split_for_navigation.is_collapsed()
                && split_for_navigation.shows_content()
            {
                split_for_navigation.set_show_content(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        split.add_controller(key_controller);
        split.set_parent(self);
        start_task_source_discovery(self, self.imp().task_source_filter.borrow().clone(), false);
    }

    fn open_task_dialog(
        &self,
        task_store: gio::ListStore,
        task_list: gtk::ListBox,
        empty_tasks: gtk::Label,
        task_to_edit: Option<TaskItemObject>,
    ) {
        let editing = task_to_edit.is_some();
        let initial_component = task_to_edit
            .as_ref()
            .map(TaskItemObject::component)
            .unwrap_or_default();
        let initial_draft = task_editor_draft_from_component(&initial_component);
        let dirty = Rc::new(Cell::new(false));
        let dialog_state = Rc::new(Cell::new(TaskEditorDialogState::Clean));
        let save_control = TaskSaveControl::new();
        let request_generation = Rc::new(Cell::new(0_u64));
        let close_confirmed = Rc::new(Cell::new(false));
        let close_prompt_open = Rc::new(Cell::new(false));
        let source_capabilities =
            Rc::new(RefCell::new(HashMap::<String, TaskSourceCapability>::new()));

        let dialog_title = if editing {
            "Aufgabe bearbeiten"
        } else {
            "Neue Aufgabe"
        };
        let dialog = adw::Window::builder()
            .title(dialog_title)
            .modal(true)
            .default_width(540)
            .default_height(520)
            .build();
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&window));
            }
        }

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let cancel_button = gtk::Button::with_label("Abbrechen");
        header_bar.pack_start(&cancel_button);
        let save_button = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&save_button);
        toolbar_view.add_top_bar(&header_bar);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let source_model = gtk::StringList::new(&["Aufgabenlisten werden geladen…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Aufgabenliste")
            .model(&source_model)
            .build();
        source_row.set_sensitive(!editing);
        let settings_for_default_task = AppSettings::try_new();
        let source_uids_for_selection = Rc::clone(&source_uids);
        let source_capabilities_for_selection = Rc::clone(&source_capabilities);
        let save_button_for_selection = save_button.clone();
        source_row.connect_selected_notify(move |row| {
            let Some(source_uid) = source_uids_for_selection
                .borrow()
                .get(row.selected() as usize)
                .cloned()
            else {
                return;
            };
            let writable = task_source_is_writable(&source_uid, &source_capabilities_for_selection);
            if writable {
                if let Some(settings) = settings_for_default_task.as_ref() {
                    settings.set_default_task_source_uid(&source_uid);
                }
            }
            if !editing {
                save_button_for_selection.set_sensitive(writable);
            }
        });
        let details = adw::PreferencesGroup::builder().title("Details").build();
        details.add(&source_row);

        let title_entry = adw::EntryRow::builder().title("Titel").build();
        details.add(&title_entry);
        let dtstart_entry = adw::EntryRow::builder()
            .title("Beginn (YYYY-MM-DD, optional)")
            .build();
        dtstart_entry.set_tooltip_text(Some(
            "Neue Werte sind date-only. Importierte date-time-Werte bleiben unverändert, solange sie nicht bearbeitet werden.",
        ));
        details.add(&dtstart_entry);
        let due_entry = adw::EntryRow::builder()
            .title("Fällig am (YYYY-MM-DD, optional)")
            .build();
        due_entry.set_tooltip_text(Some(
            "Neue Werte sind date-only. Importierte date-time-Werte bleiben unverändert, solange sie nicht bearbeitet werden.",
        ));
        details.add(&due_entry);
        let completed_entry = adw::EntryRow::builder()
            .title("Abgeschlossen am (YYYY-MM-DD, optional)")
            .build();
        completed_entry.set_tooltip_text(Some(
            "Ein Abschlussdatum ist nur für den Status Erledigt zulässig und darf nicht in der Zukunft liegen.",
        ));
        details.add(&completed_entry);
        let recurrence_entry = adw::EntryRow::builder()
            .title("Wiederholung (RRULE, optional)")
            .build();
        recurrence_entry.set_tooltip_text(Some("z. B. FREQ=WEEKLY;BYDAY=MO;COUNT=10"));
        details.add(&recurrence_entry);
        let rdate_entry = adw::EntryRow::builder()
            .title("Zusätzliche Fälligkeiten (RDATE, kommasepariert)")
            .build();
        rdate_entry.set_tooltip_text(Some("z. B. 20260812,20260819"));
        details.add(&rdate_entry);
        let exdate_entry = adw::EntryRow::builder()
            .title("Ausnahmen (EXDATE, kommasepariert)")
            .build();
        exdate_entry.set_tooltip_text(Some("z. B. 20260815,20260822"));
        details.add(&exdate_entry);

        let priority_row = adw::ActionRow::builder()
            .title("Priorität (1 = hoch, 0 = normal)")
            .build();
        let priority_spin = gtk::SpinButton::with_range(0.0, 9.0, 1.0);
        priority_spin.set_numeric(true);
        priority_spin.set_value(0.0);
        priority_row.add_suffix(&priority_spin);
        details.add(&priority_row);

        let status_row = adw::ComboRow::builder()
            .title("Status")
            .model(&gtk::StringList::new(&[
                "Nicht begonnen",
                "In Bearbeitung",
                "Erledigt",
                "Abgebrochen",
            ]))
            .build();
        details.add(&status_row);
        let percent_row = adw::SpinRow::with_range(0.0, 100.0, 1.0);
        percent_row.set_title("Fortschritt (Prozent)");
        percent_row.set_digits(0);
        percent_row.set_snap_to_ticks(true);
        details.add(&percent_row);

        let categories_entry = adw::EntryRow::builder()
            .title("Kategorien (Komma getrennt)")
            .build();
        details.add(&categories_entry);
        let attachment_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::None)
            .height_request(72)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let attachment_buffer = attachment_view.buffer();
        let attachment_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(72)
            .max_content_height(160)
            .build();
        attachment_scroll.set_child(Some(&attachment_view));
        let attachment_picker = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Dateien als Aufgabenanhänge auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&attachment_picker, "Dateien als Aufgabenanhänge auswählen");
        let attachment_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(6)
            .margin_top(6)
            .build();
        let attachment_title = gtk::Label::builder()
            .label("Anhänge (URI- oder vollständige ATTACH-Zeile pro Zeile)")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        attachment_header.append(&attachment_title);
        attachment_header.append(&attachment_picker);
        let attachment_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        attachment_box.append(&attachment_header);
        attachment_box.append(&attachment_scroll);
        details.add(&attachment_box);
        let alarm_editor = AlarmEditor::new(
            (!initial_component.trim().is_empty()).then_some(initial_component.as_str()),
            &initial_draft.summary,
        );
        let alarm_dirty_gesture = gtk::GestureClick::new();
        let dirty_for_alarm = Rc::clone(&dirty);
        let dialog_state_for_alarm = Rc::clone(&dialog_state);
        alarm_dirty_gesture.connect_pressed(move |_, _, _, _| {
            dirty_for_alarm.set(true);
            dialog_state_for_alarm.set(TaskEditorDialogState::Dirty);
        });
        alarm_editor.widget().add_controller(alarm_dirty_gesture);
        details.add(alarm_editor.widget());
        content_box.append(&details);

        let attachment_buffer_for_picker = attachment_buffer.clone();
        attachment_picker.connect_clicked(move |button| {
            let Some(root) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let file_dialog = gtk::FileDialog::builder()
                .title("Aufgabenanhänge auswählen")
                .modal(true)
                .build();
            let attachment_buffer_for_result = attachment_buffer_for_picker.clone();
            file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
                let Ok(files) = result else {
                    return;
                };
                let mut values = attachment_buffer_for_result
                    .text(
                        &attachment_buffer_for_result.start_iter(),
                        &attachment_buffer_for_result.end_iter(),
                        false,
                    )
                    .lines()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                for index in 0..files.n_items() {
                    let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                        continue;
                    };
                    let uri = file.uri().to_string();
                    if !uri.is_empty() && !values.iter().any(|value| value == &uri) {
                        values.push(uri);
                    }
                }
                attachment_buffer_for_result.set_text(&values.join("\n"));
            });
        });

        let description_group = adw::PreferencesGroup::builder()
            .title("Beschreibung")
            .build();
        let description_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(120)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        description_group.add(&description_view);
        content_box.append(&description_group);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content_box)
            .build();
        toolbar_view.set_content(Some(&scroll));
        dialog.set_content(Some(&toolbar_view));

        let edit_source_uid = task_to_edit
            .as_ref()
            .map(TaskItemObject::source_uid)
            .filter(|uid| !uid.is_empty());
        let title = task_to_edit
            .as_ref()
            .map(TaskItemObject::title)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| initial_draft.summary.clone());
        let (initial_percent, initial_completed) =
            task_initial_editor_lifecycle(&initial_draft, Local::now().date_naive());
        title_entry.set_text(&title);
        dtstart_entry.set_text(&initial_draft.dtstart.input);
        due_entry.set_text(&initial_draft.due.input);
        completed_entry.set_text(&initial_completed);
        recurrence_entry.set_text(&initial_draft.recurrence);
        rdate_entry.set_text(&initial_draft.rdates);
        exdate_entry.set_text(&initial_draft.exdates);
        priority_spin.set_value(initial_draft.priority as f64);
        status_row.set_selected(task_status_index(&initial_component));
        percent_row.set_value(initial_percent as f64);
        categories_entry.set_text(&initial_draft.categories);
        attachment_buffer.set_text(&initial_draft.attachments);
        description_view
            .buffer()
            .set_text(&initial_draft.description);
        if initial_draft.unsupported_status.is_some() || initial_draft.unsupported_percent.is_some()
        {
            let mut imported = Vec::new();
            if let Some(status) = initial_draft.unsupported_status.as_deref() {
                imported.push(format!("STATUS={status}"));
            }
            if let Some(percent) = initial_draft.unsupported_percent.as_deref() {
                imported.push(format!("PERCENT-COMPLETE={percent}"));
            }
            let imported_label = gtk::Label::builder()
                .label(format!(
                    "Importierte Werte werden unverändert bewahrt, bis sie bearbeitet werden: {}",
                    imported.join(", ")
                ))
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(vec!["warning", "dim-label"])
                .build();
            details.add(&imported_label);
            status_row.set_tooltip_text(Some(
                "Importierter, nicht unterstützter STATUS-Wert; unverändert bewahrt",
            ));
            percent_row.set_tooltip_text(Some(
                "Importierter, nicht unterstützter PERCENT-COMPLETE-Wert; unverändert bewahrt",
            ));
        }
        let percent_adjustment = percent_row.adjustment();
        // Imported VTODO lifecycle values can be outside the portable picker
        // vocabulary. Track direct editor interaction separately from dirty
        // state so saving another field cannot rewrite those values.
        let lifecycle_edits = Rc::new(RefCell::new(
            if initial_draft.unsupported_status.is_none()
                && initial_draft.unsupported_percent.is_none()
            {
                // The standard editor normalizes incomplete built-in lifecycle
                // states when it presents them (for example COMPLETED without a
                // date). Persist that visible repair, while keeping imported
                // unsupported values untouched until interaction.
                TaskLifecycleEdits {
                    status: false,
                    percent: initial_percent != initial_draft.percent,
                    completed: initial_completed.trim() != initial_draft.completed.input.trim(),
                }
            } else {
                TaskLifecycleEdits::default()
            },
        ));

        for entry in [
            title_entry.clone(),
            dtstart_entry.clone(),
            due_entry.clone(),
            recurrence_entry.clone(),
            rdate_entry.clone(),
            exdate_entry.clone(),
            categories_entry.clone(),
        ] {
            let dirty_for_entry = Rc::clone(&dirty);
            let dialog_state_for_entry = Rc::clone(&dialog_state);
            entry.connect_changed(move |_| {
                dirty_for_entry.set(true);
                dialog_state_for_entry.set(TaskEditorDialogState::Dirty);
            });
        }
        let dirty_for_priority = Rc::clone(&dirty);
        let dialog_state_for_priority = Rc::clone(&dialog_state);
        priority_spin.connect_value_changed(move |_| {
            dirty_for_priority.set(true);
            dialog_state_for_priority.set(TaskEditorDialogState::Dirty);
        });
        let dirty_for_status = Rc::clone(&dirty);
        let dialog_state_for_status = Rc::clone(&dialog_state);
        let lifecycle_edits_for_status = Rc::clone(&lifecycle_edits);
        status_row.connect_selected_notify(move |_| {
            dirty_for_status.set(true);
            dialog_state_for_status.set(TaskEditorDialogState::Dirty);
            lifecycle_edits_for_status.borrow_mut().status = true;
        });
        let dirty_for_percent = Rc::clone(&dirty);
        let dialog_state_for_percent = Rc::clone(&dialog_state);
        let lifecycle_edits_for_percent = Rc::clone(&lifecycle_edits);
        percent_adjustment.connect_value_changed(move |_| {
            dirty_for_percent.set(true);
            dialog_state_for_percent.set(TaskEditorDialogState::Dirty);
            lifecycle_edits_for_percent.borrow_mut().percent = true;
        });
        let dirty_for_completed = Rc::clone(&dirty);
        let dialog_state_for_completed = Rc::clone(&dialog_state);
        let lifecycle_edits_for_completed = Rc::clone(&lifecycle_edits);
        completed_entry.connect_changed(move |_| {
            dirty_for_completed.set(true);
            dialog_state_for_completed.set(TaskEditorDialogState::Dirty);
            lifecycle_edits_for_completed.borrow_mut().completed = true;
        });
        let dirty_for_description = Rc::clone(&dirty);
        let dialog_state_for_description = Rc::clone(&dialog_state);
        description_view.buffer().connect_changed(move |_| {
            dirty_for_description.set(true);
            dialog_state_for_description.set(TaskEditorDialogState::Dirty);
        });
        let dirty_for_attachments = Rc::clone(&dirty);
        let dialog_state_for_attachments = Rc::clone(&dialog_state);
        attachment_buffer.connect_changed(move |_| {
            dirty_for_attachments.set(true);
            dialog_state_for_attachments.set(TaskEditorDialogState::Dirty);
        });

        // Keep the GTK3 lifecycle invariants synchronized while the user is
        // editing. The save validator repeats these checks as a final gate.
        let lifecycle_syncing = Rc::new(Cell::new(false));
        let syncing_for_status = Rc::clone(&lifecycle_syncing);
        let status_for_status = status_row.clone();
        let percent_for_status = percent_row.clone();
        let completed_for_status = completed_entry.clone();
        status_row.connect_selected_notify(move |_| {
            if syncing_for_status.get() {
                return;
            }
            syncing_for_status.set(true);
            match task_status_from_index(status_for_status.selected()) {
                "NEEDS-ACTION" => {
                    percent_for_status.set_value(0.0);
                    completed_for_status.set_text("");
                }
                "IN-PROCESS" => {
                    let percent = percent_for_status.value().round() as u8;
                    if !(1..=99).contains(&percent) {
                        percent_for_status.set_value(50.0);
                    }
                    completed_for_status.set_text("");
                }
                "COMPLETED" => {
                    percent_for_status.set_value(100.0);
                    if completed_for_status.text().trim().is_empty() {
                        completed_for_status
                            .set_text(&Local::now().date_naive().format("%Y-%m-%d").to_string());
                    }
                }
                "CANCELLED" => {
                    percent_for_status.set_value(0.0);
                    completed_for_status.set_text("");
                }
                _ => {}
            }
            syncing_for_status.set(false);
        });

        let syncing_for_percent = Rc::clone(&lifecycle_syncing);
        let status_for_percent = status_row.clone();
        let completed_for_percent = completed_entry.clone();
        let percent_adjustment_for_sync = percent_adjustment.clone();
        percent_adjustment_for_sync.connect_value_changed(move |adjustment| {
            if syncing_for_percent.get() {
                return;
            }
            syncing_for_percent.set(true);
            let percent = adjustment.value().round().clamp(0.0, 100.0) as u8;
            if percent == 0 {
                status_for_percent.set_selected(0);
                completed_for_percent.set_text("");
            } else if percent == 100 {
                status_for_percent.set_selected(2);
                if completed_for_percent.text().trim().is_empty() {
                    completed_for_percent
                        .set_text(&Local::now().date_naive().format("%Y-%m-%d").to_string());
                }
            } else {
                status_for_percent.set_selected(1);
                completed_for_percent.set_text("");
            }
            syncing_for_percent.set(false);
        });

        let syncing_for_completed = Rc::clone(&lifecycle_syncing);
        let status_for_completed = status_row.clone();
        let percent_for_completed = percent_row.clone();
        completed_entry.connect_changed(move |entry| {
            if syncing_for_completed.get() {
                return;
            }
            syncing_for_completed.set(true);
            if entry.text().trim().is_empty() {
                if status_for_completed.selected() == 2 {
                    status_for_completed.set_selected(0);
                    percent_for_completed.set_value(0.0);
                }
            } else {
                status_for_completed.set_selected(2);
                percent_for_completed.set_value(100.0);
            }
            syncing_for_completed.set(false);
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let edit_source_uid_for_load = edit_source_uid.clone();
        let local_calendar_available = CalendarStoreService::new().is_writable();
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_task_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        let source_capabilities_for_load = Rc::clone(&source_capabilities);
        let save_button_for_load = save_button.clone();
        let dialog_weak_for_load = dialog.downgrade();
        let request_generation_for_load = Rc::clone(&request_generation);
        let source_load_generation = request_generation.get();
        save_button.set_sensitive(false);
        let (source_sender, source_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let discovery =
                ECalService::new().discover_source_capabilities(ECalComponentType::Task);
            if let Err(error) = source_sender.send_blocking(discovery) {
                tracing::debug!(%error, "Task editor source capability result was dropped");
            }
        });
        glib::spawn_future_local(async move {
            let discovery = source_receiver.recv().await;
            let Some(dialog) = dialog_weak_for_load.upgrade() else {
                return;
            };
            if !task_editor_result_is_current(
                request_generation_for_load.get(),
                source_load_generation,
                dialog.is_visible(),
            ) {
                return;
            }
            let (mut active_sources, discovery_failed) = match discovery {
                Ok(Ok(discovery)) => {
                    if let Some(warning) = discovery.warning.as_deref() {
                        tracing::warn!(%warning, "Some EDS task lists are unavailable in task editor");
                    }
                    (discovery.sources, false)
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Could not reach exact EDS task lists for task dialog");
                    (Vec::new(), true)
                }
                Err(error) => {
                    tracing::debug!(%error, "Task editor source capability channel closed");
                    (Vec::new(), true)
                }
            };
            active_sources.sort_by_cached_key(|source| {
                (
                    source.display_name.to_lowercase(),
                    source.source_uid.clone(),
                )
            });
            let mut names = Vec::new();
            let mut capabilities = HashMap::new();
            let mut uids = source_uids_for_load.borrow_mut();
            uids.clear();
            for source in active_sources {
                let uid = source.source_uid.clone();
                names.push(task_source_capability_label(&source));
                capabilities.insert(uid.clone(), task_source_capability_from_ecal(&source));
                uids.push(uid);
            }
            let preserved_source_uid = edit_source_uid_for_load.as_deref().filter(|uid| {
                *uid != "local-calendar" && !uids.iter().any(|candidate| candidate == uid)
            });
            if let Some(uid) = preserved_source_uid {
                names.push(format!("Nicht verfügbare Quelle · {uid}"));
                uids.push(uid.to_string());
                capabilities.insert(uid.to_string(), TaskSourceCapability::Unavailable);
            }
            if local_calendar_available {
                names.push("Lokale Aufgabenliste".to_string());
                uids.push("local-calendar".to_string());
                capabilities.insert("local-calendar".to_string(), TaskSourceCapability::Writable);
            } else if edit_source_uid_for_load.as_deref() == Some("local-calendar") {
                names.push("Nicht verfügbare Quelle · local-calendar".to_string());
                uids.push("local-calendar".to_string());
                capabilities.insert(
                    "local-calendar".to_string(),
                    TaskSourceCapability::Unavailable,
                );
            }
            if names.is_empty() {
                names.push(if discovery_failed {
                    "Aufgabenlisten sind nicht erreichbar".to_string()
                } else {
                    "Keine aktive Aufgabenliste verfügbar".to_string()
                });
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            let selected_index = task_default_source_index(
                &uids,
                preferred_source_uid.as_deref(),
                edit_source_uid_for_load.as_deref(),
                &capabilities,
                local_calendar_available,
            );
            let selected_uid = selected_index.and_then(|index| uids.get(index).cloned());
            drop(uids);
            source_capabilities_for_load.replace(capabilities);
            if let Some(index) = selected_index {
                source_row_for_load.set_selected(index as u32);
            }
            let writable = selected_uid
                .as_deref()
                .is_some_and(|uid| task_source_is_writable(uid, &source_capabilities_for_load));
            save_button_for_load.set_sensitive(writable);
        });

        let dialog_weak = dialog.downgrade();
        let page_weak = self.downgrade();
        let source_uids_for_save = Rc::clone(&source_uids);
        let task_to_edit_for_save = task_to_edit.clone();
        let source_capabilities_for_save = Rc::clone(&source_capabilities);
        let save_control_for_save = Arc::clone(&save_control);
        let request_generation_for_save = Rc::clone(&request_generation);
        let dirty_for_save = Rc::clone(&dirty);
        let lifecycle_edits_for_save = Rc::clone(&lifecycle_edits);
        let dialog_state_for_save = Rc::clone(&dialog_state);
        let close_confirmed_for_save = Rc::clone(&close_confirmed);
        let save_button_for_signal = save_button.clone();
        let save_button_for_closure = save_button.clone();
        let cancel_button_for_save = cancel_button.clone();
        save_button_for_signal.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            if save_control_for_save.phase() != TaskSavePhase::Editing {
                return;
            }
            let source_uid = if let Some(task) = task_to_edit_for_save.as_ref() {
                task.source_uid()
            } else {
                let selected = source_row.selected() as usize;
                let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() else {
                    show_toast_for_widget(&dialog, "Keine aktive Aufgabenliste ist ausgewählt");
                    return;
                };
                source_uid
            };
            if !task_source_is_writable(&source_uid, &source_capabilities_for_save) {
                show_toast_for_widget(
                    &dialog,
                    "Die ausgewählte Aufgabenliste ist nicht schreibfähig oder nicht verfügbar",
                );
                return;
            };
            // A save attempt is a dirty transaction until the backend has
            // confirmed it. This also keeps a failed no-op save reopenable.
            dirty_for_save.set(true);
            let description = description_view.buffer().text(
                &description_view.buffer().start_iter(),
                &description_view.buffer().end_iter(),
                false,
            );
            let attachments = attachment_buffer.text(
                &attachment_buffer.start_iter(),
                &attachment_buffer.end_iter(),
                false,
            );
            let recurrence = recurrence_entry.text();
            let rdates = rdate_entry.text();
            let exdates = exdate_entry.text();
            let priority = priority_spin.value() as u8;
            let categories = categories_entry.text();
            alarm_editor.set_summary(&title_entry.text());
            let alarms = alarm_editor.alarms();
            let status = task_status_from_index(status_row.selected());
            let percent = percent_row.value().round().clamp(0.0, 100.0) as u8;
            let component_result = match task_to_edit_for_save.as_ref() {
                Some(task) => update_task_component_with_dates_and_lifecycle_edits(
                    &task.component(),
                    &title_entry.text(),
                    &dtstart_entry.text(),
                    &due_entry.text(),
                    &completed_entry.text(),
                    &recurrence,
                    &rdates,
                    &exdates,
                    priority,
                    &categories,
                    &alarms,
                    &description,
                    &attachments,
                    status,
                    percent,
                    *lifecycle_edits_for_save.borrow(),
                ),
                None => build_task_component_with_dates(
                    &title_entry.text(),
                    &dtstart_entry.text(),
                    &due_entry.text(),
                    &completed_entry.text(),
                    &recurrence,
                    &rdates,
                    &exdates,
                    priority,
                    &categories,
                    &alarms,
                    &description,
                    &attachments,
                    status,
                    percent,
                ),
            };
            let component = match component_result {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&dialog, &error);
                    return;
                }
            };
            // Discovery can become stale while the user edits.  This is the
            // final capability gate immediately before arming the save.
            if !task_source_is_writable(&source_uid, &source_capabilities_for_save) {
                show_toast_for_widget(
                    &dialog,
                    "Die Aufgabenliste ist nicht mehr schreibfähig oder nicht verfügbar",
                );
                return;
            }
            let source_uid_for_model = source_uid.clone();
            let operation_uid = task_to_edit_for_save.as_ref().map(TaskItemObject::id);
            let expected_uid = operation_uid.clone();
            let task_to_edit_for_result = task_to_edit_for_save.clone();
            let generation = request_generation_for_save.get().wrapping_add(1);
            request_generation_for_save.set(generation);
            if !save_control_for_save.arm() {
                return;
            }
            dialog_state_for_save.set(TaskEditorDialogState::SavePending);
            let (sender, receiver) = async_channel::bounded(3);
            save_button_for_closure.set_sensitive(false);
            cancel_button_for_save.set_sensitive(true);
            let source_capabilities_for_worker = source_capabilities_for_save.borrow().clone();
            let save_control_for_worker = Arc::clone(&save_control_for_save);
            std::thread::spawn(move || {
                let local_writable = CalendarStoreService::new().is_writable();
                if task_source_capability(
                    &source_uid,
                    &source_capabilities_for_worker,
                    local_writable,
                ) != TaskSourceCapability::Writable
                {
                    if let Err(error) = sender.send_blocking(TaskSaveWorkerMessage::Finished(Err(
                        crate::services::ecal_service::ECalMutationError::Backend(
                            "Die Aufgabenliste ist unmittelbar vor dem Speichern nicht mehr schreibfähig".to_string(),
                        ),
                    ))) {
                        tracing::debug!(%error, "Capability failure result was dropped");
                    }
                    return;
                }
                if !save_control_for_worker.begin_dispatch() {
                    if let Err(error) = sender.send_blocking(
                        TaskSaveWorkerMessage::CancelledBeforeDispatch,
                    ) {
                        tracing::debug!(%error, "Cancelled task mutation result was dropped");
                    }
                    return;
                }
                if let Err(error) = sender.send_blocking(TaskSaveWorkerMessage::Committing) {
                    tracing::debug!(%error, "Task commit state was dropped");
                    return;
                }
                let service = ECalService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => service.modify_component(
                        &source_uid,
                        ECalComponentType::Task,
                        uid,
                        &component,
                    ),
                    None => {
                        service.create_component(&source_uid, ECalComponentType::Task, &component)
                    }
                };
                let result = result.and_then(|mut result| {
                    if result
                        .component
                        .as_deref()
                        .is_some_and(|component| !component.trim().is_empty())
                    {
                        return Ok(result);
                    }
                    let uid = result.uid.clone();
                    let authoritative = load_snapshot_prefer_eds()
                        .snapshot
                        .tasks
                        .into_iter()
                        .find(|task| task.source_uid == source_uid && task.uid == uid)
                        .map(|task| task.component);
                    if let Some(component) = authoritative {
                        result.component = Some(component);
                        Ok(result)
                    } else {
                        Err(crate::services::ecal_service::ECalMutationError::Backend(
                            "Backend bestätigte keine autoritative VTODO-Komponente; bitte neu laden"
                                .to_string(),
                        ))
                    }
                });
                if let Err(error) = sender.send_blocking(TaskSaveWorkerMessage::Finished(result)) {
                    tracing::debug!(%error, "Task mutation result was dropped");
                }
            });

            let dialog_weak_for_result = dialog_weak.clone();
            let page_weak_for_result = page_weak.clone();
            let task_store_for_result = task_store.clone();
            let task_list_for_result = task_list.clone();
            let empty_tasks_for_result = empty_tasks.clone();
            let save_button_for_result = save_button_for_closure.clone();
            let cancel_button_for_result = cancel_button_for_save.clone();
            let save_control_for_result = Arc::clone(&save_control_for_save);
            let dialog_state_for_result = Rc::clone(&dialog_state_for_save);
            let request_generation_for_result = Rc::clone(&request_generation_for_save);
            let dirty_for_result = Rc::clone(&dirty_for_save);
            let close_confirmed_for_result = Rc::clone(&close_confirmed_for_save);
            glib::spawn_future_local(async move {
                let result = loop {
                    match receiver.recv().await {
                        Ok(TaskSaveWorkerMessage::Committing) => {
                            dialog_state_for_result.set(TaskEditorDialogState::Committing);
                            cancel_button_for_result.set_sensitive(false);
                        }
                        Ok(TaskSaveWorkerMessage::CancelledBeforeDispatch) => break None,
                        Ok(TaskSaveWorkerMessage::Finished(result)) => break Some(result),
                        Err(error) => {
                            tracing::debug!(%error, "Task mutation channel closed");
                            break None;
                        }
                    }
                };
                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                    return;
                };
                let Some(result) = result else {
                    if task_editor_result_is_current(
                        request_generation_for_result.get(),
                        generation,
                        dialog.is_visible(),
                    ) {
                        save_control_for_result.reset_after_failure();
                        dialog_state_for_result.set(TaskEditorDialogState::Dirty);
                        save_button_for_result.set_sensitive(true);
                        cancel_button_for_result.set_sensitive(true);
                    }
                    return;
                };
                let result_is_current = task_editor_result_is_current(
                    request_generation_for_result.get(),
                    generation,
                    dialog.is_visible(),
                );
                if !result_is_current {
                    return;
                }
                let backend_succeeded = result.is_ok();
                save_control_for_result.finish(backend_succeeded);
                let result_state = task_editor_result_state(
                    result_is_current,
                    backend_succeeded,
                    dirty_for_result.get(),
                );
                if !result_state.apply_row {
                    save_control_for_result.reset_after_failure();
                    dialog_state_for_result.set(TaskEditorDialogState::Dirty);
                    save_button_for_result.set_sensitive(true);
                    cancel_button_for_result.set_sensitive(true);
                    dirty_for_result.set(result_state.dirty);
                    match result {
                        Err(error) => show_toast_for_widget(
                            &dialog,
                            &format!("Aufgabe konnte nicht gespeichert werden: {error}"),
                        ),
                        Ok(_) => unreachable!("successful results must update the row"),
                    }
                    return;
                }
                let Ok(result) = result else {
                    unreachable!("successful result state must contain a backend result");
                };
                if expected_uid
                    .as_deref()
                    .is_some_and(|expected| expected != result.uid)
                {
                    save_control_for_result.reset_after_failure();
                    dialog_state_for_result.set(TaskEditorDialogState::Dirty);
                    save_button_for_result.set_sensitive(true);
                    cancel_button_for_result.set_sensitive(true);
                    show_toast_for_widget(
                        &dialog,
                        "Das bestätigte Aufgaben-Ergebnis gehört zu einer anderen UID",
                    );
                    return;
                }
                let uid = if result.uid.trim().is_empty() {
                    expected_uid.clone().unwrap_or_default()
                } else {
                    result.uid
                };
                if uid.trim().is_empty() {
                    save_control_for_result.reset_after_failure();
                    dialog_state_for_result.set(TaskEditorDialogState::Dirty);
                    save_button_for_result.set_sensitive(true);
                    cancel_button_for_result.set_sensitive(true);
                    show_toast_for_widget(&dialog, "ECal lieferte keine gültige Aufgaben-UID");
                    return;
                }
                let Some(component) = result
                    .component
                    .filter(|component| !component.trim().is_empty())
                else {
                    save_control_for_result.reset_after_failure();
                    dialog_state_for_result.set(TaskEditorDialogState::Dirty);
                    save_button_for_result.set_sensitive(true);
                    cancel_button_for_result.set_sensitive(true);
                    show_toast_for_widget(
                        &dialog,
                        "Backend bestätigte keine autoritative Aufgabe; bitte neu laden",
                    );
                    return;
                };
                let task = match task_item_from_confirmed_component(
                    &source_uid_for_model,
                    &uid,
                    &component,
                ) {
                    Ok(task) => task,
                    Err(error) => {
                        save_control_for_result.reset_after_failure();
                        dialog_state_for_result.set(TaskEditorDialogState::Dirty);
                        save_button_for_result.set_sensitive(true);
                        cancel_button_for_result.set_sensitive(true);
                        show_toast_for_widget(
                            &dialog,
                            &format!("Bestätigte Aufgabe ist ungültig: {error}"),
                        );
                        return;
                    }
                };
                if let Some(existing) = task_to_edit_for_result.as_ref() {
                    let mut store_index = None;
                    for index in 0..task_store_for_result.n_items() {
                        if task_store_for_result
                            .item(index)
                            .and_downcast::<TaskItemObject>()
                            .is_some_and(|task| {
                                task.id() == existing.id()
                                    && task.source_uid() == existing.source_uid()
                            })
                        {
                            store_index = Some(index);
                            break;
                        }
                    }
                    if let Some(index) = store_index {
                        task_store_for_result.remove(index);
                    }
                    remove_task_row(
                        &task_list_for_result,
                        &existing.source_uid(),
                        &existing.id(),
                    );
                }
                task_store_for_result.append(&task);
                if let Some(page) = page_weak_for_result.upgrade() {
                    render_task_rows(
                        &page,
                        &task_list_for_result,
                        &task_store_for_result,
                        &empty_tasks_for_result,
                    );
                    let message = if task_to_edit_for_result.is_some() {
                        format!("Aufgabe aktualisiert ({uid})")
                    } else {
                        format!("Aufgabe gespeichert ({uid})")
                    };
                    page.show_toast(&message);
                }
                dirty_for_result.set(result_state.dirty);
                dialog_state_for_result.set(TaskEditorDialogState::Closed);
                close_confirmed_for_result.set(true);
                dialog.close();
            });
        });

        let dialog_weak_for_close = dialog.downgrade();
        let close_confirmed_for_close = Rc::clone(&close_confirmed);
        let close_prompt_open_for_close = Rc::clone(&close_prompt_open);
        let dirty_for_close = Rc::clone(&dirty);
        let save_control_for_close = Arc::clone(&save_control);
        let dialog_state_for_close = Rc::clone(&dialog_state);
        let request_generation_for_close = Rc::clone(&request_generation);
        let save_button_for_close = save_button.clone();
        dialog.connect_close_request(move |_| {
            if close_confirmed_for_close.replace(false) {
                return glib::Propagation::Proceed;
            }
            match task_close_decision(
                dirty_for_close.get(),
                save_control_for_close.phase(),
                close_prompt_open_for_close.get(),
            ) {
                TaskCloseDecision::Allow => return glib::Propagation::Proceed,
                TaskCloseDecision::BlockSave => {
                    if let Some(dialog) = dialog_weak_for_close.upgrade() {
                        show_toast_for_widget(&dialog, "Speichern läuft noch; bitte kurz warten");
                    }
                    return glib::Propagation::Stop;
                }
                TaskCloseDecision::BlockCommit => {
                    if let Some(dialog) = dialog_weak_for_close.upgrade() {
                        show_toast_for_widget(
                            &dialog,
                            "Speichern wird gerade verbindlich ausgeführt; Schließen ist nicht verfügbar",
                        );
                    }
                    return glib::Propagation::Stop;
                }
                TaskCloseDecision::KeepPrompt => return glib::Propagation::Stop,
                TaskCloseDecision::Prompt => close_prompt_open_for_close.set(true),
            }
            let confirmation = adw::AlertDialog::new(
                Some("Ungespeicherte Aufgabenänderungen"),
                Some("Die Aufgabe wurde seit dem letzten bestätigten Speichern geändert."),
            );
            confirmation.add_responses(&[
                ("continue", "Weiter bearbeiten"),
                ("save", "Speichern"),
                ("discard", "Verwerfen"),
            ]);
            confirmation.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            let dialog_weak_for_response = dialog_weak_for_close.clone();
            let close_confirmed_for_response = Rc::clone(&close_confirmed_for_close);
            let close_prompt_open_for_response = Rc::clone(&close_prompt_open_for_close);
            let request_generation_for_response = Rc::clone(&request_generation_for_close);
            let dialog_state_for_response = Rc::clone(&dialog_state_for_close);
            let save_button_for_response = save_button_for_close.clone();
            confirmation.connect_response(None, move |alert, response| {
                alert.close();
                close_prompt_open_for_response.set(false);
                let Some(dialog) = dialog_weak_for_response.upgrade() else {
                    return;
                };
                match response {
                    "discard" => {
                        request_generation_for_response
                            .set(request_generation_for_response.get().wrapping_add(1));
                        dialog_state_for_response.set(TaskEditorDialogState::Closed);
                        close_confirmed_for_response.set(true);
                        dialog.close();
                    }
                    "save" => save_button_for_response.emit_clicked(),
                    _ => dialog_state_for_response.set(TaskEditorDialogState::Dirty),
                }
            });
            if let Some(dialog) = dialog_weak_for_close.upgrade() {
                confirmation.present(Some(&dialog));
            } else {
                close_prompt_open_for_close.set(false);
            }
            glib::Propagation::Stop
        });

        let dialog_weak_for_cancel = dialog.downgrade();
        let save_control_for_cancel = Arc::clone(&save_control);
        let dialog_state_for_cancel = Rc::clone(&dialog_state);
        let request_generation_for_cancel = Rc::clone(&request_generation);
        let close_confirmed_for_cancel = Rc::clone(&close_confirmed);
        cancel_button.connect_clicked(move |_| {
            match save_control_for_cancel.phase() {
                TaskSavePhase::DispatchPending => {
                    match save_control_for_cancel.cancel_before_dispatch() {
                        TaskCancelResult::CancelledBeforeDispatch => {
                            request_generation_for_cancel
                                .set(request_generation_for_cancel.get().wrapping_add(1));
                            dialog_state_for_cancel.set(TaskEditorDialogState::Closed);
                            close_confirmed_for_cancel.set(true);
                            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                                dialog.close();
                            }
                        }
                        TaskCancelResult::Committing | TaskCancelResult::DispatchRace => {
                            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                                show_toast_for_widget(
                                    &dialog,
                                    "Speichern wird gerade verbindlich ausgeführt; Abbrechen ist nicht verfügbar",
                                );
                            }
                        }
                        TaskCancelResult::NotCancelable => {}
                    }
                }
                TaskSavePhase::Committing => {
                    if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                        show_toast_for_widget(
                            &dialog,
                            "Speichern wird gerade verbindlich ausgeführt; Abbrechen ist nicht verfügbar",
                        );
                    }
                }
                TaskSavePhase::Editing | TaskSavePhase::Failed => {
                    dialog_state_for_cancel.set(TaskEditorDialogState::Closed);
                    close_confirmed_for_cancel.set(true);
                    if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                        dialog.close();
                    }
                }
                TaskSavePhase::CancelledBeforeDispatch | TaskSavePhase::Completed => {}
            }
        });

        dialog.present();
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Task page result");
    }
}

fn render_task_rows(
    page: &TasksPage,
    task_list: &gtk::ListBox,
    task_store: &gio::ListStore,
    empty_tasks: &gtk::Label,
) {
    let preserved_selection = page.imp().task_selection.borrow().clone();
    page.imp().task_selection_restoring.set(true);
    let category_metadata = page
        .imp()
        .task_category_metadata
        .borrow()
        .clone()
        .unwrap_or_default();
    refresh_task_category_model(page, task_store, &category_metadata);
    while let Some(child) = task_list.first_child() {
        task_list.remove(&child);
    }

    let filter = page.imp().task_filter.get();
    let source_filter = page.imp().task_source_filter.borrow().clone();
    let category_filter = page.imp().task_category_filter.borrow().clone();
    let sort = page.imp().task_sort.get();
    let context = current_task_filter_context();
    let query = TaskQuery::parse(
        page.imp().task_search_mode.get(),
        page.imp().task_query.borrow().as_str(),
    );
    let mut visible = 0;
    let mut tasks = Vec::new();
    for index in 0..task_store.n_items() {
        let Some(task) = task_store.item(index).and_downcast::<TaskItemObject>() else {
            continue;
        };
        if task_matches_filter_with_context(
            &task,
            filter,
            source_filter.as_deref(),
            category_filter.as_deref(),
            &query,
            context,
        ) {
            tasks.push(task);
        }
    }
    tasks.sort_by(|left, right| {
        compare_tasks(
            left,
            right,
            sort,
            page.imp().task_sort_descending.get(),
            context,
        )
    });
    for task in tasks {
        let selected = preserved_selection.contains(&TaskSelectionIdentity::from_task(&task));
        append_task_row(
            page,
            task_list,
            task_store,
            empty_tasks,
            &task,
            context,
            &category_metadata,
        );
        if selected && let Some(row) = task_list.last_child().and_downcast::<gtk::ListBoxRow>() {
            task_list.select_row(Some(&row));
        }
        visible += 1;
    }
    page.imp().task_selection_restoring.set(false);
    // A filter change or backend removal can make a preserved identity
    // invisible. Keep the page-owned selection and its actions aligned with
    // the rows GTK actually restored instead of exporting a hidden stale task.
    page.imp()
        .task_selection
        .replace(task_selection_from_list(task_list));
    update_task_selection_controls(page);
    empty_tasks.set_visible(visible == 0);
    if visible == 0 {
        empty_tasks.set_label(if query_is_empty(&query) && category_filter.is_none() {
            match filter {
                TaskFilter::All => "Keine Aufgaben geladen",
                TaskFilter::Today => "Keine heute fällige Aufgabe",
                TaskFilter::Next7 => "Keine Aufgabe in den nächsten 7 Tagen",
                TaskFilter::Next30 => "Keine Aufgabe in den nächsten 30 Tagen",
                TaskFilter::Next365 => "Keine aktive Aufgabe in den nächsten 365 Tagen",
                TaskFilter::Important => "Keine wichtigen Aufgaben",
                TaskFilter::Overdue => "Keine überfällige Aufgabe",
                TaskFilter::Scheduled => "Keine Aufgabe mit Fälligkeit",
                TaskFilter::NoDue => "Keine Aufgabe ohne Fälligkeit",
                TaskFilter::WithAttachments => "Keine Aufgabe mit Anhängen",
                TaskFilter::Started => "Keine begonnene Aufgabe",
                TaskFilter::WithoutCategory => "Keine Aufgabe ohne Kategorie",
                TaskFilter::Uncompleted => "Keine offene Aufgabe",
                TaskFilter::InProgress => "Keine Aufgabe in Bearbeitung",
                TaskFilter::Cancelled => "Keine abgebrochene Aufgabe",
                TaskFilter::Completed => "Keine erledigten Aufgaben",
            }
        } else {
            "Keine passenden Aufgaben"
        });
    }
}

fn task_selection_from_list(task_list: &gtk::ListBox) -> Vec<TaskSelectionIdentity> {
    task_list
        .selected_rows()
        .into_iter()
        .filter_map(|row| {
            let task_row = row.child()?.downcast::<adw::ActionRow>().ok()?;
            let read_data = |key: &str| unsafe {
                task_row
                    .data::<String>(key)
                    .map(|value| value.as_ref().clone())
            };
            let source_uid = read_data("task-source-uid")?;
            let task_uid = read_data("task-id")?;
            Some(TaskSelectionIdentity {
                source_uid,
                task_uid,
            })
        })
        .collect()
}

fn task_visible_identities(task_list: &gtk::ListBox) -> Result<Vec<TaskSelectionIdentity>, String> {
    let mut identities = Vec::new();
    let mut child = task_list.first_child();
    while let Some(current) = child {
        let next = current.next_sibling();
        let row = current
            .downcast::<gtk::ListBoxRow>()
            .map_err(|_| "Eine sichtbare Aufgabenzeile ist ungültig".to_string())?;
        let action_row = row
            .child()
            .and_downcast::<adw::ActionRow>()
            .ok_or_else(|| "Eine sichtbare Aufgabenzeile ist nicht mehr verfügbar".to_string())?;
        let read_data = |key: &str| unsafe {
            action_row
                .data::<String>(key)
                .map(|value| value.as_ref().clone())
        };
        let source_uid = read_data("task-source-uid")
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Eine sichtbare Aufgabe hat keine gültige Quelle".to_string())?;
        let task_uid = read_data("task-id")
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Eine sichtbare Aufgabe hat keine gültige Kennung".to_string())?;
        identities.push(TaskSelectionIdentity {
            source_uid,
            task_uid,
        });
        child = next;
    }
    Ok(identities)
}

fn task_assignment_action_enabled(
    selection: &[TaskSelectionIdentity],
    candidates: &[TaskExportCandidate],
    metadata: &HashMap<String, ECalSourceCapability>,
    mutation_running: bool,
) -> bool {
    if mutation_running {
        return false;
    }
    let Ok(candidate) = selected_task_candidate(selection, candidates) else {
        return false;
    };
    if validated_task_vtodo_component(candidate, &candidate.identity).is_err() {
        return false;
    }
    metadata
        .get(&candidate.identity.source_uid)
        .is_some_and(|source| {
            source.connected
                && source.writable
                && source.task_assignment_supported
                && source.task_assignment_conversion_supported
        })
}

fn update_task_selection_controls(page: &TasksPage) {
    let selection = page.imp().task_selection.borrow().clone();
    let candidates = page
        .imp()
        .task_store
        .borrow()
        .as_ref()
        .map(task_export_candidates)
        .unwrap_or_default();
    let state = task_selection_action_state(
        selection.len(),
        build_task_forward_payload(&selection, &candidates).is_ok(),
        selected_task_open_url(&selection, &candidates).is_ok(),
        build_task_print_document(&selection, &candidates).is_ok(),
    );
    let preview_available = task_preview_document(&selection, &candidates).is_ok();
    let mutation_running = task_mutation_batch_running(page);
    let capabilities = page.imp().task_source_capabilities.borrow();
    let primary_source = page.imp().task_source_filter.borrow().clone();
    let refresh_known_unsupported = primary_source.as_ref().is_some_and(|source_uid| {
        page.imp()
            .task_refresh_unsupported_sources
            .borrow()
            .contains(source_uid)
    });
    let refresh_enabled = task_refresh_action_enabled(
        primary_source.as_deref(),
        primary_source
            .as_ref()
            .and_then(|source_uid| capabilities.get(source_uid))
            .is_some_and(|capability| {
                matches!(
                    capability,
                    TaskSourceCapability::Writable | TaskSourceCapability::ReadOnly
                )
            }),
        refresh_known_unsupported,
        page.imp().task_refresh_running.get(),
        mutation_running,
    );
    let local_writable = CalendarStoreService::new().is_writable();
    let paste_destination = task_paste_destination(
        &page.imp().task_source_uids.borrow(),
        &capabilities,
        local_writable,
        AppSettings::try_new()
            .map(|settings| settings.default_task_source_uid())
            .filter(|uid| !uid.trim().is_empty())
            .as_deref(),
    );
    let calendar_clipboard_available =
        page.imp()
            .task_paste_button
            .borrow()
            .as_ref()
            .is_some_and(|button| {
                button
                    .display()
                    .clipboard()
                    .formats()
                    .contains_type(glib::Type::STRING)
            });
    let clipboard_state = task_clipboard_action_state(
        selection.len(),
        build_selected_task_export(&selection, &candidates).is_ok(),
        task_delete_action_enabled(
            &selection,
            &candidates,
            &capabilities,
            local_writable,
            mutation_running,
        ),
        paste_destination.is_ok(),
        calendar_clipboard_available,
        mutation_running,
    );
    let lifecycle_state = task_lifecycle_action_state(
        &selection,
        &candidates,
        &capabilities,
        local_writable,
        mutation_running,
    );
    let assignment_enabled = task_assignment_action_enabled(
        &selection,
        &candidates,
        &page.imp().task_source_metadata.borrow(),
        mutation_running,
    );
    if let Some(export_button) = page.imp().task_export_button.borrow().as_ref() {
        export_button.set_sensitive(state.export_enabled);
    }
    if let Some(forward_button) = page.imp().task_forward_button.borrow().as_ref() {
        forward_button.set_sensitive(state.forward_enabled);
    }
    if let Some(assign_button) = page.imp().task_assign_button.borrow().as_ref() {
        assign_button.set_sensitive(assignment_enabled);
        assign_button.set_tooltip_text(Some(if page.imp().task_assignment_running.get() {
            "Aufgabe wird zugewiesen …"
        } else if assignment_enabled {
            "Ausgewählte Aufgabe zuweisen"
        } else {
            "Zuweisen benötigt genau eine schreibfähige Aufgabe und Anbieter-Unterstützung"
        }));
    }
    if let Some(open_url_button) = page.imp().task_open_url_button.borrow().as_ref() {
        open_url_button.set_sensitive(state.open_url_enabled);
    }
    if let Some(print_button) = page.imp().task_print_button.borrow().as_ref() {
        print_button.set_sensitive(state.print_enabled);
    }
    if let Some(copy_button) = page.imp().task_copy_button.borrow().as_ref() {
        copy_button.set_sensitive(clipboard_state.copy_enabled);
    }
    if let Some(cut_button) = page.imp().task_cut_button.borrow().as_ref() {
        cut_button.set_sensitive(clipboard_state.cut_enabled);
    }
    if let Some(paste_button) = page.imp().task_paste_button.borrow().as_ref() {
        paste_button.set_sensitive(clipboard_state.paste_enabled);
        paste_button.set_tooltip_text(Some(if paste_destination.is_ok() {
            "Aufgaben aus der Zwischenablage in die Standard-Aufgabenliste einfügen"
        } else {
            "Keine schreibfähige Standard-Aufgabenliste verfügbar"
        }));
    }
    if let Some(select_all_button) = page.imp().task_select_all_button.borrow().as_ref() {
        select_all_button.set_sensitive(clipboard_state.select_all_enabled);
    }
    if let Some(bulk_edit_button) = page.imp().task_bulk_edit_button.borrow().as_ref() {
        // Evolution exposes Bulk Edit for any non-empty selection.  Detailed
        // Source+UID and writable-source validation belongs to activation so
        // unsupported selections receive a visible fail-closed explanation.
        bulk_edit_button.set_sensitive(!selection.is_empty() && !mutation_running);
    }
    if let Some(preview_find_entry) = page.imp().task_preview_find_entry.borrow().as_ref() {
        preview_find_entry.set_sensitive(preview_available);
    }
    if !preview_available
        && let Some(preview_find_bar) = page.imp().task_preview_find_bar.borrow().as_ref()
    {
        preview_find_bar.set_visible(false);
    }
    if let Some(complete_button) = page.imp().task_complete_button.borrow().as_ref() {
        complete_button.set_sensitive(lifecycle_state.complete_enabled);
    }
    if let Some(incomplete_button) = page.imp().task_incomplete_button.borrow().as_ref() {
        incomplete_button.set_sensitive(lifecycle_state.incomplete_enabled);
    }
    let delete_plan =
        build_task_delete_plan(&selection, &candidates, &capabilities, local_writable);
    let delete_enabled = task_delete_action_enabled(
        &selection,
        &candidates,
        &capabilities,
        local_writable,
        mutation_running,
    );
    let delete_tooltip = delete_plan
        .as_ref()
        .err()
        .map_or_else(|| "Ausgewählte Aufgaben löschen".to_string(), Clone::clone);
    if let Some(delete_button) = page.imp().task_delete_selection_button.borrow().as_ref() {
        delete_button.set_sensitive(delete_enabled);
        delete_button.set_tooltip_text(Some(&delete_tooltip));
    }
    let purge_plan = build_task_purge_plan(&candidates, &capabilities, local_writable);
    if let Some(purge_button) = page.imp().task_purge_button.borrow().as_ref() {
        purge_button.set_sensitive(!mutation_running && purge_plan.is_ok());
        purge_button.set_tooltip_text(Some(purge_plan.as_ref().err().map_or(
            "Alle erledigten Aufgaben dauerhaft löschen (Strg+E)",
            String::as_str,
        )));
    }
    if let Some(refresh_button) = page.imp().task_refresh_button.borrow().as_ref() {
        refresh_button.set_sensitive(refresh_enabled);
        refresh_button.set_tooltip_text(Some(if page.imp().task_refresh_running.get() {
            "Aufgabenliste wird aktualisiert…"
        } else if primary_source.is_none() {
            "Zum Aktualisieren eine einzelne Aufgabenliste auswählen"
        } else if refresh_known_unsupported {
            "Diese Aufgabenliste unterstützt keine Anbieter-Aktualisierung"
        } else if !refresh_enabled {
            "Die ausgewählte Aufgabenliste ist nicht verfügbar"
        } else {
            "Ausgewählte Aufgabenliste beim Anbieter aktualisieren"
        }));
    }
    let source_action_state = task_source_action_state(
        primary_source.as_deref(),
        &page.imp().task_source_metadata.borrow(),
        mutation_running,
    );
    if let Some(button) = page.imp().task_source_new_button.borrow().as_ref() {
        button.set_sensitive(!mutation_running);
        button.set_tooltip_text(Some(if mutation_running {
            "Eine andere Aufgabenänderung läuft bereits"
        } else {
            "Neue lokale Aufgabenliste"
        }));
    }
    if let Some(button) = page.imp().task_source_copy_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.copy_enabled);
        button.set_tooltip_text(Some(if primary_source.is_none() {
            "Zum Kopieren eine einzelne Aufgabenliste auswählen"
        } else if !source_action_state.copy_enabled {
            "Die ausgewählte Aufgabenliste ist nicht als EDS-Quelle verfügbar"
        } else {
            "Ausgewählte Aufgabenliste kopieren"
        }));
    }
    if let Some(button) = page.imp().task_source_rename_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.rename_enabled);
        button.set_tooltip_text(Some(if primary_source.is_none() {
            "Zum Umbenennen eine einzelne Aufgabenliste auswählen"
        } else if !source_action_state.rename_enabled {
            "Nur schreibfähige Aufgabenlisten außerhalb einer Sammlung können umbenannt werden"
        } else {
            "Ausgewählte Aufgabenliste umbenennen"
        }));
    }
    if let Some(button) = page.imp().task_source_properties_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.properties_enabled);
        button.set_tooltip_text(Some(if primary_source.is_none() {
            "Zum Bearbeiten eine einzelne Aufgabenliste auswählen"
        } else if !source_action_state.properties_enabled {
            "Die Eigenschaften dieser Aufgabenliste sind nicht schreibbar"
        } else {
            "Eigenschaften der ausgewählten Aufgabenliste"
        }));
    }
    if let Some(button) = page.imp().task_source_delete_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.delete_enabled);
        button.set_tooltip_text(Some(if primary_source.is_none() {
            "Zum Löschen eine einzelne Aufgabenliste auswählen"
        } else if !source_action_state.delete_enabled {
            "Diese Aufgabenliste kann nicht entfernt werden"
        } else {
            "Ausgewählte Aufgabenliste löschen"
        }));
    }
    let label = match selection.len() {
        0 => "Keine Auswahl".to_string(),
        1 => "1 Aufgabe ausgewählt".to_string(),
        count => format!("{count} Aufgaben ausgewählt"),
    };
    if let Some(selection_status) = page.imp().task_selection_status.borrow().as_ref() {
        selection_status.set_label(&label);
    }
    refresh_task_preview(page);
}

fn task_mutation_batch_running(page: &TasksPage) -> bool {
    page.imp().task_lifecycle_batch_running.get()
        || page.imp().task_delete_batch_running.get()
        || page.imp().task_paste_batch_running.get()
        || page.imp().task_bulk_edit_running.get()
        || page.imp().task_refresh_running.get()
        || page.imp().task_source_management_running.get()
        || page.imp().task_assignment_running.get()
}

#[derive(Debug)]
enum TaskRefreshFailure {
    Unsupported,
    Backend(String),
}

fn task_refresh_action_enabled(
    primary_source: Option<&str>,
    source_available: bool,
    known_unsupported: bool,
    refresh_running: bool,
    mutation_running: bool,
) -> bool {
    primary_source.is_some_and(|uid| !uid.trim().is_empty())
        && source_available
        && !known_unsupported
        && !refresh_running
        && !mutation_running
}

fn task_refresh_result_is_current(
    current_generation: u64,
    result_generation: u64,
    current_source: Option<&str>,
    result_source: &str,
) -> bool {
    current_generation == result_generation && current_source == Some(result_source)
}

fn apply_authoritative_task_snapshot(
    page: &TasksPage,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    result: CalendarLoadResult,
) {
    if let Some(warning) = result.warning.as_deref() {
        tracing::warn!(%warning, "Task refresh reload completed with a backend warning");
    }
    task_store.remove_all();
    for task in result.snapshot.tasks {
        upsert_task_store(task_list, task_store, task);
    }
    render_task_rows(page, task_list, task_store, empty_tasks);
}

fn start_task_source_refresh(page: &TasksPage, button: &gtk::Button) {
    if task_mutation_batch_running(page) || page.imp().task_refresh_running.get() {
        return;
    }
    let Some(source_uid) = page.imp().task_source_filter.borrow().clone() else {
        show_toast_for_widget(button, "Zum Aktualisieren eine Aufgabenliste auswählen");
        return;
    };
    if !page
        .imp()
        .task_source_capabilities
        .borrow()
        .get(&source_uid)
        .is_some_and(|capability| {
            matches!(
                capability,
                TaskSourceCapability::Writable | TaskSourceCapability::ReadOnly
            )
        })
    {
        show_toast_for_widget(
            button,
            "Die ausgewählte Aufgabenliste ist nicht mehr verfügbar",
        );
        return;
    }
    if page
        .imp()
        .task_refresh_unsupported_sources
        .borrow()
        .contains(&source_uid)
    {
        show_toast_for_widget(
            button,
            "Diese Aufgabenliste unterstützt keine Anbieter-Aktualisierung",
        );
        return;
    }
    let (Some(task_store), Some(task_list), Some(empty_tasks)) = (
        page.imp().task_store.borrow().as_ref().cloned(),
        page.imp().task_list.borrow().as_ref().cloned(),
        page.imp().task_empty_state.borrow().as_ref().cloned(),
    ) else {
        return;
    };

    let generation = page.imp().task_refresh_generation.get().wrapping_add(1);
    page.imp().task_refresh_generation.set(generation);
    page.imp().task_refresh_running.set(true);
    update_task_selection_controls(page);
    show_toast_for_widget(button, "Aufgabenliste wird aktualisiert…");

    let (sender, receiver) = async_channel::bounded(1);
    let source_uid_for_worker = source_uid.clone();
    std::thread::spawn(move || {
        let service = ECalService::new();
        let result = match service.refresh_source(&source_uid_for_worker, ECalComponentType::Task) {
            Ok(outcome) => {
                let reload = load_snapshot_prefer_eds();
                if outcome == ECalRefreshOutcome::RemoteRefreshed
                    && (reload.origin != CalendarDataOrigin::Eds || reload.warning.is_some())
                {
                    Err(TaskRefreshFailure::Backend(
                        "Die aktualisierte Aufgabenliste konnte nicht autoritativ neu geladen werden"
                            .to_string(),
                    ))
                } else {
                    Ok((outcome, reload))
                }
            }
            Err(ECalServiceError::Unsupported) => Err(TaskRefreshFailure::Unsupported),
            Err(ECalServiceError::Backend(message)) => Err(TaskRefreshFailure::Backend(message)),
        };
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Task refresh result was dropped before delivery");
        }
    });

    let page_weak = page.downgrade();
    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if page.imp().task_refresh_generation.get() != generation {
            return;
        }
        page.imp().task_refresh_running.set(false);
        let is_current = task_refresh_result_is_current(
            page.imp().task_refresh_generation.get(),
            generation,
            page.imp().task_source_filter.borrow().as_deref(),
            &source_uid,
        );
        if !is_current {
            update_task_selection_controls(&page);
            return;
        }
        match result {
            Ok((outcome, reload)) => {
                page.imp()
                    .task_refresh_unsupported_sources
                    .borrow_mut()
                    .remove(&source_uid);
                apply_authoritative_task_snapshot(
                    &page,
                    &task_store,
                    &task_list,
                    &empty_tasks,
                    reload,
                );
                page.reload_task_category_metadata();
                page.show_toast(match outcome {
                    ECalRefreshOutcome::LocalReloadOnly => "Lokale Aufgabenliste wurde neu geladen",
                    ECalRefreshOutcome::RemoteRefreshed => "Aufgabenliste wurde aktualisiert",
                });
            }
            Err(TaskRefreshFailure::Unsupported) => {
                page.imp()
                    .task_refresh_unsupported_sources
                    .borrow_mut()
                    .insert(source_uid);
                page.show_toast("Diese Aufgabenliste unterstützt keine Anbieter-Aktualisierung");
            }
            Err(TaskRefreshFailure::Backend(message)) => page.show_toast(&message),
        }
        update_task_selection_controls(&page);
    });
}

fn apply_task_preview_find(page: &TasksPage, state: TaskPreviewFindState) {
    let Some(text_view) = page.imp().task_preview_content.borrow().as_ref().cloned() else {
        return;
    };
    let buffer = text_view.buffer();
    let tag = buffer
        .tag_table()
        .lookup("task-preview-find-match")
        .unwrap_or_else(|| {
            let tag = gtk::TextTag::builder()
                .name("task-preview-find-match")
                .background("#f8e45c")
                .build();
            buffer.tag_table().add(&tag);
            tag
        });
    let mut start = buffer.start_iter();
    let mut end = buffer.end_iter();
    buffer.remove_tag(&tag, &mut start, &mut end);
    for (match_start, match_end) in &state.matches {
        let mut match_start_iter = buffer.iter_at_offset(*match_start as i32);
        let mut match_end_iter = buffer.iter_at_offset(*match_end as i32);
        buffer.apply_tag(&tag, &mut match_start_iter, &mut match_end_iter);
    }
    if let Some(index) = state.current {
        if let Some((match_start, match_end)) = state.matches.get(index).copied() {
            let mut match_start_iter = buffer.iter_at_offset(match_start as i32);
            let mut match_end_iter = buffer.iter_at_offset(match_end as i32);
            buffer.select_range(&mut match_start_iter, &mut match_end_iter);
            text_view.scroll_to_iter(&mut match_start_iter, 0.2, false, 0.0, 0.0);
        }
    }
    if let Some(status) = page.imp().task_preview_find_status.borrow().as_ref() {
        let label = match state.current {
            Some(index) => format!("{}/{}", index + 1, state.matches.len()),
            None if state.query.is_empty() => String::new(),
            None => "0/0".to_string(),
        };
        status.set_label(&label);
    }
    page.imp().task_preview_find.replace(state);
}

fn refresh_task_preview(page: &TasksPage) {
    let visible = AppSettings::try_new()
        .map(|settings| settings.task_preview_visible())
        .or_else(|| {
            page.imp()
                .task_preview_toggle
                .borrow()
                .as_ref()
                .map(gtk::ToggleButton::is_active)
        })
        .unwrap_or(true);
    if let Some(preview_box) = page.imp().task_preview_box.borrow().as_ref() {
        preview_box.set_visible(visible);
    }
    let Some(text_view) = page.imp().task_preview_content.borrow().as_ref().cloned() else {
        return;
    };
    if !visible {
        text_view.buffer().set_text("");
        page.imp().task_preview_identity.replace(None);
        page.imp().task_preview_text.replace(String::new());
        if let Some(entry) = page.imp().task_preview_find_entry.borrow().as_ref() {
            entry.set_text("");
        }
        if let Some(find_bar) = page.imp().task_preview_find_bar.borrow().as_ref() {
            find_bar.set_visible(false);
        }
        apply_task_preview_find(page, TaskPreviewFindState::default());
        return;
    }
    let selection = page.imp().task_selection.borrow().clone();
    let candidates = page
        .imp()
        .task_store
        .borrow()
        .as_ref()
        .map(task_export_candidates)
        .unwrap_or_default();
    let document = task_preview_document(&selection, &candidates);
    let (identity, text) = match document {
        Ok(document) => (Some(document.identity), document.text),
        Err(_error) if selection.len() != 1 => (None, "".to_string()),
        Err(error) => (None, format!("Vorschau nicht verfügbar: {error}")),
    };
    let changed = *page.imp().task_preview_identity.borrow() != identity
        || *page.imp().task_preview_text.borrow() != text;
    if changed {
        text_view.buffer().set_text(&text);
        page.imp().task_preview_identity.replace(identity);
        page.imp().task_preview_text.replace(text.clone());
        // Selection/component changes must never retain a stale query, match
        // range, or visible search bar from the previously rendered task.
        if let Some(entry) = page.imp().task_preview_find_entry.borrow().as_ref() {
            entry.set_text("");
        }
        if let Some(find_bar) = page.imp().task_preview_find_bar.borrow().as_ref() {
            find_bar.set_visible(false);
        }
        apply_task_preview_find(page, TaskPreviewFindState::default());
    }
}

fn task_lifecycle_authoritative_component(
    mutation: &TaskLifecycleMutation,
    result_uid: &str,
    result_component: Option<String>,
) -> Result<String, String> {
    let component =
        task_authoritative_component(&mutation.identity.task_uid, result_uid, result_component)?;
    validated_task_vtodo_component(
        &TaskExportCandidate {
            identity: mutation.identity.clone(),
            component,
        },
        &mutation.identity,
    )
}

fn apply_task_lifecycle_successes(
    task_store: &gio::ListStore,
    reduction: &mut TaskLifecycleBatchReduction,
) {
    let mut applied = Vec::new();
    for success in &reduction.successes {
        let matches = (0..task_store.n_items())
            .filter_map(|index| task_store.item(index).and_downcast::<TaskItemObject>())
            .filter(|task| {
                task.source_uid() == success.identity.source_uid
                    && task.id() == success.identity.task_uid
            })
            .collect::<Vec<_>>();
        let [task] = matches.as_slice() else {
            reduction.failures.push((
                success.identity.clone(),
                "Die bestätigte Aufgabe ist nicht mehr eindeutig geladen; bitte neu laden"
                    .to_string(),
            ));
            continue;
        };
        task.set_component(success.component.clone());
        task.set_completed(task_lifecycle_component_is_complete(&success.component));
        applied.push(success.identity.clone());
    }
    reduction
        .successes
        .retain(|success| applied.contains(&success.identity));
}

fn start_task_lifecycle_batch(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    operation: TaskLifecycleBatchOperation,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    let selection = page.imp().task_selection.borrow().clone();
    let candidates = task_export_candidates(task_store);
    let capabilities = page.imp().task_source_capabilities.borrow().clone();
    let completed_timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let plan = match build_task_lifecycle_batch_plan(
        &selection,
        &candidates,
        &capabilities,
        CalendarStoreService::new().is_writable(),
        operation,
        &completed_timestamp,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            show_toast_for_widget(button, &error);
            return;
        }
    };

    let generation = page
        .imp()
        .task_lifecycle_batch_generation
        .get()
        .wrapping_add(1);
    page.imp().task_lifecycle_batch_generation.set(generation);
    page.imp().task_lifecycle_batch_running.set(true);
    task_list.set_sensitive(false);
    update_task_selection_controls(page);
    show_toast_for_widget(button, operation.progress_label());

    let (sender, receiver) = async_channel::bounded(1);
    let plan_for_worker = plan.clone();
    std::thread::spawn(move || {
        let local_writable = CalendarStoreService::new().is_writable();
        let service = ECalService::new();
        let results = plan_for_worker
            .mutations
            .into_iter()
            .map(|mutation| {
                if task_source_capability(
                    &mutation.identity.source_uid,
                    &capabilities,
                    local_writable,
                ) != TaskSourceCapability::Writable
                {
                    return TaskLifecycleDispatchResult::Failure {
                        identity: mutation.identity,
                        error: "Die Aufgabenliste ist unmittelbar vor der Stapelaktion nicht mehr schreibfähig"
                            .to_string(),
                    };
                }
                match service.modify_component(
                    &mutation.identity.source_uid,
                    ECalComponentType::Task,
                    &mutation.identity.task_uid,
                    &mutation.component,
                ) {
                    Ok(mut result) => {
                        if result
                            .component
                            .as_deref()
                            .is_none_or(|component| component.trim().is_empty())
                        {
                            result.component = load_snapshot_prefer_eds()
                                .snapshot
                                .tasks
                                .into_iter()
                                .find(|task| {
                                    task.source_uid == mutation.identity.source_uid
                                        && task.uid == mutation.identity.task_uid
                                })
                                .map(|task| task.component);
                        }
                        match task_lifecycle_authoritative_component(
                            &mutation,
                            &result.uid,
                            result.component,
                        ) {
                        Ok(component) => TaskLifecycleDispatchResult::Success(
                            TaskLifecycleMutation {
                                component,
                                ..mutation
                            },
                        ),
                        Err(error) => TaskLifecycleDispatchResult::Failure {
                            identity: mutation.identity,
                            error,
                        },
                        }
                    }
                    Err(error) => TaskLifecycleDispatchResult::Failure {
                        identity: mutation.identity,
                        error: error.to_string(),
                    },
                }
            })
            .collect::<Vec<_>>();
        if let Err(error) = sender.send_blocking(results) {
            tracing::debug!(%error, "Task lifecycle batch result was dropped");
        }
    });

    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    let button = button.clone();
    glib::spawn_future_local(async move {
        let received = receiver.recv().await;
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_lifecycle_result_is_current(
            page.imp().task_lifecycle_batch_generation.get(),
            generation,
        ) {
            return;
        }
        page.imp().task_lifecycle_batch_running.set(false);
        task_list.set_sensitive(true);
        let results = match received {
            Ok(results) => results,
            Err(error) => {
                tracing::debug!(%error, "Task lifecycle batch channel closed");
                update_task_selection_controls(&page);
                show_toast_for_widget(
                    &button,
                    "Die Stapelaktion wurde nicht bestätigt; Aufgaben wurden nicht lokal geändert",
                );
                return;
            }
        };
        let mut reduction = reduce_task_lifecycle_batch_results(&plan, &results);
        apply_task_lifecycle_successes(&task_store, &mut reduction);
        if !reduction.successes.is_empty() {
            render_task_rows(&page, &task_list, &task_store, &empty_tasks);
        } else {
            update_task_selection_controls(&page);
        }
        match (reduction.successes.len(), reduction.failures.len()) {
            (successes, 0) => page.show_toast(&format!(
                "{successes} Aufgabe{} aktualisiert",
                if successes == 1 { "" } else { "n" }
            )),
            (0, failures) => page.show_toast(&format!(
                "Keine Aufgabe aktualisiert; {failures} Fehler. Bitte autoritative Daten neu laden"
            )),
            (successes, failures) => page.show_toast(&format!(
                "{successes} Aufgabe{} aktualisiert; {failures} Fehler. Fehlgeschlagene Aufgaben blieben unverändert",
                if successes == 1 { "" } else { "n" }
            )),
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPasteWorkerResult {
    created: Vec<TaskPasteMutation>,
    failure: Option<(TaskSelectionIdentity, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TaskPasteReduction {
    created: Vec<TaskPasteMutation>,
    failure: Option<(TaskSelectionIdentity, String)>,
    unattempted: Vec<TaskSelectionIdentity>,
}

fn reduce_task_paste_worker_result(
    plan: &TaskPastePlan,
    result: &TaskPasteWorkerResult,
) -> TaskPasteReduction {
    let expected = plan.mutations.as_slice();
    let created_valid = result.created.len() <= expected.len()
        && result
            .created
            .iter()
            .zip(expected)
            .all(|(created, expected)| created.identity == expected.identity)
        && result
            .created
            .iter()
            .map(|created| &created.identity)
            .collect::<HashSet<_>>()
            .len()
            == result.created.len();
    let failure_valid = result.failure.as_ref().is_none_or(|(identity, _)| {
        expected
            .get(result.created.len())
            .is_some_and(|expected| expected.identity == *identity)
    });
    if !created_valid || !failure_valid {
        return TaskPasteReduction {
            failure: Some((
                expected
                    .first()
                    .map(|mutation| mutation.identity.clone())
                    .unwrap_or(TaskSelectionIdentity {
                        source_uid: String::new(),
                        task_uid: String::new(),
                    }),
                "Das Backend lieferte ein veraltetes oder mehrdeutiges Einfüge-Ergebnis"
                    .to_string(),
            )),
            unattempted: expected
                .iter()
                .map(|mutation| mutation.identity.clone())
                .collect(),
            ..TaskPasteReduction::default()
        };
    }
    let attempted = result.created.len() + usize::from(result.failure.is_some());
    TaskPasteReduction {
        created: result.created.clone(),
        failure: result.failure.clone(),
        unattempted: expected
            .iter()
            .skip(attempted)
            .map(|mutation| mutation.identity.clone())
            .collect(),
    }
}

fn task_paste_result_is_current(expected_generation: u64, result_generation: u64) -> bool {
    expected_generation == result_generation
}

fn reload_task_snapshot_after_paste_uncertainty(
    page: &TasksPage,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    let generation = page.imp().task_paste_batch_generation.get();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let snapshot = load_snapshot_prefer_eds().snapshot;
        if let Err(error) = sender.send_blocking(snapshot) {
            tracing::debug!(%error, "Task paste recovery snapshot was dropped");
        }
    });
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    glib::spawn_future_local(async move {
        let Ok(snapshot) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_paste_result_is_current(page.imp().task_paste_batch_generation.get(), generation) {
            return;
        }
        task_store.remove_all();
        for task in snapshot.tasks {
            let priority = if task.priority == 0 {
                "normal".to_string()
            } else {
                task.priority.to_string()
            };
            task_store.append(&TaskItemObject::new(
                &task.source_uid,
                &task.uid,
                &task.summary,
                &task.due,
                &priority,
                task.completed,
                &task.component,
            ));
        }
        render_task_rows(&page, &task_list, &task_store, &empty_tasks);
    });
}

fn start_task_clipboard_paste(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    let capabilities = page.imp().task_source_capabilities.borrow().clone();
    let source_uids = page.imp().task_source_uids.borrow().clone();
    let preferred = AppSettings::try_new()
        .map(|settings| settings.default_task_source_uid())
        .filter(|uid| !uid.trim().is_empty());
    let destination = match task_paste_destination(
        &source_uids,
        &capabilities,
        CalendarStoreService::new().is_writable(),
        preferred.as_deref(),
    ) {
        Ok(destination) => destination,
        Err(error) => {
            show_toast_for_widget(button, &error);
            return;
        }
    };
    let generation = page.imp().task_paste_batch_generation.get().wrapping_add(1);
    page.imp().task_paste_batch_generation.set(generation);
    page.imp().task_paste_batch_running.set(true);
    update_task_selection_controls(page);

    let button_for_result = button.clone();
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    button.display().clipboard().read_text_async(None::<&gio::Cancellable>, move |result| {
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_paste_result_is_current(page.imp().task_paste_batch_generation.get(), generation) {
            return;
        }
        let payload = match result {
            Ok(Some(payload)) => payload.to_string(),
            Ok(None) => {
                page.imp().task_paste_batch_running.set(false);
                update_task_selection_controls(&page);
                show_toast_for_widget(&button_for_result, "Die Zwischenablage enthält keinen Text");
                return;
            }
            Err(error) => {
                page.imp().task_paste_batch_running.set(false);
                update_task_selection_controls(&page);
                show_toast_for_widget(&button_for_result, &format!("Zwischenablage nicht lesbar: {error}"));
                return;
            }
        };
        // Re-resolve the current default source after the asynchronous read.
        let capabilities = page.imp().task_source_capabilities.borrow().clone();
        let source_uids = page.imp().task_source_uids.borrow().clone();
        let preferred = AppSettings::try_new()
            .map(|settings| settings.default_task_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        let current_destination = match task_paste_destination(
            &source_uids,
            &capabilities,
            CalendarStoreService::new().is_writable(),
            preferred.as_deref(),
        ) {
            Ok(current_destination) if current_destination == destination => current_destination,
            Ok(_) | Err(_) => {
                page.imp().task_paste_batch_running.set(false);
                update_task_selection_controls(&page);
                show_toast_for_widget(
                    &button_for_result,
                    "Die Standard-Aufgabenliste hat sich geändert oder ist nicht mehr schreibfähig",
                );
                return;
            }
        };
        let components = match parse_task_clipboard_payload(&payload) {
            Ok(components) => components,
            Err(error) => {
                page.imp().task_paste_batch_running.set(false);
                update_task_selection_controls(&page);
                show_toast_for_widget(&button_for_result, &error);
                return;
            }
        };
        let fresh_uids = components
            .iter()
            .map(|_| format!("evolution-gtk4-{}", uuid::Uuid::new_v4()))
            .collect::<Vec<_>>();
        let existing = task_export_candidates(&task_store)
            .into_iter()
            .map(|candidate| candidate.identity)
            .collect::<Vec<_>>();
        let plan = match plan_task_clipboard_paste_with_uids(
            &payload,
            &current_destination,
            &existing,
            &fresh_uids,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                page.imp().task_paste_batch_running.set(false);
                update_task_selection_controls(&page);
                show_toast_for_widget(&button_for_result, &error);
                return;
            }
        };
        task_list.set_sensitive(false);
        update_task_selection_controls(&page);
        show_toast_for_widget(&button_for_result, "Aufgaben werden eingefügt …");
        let (sender, receiver) = async_channel::bounded(1);
        let plan_for_worker = plan.clone();
        std::thread::spawn(move || {
            let local_writable = CalendarStoreService::new().is_writable();
            let service = ECalService::new();
            let mut result = TaskPasteWorkerResult {
                created: Vec::new(),
                failure: None,
            };
            for mutation in plan_for_worker.mutations {
                if task_source_capability(&mutation.identity.source_uid, &capabilities, local_writable)
                    != TaskSourceCapability::Writable
                {
                    result.failure = Some((
                        mutation.identity,
                        "Die Ziel-Aufgabenliste ist unmittelbar vor dem Einfügen nicht mehr schreibfähig"
                            .to_string(),
                    ));
                    break;
                }
                match service.create_component(
                    &mutation.identity.source_uid,
                    ECalComponentType::Task,
                    &mutation.component,
                ) {
                    Ok(mut confirmed) => {
                        if confirmed
                            .component
                            .as_deref()
                            .is_none_or(|component| component.trim().is_empty())
                        {
                            confirmed.component = load_snapshot_prefer_eds()
                                .snapshot
                                .tasks
                                .into_iter()
                                .find(|task| {
                                    task.source_uid == mutation.identity.source_uid
                                        && task.uid == mutation.identity.task_uid
                                })
                                .map(|task| task.component);
                        }
                        match task_authoritative_component(
                            &mutation.identity.task_uid,
                            &confirmed.uid,
                            confirmed.component,
                        )
                        .and_then(|component| {
                            validated_task_vtodo_component(
                                &TaskExportCandidate {
                                    identity: mutation.identity.clone(),
                                    component,
                                },
                                &mutation.identity,
                            )
                        }) {
                            Ok(component) => result.created.push(TaskPasteMutation {
                                component,
                                ..mutation
                            }),
                            Err(error) => {
                                result.failure = Some((mutation.identity, error));
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        result.failure = Some((mutation.identity, error.to_string()));
                        break;
                    }
                }
            }
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task paste result was dropped");
            }
        });

        let page_weak = page.downgrade();
        let button = button_for_result.clone();
        glib::spawn_future_local(async move {
            let received = receiver.recv().await;
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !task_paste_result_is_current(page.imp().task_paste_batch_generation.get(), generation) {
                return;
            }
            page.imp().task_paste_batch_running.set(false);
            task_list.set_sensitive(true);
            let result = match received {
                Ok(result) => result,
                Err(error) => {
                    tracing::debug!(%error, "Task paste result channel closed");
                    update_task_selection_controls(&page);
                    reload_task_snapshot_after_paste_uncertainty(
                        &page,
                        &task_store,
                        &task_list,
                        &empty_tasks,
                    );
                    show_toast_for_widget(
                        &button,
                        "Einfüge-Ergebnis ging verloren; autoritative Aufgaben werden neu geladen",
                    );
                    return;
                }
            };
            let mut reduction = reduce_task_paste_worker_result(&plan, &result);
            let mut applied = Vec::new();
            let mut recovery_required = reduction.failure.is_some()
                && reduction.created.is_empty()
                && reduction.unattempted.len() == plan.mutations.len();
            for mutation in &reduction.created {
                if task_export_candidates(&task_store)
                    .iter()
                    .any(|candidate| candidate.identity == mutation.identity)
                {
                    reduction.failure = Some((
                        mutation.identity.clone(),
                        "Eine bestätigte neue Aufgabe kollidiert mit der aktuellen Auswahl".to_string(),
                    ));
                    recovery_required = true;
                    continue;
                }
                match task_item_from_confirmed_component(
                    &mutation.identity.source_uid,
                    &mutation.identity.task_uid,
                    &mutation.component,
                ) {
                    Ok(item) => {
                        task_store.append(&item);
                        applied.push(mutation.identity.clone());
                    }
                    Err(error) => {
                        reduction.failure = Some((mutation.identity.clone(), error));
                        recovery_required = true;
                    }
                }
            }
            reduction.created.retain(|mutation| applied.contains(&mutation.identity));
            render_task_rows(&page, &task_list, &task_store, &empty_tasks);
            if recovery_required {
                reload_task_snapshot_after_paste_uncertainty(
                    &page,
                    &task_store,
                    &task_list,
                    &empty_tasks,
                );
            }
            match (reduction.created.len(), reduction.failure.is_some(), reduction.unattempted.len()) {
                (created, false, 0) => page.show_toast(&format!(
                    "{created} Aufgabe{} eingefügt",
                    if created == 1 { "" } else { "n" }
                )),
                (0, true, unattempted) => page.show_toast(&format!(
                    "Keine Aufgabe eingefügt; ein Fehler ist aufgetreten. {unattempted} Aufgabe{} wurden nicht versucht",
                    if unattempted == 1 { "" } else { "n" }
                )),
                (created, true, unattempted) => page.show_toast(&format!(
                    "{created} Aufgabe{} eingefügt; ein Fehler. {unattempted} Aufgabe{} wurden nicht versucht",
                    if created == 1 { "" } else { "n" },
                    if unattempted == 1 { "" } else { "n" }
                )),
                _ => page.show_toast("Das Einfügen lieferte kein bestätigtes Ergebnis"),
            }
        });
    });
}

fn present_task_delete_confirmation(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    let selection = page.imp().task_selection.borrow().clone();
    let plan = match build_task_delete_plan(
        &selection,
        &task_export_candidates(task_store),
        &page.imp().task_source_capabilities.borrow(),
        CalendarStoreService::new().is_writable(),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            show_toast_for_widget(button, &error);
            return;
        }
    };
    let prompt = task_delete_confirmation(&plan);
    let dialog = adw::AlertDialog::new(Some(&prompt.title), Some(&prompt.description));
    dialog.add_responses(&[("cancel", "Nicht löschen"), ("delete", "Löschen")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    let button = button.clone();
    let button_for_dialog = button.clone();
    dialog.connect_response(None, move |dialog, response| {
        if !task_delete_can_dispatch(response == "delete") {
            dialog.close();
            return;
        }
        dialog.close();
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        start_task_delete_batch(
            &page,
            &button,
            &task_store,
            &task_list,
            &empty_tasks,
            &plan,
            false,
        );
    });
    dialog.present(Some(&button_for_dialog));
}

fn present_task_purge_confirmation(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    let plan = match build_task_purge_plan(
        &task_export_candidates(task_store),
        &page.imp().task_source_capabilities.borrow(),
        CalendarStoreService::new().is_writable(),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            show_toast_for_widget(button, &error);
            return;
        }
    };
    let count = plan.mutations.len();
    let dialog = adw::AlertDialog::new(
        Some("Erledigte Aufgaben endgültig löschen?"),
        Some(&format!(
            "{count} erledigte Aufgabe{} werden aus allen schreibfähigen Aufgabenlisten dauerhaft gelöscht und können nicht wiederhergestellt werden.",
            if count == 1 { "" } else { "n" }
        )),
    );
    dialog.add_responses(&[("cancel", "Nicht löschen"), ("delete", "Endgültig löschen")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    let button = button.clone();
    let button_for_dialog = button.clone();
    dialog.connect_response(None, move |dialog, response| {
        dialog.close();
        if response != "delete" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        start_task_delete_batch(
            &page,
            &button,
            &task_store,
            &task_list,
            &empty_tasks,
            &plan,
            true,
        );
    });
    dialog.present(Some(&button_for_dialog));
}

fn apply_task_delete_removals(task_store: &gio::ListStore, identities: &[TaskSelectionIdentity]) {
    let mut indexes = Vec::new();
    for identity in identities {
        let matches = (0..task_store.n_items())
            .filter(|index| {
                task_store
                    .item(*index)
                    .and_downcast::<TaskItemObject>()
                    .is_some_and(|task| {
                        task.source_uid() == identity.source_uid && task.id() == identity.task_uid
                    })
            })
            .collect::<Vec<_>>();
        if let [index] = matches.as_slice() {
            indexes.push(*index);
        }
    }
    indexes.sort_unstable_by(|left, right| right.cmp(left));
    for index in indexes {
        task_store.remove(index);
    }
}

fn reload_task_snapshot_after_delete_uncertainty(
    page: &TasksPage,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    let generation = page.imp().task_delete_batch_generation.get();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let snapshot = load_snapshot_prefer_eds().snapshot;
        if let Err(error) = sender.send_blocking(snapshot) {
            tracing::debug!(%error, "Task delete recovery snapshot was dropped");
        }
    });
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    glib::spawn_future_local(async move {
        let Ok(snapshot) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_delete_result_is_current(page.imp().task_delete_batch_generation.get(), generation)
        {
            return;
        }
        task_store.remove_all();
        for task in snapshot.tasks {
            let priority = if task.priority == 0 {
                "normal".to_string()
            } else {
                task.priority.to_string()
            };
            task_store.append(&TaskItemObject::new(
                &task.source_uid,
                &task.uid,
                &task.summary,
                &task.due,
                &priority,
                task.completed,
                &task.component,
            ));
        }
        render_task_rows(&page, &task_list, &task_store, &empty_tasks);
    });
}

fn start_task_delete_batch(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    confirmed_plan: &TaskDeletePlan,
    purge_completed: bool,
) {
    if task_mutation_batch_running(page) {
        return;
    }
    // Re-resolve after the confirmation rather than deleting a task that
    // changed or disappeared while the dialog was open.
    let selection = page.imp().task_selection.borrow().clone();
    let candidates = task_export_candidates(task_store);
    let current_plan = if purge_completed {
        build_task_purge_plan(
            &candidates,
            &page.imp().task_source_capabilities.borrow(),
            CalendarStoreService::new().is_writable(),
        )
    } else {
        build_task_delete_plan(
            &selection,
            &candidates,
            &page.imp().task_source_capabilities.borrow(),
            CalendarStoreService::new().is_writable(),
        )
    };
    let plan = match current_plan {
        Ok(plan)
            if plan == *confirmed_plan
                && (purge_completed || task_delete_plan_matches_selection(&plan, &selection)) =>
        {
            plan
        }
        Ok(_) => {
            show_toast_for_widget(
                button,
                if purge_completed {
                    "Die Menge erledigter Aufgaben hat sich vor dem Löschen geändert"
                } else {
                    "Die Aufgaben-Auswahl hat sich vor dem Löschen geändert"
                },
            );
            return;
        }
        Err(error) => {
            show_toast_for_widget(button, &error);
            return;
        }
    };
    let generation = page
        .imp()
        .task_delete_batch_generation
        .get()
        .wrapping_add(1);
    page.imp().task_delete_batch_generation.set(generation);
    page.imp().task_delete_batch_running.set(true);
    task_list.set_sensitive(false);
    update_task_selection_controls(page);
    show_toast_for_widget(button, "Aufgaben werden gelöscht …");

    let capabilities = page.imp().task_source_capabilities.borrow().clone();
    let plan_for_worker = plan.clone();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let local_writable = CalendarStoreService::new().is_writable();
        let service = ECalService::new();
        let mut result = TaskDeleteWorkerResult {
            removed: Vec::new(),
            failure: None,
        };
        for mutation in plan_for_worker.mutations {
            if task_source_capability(&mutation.identity.source_uid, &capabilities, local_writable)
                != TaskSourceCapability::Writable
            {
                result.failure = Some((
                    mutation.identity,
                    "Die Aufgabenliste ist unmittelbar vor dem Löschen nicht mehr schreibfähig"
                        .to_string(),
                ));
                break;
            }
            match service.remove_component(
                &mutation.identity.source_uid,
                ECalComponentType::Task,
                &mutation.identity.task_uid,
            ) {
                Ok(confirmed) if confirmed.uid == mutation.identity.task_uid => {
                    result.removed.push(mutation.identity);
                }
                Ok(_) => {
                    result.failure = Some((
                        mutation.identity,
                        "Das Backend bestätigte das Löschen einer anderen Aufgabe".to_string(),
                    ));
                    break;
                }
                Err(error) => {
                    result.failure = Some((mutation.identity, error.to_string()));
                    break;
                }
            }
        }
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Task delete batch result was dropped");
        }
    });

    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    let button = button.clone();
    glib::spawn_future_local(async move {
        let received = receiver.recv().await;
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_delete_result_is_current(page.imp().task_delete_batch_generation.get(), generation)
        {
            return;
        }
        page.imp().task_delete_batch_running.set(false);
        task_list.set_sensitive(true);
        let result = match received {
            Ok(result) => result,
            Err(error) => {
                tracing::debug!(%error, "Task delete batch channel closed");
                update_task_selection_controls(&page);
                reload_task_snapshot_after_delete_uncertainty(
                    &page,
                    &task_store,
                    &task_list,
                    &empty_tasks,
                );
                show_toast_for_widget(
                    &button,
                    "Lösch-Ergebnis ging verloren; autoritative Aufgaben werden neu geladen",
                );
                return;
            }
        };
        let reduction = reduce_task_delete_batch_result(&plan, &result);
        apply_task_delete_removals(&task_store, &reduction.removed);
        render_task_rows(&page, &task_list, &task_store, &empty_tasks);
        match (
            reduction.removed.len(),
            reduction.failure.is_some(),
            reduction.unattempted.len(),
        ) {
            (removed, false, 0) => page.show_toast(&format!(
                "{removed} Aufgabe{} gelöscht",
                if removed == 1 { "" } else { "n" }
            )),
            (0, true, unattempted) => page.show_toast(&format!(
                "Keine Aufgabe gelöscht; ein Fehler ist aufgetreten. {unattempted} Aufgabe{} wurden nicht versucht",
                if unattempted == 1 { "" } else { "n" }
            )),
            (removed, true, unattempted) => page.show_toast(&format!(
                "{removed} Aufgabe{} gelöscht; ein Fehler. {unattempted} Aufgabe{} wurden nicht versucht",
                if removed == 1 { "" } else { "n" },
                if unattempted == 1 { "" } else { "n" }
            )),
            _ => page.show_toast("Das Löschen lieferte kein bestätigtes Ergebnis"),
        }
    });
}

fn refresh_task_category_model(
    page: &TasksPage,
    task_store: &gio::ListStore,
    category_metadata: &[CalendarCategory],
) {
    let Some(model) = page.imp().task_category_model.borrow().as_ref().cloned() else {
        return;
    };
    let Some(selector) = page.imp().task_category_selector.borrow().as_ref().cloned() else {
        return;
    };

    let mut categories = category_metadata
        .iter()
        .cloned()
        .map(|category| category.name)
        .collect::<Vec<_>>();
    let current_filter = page.imp().task_category_filter.borrow().clone();
    for index in 0..task_store.n_items() {
        let Some(task) = task_store.item(index).and_downcast::<TaskItemObject>() else {
            continue;
        };
        categories.extend(category_names_from_ical_component(&task.component()));
    }
    if let Some(current_filter) = current_filter.as_ref() {
        categories.push(current_filter.clone());
    }
    categories.retain(|category| !category.is_empty());
    categories.sort_by_cached_key(|category| category.to_lowercase());
    categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

    let mut names = vec!["Alle Kategorien".to_string()];
    names.extend(categories.clone());
    let selected = current_filter
        .as_deref()
        .and_then(|filter| {
            categories
                .iter()
                .position(|category| category.eq_ignore_ascii_case(filter))
        })
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    model.splice(0, model.n_items(), &name_refs);
    if selector.selected() != selected {
        selector.set_selected(selected);
    }
}

fn compare_tasks(
    left: &TaskItemObject,
    right: &TaskItemObject,
    sort: TaskSort,
    descending: bool,
    context: TaskFilterContext,
) -> std::cmp::Ordering {
    let completed_order = task_is_completed(left).cmp(&task_is_completed(right));
    let ordering = match sort {
        TaskSort::DueDate => compare_optional_dates(
            task_next_due_date(left, context.today),
            task_next_due_date(right, context.today),
        ),
        TaskSort::Title => left
            .title()
            .to_lowercase()
            .cmp(&right.title().to_lowercase()),
        TaskSort::Priority => {
            task_priority_value(&left.priority()).cmp(&task_priority_value(&right.priority()))
        }
    };
    let ordering = if descending {
        ordering.reverse()
    } else {
        ordering
    };
    let title_order = left
        .title()
        .to_lowercase()
        .cmp(&right.title().to_lowercase());
    let title_order = if descending {
        title_order.reverse()
    } else {
        title_order
    };
    completed_order
        .then(ordering)
        .then(title_order)
        .then_with(|| left.id().cmp(&right.id()))
}

fn compare_optional_dates(left: Option<NaiveDate>, right: Option<NaiveDate>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn query_is_empty(query: &TaskQuery) -> bool {
    query.text.is_empty() && query.predicates.is_empty()
}

#[cfg(test)]
fn task_matches_filter(task: &TaskItemObject, filter: TaskFilter, query: &str) -> bool {
    task_matches_filter_with_source(task, filter, None, query)
}

#[cfg(test)]
fn task_matches_filter_with_source(
    task: &TaskItemObject,
    filter: TaskFilter,
    source_filter: Option<&str>,
    query: &str,
) -> bool {
    task_matches_filter_with_source_and_category(task, filter, source_filter, None, query)
}

#[cfg(test)]
fn task_matches_filter_with_source_and_category(
    task: &TaskItemObject,
    filter: TaskFilter,
    source_filter: Option<&str>,
    category_filter: Option<&str>,
    query: &str,
) -> bool {
    let query = TaskQuery::parse(TaskSearchMode::AnyField, query);
    task_matches_filter_with_context(
        task,
        filter,
        source_filter,
        category_filter,
        &query,
        current_task_filter_context(),
    )
}

fn task_matches_filter_with_context(
    task: &TaskItemObject,
    filter: TaskFilter,
    source_filter: Option<&str>,
    category_filter: Option<&str>,
    query: &TaskQuery,
    context: TaskFilterContext,
) -> bool {
    if source_filter.is_some_and(|source_uid| task.source_uid() != source_uid) {
        return false;
    }
    if category_filter.is_some_and(|category_filter| {
        !task_categories(task).any(|category| category.eq_ignore_ascii_case(category_filter))
    }) {
        return false;
    }
    let matches_filter = match filter {
        TaskFilter::All => true,
        TaskFilter::Today => task_matches_due_constraint(task, TaskDateConstraint::Today, context),
        TaskFilter::Next7 => {
            task_matches_due_constraint(task, TaskDateConstraint::Next(7), context)
        }
        TaskFilter::Next30 => {
            task_matches_due_constraint(task, TaskDateConstraint::Next(30), context)
        }
        TaskFilter::Next365 => {
            task_matches_due_constraint(task, TaskDateConstraint::Next(365), context)
                && !task_is_completed(task)
        }
        TaskFilter::Important => task_priority_value(&task.priority()) > 0,
        TaskFilter::Overdue => {
            task_matches_due_constraint(task, TaskDateConstraint::Overdue, context)
        }
        TaskFilter::Scheduled => task_has_due(task),
        TaskFilter::NoDue => !task_has_due(task),
        TaskFilter::WithAttachments => task_has_attachments(task),
        TaskFilter::Started => task_started(task, context),
        TaskFilter::WithoutCategory => task_categories(task).next().is_none(),
        TaskFilter::Uncompleted => !task_is_completed(task),
        TaskFilter::InProgress => task_status_value(&task.component()) == "IN-PROCESS",
        TaskFilter::Cancelled => task_status_value(&task.component()) == "CANCELLED",
        TaskFilter::Completed => task_is_completed(task),
    };
    matches_filter && query.matches(task, context)
}

fn task_query_predicate_matches(
    task: &TaskItemObject,
    predicate: &TaskQueryPredicate,
    context: TaskFilterContext,
) -> bool {
    match predicate {
        TaskQueryPredicate::Category(category) => match category {
            Some(category) => {
                task_categories(task).any(|candidate| candidate.eq_ignore_ascii_case(category))
            }
            None => task_categories(task).next().is_none(),
        },
        TaskQueryPredicate::Status(status) => task_status_value(&task.component()) == status,
        TaskQueryPredicate::Due(constraint) => {
            task_matches_due_constraint(task, *constraint, context)
        }
        TaskQueryPredicate::Start(constraint) => {
            task_matches_start_constraint(task, *constraint, context)
        }
        TaskQueryPredicate::HasAttachments(expected) => task_has_attachments(task) == *expected,
    }
}

fn task_matches_due_constraint(
    task: &TaskItemObject,
    constraint: TaskDateConstraint,
    context: TaskFilterContext,
) -> bool {
    match constraint {
        TaskDateConstraint::None => !task_has_due(task),
        TaskDateConstraint::Today => task_due_in_range(task, context.today, 0),
        TaskDateConstraint::Next(days) => task_due_in_range(task, context.today, days),
        TaskDateConstraint::Overdue => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("Unix epoch is valid");
            task_next_due_date(task, epoch).is_some_and(|due| due >= epoch && due <= context.today)
                && !task_is_completed(task)
        }
        TaskDateConstraint::Scheduled => task_has_due(task),
        TaskDateConstraint::Started => task_started(task, context),
        TaskDateConstraint::Range(start, end) => {
            task_next_due_date(task, start).is_some_and(|due| due >= start && due <= end)
        }
    }
}

fn task_matches_start_constraint(
    task: &TaskItemObject,
    constraint: TaskDateConstraint,
    context: TaskFilterContext,
) -> bool {
    match constraint {
        TaskDateConstraint::None => !task_has_start(task),
        TaskDateConstraint::Today => {
            task_start_local_datetime(task).is_some_and(|start| start.date() == context.today)
        }
        TaskDateConstraint::Next(days) => task_start_local_datetime(task).is_some_and(|start| {
            start.date() >= context.today && start.date() <= context.today + Duration::days(days)
        }),
        TaskDateConstraint::Overdue => {
            task_start_timestamp(task).is_some_and(|start| start < context.now_timestamp)
        }
        TaskDateConstraint::Scheduled => task_has_start(task),
        TaskDateConstraint::Started => task_started(task, context),
        TaskDateConstraint::Range(start, end) => task_start_local_datetime(task)
            .is_some_and(|value| value.date() >= start && value.date() <= end),
    }
}

fn task_due_in_range(task: &TaskItemObject, start: NaiveDate, days: i64) -> bool {
    let Some(end) = start.checked_add_signed(Duration::days(days)) else {
        return false;
    };
    task_next_due_date(task, start).is_some_and(|due| due >= start && due <= end)
}

fn task_categories(task: &TaskItemObject) -> impl Iterator<Item = String> {
    category_names_from_ical_component(&task.component()).into_iter()
}

fn task_has_due(task: &TaskItemObject) -> bool {
    !ical_properties(&task.component(), "DUE").is_empty() || !task.due_date().trim().is_empty()
}

fn task_has_attachments(task: &TaskItemObject) -> bool {
    !ical_properties(&task.component(), "ATTACH").is_empty()
}

fn task_start_local_datetime(task: &TaskItemObject) -> Option<NaiveDateTime> {
    property_local_datetime(&task.component(), "DTSTART", false)
}

fn task_start_timestamp(task: &TaskItemObject) -> Option<i64> {
    let (property, value) = ical_property_with_parameters(&task.component(), "DTSTART")?;
    property_local_timestamp(&property, &value, 0)
}

fn task_has_start(task: &TaskItemObject) -> bool {
    task_start_local_datetime(task).is_some()
}

fn task_started(task: &TaskItemObject, context: TaskFilterContext) -> bool {
    task_start_timestamp(task).is_none_or(|start| start <= context.now_timestamp)
}

fn task_due_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value.trim(), "%Y%m%d"))
        .ok()
}

fn task_excluded_dates(task: &TaskItemObject) -> Vec<NaiveDate> {
    property_local_dates(&task.component(), "EXDATE")
}

fn task_additional_dates(task: &TaskItemObject) -> Vec<NaiveDate> {
    property_local_dates(&task.component(), "RDATE")
}

fn task_due_local_date(task: &TaskItemObject) -> Option<NaiveDate> {
    property_local_date(&task.component(), "DUE", false).or_else(|| task_due_date(&task.due_date()))
}

fn task_next_due_date(task: &TaskItemObject, from: NaiveDate) -> Option<NaiveDate> {
    let excluded_dates = task_excluded_dates(task);
    let rule_next = task_due_local_date(task).and_then(|due| {
        let Some(raw_rule) = ical_property(&task.component(), "RRULE") else {
            return Some(due);
        };
        let Some(rule) = parse_recurrence_rule(&raw_rule) else {
            return Some(due);
        };
        next_recurrence_date(&rule, due, from, &excluded_dates)
    });
    let rdate_next = task_additional_dates(task)
        .into_iter()
        .filter(|date| *date >= from && !excluded_dates.contains(date))
        .min();
    match (rule_next, rdate_next) {
        (Some(rule_next), Some(rdate_next)) => Some(rule_next.min(rdate_next)),
        (Some(rule_next), None) => Some(rule_next),
        (None, Some(rdate_next)) => Some(rdate_next),
        (None, None) => None,
    }
}

fn normalize_task_date_list(value: &str, field: &str) -> Result<String, String> {
    if value
        .chars()
        .any(|character| character == '\r' || character == '\n')
    {
        return Err(format!("{field} enthält ungültige Zeilenumbrüche"));
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            task_due_date(value)
                .or_else(|| parse_ical_date(value))
                .map(|date| date.format("%Y%m%d").to_string())
                .ok_or_else(|| {
                    format!("{field} muss gültige YYYY-MM-DD- oder YYYYMMDD-Werte enthalten")
                })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join(","))
}

fn task_status_value(raw: &str) -> &'static str {
    let status = ical_property(raw, "STATUS")
        .unwrap_or_default()
        .to_ascii_uppercase();
    match status.as_str() {
        "IN-PROCESS" => "IN-PROCESS",
        "COMPLETED" => "COMPLETED",
        "CANCELLED" => "CANCELLED",
        _ => "NEEDS-ACTION",
    }
}

fn task_status_index(raw: &str) -> u32 {
    match task_status_value(raw) {
        "IN-PROCESS" => 1,
        "COMPLETED" => 2,
        "CANCELLED" => 3,
        _ => 0,
    }
}

fn task_status_from_index(index: u32) -> &'static str {
    match index {
        1 => "IN-PROCESS",
        2 => "COMPLETED",
        3 => "CANCELLED",
        _ => "NEEDS-ACTION",
    }
}

fn task_status_label(raw: &str) -> &'static str {
    if ical_property(raw, "STATUS")
        .as_deref()
        .is_some_and(|status| normalize_task_status(status).is_err())
    {
        return "Importierter Status";
    }
    match task_status_value(raw) {
        "IN-PROCESS" => "In Bearbeitung",
        "COMPLETED" => "Erledigt",
        "CANCELLED" => "Abgebrochen",
        _ => "Nicht begonnen",
    }
}

fn parse_task_percent(raw: &str) -> u8 {
    let default = (task_status_value(raw) == "COMPLETED")
        .then_some(100)
        .unwrap_or(0);
    ical_property(raw, "PERCENT-COMPLETE")
        .and_then(|value| parse_task_percent_value(&value))
        .unwrap_or(default)
}

fn parse_task_percent_value(value: &str) -> Option<u8> {
    let value = value.trim();
    let percent = value.parse::<u8>().ok()?;
    (percent <= 100).then_some(percent)
}

fn normalize_task_status(value: &str) -> Result<&'static str, String> {
    match value.to_ascii_uppercase().as_str() {
        "NEEDS-ACTION" => Ok("NEEDS-ACTION"),
        "IN-PROCESS" => Ok("IN-PROCESS"),
        "COMPLETED" => Ok("COMPLETED"),
        "CANCELLED" => Ok("CANCELLED"),
        _ => Err("Der Aufgabenstatus ist ungültig".to_string()),
    }
}

fn upsert_task_row(
    page: &TasksPage,
    task_list: &gtk::ListBox,
    task_store: &gio::ListStore,
    empty_tasks: &gtk::Label,
    task: TaskRecord,
) {
    upsert_task_store(task_list, task_store, task);
    render_task_rows(page, task_list, task_store, empty_tasks);
}

fn upsert_task_store(task_list: &gtk::ListBox, task_store: &gio::ListStore, task: TaskRecord) {
    let existing_index = (0..task_store.n_items()).find(|index| {
        task_store
            .item(*index)
            .and_downcast::<TaskItemObject>()
            .is_some_and(|existing| {
                existing.id() == task.uid && existing.source_uid() == task.source_uid
            })
    });
    if let Some(index) = existing_index {
        task_store.remove(index);
        remove_task_row(task_list, &task.source_uid, &task.uid);
    }

    let priority = if task.priority == 0 {
        "normal".to_string()
    } else {
        task.priority.to_string()
    };
    let task_object = TaskItemObject::new(
        &task.source_uid,
        &task.uid,
        &task.summary,
        &task.due,
        &priority,
        task.completed,
        &task.component,
    );
    task_store.append(&task_object);
}

fn remove_task_record(
    page: &TasksPage,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    source_uid: &str,
    task_uid: &str,
) {
    let store_index = (0..task_store.n_items()).find(|index| {
        task_store
            .item(*index)
            .and_downcast::<TaskItemObject>()
            .is_some_and(|task| task.id() == task_uid && task.source_uid() == source_uid)
    });
    if let Some(index) = store_index {
        task_store.remove(index);
    }
    render_task_rows(page, task_list, task_store, empty_tasks);
}

fn append_task_row(
    page: &TasksPage,
    task_list: &gtk::ListBox,
    task_store: &gio::ListStore,
    empty_tasks: &gtk::Label,
    task: &TaskItemObject,
    context: TaskFilterContext,
    category_metadata: &[CalendarCategory],
) {
    let title = glib::markup_escape_text(&task.title());
    let due = task.due_date();
    let recurrence_due = task_next_due_date(task, context.today);
    let due_text = match (task_due_date(&due), recurrence_due) {
        (Some(base), Some(next)) if base != next => format!(
            "Fällig: {} · Nächste Fälligkeit: {}",
            due,
            next.format("%Y-%m-%d")
        ),
        (Some(_), None) if ical_property(&task.component(), "RRULE").is_some() => {
            format!("Fällig: {due} · Serie beendet")
        }
        (None, Some(next)) => format!("Nächste Fälligkeit: {}", next.format("%Y-%m-%d")),
        _ => format!("Fällig: {due}"),
    };
    let subtitle = glib::markup_escape_text(&format!(
        "{due_text} · Priorität: {} · {}",
        task.priority(),
        task_status_label(&task.component())
    ));
    let row = adw::ActionRow::builder()
        .title(title.as_str())
        .subtitle(subtitle.as_str())
        .use_markup(true)
        .build();
    row.set_activatable(true);
    unsafe {
        row.set_data("task-id", task.id());
        row.set_data("task-source-uid", task.source_uid());
    }
    if let Some(color) = category_color_for_component(category_metadata, &task.component()) {
        let category_dot = gtk::Label::new(None);
        category_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
        category_dot.set_tooltip_text(Some("Aufgabenkategorie"));
        row.add_prefix(&category_dot);
    }

    let completed = task_is_completed(task);
    let check = gtk::CheckButton::builder()
        .active(completed)
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(
        &check,
        &if completed {
            format!("Aufgabe als offen markieren: {}", task.title())
        } else {
            format!("Aufgabe als erledigt markieren: {}", task.title())
        },
    );
    let task_clone = task.clone();
    let source_uid = task.source_uid();
    let updating = Rc::new(Cell::new(false));
    let updating_for_callback = Rc::clone(&updating);
    let page_weak_for_check = page.downgrade();
    let task_list_for_check = task_list.clone();
    let task_store_for_check = task_store.clone();
    let empty_tasks_for_check = empty_tasks.clone();
    check.connect_toggled(move |button| {
        if updating_for_callback.get() {
            return;
        }
        let completed = button.is_active();
        let Some(page) = page_weak_for_check.upgrade() else {
            return;
        };
        if !task_source_is_writable_on_page(&page, &source_uid) {
            updating_for_callback.set(true);
            button.set_active(!completed);
            updating_for_callback.set(false);
            show_toast_for_widget(
                button,
                "Diese Aufgabenliste ist nicht als schreibfähig bestätigt",
            );
            return;
        }

        let status = if completed {
            "COMPLETED"
        } else {
            "NEEDS-ACTION"
        };
        let percent = if completed { "100" } else { "0" };
        let mut component = update_ical_property(&task_clone.component(), "STATUS", Some(status));
        component = update_ical_property(&component, "PERCENT-COMPLETE", Some(percent));
        component = if completed {
            update_ical_property(
                &component,
                "COMPLETED",
                Some(&Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
            )
        } else {
            update_ical_property(&component, "COMPLETED", None)
        };
        let uid = task_clone.id();
        let uid_for_worker = uid.clone();
        let source_uid_for_worker = source_uid.clone();
        button.set_sensitive(false);
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = ECalService::new().modify_component(
                &source_uid_for_worker,
                ECalComponentType::Task,
                &uid_for_worker,
                &component,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task status mutation result was dropped");
            }
        });

        let task_for_result = task_clone.clone();
        let button_for_result = button.clone();
        let updating_for_result = Rc::clone(&updating_for_callback);
        let page_for_result = page_weak_for_check.clone();
        let task_list_for_result = task_list_for_check.clone();
        let task_store_for_result = task_store_for_check.clone();
        let empty_tasks_for_result = empty_tasks_for_check.clone();
        glib::spawn_future_local(async move {
            let result = match receiver.recv().await {
                Ok(result) => result,
                Err(error) => {
                    button_for_result.set_sensitive(true);
                    tracing::debug!(%error, "Task status mutation channel closed");
                    show_toast_for_widget(
                        &button_for_result,
                        "Aufgabenstatus konnte nicht aktualisiert werden",
                    );
                    return;
                }
            };
            match result {
                Ok(result) => {
                    let component =
                        match task_authoritative_component(&uid, &result.uid, result.component) {
                            Ok(component) => component,
                            Err(error) => {
                                updating_for_result.set(true);
                                button_for_result.set_active(!completed);
                                updating_for_result.set(false);
                                show_toast_for_widget(&button_for_result, &error);
                                return;
                            }
                        };
                    task_for_result.set_completed(completed);
                    task_for_result.set_component(component);
                    if let Some(page) = page_for_result.upgrade() {
                        render_task_rows(
                            &page,
                            &task_list_for_result,
                            &task_store_for_result,
                            &empty_tasks_for_result,
                        );
                    }
                }
                Err(error) => {
                    updating_for_result.set(true);
                    button_for_result.set_active(!completed);
                    updating_for_result.set(false);
                    show_toast_for_widget(
                        &button_for_result,
                        &format!("Aufgabe konnte nicht aktualisiert werden: {error}"),
                    );
                }
            }
            button_for_result.set_sensitive(true);
        });
    });
    row.add_prefix(&check);

    let quick_edit_button = gtk::Button::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Status, Priorität, Fälligkeit und Kategorien schnell bearbeiten")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .sensitive(task_source_is_writable_on_page(page, &task.source_uid()))
        .build();
    set_accessible_label(
        &quick_edit_button,
        "Status, Priorität, Fälligkeit und Kategorien schnell bearbeiten",
    );
    row.add_suffix(&quick_edit_button);
    connect_task_quick_editor(
        page,
        &quick_edit_button,
        task_store,
        task_list,
        empty_tasks,
        task,
    );

    let edit_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Aufgabe bearbeiten")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&edit_button, "Aufgabe bearbeiten");
    row.add_suffix(&edit_button);
    let page_weak_for_edit = page.downgrade();
    let task_for_edit = task.clone();
    let task_store_for_edit = task_store.clone();
    let task_list_for_edit = task_list.clone();
    let empty_tasks_for_edit = empty_tasks.clone();
    let edit_button_for_signal = edit_button.clone();
    let edit_button_for_closure = edit_button.clone();
    edit_button_for_signal.connect_clicked(move |_| {
        let Some(page) = page_weak_for_edit.upgrade() else {
            return;
        };
        if !task_source_is_writable_on_page(&page, &task_for_edit.source_uid()) {
            show_toast_for_widget(
                &edit_button_for_closure,
                "Diese Aufgabenliste ist nicht als schreibfähig bestätigt",
            );
            return;
        }
        page.open_task_dialog(
            task_store_for_edit.clone(),
            task_list_for_edit.clone(),
            empty_tasks_for_edit.clone(),
            Some(task_for_edit.clone()),
        );
    });

    let export_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Aufgabe als iCalendar exportieren")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&export_button, "Aufgabe als iCalendar exportieren");
    row.add_suffix(&export_button);
    let task_component_for_export = task.component();
    export_button.connect_clicked(move |button| {
        if task_component_for_export.trim().is_empty() {
            show_toast_for_widget(
                button,
                "Für diese Aufgabe sind keine iCalendar-Daten verfügbar",
            );
            return;
        }
        let Some(root) = button.root() else {
            return;
        };
        let Ok(window) = root.downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("Aufgabe als iCalendar speichern")
            .initial_name("aufgabe.ics")
            .modal(true)
            .build();
        let component = task_component_for_export.clone();
        let button_for_result = button.clone();
        file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let save_result = file.replace_contents(
                export_ical_component(&component).as_bytes(),
                None,
                true,
                gio::FileCreateFlags::NONE,
                None::<&gio::Cancellable>,
            );
            match save_result {
                Ok(_) => {
                    show_toast_for_widget(&button_for_result, "Aufgabe als iCalendar exportiert")
                }
                Err(error) => show_toast_for_widget(
                    &button_for_result,
                    &format!("iCalendar konnte nicht gespeichert werden: {error}"),
                ),
            }
        });
    });

    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Aufgabe löschen")
        .css_classes(vec!["flat", "destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&delete_button, "Aufgabe löschen");
    row.add_suffix(&delete_button);
    let task_store_for_delete = task_store.clone();
    let task_list_for_delete = task_list.clone();
    let empty_tasks_for_delete = empty_tasks.clone();
    let delete_button_for_context = delete_button.clone();
    let delete_button_for_signal = delete_button.clone();
    let page_weak_for_delete = page.downgrade();
    delete_button_for_signal.connect_clicked(move |button| {
        let Some(page) = page_weak_for_delete.upgrade() else {
            return;
        };
        let Some(row) = button
            .ancestor(gtk::ListBoxRow::static_type())
            .and_downcast::<gtk::ListBoxRow>()
        else {
            show_toast_for_widget(button, "Die Aufgabe ist nicht mehr verfügbar");
            return;
        };
        // Row and context deletion must target the clicked task, not a prior
        // multi-selection.  Route it through the same preflight, confirmation,
        // iTIP/recurrence boundary, and sequential worker as the toolbar.
        task_list_for_delete.unselect_all();
        task_list_for_delete.select_row(Some(&row));
        page.imp()
            .task_selection
            .replace(task_selection_from_list(&task_list_for_delete));
        update_task_selection_controls(&page);
        present_task_delete_confirmation(
            &page,
            button,
            &task_store_for_delete,
            &task_list_for_delete,
            &empty_tasks_for_delete,
        );
    });

    // Evolution exposes the same task actions from the row context menu as
    // from its toolbar.  Keep the menu tied to the row's existing buttons so
    // confirmation, read-only checks, exports, and ECal mutations continue
    // to share one implementation.
    let context_click = gtk::GestureClick::builder().button(3).build();
    let row_weak_for_context = row.downgrade();
    let task_list_for_context = task_list.clone();
    let check_for_context = check.clone();
    let edit_button_for_context = edit_button.clone();
    let export_button_for_context = export_button.clone();
    let task_for_context = task.clone();
    context_click.connect_pressed(move |_, _, x, y| {
        let Some(row_for_context) = row_weak_for_context.upgrade() else {
            return;
        };
        if let Some(list_row) = row_for_context
            .ancestor(gtk::ListBoxRow::static_type())
            .and_downcast::<gtk::ListBoxRow>()
        {
            // Context actions always operate on the task under the pointer,
            // not an earlier multi-selection.
            task_list_for_context.unselect_all();
            task_list_for_context.select_row(Some(&list_row));
        }

        let popover = gtk::Popover::builder()
            .has_arrow(false)
            .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
            .build();
        let menu_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let edit = gtk::Button::with_label("Aufgabe bearbeiten");
        edit.set_halign(gtk::Align::Fill);
        edit.add_css_class("flat");
        let popover_for_edit = popover.clone();
        let edit_button_for_menu = edit_button_for_context.clone();
        edit.connect_clicked(move |_| {
            popover_for_edit.popdown();
            edit_button_for_menu.activate();
        });
        menu_box.append(&edit);

        let toggle = gtk::Button::with_label(if task_is_completed(&task_for_context) {
            "Als offen markieren"
        } else {
            "Als erledigt markieren"
        });
        toggle.set_halign(gtk::Align::Fill);
        toggle.add_css_class("flat");
        toggle.set_sensitive(!task_for_context.source_uid().is_empty());
        let popover_for_toggle = popover.clone();
        let check_for_menu = check_for_context.clone();
        toggle.connect_clicked(move |_| {
            popover_for_toggle.popdown();
            check_for_menu.activate();
        });
        menu_box.append(&toggle);

        let export = gtk::Button::with_label("Als iCalendar exportieren");
        export.set_halign(gtk::Align::Fill);
        export.add_css_class("flat");
        let popover_for_export = popover.clone();
        let export_button_for_menu = export_button_for_context.clone();
        export.connect_clicked(move |_| {
            popover_for_export.popdown();
            export_button_for_menu.activate();
        });
        menu_box.append(&export);

        let delete = gtk::Button::with_label("Aufgabe löschen");
        delete.set_halign(gtk::Align::Fill);
        delete.add_css_class("flat");
        delete.add_css_class("destructive-action");
        let popover_for_delete = popover.clone();
        let delete_button_for_menu = delete_button_for_context.clone();
        delete.connect_clicked(move |_| {
            popover_for_delete.popdown();
            delete_button_for_menu.activate();
        });
        menu_box.append(&delete);

        popover.set_child(Some(&menu_box));
        popover.set_parent(&row_for_context);
        let popover_weak = popover.downgrade();
        popover.connect_closed(move |_| {
            if let Some(popover) = popover_weak.upgrade() {
                popover.unparent();
            }
        });
        popover.popup();
    });
    row.add_controller(context_click);
    task_list.append(&row);
}

fn category_color_for_component(
    category_metadata: &[CalendarCategory],
    component: &str,
) -> Option<String> {
    category_names_from_ical_component(component)
        .into_iter()
        .find_map(|name| {
            category_metadata
                .iter()
                .find(|category| category.name.eq_ignore_ascii_case(&name))
                .map(|category| category.color.clone())
        })
}

fn remove_task_row(task_list: &gtk::ListBox, source_uid: &str, task_uid: &str) {
    let mut child = task_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let Some(task_row) = row
            .child()
            .and_then(|child| child.downcast::<adw::ActionRow>().ok())
        else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = task_row
                .data::<String>("task-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_task_uid = task_row
                .data::<String>("task-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_task_uid == task_uid
        };
        if matches {
            task_list.remove(&row);
            return;
        }
    }
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Task UI result");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskSourceCapability {
    Writable,
    ReadOnly,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskSourceActionState {
    copy_enabled: bool,
    rename_enabled: bool,
    properties_enabled: bool,
    delete_enabled: bool,
}

fn task_source_action_state(
    primary_source: Option<&str>,
    metadata: &HashMap<String, ECalSourceCapability>,
    operation_running: bool,
) -> TaskSourceActionState {
    let source = primary_source.and_then(|uid| metadata.get(uid));
    TaskSourceActionState {
        // Evolution enables Copy for any exact primary ESource.  The
        // app-owned portable fallback is not an ESource and is therefore not
        // projected into this authoritative metadata map.
        copy_enabled: source.is_some() && !operation_running,
        rename_enabled: source
            .is_some_and(|source| source.source_writable && !source.in_collection)
            && !operation_running,
        properties_enabled: source.is_some_and(|source| source.source_writable)
            && !operation_running,
        delete_enabled: source.is_some_and(|source| source.removable || source.remote_deletable)
            && !operation_running,
    }
}

fn start_task_source_discovery(
    page: &TasksPage,
    preferred_source_uid: Option<String>,
    show_error: bool,
) {
    let generation = page
        .imp()
        .task_source_discovery_generation
        .get()
        .wrapping_add(1);
    page.imp().task_source_discovery_generation.set(generation);
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let discovery = ECalService::new().discover_source_capabilities(ECalComponentType::Task);
        if let Err(error) = sender.send_blocking(discovery) {
            tracing::debug!(%error, "Task source capability result was dropped");
        }
    });

    let page_weak = page.downgrade();
    glib::spawn_future_local(async move {
        let (mut sources, warning) = match receiver.recv().await {
            Ok(Ok(discovery)) => (discovery.sources, discovery.warning),
            Ok(Err(error)) => {
                tracing::warn!(%error, "Could not discover exact EDS task lists");
                if let Some(page) = page_weak.upgrade()
                    && show_error
                {
                    page.show_toast(&format!(
                        "Aufgabenlisten konnten nicht neu eingelesen werden: {error}"
                    ));
                    return;
                }
                (Vec::new(), None)
            }
            Err(error) => {
                tracing::debug!(%error, "Task source capability channel closed");
                return;
            }
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if page.imp().task_source_discovery_generation.get() != generation {
            return;
        }
        if let Some(warning) = warning.as_deref() {
            tracing::warn!(%warning, "Some EDS task lists are unavailable");
        }
        sources.sort_by_cached_key(|source| {
            (
                source.display_name.to_lowercase(),
                source.source_uid.clone(),
            )
        });

        let mut names = vec!["Alle Aufgabenlisten".to_string()];
        let mut uids = Vec::new();
        let mut capabilities = HashMap::new();
        let mut metadata = HashMap::new();
        let mut refresh_unsupported = HashSet::new();
        for source in sources {
            if source.source_uid.trim().is_empty()
                || metadata.contains_key(source.source_uid.as_str())
            {
                continue;
            }
            names.push(task_source_capability_label(&source));
            let capability = task_source_capability_from_ecal(&source);
            if source.connected && !source.refresh_supported {
                refresh_unsupported.insert(source.source_uid.clone());
            }
            capabilities.insert(source.source_uid.clone(), capability);
            uids.push(source.source_uid.clone());
            metadata.insert(source.source_uid.clone(), source);
        }
        if let Some(preferred_source_uid) = preferred_source_uid.as_deref()
            && !uids.iter().any(|uid| uid == preferred_source_uid)
            && preferred_source_uid != "local-calendar"
        {
            names.push(format!("Nicht verfügbare Quelle · {preferred_source_uid}"));
            uids.push(preferred_source_uid.to_string());
            capabilities.insert(
                preferred_source_uid.to_string(),
                TaskSourceCapability::Unavailable,
            );
        }
        if CalendarStoreService::new().is_writable() {
            names.push("Lokale Aufgabenliste".to_string());
            capabilities.insert("local-calendar".to_string(), TaskSourceCapability::Writable);
            uids.push("local-calendar".to_string());
        }

        page.imp().task_source_capabilities.replace(capabilities);
        page.imp().task_source_metadata.replace(metadata);
        page.imp()
            .task_refresh_unsupported_sources
            .replace(refresh_unsupported);
        page.imp().task_source_uids.replace(uids.clone());

        let Some(source_model) = page.imp().task_source_model.borrow().as_ref().cloned() else {
            return;
        };
        let Some(source_selector) = page.imp().task_source_selector.borrow().as_ref().cloned()
        else {
            return;
        };
        let Some(source_list) = page.imp().task_source_list.borrow().as_ref().cloned() else {
            return;
        };
        let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        source_model.splice(0, source_model.n_items(), &name_refs);
        while let Some(child) = source_list.first_child() {
            source_list.remove(&child);
        }
        for (index, name) in names.iter().enumerate() {
            let escaped = glib::markup_escape_text(name);
            let row = adw::ActionRow::builder().title(escaped.as_str()).build();
            let image = gtk::Image::from_icon_name(if index == 0 {
                "view-list-symbolic"
            } else {
                "view-list-details-symbolic"
            });
            row.add_prefix(&image);
            source_list.append(&row);
        }
        let selected = preferred_source_uid
            .as_deref()
            .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
            .map(|index| index as u32 + 1)
            .unwrap_or(0);
        source_selector.set_selected(selected);
        if let Some(row) = source_list.row_at_index(selected as i32) {
            source_list.select_row(Some(&row));
        }
        update_task_selection_controls(&page);
    });
}

fn current_task_source_metadata(page: &TasksPage) -> Option<(String, ECalSourceCapability)> {
    let source_uid = page.imp().task_source_filter.borrow().clone()?;
    let source = page
        .imp()
        .task_source_metadata
        .borrow()
        .get(&source_uid)
        .cloned()?;
    Some((source_uid, source))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskAssignmentDraft {
    organizer: String,
    attendees: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskAssignmentUpdate {
    organizer_line: String,
    attendee_lines: Vec<String>,
}

fn task_top_level_property_lines(component: &str, property_name: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut primary_seen = false;
    let mut nested_depth = 0usize;
    for line in unfold_ical_lines(component) {
        if !primary_seen {
            if line.eq_ignore_ascii_case("BEGIN:VTODO") {
                primary_seen = true;
            }
            continue;
        }
        if nested_depth == 0 && line.eq_ignore_ascii_case("END:VTODO") {
            break;
        }
        if nested_depth == 0
            && line.split_once(':').is_some_and(|(property, _)| {
                property
                    .split(';')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(property_name))
            })
        {
            result.push(line.clone());
        }
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
    result
}

fn task_assignment_draft(component: &str) -> TaskAssignmentDraft {
    TaskAssignmentDraft {
        organizer: task_top_level_property_lines(component, "ORGANIZER")
            .into_iter()
            .next()
            .unwrap_or_default(),
        attendees: task_top_level_property_lines(component, "ATTENDEE").join("\n"),
    }
}

fn normalize_task_assignment_property(input: &str, property_name: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty()
        || input
            .chars()
            .any(|character| matches!(character, '\r' | '\n'))
    {
        return Err(format!("{property_name} darf nicht leer sein"));
    }
    if let Some((property, value)) = input.split_once(':') {
        let actual_name = property.split(';').next().unwrap_or_default();
        if !actual_name.eq_ignore_ascii_case(property_name)
            || property.is_empty()
            || value.trim().is_empty()
        {
            return Err(format!("Ungültige {property_name}-Eigenschaft"));
        }
        return Ok(format!("{property}:{}", value.trim()));
    }
    let address = input
        .strip_prefix("mailto:")
        .or_else(|| input.strip_prefix("MAILTO:"))
        .unwrap_or(input)
        .trim();
    let Some((local, domain)) = address.rsplit_once('@') else {
        return Err(format!(
            "{property_name} benötigt eine gültige E-Mail-Adresse"
        ));
    };
    if local.is_empty()
        || domain.is_empty()
        || address
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, ',' | ';' | ':'))
    {
        return Err(format!(
            "{property_name} benötigt eine gültige E-Mail-Adresse"
        ));
    }
    if property_name.eq_ignore_ascii_case("ATTENDEE") {
        Ok(format!(
            "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{address}"
        ))
    } else {
        Ok(format!("ORGANIZER:mailto:{address}"))
    }
}

fn parse_task_assignment_update(
    organizer: &str,
    attendees: &str,
) -> Result<TaskAssignmentUpdate, String> {
    let organizer_line = normalize_task_assignment_property(organizer, "ORGANIZER")?;
    let mut attendee_lines = Vec::new();
    for input in attendees
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let line = normalize_task_assignment_property(input, "ATTENDEE")?;
        if !attendee_lines
            .iter()
            .any(|current: &String| current.eq_ignore_ascii_case(&line))
        {
            attendee_lines.push(line);
        }
    }
    if attendee_lines.is_empty() {
        return Err("Mindestens eine teilnehmende Person ist erforderlich".to_string());
    }
    Ok(TaskAssignmentUpdate {
        organizer_line,
        attendee_lines,
    })
}

fn replace_task_assignment_properties(component: &str, update: &TaskAssignmentUpdate) -> String {
    let current_organizer = task_top_level_property_lines(component, "ORGANIZER");
    let current_attendees = task_top_level_property_lines(component, "ATTENDEE");
    if current_organizer.as_slice() == [update.organizer_line.as_str()]
        && current_attendees == update.attendee_lines
    {
        return component.to_string();
    }
    let mut output = Vec::new();
    let mut primary_seen = false;
    let mut nested_depth = 0usize;
    let mut inserted = false;
    for line in unfold_ical_lines(component) {
        if !primary_seen {
            output.push(line.clone());
            if line.eq_ignore_ascii_case("BEGIN:VTODO") {
                primary_seen = true;
            }
            continue;
        }
        let is_assignment_property = nested_depth == 0
            && line.split_once(':').is_some_and(|(property, _)| {
                property.split(';').next().is_some_and(|name| {
                    name.eq_ignore_ascii_case("ORGANIZER") || name.eq_ignore_ascii_case("ATTENDEE")
                })
            });
        if is_assignment_property {
            if !inserted {
                output.push(update.organizer_line.clone());
                output.extend(update.attendee_lines.iter().cloned());
                inserted = true;
            }
            continue;
        }
        if nested_depth == 0 && line.eq_ignore_ascii_case("END:VTODO") {
            if !inserted {
                output.push(update.organizer_line.clone());
                output.extend(update.attendee_lines.iter().cloned());
                inserted = true;
            }
            output.push(line);
            continue;
        }
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth = nested_depth.saturating_add(1);
        }
        output.push(line.clone());
        if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            nested_depth = nested_depth.saturating_sub(1);
        }
    }
    if !inserted {
        output.push(update.organizer_line.clone());
        output.extend(update.attendee_lines.iter().cloned());
    }
    fold_task_ical_component(&format!("{}\r\n", output.join("\r\n")))
}

fn build_task_assignment_request(component: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Evolution GTK4//Task Assignment//EN\r\nMETHOD:REQUEST\r\n{}\r\nEND:VCALENDAR\r\n",
        component.trim()
    )
}

fn present_task_assignment_dialog(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
) {
    let selection = page.imp().task_selection.borrow().clone();
    let candidates = task_export_candidates(task_store);
    if !task_assignment_action_enabled(
        &selection,
        &candidates,
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    ) {
        show_toast_for_widget(
            button,
            "Diese Aufgabe oder ihr Anbieter unterstützt keine Zuweisung",
        );
        return;
    }
    let Ok(candidate) = selected_task_candidate(&selection, &candidates) else {
        return;
    };
    let Ok(component) = validated_task_vtodo_component(candidate, &candidate.identity) else {
        show_toast_for_widget(button, "Die ausgewählte Aufgabe ist nicht mehr gültig");
        return;
    };
    let identity = candidate.identity.clone();
    let initial_component = component.clone();
    let draft = task_assignment_draft(&component);
    let organizer_entry = gtk::Entry::builder()
        .text(&draft.organizer)
        .placeholder_text("organizer@example.org oder vollständige ORGANIZER-Zeile")
        .hexpand(true)
        .build();
    let attendee_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::None)
        .height_request(120)
        .build();
    attendee_view.buffer().set_text(&draft.attendees);
    let attendee_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(100)
        .child(&attendee_view)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Organisator")
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&organizer_entry);
    content.append(
        &gtk::Label::builder()
            .label("Teilnehmende (eine E-Mail- oder vollständige ATTENDEE-Zeile pro Zeile)")
            .wrap(true)
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&attendee_scroll);
    content.append(
        &gtk::Label::builder()
            .label("Die Zuweisungsdaten werden gespeichert. Der Versand von iTIP-Benachrichtigungen benötigt die native Linux-/EDS-Laufzeit.")
            .wrap(true)
            .xalign(0.0)
            .css_classes(vec!["dim-label"])
            .build(),
    );
    let dialog = adw::AlertDialog::new(Some("Aufgabe zuweisen"), None);
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("assign", "Zuweisen");
    dialog.set_response_appearance("assign", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("assign"));
    dialog.set_close_response("cancel");
    let page_weak = page.downgrade();
    let task_store = task_store.clone();
    let task_list = task_list.clone();
    let empty_tasks = empty_tasks.clone();
    let assignment_button = button.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "assign" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let current_candidates = task_export_candidates(&task_store);
        if !task_assignment_action_enabled(
            std::slice::from_ref(&identity),
            &current_candidates,
            &page.imp().task_source_metadata.borrow(),
            task_mutation_batch_running(&page),
        ) {
            page.show_toast("Die Aufgabe oder Anbieter-Capability hat sich geändert");
            return;
        }
        let Ok(current) = selected_task_candidate(
            std::slice::from_ref(&identity),
            &current_candidates,
        ) else {
            page.show_toast("Die Aufgabe ist nicht mehr verfügbar");
            return;
        };
        let Ok(current_component) = validated_task_vtodo_component(current, &identity) else {
            page.show_toast("Die Aufgabe ist nicht mehr gültig");
            return;
        };
        if current_component != initial_component {
            page.show_toast("Die Aufgabe wurde inzwischen geändert; Zuweisung bitte erneut öffnen");
            return;
        }
        let buffer = attendee_view.buffer();
        let attendees = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        let update = match parse_task_assignment_update(&organizer_entry.text(), &attendees) {
            Ok(update) => update,
            Err(error) => {
                page.show_toast(&error);
                return;
            }
        };
        let component = replace_task_assignment_properties(&current_component, &update);
        let request_payload = build_task_assignment_request(&component);
        let generation = page.imp().task_assignment_generation.get().wrapping_add(1);
        page.imp().task_assignment_generation.set(generation);
        page.imp().task_assignment_running.set(true);
        update_task_selection_controls(&page);
        let identity_for_worker = identity.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = ECalService::new()
                .modify_component(
                    &identity_for_worker.source_uid,
                    ECalComponentType::Task,
                    &identity_for_worker.task_uid,
                    &component,
                )
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    let reload = load_snapshot_prefer_eds();
                    let confirmed = reload.origin == CalendarDataOrigin::Eds
                        && reload.warning.is_none()
                        && reload.snapshot.tasks.iter().any(|task| {
                            task.source_uid == identity_for_worker.source_uid
                                && task.uid == identity_for_worker.task_uid
                        });
                    confirmed.then_some(reload).ok_or_else(|| {
                        "Die Zuweisung wurde gespeichert, konnte aber nicht autoritativ neu geladen werden"
                            .to_string()
                    })
                });
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task assignment result was dropped");
            }
        });
        let page_weak = page.downgrade();
        let button_for_result = assignment_button.clone();
        let task_store_for_result = task_store.clone();
        let task_list_for_result = task_list.clone();
        let empty_tasks_for_result = empty_tasks.clone();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if page.imp().task_assignment_generation.get() != generation {
                return;
            }
            page.imp().task_assignment_running.set(false);
            match result {
                Ok(reload) => {
                    apply_authoritative_task_snapshot(
                        &page,
                        &task_store_for_result,
                        &task_list_for_result,
                        &empty_tasks_for_result,
                        reload,
                    );
                    button_for_result
                        .display()
                        .clipboard()
                        .set_text(&request_payload);
                    page.show_toast(
                        "Aufgabe wurde zugewiesen und METHOD:REQUEST bereitgestellt; echter iTIP-Versand bleibt Linux-/EDS-Handoff",
                    );
                }
                Err(error) => page.show_toast(&error),
            }
            update_task_selection_controls(&page);
        });
    });
    dialog.present(Some(button));
}

fn begin_task_source_management(page: &TasksPage) -> Option<u64> {
    if task_mutation_batch_running(page) {
        return None;
    }
    let generation = page
        .imp()
        .task_source_management_generation
        .get()
        .wrapping_add(1);
    page.imp().task_source_management_generation.set(generation);
    page.imp().task_source_management_running.set(true);
    update_task_selection_controls(page);
    Some(generation)
}

fn task_source_management_result_is_current(page: &TasksPage, generation: u64) -> bool {
    page.imp().task_source_management_generation.get() == generation
}

fn task_source_color_hex(color: &gtk::gdk::RGBA) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        channel(color.red()),
        channel(color.green()),
        channel(color.blue())
    )
}

fn present_new_local_task_source(page: &TasksPage, button: &gtk::Button) {
    if task_mutation_batch_running(page) {
        show_toast_for_widget(button, "Eine andere Aufgabenänderung läuft bereits");
        return;
    }
    let name_entry = gtk::Entry::builder()
        .placeholder_text("Name der Aufgabenliste")
        .activates_default(true)
        .hexpand(true)
        .build();
    let color_dialog = gtk::ColorDialog::builder()
        .title("Farbe der Aufgabenliste")
        .build();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    if let Ok(color) = gtk::gdk::RGBA::parse("#62a0ea") {
        color_button.set_rgba(&color);
    }
    color_button.set_tooltip_text(Some("Farbe der Aufgabenliste auswählen"));
    let default_check = gtk::CheckButton::with_label("Als Standard-Aufgabenliste verwenden");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Name")
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&name_entry);
    content.append(
        &gtk::Label::builder()
            .label("Farbe")
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&color_button);
    content.append(&default_check);
    let dialog = adw::AlertDialog::new(
        Some("Neue lokale Aufgabenliste"),
        Some("Die Aufgabenliste wird im lokalen EDS-Kalenderdienst angelegt."),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("create", "Erstellen");
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");
    let page_weak = page.downgrade();
    let button_for_result = button.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "create" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let display_name = name_entry.text().trim().to_string();
        if display_name.is_empty() {
            show_toast_for_widget(&button_for_result, "Der Name darf nicht leer sein");
            return;
        }
        let color = task_source_color_hex(&color_button.rgba());
        let make_default = default_check.is_active();
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                "Eine andere Aufgabenänderung läuft bereits",
            );
            return;
        };
        show_toast_for_widget(&button_for_result, "Aufgabenliste wird erstellt …");
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = ECalService::new().create_local_source(
                &display_name,
                &color,
                ECalComponentType::Task,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Local task source creation result was dropped");
            }
        });
        let page_weak = page.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !task_source_management_result_is_current(&page, generation) {
                return;
            }
            page.imp().task_source_management_running.set(false);
            match result {
                Ok(created) => {
                    if make_default
                        && let Some(settings) = AppSettings::try_new()
                    {
                        settings.set_default_task_source_uid(&created.source_uid);
                    }
                    match created.status {
                        ECalLocalSourceCreateStatus::Available => {
                            page.show_toast("Lokale Aufgabenliste wurde erstellt");
                        }
                        ECalLocalSourceCreateStatus::CreatedUnavailable(warning) => {
                            page.show_toast(&format!(
                                "Aufgabenliste wurde erstellt, ist aber noch nicht verfügbar: {warning}"
                            ));
                        }
                    }
                    start_task_source_discovery(&page, Some(created.source_uid), true);
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast(
                        "Lokale EDS-Aufgabenlisten werden von diesem Backend nicht unterstützt",
                    );
                }
                Err(ECalServiceError::Backend(message)) => page.show_toast(&message),
            }
            update_task_selection_controls(&page);
        });
    });
    dialog.present(Some(button));
}

fn task_source_custom_file_label(uri: Option<&str>) -> String {
    let Some(uri) = uri.filter(|uri| !uri.trim().is_empty()) else {
        return "Keine iCalendar-Datei ausgewählt".to_string();
    };
    gio::File::for_uri(uri)
        .path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| uri.to_string())
}

fn present_task_source_properties(page: &TasksPage, button: &gtk::Button) {
    let Some((source_uid, _)) = current_task_source_metadata(page) else {
        show_toast_for_widget(button, "Zum Bearbeiten eine Aufgabenliste auswählen");
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.properties_enabled {
        show_toast_for_widget(
            button,
            "Die Eigenschaften dieser Aufgabenliste sind nicht schreibbar",
        );
        return;
    }
    let Some(generation) = begin_task_source_management(page) else {
        show_toast_for_widget(button, "Eine andere Aufgabenänderung läuft bereits");
        return;
    };
    show_toast_for_widget(button, "Aufgabenlisten-Eigenschaften werden geladen …");
    let source_uid_for_worker = source_uid.clone();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = ECalService::new()
            .local_source_properties(&source_uid_for_worker, ECalComponentType::Task);
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Local task source properties result was dropped");
        }
    });
    let page_weak = page.downgrade();
    let button = button.clone();
    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_source_management_result_is_current(&page, generation) {
            return;
        }
        match result {
            Ok(properties) => {
                present_loaded_task_source_properties(&page, &button, generation, properties)
            }
            Err(ECalServiceError::Unsupported) => {
                page.imp().task_source_management_running.set(false);
                page.show_toast(
                    "Provider-spezifische Eigenschaften sind für diese Aufgabenliste noch nicht verfügbar",
                );
                update_task_selection_controls(&page);
            }
            Err(ECalServiceError::Backend(message)) => {
                page.imp().task_source_management_running.set(false);
                page.show_toast(&message);
                update_task_selection_controls(&page);
            }
        }
    });
}

fn present_loaded_task_source_properties(
    page: &TasksPage,
    button: &gtk::Button,
    generation: u64,
    properties: ECalLocalSourceProperties,
) {
    let name_entry = gtk::Entry::builder()
        .text(&properties.display_name)
        .placeholder_text("Name der Aufgabenliste")
        .activates_default(true)
        .hexpand(true)
        .build();
    let color_dialog = gtk::ColorDialog::builder()
        .title("Farbe der Aufgabenliste")
        .build();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    if let Ok(color) = gtk::gdk::RGBA::parse(&properties.color) {
        color_button.set_rgba(&color);
    }
    let email_entry = gtk::Entry::builder()
        .text(&properties.email_address)
        .placeholder_text("E-Mail-Adresse (optional)")
        .hexpand(true)
        .build();
    let default_was_selected = AppSettings::try_new()
        .is_some_and(|settings| settings.default_task_source_uid() == properties.source_uid);
    let default_check = gtk::CheckButton::with_label("Als Standard-Aufgabenliste verwenden");
    default_check.set_active(default_was_selected);

    let custom_file_uri = Rc::new(RefCell::new(properties.custom_file_uri.clone()));
    let custom_file_check = gtk::CheckButton::with_label("Vorhandene iCalendar-Datei verwenden");
    custom_file_check.set_active(!properties.is_builtin && properties.custom_file_uri.is_some());
    custom_file_check.set_sensitive(!properties.is_builtin);
    let custom_file_label = gtk::Label::builder()
        .label(task_source_custom_file_label(
            properties.custom_file_uri.as_deref(),
        ))
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .hexpand(true)
        .halign(gtk::Align::Start)
        .build();
    let choose_file_button = gtk::Button::builder()
        .label("Datei wählen …")
        .sensitive(!properties.is_builtin && properties.custom_file_uri.is_some())
        .build();
    let file_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    file_row.append(&custom_file_label);
    file_row.append(&choose_file_button);
    file_row.set_visible(!properties.is_builtin && properties.custom_file_uri.is_some());
    let custom_writable_check =
        gtk::CheckButton::with_label("Evolution darf die iCalendar-Datei aktualisieren");
    custom_writable_check.set_active(properties.custom_file_writable);
    custom_writable_check
        .set_visible(!properties.is_builtin && properties.custom_file_uri.is_some());
    let builtin_hint = gtk::Label::builder()
        .label("Die eingebaute Aufgabenliste kann keine externe iCalendar-Datei verwenden.")
        .wrap(true)
        .xalign(0.0)
        .css_classes(vec!["dim-label"])
        .visible(properties.is_builtin)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    for (label, widget) in [
        ("Name", name_entry.clone().upcast::<gtk::Widget>()),
        ("Farbe", color_button.clone().upcast::<gtk::Widget>()),
        ("E-Mail", email_entry.clone().upcast::<gtk::Widget>()),
    ] {
        content.append(
            &gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .build(),
        );
        content.append(&widget);
    }
    content.append(&default_check);
    content.append(&custom_file_check);
    content.append(&file_row);
    content.append(&custom_writable_check);
    content.append(&builtin_hint);

    let dialog = adw::AlertDialog::new(
        Some("Eigenschaften der Aufgabenliste"),
        Some("Lokale EDS-Eigenschaften bearbeiten"),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("save", "Speichern");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    let update_save_state = {
        let dialog = dialog.downgrade();
        let name_entry = name_entry.clone();
        let custom_file_check = custom_file_check.clone();
        let custom_file_uri = Rc::clone(&custom_file_uri);
        move || {
            if let Some(dialog) = dialog.upgrade() {
                dialog.set_response_enabled(
                    "save",
                    !name_entry.text().trim().is_empty()
                        && (!custom_file_check.is_active() || custom_file_uri.borrow().is_some()),
                );
            }
        }
    };
    update_save_state();
    let update_save_state_for_name = update_save_state.clone();
    name_entry.connect_changed(move |_| update_save_state_for_name());
    let update_save_state_for_toggle = update_save_state.clone();
    let file_row_for_toggle = file_row.clone();
    let choose_file_for_toggle = choose_file_button.clone();
    let writable_for_toggle = custom_writable_check.clone();
    custom_file_check.connect_toggled(move |check| {
        let active = check.is_active();
        file_row_for_toggle.set_visible(active);
        choose_file_for_toggle.set_sensitive(active);
        writable_for_toggle.set_visible(active);
        update_save_state_for_toggle();
    });

    let button_for_file = button.clone();
    let file_label_for_choose = custom_file_label.clone();
    let custom_file_uri_for_choose = Rc::clone(&custom_file_uri);
    let update_save_state_for_file = update_save_state.clone();
    choose_file_button.connect_clicked(move |_| {
        let Some(window) = button_for_file.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("iCalendar-Datei auswählen")
            .modal(true)
            .build();
        let file_label = file_label_for_choose.clone();
        let custom_file_uri = Rc::clone(&custom_file_uri_for_choose);
        let update_save_state = update_save_state_for_file.clone();
        file_dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let uri = file.uri().to_string();
            file_label.set_label(&task_source_custom_file_label(Some(&uri)));
            custom_file_uri.replace(Some(uri));
            update_save_state();
        });
    });

    let page_weak = page.downgrade();
    let source_uid = properties.source_uid.clone();
    dialog.connect_response(None, move |_, response| {
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !task_source_management_result_is_current(&page, generation) {
            return;
        }
        if response != "save" {
            page.imp().task_source_management_running.set(false);
            update_task_selection_controls(&page);
            return;
        }
        let update = ECalLocalSourcePropertiesUpdate {
            display_name: name_entry.text().trim().to_string(),
            color: task_source_color_hex(&color_button.rgba()),
            custom_file_uri: custom_file_check
                .is_active()
                .then(|| custom_file_uri.borrow().clone())
                .flatten(),
            custom_file_writable: custom_writable_check.is_active(),
            email_address: email_entry.text().trim().to_string(),
        };
        let make_default = default_check.is_active();
        let source_uid_for_worker = source_uid.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = ECalService::new().update_local_source_properties(
                &source_uid_for_worker,
                ECalComponentType::Task,
                &update,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Local task source update result was dropped");
            }
        });
        let page_weak = page.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !task_source_management_result_is_current(&page, generation) {
                return;
            }
            page.imp().task_source_management_running.set(false);
            match result {
                Ok(updated) => {
                    if let Some(settings) = AppSettings::try_new() {
                        if make_default {
                            settings.set_default_task_source_uid(&updated.source_uid);
                        } else if default_was_selected {
                            settings.set_default_task_source_uid("");
                        }
                    }
                    match updated.status {
                        ECalLocalSourceCreateStatus::Available => {
                            page.show_toast("Aufgabenlisten-Eigenschaften wurden gespeichert");
                        }
                        ECalLocalSourceCreateStatus::CreatedUnavailable(warning) => {
                            page.show_toast(&format!(
                                "Eigenschaften wurden gespeichert, die Aufgabenliste ist aber noch nicht verfügbar: {warning}"
                            ));
                        }
                    }
                    start_task_source_discovery(&page, Some(updated.source_uid), true);
                }
                Err(ECalServiceError::Unsupported) => page.show_toast(
                    "Provider-spezifische Eigenschaften sind für diese Aufgabenliste noch nicht verfügbar",
                ),
                Err(ECalServiceError::Backend(message)) => page.show_toast(&message),
            }
            update_task_selection_controls(&page);
        });
    });
    dialog.present(Some(button));
}

fn present_task_source_rename(page: &TasksPage, button: &gtk::Button) {
    let Some((source_uid, source)) = current_task_source_metadata(page) else {
        show_toast_for_widget(button, "Zum Umbenennen eine Aufgabenliste auswählen");
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.rename_enabled {
        show_toast_for_widget(button, "Diese Aufgabenliste kann nicht umbenannt werden");
        return;
    }

    let entry = gtk::Entry::builder()
        .text(&source.display_name)
        .activates_default(true)
        .hexpand(true)
        .build();
    entry.set_accessible_role(gtk::AccessibleRole::TextBox);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Neuer Name")
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&entry);
    let dialog = adw::AlertDialog::new(
        Some("Aufgabenliste umbenennen"),
        Some("Der neue Name wird direkt in der ausgewählten EDS-Quelle gespeichert."),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("rename", "Umbenennen");
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");
    let page_weak = page.downgrade();
    let button_for_result = button.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "rename" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let display_name = entry.text().trim().to_string();
        if display_name.is_empty() {
            show_toast_for_widget(&button_for_result, "Der Name darf nicht leer sein");
            return;
        }
        if display_name == source.display_name {
            return;
        }
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                "Eine andere Aufgabenänderung läuft bereits",
            );
            return;
        };
        show_toast_for_widget(&button_for_result, "Aufgabenliste wird umbenannt …");
        let (sender, receiver) = async_channel::bounded(1);
        let source_uid_for_worker = source_uid.clone();
        std::thread::spawn(move || {
            let result = ECalService::new().rename_source(
                &source_uid_for_worker,
                ECalComponentType::Task,
                &display_name,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task source rename result was dropped");
            }
        });
        let page_weak = page.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !task_source_management_result_is_current(&page, generation) {
                return;
            }
            page.imp().task_source_management_running.set(false);
            let preferred = page.imp().task_source_filter.borrow().clone();
            match result {
                Ok(()) => {
                    page.show_toast("Aufgabenliste wurde umbenannt");
                    start_task_source_discovery(&page, preferred, true);
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast("Umbenennen wird von diesem Backend nicht unterstützt");
                    start_task_source_discovery(&page, preferred, false);
                }
                Err(ECalServiceError::Backend(message)) => {
                    page.show_toast(&message);
                    start_task_source_discovery(&page, preferred, false);
                }
            }
            update_task_selection_controls(&page);
        });
    });
    dialog.present(Some(button));
}

fn present_task_source_copy(page: &TasksPage, button: &gtk::Button) {
    let Some((source_uid, source)) = current_task_source_metadata(page) else {
        show_toast_for_widget(button, "Zum Kopieren eine Aufgabenliste auswählen");
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.copy_enabled {
        show_toast_for_widget(button, "Diese Aufgabenliste kann nicht kopiert werden");
        return;
    }
    let mut destinations = page
        .imp()
        .task_source_metadata
        .borrow()
        .values()
        .filter(|candidate| candidate.source_uid != source_uid)
        .cloned()
        .collect::<Vec<_>>();
    destinations.sort_by_cached_key(|candidate| {
        (
            candidate.display_name.to_lowercase(),
            candidate.source_uid.clone(),
        )
    });
    if destinations.is_empty() {
        show_toast_for_widget(button, "Es ist keine andere EDS-Aufgabenliste verfügbar");
        return;
    }
    let destination_names = destinations
        .iter()
        .map(task_source_capability_label)
        .collect::<Vec<_>>();
    let destination_uids = destinations
        .iter()
        .map(|candidate| candidate.source_uid.clone())
        .collect::<Vec<_>>();
    let destination_refs = destination_names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let destination_model = gtk::StringList::new(&destination_refs);
    let destination_selector =
        gtk::DropDown::new(Some(destination_model), None::<&gtk::Expression>);
    destination_selector.set_selected(0);
    destination_selector.set_tooltip_text(Some("Ziel-Aufgabenliste auswählen"));
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Ziel-Aufgabenliste")
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&destination_selector);
    let dialog = adw::AlertDialog::new(
        Some("Aufgabenliste kopieren"),
        Some(&format!(
            "Alle Aufgaben aus „{}“ werden in die gewählte Liste kopiert. Gleiche UIDs werden aktualisiert.",
            source.display_name
        )),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("copy", "Kopieren");
    dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("copy"));
    dialog.set_close_response("cancel");
    let page_weak = page.downgrade();
    let button_for_result = button.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "copy" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let Some(destination_uid) = destination_uids
            .get(destination_selector.selected() as usize)
            .cloned()
        else {
            show_toast_for_widget(&button_for_result, "Ungültige Ziel-Aufgabenliste");
            return;
        };
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                "Eine andere Aufgabenänderung läuft bereits",
            );
            return;
        };
        show_toast_for_widget(&button_for_result, "Aufgabenliste wird kopiert …");
        let (sender, receiver) = async_channel::bounded(1);
        let source_uid_for_worker = source_uid.clone();
        std::thread::spawn(move || {
            let result = ECalService::new()
                .copy_source(
                    &source_uid_for_worker,
                    &destination_uid,
                    ECalComponentType::Task,
                )
                .map(|copy| (copy, load_snapshot_prefer_eds()));
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task source copy result was dropped");
            }
        });
        let page_weak = page.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !task_source_management_result_is_current(&page, generation) {
                return;
            }
            page.imp().task_source_management_running.set(false);
            match result {
                Ok((copy, reload)) => {
                    if reload.origin == CalendarDataOrigin::Eds && reload.warning.is_none() {
                        if let (Some(task_store), Some(task_list), Some(empty_tasks)) = (
                            page.imp().task_store.borrow().as_ref().cloned(),
                            page.imp().task_list.borrow().as_ref().cloned(),
                            page.imp().task_empty_state.borrow().as_ref().cloned(),
                        ) {
                            apply_authoritative_task_snapshot(
                                &page,
                                &task_store,
                                &task_list,
                                &empty_tasks,
                                reload,
                            );
                            page.reload_task_category_metadata();
                        }
                    } else {
                        page.show_toast(
                            "Die Kopie wurde bestätigt, konnte aber nicht autoritativ neu geladen werden",
                        );
                    }
                    let changed = copy.copied_count;
                    let created = copy.copied_count.saturating_sub(copy.updated_count);
                    match copy.status {
                        ECalSourceCopyStatus::Complete => page.show_toast(&format!(
                            "Aufgabenliste wurde kopiert ({changed} Aufgaben; {} neu, {} aktualisiert)",
                            created, copy.updated_count
                        )),
                        ECalSourceCopyStatus::PartialFailure(message) => page.show_toast(&format!(
                            "Aufgabenliste wurde teilweise kopiert ({changed} bestätigt): {message}"
                        )),
                    }
                    start_task_source_discovery(
                        &page,
                        page.imp().task_source_filter.borrow().clone(),
                        false,
                    );
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast("Kopieren wird von diesem Backend nicht unterstützt");
                }
                Err(ECalServiceError::Backend(message)) => page.show_toast(&message),
            }
            update_task_selection_controls(&page);
        });
    });
    dialog.present(Some(button));
}

fn present_task_source_delete_confirmation(page: &TasksPage, button: &gtk::Button) {
    let Some((source_uid, source)) = current_task_source_metadata(page) else {
        show_toast_for_widget(button, "Zum Löschen eine Aufgabenliste auswählen");
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.delete_enabled {
        show_toast_for_widget(button, "Diese Aufgabenliste kann nicht gelöscht werden");
        return;
    }
    let remote = source.remote_deletable;
    let title = if remote {
        "Remote-Aufgabenliste löschen?"
    } else {
        "Aufgabenliste entfernen?"
    };
    let description = if remote {
        format!(
            "„{}“ und ihre Aufgaben werden dauerhaft beim Anbieter gelöscht.",
            source.display_name
        )
    } else {
        format!(
            "„{}“ wird aus Evolution entfernt. Diese Aktion kann nicht rückgängig gemacht werden.",
            source.display_name
        )
    };
    let dialog = adw::AlertDialog::new(Some(title), Some(&description));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("delete", "Löschen");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let page_weak = page.downgrade();
    let button_for_result = button.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "delete" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                "Eine andere Aufgabenänderung läuft bereits",
            );
            return;
        };
        show_toast_for_widget(&button_for_result, "Aufgabenliste wird gelöscht …");
        let (sender, receiver) = async_channel::bounded(1);
        let source_uid_for_worker = source_uid.clone();
        std::thread::spawn(move || {
            let result =
                ECalService::new().delete_source(&source_uid_for_worker, ECalComponentType::Task);
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task source delete result was dropped");
            }
        });
        let page_weak = page.downgrade();
        let source_uid_for_result = source_uid.clone();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !task_source_management_result_is_current(&page, generation) {
                return;
            }
            page.imp().task_source_management_running.set(false);
            match result {
                Ok(()) => {
                    if page.imp().task_source_filter.borrow().as_deref()
                        == Some(source_uid_for_result.as_str())
                    {
                        page.imp().task_source_filter.replace(None);
                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_task_source_filter("");
                        }
                    }
                    if let (Some(task_store), Some(task_list), Some(empty_tasks)) = (
                        page.imp().task_store.borrow().as_ref().cloned(),
                        page.imp().task_list.borrow().as_ref().cloned(),
                        page.imp().task_empty_state.borrow().as_ref().cloned(),
                    ) {
                        for index in (0..task_store.n_items()).rev() {
                            if task_store
                                .item(index)
                                .and_downcast::<TaskItemObject>()
                                .is_some_and(|task| task.source_uid() == source_uid_for_result)
                            {
                                task_store.remove(index);
                            }
                        }
                        render_task_rows(&page, &task_list, &task_store, &empty_tasks);
                    }
                    page.show_toast("Aufgabenliste wurde gelöscht");
                    start_task_source_discovery(
                        &page,
                        page.imp().task_source_filter.borrow().clone(),
                        true,
                    );
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast("Löschen wird von diesem Backend nicht unterstützt");
                    start_task_source_discovery(
                        &page,
                        page.imp().task_source_filter.borrow().clone(),
                        false,
                    );
                }
                Err(ECalServiceError::Backend(message)) => {
                    page.show_toast(&message);
                    start_task_source_discovery(
                        &page,
                        page.imp().task_source_filter.borrow().clone(),
                        false,
                    );
                }
            }
            update_task_selection_controls(&page);
        });
    });
    dialog.present(Some(button));
}

fn task_source_capability_from_ecal(source: &ECalSourceCapability) -> TaskSourceCapability {
    if !source.connected || !source.available {
        TaskSourceCapability::Unavailable
    } else if source.writable {
        TaskSourceCapability::Writable
    } else {
        TaskSourceCapability::ReadOnly
    }
}

fn task_source_capability_label(source: &ECalSourceCapability) -> String {
    let display_name = if source.display_name.trim().is_empty() {
        source.source_uid.as_str()
    } else {
        source.display_name.as_str()
    };
    match task_source_capability_from_ecal(source) {
        TaskSourceCapability::Writable => display_name.to_string(),
        TaskSourceCapability::ReadOnly => format!("{display_name} (schreibgeschützt)"),
        TaskSourceCapability::Unavailable => format!("{display_name} (nicht verfügbar)"),
        TaskSourceCapability::Unknown => unreachable!("ECal discovery never projects Unknown"),
    }
}

fn task_source_capability(
    source_uid: &str,
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
) -> TaskSourceCapability {
    if source_uid.trim().is_empty() {
        return TaskSourceCapability::Unavailable;
    }
    if source_uid == "local-calendar" {
        return if local_writable {
            TaskSourceCapability::Writable
        } else {
            TaskSourceCapability::Unavailable
        };
    }
    capabilities
        .get(source_uid)
        .copied()
        .unwrap_or(TaskSourceCapability::Unknown)
}

fn task_source_is_writable_with_local(
    source_uid: &str,
    capabilities: &Rc<RefCell<HashMap<String, TaskSourceCapability>>>,
    local_writable: bool,
) -> bool {
    task_source_capability(source_uid, &capabilities.borrow(), local_writable)
        == TaskSourceCapability::Writable
}

fn task_source_is_writable(
    source_uid: &str,
    capabilities: &Rc<RefCell<HashMap<String, TaskSourceCapability>>>,
) -> bool {
    task_source_is_writable_with_local(
        source_uid,
        capabilities,
        CalendarStoreService::new().is_writable(),
    )
}

fn task_source_is_writable_on_page(page: &TasksPage, source_uid: &str) -> bool {
    task_source_capability(
        source_uid,
        &page.imp().task_source_capabilities.borrow(),
        CalendarStoreService::new().is_writable(),
    ) == TaskSourceCapability::Writable
}

fn task_default_source_index(
    source_uids: &[String],
    preferred_source_uid: Option<&str>,
    edit_source_uid: Option<&str>,
    capabilities: &HashMap<String, TaskSourceCapability>,
    local_writable: bool,
) -> Option<usize> {
    if let Some(edit_source_uid) = edit_source_uid {
        return source_uids
            .iter()
            .position(|source_uid| source_uid == edit_source_uid);
    }
    preferred_source_uid
        .and_then(|preferred| {
            source_uids.iter().position(|source_uid| {
                source_uid == preferred
                    && task_source_capability(source_uid, capabilities, local_writable)
                        == TaskSourceCapability::Writable
            })
        })
        .or_else(|| {
            source_uids.iter().position(|source_uid| {
                task_source_capability(source_uid, capabilities, local_writable)
                    == TaskSourceCapability::Writable
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum TaskSavePhase {
    Editing = 0,
    DispatchPending = 1,
    Committing = 2,
    CancelledBeforeDispatch = 3,
    Completed = 4,
    Failed = 5,
}

impl TaskSavePhase {
    fn from_atomic(value: u8) -> Self {
        match value {
            1 => Self::DispatchPending,
            2 => Self::Committing,
            3 => Self::CancelledBeforeDispatch,
            4 => Self::Completed,
            5 => Self::Failed,
            _ => Self::Editing,
        }
    }
}

#[derive(Debug)]
struct TaskSaveControl {
    phase: AtomicU8,
}

impl TaskSaveControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(TaskSavePhase::Editing as u8),
        })
    }

    fn phase(&self) -> TaskSavePhase {
        TaskSavePhase::from_atomic(self.phase.load(Ordering::Acquire))
    }

    fn arm(&self) -> bool {
        self.phase
            .compare_exchange(
                TaskSavePhase::Editing as u8,
                TaskSavePhase::DispatchPending as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// This compare-and-exchange is the cancellation boundary.  If it wins,
    /// the worker's dispatch CAS cannot start the synchronous backend call.
    fn cancel_before_dispatch(&self) -> TaskCancelResult {
        match self.phase.compare_exchange(
            TaskSavePhase::DispatchPending as u8,
            TaskSavePhase::CancelledBeforeDispatch as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => TaskCancelResult::CancelledBeforeDispatch,
            Err(value) if TaskSavePhase::from_atomic(value) == TaskSavePhase::Committing => {
                TaskCancelResult::Committing
            }
            Err(value) if TaskSavePhase::from_atomic(value) == TaskSavePhase::DispatchPending => {
                TaskCancelResult::DispatchRace
            }
            Err(_) => TaskCancelResult::NotCancelable,
        }
    }

    fn begin_dispatch(&self) -> bool {
        self.phase
            .compare_exchange(
                TaskSavePhase::DispatchPending as u8,
                TaskSavePhase::Committing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn finish(&self, success: bool) {
        self.phase.store(
            if success {
                TaskSavePhase::Completed as u8
            } else {
                TaskSavePhase::Failed as u8
            },
            Ordering::Release,
        );
    }

    fn reset_after_failure(&self) {
        self.phase
            .store(TaskSavePhase::Editing as u8, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskCancelResult {
    CancelledBeforeDispatch,
    Committing,
    DispatchRace,
    NotCancelable,
}

#[derive(Debug)]
enum TaskSaveWorkerMessage {
    Committing,
    CancelledBeforeDispatch,
    Finished(
        Result<
            crate::services::ecal_service::ECalMutationResult,
            crate::services::ecal_service::ECalMutationError,
        >,
    ),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskEditorDialogState {
    Clean,
    Dirty,
    Prompting,
    SavePending,
    Committing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskEditorDialogAction {
    Edit,
    CloseRequest,
    Save,
    Discard,
    ContinueEditing,
    CancelBeforeDispatch,
    DispatchStarted,
    BackendSuccess,
    BackendFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskEditorDialogEffect {
    None,
    ShowPrompt,
    DispatchSave,
    Close,
    BlockClose,
    DisableClose,
}

/// Pure boundary for the GTK dialog.  The callbacks below only perform the
/// effect selected here; no backend result or widget is allowed to decide
/// whether a pending edit was discarded.
fn task_editor_dialog_transition(
    state: TaskEditorDialogState,
    action: TaskEditorDialogAction,
) -> (TaskEditorDialogState, TaskEditorDialogEffect) {
    use TaskEditorDialogAction as Action;
    use TaskEditorDialogEffect as Effect;
    use TaskEditorDialogState as State;

    match (state, action) {
        (State::Clean, Action::Edit) => (State::Dirty, Effect::None),
        (State::Clean, Action::CloseRequest) => (State::Closed, Effect::Close),
        (State::Dirty, Action::CloseRequest) => (State::Prompting, Effect::ShowPrompt),
        (State::Prompting, Action::ContinueEditing) => (State::Dirty, Effect::None),
        (State::Prompting, Action::Discard) => (State::Closed, Effect::Close),
        (State::Prompting, Action::Save) => (State::SavePending, Effect::DispatchSave),
        (State::Dirty, Action::Save) => (State::SavePending, Effect::DispatchSave),
        (State::SavePending, Action::DispatchStarted) => (State::Committing, Effect::DisableClose),
        (State::SavePending, Action::CancelBeforeDispatch) => (State::Closed, Effect::Close),
        (State::SavePending, Action::CloseRequest) => (State::SavePending, Effect::BlockClose),
        (State::Committing, Action::CloseRequest | Action::CancelBeforeDispatch) => {
            (State::Committing, Effect::BlockClose)
        }
        (State::Committing, Action::BackendSuccess) => (State::Closed, Effect::Close),
        (State::Committing, Action::BackendFailure) => (State::Dirty, Effect::None),
        (State::SavePending, Action::BackendFailure) => (State::Dirty, Effect::None),
        (State::Closed, _) => (State::Closed, Effect::None),
        (current, _) => (current, Effect::None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskCloseDecision {
    Allow,
    Prompt,
    KeepPrompt,
    BlockSave,
    BlockCommit,
}

fn task_close_decision(
    dirty: bool,
    save_phase: TaskSavePhase,
    prompt_open: bool,
) -> TaskCloseDecision {
    match save_phase {
        TaskSavePhase::Committing => TaskCloseDecision::BlockCommit,
        TaskSavePhase::DispatchPending => TaskCloseDecision::BlockSave,
        TaskSavePhase::CancelledBeforeDispatch
        | TaskSavePhase::Completed
        | TaskSavePhase::Failed
        | TaskSavePhase::Editing => {
            if !dirty {
                TaskCloseDecision::Allow
            } else if prompt_open {
                TaskCloseDecision::KeepPrompt
            } else {
                TaskCloseDecision::Prompt
            }
        }
    }
}

fn task_editor_result_is_current(
    current_generation: u64,
    expected_generation: u64,
    dialog_visible: bool,
) -> bool {
    current_generation == expected_generation && dialog_visible
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskEditorResultState {
    apply_row: bool,
    dirty: bool,
}

fn task_editor_result_state(
    current: bool,
    backend_succeeded: bool,
    dirty_before: bool,
) -> TaskEditorResultState {
    if current && backend_succeeded {
        TaskEditorResultState {
            apply_row: true,
            dirty: false,
        }
    } else {
        TaskEditorResultState {
            apply_row: false,
            dirty: if current { true } else { dirty_before },
        }
    }
}

fn task_item_from_confirmed_component(
    source_uid: &str,
    uid: &str,
    component: &str,
) -> Result<TaskItemObject, String> {
    if !unfold_ical_lines(component)
        .iter()
        .any(|line| line.eq_ignore_ascii_case("BEGIN:VTODO"))
    {
        return Err("Die Komponente ist kein VTODO".to_string());
    }
    if let Some(component_uid) = ical_property(component, "UID")
        && component_uid != uid
    {
        return Err("Die bestätigte Komponente enthält eine andere UID".to_string());
    }
    let draft = task_editor_draft_from_component(component);
    let due = match draft.due.original_kind {
        Some(TaskDateValueKind::DateOnly) => draft.due.input.clone(),
        Some(TaskDateValueKind::DateTime) => property_local_display(component, "DUE", false)
            .unwrap_or_else(|| draft.due.input.clone()),
        None => String::new(),
    };
    let priority = if draft.priority == 0 {
        "normal".to_string()
    } else {
        draft.priority.to_string()
    };
    Ok(TaskItemObject::new(
        source_uid,
        uid,
        &draft.summary,
        &due,
        &priority,
        draft.status == "COMPLETED",
        component,
    ))
}

fn task_authoritative_component(
    expected_uid: &str,
    returned_uid: &str,
    component: Option<String>,
) -> Result<String, String> {
    if !returned_uid.trim().is_empty() && returned_uid != expected_uid {
        return Err("Das bestätigte Aufgaben-Ergebnis gehört zu einer anderen UID".to_string());
    }
    let component = component
        .filter(|component| !component.trim().is_empty())
        .ok_or_else(|| {
            "Backend bestätigte keine autoritative Aufgabe; bitte neu laden".to_string()
        })?;
    if ical_property(&component, "UID").is_some_and(|uid| uid != expected_uid) {
        return Err("Die bestätigte Komponente enthält eine andere UID".to_string());
    }
    Ok(component)
}

fn connect_task_quick_editor(
    page: &TasksPage,
    button: &gtk::Button,
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    task: &TaskItemObject,
) {
    let values = task_quick_edit_values(task);
    let popover = gtk::Popover::builder()
        .has_arrow(true)
        .autohide(true)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(10)
        .margin_bottom(10)
        .width_request(320)
        .build();
    let heading = gtk::Label::builder()
        .label("Aufgabe schnell bearbeiten")
        .halign(gtk::Align::Start)
        .css_classes(vec!["heading"])
        .build();
    content.append(&heading);

    let status_model = gtk::StringList::new(&[
        "Nicht begonnen",
        "In Bearbeitung",
        "Erledigt",
        "Abgebrochen",
    ]);
    let status_selector = gtk::DropDown::new(Some(status_model), None::<&gtk::Expression>);
    status_selector.set_selected(task_status_index(&task.component()));
    let status_row = adw::ActionRow::builder().title("Status").build();
    status_row.add_suffix(&status_selector);
    content.append(&status_row);

    let priority_spin = gtk::SpinButton::with_range(0.0, 9.0, 1.0);
    priority_spin.set_numeric(true);
    priority_spin.set_value(values.priority as f64);
    let priority_row = adw::ActionRow::builder()
        .title("Priorität (0 = normal)")
        .build();
    priority_row.add_suffix(&priority_spin);
    content.append(&priority_row);

    let due_entry = gtk::Entry::builder()
        .text(&values.due)
        .placeholder_text("YYYY-MM-DD")
        .width_chars(10)
        .build();
    let due_row = adw::ActionRow::builder().title("Fällig am").build();
    due_row.add_suffix(&due_entry);
    content.append(&due_row);

    let categories_entry = gtk::Entry::builder()
        .text(&values.categories)
        .placeholder_text("z. B. Arbeit, Privat")
        .width_chars(18)
        .build();
    let categories_row = adw::ActionRow::builder().title("Kategorien").build();
    categories_row.add_suffix(&categories_entry);
    content.append(&categories_row);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .margin_top(4)
        .build();
    let cancel_button = gtk::Button::with_label("Abbrechen");
    cancel_button.add_css_class("flat");
    let save_button = gtk::Button::with_label("Speichern");
    save_button.add_css_class("suggested-action");
    buttons.append(&cancel_button);
    buttons.append(&save_button);
    content.append(&buttons);
    popover.set_child(Some(&content));
    popover.set_parent(button);

    let popover_weak = popover.downgrade();
    let popover_for_button = popover_weak.clone();
    button.connect_clicked(move |_| {
        if let Some(popover) = popover_for_button.upgrade() {
            popover.popup();
        }
    });
    let popover_for_cancel = popover_weak.clone();
    cancel_button.connect_clicked(move |_| {
        if let Some(popover) = popover_for_cancel.upgrade() {
            popover.popdown();
        }
    });

    let page_weak = page.downgrade();
    let task_store_for_save = task_store.clone();
    let task_list_for_save = task_list.clone();
    let empty_tasks_for_save = empty_tasks.clone();
    let task_for_save = task.clone();
    let popover_for_save = popover_weak;
    save_button.connect_clicked(move |save_button| {
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let status = task_status_from_index(status_selector.selected());
        let percent = match status {
            "COMPLETED" => 100,
            "NEEDS-ACTION" => 0,
            _ => values.percent,
        };
        let priority = priority_spin.value().round().clamp(0.0, 9.0) as u8;
        let due = due_entry.text().trim().to_string();
        let categories = categories_entry.text().to_string();
        let component = match update_task_quick_fields(
            &task_for_save,
            &due,
            priority,
            &categories,
            status,
            percent,
        ) {
            Ok(component) => component,
            Err(error) => {
                show_toast_for_widget(save_button, &error);
                return;
            }
        };
        let source_uid = task_for_save.source_uid();
        let task_uid = task_for_save.id();
        if !task_source_is_writable_on_page(&page, &source_uid) {
            show_toast_for_widget(
                save_button,
                "Diese Aufgabenliste ist nicht mehr als schreibfähig bestätigt",
            );
            return;
        }
        let title = task_for_save.title();
        let priority_for_model = if priority == 0 {
            "normal".to_string()
        } else {
            priority.to_string()
        };
        let completed = status == "COMPLETED";
        let (sender, receiver) = async_channel::bounded(1);
        save_button.set_sensitive(false);
        std::thread::spawn(move || {
            let result = ECalService::new().modify_component(
                &source_uid,
                ECalComponentType::Task,
                &task_uid,
                &component,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task quick edit result was dropped");
            }
        });

        let save_button_for_result = save_button.clone();
        let popover_for_result = popover_for_save.clone();
        let page_weak_for_result = page.downgrade();
        let source_uid_for_result = task_for_save.source_uid();
        let task_uid_for_result = task_for_save.id();
        let task_store_for_result = task_store_for_save.clone();
        let task_list_for_result = task_list_for_save.clone();
        let empty_tasks_for_result = empty_tasks_for_save.clone();
        glib::spawn_future_local(async move {
            match receiver.recv().await {
                Ok(Ok(result)) => {
                    let uid = result.uid;
                    let component = match task_authoritative_component(
                        &task_uid_for_result,
                        &uid,
                        result.component,
                    ) {
                        Ok(component) => component,
                        Err(error) => {
                            show_toast_for_widget(&save_button_for_result, &error);
                            return;
                        }
                    };
                    let mut updated = false;
                    for index in 0..task_store_for_result.n_items() {
                        let Some(existing) = task_store_for_result
                            .item(index)
                            .and_downcast::<TaskItemObject>()
                        else {
                            continue;
                        };
                        if existing.id() == task_uid_for_result
                            && existing.source_uid() == source_uid_for_result
                        {
                            existing.set_title(title.clone());
                            existing.set_due_date(due.clone());
                            existing.set_priority(priority_for_model.clone());
                            existing.set_completed(completed);
                            existing.set_component(component.clone());
                            updated = true;
                            break;
                        }
                    }
                    if updated {
                        if let Some(page) = page_weak_for_result.upgrade() {
                            render_task_rows(
                                &page,
                                &task_list_for_result,
                                &task_store_for_result,
                                &empty_tasks_for_result,
                            );
                            page.show_toast(&format!("Aufgabe aktualisiert ({uid})"));
                        }
                    } else if let Some(page) = page_weak_for_result.upgrade() {
                        page.show_toast(
                            "Aufgabe wurde bestätigt, ist aber nicht mehr in dieser Liste geladen",
                        );
                    }
                    if let Some(popover) = popover_for_result.upgrade() {
                        popover.popdown();
                    }
                }
                Ok(Err(error)) => {
                    show_toast_for_widget(
                        &save_button_for_result,
                        &format!("Aufgabe konnte nicht aktualisiert werden: {error}"),
                    );
                }
                Err(error) => {
                    tracing::debug!(%error, "Task quick edit channel closed");
                    show_toast_for_widget(
                        &save_button_for_result,
                        "Aufgabenänderung konnte nicht abgeschlossen werden",
                    );
                }
            }
            save_button_for_result.set_sensitive(true);
        });
    });
}

fn build_task_component_with_dates(
    title: &str,
    dtstart: &str,
    due: &str,
    completed: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    priority: u8,
    categories: &str,
    alarms: &[Alarm],
    description: &str,
    attachments: &str,
    status: &str,
    percent: u8,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Eine Aufgabe benötigt einen Titel".to_string());
    }
    if priority > 9 {
        return Err("Die Priorität muss zwischen 0 und 9 liegen".to_string());
    }

    let attachment_lines = parse_task_attachment_input(attachments)?;
    let empty_draft = TaskEditorDraft::default_for_new();
    let (dtstart_mutation, dtstart_date) =
        task_date_mutation(&empty_draft.dtstart, dtstart, "DTSTART")?;
    let (due_mutation, due_date) = task_date_mutation(&empty_draft.due, due, "DUE")?;
    let (completed_mutation, completed_date) =
        task_date_mutation(&empty_draft.completed, completed, "COMPLETED")?;
    let completion_present = !matches!(completed_mutation, TaskDateMutation::Remove);
    if (task_date_mutation_changed(dtstart_mutation) || task_date_mutation_changed(due_mutation))
        && let (Some(start), Some(due)) = (dtstart_date, due_date)
        && due <= start
    {
        return Err("Das Fälligkeitsdatum muss nach dem Beginn liegen".to_string());
    }
    validate_task_recurrence(
        "",
        recurrence,
        !matches!(dtstart_mutation, TaskDateMutation::Remove),
    )?;
    let lifecycle = task_lifecycle_for_date_with_presence(
        status,
        percent,
        completed_date,
        completion_present,
        Local::now().date_naive(),
    )?;
    let rdates = normalize_task_date_list(rdates, "RDATE")?;
    let exdates = normalize_task_date_list(exdates, "EXDATE")?;

    let mut component = vec![
        "BEGIN:VTODO".to_string(),
        format!("UID:evolution-gtk4-{}", uuid::Uuid::new_v4()),
        format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
        format!("STATUS:{}", lifecycle.status),
        format!("PERCENT-COMPLETE:{}", lifecycle.percent),
        format!("SUMMARY:{}", escape_ical_text(title)),
    ];
    if let TaskDateMutation::Set(date) = dtstart_mutation {
        component.push(format!("DTSTART;VALUE=DATE:{}", date.format("%Y%m%d")));
    }
    if let TaskDateMutation::Set(date) = due_mutation {
        component.push(format!("DUE;VALUE=DATE:{}", date.format("%Y%m%d")));
    }
    if !recurrence.trim().is_empty() {
        component.push(format!("RRULE:{}", recurrence.trim()));
    }
    if !rdates.is_empty() {
        component.push(format!("RDATE;VALUE=DATE:{rdates}"));
    }
    if !exdates.is_empty() {
        component.push(format!("EXDATE;VALUE=DATE:{exdates}"));
    }
    if priority > 0 {
        component.push(format!("PRIORITY:{priority}"));
    }
    let categories = serialize_categories(categories);
    if !categories.is_empty() {
        component.push(format!("CATEGORIES:{categories}"));
    }
    component.extend(
        attachment_lines
            .iter()
            .map(|attachment| attachment.raw.clone()),
    );
    if !description.trim().is_empty() {
        component.push(format!(
            "DESCRIPTION:{}",
            escape_ical_text(description.trim())
        ));
    }
    if lifecycle.completion_present {
        let Some(date) = lifecycle.completed else {
            return Err("Erledigte Aufgaben benötigen ein gültiges Abschlussdatum".to_string());
        };
        component.push(format!("COMPLETED;VALUE=DATE:{}", date.format("%Y%m%d")));
    }
    component.push("END:VTODO".to_string());
    Ok(fold_task_ical_component(&replace_component_alarms(
        &format!("{}\r\n", component.join("\r\n")),
        alarms,
    )))
}

fn build_task_component(
    title: &str,
    due: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    priority: u8,
    categories: &str,
    alarms: &[Alarm],
    description: &str,
    attachments: &str,
    status: &str,
    percent: u8,
) -> Result<String, String> {
    build_task_component_with_dates(
        title,
        "",
        due,
        "",
        recurrence,
        rdates,
        exdates,
        priority,
        categories,
        alarms,
        description,
        attachments,
        status,
        percent,
    )
}

fn update_task_component_with_dates(
    existing: &str,
    title: &str,
    dtstart: &str,
    due: &str,
    completed: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    priority: u8,
    categories: &str,
    alarms: &[Alarm],
    description: &str,
    attachments: &str,
    status: &str,
    percent: u8,
) -> Result<String, String> {
    let draft = task_editor_draft_from_component(existing);
    let lifecycle_edits = task_lifecycle_edits_inferred(&draft, status, percent, completed);
    update_task_component_with_dates_and_lifecycle_edits(
        existing,
        title,
        dtstart,
        due,
        completed,
        recurrence,
        rdates,
        exdates,
        priority,
        categories,
        alarms,
        description,
        attachments,
        status,
        percent,
        lifecycle_edits,
    )
}

fn update_task_component_with_dates_and_lifecycle_edits(
    existing: &str,
    title: &str,
    dtstart: &str,
    due: &str,
    completed: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    priority: u8,
    categories: &str,
    alarms: &[Alarm],
    description: &str,
    attachments: &str,
    status: &str,
    percent: u8,
    lifecycle_edits: TaskLifecycleEdits,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Eine Aufgabe benötigt einen Titel".to_string());
    }
    if priority > 9 {
        return Err("Die Priorität muss zwischen 0 und 9 liegen".to_string());
    }

    let draft = task_editor_draft_from_component(existing);
    let (dtstart_mutation, dtstart_date) = task_date_mutation(&draft.dtstart, dtstart, "DTSTART")?;
    let (due_mutation, due_date) = task_date_mutation(&draft.due, due, "DUE")?;
    let (completed_mutation, completed_date) =
        task_date_mutation(&draft.completed, completed, "COMPLETED")?;
    let completion_present = !matches!(completed_mutation, TaskDateMutation::Remove);
    if (task_date_mutation_changed(dtstart_mutation) || task_date_mutation_changed(due_mutation))
        && let (Some(start), Some(due)) = (dtstart_date, due_date)
        && due <= start
    {
        return Err("Das Fälligkeitsdatum muss nach dem Beginn liegen".to_string());
    }
    validate_task_recurrence(
        existing,
        recurrence,
        !matches!(dtstart_mutation, TaskDateMutation::Remove),
    )?;
    // A VTODO imported from another client can contain extensions or values
    // the portable lifecycle controls cannot represent.  Retain all three
    // lifecycle properties until the user changes one of those controls.
    // Once they do, match Evolution's coupled status/percent/completed
    // workflow and write a coherent replacement set.
    let lifecycle =
        (lifecycle_edits.status || lifecycle_edits.percent || lifecycle_edits.completed)
            .then(|| {
                task_lifecycle_for_date_with_presence(
                    status,
                    percent,
                    completed_date,
                    completion_present,
                    Local::now().date_naive(),
                )
            })
            .transpose()?;
    let normalized_rdates = if rdates.trim() == draft.rdates.trim() {
        None
    } else {
        Some(normalize_task_date_list(rdates, "RDATE")?)
    };
    let normalized_exdates = if exdates.trim() == draft.exdates.trim() {
        None
    } else {
        Some(normalize_task_date_list(exdates, "EXDATE")?)
    };
    let attachment_draft =
        TaskAttachmentDraft::from_component(existing).with_editor_text(attachments);
    let parsed_attachments = attachment_draft
        .attachments_changed
        .then(|| parse_task_attachment_input(attachments))
        .transpose()?;
    let alarms_changed = parse_alarms(existing) != alarms;

    let mut component = update_ical_property(existing, "SUMMARY", Some(&escape_ical_text(title)));
    component = apply_task_date_pair_mutations(
        &component,
        &draft.dtstart,
        &draft.due,
        dtstart_mutation,
        due_mutation,
    );
    if recurrence.trim() != draft.recurrence.trim() {
        component = update_ical_property(
            &component,
            "RRULE",
            (!recurrence.trim().is_empty()).then_some(recurrence.trim()),
        );
    }
    if let Some(rdates) = normalized_rdates.as_deref() {
        component = update_ical_property(
            &component,
            "RDATE;VALUE=DATE",
            (!rdates.is_empty()).then_some(rdates),
        );
    }
    if let Some(exdates) = normalized_exdates.as_deref() {
        component = update_ical_property(
            &component,
            "EXDATE;VALUE=DATE",
            (!exdates.is_empty()).then_some(exdates),
        );
    }
    component = if priority == 0 {
        update_ical_property(&component, "PRIORITY", None)
    } else {
        update_ical_property(&component, "PRIORITY", Some(&priority.to_string()))
    };
    let categories = serialize_categories(categories);
    component = if categories.is_empty() {
        update_ical_property(&component, "CATEGORIES", None)
    } else {
        update_ical_property(&component, "CATEGORIES", Some(&categories))
    };
    if let Some(parsed_attachments) = parsed_attachments.as_deref() {
        component = replace_task_attachments_lossless(&component, parsed_attachments);
    }
    if alarms_changed {
        component = replace_component_alarms(&component, alarms);
    }
    if description.trim() != draft.description.trim() {
        component = if description.trim().is_empty() {
            update_ical_property(&component, "DESCRIPTION", None)
        } else {
            update_ical_property(
                &component,
                "DESCRIPTION",
                Some(&escape_ical_text(description.trim())),
            )
        };
    }

    if let Some(lifecycle) = lifecycle {
        let status_unchanged = draft
            .status_raw
            .as_deref()
            .is_some_and(|raw| raw.trim().eq_ignore_ascii_case(lifecycle.status));
        if !status_unchanged {
            component = update_ical_property(&component, "STATUS", Some(lifecycle.status));
        }
        let percent_text = lifecycle.percent.to_string();
        if !draft
            .percent_raw
            .as_deref()
            .and_then(parse_task_percent_value)
            .is_some_and(|value| value == lifecycle.percent)
        {
            component = update_ical_property(&component, "PERCENT-COMPLETE", Some(&percent_text));
        }
        component = if lifecycle.completion_present {
            match completed_mutation {
                TaskDateMutation::Preserve => component,
                TaskDateMutation::Set(date) => {
                    apply_task_date_mutation(&component, "COMPLETED", TaskDateMutation::Set(date))
                }
                TaskDateMutation::Remove => update_ical_property(&component, "COMPLETED", None),
            }
        } else {
            update_ical_property(&component, "COMPLETED", None)
        };
    }
    Ok(update_ical_property(
        &component,
        "DTSTAMP",
        Some(&Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
    ))
}

fn update_task_component(
    existing: &str,
    title: &str,
    due: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    priority: u8,
    categories: &str,
    alarms: &[Alarm],
    description: &str,
    attachments: &str,
    status: &str,
    percent: u8,
) -> Result<String, String> {
    let draft = task_editor_draft_from_component(existing);
    update_task_component_with_dates(
        existing,
        title,
        &draft.dtstart.input,
        due,
        &draft.completed.input,
        recurrence,
        rdates,
        exdates,
        priority,
        categories,
        alarms,
        description,
        attachments,
        status,
        percent,
    )
}

fn fold_task_ical_component(raw: &str) -> String {
    let mut folded = String::new();
    for line in unfold_ical_lines(raw) {
        if line.len() <= 75 {
            folded.push_str(&line);
            folded.push_str("\r\n");
            continue;
        }

        let mut current = String::new();
        let mut first = true;
        for character in line.chars() {
            let limit = if first { 75 } else { 74 };
            if current.len() + character.len_utf8() > limit && !current.is_empty() {
                folded.push_str(&current);
                folded.push_str("\r\n");
                current.clear();
                first = false;
                current.push(' ');
            }
            current.push(character);
        }
        if !current.is_empty() {
            folded.push_str(&current);
            folded.push_str("\r\n");
        }
    }
    folded
}

fn ical_property(raw: &str, property_name: &str) -> Option<String> {
    let target = property_name
        .split(';')
        .next()
        .unwrap_or(property_name)
        .to_ascii_uppercase();
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
    lines.into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let property = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (property == target).then(|| unescape_ical_text(value))
    })
}

fn ical_property_with_parameters(raw: &str, property_name: &str) -> Option<(String, String)> {
    let target = property_name
        .split(';')
        .next()
        .unwrap_or(property_name)
        .to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (name == target).then(|| (property.to_string(), value.to_string()))
    })
}

fn ical_properties(raw: &str, property_name: &str) -> Vec<String> {
    let target = property_name
        .split(';')
        .next()
        .unwrap_or(property_name)
        .to_ascii_uppercase();
    unfold_ical_lines(raw)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let property = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            (property == target).then(|| unescape_ical_text(value))
        })
        .collect()
}

fn task_attachment_line_from_logical(line: &str) -> Option<TaskAttachmentLine> {
    let (property, value) = line.split_once(':')?;
    let name = property.split(';').next().unwrap_or(property).trim();
    if !name.eq_ignore_ascii_case("ATTACH") {
        return None;
    }
    Some(TaskAttachmentLine {
        raw: line.to_string(),
        property: property.to_string(),
        value: value.to_string(),
        kind: task_attachment_value_kind(property),
    })
}

fn task_top_level_attachment_lines(component: &str) -> Vec<TaskAttachmentLine> {
    let mut primary_seen = false;
    let mut nested_depth = 0usize;
    let mut attachments = Vec::new();
    for line in unfold_ical_lines(component) {
        if !primary_seen {
            primary_seen = line.eq_ignore_ascii_case("BEGIN:VTODO");
            continue;
        }
        if nested_depth == 0 {
            if let Some(attachment) = task_attachment_line_from_logical(&line) {
                attachments.push(attachment);
            }
        }
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
    attachments
}

fn task_attachment_value_kind(property: &str) -> TaskAttachmentValueKind {
    property
        .split(';')
        .skip(1)
        .find_map(|parameter| {
            let (name, value) = parameter.split_once('=')?;
            name.trim().eq_ignore_ascii_case("VALUE").then(|| {
                match value.trim().trim_matches('"').to_ascii_uppercase().as_str() {
                    "URI" => TaskAttachmentValueKind::Uri,
                    "BINARY" => TaskAttachmentValueKind::Binary,
                    _ => TaskAttachmentValueKind::Other,
                }
            })
        })
        .unwrap_or(TaskAttachmentValueKind::Other)
}

fn parse_task_attachment_input(value: &str) -> Result<Vec<TaskAttachmentLine>, String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            if line
                .chars()
                .any(|character| character == '\r' || character == '\n' || character.is_control())
            {
                return Err("Anhänge enthalten ungültige Steuerzeichen".to_string());
            }
            let (property, value) = line
                .split_once(':')
                .filter(|(property, _)| {
                    property
                        .split(';')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("ATTACH"))
                })
                .unwrap_or(("ATTACH;VALUE=URI", line));
            if property
                .chars()
                .any(|character| character == '\r' || character == '\n' || character == ':')
            {
                return Err("Die ATTACH-Eigenschaft ist ungültig".to_string());
            }
            let safe_value = serialize_ical_attachment_value(value.trim());
            if safe_value.is_empty() {
                return Err("Ein Anhang benötigt einen URI- oder BINARY-Wert".to_string());
            }
            Ok(TaskAttachmentLine {
                raw: format!("{property}:{safe_value}"),
                property: property.to_string(),
                value: safe_value,
                kind: task_attachment_value_kind(property),
            })
        })
        .collect()
}

fn replace_task_attachments_lossless(
    component: &str,
    attachments: &[TaskAttachmentLine],
) -> String {
    let existing = TaskAttachmentDraft::from_component(component).lines;
    let serialized = attachments
        .iter()
        .map(|attachment| {
            existing
                .iter()
                .find(|current| {
                    current.raw == attachment.raw
                        || (attachment.kind == TaskAttachmentValueKind::Uri
                            && current.kind == TaskAttachmentValueKind::Uri
                            && current.value == attachment.value)
                })
                .map_or(attachment.raw.as_str(), |current| current.raw.as_str())
        })
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut primary_seen = false;
    let mut nested_depth = 0usize;
    let mut replaced = false;

    for line in unfold_ical_lines(component) {
        if !primary_seen {
            output.push(line.clone());
            if line.eq_ignore_ascii_case("BEGIN:VTODO") {
                primary_seen = true;
            }
            continue;
        }

        let is_top_level_attach = nested_depth == 0
            && line.split_once(':').is_some_and(|(property, _)| {
                property
                    .split(';')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case("ATTACH"))
            });
        if is_top_level_attach {
            if !replaced {
                output.extend(serialized.iter().map(|line| (*line).to_string()));
                replaced = true;
            }
            continue;
        }

        if nested_depth == 0 && line.eq_ignore_ascii_case("END:VTODO") {
            if !replaced {
                output.extend(serialized.iter().map(|line| (*line).to_string()));
                replaced = true;
            }
            output.push(line);
            continue;
        }

        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            nested_depth = nested_depth.saturating_add(1);
        }
        output.push(line.clone());
        if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            nested_depth = nested_depth.saturating_sub(1);
        }
    }

    if !replaced {
        output.extend(serialized.iter().map(|line| (*line).to_string()));
    }
    fold_task_ical_component(&format!("{}\r\n", output.join("\r\n")))
}

fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn serialize_categories(value: &str) -> String {
    value
        .split(',')
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .map(escape_ical_text)
        .collect::<Vec<_>>()
        .join(",")
}

fn unfold_ical_lines(raw: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::reminder_service::AlarmRelation;

    fn fixed_context(date: &str, hour: u32) -> TaskFilterContext {
        let today = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
        let now = today.and_hms_opt(hour, 30, 0).unwrap();
        TaskFilterContext {
            today,
            now,
            now_timestamp: now.and_utc().timestamp(),
        }
    }

    fn fixture_task(
        uid: &str,
        title: &str,
        due: Option<&str>,
        start: Option<&str>,
        status: &str,
        categories: Option<&str>,
        attachment: bool,
        description: &str,
    ) -> TaskItemObject {
        let mut lines = vec![
            "BEGIN:VTODO".to_string(),
            format!("UID:{uid}"),
            format!("SUMMARY:{title}"),
            format!("STATUS:{status}"),
        ];
        if let Some(due) = due {
            lines.push(format!("DUE;VALUE=DATE:{}", due.replace('-', "")));
        }
        if let Some(start) = start {
            lines.push(format!("DTSTART;VALUE=DATE:{}", start.replace('-', "")));
        }
        if let Some(categories) = categories {
            lines.push(format!("CATEGORIES:{categories}"));
        }
        if attachment {
            lines.push("ATTACH;VALUE=URI:https://files.example.test/task.pdf".to_string());
        }
        if !description.is_empty() {
            lines.push(format!("DESCRIPTION:{description}"));
        }
        lines.push("END:VTODO".to_string());
        TaskItemObject::new(
            "local-calendar",
            uid,
            title,
            due.unwrap_or_default(),
            "1",
            status == "COMPLETED",
            &format!("{}\r\n", lines.join("\r\n")),
        )
    }

    fn task_selection(source_uid: &str, task_uid: &str) -> TaskSelectionIdentity {
        TaskSelectionIdentity {
            source_uid: source_uid.to_string(),
            task_uid: task_uid.to_string(),
        }
    }

    fn export_candidate(source_uid: &str, task_uid: &str, component: &str) -> TaskExportCandidate {
        TaskExportCandidate {
            identity: task_selection(source_uid, task_uid),
            component: component.to_string(),
        }
    }

    #[test]
    fn task_selection_actions_are_sensitive_for_zero_one_and_many() {
        assert_eq!(
            task_selection_action_state(0, false, false, false),
            TaskSelectionActionState {
                export_enabled: false,
                single_task_actions_enabled: false,
                forward_enabled: false,
                open_url_enabled: false,
                print_enabled: false,
            }
        );
        assert_eq!(
            task_selection_action_state(1, true, true, true),
            TaskSelectionActionState {
                export_enabled: true,
                single_task_actions_enabled: true,
                forward_enabled: true,
                open_url_enabled: true,
                print_enabled: true,
            }
        );
        assert_eq!(
            task_selection_action_state(2, true, true, true),
            TaskSelectionActionState {
                export_enabled: true,
                single_task_actions_enabled: false,
                forward_enabled: false,
                open_url_enabled: false,
                print_enabled: false,
            }
        );
    }

    #[test]
    fn combined_task_export_contains_exactly_the_selected_vtodos() {
        let first = task_selection("local-calendar", "first");
        let second = task_selection("local-calendar", "second");
        let export = build_selected_task_export(
            &[first.clone(), second.clone()],
            &[
                export_candidate(
                    "local-calendar",
                    "first",
                    "BEGIN:VTODO\r\nUID:first\r\nSUMMARY:First\r\nEND:VTODO\r\n",
                ),
                export_candidate(
                    "local-calendar",
                    "second",
                    "BEGIN:VTODO\r\nUID:second\r\nSUMMARY:Second\r\nEND:VTODO\r\n",
                ),
                export_candidate(
                    "local-calendar",
                    "unselected",
                    "BEGIN:VTODO\r\nUID:unselected\r\nEND:VTODO\r\n",
                ),
            ],
        )
        .expect("selected components should export");
        assert!(export.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert_eq!(export.matches("BEGIN:VTODO").count(), 2);
        assert!(export.contains("UID:first\r\n"));
        assert!(export.contains("UID:second\r\n"));
        assert!(!export.contains("UID:unselected\r\n"));
        assert!(export.ends_with("END:VCALENDAR\r\n"));
    }

    #[test]
    fn stale_or_invalid_task_selection_fails_before_export() {
        let selected = task_selection("local-calendar", "missing");
        assert!(build_selected_task_export(&[selected], &[]).is_err());

        let invalid = export_candidate(
            "local-calendar",
            "invalid",
            "BEGIN:VEVENT\r\nUID:invalid\r\nEND:VEVENT\r\n",
        );
        assert!(build_selected_task_export(&[invalid.identity.clone()], &[invalid]).is_err());
        let mismatched = export_candidate(
            "local-calendar",
            "selected",
            "BEGIN:VTODO\r\nUID:other\r\nEND:VTODO\r\n",
        );
        assert!(build_selected_task_export(&[mismatched.identity.clone()], &[mismatched]).is_err());
        let duplicate_uid = export_candidate(
            "local-calendar",
            "duplicate",
            "BEGIN:VTODO\r\nUID:duplicate\r\nUID:duplicate\r\nEND:VTODO\r\n",
        );
        assert!(
            build_selected_task_export(&[duplicate_uid.identity.clone()], &[duplicate_uid])
                .is_err()
        );
        assert!(build_selected_task_export(&[], &[]).is_err());
    }

    #[test]
    fn task_clipboard_action_sensitivity_matches_copy_cut_paste_select_all_contract() {
        let none = task_clipboard_action_state(0, false, false, true, true, false);
        assert!(!none.copy_enabled && !none.cut_enabled);
        assert!(none.paste_enabled && none.select_all_enabled);

        let copy_only = task_clipboard_action_state(2, true, false, true, true, false);
        assert!(copy_only.copy_enabled);
        assert!(!copy_only.cut_enabled);

        let ready = task_clipboard_action_state(2, true, true, true, true, false);
        assert!(ready.copy_enabled && ready.cut_enabled && ready.paste_enabled);
        assert!(ready.select_all_enabled);

        let no_calendar = task_clipboard_action_state(1, true, true, true, false, false);
        assert!(!no_calendar.paste_enabled);

        let busy = task_clipboard_action_state(1, true, true, true, true, true);
        assert!(!busy.copy_enabled && !busy.cut_enabled && !busy.paste_enabled);
        assert!(busy.select_all_enabled);
    }

    #[test]
    fn task_clipboard_round_trip_preserves_vtodo_and_rekeys_destination_identity() {
        let source_first = task_selection("source-a", "first");
        let source_second = task_selection("source-b", "second");
        let payload = build_selected_task_export(
            &[source_first.clone(), source_second.clone()],
            &[
                export_candidate(
                    "source-a",
                    "first",
                    concat!(
                        "BEGIN:VTODO\r\nUID:first\r\nSUMMARY:First\r\nX-KEEP:value\r\n",
                        "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
                        "END:VTODO\r\n"
                    ),
                ),
                export_candidate(
                    "source-b",
                    "second",
                    "BEGIN:VTODO\r\nUID:second\r\nDESCRIPTION:Second\r\nEND:VTODO\r\n",
                ),
            ],
        )
        .unwrap();
        let plan = plan_task_clipboard_paste_with_uids(
            &payload,
            "local-calendar",
            &[],
            &["new-first".to_string(), "new-second".to_string()],
        )
        .unwrap();
        assert_eq!(plan.destination_uid, "local-calendar");
        assert_eq!(plan.mutations.len(), 2);
        assert_eq!(
            plan.mutations[0].identity,
            task_selection("local-calendar", "new-first")
        );
        assert!(plan.mutations[0].component.contains("UID:new-first\r\n"));
        assert!(plan.mutations[0].component.contains("X-KEEP:value\r\n"));
        assert!(plan.mutations[0].component.contains("BEGIN:VALARM\r\n"));
        assert!(plan.mutations[1].component.contains("UID:new-second\r\n"));
        assert!(!plan.mutations[0].component.contains("UID:first\r\n"));

        assert!(
            plan_task_clipboard_paste_with_uids(
                &payload,
                "local-calendar",
                &[task_selection("local-calendar", "new-first")],
                &["new-first".to_string(), "new-second".to_string()],
            )
            .is_err()
        );
        assert!(plan_task_clipboard_paste_with_uids(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:tz\r\nDTSTART;TZID=Europe/Berlin:20260823T120000\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
            "local-calendar",
            &[],
            &["new-tz".to_string()],
        )
        .is_err());
        let duplicate_source_uid_payload = concat!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
            "BEGIN:VTODO\r\nUID:shared\r\nEND:VTODO\r\n",
            "BEGIN:VTODO\r\nUID:shared\r\nEND:VTODO\r\n",
            "END:VCALENDAR\r\n"
        );
        let duplicate_source_uids = plan_task_clipboard_paste_with_uids(
            duplicate_source_uid_payload,
            "local-calendar",
            &[],
            &["fresh-one".to_string(), "fresh-two".to_string()],
        )
        .unwrap();
        assert_eq!(duplicate_source_uids.mutations.len(), 2);
        assert_eq!(
            parse_task_clipboard_payload("BEGIN:VTODO\r\nUID:bare\r\nEND:VTODO\r\n").unwrap(),
            vec!["BEGIN:VTODO\r\nUID:bare\r\nEND:VTODO".to_string()]
        );
    }

    #[test]
    fn task_paste_destination_uses_default_then_confirmed_local_fallback() {
        let source_uids = vec!["remote".to_string(), "local-calendar".to_string()];
        let capabilities = HashMap::from([("remote".to_string(), TaskSourceCapability::Unknown)]);
        assert_eq!(
            task_paste_destination(&source_uids, &capabilities, true, Some("remote")).unwrap(),
            "local-calendar"
        );
        assert!(
            task_paste_destination(&source_uids, &capabilities, false, Some("remote")).is_err()
        );
    }

    #[test]
    fn task_paste_reducer_accepts_exact_prefix_and_rejects_partial_stale_results() {
        let plan = TaskPastePlan {
            destination_uid: "local-calendar".to_string(),
            mutations: ["one", "two", "three"]
                .into_iter()
                .map(|uid| TaskPasteMutation {
                    identity: task_selection("local-calendar", uid),
                    component: format!("BEGIN:VTODO\r\nUID:{uid}\r\nEND:VTODO"),
                })
                .collect(),
        };
        let partial = reduce_task_paste_worker_result(
            &plan,
            &TaskPasteWorkerResult {
                created: vec![TaskPasteMutation {
                    component: "BEGIN:VTODO\r\nUID:one\r\nX-NORMALIZED:yes\r\nEND:VTODO"
                        .to_string(),
                    ..plan.mutations[0].clone()
                }],
                failure: Some((plan.mutations[1].identity.clone(), "failed".to_string())),
            },
        );
        assert_eq!(partial.created.len(), 1);
        assert_eq!(
            partial.unattempted,
            vec![plan.mutations[2].identity.clone()]
        );

        let stale = reduce_task_paste_worker_result(
            &plan,
            &TaskPasteWorkerResult {
                created: vec![plan.mutations[1].clone()],
                failure: None,
            },
        );
        assert!(stale.created.is_empty());
        assert!(stale.failure.is_some());
        assert_eq!(stale.unattempted.len(), 3);
        assert!(task_paste_result_is_current(4, 4));
        assert!(!task_paste_result_is_current(4, 3));
    }

    #[test]
    fn task_bulk_edit_transforms_only_opted_in_fields_and_preserves_categories_alarms_unknowns() {
        let component = concat!(
            "BEGIN:VTODO\r\nUID:bulk\r\nSUMMARY:Keep\r\nPRIORITY:4\r\n",
            "CATEGORIES:Research\\,Ops,Old\r\nRRULE:FREQ=DAILY\r\nX-KEEP:value\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VTODO\r\n"
        );
        let draft = TaskBulkEditDraft {
            due: Some("2026-08-24".to_string()),
            priority: Some(String::new()),
            class: Some("private".to_string()),
            estimated_duration: Some("PT1H30M".to_string()),
            categories_add: vec!["New".to_string()],
            categories_remove: vec!["Old".to_string()],
            ..TaskBulkEditDraft::default()
        };
        let updated =
            transform_task_bulk_edit_component(&component, &draft, "20260823T120000Z").unwrap();
        assert!(updated.contains("SUMMARY:Keep\r\n"));
        assert!(updated.contains("DUE;VALUE=DATE:20260824\r\n"));
        assert!(!updated.contains("PRIORITY:"));
        assert!(updated.contains("CLASS:PRIVATE\r\n"));
        assert!(updated.contains("ESTIMATED-DURATION:PT1H30M\r\n"));
        assert!(updated.contains("CATEGORIES:Research\\,Ops,New\r\n"));
        assert!(!updated.contains("CATEGORIES:Research\\,Ops,Old\r\n"));
        assert!(updated.contains("RRULE:FREQ=DAILY\r\n"));
        assert!(updated.contains("X-KEEP:value\r\n"));
        assert!(updated.contains("BEGIN:VALARM\r\n"));
        let item = task_item_from_confirmed_component("writable", "bulk", &updated).unwrap();
        assert_eq!(item.due_date(), "2026-08-24");
        assert_eq!(item.priority(), "normal");
        for invalid in ["PT", "PP1D", "P1", "P1H", "PT1M1H", "P1D1W", "P1W2D"] {
            assert!(validate_task_bulk_duration(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn task_bulk_edit_lifecycle_preflight_and_reducer_are_fail_closed() {
        let identity = task_selection("writable", "bulk");
        let candidates = vec![export_candidate(
            "writable",
            "bulk",
            "BEGIN:VTODO\r\nUID:bulk\r\nSTATUS:NEEDS-ACTION\r\nX-KEEP:yes\r\nEND:VTODO\r\n",
        )];
        let complete = TaskBulkEditDraft {
            percent: Some("100".to_string()),
            ..TaskBulkEditDraft::default()
        };
        let plan = build_task_bulk_edit_plan(
            std::slice::from_ref(&identity),
            &candidates,
            &task_lifecycle_capabilities(),
            true,
            &complete,
            "20260823T120000Z",
        )
        .unwrap();
        assert!(plan.mutations[0].component.contains("STATUS:COMPLETED\r\n"));
        assert!(
            plan.mutations[0]
                .component
                .contains("PERCENT-COMPLETE:100\r\n")
        );
        assert!(
            plan.mutations[0]
                .component
                .contains("COMPLETED:20260823T120000Z\r\n")
        );
        assert!(
            build_task_bulk_edit_plan(
                std::slice::from_ref(&identity),
                &candidates,
                &HashMap::from([("writable".to_string(), TaskSourceCapability::ReadOnly)]),
                true,
                &complete,
                "20260823T120000Z",
            )
            .is_err()
        );
        assert!(
            build_task_bulk_edit_plan(
                std::slice::from_ref(&identity),
                &candidates,
                &task_lifecycle_capabilities(),
                true,
                &TaskBulkEditDraft::default(),
                "20260823T120000Z",
            )
            .is_err()
        );
        let partial = TaskBulkEditWorkerResult {
            successes: vec![plan.mutations[0].clone()],
            failure: None,
        };
        let reduction = reduce_task_bulk_edit_result(&plan, &partial);
        assert_eq!(reduction.successes.len(), 1);
        assert!(reduction.failure.is_none());
        assert_eq!(reduction.unattempted, 0);
        let mut batch_plan = plan.clone();
        for uid in ["bulk-two", "bulk-three"] {
            batch_plan.mutations.push(TaskBulkEditMutation {
                identity: task_selection("writable", uid),
                component: format!("BEGIN:VTODO\r\nUID:{uid}\r\nEND:VTODO"),
            });
        }
        let reduction = reduce_task_bulk_edit_result(
            &batch_plan,
            &TaskBulkEditWorkerResult {
                successes: vec![batch_plan.mutations[0].clone()],
                failure: Some((
                    batch_plan.mutations[1].identity.clone(),
                    "backend failed".to_string(),
                )),
            },
        );
        assert!(reduction.failure.is_some());
        assert_eq!(reduction.unattempted, 1);
        let stale = TaskBulkEditWorkerResult {
            successes: vec![TaskBulkEditMutation {
                identity: task_selection("writable", "other"),
                component: plan.mutations[0].component.clone(),
            }],
            failure: None,
        };
        assert!(
            reduce_task_bulk_edit_result(&plan, &stale)
                .successes
                .is_empty()
        );
        assert!(task_bulk_edit_result_is_current(2, 2));
        assert!(!task_bulk_edit_result_is_current(2, 1));
    }

    #[test]
    fn multiple_selection_keeps_single_record_actions_disabled() {
        let actions = task_selection_action_state(3, true, true, true);
        assert!(actions.export_enabled);
        assert!(!actions.single_task_actions_enabled);
        assert!(!actions.forward_enabled && !actions.open_url_enabled && !actions.print_enabled);
    }

    #[test]
    fn task_forward_payload_is_exact_and_never_mutates_the_vtodo() {
        let selected = task_selection("local-calendar", "forward");
        let component = concat!(
            "BEGIN:VTODO\r\nUID:forward\r\nSUMMARY:Forward\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let payload = build_task_forward_payload(
            std::slice::from_ref(&selected),
            &[export_candidate("local-calendar", "forward", component)],
        )
        .expect("one exact VTODO should forward");
        assert!(payload.contains("METHOD:PUBLISH\r\n"));
        assert_eq!(payload.matches("BEGIN:VTODO").count(), 1);
        assert!(!payload.contains("BEGIN:VALARM"));
        assert_eq!(component.matches("BEGIN:VALARM").count(), 1);
        assert!(build_task_forward_payload(&[selected.clone(), selected], &[]).is_err());
    }

    #[test]
    fn task_open_url_requires_one_exact_http_vtodo_url() {
        let selected = task_selection("local-calendar", "url");
        let candidate = export_candidate(
            "local-calendar",
            "url",
            "BEGIN:VTODO\r\nUID:url\r\nURL:https://example.test/task\r\nEND:VTODO\r\n",
        );
        assert_eq!(
            selected_task_open_url(std::slice::from_ref(&selected), &[candidate]).unwrap(),
            "https://example.test/task"
        );
        for invalid in [
            "mailto:owner@example.test",
            "file:///tmp/task",
            "https://",
            "https://example.test bad",
            "https://:443/task",
            "https://example.test:not-a-port/task",
        ] {
            assert!(validate_task_open_url(invalid).is_err(), "{invalid:?}");
        }
        assert!(selected_task_open_url(&[selected], &[]).is_err());
    }

    #[test]
    fn task_print_document_is_escaped_immutable_and_single_selection_only() {
        let selected = task_selection("local-calendar", "print");
        let component = concat!(
            "BEGIN:VTODO\r\nUID:print\r\nSUMMARY:<script>\r\n",
            "DESCRIPTION:Line one\\n<script>alert(1)</script>\r\n",
            "DUE;VALUE=DATE:20260824\r\nEND:VTODO\r\n"
        );
        let document = build_task_print_document(
            std::slice::from_ref(&selected),
            &[export_candidate("local-calendar", "print", component)],
        )
        .expect("exact task should prepare a print document");
        assert!(document.html.contains("&lt;script&gt;"));
        assert!(!document.html.contains("<script>"));
        assert!(document.html.contains("<pre>"));
        assert!(document.plain_text.contains("Status:"));
        assert!(
            document
                .plain_text
                .contains("Line one\n<script>alert(1)</script>")
        );
        assert_eq!(component.matches("BEGIN:VTODO").count(), 1);
        assert!(build_task_print_document(&[selected], &[]).is_err());
        assert!(
            build_task_print_document(
                &[task_selection("local-calendar", "print")],
                &[export_candidate(
                    "local-calendar",
                    "print",
                    "BEGIN:VTODO\r\nUID:stale\r\nEND:VTODO\r\n",
                )],
            )
            .is_err()
        );
    }

    fn task_lifecycle_capabilities() -> HashMap<String, TaskSourceCapability> {
        HashMap::from([("writable".to_string(), TaskSourceCapability::Writable)])
    }

    #[test]
    fn task_lifecycle_actions_match_upstream_zero_one_many_and_writable_gating() {
        let incomplete = task_selection("writable", "incomplete");
        let complete = task_selection("writable", "complete");
        let candidates = vec![
            export_candidate(
                "writable",
                "incomplete",
                "BEGIN:VTODO\r\nUID:incomplete\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\n",
            ),
            export_candidate(
                "writable",
                "complete",
                "BEGIN:VTODO\r\nUID:complete\r\nSTATUS:COMPLETED\r\nCOMPLETED:20260823T120000Z\r\nEND:VTODO\r\n",
            ),
        ];
        let capabilities = task_lifecycle_capabilities();
        assert_eq!(
            task_lifecycle_action_state(&[], &candidates, &capabilities, true, false),
            TaskLifecycleActionState::default()
        );
        assert_eq!(
            task_lifecycle_action_state(
                std::slice::from_ref(&incomplete),
                &candidates,
                &capabilities,
                true,
                false,
            ),
            TaskLifecycleActionState {
                complete_enabled: true,
                incomplete_enabled: false,
            }
        );
        assert_eq!(
            task_lifecycle_action_state(
                &[incomplete, complete],
                &candidates,
                &capabilities,
                true,
                false,
            ),
            TaskLifecycleActionState {
                complete_enabled: true,
                incomplete_enabled: true,
            }
        );
        assert_eq!(
            task_lifecycle_action_state(
                &[task_selection("writable", "incomplete")],
                &candidates,
                &HashMap::from([("writable".to_string(), TaskSourceCapability::ReadOnly)]),
                true,
                false,
            ),
            TaskLifecycleActionState::default()
        );
        assert_eq!(
            task_lifecycle_action_state(
                &[task_selection("writable", "incomplete")],
                &candidates,
                &capabilities,
                true,
                true,
            ),
            TaskLifecycleActionState::default()
        );
    }

    #[test]
    fn task_lifecycle_plan_is_exact_and_preserves_non_lifecycle_data() {
        let incomplete = task_selection("writable", "incomplete");
        let complete = task_selection("writable", "complete");
        let another_incomplete = task_selection("writable", "another-incomplete");
        let candidates = vec![
            export_candidate(
                "writable",
                "incomplete",
                concat!(
                    "BEGIN:VTODO\r\nUID:incomplete\r\nSTATUS:NEEDS-ACTION\r\n",
                    "PERCENT-COMPLETE:42\r\nX-KEEP:preserve\r\n",
                    "BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\n",
                    "END:VTODO\r\n"
                ),
            ),
            export_candidate(
                "writable",
                "complete",
                "BEGIN:VTODO\r\nUID:complete\r\nSTATUS:COMPLETED\r\nCOMPLETED:20260820T010203Z\r\nX-KEEP:done\r\nEND:VTODO\r\n",
            ),
            export_candidate(
                "writable",
                "another-incomplete",
                "BEGIN:VTODO\r\nUID:another-incomplete\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\n",
            ),
        ];
        let plan = build_task_lifecycle_batch_plan(
            &[
                incomplete.clone(),
                complete.clone(),
                another_incomplete.clone(),
            ],
            &candidates,
            &task_lifecycle_capabilities(),
            true,
            TaskLifecycleBatchOperation::Complete,
            "20260823T120000Z",
        )
        .expect("all exact writable selections should plan");
        assert_eq!(plan.mutations.len(), 3);
        let newly_complete = &plan.mutations[0].component;
        assert!(newly_complete.contains("STATUS:COMPLETED\r\n"));
        assert!(newly_complete.contains("PERCENT-COMPLETE:100\r\n"));
        assert!(newly_complete.contains("COMPLETED:20260823T120000Z\r\n"));
        assert!(newly_complete.contains("X-KEEP:preserve\r\n"));
        assert!(newly_complete.contains("BEGIN:VALARM\r\nACTION:AUDIO\r\n"));
        let already_complete = &plan.mutations[1].component;
        assert!(already_complete.contains("PERCENT-COMPLETE:100\r\n"));
        assert!(already_complete.contains("COMPLETED:20260820T010203Z\r\n"));
        assert!(!already_complete.contains("COMPLETED:20260823T120000Z\r\n"));
        assert!(
            plan.mutations[2]
                .component
                .contains("COMPLETED:20260823T120000Z\r\n")
        );

        let incomplete_plan = build_task_lifecycle_batch_plan(
            &[incomplete, complete, another_incomplete],
            &candidates,
            &task_lifecycle_capabilities(),
            true,
            TaskLifecycleBatchOperation::Incomplete,
            "20260823T120000Z",
        )
        .expect("mixed selection should allow mark incomplete");
        for mutation in incomplete_plan.mutations {
            assert!(mutation.component.contains("STATUS:NEEDS-ACTION\r\n"));
            assert!(!mutation.component.contains("PERCENT-COMPLETE:"));
            assert!(!mutation.component.contains("COMPLETED:"));
        }

        assert!(
            build_task_lifecycle_batch_plan(
                &[task_selection("writable", "missing")],
                &candidates,
                &task_lifecycle_capabilities(),
                true,
                TaskLifecycleBatchOperation::Complete,
                "20260823T120000Z",
            )
            .is_err()
        );
        assert!(
            build_task_lifecycle_batch_plan(
                &[task_selection("writable", "incomplete")],
                &[candidates[0].clone(), candidates[0].clone(),],
                &task_lifecycle_capabilities(),
                true,
                TaskLifecycleBatchOperation::Complete,
                "20260823T120000Z",
            )
            .is_err()
        );
        assert!(
            build_task_lifecycle_batch_plan(
                &[task_selection("writable", "incomplete")],
                &[export_candidate(
                    "writable",
                    "incomplete",
                    "BEGIN:VTODO\r\nUID:other\r\nEND:VTODO\r\n",
                )],
                &task_lifecycle_capabilities(),
                true,
                TaskLifecycleBatchOperation::Complete,
                "20260823T120000Z",
            )
            .is_err()
        );
    }

    #[test]
    fn task_lifecycle_reducer_handles_full_partial_failure_and_stale_results() {
        let plan = TaskLifecycleBatchPlan {
            operation: TaskLifecycleBatchOperation::Complete,
            mutations: vec![
                TaskLifecycleMutation {
                    identity: task_selection("writable", "first"),
                    component: "BEGIN:VTODO\r\nUID:first\r\nEND:VTODO\r\n".to_string(),
                    completed: true,
                },
                TaskLifecycleMutation {
                    identity: task_selection("writable", "second"),
                    component: "BEGIN:VTODO\r\nUID:second\r\nEND:VTODO\r\n".to_string(),
                    completed: true,
                },
            ],
        };
        let full = reduce_task_lifecycle_batch_results(
            &plan,
            &[
                TaskLifecycleDispatchResult::Success(plan.mutations[0].clone()),
                TaskLifecycleDispatchResult::Success(plan.mutations[1].clone()),
            ],
        );
        assert_eq!(full.successes.len(), 2);
        assert!(full.failures.is_empty());

        let partial = reduce_task_lifecycle_batch_results(
            &plan,
            &[
                TaskLifecycleDispatchResult::Success(plan.mutations[0].clone()),
                TaskLifecycleDispatchResult::Failure {
                    identity: plan.mutations[1].identity.clone(),
                    error: "provider failure".to_string(),
                },
            ],
        );
        assert_eq!(partial.successes.len(), 1);
        assert_eq!(partial.failures.len(), 1);

        let failed = reduce_task_lifecycle_batch_results(
            &plan,
            &[
                TaskLifecycleDispatchResult::Failure {
                    identity: plan.mutations[0].identity.clone(),
                    error: "first failed".to_string(),
                },
                TaskLifecycleDispatchResult::Failure {
                    identity: plan.mutations[1].identity.clone(),
                    error: "second failed".to_string(),
                },
            ],
        );
        assert!(failed.successes.is_empty());
        assert_eq!(failed.failures.len(), 2);
        assert!(task_lifecycle_result_is_current(7, 7));
        assert!(!task_lifecycle_result_is_current(7, 6));
    }

    #[test]
    fn task_delete_selection_requires_exact_writable_zero_one_many_plan() {
        let first = task_selection("writable", "first");
        let second = task_selection("writable", "second");
        let candidates = vec![
            export_candidate(
                "writable",
                "first",
                "BEGIN:VTODO\r\nUID:first\r\nSUMMARY:First\r\nEND:VTODO\r\n",
            ),
            export_candidate(
                "writable",
                "second",
                "BEGIN:VTODO\r\nUID:second\r\nSUMMARY:Second\r\nEND:VTODO\r\n",
            ),
        ];
        let capabilities = task_lifecycle_capabilities();
        assert!(!task_delete_action_enabled(
            &[],
            &candidates,
            &capabilities,
            true,
            false,
        ));
        assert!(task_delete_action_enabled(
            std::slice::from_ref(&first),
            &candidates,
            &capabilities,
            true,
            false,
        ));
        assert!(task_delete_action_enabled(
            &[first.clone(), second.clone()],
            &candidates,
            &capabilities,
            true,
            false,
        ));
        assert!(!task_delete_action_enabled(
            std::slice::from_ref(&first),
            &candidates,
            &HashMap::from([("writable".to_string(), TaskSourceCapability::ReadOnly)]),
            true,
            false,
        ));
        assert!(!task_delete_action_enabled(
            std::slice::from_ref(&first),
            &candidates,
            &capabilities,
            true,
            true,
        ));
        assert!(
            build_task_delete_plan(
                &[task_selection("writable", "missing")],
                &candidates,
                &capabilities,
                true,
            )
            .is_err()
        );
        assert!(
            build_task_delete_plan(
                std::slice::from_ref(&first),
                &[candidates[0].clone(), candidates[0].clone()],
                &capabilities,
                true,
            )
            .is_err()
        );
        assert!(
            build_task_delete_plan(&[first.clone(), first], &candidates, &capabilities, true,)
                .is_err()
        );
        assert!(
            build_task_delete_plan(
                &[second],
                &[export_candidate(
                    "writable",
                    "second",
                    "BEGIN:VTODO\r\nUID:other\r\nEND:VTODO\r\n",
                )],
                &capabilities,
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn task_purge_plans_all_completed_writable_tasks_without_selection() {
        let candidates = vec![
            export_candidate(
                "writable",
                "complete-status",
                "BEGIN:VTODO\r\nUID:complete-status\r\nSUMMARY:Done\r\nSTATUS:COMPLETED\r\nORGANIZER:mailto:owner@example.test\r\nEND:VTODO\r\n",
            ),
            export_candidate(
                "writable",
                "complete-percent",
                "BEGIN:VTODO\r\nUID:complete-percent\r\nPERCENT-COMPLETE:100\r\nEND:VTODO\r\n",
            ),
            export_candidate(
                "writable",
                "open",
                "BEGIN:VTODO\r\nUID:open\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\n",
            ),
            export_candidate(
                "readonly",
                "readonly-complete",
                "BEGIN:VTODO\r\nUID:readonly-complete\r\nSTATUS:COMPLETED\r\nEND:VTODO\r\n",
            ),
        ];
        let capabilities = HashMap::from([
            ("writable".to_string(), TaskSourceCapability::Writable),
            ("readonly".to_string(), TaskSourceCapability::ReadOnly),
        ]);
        let plan = build_task_purge_plan(&candidates, &capabilities, true)
            .expect("completed tasks from writable sources should purge");
        assert_eq!(
            plan.mutations
                .iter()
                .map(|mutation| mutation.identity.task_uid.as_str())
                .collect::<Vec<_>>(),
            vec!["complete-status", "complete-percent"]
        );
        assert!(
            build_task_purge_plan(&candidates[2..], &capabilities, true).is_err(),
            "open and read-only completed tasks must not create a purge"
        );
        assert!(
            build_task_purge_plan(
                &[export_candidate(
                    "writable",
                    "instance",
                    "BEGIN:VTODO\r\nUID:instance\r\nRECURRENCE-ID:20260823T120000Z\r\nSTATUS:COMPLETED\r\nEND:VTODO\r\n",
                )],
                &capabilities,
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn task_delete_confirmation_is_named_counted_and_cancelled_without_dispatch() {
        let named = TaskDeletePlan {
            mutations: vec![TaskDeleteMutation {
                identity: task_selection("writable", "named"),
                summary: "<unsafe & task>".to_string(),
            }],
        };
        let named_prompt = task_delete_confirmation(&named);
        assert!(named_prompt.title.contains("&lt;unsafe &amp; task&gt;"));
        assert!(named_prompt.description.contains("unwiderruflich"));
        let many = TaskDeletePlan {
            mutations: vec![
                TaskDeleteMutation {
                    identity: task_selection("writable", "first"),
                    summary: "First".to_string(),
                },
                TaskDeleteMutation {
                    identity: task_selection("writable", "second"),
                    summary: "Second".to_string(),
                },
            ],
        };
        let count_prompt = task_delete_confirmation(&many);
        assert!(count_prompt.title.contains("2 Aufgaben"));
        assert!(count_prompt.description.contains("2 Aufgaben"));
        assert!(!task_delete_can_dispatch(false));
        assert!(task_delete_can_dispatch(true));
        assert!(task_delete_plan_matches_selection(
            &many,
            &[
                task_selection("writable", "first"),
                task_selection("writable", "second"),
            ],
        ));
        assert!(!task_delete_plan_matches_selection(
            &many,
            &[task_selection("writable", "second")],
        ));
    }

    #[test]
    fn task_delete_rejects_recurrence_and_single_assigned_but_allows_multi_assigned() {
        let capabilities = task_lifecycle_capabilities();
        let recurrence = export_candidate(
            "writable",
            "recurrence",
            "BEGIN:VTODO\r\nUID:recurrence\r\nRECURRENCE-ID:20260823T120000Z\r\nEND:VTODO\r\n",
        );
        assert!(task_delete_action_enabled(
            std::slice::from_ref(&recurrence.identity),
            std::slice::from_ref(&recurrence),
            &capabilities,
            true,
            false,
        ));
        assert!(
            build_task_delete_plan(
                &[recurrence.identity.clone()],
                &[recurrence],
                &capabilities,
                true,
            )
            .is_err()
        );
        let assigned = export_candidate(
            "writable",
            "assigned",
            "BEGIN:VTODO\r\nUID:assigned\r\nORGANIZER:mailto:owner@example.test\r\nATTENDEE:mailto:user@example.test\r\nEND:VTODO\r\n",
        );
        assert!(task_delete_action_enabled(
            std::slice::from_ref(&assigned.identity),
            std::slice::from_ref(&assigned),
            &capabilities,
            true,
            false,
        ));
        assert!(
            build_task_delete_plan(
                std::slice::from_ref(&assigned.identity),
                std::slice::from_ref(&assigned),
                &capabilities,
                true,
            )
            .is_err()
        );
        let plain = export_candidate(
            "writable",
            "plain",
            "BEGIN:VTODO\r\nUID:plain\r\nEND:VTODO\r\n",
        );
        assert!(
            build_task_delete_plan(
                &[assigned.identity.clone(), plain.identity.clone()],
                &[assigned, plain],
                &capabilities,
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn task_delete_reducer_stops_on_first_failure_and_rejects_stale_results() {
        let plan = TaskDeletePlan {
            mutations: ["first", "second", "third"]
                .into_iter()
                .map(|uid| TaskDeleteMutation {
                    identity: task_selection("writable", uid),
                    summary: uid.to_string(),
                })
                .collect(),
        };
        let full = reduce_task_delete_batch_result(
            &plan,
            &TaskDeleteWorkerResult {
                removed: plan
                    .mutations
                    .iter()
                    .map(|mutation| mutation.identity.clone())
                    .collect(),
                failure: None,
            },
        );
        assert_eq!(full.removed.len(), 3);
        assert!(full.failure.is_none() && full.unattempted.is_empty());

        let partial = reduce_task_delete_batch_result(
            &plan,
            &TaskDeleteWorkerResult {
                removed: vec![plan.mutations[0].identity.clone()],
                failure: Some((
                    plan.mutations[1].identity.clone(),
                    "provider failure".to_string(),
                )),
            },
        );
        assert_eq!(partial.removed, vec![plan.mutations[0].identity.clone()]);
        assert!(partial.failure.is_some());
        assert_eq!(
            partial.unattempted,
            vec![plan.mutations[2].identity.clone()]
        );

        let failed = reduce_task_delete_batch_result(
            &plan,
            &TaskDeleteWorkerResult {
                removed: vec![],
                failure: Some((plan.mutations[0].identity.clone(), "failed".to_string())),
            },
        );
        assert!(failed.removed.is_empty() && failed.failure.is_some());
        assert_eq!(failed.unattempted.len(), 2);

        let duplicate = reduce_task_delete_batch_result(
            &plan,
            &TaskDeleteWorkerResult {
                removed: vec![
                    plan.mutations[0].identity.clone(),
                    plan.mutations[0].identity.clone(),
                ],
                failure: None,
            },
        );
        assert!(duplicate.removed.is_empty() && duplicate.failure.is_some());
        assert_eq!(duplicate.unattempted.len(), 3);
        assert!(task_delete_result_is_current(4, 4));
        assert!(!task_delete_result_is_current(4, 3));
    }

    #[test]
    fn task_component_serialization_validates_and_escapes_fields() {
        let component = build_task_component_with_dates(
            "Prepare, report; Q4",
            "2026-08-10",
            "2026-08-11",
            "",
            "FREQ=WEEKLY;BYDAY=MO;COUNT=4",
            "2026-08-12,20260819",
            "2026-08-15",
            1,
            "Work, Important",
            &[Alarm::new_display_relative(
                AlarmRelation::Start,
                -30 * 60,
                None,
            )],
            "Line one\nLine two",
            "",
            "NEEDS-ACTION",
            0,
        )
        .expect("task component should be valid");
        assert!(component.starts_with("BEGIN:VTODO\r\n"));
        assert!(component.contains("SUMMARY:Prepare\\, report\\; Q4\r\n"));
        assert!(component.contains("DUE;VALUE=DATE:20260811\r\n"));
        assert!(component.contains("RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=4\r\n"));
        assert!(component.contains("RDATE;VALUE=DATE:20260812,20260819\r\n"));
        assert!(component.contains("EXDATE;VALUE=DATE:20260815\r\n"));
        assert!(component.contains("PRIORITY:1\r\n"));
        assert!(component.contains("CATEGORIES:Work,Important\r\n"));
        assert!(component.contains("TRIGGER:-PT30M\r\n"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(component.ends_with("END:VTODO\r\n"));
    }

    #[test]
    fn task_component_rejects_invalid_due_date_and_empty_title() {
        assert!(
            build_task_component(
                "",
                "2026-08-11",
                "",
                "",
                "",
                0,
                "",
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            )
            .is_err()
        );
        assert!(
            build_task_component(
                "Task",
                "11.08.2026",
                "",
                "",
                "",
                0,
                "",
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            )
            .is_err()
        );
        assert!(
            build_task_component(
                "Task",
                "",
                "",
                "",
                "",
                10,
                "",
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn task_component_update_preserves_uid_and_nested_alarm_data() {
        let existing = concat!(
            "BEGIN:VTODO\r\n",
            "UID:task-1\r\n",
            "DTSTAMP:20260810T090000Z\r\n",
            "DTSTART;VALUE=DATE:20260810\r\n",
            "STATUS:NEEDS-ACTION\r\n",
            "SUMMARY:Old title\r\n",
            "DUE;VALUE=DATE:20260811\r\n",
            "RDATE;VALUE=DATE:20260812\r\n",
            "EXDATE;VALUE=DATE:20260813\r\n",
            "DESCRIPTION:Old description\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "DESCRIPTION:Reminder\r\n",
            "END:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let updated = update_task_component(
            existing,
            "New, title",
            "2026-08-20",
            "FREQ=MONTHLY;COUNT=2",
            "2026-08-22,20260829",
            "20260824",
            2,
            "Work,Next",
            &[Alarm::new_display_relative(
                AlarmRelation::Start,
                -10 * 60,
                Some("Reminder".to_string()),
            )],
            "New description",
            "",
            "NEEDS-ACTION",
            0,
        )
        .expect("task update should be valid");
        assert!(updated.contains("UID:task-1\r\n"));
        assert!(updated.contains("SUMMARY:New\\, title\r\n"));
        assert!(updated.contains("DUE;VALUE=DATE:20260820\r\n"));
        assert!(updated.contains("RRULE:FREQ=MONTHLY;COUNT=2\r\n"));
        assert!(updated.contains("RDATE;VALUE=DATE:20260822,20260829\r\n"));
        assert!(updated.contains("EXDATE;VALUE=DATE:20260824\r\n"));
        assert!(updated.contains("PRIORITY:2\r\n"));
        assert!(updated.contains("CATEGORIES:Work,Next\r\n"));
        assert!(updated.contains("DESCRIPTION:New description\r\n"));
        assert!(updated.contains("BEGIN:VALARM\r\n"));
        assert!(updated.contains("TRIGGER:-PT10M\r\n"));
        assert!(updated.contains("DESCRIPTION:Reminder\r\n"));
        assert!(updated.contains("STATUS:NEEDS-ACTION\r\n"));
        assert!(!updated.contains("COMPLETED:"));

        let in_progress = update_task_component(
            existing,
            "New, title",
            "2026-08-20",
            "",
            "",
            "",
            2,
            "Work,Next",
            &[Alarm::new_display_relative(
                AlarmRelation::Start,
                -10 * 60,
                None,
            )],
            "New description",
            "",
            "IN-PROCESS",
            45,
        )
        .expect("in-progress task update should be valid");
        assert!(in_progress.contains("STATUS:IN-PROCESS\r\n"));
        assert!(in_progress.contains("PERCENT-COMPLETE:45\r\n"));
        assert!(!in_progress.contains("COMPLETED:"));

        let cleared = update_task_component(
            existing,
            "New, title",
            "2026-08-20",
            "",
            "",
            "",
            2,
            "Work,Next",
            &[],
            "New description",
            "",
            "NEEDS-ACTION",
            0,
        )
        .expect("task reminder should be removable");
        assert!(!cleared.contains("BEGIN:VALARM"));
    }

    #[test]
    fn task_attachments_roundtrip_and_preserve_existing_parameters() {
        let component = build_task_component(
            "Prepare report",
            "2026-08-11",
            "",
            "",
            "",
            1,
            "Work",
            &[],
            "",
            "https://files.example.test/report.pdf?kind=a,b;rev=2\nfile:///tmp/notes.txt",
            "NEEDS-ACTION",
            0,
        )
        .expect("task with attachments should serialize");
        assert!(
            component
                .contains("ATTACH;VALUE=URI:https://files.example.test/report.pdf?kind=a,b;rev=2")
        );
        assert!(component.contains("ATTACH;VALUE=URI:file:///tmp/notes.txt"));

        let existing = concat!(
            "BEGIN:VTODO\r\nUID:task-1\r\n",
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://files.example.test/report.pdf?kind=a,b;rev=2\r\n",
            "ATTACH;VALUE=URI:https://files.example.test/old.pdf\r\n",
            "END:VTODO\r\n"
        );
        let updated = update_task_component(
            existing,
            "Prepare report",
            "",
            "",
            "",
            "",
            0,
            "",
            &[],
            "",
            "https://files.example.test/report.pdf?kind=a,b;rev=2\nhttps://files.example.test/new.pdf",
            "NEEDS-ACTION",
            0,
        )
        .expect("task attachment update should be valid");
        let attachments = unfold_ical_lines(&updated)
            .into_iter()
            .filter(|line| line.starts_with("ATTACH;"))
            .collect::<Vec<_>>();
        assert_eq!(
            attachments,
            vec![
                "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://files.example.test/report.pdf?kind=a,b;rev=2",
                "ATTACH;VALUE=URI:https://files.example.test/new.pdf",
            ]
        );
        assert!(
            updated
                .split_terminator("\r\n")
                .filter(|line| !line.is_empty())
                .all(|line| line.len() <= 75)
        );
    }

    #[test]
    fn task_description_parser_unescapes_folded_ical_values() {
        let raw = "BEGIN:VTODO\r\nDESCRIPTION:Line one\\nLine\r\n two\r\nEND:VTODO\r\n";
        assert_eq!(
            ical_property(raw, "DESCRIPTION").as_deref(),
            Some("Line one\nLinetwo")
        );
    }

    #[test]
    fn task_filters_cover_date_priority_completion_and_text() {
        let today = Local::now().date_naive();
        let task = TaskItemObject::new(
            "local-calendar",
            "task-1",
            "Pay the electricity bill",
            &today.format("%Y-%m-%d").to_string(),
            "1",
            false,
            "BEGIN:VTODO\r\nSUMMARY:Pay the electricity bill\r\nCATEGORIES:Finance,Important\r\nEND:VTODO\r\n",
        );

        assert!(task_matches_filter(&task, TaskFilter::All, ""));
        assert!(task_matches_filter(&task, TaskFilter::Today, ""));
        assert!(task_matches_filter(&task, TaskFilter::Important, ""));
        assert!(task_matches_filter(&task, TaskFilter::All, "electricity"));
        assert!(task_matches_filter_with_source(
            &task,
            TaskFilter::All,
            Some("local-calendar"),
            ""
        ));
        assert!(task_matches_filter_with_source_and_category(
            &task,
            TaskFilter::All,
            None,
            Some("finance"),
            ""
        ));
        assert!(!task_matches_filter_with_source_and_category(
            &task,
            TaskFilter::All,
            None,
            Some("personal"),
            ""
        ));
        assert!(!task_matches_filter_with_source(
            &task,
            TaskFilter::All,
            Some("other-source"),
            ""
        ));
        assert!(!task_matches_filter(&task, TaskFilter::Completed, ""));

        task.set_completed(true);
        assert!(task_matches_filter(&task, TaskFilter::Completed, "bill"));
    }

    #[test]
    fn quick_edit_values_cover_daily_task_fields_and_preserve_other_data() {
        let task = TaskItemObject::new(
            "local-calendar",
            "task-quick-edit",
            "Prepare report",
            "2026-08-11",
            "2",
            false,
            concat!(
                "BEGIN:VTODO\r\nUID:task-quick-edit\r\n",
                "SUMMARY:Prepare report\r\nDUE;VALUE=DATE:20260811\r\n",
                "DTSTART;VALUE=DATE:20260810\r\n",
                "STATUS:IN-PROCESS\r\nPERCENT-COMPLETE:45\r\nPRIORITY:2\r\n",
                "CATEGORIES:Work,Finance\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\n",
                "ATTACH;VALUE=URI:https://files.example.test/report.pdf\r\n",
                "DESCRIPTION:Keep this description\r\n",
                "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT30M\r\n",
                "END:VALARM\r\nEND:VTODO\r\n"
            ),
        );

        let values = task_quick_edit_values(&task);
        assert_eq!(values.due, "2026-08-11");
        assert_eq!(values.priority, 2);
        assert_eq!(values.status, "IN-PROCESS");
        assert_eq!(values.percent, 45);
        assert_eq!(values.categories, "Work,Finance");

        let updated = update_task_quick_fields(
            &task,
            "2026-08-20",
            3,
            "Work,Personal",
            "COMPLETED",
            values.percent,
        )
        .expect("quick edit should produce a valid VTODO");
        assert!(updated.contains("DUE;VALUE=DATE:20260820\r\n"));
        assert!(updated.contains("PRIORITY:3\r\n"));
        assert!(updated.contains("CATEGORIES:Work,Personal\r\n"));
        assert!(updated.contains("STATUS:COMPLETED\r\n"));
        assert!(updated.contains("PERCENT-COMPLETE:100\r\n"));
        assert!(updated.contains("RRULE:FREQ=WEEKLY;COUNT=4\r\n"));
        assert!(updated.contains("TRIGGER:-PT30M\r\n"));
        assert!(updated.contains("DESCRIPTION:Keep this description\r\n"));
        assert!(updated.contains("ATTACH;VALUE=URI:https://files.example.test/report.pdf\r\n"));
    }

    #[test]
    fn task_search_matches_all_terms_and_visible_status_metadata() {
        let task = TaskItemObject::new(
            "local-calendar",
            "task-search",
            "Prepare quarterly report",
            "2026-08-20",
            "2",
            false,
            concat!(
                "BEGIN:VTODO\r\nSTATUS:IN-PROCESS\r\n",
                "CATEGORIES:Work,Finance\r\n",
                "DESCRIPTION:Send the figures to accounting\r\nEND:VTODO\r\n"
            ),
        );

        assert!(task_query_matches(&task, "  quarterly   finance "));
        assert!(task_query_matches(&task, "in bearbeitung"));
        assert!(task_query_matches(&task, "accounting"));
        assert!(!task_query_matches(&task, "quarterly personal"));
        assert_eq!(normalize_task_query("  Work\tFinance  "), "work finance");
    }

    #[test]
    fn recurring_task_uses_next_due_date_without_replacing_base_due_date() {
        let task = TaskItemObject::new(
            "local-calendar",
            "task-recurring",
            "Water plants",
            "2026-08-01",
            "normal",
            false,
            concat!(
                "BEGIN:VTODO\r\n",
                "UID:task-recurring\r\n",
                "DUE;VALUE=DATE:20260801\r\n",
                "RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=4\r\n",
                "RDATE;VALUE=DATE:20260812\r\n",
                "EXDATE;VALUE=DATE:20260812\r\n",
                "END:VTODO\r\n"
            ),
        );

        assert_eq!(task.due_date(), "2026-08-01");
        assert_eq!(
            task_next_due_date(&task, NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()),
            Some(NaiveDate::from_ymd_opt(2026, 8, 17).unwrap())
        );
        assert_eq!(
            task_next_due_date(&task, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()),
            None
        );
    }

    #[test]
    fn task_status_filters_cover_overdue_progress_and_cancelled_states() {
        let today = Local::now().date_naive();
        let overdue = TaskItemObject::new(
            "local-calendar",
            "task-overdue",
            "Overdue",
            &today.pred_opt().unwrap().format("%Y-%m-%d").to_string(),
            "normal",
            false,
            "BEGIN:VTODO\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\n",
        );
        assert!(task_matches_filter(&overdue, TaskFilter::Overdue, ""));

        let in_progress = TaskItemObject::new(
            "local-calendar",
            "task-progress",
            "In progress",
            "",
            "normal",
            false,
            "BEGIN:VTODO\r\nSTATUS:IN-PROCESS\r\nEND:VTODO\r\n",
        );
        assert!(task_matches_filter(
            &in_progress,
            TaskFilter::InProgress,
            ""
        ));
        assert!(!task_matches_filter(
            &in_progress,
            TaskFilter::Cancelled,
            ""
        ));

        let cancelled = TaskItemObject::new(
            "local-calendar",
            "task-cancelled",
            "Cancelled",
            "",
            "normal",
            false,
            "BEGIN:VTODO\r\nSTATUS:CANCELLED\r\nEND:VTODO\r\n",
        );
        assert!(task_matches_filter(&cancelled, TaskFilter::Cancelled, ""));
    }

    #[test]
    fn task_filter_truth_table_covers_upstream_daily_filters() {
        let context = fixed_context("2026-08-10", 10);
        let today = fixture_task(
            "today",
            "Plan review",
            Some("2026-08-10"),
            Some("2026-08-09"),
            "NEEDS-ACTION",
            Some("Work Team"),
            true,
            "Send the review to accounting",
        );
        let next_seven = fixture_task(
            "next-seven",
            "Next week",
            Some("2026-08-17"),
            None,
            "NEEDS-ACTION",
            None,
            false,
            "",
        );
        let after_seven = fixture_task(
            "after-seven",
            "Later",
            Some("2026-08-18"),
            None,
            "NEEDS-ACTION",
            None,
            false,
            "",
        );
        let no_due = fixture_task(
            "no-due",
            "Someday",
            None,
            None,
            "NEEDS-ACTION",
            None,
            false,
            "",
        );
        let completed = fixture_task(
            "completed",
            "Finished",
            Some("2026-08-18"),
            None,
            "COMPLETED",
            Some("Work"),
            false,
            "",
        );
        let future_start = fixture_task(
            "future-start",
            "Not started",
            None,
            Some("2026-08-11"),
            "NEEDS-ACTION",
            None,
            false,
            "",
        );

        let query = TaskQuery::default();
        let matches = |task: &TaskItemObject, filter: TaskFilter| {
            task_matches_filter_with_context(task, filter, None, None, &query, context)
        };

        assert!(matches(&today, TaskFilter::Today));
        assert!(matches(&today, TaskFilter::Overdue));
        assert!(matches(&next_seven, TaskFilter::Next7));
        assert!(!matches(&after_seven, TaskFilter::Next7));
        assert!(matches(&after_seven, TaskFilter::Next30));
        assert!(matches(&after_seven, TaskFilter::Next365));
        assert!(!matches(&completed, TaskFilter::Next365));
        assert!(matches(&no_due, TaskFilter::NoDue));
        assert!(!matches(&today, TaskFilter::NoDue));
        assert!(matches(&today, TaskFilter::Scheduled));
        assert!(matches(&today, TaskFilter::WithAttachments));
        assert!(matches(&today, TaskFilter::Started));
        assert!(matches(&no_due, TaskFilter::Started));
        assert!(!matches(&future_start, TaskFilter::Started));
        assert!(matches(&no_due, TaskFilter::WithoutCategory));
        assert!(matches(&today, TaskFilter::Uncompleted));
        assert!(!matches(&completed, TaskFilter::Uncompleted));
    }

    #[test]
    fn advanced_task_query_combines_fields_categories_dates_and_attachments() {
        let task = fixture_task(
            "advanced",
            "Plan review",
            Some("2026-08-10"),
            Some("2026-08-09"),
            "NEEDS-ACTION",
            Some("Work Team"),
            true,
            "Send the review to accounting",
        );
        let context = fixed_context("2026-08-10", 10);
        let query = TaskQuery::parse(
            TaskSearchMode::Advanced,
            r#"summary:"Plan review" description:accounting category:"Work Team" due:2026-08-10..2026-08-17 start:started status:open has:attachment"#,
        );
        assert!(query.matches(&task, context));

        let no_attachment = fixture_task(
            "advanced-no-attachment",
            "Plan review",
            Some("2026-08-10"),
            Some("2026-08-09"),
            "NEEDS-ACTION",
            Some("Work Team"),
            false,
            "Send the review to accounting",
        );
        assert!(!query.matches(&no_attachment, context));

        let no_category_query =
            TaskQuery::parse(TaskSearchMode::Advanced, "category:none due:none");
        let no_due = fixture_task(
            "advanced-no-category",
            "Someday",
            None,
            None,
            "NEEDS-ACTION",
            None,
            false,
            "",
        );
        assert!(no_category_query.matches(&no_due, context));
    }

    #[test]
    fn next_ranges_use_calendar_days_across_dst_boundary() {
        let context = fixed_context("2026-03-29", 1);
        let boundary = fixture_task(
            "dst-boundary",
            "DST boundary",
            Some("2026-04-05"),
            None,
            "NEEDS-ACTION",
            None,
            false,
            "",
        );
        let after_boundary = fixture_task(
            "dst-after-boundary",
            "After DST range",
            Some("2026-04-06"),
            None,
            "NEEDS-ACTION",
            None,
            false,
            "",
        );
        let query = TaskQuery::default();
        assert!(task_matches_filter_with_context(
            &boundary,
            TaskFilter::Next7,
            None,
            None,
            &query,
            context,
        ));
        assert!(!task_matches_filter_with_context(
            &after_boundary,
            TaskFilter::Next7,
            None,
            None,
            &query,
            context,
        ));
    }

    #[test]
    fn category_filters_keep_escaped_commas_as_one_category() {
        let task = TaskItemObject::new(
            "local-calendar",
            "escaped-category",
            "Research",
            "",
            "normal",
            false,
            "BEGIN:VTODO\r\nCATEGORIES:Research\\,Ops,Work\r\nEND:VTODO\r\n",
        );
        assert_eq!(
            task_categories(&task).collect::<Vec<_>>(),
            vec!["Research,Ops".to_string(), "Work".to_string()]
        );
        let query = TaskQuery::default();
        let context = fixed_context("2026-08-10", 10);
        assert!(task_matches_filter_with_context(
            &task,
            TaskFilter::All,
            None,
            Some("research,ops"),
            &query,
            context,
        ));
        assert!(!task_matches_filter_with_context(
            &task,
            TaskFilter::All,
            None,
            Some("research"),
            &query,
            context,
        ));
    }

    #[test]
    fn started_filter_compares_absolute_dtstart_timestamp() {
        let task = TaskItemObject::new(
            "local-calendar",
            "utc-start",
            "UTC start",
            "",
            "normal",
            false,
            "BEGIN:VTODO\r\nDTSTART:20260810T120000Z\r\nEND:VTODO\r\n",
        );
        let now = NaiveDate::from_ymd_opt(2026, 8, 10)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let now_timestamp = glib::DateTime::from_utc(2026, 8, 10, 13, 0, 0.0)
            .unwrap()
            .to_unix();
        let context = TaskFilterContext {
            today: now.date(),
            now,
            now_timestamp,
        };
        assert!(task_matches_filter_with_context(
            &task,
            TaskFilter::Started,
            None,
            None,
            &TaskQuery::default(),
            context,
        ));
    }

    #[test]
    fn task_editor_draft_preserves_date_only_and_unsupported_datetime_values() {
        let existing = concat!(
            "BEGIN:VTODO\r\n",
            "UID:task-date-types\r\n",
            "DTSTART;VALUE=DATE:20260810\r\n",
            "DUE;TZID=Europe/Berlin:20260811T120000\r\n",
            "COMPLETED:20260809T120000Z\r\n",
            "STATUS:COMPLETED\r\n",
            "PERCENT-COMPLETE:100\r\n",
            "RRULE:FREQ=WEEKLY;COUNT=2\r\n",
            "X-KEEP:one\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "TRIGGER:-PT15M\r\n",
            "END:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        assert_eq!(draft.uid, "task-date-types");
        assert_eq!(draft.dtstart.input, "2026-08-10");
        assert_eq!(draft.due.input, "2026-08-11");
        assert_eq!(draft.completed.input, "2026-08-09");
        assert_eq!(draft.due.original_kind, Some(TaskDateValueKind::DateTime));
        assert_eq!(
            draft.completed.original_kind,
            Some(TaskDateValueKind::DateTime)
        );

        let updated = update_task_component_with_dates(
            existing,
            "Renamed task",
            &draft.dtstart.input,
            &draft.due.input,
            &draft.completed.input,
            &draft.recurrence,
            &draft.rdates,
            &draft.exdates,
            draft.priority,
            &draft.categories,
            &parse_alarms(existing),
            &draft.description,
            &draft.attachments,
            "COMPLETED",
            100,
        )
        .expect("unchanged imported dates should be accepted");
        assert!(updated.contains("DTSTART;VALUE=DATE:20260810\r\n"));
        assert!(updated.contains("DUE;TZID=Europe/Berlin:20260811T120000\r\n"));
        assert!(updated.contains("COMPLETED:20260809T120000Z\r\n"));
        assert!(updated.contains("UID:task-date-types\r\n"));
        assert!(updated.contains("X-KEEP:one\r\n"));
        assert!(updated.contains("RRULE:FREQ=WEEKLY;COUNT=2\r\n"));

        let unsupported = existing.replace(
            "DUE;TZID=Europe/Berlin:20260811T120000",
            "DUE;TZID=Europe/Berlin:20260811T120000.000",
        );
        let unsupported_draft = task_editor_draft_from_component(&unsupported);
        let preserved_unsupported = update_task_component_with_dates(
            &unsupported,
            "Renamed task",
            &unsupported_draft.dtstart.input,
            &unsupported_draft.due.input,
            &unsupported_draft.completed.input,
            &unsupported_draft.recurrence,
            &unsupported_draft.rdates,
            &unsupported_draft.exdates,
            unsupported_draft.priority,
            &unsupported_draft.categories,
            &parse_alarms(&unsupported),
            &unsupported_draft.description,
            &unsupported_draft.attachments,
            "COMPLETED",
            100,
        )
        .expect("unchanged unsupported date-time values should be preserved");
        assert!(preserved_unsupported.contains("DUE;TZID=Europe/Berlin:20260811T120000.000\r\n"));

        let deliberately_edited = update_task_component_with_dates(
            existing,
            "Renamed task",
            "2026-08-12",
            "2026-08-13",
            "2026-08-08",
            &draft.recurrence,
            &draft.rdates,
            &draft.exdates,
            draft.priority,
            &draft.categories,
            &parse_alarms(existing),
            &draft.description,
            &draft.attachments,
            "COMPLETED",
            100,
        )
        .expect("deliberately edited date-time values should become date-only");
        assert!(deliberately_edited.contains("DTSTART;VALUE=DATE:20260812\r\n"));
        assert!(deliberately_edited.contains("DUE;VALUE=DATE:20260813\r\n"));
        assert!(deliberately_edited.contains("COMPLETED;VALUE=DATE:20260808\r\n"));
        assert!(!deliberately_edited.contains("DUE;TZID=Europe/Berlin:20260811T120000\r\n"));
        assert!(!deliberately_edited.contains("COMPLETED:20260809T120000Z\r\n"));
    }

    #[test]
    fn task_editor_rejects_invalid_mixed_and_out_of_order_dates() {
        assert!(
            build_task_component_with_dates(
                "Task",
                "2026-02-30",
                "",
                "",
                "",
                "",
                "",
                0,
                "",
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            )
            .is_err()
        );
        assert!(
            build_task_component_with_dates(
                "Task",
                "2026-08-10",
                "20260811",
                "",
                "",
                "",
                "",
                0,
                "",
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            )
            .is_err()
        );
        assert!(
            build_task_component_with_dates(
                "Task",
                "2026-08-11",
                "2026-08-10",
                "",
                "",
                "",
                "",
                0,
                "",
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            )
            .is_err()
        );
        assert!(parse_task_date_property("DTSTART;VALUE=DATE", "20260810T120000Z").is_err());
        assert!(parse_task_date_property("DUE;VALUE=DATE-TIME", "20260810").is_err());
    }

    #[test]
    fn task_editor_requires_start_for_recurrence() {
        let result = build_task_component_with_dates(
            "Recurring task",
            "",
            "",
            "",
            "FREQ=WEEKLY;COUNT=2",
            "",
            "",
            0,
            "",
            &[],
            "",
            "",
            "NEEDS-ACTION",
            0,
        );
        assert!(
            result
                .expect_err("recurrence without DTSTART must fail")
                .contains("DTSTART")
        );
    }

    #[test]
    fn task_editor_lifecycle_transitions_enforce_gtk3_invariants() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 23).unwrap();
        let needs_action =
            task_lifecycle_for_date("NEEDS-ACTION", 72, Some(today.pred_opt().unwrap()), today)
                .unwrap();
        assert_eq!(needs_action.percent, 0);
        assert_eq!(needs_action.completed, None);

        let in_process = task_lifecycle_for_date("IN-PROCESS", 0, None, today).unwrap();
        assert_eq!(in_process.percent, 50);
        assert_eq!(in_process.completed, None);
        let in_process_100 = task_lifecycle_for_date("IN-PROCESS", 100, None, today).unwrap();
        assert_eq!(in_process_100.percent, 50);

        let completed =
            task_lifecycle_for_date("COMPLETED", 25, Some(today.pred_opt().unwrap()), today)
                .unwrap();
        assert_eq!(completed.percent, 100);
        assert!(completed.completed.is_some());
        assert!(task_lifecycle_for_date("COMPLETED", 100, None, today).is_err());
        assert!(
            task_lifecycle_for_date("COMPLETED", 100, Some(today.succ_opt().unwrap()), today)
                .is_err()
        );

        let completed_component = concat!(
            "BEGIN:VTODO\r\nUID:lifecycle\r\nSUMMARY:Lifecycle\r\n",
            "STATUS:COMPLETED\r\nPERCENT-COMPLETE:100\r\n",
            "COMPLETED;VALUE=DATE:20260820\r\nEND:VTODO\r\n"
        );
        let reopened = update_task_component_with_dates(
            completed_component,
            "Lifecycle",
            "",
            "",
            "",
            "",
            "",
            "",
            0,
            "",
            &[],
            "",
            "",
            "NEEDS-ACTION",
            100,
        )
        .unwrap();
        assert!(reopened.contains("STATUS:NEEDS-ACTION\r\n"));
        assert!(reopened.contains("PERCENT-COMPLETE:0\r\n"));
        assert!(!reopened.contains("COMPLETED:"));
    }

    #[test]
    fn task_editor_source_and_generation_guards_block_unavailable_or_stale_work() {
        let capabilities = Rc::new(RefCell::new(HashMap::from([
            ("live-source".to_string(), TaskSourceCapability::Unknown),
            (
                "read-only-source".to_string(),
                TaskSourceCapability::ReadOnly,
            ),
            (
                "cached-source".to_string(),
                TaskSourceCapability::Unavailable,
            ),
            (
                "writable-source".to_string(),
                TaskSourceCapability::Writable,
            ),
        ])));
        assert!(!task_source_is_writable_with_local(
            "live-source",
            &capabilities,
            false
        ));
        assert!(!task_source_is_writable_with_local(
            "read-only-source",
            &capabilities,
            false
        ));
        assert!(!task_source_is_writable_with_local(
            "cached-source",
            &capabilities,
            true
        ));
        assert!(task_source_is_writable_with_local(
            "writable-source",
            &capabilities,
            false
        ));
        assert!(task_source_is_writable_with_local(
            "local-calendar",
            &capabilities,
            true
        ));
        assert!(!task_source_is_writable_with_local(
            "local-calendar",
            &capabilities,
            false
        ));
        assert!(!task_source_is_writable_with_local(
            "missing-source",
            &capabilities,
            true
        ));

        let source_uids = vec![
            "placeholder".to_string(),
            "read-only-source".to_string(),
            "live-source".to_string(),
            "local-calendar".to_string(),
            "writable-source".to_string(),
        ];
        assert_eq!(
            task_default_source_index(
                &source_uids,
                Some("read-only-source"),
                None,
                &capabilities.borrow(),
                true,
            ),
            Some(3),
            "a new task must fall back to the first confirmed writable source"
        );
        assert_eq!(
            task_default_source_index(
                &source_uids,
                Some("writable-source"),
                None,
                &capabilities.borrow(),
                true,
            ),
            Some(4)
        );
        assert_eq!(
            task_default_source_index(
                &source_uids,
                None,
                Some("read-only-source"),
                &capabilities.borrow(),
                true,
            ),
            Some(1),
            "an edit stays on its original source and is rejected as read-only"
        );

        assert_eq!(
            task_close_decision(false, TaskSavePhase::Editing, false),
            TaskCloseDecision::Allow
        );
        assert_eq!(
            task_close_decision(true, TaskSavePhase::Editing, false),
            TaskCloseDecision::Prompt
        );
        assert_eq!(
            task_close_decision(true, TaskSavePhase::Editing, true),
            TaskCloseDecision::KeepPrompt
        );
        assert_eq!(
            task_close_decision(true, TaskSavePhase::DispatchPending, false),
            TaskCloseDecision::BlockSave
        );
        assert_eq!(
            task_close_decision(true, TaskSavePhase::Committing, false),
            TaskCloseDecision::BlockCommit
        );
        assert!(task_editor_result_is_current(7, 7, true));
        assert!(!task_editor_result_is_current(8, 7, true));
        assert!(!task_editor_result_is_current(7, 7, false));

        let failed = task_editor_result_state(true, false, false);
        assert_eq!(failed.apply_row, false);
        assert!(failed.dirty);
        let successful = task_editor_result_state(true, true, true);
        assert!(successful.apply_row);
        assert!(!successful.dirty);
        let stale = task_editor_result_state(false, true, true);
        assert!(!stale.apply_row);
        assert!(stale.dirty);
    }

    #[test]
    fn task_editor_dialog_boundary_covers_save_discard_continue_and_commit() {
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Dirty,
                TaskEditorDialogAction::CloseRequest,
            ),
            (
                TaskEditorDialogState::Prompting,
                TaskEditorDialogEffect::ShowPrompt
            )
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Prompting,
                TaskEditorDialogAction::ContinueEditing,
            ),
            (TaskEditorDialogState::Dirty, TaskEditorDialogEffect::None)
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Prompting,
                TaskEditorDialogAction::Discard,
            ),
            (TaskEditorDialogState::Closed, TaskEditorDialogEffect::Close)
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Prompting,
                TaskEditorDialogAction::Save,
            ),
            (
                TaskEditorDialogState::SavePending,
                TaskEditorDialogEffect::DispatchSave
            )
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::SavePending,
                TaskEditorDialogAction::DispatchStarted,
            ),
            (
                TaskEditorDialogState::Committing,
                TaskEditorDialogEffect::DisableClose
            )
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::SavePending,
                TaskEditorDialogAction::CancelBeforeDispatch,
            ),
            (TaskEditorDialogState::Closed, TaskEditorDialogEffect::Close)
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Committing,
                TaskEditorDialogAction::CloseRequest,
            ),
            (
                TaskEditorDialogState::Committing,
                TaskEditorDialogEffect::BlockClose
            )
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Committing,
                TaskEditorDialogAction::BackendFailure,
            ),
            (TaskEditorDialogState::Dirty, TaskEditorDialogEffect::None)
        );
    }

    #[test]
    fn task_save_cancel_is_honest_at_the_synchronous_dispatch_boundary() {
        let cancelled = TaskSaveControl::new();
        assert!(cancelled.arm());
        assert_eq!(
            cancelled.cancel_before_dispatch(),
            TaskCancelResult::CancelledBeforeDispatch
        );
        let mut backend_calls = 0;
        if cancelled.begin_dispatch() {
            backend_calls += 1;
        }
        assert_eq!(backend_calls, 0, "cancel-before-dispatch must not mutate");
        assert_eq!(cancelled.phase(), TaskSavePhase::CancelledBeforeDispatch);
        assert_eq!(
            task_close_decision(false, cancelled.phase(), false),
            TaskCloseDecision::Allow
        );

        let committing = TaskSaveControl::new();
        assert!(committing.arm());
        assert!(committing.begin_dispatch());
        assert_eq!(committing.phase(), TaskSavePhase::Committing);
        assert_eq!(
            committing.cancel_before_dispatch(),
            TaskCancelResult::Committing
        );
        assert_eq!(
            task_close_decision(true, committing.phase(), false),
            TaskCloseDecision::BlockCommit
        );
        assert_eq!(
            task_editor_dialog_transition(
                TaskEditorDialogState::Committing,
                TaskEditorDialogAction::CancelBeforeDispatch,
            ),
            (
                TaskEditorDialogState::Committing,
                TaskEditorDialogEffect::BlockClose
            )
        );
    }

    #[test]
    fn task_editor_result_generation_is_stale_a_b_and_failure_is_non_optimistic() {
        let current_generation = 12;
        let generation_a = 11;
        let generation_b = 12;
        assert!(!task_editor_result_is_current(
            current_generation,
            generation_a,
            true
        ));
        assert!(task_editor_result_is_current(
            current_generation,
            generation_b,
            true
        ));

        let failed = task_editor_result_state(true, false, false);
        assert!(!failed.apply_row);
        assert!(failed.dirty);
        let success = task_editor_result_state(true, true, true);
        assert!(success.apply_row);
        assert!(!success.dirty);
        let stale_success = task_editor_result_state(false, true, true);
        assert!(!stale_success.apply_row);
        assert!(stale_success.dirty);
    }

    #[test]
    fn task_editor_requires_a_confirmed_authoritative_component() {
        let confirmed = "BEGIN:VTODO\r\nUID:confirmed\r\nEND:VTODO\r\n";
        assert_eq!(
            task_authoritative_component("confirmed", "confirmed", Some(confirmed.to_string()))
                .unwrap(),
            confirmed
        );
        assert!(task_authoritative_component("confirmed", "confirmed", None).is_err());
        assert!(
            task_authoritative_component("confirmed", "other", Some(confirmed.to_string()))
                .is_err()
        );
    }

    #[test]
    fn task_editor_displays_local_and_utc_dates_without_rewriting_unchanged_values() {
        let floating = concat!(
            "BEGIN:VTODO\r\nUID:floating\r\n",
            "DUE:20260810T120000\r\nEND:VTODO\r\n"
        );
        let floating_draft = task_editor_draft_from_component(floating);
        assert_eq!(floating_draft.due.input, "2026-08-10");

        let utc = concat!(
            "BEGIN:VTODO\r\nUID:utc\r\n",
            "DUE:20260810T233000Z\r\nEND:VTODO\r\n"
        );
        let utc_draft = task_editor_draft_from_component(utc);
        assert_eq!(
            utc_draft.due.input,
            property_local_date(utc, "DUE", false)
                .unwrap()
                .format("%Y-%m-%d")
                .to_string()
        );
        let unchanged = update_task_component_with_dates(
            utc,
            "UTC",
            "",
            &utc_draft.due.input,
            "",
            "",
            "",
            "",
            0,
            "",
            &[],
            "",
            &utc_draft.attachments,
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        assert!(unchanged.contains("DUE:20260810T233000Z\r\n"));
    }

    #[test]
    fn task_editor_pairs_date_and_datetime_fields_when_one_is_edited() {
        let existing = concat!(
            "BEGIN:VTODO\r\nUID:paired\r\n",
            "DTSTART;VALUE=DATE:20260810\r\n",
            "DUE;TZID=Europe/Berlin:20260811T120000\r\n",
            "STATUS:NEEDS-ACTION\r\nPERCENT-COMPLETE:0\r\n",
            "END:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        let edited = update_task_component_with_dates(
            existing,
            "Paired",
            &draft.dtstart.input,
            "2026-08-12",
            "",
            "",
            "",
            "",
            0,
            "",
            &[],
            "",
            &draft.attachments,
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        assert!(edited.contains("DTSTART:20260810T000000\r\n"));
        assert!(edited.contains("DUE;TZID=Europe/Berlin:20260812T120000\r\n"));
        assert!(!edited.contains("DTSTART;VALUE=DATE:20260810\r\n"));
        assert!(!edited.contains("DUE;VALUE=DATE:20260812\r\n"));
    }

    #[test]
    fn task_editor_preserves_unsupported_imported_lifecycle_until_explicit_change() {
        let existing = concat!(
            "BEGIN:VTODO\r\nUID:unsupported-lifecycle\r\nSUMMARY:Imported\r\n",
            "STATUS:X-IMPORTED\r\nPERCENT-COMPLETE:bogus\r\n",
            "END:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        assert_eq!(draft.unsupported_status.as_deref(), Some("X-IMPORTED"));
        assert_eq!(draft.unsupported_percent.as_deref(), Some("bogus"));
        assert_eq!(
            task_initial_editor_lifecycle(&draft, NaiveDate::from_ymd_opt(2026, 8, 23).unwrap()),
            (0, "".to_string())
        );
        let unchanged = update_task_component_with_dates(
            existing,
            "Imported",
            "",
            "",
            "",
            "",
            "",
            "",
            0,
            "",
            &[],
            "",
            &draft.attachments,
            draft.status,
            draft.percent,
        )
        .unwrap();
        assert!(unchanged.contains("STATUS:X-IMPORTED\r\n"));
        assert!(unchanged.contains("PERCENT-COMPLETE:bogus\r\n"));
    }

    #[test]
    fn task_editor_preserves_each_imported_lifecycle_property_on_unrelated_save() {
        let unknown_status = concat!(
            "BEGIN:VTODO\r\nUID:unknown-status\r\nSUMMARY:Imported\r\n",
            "STATUS:X-IMPORTED\r\nPERCENT-COMPLETE:37\r\n",
            "X-UNKNOWN:keep\r\nBEGIN:VALARM\r\nACTION:AUDIO\r\n",
            "TRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VTODO\r\n"
        );
        let unknown_percent = concat!(
            "BEGIN:VTODO\r\nUID:unknown-percent\r\nSUMMARY:Imported\r\n",
            "STATUS:IN-PROCESS\r\nPERCENT-COMPLETE:bogus\r\n",
            "ATTACH;VALUE=URI:https://example.test/keep\r\n",
            "END:VTODO\r\n"
        );

        for (existing, expected_status, expected_percent, preserved) in [
            (
                unknown_status,
                "STATUS:X-IMPORTED\r\n",
                "PERCENT-COMPLETE:37\r\n",
                "X-UNKNOWN:keep\r\n",
            ),
            (
                unknown_percent,
                "STATUS:IN-PROCESS\r\n",
                "PERCENT-COMPLETE:bogus\r\n",
                "ATTACH;VALUE=URI:https://example.test/keep\r\n",
            ),
        ] {
            let draft = task_editor_draft_from_component(existing);
            let updated = update_task_component_with_dates(
                existing,
                "Changed only the title",
                &draft.dtstart.input,
                &draft.due.input,
                &draft.completed.input,
                &draft.recurrence,
                &draft.rdates,
                &draft.exdates,
                draft.priority,
                &draft.categories,
                &parse_alarms(existing),
                &draft.description,
                &draft.attachments,
                draft.status,
                draft.percent,
            )
            .expect("unrelated edit must retain imported lifecycle data");
            assert!(updated.contains(expected_status));
            assert!(updated.contains(expected_percent));
            assert!(updated.contains(preserved));
        }
    }

    #[test]
    fn task_editor_does_not_invent_a_missing_percent_on_unrelated_save() {
        let existing = concat!(
            "BEGIN:VTODO\r\nUID:no-percent\r\nSUMMARY:Imported\r\n",
            "STATUS:NEEDS-ACTION\r\nX-UNKNOWN:keep\r\nEND:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        let updated = update_task_component_with_dates(
            existing,
            "Changed only the title",
            &draft.dtstart.input,
            &draft.due.input,
            &draft.completed.input,
            &draft.recurrence,
            &draft.rdates,
            &draft.exdates,
            draft.priority,
            &draft.categories,
            &parse_alarms(existing),
            &draft.description,
            &draft.attachments,
            draft.status,
            draft.percent,
        )
        .expect("unrelated edit must retain an absent lifecycle property");
        assert!(!updated.contains("PERCENT-COMPLETE:"));
        assert!(updated.contains("X-UNKNOWN:keep\r\n"));
    }

    #[test]
    fn task_editor_replaces_imported_lifecycle_only_after_a_lifecycle_edit() {
        let existing = concat!(
            "BEGIN:VTODO\r\nUID:explicit-lifecycle\r\nSUMMARY:Imported\r\n",
            "STATUS:X-IMPORTED\r\nPERCENT-COMPLETE:bogus\r\n",
            "ATTACH;X-SAFE=keep;VALUE=URI:https://example.test/keep\r\n",
            "X-UNKNOWN:keep\r\nBEGIN:VALARM\r\nACTION:EMAIL\r\n",
            "TRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        let updated = update_task_component_with_dates_and_lifecycle_edits(
            existing,
            "Changed lifecycle",
            &draft.dtstart.input,
            &draft.due.input,
            "",
            &draft.recurrence,
            &draft.rdates,
            &draft.exdates,
            draft.priority,
            &draft.categories,
            &parse_alarms(existing),
            &draft.description,
            &draft.attachments,
            "IN-PROCESS",
            50,
            TaskLifecycleEdits {
                status: true,
                ..TaskLifecycleEdits::default()
            },
        )
        .expect("an explicit lifecycle edit should normalize its coupled fields");
        assert!(updated.contains("STATUS:IN-PROCESS\r\n"));
        assert!(updated.contains("PERCENT-COMPLETE:50\r\n"));
        assert!(!updated.contains("COMPLETED:"));
        assert!(updated.contains("ATTACH;X-SAFE=keep;VALUE=URI:https://example.test/keep\r\n"));
        assert!(updated.contains("X-UNKNOWN:keep\r\n"));
        assert!(updated.contains("ACTION:EMAIL\r\n"));
    }

    #[test]
    fn task_editor_attachments_are_lossless_for_noop_and_safe_when_reordered() {
        let existing = concat!(
            "BEGIN:VTODO\r\nUID:attachments\r\nSUMMARY:Attachments\r\n",
            "STATUS:NEEDS-ACTION\r\nPERCENT-COMPLETE:0\r\n",
            "ATTACH;FMTTYPE=application/pdf;X-PARAM=keep;VALUE=URI:https://example.test/a.pdf\r\n",
            "ATTACH;ENCODING=BASE64;FMTTYPE=application/octet-stream;VALUE=BINARY:QUJDRA==\r\n",
            "BEGIN:VALARM\r\nACTION:AUDIO\r\n",
            "ATTACH;VALUE=URI:https://example.test/alarm.wav\r\n",
            "END:VALARM\r\nEND:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        assert!(!draft.attachments_changed);
        assert_eq!(draft.attachment_lines.len(), 2);
        assert!(
            draft.attachment_lines[0]
                .raw
                .contains("X-PARAM=keep;VALUE=URI")
        );
        assert_eq!(
            draft.attachment_lines[1].kind,
            TaskAttachmentValueKind::Binary
        );
        assert!(!draft.attachments.contains("alarm.wav"));

        let unchanged = update_task_component_with_dates(
            existing,
            "Attachments edited elsewhere",
            "",
            "",
            "",
            "",
            "",
            "",
            0,
            "",
            &parse_alarms(existing),
            "",
            &draft.attachments,
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        assert!(unfold_ical_lines(&unchanged).iter().any(|line| {
            line == "ATTACH;FMTTYPE=application/pdf;X-PARAM=keep;VALUE=URI:https://example.test/a.pdf"
        }));
        assert!(unfold_ical_lines(&unchanged).iter().any(|line| {
            line == "ATTACH;ENCODING=BASE64;FMTTYPE=application/octet-stream;VALUE=BINARY:QUJDRA=="
        }));
        assert!(
            unfold_ical_lines(&unchanged)
                .iter()
                .any(|line| { line == "ATTACH;VALUE=URI:https://example.test/alarm.wav" })
        );

        let edited_text = format!(
            "https://example.test/new.pdf\n{}\nATTACH;ENCODING=BASE64;VALUE=BINARY:QkFTRTY0",
            draft.attachment_lines[1].raw
        );
        let edited_draft =
            TaskAttachmentDraft::from_component(existing).with_editor_text(&edited_text);
        assert!(edited_draft.attachments_changed);
        let edited = update_task_component_with_dates(
            existing,
            "Attachments reordered",
            "",
            "",
            "",
            "",
            "",
            "",
            0,
            "",
            &parse_alarms(existing),
            "",
            &edited_draft.editor_text,
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        let top_level = task_top_level_attachment_lines(&edited);
        assert_eq!(top_level.len(), 3);
        assert_eq!(top_level[0].value, "https://example.test/new.pdf");
        assert_eq!(
            top_level[1].raw,
            "ATTACH;ENCODING=BASE64;FMTTYPE=application/octet-stream;VALUE=BINARY:QUJDRA=="
        );
        assert_eq!(
            top_level[2].raw,
            "ATTACH;ENCODING=BASE64;VALUE=BINARY:QkFTRTY0"
        );
        assert!(
            unfold_ical_lines(&edited)
                .iter()
                .any(|line| { line == "ATTACH;VALUE=URI:https://example.test/alarm.wav" })
        );
    }

    #[test]
    fn task_editor_preserves_ordered_alarms_attachment_parameters_and_unknown_properties() {
        let existing = concat!(
            "BEGIN:VTODO\r\nUID:preserve-all\r\n",
            "DTSTART;VALUE=DATE:20260810\r\nDUE;VALUE=DATE:20260811\r\n",
            "STATUS:NEEDS-ACTION\r\nPERCENT-COMPLETE:0\r\n",
            "RRULE:FREQ=DAILY;COUNT=3\r\n",
            "ATTACH;FMTTYPE=application/pdf;X-SAFE=keep;VALUE=URI:https://example.test/a.pdf\r\n",
            "X-UNKNOWN:keep\r\n",
            "BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT5M\r\nX-ALARM:FIRST\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nX-ALARM:SECOND\r\nEND:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let draft = task_editor_draft_from_component(existing);
        let updated = update_task_component_with_dates(
            existing,
            "Changed title",
            &draft.dtstart.input,
            &draft.due.input,
            &draft.completed.input,
            &draft.recurrence,
            &draft.rdates,
            &draft.exdates,
            draft.priority,
            &draft.categories,
            &parse_alarms(existing),
            &draft.description,
            &draft.attachments,
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        assert!(unfold_ical_lines(&updated).iter().any(|line| {
            line == "ATTACH;FMTTYPE=application/pdf;X-SAFE=keep;VALUE=URI:https://example.test/a.pdf"
        }));
        assert!(updated.contains("X-UNKNOWN:keep\r\n"));
        let first = updated.find("X-ALARM:FIRST").unwrap();
        let second = updated.find("X-ALARM:SECOND").unwrap();
        assert!(first < second);
        assert!(updated.contains("RRULE:FREQ=DAILY;COUNT=3\r\n"));
        assert!(updated.contains("UID:preserve-all\r\n"));
    }

    #[test]
    fn task_list_print_snapshot_keeps_visible_order_and_escapes_all_cells() {
        let first = TaskSelectionIdentity {
            source_uid: "source-a".to_string(),
            task_uid: "first".to_string(),
        };
        let second = TaskSelectionIdentity {
            source_uid: "source-b".to_string(),
            task_uid: "second".to_string(),
        };
        let candidates = vec![
            TaskExportCandidate {
                identity: first.clone(),
                component: concat!(
                    "BEGIN:VTODO\r\nUID:first\r\nSUMMARY:First\r\n",
                    "STATUS:NEEDS-ACTION\r\nDUE;VALUE=DATE:20260820\r\n",
                    "PRIORITY:1\r\nCATEGORIES:Work\r\nEND:VTODO\r\n"
                )
                .to_string(),
            },
            TaskExportCandidate {
                identity: second.clone(),
                component: concat!(
                    "BEGIN:VTODO\r\nUID:second\r\nSUMMARY:<Second & urgent>\r\n",
                    "STATUS:COMPLETED\r\nCATEGORIES:Research\r\nEND:VTODO\r\n"
                )
                .to_string(),
            },
        ];
        let document =
            build_task_list_print_document(&[second.clone(), first.clone()], &candidates).unwrap();
        assert_eq!(document.row_count, 2);
        assert!(document.html.contains("<title>Print Tasks</title>"));
        assert!(document.html.contains("&lt;Second &amp; urgent&gt;"));
        assert!(document.html.contains("<td>source-b</td>"));
        assert!(
            document.plain_text.find("<Second & urgent>").unwrap()
                < document.plain_text.find("First").unwrap()
        );
        assert!(document.html.find("source-b").unwrap() < document.html.find("source-a").unwrap());
    }

    #[test]
    fn task_list_print_fails_closed_for_empty_duplicate_and_stale_rows() {
        let identity = TaskSelectionIdentity {
            source_uid: "source-a".to_string(),
            task_uid: "print-id".to_string(),
        };
        let candidate = TaskExportCandidate {
            identity: identity.clone(),
            component: "BEGIN:VTODO\r\nUID:print-id\r\nSUMMARY:Task\r\nEND:VTODO\r\n".to_string(),
        };
        assert!(build_task_list_print_document(&[], std::slice::from_ref(&candidate)).is_err());
        assert!(
            build_task_list_print_document(
                &[identity.clone(), identity.clone()],
                std::slice::from_ref(&candidate)
            )
            .is_err()
        );
        let stale = TaskExportCandidate {
            identity: identity.clone(),
            component: "BEGIN:VTODO\r\nUID:not-print-id\r\nSUMMARY:Task\r\nEND:VTODO\r\n"
                .to_string(),
        };
        assert!(build_task_list_print_document(&[identity], &[stale]).is_err());
    }

    #[test]
    fn task_list_print_and_preview_have_distinct_honest_preparation_states() {
        assert_eq!(
            TaskListPrintAction::Print.preparing_label(),
            "Druckliste wird vorbereitet …"
        );
        assert_eq!(
            TaskListPrintAction::Preview.preparing_label(),
            "Druckvorschau wird vorbereitet …"
        );
        assert_eq!(
            TaskListPrintAction::Print.preparation_label(),
            "Druckdokument vorbereitet"
        );
        assert_eq!(
            TaskListPrintAction::Preview.preparation_label(),
            "Druckvorschau vorbereitet"
        );
    }

    #[test]
    fn task_refresh_requires_one_available_supported_primary_source_and_idle_state() {
        assert!(task_refresh_action_enabled(
            Some("source-a"),
            true,
            false,
            false,
            false
        ));
        assert!(!task_refresh_action_enabled(
            None, true, false, false, false
        ));
        assert!(!task_refresh_action_enabled(
            Some("source-a"),
            false,
            false,
            false,
            false
        ));
        assert!(!task_refresh_action_enabled(
            Some("source-a"),
            true,
            true,
            false,
            false
        ));
        assert!(!task_refresh_action_enabled(
            Some("source-a"),
            true,
            false,
            true,
            false
        ));
        assert!(!task_refresh_action_enabled(
            Some("source-a"),
            true,
            false,
            false,
            true
        ));
    }

    #[test]
    fn task_refresh_result_requires_matching_generation_and_primary_source() {
        assert!(task_refresh_result_is_current(
            7,
            7,
            Some("source-a"),
            "source-a"
        ));
        assert!(!task_refresh_result_is_current(
            8,
            7,
            Some("source-a"),
            "source-a"
        ));
        assert!(!task_refresh_result_is_current(
            7,
            7,
            Some("source-b"),
            "source-a"
        ));
        assert!(!task_refresh_result_is_current(7, 7, None, "source-a"));
    }

    #[test]
    fn exact_ecal_task_source_capabilities_project_writable_readonly_and_unavailable() {
        let mut source = ECalSourceCapability {
            source_uid: "tasks-a".to_string(),
            display_name: "Team Tasks".to_string(),
            connected: true,
            available: true,
            writable: true,
            source_writable: true,
            refresh_supported: true,
            removable: false,
            remote_deletable: false,
            in_collection: false,
            task_assignment_supported: false,
            task_assignment_conversion_supported: false,
        };
        assert_eq!(
            task_source_capability_from_ecal(&source),
            TaskSourceCapability::Writable
        );
        assert_eq!(task_source_capability_label(&source), "Team Tasks");

        source.writable = false;
        assert_eq!(
            task_source_capability_from_ecal(&source),
            TaskSourceCapability::ReadOnly
        );
        assert_eq!(
            task_source_capability_label(&source),
            "Team Tasks (schreibgeschützt)"
        );

        source.connected = false;
        source.available = false;
        assert_eq!(
            task_source_capability_from_ecal(&source),
            TaskSourceCapability::Unavailable
        );
        assert_eq!(
            task_source_capability_label(&source),
            "Team Tasks (nicht verfügbar)"
        );
    }

    #[test]
    fn exact_ecal_task_source_label_falls_back_to_stable_uid() {
        let source = ECalSourceCapability {
            source_uid: "tasks-a".to_string(),
            display_name: "  ".to_string(),
            connected: true,
            available: true,
            writable: true,
            source_writable: true,
            refresh_supported: false,
            removable: false,
            remote_deletable: false,
            in_collection: false,
            task_assignment_supported: false,
            task_assignment_conversion_supported: false,
        };
        assert_eq!(task_source_capability_label(&source), "tasks-a");
    }

    #[test]
    fn task_source_management_actions_match_upstream_source_metadata_gates() {
        let source = ECalSourceCapability {
            source_uid: "tasks-a".to_string(),
            display_name: "Tasks".to_string(),
            connected: true,
            available: true,
            writable: true,
            source_writable: true,
            refresh_supported: false,
            removable: false,
            remote_deletable: false,
            in_collection: false,
            task_assignment_supported: false,
            task_assignment_conversion_supported: false,
        };
        let mut metadata = HashMap::from([("tasks-a".to_string(), source.clone())]);
        assert_eq!(
            task_source_action_state(Some("tasks-a"), &metadata, false),
            TaskSourceActionState {
                copy_enabled: true,
                rename_enabled: true,
                properties_enabled: true,
                delete_enabled: false,
            }
        );

        // Upstream source actions use ESource:writable, independently of the
        // connected calendar client's read-only state used for task writes.
        metadata.get_mut("tasks-a").unwrap().writable = false;
        assert_eq!(
            task_source_action_state(Some("tasks-a"), &metadata, false),
            TaskSourceActionState {
                copy_enabled: true,
                rename_enabled: true,
                properties_enabled: true,
                delete_enabled: false,
            }
        );
        metadata.get_mut("tasks-a").unwrap().source_writable = false;
        assert_eq!(
            task_source_action_state(Some("tasks-a"), &metadata, false),
            TaskSourceActionState {
                copy_enabled: true,
                rename_enabled: false,
                properties_enabled: false,
                delete_enabled: false,
            }
        );
        metadata.get_mut("tasks-a").unwrap().source_writable = true;

        metadata.get_mut("tasks-a").unwrap().in_collection = true;
        metadata.get_mut("tasks-a").unwrap().remote_deletable = true;
        assert_eq!(
            task_source_action_state(Some("tasks-a"), &metadata, false),
            TaskSourceActionState {
                copy_enabled: true,
                rename_enabled: false,
                properties_enabled: true,
                delete_enabled: true,
            }
        );
        assert_eq!(
            task_source_action_state(Some("tasks-a"), &metadata, true),
            TaskSourceActionState {
                copy_enabled: false,
                rename_enabled: false,
                properties_enabled: false,
                delete_enabled: false,
            }
        );
        assert_eq!(
            task_source_action_state(None, &metadata, false),
            TaskSourceActionState {
                copy_enabled: false,
                rename_enabled: false,
                properties_enabled: false,
                delete_enabled: false,
            }
        );
    }

    #[test]
    fn task_assignment_action_requires_exact_writable_supported_source_and_selection() {
        let identity = TaskSelectionIdentity {
            source_uid: "tasks-a".to_string(),
            task_uid: "assign-1".to_string(),
        };
        let candidate = TaskExportCandidate {
            identity: identity.clone(),
            component: "BEGIN:VTODO\r\nUID:assign-1\r\nSUMMARY:Assign\r\nEND:VTODO\r\n".to_string(),
        };
        let source = ECalSourceCapability {
            source_uid: "tasks-a".to_string(),
            display_name: "Tasks".to_string(),
            connected: true,
            available: true,
            source_writable: true,
            writable: true,
            refresh_supported: false,
            removable: false,
            remote_deletable: false,
            in_collection: false,
            task_assignment_supported: true,
            task_assignment_conversion_supported: true,
        };
        let mut metadata = HashMap::from([("tasks-a".to_string(), source)]);
        assert!(task_assignment_action_enabled(
            std::slice::from_ref(&identity),
            std::slice::from_ref(&candidate),
            &metadata,
            false,
        ));
        metadata
            .get_mut("tasks-a")
            .unwrap()
            .task_assignment_conversion_supported = false;
        assert!(!task_assignment_action_enabled(
            std::slice::from_ref(&identity),
            std::slice::from_ref(&candidate),
            &metadata,
            false,
        ));
        metadata
            .get_mut("tasks-a")
            .unwrap()
            .task_assignment_conversion_supported = true;
        metadata.get_mut("tasks-a").unwrap().writable = false;
        assert!(!task_assignment_action_enabled(
            std::slice::from_ref(&identity),
            std::slice::from_ref(&candidate),
            &metadata,
            false,
        ));
        assert!(!task_assignment_action_enabled(
            &[],
            std::slice::from_ref(&candidate),
            &metadata,
            false,
        ));
    }

    #[test]
    fn task_assignment_parser_and_replacement_preserve_unrelated_and_nested_data() {
        let original = concat!(
            "BEGIN:VTODO\r\nUID:assign-1\r\nSUMMARY:Assign\r\n",
            "ORGANIZER;CN=Old:mailto:old@example.org\r\n",
            "ATTENDEE;ROLE=OPT-PARTICIPANT:mailto:old-attendee@example.org\r\n",
            "X-EVOLUTION-KEEP:value\r\nBEGIN:VALARM\r\n",
            "ATTENDEE:mailto:nested@example.org\r\nACTION:DISPLAY\r\nEND:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let unchanged_draft = task_assignment_draft(original);
        let unchanged =
            parse_task_assignment_update(&unchanged_draft.organizer, &unchanged_draft.attendees)
                .unwrap();
        assert_eq!(
            replace_task_assignment_properties(original, &unchanged),
            original
        );

        let update = parse_task_assignment_update(
            "owner@example.org",
            "one@example.org\none@example.org\nATTENDEE;ROLE=CHAIR:mailto:chair@example.org",
        )
        .unwrap();
        let replaced = replace_task_assignment_properties(original, &update);
        assert_eq!(
            task_top_level_property_lines(&replaced, "ORGANIZER"),
            vec!["ORGANIZER:mailto:owner@example.org"]
        );
        assert_eq!(
            task_top_level_property_lines(&replaced, "ATTENDEE").len(),
            2
        );
        assert!(replaced.contains("X-EVOLUTION-KEEP:value"));
        assert!(replaced.contains("ATTENDEE:mailto:nested@example.org"));
        let request = build_task_assignment_request(&replaced);
        assert!(request.contains("METHOD:REQUEST\r\nBEGIN:VTODO"));
        assert!(request.ends_with("END:VCALENDAR\r\n"));
        assert!(parse_task_assignment_update("owner@example.org", "").is_err());
        assert!(parse_task_assignment_update("invalid", "one@example.org").is_err());
    }

    #[test]
    fn task_preview_requires_exact_single_current_task_and_renders_safe_metadata() {
        let identity = TaskSelectionIdentity {
            source_uid: "source-a".to_string(),
            task_uid: "preview-1".to_string(),
        };
        let component = concat!(
            "BEGIN:VTODO\r\nUID:preview-1\r\nSUMMARY:Plan\\, review\r\n",
            "LOCATION:Office\r\nGEO:52.5;13.4\r\nDTSTART;VALUE=DATE:20260820\r\n",
            "RRULE:FREQ=WEEKLY\r\nDUE;VALUE=DATE:20260821\r\n",
            "ESTIMATED-DURATION:PT1H\r\nSTATUS:IN-PROCESS\r\n",
            "CATEGORIES:Work,Research\r\nPRIORITY:3\r\nCLASS:PRIVATE\r\n",
            "ORGANIZER:mailto:owner@example.test\r\nATTENDEE:mailto:team@example.test\r\n",
            "URL:https://example.test/task\r\nDESCRIPTION:<img src=remote> safe text\r\n",
            "X-ALT-DESC:<b>inert</b>\r\n",
            "ATTACH;VALUE=URI:https://example.test/attachment.pdf\r\nEND:VTODO\r\n"
        );
        let candidates = vec![TaskExportCandidate {
            identity: identity.clone(),
            component: component.to_string(),
        }];
        let document = task_preview_document(std::slice::from_ref(&identity), &candidates).unwrap();
        assert_eq!(document.identity, identity);
        assert!(document.text.contains("Zusammenfassung\nPlan, review"));
        assert!(document.text.contains("Wiederholung\nFREQ=WEEKLY"));
        assert!(
            document
                .text
                .contains("Beschreibung\n<img src=remote> safe text")
        );
        assert!(
            document
                .text
                .contains("URI (nicht geöffnet): https://example.test/attachment.pdf")
        );

        let stale = TaskSelectionIdentity {
            source_uid: "source-a".to_string(),
            task_uid: "other".to_string(),
        };
        assert!(task_preview_document(std::slice::from_ref(&stale), &candidates).is_err());
        assert!(task_preview_document(&[identity, stale], &candidates).is_err());
    }

    #[test]
    fn task_preview_find_is_unicode_safe_wraps_and_resets() {
        let text = "Ärger 😀 Ärger";
        assert_eq!(
            task_preview_match_ranges(text, "ärGER"),
            vec![(0, 5), (8, 13)]
        );
        let state = task_preview_find_state(text, "ärger", None);
        assert_eq!(state.current, Some(0));
        assert_eq!(task_preview_find_step(&state, true).current, Some(1));
        assert_eq!(
            task_preview_find_step(&task_preview_find_step(&state, true), true).current,
            Some(0)
        );
        assert_eq!(
            task_preview_find_state("Andere Aufgabe", "ärger", None).current,
            None
        );
        assert!(task_preview_find_state(text, "", None).matches.is_empty());
        assert_eq!(task_preview_match_ranges("İstanbul", "i̇s"), vec![(0, 2)]);
    }

    #[test]
    fn task_preview_layout_settings_follow_upstream_classic_and_vertical_modes() {
        assert_eq!(
            TaskPreviewOrientation::from_setting("classic"),
            TaskPreviewOrientation::Classic
        );
        assert_eq!(
            TaskPreviewOrientation::from_setting("vertical"),
            TaskPreviewOrientation::Vertical
        );
        assert_eq!(
            TaskPreviewOrientation::from_setting("unexpected"),
            TaskPreviewOrientation::Classic
        );
        assert_eq!(TaskPreviewOrientation::Classic.setting_value(), "classic");
        assert_eq!(TaskPreviewOrientation::Vertical.setting_value(), "vertical");
        assert_eq!(TaskPreviewOrientation::Classic.index(), 0);
        assert_eq!(TaskPreviewOrientation::Vertical.index(), 1);
    }

    #[test]
    fn local_task_create_edit_and_reload_roundtrip_is_authoritative() {
        let directory = std::env::temp_dir().join(format!(
            "evolution-gtk4-task-editor-{}",
            uuid::Uuid::new_v4()
        ));
        let service = CalendarStoreService::with_writable_calendar_dir(&directory);
        let created_component = build_task_component_with_dates(
            "Local task",
            "2026-08-10",
            "2026-08-11",
            "",
            "FREQ=DAILY;COUNT=2",
            "",
            "",
            1,
            "Work",
            &[],
            "Description",
            "",
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        let (uid, _) = service.create_component(&created_component).unwrap();
        let snapshot = service.load_snapshot();
        let record = snapshot
            .tasks
            .iter()
            .find(|task| task.uid == uid)
            .expect("created task should reload");
        let updated_component = update_task_component_with_dates(
            &record.component,
            "Edited local task",
            "2026-08-10",
            "2026-08-12",
            "",
            &ical_property(&record.component, "RRULE").unwrap_or_default(),
            "",
            "",
            2,
            "Work,Home",
            &[],
            "Edited description",
            "",
            "NEEDS-ACTION",
            0,
        )
        .unwrap();
        service.modify_component(&uid, &updated_component).unwrap();
        let reloaded = service.load_snapshot();
        let edited = reloaded
            .tasks
            .iter()
            .find(|task| task.uid == uid)
            .expect("edited task should reload");
        assert_eq!(edited.summary, "Edited local task");
        assert_eq!(edited.due, "2026-08-12");
        assert!(edited.component.contains("DTSTART;VALUE=DATE:20260810\r\n"));
        assert!(edited.component.contains("RRULE:FREQ=DAILY;COUNT=2\r\n"));
        assert!(edited.component.contains("CATEGORIES:Work,Home\r\n"));
        let _ = std::fs::remove_dir_all(&directory);
    }
}

impl Default for TasksPage {
    fn default() -> Self {
        Self::new()
    }
}
