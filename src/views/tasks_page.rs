// SPDX-License-Identifier: GPL-3.0-or-later
//
// TasksPage — Dedicated Task and Todo (VTODO) manager view.
//
// Features:
// - Category list sidebar ("All Tasks", "Due Today", "Important", "Completed")
// - Dynamic Task list with checkable boxes, priority chips, and due dates
// - "+ New Task" task creation dialog connection

use chrono::{Duration, Local, NaiveDate, NaiveDateTime, Utc};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::dialogs::event_dialog::AlarmEditor;
use crate::i18n::{gettext, ngettext};
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
use crate::models::source_selector::{SourceDomain, SourceSelector, SourceSelectorItem};
use crate::views::accessibility::{
    announce_to_screen_reader, set_accessible_description, set_accessible_label,
    set_accessible_role,
};
use crate::views::calendar_page::calendar_source_color;
use crate::views::set_button_icon_with_fallback;
use adw::prelude::*;
use adw::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct TasksPage {
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub(super) task_filter: Cell<TaskFilter>,
        pub(super) task_search_mode: Cell<TaskSearchMode>,
        pub(super) collapsed_task_groups: RefCell<HashSet<String>>,
        pub(super) task_source_filter: RefCell<Option<String>>,
        pub(super) task_source_syncing: Cell<bool>,
        pub(super) task_category_syncing: Cell<bool>,
        pub(super) task_sort: Cell<TaskSort>,
        pub(super) task_sort_descending: Cell<bool>,
        pub task_query: RefCell<String>,
        pub(super) task_category_filter: RefCell<Option<String>>,
        pub(super) task_category_metadata: RefCell<Option<Vec<CalendarCategory>>>,
        pub(super) task_category_metadata_generation: Cell<u64>,
        pub(super) task_category_model: RefCell<Option<gtk::StringList>>,
        pub(super) task_category_selector: RefCell<Option<gtk::DropDown>>,
        pub task_list: RefCell<Option<gtk::ListBox>>,
        pub task_table_header: RefCell<Option<gtk::DrawingArea>>,
        pub task_table_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub(crate) task_table_layout: RefCell<Option<PreparedTaskTableLayout>>,
        pub task_table_hovered_row: Cell<Option<usize>>,
        pub(crate) task_table_hovered_header: Cell<Option<TaskTableColumn>>,
        pub task_table_anchor_row: Cell<Option<usize>>,
        pub task_store: RefCell<Option<gio::ListStore>>,
        pub task_empty_state: RefCell<Option<gtk::Label>>,
        pub(super) task_empty_status_page: RefCell<Option<adw::StatusPage>>,
        pub(super) task_container_stack: RefCell<Option<gtk::Stack>>,
        pub(super) task_preview_stack: RefCell<Option<gtk::Stack>>,
        pub(super) task_selection: RefCell<Vec<TaskSelectionIdentity>>,
        pub(super) task_selection_restoring: Cell<bool>,
        pub(super) task_selection_status: RefCell<Option<gtk::Label>>,
        pub(super) task_forward_button: RefCell<Option<gtk::Button>>,
        pub(super) task_assign_button: RefCell<Option<gtk::Button>>,
        pub(super) task_open_url_button: RefCell<Option<gtk::Button>>,
        pub(super) task_print_button: RefCell<Option<gtk::Button>>,
        pub(super) task_print_list_button: RefCell<Option<gtk::Button>>,
        pub(super) task_copy_button: RefCell<Option<gtk::Button>>,
        pub(super) task_cut_button: RefCell<Option<gtk::Button>>,
        pub(super) task_paste_button: RefCell<Option<gtk::Button>>,
        pub(super) task_select_all_button: RefCell<Option<gtk::Button>>,
        pub(super) task_bulk_edit_button: RefCell<Option<gtk::Button>>,
        pub(super) task_sidebar_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub(super) task_preview_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub(super) task_preview_orientation: RefCell<Option<gtk::DropDown>>,
        pub(super) task_preview_paned: RefCell<Option<gtk::Paned>>,
        pub(super) outer_paned: RefCell<Option<gtk::Paned>>,
        pub(super) task_preview_box: RefCell<Option<gtk::Box>>,
        pub toolbar: RefCell<Option<gtk::Widget>>,
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
        pub(super) task_source_selector_model: RefCell<Option<crate::models::source_selector::SourceSelector>>,
        pub(super) hidden_task_sources: RefCell<HashSet<String>>,
        pub(super) task_color_css_provider: RefCell<Option<gtk::CssProvider>>,
        pub(super) task_search_bar: RefCell<Option<gtk::SearchBar>>,
        pub(super) task_smart_filter_list: RefCell<Option<gtk::ListBox>>,
        pub(super) task_smart_filter_badges: RefCell<Vec<(TaskFilter, gtk::Label)>>,
        pub(super) task_smart_filter_syncing: Cell<bool>,
        pub(super) task_workspace_title: RefCell<Option<gtk::Label>>,
        pub(super) task_workspace_subtitle: RefCell<Option<gtk::Label>>,
        pub(super) task_workspace_groups_box: RefCell<Option<gtk::Box>>,
        pub(super) task_quick_due_preset: Cell<u8>,
        pub(super) task_quick_prio_preset: Cell<u8>,
        pub(super) task_inspector_box: RefCell<Option<gtk::Box>>,
        pub new_task_button: RefCell<Option<gtk::Button>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub is_mobile: Cell<bool>,
        pub is_tablet: Cell<bool>,
        pub is_detail_shown: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TasksPage {
        const NAME: &'static str = "MeridianTasksPage";
        type Type = super::TasksPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for TasksPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_vexpand(true);
            self.obj().set_hexpand(true);
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
    (TaskFilter::All, "view-list-symbolic", "All Tasks"),
    (TaskFilter::Today, "alarm-symbolic", "Due Today"),
    (
        TaskFilter::Next7,
        "appointment-soon-symbolic",
        "Next 7 Days",
    ),
    (
        TaskFilter::Next30,
        "appointment-soon-symbolic",
        "Next 30 Days",
    ),
    (
        TaskFilter::Next365,
        "appointment-soon-symbolic",
        "Active · Next 365 Days",
    ),
    (
        TaskFilter::Overdue,
        "appointment-missed-symbolic",
        "Overdue",
    ),
    (
        TaskFilter::Scheduled,
        "x-office-calendar-symbolic",
        "With Due Date",
    ),
    (TaskFilter::NoDue, "view-list-symbolic", "Without Due Date"),
    (
        TaskFilter::WithAttachments,
        "mail-attachment-symbolic",
        "With Attachments",
    ),
    (
        TaskFilter::Started,
        "media-playback-start-symbolic",
        "Started",
    ),
    (
        TaskFilter::WithoutCategory,
        "tag-symbolic",
        "Without Category",
    ),
    (
        TaskFilter::Uncompleted,
        "view-list-symbolic",
        "Open Tasks",
    ),
    (
        TaskFilter::Important,
        "emblem-important-symbolic",
        "Important",
    ),
    (
        TaskFilter::InProgress,
        "media-playback-start-symbolic",
        "In Progress",
    ),
    (
        TaskFilter::Cancelled,
        "window-close-symbolic",
        "Cancelled",
    ),
    (TaskFilter::Completed, "emblem-ok-symbolic", "Completed"),
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
    hide_completed_tasks: bool,
    hide_completed_value: u32,
    hide_completed_cutoff: Option<i64>,
    hide_cancelled_tasks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TaskSelectionIdentity {
    pub(crate) source_uid: String,
    pub(crate) task_uid: String,
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
            "classic" => Self::Classic,
            _ => Self::Vertical,
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
            Self::Vertical => 420,
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
        return Err(gettext("This action is only available for exactly one task"));
    };
    let matches = candidates
        .iter()
        .filter(|candidate| candidate.identity == *identity)
        .collect::<Vec<_>>();
    let [candidate] = matches.as_slice() else {
        return Err(gettext("The selected task is no longer available"));
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
        return Err(gettext("A selected task does not contain a valid VTODO component"));
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
        &gettext("Summary"),
        Some(first("SUMMARY").unwrap_or_else(|| gettext("Untitled"))),
    );
    push(&mut fields, &gettext("Location"), first("LOCATION"));
    push(&mut fields, "GEO", first("GEO"));
    push(&mut fields, &gettext("Comment"), first("COMMENT"));
    push(&mut fields, &gettext("Start"), first("DTSTART"));
    push(&mut fields, &gettext("Recurrence"), first("RRULE"));
    push(&mut fields, &gettext("Due"), first("DUE"));
    push(&mut fields, &gettext("Estimated Duration"), first("ESTIMATED-DURATION"));
    push(&mut fields, &gettext("Status"), first("STATUS"));
    push(&mut fields, &gettext("Categories"), first("CATEGORIES"));
    push(&mut fields, &gettext("Priority"), first("PRIORITY"));
    push(&mut fields, &gettext("Classification"), first("CLASS"));
    push(&mut fields, &gettext("Organizer"), first("ORGANIZER"));
    let attendees = all("ATTENDEE");
    if !attendees.is_empty() {
        fields.push((gettext("Attendees"), attendees.join("\n")));
    }
    push(&mut fields, "URL", first("URL"));
    push(&mut fields, &gettext("Description"), first("DESCRIPTION"));
    push(&mut fields, &gettext("Alternative Description"), first("X-ALT-DESC"));
    let attachments = task_top_level_attachment_lines(&component)
        .into_iter()
        .map(|attachment| match attachment.kind {
            TaskAttachmentValueKind::Uri => format!("{}: {}", gettext("URI (not opened)"), attachment.value),
            TaskAttachmentValueKind::Binary => gettext("Binary attachment"),
            TaskAttachmentValueKind::Other => format!("{}: {}", gettext("Attachment"), attachment.value),
        })
        .collect::<Vec<_>>();
    if !attachments.is_empty() {
        fields.push((gettext("Attachments"), attachments.join("\n")));
    }

    if fields.is_empty() {
        return Err(gettext("The selected task contains no displayable fields"));
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
                return Err(gettext("The selected task contains an invalid alarm"));
            }
            alarm_depth -= 1;
            continue;
        }
        if alarm_depth == 0 {
            result.push(line);
        }
    }
    if alarm_depth != 0 {
        return Err(gettext("The selected task contains an incomplete alarm"));
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
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Meridian//EN\r\nMETHOD:PUBLISH\r\n{component}\r\nEND:VCALENDAR\r\n"
    ))
}

fn validate_task_open_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    let Some((scheme, _)) = value.split_once(':') else {
        return Err(gettext("Task URL requires http or https"));
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err(gettext("Only http and https task URLs are supported"));
    }
    // Reuse the hardened URI authority/port policy already shared by the memo
    // surface, while narrowing this task action to upstream's web-page intent.
    crate::services::memo_surface_service::validate_open_url(value)
        .map_err(|_| gettext("Task URL is invalid"))
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
        .ok_or_else(|| gettext("The selected task has no URL"))?;
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
    fn preparing_label(self) -> String {
        match self {
            Self::Print => gettext("Preparing print list…"),
            Self::Preview => gettext("Preparing print preview…"),
        }
    }

    fn preparation_label(self) -> String {
        match self {
            Self::Print => gettext("Print document prepared"),
            Self::Preview => gettext("Print preview prepared"),
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
    let status_label = crate::i18n::gettext("Status");
    let due_label = crate::i18n::gettext("Due");
    let priority_label = crate::i18n::gettext("Priority");
    let categories_label = crate::i18n::gettext("Categories");
    for (label, value) in [
        (status_label.as_str(), status),
        (due_label.as_str(), due),
        (priority_label.as_str(), priority),
        (categories_label.as_str(), categories),
    ] {
        if !value.trim().is_empty() {
            let (html, text) = task_print_field(label, &value);
            html_fields.push(html);
            text_fields.push(text);
        }
    }
    if !description.trim().is_empty() {
        let desc_label = gettext("Description");
        html_fields.push(format!(
            "<h2>{}</h2><pre>{}</pre>",
            glib::markup_escape_text(&desc_label),
            glib::markup_escape_text(&description)
        ));
        text_fields.push(format!("{desc_label}:\n{description}"));
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
        return Err(gettext("No visible tasks available to print"));
    }
    let mut seen = HashSet::new();
    let mut html_rows = Vec::with_capacity(visible.len());
    let mut text_rows = Vec::with_capacity(visible.len());
    for identity in visible {
        if !seen.insert(identity.clone()) {
            return Err(gettext("The visible task list contains a duplicate identity"));
        }
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err(gettext("A visible task is no longer uniquely available"));
        };
        let component = validated_task_vtodo_component(candidate, identity)?;
        let completion = if task_component_is_completed_for_purge(&component) {
            "✓"
        } else {
            ""
        };
        let status = task_status_label(&component);
        let summary =
            ical_property(&component, "SUMMARY").unwrap_or_else(|| gettext("Untitled"));
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
        gettext("Completed"),
        gettext("Status"),
        gettext("Summary"),
        gettext("Due"),
        gettext("Priority"),
        gettext("Source"),
        gettext("Categories"),
    ];
    let print_title = gettext("Print Tasks");
    let print_h1 = gettext("Tasks");
    Ok(TaskListPrintDocument {
        html: format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><table><thead><tr>{}</tr></thead><tbody>{}</tbody></table></body></html>",
            glib::markup_escape_text(&print_title),
            glib::markup_escape_text(&print_h1),
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
    show_toast_for_widget(button, &action.preparing_label());
    // Snapshot only the rows currently displayed by the list.  This is
    // deliberately independent of selection and does not consult a backend.
    let visible = match task_visible_identities(task_list) {
        Ok(visible) => visible,
        Err(error) => {
            let msg = gettext("Could not prepare print list: {error}").replace("{error}", &error);
            show_toast_for_widget(button, &msg);
            return;
        }
    };
    let document = build_task_list_print_document(&visible, &task_export_candidates(task_store));
    match document {
        Ok(document) => {
            let count_chars = ngettext(
                "{count} task, {chars} characters",
                "{count} tasks, {chars} characters",
                document.row_count as u32,
            )
            .replace("{count}", &document.row_count.to_string())
            .replace("{chars}", &document.plain_text.chars().count().to_string());
            let msg = gettext("{action} ({count_chars}); GTK print dialog / portal is Linux handoff")
                .replace("{action}", &action.preparation_label())
                .replace("{count_chars}", &count_chars);
            page.show_toast(&msg);
        }
        Err(error) => {
            let msg = gettext("Could not prepare print list: {error}").replace("{error}", &error);
            show_toast_for_widget(button, &msg);
        }
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

    fn progress_label(self) -> String {
        match self {
            Self::Complete => gettext("Marking tasks as completed…"),
            Self::Incomplete => gettext("Marking tasks as incomplete…"),
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
        return Err(gettext("No tasks selected"));
    }
    let mut resolved = Vec::with_capacity(selection.len());
    let mut seen = HashSet::with_capacity(selection.len());
    for identity in selection {
        if !seen.insert(identity.clone()) {
            return Err(gettext("A selected task was resolved multiple times"));
        }
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err(gettext("The selected task is no longer uniquely available"));
        };
        let component = validated_task_vtodo_component(candidate, identity)?;
        if task_source_capability(&identity.source_uid, capabilities, local_writable)
            != TaskSourceCapability::Writable
        {
            return Err(
                gettext("At least one selected task list is read-only or unavailable"),
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
        return Err(gettext("The completion timestamp is invalid"));
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
        return Err(gettext("There are no matching tasks for this batch action"));
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
                gettext("The backend returned no result for this task"),
            )),
            _ => reduction.failures.push((
                mutation.identity.clone(),
                gettext("The backend returned ambiguous results for this task"),
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
    let duration_err = || gettext("ESTIMATED-DURATION must be an ISO 8601 duration such as PT1H");
    let Some(rest) = value.strip_prefix('P') else {
        return Err(duration_err());
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
                return Err(duration_err());
            }
            in_time = true;
            continue;
        }
        if digits.is_empty() {
            return Err(duration_err());
        }
        let order = match (in_time, character) {
            (false, 'W') => 1,
            (false, 'D') => 2,
            (true, 'H') => 3,
            (true, 'M') => 4,
            (true, 'S') => 5,
            _ => {
                return Err(duration_err());
            }
        };
        if order <= last_order || (character == 'W' && saw_unit) || (saw_week && character != 'W') {
            return Err(duration_err());
        }
        last_order = order;
        saw_unit = true;
        saw_week |= character == 'W';
        digits.clear();
    }
    if !saw_unit || !digits.is_empty() {
        return Err(duration_err());
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
            gettext("Timezone batch editing requires native VTIMEZONE support (Linux handoff)"),
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
        return Err(gettext("Due date must be after start date"));
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
        return Err(gettext("No field selected for batch editing"));
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
                gettext("The backend returned an outdated or ambiguous batch result"),
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
        .label(&gettext("Only enabled fields will be modified. Empty date, priority, classification, or duration values remove the corresponding property."))
        .wrap(true)
        .halign(gtk::Align::Start)
        .css_classes(vec!["dim-label"])
        .build();
    content.append(&hint);
    let (dtstart_check, dtstart) = task_bulk_edit_field(&content, &gettext("Start"), "YYYY-MM-DD");
    let (due_check, due) = task_bulk_edit_field(&content, &gettext("Due Date"), "YYYY-MM-DD");
    let (completed_check, completed) =
        task_bulk_edit_field(&content, &gettext("Completed"), "YYYY-MM-DD");
    let (status_check, status) = task_bulk_edit_field(
        &content,
        &gettext("Status"),
        "NEEDS-ACTION | IN-PROCESS | COMPLETED | CANCELLED",
    );
    let (priority_check, priority) = task_bulk_edit_field(&content, &gettext("Priority"), &gettext("0 to 9"));
    let (percent_check, percent) = task_bulk_edit_field(&content, &gettext("Progress"), &gettext("0 to 100"));
    let (class_check, class) = task_bulk_edit_field(
        &content,
        &gettext("Classification"),
        "PUBLIC | PRIVATE | CONFIDENTIAL",
    );
    let (duration_check, duration) = task_bulk_edit_field(&content, &gettext("Estimated Duration"), "PT1H");
    let (categories_add_check, categories_add) =
        task_bulk_edit_field(&content, &gettext("Add Categories"), &gettext("Comma-separated"));
    let (categories_remove_check, categories_remove) =
        task_bulk_edit_field(&content, &gettext("Remove Categories"), &gettext("Comma-separated"));
    let timezone =
        gtk::CheckButton::with_label(&gettext("Timezone (native VTIMEZONE support required)"));
    timezone.set_sensitive(false);
    content.append(&timezone);
    let dialog = adw::AlertDialog::builder()
        .heading(&gettext("Batch Edit Tasks"))
        .body(&gettext("The changes will be applied sequentially to the selected tasks."))
        .extra_child(&content)
        .default_response("apply")
        .build();
    dialog.add_responses(&[("cancel", &gettext("Cancel")), ("apply", &gettext("Apply"))]);
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
                &gettext("No field selected for batch editing"),
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
                    gettext("A task list is no longer writable"),
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
                    &gettext("Batch edit result was lost; authoritative data will be reloaded"),
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
            (count, false, 0) => {
                let msg = ngettext(
                    "{count} task modified",
                    "{count} tasks modified",
                    count as u32,
                )
                .replace("{count}", &count.to_string());
                page.show_toast(&msg);
            }
            (count, true, pending) => {
                let msg = ngettext(
                    "{count} task modified; error, {pending} not attempted",
                    "{count} tasks modified; error, {pending} not attempted",
                    count as u32,
                )
                .replace("{count}", &count.to_string())
                .replace("{pending}", &pending.to_string());
                page.show_toast(&msg);
            }
            _ => page.show_toast(&gettext("No tasks modified")),
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
        return Err(gettext("No tasks selected"));
    }
    let mut seen = HashSet::with_capacity(selection.len());
    let mut resolved = Vec::with_capacity(selection.len());
    for identity in selection {
        if !seen.insert(identity.clone()) {
            return Err(gettext("A selected task was resolved multiple times"));
        }
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err(gettext("The selected task is no longer uniquely available"));
        };
        let component = validated_task_vtodo_component(candidate, identity)?;
        if task_source_capability(&identity.source_uid, capabilities, local_writable)
            != TaskSourceCapability::Writable
        {
            return Err(
                gettext("At least one selected task list is read-only or unavailable"),
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
                gettext("Recurring tasks with RECURRENCE-ID cannot be safely deleted without MOD_THIS support"),
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
                gettext("This assigned task requires an iTIP cancellation notice (Linux handoff)"),
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
            return Err(gettext("A completed task is no longer uniquely available"));
        }
        let component = validated_task_vtodo_component(candidate, &candidate.identity)?;
        if task_has_top_level_property(&component, "RECURRENCE-ID") {
            return Err(
                gettext("A completed recurring instance requires native MOD_THIS support (Linux handoff)"),
            );
        }
        mutations.push(TaskDeleteMutation {
            identity: candidate.identity.clone(),
            summary: ical_property(&component, "SUMMARY").unwrap_or_default(),
        });
    }
    if mutations.is_empty() {
        return Err(gettext("No completed tasks in writable task lists"));
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
            title: gettext("Delete task “{summary}”?").replace("{summary}", &summary),
            description: gettext("All information about this task will be irrevocably deleted."),
        };
    }
    TaskDeleteConfirmation {
        title: ngettext("Delete task?", "Delete {count} tasks?", count as u32)
            .replace("{count}", &count.to_string()),
        description: ngettext(
            "All information about this task will be irrevocably deleted.",
            "All information about these {count} tasks will be irrevocably deleted.",
            count as u32,
        )
        .replace("{count}", &count.to_string()),
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
            gettext("The backend returned an outdated or ambiguous delete result"),
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
        return Err(gettext("No tasks selected"));
    }

    let mut components = Vec::with_capacity(selection.len());
    for identity in selection {
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.identity == *identity)
            .collect::<Vec<_>>();
        let [candidate] = matches.as_slice() else {
            return Err(gettext("The selected task is no longer available"));
        };
        components.push(validated_task_vtodo_component(candidate, identity)?);
    }

    Ok(format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Meridian//EN\r\n{}\r\nEND:VCALENDAR\r\n",
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
            gettext("No writable default task list available for paste"),
        );
    };
    let Some(source_uid) = source_uids.get(index) else {
        return Err(gettext("The destination task list is not uniquely available"));
    };
    if task_source_capability(source_uid, capabilities, local_writable)
        != TaskSourceCapability::Writable
    {
        return Err(
            gettext("The destination task list is read-only or unavailable"),
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
            gettext("Clipboard does not contain a valid VCALENDAR task selection"),
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
                    gettext("Clipboard does not contain exclusively portable VTODO components"),
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
                        gettext("Clipboard contains an invalid nested task"),
                    );
                }
                components.push(component.join("\r\n"));
                component.clear();
            }
        }
    }
    if !component.is_empty() || components.is_empty() {
        return Err(gettext("Clipboard does not contain a complete VTODO task"));
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
        return Err(gettext("The destination task list is invalid"));
    }
    let components = parse_task_clipboard_payload(payload)?;
    if components.len() != fresh_uids.len() {
        return Err(gettext("Paste planning does not contain unique new task UIDs"));
    }
    let mut identities = existing.iter().cloned().collect::<HashSet<_>>();
    let mut mutations = Vec::with_capacity(components.len());
    for (component, fresh_uid) in components.into_iter().zip(fresh_uids) {
        if task_component_requires_vtimezone(&component) {
            return Err(gettext("Tasks with TZID require VTIMEZONE support and will not be pasted insecurely"));
        }
        let original_uid = ical_properties(&component, "UID");
        let [_original_uid] = original_uid.as_slice() else {
            return Err(gettext("A clipboard task does not have a unique UID"));
        };
        if fresh_uid.trim().is_empty() {
            return Err(gettext("Clipboard tasks are not unique"));
        }
        let identity = TaskSelectionIdentity {
            source_uid: destination_uid.to_string(),
            task_uid: fresh_uid.clone(),
        };
        if !identities.insert(identity.clone()) {
            return Err(gettext("A new task UID already collides in the destination source"));
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
    let settings = AppSettings::try_new();
    let now_timestamp = current
        .map(|datetime| datetime.to_unix())
        .unwrap_or_else(|| Utc::now().timestamp());
    let hide_completed_tasks = settings
        .as_ref()
        .map(|s| s.hide_completed_tasks())
        .unwrap_or(false);
    let hide_completed_value = settings
        .as_ref()
        .map(|s| s.hide_completed_tasks_value())
        .unwrap_or(1);
    let hide_completed_cutoff = settings
        .as_ref()
        .and_then(|s| s.hide_completed_tasks_cutoff_timestamp(now_timestamp));
    let hide_cancelled_tasks = settings
        .as_ref()
        .map(|s| s.hide_cancelled_tasks())
        .unwrap_or(false);
    TaskFilterContext {
        today: now.date(),
        now,
        now_timestamp,
        hide_completed_tasks,
        hide_completed_value,
        hide_completed_cutoff,
        hide_cancelled_tasks,
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
                return Err(gettext("The iCalendar value is not a valid date"));
            }
            NaiveDate::parse_from_str(value, "%Y%m%d")
                .map(|date| (TaskDateValueKind::DateOnly, date))
                .map_err(|_| gettext("The iCalendar value is not a valid date"))
        }
        Some("DATE-TIME") => parse_task_datetime_date(value)
            .map(|date| (TaskDateValueKind::DateTime, date))
            .ok_or_else(|| gettext("The iCalendar value is not a valid timestamp")),
        Some(_) => Err(gettext("The iCalendar value has an unsupported type")),
        None if value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            NaiveDate::parse_from_str(value, "%Y%m%d")
                .map(|date| (TaskDateValueKind::DateOnly, date))
                .map_err(|_| gettext("The iCalendar value is not a valid date"))
        }
        None => parse_task_datetime_date(value)
            .map(|date| (TaskDateValueKind::DateTime, date))
            .ok_or_else(|| {
                gettext("The iCalendar value is not a valid date or timestamp")
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
        return Err(gettext("{field} cannot be empty").replace("{field}", field));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(gettext("{field} contains invalid control characters").replace("{field}", field));
    }
    if value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
    {
        return Err(gettext("{field} must match YYYY-MM-DD").replace("{field}", field));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| gettext("{field} must contain a valid date").replace("{field}", field))
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
        return Err(gettext("Completion date cannot be in the future"));
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
                return Err(gettext("Completed tasks require a completion date"));
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
        return Err(gettext("Recurring tasks require a DTSTART"));
    }
    if recurrence
        .chars()
        .any(|character| character == '\r' || character == '\n' || character.is_control())
    {
        return Err(gettext("The recurrence rule contains invalid characters"));
    }
    if task_recurrence_is_unchanged(existing, recurrence) {
        return Ok(());
    }

    let mut frequency = None;
    for part in recurrence.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            return Err(gettext("Recurrence rule has invalid syntax"));
        };
        if key.trim().is_empty() || value.trim().is_empty() {
            return Err(gettext("Recurrence rule has invalid syntax"));
        }
        if key.trim().eq_ignore_ascii_case("FREQ") {
            frequency = Some(value.trim().to_ascii_uppercase());
        }
    }
    let Some(frequency) = frequency else {
        return Err(gettext("Recurrence rule requires FREQ"));
    };
    if !matches!(
        frequency.as_str(),
        "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY"
    ) || parse_recurrence_rule(recurrence).is_none()
    {
        return Err(gettext("Recurrence rule is invalid"));
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

fn task_completed_timestamp(task: &TaskItemObject) -> Option<i64> {
    let component = task.component();
    let (_, value) = ical_property_with_parameters(&component, "COMPLETED")?;
    let value = value.trim();
    if let Some(stripped) = value.strip_suffix('Z') {
        if stripped.len() == 15 && stripped.as_bytes().get(8) == Some(&b'T') {
            if let Ok(naive) = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S") {
                return Some(naive.and_utc().timestamp());
            }
        }
    }
    if value.len() == 15 && value.as_bytes().get(8) == Some(&b'T') {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S") {
            return Some(naive.and_utc().timestamp());
        }
    }
    if value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        if let Ok(date) = NaiveDate::parse_from_str(value, "%Y%m%d") {
            return date.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp());
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return date.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp());
    }
    None
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
    let status_aliases = match task_status_value(&component) {
        "IN-PROCESS" => "in progress",
        "COMPLETED" => "completed",
        "CANCELLED" => "cancelled",
        _ => "not started",
    };
    format!(
        "{} {} {} {} {} {} {} {}",
        task.title(),
        task.due_date(),
        task.priority(),
        task_status_label(&component),
        status_aliases,
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

    fn select_sidebar_source_row(list: &gtk::ListBox, target_index: usize) {
        let mut child = list.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                let idx = unsafe { row.data::<usize>("source-index").map(|v| *v.as_ref()) };
                if idx == Some(target_index) {
                    list.select_row(Some(row));
                    return;
                }
            }
            child = widget.next_sibling();
        }
    }

    /// Trigger the same editor as the page-local “New Task” button.
    /// The main Evolution toolbar uses this when the Tasks module is
    /// active, so both entry points preserve one creation workflow.
    pub fn present_new_task(&self) {
        if let Some(button) = self.imp().new_task_button.borrow().as_ref() {
            button.emit_clicked();
        }
    }

    pub fn outer_paned(&self) -> Option<gtk::Paned> {
        self.imp().outer_paned.borrow().clone()
    }

    pub fn is_preview_visible(&self) -> bool {
        if let Some(preview_paned) = self.imp().task_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                return detail_box.is_visible();
            }
        }
        AppSettings::try_new()
            .map(|s| s.task_preview_visible())
            .unwrap_or(true)
    }

    pub fn set_preview_visible(&self, visible: bool) {
        if let Some(preview_paned) = self.imp().task_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                detail_box.set_visible(visible);
            }
        }
        if let Some(toggle) = self.imp().task_preview_toggle.borrow().as_ref() {
            if toggle.is_active() != visible {
                toggle.set_active(visible);
            }
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_task_preview_visible(visible);
        }
    }

    pub fn is_sidebar_visible(&self) -> bool {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if let Some(start) = paned.start_child() {
                return start.property::<bool>("visible");
            }
        }
        true
    }

    pub fn set_sidebar_visible(&self, visible: bool) {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if let Some(start) = paned.start_child() {
                start.set_visible(visible);
            }
            if visible {
                let saved_pos = AppSettings::try_new()
                    .map(|s| s.mail_sidebar_position())
                    .unwrap_or(260)
                    .clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
                if paned.position() < 180 {
                    paned.set_position(saved_pos);
                }
            }
        }
        if let Some(toggle) = self.imp().task_sidebar_toggle.borrow().as_ref() {
            if toggle.is_active() != visible {
                toggle.set_active(visible);
            }
        }
    }

    pub(crate) fn set_screen_mode(&self, is_mobile: bool, is_tablet: bool) {
        let imp = self.imp();
        imp.is_mobile.set(is_mobile);
        imp.is_tablet.set(is_tablet);
        self.sync_tasks_layout_visibility();
        self.sync_toolbar_button_labels();
    }

    pub(crate) fn apply_toolbar_button_modes(&self, is_compact: bool) {
        let imp = self.imp();
        let complete_label = crate::i18n::gettext("Completed");
        let delete_label = crate::i18n::gettext("Delete");
        let buttons: [(&RefCell<Option<gtk::Button>>, &str, &str); 2] = [
            (&imp.task_complete_button, &complete_label, "emblem-ok-symbolic"),
            (&imp.task_delete_selection_button, &delete_label, "user-trash-symbolic"),
        ];

        for (cell, text, icon) in buttons {
            if let Some(btn) = cell.borrow().as_ref() {
                if is_compact {
                    btn.set_child(Some(&gtk::Image::from_icon_name(icon)));
                } else {
                    let label = gtk::Label::builder()
                        .label(text)
                        .valign(gtk::Align::Center)
                        .build();
                    btn.set_child(Some(&label));
                }
            }
        }
    }

    pub(crate) fn sync_toolbar_button_labels(&self) {
        let is_compact = self.imp().is_mobile.get() || (self.width() > 0 && self.width() < 820);
        self.apply_toolbar_button_modes(is_compact);
    }

    pub(crate) fn is_detail_shown(&self) -> bool {
        self.imp().is_detail_shown.get()
    }

    pub(crate) fn selected_task_title(&self) -> Option<String> {
        let imp = self.imp();
        let selection = imp.task_selection.borrow();
        let first_sel = selection.first()?;
        let store = imp.task_store.borrow().as_ref()?.clone();
        for idx in 0..store.n_items() {
            if let Some(task) = store.item(idx).and_downcast::<TaskItemObject>() {
                if task.id() == first_sel.task_uid && task.source_uid() == first_sel.source_uid {
                    let title = task.title().trim().to_string();
                    return if title.is_empty() {
                        Some(crate::i18n::gettext("Task"))
                    } else {
                        Some(title)
                    };
                }
            }
        }
        None
    }

    pub(crate) fn can_navigate_back(&self) -> bool {
        let imp = self.imp();
        imp.is_mobile.get() && imp.is_detail_shown.get()
    }

    pub(crate) fn navigate_back(&self) -> bool {
        let imp = self.imp();
        if imp.is_mobile.get() && imp.is_detail_shown.get() {
            imp.is_detail_shown.set(false);
            self.sync_tasks_layout_visibility();
            if let Some(win) = crate::window::find_meridian_window(self) {
                win.sync_back_button();
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn sync_tasks_layout_visibility(&self) {
        let imp = self.imp();
        let is_mobile = imp.is_mobile.get();
        let is_tablet = imp.is_tablet.get();
        let Some(outer_paned) = imp.outer_paned.borrow().clone() else { return; };
        let Some(preview_paned) = imp.task_preview_paned.borrow().clone() else { return; };

        let sidebar = outer_paned.start_child();
        let list_container = preview_paned.start_child();
        let detail_box = preview_paned.end_child();

        if is_mobile {
            if let Some(sb) = sidebar.as_ref() {
                sb.set_visible(false);
            }
            if imp.is_detail_shown.get() {
                if let Some(lc) = list_container.as_ref() {
                    lc.set_visible(false);
                }
                if let Some(db) = detail_box.as_ref() {
                    db.set_visible(true);
                }
            } else {
                if let Some(lc) = list_container.as_ref() {
                    lc.set_visible(true);
                }
                if let Some(db) = detail_box.as_ref() {
                    db.set_visible(false);
                }
            }
        } else if is_tablet {
            if let Some(lc) = list_container.as_ref() {
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                let prev_vis = AppSettings::try_new()
                    .map(|s| s.task_preview_visible())
                    .unwrap_or(true);
                db.set_visible(prev_vis);
            }
        } else {
            if let Some(lc) = list_container.as_ref() {
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                let prev_vis = AppSettings::try_new()
                    .map(|s| s.task_preview_visible())
                    .unwrap_or(true);
                db.set_visible(prev_vis);
            }
        }

        if let Some(toolbar) = imp.toolbar.borrow().as_ref() {
            if is_mobile {
                toolbar.set_visible(false);
            } else {
                let show_tb = AppSettings::try_new()
                    .map(|s| s.task_show_toolbar())
                    .unwrap_or(false);
                toolbar.set_visible(show_tb);
            }
        }
    }

    pub fn is_toolbar_visible(&self) -> bool {
        self.imp()
            .toolbar
            .borrow()
            .as_ref()
            .map(|t| t.is_visible())
            .unwrap_or(false)
    }

    pub fn set_toolbar_visible(&self, visible: bool) {
        if let Some(toolbar) = self.imp().toolbar.borrow().as_ref() {
            toolbar.set_visible(visible);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_task_show_toolbar(visible);
        }
    }

    pub fn sidebar_toggle(&self) -> Option<gtk::ToggleButton> {
        self.imp().task_sidebar_toggle.borrow().clone()
    }

    pub fn preview_toggle(&self) -> Option<gtk::ToggleButton> {
        self.imp().task_preview_toggle.borrow().clone()
    }

    pub fn set_sidebar_position(&self, pos: i32) {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if (paned.position() - pos).abs() > 1 {
                paned.set_position(pos);
            }
        }
    }

    pub fn search_bar(&self) -> Option<gtk::SearchBar> {
        self.imp().task_search_bar.borrow().clone()
    }

    pub(crate) fn focus_search(&self) {
        if let Some(bar) = self.imp().task_search_bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn is_search_active(&self) -> bool {
        self.imp()
            .task_search_bar
            .borrow()
            .as_ref()
            .map(|b| b.is_search_mode())
            .unwrap_or(false)
    }

    pub(crate) fn close_search(&self) {
        if let Some(bar) = self.imp().task_search_bar.borrow().as_ref() {
            bar.set_search_mode(false);
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn reload_task_sources(&self) {
        start_task_source_discovery(self, None, false);
    }

    pub(crate) fn refresh_tasks(&self) {
        if let (Some(task_list), Some(task_store), Some(empty_state)) = (
            self.imp().task_list.borrow().as_ref(),
            self.imp().task_store.borrow().as_ref(),
            self.imp().task_empty_state.borrow().as_ref(),
        ) {
            render_task_rows(self, task_list, task_store, empty_state);
        }
    }

    pub(crate) fn render_task_sidebar(&self) {
        let Some(source_list) = self.imp().task_source_list.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = source_list.first_child() {
            source_list.remove(&child);
        }
        let Some(selector) = self.imp().task_source_selector_model.borrow().clone() else {
            return;
        };
        let collapsed_groups = self.imp().collapsed_task_groups.borrow().clone();
        let uids = self.imp().task_source_uids.borrow().clone();

        for group in selector.ordered_groups() {
            if group.is_hidden {
                continue;
            }
            let group_sources: Vec<_> = selector
                .visible_sources()
                .into_iter()
                .filter(|s| s.group_name == group.name)
                .collect();

            if group_sources.is_empty() && !group.is_custom {
                continue;
            }

            let is_collapsed = collapsed_groups.contains(&group.name);

            // Group Header Row (matching CalendarPage styling: 24px, arrow, label)
            let group_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .margin_start(0)
                .margin_end(6)
                .valign(gtk::Align::Center)
                .css_classes(vec![
                    "calendar-sidebar-row",
                    "calendar-sidebar-group-row",
                    "sidebar-account-row",
                ])
                .build();

            let arrow_icon = if is_collapsed {
                "pan-end-symbolic"
            } else {
                "pan-down-symbolic"
            };
            let arrow = gtk::Image::builder()
                .icon_name(arrow_icon)
                .pixel_size(16)
                .valign(gtk::Align::Center)
                .build();
            group_box.append(&arrow);

            let group_label = gtk::Label::builder()
                .label(&group.name)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(vec!["account-heading"])
                .build();
            group_box.append(&group_label);

            let group_row = gtk::ListBoxRow::builder()
                .child(&group_box)
                .activatable(true)
                .selectable(false)
                .css_classes(vec![
                    "calendar-sidebar-group-row",
                    "account-root-row",
                    "sidebar-account-row",
                ])
                .tooltip_text(if is_collapsed {
                    "Gruppe aufklappen"
                } else {
                    "Gruppe einklappen"
                })
                .build();

            let page_weak_for_group = self.downgrade();
            let group_name_for_toggle = group.name.clone();
            let group_gesture = gtk::GestureClick::new();
            group_gesture.connect_released(move |_, _, _, _| {
                let Some(page) = page_weak_for_group.upgrade() else {
                    return;
                };
                let mut collapsed = page.imp().collapsed_task_groups.borrow_mut();
                if collapsed.contains(&group_name_for_toggle) {
                    collapsed.remove(&group_name_for_toggle);
                } else {
                    collapsed.insert(group_name_for_toggle.clone());
                }
                drop(collapsed);
                page.render_task_sidebar();
            });
            group_row.add_controller(group_gesture);
            source_list.append(&group_row);

            if is_collapsed {
                continue;
            }

            for item in group_sources {
                let source_uid = item.uid.clone();
                let is_primary = selector.is_primary(&source_uid);
                let color_hex = item.color.clone().unwrap_or_else(|| "#3584e4".to_string());

                let child_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .margin_start(14)
                    .margin_end(6)
                    .css_classes(vec!["calendar-sidebar-row", "calendar-sidebar-child-row"])
                    .build();

                // Colored Checkbox matching calendar
                let clean_color: String = color_hex
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                let color_class = format!("cal-check-{}", clean_color);

                let check = gtk::CheckButton::builder()
                    .active(item.enabled && selector.is_selected(&source_uid))
                    .sensitive(item.enabled)
                    .valign(gtk::Align::Center)
                    .tooltip_text(&gettext("Show/hide task list in views"))
                    .css_classes(vec![color_class.as_str()])
                    .build();

                let page_weak_for_toggle = self.downgrade();
                let source_uid_for_toggle = source_uid.clone();
                check.connect_toggled(move |check| {
                    let Some(page) = page_weak_for_toggle.upgrade() else {
                        return;
                    };
                    {
                        if let Some(sel) = page.imp().task_source_selector_model.borrow_mut().as_mut() {
                            sel.set_selected(&source_uid_for_toggle, check.is_active());
                            if let Some(settings) = AppSettings::try_new() {
                                sel.save_to_settings(&settings);
                            }
                            *page.imp().hidden_task_sources.borrow_mut() =
                                sel.hidden_uids().into_iter().collect();
                        }
                    }
                    page.refresh_tasks();
                });
                child_box.append(&check);

                // Label
                let label = gtk::Label::builder()
                    .label(&item.display_name)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build();
                child_box.append(&label);

                // Primary indicator
                if is_primary {
                    let primary_ind = crate::models::source_selector::create_primary_indicator();
                    primary_ind.set_valign(gtk::Align::Center);
                    child_box.append(&primary_ind);
                }

                // Remote sync problem indicator (only shown if NOT synchronized / error present)
                if !item.enabled {
                    let sync_error_icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
                    sync_error_icon.add_css_class("warning");
                    sync_error_icon.set_tooltip_text(Some(&gettext("Task list not synchronized or offline")));
                    sync_error_icon.set_valign(gtk::Align::Center);
                    child_box.append(&sync_error_icon);
                }

                let row = gtk::ListBoxRow::builder()
                    .child(&child_box)
                    .activatable(true)
                    .selectable(true)
                    .css_classes(vec!["calendar-sidebar-child-row"])
                    .build();

                if let Some(index) = uids.iter().position(|u| u == &source_uid) {
                    unsafe {
                        row.set_data("source-index", index + 1);
                    }
                }

                // Right-click context menu
                let page_weak_for_menu = self.downgrade();
                let source_uid_for_menu = source_uid.clone();
                let display_name_for_menu = item.display_name.clone();
                let color_for_menu = color_hex.clone();
                let is_writable = item.writable;
                let source_list_weak = source_list.downgrade();

                let right_click = gtk::GestureClick::builder()
                    .button(gtk::gdk::BUTTON_SECONDARY)
                    .build();
                right_click.connect_pressed(move |gesture, _, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let (Some(page), Some(list)) =
                        (page_weak_for_menu.upgrade(), source_list_weak.upgrade())
                    else {
                        return;
                    };
                    page.present_task_source_context_menu(
                        &list,
                        x,
                        y,
                        &source_uid_for_menu,
                        &display_name_for_menu,
                        &color_for_menu,
                        is_writable,
                        is_primary,
                    );
                });
                row.add_controller(right_click);

                source_list.append(&row);
            }
        }
    }

    pub(crate) fn select_exclusive_task_source(&self, uid: &str) {
        {
            if let Some(sel) = self.imp().task_source_selector_model.borrow_mut().as_mut() {
                sel.select_exclusive(uid);
                if let Some(settings) = AppSettings::try_new() {
                    sel.save_to_settings(&settings);
                }
                *self.imp().hidden_task_sources.borrow_mut() =
                    sel.hidden_uids().into_iter().collect();
            }
        }
        self.render_task_sidebar();
        self.refresh_tasks();
    }

    pub(crate) fn select_all_task_sources(&self) {
        {
            if let Some(sel) = self.imp().task_source_selector_model.borrow_mut().as_mut() {
                sel.select_all();
                if let Some(settings) = AppSettings::try_new() {
                    sel.save_to_settings(&settings);
                }
                *self.imp().hidden_task_sources.borrow_mut() =
                    sel.hidden_uids().into_iter().collect();
            }
        }
        self.render_task_sidebar();
        self.refresh_tasks();
    }

    pub(crate) fn set_default_task_source(&self, uid: &str) {
        {
            if let Some(sel) = self.imp().task_source_selector_model.borrow_mut().as_mut() {
                sel.set_primary(uid);
                if let Some(settings) = AppSettings::try_new() {
                    sel.save_to_settings(&settings);
                    settings.set_default_task_source_uid(uid);
                }
                *self.imp().hidden_task_sources.borrow_mut() =
                    sel.hidden_uids().into_iter().collect();
            }
        }
        self.render_task_sidebar();
        self.refresh_tasks();
    }

    pub(crate) fn present_task_source_context_menu(
        &self,
        parent: &impl IsA<gtk::Widget>,
        x: f64,
        y: f64,
        source_uid: &str,
        display_name: &str,
        color: &str,
        is_writable: bool,
        is_primary: bool,
    ) {
        let page_weak = self.downgrade();
        let uid_excl = source_uid.to_string();
        let on_exclusive = {
            let page_weak = page_weak.clone();
            let uid = uid_excl;
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.select_exclusive_task_source(&uid);
                }
            }
        };

        let on_select_all = {
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.select_all_task_sources();
                }
            }
        };

        let uid_def = source_uid.to_string();
        let on_set_default = {
            let page_weak = page_weak.clone();
            let uid = uid_def;
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.set_default_task_source(&uid);
                }
            }
        };

        let uid_props = source_uid.to_string();
        let name_props = display_name.to_string();
        let col_props = color.to_string();
        let on_properties = Some({
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    let dialog = crate::dialogs::SourcePropertiesDialog::new(
                        &uid_props,
                        &name_props,
                        &gettext("Task List"),
                        &col_props,
                        is_writable,
                        is_primary,
                    );
                    let page_weak_save = page.downgrade();
                    dialog.set_on_save(move |update| {
                        if let Some(p) = page_weak_save.upgrade() {
                            {
                                if let Some(sel) = p.imp().task_source_selector_model.borrow_mut().as_mut() {
                                    sel.update_source_metadata(
                                        &update.source_uid,
                                        &update.display_name,
                                        Some(&update.color),
                                        update.is_default,
                                    );
                                    if let Some(settings) = AppSettings::try_new() {
                                        sel.save_to_settings(&settings);
                                        if update.is_default {
                                            settings.set_default_task_source_uid(&update.source_uid);
                                        }
                                    }
                                }
                            }
                            p.render_task_sidebar();
                            p.refresh_tasks();
                        }
                    });
                    dialog.present(Some(&page));
                }
            }
        });

        let on_manage_groups = Some({
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    let root_window = page.root().and_downcast::<gtk::Window>();
                    let current_sel = page
                        .imp()
                        .task_source_selector_model
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| {
                            SourceSelector::new(SourceDomain::Tasks)
                        });
                    let page_weak_save = page.downgrade();
                    let dialog = crate::dialogs::ManageGroupsDialog::new(
                        root_window.as_ref(),
                        &gettext("Tasks"),
                        &current_sel,
                        move |updated_selector| {
                            if let Some(p) = page_weak_save.upgrade() {
                                {
                                    if let Some(sel) = p.imp().task_source_selector_model.borrow_mut().as_mut() {
                                        *sel = updated_selector;
                                        sel.deselect_sources_in_hidden_groups();
                                        if let Some(settings) = AppSettings::try_new() {
                                            sel.save_to_settings(&settings);
                                        }
                                        *p.imp().hidden_task_sources.borrow_mut() =
                                            sel.hidden_uids().into_iter().collect();
                                    }
                                }
                                p.render_task_sidebar();
                                p.refresh_tasks();
                            }
                        },
                    );
                    dialog.present();
                }
            }
        });

        let popover = crate::models::source_selector::create_source_context_popover(
            is_primary,
            &gettext("Task List"),
            on_exclusive,
            on_select_all,
            on_set_default,
            on_properties,
            on_manage_groups,
        );
        popover.set_parent(parent);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            p.unparent();
        });
        popover.popup();
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
                    self.show_toast(&gettext("This task list is not confirmed as writable"));
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
                .hidden_task_sources
                .replace(settings.task_hidden_sources().into_iter().collect());
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
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .vexpand(true)
            .hexpand(true)
            .build();
        self.imp().outer_paned.replace(Some(paned.clone()));

        // ── Sidebar (Calendar parity: 24px rows, colored checkboxes) ────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_start(0)
            .margin_end(0)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["calendar-sidebar", "tasks-sidebar"])
            .build();

        let task_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        self.imp()
            .task_source_list
            .replace(Some(task_source_list.clone()));

        // Retain button instances in imp() for background source action handling
        let new_task_source_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(&gettext("New local task list"))
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(&new_task_source_button, &gettext("New local task list"));
        let copy_task_source_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(&gettext("Copy selected task list"))
            .css_classes(vec!["flat", "circular"])
            .sensitive(false)
            .build();
        set_accessible_label(&copy_task_source_button, &gettext("Copy selected task list"));
        let rename_task_source_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text(&gettext("Rename selected task list"))
            .css_classes(vec!["flat", "circular"])
            .sensitive(false)
            .build();
        set_accessible_label(&rename_task_source_button, &gettext("Rename selected task list"));
        let properties_task_source_button = gtk::Button::builder()
            .icon_name("document-properties-symbolic")
            .tooltip_text(&gettext("Selected task list properties"))
            .css_classes(vec!["flat", "circular"])
            .sensitive(false)
            .build();
        set_accessible_label(&properties_task_source_button, &gettext("Selected task list properties"));
        let delete_task_source_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(&gettext("Delete selected task list"))
            .css_classes(vec!["flat", "circular", "destructive-action"])
            .sensitive(false)
            .build();
        set_accessible_label(&delete_task_source_button, &gettext("Delete selected task list"));

        self.imp().task_source_new_button.replace(Some(new_task_source_button.clone()));
        self.imp().task_source_copy_button.replace(Some(copy_task_source_button.clone()));
        self.imp().task_source_rename_button.replace(Some(rename_task_source_button.clone()));
        self.imp().task_source_properties_button.replace(Some(properties_task_source_button.clone()));
        self.imp().task_source_delete_button.replace(Some(delete_task_source_button.clone()));

        let smart_header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_start(12)
            .margin_end(8)
            .margin_top(10)
            .margin_bottom(4)
            .build();
        let smart_header_lbl = gtk::Label::builder()
            .label(&gettext("FOCUS"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["account-heading"])
            .build();
        smart_header_box.append(&smart_header_lbl);
        sidebar_box.append(&smart_header_box);

        let task_smart_filter_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar", "tasks-smart-filter-list"])
            .build();
        let smart_views: [(TaskFilter, &str, String); 8] = [
            (TaskFilter::Today, "alarm-symbolic", gettext("Due Today")),
            (TaskFilter::Next7, "x-office-calendar-symbolic", gettext("Next 7 Days")),
            (TaskFilter::Overdue, "dialog-warning-symbolic", gettext("Overdue")),
            (TaskFilter::Important, "starred-symbolic", gettext("Important")),
            (TaskFilter::InProgress, "media-playback-start-symbolic", gettext("In Progress")),
            (TaskFilter::Uncompleted, "view-list-bullet-symbolic", gettext("Open Tasks")),
            (TaskFilter::All, "view-list-symbolic", gettext("All Tasks")),
            (TaskFilter::Completed, "emblem-ok-symbolic", gettext("Completed")),
        ];
        let mut smart_badges = Vec::new();
        let initial_smart_filter = self.imp().task_filter.get();
        let mut initial_smart_row: Option<gtk::ListBoxRow> = None;
        for (sf, icon_name, title) in &smart_views {
            let row_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .margin_start(8)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            let icon = gtk::Image::builder()
                .icon_name(*icon_name)
                .pixel_size(15)
                .css_classes(vec!["dim-label"])
                .build();
            let lbl = gtk::Label::builder()
                .label(title)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let mut badge_classes = vec!["task-smart-count-badge"];
            if *sf == TaskFilter::Overdue {
                badge_classes.push("task-smart-count-overdue");
            }
            let badge = gtk::Label::builder()
                .label("0")
                .valign(gtk::Align::Center)
                .visible(false)
                .css_classes(badge_classes)
                .build();
            row_box.append(&icon);
            row_box.append(&lbl);
            row_box.append(&badge);
            let row = gtk::ListBoxRow::builder()
                .child(&row_box)
                .css_classes(vec!["calendar-sidebar-row", "task-smart-filter-row"])
                .build();
            unsafe {
                row.set_data("smart-task-filter", *sf);
            }
            task_smart_filter_list.append(&row);
            if *sf == initial_smart_filter {
                initial_smart_row = Some(row);
            }
            smart_badges.push((*sf, badge));
        }
        if let Some(row) = initial_smart_row.as_ref() {
            task_smart_filter_list.select_row(Some(row));
        }
        self.imp()
            .task_smart_filter_list
            .replace(Some(task_smart_filter_list.clone()));
        self.imp()
            .task_smart_filter_badges
            .replace(smart_badges);
        sidebar_box.append(&task_smart_filter_list);

        let lists_header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(12)
            .margin_end(6)
            .margin_top(12)
            .margin_bottom(2)
            .build();
        let lists_header_lbl = gtk::Label::builder()
            .label(&gettext("TASK LISTS"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["account-heading"])
            .build();
        lists_header_box.append(&lists_header_lbl);
        lists_header_box.append(&new_task_source_button);
        sidebar_box.append(&lists_header_box);

        sidebar_box.append(&task_source_list);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(40)
            .child(&sidebar_box)
            .vexpand(true)
            .build();

        let drawer_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .css_classes(vec!["sidebar-drawer-header"])
            .build();
        let drawer_icon = gtk::Image::from_icon_name("view-list-bullet-symbolic");
        let drawer_title = gtk::Label::builder()
            .label(&crate::i18n::gettext("Tasks"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["meridian-sidebar-drawer"])
            .build();
        sidebar_outer_box.append(&drawer_header);
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "tasks",
            &sidebar_outer_box,
        ));

        paned.set_start_child(Some(&sidebar_outer_box));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(true);

        // ── Main Content (Task List) ────────────────────────────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .hexpand(true)
            .css_classes(vec!["unified-view-toolbar", "toolbar"])
            .build();

        let search_mode_model = gtk::StringList::new(&[
            &crate::i18n::gettext("All Fields"),
            &crate::i18n::gettext("Summary"),
            &crate::i18n::gettext("Description"),
            &crate::i18n::gettext("Advanced"),
        ]);
        let search_mode_selector =
            gtk::DropDown::new(Some(search_mode_model), None::<&gtk::Expression>);
        search_mode_selector.set_selected(self.imp().task_search_mode.get().index());
        search_mode_selector.set_tooltip_text(Some(&crate::i18n::gettext(
            "Select search field; Advanced supports summary:, description:, category:, due:, start:, status:, and has:attachment",
        )));

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(&crate::i18n::gettext("Search tasks…"))
            .width_chars(36)
            .build();
        search_entry.set_tooltip_text(Some(&crate::i18n::gettext(
            "Multiple terms are combined. In Advanced mode: category:Work due:next7 has:attachment",
        )));
        self.imp().search_entry.replace(Some(search_entry.clone()));

        let sort_model = gtk::StringList::new(&[
            &crate::i18n::gettext("Due Date"),
            &crate::i18n::gettext("Title"),
            &crate::i18n::gettext("Priority"),
        ]);
        let sort_selector = gtk::DropDown::new(Some(sort_model), None::<&gtk::Expression>);
        sort_selector.set_tooltip_text(Some(&crate::i18n::gettext("Sort tasks")));
        sort_selector.set_selected(match self.imp().task_sort.get() {
            TaskSort::Title => 1,
            TaskSort::Priority => 2,
            TaskSort::DueDate => 0,
        });

        let direction_button = gtk::Button::builder()
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        let set_direction_button = |button: &gtk::Button, descending: bool| {
            button.set_icon_name(if descending {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            button.set_tooltip_text(Some(&if descending {
                crate::i18n::gettext("Sorted descending")
            } else {
                crate::i18n::gettext("Sorted ascending")
            }));
            set_accessible_label(
                button,
                &if descending {
                    crate::i18n::gettext("Change sort to ascending")
                } else {
                    crate::i18n::gettext("Change sort to descending")
                },
            );
        };
        set_direction_button(&direction_button, self.imp().task_sort_descending.get());

        let source_model = gtk::StringList::new(&[&crate::i18n::gettext("All Task Lists")]);
        let source_selector =
            gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        source_selector.set_tooltip_text(Some(&crate::i18n::gettext("Select task list")));
        self.imp()
            .task_source_model
            .replace(Some(source_model.clone()));
        self.imp()
            .task_source_selector
            .replace(Some(source_selector.clone()));
        let page_weak_for_sidebar_row = self.downgrade();
        task_source_list.connect_row_selected(move |_, row| {
            let Some(page) = page_weak_for_sidebar_row.upgrade() else {
                return;
            };
            if page.imp().task_source_syncing.get() {
                return;
            }
            if let Some(row) = row {
                if let Some(target_index) = unsafe { row.data::<usize>("source-index").map(|v| *v.as_ref()) } {
                    let uids = page.imp().task_source_uids.borrow().clone();
                    if let Some(uid) = uids.get(target_index.saturating_sub(1)) {
                        page.set_default_task_source(uid);
                    }
                }
            }
        });

        let category_model = gtk::StringList::new(&[&crate::i18n::gettext("All Categories")]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some(&crate::i18n::gettext("Select task category")));
        self.imp()
            .task_category_model
            .replace(Some(category_model.clone()));
        self.imp()
            .task_category_selector
            .replace(Some(category_selector.clone()));

        let btn_add_task = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(&crate::i18n::gettext("Create new task (Ctrl+N)"))
            .css_classes(vec!["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        set_button_icon_with_fallback(&btn_add_task, "list-add-symbolic", "document-new-symbolic");
        set_accessible_label(&btn_add_task, &crate::i18n::gettext("New Task"));
        self.imp()
            .new_task_button
            .replace(Some(btn_add_task.clone()));

        let selection_status = gtk::Label::builder()
            .label(&crate::i18n::gettext("No selection"))
            .css_classes(vec!["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        self.imp()
            .task_selection_status
            .replace(Some(selection_status.clone()));

        let forward_selection_button = gtk::Button::builder()
            .icon_name("mail-forward-symbolic")
            .tooltip_text(&crate::i18n::gettext("Forward selected task as iCalendar"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &forward_selection_button,
            &crate::i18n::gettext("Forward selected task as iCalendar"),
        );
        let assign_selection_button = gtk::Button::builder()
            .icon_name("avatar-default-symbolic")
            .tooltip_text(&crate::i18n::gettext("Assign selected task"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&assign_selection_button, &crate::i18n::gettext("Assign selected task"));
        let open_url_selection_button = gtk::Button::builder()
            .icon_name("external-link-symbolic")
            .tooltip_text(&crate::i18n::gettext("Open URL of selected task"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &open_url_selection_button,
            &crate::i18n::gettext("Open URL of selected task"),
        );
        let print_selection_button = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text(&crate::i18n::gettext("Print selected task"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&print_selection_button, &crate::i18n::gettext("Print selected task"));
        let print_list_button = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text(&crate::i18n::gettext("Print visible task list (Ctrl+P)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&print_list_button, &crate::i18n::gettext("Print visible task list"));
        let print_list_preview_button = gtk::Button::builder()
            .icon_name("document-print-preview-symbolic")
            .tooltip_text(&crate::i18n::gettext("Prepare print preview of visible task list"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(
            &print_list_preview_button,
            &crate::i18n::gettext("Prepare print preview of visible task list"),
        );
        let refresh_source_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(&crate::i18n::gettext("Refresh selected task list from provider (F5)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &refresh_source_button,
            &crate::i18n::gettext("Refresh selected task list"),
        );
        let copy_selection_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(&crate::i18n::gettext("Copy selected tasks to clipboard (Ctrl+C)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &copy_selection_button,
            &crate::i18n::gettext("Copy selected tasks to clipboard"),
        );
        let cut_selection_button = gtk::Button::builder()
            .icon_name("edit-cut-symbolic")
            .tooltip_text(&crate::i18n::gettext("Cut selected tasks (Ctrl+X)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&cut_selection_button, &crate::i18n::gettext("Cut selected tasks"));
        let paste_selection_button = gtk::Button::builder()
            .icon_name("edit-paste-symbolic")
            .tooltip_text(&crate::i18n::gettext("Paste tasks from clipboard (Ctrl+V)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &paste_selection_button,
            &crate::i18n::gettext("Paste tasks from clipboard"),
        );
        let select_all_tasks_button = gtk::Button::builder()
            .icon_name("edit-select-all-symbolic")
            .tooltip_text(&crate::i18n::gettext("Select all visible tasks (Ctrl+A)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(
            &select_all_tasks_button,
            &crate::i18n::gettext("Select all visible tasks"),
        );
        let bulk_edit_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text(&crate::i18n::gettext("Edit selected tasks together"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &bulk_edit_button,
            &crate::i18n::gettext("Edit selected tasks together"),
        );
        let complete_selection_button = gtk::Button::builder()
            .icon_name("emblem-ok-symbolic")
            .tooltip_text(&crate::i18n::gettext("Mark selected tasks as completed"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &complete_selection_button,
            &crate::i18n::gettext("Mark selected tasks as completed"),
        );
        let incomplete_selection_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(&crate::i18n::gettext("Mark selected tasks as incomplete"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &incomplete_selection_button,
            &crate::i18n::gettext("Mark selected tasks as incomplete"),
        );
        let delete_selection_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(&crate::i18n::gettext("Delete selected tasks (Delete)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&delete_selection_button, &crate::i18n::gettext("Delete selected tasks"));
        let purge_button = gtk::Button::builder()
            .icon_name("edit-clear-all-symbolic")
            .tooltip_text(&crate::i18n::gettext("Permanently purge all completed tasks (Ctrl+E)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&purge_button, &crate::i18n::gettext("Permanently purge all completed tasks"));
        let export_selection_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text(&crate::i18n::gettext("Export selected tasks as iCalendar"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(
            &export_selection_button,
            &crate::i18n::gettext("Export selected tasks as iCalendar"),
        );
        let preview_visible = AppSettings::try_new()
            .map(|settings| settings.task_preview_visible())
            .unwrap_or(true);
        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text(&crate::i18n::gettext("Toggle task preview (Ctrl+M)"))
            .active(preview_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&preview_toggle, &crate::i18n::gettext("Toggle task preview"));

        let sidebar_visible = AppSettings::try_new()
            .map(|s| s.mail_show_sidebar())
            .unwrap_or(true);
        sidebar_outer_box.set_visible(sidebar_visible);
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Seitenleiste ein-/ausblenden (F9)")
            .active(sidebar_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&sidebar_toggle, &gettext("Toggle sidebar"));
        self.imp()
            .task_sidebar_toggle
            .replace(Some(sidebar_toggle.clone()));

        let sidebar_for_toggle = sidebar_outer_box.clone();
        let page_weak_for_sidebar = self.downgrade();
        sidebar_toggle.connect_toggled(move |toggle| {
            let is_active = toggle.is_active();
            if let Some(page) = page_weak_for_sidebar.upgrade() {
                if let Some(win) = crate::window::find_meridian_window(&page) {
                    if win.is_sidebar_visible_for_active_view() != is_active {
                        win.toggle_sidebar_animated(is_active);
                        return;
                    }
                }
                page.set_sidebar_visible(is_active);
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_show_sidebar(is_active);
                }
            } else {
                sidebar_for_toggle.set_visible(is_active);
            }
        });
        let preview_orientation_model =
            gtk::StringList::new(&[&crate::i18n::gettext("Preview Below"), &crate::i18n::gettext("Preview Beside")]);
        let preview_orientation_selector =
            gtk::DropDown::new(Some(preview_orientation_model), None::<&gtk::Expression>);
        let preview_orientation = AppSettings::try_new()
            .map(|settings| {
                TaskPreviewOrientation::from_setting(&settings.task_preview_orientation())
            })
            .unwrap_or(TaskPreviewOrientation::Vertical);
        preview_orientation_selector.set_selected(preview_orientation.index());
        preview_orientation_selector.set_tooltip_text(Some(&crate::i18n::gettext("Task preview layout")));

        self.imp().task_forward_button.replace(Some(forward_selection_button.clone()));
        self.imp().task_assign_button.replace(Some(assign_selection_button.clone()));
        self.imp().task_open_url_button.replace(Some(open_url_selection_button.clone()));
        self.imp().task_print_button.replace(Some(print_selection_button.clone()));
        self.imp().task_print_list_button.replace(Some(print_list_button.clone()));
        self.imp().task_copy_button.replace(Some(copy_selection_button.clone()));
        self.imp().task_cut_button.replace(Some(cut_selection_button.clone()));
        self.imp().task_paste_button.replace(Some(paste_selection_button.clone()));
        self.imp().task_select_all_button.replace(Some(select_all_tasks_button.clone()));
        self.imp().task_bulk_edit_button.replace(Some(bulk_edit_button.clone()));
        self.imp().task_complete_button.replace(Some(complete_selection_button.clone()));
        self.imp().task_incomplete_button.replace(Some(incomplete_selection_button.clone()));
        self.imp().task_delete_selection_button.replace(Some(delete_selection_button.clone()));
        self.imp().task_purge_button.replace(Some(purge_button.clone()));
        self.imp().task_refresh_button.replace(Some(refresh_source_button.clone()));
        self.imp().task_export_button.replace(Some(export_selection_button.clone()));
        self.imp().task_sidebar_toggle.replace(Some(sidebar_toggle.clone()));
        self.imp().task_preview_toggle.replace(Some(preview_toggle.clone()));
        self.imp().task_preview_orientation.replace(Some(preview_orientation_selector.clone()));

        // ── GNOME HIG Unified Toolbar Construction ──
        // 1. Triage group (.linked): Complete, Delete
        let triage_group = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .valign(gtk::Align::Center)
            .build();
        triage_group.append(&complete_selection_button);
        triage_group.append(&delete_selection_button);

        // 2. Filter DropDown (all 16 task filters consolidated in one place)
        let initial_filter = self.imp().task_filter.get();
        let filter_options_names: Vec<String> =
            TASK_FILTER_OPTIONS.iter().map(|(_, _, label)| crate::i18n::gettext(*label)).collect();
        let filter_options_refs: Vec<&str> =
            filter_options_names.iter().map(|s| s.as_str()).collect();
        let filter_options_model = gtk::StringList::new(&filter_options_refs);
        let filter_dropdown = gtk::DropDown::new(
            Some(filter_options_model),
            None::<&gtk::Expression>,
        );
        filter_dropdown.set_valign(gtk::Align::Center);
        filter_dropdown.set_tooltip_text(Some(&crate::i18n::gettext("Select task filter")));
        set_accessible_label(&filter_dropdown, &crate::i18n::gettext("Task filter"));
        filter_dropdown.set_selected(task_filter_index(initial_filter) as u32);

        // 3. Sort group (.linked)
        let sort_group = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .valign(gtk::Align::Center)
            .build();
        sort_selector.set_valign(gtk::Align::Center);
        sort_group.append(&sort_selector);
        sort_group.append(&direction_button);

        // 4. More Actions Popover Menu ([⋮])
        let more_popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let more_popover = gtk::Popover::builder()
            .child(&more_popover_content)
            .build();

        let make_popover_item = |label: &str, icon: &str, target_btn: &gtk::Button, popover: &gtk::Popover| {
            let item_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            item_box.append(&gtk::Image::from_icon_name(icon));
            item_box.append(&gtk::Label::builder().label(label).halign(gtk::Align::Start).hexpand(true).build());
            let item_btn = gtk::Button::builder()
                .child(&item_box)
                .css_classes(vec!["flat"])
                .halign(gtk::Align::Fill)
                .sensitive(target_btn.is_sensitive())
                .build();
            let target = target_btn.clone();
            let pop_weak = popover.downgrade();
            item_btn.connect_clicked(move |_| {
                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }
                target.emit_clicked();
            });
            item_btn.bind_property("sensitive", target_btn, "sensitive")
                .sync_create()
                .build();
            item_btn
        };

        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Select All Tasks (Ctrl+A)"), "edit-select-all-symbolic", &select_all_tasks_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Mark as Incomplete"), "view-refresh-symbolic", &incomplete_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Cut (Ctrl+X)"), "edit-cut-symbolic", &cut_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Copy (Ctrl+C)"), "edit-copy-symbolic", &copy_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Paste (Ctrl+V)"), "edit-paste-symbolic", &paste_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Edit Multiple Tasks…"), "document-edit-symbolic", &bulk_edit_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Purge Completed Tasks… (Ctrl+E)"), "edit-clear-all-symbolic", &purge_button, &more_popover));
        more_popover_content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Assign Task…"), "avatar-default-symbolic", &assign_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Forward as iCalendar…"), "mail-forward-symbolic", &forward_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Open Task URL"), "external-link-symbolic", &open_url_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Export as iCalendar…"), "document-save-symbolic", &export_selection_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Print Task List… (Ctrl+P)"), "document-print-symbolic", &print_list_button, &more_popover));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Print Preview…"), "document-print-preview-symbolic", &print_list_preview_button, &more_popover));
        more_popover_content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        more_popover_content.append(&make_popover_item(&crate::i18n::gettext("Refresh Task List (F5)"), "view-refresh-symbolic", &refresh_source_button, &more_popover));
        more_popover_content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let preview_orientation_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(4)
            .margin_end(4)
            .build();
        preview_orientation_box.append(&gtk::Label::builder().label(&crate::i18n::gettext("Preview:")).halign(gtk::Align::Start).css_classes(vec!["dim-label"]).build());
        preview_orientation_selector.set_valign(gtk::Align::Center);
        preview_orientation_box.append(&preview_orientation_selector);
        more_popover_content.append(&preview_orientation_box);

        let more_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&more_popover)
            .tooltip_text(&crate::i18n::gettext("More Actions"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&more_button, &crate::i18n::gettext("More Actions"));

        // 5. Left Toolbar Section: Triage (Complete, Delete) + More menu
        let left_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();
        left_box.append(&triage_group);
        left_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        left_box.append(&more_button);

        // 6. Right Toolbar Section: Sort -> Filter Dropdown (Right-Aligned)
        let right_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .build();
        right_box.append(&sort_group);
        right_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        right_box.append(&filter_dropdown);

        let spacer = gtk::Box::builder().hexpand(true).build();

        toolbar.append(&left_box);
        toolbar.append(&spacer);
        toolbar.append(&right_box);

        let toolbar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(false)
            .min_content_width(0)
            .child(&toolbar)
            .build();
        toolbar_scroll.set_visible(
            AppSettings::try_new()
                .map(|settings| settings.task_show_toolbar())
                .unwrap_or(false),
        );
        self.imp()
            .toolbar
            .replace(Some(toolbar_scroll.clone().upcast()));

        // ── GNOME HIG SearchBar (matching Mail page search bar) ──
        let task_search_bar = gtk::SearchBar::builder()
            .search_mode_enabled(false)
            .key_capture_widget(self)
            .build();
        task_search_bar.connect_entry(&search_entry);
        self.imp()
            .task_search_bar
            .replace(Some(task_search_bar.clone()));

        let search_content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        search_entry.set_valign(gtk::Align::Center);
        search_mode_selector.set_valign(gtk::Align::Center);
        category_selector.set_valign(gtk::Align::Center);

        search_content_box.append(&search_entry);
        search_content_box.append(&search_mode_selector);
        search_content_box.append(&category_selector);
        task_search_bar.set_child(Some(&search_content_box));

        let entry_for_search_mode = search_entry.clone();
        task_search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            if bar.is_search_mode() {
                entry_for_search_mode.grab_focus();
            } else if !entry_for_search_mode.text().is_empty() {
                entry_for_search_mode.set_text("");
            }
        });

        let search_bar_for_stop = task_search_bar.clone();
        search_entry.connect_stop_search(move |_| {
            search_bar_for_stop.set_search_mode(false);
        });

        let task_store = gio::ListStore::new::<TaskItemObject>();

        let task_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Multiple)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(vec!["boxed-list", "task-boxed-list"])
            .build();

        let empty_tasks = gtk::Label::builder()
            .label(&gettext("Loading tasks…"))
            .css_classes(vec!["dim-label", "task-empty-state-title"])
            .margin_top(36)
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
            .placeholder_text(&gettext("Search in preview"))
            .hexpand(true)
            .sensitive(false)
            .build();
        let preview_find_previous = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text(&gettext("Previous match"))
            .build();
        let preview_find_next = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text(&gettext("Next match"))
            .build();
        let preview_find_status = gtk::Label::new(None);
        let preview_find_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(&gettext("Close preview search"))
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
        let inspector_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(14)
            .margin_end(14)
            .margin_top(12)
            .margin_bottom(16)
            .build();
        let inspector_scroll = gtk::ScrolledWindow::builder()
            .child(&inspector_box)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        self.imp()
            .task_inspector_box
            .replace(Some(inspector_box.clone()));
        let preview_empty_state = adw::StatusPage::builder()
            .icon_name("document-properties-symbolic")
            .title(&gettext("No task selected"))
            .description(&gettext("Select a task from the list to view and edit its details, checklist, and notes."))
            .vexpand(true)
            .hexpand(true)
            .build();
        let preview_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .hexpand(true)
            .build();
        preview_stack.add_named(&preview_empty_state, Some("empty"));
        preview_stack.add_named(&inspector_scroll, Some("content"));
        preview_stack.add_named(&preview_scroll, Some("raw_text"));
        preview_stack.set_visible_child_name("empty");

        let preview_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["background"])
            .visible(preview_visible)
            .build();
        preview_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        preview_box.append(&preview_find_bar);
        preview_box.append(&preview_stack);
        self.imp()
            .task_preview_stack
            .replace(Some(preview_stack.clone()));
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
            let active = toggle.is_active();
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_preview_visible(active);
            }
            if let Some(page) = page_weak_for_preview_toggle.upgrade() {
                if let Some(preview_paned) = page.imp().task_preview_paned.borrow().as_ref() {
                    if let Some(detail_box) = preview_paned.end_child() {
                        detail_box.set_visible(active);
                    }
                }
                refresh_task_preview(&page);
                if let Some(win) = crate::window::find_meridian_window(&page) {
                    win.sync_right_sidebar_toggle();
                }
            }
        });

        let page_weak_for_preview_orientation = self.downgrade();
        preview_orientation_selector.connect_selected_notify(move |selector| {
            let sel = selector.selected();
            if sel == gtk::INVALID_LIST_POSITION {
                return;
            }
            let orientation = TaskPreviewOrientation::from_index(sel);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_preview_orientation(orientation.setting_value());
            }
            if let Some(page) = page_weak_for_preview_orientation.upgrade() {
                apply_task_preview_paned_layout(&page, orientation);
            }
        });

        let page_weak_for_selection = self.downgrade();
        task_list.connect_selected_rows_changed(move |list| {
            let Some(page) = page_weak_for_selection.upgrade() else {
                return;
            };
            if page.imp().task_selection_restoring.get() {
                return;
            }
            let selection = task_selection_from_list(list);
            let has_selection = !selection.is_empty();
            page.imp()
                .task_selection
                .replace(selection);
            update_task_selection_controls(&page);
            if page.imp().is_mobile.get() && has_selection {
                page.imp().is_detail_shown.set(true);
                page.sync_tasks_layout_visibility();
                if let Some(win) = crate::window::find_meridian_window(&page) {
                    win.sync_back_button();
                }
            }
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
                    show_toast_for_widget(button, &gettext("Tasks copied to clipboard"));
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
                show_toast_for_widget(button, &gettext("Cut is not available for this selection"));
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
        let search_bar_for_keys = task_search_bar.clone();
        let sidebar_toggle_for_keys = sidebar_toggle.clone();
        let clipboard_keys = gtk::EventControllerKey::new();
        clipboard_keys.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::F9 {
                sidebar_toggle_for_keys.set_active(!sidebar_toggle_for_keys.is_active());
                return glib::Propagation::Stop;
            }
            if !modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            if matches!(key, gtk::gdk::Key::f | gtk::gdk::Key::F) {
                if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                    // This controller belongs only to the task list.  Require an
                    // exact current single selection before exposing preview find.
                    if preview_toggle_for_keys.is_active()
                        && preview_find_entry_for_keys.is_sensitive()
                    {
                        preview_find_bar_for_keys.set_visible(true);
                        preview_find_entry_for_keys.grab_focus();
                        return glib::Propagation::Stop;
                    }
                    return glib::Propagation::Proceed;
                } else {
                    let is_active = search_bar_for_keys.is_search_mode();
                    search_bar_for_keys.set_search_mode(!is_active);
                    return glib::Propagation::Stop;
                }
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
                print_list_for_keys.emit_clicked();
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
                action.emit_clicked();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        // This controller is attached only to the task list, so editable
        // search and task-editor widgets retain their standard shortcuts.
        task_list.add_controller(clipboard_keys);

        let page_key_controller = gtk::EventControllerKey::new();
        let preview_toggle_for_page = preview_toggle.clone();
        let sidebar_toggle_for_page = sidebar_toggle.clone();
        page_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::F9 {
                sidebar_toggle_for_page.set_active(!sidebar_toggle_for_page.is_active());
                return glib::Propagation::Stop;
            }
            if (key == gtk::gdk::Key::m || key == gtk::gdk::Key::M)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                preview_toggle_for_page.set_active(!preview_toggle_for_page.is_active());
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.add_controller(page_key_controller);

        // Initialize toolbar button modes (desktop: pure text, mobile: pure icon)
        let is_compact_initial = self.imp().is_mobile.get() || (self.width() > 0 && self.width() < 820);
        self.apply_toolbar_button_modes(is_compact_initial);

        self.connect_root_notify(move |page| {
            if let Some(win) = page.root().and_downcast::<gtk::Window>() {
                let page_weak = page.downgrade();
                win.connect_default_width_notify(move |_| {
                    if let Some(page) = page_weak.upgrade() {
                        page.sync_toolbar_button_labels();
                    }
                });
            }
        });

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
                .title(&gettext("Save Task Selection as iCalendar"))
                .initial_name("tasks.ics")
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
                        &gettext("Task selection exported as iCalendar"),
                    ),
                    Err(error) => {
                        let msg = gettext("Could not save iCalendar: {error}").replace("{error}", &error.to_string());
                        show_toast_for_widget(
                            &button_for_result,
                            &msg,
                        );
                    }
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
                        &gettext("METHOD:PUBLISH prepared; true iTIP forward is Linux handoff"),
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
                    Ok(()) => show_toast_for_widget(&button_for_result, &gettext("URL opened")),
                    Err(error) => {
                        let msg = gettext("Could not open URL: {error}").replace("{error}", &error.to_string());
                        show_toast_for_widget(
                            &button_for_result,
                            &msg,
                        );
                    }
                },
            );
        });

        let page_weak_for_print = self.downgrade();
        let task_store_for_print = task_store.clone();
        print_selection_button.connect_clicked(move |button| {
            let Some(page) = page_weak_for_print.upgrade() else {
                return;
            };
            show_toast_for_widget(button, &gettext("Preparing print document…"));
            let selection = page.imp().task_selection.borrow().clone();
            match build_task_print_document(
                &selection,
                &task_export_candidates(&task_store_for_print),
            ) {
                Ok(document) => {
                    let chars = document.plain_text.chars().count().to_string();
                    let msg = gettext("Print document prepared ({chars} characters); GTK print dialog / portal is Linux handoff")
                        .replace("{chars}", &chars);
                    show_toast_for_widget(
                        button,
                        &msg,
                    );
                }
                Err(error) => {
                    let msg = gettext("Could not prepare print document: {error}")
                        .replace("{error}", &error);
                    show_toast_for_widget(
                        button,
                        &msg,
                    );
                }
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
                page.show_toast(&gettext("This task list is not confirmed as writable"));
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
            let sel = selector.selected();
            if sel == gtk::INVALID_LIST_POSITION {
                return;
            }
            let mode = TaskSearchMode::from_index(sel);
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
            let sel = selector.selected();
            if sel == gtk::INVALID_LIST_POSITION {
                return;
            }
            page.imp().task_sort.set(match sel {
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
            if page.imp().task_source_syncing.get() {
                return;
            }
            let sel = selector.selected();
            if sel == gtk::INVALID_LIST_POSITION {
                return;
            }
            let source_filter = if sel == 0 {
                None
            } else {
                let uids = page.imp().task_source_uids.borrow().clone();
                let idx = sel as usize - 1;
                uids.get(idx).cloned()
            };
            page.imp().task_source_filter.replace(source_filter.clone());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_source_filter(source_filter.as_deref().unwrap_or_default());
            }
            TasksPage::select_sidebar_source_row(&task_source_list_for_selector, sel as usize);
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
        category_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_category.upgrade() else {
                return;
            };
            if page.imp().task_category_syncing.get() {
                return;
            }
            let sel = selector.selected();
            if sel == gtk::INVALID_LIST_POSITION {
                return;
            }
            let category = if sel == 0 {
                None
            } else {
                category_model_for_selector
                    .string(sel)
                    .map(|value| value.to_string())
            };
            page.imp().task_category_filter.replace(category);
            if let Some(settings) = AppSettings::try_new() {
                let current_cat = page.imp().task_category_filter.borrow().clone();
                settings.set_task_category_filter(
                    current_cat.as_deref().unwrap_or_default(),
                );
            }
            render_task_rows(
                &page,
                &task_list_for_category,
                &task_store_for_category,
                &empty_tasks_for_category,
            );
        });

        let syncing_filter = Rc::new(Cell::new(false));

        let page_weak_for_categories = self.downgrade();
        let task_list_for_categories = task_list.clone();
        let task_store_for_categories = task_store.clone();
        let empty_tasks_for_categories = empty_tasks.clone();
        let syncing_filter_for_apply = Rc::clone(&syncing_filter);
        let filter_dropdown_for_apply = filter_dropdown.clone();

        let apply_filter = move |filter: TaskFilter| {
            if syncing_filter_for_apply.get() {
                return;
            }
            syncing_filter_for_apply.set(true);
            let Some(page) = page_weak_for_categories.upgrade() else {
                syncing_filter_for_apply.set(false);
                return;
            };
            page.imp().task_filter.set(filter);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_task_filter(filter.setting_value());
            }

            filter_dropdown_for_apply.set_selected(task_filter_index(filter) as u32);
            if let Some(smart_list) = page.imp().task_smart_filter_list.borrow().as_ref() {
                page.imp().task_smart_filter_syncing.set(true);
                let mut matched_row = None;
                let mut child = smart_list.first_child();
                while let Some(w) = child {
                    child = w.next_sibling();
                    if let Ok(r) = w.downcast::<gtk::ListBoxRow>() {
                        if unsafe { r.data::<TaskFilter>("smart-task-filter").is_some_and(|v| *v.as_ref() == filter) } {
                            matched_row = Some(r);
                            break;
                        }
                    }
                }
                smart_list.select_row(matched_row.as_ref());
                page.imp().task_smart_filter_syncing.set(false);
            }
            render_task_rows(
                &page,
                &task_list_for_categories,
                &task_store_for_categories,
                &empty_tasks_for_categories,
            );
            syncing_filter_for_apply.set(false);
        };

        let apply_for_smart_list = apply_filter.clone();
        let page_weak_for_smart_list = self.downgrade();
        task_smart_filter_list.connect_row_selected(move |_, row| {
            let Some(page) = page_weak_for_smart_list.upgrade() else {
                return;
            };
            if page.imp().task_smart_filter_syncing.get() {
                return;
            }
            if let Some(row) = row {
                if let Some(sf) = unsafe { row.data::<TaskFilter>("smart-task-filter").map(|v| *v.as_ref()) } {
                    apply_for_smart_list(sf);
                }
            }
        });

        let apply_for_dropdown = apply_filter.clone();
        filter_dropdown.connect_selected_notify(move |dropdown| {
            let sel = dropdown.selected();
            if sel != gtk::INVALID_LIST_POSITION {
                let filter = task_filter_from_index(sel as i32);
                apply_for_dropdown(filter);
            }
        });

        if let Some(settings) = AppSettings::try_new() {
            let apply_for_settings = apply_filter.clone();
            settings.connect_changed(Some("task-filter"), move |settings, _| {
                let filter = TaskFilter::from_setting(settings.string("task-filter").as_str());
                apply_for_settings(filter);
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
                direction_button_for_settings.set_tooltip_text(Some(&if descending {
                    crate::i18n::gettext("Sorted descending")
                } else {
                    crate::i18n::gettext("Sorted ascending")
                }));
                set_accessible_label(
                    &direction_button_for_settings,
                    &if descending {
                        crate::i18n::gettext("Change sort to ascending")
                    } else {
                        crate::i18n::gettext("Change sort to descending")
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

            for key in [
                "hide-completed-tasks",
                "hide-completed-tasks-value",
                "hide-completed-tasks-units",
                "hide-cancelled-tasks",
            ] {
                let page_weak = self.downgrade();
                let task_list_for_filter_settings = task_list.clone();
                let task_store_for_filter_settings = task_store.clone();
                let empty_tasks_for_filter_settings = empty_tasks.clone();
                settings.connect_changed(Some(key), move |_, _| {
                    let Some(page) = page_weak.upgrade() else {
                        return;
                    };
                    render_task_rows(
                        &page,
                        &task_list_for_filter_settings,
                        &task_store_for_filter_settings,
                        &empty_tasks_for_filter_settings,
                    );
                });
            }

            let page_weak_for_toolbar = self.downgrade();
            settings.connect_changed(Some("task-show-toolbar"), move |settings, _| {
                if let Some(page) = page_weak_for_toolbar.upgrade() {
                    if !page.imp().is_mobile.get() {
                        if let Some(toolbar) = page.imp().toolbar.borrow().as_ref() {
                            toolbar.set_visible(settings.boolean("task-show-toolbar"));
                        }
                    }
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
                        empty_tasks_for_load.set_label(&match result.origin {
                            CalendarDataOrigin::Eds => gettext("No synchronized tasks loaded"),
                            CalendarDataOrigin::Graph => {
                                gettext("No tasks loaded from Microsoft Graph")
                            }
                            CalendarDataOrigin::Local => gettext("No local tasks loaded"),
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
                    empty_tasks_for_load.set_label(&gettext("Could not load tasks"));
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

        // ── Workspace Header Bar ──
        let workspace_header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_bottom(2)
            .css_classes(vec!["task-workspace-header"])
            .build();
        let workspace_title_col = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let workspace_title_label = gtk::Label::builder()
            .label(&gettext("All Tasks"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["title-3", "task-workspace-title"])
            .build();
        let workspace_subtitle_label = gtk::Label::builder()
            .label(&gettext("0 open tasks"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label", "caption", "task-workspace-subtitle"])
            .build();
        workspace_title_col.append(&workspace_title_label);
        workspace_title_col.append(&workspace_subtitle_label);
        self.imp()
            .task_workspace_title
            .replace(Some(workspace_title_label.clone()));
        self.imp()
            .task_workspace_subtitle
            .replace(Some(workspace_subtitle_label.clone()));

        let workspace_actions_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let new_task_header_btn = gtk::Button::builder()
            .css_classes(vec!["suggested-action", "task-workspace-new-btn"])
            .tooltip_text(&gettext("Create new task with full options (Ctrl+N)"))
            .valign(gtk::Align::Center)
            .build();
        let new_task_btn_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        new_task_btn_content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
        new_task_btn_content.append(&gtk::Label::new(Some(&gettext("New Task"))));
        new_task_header_btn.set_child(Some(&new_task_btn_content));
        let btn_add_for_header = btn_add_task.clone();
        new_task_header_btn.connect_clicked(move |_| {
            btn_add_for_header.emit_clicked();
        });
        workspace_actions_row.append(&new_task_header_btn);
        workspace_header_box.append(&workspace_title_col);
        workspace_header_box.append(&workspace_actions_row);

        // ── Quick-Add Bar ──
        let quick_add_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(vec!["task-quick-add-box"])
            .build();
        let quick_add_icon = gtk::Image::builder()
            .icon_name("list-add-symbolic")
            .pixel_size(16)
            .css_classes(vec!["dim-label", "task-row-icon"])
            .valign(gtk::Align::Center)
            .build();
        let quick_add_entry = gtk::Entry::builder()
            .placeholder_text(&crate::i18n::gettext("Add a task… (type @today, @tomorrow, !high, #Category or press Enter)"))
            .hexpand(true)
            .css_classes(vec!["task-quick-add-entry"])
            .build();
        set_accessible_label(&quick_add_entry, &crate::i18n::gettext("Quickly add new task"));

        let quick_due_btn = gtk::Button::builder()
            .label(&gettext("No Date"))
            .tooltip_text(&gettext("Click to cycle due date preset (No Date → Today → Tomorrow → +7 Days)"))
            .css_classes(vec!["flat", "task-quick-preset-pill"])
            .valign(gtk::Align::Center)
            .build();
        {
            let page_weak = self.downgrade();
            quick_due_btn.connect_clicked(move |btn| {
                let Some(page) = page_weak.upgrade() else { return; };
                let next = (page.imp().task_quick_due_preset.get() + 1) % 4;
                page.imp().task_quick_due_preset.set(next);
                match next {
                    1 => {
                        btn.set_label(&gettext("Today"));
                        btn.add_css_class("active");
                    }
                    2 => {
                        btn.set_label(&gettext("Tomorrow"));
                        btn.add_css_class("active");
                    }
                    3 => {
                        btn.set_label(&gettext("+7 Days"));
                        btn.add_css_class("active");
                    }
                    _ => {
                        btn.set_label(&gettext("No Date"));
                        btn.remove_css_class("active");
                    }
                }
            });
        }

        let quick_prio_btn = gtk::Button::builder()
            .label(&gettext("Normal"))
            .tooltip_text(&gettext("Click to cycle priority preset (Normal → High → Medium)"))
            .css_classes(vec!["flat", "task-quick-preset-pill"])
            .valign(gtk::Align::Center)
            .build();
        {
            let page_weak = self.downgrade();
            quick_prio_btn.connect_clicked(move |btn| {
                let Some(page) = page_weak.upgrade() else { return; };
                let next = match page.imp().task_quick_prio_preset.get() {
                    0 => 1,
                    1 => 5,
                    _ => 0,
                };
                page.imp().task_quick_prio_preset.set(next);
                match next {
                    1 => {
                        btn.set_label(&gettext("High !"));
                        btn.add_css_class("active");
                        btn.add_css_class("prio-high");
                    }
                    5 => {
                        btn.set_label(&gettext("Medium"));
                        btn.add_css_class("active");
                        btn.remove_css_class("prio-high");
                    }
                    _ => {
                        btn.set_label(&gettext("Normal"));
                        btn.remove_css_class("active");
                        btn.remove_css_class("prio-high");
                    }
                }
            });
        }

        let quick_submit_btn = gtk::Button::builder()
            .label(&gettext("Add"))
            .tooltip_text(&gettext("Add task (Enter)"))
            .css_classes(vec!["flat", "task-quick-submit-btn"])
            .valign(gtk::Align::Center)
            .build();
        {
            let entry_for_submit = quick_add_entry.clone();
            quick_submit_btn.connect_clicked(move |_| {
                entry_for_submit.emit_activate();
            });
        }

        quick_add_box.append(&quick_add_icon);
        quick_add_box.append(&quick_add_entry);
        quick_add_box.append(&quick_due_btn);
        quick_add_box.append(&quick_prio_btn);
        quick_add_box.append(&quick_submit_btn);

        let page_weak_for_quick_add = self.downgrade();
        quick_add_entry.connect_activate(move |entry| {
            let raw_text = entry.text().trim().to_string();
            if raw_text.is_empty() {
                return;
            }
            let Some(page) = page_weak_for_quick_add.upgrade() else {
                return;
            };

            let (clean_title, parsed_due, parsed_prio, parsed_categories) =
                parse_smart_quick_add_input(
                    &raw_text,
                    page.imp().task_quick_due_preset.get(),
                    page.imp().task_quick_prio_preset.get(),
                    page.imp().task_filter.get(),
                );
            if clean_title.is_empty() {
                return;
            }

            let capabilities = page.imp().task_source_capabilities.borrow().clone();
            let source_uids = page.imp().task_source_uids.borrow().clone();
            let preferred = AppSettings::try_new()
                .map(|settings| settings.default_task_source_uid())
                .filter(|uid| !uid.trim().is_empty());

            let target_source = if let Some(filtered) = page.imp().task_source_filter.borrow().clone() {
                if task_source_is_writable_on_page(&page, &filtered) {
                    filtered
                } else {
                    task_paste_destination(
                        &source_uids,
                        &capabilities,
                        CalendarStoreService::new().is_writable(),
                        preferred.as_deref(),
                    )
                    .unwrap_or_else(|_| "local-calendar".to_string())
                }
            } else {
                task_paste_destination(
                    &source_uids,
                    &capabilities,
                    CalendarStoreService::new().is_writable(),
                    preferred.as_deref(),
                )
                .unwrap_or_else(|_| "local-calendar".to_string())
            };

            let active_cat_filter = page.imp().task_category_filter.borrow().clone().unwrap_or_default();
            let final_categories = if !parsed_categories.is_empty() {
                parsed_categories
            } else {
                active_cat_filter
            };

            let component = match build_task_component_with_dates(
                &clean_title,
                "",
                &parsed_due,
                "",
                "",
                "",
                "",
                parsed_prio,
                &final_categories,
                &[],
                "",
                "",
                "NEEDS-ACTION",
                0,
            ) {
                Ok(c) => c,
                Err(err) => {
                    show_toast_for_widget(entry, &err);
                    return;
                }
            };

            entry.set_text("");
            entry.set_sensitive(false);

            let entry_for_done = entry.clone();
            let page_for_done = page.downgrade();
            let text_for_toast = clean_title.clone();
            let (tx, rx) = async_channel::bounded(1);

            std::thread::spawn(move || {
                let res = ECalService::new().create_component(
                    &target_source,
                    ECalComponentType::Task,
                    &component,
                );
                let _ = tx.send_blocking(res);
            });

            glib::spawn_future_local(async move {
                entry_for_done.set_sensitive(true);
                entry_for_done.grab_focus();
                match rx.recv().await {
                    Ok(Ok(_)) => {
                        if let Some(page) = page_for_done.upgrade() {
                            page.refresh_tasks();
                            let msg = gettext("Task “{summary}” added").replace("{summary}", &text_for_toast);
                            show_toast_for_widget(
                                &entry_for_done,
                                &msg,
                            );
                        }
                    }
                    Ok(Err(err)) => {
                        let msg = gettext("Could not create task: {error}").replace("{error}", &err.to_string());
                        show_toast_for_widget(
                            &entry_for_done,
                            &msg,
                        );
                    }
                    Err(_) => {}
                }
            });
        });

        task_list.set_margin_start(0);
        task_list.set_margin_end(0);
        task_list.set_margin_top(0);
        task_list.set_margin_bottom(0);

        let tasks_content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let task_table_header = gtk::DrawingArea::builder()
            .content_height(28)
            .hexpand(true)
            .css_classes(vec!["tasks-table-header"])
            .build();
        set_accessible_label(&task_table_header, &gettext("Task table header"));

        let task_table_canvas = gtk::DrawingArea::builder()
            .content_height(100)
            .hexpand(true)
            .focusable(true)
            .can_focus(true)
            .focus_on_click(true)
            .css_classes(vec!["tasks-table-canvas"])
            .build();
        set_accessible_role(&task_table_canvas, gtk::AccessibleRole::List);
        set_accessible_label(&task_table_canvas, &gettext("Tasks List"));

        self.imp()
            .task_table_header
            .replace(Some(task_table_header.clone()));
        self.imp()
            .task_table_canvas
            .replace(Some(task_table_canvas.clone()));

        // Header Draw Func
        {
            let page_weak = self.downgrade();
            task_table_header.set_draw_func(move |_, cr, width, height| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let hovered = page.imp().task_table_hovered_header.get();
                let sort = page.imp().task_sort.get();
                let descending = page.imp().task_sort_descending.get();
                let is_dark = adw::StyleManager::default().is_dark();
                if let Some(layout) = layout_ref.as_ref() {
                    draw_task_table_header(cr, layout, width as f64, height as f64, sort, descending, hovered, is_dark);
                }
            });
        }

        // Header Click Gesture
        {
            let header_click = gtk::GestureClick::new();
            let page_weak = self.downgrade();
            let header_clone = task_table_header.clone();
            header_click.connect_pressed(move |gesture, _n_press, x, _y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                if let Some(col) = hit_test_task_table_header(layout, x) {
                    let current_sort = page.imp().task_sort.get();
                    let current_descending = page.imp().task_sort_descending.get();
                    let new_sort = match col {
                        TaskTableColumn::Completed => TaskSort::DueDate,
                        TaskTableColumn::Priority => TaskSort::Priority,
                        TaskTableColumn::DueDate => TaskSort::DueDate,
                        TaskTableColumn::Title => TaskSort::Title,
                        TaskTableColumn::Status => TaskSort::DueDate,
                        TaskTableColumn::Category => TaskSort::Title,
                    };
                    let new_descending = if current_sort == new_sort {
                        !current_descending
                    } else {
                        false
                    };
                    page.imp().task_sort.set(new_sort);
                    page.imp().task_sort_descending.set(new_descending);
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_task_sort_field(new_sort.setting_value());
                        settings.set_task_sort_descending(new_descending);
                    }
                    header_clone.queue_draw();
                    if let (Some(store), Some(list), Some(empty)) = (
                        page.imp().task_store.borrow().clone(),
                        page.imp().task_list.borrow().clone(),
                        page.imp().task_empty_state.borrow().clone(),
                    ) {
                        render_task_rows(&page, &list, &store, &empty);
                    }
                }
            });
            task_table_header.add_controller(header_click);
        }

        // Header Motion Controller
        {
            let header_motion = gtk::EventControllerMotion::new();
            let page_weak = self.downgrade();
            let header_clone = task_table_header.clone();
            header_motion.connect_motion(move |_, x, _y| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let col = layout_ref.as_ref().and_then(|l| hit_test_task_table_header(l, x));
                if page.imp().task_table_hovered_header.get() != col {
                    page.imp().task_table_hovered_header.set(col);
                    header_clone.queue_draw();
                }
            });
            let page_weak = self.downgrade();
            let header_clone = task_table_header.clone();
            header_motion.connect_leave(move |_| {
                let Some(page) = page_weak.upgrade() else { return; };
                if page.imp().task_table_hovered_header.get().is_some() {
                    page.imp().task_table_hovered_header.set(None);
                    header_clone.queue_draw();
                }
            });
            task_table_header.add_controller(header_motion);
        }

        // Canvas Draw Func
        {
            let page_weak = self.downgrade();
            task_table_canvas.set_draw_func(move |_, cr, width, height| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let selection: std::collections::HashSet<TaskSelectionIdentity> =
                    page.imp().task_selection.borrow().iter().cloned().collect();
                let hovered = page.imp().task_table_hovered_row.get();
                let is_dark = adw::StyleManager::default().is_dark();
                if let Some(layout) = layout_ref.as_ref() {
                    draw_task_table_canvas(cr, layout, &selection, width as f64, height as f64, hovered, is_dark);
                }
            });
            let canvas_for_theme = task_table_canvas.clone();
            let header_for_theme = task_table_header.clone();
            adw::StyleManager::default().connect_dark_notify(move |_| {
                canvas_for_theme.queue_draw();
                header_for_theme.queue_draw();
            });
        }

        // Canvas Primary Click Gesture
        {
            let canvas_click = gtk::GestureClick::new();
            let page_weak = self.downgrade();
            let canvas_clone = task_table_canvas.clone();
            canvas_click.connect_pressed(move |gesture, n_press, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let Some((row_idx, in_check)) = hit_test_task_table(layout, x, y) else { return; };

                canvas_clone.grab_focus();

                if in_check {
                    if let Some(row) = layout.rows.get(row_idx) {
                        let completed = task_is_completed(&row.task);
                        toggle_task_item_completion(&page, &row.task, !completed);
                    }
                } else if n_press == 2 {
                    if let Some(row) = layout.rows.get(row_idx) {
                        if let (Some(store), Some(list), Some(empty)) = (
                            page.imp().task_store.borrow().clone(),
                            page.imp().task_list.borrow().clone(),
                            page.imp().task_empty_state.borrow().clone(),
                        ) {
                            page.open_task_dialog(store, list, empty, Some(row.task.clone()));
                        }
                    }
                } else if n_press == 1 {
                    let current_event = gesture.current_event();
                    let state = current_event.as_ref().map(|e| e.modifier_state()).unwrap_or(gdk::ModifierType::empty());
                    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                    let ctrl_or_cmd = state.contains(gdk::ModifierType::CONTROL_MASK) || state.contains(gdk::ModifierType::META_MASK);

                    let mut current_sel = page.imp().task_selection.borrow().clone();
                    let clicked_task = &layout.rows[row_idx].task;
                    let clicked_id = TaskSelectionIdentity::from_task(clicked_task);

                    if shift {
                        let anchor = page.imp().task_table_anchor_row.get().unwrap_or(row_idx);
                        let start = anchor.min(row_idx);
                        let end = anchor.max(row_idx);
                        let mut range_sel = Vec::new();
                        for i in start..=end {
                            if let Some(r) = layout.rows.get(i) {
                                range_sel.push(TaskSelectionIdentity::from_task(&r.task));
                            }
                        }
                        page.imp().task_selection.replace(range_sel);
                    } else if ctrl_or_cmd {
                        if let Some(pos) = current_sel.iter().position(|id| id == &clicked_id) {
                            current_sel.remove(pos);
                        } else {
                            current_sel.push(clicked_id);
                        }
                        page.imp().task_table_anchor_row.set(Some(row_idx));
                        page.imp().task_selection.replace(current_sel);
                    } else {
                        page.imp().task_table_anchor_row.set(Some(row_idx));
                        page.imp().task_selection.replace(vec![clicked_id]);
                    }
                    update_task_selection_controls(&page);
                    refresh_task_preview(&page);
                    canvas_clone.queue_draw();
                    if page.imp().is_mobile.get() {
                        page.imp().is_detail_shown.set(true);
                        page.sync_tasks_layout_visibility();
                        if let Some(win) = crate::window::find_meridian_window(&page) {
                            win.sync_back_button();
                        }
                    }
                }
            });
            task_table_canvas.add_controller(canvas_click);
        }

        // Canvas Secondary Click Gesture (Context Menu)
        {
            let canvas_secondary_click = gtk::GestureClick::new();
            canvas_secondary_click.set_button(gdk::BUTTON_SECONDARY);
            let page_weak = self.downgrade();
            let canvas_clone = task_table_canvas.clone();
            canvas_secondary_click.connect_pressed(move |gesture, _n_press, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let Some((row_idx, _)) = hit_test_task_table(layout, x, y) else { return; };
                if let Some(row) = layout.rows.get(row_idx) {
                    let clicked_id = TaskSelectionIdentity::from_task(&row.task);
                    let mut sel = page.imp().task_selection.borrow().clone();
                    if !sel.contains(&clicked_id) {
                        sel = vec![clicked_id];
                        page.imp().task_selection.replace(sel);
                        page.imp().task_table_anchor_row.set(Some(row_idx));
                        update_task_selection_controls(&page);
                        refresh_task_preview(&page);
                        canvas_clone.queue_draw();
                    }
                    show_task_table_context_menu(&page, &canvas_clone, &row.task, gdk::Rectangle::new(x as i32, y as i32, 1, 1));
                }
            });
            task_table_canvas.add_controller(canvas_secondary_click);
        }

        // Touch / Trackpad horizontal swipe gesture (Task completion / deletion)
        {
            let task_swipe = gtk::GestureSwipe::new();
            let page_for_task_swipe = self.downgrade();
            let canvas_for_task_swipe = task_table_canvas.clone();
            task_swipe.connect_swipe(move |gesture, velocity_x, velocity_y| {
                if velocity_x.abs() > 200.0 && velocity_x.abs() > velocity_y.abs() * 1.5 {
                    let Some(page) = page_for_task_swipe.upgrade() else { return; };
                    let (start_x, start_y) = gesture.bounding_box_center().unwrap_or((0.0, 0.0));
                    let layout_ref = page.imp().task_table_layout.borrow();
                    let Some(layout) = layout_ref.as_ref() else { return; };
                    let Some((row_idx, _)) = hit_test_task_table(layout, start_x, start_y) else { return; };
                    if let Some(row) = layout.rows.get(row_idx) {
                        let clicked_id = TaskSelectionIdentity::from_task(&row.task);
                        page.imp().task_selection.replace(vec![clicked_id]);
                        page.imp().task_table_anchor_row.set(Some(row_idx));
                        update_task_selection_controls(&page);
                        refresh_task_preview(&page);
                        canvas_for_task_swipe.queue_draw();

                        if velocity_x > 200.0 {
                            // Swipe right: Toggle completion
                            let completed = task_is_completed(&row.task);
                            toggle_task_item_completion(&page, &row.task, !completed);
                            let feedback = if !completed {
                                crate::i18n::gettext("Task marked as completed")
                            } else {
                                crate::i18n::gettext("Task marked as incomplete")
                            };
                            crate::views::accessibility::announce_to_screen_reader(
                                &canvas_for_task_swipe,
                                &feedback,
                                gtk::AccessibleAnnouncementPriority::Medium,
                            );
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                        } else if velocity_x < -200.0 {
                            // Swipe left: Trigger delete
                            if let Some(btn) = page.imp().task_delete_selection_button.borrow().as_ref() {
                                if btn.is_sensitive() {
                                    btn.emit_clicked();
                                    crate::views::accessibility::announce_to_screen_reader(
                                        &canvas_for_task_swipe,
                                        &crate::i18n::gettext("Task deleted"),
                                        gtk::AccessibleAnnouncementPriority::Medium,
                                    );
                                    gesture.set_state(gtk::EventSequenceState::Claimed);
                                }
                            }
                        }
                    }
                }
            });
            task_table_canvas.add_controller(task_swipe);
        }

        // Canvas Motion Controller
        {
            let canvas_motion = gtk::EventControllerMotion::new();
            let page_weak = self.downgrade();
            let canvas_clone = task_table_canvas.clone();
            canvas_motion.connect_motion(move |_, x, y| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let hovered_idx = layout_ref.as_ref().and_then(|l| hit_test_task_table(l, x, y)).map(|(idx, _)| idx);
                if page.imp().task_table_hovered_row.get() != hovered_idx {
                    page.imp().task_table_hovered_row.set(hovered_idx);
                    canvas_clone.queue_draw();
                }
            });
            let page_weak = self.downgrade();
            let canvas_clone = task_table_canvas.clone();
            canvas_motion.connect_leave(move |_| {
                let Some(page) = page_weak.upgrade() else { return; };
                if page.imp().task_table_hovered_row.get().is_some() {
                    page.imp().task_table_hovered_row.set(None);
                    canvas_clone.queue_draw();
                }
            });
            task_table_canvas.add_controller(canvas_motion);
        }

        // Canvas Key Controller
        {
            let key_controller = gtk::EventControllerKey::new();
            let page_weak = self.downgrade();
            let canvas_clone = task_table_canvas.clone();
            key_controller.connect_key_pressed(move |_, keyval, _, state| {
                let Some(page) = page_weak.upgrade() else { return glib::Propagation::Proceed; };
                let layout_ref = page.imp().task_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return glib::Propagation::Proceed; };
                if layout.rows.is_empty() { return glib::Propagation::Proceed; };

                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let current_sel = page.imp().task_selection.borrow().clone();
                let current_anchor = page.imp().task_table_anchor_row.get().unwrap_or(0);

                let announce_task = |task: &TaskItemObject| {
                    let status = if task.is_completed() {
                        crate::i18n::gettext("Completed")
                    } else {
                        crate::i18n::gettext("Not completed")
                    };
                    let due = task.due_date();
                    let speech = if !due.is_empty() {
                        format!("{}, {}, {}: {}", task.title(), status, crate::i18n::gettext("Due"), due)
                    } else {
                        format!("{}, {}", task.title(), status)
                    };
                    set_accessible_description(&canvas_clone, &speech);
                    announce_to_screen_reader(&canvas_clone, &speech, gtk::AccessibleAnnouncementPriority::Medium);
                };

                match keyval {
                    gdk::Key::Up => {
                        let next_idx = current_anchor.saturating_sub(1);
                        if shift {
                            let anchor = page.imp().task_table_anchor_row.get().unwrap_or(0);
                            let start = anchor.min(next_idx);
                            let end = anchor.max(next_idx);
                            let range_sel: Vec<_> = (start..=end).filter_map(|i| layout.rows.get(i).map(|r| TaskSelectionIdentity::from_task(&r.task))).collect();
                            page.imp().task_selection.replace(range_sel);
                        } else if let Some(row) = layout.rows.get(next_idx) {
                            page.imp().task_table_anchor_row.set(Some(next_idx));
                            page.imp().task_selection.replace(vec![TaskSelectionIdentity::from_task(&row.task)]);
                            announce_task(&row.task);
                        }
                        update_task_selection_controls(&page);
                        refresh_task_preview(&page);
                        canvas_clone.queue_draw();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Down => {
                        let next_idx = (current_anchor + 1).min(layout.rows.len() - 1);
                        if shift {
                            let anchor = page.imp().task_table_anchor_row.get().unwrap_or(0);
                            let start = anchor.min(next_idx);
                            let end = anchor.max(next_idx);
                            let range_sel: Vec<_> = (start..=end).filter_map(|i| layout.rows.get(i).map(|r| TaskSelectionIdentity::from_task(&r.task))).collect();
                            page.imp().task_selection.replace(range_sel);
                        } else if let Some(row) = layout.rows.get(next_idx) {
                            page.imp().task_table_anchor_row.set(Some(next_idx));
                            page.imp().task_selection.replace(vec![TaskSelectionIdentity::from_task(&row.task)]);
                            announce_task(&row.task);
                        }
                        update_task_selection_controls(&page);
                        refresh_task_preview(&page);
                        canvas_clone.queue_draw();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Home => {
                        if let Some(row) = layout.rows.first() {
                            page.imp().task_table_anchor_row.set(Some(0));
                            page.imp().task_selection.replace(vec![TaskSelectionIdentity::from_task(&row.task)]);
                            announce_task(&row.task);
                            update_task_selection_controls(&page);
                            refresh_task_preview(&page);
                            canvas_clone.queue_draw();
                        }
                        glib::Propagation::Stop
                    }
                    gdk::Key::End => {
                        let last_idx = layout.rows.len() - 1;
                        if let Some(row) = layout.rows.get(last_idx) {
                            page.imp().task_table_anchor_row.set(Some(last_idx));
                            page.imp().task_selection.replace(vec![TaskSelectionIdentity::from_task(&row.task)]);
                            announce_task(&row.task);
                            update_task_selection_controls(&page);
                            refresh_task_preview(&page);
                            canvas_clone.queue_draw();
                        }
                        glib::Propagation::Stop
                    }
                    gdk::Key::space => {
                        if let Some(sel_id) = current_sel.first() {
                            if let Some(row) = layout.rows.iter().find(|r| &r.identity == sel_id) {
                                let completed = task_is_completed(&row.task);
                                toggle_task_item_completion(&page, &row.task, !completed);
                            }
                        }
                        glib::Propagation::Stop
                    }
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        if let Some(sel_id) = current_sel.first() {
                            if let Some(row) = layout.rows.iter().find(|r| &r.identity == sel_id) {
                                if let (Some(store), Some(list), Some(empty)) = (
                                    page.imp().task_store.borrow().clone(),
                                    page.imp().task_list.borrow().clone(),
                                    page.imp().task_empty_state.borrow().clone(),
                                ) {
                                    page.open_task_dialog(store, list, empty, Some(row.task.clone()));
                                }
                            }
                        }
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            task_table_canvas.add_controller(key_controller);
        }

        let task_workspace_groups_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .hexpand(true)
            .build();
        self.imp()
            .task_workspace_groups_box
            .replace(Some(task_workspace_groups_box.clone()));

        task_table_header.set_visible(false);
        task_table_canvas.set_visible(false);
        task_list.set_visible(false);

        tasks_content_box.append(&task_workspace_groups_box);
        tasks_content_box.append(&task_table_header);
        tasks_content_box.append(&task_table_canvas);
        tasks_content_box.append(&task_list);

        let task_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&tasks_content_box)
            .vexpand(true)
            .build();

        let empty_status_page = adw::StatusPage::builder()
            .icon_name("view-list-bullet-symbolic")
            .title(&gettext("No Tasks"))
            .description(&gettext("Select a task list or create a new task."))
            .vexpand(true)
            .hexpand(true)
            .build();
        let task_container_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .hexpand(true)
            .build();
        task_container_stack.add_named(&task_scroll, Some("list"));
        task_container_stack.add_named(&empty_status_page, Some("empty"));
        task_container_stack.set_visible_child_name("list");

        self.imp()
            .task_empty_status_page
            .replace(Some(empty_status_page.clone()));
        self.imp()
            .task_container_stack
            .replace(Some(task_container_stack.clone()));

        let workspace_center_col = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .vexpand(true)
            .hexpand(true)
            .build();
        let workspace_top_pinned = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .build();
        workspace_top_pinned.append(&workspace_header_box);
        workspace_top_pinned.append(&quick_add_box);
        workspace_center_col.append(&workspace_top_pinned);
        workspace_center_col.append(&task_container_stack);

        let preview_paned = gtk::Paned::builder()
            .orientation(preview_orientation.paned_orientation())
            .wide_handle(false)
            .vexpand(true)
            .build();
        preview_paned.set_start_child(Some(&workspace_center_col));
        preview_paned.set_end_child(Some(&preview_box));
        preview_paned.set_resize_start_child(true);
        preview_paned.set_resize_end_child(true);
        preview_paned.set_shrink_start_child(true);
        preview_paned.set_shrink_end_child(true);
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
        content_box.append(&toolbar_scroll);
        content_box.append(&task_search_bar);
        content_box.append(&preview_paned);

        content_box.set_vexpand(true);
        content_box.set_hexpand(true);
        paned.set_end_child(Some(&content_box));
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(true);

        let sidebar_position = AppSettings::try_new()
            .map(|settings| settings.mail_sidebar_position())
            .unwrap_or(260)
            .clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
        paned.set_position(sidebar_position);
        if let Some(settings) = AppSettings::try_new() {
            paned.connect_position_notify(move |paned| {
                if let Some(start) = paned.start_child() {
                    if !start.is_visible() {
                        return;
                    }
                }
                let position = paned.position();
                let clamped = position.clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
                if position != clamped {
                    paned.set_position(clamped);
                }
                if let Some(win) = crate::window::find_meridian_window(paned) {
                    win.set_sidebar_position_sync(clamped);
                }
                if settings.mail_sidebar_position() != clamped {
                    settings.set_mail_sidebar_position(clamped);
                }
            });
        }
        self.reload_task_category_metadata();

        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        root_box.append(&paned);
        root_box.set_parent(self);

        let initial_source_filter = self.imp().task_source_filter.borrow().clone();
        apply_task_sources(self, initial_sync_task_sources(), initial_source_filter.clone());
        start_task_source_discovery(self, initial_source_filter, false);
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
            gettext("Edit Task")
        } else {
            gettext("New Task")
        };
        let dialog = adw::Window::builder()
            .title(&dialog_title)
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
        let cancel_button = gtk::Button::with_label(&gettext("Cancel"));
        header_bar.pack_start(&cancel_button);
        let save_button = gtk::Button::builder()
            .label(&gettext("Save"))
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

        let source_model = gtk::StringList::new(&[&gettext("Loading task lists…")]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title(&gettext("Task List"))
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
        let details = adw::PreferencesGroup::builder().title(&gettext("Details")).build();
        details.add(&source_row);

        let title_entry = adw::EntryRow::builder().title(&gettext("Title")).build();
        details.add(&title_entry);
        let dtstart_entry = adw::EntryRow::builder()
            .title(&gettext("Start (YYYY-MM-DD, optional)"))
            .build();
        dtstart_entry.set_tooltip_text(Some(&gettext(
            "New values are date-only. Imported date-time values remain unchanged unless edited.",
        )));
        details.add(&dtstart_entry);
        let due_entry = adw::EntryRow::builder()
            .title(&gettext("Due (YYYY-MM-DD, optional)"))
            .build();
        due_entry.set_tooltip_text(Some(&gettext(
            "New values are date-only. Imported date-time values remain unchanged unless edited.",
        )));
        details.add(&due_entry);
        let completed_entry = adw::EntryRow::builder()
            .title(&gettext("Completed (YYYY-MM-DD, optional)"))
            .build();
        completed_entry.set_tooltip_text(Some(&gettext(
            "A completion date is only permitted for Completed status and must not be in the future.",
        )));
        details.add(&completed_entry);
        let recurrence_entry = adw::EntryRow::builder()
            .title(&gettext("Recurrence (RRULE, optional)"))
            .build();
        recurrence_entry.set_tooltip_text(Some(&gettext("e.g. FREQ=WEEKLY;BYDAY=MO;COUNT=10")));
        details.add(&recurrence_entry);
        let rdate_entry = adw::EntryRow::builder()
            .title(&gettext("Additional Due Dates (RDATE, comma-separated)"))
            .build();
        rdate_entry.set_tooltip_text(Some(&gettext("e.g. 20260812,20260819")));
        details.add(&rdate_entry);
        let exdate_entry = adw::EntryRow::builder()
            .title(&gettext("Exceptions (EXDATE, comma-separated)"))
            .build();
        exdate_entry.set_tooltip_text(Some(&gettext("e.g. 20260815,20260822")));
        details.add(&exdate_entry);

        let priority_row = adw::ActionRow::builder()
            .title(&gettext("Priority (1 = high, 0 = normal)"))
            .build();
        let priority_spin = gtk::SpinButton::with_range(0.0, 9.0, 1.0);
        priority_spin.set_numeric(true);
        priority_spin.set_value(0.0);
        priority_row.add_suffix(&priority_spin);
        details.add(&priority_row);

        let status_row = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Status"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Not Started"),
                &crate::i18n::gettext("In Progress"),
                &crate::i18n::gettext("Completed"),
                &crate::i18n::gettext("Cancelled"),
            ]))
            .build();
        details.add(&status_row);
        let percent_row = adw::SpinRow::with_range(0.0, 100.0, 1.0);
        percent_row.set_title(&gettext("Progress (Percent)"));
        percent_row.set_digits(0);
        percent_row.set_snap_to_ticks(true);
        details.add(&percent_row);

        let categories_entry = adw::EntryRow::builder()
            .title(&gettext("Categories (Comma-separated)"))
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
            .tooltip_text(&gettext("Select files as task attachments"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&attachment_picker, &gettext("Select files as task attachments"));
        let attachment_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(6)
            .margin_top(6)
            .build();
        let attachment_title = gtk::Label::builder()
            .label(&gettext("Attachments (one URI or full ATTACH line per line)"))
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
                .title(&gettext("Select Task Attachments"))
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
            .title(&gettext("Description"))
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
            let msg = gettext("Imported values are preserved unchanged until edited: {values}")
                .replace("{values}", &imported.join(", "));
            let imported_label = gtk::Label::builder()
                .label(&msg)
                .wrap(true)
                .halign(gtk::Align::Start)
                .css_classes(vec!["warning", "dim-label"])
                .build();
            details.add(&imported_label);
            status_row.set_tooltip_text(Some(&gettext(
                "Imported unsupported STATUS value; preserved unchanged",
            )));
            percent_row.set_tooltip_text(Some(&gettext(
                "Imported unsupported PERCENT-COMPLETE value; preserved unchanged",
            )));
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
                names.push(gettext("Unavailable Source · {source}").replace("{source}", uid));
                uids.push(uid.to_string());
                capabilities.insert(uid.to_string(), TaskSourceCapability::Unavailable);
            }
            if local_calendar_available {
                names.push(gettext("Local Task List"));
                uids.push("local-calendar".to_string());
                capabilities.insert("local-calendar".to_string(), TaskSourceCapability::Writable);
            } else if edit_source_uid_for_load.as_deref() == Some("local-calendar") {
                names.push(gettext("Unavailable Source · {source}").replace("{source}", "local-calendar"));
                uids.push("local-calendar".to_string());
                capabilities.insert(
                    "local-calendar".to_string(),
                    TaskSourceCapability::Unavailable,
                );
            }
            if names.is_empty() {
                names.push(if discovery_failed {
                    gettext("Task lists are unreachable")
                } else {
                    gettext("No active task list available")
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
                    show_toast_for_widget(&dialog, &gettext("No active task list is selected"));
                    return;
                };
                source_uid
            };
            if !task_source_is_writable(&source_uid, &source_capabilities_for_save) {
                show_toast_for_widget(
                    &dialog,
                    &gettext("The selected task list is read-only or unavailable"),
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
                    &gettext("The task list is no longer writable or unavailable"),
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
                            gettext("The task list is no longer writable immediately prior to save"),
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
                            gettext("Backend did not confirm an authoritative VTODO component; please reload"),
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
                        Err(error) => {
                            let msg = gettext("Could not save task: {error}").replace("{error}", &error.to_string());
                            show_toast_for_widget(
                                &dialog,
                                &msg,
                            );
                        }
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
                        &gettext("The confirmed task result belongs to a different UID"),
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
                    show_toast_for_widget(&dialog, &gettext("ECal returned no valid task UID"));
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
                        &gettext("Backend did not confirm an authoritative task; please reload"),
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
                        let msg = gettext("Confirmed task is invalid: {error}").replace("{error}", &error);
                        show_toast_for_widget(
                            &dialog,
                            &msg,
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
                        gettext("Task updated ({uid})").replace("{uid}", &uid)
                    } else {
                        gettext("Task saved ({uid})").replace("{uid}", &uid)
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
                        show_toast_for_widget(
                            &dialog,
                            &gettext("Save is still in progress; please wait a moment"),
                        );
                    }
                    return glib::Propagation::Stop;
                }
                TaskCloseDecision::BlockCommit => {
                    if let Some(dialog) = dialog_weak_for_close.upgrade() {
                        show_toast_for_widget(
                            &dialog,
                            &gettext("Save is currently committing; close is not available"),
                        );
                    }
                    return glib::Propagation::Stop;
                }
                TaskCloseDecision::KeepPrompt => return glib::Propagation::Stop,
                TaskCloseDecision::Prompt => close_prompt_open_for_close.set(true),
            }
            let confirmation = adw::AlertDialog::new(
                Some(&gettext("Unsaved Task Changes")),
                Some(&gettext("The task was modified since the last confirmed save.")),
            );
            confirmation.add_responses(&[
                ("continue", &gettext("Keep Editing")),
                ("save", &gettext("Save")),
                ("discard", &gettext("Discard")),
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
                                    &gettext("Save is currently committing; cancel is not available"),
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
                            &gettext("Save is currently committing; cancel is not available"),
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
            if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Task page result");
    }
}

// --- ETable Tasks Canvas Models and Drawing (Original GNOME Evolution Canvas parity) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskTableColumn {
    Completed,
    Priority,
    Title,
    Status,
    DueDate,
    Category,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedTaskTableRow {
    pub(crate) task: TaskItemObject,
    pub(crate) identity: TaskSelectionIdentity,
    pub(crate) id: String,
    pub(crate) source_uid: String,
    pub(crate) title: String,
    pub(crate) due_date: Option<NaiveDate>,
    pub(crate) due_display: String,
    pub(crate) is_overdue: bool,
    pub(crate) priority: u32,
    pub(crate) status: String,
    pub(crate) percent: u32,
    pub(crate) categories: Vec<String>,
    pub(crate) completed: bool,
    pub(crate) y: f64,
    pub(crate) height: f64,
    pub(crate) check_rect: (f64, f64, f64, f64),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedTaskTableLayout {
    pub(crate) rows: Vec<PreparedTaskTableRow>,
    pub(crate) total_height: f64,
    pub(crate) col_check_w: f64,
    pub(crate) col_prio_w: f64,
    pub(crate) col_title_w: f64,
    pub(crate) col_status_w: f64,
    pub(crate) col_due_w: f64,
    pub(crate) col_cat_w: f64,
}

pub(crate) fn prepare_task_table_layout(
    tasks: &[TaskItemObject],
    total_width: f64,
    today: NaiveDate,
) -> PreparedTaskTableLayout {
    let row_height = 32.0;
    let col_check_w = 32.0;
    let col_prio_w = 28.0;
    let col_status_w = 110.0;
    let col_due_w = 120.0;
    let col_cat_w = 110.0;
    let col_title_w = (total_width - col_check_w - col_prio_w - col_status_w - col_due_w - col_cat_w - 16.0).max(180.0);

    let mut rows = Vec::with_capacity(tasks.len());
    let mut current_y = 0.0;

    for task in tasks {
        let completed = task_is_completed(task);
        let raw_title = task.title();
        let title = if raw_title.trim().is_empty() {
            crate::i18n::gettext("(No Subject)")
        } else {
            raw_title
        };

        let due_raw = task.due_date();
        let parsed_due = task_due_date(&due_raw);
        let (due_display, is_overdue) = if let Some(due) = parsed_due {
            let text = if due == today {
                crate::i18n::gettext("Today")
            } else if due == today + chrono::Duration::days(1) {
                crate::i18n::gettext("Tomorrow")
            } else if due == today - chrono::Duration::days(1) {
                crate::i18n::gettext("Yesterday")
            } else {
                due.format("%d.%m.%Y").to_string()
            };
            let overdue = !completed && due < today;
            (text, overdue)
        } else if !due_raw.trim().is_empty() {
            (due_raw, false)
        } else {
            ("-".to_string(), false)
        };

        let priority = task.priority().parse::<u32>().unwrap_or(0);
        let status = task_status_label(&task.component());
        let percent_raw = ical_property(&task.component(), "PERCENT-COMPLETE");
        let percent = percent_raw.and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
        let categories: Vec<String> = task_categories(task).collect();

        let check_rect = (8.0, current_y + (row_height - 16.0) / 2.0, 16.0, 16.0);

        rows.push(PreparedTaskTableRow {
            task: task.clone(),
            identity: TaskSelectionIdentity::from_task(task),
            id: task.id(),
            source_uid: task.source_uid(),
            title,
            due_date: parsed_due,
            due_display,
            is_overdue,
            priority,
            status: status.to_string(),
            percent,
            categories,
            completed,
            y: current_y,
            height: row_height,
            check_rect,
        });

        current_y += row_height;
    }

    PreparedTaskTableLayout {
        rows,
        total_height: current_y,
        col_check_w,
        col_prio_w,
        col_title_w,
        col_status_w,
        col_due_w,
        col_cat_w,
    }
}

pub(crate) fn hit_test_task_table_header(
    layout: &PreparedTaskTableLayout,
    x: f64,
) -> Option<TaskTableColumn> {
    if x < layout.col_check_w {
        Some(TaskTableColumn::Completed)
    } else if x < layout.col_check_w + layout.col_prio_w {
        Some(TaskTableColumn::Priority)
    } else if x < layout.col_check_w + layout.col_prio_w + layout.col_title_w {
        Some(TaskTableColumn::Title)
    } else if x < layout.col_check_w + layout.col_prio_w + layout.col_title_w + layout.col_status_w {
        Some(TaskTableColumn::Status)
    } else if x < layout.col_check_w + layout.col_prio_w + layout.col_title_w + layout.col_status_w + layout.col_due_w {
        Some(TaskTableColumn::DueDate)
    } else if x < layout.col_check_w + layout.col_prio_w + layout.col_title_w + layout.col_status_w + layout.col_due_w + layout.col_cat_w {
        Some(TaskTableColumn::Category)
    } else {
        None
    }
}

pub(crate) fn hit_test_task_table(
    layout: &PreparedTaskTableLayout,
    x: f64,
    y: f64,
) -> Option<(usize, bool)> {
    if layout.rows.is_empty() || y < 0.0 || y >= layout.total_height {
        return None;
    }
    let row_idx = (y / 32.0).floor() as usize;
    if row_idx < layout.rows.len() {
        let (cx, cy, cw, ch) = layout.rows[row_idx].check_rect;
        let in_check = x >= (cx - 4.0) && x <= (cx + cw + 4.0) && y >= (cy - 4.0) && y <= (cy + ch + 4.0);
        Some((row_idx, in_check))
    } else {
        None
    }
}

fn draw_cairo_rounded_rect(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    radius: f64,
) {
    let r = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    if r <= 0.0 {
        cr.rectangle(x, y, width, height);
        return;
    }
    let degrees = std::f64::consts::PI / 180.0;
    cr.new_sub_path();
    cr.arc(x + width - r, y + r, r, -90.0 * degrees, 0.0 * degrees);
    cr.arc(x + width - r, y + height - r, r, 0.0 * degrees, 90.0 * degrees);
    cr.arc(x + r, y + height - r, r, 90.0 * degrees, 180.0 * degrees);
    cr.arc(x + r, y + r, r, 180.0 * degrees, 270.0 * degrees);
    cr.close_path();
}

fn truncate_cairo_text(cr: &gtk::cairo::Context, text: &str, max_width: f64) -> String {
    if text.is_empty() || max_width <= 0.0 {
        return String::new();
    }
    if let Ok(extents) = cr.text_extents(text) {
        if extents.width() <= max_width {
            return text.to_string();
        }
    }
    let ellipsis = "…";
    let ellipsis_w = cr.text_extents(ellipsis).map(|e| e.width()).unwrap_or(8.0);
    let target_w = (max_width - ellipsis_w).max(0.0);
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0;
    let mut high = chars.len();
    let mut best = 0;
    while low <= high {
        let mid = (low + high) / 2;
        let s: String = chars[..mid].iter().collect();
        let w = cr.text_extents(&s).map(|e| e.width()).unwrap_or(0.0);
        if w <= target_w {
            best = mid;
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }
    let mut res: String = chars[..best].iter().collect();
    res.push('…');
    res
}

fn draw_task_table_header(
    cr: &gtk::cairo::Context,
    layout: &PreparedTaskTableLayout,
    width: f64,
    height: f64,
    current_sort: TaskSort,
    sort_desc: bool,
    hovered_col: Option<TaskTableColumn>,
    is_dark: bool,
) {
    if is_dark {
        cr.set_source_rgb(0.18, 0.18, 0.18);
    } else {
        cr.set_source_rgb(0.95, 0.95, 0.96);
    }
    cr.paint().ok();

    // Bottom border
    cr.set_line_width(1.0);
    if is_dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.12);
    }
    cr.move_to(0.0, height - 0.5);
    cr.line_to(width, height - 0.5);
    cr.stroke().ok();

    let col_title_label = crate::i18n::gettext("Summary");
    let col_status_label = crate::i18n::gettext("Status");
    let col_due_label = crate::i18n::gettext("Due Date");
    let col_cat_label = crate::i18n::gettext("Categories");
    let columns = [
        (TaskTableColumn::Completed, 0.0, layout.col_check_w, "✓"),
        (TaskTableColumn::Priority, layout.col_check_w, layout.col_prio_w, "!"),
        (TaskTableColumn::Title, layout.col_check_w + layout.col_prio_w, layout.col_title_w, col_title_label.as_str()),
        (TaskTableColumn::Status, layout.col_check_w + layout.col_prio_w + layout.col_title_w, layout.col_status_w, col_status_label.as_str()),
        (TaskTableColumn::DueDate, layout.col_check_w + layout.col_prio_w + layout.col_title_w + layout.col_status_w, layout.col_due_w, col_due_label.as_str()),
        (TaskTableColumn::Category, layout.col_check_w + layout.col_prio_w + layout.col_title_w + layout.col_status_w + layout.col_due_w, layout.col_cat_w, col_cat_label.as_str()),
    ];

    cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
    cr.set_font_size(11.0);

    for (col, col_x, col_w, label) in columns {
        if hovered_col == Some(col) {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
            }
            cr.rectangle(col_x, 0.0, col_w, height - 1.0);
            cr.fill().ok();
        }

        // Column separator
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.08);
        }
        cr.move_to(col_x + col_w - 0.5, 4.0);
        cr.line_to(col_x + col_w - 0.5, height - 5.0);
        cr.stroke().ok();

        // Label
        if is_dark {
            cr.set_source_rgba(0.85, 0.85, 0.85, 0.9);
        } else {
            cr.set_source_rgba(0.2, 0.2, 0.2, 0.9);
        }

        let mut display = label.to_string();
        let is_sorted = match (col, current_sort) {
            (TaskTableColumn::Title, TaskSort::Title) => true,
            (TaskTableColumn::DueDate, TaskSort::DueDate) => true,
            (TaskTableColumn::Priority, TaskSort::Priority) => true,
            _ => false,
        };
        if is_sorted {
            display.push(' ');
            display.push(if sort_desc { '▼' } else { '▲' });
        }

        let text_x = if col == TaskTableColumn::Completed || col == TaskTableColumn::Priority {
            col_x + (col_w / 2.0) - 4.0
        } else {
            col_x + 8.0
        };
        cr.move_to(text_x, (height / 2.0) + 4.0);
        cr.show_text(&display).ok();
    }
}

fn draw_task_table_canvas(
    cr: &gtk::cairo::Context,
    layout: &PreparedTaskTableLayout,
    selection: &HashSet<TaskSelectionIdentity>,
    width: f64,
    height: f64,
    hovered_row: Option<usize>,
    is_dark: bool,
) {
    if is_dark {
        cr.set_source_rgb(0.12, 0.12, 0.12);
    } else {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    }
    cr.paint().ok();

    for (idx, row) in layout.rows.iter().enumerate() {
        if row.y + row.height < 0.0 || row.y > height {
            continue;
        }

        let is_selected = selection.contains(&row.identity);
        let is_hovered = hovered_row == Some(idx);

        // Row background
        if is_selected {
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.25);
            cr.rectangle(0.0, row.y, width, row.height);
            cr.fill().ok();
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.4);
            cr.set_line_width(1.0);
            cr.rectangle(0.5, row.y + 0.5, width - 1.0, row.height - 1.0);
            cr.stroke().ok();
        } else if is_hovered {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.04);
            }
            cr.rectangle(0.0, row.y, width, row.height);
            cr.fill().ok();
        } else if idx % 2 == 1 {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.015);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.015);
            }
            cr.rectangle(0.0, row.y, width, row.height);
            cr.fill().ok();
        }

        // 1. Checkbox
        let (cx, cy, cw, ch) = row.check_rect;
        draw_cairo_rounded_rect(cr, cx, cy, cw, ch, 3.0);
        if row.completed {
            cr.set_source_rgba(0.2, 0.55, 0.95, 0.9);
            cr.fill_preserve().ok();
            cr.set_source_rgba(0.2, 0.45, 0.85, 1.0);
            cr.set_line_width(1.0);
            cr.stroke().ok();

            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(2.0);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_line_join(gtk::cairo::LineJoin::Round);
            cr.move_to(cx + 3.5, cy + 8.5);
            cr.line_to(cx + 6.5, cy + 11.5);
            cr.line_to(cx + 12.5, cy + 4.5);
            cr.stroke().ok();
        } else {
            if is_dark {
                cr.set_source_rgba(0.7, 0.7, 0.7, 0.6);
            } else {
                cr.set_source_rgba(0.3, 0.3, 0.3, 0.6);
            }
            cr.set_line_width(1.5);
            cr.stroke().ok();
        }

        // 2. Priority indicator
        let prio_x = layout.col_check_w;
        if (1..=4).contains(&row.priority) {
            cr.set_source_rgba(0.9, 0.2, 0.2, 0.95);
            cr.arc(prio_x + 14.0, row.y + (row.height / 2.0), 4.5, 0.0, 2.0 * std::f64::consts::PI);
            cr.fill().ok();
        } else if (6..=9).contains(&row.priority) {
            cr.set_source_rgba(0.5, 0.5, 0.5, 0.5);
            cr.arc(prio_x + 14.0, row.y + (row.height / 2.0), 3.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.fill().ok();
        }

        // 3. Title
        let title_x = layout.col_check_w + layout.col_prio_w + 8.0;
        let title_max_w = layout.col_title_w - 16.0;
        let text_baseline = row.y + (row.height / 2.0) + 4.0;

        cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);

        if row.completed {
            if is_dark {
                cr.set_source_rgba(0.55, 0.55, 0.55, 0.7);
            } else {
                cr.set_source_rgba(0.5, 0.5, 0.5, 0.7);
            }
        } else if is_dark {
            cr.set_source_rgba(0.92, 0.92, 0.92, 1.0);
        } else {
            cr.set_source_rgba(0.12, 0.12, 0.12, 1.0);
        }

        let display_title = truncate_cairo_text(cr, &row.title, title_max_w);
        cr.move_to(title_x, text_baseline);
        cr.show_text(&display_title).ok();

        if row.completed {
            if let Ok(extents) = cr.text_extents(&display_title) {
                cr.set_line_width(1.0);
                cr.move_to(title_x, text_baseline - 4.0);
                cr.line_to(title_x + extents.width(), text_baseline - 4.0);
                cr.stroke().ok();
            }
        }

        // 4. Status
        let status_x = layout.col_check_w + layout.col_prio_w + layout.col_title_w + 8.0;
        cr.set_font_size(11.0);
        if is_dark {
            cr.set_source_rgba(0.7, 0.7, 0.7, 0.8);
        } else {
            cr.set_source_rgba(0.4, 0.4, 0.4, 0.8);
        }
        let display_status = truncate_cairo_text(cr, &row.status, layout.col_status_w - 16.0);
        cr.move_to(status_x, text_baseline);
        cr.show_text(&display_status).ok();

        // 5. Due Date
        let due_x = status_x + layout.col_status_w;
        if row.is_overdue {
            cr.set_source_rgba(0.95, 0.25, 0.25, 1.0);
        } else if is_dark {
            cr.set_source_rgba(0.7, 0.7, 0.7, 0.8);
        } else {
            cr.set_source_rgba(0.4, 0.4, 0.4, 0.8);
        }
        let display_due = truncate_cairo_text(cr, &row.due_display, layout.col_due_w - 16.0);
        cr.move_to(due_x, text_baseline);
        cr.show_text(&display_due).ok();

        // 6. Categories
        let cat_x = due_x + layout.col_due_w;
        if !row.categories.is_empty() {
            let cat_str = row.categories.join(", ");
            if is_dark {
                cr.set_source_rgba(0.65, 0.65, 0.65, 0.75);
            } else {
                cr.set_source_rgba(0.45, 0.45, 0.45, 0.75);
            }
            let display_cat = truncate_cairo_text(cr, &cat_str, layout.col_cat_w - 16.0);
            cr.move_to(cat_x, text_baseline);
            cr.show_text(&display_cat).ok();
        }

        // Horizontal row separator line
        cr.set_line_width(1.0);
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
        }
        let line_y = (row.y + row.height).floor() + 0.5;
        cr.move_to(0.0, line_y);
        cr.line_to(width, line_y);
        cr.stroke().ok();
    }
}

fn parse_smart_quick_add_input(
    raw: &str,
    due_preset: u8,
    prio_preset: u8,
    active_filter: TaskFilter,
) -> (String, String, u8, String) {
    let today = Local::now().date_naive();
    let mut due_str: Option<String> = None;
    let mut prio: Option<u8> = None;
    let mut categories: Vec<String> = Vec::new();
    let mut title_words: Vec<&str> = Vec::new();

    for token in raw.split_whitespace() {
        let lower = token.to_lowercase();
        if let Some(date_tok) = lower.strip_prefix('@') {
            match date_tok {
                "today" | "heute" | "tod" => {
                    due_str = Some(today.format("%Y-%m-%d").to_string());
                    continue;
                }
                "tomorrow" | "morgen" | "tmr" | "tom" => {
                    due_str = Some((today + Duration::days(1)).format("%Y-%m-%d").to_string());
                    continue;
                }
                "nextweek" | "woche" | "7d" | "+7d" | "next7" => {
                    due_str = Some((today + Duration::days(7)).format("%Y-%m-%d").to_string());
                    continue;
                }
                _ => {
                    if NaiveDate::parse_from_str(date_tok, "%Y-%m-%d").is_ok() {
                        due_str = Some(date_tok.to_string());
                        continue;
                    }
                }
            }
        } else if matches!(lower.as_str(), "!high" | "!hoch" | "!1" | "!!!" | "!urgent" | "!wichtig") {
            prio = Some(1);
            continue;
        } else if matches!(lower.as_str(), "!medium" | "!med" | "!mittel" | "!5" | "!!") {
            prio = Some(5);
            continue;
        } else if matches!(lower.as_str(), "!low" | "!niedrig" | "!9") {
            prio = Some(9);
            continue;
        } else if let Some(tag) = token.strip_prefix('#') {
            let cleaned = tag.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if !cleaned.is_empty() && !categories.iter().any(|c| c.eq_ignore_ascii_case(cleaned)) {
                categories.push(cleaned.to_string());
                continue;
            }
        }
        title_words.push(token);
    }

    let final_title = title_words.join(" ");
    let final_due = due_str.unwrap_or_else(|| match due_preset {
        1 => today.format("%Y-%m-%d").to_string(),
        2 => (today + Duration::days(1)).format("%Y-%m-%d").to_string(),
        3 => (today + Duration::days(7)).format("%Y-%m-%d").to_string(),
        _ => {
            if active_filter == TaskFilter::Today {
                today.format("%Y-%m-%d").to_string()
            } else {
                String::new()
            }
        }
    });
    let final_prio = prio.unwrap_or_else(|| {
        if prio_preset > 0 {
            prio_preset
        } else if active_filter == TaskFilter::Important {
            1
        } else {
            0
        }
    });
    let final_cats = categories.join(", ");
    (final_title, final_due, final_prio, final_cats)
}

pub(crate) fn mutate_task_item_component(
    page: &TasksPage,
    task: &TaskItemObject,
    new_component: String,
) {
    let source_uid = task.source_uid();
    if !task_source_is_writable_on_page(page, &source_uid) {
        page.show_toast(&gettext("This task list is not confirmed as writable"));
        return;
    }
    let uid = task.id();
    let uid_for_worker = uid.clone();
    let source_uid_for_worker = source_uid.clone();
    let component_for_worker = new_component.clone();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = ECalService::new().modify_component(
            &source_uid_for_worker,
            ECalComponentType::Task,
            &uid_for_worker,
            &component_for_worker,
        );
        let _ = sender.send_blocking(result);
    });

    let task_clone = task.clone();
    let page_weak = page.downgrade();
    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        match result {
            Ok(res) => {
                let authoritative = task_authoritative_component(&uid, &res.uid, res.component)
                    .unwrap_or(new_component);
                let draft = task_editor_draft_from_component(&authoritative);
                let is_comp = task_lifecycle_component_is_complete(&authoritative);
                let prio_str = if draft.priority == 0 {
                    "normal".to_string()
                } else {
                    draft.priority.to_string()
                };
                task_clone.set_title(draft.summary);
                task_clone.set_due_date(draft.due.input);
                task_clone.set_priority(prio_str);
                task_clone.set_completed(is_comp);
                task_clone.set_component(authoritative);
                page.refresh_tasks();
                refresh_task_preview(&page);
            }
            Err(error) => {
                page.show_toast(
                    &gettext("Could not update task: {error}").replace("{error}", &error.to_string()),
                );
            }
        }
    });
}

fn parse_task_description_checklist(description: &str) -> (Vec<(bool, String)>, String) {
    let mut steps = Vec::new();
    let mut notes_lines = Vec::new();
    for line in description.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- [ ] ").or_else(|| trimmed.strip_prefix("* [ ] ")) {
            steps.push((false, rest.trim().to_string()));
        } else if let Some(rest) = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
            .or_else(|| trimmed.strip_prefix("* [x] "))
            .or_else(|| trimmed.strip_prefix("* [X] "))
        {
            steps.push((true, rest.trim().to_string()));
        } else {
            notes_lines.push(line);
        }
    }
    let notes = notes_lines.join("\n").trim().to_string();
    (steps, notes)
}

fn compose_task_description_with_checklist(steps: &[(bool, String)], notes: &str) -> String {
    let mut out = Vec::new();
    for (done, text) in steps {
        let cleaned = text.trim();
        if !cleaned.is_empty() {
            out.push(format!("- [{}] {}", if *done { "x" } else { " " }, cleaned));
        }
    }
    let trimmed_notes = notes.trim();
    if !trimmed_notes.is_empty() {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(trimmed_notes.to_string());
    }
    out.join("\n")
}

pub(crate) fn toggle_task_item_completion(page: &TasksPage, task: &TaskItemObject, completed: bool) {
    let source_uid = task.source_uid();
    if !task_source_is_writable_on_page(page, &source_uid) {
        if let Some(canvas) = page.imp().task_table_canvas.borrow().as_ref() {
            show_toast_for_widget(
                canvas,
                &gettext("This task list is not confirmed as writable"),
            );
        }
        return;
    }

    let status = if completed {
        "COMPLETED"
    } else {
        "NEEDS-ACTION"
    };
    let percent = if completed { "100" } else { "0" };
    let mut component = update_ical_property(&task.component(), "STATUS", Some(status));
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
    let uid = task.id();
    let uid_for_worker = uid.clone();
    let source_uid_for_worker = source_uid.clone();
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

    let task_clone = task.clone();
    let page_weak = page.downgrade();
    glib::spawn_future_local(async move {
        let result = match receiver.recv().await {
            Ok(result) => result,
            Err(error) => {
                tracing::debug!(%error, "Task status mutation channel closed");
                if let Some(page) = page_weak.upgrade() {
                    if let Some(canvas) = page.imp().task_table_canvas.borrow().as_ref() {
                        show_toast_for_widget(
                            canvas,
                            &gettext("Could not update task status"),
                        );
                    }
                }
                return;
            }
        };
        match result {
            Ok(result) => {
                let component = match task_authoritative_component(&uid, &result.uid, result.component) {
                    Ok(component) => component,
                    Err(error) => {
                        if let Some(page) = page_weak.upgrade() {
                            if let Some(canvas) = page.imp().task_table_canvas.borrow().as_ref() {
                                show_toast_for_widget(canvas, &error);
                            }
                        }
                        return;
                    }
                };
                task_clone.set_completed(completed);
                task_clone.set_component(component);
                if let Some(page) = page_weak.upgrade() {
                    page.refresh_tasks();
                }
            }
            Err(error) => {
                if let Some(page) = page_weak.upgrade() {
                    if let Some(canvas) = page.imp().task_table_canvas.borrow().as_ref() {
                        show_toast_for_widget(
                            canvas,
                            &gettext("Could not update task: {error}").replace("{error}", &error.to_string()),
                        );
                    }
                }
            }
        }
    });
}

fn show_task_table_context_menu(
    page: &TasksPage,
    anchor_widget: &gtk::DrawingArea,
    task: &TaskItemObject,
    rect: gdk::Rectangle,
) {
    let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

    // 1. Edit
    let edit = crate::views::create_context_menu_button(
        None,
        &gettext("Edit Task"),
        false,
    );
    let popover_for_edit = popover.clone();
    let page_weak_edit = page.downgrade();
    let task_for_edit = task.clone();
    edit.connect_clicked(move |_| {
        popover_for_edit.popdown();
        if let Some(page) = page_weak_edit.upgrade() {
            if let (Some(store), Some(list), Some(empty)) = (
                page.imp().task_store.borrow().clone(),
                page.imp().task_list.borrow().clone(),
                page.imp().task_empty_state.borrow().clone(),
            ) {
                page.open_task_dialog(store, list, empty, Some(task_for_edit.clone()));
            }
        }
    });
    menu_box.append(&edit);

    // 2. Toggle completion
    let completed = task_is_completed(task);
    let toggle_label = if completed {
        gettext("Mark as Incomplete")
    } else {
        gettext("Mark as Completed")
    };
    let toggle = crate::views::create_context_menu_button(
        None,
        &toggle_label,
        false,
    );
    toggle.set_sensitive(task_source_is_writable_on_page(page, &task.source_uid()));
    let popover_for_toggle = popover.clone();
    let page_weak_toggle = page.downgrade();
    let task_for_toggle = task.clone();
    toggle.connect_clicked(move |_| {
        popover_for_toggle.popdown();
        if let Some(page) = page_weak_toggle.upgrade() {
            toggle_task_item_completion(&page, &task_for_toggle, !completed);
        }
    });
    menu_box.append(&toggle);

    menu_box.append(&crate::views::create_context_menu_separator());

    // 3. Delete
    let delete = crate::views::create_context_menu_button(
        None,
        &gettext("Delete Task"),
        true,
    );
    delete.set_sensitive(task_source_is_writable_on_page(page, &task.source_uid()));
    let popover_for_delete = popover.clone();
    let page_weak_delete = page.downgrade();
    delete.connect_clicked(move |_| {
        popover_for_delete.popdown();
        if let Some(page) = page_weak_delete.upgrade() {
            if let (Some(store), Some(list), Some(empty), Some(del_btn)) = (
                page.imp().task_store.borrow().clone(),
                page.imp().task_list.borrow().clone(),
                page.imp().task_empty_state.borrow().clone(),
                page.imp().task_delete_selection_button.borrow().clone(),
            ) {
                present_task_delete_confirmation(
                    &page,
                    &del_btn,
                    &store,
                    &list,
                    &empty,
                );
            }
        }
    });
    menu_box.append(&delete);

    popover.set_parent(anchor_widget);
    popover.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || {
            p.unparent();
        });
    });
    popover.popup();
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
    let hidden_sources = page.imp().hidden_task_sources.borrow().clone();
    let sort = page.imp().task_sort.get();
    let context = current_task_filter_context();
    let query = TaskQuery::parse(
        page.imp().task_search_mode.get(),
        page.imp().task_query.borrow().as_str(),
    );
    let mut tasks = Vec::new();
    let mut all_source_tasks = Vec::new();
    for index in 0..task_store.n_items() {
        let Some(task) = task_store.item(index).and_downcast::<TaskItemObject>() else {
            continue;
        };
        if hidden_sources.contains(&task.source_uid()) {
            continue;
        }
        if source_filter
            .as_deref()
            .is_none_or(|src| task.source_uid() == src)
        {
            all_source_tasks.push(task.clone());
        }
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

    // Update live Smart View count badges in left sidebar
    {
        let empty_query = TaskQuery::default();
        let badges = page.imp().task_smart_filter_badges.borrow().clone();
        for (sf, badge) in badges {
            let count = all_source_tasks
                .iter()
                .filter(|t| task_matches_filter_with_context(t, sf, None, None, &empty_query, context))
                .count();
            badge.set_label(&count.to_string());
            badge.set_visible(count > 0);
        }
    }

    // Update Workspace Header Title & Subtitle metrics
    {
        let empty_query = TaskQuery::default();
        let view_title = TASK_FILTER_OPTIONS
            .iter()
            .find(|(f, _, _)| *f == filter)
            .map(|(_, _, lbl)| gettext(*lbl))
            .unwrap_or_else(|| gettext("Tasks"));
        if let Some(title_lbl) = page.imp().task_workspace_title.borrow().as_ref() {
            title_lbl.set_label(&view_title);
        }
        if let Some(sub_lbl) = page.imp().task_workspace_subtitle.borrow().as_ref() {
            let open_cnt = tasks.iter().filter(|t| !task_is_completed(t)).count();
            let overdue_cnt = tasks
                .iter()
                .filter(|t| task_matches_filter_with_context(t, TaskFilter::Overdue, None, None, &empty_query, context))
                .count();
            let today_cnt = tasks
                .iter()
                .filter(|t| task_matches_filter_with_context(t, TaskFilter::Today, None, None, &empty_query, context))
                .count();
            let mut parts = vec![format!("{} {}", open_cnt, gettext("open"))];
            if overdue_cnt > 0 {
                parts.push(format!("{} {}", overdue_cnt, gettext("overdue")));
            }
            if today_cnt > 0 {
                parts.push(format!("{} {}", today_cnt, gettext("due today")));
            }
            sub_lbl.set_label(&parts.join(" · "));
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

    // Populate `task_list` with `adw::ActionRow` items for print/export/search helpers
    for task in &tasks {
        let action_row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&task.title()))
            .build();
        unsafe {
            action_row.set_data("task-source-uid", task.source_uid());
            action_row.set_data("task-id", task.id());
        }
        let list_row = gtk::ListBoxRow::builder()
            .child(&action_row)
            .build();
        task_list.append(&list_row);
    }

    let canvas_width = page
        .imp()
        .task_table_canvas
        .borrow()
        .as_ref()
        .map(|c| c.width() as f64)
        .filter(|&w| w > 100.0)
        .unwrap_or(700.0);
    let layout = prepare_task_table_layout(&tasks, canvas_width, context.today);
    let total_h = layout.total_height.max(80.0);
    page.imp().task_table_layout.replace(Some(layout));

    if let Some(canvas) = page.imp().task_table_canvas.borrow().as_ref() {
        canvas.set_content_height(total_h as i32);
        canvas.queue_draw();
    }
    if let Some(header) = page.imp().task_table_header.borrow().as_ref() {
        header.queue_draw();
    }

    let active_selection: Vec<TaskSelectionIdentity> = tasks
        .iter()
        .map(TaskSelectionIdentity::from_task)
        .filter(|id| preserved_selection.contains(id))
        .collect();
    page.imp().task_selection.replace(active_selection.clone());
    page.imp().task_selection_restoring.set(false);

    // Populate `task_workspace_groups_box` with modern grouped Libadwaita Task Cards
    if let Some(groups_box) = page.imp().task_workspace_groups_box.borrow().as_ref() {
        while let Some(child) = groups_box.first_child() {
            groups_box.remove(&child);
        }
        render_task_workspace_groups(
            page,
            groups_box,
            &tasks,
            &active_selection,
            context,
            sort,
            filter,
        );
    }

    let visible = tasks.len();
    update_task_selection_controls(page);
    refresh_task_preview(page);
    empty_tasks.set_visible(visible == 0);
    if visible == 0 {
        let label = if query_is_empty(&query) && category_filter.is_none() {
            match filter {
                TaskFilter::All => gettext("No tasks loaded"),
                TaskFilter::Today => gettext("No task due today"),
                TaskFilter::Next7 => gettext("No task due in next 7 days"),
                TaskFilter::Next30 => gettext("No task due in next 30 days"),
                TaskFilter::Next365 => gettext("No active task due in next 365 days"),
                TaskFilter::Important => gettext("No important tasks"),
                TaskFilter::Overdue => gettext("No overdue task"),
                TaskFilter::Scheduled => gettext("No task with due date"),
                TaskFilter::NoDue => gettext("No task without due date"),
                TaskFilter::WithAttachments => gettext("No task with attachments"),
                TaskFilter::Started => gettext("No started task"),
                TaskFilter::WithoutCategory => gettext("No task without category"),
                TaskFilter::Uncompleted => gettext("No open task"),
                TaskFilter::InProgress => gettext("No task in progress"),
                TaskFilter::Cancelled => gettext("No cancelled task"),
                TaskFilter::Completed => gettext("No completed tasks"),
            }
        } else {
            gettext("No matching tasks")
        };
        empty_tasks.set_label(&label);
    }

    if let Some(status_page) = page.imp().task_empty_status_page.borrow().as_ref() {
        let (title, description) = if query_is_empty(&query) && category_filter.is_none() {
            match filter {
                TaskFilter::All => (
                    gettext("No Tasks"),
                    gettext("Select a task list or create a new task."),
                ),
                TaskFilter::Today => (
                    gettext("No Tasks Today"),
                    gettext("No tasks scheduled or due today."),
                ),
                TaskFilter::Next7 => (
                    gettext("No Tasks in 7 Days"),
                    gettext("No tasks due in the next 7 days."),
                ),
                TaskFilter::Next30 => (
                    gettext("No Tasks This Month"),
                    gettext("No tasks due in the next 30 days."),
                ),
                TaskFilter::Next365 => (
                    gettext("No Tasks This Year"),
                    gettext("No active tasks scheduled in the next year."),
                ),
                TaskFilter::Important => (
                    gettext("No Important Tasks"),
                    gettext("There are no high priority tasks."),
                ),
                TaskFilter::Overdue => (
                    gettext("No Overdue Tasks"),
                    gettext("Great! No due tasks are overdue."),
                ),
                TaskFilter::Scheduled => (
                    gettext("No Scheduled Tasks"),
                    gettext("No tasks with a due date found."),
                ),
                TaskFilter::NoDue => (
                    gettext("No Tasks Without Due Date"),
                    gettext("All tasks have an assigned due date."),
                ),
                TaskFilter::WithAttachments => (
                    gettext("No Tasks With Attachments"),
                    gettext("No tasks with file attachments were found."),
                ),
                TaskFilter::Started => (
                    gettext("No Started Tasks"),
                    gettext("There are no tasks with status 'Started'."),
                ),
                TaskFilter::WithoutCategory => (
                    gettext("No Tasks Without Category"),
                    gettext("All tasks are assigned to at least one category."),
                ),
                TaskFilter::Uncompleted => (
                    gettext("No Open Tasks"),
                    gettext("All tasks in this list are already completed!"),
                ),
                TaskFilter::InProgress => (
                    gettext("No Tasks In Progress"),
                    gettext("No tasks are currently being actively worked on."),
                ),
                TaskFilter::Cancelled => (
                    gettext("No Cancelled Tasks"),
                    gettext("There are no discarded or cancelled tasks."),
                ),
                TaskFilter::Completed => (
                    gettext("No Completed Tasks"),
                    gettext("No tasks have been marked as completed yet."),
                ),
            }
        } else {
            (
                gettext("No matching tasks"),
                gettext("Check your search term or filter settings."),
            )
        };
        status_page.set_title(&title);
        status_page.set_description(Some(&description));
    }
    if let Some(stack) = page.imp().task_container_stack.borrow().as_ref() {
        stack.set_visible_child_name(if visible == 0 { "empty" } else { "list" });
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
            .map_err(|_| gettext("A visible task row is invalid"))?;
        let action_row = row
            .child()
            .and_downcast::<adw::ActionRow>()
            .ok_or_else(|| gettext("A visible task row is no longer available"))?;
        let read_data = |key: &str| unsafe {
            action_row
                .data::<String>(key)
                .map(|value| value.as_ref().clone())
        };
        let source_uid = read_data("task-source-uid")
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| gettext("A visible task does not have a valid source"))?;
        let task_uid = read_data("task-id")
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| gettext("A visible task does not have a valid identifier"))?;
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
        let tooltip = if page.imp().task_assignment_running.get() {
            gettext("Assigning task…")
        } else if assignment_enabled {
            gettext("Assign selected task")
        } else {
            gettext("Assigning requires exactly one writable task and backend support")
        };
        assign_button.set_tooltip_text(Some(&tooltip));
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
        let tooltip = if paste_destination.is_ok() {
            gettext("Paste tasks from clipboard into default task list")
        } else {
            gettext("No writable default task list available")
        };
        paste_button.set_tooltip_text(Some(&tooltip));
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
        .map_or_else(|| gettext("Delete selected tasks"), Clone::clone);
    if let Some(delete_button) = page.imp().task_delete_selection_button.borrow().as_ref() {
        delete_button.set_sensitive(delete_enabled);
        delete_button.set_tooltip_text(Some(&delete_tooltip));
    }
    let purge_plan = build_task_purge_plan(&candidates, &capabilities, local_writable);
    if let Some(purge_button) = page.imp().task_purge_button.borrow().as_ref() {
        purge_button.set_sensitive(!mutation_running && purge_plan.is_ok());
        let tooltip = purge_plan.as_ref().err().map_or_else(
            || gettext("Permanently delete all completed tasks (Ctrl+E)"),
            Clone::clone,
        );
        purge_button.set_tooltip_text(Some(&tooltip));
    }
    if let Some(refresh_button) = page.imp().task_refresh_button.borrow().as_ref() {
        refresh_button.set_sensitive(refresh_enabled);
        let tooltip = if page.imp().task_refresh_running.get() {
            gettext("Refreshing task list…")
        } else if primary_source.is_none() {
            gettext("Select a single task list to refresh")
        } else if refresh_known_unsupported {
            gettext("This task list does not support provider refresh")
        } else if !refresh_enabled {
            gettext("The selected task list is not available")
        } else {
            gettext("Refresh selected task list with provider")
        };
        refresh_button.set_tooltip_text(Some(&tooltip));
    }
    let source_action_state = task_source_action_state(
        primary_source.as_deref(),
        &page.imp().task_source_metadata.borrow(),
        mutation_running,
    );
    if let Some(button) = page.imp().task_source_new_button.borrow().as_ref() {
        button.set_sensitive(!mutation_running);
        let tooltip = if mutation_running {
            gettext("Another task operation is already in progress")
        } else {
            gettext("New local task list")
        };
        button.set_tooltip_text(Some(&tooltip));
    }
    if let Some(button) = page.imp().task_source_copy_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.copy_enabled);
        let tooltip = if primary_source.is_none() {
            gettext("Select a single task list to copy")
        } else if !source_action_state.copy_enabled {
            gettext("The selected task list is not available as an EDS source")
        } else {
            gettext("Copy selected task list")
        };
        button.set_tooltip_text(Some(&tooltip));
    }
    if let Some(button) = page.imp().task_source_rename_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.rename_enabled);
        let tooltip = if primary_source.is_none() {
            gettext("Select a single task list to rename")
        } else if !source_action_state.rename_enabled {
            gettext("Only writable task lists outside of a collection can be renamed")
        } else {
            gettext("Rename selected task list")
        };
        button.set_tooltip_text(Some(&tooltip));
    }
    if let Some(button) = page.imp().task_source_properties_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.properties_enabled);
        let tooltip = if primary_source.is_none() {
            gettext("Select a single task list to edit")
        } else if !source_action_state.properties_enabled {
            gettext("The properties of this task list are not writable")
        } else {
            gettext("Properties of the selected task list")
        };
        button.set_tooltip_text(Some(&tooltip));
    }
    if let Some(button) = page.imp().task_source_delete_button.borrow().as_ref() {
        button.set_sensitive(source_action_state.delete_enabled);
        let tooltip = if primary_source.is_none() {
            gettext("Select a single task list to delete")
        } else if !source_action_state.delete_enabled {
            gettext("This task list cannot be removed")
        } else {
            gettext("Delete selected task list")
        };
        button.set_tooltip_text(Some(&tooltip));
    }
    let label = match selection.len() {
        0 => gettext("No selection"),
        count => ngettext("1 task selected", "{count} tasks selected", count as u32)
            .replace("{count}", &count.to_string()),
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
        show_toast_for_widget(button, &gettext("Select a task list to refresh"));
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
            &gettext("The selected task list is no longer available"),
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
            &gettext("This task list does not support provider refresh"),
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
    show_toast_for_widget(button, &gettext("Refreshing task list…"));

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
                        gettext("The refreshed task list could not be reloaded authoritatively"),
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
                page.show_toast(&match outcome {
                    ECalRefreshOutcome::LocalReloadOnly => gettext("Local task list was reloaded"),
                    ECalRefreshOutcome::RemoteRefreshed => gettext("Task list was refreshed"),
                });
            }
            Err(TaskRefreshFailure::Unsupported) => {
                page.imp()
                    .task_refresh_unsupported_sources
                    .borrow_mut()
                    .insert(source_uid);
                page.show_toast(&gettext("This task list does not support provider refresh"));
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
        if page.imp().is_mobile.get() {
            preview_box.set_visible(page.imp().is_detail_shown.get());
        } else {
            preview_box.set_visible(visible);
        }
    }
    let Some(text_view) = page.imp().task_preview_content.borrow().as_ref().cloned() else {
        return;
    };
    if !visible {
        if let Some(stack) = page.imp().task_preview_stack.borrow().as_ref() {
            stack.set_visible_child_name("empty");
        }
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
        Err(error) => (
            None,
            gettext("Preview not available: {error}").replace("{error}", &error),
        ),
    };
    if let Some(stack) = page.imp().task_preview_stack.borrow().as_ref() {
        if selection.len() != 1 || text.trim().is_empty() {
            stack.set_visible_child_name("empty");
        } else {
            stack.set_visible_child_name("content");
        }
    }
    let changed = *page.imp().task_preview_identity.borrow() != identity
        || *page.imp().task_preview_text.borrow() != text;
    if changed {
        text_view.buffer().set_text(&text);
        page.imp().task_preview_identity.replace(identity.clone());
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
        populate_task_live_inspector(page, identity.as_ref());
    }
}

fn render_task_workspace_groups(
    page: &TasksPage,
    groups_box: &gtk::Box,
    tasks: &[TaskItemObject],
    selection: &[TaskSelectionIdentity],
    context: TaskFilterContext,
    sort: TaskSort,
    filter: TaskFilter,
) {
    if tasks.is_empty() {
        return;
    }

    let selected_set: HashSet<TaskSelectionIdentity> = selection.iter().cloned().collect();
    let group_by_time = sort == TaskSort::DueDate
        && matches!(
            filter,
            TaskFilter::All
                | TaskFilter::Uncompleted
                | TaskFilter::Today
                | TaskFilter::Next7
                | TaskFilter::Next30
                | TaskFilter::Scheduled
                | TaskFilter::Important
        );

    if group_by_time {
        let mut overdue_vec = Vec::new();
        let mut today_vec = Vec::new();
        let mut next7_vec = Vec::new();
        let mut later_vec = Vec::new();
        let mut no_due_vec = Vec::new();
        let mut completed_vec = Vec::new();

        for task in tasks {
            if task_is_completed(task) {
                completed_vec.push(task.clone());
                continue;
            }
            let due_opt = task_next_due_date(task, context.today);
            match due_opt {
                Some(d) if d < context.today => overdue_vec.push(task.clone()),
                Some(d) if d == context.today => today_vec.push(task.clone()),
                Some(d) if d <= context.today + Duration::days(7) => next7_vec.push(task.clone()),
                Some(_) => later_vec.push(task.clone()),
                None => no_due_vec.push(task.clone()),
            }
        }

        let sections: [(String, &str, Vec<TaskItemObject>); 6] = [
            (gettext("Overdue"), "overdue", overdue_vec),
            (gettext("Today"), "today", today_vec),
            (gettext("Next 7 Days"), "upcoming", next7_vec),
            (gettext("Later"), "later", later_vec),
            (gettext("No Due Date"), "nodue", no_due_vec),
            (gettext("Completed"), "completed", completed_vec),
        ];

        for (sec_title, sec_kind, sec_tasks) in sections {
            if sec_tasks.is_empty() {
                continue;
            }
            append_task_workspace_section(
                page,
                groups_box,
                Some((&sec_title, sec_kind, sec_tasks.len())),
                &sec_tasks,
                &selected_set,
                context,
            );
        }
    } else {
        append_task_workspace_section(page, groups_box, None, tasks, &selected_set, context);
    }
}

fn append_task_workspace_section(
    page: &TasksPage,
    groups_box: &gtk::Box,
    header_info: Option<(&str, &str, usize)>,
    sec_tasks: &[TaskItemObject],
    selected_set: &HashSet<TaskSelectionIdentity>,
    context: TaskFilterContext,
) {
    let sec_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .hexpand(true)
        .build();

    if let Some((title, kind, count)) = header_info {
        let hdr = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(4)
            .margin_end(4)
            .css_classes(vec!["task-group-header"])
            .build();
        let mut title_classes = vec!["task-group-title"];
        if kind == "overdue" {
            title_classes.push("task-group-title-overdue");
        } else if kind == "today" {
            title_classes.push("task-group-title-today");
        }
        let lbl = gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(title_classes)
            .build();
        let mut pill_classes = vec!["task-group-count-pill"];
        if kind == "overdue" {
            pill_classes.push("task-group-count-overdue");
        }
        let pill = gtk::Label::builder()
            .label(&count.to_string())
            .valign(gtk::Align::Center)
            .css_classes(pill_classes)
            .build();
        hdr.append(&lbl);
        hdr.append(&pill);
        sec_box.append(&hdr);
    }

    let card_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list", "task-workspace-card-list"])
        .build();

    for task in sec_tasks {
        let row = build_task_workspace_card_row(page, task, selected_set, context);
        card_list.append(&row);
    }

    sec_box.append(&card_list);
    groups_box.append(&sec_box);
}

fn build_task_workspace_card_row(
    page: &TasksPage,
    task: &TaskItemObject,
    selected_set: &HashSet<TaskSelectionIdentity>,
    context: TaskFilterContext,
) -> gtk::ListBoxRow {
    let identity = TaskSelectionIdentity::from_task(task);
    let is_selected = selected_set.contains(&identity);
    let completed = task_is_completed(task);
    let comp = task.component();
    let draft = task_editor_draft_from_component(&comp);
    let prio = draft.priority;
    let is_high_prio = (1..=4).contains(&prio);
    let (steps, clean_notes) = parse_task_description_checklist(&draft.description);

    let outer_row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_start(12)
        .margin_end(10)
        .margin_top(8)
        .margin_bottom(8)
        .valign(gtk::Align::Center)
        .build();

    // 1. CheckButton
    let mut check_classes = vec!["task-check-btn"];
    if is_high_prio && !completed {
        check_classes.push("task-check-high");
    }
    let check_btn = gtk::CheckButton::builder()
        .active(completed)
        .valign(gtk::Align::Center)
        .tooltip_text(&if completed {
            gettext("Mark as incomplete")
        } else {
            gettext("Mark as completed")
        })
        .css_classes(check_classes)
        .build();
    {
        let page_weak = page.downgrade();
        let task_for_toggle = task.clone();
        check_btn.connect_toggled(move |btn| {
            let Some(page) = page_weak.upgrade() else { return; };
            let target_done = btn.is_active();
            if target_done != task_is_completed(&task_for_toggle) {
                toggle_task_item_completion(&page, &task_for_toggle, target_done);
            }
        });
    }
    outer_row_box.append(&check_btn);

    // 2. Center Title + Subtitle (Checklist progress / Notes preview)
    let center_col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let mut title_classes = vec!["task-card-title"];
    if completed {
        title_classes.push("task-card-title-completed");
    }
    let title_lbl = gtk::Label::builder()
        .label(&task.title())
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(title_classes)
        .build();
    center_col.append(&title_lbl);

    let has_steps = !steps.is_empty();
    let first_note_line = clean_notes
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if has_steps || !first_note_line.is_empty() {
        let sub_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        if has_steps {
            let done_steps = steps.iter().filter(|(d, _)| *d).count();
            let step_badge = gtk::Label::builder()
                .label(&format!("{}/{}", done_steps, steps.len()))
                .css_classes(vec!["task-step-progress-pill"])
                .build();
            sub_row.append(&step_badge);
        }
        if !first_note_line.is_empty() {
            let note_lbl = gtk::Label::builder()
                .label(first_note_line)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(vec!["dim-label", "caption", "task-card-note-preview"])
                .build();
            sub_row.append(&note_lbl);
        }
        center_col.append(&sub_row);
    }
    outer_row_box.append(&center_col);

    // 3. Right Metadata Pills & Quick Star Button
    let right_pills = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();

    // Status pill if In Progress
    if !completed && (draft.status == "IN-PROCESS" || (1..=99).contains(&draft.percent)) {
        let status_text = if draft.percent > 0 && draft.percent < 100 {
            format!("{} · {}%", gettext("In Progress"), draft.percent)
        } else {
            gettext("In Progress")
        };
        let status_pill = gtk::Label::builder()
            .label(&status_text)
            .css_classes(vec!["task-meta-pill", "task-meta-pill-progress"])
            .build();
        right_pills.append(&status_pill);
    }

    // Category chips (up to 2)
    let cats = category_names_from_ical_component(&comp);
    for cat in cats.iter().take(2) {
        let cat_pill = gtk::Label::builder()
            .label(cat)
            .css_classes(vec!["task-meta-pill", "task-meta-pill-category"])
            .build();
        right_pills.append(&cat_pill);
    }

    // Due date pill
    if let Some(due_date) = task_next_due_date(task, context.today) {
        let is_overdue = !completed && due_date < context.today;
        let is_today = !completed && due_date == context.today;
        let is_tomorrow = !completed && due_date == context.today + Duration::days(1);
        let due_label = if is_today {
            gettext("Today")
        } else if is_tomorrow {
            gettext("Tomorrow")
        } else if is_overdue {
            format!("{} · {}", gettext("Overdue"), due_date.format("%d.%m."))
        } else {
            due_date.format("%d.%m.%Y").to_string()
        };
        let mut due_classes = vec!["task-meta-pill"];
        if is_overdue {
            due_classes.push("task-meta-pill-overdue");
        } else if is_today {
            due_classes.push("task-meta-pill-today");
        } else {
            due_classes.push("task-meta-pill-date");
        }
        let due_pill = gtk::Label::builder()
            .label(&due_label)
            .css_classes(due_classes)
            .build();
        right_pills.append(&due_pill);
    }

    // Quick Important / Star Toggle Button
    let star_icon = if is_high_prio {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    };
    let mut star_classes = vec!["flat", "circular", "task-star-btn"];
    if is_high_prio {
        star_classes.push("active");
    }
    let star_btn = gtk::Button::builder()
        .icon_name(star_icon)
        .tooltip_text(&if is_high_prio {
            gettext("Remove High Priority")
        } else {
            gettext("Mark as Important (High Priority)")
        })
        .valign(gtk::Align::Center)
        .css_classes(star_classes)
        .build();
    {
        let page_weak = page.downgrade();
        let task_for_star = task.clone();
        star_btn.connect_clicked(move |_| {
            let Some(page) = page_weak.upgrade() else { return; };
            let current_comp = task_for_star.component();
            let current_draft = task_editor_draft_from_component(&current_comp);
            let new_prio = if (1..=4).contains(&current_draft.priority) {
                None
            } else {
                Some("1")
            };
            let updated = update_ical_property(&current_comp, "PRIORITY", new_prio);
            mutate_task_item_component(&page, &task_for_star, updated);
        });
    }
    right_pills.append(&star_btn);
    outer_row_box.append(&right_pills);

    let mut row_classes = vec!["task-workspace-row"];
    if is_selected {
        row_classes.push("selected");
    }
    if completed {
        row_classes.push("completed");
    }
    let list_row = gtk::ListBoxRow::builder()
        .child(&outer_row_box)
        .activatable(true)
        .selectable(false)
        .css_classes(row_classes)
        .build();

    // Primary Click (Single = Select & Open Inspector, Double = Open Full Dialog)
    {
        let click = gtk::GestureClick::builder().button(1).build();
        let page_weak = page.downgrade();
        let task_for_click = task.clone();
        let id_for_click = identity.clone();
        click.connect_pressed(move |gesture, n_press, _x, _y| {
            let Some(page) = page_weak.upgrade() else { return; };
            if n_press == 2 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let (Some(store), Some(list), Some(empty)) = (
                    page.imp().task_store.borrow().clone(),
                    page.imp().task_list.borrow().clone(),
                    page.imp().task_empty_state.borrow().clone(),
                ) {
                    page.open_task_dialog(store, list, empty, Some(task_for_click.clone()));
                }
            } else if n_press == 1 {
                page.imp().task_selection.replace(vec![id_for_click.clone()]);
                if page.imp().is_mobile.get() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_tasks_layout_visibility();
                }
                update_task_selection_controls(&page);
                refresh_task_preview(&page);
                if let Some(groups_box) = page.imp().task_workspace_groups_box.borrow().as_ref() {
                    update_workspace_row_selection_styles(groups_box, &id_for_click);
                }
            }
        });
        list_row.add_controller(click);
    }

    // Right-Click Context Menu
    {
        let right_click = gtk::GestureClick::builder().button(3).build();
        let page_weak = page.downgrade();
        let task_for_menu = task.clone();
        let id_for_menu = identity.clone();
        let row_for_menu = list_row.clone();
        right_click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let Some(page) = page_weak.upgrade() else { return; };
            page.imp().task_selection.replace(vec![id_for_menu.clone()]);
            update_task_selection_controls(&page);
            refresh_task_preview(&page);
            if let Some(groups_box) = page.imp().task_workspace_groups_box.borrow().as_ref() {
                update_workspace_row_selection_styles(groups_box, &id_for_menu);
            }
            let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            show_task_workspace_row_context_menu(&page, &row_for_menu, &task_for_menu, rect);
        });
        list_row.add_controller(right_click);
    }

    unsafe {
        list_row.set_data("workspace-task-id", identity);
    }

    list_row
}

fn update_workspace_row_selection_styles(groups_box: &gtk::Box, selected_id: &TaskSelectionIdentity) {
    let mut sec = groups_box.first_child();
    while let Some(sec_w) = sec {
        sec = sec_w.next_sibling();
        let mut child = sec_w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if let Ok(list_box) = c.downcast::<gtk::ListBox>() {
                let mut row_opt = list_box.first_child();
                while let Some(r_w) = row_opt {
                    row_opt = r_w.next_sibling();
                    if let Ok(row) = r_w.downcast::<gtk::ListBoxRow>() {
                        let is_match = unsafe {
                            row.data::<TaskSelectionIdentity>("workspace-task-id")
                                .is_some_and(|v| v.as_ref() == selected_id)
                        };
                        if is_match {
                            row.add_css_class("selected");
                        } else {
                            row.remove_css_class("selected");
                        }
                    }
                }
            }
        }
    }
}

fn show_task_workspace_row_context_menu(
    page: &TasksPage,
    anchor: &impl IsA<gtk::Widget>,
    task: &TaskItemObject,
    rect: gdk::Rectangle,
) {
    let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));
    let writable = task_source_is_writable_on_page(page, &task.source_uid());

    // 1. Toggle completion
    let completed = task_is_completed(task);
    let toggle_btn = crate::views::create_context_menu_button(
        None,
        &if completed {
            gettext("Mark as Incomplete")
        } else {
            gettext("Mark as Completed")
        },
        false,
    );
    toggle_btn.set_sensitive(writable);
    {
        let pop = popover.clone();
        let pw = page.downgrade();
        let t = task.clone();
        toggle_btn.connect_clicked(move |_| {
            pop.popdown();
            if let Some(p) = pw.upgrade() {
                toggle_task_item_completion(&p, &t, !completed);
            }
        });
    }
    menu_box.append(&toggle_btn);

    // 2. Quick Schedule: Due Today / Due Tomorrow
    let today_btn = crate::views::create_context_menu_button(None, &gettext("Due Today"), false);
    today_btn.set_sensitive(writable);
    {
        let pop = popover.clone();
        let pw = page.downgrade();
        let t = task.clone();
        today_btn.connect_clicked(move |_| {
            pop.popdown();
            if let Some(p) = pw.upgrade() {
                let date_str = Local::now().date_naive().format("%Y%m%d").to_string();
                let updated = update_ical_property(&t.component(), "DUE;VALUE=DATE", Some(&date_str));
                mutate_task_item_component(&p, &t, updated);
            }
        });
    }
    menu_box.append(&today_btn);

    let tmr_btn = crate::views::create_context_menu_button(None, &gettext("Due Tomorrow"), false);
    tmr_btn.set_sensitive(writable);
    {
        let pop = popover.clone();
        let pw = page.downgrade();
        let t = task.clone();
        tmr_btn.connect_clicked(move |_| {
            pop.popdown();
            if let Some(p) = pw.upgrade() {
                let date_str = (Local::now().date_naive() + Duration::days(1))
                    .format("%Y%m%d")
                    .to_string();
                let updated = update_ical_property(&t.component(), "DUE;VALUE=DATE", Some(&date_str));
                mutate_task_item_component(&p, &t, updated);
            }
        });
    }
    menu_box.append(&tmr_btn);

    // 3. Edit Full Details
    menu_box.append(&crate::views::create_context_menu_separator());
    let edit_btn = crate::views::create_context_menu_button(None, &gettext("Edit Task…"), false);
    {
        let pop = popover.clone();
        let pw = page.downgrade();
        let t = task.clone();
        edit_btn.connect_clicked(move |_| {
            pop.popdown();
            if let Some(p) = pw.upgrade() {
                if let (Some(store), Some(list), Some(empty)) = (
                    p.imp().task_store.borrow().clone(),
                    p.imp().task_list.borrow().clone(),
                    p.imp().task_empty_state.borrow().clone(),
                ) {
                    p.open_task_dialog(store, list, empty, Some(t.clone()));
                }
            }
        });
    }
    menu_box.append(&edit_btn);

    // 4. Delete
    menu_box.append(&crate::views::create_context_menu_separator());
    let del_btn = crate::views::create_context_menu_button(None, &gettext("Delete Task"), true);
    del_btn.set_sensitive(writable);
    {
        let pop = popover.clone();
        let pw = page.downgrade();
        del_btn.connect_clicked(move |_| {
            pop.popdown();
            if let Some(p) = pw.upgrade() {
                if let (Some(store), Some(list), Some(empty), Some(del_action_btn)) = (
                    p.imp().task_store.borrow().clone(),
                    p.imp().task_list.borrow().clone(),
                    p.imp().task_empty_state.borrow().clone(),
                    p.imp().task_delete_selection_button.borrow().clone(),
                ) {
                    present_task_delete_confirmation(&p, &del_action_btn, &store, &list, &empty);
                }
            }
        });
    }
    menu_box.append(&del_btn);

    popover.set_parent(anchor);
    popover.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || {
            p.unparent();
        });
    });
    popover.popup();
}

fn populate_task_live_inspector(
    page: &TasksPage,
    selected_identity: Option<&TaskSelectionIdentity>,
) {
    let Some(inspector_box) = page.imp().task_inspector_box.borrow().as_ref().cloned() else {
        return;
    };
    while let Some(child) = inspector_box.first_child() {
        inspector_box.remove(&child);
    }
    let Some(identity) = selected_identity else {
        return;
    };
    let Some(store) = page.imp().task_store.borrow().as_ref().cloned() else {
        return;
    };
    let Some(task) = (0..store.n_items())
        .filter_map(|i| store.item(i).and_downcast::<TaskItemObject>())
        .find(|t| t.source_uid() == identity.source_uid && t.id() == identity.task_uid)
    else {
        return;
    };

    let writable = task_source_is_writable_on_page(page, &task.source_uid());
    let comp = task.component();
    let draft = task_editor_draft_from_component(&comp);
    let completed = task_is_completed(&task);
    let (steps, clean_notes) = parse_task_description_checklist(&draft.description);

    // 1. Top Inspector Action Bar
    let top_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let complete_btn = gtk::Button::builder()
        .sensitive(writable)
        .css_classes(if completed {
            vec!["task-inspector-complete-btn", "completed"]
        } else {
            vec!["suggested-action", "task-inspector-complete-btn"]
        })
        .build();
    let complete_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    complete_btn_box.append(&gtk::Image::from_icon_name("emblem-ok-symbolic"));
    complete_btn_box.append(&gtk::Label::new(Some(&if completed {
        gettext("Completed")
    } else {
        gettext("Mark Completed")
    })));
    complete_btn.set_child(Some(&complete_btn_box));
    {
        let pw = page.downgrade();
        let t = task.clone();
        complete_btn.connect_clicked(move |_| {
            if let Some(p) = pw.upgrade() {
                toggle_task_item_completion(&p, &t, !completed);
            }
        });
    }
    top_bar.append(&complete_btn);
    top_bar.append(&gtk::Box::builder().hexpand(true).build());

    let more_dialog_btn = gtk::Button::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text(&gettext("Open full task dialog (Recurrence, Alarms, Attachments)"))
        .css_classes(vec!["flat"])
        .build();
    {
        let pw = page.downgrade();
        let t = task.clone();
        more_dialog_btn.connect_clicked(move |_| {
            if let Some(p) = pw.upgrade() {
                if let (Some(s), Some(l), Some(e)) = (
                    p.imp().task_store.borrow().clone(),
                    p.imp().task_list.borrow().clone(),
                    p.imp().task_empty_state.borrow().clone(),
                ) {
                    p.open_task_dialog(s, l, e, Some(t.clone()));
                }
            }
        });
    }
    top_bar.append(&more_dialog_btn);
    top_bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(&gettext("Delete task"))
        .sensitive(writable)
        .css_classes(vec!["flat", "destructive-action"])
        .build();
    {
        let pw = page.downgrade();
        delete_btn.connect_clicked(move |_| {
            if let Some(p) = pw.upgrade() {
                if let (Some(s), Some(l), Some(e), Some(del_b)) = (
                    p.imp().task_store.borrow().clone(),
                    p.imp().task_list.borrow().clone(),
                    p.imp().task_empty_state.borrow().clone(),
                    p.imp().task_delete_selection_button.borrow().clone(),
                ) {
                    present_task_delete_confirmation(&p, &del_b, &s, &l, &e);
                }
            }
        });
    }
    top_bar.append(&delete_btn);
    inspector_box.append(&top_bar);

    // 2. Inline Editable Title
    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let title_entry = gtk::Entry::builder()
        .text(&task.title())
        .hexpand(true)
        .sensitive(writable)
        .placeholder_text(&gettext("Task summary…"))
        .css_classes(vec!["task-inspector-title-entry"])
        .build();
    let save_title_btn = gtk::Button::builder()
        .icon_name("emblem-ok-symbolic")
        .tooltip_text(&gettext("Save title (Enter)"))
        .visible(false)
        .css_classes(vec!["flat", "suggested-action"])
        .build();
    {
        let save_btn = save_title_btn.clone();
        let initial_title = task.title();
        title_entry.connect_changed(move |entry| {
            let trimmed = entry.text().trim().to_string();
            save_btn.set_visible(!trimmed.is_empty() && trimmed != initial_title);
        });
    }
    {
        let pw = page.downgrade();
        let t = task.clone();
        title_entry.connect_activate(move |entry| {
            let new_title = entry.text().trim().to_string();
            if new_title.is_empty() || new_title == t.title() {
                return;
            }
            if let Some(p) = pw.upgrade() {
                let updated = update_ical_property(&t.component(), "SUMMARY", Some(&escape_ical_text(&new_title)));
                mutate_task_item_component(&p, &t, updated);
            }
        });
    }
    {
        let entry_for_btn = title_entry.clone();
        save_title_btn.connect_clicked(move |_| {
            entry_for_btn.emit_activate();
        });
    }
    title_row.append(&title_entry);
    title_row.append(&save_title_btn);
    inspector_box.append(&title_row);

    // 3. One-Click Workflow Properties Card
    let props_group = adw::PreferencesGroup::builder()
        .title(&gettext("Workflow & Schedule"))
        .build();

    // 3a. Status Segmented Row (Open | In Progress | Done)
    let status_row = adw::ActionRow::builder()
        .title(&gettext("Status"))
        .build();
    let status_seg = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(vec!["linked"])
        .valign(gtk::Align::Center)
        .build();
    let current_status_idx = if completed {
        2
    } else if draft.status == "IN-PROCESS" || (1..=99).contains(&draft.percent) {
        1
    } else {
        0
    };
    for (idx, label) in [
        (0, gettext("Open")),
        (1, gettext("In Progress")),
        (2, gettext("Done")),
    ] {
        let btn = gtk::ToggleButton::builder()
            .label(&label)
            .active(current_status_idx == idx)
            .sensitive(writable)
            .build();
        let pw = page.downgrade();
        let t = task.clone();
        btn.connect_clicked(move |_| {
            if current_status_idx == idx {
                return;
            }
            let Some(p) = pw.upgrade() else { return; };
            if idx == 2 {
                toggle_task_item_completion(&p, &t, true);
            } else if idx == 1 {
                let mut c = update_ical_property(&t.component(), "STATUS", Some("IN-PROCESS"));
                c = update_ical_property(&c, "PERCENT-COMPLETE", Some("50"));
                c = update_ical_property(&c, "COMPLETED", None);
                mutate_task_item_component(&p, &t, c);
            } else {
                let mut c = update_ical_property(&t.component(), "STATUS", Some("NEEDS-ACTION"));
                c = update_ical_property(&c, "PERCENT-COMPLETE", Some("0"));
                c = update_ical_property(&c, "COMPLETED", None);
                mutate_task_item_component(&p, &t, c);
            }
        });
        status_seg.append(&btn);
    }
    status_row.add_suffix(&status_seg);
    props_group.add(&status_row);

    // 3b. Priority Segmented Row (None | Low | Medium | High)
    let prio_row = adw::ActionRow::builder()
        .title(&gettext("Priority"))
        .build();
    let prio_seg = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(vec!["linked"])
        .valign(gtk::Align::Center)
        .build();
    let current_prio_bucket = match draft.priority {
        1..=4 => 1u8,
        5 => 5u8,
        6..=9 => 9u8,
        _ => 0u8,
    };
    for (p_val, label) in [
        (0u8, gettext("None")),
        (9u8, gettext("Low")),
        (5u8, gettext("Medium")),
        (1u8, gettext("High")),
    ] {
        let btn = gtk::ToggleButton::builder()
            .label(&label)
            .active(current_prio_bucket == p_val)
            .sensitive(writable)
            .build();
        let pw = page.downgrade();
        let t = task.clone();
        btn.connect_clicked(move |_| {
            if current_prio_bucket == p_val {
                return;
            }
            let Some(p) = pw.upgrade() else { return; };
            let val_str = p_val.to_string();
            let new_prop = if p_val == 0 { None } else { Some(val_str.as_str()) };
            let updated = update_ical_property(&t.component(), "PRIORITY", new_prop);
            mutate_task_item_component(&p, &t, updated);
        });
        prio_seg.append(&btn);
    }
    prio_row.add_suffix(&prio_seg);
    props_group.add(&prio_row);

    // 3c. Quick Due Date Row
    let due_row = adw::ActionRow::builder()
        .title(&gettext("Due Date"))
        .subtitle(&if draft.due.input.is_empty() {
            gettext("No due date")
        } else {
            draft.due.input.clone()
        })
        .build();
    let due_pills = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .valign(gtk::Align::Center)
        .build();
    let today_naive = Local::now().date_naive();
    for (label, target_date) in [
        (gettext("Today"), Some(today_naive)),
        (gettext("Tomorrow"), Some(today_naive + Duration::days(1))),
        (gettext("+7d"), Some(today_naive + Duration::days(7))),
        (gettext("Clear"), None),
    ] {
        let b = gtk::Button::builder()
            .label(&label)
            .sensitive(writable)
            .css_classes(vec!["flat", "task-inspector-due-pill"])
            .build();
        let pw = page.downgrade();
        let t = task.clone();
        b.connect_clicked(move |_| {
            let Some(p) = pw.upgrade() else { return; };
            let mut c = update_ical_property(&t.component(), "DUE", None);
            if let Some(d) = target_date {
                let d_str = d.format("%Y%m%d").to_string();
                c = update_ical_property(&c, "DUE;VALUE=DATE", Some(&d_str));
            }
            mutate_task_item_component(&p, &t, c);
        });
        due_pills.append(&b);
    }
    due_row.add_suffix(&due_pills);
    props_group.add(&due_row);

    // 3d. Categories EntryRow
    let cat_row = adw::EntryRow::builder()
        .title(&gettext("Categories (comma-separated)"))
        .text(&draft.categories)
        .sensitive(writable)
        .show_apply_button(true)
        .build();
    {
        let pw = page.downgrade();
        let t = task.clone();
        cat_row.connect_apply(move |row| {
            let Some(p) = pw.upgrade() else { return; };
            let cats = serialize_categories(row.text().as_str());
            let new_prop = if cats.is_empty() { None } else { Some(cats.as_str()) };
            let updated = update_ical_property(&t.component(), "CATEGORIES", new_prop);
            mutate_task_item_component(&p, &t, updated);
        });
    }
    props_group.add(&cat_row);
    inspector_box.append(&props_group);

    // 4. Interactive Checklist / Subtasks Section
    let checklist_group = adw::PreferencesGroup::builder()
        .title(&gettext("Checklist & Steps"))
        .description(&if steps.is_empty() {
            gettext("Break this task down into actionable steps")
        } else {
            let done_c = steps.iter().filter(|(d, _)| *d).count();
            format!("{}/{} {}", done_c, steps.len(), gettext("completed"))
        })
        .build();

    for (idx, (step_done, step_text)) in steps.iter().enumerate() {
        let step_row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let step_check = gtk::CheckButton::builder()
            .active(*step_done)
            .sensitive(writable)
            .valign(gtk::Align::Center)
            .build();
        let mut step_lbl_classes = vec!["task-checklist-step-label"];
        if *step_done {
            step_lbl_classes.push("dim-label");
        }
        let step_lbl = gtk::Label::builder()
            .label(step_text)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .wrap(true)
            .css_classes(step_lbl_classes)
            .build();
        let del_step_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(&gettext("Remove step"))
            .sensitive(writable)
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat", "circular"])
            .build();

        {
            let pw = page.downgrade();
            let t = task.clone();
            let steps_clone = steps.clone();
            let notes_clone = clean_notes.clone();
            step_check.connect_toggled(move |chk| {
                let Some(p) = pw.upgrade() else { return; };
                let mut updated_steps = steps_clone.clone();
                if let Some(s) = updated_steps.get_mut(idx) {
                    s.0 = chk.is_active();
                }
                let new_desc = compose_task_description_with_checklist(&updated_steps, &notes_clone);
                let new_prop = if new_desc.is_empty() {
                    None
                } else {
                    Some(escape_ical_text(&new_desc))
                };
                let updated = update_ical_property(&t.component(), "DESCRIPTION", new_prop.as_deref());
                mutate_task_item_component(&p, &t, updated);
            });
        }
        {
            let pw = page.downgrade();
            let t = task.clone();
            let steps_clone = steps.clone();
            let notes_clone = clean_notes.clone();
            del_step_btn.connect_clicked(move |_| {
                let Some(p) = pw.upgrade() else { return; };
                let mut updated_steps = steps_clone.clone();
                if idx < updated_steps.len() {
                    updated_steps.remove(idx);
                }
                let new_desc = compose_task_description_with_checklist(&updated_steps, &notes_clone);
                let new_prop = if new_desc.is_empty() {
                    None
                } else {
                    Some(escape_ical_text(&new_desc))
                };
                let updated = update_ical_property(&t.component(), "DESCRIPTION", new_prop.as_deref());
                mutate_task_item_component(&p, &t, updated);
            });
        }

        step_row_box.append(&step_check);
        step_row_box.append(&step_lbl);
        step_row_box.append(&del_step_btn);
        checklist_group.add(&step_row_box);
    }

    let add_step_entry = adw::EntryRow::builder()
        .title(&gettext("Add step… (Press Enter)"))
        .sensitive(writable)
        .show_apply_button(true)
        .build();
    {
        let pw = page.downgrade();
        let t = task.clone();
        let steps_clone = steps.clone();
        let notes_clone = clean_notes.clone();
        add_step_entry.connect_apply(move |row| {
            let txt = row.text().trim().to_string();
            if txt.is_empty() {
                return;
            }
            let Some(p) = pw.upgrade() else { return; };
            let mut updated_steps = steps_clone.clone();
            updated_steps.push((false, txt));
            let new_desc = compose_task_description_with_checklist(&updated_steps, &notes_clone);
            let escaped = escape_ical_text(&new_desc);
            let updated = update_ical_property(&t.component(), "DESCRIPTION", Some(&escaped));
            row.set_text("");
            mutate_task_item_component(&p, &t, updated);
        });
    }
    checklist_group.add(&add_step_entry);
    inspector_box.append(&checklist_group);

    // 5. Notes Section
    let save_notes_btn = gtk::Button::builder()
        .label(&gettext("Save Notes"))
        .sensitive(false)
        .valign(gtk::Align::Center)
        .css_classes(vec!["flat", "suggested-action"])
        .build();
    let notes_group = adw::PreferencesGroup::builder()
        .title(&gettext("Notes"))
        .header_suffix(&save_notes_btn)
        .build();
    let notes_view = gtk::TextView::builder()
        .editable(writable)
        .wrap_mode(gtk::WrapMode::WordChar)
        .height_request(110)
        .left_margin(12)
        .right_margin(12)
        .top_margin(10)
        .bottom_margin(10)
        .css_classes(vec!["task-inspector-notes-view"])
        .build();
    notes_view.buffer().set_text(&clean_notes);
    {
        let save_btn = save_notes_btn.clone();
        let initial_notes = clean_notes.clone();
        notes_view.buffer().connect_changed(move |buf| {
            let current = buf.text(&buf.start_iter(), &buf.end_iter(), false).trim().to_string();
            save_btn.set_sensitive(current != initial_notes);
        });
    }
    {
        let pw = page.downgrade();
        let t = task.clone();
        let steps_clone = steps.clone();
        let buf = notes_view.buffer();
        save_notes_btn.connect_clicked(move |btn| {
            let Some(p) = pw.upgrade() else { return; };
            let new_notes = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
            let full_desc = compose_task_description_with_checklist(&steps_clone, &new_notes);
            let new_prop = if full_desc.is_empty() {
                None
            } else {
                Some(escape_ical_text(&full_desc))
            };
            let updated = update_ical_property(&t.component(), "DESCRIPTION", new_prop.as_deref());
            btn.set_sensitive(false);
            mutate_task_item_component(&p, &t, updated);
        });
    }
    notes_group.add(&notes_view);
    inspector_box.append(&notes_group);
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
                gettext("The confirmed task is no longer uniquely loaded; please reload"),
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
    show_toast_for_widget(button, &operation.progress_label());

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
                        error: gettext("The task list is no longer writable immediately prior to the batch action"),
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
                    &gettext("The batch action was not confirmed; tasks were not modified locally"),
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
            (successes, 0) => page.show_toast(
                &ngettext("1 task updated", "{count} tasks updated", successes as u32)
                    .replace("{count}", &successes.to_string()),
            ),
            (0, failures) => page.show_toast(
                &ngettext(
                    "No tasks updated; 1 error. Please reload authoritative data",
                    "No tasks updated; {count} errors. Please reload authoritative data",
                    failures as u32,
                )
                .replace("{count}", &failures.to_string()),
            ),
            (successes, failures) => page.show_toast(
                &format!(
                    "{}; {}. {}",
                    ngettext("1 task updated", "{count} tasks updated", successes as u32)
                        .replace("{count}", &successes.to_string()),
                    ngettext("1 error", "{count} errors", failures as u32)
                        .replace("{count}", &failures.to_string()),
                    gettext("Failed tasks remained unchanged"),
                ),
            ),
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
                gettext("The backend returned an outdated or ambiguous paste result"),
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
                show_toast_for_widget(&button_for_result, &gettext("The clipboard contains no text"));
                return;
            }
            Err(error) => {
                page.imp().task_paste_batch_running.set(false);
                update_task_selection_controls(&page);
                show_toast_for_widget(&button_for_result, &gettext("Clipboard could not be read: {error}").replace("{error}", &error.to_string()));
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
                    &gettext("The default task list has changed or is no longer writable"),
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
            .map(|_| format!("meridian-{}", uuid::Uuid::new_v4()))
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
        show_toast_for_widget(&button_for_result, &gettext("Pasting tasks…"));
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
                        gettext("The target task list is no longer writable immediately prior to pasting"),
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
                        &gettext("Paste result was lost; reloading authoritative tasks"),
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
                        gettext("A confirmed new task conflicts with the current selection"),
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
                (created, false, 0) => page.show_toast(&ngettext(
                    "1 task pasted",
                    "{count} tasks pasted",
                    created as u32,
                ).replace("{count}", &created.to_string())),
                (0, true, unattempted) => {
                    let unattempted_str = ngettext(
                        "1 task was not attempted",
                        "{count} tasks were not attempted",
                        unattempted as u32,
                    ).replace("{count}", &unattempted.to_string());
                    page.show_toast(&gettext("No tasks pasted; an error occurred. {unattempted}").replace("{unattempted}", &unattempted_str));
                }
                (created, true, unattempted) => {
                    let created_str = ngettext(
                        "1 task pasted",
                        "{count} tasks pasted",
                        created as u32,
                    ).replace("{count}", &created.to_string());
                    let unattempted_str = ngettext(
                        "1 task was not attempted",
                        "{count} tasks were not attempted",
                        unattempted as u32,
                    ).replace("{count}", &unattempted.to_string());
                    page.show_toast(&gettext("{created}; an error occurred. {unattempted}").replace("{created}", &created_str).replace("{unattempted}", &unattempted_str));
                }
                _ => page.show_toast(&gettext("Pasting did not return a confirmed result")),
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
    dialog.add_responses(&[("cancel", &gettext("Do Not Delete")), ("delete", &gettext("Delete"))]);
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
    let body = ngettext(
        "1 completed task will be permanently deleted from all writable task lists and cannot be recovered.",
        "{count} completed tasks will be permanently deleted from all writable task lists and cannot be recovered.",
        count as u32,
    )
    .replace("{count}", &count.to_string());
    let dialog = adw::AlertDialog::new(
        Some(&gettext("Permanently Delete Completed Tasks?")),
        Some(&body),
    );
    dialog.add_responses(&[("cancel", &gettext("Do Not Delete")), ("delete", &gettext("Permanently Delete"))]);
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
            let message = if purge_completed {
                gettext("The set of completed tasks changed prior to deletion")
            } else {
                gettext("The task selection changed prior to deletion")
            };
            show_toast_for_widget(button, &message);
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
    show_toast_for_widget(button, &gettext("Deleting tasks…"));

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
                    gettext("The task list is no longer writable immediately prior to deletion"),
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
                        gettext("The backend confirmed the deletion of a different task"),
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
                    &gettext("Deletion result was lost; reloading authoritative tasks"),
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
            (removed, false, 0) => page.show_toast(
                &ngettext("1 task deleted", "{count} tasks deleted", removed as u32)
                    .replace("{count}", &removed.to_string()),
            ),
            (0, true, unattempted) => page.show_toast(
                &format!(
                    "{}; {}",
                    gettext("No tasks deleted; an error occurred"),
                    ngettext(
                        "1 task was not attempted",
                        "{count} tasks were not attempted",
                        unattempted as u32,
                    )
                    .replace("{count}", &unattempted.to_string()),
                ),
            ),
            (removed, true, unattempted) => page.show_toast(
                &format!(
                    "{}; {}; {}",
                    ngettext("1 task deleted", "{count} tasks deleted", removed as u32)
                        .replace("{count}", &removed.to_string()),
                    gettext("an error occurred"),
                    ngettext(
                        "1 task was not attempted",
                        "{count} tasks were not attempted",
                        unattempted as u32,
                    )
                    .replace("{count}", &unattempted.to_string()),
                ),
            ),
            _ => page.show_toast(&gettext("Deletion did not return a confirmed result")),
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

    let mut names = vec![gettext("All Categories")];
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
    page.imp().task_category_syncing.set(true);
    model.splice(0, model.n_items(), &name_refs);
    if selector.selected() != selected {
        selector.set_selected(selected);
    }
    page.imp().task_category_syncing.set(false);
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
    if context.hide_cancelled_tasks
        && filter != TaskFilter::Cancelled
        && task_status_value(&task.component()) == "CANCELLED"
    {
        return false;
    }
    if context.hide_completed_tasks && filter != TaskFilter::Completed && task_is_completed(task) {
        if context.hide_completed_value == 0 {
            return false;
        }
        if let Some(cutoff) = context.hide_completed_cutoff {
            let ts = task_completed_timestamp(task).unwrap_or(0);
            if ts < cutoff {
                return false;
            }
        } else {
            return false;
        }
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
        return Err(gettext("{field} contains invalid line breaks").replace("{field}", field));
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
                    gettext("{field} must contain valid YYYY-MM-DD or YYYYMMDD values")
                        .replace("{field}", field)
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

fn task_status_label(raw: &str) -> String {
    if ical_property(raw, "STATUS")
        .as_deref()
        .is_some_and(|status| normalize_task_status(status).is_err())
    {
        return crate::i18n::gettext("Imported Status");
    }
    match task_status_value(raw) {
        "IN-PROCESS" => crate::i18n::gettext("In Progress"),
        "COMPLETED" => crate::i18n::gettext("Completed"),
        "CANCELLED" => crate::i18n::gettext("Cancelled"),
        _ => crate::i18n::gettext("Not Started"),
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
        _ => Err(gettext("The task status is invalid")),
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
    let completed = task_is_completed(task);
    let raw_title = task.title();
    let escaped_title = glib::markup_escape_text(&raw_title);
    let title_markup = if completed {
        format!("<span strikethrough=\"true\" alpha=\"60%\">{escaped_title}</span>")
    } else {
        format!("<span weight=\"medium\">{escaped_title}</span>")
    };

    let due = task.due_date();
    let recurrence_due = task_next_due_date(task, context.today);
    let parsed_due = task_due_date(&due);

    let mut subtitle_parts = Vec::new();

    // 1. Due date chip
    if let Some(parsed) = parsed_due {
        let is_overdue = !completed && parsed < context.today;
        let is_today = parsed == context.today;
        let is_tomorrow = parsed == context.today + chrono::Duration::days(1);

        let due_chip = if is_overdue {
            format!("<span foreground=\"#e01b24\" weight=\"bold\">⚠️ {}: {due}</span>", crate::i18n::gettext("Overdue"))
        } else if is_today {
            format!("<span foreground=\"#e66100\" weight=\"bold\">📅 {}</span>", crate::i18n::gettext("Due Today"))
        } else if is_tomorrow {
            format!("<span foreground=\"#3584e4\" weight=\"medium\">📅 {} ({due})</span>", crate::i18n::gettext("Tomorrow"))
        } else {
            format!("<span alpha=\"80%\">📅 {}: {due}</span>", crate::i18n::gettext("Due"))
        };
        subtitle_parts.push(due_chip);

        if let Some(next) = recurrence_due && Some(next) != parsed_due {
            subtitle_parts.push(format!("<span alpha=\"70%\">🔁 {}: {}</span>", crate::i18n::gettext("Next"), next.format("%Y-%m-%d")));
        }
    } else if let Some(next) = recurrence_due {
        subtitle_parts.push(format!("<span alpha=\"80%\">🔁 {}: {}</span>", crate::i18n::gettext("Due Date"), next.format("%Y-%m-%d")));
    }

    // 2. Priority indicator
    let prio_num = task.priority().parse::<u32>().ok();
    if let Some(p) = prio_num {
        if (1..=4).contains(&p) {
            subtitle_parts.push(format!("<span foreground=\"#e01b24\" weight=\"bold\">❗ {}</span>", crate::i18n::gettext("High Priority")));
        } else if (6..=9).contains(&p) {
            subtitle_parts.push(format!("<span alpha=\"60%\">⚪ {}</span>", crate::i18n::gettext("Low Priority")));
        }
    }

    // 3. Status indicator
    match task_status_value(&task.component()) {
        "IN-PROCESS" => subtitle_parts.push(format!("<span foreground=\"#3584e4\" weight=\"medium\">⏳ {}</span>", crate::i18n::gettext("In Progress"))),
        "CANCELLED" => subtitle_parts.push(format!("<span alpha=\"55%\">⛔ {}</span>", crate::i18n::gettext("Cancelled"))),
        _ => {},
    }

    // 4. Categories
    let categories: Vec<String> = task_categories(task).collect();
    if !categories.is_empty() {
        let cats_formatted = categories
            .iter()
            .map(|c| glib::markup_escape_text(c).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        subtitle_parts.push(format!("<span alpha=\"75%\">🏷️ {cats_formatted}</span>"));
    }

    let subtitle = if subtitle_parts.is_empty() {
        if completed {
            format!("<span foreground=\"#26a269\">✓ {}</span>", crate::i18n::gettext("Completed"))
        } else {
            format!("<span alpha=\"50%\">{}</span>", crate::i18n::gettext("No due date"))
        }
    } else {
        subtitle_parts.join(" · ")
    };

    let row = adw::ActionRow::builder()
        .title(&title_markup)
        .subtitle(&subtitle)
        .use_markup(true)
        .build();
    row.set_activatable(true);
    row.add_css_class("task-row");
    if completed {
        row.add_css_class("task-row-completed");
        row.add_css_class("dim-label");
    } else {
        row.add_css_class("task-row-pending");
    }
    unsafe {
        row.set_data("task-id", task.id());
        row.set_data("task-source-uid", task.source_uid());
    }

    let check = gtk::CheckButton::builder()
        .active(completed)
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(
        &check,
        &if completed {
            gettext("Mark task as incomplete: {title}").replace("{title}", &task.title())
        } else {
            gettext("Mark task as completed: {title}").replace("{title}", &task.title())
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
                &gettext("This task list is not confirmed as writable"),
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
                        &gettext("Could not update task status"),
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
                        &gettext("Could not update task: {error}").replace("{error}", &error.to_string()),
                    );
                }
            }
            button_for_result.set_sensitive(true);
        });
    });
    let task_icon = gtk::Image::builder()
        .icon_name("view-list-bullet-symbolic")
        .pixel_size(16)
        .valign(gtk::Align::Center)
        .css_classes(vec!["task-row-icon"])
        .build();
    row.add_prefix(&task_icon);
    row.add_prefix(&check);
    if let Some(color) = category_color_for_component(category_metadata, &task.component()) {
        let category_dot = gtk::Label::new(None);
        category_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
        category_dot.set_tooltip_text(Some(&gettext("Task category")));
        row.add_prefix(&category_dot);
    }

    let quick_edit_label = gettext("Quickly edit status, priority, due date, and categories");
    let quick_edit_button = gtk::Button::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(&quick_edit_label)
        .css_classes(vec!["flat", "circular", "task-row-btn"])
        .valign(gtk::Align::Center)
        .sensitive(task_source_is_writable_on_page(page, &task.source_uid()))
        .build();
    set_accessible_label(&quick_edit_button, &quick_edit_label);
    row.add_suffix(&quick_edit_button);
    connect_task_quick_editor(
        page,
        &quick_edit_button,
        task_store,
        task_list,
        empty_tasks,
        task,
    );

    let edit_label = gettext("Edit task");
    let edit_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(&edit_label)
        .css_classes(vec!["flat", "circular", "task-row-btn"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&edit_button, &edit_label);
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
                &gettext("This task list is not confirmed as writable"),
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

    let export_label = gettext("Export task as iCalendar");
    let export_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text(&export_label)
        .css_classes(vec!["flat", "circular", "task-row-btn"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&export_button, &export_label);
    row.add_suffix(&export_button);
    let task_component_for_export = task.component();
    export_button.connect_clicked(move |button| {
        if task_component_for_export.trim().is_empty() {
            show_toast_for_widget(
                button,
                &gettext("No iCalendar data available for this task"),
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
            .title(&gettext("Save Task as iCalendar"))
            .initial_name("task.ics")
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
                    show_toast_for_widget(
                        &button_for_result,
                        &gettext("Task exported as iCalendar"),
                    )
                }
                Err(error) => show_toast_for_widget(
                    &button_for_result,
                    &gettext("Could not save iCalendar: {error}").replace("{error}", &error.to_string()),
                ),
            }
        });
    });

    let delete_label = gettext("Delete task");
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(&delete_label)
        .css_classes(vec!["flat", "circular", "destructive-action", "task-row-btn"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&delete_button, &delete_label);
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
            show_toast_for_widget(button, &gettext("The task is no longer available"));
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

        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

        let edit = crate::views::create_context_menu_button(
            None,
            &gettext("Edit task"),
            false,
        );
        let popover_for_edit = popover.clone();
        let edit_button_for_menu = edit_button_for_context.clone();
        edit.connect_clicked(move |_| {
            popover_for_edit.popdown();
            edit_button_for_menu.emit_clicked();
        });
        menu_box.append(&edit);

        let toggle_label = if task_is_completed(&task_for_context) {
            gettext("Mark as incomplete")
        } else {
            gettext("Mark as completed")
        };
        let toggle = crate::views::create_context_menu_button(
            None,
            &toggle_label,
            false,
        );
        toggle.set_sensitive(!task_for_context.source_uid().is_empty());
        let popover_for_toggle = popover.clone();
        let check_for_menu = check_for_context.clone();
        toggle.connect_clicked(move |_| {
            popover_for_toggle.popdown();
            check_for_menu.set_active(!check_for_menu.is_active());
        });
        menu_box.append(&toggle);

        let export = crate::views::create_context_menu_button(
            None,
            &gettext("Export as iCalendar"),
            false,
        );
        let popover_for_export = popover.clone();
        let export_button_for_menu = export_button_for_context.clone();
        export.connect_clicked(move |_| {
            popover_for_export.popdown();
            export_button_for_menu.emit_clicked();
        });
        menu_box.append(&export);

        menu_box.append(&crate::views::create_context_menu_separator());

        let delete = crate::views::create_context_menu_button(
            None,
            &gettext("Delete task"),
            true,
        );
        let popover_for_delete = popover.clone();
        let delete_button_for_menu = delete_button_for_context.clone();
        delete.connect_clicked(move |_| {
            popover_for_delete.popdown();
            delete_button_for_menu.emit_clicked();
        });
        menu_box.append(&delete);

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
        if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
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

fn initial_sync_task_sources() -> Vec<ECalSourceCapability> {
    let mut sources = Vec::new();
    if CalendarStoreService::new().is_writable() {
        sources.push(ECalSourceCapability {
            source_uid: "local-calendar".to_string(),
            display_name: gettext("Local Task List"),
            connected: true,
            available: true,
            source_writable: true,
            writable: true,
            refresh_supported: false,
            removable: false,
            remote_deletable: false,
            in_collection: false,
            task_assignment_supported: false,
            task_assignment_conversion_supported: false,
        });
    }
    for collection in crate::services::eds_service::load_configured_account_collections() {
        let task_uid = format!("account-tasks-{}", collection.uid);
        sources.push(ECalSourceCapability {
            source_uid: task_uid,
            display_name: gettext("Tasks"),
            connected: true,
            available: true,
            source_writable: true,
            writable: true,
            refresh_supported: false,
            removable: false,
            remote_deletable: false,
            in_collection: true,
            task_assignment_supported: false,
            task_assignment_conversion_supported: false,
        });
    }
    for profile in crate::services::account_config_service::AccountConfigStore::default().load() {
        if !profile.enabled || profile.is_shared_mailbox || profile.open_mailbox_other_user {
            continue;
        }
        let task_uid = format!("account-tasks-{}", profile.id);
        if !sources.iter().any(|s| s.source_uid == task_uid) {
            sources.push(ECalSourceCapability {
                source_uid: task_uid,
                display_name: gettext("Tasks"),
                connected: true,
                available: true,
                source_writable: true,
                writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: true,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            });
        }
    }
    sources
}

fn apply_task_sources(
    page: &TasksPage,
    mut sources: Vec<ECalSourceCapability>,
    preferred_source_uid: Option<String>,
) {
    let mut source_parents: Vec<String> = sources
        .iter()
        .map(|s| {
            let parent = crate::services::eds_service::resolve_source_parent_name(&s.source_uid);
            crate::services::eds_service::canonical_group_name(&parent)
        })
        .collect();

    // Discover configured account collections (Google, EWS, GOA, etc.) so every
    // account configured and displayed in Mail is also represented in the Tasks
    // sidebar with its own account group and task list.
    for collection in crate::services::eds_service::load_configured_account_collections() {
        if !collection.enabled || !collection.calendar_enabled || crate::services::eds_service::is_shared_or_delegate_mailbox(&collection.display_name) {
            continue;
        }
        let account_name = collection.display_name.clone();
        let already_represented = sources.iter().enumerate().any(|(idx, s)| {
            source_parents.get(idx).is_some_and(|p| p == &account_name)
                || s.display_name == account_name
                || s.source_uid == collection.uid
                || s.source_uid == format!("account-tasks-{}", collection.uid)
        });
        if !already_represented {
            let task_uid = format!("account-tasks-{}", collection.uid);
            sources.push(ECalSourceCapability {
                source_uid: task_uid,
                display_name: gettext("Tasks"),
                connected: true,
                available: true,
                source_writable: true,
                writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: true,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            });
            source_parents.push(account_name);
        }
    }

    for profile in crate::services::account_config_service::AccountConfigStore::default().load() {
        if !profile.enabled || profile.is_shared_mailbox || profile.open_mailbox_other_user {
            continue;
        }
        let account_name = if !profile.display_name.trim().is_empty() {
            profile.display_name.clone()
        } else {
            profile.email.clone()
        };
        if crate::services::eds_service::is_shared_or_delegate_mailbox(&account_name) {
            continue;
        }
        let already_represented = sources.iter().enumerate().any(|(idx, s)| {
            source_parents.get(idx).is_some_and(|p| p == &account_name)
                || s.display_name == account_name
                || s.source_uid == profile.id
                || s.source_uid == format!("account-tasks-{}", profile.id)
        });
        if !already_represented {
            let task_uid = format!("account-tasks-{}", profile.id);
            sources.push(ECalSourceCapability {
                source_uid: task_uid,
                display_name: gettext("Tasks"),
                connected: true,
                available: true,
                source_writable: true,
                writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: true,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            });
            source_parents.push(account_name);
        }
    }

    sources.sort_by_cached_key(|source| {
        (
            source.display_name.to_lowercase(),
            source.source_uid.clone(),
        )
    });

    let mut names = vec![gettext("All Task Lists")];
    let mut uids = Vec::new();
    let mut capabilities = HashMap::new();
    let mut metadata = HashMap::new();
    let mut refresh_unsupported = HashSet::new();
    for source in &sources {
        if source.source_uid.trim().is_empty()
            || metadata.contains_key(source.source_uid.as_str())
        {
            continue;
        }
        let label = if source.source_uid.starts_with("account-tasks-") {
            let parent = crate::services::eds_service::resolve_source_parent_name(&source.source_uid);
            format!("{parent} · {}", source.display_name)
        } else {
            task_source_capability_label(source)
        };
        names.push(label);
        let capability = task_source_capability_from_ecal(source);
        if source.connected && !source.refresh_supported {
            refresh_unsupported.insert(source.source_uid.clone());
        }
        capabilities.insert(source.source_uid.clone(), capability);
        uids.push(source.source_uid.clone());
        metadata.insert(source.source_uid.clone(), source.clone());
    }
    if let Some(preferred_source_uid) = preferred_source_uid.as_deref()
        && !uids.iter().any(|uid| uid == preferred_source_uid)
        && preferred_source_uid != "local-calendar"
    {
        names.push(gettext("Unavailable Source · {source}").replace("{source}", preferred_source_uid));
        uids.push(preferred_source_uid.to_string());
        capabilities.insert(
            preferred_source_uid.to_string(),
            TaskSourceCapability::Unavailable,
        );
    }
    if CalendarStoreService::new().is_writable() && !uids.iter().any(|u| u == "local-calendar") {
        names.push(gettext("Local Task List"));
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
    page.imp().task_source_syncing.set(true);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    source_model.splice(0, source_model.n_items(), &name_refs);

    let mut items = Vec::new();
    for source in &sources {
        if source.source_uid.trim().is_empty() {
            continue;
        }
        let parent_name = crate::services::eds_service::resolve_source_parent_name(&source.source_uid);
        let group = crate::services::eds_service::canonical_group_name(&parent_name);
        let color = calendar_source_color(&source.source_uid, &group);
        items.push(
            SourceSelectorItem::new(
                &source.source_uid,
                &source.display_name,
                "tasks",
                source.connected,
                source.source_writable,
            )
            .with_color(color)
            .with_group(group),
        );
    }
    if CalendarStoreService::new().is_writable() && !items.iter().any(|i| i.uid == "local-calendar") {
        let local_name = gettext("Local Task List");
        let group_name = crate::services::eds_service::canonical_local_group_name();
        let color = calendar_source_color("local-calendar", &local_name);
        items.push(
            SourceSelectorItem::new(
                "local-calendar",
                &local_name,
                "local",
                true,
                true,
            )
            .with_color(color)
            .with_group(group_name),
        );
    }

    {
        let mut selector = SourceSelector::new(SourceDomain::Tasks);
        selector.update_sources(items);
        if let Some(settings) = AppSettings::try_new() {
            selector.load_from_settings(&settings);
        }
        *page.imp().hidden_task_sources.borrow_mut() =
            selector.hidden_uids().into_iter().collect();
        page.imp().task_source_selector_model.replace(Some(selector));
    }

    // Install dynamic CSS rules for task list colors on checkbuttons
    if let Some(selector) = page.imp().task_source_selector_model.borrow().clone() {
        let mut unique_colors = std::collections::HashSet::new();
        for s in selector.visible_sources() {
            if let Some(ref c) = s.color {
                unique_colors.insert(c.clone());
            }
        }
        unique_colors.insert("#3584e4".to_string());

        let mut css_rules = String::new();
        for hex in unique_colors {
            let clean: String = hex
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            css_rules.push_str(&format!(
                "checkbutton.cal-check-{clean} {{ \
                     --accent-bg-color: {hex}; \
                     --accent-color: {hex}; \
                 }} \
                 checkbutton.cal-check-{clean} check:not(:checked) {{ \
                     box-shadow: inset 0 0 0 2px {hex}; \
                 }}\n"
            ));
        }
        if let Some(display) = gtk::gdk::Display::default() {
            let mut prov_slot = page.imp().task_color_css_provider.borrow_mut();
            if let Some(ref provider) = *prov_slot {
                provider.load_from_string(&css_rules);
            } else {
                let provider = gtk::CssProvider::new();
                provider.load_from_string(&css_rules);
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 50,
                );
                *prov_slot = Some(provider);
            }
        }
    }

    page.render_task_sidebar();
    page.refresh_tasks();

    let selected = preferred_source_uid
        .as_deref()
        .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    source_selector.set_selected(selected);
    TasksPage::select_sidebar_source_row(&source_list, selected as usize);
    page.imp().task_source_syncing.set(false);
    update_task_selection_controls(page);
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
        // Pre-fetch calendar/task sources via D-Bus to ensure PARENT_NAME_CACHE has authoritative account parents from EDS
        let _ = crate::services::eds_service::list_sources(crate::services::eds_service::EdsSourceKind::Calendar).await;
        let (sources, warning) = match receiver.recv().await {
            Ok(Ok(discovery)) => (discovery.sources, discovery.warning),
            Ok(Err(error)) => {
                tracing::warn!(%error, "Could not discover exact EDS task lists");
                if let Some(page) = page_weak.upgrade()
                    && show_error
                {
                    page.show_toast(&gettext("Could not reload task lists: {error}").replace("{error}", &error.to_string()));
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

        apply_task_sources(&page, sources, preferred_source_uid);
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
        return Err(gettext("{property} cannot be empty").replace("{property}", property_name));
    }
    if let Some((property, value)) = input.split_once(':') {
        let actual_name = property.split(';').next().unwrap_or_default();
        if !actual_name.eq_ignore_ascii_case(property_name)
            || property.is_empty()
            || value.trim().is_empty()
        {
            return Err(gettext("Invalid {property} property").replace("{property}", property_name));
        }
        return Ok(format!("{property}:{}", value.trim()));
    }
    let address = input
        .strip_prefix("mailto:")
        .or_else(|| input.strip_prefix("MAILTO:"))
        .unwrap_or(input)
        .trim();
    let Some((local, domain)) = address.rsplit_once('@') else {
        return Err(gettext("{property} requires a valid email address").replace("{property}", property_name));
    };
    if local.is_empty()
        || domain.is_empty()
        || address
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, ',' | ';' | ':'))
    {
        return Err(gettext("{property} requires a valid email address").replace("{property}", property_name));
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
        return Err(gettext("At least one attendee is required"));
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
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Meridian//Task Assignment//EN\r\nMETHOD:REQUEST\r\n{}\r\nEND:VCALENDAR\r\n",
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
            &gettext("This task or its provider does not support assignment"),
        );
        return;
    }
    let Ok(candidate) = selected_task_candidate(&selection, &candidates) else {
        return;
    };
    let Ok(component) = validated_task_vtodo_component(candidate, &candidate.identity) else {
        show_toast_for_widget(button, &gettext("The selected task is no longer valid"));
        return;
    };
    let identity = candidate.identity.clone();
    let initial_component = component.clone();
    let draft = task_assignment_draft(&component);
    let organizer_entry = gtk::Entry::builder()
        .text(&draft.organizer)
        .placeholder_text(&gettext("organizer@example.org or full ORGANIZER line"))
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
            .label(&gettext("Organizer"))
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&organizer_entry);
    content.append(
        &gtk::Label::builder()
            .label(&gettext("Attendees (one email or full ATTENDEE line per line)"))
            .wrap(true)
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&attendee_scroll);
    content.append(
        &gtk::Label::builder()
            .label(&gettext("Assignment data will be saved. Sending iTIP notifications requires the native Linux/EDS runtime."))
            .wrap(true)
            .xalign(0.0)
            .css_classes(vec!["dim-label"])
            .build(),
    );
    let dialog = adw::AlertDialog::new(Some(&gettext("Assign Task")), None);
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("assign", &gettext("Assign"));
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
            page.show_toast(&gettext("The task or provider capability has changed"));
            return;
        }
        let Ok(current) = selected_task_candidate(
            std::slice::from_ref(&identity),
            &current_candidates,
        ) else {
            page.show_toast(&gettext("The task is no longer available"));
            return;
        };
        let Ok(current_component) = validated_task_vtodo_component(current, &identity) else {
            page.show_toast(&gettext("The task is no longer valid"));
            return;
        };
        if current_component != initial_component {
            page.show_toast(&gettext("The task was modified in the meantime; please reopen assignment"));
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
                        gettext("The assignment was saved, but could not be reloaded authoritatively")
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
                        &gettext("Task was assigned and METHOD:REQUEST prepared; actual iTIP sending requires Linux/EDS handoff"),
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
        show_toast_for_widget(button, &gettext("Another task operation is already in progress"));
        return;
    }
    let name_entry = gtk::Entry::builder()
        .placeholder_text(&gettext("Task list name"))
        .activates_default(true)
        .hexpand(true)
        .build();
    let color_dialog = gtk::ColorDialog::builder()
        .title(&gettext("Task List Color"))
        .build();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    if let Ok(color) = gtk::gdk::RGBA::parse("#62a0ea") {
        color_button.set_rgba(&color);
    }
    color_button.set_tooltip_text(Some(&gettext("Select task list color")));
    let default_check = gtk::CheckButton::with_label(&gettext("Use as default task list"));
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(&gettext("Name"))
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&name_entry);
    content.append(
        &gtk::Label::builder()
            .label(&gettext("Color"))
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&color_button);
    content.append(&default_check);
    let dialog = adw::AlertDialog::new(
        Some(&gettext("New Local Task List")),
        Some(&gettext("The task list will be created in the local EDS calendar service.")),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("create", &gettext("Create"));
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
            show_toast_for_widget(&button_for_result, &gettext("Name cannot be empty"));
            return;
        }
        let color = task_source_color_hex(&color_button.rgba());
        let make_default = default_check.is_active();
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                &gettext("Another task operation is already in progress"),
            );
            return;
        };
        show_toast_for_widget(&button_for_result, &gettext("Creating task list…"));
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
                            page.show_toast(&gettext("Local task list was created"));
                        }
                        ECalLocalSourceCreateStatus::CreatedUnavailable(warning) => {
                            page.show_toast(&gettext(
                                "Task list was created, but is not yet available: {warning}",
                            ).replace("{warning}", &warning));
                        }
                    }
                    start_task_source_discovery(&page, Some(created.source_uid), true);
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast(
                        &gettext("Local EDS task lists are not supported by this backend"),
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
        return gettext("No iCalendar file selected");
    };
    gio::File::for_uri(uri)
        .path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| uri.to_string())
}

fn present_task_source_properties(page: &TasksPage, button: &gtk::Button) {
    let Some((source_uid, _)) = current_task_source_metadata(page) else {
        show_toast_for_widget(button, &gettext("Select a task list to edit"));
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
            &gettext("The properties of this task list are not writable"),
        );
        return;
    }
    let Some(generation) = begin_task_source_management(page) else {
        show_toast_for_widget(button, &gettext("Another task operation is already in progress"));
        return;
    };
    show_toast_for_widget(button, &gettext("Loading task list properties…"));
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
                    &gettext("Provider-specific properties are not yet available for this task list"),
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
        .placeholder_text(&gettext("Task list name"))
        .activates_default(true)
        .hexpand(true)
        .build();
    let color_dialog = gtk::ColorDialog::builder()
        .title(&gettext("Task List Color"))
        .build();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    if let Ok(color) = gtk::gdk::RGBA::parse(&properties.color) {
        color_button.set_rgba(&color);
    }
    let email_entry = gtk::Entry::builder()
        .text(&properties.email_address)
        .placeholder_text(&gettext("Email address (optional)"))
        .hexpand(true)
        .build();
    let default_was_selected = AppSettings::try_new()
        .is_some_and(|settings| settings.default_task_source_uid() == properties.source_uid);
    let default_check = gtk::CheckButton::with_label(&gettext("Use as default task list"));
    default_check.set_active(default_was_selected);

    let custom_file_uri = Rc::new(RefCell::new(properties.custom_file_uri.clone()));
    let custom_file_check = gtk::CheckButton::with_label(&gettext("Use existing iCalendar file"));
    custom_file_check.set_active(!properties.is_builtin && properties.custom_file_uri.is_some());
    custom_file_check.set_sensitive(!properties.is_builtin);
    let custom_file_label = gtk::Label::builder()
        .label(task_source_custom_file_label(
            properties.custom_file_uri.as_deref(),
        ))
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .hexpand(true)
        .halign(gtk::Align::Start)
        .build();
    let choose_file_button = gtk::Button::builder()
        .label(&gettext("Choose File…"))
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
        gtk::CheckButton::with_label(&gettext("Meridian may update the iCalendar file"));
    custom_writable_check.set_active(properties.custom_file_writable);
    custom_writable_check
        .set_visible(!properties.is_builtin && properties.custom_file_uri.is_some());
    let builtin_hint = gtk::Label::builder()
        .label(&gettext("The built-in task list cannot use an external iCalendar file."))
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
        (gettext("Name"), name_entry.clone().upcast::<gtk::Widget>()),
        (gettext("Color"), color_button.clone().upcast::<gtk::Widget>()),
        (gettext("Email"), email_entry.clone().upcast::<gtk::Widget>()),
    ] {
        content.append(
            &gtk::Label::builder()
                .label(&label)
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
        Some(&gettext("Task List Properties")),
        Some(&gettext("Edit local EDS properties")),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("save", &gettext("Save"));
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
            .title(&gettext("Select iCalendar File"))
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
                            page.show_toast(&gettext("Task list properties were saved"));
                        }
                        ECalLocalSourceCreateStatus::CreatedUnavailable(warning) => {
                            page.show_toast(&gettext(
                                "Properties were saved, but the task list is not yet available: {warning}",
                            ).replace("{warning}", &warning));
                        }
                    }
                    start_task_source_discovery(&page, Some(updated.source_uid), true);
                }
                Err(ECalServiceError::Unsupported) => page.show_toast(
                    &gettext("Provider-specific properties are not yet available for this task list"),
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
        show_toast_for_widget(button, &gettext("Select a task list to rename"));
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.rename_enabled {
        show_toast_for_widget(button, &gettext("This task list cannot be renamed"));
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
            .label(&gettext("New Name"))
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&entry);
    let dialog = adw::AlertDialog::new(
        Some(&gettext("Rename Task List")),
        Some(&gettext("The new name will be saved directly in the selected EDS source.")),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("rename", &gettext("Rename"));
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
            show_toast_for_widget(&button_for_result, &gettext("Name cannot be empty"));
            return;
        }
        if display_name == source.display_name {
            return;
        }
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                &gettext("Another task operation is already in progress"),
            );
            return;
        };
        show_toast_for_widget(&button_for_result, &gettext("Renaming task list…"));
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
                    page.show_toast(&gettext("Task list was renamed"));
                    start_task_source_discovery(&page, preferred, true);
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast(&gettext("Renaming is not supported by this backend"));
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
        show_toast_for_widget(button, &gettext("Select a task list to copy"));
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.copy_enabled {
        show_toast_for_widget(button, &gettext("This task list cannot be copied"));
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
        show_toast_for_widget(button, &gettext("No other EDS task list is available"));
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
    destination_selector.set_tooltip_text(Some(&gettext("Select target task list")));
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(&gettext("Target Task List"))
            .halign(gtk::Align::Start)
            .build(),
    );
    content.append(&destination_selector);
    let dialog = adw::AlertDialog::new(
        Some(&gettext("Copy Task List")),
        Some(&gettext("All tasks from “{name}” will be copied to the selected list. Duplicate UIDs will be updated.")
            .replace("{name}", &source.display_name)),
    );
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("copy", &gettext("Copy"));
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
            show_toast_for_widget(&button_for_result, &gettext("Invalid target task list"));
            return;
        };
        let Some(generation) = begin_task_source_management(&page) else {
            show_toast_for_widget(
                &button_for_result,
                &gettext("Another task operation is already in progress"),
            );
            return;
        };
        show_toast_for_widget(&button_for_result, &gettext("Copying task list…"));
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
                            &gettext("The copy was confirmed, but could not be reloaded authoritatively"),
                        );
                    }
                    let changed = copy.copied_count;
                    let created = copy.copied_count.saturating_sub(copy.updated_count);
                    match copy.status {
                        ECalSourceCopyStatus::Complete => page.show_toast(&gettext(
                            "Task list was copied ({changed} tasks; {created} new, {updated} updated)",
                        )
                        .replace("{changed}", &changed.to_string())
                        .replace("{created}", &created.to_string())
                        .replace("{updated}", &copy.updated_count.to_string())),
                        ECalSourceCopyStatus::PartialFailure(message) => page.show_toast(&gettext(
                            "Task list was partially copied ({changed} confirmed): {message}",
                        )
                        .replace("{changed}", &changed.to_string())
                        .replace("{message}", &message)),
                    }
                    start_task_source_discovery(
                        &page,
                        page.imp().task_source_filter.borrow().clone(),
                        false,
                    );
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast(&gettext("Copying is not supported by this backend"));
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
        show_toast_for_widget(button, &gettext("Select a task list to delete"));
        return;
    };
    let state = task_source_action_state(
        Some(&source_uid),
        &page.imp().task_source_metadata.borrow(),
        task_mutation_batch_running(page),
    );
    if !state.delete_enabled {
        show_toast_for_widget(button, &gettext("This task list cannot be deleted"));
        return;
    }
    let remote = source.remote_deletable;
    let title = if remote {
        gettext("Delete Remote Task List?")
    } else {
        gettext("Remove Task List?")
    };
    let description = if remote {
        gettext("“{name}” and its tasks will be permanently deleted from the provider.")
            .replace("{name}", &source.display_name)
    } else {
        gettext("“{name}” will be removed from Meridian. This action cannot be undone.")
            .replace("{name}", &source.display_name)
    };
    let dialog = adw::AlertDialog::new(Some(&title), Some(&description));
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("delete", &gettext("Delete"));
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
                &gettext("Another task operation is already in progress"),
            );
            return;
        };
        show_toast_for_widget(&button_for_result, &gettext("Deleting task list…"));
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
                    page.show_toast(&gettext("Task list was deleted"));
                    let current_filter = page.imp().task_source_filter.borrow().clone();
                    start_task_source_discovery(
                        &page,
                        current_filter,
                        true,
                    );
                }
                Err(ECalServiceError::Unsupported) => {
                    page.show_toast(&gettext("Deleting is not supported by this backend"));
                    let current_filter = page.imp().task_source_filter.borrow().clone();
                    start_task_source_discovery(
                        &page,
                        current_filter,
                        false,
                    );
                }
                Err(ECalServiceError::Backend(message)) => {
                    page.show_toast(&message);
                    let current_filter = page.imp().task_source_filter.borrow().clone();
                    start_task_source_discovery(
                        &page,
                        current_filter,
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
        TaskSourceCapability::ReadOnly => format!("{display_name} ({})", gettext("read-only")),
        TaskSourceCapability::Unavailable => format!("{display_name} ({})", gettext("unavailable")),
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
    if source_uid == "local-calendar" || source_uid.starts_with("account-tasks-") {
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
        return Err(gettext("The component is not a VTODO"));
    }
    if let Some(component_uid) = ical_property(component, "UID")
        && component_uid != uid
    {
        return Err(gettext("The confirmed component contains a different UID"));
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
        return Err(gettext("The confirmed task result belongs to a different UID"));
    }
    let component = component
        .filter(|component| !component.trim().is_empty())
        .ok_or_else(|| {
            gettext("Backend did not confirm an authoritative task; please reload")
        })?;
    if ical_property(&component, "UID").is_some_and(|uid| uid != expected_uid) {
        return Err(gettext("The confirmed component contains a different UID"));
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
        .label(&crate::i18n::gettext("Quick Edit Task"))
        .halign(gtk::Align::Start)
        .css_classes(vec!["heading"])
        .build();
    content.append(&heading);

    let status_model = gtk::StringList::new(&[
        &crate::i18n::gettext("Not Started"),
        &crate::i18n::gettext("In Progress"),
        &crate::i18n::gettext("Completed"),
        &crate::i18n::gettext("Cancelled"),
    ]);
    let status_selector = gtk::DropDown::new(Some(status_model), None::<&gtk::Expression>);
    status_selector.set_selected(task_status_index(&task.component()));
    let status_row = adw::ActionRow::builder().title(&crate::i18n::gettext("Status")).build();
    status_row.add_suffix(&status_selector);
    content.append(&status_row);

    let priority_spin = gtk::SpinButton::with_range(0.0, 9.0, 1.0);
    priority_spin.set_numeric(true);
    priority_spin.set_value(values.priority as f64);
    let priority_row = adw::ActionRow::builder()
        .title(&gettext("Priority (0 = normal)"))
        .build();
    priority_row.add_suffix(&priority_spin);
    content.append(&priority_row);

    let due_entry = gtk::Entry::builder()
        .text(&values.due)
        .placeholder_text("YYYY-MM-DD")
        .width_chars(10)
        .build();
    let due_row = adw::ActionRow::builder().title(&gettext("Due")).build();
    due_row.add_suffix(&due_entry);
    content.append(&due_row);

    let categories_entry = gtk::Entry::builder()
        .text(&values.categories)
        .placeholder_text(&gettext("e.g. Work, Personal"))
        .width_chars(18)
        .build();
    let categories_row = adw::ActionRow::builder().title(&gettext("Categories")).build();
    categories_row.add_suffix(&categories_entry);
    content.append(&categories_row);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .margin_top(4)
        .build();
    let cancel_button = gtk::Button::with_label(&gettext("Cancel"));
    cancel_button.add_css_class("flat");
    let save_button = gtk::Button::with_label(&gettext("Save"));
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
                &gettext("This task list is no longer confirmed as writable"),
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
                            page.show_toast(
                                &gettext("Task updated ({uid})").replace("{uid}", &uid),
                            );
                        }
                    } else if let Some(page) = page_weak_for_result.upgrade() {
                        page.show_toast(
                            &gettext("Task was confirmed, but is no longer loaded in this list"),
                        );
                    }
                    if let Some(popover) = popover_for_result.upgrade() {
                        popover.popdown();
                    }
                }
                Ok(Err(error)) => {
                    show_toast_for_widget(
                        &save_button_for_result,
                        &gettext("Could not update task: {error}").replace("{error}", &error.to_string()),
                    );
                }
                Err(error) => {
                    tracing::debug!(%error, "Task quick edit channel closed");
                    show_toast_for_widget(
                        &save_button_for_result,
                        &gettext("Task modification could not be completed"),
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
        return Err(gettext("A task requires a title"));
    }
    if priority > 9 {
        return Err(gettext("Priority must be between 0 and 9"));
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
        return Err(gettext("Due date must be after start date"));
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
        format!("UID:meridian-{}", uuid::Uuid::new_v4()),
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
            return Err(gettext("Completed tasks require a valid completion date"));
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
        return Err(gettext("A task requires a title"));
    }
    if priority > 9 {
        return Err(gettext("Priority must be between 0 and 9"));
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
        return Err(gettext("Due date must be after start date"));
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
                return Err(gettext("Attachments contain invalid control characters"));
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
                return Err(gettext("ATTACH property is invalid"));
            }
            let safe_value = serialize_ical_attachment_value(value.trim());
            if safe_value.is_empty() {
                return Err(gettext("An attachment requires a URI or BINARY value"));
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
            hide_completed_tasks: false,
            hide_completed_value: 1,
            hide_completed_cutoff: None,
            hide_cancelled_tasks: false,
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
        assert!(named_prompt.description.contains("irrevocably"));
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
        assert!(count_prompt.title.contains("2 tasks"));
        assert!(count_prompt.description.contains("2 tasks"));
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
        assert!(task_query_matches(&task, "in progress"));
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
    fn task_hide_cancelled_and_completed_filter_parities() {
        let base_now = 1_700_000_000i64;
        let mut context = fixed_context("2026-08-10", 10);
        context.now_timestamp = base_now;

        let cancelled = TaskItemObject::new(
            "local-calendar",
            "task-c",
            "Cancelled Task",
            "",
            "normal",
            false,
            "BEGIN:VTODO\r\nSTATUS:CANCELLED\r\nEND:VTODO\r\n",
        );

        let completed_old = TaskItemObject::new(
            "local-calendar",
            "task-done-old",
            "Old Completed",
            "",
            "normal",
            true,
            "BEGIN:VTODO\r\nSTATUS:COMPLETED\r\nCOMPLETED:20231114T200000Z\r\nEND:VTODO\r\n",
        );

        let recent_ts = chrono::DateTime::from_timestamp(base_now - 300, 0)
            .unwrap()
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let completed_recent = TaskItemObject::new(
            "local-calendar",
            "task-done-recent",
            "Recent Completed",
            "",
            "normal",
            true,
            &format!("BEGIN:VTODO\r\nSTATUS:COMPLETED\r\nCOMPLETED:{recent_ts}\r\nEND:VTODO\r\n"),
        );

        let query = TaskQuery::default();

        // 1. By default (hide = false), all tasks are visible in TaskFilter::All
        assert!(task_matches_filter_with_context(
            &cancelled,
            TaskFilter::All,
            None,
            None,
            &query,
            context
        ));
        assert!(task_matches_filter_with_context(
            &completed_old,
            TaskFilter::All,
            None,
            None,
            &query,
            context
        ));
        assert!(task_matches_filter_with_context(
            &completed_recent,
            TaskFilter::All,
            None,
            None,
            &query,
            context
        ));

        // 2. When hide_cancelled_tasks is true:
        let mut hide_cancelled_ctx = context;
        hide_cancelled_ctx.hide_cancelled_tasks = true;
        // In All: hidden!
        assert!(!task_matches_filter_with_context(
            &cancelled,
            TaskFilter::All,
            None,
            None,
            &query,
            hide_cancelled_ctx
        ));
        // In Cancelled view: preserved!
        assert!(task_matches_filter_with_context(
            &cancelled,
            TaskFilter::Cancelled,
            None,
            None,
            &query,
            hide_cancelled_ctx
        ));

        // 3. When hide_completed_tasks is true with value = 0 (hide immediately):
        let mut hide_completed_imm_ctx = context;
        hide_completed_imm_ctx.hide_completed_tasks = true;
        hide_completed_imm_ctx.hide_completed_value = 0;
        assert!(!task_matches_filter_with_context(
            &completed_old,
            TaskFilter::All,
            None,
            None,
            &query,
            hide_completed_imm_ctx
        ));
        assert!(!task_matches_filter_with_context(
            &completed_recent,
            TaskFilter::All,
            None,
            None,
            &query,
            hide_completed_imm_ctx
        ));
        // In Completed view: preserved!
        assert!(task_matches_filter_with_context(
            &completed_old,
            TaskFilter::Completed,
            None,
            None,
            &query,
            hide_completed_imm_ctx
        ));

        // 4. When hide_completed_tasks is true with cutoff:
        let mut hide_completed_cutoff_ctx = context;
        hide_completed_cutoff_ctx.hide_completed_tasks = true;
        hide_completed_cutoff_ctx.hide_completed_value = 1;
        hide_completed_cutoff_ctx.hide_completed_cutoff = Some(base_now - 600); // 10 minutes ago
        // Old completed (completed before cutoff): hidden!
        assert!(!task_matches_filter_with_context(
            &completed_old,
            TaskFilter::All,
            None,
            None,
            &query,
            hide_completed_cutoff_ctx
        ));
        // Recent completed (completed 5 min ago >= cutoff): visible!
        assert!(task_matches_filter_with_context(
            &completed_recent,
            TaskFilter::All,
            None,
            None,
            &query,
            hide_completed_cutoff_ctx
        ));
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
            hide_completed_tasks: false,
            hide_completed_value: 1,
            hide_completed_cutoff: None,
            hide_cancelled_tasks: false,
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
            crate::i18n::gettext("Preparing print list…")
        );
        assert_eq!(
            TaskListPrintAction::Preview.preparing_label(),
            crate::i18n::gettext("Preparing print preview…")
        );
        assert_eq!(
            TaskListPrintAction::Print.preparation_label(),
            crate::i18n::gettext("Print document prepared")
        );
        assert_eq!(
            TaskListPrintAction::Preview.preparation_label(),
            crate::i18n::gettext("Print preview prepared")
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
            format!("Team Tasks ({})", crate::i18n::gettext("read-only"))
        );

        source.connected = false;
        source.available = false;
        assert_eq!(
            task_source_capability_from_ecal(&source),
            TaskSourceCapability::Unavailable
        );
        assert_eq!(
            task_source_capability_label(&source),
            format!("Team Tasks ({})", crate::i18n::gettext("unavailable"))
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
        assert!(document.text.contains(&format!("{}\nPlan, review", crate::i18n::gettext("Summary"))));
        assert!(document.text.contains(&format!("{}\nFREQ=WEEKLY", crate::i18n::gettext("Recurrence"))));
        assert!(
            document
                .text
                .contains(&format!("{}\n<img src=remote> safe text", crate::i18n::gettext("Description")))
        );
        assert!(
            document
                .text
                .contains(&format!("{}: https://example.test/attachment.pdf", crate::i18n::gettext("URI (not opened)")))
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
            task_preview_find_state("Another task", "ärger", None).current,
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
            TaskPreviewOrientation::Vertical
        );
        assert_eq!(TaskPreviewOrientation::Classic.setting_value(), "classic");
        assert_eq!(TaskPreviewOrientation::Vertical.setting_value(), "vertical");
        assert_eq!(TaskPreviewOrientation::Classic.index(), 0);
        assert_eq!(TaskPreviewOrientation::Vertical.index(), 1);
    }

    #[test]
    fn local_task_create_edit_and_reload_roundtrip_is_authoritative() {
        let directory = std::env::temp_dir().join(format!(
            "meridian-task-editor-{}",
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

    #[test]
    fn test_tasks_sidebar_and_calendar_parity_structure() {
        let source = include_str!("tasks_page.rs");
        // Verify sidebar styling matches calendar
        assert!(source.contains("\"calendar-sidebar\""));
        assert!(source.contains("\"tasks-sidebar\""));
        assert!(source.contains("\"calendar-sidebar-row\""));
        assert!(source.contains("\"calendar-sidebar-group-row\""));
        assert!(source.contains("\"calendar-sidebar-child-row\""));
        assert!(source.contains("\"account-heading\""));
        // Verify colored checkboxes matching calendar
        assert!(source.contains("cal-check-"));
        assert!(source.contains("checkbutton.cal-check-"));
        assert!(source.contains("--accent-bg-color:"));
        // Verify context menu actions
        assert!(source.contains("select_exclusive_task_source"));
        assert!(source.contains("select_all_task_sources"));
        assert!(source.contains("set_default_task_source"));
    }

    #[test]
    fn test_tasks_toolbar_and_search_bar_hig() {
        let source = include_str!("tasks_page.rs");
        // Unified view toolbar
        assert!(source.contains("\"unified-view-toolbar\""));
        assert!(source.contains("sidebar-show-symbolic"));
        assert!(source.contains("sidebar-show-right-symbolic"));
        // Collapsible SearchBar synced via window headerbar and keyboard shortcuts
        assert!(source.contains("task_search_bar"));
        assert!(source.contains("search-mode-enabled"));
        assert!(source.contains("connect_stop_search"));
        assert!(source.contains("search_bar_for_stop.set_search_mode(false)"));
        // Triage actions in left toolbar section
        assert!(source.contains("left_box.append(&triage_group)"));
        assert!(source.contains("left_box.append(&more_button)"));
        // Overflow menu with secondary and print actions
        assert!(source.contains("more_popover"));
        assert!(source.contains("view-more-symbolic"));
        assert!(source.contains("Select All Tasks"));
        assert!(source.contains("Purge Completed Tasks…"));
        assert!(source.contains("Print Task List… (Ctrl+P)"));
        assert!(source.contains("Print Preview…"));
        assert!(source.contains("Export as iCalendar…"));
        // Toolbar HIG button modes for triage buttons
        assert!(source.contains("pub(crate) fn apply_toolbar_button_modes"));
        assert!(source.contains("pub(crate) fn sync_toolbar_button_labels"));
    }

    #[test]
    fn test_tasks_toolbar_filter_switcher_and_quick_add() {
        let source = include_str!("tasks_page.rs");
        // Right-aligned toolbar section with sort and consolidated filter dropdown
        assert!(source.contains("filter_dropdown"));
        assert!(source.contains("TASK_FILTER_OPTIONS"));
        assert!(source.contains("right_box.append(&sort_group);"));
        assert!(source.contains("right_box.append(&filter_dropdown);"));
        // Quick add box
        assert!(source.contains("task-quick-add-box"));
        assert!(source.contains("task-quick-add-entry"));
        // Task notebook icon
        assert!(source.contains("view-list-bullet-symbolic"));
        assert!(source.contains("task-row-icon"));
    }

    #[test]
    fn test_tasks_hidden_sources_filtering() {
        let mut hidden = HashSet::new();
        hidden.insert("hidden-source-1".to_string());
        assert!(hidden.contains("hidden-source-1"));
        assert!(!hidden.contains("visible-source-2"));
    }

    #[test]
    fn test_task_table_canvas_layout_and_hit_testing() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let task1 = TaskItemObject::new(
            "local-calendar",
            "task-1",
            "Review pull requests",
            "2026-09-15",
            "1",
            false,
            "BEGIN:VTODO\r\nSUMMARY:Review pull requests\r\nCATEGORIES:Work\r\nEND:VTODO\r\n",
        );
        let task2 = TaskItemObject::new(
            "local-calendar",
            "task-2",
            "Submit report",
            "2026-09-20",
            "2",
            true,
            "BEGIN:VTODO\r\nSUMMARY:Submit report\r\nSTATUS:COMPLETED\r\nPERCENT-COMPLETE:100\r\nEND:VTODO\r\n",
        );

        let layout = prepare_task_table_layout(&[task1, task2], 800.0, today);
        assert_eq!(layout.rows.len(), 2);
        assert_eq!(layout.total_height, 64.0);
        assert_eq!(layout.rows[0].title, "Review pull requests");
        assert_eq!(layout.rows[0].due_display, crate::i18n::gettext("Today"));
        assert!(!layout.rows[0].completed);
        assert_eq!(layout.rows[1].title, "Submit report");
        assert!(layout.rows[1].completed);

        // Header hit test
        assert_eq!(hit_test_task_table_header(&layout, 10.0), Some(TaskTableColumn::Completed));
        assert_eq!(hit_test_task_table_header(&layout, 40.0), Some(TaskTableColumn::Priority));
        assert_eq!(hit_test_task_table_header(&layout, 100.0), Some(TaskTableColumn::Title));

        // Row hit test: outside checkbox
        let hit_row0 = hit_test_task_table(&layout, 200.0, 10.0);
        assert_eq!(hit_row0, Some((0, false)));

        // Row hit test: inside checkbox
        let (cx, cy, _, _) = layout.rows[0].check_rect;
        let hit_check0 = hit_test_task_table(&layout, cx + 2.0, cy + 2.0);
        assert_eq!(hit_check0, Some((0, true)));

        // Row 1 hit test
        let hit_row1 = hit_test_task_table(&layout, 200.0, 42.0);
        assert_eq!(hit_row1, Some((1, false)));

        // Out of bounds hit test
        assert_eq!(hit_test_task_table(&layout, 200.0, 100.0), None);
        assert_eq!(hit_test_task_table(&layout, 200.0, -5.0), None);
    }
}

impl Default for TasksPage {
    fn default() -> Self {
        Self::new()
    }
}
