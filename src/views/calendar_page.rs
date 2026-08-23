// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPage — Calendar view with Day/Week/Month navigation, event creation, and task management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc, Weekday};
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_category_service::category_names_from_ical_component;
use crate::services::calendar_store_service::{
    CalendarEventRecord, CalendarSnapshot, CalendarStoreService, TaskRecord, export_ical_component,
    update_ical_property,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalMutationResult, ECalRecord,
    ECalService, ECalSyncEvent, load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::ical_time_service::{
    property_local_date, property_local_dates, property_local_datetime, property_local_display,
};
#[cfg(test)]
use crate::services::recurrence_service::recurrence_matches_without_count;
use crate::services::recurrence_service::{
    RecurrenceByDay, next_recurrence_date, parse_extended_weekday_token, parse_recurrence_rule,
    recurrence_occurs_on as recurrence_rule_occurs_on,
};
use crate::services::settings_service::AppSettings;
use crate::views::accessibility::set_accessible_label;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalendarView {
    Day,
    Week,
    WorkWeek,
    Month,
    Agenda,
}

const AGENDA_VISIBLE_DAYS: i64 = 90;
const MONTH_GRID_CELLS: i64 = 42;

/// A civil-date interval.  `end` is exclusive everywhere in the view layer;
/// no timestamp or 24-hour arithmetic is used to express a calendar range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CalendarDateRange {
    pub(crate) start: NaiveDate,
    pub(crate) end: NaiveDate,
}

impl CalendarDateRange {
    pub(crate) fn new(start: NaiveDate, end: NaiveDate) -> Self {
        assert!(
            start < end,
            "calendar ranges must be non-empty and half-open"
        );
        Self { start, end }
    }

    pub(crate) fn from_days(start: NaiveDate, days: i64) -> Self {
        assert!(days > 0, "calendar ranges must contain at least one date");
        let end = start
            .checked_add_signed(Duration::days(days))
            .expect("calendar date range overflow");
        Self::new(start, end)
    }

    pub(crate) fn single(date: NaiveDate) -> Self {
        Self::from_days(date, 1)
    }

    pub(crate) fn len_days(self) -> i64 {
        (self.end - self.start).num_days()
    }

    pub(crate) fn last_inclusive(self) -> NaiveDate {
        self.end
            .pred_opt()
            .expect("a non-empty range has an inclusive end")
    }

    pub(crate) fn contains(self, date: NaiveDate) -> bool {
        self.start <= date && date < self.end
    }

    pub(crate) fn intersects(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub(crate) fn dates(self) -> impl Iterator<Item = NaiveDate> {
        (0..self.len_days())
            .filter_map(move |offset| self.start.checked_add_signed(Duration::days(offset)))
    }
}

/// GTK-free description of one visible calendar viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CalendarViewport {
    pub(crate) view: CalendarView,
    pub(crate) anchor_date: NaiveDate,
    pub(crate) visible_range: CalendarDateRange,
}

impl CalendarViewport {
    pub(crate) fn for_view(
        view: CalendarView,
        anchor_date: NaiveDate,
        first_weekday: Weekday,
    ) -> Self {
        let visible_range = match view {
            CalendarView::Day => CalendarDateRange::single(anchor_date),
            CalendarView::Week | CalendarView::WorkWeek => {
                CalendarDateRange::from_days(week_start_for(anchor_date, first_weekday), 7)
            }
            CalendarView::Month => {
                let month_start = anchor_date
                    .with_day(1)
                    .expect("every valid calendar date has a first day");
                CalendarDateRange::from_days(
                    week_start_for(month_start, first_weekday),
                    MONTH_GRID_CELLS,
                )
            }
            CalendarView::Agenda => CalendarDateRange::from_days(anchor_date, AGENDA_VISIBLE_DAYS),
        };
        Self {
            view,
            anchor_date,
            visible_range,
        }
    }
}

/// GTK-free state contract shared by navigation, loading and selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CalendarViewState {
    pub(crate) view: CalendarView,
    pub(crate) anchor_date: NaiveDate,
    pub(crate) visible_range: CalendarDateRange,
    pub(crate) load_generation: u64,
    pub(crate) selected_range: Option<CalendarDateRange>,
}

impl CalendarViewState {
    pub(crate) fn new(view: CalendarView, anchor_date: NaiveDate, first_weekday: Weekday) -> Self {
        let viewport = CalendarViewport::for_view(view, anchor_date, first_weekday);
        Self {
            view,
            anchor_date,
            visible_range: viewport.visible_range,
            load_generation: 0,
            selected_range: None,
        }
    }

    pub(crate) fn viewport(&self) -> CalendarViewport {
        CalendarViewport {
            view: self.view,
            anchor_date: self.anchor_date,
            visible_range: self.visible_range,
        }
    }

    pub(crate) fn set_view(&mut self, view: CalendarView, first_weekday: Weekday) {
        self.view = view;
        self.recompute_range(first_weekday);
    }

    pub(crate) fn set_anchor(&mut self, anchor_date: NaiveDate, first_weekday: Weekday) {
        self.anchor_date = anchor_date;
        self.recompute_range(first_weekday);
    }

    #[allow(dead_code)]
    pub(crate) fn set_selected_range(&mut self, selected_range: Option<CalendarDateRange>) {
        self.selected_range = selected_range;
    }

    pub(crate) fn navigate(&mut self, direction: i32, first_weekday: Weekday) {
        let direction = i64::from(direction.signum());
        if direction == 0 {
            return;
        }
        let next_anchor = match self.view {
            CalendarView::Day => self
                .anchor_date
                .checked_add_signed(Duration::days(direction))
                .expect("day navigation overflow"),
            CalendarView::Week | CalendarView::WorkWeek => self
                .visible_range
                .start
                .checked_add_signed(Duration::days(direction * 7))
                .expect("week navigation overflow"),
            CalendarView::Month => shift_month(self.anchor_date, direction as i32),
            CalendarView::Agenda => self
                .visible_range
                .start
                .checked_add_signed(Duration::days(direction * AGENDA_VISIBLE_DAYS))
                .expect("agenda navigation overflow"),
        };
        self.anchor_date = next_anchor;
        self.selected_range = None;
        self.recompute_range(first_weekday);
    }

    pub(crate) fn today(&mut self, today: NaiveDate, first_weekday: Weekday) {
        self.anchor_date = today;
        self.selected_range = None;
        self.recompute_range(first_weekday);
    }

    fn recompute_range(&mut self, first_weekday: Weekday) {
        self.visible_range =
            CalendarViewport::for_view(self.view, self.anchor_date, first_weekday).visible_range;
    }
}

fn advance_calendar_generation(current: u64) -> u64 {
    current
        .checked_add(1)
        .expect("calendar load generation exhausted")
}

/// Latest-wins gate for snapshot jobs.  It is deliberately GTK-free so stale
/// and cancellation behavior can be tested without constructing a widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CalendarLoadGate {
    generation: u64,
    loading: bool,
}

impl CalendarLoadGate {
    fn begin(&mut self) -> u64 {
        self.generation = advance_calendar_generation(self.generation);
        self.loading = true;
        self.generation
    }

    fn accepts(&self, generation: u64) -> bool {
        self.loading && self.generation == generation
    }

    fn is_current(&self, generation: u64) -> bool {
        self.loading && self.generation == generation
    }

    fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.loading = false;
        true
    }

    fn cancel(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.loading = false;
        true
    }
}

/// Immutable, local candidate index for one calendar snapshot.  Non-recurring
/// events are put into civil-date buckets; recurring events are kept in one
/// small side list and expanded only once per requested range.  This keeps an
/// Agenda query out of the N-events-by-90-days full-scan shape.
#[derive(Debug, Clone)]
struct CalendarSnapshotIndex {
    snapshot: Arc<CalendarSnapshot>,
    date_buckets: BTreeMap<NaiveDate, Vec<usize>>,
    recurring_events: Vec<usize>,
    long_events: Vec<(usize, CalendarDateRange)>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CalendarIndexQueryStats {
    date_bucket_lookups: usize,
    bucketed_event_refs: usize,
    recurring_events_considered: usize,
    long_intervals_considered: usize,
}

impl CalendarSnapshotIndex {
    fn new(snapshot: Arc<CalendarSnapshot>) -> Self {
        let mut index = Self {
            snapshot,
            date_buckets: BTreeMap::new(),
            recurring_events: Vec::new(),
            long_events: Vec::new(),
        };

        for (event_index, event) in index.snapshot.events.iter().enumerate() {
            let recurring = ical_property(&event.component, "RRULE").is_some()
                || ical_property(&event.component, "RDATE").is_some();
            let Some(range) = record_display_range(event) else {
                continue;
            };

            if recurring {
                index.recurring_events.push(event_index);
                continue;
            }

            // Avoid materializing an unbounded number of buckets for a broken
            // or very long imported event.  It remains an indexed interval.
            if range.len_days() <= 366 {
                for date in range.dates() {
                    index
                        .date_buckets
                        .entry(date)
                        .or_default()
                        .push(event_index);
                }
            } else {
                index.long_events.push((event_index, range));
            }
        }

        index
    }

    fn candidate_event_indices(&self, range: CalendarDateRange) -> Vec<usize> {
        self.candidate_event_indices_with_stats(range).0
    }

    fn candidate_event_indices_with_stats(
        &self,
        range: CalendarDateRange,
    ) -> (Vec<usize>, CalendarIndexQueryStats) {
        let mut indices = BTreeSet::new();
        let mut stats = CalendarIndexQueryStats::default();
        for date in range.dates() {
            stats.date_bucket_lookups += 1;
            if let Some(bucket) = self.date_buckets.get(&date) {
                stats.bucketed_event_refs += bucket.len();
                indices.extend(bucket.iter().copied());
            }
        }
        stats.recurring_events_considered = self.recurring_events.len();
        indices.extend(self.recurring_events.iter().copied());
        stats.long_intervals_considered = self.long_events.len();
        indices.extend(
            self.long_events
                .iter()
                .filter(|(_, event_range)| event_range.intersects(range))
                .map(|(event_index, _)| *event_index),
        );

        let mut indices = indices.into_iter().collect::<Vec<_>>();
        indices.sort_by(|left, right| {
            let left_event = &self.snapshot.events[*left];
            let right_event = &self.snapshot.events[*right];
            left_event
                .source_uid
                .cmp(&right_event.source_uid)
                .then_with(|| left_event.uid.cmp(&right_event.uid))
                .then_with(|| left.cmp(right))
        });
        (indices, stats)
    }
}

fn calendar_view_from_name(value: &str) -> CalendarView {
    match value {
        "day" => CalendarView::Day,
        "week" => CalendarView::Week,
        "workweek" => CalendarView::WorkWeek,
        "agenda" => CalendarView::Agenda,
        _ => CalendarView::Month,
    }
}

fn calendar_view_name(view: CalendarView) -> &'static str {
    match view {
        CalendarView::Day => "day",
        CalendarView::Week => "week",
        CalendarView::WorkWeek => "workweek",
        CalendarView::Month => "month",
        CalendarView::Agenda => "agenda",
    }
}

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug)]
    pub struct CalendarPage {
        pub current_date: RefCell<NaiveDate>,
        pub(crate) current_view: RefCell<CalendarView>,
        pub(crate) view_state: RefCell<CalendarViewState>,
        pub settings: RefCell<Option<AppSettings>>,
        pub calendar_stack: RefCell<Option<adw::ViewStack>>,
        pub calendar_view_buttons: RefCell<Option<Vec<gtk::ToggleButton>>>,
        pub new_event_button: RefCell<Option<gtk::Button>>,
        pub syncing_calendar_view: Cell<bool>,
        pub month_grid: RefCell<Option<gtk::Grid>>,
        pub month_date_label: RefCell<Option<gtk::Label>>,
        pub day_view: RefCell<Option<gtk::Box>>,
        pub week_view: RefCell<Option<gtk::Box>>,
        pub workweek_view: RefCell<Option<gtk::Box>>,
        pub agenda_view: RefCell<Option<gtk::Box>>,
        pub event_records: RefCell<Option<Rc<RefCell<Vec<CalendarEventRecord>>>>>,
        pub event_list: RefCell<Option<gtk::ListBox>>,
        pub calendar_data_state: RefCell<Option<gtk::Label>>,
        pub task_records: RefCell<Option<Rc<RefCell<Vec<TaskRecord>>>>>,
        pub task_list: RefCell<Option<gtk::ListBox>>,
        pub task_state: RefCell<Option<gtk::Label>>,
        pub mini_calendar: RefCell<Option<gtk::Calendar>>,
        pub syncing_mini_calendar: Cell<bool>,
        pub calendar_search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub calendar_search_query: RefCell<String>,
        pub calendar_category_filter: RefCell<Option<String>>,
        pub calendar_category_model: RefCell<Option<gtk::StringList>>,
        pub calendar_category_selector: RefCell<Option<gtk::DropDown>>,
        pub hidden_calendar_sources: RefCell<HashSet<String>>,
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub(super) calendar_load_cancel: RefCell<Option<Arc<AtomicBool>>>,
        pub(super) calendar_load_gate: RefCell<CalendarLoadGate>,
        pub(super) calendar_loading: Cell<bool>,
        pub(super) calendar_snapshot_index: RefCell<Option<Arc<CalendarSnapshotIndex>>>,
    }

    impl Default for CalendarPage {
        fn default() -> Self {
            let current_date = Local::now().date_naive();
            Self {
                current_date: RefCell::new(current_date),
                current_view: RefCell::new(CalendarView::Month),
                view_state: RefCell::new(CalendarViewState::new(
                    CalendarView::Month,
                    current_date,
                    Weekday::Mon,
                )),
                settings: RefCell::new(None),
                calendar_stack: RefCell::new(None),
                calendar_view_buttons: RefCell::new(None),
                new_event_button: RefCell::new(None),
                syncing_calendar_view: Cell::new(false),
                month_grid: RefCell::new(None),
                month_date_label: RefCell::new(None),
                day_view: RefCell::new(None),
                week_view: RefCell::new(None),
                workweek_view: RefCell::new(None),
                agenda_view: RefCell::new(None),
                event_records: RefCell::new(None),
                event_list: RefCell::new(None),
                calendar_data_state: RefCell::new(None),
                task_records: RefCell::new(None),
                task_list: RefCell::new(None),
                task_state: RefCell::new(None),
                mini_calendar: RefCell::new(None),
                syncing_mini_calendar: Cell::new(false),
                calendar_search_entry: RefCell::new(None),
                calendar_search_query: RefCell::new(String::new()),
                calendar_category_filter: RefCell::new(None),
                calendar_category_model: RefCell::new(None),
                calendar_category_selector: RefCell::new(None),
                hidden_calendar_sources: RefCell::new(HashSet::new()),
                ecal_monitor_cancel: RefCell::new(None),
                calendar_load_cancel: RefCell::new(None),
                calendar_load_gate: RefCell::new(CalendarLoadGate::default()),
                calendar_loading: Cell::new(false),
                calendar_snapshot_index: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarPage {
        const NAME: &'static str = "EvolutionCalendarPage";
        type Type = super::CalendarPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for CalendarPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(cancel) = self.calendar_load_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Release);
            }
            let generation = self.calendar_load_gate.borrow().generation;
            self.calendar_load_gate.borrow_mut().cancel(generation);
            self.calendar_loading.set(false);
            if let Some(cancel) = self.ecal_monitor_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CalendarPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct CalendarPage(ObjectSubclass<imp::CalendarPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

struct CalendarSnapshotLoad {
    generation: u64,
    cancellation: Arc<AtomicBool>,
    result: Result<
        (
            Arc<CalendarSnapshotIndex>,
            CalendarDataOrigin,
            Option<String>,
        ),
        String,
    >,
}

impl CalendarPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Re-render category names, colors, and all open calendar/task rows after
    /// the presentation-only category store changed in Preferences.
    pub fn refresh_category_presentation(&self) {
        self.refresh_calendar_views();
        self.refresh_event_list_visibility();
    }

    /// Trigger the same event editor as the calendar toolbar.  The main
    /// Evolution window uses this entry point so File → New, the contextual
    /// header action, and the calendar page share one refresh workflow.
    pub fn present_new_event(&self) {
        if let Some(button) = self.imp().new_event_button.borrow().as_ref() {
            button.activate();
        }
    }

    pub(crate) fn focus_search(&self) {
        if let Some(entry) = self.imp().calendar_search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(entry) = self.imp().calendar_search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn open_search_result(&self, source_uid: &str, uid: &str) -> bool {
        let Some(records) = self.imp().event_records.borrow().as_ref().cloned() else {
            return false;
        };
        let Some(event) = records
            .borrow()
            .iter()
            .find(|event| event.source_uid == source_uid && event.uid == uid)
            .cloned()
        else {
            return false;
        };
        open_event_editor_for_widget(self, &event);
        true
    }

    /// Reload the portable/EDS calendar snapshot after a dialog operation.
    ///
    /// EDS-backed installations normally receive the same change through the
    /// native monitor.  The explicit reload is still required for the
    /// portable local store and also makes the UI deterministic when a
    /// backend emits its signal late or not at all.
    pub(crate) fn reload_calendar_data(&self) {
        self.start_calendar_snapshot_load();
    }

    fn start_calendar_snapshot_load(&self) {
        self.cancel_calendar_snapshot_load();

        let generation = self.imp().calendar_load_gate.borrow_mut().begin();
        self.imp().view_state.borrow_mut().load_generation = generation;
        self.set_calendar_loading_status(true, "Kalender werden geladen …");

        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .calendar_load_cancel
            .replace(Some(Arc::clone(&cancellation)));

        let page = self.downgrade();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn({
            let cancellation = Arc::clone(&cancellation);
            move || {
                if cancellation.load(Ordering::Acquire) {
                    return;
                }
                let result = std::panic::catch_unwind(|| {
                    let crate::services::ecal_service::CalendarLoadResult {
                        snapshot,
                        origin,
                        warning,
                    } = load_snapshot_prefer_eds();
                    let snapshot_index = Arc::new(CalendarSnapshotIndex::new(Arc::new(snapshot)));
                    (snapshot_index, origin, warning)
                })
                .map_err(|_| "Der Kalender-Worker ist unerwartet fehlgeschlagen".to_string());
                if cancellation.load(Ordering::Acquire) {
                    return;
                }
                let load = CalendarSnapshotLoad {
                    generation,
                    cancellation,
                    result,
                };
                if let Err(error) = sender.send_blocking(load) {
                    tracing::debug!(%error, "Calendar snapshot result was dropped");
                }
            }
        });
        let cancellation_for_receiver = Arc::clone(&cancellation);
        glib::spawn_future_local(async move {
            match receiver.recv().await {
                Ok(load) => {
                    if let Some(page) = page.upgrade() {
                        page.apply_calendar_snapshot_load(load);
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "Calendar snapshot channel closed");
                    if let Some(page) = page.upgrade()
                        && page
                            .finish_calendar_snapshot_load(generation, &cancellation_for_receiver)
                    {
                        page.set_calendar_loading_status(
                            false,
                            "Kalender konnten nicht geladen werden",
                        );
                    }
                }
            }
        });
    }

    fn cancel_calendar_snapshot_load(&self) {
        if let Some(cancel) = self.imp().calendar_load_cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Release);
        }
        let generation = self.imp().calendar_load_gate.borrow().generation;
        if self
            .imp()
            .calendar_load_gate
            .borrow_mut()
            .cancel(generation)
        {
            self.set_calendar_loading_status(false, "Kalender-Laden abgebrochen");
        }
    }

    fn apply_calendar_snapshot_load(&self, load: CalendarSnapshotLoad) {
        let CalendarSnapshotLoad {
            generation,
            cancellation,
            result,
        } = load;
        let is_current = self.imp().calendar_load_gate.borrow().accepts(generation)
            && self.imp().view_state.borrow().load_generation == generation
            && !cancellation.load(Ordering::Acquire);
        if !is_current {
            return;
        }

        let (snapshot_index, origin, warning) = match result {
            Ok(result) => result,
            Err(error) => {
                if self.finish_calendar_snapshot_load(generation, &cancellation) {
                    self.set_calendar_loading_status(
                        false,
                        &format!("Kalender konnten nicht geladen werden: {error}"),
                    );
                }
                return;
            }
        };

        let Some(event_list) = self.imp().event_list.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_loading_status(false, "Kalenderansicht nicht verfügbar");
            }
            return;
        };
        let Some(event_records) = self.imp().event_records.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_loading_status(false, "Kalenderdaten nicht verfügbar");
            }
            return;
        };
        let Some(task_records) = self.imp().task_records.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_loading_status(false, "Aufgabendaten nicht verfügbar");
            }
            return;
        };

        if let Some(warning) = warning {
            tracing::warn!(%warning, "Calendar data loaded with a backend warning");
        }

        self.imp()
            .calendar_snapshot_index
            .replace(Some(Arc::clone(&snapshot_index)));
        event_records.borrow_mut().clear();
        while let Some(child) = event_list.first_child() {
            event_list.remove(&child);
        }
        task_records.replace(snapshot_index.snapshot.tasks.clone());

        let count = snapshot_index.snapshot.events.len();
        let state_label = self.imp().calendar_data_state.borrow().as_ref().cloned();
        if count == 0 {
            event_list.set_visible(false);
            if let Some(state_label) = state_label {
                match origin {
                    CalendarDataOrigin::Eds => {
                        state_label.set_label("Keine Termine aus verbundenen Kalendern gefunden")
                    }
                    CalendarDataOrigin::Graph => {
                        state_label.set_label("Keine Termine aus verbundenen Kalendern gefunden")
                    }
                    CalendarDataOrigin::Local => state_label
                        .set_label("Keine Termine aus dem lokalen Kalenderbestand gefunden"),
                }
            }
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_loading_status(false, "");
            }
        } else {
            event_list.set_visible(false);
            if let Some(state_label) = state_label {
                let origin = match origin {
                    CalendarDataOrigin::Eds => "verbundenen Kalendern",
                    CalendarDataOrigin::Graph => "verbundenen Kalendern",
                    CalendarDataOrigin::Local => "lokalem Kalenderbestand",
                };
                state_label.set_label(&format!("{count} Termin(e) aus {origin} geladen."));
            }
            let events = snapshot_index.snapshot.events.clone();
            event_records.borrow_mut().extend(events.clone());

            let page_weak = self.downgrade();
            let event_list_clone = event_list.clone();
            let event_records_clone = Rc::clone(&event_records);
            let cancellation = Arc::clone(&cancellation);
            glib::spawn_future_local(async move {
                let chunk_size = 40;
                let mut index = 0;
                while index < events.len() {
                    let Some(page) = page_weak.upgrade() else {
                        return;
                    };
                    if !page.calendar_load_is_current(generation, &cancellation) {
                        return;
                    }
                    let end = (index + chunk_size).min(events.len());
                    for event in &events[index..end] {
                        append_event_row(
                            &event_list_clone,
                            Rc::clone(&event_records_clone),
                            event.clone(),
                        );
                    }
                    index = end;
                    if index < events.len() {
                        glib::timeout_future_with_priority(
                            glib::Priority::LOW,
                            std::time::Duration::from_millis(5),
                        )
                        .await;
                    }
                }
                if let Some(page) = page_weak.upgrade() {
                    if !page.calendar_load_is_current(generation, &cancellation) {
                        return;
                    }
                    if !page.finish_calendar_snapshot_load(generation, &cancellation) {
                        return;
                    }
                    page.set_calendar_loading_status(false, "");
                    page.refresh_task_sidebar();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                }
            });
        }

        self.refresh_task_sidebar();
        self.refresh_calendar_views();
        self.refresh_event_list_visibility();
    }

    fn finish_calendar_snapshot_load(
        &self,
        generation: u64,
        cancellation: &Arc<AtomicBool>,
    ) -> bool {
        if !self.calendar_load_is_current(generation, cancellation)
            || !self
                .imp()
                .calendar_load_gate
                .borrow_mut()
                .finish(generation)
        {
            return false;
        }
        self.imp().calendar_load_cancel.borrow_mut().take();
        true
    }

    fn calendar_load_is_current(&self, generation: u64, cancellation: &Arc<AtomicBool>) -> bool {
        self.imp()
            .calendar_load_gate
            .borrow()
            .is_current(generation)
            && self.imp().view_state.borrow().load_generation == generation
            && !cancellation.load(Ordering::Acquire)
    }

    fn set_calendar_loading_status(&self, loading: bool, status: &str) {
        self.imp().calendar_loading.set(loading);
        if !status.is_empty() {
            if let Some(label) = self.imp().calendar_data_state.borrow().as_ref() {
                label.set_label(status);
            }
        }
    }

    fn month_name(month: u32) -> &'static str {
        match month {
            1 => "Januar",
            2 => "Februar",
            3 => "März",
            4 => "April",
            5 => "Mai",
            6 => "Juni",
            7 => "Juli",
            8 => "August",
            9 => "September",
            10 => "Oktober",
            11 => "November",
            12 => "Dezember",
            _ => "August",
        }
    }

    fn build_ui(&self) {
        let settings = AppSettings::try_new();
        self.imp().settings.replace(settings.clone());
        if let Some(settings) = settings.as_ref() {
            self.imp()
                .hidden_calendar_sources
                .replace(settings.calendar_hidden_sources().into_iter().collect());
        }
        // Keep the classic Evolution split on wide windows, while using the
        // same adaptive navigation contract as the other groupware modules
        // when the window becomes narrow.
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(240.0)
            .max_sidebar_width(240.0)
            .sidebar_width_fraction(0.0)
            .build();

        // ── Sidebar (Calendar Picker + Task List) ───────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(4)
            .margin_end(4)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let mini_calendar = gtk::Calendar::new();
        self.imp()
            .mini_calendar
            .replace(Some(mini_calendar.clone()));
        let page_for_mini_calendar = self.downgrade();
        mini_calendar.connect_day_selected(move |calendar| {
            let Some(page) = page_for_mini_calendar.upgrade() else {
                return;
            };
            if page.imp().syncing_mini_calendar.get() {
                return;
            }
            let date = calendar.date();
            if let Some(selected_date) = NaiveDate::from_ymd_opt(
                date.year(),
                date.month() as u32,
                date.day_of_month() as u32,
            ) {
                page.set_calendar_anchor(selected_date);
                page.refresh_calendar_views();
            }
        });
        sidebar_box.append(&mini_calendar);

        let source_header = gtk::Label::builder()
            .label("Meine Kalender")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .margin_top(8)
            .build();
        sidebar_box.append(&source_header);

        let source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();

        sidebar_box.append(&source_list);

        let calendar_state = gtk::Label::builder()
            .label("Kalender werden nach der Synchronisation angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .halign(gtk::Align::Start)
            .build();
        sidebar_box.append(&calendar_state);

        let local_item_state = gtk::Label::builder()
            .label("Keine lokalen .ics-Daten geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .halign(gtk::Align::Start)
            .build();
        sidebar_box.append(&local_item_state);
        self.imp()
            .calendar_data_state
            .replace(Some(local_item_state.clone()));

        let event_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        sidebar_box.append(&event_list);
        let event_records = Rc::new(RefCell::new(Vec::<CalendarEventRecord>::new()));
        self.imp()
            .event_records
            .replace(Some(Rc::clone(&event_records)));
        self.imp().event_list.replace(Some(event_list.clone()));
        let task_records = Rc::new(RefCell::new(Vec::<TaskRecord>::new()));
        self.imp()
            .task_records
            .replace(Some(Rc::clone(&task_records)));

        let category_model = gtk::StringList::new(&["Alle Kategorien"]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some("Terminkategorie auswählen"));
        self.imp()
            .calendar_category_model
            .replace(Some(category_model.clone()));
        self.imp()
            .calendar_category_selector
            .replace(Some(category_selector.clone()));

        let calendar_search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Termine durchsuchen…")
            .hexpand(true)
            .width_chars(18)
            .build();
        self.imp()
            .calendar_search_entry
            .replace(Some(calendar_search_entry.clone()));
        let page_weak_for_search = self.downgrade();
        calendar_search_entry.connect_search_changed(move |entry| {
            let Some(page) = page_weak_for_search.upgrade() else {
                return;
            };
            page.imp()
                .calendar_search_query
                .replace(entry.text().trim().to_lowercase());
            page.refresh_calendar_views();
            page.refresh_event_list_visibility();
        });

        let page_weak_for_category = self.downgrade();
        let category_model_for_selector = category_model.clone();
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
            page.imp().calendar_category_filter.replace(category);
            page.refresh_calendar_views();
            page.refresh_event_list_visibility();
        });

        self.start_calendar_snapshot_load();

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let event_list_for_monitor = event_list.clone();
            let event_records_for_monitor = Rc::clone(&event_records);
            let task_records_for_monitor = Rc::clone(&task_records);
            let local_item_state_for_monitor = local_item_state.clone();
            let page_for_monitor = self.downgrade();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    if matches!(&event, ECalSyncEvent::Change(_))
                        && page_for_monitor
                            .upgrade()
                            .is_some_and(|page| page.imp().calendar_loading.get())
                    {
                        if let Some(page) = page_for_monitor.upgrade() {
                            // A snapshot or its bounded GTK build owns the
                            // current list.  Re-query after the monitor event
                            // instead of letting a live delta race stale rows
                            // into that build.
                            page.reload_calendar_data();
                        }
                        continue;
                    }
                    match event {
                        ECalSyncEvent::Ready(status) => {
                            if let Some(warning) = status.warning {
                                tracing::warn!(
                                    configured = status.configured_sources,
                                    active = status.active_sources,
                                    failed = status.failed_sources,
                                    %warning,
                                    "ECal signal monitor started with source warnings"
                                );
                            }
                        }
                        ECalSyncEvent::Change(change) => match change.kind {
                            ECalChangeKind::Removed => {
                                if change.source_type == ECalComponentType::Task {
                                    remove_task_record(
                                        Rc::clone(&task_records_for_monitor),
                                        &change.source_uid,
                                        &change.uid,
                                    );
                                } else if change.source_type == ECalComponentType::Event {
                                    remove_event_row(
                                        &event_list_for_monitor,
                                        Rc::clone(&event_records_for_monitor),
                                        &change.source_uid,
                                        &change.uid,
                                    );
                                }
                            }
                            ECalChangeKind::Added | ECalChangeKind::Modified => {
                                match parse_change_record(&change) {
                                    Some(ECalRecord::Event(event)) => {
                                        upsert_event_row(
                                            &event_list_for_monitor,
                                            Rc::clone(&event_records_for_monitor),
                                            event,
                                        );
                                    }
                                    Some(ECalRecord::Task(task)) => {
                                        upsert_task_record(
                                            Rc::clone(&task_records_for_monitor),
                                            task,
                                        );
                                    }
                                    Some(ECalRecord::Memo(_)) | None => {}
                                }
                            }
                        },
                        ECalSyncEvent::Error(error) => {
                            tracing::warn!(%error, "ECal signal monitor stopped");
                            break;
                        }
                    }
                    let count = event_records_for_monitor.borrow().len();
                    event_list_for_monitor.set_visible(count > 0);
                    if count > 0 {
                        local_item_state_for_monitor
                            .set_label(&format!("{count} Termin(e) synchronisiert."));
                    }
                    if let Some(page) = page_for_monitor.upgrade() {
                        page.rebuild_calendar_index_from_records();
                        page.refresh_task_sidebar();
                        page.refresh_calendar_views();
                        page.refresh_event_list_visibility();
                    }
                }
            });
        }

        let source_list_for_load = source_list.clone();
        let calendar_state_for_load = calendar_state.clone();
        let page_weak_for_source = self.downgrade();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
            let mut sources = discovery.sources;
            sources.sort_by_cached_key(|source| {
                (source.display_name.to_lowercase(), source.uid.clone())
            });
            let hidden_sources = page_weak_for_source
                .upgrade()
                .map(|page| page.imp().hidden_calendar_sources.borrow().clone())
                .unwrap_or_default();
            let mut displayed_sources = 0usize;

            for source in sources {
                let source_uid = source.uid.clone();
                let escaped_title = glib::markup_escape_text(&source.display_name);
                let subtitle = if source.enabled {
                    "aktiv"
                } else {
                    "deaktiviert"
                };
                let escaped_subtitle = glib::markup_escape_text(subtitle);
                let row = adw::ActionRow::builder()
                    .title(escaped_title.as_str())
                    .subtitle(escaped_subtitle.as_str())
                    .use_markup(true)
                    .build();
                let check = gtk::CheckButton::builder()
                    .active(source.enabled && !hidden_sources.contains(&source_uid))
                    .sensitive(source.enabled)
                    .valign(gtk::Align::Center)
                    .tooltip_text("Kalender in den Ansichten ein-/ausblenden")
                    .build();
                let page_weak_for_toggle = page_weak_for_source.clone();
                let source_uid_for_toggle = source_uid.clone();
                check.connect_toggled(move |check| {
                    let Some(page) = page_weak_for_toggle.upgrade() else {
                        return;
                    };
                    let mut hidden_sources = page.imp().hidden_calendar_sources.borrow_mut();
                    if check.is_active() {
                        hidden_sources.remove(&source_uid_for_toggle);
                    } else {
                        hidden_sources.insert(source_uid_for_toggle.clone());
                    }
                    let hidden_sources = hidden_sources.iter().cloned().collect::<Vec<_>>();
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_hidden_sources(&hidden_sources);
                    }
                    page.refresh_task_sidebar();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                });
                row.add_prefix(&check);
                source_list_for_load.append(&row);
                displayed_sources += 1;
            }

            if CalendarStoreService::new().is_writable() {
                let source_uid = "local-calendar".to_string();
                let row = adw::ActionRow::builder()
                    .title("Lokaler Kalender")
                    .subtitle("App-eigener iCalendar-Speicher")
                    .build();
                let check = gtk::CheckButton::builder()
                    .active(!hidden_sources.contains(&source_uid))
                    .valign(gtk::Align::Center)
                    .tooltip_text("Kalender in den Ansichten ein-/ausblenden")
                    .build();
                let page_weak_for_toggle = page_weak_for_source.clone();
                check.connect_toggled(move |check| {
                    let Some(page) = page_weak_for_toggle.upgrade() else {
                        return;
                    };
                    let mut hidden_sources = page.imp().hidden_calendar_sources.borrow_mut();
                    if check.is_active() {
                        hidden_sources.remove("local-calendar");
                    } else {
                        hidden_sources.insert("local-calendar".to_string());
                    }
                    let hidden_sources = hidden_sources.iter().cloned().collect::<Vec<_>>();
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_hidden_sources(&hidden_sources);
                    }
                    page.refresh_task_sidebar();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                });
                row.add_prefix(&check);
                source_list_for_load.append(&row);
                displayed_sources += 1;
            }

            if displayed_sources == 0 {
                let status = discovery
                    .error
                    .map(|error| format!("Kalender sind nicht erreichbar: {error}"))
                    .unwrap_or_else(|| "Keine Kalenderquellen verfügbar".to_string());
                calendar_state_for_load.set_label(&status);
            } else {
                calendar_state_for_load.set_visible(false);
            }
        });

        let tasks_label = gtk::Label::builder()
            .label("Aufgaben")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .margin_top(16)
            .build();
        sidebar_box.append(&tasks_label);

        let task_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        sidebar_box.append(&task_list);
        self.imp().task_list.replace(Some(task_list));

        let task_state = gtk::Label::builder()
            .label("Keine Aufgaben geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .halign(gtk::Align::Start)
            .build();
        sidebar_box.append(&task_state);
        self.imp().task_state.replace(Some(task_state));

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
        let drawer_icon = gtk::Image::from_icon_name("x-office-calendar-symbolic");
        let drawer_title = gtk::Label::builder()
            .label("Kalender")
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
            "calendar",
            &sidebar_outer_box,
        ));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Kalender")
            .child(&sidebar_outer_box)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();
        split.set_sidebar(Some(&sidebar_page));

        // ── Calendar Content (View Switcher: Day/Week/Month) ────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();

        let view_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .halign(gtk::Align::Center)
            .build();

        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .css_classes(vec!["flat"])
            .tooltip_text("Vorheriger Zeitraum")
            .build();
        set_accessible_label(&prev_btn, "Vorherigen Kalenderzeitraum anzeigen");

        let today_btn = gtk::Button::builder()
            .label("Heute")
            .css_classes(vec!["flat"])
            .tooltip_text("Zu heute wechseln")
            .build();
        set_accessible_label(&today_btn, "Zum heutigen Datum wechseln");

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .css_classes(vec!["flat"])
            .tooltip_text("Nächster Zeitraum")
            .build();
        set_accessible_label(&next_btn, "Nächsten Kalenderzeitraum anzeigen");

        let date_label = gtk::Label::builder()
            .label("")
            .css_classes(vec!["title-3"])
            .hexpand(true)
            .build();
        self.imp()
            .month_date_label
            .replace(Some(date_label.clone()));

        let btn_add_event = gtk::Button::builder()
            .label("Neuer Termin")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["suggested-action"])
            .margin_end(8)
            .build();

        let btn_availability = gtk::Button::builder()
            .label("Freie Zeiten")
            .icon_name("appointment-soon-symbolic")
            .css_classes(vec!["flat"])
            .tooltip_text("Freie Zeitblöcke im lokalen Kalender anzeigen")
            .margin_end(8)
            .build();

        btn_availability.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |button| {
                let date = *page.imp().current_date.borrow();
                let dialog = crate::dialogs::AvailabilityDialog::new(date);
                if let Some(window) = button.root().and_downcast::<gtk::Window>() {
                    dialog.set_transient_for(Some(&window));
                }
                dialog.present();
            }
        ));

        btn_add_event.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog =
                    crate::dialogs::EventDialog::new_for_date(*page.imp().current_date.borrow());
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let page_for_refresh = page.downgrade();
                dialog.connect_close_request(move |_| {
                    if let Some(page) = page_for_refresh.upgrade() {
                        page.reload_calendar_data();
                    }
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));
        self.imp()
            .new_event_button
            .replace(Some(btn_add_event.clone()));

        // View mode toggle
        let view_toggle = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .build();

        let initial_view = settings
            .as_ref()
            .map(|settings| settings.calendar_view())
            .unwrap_or_else(|| "month".to_string());
        let initial_calendar_view = calendar_view_from_name(&initial_view);
        *self.imp().current_view.borrow_mut() = initial_calendar_view;
        self.imp()
            .view_state
            .borrow_mut()
            .set_view(initial_calendar_view, configured_first_weekday());

        let day_btn = gtk::ToggleButton::builder()
            .label("Tag")
            .active(initial_calendar_view == CalendarView::Day)
            .build();
        let week_btn = gtk::ToggleButton::builder()
            .label("Woche")
            .group(&day_btn)
            .active(initial_calendar_view == CalendarView::Week)
            .build();
        let workweek_btn = gtk::ToggleButton::builder()
            .label("Arbeitswoche")
            .group(&day_btn)
            .active(initial_calendar_view == CalendarView::WorkWeek)
            .build();
        let month_btn = gtk::ToggleButton::builder()
            .label("Monat")
            .group(&day_btn)
            .active(initial_calendar_view == CalendarView::Month)
            .build();
        let agenda_btn = gtk::ToggleButton::builder()
            .label("Agenda")
            .group(&day_btn)
            .active(initial_calendar_view == CalendarView::Agenda)
            .build();

        view_toggle.append(&day_btn);
        view_toggle.append(&week_btn);
        view_toggle.append(&workweek_btn);
        view_toggle.append(&month_btn);
        view_toggle.append(&agenda_btn);

        view_toolbar.append(&prev_btn);
        view_toolbar.append(&today_btn);
        view_toolbar.append(&next_btn);
        view_toolbar.append(&date_label);
        view_toolbar.append(&btn_availability);
        view_toolbar.append(&btn_add_event);
        view_toolbar.append(&calendar_search_entry);
        view_toolbar.append(&category_selector);
        view_toolbar.append(&view_toggle);

        content_box.append(&view_toolbar);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // View Stack for Day/Week/Month/Agenda view switching
        let calendar_stack = adw::ViewStack::new();

        // 1. Month Grid View
        let month_grid = gtk::Grid::builder()
            .row_homogeneous(true)
            .column_homogeneous(true)
            .row_spacing(1)
            .column_spacing(1)
            .vexpand(true)
            .hexpand(true)
            .margin_start(4)
            .margin_end(4)
            .margin_bottom(4)
            .build();

        self.imp().month_grid.replace(Some(month_grid.clone()));

        let month_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&month_grid)
            .build();

        calendar_stack.add_titled(&month_scroll, Some("month"), "Monat");

        // 2. Day View
        let day_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .spacing(8)
            .build();
        self.imp().day_view.replace(Some(day_view.clone()));
        let day_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&day_view)
            .build();

        calendar_stack.add_titled(&day_scroll, Some("day"), "Tag");

        // 3. Week View
        let week_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .spacing(8)
            .build();
        self.imp().week_view.replace(Some(week_view.clone()));
        let week_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&week_view)
            .build();

        calendar_stack.add_titled(&week_scroll, Some("week"), "Woche");

        // 4. Workweek time grid
        let workweek_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .spacing(8)
            .build();
        self.imp()
            .workweek_view
            .replace(Some(workweek_view.clone()));
        let workweek_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&workweek_view)
            .build();

        calendar_stack.add_titled(&workweek_scroll, Some("workweek"), "Arbeitswoche");

        // 5. Agenda View
        let agenda_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .spacing(8)
            .build();
        self.imp().agenda_view.replace(Some(agenda_view.clone()));
        let agenda_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&agenda_view)
            .build();

        calendar_stack.add_titled(&agenda_scroll, Some("agenda"), "Agenda");

        calendar_stack.set_visible_child_name(match initial_calendar_view {
            CalendarView::Day => "day",
            CalendarView::Week => "week",
            CalendarView::WorkWeek => "workweek",
            CalendarView::Month => "month",
            CalendarView::Agenda => "agenda",
        });
        self.imp()
            .calendar_stack
            .replace(Some(calendar_stack.clone()));
        self.imp().calendar_view_buttons.replace(Some(vec![
            day_btn.clone(),
            week_btn.clone(),
            workweek_btn.clone(),
            month_btn.clone(),
            agenda_btn.clone(),
        ]));

        // View toggle handler
        day_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::Day);
                }
            }
        ));
        week_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::Week);
                }
            }
        ));
        workweek_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::WorkWeek);
                }
            }
        ));
        month_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::Month);
                }
            }
        ));
        agenda_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::Agenda);
                }
            }
        ));

        if let Some(settings) = self.imp().settings.borrow().as_ref().cloned() {
            for key in ["calendar-time-zone", "date-format", "time-format"] {
                let page_weak = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(page) = page_weak.upgrade() {
                        page.refresh_calendar_time_display();
                    }
                });
            }

            let page_weak = self.downgrade();
            settings.connect_changed(Some("calendar-view"), move |_, _| {
                if let Some(page) = page_weak.upgrade() {
                    page.sync_calendar_view_from_settings();
                }
            });

            for key in [
                "calendar-week-start",
                "calendar-workdays",
                "calendar-workday-start",
                "calendar-workday-end",
                "calendar-show-week-numbers",
            ] {
                let page_weak = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(page) = page_weak.upgrade() {
                        page.refresh_calendar_views();
                    }
                });
            }

            let page_weak = self.downgrade();
            settings.connect_changed(Some("calendar-hidden-sources"), move |_, _| {
                if let Some(page) = page_weak.upgrade() {
                    page.refresh_hidden_calendar_sources();
                    page.refresh_task_sidebar();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                }
            });
        }

        // Date navigation handlers
        prev_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.shift_period(-1);
                page.refresh_calendar_views();
            }
        ));

        next_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.shift_period(1);
                page.refresh_calendar_views();
            }
        ));

        today_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let today = Local::now().date_naive();
                page.imp()
                    .view_state
                    .borrow_mut()
                    .today(today, configured_first_weekday());
                *page.imp().current_date.borrow_mut() = today;
                page.sync_mini_calendar();
                page.refresh_calendar_views();
            }
        ));

        calendar_stack.set_vexpand(true);
        content_box.set_vexpand(true);
        content_box.append(&calendar_stack);

        self.refresh_calendar_views();

        let content_page = adw::NavigationPage::builder()
            .title("Kalender")
            .child(&content_box)
            .build();
        split.set_content(Some(&content_page));
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

    fn select_calendar_view(&self, view: CalendarView) {
        if self.imp().syncing_calendar_view.get() {
            return;
        }
        *self.imp().current_view.borrow_mut() = view;
        self.imp()
            .view_state
            .borrow_mut()
            .set_view(view, configured_first_weekday());
        if let Some(stack) = self.imp().calendar_stack.borrow().as_ref() {
            stack.set_visible_child_name(calendar_view_name(view));
        }
        if let Some(settings) = self.imp().settings.borrow().as_ref() {
            settings.set_calendar_view(calendar_view_name(view));
        }
        self.refresh_calendar_views();
    }

    fn sync_calendar_view_from_settings(&self) {
        let view = self
            .imp()
            .settings
            .borrow()
            .as_ref()
            .map(|settings| calendar_view_from_name(&settings.calendar_view()))
            .unwrap_or_else(|| *self.imp().current_view.borrow());

        self.imp().syncing_calendar_view.set(true);
        *self.imp().current_view.borrow_mut() = view;
        self.imp()
            .view_state
            .borrow_mut()
            .set_view(view, configured_first_weekday());
        if let Some(stack) = self.imp().calendar_stack.borrow().as_ref() {
            stack.set_visible_child_name(calendar_view_name(view));
        }
        if let Some(buttons) = self.imp().calendar_view_buttons.borrow().as_ref() {
            for (button, candidate) in buttons.iter().zip([
                CalendarView::Day,
                CalendarView::Week,
                CalendarView::WorkWeek,
                CalendarView::Month,
                CalendarView::Agenda,
            ]) {
                button.set_active(candidate == view);
            }
        }
        self.imp().syncing_calendar_view.set(false);
        self.refresh_calendar_views();
    }

    fn refresh_hidden_calendar_sources(&self) {
        let hidden_sources = self
            .imp()
            .settings
            .borrow()
            .as_ref()
            .map(|settings| settings.calendar_hidden_sources())
            .unwrap_or_default();
        self.imp()
            .hidden_calendar_sources
            .replace(hidden_sources.into_iter().collect());
    }

    fn refresh_task_sidebar(&self) {
        let Some(task_list) = self.imp().task_list.borrow().as_ref().cloned() else {
            return;
        };
        let Some(records) = self.imp().task_records.borrow().as_ref().cloned() else {
            return;
        };

        while let Some(child) = task_list.first_child() {
            task_list.remove(&child);
        }

        let all_count = records.borrow().len();
        let mut tasks = records
            .borrow()
            .iter()
            .filter(|task| self.calendar_source_is_visible(&task.source_uid))
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            left.completed
                .cmp(&right.completed)
                .then_with(|| compare_task_due(&left.due, &right.due))
                .then_with(|| {
                    left.summary
                        .to_lowercase()
                        .cmp(&right.summary.to_lowercase())
                })
        });

        for task in tasks.iter().cloned() {
            append_calendar_task_row(self, &task_list, task);
        }

        let visible_count = tasks.len();
        task_list.set_visible(visible_count > 0);
        if let Some(state) = self.imp().task_state.borrow().as_ref() {
            state.set_visible(visible_count == 0);
            let label = if all_count == 0 {
                "Keine Aufgaben geladen".to_string()
            } else if visible_count == 0 {
                "Keine sichtbaren Aufgaben".to_string()
            } else if visible_count == 1 {
                "1 Aufgabe aus synchronisiertem/lokalem Bestand geladen".to_string()
            } else {
                format!("{visible_count} Aufgaben aus synchronisiertem/lokalem Bestand geladen")
            };
            state.set_label(&label);
        }
    }

    fn shift_period(&self, direction: i32) {
        self.imp()
            .view_state
            .borrow_mut()
            .navigate(direction, configured_first_weekday());
        *self.imp().current_date.borrow_mut() = self.imp().view_state.borrow().anchor_date;
        self.sync_mini_calendar();
    }

    fn sync_mini_calendar(&self) {
        let Some(calendar) = self.imp().mini_calendar.borrow().as_ref().cloned() else {
            return;
        };
        let date = *self.imp().current_date.borrow();
        let Ok(date_time) = glib::DateTime::new(
            &glib::TimeZone::local(),
            date.year(),
            date.month() as i32,
            date.day() as i32,
            0,
            0,
            0.0,
        ) else {
            return;
        };
        self.imp().syncing_mini_calendar.set(true);
        calendar.select_day(&date_time);
        self.imp().syncing_mini_calendar.set(false);
    }

    fn set_calendar_anchor(&self, date: NaiveDate) {
        *self.imp().current_date.borrow_mut() = date;
        self.imp()
            .view_state
            .borrow_mut()
            .set_anchor(date, configured_first_weekday());
    }

    fn current_calendar_viewport(&self) -> CalendarViewport {
        self.imp().view_state.borrow().viewport()
    }

    fn rebuild_calendar_index_from_records(&self) {
        let Some(event_records) = self.imp().event_records.borrow().as_ref().cloned() else {
            return;
        };
        let events = event_records.borrow().clone();
        let tasks = self
            .imp()
            .task_records
            .borrow()
            .as_ref()
            .map(|records| records.borrow().clone())
            .unwrap_or_default();
        let snapshot = Arc::new(CalendarSnapshot {
            events,
            tasks,
            memos: Vec::new(),
        });
        self.imp()
            .calendar_snapshot_index
            .replace(Some(Arc::new(CalendarSnapshotIndex::new(snapshot))));
    }

    fn refresh_calendar_views(&self) {
        let current_date = *self.imp().current_date.borrow();
        let current_view = *self.imp().current_view.borrow();
        self.imp().view_state.borrow_mut().view = current_view;
        self.imp()
            .view_state
            .borrow_mut()
            .set_anchor(current_date, configured_first_weekday());
        self.refresh_task_sidebar();
        self.refresh_calendar_category_model();
        match *self.imp().current_view.borrow() {
            CalendarView::Month => self.refresh_month_grid(),
            CalendarView::Day => self.refresh_day_view(),
            CalendarView::Week => self.refresh_week_view(),
            CalendarView::WorkWeek => self.refresh_workweek_view(),
            CalendarView::Agenda => self.refresh_agenda_view(),
        }
        self.refresh_period_label();
    }

    fn refresh_calendar_time_display(&self) {
        if let Some(records) = self.imp().event_records.borrow().as_ref().cloned() {
            for event in records.borrow_mut().iter_mut() {
                if let Some(start) =
                    property_local_display(&event.component, "DTSTART", event.all_day)
                {
                    event.start = start;
                }
                if let Some(end) = property_local_display(&event.component, "DTEND", event.all_day)
                {
                    event.end = end;
                }
            }

            if let Some(event_list) = self.imp().event_list.borrow().as_ref().cloned() {
                while let Some(child) = event_list.first_child() {
                    event_list.remove(&child);
                }
                for event in records.borrow().iter().cloned() {
                    append_event_row(&event_list, Rc::clone(&records), event);
                }
            }
            self.rebuild_calendar_index_from_records();
        }
        self.refresh_calendar_views();
        self.refresh_event_list_visibility();
    }

    fn refresh_calendar_category_model(&self) {
        let Some(model) = self
            .imp()
            .calendar_category_model
            .borrow()
            .as_ref()
            .cloned()
        else {
            return;
        };
        let Some(selector) = self
            .imp()
            .calendar_category_selector
            .borrow()
            .as_ref()
            .cloned()
        else {
            return;
        };

        let mut categories = CalendarCategoryStore::default()
            .load()
            .into_iter()
            .map(|category| category.name)
            .collect::<Vec<_>>();
        if let Some(records) = self.imp().event_records.borrow().as_ref() {
            categories.extend(
                records
                    .borrow()
                    .iter()
                    .flat_map(|event| category_names_from_ical_component(&event.component)),
            );
        }
        categories.retain(|category| !category.trim().is_empty());
        categories.sort_by_cached_key(|category| category.to_lowercase());
        categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        let current_filter = self.imp().calendar_category_filter.borrow().clone();
        let selected = current_filter
            .as_deref()
            .and_then(|filter| {
                categories
                    .iter()
                    .position(|category| category.eq_ignore_ascii_case(filter))
            })
            .map(|index| index as u32 + 1)
            .unwrap_or(0);
        if current_filter.is_some() && selected == 0 {
            self.imp().calendar_category_filter.replace(None);
        }

        let mut names = vec!["Alle Kategorien".to_string()];
        names.extend(categories);
        let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        model.splice(0, model.n_items(), &name_refs);
        if selector.selected() != selected {
            selector.set_selected(selected);
        }
    }

    fn refresh_event_list_visibility(&self) {
        let Some(event_list) = self.imp().event_list.borrow().as_ref().cloned() else {
            return;
        };
        let mut visible_count = 0usize;
        let mut child = event_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                continue;
            };
            let Some(event_row) = row
                .child()
                .and_then(|child| child.downcast::<adw::ActionRow>().ok())
            else {
                continue;
            };
            let source_uid = unsafe {
                event_row
                    .data::<String>("event-source-uid")
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default()
            };
            let category_matches = unsafe {
                event_row
                    .data::<String>("event-categories")
                    .is_none_or(|value| self.calendar_category_matches(value.as_ref()))
            };
            let search_matches = unsafe {
                event_row
                    .data::<String>("event-search")
                    .is_none_or(|value| self.calendar_search_matches(value.as_ref()))
            };
            let visible =
                self.calendar_source_is_visible(&source_uid) && category_matches && search_matches;
            row.set_visible(visible);
            if visible {
                visible_count += 1;
            }
        }
        event_list.set_visible(visible_count > 0);
    }

    fn refresh_period_label(&self) {
        let Some(label) = self.imp().month_date_label.borrow().as_ref().cloned() else {
            return;
        };
        let viewport = self.current_calendar_viewport();
        let date = viewport.anchor_date;
        let view = viewport.view;
        let text = match view {
            CalendarView::Day => format_date(date),
            CalendarView::Week => {
                format!(
                    "Woche {}",
                    format_calendar_range(
                        viewport.visible_range.start,
                        viewport.visible_range.last_inclusive(),
                    )
                )
            }
            CalendarView::WorkWeek => {
                let days = workweek_display_dates(viewport.visible_range, &configured_workdays());
                let start = days
                    .first()
                    .copied()
                    .unwrap_or(viewport.visible_range.start);
                let end = days
                    .last()
                    .copied()
                    .unwrap_or(viewport.visible_range.last_inclusive());
                format!("Arbeitswoche {}", format_calendar_range(start, end))
            }
            CalendarView::Month => {
                format!("{} {}", Self::month_name(date.month()), date.year())
            }
            CalendarView::Agenda => format!("Agenda ab {}", format_date(date)),
        };
        label.set_label(&text);
    }

    fn refresh_day_view(&self) {
        let Some(view) = self.imp().day_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        let date = self.current_calendar_viewport().visible_range.start;
        let heading = gtk::Label::builder()
            .label(&format_date(date))
            .css_classes(vec!["title-3"])
            .halign(gtk::Align::Start)
            .build();
        view.append(&heading);
        view.append(
            &gtk::Label::builder()
                .label(&format!("Arbeitszeit: {}", configured_workday_text()))
                .css_classes(vec!["dim-label"])
                .halign(gtk::Align::Start)
                .build(),
        );

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        let mut events = self
            .events_by_date(CalendarDateRange::single(date))
            .remove(&date)
            .unwrap_or_default();
        events.sort_by_key(|event| event_start_datetime(event));
        if events.is_empty() {
            list.append(&empty_period_row(
                "Keine Termine",
                "Für diesen Tag sind keine VEVENTs geladen.",
            ));
        } else {
            for event in events {
                list.append(&calendar_event_row(&event));
            }
        }
        view.append(&list);
    }

    fn refresh_week_view(&self) {
        let Some(view) = self.imp().week_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        let week_start = self.current_calendar_viewport().visible_range.start;
        let week_number = AppSettings::try_new()
            .filter(|settings| settings.calendar_show_week_numbers())
            .map(|_| format!(" · KW {:02}", week_start.iso_week().week()))
            .unwrap_or_default();
        let week_heading = gtk::Label::builder()
            .label(&format!(
                "Termine dieser Woche{week_number} · Arbeitszeit: {}",
                configured_workday_text()
            ))
            .css_classes(vec!["title-3"])
            .halign(gtk::Align::Start)
            .build();
        view.append(&week_heading);

        let visible_range = CalendarDateRange::from_days(week_start, 7);
        let events_by_date = self.events_by_date(visible_range);
        for day in visible_range.dates() {
            let day_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .build();
            day_box.add_css_class("card");
            let heading = gtk::Label::builder()
                .label(&format_date(day))
                .halign(gtk::Align::Start)
                .margin_start(10)
                .margin_top(8)
                .build();
            if day == Local::now().date_naive() {
                heading.add_css_class("accent");
            }
            day_box.append(&heading);

            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(vec!["boxed-list"])
                .margin_start(4)
                .margin_end(4)
                .margin_bottom(4)
                .build();
            let mut events = events_by_date.get(&day).cloned().unwrap_or_default();
            events.sort_by_key(|event| event_start_datetime(event));
            if events.is_empty() {
                list.append(&empty_period_row("Keine Termine", ""));
            } else {
                for event in events {
                    list.append(&calendar_event_row(&event));
                }
            }
            day_box.append(&list);
            view.append(&day_box);
        }
    }

    fn refresh_workweek_view(&self) {
        let Some(view) = self.imp().workweek_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);

        let viewport = self.current_calendar_viewport();
        let start_date = viewport.visible_range.start;
        let workdays = configured_workdays();
        let days = workweek_display_dates(viewport.visible_range, &workdays);
        let events_by_date = self.events_by_date(viewport.visible_range);
        let (workday_start, workday_end) = configured_workday_range();
        let slot_count = ((workday_end - workday_start) + 29) / 30;

        view.append(
            &gtk::Label::builder()
                .label(&format!(
                    "Arbeitswoche · {}–{} · Zeitleiste {}–{}",
                    days.first()
                        .map(|day| format_date(*day))
                        .unwrap_or_else(|| format_date(start_date)),
                    days.last()
                        .map(|day| format_date(*day))
                        .unwrap_or_else(|| format_date(start_date)),
                    format_clock_minutes(workday_start),
                    format_clock_minutes(workday_end),
                ))
                .css_classes(vec!["title-3"])
                .halign(gtk::Align::Start)
                .build(),
        );

        let grid = gtk::Grid::builder()
            .column_spacing(2)
            .row_spacing(2)
            .hexpand(true)
            .build();
        grid.attach(
            &gtk::Label::builder()
                .label("Zeit")
                .css_classes(vec!["heading", "dim-label"])
                .margin_end(6)
                .build(),
            0,
            0,
            1,
            1,
        );

        let mut all_day_cells = Vec::with_capacity(days.len());
        for (day_index, day) in days.iter().enumerate() {
            let heading = gtk::Label::builder()
                .label(&format!("{}\n{}.", weekday_name(*day), day.day()))
                .css_classes(vec!["heading"])
                .halign(gtk::Align::Center)
                .hexpand(true)
                .build();
            if *day == Local::now().date_naive() {
                heading.add_css_class("accent");
            }
            grid.attach(&heading, day_index as i32 + 1, 0, 1, 1);

            let all_day_cell = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .width_request(150)
                .build();
            let all_day_frame = gtk::Frame::builder().child(&all_day_cell).build();
            install_day_drop_target(&all_day_frame, self, *day);
            grid.attach(&all_day_frame, day_index as i32 + 1, 1, 1, 1);
            all_day_cells.push(all_day_cell);
        }

        let mut cells = (0..days.len())
            .map(|_| Vec::<gtk::Box>::with_capacity(slot_count as usize))
            .collect::<Vec<_>>();
        for slot in 0..slot_count {
            let row = slot as i32 + 2;
            let minute = workday_start + slot * 30;
            grid.attach(
                &gtk::Label::builder()
                    .label(&format_clock_minutes(minute))
                    .css_classes(vec!["caption", "dim-label"])
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .margin_end(6)
                    .margin_top(6)
                    .build(),
                0,
                row,
                1,
                1,
            );
            for (day_index, day_cells) in cells.iter_mut().enumerate() {
                let cell = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(2)
                    .width_request(150)
                    .height_request(42)
                    .valign(gtk::Align::Fill)
                    .build();
                let frame = gtk::Frame::builder().child(&cell).build();
                if slot % 2 == 0 {
                    frame.add_css_class("view");
                }
                install_day_drop_target(&frame, self, days[day_index]);
                grid.attach(&frame, day_index as i32 + 1, row, 1, 1);
                day_cells.push(cell);
            }
        }

        for (day_index, day) in days.iter().enumerate() {
            let mut events = events_by_date.get(day).cloned().unwrap_or_default();
            events.sort_by_key(event_start_datetime);
            for event in events {
                if event.all_day {
                    all_day_cells[day_index].append(&calendar_event_row(&event));
                    continue;
                }

                let slot = event_start_datetime(&event)
                    .filter(|start| start.date() == *day)
                    .map(|start| {
                        let minutes = start.time().hour() * 60 + start.time().minute();
                        minutes
                            .saturating_sub(workday_start)
                            .checked_div(30)
                            .unwrap_or(0)
                    })
                    .unwrap_or(0)
                    .min(slot_count.saturating_sub(1));
                let title = if event.summary.trim().is_empty() {
                    "(Ohne Titel)"
                } else {
                    event.summary.as_str()
                };
                let event_button = gtk::Button::builder()
                    .halign(gtk::Align::Fill)
                    .valign(gtk::Align::Start)
                    .hexpand(true)
                    .css_classes(vec!["flat", "card"])
                    .build();
                let event_label = gtk::Label::builder()
                    .halign(gtk::Align::Fill)
                    .valign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .margin_start(4)
                    .margin_end(4)
                    .margin_top(3)
                    .margin_bottom(3)
                    .build();
                event_label.set_markup(&calendar_event_markup(
                    &event.component,
                    &format!("{} · {}", event_time_text(&event), title),
                ));
                event_button.set_child(Some(&event_label));
                set_accessible_label(&event_button, &format!("Termin öffnen: {title}"));
                let event_for_click = event.clone();
                event_button.connect_clicked(move |button| {
                    open_event_editor_for_widget(button, &event_for_click);
                });
                install_event_drag_source(&event_button, &event, *day);
                cells[day_index][slot as usize].append(&event_button);
            }
        }

        view.append(&grid);
    }

    fn refresh_agenda_view(&self) {
        let Some(view) = self.imp().agenda_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);

        let visible_range = self.current_calendar_viewport().visible_range;
        let start_date = visible_range.start;
        let end_date = visible_range.last_inclusive();
        view.append(
            &gtk::Label::builder()
                .label(&format!(
                    "Kommende Termine · {} bis {} · Arbeitszeit: {}",
                    format_date(start_date),
                    format_date(end_date),
                    configured_workday_text()
                ))
                .css_classes(vec!["title-3"])
                .halign(gtk::Align::Start)
                .build(),
        );

        let events_by_date = self.events_by_date(visible_range);
        let mut has_events = false;
        for date in visible_range.dates() {
            let mut events = events_by_date.get(&date).cloned().unwrap_or_default();
            events.sort_by_key(|event| event_start_datetime(event));
            if !events.is_empty() {
                has_events = true;
                let date_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(4)
                    .build();
                date_box.add_css_class("card");
                date_box.append(
                    &gtk::Label::builder()
                        .label(&format!(
                            "{} · KW {:02}",
                            format_date(date),
                            date.iso_week().week()
                        ))
                        .css_classes(vec!["title-4"])
                        .halign(gtk::Align::Start)
                        .margin_start(10)
                        .margin_top(8)
                        .build(),
                );
                let list = gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .css_classes(vec!["boxed-list"])
                    .margin_start(4)
                    .margin_end(4)
                    .margin_bottom(4)
                    .build();
                for event in events {
                    list.append(&calendar_event_row(&event));
                }
                date_box.append(&list);
                view.append(&date_box);
            }
        }

        if !has_events {
            view.append(&empty_period_row(
                "Keine kommenden Termine",
                "Für die nächsten 90 Tage sind keine VEVENTs geladen.",
            ));
        }
    }

    fn events_by_date(
        &self,
        range: CalendarDateRange,
    ) -> BTreeMap<NaiveDate, Vec<CalendarEventRecord>> {
        let mut grouped = BTreeMap::<NaiveDate, Vec<CalendarEventRecord>>::new();
        if let Some(index) = self
            .imp()
            .calendar_snapshot_index
            .borrow()
            .as_ref()
            .cloned()
        {
            for event_index in index.candidate_event_indices(range) {
                let Some(event) = index.snapshot.events.get(event_index) else {
                    continue;
                };
                if !self.event_is_visible(event) {
                    continue;
                }
                for date in event_occurrence_dates_in_range(event, range) {
                    grouped.entry(date).or_default().push(event.clone());
                }
            }
        } else if let Some(records) = self.imp().event_records.borrow().as_ref().cloned() {
            for date in range.dates() {
                for event in records.borrow().iter() {
                    if self.event_is_visible(event) && event_occurs_on(event, date) {
                        grouped.entry(date).or_default().push(event.clone());
                    }
                }
            }
        }

        for events in grouped.values_mut() {
            events.sort_by(|left, right| {
                event_start_datetime(left)
                    .cmp(&event_start_datetime(right))
                    .then_with(|| left.source_uid.cmp(&right.source_uid))
                    .then_with(|| left.uid.cmp(&right.uid))
            });
            let mut seen = BTreeSet::new();
            events.retain(|event| seen.insert((event.source_uid.clone(), event.uid.clone())));
        }
        grouped
    }

    fn calendar_source_is_visible(&self, source_uid: &str) -> bool {
        !self
            .imp()
            .hidden_calendar_sources
            .borrow()
            .contains(source_uid)
    }

    fn calendar_category_matches(&self, categories: &str) -> bool {
        self.imp()
            .calendar_category_filter
            .borrow()
            .as_deref()
            .is_none_or(|filter| {
                categories
                    .split('\u{1f}')
                    .any(|category| category.eq_ignore_ascii_case(filter))
            })
    }

    fn calendar_search_matches(&self, search_text: &str) -> bool {
        let query = self.imp().calendar_search_query.borrow();
        query.is_empty() || search_text.contains(query.as_str())
    }

    fn event_is_visible(&self, event: &CalendarEventRecord) -> bool {
        self.calendar_source_is_visible(&event.source_uid)
            && self.calendar_search_matches(&calendar_event_search_text(event))
            && self.calendar_category_matches(
                &category_names_from_ical_component(&event.component).join("\u{1f}"),
            )
    }

    fn refresh_month_grid(&self) {
        let Some(grid) = self.imp().month_grid.borrow().as_ref().cloned() else {
            return;
        };
        let viewport = self.current_calendar_viewport();
        let current_date = viewport.anchor_date;
        let year = current_date.year();
        let month = current_date.month();
        let Some(month_start) = NaiveDate::from_ymd_opt(year, month, 1) else {
            return;
        };
        let today = Local::now().date_naive();
        let show_week_numbers = AppSettings::try_new()
            .map(|settings| settings.calendar_show_week_numbers())
            .unwrap_or(false);
        let column_offset = if show_week_numbers { 1 } else { 0 };
        while let Some(child) = grid.first_child() {
            grid.remove(&child);
        }
        let events_by_date = self.events_by_date(viewport.visible_range);

        for (column, weekday) in configured_weekday_names().iter().enumerate() {
            let label = gtk::Label::builder()
                .label(*weekday)
                .css_classes(vec!["heading", "dim-label"])
                .margin_top(4)
                .margin_bottom(4)
                .build();
            grid.attach(&label, column as i32 + column_offset, 0, 1, 1);
        }
        if show_week_numbers {
            let label = gtk::Label::builder()
                .label("KW")
                .css_classes(vec!["heading", "dim-label"])
                .margin_top(4)
                .margin_bottom(4)
                .build();
            grid.attach(&label, 0, 0, 1, 1);
        }

        for (cell_index, date) in viewport.visible_range.dates().enumerate() {
            let row = (cell_index / 7) as i32 + 1;
            let column = (cell_index % 7) as i32;
            let cell_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_start(4)
                .margin_end(4)
                .margin_top(4)
                .margin_bottom(4)
                .build();

            let cell_frame = gtk::Frame::builder().child(&cell_box).build();
            install_day_drop_target(&cell_frame, self, date);
            let day_label = gtk::Label::builder()
                .label(&date.day().to_string())
                .halign(gtk::Align::Start)
                .css_classes(vec!["caption"])
                .build();
            let outside_month = date.year() != year || date.month() != month;
            if outside_month {
                cell_frame.add_css_class("dim-label");
                day_label.add_css_class("dim-label");
            }
            if date == today {
                day_label.add_css_class("accent");
                day_label.add_css_class("title-4");
            }
            cell_box.append(&day_label);

            let day_events = events_by_date.get(&date).cloned().unwrap_or_default();
            for event in day_events.iter().take(3) {
                let event_label = gtk::Label::builder()
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .xalign(0.0)
                    .hexpand(true)
                    .build();
                event_label.set_markup(&calendar_event_markup(&event.component, &event.summary));
                let event_button = gtk::Button::builder()
                    .halign(gtk::Align::Fill)
                    .hexpand(true)
                    .css_classes(vec!["flat", "caption"])
                    .child(&event_label)
                    .build();
                set_accessible_label(&event_button, &format!("Termin öffnen: {}", event.summary));
                let event_for_click = event.clone();
                event_button.connect_clicked(move |button| {
                    open_event_editor_for_widget(button, &event_for_click);
                });
                install_event_drag_source(&event_button, event, date);
                cell_box.append(&event_button);
            }
            if day_events.len() > 3 {
                let more_label = gtk::Label::builder()
                    .label(&format!("+{} weitere", day_events.len() - 3))
                    .halign(gtk::Align::Start)
                    .css_classes(vec!["caption", "dim-label"])
                    .build();
                cell_box.append(&more_label);
            }

            cell_frame.add_css_class("card");
            grid.attach(&cell_frame, column + column_offset, row, 1, 1);
        }

        if show_week_numbers {
            for row in 0..6_i32 {
                let week_date = viewport
                    .visible_range
                    .start
                    .checked_add_signed(Duration::days(i64::from(row * 7)))
                    .unwrap_or(month_start);
                let label = gtk::Label::builder()
                    .label(&format!("{:02}", week_date.iso_week().week()))
                    .css_classes(vec!["caption", "dim-label"])
                    .valign(gtk::Align::Start)
                    .margin_top(6)
                    .build();
                grid.attach(&label, 0, row + 1, 1, 1);
            }
        }
    }

    fn move_event_by_days(&self, event: CalendarEventRecord, delta_days: i64) {
        if delta_days == 0 {
            return;
        }
        if let Some(error) = event_mutation_error(&event) {
            show_toast_for_widget(self, error);
            return;
        }
        let updated_component = match shift_event_component(&event.component, delta_days) {
            Ok(component) => component,
            Err(error) => {
                show_toast_for_widget(self, &error);
                return;
            }
        };
        let source_uid = event.source_uid.clone();
        let uid = event.uid.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = ECalService::new().modify_component(
                &source_uid,
                ECalComponentType::Event,
                &uid,
                &updated_component,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Event move result was dropped");
            }
        });

        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            match result {
                Ok(result) => {
                    let component = match validated_event_mutation_component(&event, &result) {
                        Ok(component) => component,
                        Err(error) => {
                            show_toast_for_widget(&page, &error);
                            return;
                        }
                    };
                    let mut updated_event = event.clone();
                    updated_event.component = component.clone();
                    updated_event.start =
                        property_local_display(&component, "DTSTART", updated_event.all_day)
                            .unwrap_or_default();
                    updated_event.end =
                        property_local_display(&component, "DTEND", updated_event.all_day)
                            .unwrap_or_default();
                    let records = page.imp().event_records.borrow().as_ref().cloned();
                    if let Some(records) = records {
                        let mut records = records.borrow_mut();
                        if let Some(existing) = records.iter_mut().find(|existing| {
                            same_event_identity(
                                existing,
                                &updated_event.source_uid,
                                &updated_event.uid,
                            )
                        }) {
                            *existing = updated_event;
                        }
                    }
                    page.rebuild_calendar_index_from_records();
                    page.refresh_calendar_views();
                    show_toast_for_widget(&page, &format!("Termin verschoben ({})", result.uid));
                }
                Err(error) => {
                    show_toast_for_widget(
                        &page,
                        &format!("Termin konnte nicht verschoben werden: {error}"),
                    );
                }
            }
        });
    }
}

fn install_event_drag_source<W: IsA<gtk::Widget>>(
    widget: &W,
    event: &CalendarEventRecord,
    occurrence_date: NaiveDate,
) {
    if event.source_uid.trim().is_empty() || event.uid.trim().is_empty() {
        return;
    }
    let payload = format!(
        "{}\n{}\n{}",
        event.source_uid,
        event.uid,
        occurrence_date.format("%Y-%m-%d")
    );
    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(gdk::DragAction::MOVE);
    drag_source
        .connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
    widget.add_controller(drag_source);
}

fn install_day_drop_target<W: IsA<gtk::Widget>>(
    widget: &W,
    page: &CalendarPage,
    target_date: NaiveDate,
) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let page_weak = page.downgrade();
    drop_target.connect_drop(move |_, value, _, _| {
        let Ok(payload) = value.get::<String>() else {
            return false;
        };
        let mut payload_parts = payload.splitn(3, '\n');
        let Some(source_uid) = payload_parts.next() else {
            return false;
        };
        let Some(uid) = payload_parts.next() else {
            return false;
        };
        let occurrence_date = payload_parts
            .next()
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
        let Some(page) = page_weak.upgrade() else {
            return false;
        };
        let records = page.imp().event_records.borrow().as_ref().cloned();
        let Some(event) = records.and_then(|records| {
            records
                .borrow()
                .iter()
                .find(|event| event.source_uid == source_uid && event.uid == uid)
                .cloned()
        }) else {
            show_toast_for_widget(&page, "Der gezogene Termin ist nicht mehr geladen");
            return false;
        };
        let Some(start_date) = occurrence_date.or_else(|| event_start_date(&event)) else {
            show_toast_for_widget(&page, "Der Termin besitzt kein gültiges Startdatum");
            return false;
        };
        page.move_event_by_days(event, (target_date - start_date).num_days());
        true
    });
    widget.add_controller(drop_target);
}

fn shift_event_component(raw: &str, delta_days: i64) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("Der Termin enthält keine iCalendar-Daten".to_string());
    }

    let mut updated = raw.to_string();
    for property_name in ["DTSTART", "DTEND", "RECURRENCE-ID"] {
        let Some(value) = ical_property(&updated, property_name) else {
            continue;
        };
        let Some(shifted) = shift_ical_datetime_value(&value, delta_days) else {
            return Err(format!(
                "{property_name} konnte beim Verschieben nicht ausgewertet werden"
            ));
        };
        let property_prefix = ical_property_prefix(&updated, property_name)
            .unwrap_or_else(|| property_name.to_string());
        updated = update_ical_property(&updated, &property_prefix, Some(&shifted));
    }
    for property_name in ["RDATE", "EXDATE"] {
        updated = shift_ical_date_list_property(&updated, property_name, delta_days)?;
    }
    updated = shift_rrule(&updated, delta_days)?;
    Ok(updated)
}

fn shift_ical_date_list_property(
    raw: &str,
    property_name: &str,
    delta_days: i64,
) -> Result<String, String> {
    let target = property_name.to_ascii_uppercase();
    let mut lines = Vec::new();
    let mut component_depth = 0usize;
    let mut replaced = false;

    for line in unfold_ical_lines(raw) {
        let Some((property, value)) = line.split_once(':') else {
            lines.push(line);
            continue;
        };
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        if name == target && component_depth <= 1 {
            let shifted_values = value
                .split(',')
                .map(|value| {
                    shift_ical_datetime_value(value, delta_days).ok_or_else(|| {
                        format!("{property_name} konnte beim Verschieben nicht ausgewertet werden")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            // Keep every line's own TZID/VALUE parameters. Combining lines
            // under the first prefix would silently change a mixed-zone
            // EXDATE/RDATE set.
            lines.push(format!("{property}:{}", shifted_values.join(",")));
            replaced = true;
        } else {
            lines.push(line);
        }

        if name == "BEGIN" {
            component_depth = component_depth.saturating_add(1);
        } else if name == "END" {
            component_depth = component_depth.saturating_sub(1);
        }
    }

    if !replaced {
        return Ok(raw.to_string());
    }
    Ok(format!("{}\r\n", lines.join("\r\n")))
}

fn shift_rrule(raw: &str, delta_days: i64) -> Result<String, String> {
    let Some(rule) = ical_property(raw, "RRULE") else {
        return Ok(raw.to_string());
    };
    let parsed_rule = parse_recurrence_rule(&rule).ok_or_else(|| {
        "Die Wiederholungsregel ist ungültig und wurde nicht verschoben".to_string()
    })?;

    // A date shift is exact for daily rules and weekly rules. Weekly BYDAY
    // values must move with DTSTART; leaving them untouched would create a
    // different series. More complex monthly/yearly filters cannot be
    // translated losslessly by a simple drag, so report that fact instead of
    // writing a plausible-looking but wrong recurrence.
    if !matches!(parsed_rule.frequency.as_str(), "DAILY" | "WEEKLY") {
        return Err(
            "Diese Wiederholung kann nicht sicher per Ziehen verschoben werden; bitte den Termin bearbeiten"
                .to_string(),
        );
    }

    let mut changed = false;
    let shifted_rule = rule
        .split(';')
        .map(|part| {
            let Some((key, value)) = part.split_once('=') else {
                return Ok(part.to_string());
            };
            if key.eq_ignore_ascii_case("UNTIL") {
                let shifted = shift_ical_datetime_value(value, delta_days).ok_or_else(|| {
                    "RRULE-UNTIL konnte beim Verschieben nicht ausgewertet werden".to_string()
                })?;
                changed = true;
                return Ok(format!("{key}={shifted}"));
            }
            if key.eq_ignore_ascii_case("BYDAY") {
                let shifted_days = value
                    .split(',')
                    .map(|token| {
                        let by_day = parse_extended_weekday_token(token).ok_or_else(|| {
                            "RRULE-BYDAY konnte beim Verschieben nicht ausgewertet werden"
                                .to_string()
                        })?;
                        Ok(format_shifted_recurrence_day(by_day, delta_days))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                changed = true;
                return Ok(format!("{key}={}", shifted_days.join(",")));
            }
            if matches!(
                key.to_ascii_uppercase().as_str(),
                "BYMONTHDAY" | "BYMONTH" | "BYSETPOS"
            ) {
                return Err(
                    "Diese Wiederholung enthält Filter, die beim Ziehen nicht sicher verschoben werden können"
                        .to_string(),
                );
            }
            Ok(part.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?
        .join(";");
    if !changed {
        return Ok(raw.to_string());
    }
    let property_prefix = ical_property_prefix(raw, "RRULE").unwrap_or_else(|| "RRULE".to_string());
    Ok(update_ical_property(
        raw,
        &property_prefix,
        Some(&shifted_rule),
    ))
}

fn format_shifted_recurrence_day(by_day: RecurrenceByDay, delta_days: i64) -> String {
    let weekday_index =
        (i64::from(by_day.weekday.num_days_from_monday()) + delta_days).rem_euclid(7);
    let weekday = match weekday_index {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    };
    let token = match weekday {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    };
    by_day
        .ordinal
        .map(|ordinal| format!("{ordinal}{token}"))
        .unwrap_or_else(|| token.to_string())
}

fn ical_property_prefix(raw: &str, target: &str) -> Option<String> {
    let target = target.to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, _) = line.split_once(':')?;
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (name == target).then(|| property.to_string())
    })
}

fn shift_ical_datetime_value(value: &str, delta_days: i64) -> Option<String> {
    let value = value.trim();
    if value.len() == 8 {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some(
            (date + Duration::days(delta_days))
                .format("%Y%m%d")
                .to_string(),
        );
    }
    if value.len() < 15 || value.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    if !value.is_char_boundary(15) {
        return None;
    }
    let datetime = NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()?;
    let suffix = &value[15..];
    Some(format!(
        "{}{}",
        (datetime + Duration::days(delta_days)).format("%Y%m%dT%H%M%S"),
        suffix
    ))
}

fn week_start_for(date: NaiveDate, first_weekday: Weekday) -> NaiveDate {
    let current = i64::from(date.weekday().num_days_from_monday());
    let first = i64::from(first_weekday.num_days_from_monday());
    date - Duration::days((current - first).rem_euclid(7))
}

fn configured_first_weekday() -> Weekday {
    AppSettings::try_new()
        .map(|settings| settings.calendar_week_start())
        .filter(|value| value == "sunday")
        .map(|_| Weekday::Sun)
        .unwrap_or(Weekday::Mon)
}

fn configured_workdays() -> Vec<Weekday> {
    let value = AppSettings::try_new()
        .map(|settings| settings.calendar_workdays())
        .unwrap_or_else(|| "monday,tuesday,wednesday,thursday,friday".to_string());
    let days = value
        .split(',')
        .filter_map(|day| match day {
            "monday" => Some(Weekday::Mon),
            "tuesday" => Some(Weekday::Tue),
            "wednesday" => Some(Weekday::Wed),
            "thursday" => Some(Weekday::Thu),
            "friday" => Some(Weekday::Fri),
            "saturday" => Some(Weekday::Sat),
            "sunday" => Some(Weekday::Sun),
            _ => None,
        })
        .collect::<Vec<_>>();
    if days.is_empty() {
        vec![
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ]
    } else {
        days
    }
}

fn workweek_display_dates(basis_range: CalendarDateRange, workdays: &[Weekday]) -> Vec<NaiveDate> {
    basis_range
        .dates()
        .filter(|date| workdays.contains(&date.weekday()))
        .collect()
}

fn configured_weekday_names() -> [&'static str; 7] {
    if configured_first_weekday() == Weekday::Sun {
        ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"]
    } else {
        ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"]
    }
}

fn configured_workday_text() -> String {
    let (start, end) = configured_workday_range();
    format!(
        "{}–{}",
        format_clock_minutes(start),
        format_clock_minutes(end)
    )
}

fn configured_workday_range() -> (u32, u32) {
    let settings = AppSettings::try_new();
    let configured_start = settings
        .as_ref()
        .map(|settings| settings.calendar_workday_start_minutes())
        .unwrap_or(540);
    let configured_end = settings
        .as_ref()
        .map(|settings| settings.calendar_workday_end_minutes())
        .unwrap_or(1_020);
    normalized_workday_range(configured_start, configured_end)
}

fn normalized_workday_range(configured_start: u32, configured_end: u32) -> (u32, u32) {
    let start = configured_start.min(1_410);
    let end = configured_end.max(start.saturating_add(30)).min(1_440);
    (start, end)
}

fn format_clock_minutes(minutes: u32) -> String {
    let hour = (minutes / 60) % 24;
    let minute = minutes % 60;
    let use_twelve_hour_clock =
        AppSettings::try_new().is_some_and(|settings| settings.time_format() == "12h");
    if use_twelve_hour_clock {
        let suffix = if hour < 12 { "AM" } else { "PM" };
        let display_hour = match hour % 12 {
            0 => 12,
            value => value,
        };
        format!("{display_hour}:{minute:02} {suffix}")
    } else {
        format!("{hour:02}:{minute:02}")
    }
}

fn shift_month(date: NaiveDate, direction: i32) -> NaiveDate {
    let mut year = date.year();
    let mut month = date.month() as i32 + direction;
    if month == 0 {
        year -= 1;
        month = 12;
    } else if month == 13 {
        year += 1;
        month = 1;
    }
    NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(date)
}

fn weekday_name(date: NaiveDate) -> &'static str {
    [
        "Montag",
        "Dienstag",
        "Mittwoch",
        "Donnerstag",
        "Freitag",
        "Samstag",
        "Sonntag",
    ][date.weekday().num_days_from_monday() as usize]
}

fn format_calendar_range(start: NaiveDate, end: NaiveDate) -> String {
    match AppSettings::try_new()
        .map(|settings| settings.date_format())
        .as_deref()
    {
        Some("iso") => format!(
            "{} \u{2013} {}",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        ),
        Some("us") => format!(
            "{:02}/{:02}/{} \u{2013} {:02}/{:02}/{}",
            start.month(),
            start.day(),
            start.year(),
            end.month(),
            end.day(),
            end.year()
        ),
        _ if start.month() == end.month() && start.year() == end.year() => format!(
            "{}.\u{2013}{}. {} {}",
            start.day(),
            end.day(),
            CalendarPage::month_name(start.month()),
            start.year()
        ),
        _ => format!(
            "{}. {} {} \u{2013} {}. {} {}",
            start.day(),
            CalendarPage::month_name(start.month()),
            start.year(),
            end.day(),
            CalendarPage::month_name(end.month()),
            end.year()
        ),
    }
}

fn format_date(date: NaiveDate) -> String {
    match AppSettings::try_new()
        .map(|settings| settings.date_format())
        .as_deref()
    {
        Some("iso") => date.format("%Y-%m-%d").to_string(),
        Some("us") => format!("{:02}/{:02}/{}", date.month(), date.day(), date.year()),
        _ => format!(
            "{}, {}. {} {}",
            weekday_name(date),
            date.day(),
            CalendarPage::month_name(date.month()),
            date.year()
        ),
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn empty_period_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .sensitive(false)
        .build()
}

fn compare_task_due(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.trim().is_empty(), right.trim().is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left.cmp(right),
    }
}

fn format_calendar_task_date(value: &str) -> String {
    let date = value.split_whitespace().next().unwrap_or(value);
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(format_date)
        .unwrap_or_else(|_| value.to_string())
}

fn calendar_task_subtitle(task: &TaskRecord) -> String {
    let mut details = Vec::new();
    if !task.due.trim().is_empty() {
        details.push(format!("Fällig: {}", format_calendar_task_date(&task.due)));
    }
    if task.priority > 0 {
        let priority = match task.priority {
            1..=4 => "hoch",
            5 => "normal",
            _ => "niedrig",
        };
        details.push(format!("Priorität: {priority}"));
    }
    details.push(if task.completed {
        "Erledigt".to_string()
    } else {
        "Offen".to_string()
    });
    if !task.source_display_name.trim().is_empty() {
        details.push(task.source_display_name.clone());
    }
    details.join(" · ")
}

fn set_task_completion_component(raw: &str, completed: bool) -> String {
    let status = if completed {
        "COMPLETED"
    } else {
        "NEEDS-ACTION"
    };
    let percent = if completed { "100" } else { "0" };
    let mut component = update_ical_property(raw, "STATUS", Some(status));
    component = update_ical_property(&component, "PERCENT-COMPLETE", Some(percent));
    if completed {
        let completed_at = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        update_ical_property(&component, "COMPLETED", Some(&completed_at))
    } else {
        update_ical_property(&component, "COMPLETED", None)
    }
}

fn append_calendar_task_row(page: &CalendarPage, task_list: &gtk::ListBox, task: TaskRecord) {
    let title = if task.summary.trim().is_empty() {
        "(Ohne Betreff)".to_string()
    } else {
        task.summary.clone()
    };
    let row = adw::ActionRow::builder()
        .title(&title)
        .subtitle(&calendar_task_subtitle(&task))
        .build();
    if task.completed {
        row.add_css_class("dim-label");
    }

    let check = gtk::CheckButton::builder()
        .active(task.completed)
        .sensitive(!task.source_uid.trim().is_empty())
        .valign(gtk::Align::Center)
        .tooltip_text(if task.source_uid.trim().is_empty() {
            "Importierte Aufgaben sind schreibgeschützt"
        } else {
            "Aufgabe als erledigt markieren"
        })
        .build();
    row.add_prefix(&check);
    task_list.append(&row);

    if task.source_uid.trim().is_empty() {
        return;
    }

    let page_weak = page.downgrade();
    check.connect_toggled(move |check| {
        let completed = check.is_active();
        let task = task.clone();
        if completed == task.completed {
            return;
        }
        let source_uid = task.source_uid.clone();
        let uid = task.uid.clone();
        let updated_component = set_task_completion_component(&task.component, completed);
        let (sender, receiver) = async_channel::bounded(1);
        let component_for_worker = updated_component.clone();
        std::thread::spawn(move || {
            let result = ECalService::new().modify_component(
                &source_uid,
                ECalComponentType::Task,
                &uid,
                &component_for_worker,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task completion result was dropped");
            }
        });

        check.set_sensitive(false);
        let check_for_result = check.clone();
        let page_weak = page_weak.clone();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            match result {
                Ok(result) => {
                    let component = result.component.unwrap_or(updated_component);
                    if let Some(records) = page.imp().task_records.borrow().as_ref().cloned() {
                        if let Some(existing) = records.borrow_mut().iter_mut().find(|existing| {
                            existing.source_uid == task.source_uid && existing.uid == task.uid
                        }) {
                            existing.component = component;
                            existing.completed = completed;
                        }
                    }
                    check_for_result.set_sensitive(true);
                    page.refresh_task_sidebar();
                    show_toast_for_widget(
                        &page,
                        if completed {
                            "Aufgabe erledigt"
                        } else {
                            "Aufgabe wieder geöffnet"
                        },
                    );
                }
                Err(error) => {
                    check_for_result.set_active(task.completed);
                    check_for_result.set_sensitive(true);
                    show_toast_for_widget(
                        &page,
                        &format!("Aufgabe konnte nicht geändert werden: {error}"),
                    );
                }
            }
        });
    });
}

fn upsert_task_record(task_records: Rc<RefCell<Vec<TaskRecord>>>, task: TaskRecord) {
    task_records
        .borrow_mut()
        .retain(|existing| existing.source_uid != task.source_uid || existing.uid != task.uid);
    task_records.borrow_mut().push(task);
}

fn remove_task_record(task_records: Rc<RefCell<Vec<TaskRecord>>>, source_uid: &str, uid: &str) {
    task_records
        .borrow_mut()
        .retain(|task| task.source_uid != source_uid || task.uid != uid);
}

fn calendar_event_search_text(event: &CalendarEventRecord) -> String {
    format!(
        "{} {} {} {} {} {}",
        event.summary,
        event.location,
        event.description,
        event.start,
        event.end,
        event.source_display_name,
    )
    .to_lowercase()
}

fn calendar_event_row(event: &CalendarEventRecord) -> adw::ActionRow {
    let mut details = vec![event_time_text(event)];
    if !event.location.trim().is_empty() {
        details.push(event.location.trim().to_string());
    }
    if !event.source_display_name.trim().is_empty() {
        details.push(event.source_display_name.trim().to_string());
    }
    let row = adw::ActionRow::builder()
        .title(
            glib::markup_escape_text(if event.summary.trim().is_empty() {
                "(Ohne Titel)"
            } else {
                event.summary.as_str()
            })
            .as_str(),
        )
        .subtitle(glib::markup_escape_text(&details.join(" · ")).as_str())
        .use_markup(true)
        .activatable(true)
        .build();
    if let Some(color) = category_color_for_component(&event.component) {
        let category_dot = gtk::Label::new(None);
        category_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
        category_dot.set_tooltip_text(Some("Kalenderkategorie"));
        row.add_prefix(&category_dot);
    }
    let event_for_activation = event.clone();
    row.connect_activated(move |row| {
        open_event_editor_for_widget(row, &event_for_activation);
    });
    row
}

fn open_event_editor_for_widget<W: IsA<gtk::Widget>>(widget: &W, event: &CalendarEventRecord) {
    if let Some(error) = event_mutation_error(event) {
        show_toast_for_widget(widget, error);
        return;
    }
    let dialog = crate::dialogs::EventDialog::new_for_edit(event);
    if let Some(root) = widget.root() {
        let evolution_window = root
            .clone()
            .downcast::<crate::window::EvolutionWindow>()
            .ok();
        if let Ok(window) = root.downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&window));
        }
        if let Some(window) = evolution_window {
            let window = window.downgrade();
            dialog.connect_close_request(move |_| {
                if let Some(window) = window.upgrade() {
                    window.refresh_calendar_page();
                }
                glib::Propagation::Proceed
            });
        }
    }
    dialog.present();
}

fn event_time_text(event: &CalendarEventRecord) -> String {
    if event.all_day || event.start.len() <= 10 {
        return "Ganztägig".to_string();
    }
    let start = event
        .start
        .get(11..16)
        .map(format_clock_text)
        .unwrap_or_else(|| event.start.clone());
    if event.end.len() > 15 {
        let end = event
            .end
            .get(11..16)
            .map(format_clock_text)
            .unwrap_or_else(|| event.end.clone());
        format!("{start}–{end}")
    } else {
        start
    }
}

fn calendar_event_markup(component: &str, text: &str) -> String {
    let escaped_text = glib::markup_escape_text(text);
    match category_color_for_component(component) {
        Some(color) => format!("<span foreground=\"{color}\">●</span> {escaped_text}"),
        None => escaped_text.to_string(),
    }
}

fn category_color_for_component(component: &str) -> Option<String> {
    CalendarCategoryStore::default().color_for_component(component)
}

fn format_clock_text(value: &str) -> String {
    let Some((hour, minute)) = value.split_once(':') else {
        return value.to_string();
    };
    let Ok(hour) = hour.parse::<u32>() else {
        return value.to_string();
    };
    let Ok(minute) = minute.parse::<u32>() else {
        return value.to_string();
    };
    format_clock_minutes(hour.saturating_mul(60).saturating_add(minute))
}

fn event_start_date(event: &CalendarEventRecord) -> Option<NaiveDate> {
    property_local_date(&event.component, "DTSTART", event.all_day)
        .or_else(|| NaiveDate::parse_from_str(event.start.get(..10)?, "%Y-%m-%d").ok())
}

fn event_start_datetime(event: &CalendarEventRecord) -> Option<NaiveDateTime> {
    property_local_datetime(&event.component, "DTSTART", event.all_day)
        .or_else(|| displayed_event_datetime(&event.start))
}

fn event_end_datetime(event: &CalendarEventRecord) -> Option<NaiveDateTime> {
    property_local_datetime(&event.component, "DTEND", event.all_day)
        .or_else(|| displayed_event_datetime(&event.end))
}

fn displayed_event_datetime(value: &str) -> Option<NaiveDateTime> {
    if let Some(value) = value.get(..16) {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M") {
            return Some(datetime);
        }
    }
    NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)
}

/// Build an index span from the already-normalized record fields.  This path
/// is intentionally limited to `chrono`/plain strings: the snapshot worker
/// must not open GSettings or construct GLib/GObject values merely to build
/// its immutable candidate index.  The main-thread occurrence path still uses
/// the component-aware timezone conversion below.
fn record_display_range(event: &CalendarEventRecord) -> Option<CalendarDateRange> {
    let start_datetime = displayed_event_datetime(&event.start)?;
    let start = start_datetime.date();
    if event.all_day {
        let end = displayed_event_datetime(&event.end)
            .map(|datetime| datetime.date())
            .filter(|end| *end > start)
            .or_else(|| start.succ_opt())?;
        return Some(CalendarDateRange::new(start, end));
    }

    let end_datetime = displayed_event_datetime(&event.end).unwrap_or(start_datetime);
    if end_datetime <= start_datetime {
        return Some(CalendarDateRange::single(start));
    }
    let end = if end_datetime.time() == chrono::NaiveTime::from_hms_opt(0, 0, 0)? {
        end_datetime.date()
    } else {
        end_datetime.date().succ_opt()?
    };
    Some(CalendarDateRange::new(start, end.max(start.succ_opt()?)))
}

fn event_display_range(event: &CalendarEventRecord) -> Option<CalendarDateRange> {
    let start = event_start_date(event)?;
    if event.all_day {
        let end = property_local_date(&event.component, "DTEND", true)
            .or_else(|| NaiveDate::parse_from_str(event.end.get(..10)?, "%Y-%m-%d").ok())
            .filter(|end| *end > start)
            .or_else(|| start.succ_opt())?;
        return Some(CalendarDateRange::new(start, end));
    }

    let start_datetime = event_start_datetime(event)?;
    let end_datetime = event_end_datetime(event).unwrap_or(start_datetime);
    if end_datetime <= start_datetime {
        return Some(CalendarDateRange::single(start_datetime.date()));
    }

    // DTEND is exclusive for an event interval.  An event ending exactly at
    // midnight therefore does not occupy the following calendar day.
    let end = if end_datetime.time().hour() == 0
        && end_datetime.time().minute() == 0
        && end_datetime.time().second() == 0
        && end_datetime.time().nanosecond() == 0
    {
        end_datetime.date()
    } else {
        end_datetime.date().succ_opt()?
    };
    Some(CalendarDateRange::new(
        start_datetime.date(),
        end.max(start_datetime.date().succ_opt()?),
    ))
}

fn event_occurrence_dates_in_range(
    event: &CalendarEventRecord,
    range: CalendarDateRange,
) -> Vec<NaiveDate> {
    let Some(initial_range) = event_display_range(event) else {
        return Vec::new();
    };
    let initial_start = initial_range.start;
    let span_days = initial_range.len_days().saturating_sub(1);
    let excluded_dates = exdate_values(event);
    let rdate_values = property_local_dates(&event.component, "RDATE");
    let recurrence_rule =
        ical_property(&event.component, "RRULE").and_then(|raw| parse_recurrence_rule(&raw));
    let mut occurrence_starts = BTreeSet::new();

    if !excluded_dates.contains(&initial_start) {
        occurrence_starts.insert(initial_start);
    }
    for rdate in rdate_values {
        if !excluded_dates.contains(&rdate) {
            occurrence_starts.insert(rdate);
        }
    }

    if let Some(rule) = recurrence_rule.as_ref() {
        let from = range
            .start
            .checked_sub_signed(Duration::days(span_days))
            .unwrap_or(range.start)
            .max(initial_start);
        let mut cursor = from;
        while let Some(occurrence) =
            next_recurrence_date(rule, initial_start, cursor, &excluded_dates)
        {
            if occurrence >= range.end {
                break;
            }
            occurrence_starts.insert(occurrence);
            let Some(next) = occurrence.succ_opt() else {
                break;
            };
            cursor = next;
        }
    }

    let mut dates = BTreeSet::new();
    for occurrence_start in occurrence_starts {
        for offset in 0..=span_days {
            let Some(date) = occurrence_start.checked_add_signed(Duration::days(offset)) else {
                break;
            };
            if range.contains(date) {
                dates.insert(date);
            }
            if date >= range.last_inclusive() {
                break;
            }
        }
    }
    dates.into_iter().collect()
}

fn event_occurs_on(event: &CalendarEventRecord, date: NaiveDate) -> bool {
    let Some(range) = event_display_range(event) else {
        return false;
    };
    let start = range.start;
    let excluded_dates = exdate_values(event);
    let rdate_values = property_local_dates(&event.component, "RDATE");
    let recurrence_rule =
        ical_property(&event.component, "RRULE").and_then(|raw| parse_recurrence_rule(&raw));
    (0..range.len_days()).any(|offset| {
        let Some(candidate) = date.checked_sub_signed(Duration::days(offset)) else {
            return false;
        };
        event_occurrence_start_is_on(
            candidate,
            start,
            &excluded_dates,
            &rdate_values,
            recurrence_rule.as_ref(),
        )
    })
}

fn event_occurrence_start_is_on(
    date: NaiveDate,
    initial_start: NaiveDate,
    excluded_dates: &[NaiveDate],
    rdate_values: &[NaiveDate],
    recurrence_rule: Option<&crate::services::recurrence_service::RecurrenceRule>,
) -> bool {
    if excluded_dates.contains(&date) {
        return false;
    }
    if date == initial_start || rdate_values.contains(&date) {
        return true;
    }
    recurrence_rule
        .is_some_and(|rule| recurrence_rule_occurs_on(rule, initial_start, date, excluded_dates))
}

fn same_event_identity(event: &CalendarEventRecord, source_uid: &str, uid: &str) -> bool {
    event.source_uid == source_uid && event.uid == uid
}

fn event_mutation_error(event: &CalendarEventRecord) -> Option<&'static str> {
    if event.source_uid.trim().is_empty() {
        return Some("Dieser Termin stammt aus einer schreibgeschützten importierten Quelle");
    }
    if event.uid.trim().is_empty() {
        return Some("Dieser Termin besitzt keine stabile UID und kann nicht geändert werden");
    }
    if event.component.trim().is_empty() {
        return Some("Für diesen Termin sind keine iCalendar-Daten verfügbar");
    }
    match ical_property(&event.component, "UID") {
        Some(uid) if uid == event.uid => None,
        Some(_) => Some("Die Termin-UID und die iCalendar-UID stimmen nicht überein"),
        None => Some("Die iCalendar-Komponente besitzt keine stabile UID"),
    }
}

fn validated_event_mutation_component(
    event: &CalendarEventRecord,
    result: &ECalMutationResult,
) -> Result<String, String> {
    if result.uid != event.uid {
        return Err(
            "Der Kalenderdienst lieferte eine andere Termin-UID zurück; die Ansicht wurde nicht geändert"
                .to_string(),
        );
    }
    let component = result
        .component
        .clone()
        .filter(|component| !component.trim().is_empty())
        .ok_or_else(|| {
            "Der Kalenderdienst bestätigte die Änderung ohne iCalendar-Komponente; die Ansicht wurde nicht geändert"
                .to_string()
        })?;
    if ical_property(&component, "UID").as_deref() != Some(event.uid.as_str()) {
        return Err(
            "Der Kalenderdienst lieferte eine iCalendar-Komponente mit anderer UID zurück; die Ansicht wurde nicht geändert"
                .to_string(),
        );
    }
    Ok(component)
}

fn exdate_values(event: &CalendarEventRecord) -> Vec<NaiveDate> {
    property_local_dates(&event.component, "EXDATE")
}

fn ical_property(raw: &str, target: &str) -> Option<String> {
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (name == target.to_ascii_uppercase()).then(|| unescape_ical_text(value))
    })
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

fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn upsert_event_row(
    event_list: &gtk::ListBox,
    event_records: Rc<RefCell<Vec<CalendarEventRecord>>>,
    event: CalendarEventRecord,
) {
    let existing_index = event_records
        .borrow()
        .iter()
        .position(|existing| same_event_identity(existing, &event.source_uid, &event.uid));
    if let Some(index) = existing_index {
        event_records.borrow_mut().remove(index);
        remove_event_list_row(event_list, &event.source_uid, &event.uid);
    }
    event_records.borrow_mut().push(event.clone());
    append_event_row(event_list, event_records, event);
}

fn remove_event_row(
    event_list: &gtk::ListBox,
    event_records: Rc<RefCell<Vec<CalendarEventRecord>>>,
    source_uid: &str,
    uid: &str,
) {
    let Some(index) = event_records
        .borrow()
        .iter()
        .position(|event| same_event_identity(event, source_uid, uid))
    else {
        return;
    };
    event_records.borrow_mut().remove(index);
    remove_event_list_row(event_list, source_uid, uid);
}

fn append_event_row(
    event_list: &gtk::ListBox,
    event_records: Rc<RefCell<Vec<CalendarEventRecord>>>,
    event: CalendarEventRecord,
) {
    let source = if event.source_display_name.is_empty() {
        String::new()
    } else {
        format!(" · {}", event.source_display_name)
    };
    let title = glib::markup_escape_text(&event.summary);
    let subtitle_text = format!(
        "{}{}",
        if event.end.is_empty() {
            event.start.clone()
        } else {
            format!("{}–{}", event.start, event.end)
        },
        source
    );
    let row = adw::ActionRow::builder()
        .title(title.as_str())
        .subtitle(glib::markup_escape_text(&subtitle_text).as_str())
        .use_markup(true)
        .activatable(true)
        .build();
    unsafe {
        row.set_data("event-id", event.uid.clone());
        row.set_data("event-source-uid", event.source_uid.clone());
        row.set_data(
            "event-categories",
            category_names_from_ical_component(&event.component).join("\u{1f}"),
        );
        row.set_data("event-search", calendar_event_search_text(&event));
    }
    let event_for_activation = event.clone();
    row.connect_activated(move |row| {
        open_event_editor_for_widget(row, &event_for_activation);
    });

    let edit_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Termin bearbeiten")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&edit_button, "Termin bearbeiten");
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Termin löschen")
        .css_classes(vec!["flat", "destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&delete_button, "Termin löschen");
    let export_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Termin als iCalendar exportieren")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&export_button, "Termin als iCalendar exportieren");
    row.add_suffix(&edit_button);
    row.add_suffix(&export_button);
    row.add_suffix(&delete_button);

    let event_for_edit = event.clone();
    let edit_button_for_context = edit_button.clone();
    let edit_button_for_signal = edit_button.clone();
    let edit_button_for_click = edit_button.clone();
    edit_button_for_signal.connect_clicked(move |_| {
        if let Some(error) = event_mutation_error(&event_for_edit) {
            show_toast_for_widget(&edit_button_for_click, error);
            return;
        }
        let dialog = crate::dialogs::EventDialog::new_for_edit(&event_for_edit);
        if let Some(root) = edit_button_for_click.root() {
            let evolution_window = root
                .clone()
                .downcast::<crate::window::EvolutionWindow>()
                .ok();
            if let Ok(window) = root.downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&window));
            }
            if let Some(window) = evolution_window {
                let window = window.downgrade();
                dialog.connect_close_request(move |_| {
                    if let Some(window) = window.upgrade() {
                        window.refresh_calendar_page();
                    }
                    glib::Propagation::Proceed
                });
            }
        }
        dialog.present();
    });

    let event_for_export = event.clone();
    let export_button_for_context = export_button.clone();
    let export_button_for_signal = export_button.clone();
    export_button_for_signal.connect_clicked(move |_| {
        if event_for_export.component.trim().is_empty() {
            show_toast_for_widget(
                &export_button,
                "Für diesen Termin sind keine iCalendar-Daten verfügbar",
            );
            return;
        }
        let Some(root) = export_button.root() else {
            return;
        };
        let Ok(window) = root.downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("Termin als iCalendar speichern")
            .initial_name("termin.ics")
            .modal(true)
            .build();
        let event_for_dialog = event_for_export.clone();
        let export_button_for_result = export_button.clone();
        file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let save_result = file.replace_contents(
                export_ical_component(&event_for_dialog.component).as_bytes(),
                None,
                true,
                gio::FileCreateFlags::NONE,
                None::<&gio::Cancellable>,
            );
            match save_result {
                Ok(_) => show_toast_for_widget(
                    &export_button_for_result,
                    "Termin als iCalendar exportiert",
                ),
                Err(error) => show_toast_for_widget(
                    &export_button_for_result,
                    &format!("iCalendar konnte nicht gespeichert werden: {error}"),
                ),
            }
        });
    });

    let event_for_delete = event.clone();
    let event_records_for_delete = Rc::clone(&event_records);
    let event_list_for_delete = event_list.clone();
    let delete_button_for_context = delete_button.clone();
    let delete_button_for_signal = delete_button.clone();
    delete_button_for_signal.connect_clicked(move |_| {
        if let Some(error) = event_mutation_error(&event_for_delete) {
            show_toast_for_widget(&delete_button, error);
            return;
        }
        let confirmation_message = if event_for_delete.source_uid == "local-calendar" {
            "Der Termin wird dauerhaft aus dem lokalen Kalender entfernt."
        } else {
            "Der Termin wird dauerhaft aus dem synchronisierten Kalender entfernt."
        };
        let event_for_action = event_for_delete.clone();
        let event_records_for_action = Rc::clone(&event_records_for_delete);
        let event_list_for_action = event_list_for_delete.clone();
        let delete_button_for_action = delete_button.clone();
        crate::dialogs::run_destructive_action(
            &delete_button,
            "Termin löschen?",
            confirmation_message,
            "Löschen",
            move || {
                let source_uid = event_for_action.source_uid.clone();
                let source_uid_for_result = source_uid.clone();
                let uid = event_for_action.uid.clone();
                let uid_for_result = uid.clone();
                let (sender, receiver) = async_channel::bounded(1);
                delete_button_for_action.set_sensitive(false);
                std::thread::spawn(move || {
                    let result = ECalService::new().remove_component(
                        &source_uid,
                        ECalComponentType::Event,
                        &uid,
                    );
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Event deletion result was dropped");
                    }
                });

                let event_records_for_result = Rc::clone(&event_records_for_action);
                let event_list_for_result = event_list_for_action.clone();
                let delete_button_for_result = delete_button_for_action.clone();
                glib::spawn_future_local(async move {
                    match receiver.recv().await {
                        Ok(Ok(result)) if result.uid == uid_for_result => {
                            if let Some(row) = delete_button_for_result
                                .ancestor(gtk::ListBoxRow::static_type())
                                .and_downcast::<gtk::ListBoxRow>()
                            {
                                event_list_for_result.remove(&row);
                            }
            event_records_for_result.borrow_mut().retain(|event| {
                                !same_event_identity(
                                    event,
                                    &source_uid_for_result,
                                    &uid_for_result,
                                )
                            });
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Termin gelöscht ({})", result.uid),
                            );
                            if let Some(window) = delete_button_for_result
                                .root()
                                .and_downcast::<crate::window::EvolutionWindow>()
                            {
                                window.refresh_calendar_page();
                            }
                        }
                        Ok(Ok(result)) => {
                            delete_button_for_result.set_sensitive(true);
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!(
                                    "ECal bestätigte eine andere Termin-UID ({}); nichts wurde entfernt",
                                    result.uid
                                ),
                            );
                        }
                        Ok(Err(error)) => {
                            delete_button_for_result.set_sensitive(true);
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Termin konnte nicht gelöscht werden: {error}"),
                            );
                        }
                        Err(error) => {
                            delete_button_for_result.set_sensitive(true);
                            tracing::debug!(%error, "Event deletion channel closed");
                        }
                    }
                });
            },
        );
    });

    // Keep the familiar Evolution event context menu available in addition
    // to the compact row buttons.  The menu activates those same buttons so
    // read-only handling, export, confirmation, and refresh semantics stay
    // identical for both interaction paths.
    let context_click = gtk::GestureClick::builder().button(3).build();
    let row_weak_for_context = row.downgrade();
    context_click.connect_pressed(move |_, _, x, y| {
        let Some(row_for_context) = row_weak_for_context.upgrade() else {
            return;
        };
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

        let edit = gtk::Button::with_label("Termin bearbeiten");
        edit.set_halign(gtk::Align::Fill);
        edit.add_css_class("flat");
        let popover_for_edit = popover.clone();
        let edit_button_for_menu = edit_button_for_context.clone();
        edit.connect_clicked(move |_| {
            popover_for_edit.popdown();
            edit_button_for_menu.activate();
        });
        menu_box.append(&edit);

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

        let delete = gtk::Button::with_label("Termin löschen");
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

    let double_click = gtk::GestureClick::builder().button(1).build();
    let edit_button_for_double_click = edit_button.clone();
    double_click.connect_pressed(move |gesture, n_press, _, _| {
        if n_press == 2 {
            edit_button_for_double_click.activate();
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    row.add_controller(double_click);

    event_list.append(&row);
}

fn remove_event_list_row(event_list: &gtk::ListBox, source_uid: &str, uid: &str) {
    let mut child = event_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let Some(event_row) = row
            .child()
            .and_then(|child| child.downcast::<adw::ActionRow>().ok())
        else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = event_row
                .data::<String>("event-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = event_row
                .data::<String>("event-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_uid == uid
        };
        if matches {
            event_list.remove(&row);
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
    tracing::warn!(%message, "Calendar UI result");
}

impl Default for CalendarPage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with_start(start: &str) -> CalendarEventRecord {
        CalendarEventRecord {
            uid: "event-1".to_string(),
            source_uid: "source-1".to_string(),
            source_display_name: "Kalender".to_string(),
            component: String::new(),
            summary: "Termin".to_string(),
            start: start.to_string(),
            end: String::new(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        }
    }

    #[test]
    fn month_event_date_uses_the_calendar_date_for_all_day_and_timed_events() {
        assert_eq!(
            event_start_date(&event_with_start("2026-08-10")),
            NaiveDate::from_ymd_opt(2026, 8, 10)
        );
        assert_eq!(
            event_start_date(&event_with_start("2026-08-10 12:00")),
            NaiveDate::from_ymd_opt(2026, 8, 10)
        );
    }

    #[test]
    fn month_event_date_rejects_unparseable_backend_values() {
        assert_eq!(event_start_date(&event_with_start("not-a-date")), None);
    }

    #[test]
    fn calendar_search_text_includes_event_metadata() {
        let mut event = event_with_start("2026-08-10 12:00");
        event.location = "Berlin Büro".to_string();
        event.description = "Quartalsplanung".to_string();
        let search = calendar_event_search_text(&event);
        assert!(search.contains("berlin büro"));
        assert!(search.contains("quartalsplanung"));
        assert!(search.contains("2026-08-10"));
    }

    #[test]
    fn task_completion_toggle_preserves_nested_alarm_data() {
        let task = concat!(
            "BEGIN:VTODO\r\nUID:task-1\r\nSUMMARY:Bericht\r\n",
            "STATUS:NEEDS-ACTION\r\nPERCENT-COMPLETE:0\r\n",
            "BEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\n",
            "DESCRIPTION:Erinnerung\r\nEND:VALARM\r\nEND:VTODO\r\n"
        );
        let completed = set_task_completion_component(task, true);
        assert!(completed.contains("STATUS:COMPLETED"));
        assert!(completed.contains("PERCENT-COMPLETE:100"));
        assert!(completed.contains("TRIGGER:-PT15M"));
        assert!(completed.contains("DESCRIPTION:Erinnerung"));

        let reopened = set_task_completion_component(&completed, false);
        assert!(reopened.contains("STATUS:NEEDS-ACTION"));
        assert!(reopened.contains("PERCENT-COMPLETE:0"));
        assert!(!reopened.contains("COMPLETED:"));
        assert!(reopened.contains("BEGIN:VALARM"));
    }

    #[test]
    fn configured_week_start_controls_workweek_days() {
        let sunday = NaiveDate::from_ymd_opt(2026, 8, 9).unwrap();
        let monday_start = week_start_for(sunday, Weekday::Mon);
        assert_eq!(monday_start.weekday(), Weekday::Mon);
        assert_eq!(monday_start, NaiveDate::from_ymd_opt(2026, 8, 3).unwrap());
        assert_eq!(
            monday_start + Duration::days(4),
            NaiveDate::from_ymd_opt(2026, 8, 7).unwrap()
        );

        let sunday_start = week_start_for(sunday, Weekday::Sun);
        assert_eq!(sunday_start, sunday);
        let workdays = workweek_display_dates(
            CalendarDateRange::from_days(sunday_start, 7),
            &[
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
        );
        assert_eq!(
            workdays,
            (1..=5)
                .map(|offset| sunday + Duration::days(offset))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn workday_range_normalization_has_explicit_boundary_behavior() {
        assert_eq!(normalized_workday_range(600, 900), (600, 900));
        assert_eq!(normalized_workday_range(600, 600), (600, 630));
        assert_eq!(normalized_workday_range(1_500, 0), (1_410, 1_440));
    }

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").expect("test date should parse")
    }

    fn all_day_event(source_uid: &str, uid: &str, start: &str, end: &str) -> CalendarEventRecord {
        CalendarEventRecord {
            uid: uid.to_string(),
            source_uid: source_uid.to_string(),
            source_display_name: source_uid.to_string(),
            component: format!(
                "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART;VALUE=DATE:{}\r\nDTEND;VALUE=DATE:{}\r\nEND:VEVENT\r\n",
                start.replace('-', ""),
                end.replace('-', "")
            ),
            summary: uid.to_string(),
            start: start.to_string(),
            end: end.to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        }
    }

    #[test]
    fn viewport_truth_table_is_half_open_for_monday_and_sunday_weeks() {
        let anchor = date("2026-08-12");
        for (first_weekday, expected_week_start) in [
            (Weekday::Mon, date("2026-08-10")),
            (Weekday::Sun, date("2026-08-09")),
        ] {
            let day = CalendarViewport::for_view(CalendarView::Day, anchor, first_weekday);
            assert_eq!(day.visible_range.len_days(), 1);
            assert_eq!(day.visible_range.start, anchor);

            let week = CalendarViewport::for_view(CalendarView::Week, anchor, first_weekday);
            assert_eq!(week.visible_range.start, expected_week_start);
            assert_eq!(week.visible_range.len_days(), 7);
            assert!(!week.visible_range.contains(week.visible_range.end));

            let workweek =
                CalendarViewport::for_view(CalendarView::WorkWeek, anchor, first_weekday);
            assert_eq!(workweek.visible_range, week.visible_range);
            assert_eq!(
                workweek_display_dates(
                    workweek.visible_range,
                    &[
                        Weekday::Mon,
                        Weekday::Tue,
                        Weekday::Wed,
                        Weekday::Thu,
                        Weekday::Fri,
                    ],
                )
                .len(),
                5
            );

            let month = CalendarViewport::for_view(CalendarView::Month, anchor, first_weekday);
            assert_eq!(month.visible_range.len_days(), 42);
            assert!(month.visible_range.contains(date("2026-08-01")));
            assert!(month.visible_range.contains(date("2026-08-31")));
        }

        assert_eq!(
            CalendarViewport::for_view(CalendarView::Month, date("2026-02-15"), Weekday::Mon)
                .visible_range
                .start,
            date("2026-01-26")
        );
        assert_eq!(
            CalendarViewport::for_view(CalendarView::Month, date("2026-02-15"), Weekday::Sun)
                .visible_range
                .start,
            date("2026-02-01")
        );
    }

    #[test]
    fn month_lengths_and_agenda_length_do_not_change_the_contract() {
        for (year, month_number, last_day) in
            [(2024, 2, 29), (2025, 2, 28), (2026, 4, 30), (2026, 1, 31)]
        {
            let month_view = CalendarViewport::for_view(
                CalendarView::Month,
                NaiveDate::from_ymd_opt(year, month_number, 15).unwrap(),
                Weekday::Mon,
            );
            assert_eq!(month_view.visible_range.len_days(), MONTH_GRID_CELLS);
            let dates = month_view.visible_range.dates().collect::<Vec<_>>();
            assert_eq!(dates.len(), MONTH_GRID_CELLS as usize);
            assert_eq!(dates.first(), Some(&month_view.visible_range.start));
            assert_eq!(
                dates.last(),
                Some(&month_view.visible_range.last_inclusive())
            );
            assert!(
                !month_view
                    .visible_range
                    .contains(month_view.visible_range.end)
            );
            assert!(
                month_view
                    .visible_range
                    .contains(NaiveDate::from_ymd_opt(year, month_number, last_day).unwrap())
            );
        }

        let agenda =
            CalendarViewport::for_view(CalendarView::Agenda, date("2026-01-01"), Weekday::Mon);
        assert_eq!(agenda.visible_range.len_days(), AGENDA_VISIBLE_DAYS);
        assert_eq!(agenda.visible_range.last_inclusive(), date("2026-03-31"));
        assert!(!agenda.visible_range.contains(date("2026-04-01")));
        assert_eq!(
            agenda.visible_range.dates().count(),
            AGENDA_VISIBLE_DAYS as usize
        );
    }

    #[test]
    fn navigation_and_today_are_deterministic_across_year_boundaries() {
        let mut day = CalendarViewState::new(CalendarView::Day, date("2026-12-31"), Weekday::Mon);
        day.navigate(1, Weekday::Mon);
        assert_eq!(day.anchor_date, date("2027-01-01"));
        day.navigate(-1, Weekday::Mon);
        assert_eq!(day.anchor_date, date("2026-12-31"));

        let mut week = CalendarViewState::new(CalendarView::Week, date("2026-12-31"), Weekday::Mon);
        week.navigate(1, Weekday::Mon);
        assert_eq!(week.visible_range.start, date("2027-01-04"));
        assert_eq!(week.visible_range.len_days(), 7);

        let mut workweek =
            CalendarViewState::new(CalendarView::WorkWeek, date("2026-12-31"), Weekday::Sun);
        workweek.navigate(1, Weekday::Sun);
        assert_eq!(workweek.visible_range.start, date("2027-01-03"));

        let mut month =
            CalendarViewState::new(CalendarView::Month, date("2026-12-31"), Weekday::Mon);
        month.navigate(1, Weekday::Mon);
        assert_eq!(month.anchor_date, date("2027-01-01"));
        month.navigate(-1, Weekday::Mon);
        assert_eq!(month.anchor_date, date("2026-12-01"));

        let mut leap_month =
            CalendarViewState::new(CalendarView::Month, date("2024-02-29"), Weekday::Mon);
        leap_month.navigate(1, Weekday::Mon);
        assert_eq!(leap_month.anchor_date, date("2024-03-01"));
        leap_month.navigate(-1, Weekday::Mon);
        assert_eq!(leap_month.anchor_date, date("2024-02-01"));
        assert!(leap_month.visible_range.contains(date("2024-02-29")));

        let mut agenda =
            CalendarViewState::new(CalendarView::Agenda, date("2026-01-01"), Weekday::Mon);
        agenda.navigate(1, Weekday::Mon);
        assert_eq!(agenda.anchor_date, date("2026-04-01"));
        agenda.set_selected_range(Some(CalendarDateRange::single(date("2026-04-02"))));
        agenda.today(date("2026-10-25"), Weekday::Mon);
        assert_eq!(agenda.anchor_date, date("2026-10-25"));
        assert_eq!(agenda.selected_range, None);

        for view in [
            CalendarView::Day,
            CalendarView::Week,
            CalendarView::WorkWeek,
            CalendarView::Month,
            CalendarView::Agenda,
        ] {
            let mut state = CalendarViewState::new(view, date("2026-01-31"), Weekday::Sun);
            state.set_selected_range(Some(CalendarDateRange::single(date("2026-02-01"))));
            state.today(date("2027-01-01"), Weekday::Sun);
            assert_eq!(state.anchor_date, date("2027-01-01"));
            assert_eq!(
                state.visible_range,
                CalendarViewport::for_view(view, date("2027-01-01"), Weekday::Sun).visible_range
            );
            assert_eq!(state.selected_range, None);
        }
    }

    #[test]
    fn dtend_exclusive_midnight_and_multiday_ranges_are_preserved() {
        let all_day = all_day_event("source", "all-day", "2026-08-10", "2026-08-13");
        assert_eq!(
            event_display_range(&all_day),
            Some(CalendarDateRange::new(
                date("2026-08-10"),
                date("2026-08-13")
            ))
        );
        let all_day_dates = event_occurrence_dates_in_range(
            &all_day,
            CalendarDateRange::new(date("2026-08-10"), date("2026-08-14")),
        );
        assert_eq!(
            all_day_dates,
            vec![date("2026-08-10"), date("2026-08-11"), date("2026-08-12")]
        );

        let mut midnight = event_with_start("2026-08-10 23:00");
        midnight.end = "2026-08-11 00:00".to_string();
        assert_eq!(
            event_display_range(&midnight),
            Some(CalendarDateRange::new(
                date("2026-08-10"),
                date("2026-08-11")
            ))
        );
        assert_eq!(
            event_occurrence_dates_in_range(
                &midnight,
                CalendarDateRange::new(date("2026-08-10"), date("2026-08-12")),
            ),
            vec![date("2026-08-10")]
        );

        let mut multiday = event_with_start("2026-08-10 23:00");
        multiday.end = "2026-08-11 01:00".to_string();
        assert_eq!(
            event_display_range(&multiday),
            Some(CalendarDateRange::new(
                date("2026-08-10"),
                date("2026-08-12")
            ))
        );
        assert_eq!(
            event_occurrence_dates_in_range(
                &multiday,
                CalendarDateRange::new(date("2026-08-10"), date("2026-08-12")),
            ),
            vec![date("2026-08-10"), date("2026-08-11")]
        );
    }

    #[test]
    fn rrule_rdate_exdate_and_event_span_are_indexed_as_occurrences() {
        let mut event = all_day_event("source", "series", "2026-08-03", "2026-08-05");
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:series\r\n",
            "DTSTART;VALUE=DATE:20260803\r\n",
            "DTEND;VALUE=DATE:20260805\r\n",
            "RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=3\r\n",
            "RDATE;VALUE=DATE:20260819\r\n",
            "EXDATE;VALUE=DATE:20260810\r\nEND:VEVENT\r\n"
        )
        .to_string();
        let dates = event_occurrence_dates_in_range(
            &event,
            CalendarDateRange::new(date("2026-08-03"), date("2026-08-25")),
        );
        assert_eq!(
            dates,
            vec![
                date("2026-08-03"),
                date("2026-08-04"),
                date("2026-08-17"),
                date("2026-08-18"),
                date("2026-08-19"),
                date("2026-08-20"),
            ]
        );
    }

    #[test]
    fn stable_source_uid_and_uid_are_kept_in_candidate_order() {
        let first = all_day_event("source-b", "same-uid", "2026-08-10", "2026-08-11");
        let second = all_day_event("source-a", "same-uid", "2026-08-10", "2026-08-11");
        let third = all_day_event("source-a", "other-uid", "2026-08-10", "2026-08-11");
        assert!(same_event_identity(&first, "source-b", "same-uid"));
        assert!(!same_event_identity(&first, "source-a", "same-uid"));

        let snapshot = Arc::new(CalendarSnapshot {
            events: vec![first, second, third],
            tasks: Vec::new(),
            memos: Vec::new(),
        });
        let index = CalendarSnapshotIndex::new(snapshot);
        let candidates =
            index.candidate_event_indices(CalendarDateRange::single(date("2026-08-10")));
        let snapshot = Arc::clone(&index.snapshot);
        let identities = candidates
            .iter()
            .map(|event_index| {
                (
                    snapshot.events[*event_index].source_uid.as_str(),
                    snapshot.events[*event_index].uid.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            identities,
            vec![
                ("source-a", "other-uid"),
                ("source-a", "same-uid"),
                ("source-b", "same-uid"),
            ]
        );
    }

    #[test]
    fn stale_slow_load_cannot_finish_fast_newer_load() {
        let mut gate = CalendarLoadGate::default();
        let slow_a = gate.begin();
        let fast_b = gate.begin();
        assert!(fast_b > slow_a);
        assert!(!gate.finish(slow_a));
        assert!(gate.loading);
        assert!(gate.finish(fast_b));
        assert!(!gate.loading);
        assert!(!gate.is_current(fast_b));
    }

    #[test]
    fn cancellation_clears_loading_and_rejects_late_result() {
        let mut gate = CalendarLoadGate::default();
        let generation = gate.begin();
        assert!(gate.cancel(generation));
        assert!(!gate.loading);
        assert!(!gate.finish(generation));
    }

    #[test]
    fn candidate_index_avoids_n_times_90_agenda_scans() {
        let mut events = Vec::new();
        for index in 0..256_i64 {
            let start = date("2030-01-01") + Duration::days(index);
            let end = start.succ_opt().unwrap();
            events.push(all_day_event(
                "far-away",
                &format!("event-{index}"),
                &start.to_string(),
                &end.to_string(),
            ));
        }
        events.push(all_day_event(
            "nearby",
            "agenda-event",
            "2026-02-10",
            "2026-02-11",
        ));
        let index = CalendarSnapshotIndex::new(Arc::new(CalendarSnapshot {
            events,
            tasks: Vec::new(),
            memos: Vec::new(),
        }));
        let agenda = CalendarDateRange::from_days(date("2026-01-01"), AGENDA_VISIBLE_DAYS);
        let (candidates, stats) = index.candidate_event_indices_with_stats(agenda);
        assert_eq!(stats.date_bucket_lookups, AGENDA_VISIBLE_DAYS as usize);
        assert_eq!(stats.bucketed_event_refs, 1);
        assert_eq!(stats.recurring_events_considered, 0);
        assert_eq!(stats.long_intervals_considered, 0);
        assert_eq!(candidates.len(), 1);
        assert_eq!(index.snapshot.events[candidates[0]].uid, "agenda-event");
    }

    #[test]
    fn berlin_dst_civil_dates_never_depend_on_24_hour_timestamps() {
        for start in [date("2026-03-29"), date("2026-10-25")] {
            let range = CalendarDateRange::from_days(start, 7);
            let dates = range.dates().collect::<Vec<_>>();
            assert_eq!(dates.len(), 7);
            assert_eq!(dates[0], start);
            assert_eq!(dates[6], start + Duration::days(6));
            assert_eq!((range.end - range.start).num_days(), 7);
        }

        for raw in ["20260329T013000", "20261025T013000"] {
            let shifted = shift_ical_datetime_value(raw, 1).expect("civil date should shift");
            let expected = (NaiveDate::parse_from_str(&raw[..8], "%Y%m%d").unwrap()
                + Duration::days(1))
            .format("%Y%m%d")
            .to_string();
            assert!(shifted.starts_with(&expected));
        }
    }

    #[test]
    fn berlin_dst_drag_keeps_tzid_wall_clock_and_moves_the_civil_date() {
        for (start, end, shifted_start, shifted_end) in [
            (
                "20260329T013000",
                "20260329T023000",
                "20260330T013000",
                "20260330T023000",
            ),
            (
                "20261025T013000",
                "20261025T023000",
                "20261026T013000",
                "20261026T023000",
            ),
        ] {
            let raw = format!(
                "BEGIN:VEVENT\r\nUID:dst-{start}\r\nDTSTART;TZID=Europe/Berlin:{start}\r\nDTEND;TZID=Europe/Berlin:{end}\r\nEND:VEVENT\r\n"
            );
            let shifted = shift_event_component(&raw, 1).expect("DST event should move");
            assert!(shifted.contains(&format!("DTSTART;TZID=Europe/Berlin:{shifted_start}")));
            assert!(shifted.contains(&format!("DTEND;TZID=Europe/Berlin:{shifted_end}")));
        }
    }

    #[test]
    fn event_occurrence_includes_timed_and_all_day_spans() {
        let mut timed = event_with_start("2026-08-10 23:00");
        timed.end = "2026-08-11 01:00".to_string();
        assert!(event_occurs_on(
            &timed,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        ));
        assert!(event_occurs_on(
            &timed,
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()
        ));

        let mut all_day = event_with_start("2026-08-10");
        all_day.all_day = true;
        all_day.end = "2026-08-13".to_string();
        assert!(event_occurs_on(
            &all_day,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        ));
        assert!(event_occurs_on(
            &all_day,
            NaiveDate::from_ymd_opt(2026, 8, 12).unwrap()
        ));
        assert!(!event_occurs_on(
            &all_day,
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        ));
    }

    #[test]
    fn timed_event_ending_at_midnight_does_not_occupy_the_next_day() {
        let mut event = event_with_start("2026-08-10 23:00");
        event.end = "2026-08-11 00:00".to_string();

        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()
        ));
    }

    #[test]
    fn rdate_occurrences_keep_the_event_span_and_exdate_removes_the_span() {
        let mut event = event_with_start("2026-08-03");
        event.all_day = true;
        event.end = "2026-08-05".to_string();
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-rdate-span\r\n",
            "DTSTART;VALUE=DATE:20260803\r\n",
            "DTEND;VALUE=DATE:20260805\r\n",
            "RDATE;VALUE=DATE:20260812\r\n",
            "END:VEVENT\r\n"
        )
        .to_string();

        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap()
        ));

        event.component = event
            .component
            .replace("RDATE;VALUE=DATE:20260812", "EXDATE;VALUE=DATE:20260812");
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 12).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        ));
    }

    #[test]
    fn malformed_recurrence_does_not_create_occurrences_beyond_dtstart() {
        let mut event = event_with_start("2026-08-03");
        event.all_day = true;
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-invalid-rule\r\n",
            "DTSTART;VALUE=DATE:20260803\r\n",
            "RRULE:FREQ=DAILY;COUNT=invalid\r\n",
            "END:VEVENT\r\n"
        )
        .to_string();

        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()
        ));
    }

    #[test]
    fn recurrence_rules_cover_weekdays_count_until_and_exdate() {
        let mut event = event_with_start("2026-08-03");
        event.all_day = true;
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\nDTSTART;VALUE=DATE:20260803\r\n",
            "RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4\r\n",
            "EXDATE;VALUE=DATE:20260810\r\nEND:VEVENT\r\n"
        )
        .to_string();
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()
        ));
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 19).unwrap()
        ));
    }

    #[test]
    fn recurrence_supports_ordinal_month_weekdays() {
        let mut event = event_with_start("2026-08-03");
        event.all_day = true;
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-ordinal\r\nDTSTART;VALUE=DATE:20260803\r\n",
            "RRULE:FREQ=MONTHLY;BYDAY=1MO;COUNT=3\r\nEND:VEVENT\r\n"
        )
        .to_string();

        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()
        ));
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 9, 7).unwrap()
        ));
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
        ));
        assert_eq!(
            parse_extended_weekday_token("-1FR"),
            Some(RecurrenceByDay {
                weekday: Weekday::Fri,
                ordinal: Some(-1)
            })
        );
    }

    #[test]
    fn recurrence_supports_set_positions_and_negative_month_days() {
        let rule =
            parse_recurrence_rule("FREQ=MONTHLY;BYDAY=FR;BYSETPOS=-1").expect("rule should parse");
        let start = NaiveDate::from_ymd_opt(2026, 8, 28).unwrap();
        assert!(recurrence_matches_without_count(
            &rule,
            start,
            NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()
        ));
        assert!(!recurrence_matches_without_count(
            &rule,
            start,
            NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()
        ));

        let by_month_day = parse_recurrence_rule("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1")
            .expect("yearly rule should parse");
        assert!(recurrence_matches_without_count(
            &by_month_day,
            NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            NaiveDate::from_ymd_opt(2027, 2, 28).unwrap()
        ));
    }

    #[test]
    fn recurrence_until_stops_later_occurrences() {
        let mut event = event_with_start("2026-08-01");
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-2\r\nDTSTART:20260801T090000Z\r\n",
            "RRULE:FREQ=DAILY;UNTIL=20260803T235959Z\r\nEND:VEVENT\r\n"
        )
        .to_string();
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()
        ));
    }

    #[test]
    fn recurrence_exdate_can_remove_the_initial_occurrence() {
        let mut event = event_with_start("2026-08-03");
        event.all_day = true;
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-3\r\nDTSTART;VALUE=DATE:20260803\r\n",
            "RRULE:FREQ=WEEKLY;COUNT=3\r\n",
            "EXDATE;VALUE=DATE:20260803\r\nEND:VEVENT\r\n"
        )
        .to_string();
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()
        ));
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        ));
    }

    #[test]
    fn recurrence_rdate_adds_explicit_occurrences_without_an_rrule() {
        let mut event = event_with_start("2026-08-03");
        event.all_day = true;
        event.component = concat!(
            "BEGIN:VEVENT\r\nUID:event-rdate\r\nDTSTART;VALUE=DATE:20260803\r\n",
            "RDATE;VALUE=DATE:20260812,20260819\r\nEND:VEVENT\r\n"
        )
        .to_string();

        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 12).unwrap()
        ));
        assert!(event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 19).unwrap()
        ));
        assert!(!event_occurs_on(
            &event,
            NaiveDate::from_ymd_opt(2026, 8, 20).unwrap()
        ));
    }

    #[test]
    fn event_time_text_distinguishes_all_day_and_timed_events() {
        let mut event = event_with_start("2026-08-10 09:30");
        event.end = "2026-08-10 10:45".to_string();
        assert_eq!(event_time_text(&event), "09:30–10:45");
        event.all_day = true;
        assert_eq!(event_time_text(&event), "Ganztägig");
    }

    #[test]
    fn moving_simple_event_shifts_dates_and_preserves_timezone_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:event-1\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T130000\r\n",
            "END:VEVENT\r\n"
        );
        let shifted = shift_event_component(raw, 2).expect("simple event should move");
        assert!(shifted.contains("DTSTART;TZID=Europe/Berlin:20260812T120000"));
        assert!(shifted.contains("DTEND;TZID=Europe/Berlin:20260812T130000"));
    }

    #[test]
    fn shifting_malformed_non_ascii_datetime_is_rejected_without_panicking() {
        assert_eq!(shift_ical_datetime_value("20260810T12000ä0", 1), None);
    }

    #[test]
    fn moving_all_day_event_shifts_date_values() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "DTSTART;VALUE=DATE:20260810\r\n",
            "DTEND;VALUE=DATE:20260811\r\nEND:VEVENT\r\n"
        );
        let shifted = shift_event_component(raw, -1).expect("all-day event should move");
        assert!(shifted.contains("DTSTART;VALUE=DATE:20260809"));
        assert!(shifted.contains("DTEND;VALUE=DATE:20260810"));
    }

    #[test]
    fn moving_recurring_event_shifts_series_dates_and_until() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "DTSTART:20260810T120000Z\r\n",
            "DTEND:20260810T130000Z\r\n",
            "RRULE:FREQ=WEEKLY;UNTIL=20260901T120000Z\r\n",
            "RDATE:20260817T120000Z,20260824T120000Z\r\n",
            "EXDATE:20260831T120000Z\r\n",
            "END:VEVENT\r\n"
        );
        let shifted = shift_event_component(raw, 2).expect("series move should work");
        assert!(shifted.contains("DTSTART:20260812T120000Z"));
        assert!(shifted.contains("DTEND:20260812T130000Z"));
        assert!(shifted.contains("RRULE:FREQ=WEEKLY;UNTIL=20260903T120000Z"));
        assert!(shifted.contains("RDATE:20260819T120000Z,20260826T120000Z"));
        assert!(shifted.contains("EXDATE:20260902T120000Z"));
    }

    #[test]
    fn moving_weekly_byday_event_shifts_the_weekdays_with_the_series() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:event-weekly-byday\r\n",
            "DTSTART;VALUE=DATE:20260803\r\n",
            "DTEND;VALUE=DATE:20260804\r\n",
            "RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4\r\n",
            "END:VEVENT\r\n"
        );
        let shifted = shift_event_component(raw, 2).expect("weekly series should move");
        assert!(shifted.contains("DTSTART;VALUE=DATE:20260805"));
        assert!(shifted.contains("DTEND;VALUE=DATE:20260806"));
        assert!(shifted.contains("RRULE:FREQ=WEEKLY;BYDAY=WE,FR;COUNT=4"));
    }

    #[test]
    fn moving_complex_monthly_recurrence_reports_lossless_translation_limit() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-monthly\r\n",
            "DTSTART;VALUE=DATE:20260803\r\n",
            "DTEND;VALUE=DATE:20260804\r\n",
            "RRULE:FREQ=MONTHLY;BYDAY=1MO;COUNT=3\r\n",
            "END:VEVENT\r\n"
        );
        let error =
            shift_event_component(raw, 1).expect_err("complex series must not be corrupted");
        assert!(error.contains("nicht sicher"));
    }

    #[test]
    fn shifting_date_lists_preserves_each_timezone_parameter() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-zones\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T130000\r\n",
            "RDATE;TZID=Europe/Berlin:20260817T120000\r\n",
            "RDATE;TZID=America/New_York:20260824T120000\r\n",
            "END:VEVENT\r\n"
        );
        let shifted = shift_event_component(raw, 1).expect("mixed-zone list should move");
        assert!(shifted.contains("RDATE;TZID=Europe/Berlin:20260818T120000"));
        assert!(shifted.contains("RDATE;TZID=America/New_York:20260825T120000"));
    }

    #[test]
    fn mutation_validation_requires_the_original_source_and_uid() {
        let event = CalendarEventRecord {
            uid: "event-1".to_string(),
            source_uid: "source-1".to_string(),
            source_display_name: "Kalender".to_string(),
            component: "BEGIN:VEVENT\r\nUID:event-1\r\nEND:VEVENT\r\n".to_string(),
            summary: "Termin".to_string(),
            start: "2026-08-10".to_string(),
            end: "2026-08-11".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };
        let valid = ECalMutationResult {
            uid: "event-1".to_string(),
            component: Some(event.component.clone()),
        };
        assert_eq!(event_mutation_error(&event), None);
        assert_eq!(
            validated_event_mutation_component(&event, &valid),
            Ok(event.component.clone())
        );

        let wrong_uid = ECalMutationResult {
            uid: "event-2".to_string(),
            component: Some(event.component.clone()),
        };
        assert!(validated_event_mutation_component(&event, &wrong_uid).is_err());

        let wrong_component = ECalMutationResult {
            uid: "event-1".to_string(),
            component: Some("BEGIN:VEVENT\r\nUID:event-2\r\nEND:VEVENT\r\n".to_string()),
        };
        assert!(validated_event_mutation_component(&event, &wrong_component).is_err());

        let mut imported = event.clone();
        imported.source_uid.clear();
        assert!(event_mutation_error(&imported).is_some());
    }
}
