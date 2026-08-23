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
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::dialogs::event_dialog::AlarmEditor;
use crate::models::task_item::TaskItemObject;
use crate::services::calendar_category_service::{
    CalendarCategory, CalendarCategoryStore, category_names_from_ical_component,
};
use crate::services::calendar_store_service::{
    CalendarStoreService, TaskRecord, export_ical_component, serialize_ical_attachment_value,
    update_ical_property,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalMutationError, ECalMutationResult,
    ECalRecord, ECalService, ECalSyncEvent, load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
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
                (display_date.format("%Y-%m-%d").to_string(), Some(kind), Some(display_date))
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
        status: draft.unsupported_status.is_none()
            && !draft.status.eq_ignore_ascii_case(status),
        percent: draft.unsupported_percent.is_none()
            && !draft
                .percent_raw
                .as_deref()
                .and_then(parse_task_percent_value)
                .is_some_and(|value| value == percent),
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

fn task_initial_editor_lifecycle(
    draft: &TaskEditorDraft,
    today: NaiveDate,
) -> (u8, String) {
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
            let name = parameter.split_once('=').map_or(*parameter, |(name, _)| name);
            !name.eq_ignore_ascii_case("VALUE")
                && !(kind == TaskDateValueKind::DateOnly
                    && name.eq_ignore_ascii_case("TZID"))
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
    let Some(pair_kind) = task_date_pair_kind(dtstart, due, dtstart_mutation, due_mutation)
    else {
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
                if !task_source_is_writable_now(&source_uid) {
                    self.show_toast(
                        "Diese Aufgabenliste ist nicht als schreibfähig bestätigt",
                    );
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
            .margin_top(4)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&header);

        let task_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        sidebar_box.append(&task_source_list);

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
            .width_request(240)
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
            .width_request(240)
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
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_selector =
            gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        source_selector.set_tooltip_text(Some("Aufgabenliste auswählen"));
        let source_selector_for_sidebar = source_selector.clone();
        let split_for_source_sidebar = split.clone();
        task_source_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                source_selector_for_sidebar.set_selected(row.index() as u32);
                split_for_source_sidebar.set_show_content(true);
            }
        });
        let initial_source_filter = self.imp().task_source_filter.borrow().clone();
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

        toolbar.append(&search_mode_selector);
        toolbar.append(&search_entry);
        toolbar.append(&sort_selector);
        toolbar.append(&direction_button);
        toolbar.append(&source_selector);
        toolbar.append(&category_selector);
        toolbar.append(&btn_add_task);

        content_box.append(&toolbar);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let task_store = gio::ListStore::new::<TaskItemObject>();

        let task_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
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
        content_box.append(&empty_tasks);

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
            if !task_source_is_writable_now(&source_uid) {
                page.show_toast(
                    "Diese Aufgabenliste ist nicht als schreibfähig bestätigt",
                );
                return;
            }
            page.open_task_dialog(
                task_store_for_activation.clone(),
                task_list_for_activation.clone(),
                empty_tasks_for_activation.clone(),
                Some(task),
            );
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_selector_for_load = source_selector.clone();
        let task_source_list_for_load = task_source_list.clone();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
            let mut sources = discovery.sources;
            sources.retain(|source| source.enabled);
            sources.sort_by_cached_key(|source| {
                (source.display_name.to_lowercase(), source.uid.clone())
            });

            let mut names = vec!["Alle Aufgabenlisten".to_string()];
            let mut uids = Vec::new();
            for source in sources {
                names.push(source.display_name);
                uids.push(source.uid);
            }
            if let Some(initial_source_filter) = initial_source_filter.as_deref()
                && !uids.iter().any(|uid| uid == initial_source_filter)
            {
                names.push(format!("Nicht verfügbare Quelle · {initial_source_filter}"));
                uids.push(initial_source_filter.to_string());
            }
            if CalendarStoreService::new().is_writable() {
                names.push("Lokale Aufgabenliste".to_string());
                uids.push("local-calendar".to_string());
            }

            let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            source_uids_for_load.replace(uids.clone());
            while let Some(child) = task_source_list_for_load.first_child() {
                task_source_list_for_load.remove(&child);
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
                task_source_list_for_load.append(&row);
            }
            let selected = initial_source_filter
                .as_deref()
                .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
                .map(|index| index as u32 + 1)
                .unwrap_or(0);
            source_selector_for_load.set_selected(selected);
            if let Some(row) = task_source_list_for_load.row_at_index(selected as i32) {
                task_source_list_for_load.select_row(Some(&row));
            }
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
        let source_uids_for_selector = Rc::clone(&source_uids);
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
                source_uids_for_selector
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
            let source_uids_for_settings = Rc::clone(&source_uids);
            settings.connect_changed(Some("task-source-filter"), move |settings, _| {
                let value = settings.string("task-source-filter");
                let selected = if value.trim().is_empty() {
                    Some(0)
                } else {
                    source_uids_for_settings
                        .borrow()
                        .iter()
                        .position(|uid| uid == value.as_str())
                        .map(|index| index as u32 + 1)
                };
                let Some(page) = page_weak.upgrade() else {
                    return;
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
                        let Some(page) = page_weak_for_load.upgrade() else {
                            return;
                        };
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

        content_box.append(&task_scroll);

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
            let writable = task_source_is_writable(
                &source_uid,
                &source_capabilities_for_selection,
            );
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
        if initial_draft.unsupported_status.is_some() || initial_draft.unsupported_percent.is_some() {
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
            status_row.set_tooltip_text(Some("Importierter, nicht unterstützter STATUS-Wert; unverändert bewahrt"));
            percent_row.set_tooltip_text(Some("Importierter, nicht unterstützter PERCENT-COMPLETE-Wert; unverändert bewahrt"));
        }
        let percent_adjustment = percent_row.adjustment();

        for entry in [
            title_entry.clone(),
            dtstart_entry.clone(),
            due_entry.clone(),
            completed_entry.clone(),
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
        status_row.connect_selected_notify(move |_| {
            dirty_for_status.set(true);
            dialog_state_for_status.set(TaskEditorDialogState::Dirty);
        });
        let dirty_for_percent = Rc::clone(&dirty);
        let dialog_state_for_percent = Rc::clone(&dialog_state);
        percent_adjustment.connect_value_changed(move |_| {
            dirty_for_percent.set(true);
            dialog_state_for_percent.set(TaskEditorDialogState::Dirty);
        });
        let dirty_for_description = Rc::clone(&dirty);
        let dialog_state_for_description = Rc::clone(&dialog_state);
        description_view
            .buffer()
            .connect_changed(move |_| {
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
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
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
            if let Some(error) = discovery.error.as_deref() {
                tracing::warn!(%error, "Could not reach EDS task lists for task dialog");
            }
            let active_sources: Vec<_> = discovery
                .sources
                .into_iter()
                .filter(|source| source.enabled)
                .collect();
            let source_capability = if matches!(
                discovery.origin,
                eds_service::EdsSourceOrigin::Live
            ) {
                // EdsSource currently carries no can-write capability.  A
                // live registry entry is therefore known to exist, but its
                // mutation capability is unknown and must fail closed.
                TaskSourceCapability::Unknown
            } else {
                TaskSourceCapability::Unavailable
            };
            let mut names = Vec::new();
            let mut capabilities = HashMap::new();
            let mut uids = source_uids_for_load.borrow_mut();
            uids.clear();
            for source in active_sources {
                let uid = source.uid;
                let name = match source_capability {
                    TaskSourceCapability::Unknown => {
                        format!("{} (Schreibfähigkeit unbekannt)", source.display_name)
                    }
                    TaskSourceCapability::ReadOnly => {
                        format!("{} (schreibgeschützt)", source.display_name)
                    }
                    TaskSourceCapability::Unavailable => {
                        format!("{} (nicht verfügbar)", source.display_name)
                    }
                    TaskSourceCapability::Writable => source.display_name,
                };
                names.push(name);
                capabilities.insert(uid.clone(), source_capability);
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
                names.push(if discovery.error.is_some() {
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
            let writable = selected_uid.as_deref().is_some_and(|uid| {
                task_source_is_writable(uid, &source_capabilities_for_load)
            });
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
                Some(task) => update_task_component_with_dates(
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
        append_task_row(
            page,
            task_list,
            task_store,
            empty_tasks,
            &task,
            context,
            &category_metadata,
        );
        visible += 1;
    }
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
        if !task_source_is_writable_now(&source_uid) {
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
        if !task_source_is_writable_now(&source_uid) {
            updating_for_callback.set(true);
            button.set_active(!completed);
            updating_for_callback.set(false);
            show_toast_for_widget(
                button,
                "Diese Aufgabenliste ist nicht mehr als schreibfähig bestätigt",
            );
            return;
        }
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
                    let component = match task_authoritative_component(
                        &uid,
                        &result.uid,
                        result.component,
                    ) {
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
        .sensitive(task_source_is_writable_now(&task.source_uid()))
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
        if !task_source_is_writable_now(&task_for_edit.source_uid()) {
            show_toast_for_widget(
                &edit_button_for_closure,
                "Diese Aufgabenliste ist nicht als schreibfähig bestätigt",
            );
            return;
        }
        let Some(page) = page_weak_for_edit.upgrade() else {
            return;
        };
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
    let task_for_delete = task.clone();
    let task_store_for_delete = task_store.clone();
    let task_list_for_delete = task_list.clone();
    let empty_tasks_for_delete = empty_tasks.clone();
    let delete_button_for_context = delete_button.clone();
    let delete_button_for_signal = delete_button.clone();
    let page_weak_for_delete = page.downgrade();
    delete_button_for_signal.connect_clicked(move |_| {
        let source_uid = task_for_delete.source_uid();
        if !task_source_is_writable_now(&source_uid) {
            show_toast_for_widget(
                &delete_button,
                "Diese Aufgabenliste ist nicht als schreibfähig bestätigt",
            );
            return;
        }
        let confirmation_message = if source_uid == "local-calendar" {
            "Die Aufgabe wird dauerhaft aus der lokalen Aufgabenliste entfernt."
        } else {
            "Die Aufgabe wird dauerhaft aus der synchronisierten Aufgabenliste entfernt."
        };
        let task_for_action = task_for_delete.clone();
        let task_store_for_action = task_store_for_delete.clone();
        let task_list_for_action = task_list_for_delete.clone();
        let empty_tasks_for_action = empty_tasks_for_delete.clone();
        let delete_button_for_action = delete_button.clone();
        let page_weak_for_action = page_weak_for_delete.clone();
        crate::dialogs::run_destructive_action(
            &delete_button,
            "Aufgabe löschen?",
            confirmation_message,
            "Löschen",
            move || {
                let source_uid = task_for_action.source_uid();
                if !task_source_is_writable_now(&source_uid) {
                    show_toast_for_widget(
                        &delete_button_for_action,
                        "Diese Aufgabenliste ist nicht mehr als schreibfähig bestätigt",
                    );
                    return;
                }
                let source_uid_for_result = source_uid.clone();
                let uid = task_for_action.id();
                let (sender, receiver) = async_channel::bounded(1);
                delete_button_for_action.set_sensitive(false);
                std::thread::spawn(move || {
                    let result = ECalService::new().remove_component(
                        &source_uid,
                        ECalComponentType::Task,
                        &uid,
                    );
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Task deletion result was dropped");
                    }
                });

                let task_store_for_result = task_store_for_action.clone();
                let task_list_for_result = task_list_for_action.clone();
                let empty_tasks_for_result = empty_tasks_for_action.clone();
                let delete_button_for_result = delete_button_for_action.clone();
                let page_weak_for_result = page_weak_for_action.clone();
                glib::spawn_future_local(async move {
                    match receiver.recv().await {
                        Ok(Ok(result)) => {
                            let uid = result.uid;
                            let mut store_index = None;
                            for index in 0..task_store_for_result.n_items() {
                                if task_store_for_result
                                    .item(index)
                                    .and_downcast::<TaskItemObject>()
                                    .is_some_and(|task| {
                                        task.id() == uid
                                            && task.source_uid() == source_uid_for_result
                                    })
                                {
                                    store_index = Some(index);
                                    break;
                                }
                            }
                            if let Some(index) = store_index {
                                task_store_for_result.remove(index);
                            }
                            if let Some(page) = page_weak_for_result.upgrade() {
                                render_task_rows(
                                    &page,
                                    &task_list_for_result,
                                    &task_store_for_result,
                                    &empty_tasks_for_result,
                                );
                            } else if let Some(row) = delete_button_for_result
                                .ancestor(gtk::ListBoxRow::static_type())
                                .and_downcast::<gtk::ListBoxRow>()
                            {
                                task_list_for_result.remove(&row);
                                empty_tasks_for_result
                                    .set_visible(task_store_for_result.n_items() == 0);
                            }
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Aufgabe gelöscht ({uid})"),
                            );
                        }
                        Ok(Err(error)) => {
                            delete_button_for_result.set_sensitive(true);
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Aufgabe konnte nicht gelöscht werden: {error}"),
                            );
                        }
                        Err(error) => {
                            delete_button_for_result.set_sensitive(true);
                            tracing::debug!(%error, "Task deletion channel closed");
                        }
                    }
                });
            },
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

fn task_source_is_writable_now(source_uid: &str) -> bool {
    if source_uid == "local-calendar" {
        CalendarStoreService::new().is_writable()
    } else {
        // Row actions do not have a fresh EDS capability snapshot.  Unknown
        // external capabilities fail closed instead of being guessed from a
        // live source listing.
        false
    }
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
    Finished(Result<crate::services::ecal_service::ECalMutationResult, crate::services::ecal_service::ECalMutationError>),
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
        (State::SavePending, Action::DispatchStarted) => {
            (State::Committing, Effect::DisableClose)
        }
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
        if !task_source_is_writable_now(&source_uid) {
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
        let page_weak_for_result = page_weak.clone();
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
                        page.show_toast("Aufgabe wurde bestätigt, ist aber nicht mehr in dieser Liste geladen");
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
    if (task_date_mutation_changed(dtstart_mutation)
        || task_date_mutation_changed(due_mutation))
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
    let lifecycle_edits =
        task_lifecycle_edits_inferred(&draft, status, percent, completed);
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
    if (task_date_mutation_changed(dtstart_mutation)
        || task_date_mutation_changed(due_mutation))
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
    let status_is_unchanged = status.eq_ignore_ascii_case(draft.status);
    let percent_is_unchanged = if draft.unsupported_percent.is_some() {
        percent == draft.percent
    } else {
        draft
            .percent_raw
            .as_deref()
            .and_then(parse_task_percent_value)
            .is_some_and(|value| value == percent)
    };
    let preserve_unsupported_status = draft.unsupported_status.is_some() && status_is_unchanged;
    let preserve_unsupported_percent = draft.unsupported_percent.is_some() && percent_is_unchanged;
    let preserve_unsupported_lifecycle = preserve_unsupported_status
        && preserve_unsupported_percent
        && completed.trim() == draft.completed.input.trim();
    let lifecycle = if preserve_unsupported_lifecycle {
        None
    } else {
        Some(task_lifecycle_for_date_with_presence(
            status,
            percent,
            completed_date,
            completion_present,
            Local::now().date_naive(),
        )?)
    };
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
    let attachment_draft = TaskAttachmentDraft::from_component(existing).with_editor_text(attachments);
    let parsed_attachments = attachment_draft.attachments_changed
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
        if !preserve_unsupported_status {
            let status_unchanged = draft
                .status_raw
                .as_deref()
                .is_some_and(|raw| raw.trim().eq_ignore_ascii_case(lifecycle.status));
            if !status_unchanged {
                component = update_ical_property(&component, "STATUS", Some(lifecycle.status));
            }
        }
        if !preserve_unsupported_percent {
            let percent_text = lifecycle.percent.to_string();
            if !draft
                .percent_raw
                .as_deref()
                .and_then(parse_task_percent_value)
                .is_some_and(|value| value == lifecycle.percent)
            {
                component =
                    update_ical_property(&component, "PERCENT-COMPLETE", Some(&percent_text));
            }
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
            name.trim()
                .eq_ignore_ascii_case("VALUE")
                .then(|| match value.trim().trim_matches('"').to_ascii_uppercase().as_str() {
                    "URI" => TaskAttachmentValueKind::Uri,
                    "BINARY" => TaskAttachmentValueKind::Binary,
                    _ => TaskAttachmentValueKind::Other,
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
            && line
                .split_once(':')
                .is_some_and(|(property, _)| {
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
            (
                "live-source".to_string(),
                TaskSourceCapability::Unknown,
            ),
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
        assert_eq!(
            cancelled.phase(),
            TaskSavePhase::CancelledBeforeDispatch
        );
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
            task_authoritative_component(
                "confirmed",
                "confirmed",
                Some(confirmed.to_string())
            )
            .unwrap(),
            confirmed
        );
        assert!(task_authoritative_component("confirmed", "confirmed", None).is_err());
        assert!(task_authoritative_component(
            "confirmed",
            "other",
            Some(confirmed.to_string())
        )
        .is_err());
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
        assert_eq!(task_initial_editor_lifecycle(&draft, NaiveDate::from_ymd_opt(2026, 8, 23).unwrap()), (0, "".to_string()));
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
        assert!(draft.attachment_lines[0]
            .raw
            .contains("X-PARAM=keep;VALUE=URI"));
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
        assert!(unfold_ical_lines(&unchanged).iter().any(|line| {
            line == "ATTACH;VALUE=URI:https://example.test/alarm.wav"
        }));

        let edited_text = format!(
            "https://example.test/new.pdf\n{}\nATTACH;ENCODING=BASE64;VALUE=BINARY:QkFTRTY0",
            draft.attachment_lines[1].raw
        );
        let edited_draft = TaskAttachmentDraft::from_component(existing)
            .with_editor_text(&edited_text);
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
        assert!(unfold_ical_lines(&edited).iter().any(|line| {
            line == "ATTACH;VALUE=URI:https://example.test/alarm.wav"
        }));
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
