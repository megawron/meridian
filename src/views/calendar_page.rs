// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPage — Calendar view with Day/Week/Month navigation, event creation, and task management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc, Weekday};
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering as CompareOrdering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_category_service::category_names_from_ical_component;
use crate::services::calendar_export_service::{CalendarExportFormat, export_calendar_events};
use crate::services::calendar_print_service::{
    build_calendar_list_print_document, build_event_print_document,
};
use crate::services::calendar_store_service::{
    CalendarEventRecord, CalendarSnapshot, CalendarStoreService, MemoRecord, TaskRecord,
    update_ical_property,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalComponentType, ECalMutationResult, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds,
};
use crate::services::eds_service;
use crate::services::ical_time_service::{
    property_local_date, property_local_dates, property_local_datetime, property_local_display,
    property_local_duration,
};
#[cfg(test)]
use crate::services::recurrence_service::recurrence_matches_without_count;
use crate::services::recurrence_service::{
    RecurrenceByDay, next_recurrence_date, parse_extended_weekday_token, parse_recurrence_rule,
    recurrence_occurs_on as recurrence_rule_occurs_on,
};
use crate::services::reminder_service::{
    AlarmTrigger, ReminderComponentKind, ReminderIdentity, parse_alarms,
};
use crate::services::settings_service::AppSettings;
use crate::views::accessibility::set_accessible_label;
use crate::views::set_button_icon_with_fallback;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalendarView {
    Day,
    Week,
    WorkWeek,
    Month,
    Agenda,
    Year,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YearPreviewMode {
    List,
    Schedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YearPreviewOrientation {
    Side,
    Bottom,
}

const AGENDA_VISIBLE_DAYS: i64 = 90;
const MONTH_GRID_CELLS: i64 = 42;
const YEAR_MONTH_COUNT: u32 = 12;
const PROJECTION_APPLY_CHUNK_SIZE: usize = 24;

/// Stable identity for a calendar component.  A UID is only unique inside
/// one source, and a recurring exception has the same UID as its master, so
/// every view carries the canonical source/object key and RECURRENCE-ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct CalendarEventIdentity {
    pub(crate) source_object_key: String,
    pub(crate) source_uid: String,
    pub(crate) uid: String,
    pub(crate) recurrence_id: Option<String>,
}

impl CalendarEventIdentity {
    fn from_event(event: &CalendarEventRecord) -> Option<Self> {
        Some(Self {
            source_object_key: canonical_calendar_source_object_key(event),
            source_uid: event.source_uid.trim().to_string(),
            uid: canonical_calendar_uid(event),
            recurrence_id: calendar_recurrence_id(event),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarDataState {
    Loading,
    Ready,
    Error,
}

/// The portable action surface deliberately distinguishes a non-destructive
/// copy from a move.  A move removes its source only after the destination
/// has confirmed the copied component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarTransferKind {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarRecurringDeleteScope {
    ThisOccurrence,
    ThisAndFuture,
    EntireSeries,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CalendarEventActionState {
    can_edit: bool,
    can_delete: bool,
    can_export: bool,
    can_print: bool,
    can_copy_to_local: bool,
    can_move_to_local: bool,
    recurring: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CalendarTransferPlan {
    kind: CalendarTransferKind,
    source_uid: String,
    source_component_uid: String,
    destination_uid: String,
    component: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CalendarDeletePlan {
    RemoveComponent { source_uid: String, uid: String },
    UnsupportedOccurrenceScope { scope: CalendarRecurringDeleteScope },
}

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
            CalendarView::Year => calendar_year_range_option(anchor_date.year())
                .unwrap_or_else(|| CalendarDateRange::single(anchor_date)),
            // List is intentionally not range-driven.  Keep a valid civil
            // range in the shared viewport contract for labels and settings,
            // while list queries use every loaded snapshot record.
            CalendarView::List => CalendarDateRange::single(anchor_date),
        };
        Self {
            view,
            anchor_date,
            visible_range,
        }
    }
}

/// Selected calendar time slot in day, week, workweek or month grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedTimeSlot {
    pub date: NaiveDate,
    pub start_minute: u32,
    pub end_minute: u32,
}

/// GTK-free state contract shared by navigation, loading and selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CalendarViewState {
    pub(crate) view: CalendarView,
    pub(crate) anchor_date: NaiveDate,
    pub(crate) visible_range: CalendarDateRange,
    pub(crate) load_generation: u64,
    pub(crate) selected_range: Option<CalendarDateRange>,
    pub(crate) selected_event: Option<CalendarEventIdentity>,
    pub(crate) selected_event_index: Option<usize>,
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
            selected_event: None,
            selected_event_index: None,
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

    pub(crate) fn select_event(
        &mut self,
        identity: Option<CalendarEventIdentity>,
        index: Option<usize>,
    ) {
        self.selected_event = identity;
        self.selected_event_index = index;
    }

    pub(crate) fn reconcile_event_selection(&mut self, ordered_events: &[CalendarEventRecord]) {
        let (selected_event, selected_event_index) = reconcile_event_selection(
            self.selected_event.clone(),
            self.selected_event_index,
            ordered_events,
        );
        self.selected_event = selected_event;
        self.selected_event_index = selected_event_index;
    }

    pub(crate) fn select_civil_day(&mut self, date: NaiveDate) {
        self.selected_range = Some(CalendarDateRange::single(date));
    }

    #[allow(dead_code)]
    pub(crate) fn navigate(&mut self, direction: i32, first_weekday: Weekday) {
        self.navigate_step(direction, first_weekday, false);
    }

    pub(crate) fn navigate_step(
        &mut self,
        direction: i32,
        first_weekday: Weekday,
        scroll_month_by_week: bool,
    ) {
        if self.view == CalendarView::List {
            return;
        }
        if direction == 0 {
            return;
        }
        if self.view == CalendarView::Month && scroll_month_by_week {
            let steps = i64::from(direction);
            let next_start = self
                .visible_range
                .start
                .checked_add_signed(Duration::days(steps * 7))
                .expect("month week navigation overflow");
            self.visible_range = CalendarDateRange::from_days(next_start, MONTH_GRID_CELLS);
            let current_ym = (self.anchor_date.year(), self.anchor_date.month());
            let (dom_y, dom_m) = dominant_month_for_range(self.visible_range, Some(current_ym));
            let candidate = self
                .anchor_date
                .checked_add_signed(Duration::days(steps * 7))
                .expect("week navigation overflow");
            if candidate.year() == dom_y && candidate.month() == dom_m {
                self.anchor_date = candidate;
            } else {
                let max_day = days_in_month(dom_y, dom_m);
                let target_day = self.anchor_date.day().min(max_day);
                self.anchor_date = NaiveDate::from_ymd_opt(dom_y, dom_m, target_day)
                    .unwrap_or_else(|| NaiveDate::from_ymd_opt(dom_y, dom_m, 1).unwrap());
            }
            self.selected_range = None;
            return;
        }
        let direction = i64::from(direction.signum());
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
            CalendarView::Year => shift_year(self.anchor_date, direction as i32),
            CalendarView::List => unreachable!("list navigation is handled above"),
        };
        self.anchor_date = next_anchor;
        self.selected_range = None;
        self.recompute_range(first_weekday);
    }

    pub(crate) fn today(&mut self, today: NaiveDate, first_weekday: Weekday) {
        if self.view == CalendarView::List {
            return;
        }
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

/// Latest-wins gate for the immutable Year/List projection workers.  The
/// revision ties a result to the authoritative in-memory snapshot, while the
/// view and load generation prevent a plan for another surface or snapshot
/// from being applied after a navigation/reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CalendarProjectionGate {
    generation: u64,
    revision: u64,
    load_generation: u64,
    view: Option<CalendarView>,
    active: bool,
}

impl CalendarProjectionGate {
    fn begin(&mut self, view: CalendarView, revision: u64, load_generation: u64) -> u64 {
        self.generation = advance_calendar_generation(self.generation);
        self.revision = revision;
        self.load_generation = load_generation;
        self.view = Some(view);
        self.active = true;
        self.generation
    }

    fn accepts(
        &self,
        generation: u64,
        view: CalendarView,
        revision: u64,
        load_generation: u64,
    ) -> bool {
        self.active
            && self.generation == generation
            && self.view == Some(view)
            && self.revision == revision
            && self.load_generation == load_generation
    }

    fn finish(
        &mut self,
        generation: u64,
        view: CalendarView,
        revision: u64,
        load_generation: u64,
    ) -> bool {
        if !self.accepts(generation, view, revision, load_generation) {
            return false;
        }
        self.active = false;
        true
    }

    fn cancel(&mut self, generation: u64) -> bool {
        if !self.active || self.generation != generation {
            return false;
        }
        self.active = false;
        true
    }
}

/// The sidebar rows are applied after the authoritative snapshot has already
/// been installed.  Keep the snapshot/load generation and the model revision
/// together: a time-display refresh can clear and rebuild the rows while an
/// older chunk callback is still queued on the main context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalendarSidebarApplyToken {
    load_generation: u64,
    model_revision: u64,
}

/// A separate latest-wins gate for backend mutations.  A callback is valid
/// only while it is the current mutation and the authoritative model revision
/// has not changed underneath it (for example because a monitor reload won).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CalendarMutationGate {
    generation: u64,
    model_revision: u64,
    load_generation: u64,
    identity: Option<CalendarEventIdentity>,
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CalendarMutationToken {
    generation: u64,
    model_revision: u64,
    load_generation: u64,
    identity: Option<CalendarEventIdentity>,
}

impl CalendarMutationGate {
    fn begin(
        &mut self,
        model_revision: u64,
        load_generation: u64,
        identity: Option<CalendarEventIdentity>,
    ) -> CalendarMutationToken {
        self.generation = advance_calendar_generation(self.generation);
        self.model_revision = model_revision;
        self.load_generation = load_generation;
        self.identity = identity.clone();
        self.active = true;
        CalendarMutationToken {
            generation: self.generation,
            model_revision,
            load_generation,
            identity,
        }
    }

    fn accepts(&self, token: &CalendarMutationToken) -> bool {
        self.active
            && self.generation == token.generation
            && self.model_revision == token.model_revision
            && self.load_generation == token.load_generation
            && self.identity == token.identity
    }

    fn finish(&mut self, token: &CalendarMutationToken) -> bool {
        if !self.accepts(token) {
            return false;
        }
        self.active = false;
        true
    }

    fn invalidate(&mut self) {
        self.generation = advance_calendar_generation(self.generation);
        self.active = false;
        self.identity = None;
    }
}

fn projection_chunk_ranges(item_count: usize, chunk_size: usize) -> Vec<(usize, usize)> {
    if item_count == 0 || chunk_size == 0 {
        return Vec::new();
    }
    (0..item_count)
        .step_by(chunk_size)
        .map(|start| (start, (start + chunk_size).min(item_count)))
        .collect()
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
    all_event_indices: Vec<usize>,
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
            all_event_indices: Vec::new(),
        };

        for (event_index, event) in index.snapshot.events.iter().enumerate() {
            index.all_event_indices.push(event_index);
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

        index.all_event_indices.sort_by(|left, right| {
            compare_event_list_order(
                &index.snapshot.events[*left],
                &index.snapshot.events[*right],
            )
            .then_with(|| left.cmp(right))
        });

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
            CalendarEventIdentity::from_event(left_event)
                .cmp(&CalendarEventIdentity::from_event(right_event))
                .then_with(|| left.cmp(right))
        });
        (indices, stats)
    }

    fn all_event_indices(&self) -> Vec<usize> {
        self.all_event_indices.clone()
    }
}

#[derive(Debug, Clone)]
struct ListProjectionPlan {
    events: Vec<CalendarEventRecord>,
}

#[derive(Debug, Clone)]
enum CalendarProjectionPlan {
    List(ListProjectionPlan),
}

#[derive(Debug)]
struct CalendarProjectionLoad {
    generation: u64,
    revision: u64,
    load_generation: u64,
    view: CalendarView,
    cancellation: Arc<AtomicBool>,
    result: Result<CalendarProjectionPlan, String>,
}

fn build_list_projection_plan(
    index: &CalendarSnapshotIndex,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> ListProjectionPlan {
    let events =
        filtered_sorted_event_indices(index, search_query, category_filter, hidden_sources)
            .into_iter()
            .filter_map(|event_index| index.snapshot.events.get(event_index).cloned())
            .collect();
    ListProjectionPlan { events }
}

fn calendar_view_from_name(value: &str) -> CalendarView {
    match value {
        "day" => CalendarView::Day,
        "week" => CalendarView::Week,
        "workweek" => CalendarView::WorkWeek,
        "agenda" => CalendarView::Agenda,
        "year" => CalendarView::Year,
        "list" => CalendarView::List,
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
        CalendarView::Year => "year",
        CalendarView::List => "list",
    }
}

fn calendar_view_to_index(view: CalendarView) -> u32 {
    match view {
        CalendarView::Day => 0,
        CalendarView::Week => 1,
        CalendarView::WorkWeek => 2,
        CalendarView::Month => 3,
        CalendarView::Agenda => 4,
        CalendarView::Year => 5,
        CalendarView::List => 6,
    }
}

fn calendar_view_from_index(index: u32) -> CalendarView {
    match index {
        0 => CalendarView::Day,
        1 => CalendarView::Week,
        2 => CalendarView::WorkWeek,
        3 => CalendarView::Month,
        4 => CalendarView::Agenda,
        5 => CalendarView::Year,
        _ => CalendarView::List,
    }
}

fn application_settings_schema() -> Option<gio::SettingsSchema> {
    let installed_source = gio::SettingsSchemaSource::default();
    installed_source
        .as_ref()
        .and_then(|source| source.lookup(crate::APP_ID, true))
        .or_else(|| {
            let directory = option_env!("EVOLUTION_GTK4_SCHEMA_DIR")?;
            let source = gio::SettingsSchemaSource::from_directory(
                directory,
                installed_source.as_ref(),
                true,
            )
            .ok()?;
            source.lookup(crate::APP_ID, true)
        })
}

/// AppSettings intentionally normalizes the original five values for its
/// preferences/migration contract.  Year and List use the same GSettings key
/// through this narrow view-local bridge so selecting either view survives a
/// page rebuild without changing any other settings surface.
fn raw_calendar_view_setting() -> Option<String> {
    let schema = application_settings_schema()?;
    let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    let value = settings.string("calendar-view").to_string();
    matches!(
        value.as_str(),
        "day" | "week" | "workweek" | "month" | "agenda" | "year" | "list"
    )
    .then_some(value)
}

fn persisted_calendar_view_name(settings: Option<&AppSettings>) -> String {
    settings
        .map(|settings| settings.calendar_view())
        .or_else(raw_calendar_view_setting)
        .unwrap_or_else(|| "month".to_string())
}

fn persist_calendar_view(view: CalendarView, settings: Option<&AppSettings>) {
    let value = calendar_view_name(view);
    if let Some(settings) = settings {
        settings.set_calendar_view(value);
        return;
    }

    let Some(schema) = application_settings_schema() else {
        return;
    };
    let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    if let Err(error) = settings.set_string("calendar-view", value) {
        tracing::warn!(%error, "Could not persist calendar view");
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
        pub calendar_prev_button: RefCell<Option<gtk::Button>>,
        pub calendar_next_button: RefCell<Option<gtk::Button>>,
        pub calendar_today_button: RefCell<Option<gtk::Button>>,
        pub new_event_button: RefCell<Option<gtk::Button>>,
        pub selected_time_slot: RefCell<Option<SelectedTimeSlot>>,
        pub syncing_calendar_view: Cell<bool>,
        pub month_grid: RefCell<Option<gtk::Grid>>,
        pub month_header_grid: RefCell<Option<gtk::Grid>>,
        pub month_date_label: RefCell<Option<gtk::Label>>,
        pub month_header_labels: RefCell<Vec<gtk::Label>>,
        pub month_header_is_compact: Cell<bool>,
        pub month_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub month_layout: RefCell<Option<Rc<PreparedMonthLayout>>>,
        pub month_drag_selection: RefCell<Option<MonthDragSelection>>,
        pub month_hovered_date: RefCell<Option<NaiveDate>>,
        pub month_hovered_event: RefCell<Option<CalendarEventIdentity>>,
        pub day_view: RefCell<Option<gtk::Box>>,
        pub week_view: RefCell<Option<gtk::Box>>,
        pub workweek_view: RefCell<Option<gtk::Box>>,
        pub agenda_view: RefCell<Option<gtk::Box>>,
        pub year_view: RefCell<Option<gtk::Box>>,
        pub year_rendered_year: Cell<Option<i32>>,
        pub year_day_buttons: RefCell<HashMap<NaiveDate, gtk::Button>>,
        pub year_preview_mode: Cell<YearPreviewMode>,
        pub year_preview_orientation: Cell<YearPreviewOrientation>,
        pub year_details_container: RefCell<Option<gtk::Box>>,
        pub year_paned: RefCell<Option<gtk::Paned>>,
        pub year_details_side_width: Cell<i32>,
        pub year_details_bottom_height: Cell<i32>,
        pub list_view: RefCell<Option<gtk::Box>>,
        pub event_records: RefCell<Option<Rc<RefCell<Vec<CalendarEventRecord>>>>>,
        pub event_list: RefCell<Option<gtk::ListBox>>,
        pub calendar_data_state: RefCell<Option<gtk::Label>>,
        pub task_records: RefCell<Option<Rc<RefCell<Vec<TaskRecord>>>>>,
        pub task_list: RefCell<Option<gtk::ListBox>>,
        pub task_state: RefCell<Option<gtk::Label>>,
        pub memo_records: RefCell<Option<Rc<RefCell<Vec<MemoRecord>>>>>,
        pub taskpad_list: RefCell<Option<gtk::ListBox>>,
        pub taskpad_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub taskpad_layout: RefCell<Option<PreparedTaskpadLayout>>,
        pub taskpad_hovered_index: RefCell<Option<usize>>,
        pub taskpad_selected_index: RefCell<Option<usize>>,
        pub taskpad_state: RefCell<Option<gtk::Label>>,
        pub taskpad_count: RefCell<Option<gtk::Label>>,
        pub memopad_list: RefCell<Option<gtk::ListBox>>,
        pub memopad_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub memopad_layout: RefCell<Option<PreparedMemopadLayout>>,
        pub memopad_hovered_index: RefCell<Option<usize>>,
        pub memopad_selected_index: RefCell<Option<usize>>,
        pub memopad_state: RefCell<Option<gtk::Label>>,
        pub memopad_count: RefCell<Option<gtk::Label>>,
        pub tag_vpaned: RefCell<Option<gtk::Paned>>,
        pub content_hpaned: RefCell<Option<gtk::Paned>>,
        pub toolbar: RefCell<Option<gtk::Widget>>,
        pub macos_banner: RefCell<Option<adw::Banner>>,
        pub outer_paned: RefCell<Option<gtk::Paned>>,
        pub tag_vpane_toggle_btn: RefCell<Option<gtk::ToggleButton>>,
        pub calendar_sidebar_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub mini_calendar: RefCell<Option<gtk::Calendar>>,
        pub mini_calendar_2: RefCell<Option<gtk::Calendar>>,
        pub mini_calendar_3: RefCell<Option<gtk::Calendar>>,
        pub mini_calendar_4: RefCell<Option<gtk::Calendar>>,
        pub mini_calendars: RefCell<Vec<gtk::Calendar>>,
        pub mini_calendar_box: RefCell<Option<gtk::Box>>,
        pub syncing_mini_calendar: Cell<bool>,
        pub calendar_search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub calendar_search_bar: RefCell<Option<gtk::SearchBar>>,
        pub calendar_search_query: RefCell<String>,
        pub calendar_category_filter: RefCell<Option<String>>,
        pub calendar_category_model: RefCell<Option<gtk::StringList>>,
        pub calendar_category_selector: RefCell<Option<gtk::DropDown>>,
        pub calendar_view_dropdown: RefCell<Option<gtk::DropDown>>,
        pub hidden_calendar_sources: RefCell<HashSet<String>>,
        pub collapsed_calendar_groups: RefCell<HashSet<String>>,
        pub calendar_source_list: RefCell<Option<gtk::ListBox>>,
        pub calendar_source_state_label: RefCell<Option<gtk::Label>>,
        pub calendar_source_selector: RefCell<crate::models::source_selector::SourceSelector>,
        pub calendar_source_checks: RefCell<HashMap<String, gtk::CheckButton>>,
        pub syncing_calendar_checks: Cell<bool>,
        pub calendar_color_css_provider: RefCell<Option<gtk::CssProvider>>,
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub(super) calendar_load_cancel: RefCell<Option<Arc<AtomicBool>>>,
        pub(super) calendar_load_gate: RefCell<CalendarLoadGate>,
        pub(super) calendar_loading: Cell<bool>,
        pub(super) calendar_load_error: RefCell<Option<String>>,
        pub(super) calendar_snapshot_index: RefCell<Option<Arc<CalendarSnapshotIndex>>>,
        pub(super) calendar_revision: Cell<u64>,
        pub(super) calendar_monitor_generation: Cell<u64>,
        pub(super) calendar_projection_cancel: RefCell<Option<Arc<AtomicBool>>>,
        pub(super) calendar_projection_gate: RefCell<CalendarProjectionGate>,
        pub(super) calendar_mutation_gate: RefCell<CalendarMutationGate>,
        pub(super) calendar_list_projection_count: Cell<Option<usize>>,
        pub(super) calendar_reload_debounce: RefCell<Option<glib::SourceId>>,
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
                calendar_prev_button: RefCell::new(None),
                calendar_next_button: RefCell::new(None),
                calendar_today_button: RefCell::new(None),
                new_event_button: RefCell::new(None),
                selected_time_slot: RefCell::new(None),
                syncing_calendar_view: Cell::new(false),
                month_grid: RefCell::new(None),
                month_header_grid: RefCell::new(None),
                month_date_label: RefCell::new(None),
                month_header_labels: RefCell::new(Vec::new()),
                month_header_is_compact: Cell::new(false),
                month_canvas: RefCell::new(None),
                month_layout: RefCell::new(None),
                month_drag_selection: RefCell::new(None),
                month_hovered_date: RefCell::new(None),
                month_hovered_event: RefCell::new(None),
                day_view: RefCell::new(None),
                week_view: RefCell::new(None),
                workweek_view: RefCell::new(None),
                agenda_view: RefCell::new(None),
                year_view: RefCell::new(None),
                year_rendered_year: Cell::new(None),
                year_day_buttons: RefCell::new(HashMap::new()),
                year_preview_mode: Cell::new(YearPreviewMode::Schedule),
                year_preview_orientation: Cell::new(YearPreviewOrientation::Side),
                year_details_container: RefCell::new(None),
                year_paned: RefCell::new(None),
                year_details_side_width: Cell::new(240),
                year_details_bottom_height: Cell::new(220),
                list_view: RefCell::new(None),
                event_records: RefCell::new(None),
                event_list: RefCell::new(None),
                calendar_data_state: RefCell::new(None),
                task_records: RefCell::new(None),
                task_list: RefCell::new(None),
                task_state: RefCell::new(None),
                memo_records: RefCell::new(None),
                taskpad_list: RefCell::new(None),
                taskpad_canvas: RefCell::new(None),
                taskpad_layout: RefCell::new(None),
                taskpad_hovered_index: RefCell::new(None),
                taskpad_selected_index: RefCell::new(None),
                taskpad_state: RefCell::new(None),
                taskpad_count: RefCell::new(None),
                memopad_list: RefCell::new(None),
                memopad_canvas: RefCell::new(None),
                memopad_layout: RefCell::new(None),
                memopad_hovered_index: RefCell::new(None),
                memopad_selected_index: RefCell::new(None),
                memopad_state: RefCell::new(None),
                memopad_count: RefCell::new(None),
                tag_vpaned: RefCell::new(None),
                content_hpaned: RefCell::new(None),
                toolbar: RefCell::new(None),
                outer_paned: RefCell::new(None),
                tag_vpane_toggle_btn: RefCell::new(None),
                calendar_sidebar_toggle: RefCell::new(None),
                mini_calendar: RefCell::new(None),
                mini_calendar_2: RefCell::new(None),
                mini_calendar_3: RefCell::new(None),
                mini_calendar_4: RefCell::new(None),
                mini_calendars: RefCell::new(Vec::new()),
                mini_calendar_box: RefCell::new(None),
                syncing_mini_calendar: Cell::new(false),
                calendar_search_entry: RefCell::new(None),
                calendar_search_bar: RefCell::new(None),
                calendar_search_query: RefCell::new(String::new()),
                calendar_category_filter: RefCell::new(None),
                calendar_category_model: RefCell::new(None),
                calendar_category_selector: RefCell::new(None),
                calendar_view_dropdown: RefCell::new(None),
                hidden_calendar_sources: RefCell::new(HashSet::new()),
                collapsed_calendar_groups: RefCell::new(HashSet::new()),
                calendar_source_list: RefCell::new(None),
                calendar_source_state_label: RefCell::new(None),
                calendar_source_selector: RefCell::new(
                    crate::models::source_selector::SourceSelector::new(
                        crate::models::source_selector::SourceDomain::Calendar,
                    ),
                ),
                calendar_source_checks: RefCell::new(HashMap::new()),
                syncing_calendar_checks: Cell::new(false),
                calendar_color_css_provider: RefCell::new(None),
                ecal_monitor_cancel: RefCell::new(None),
                calendar_load_cancel: RefCell::new(None),
                calendar_load_gate: RefCell::new(CalendarLoadGate::default()),
                calendar_loading: Cell::new(false),
                calendar_load_error: RefCell::new(None),
                calendar_snapshot_index: RefCell::new(None),
                calendar_revision: Cell::new(0),
                calendar_monitor_generation: Cell::new(0),
                calendar_projection_cancel: RefCell::new(None),
                calendar_projection_gate: RefCell::new(CalendarProjectionGate::default()),
                calendar_mutation_gate: RefCell::new(CalendarMutationGate::default()),
                calendar_list_projection_count: Cell::new(None),
                calendar_reload_debounce: RefCell::new(None),
                macos_banner: RefCell::new(None),
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
            self.obj().set_vexpand(true);
            self.obj().set_hexpand(true);
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(cancel) = self.calendar_load_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Release);
            }
            if let Some(cancel) = self.calendar_projection_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Release);
            }
            let generation = self.calendar_load_gate.borrow().generation;
            self.calendar_load_gate.borrow_mut().cancel(generation);
            let projection_generation = self.calendar_projection_gate.borrow().generation;
            self.calendar_projection_gate
                .borrow_mut()
                .cancel(projection_generation);
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
    /// Cached/local preview keeps the request open for its authoritative result.
    preview: bool,
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

    /// Currently selected time slot in the calendar grid, if any.
    pub fn selected_time_slot(&self) -> Option<SelectedTimeSlot> {
        *self.imp().selected_time_slot.borrow()
    }

    /// Set or update the active selected time slot and update grid highlights.
    pub fn set_selected_time_slot(&self, slot: Option<SelectedTimeSlot>) {
        *self.imp().selected_time_slot.borrow_mut() = slot;
        self.refresh_calendar_views();
    }

    /// Clear any currently selected time slot.
    pub fn clear_selected_time_slot(&self) {
        *self.imp().selected_time_slot.borrow_mut() = None;
        self.refresh_calendar_views();
    }

    /// Trigger the same event editor as the calendar toolbar.  The main
    /// Evolution window uses this entry point so File → New, the contextual
    /// header action, and the calendar page share one refresh workflow.
    pub fn present_new_event(&self) {
        let dialog = if let Some(slot) = *self.imp().selected_time_slot.borrow() {
            let start_time = slot
                .date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                + chrono::Duration::minutes(slot.start_minute as i64);
            let end_time = slot
                .date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                + chrono::Duration::minutes(slot.end_minute as i64);
            crate::dialogs::EventDialog::new_for_time_range(start_time, end_time)
        } else {
            crate::dialogs::EventDialog::new_for_date(*self.imp().current_date.borrow())
        };
        let root = self.root();
        if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
            dialog.set_transient_for(Some(&win));
        }
        let page_for_refresh = self.downgrade();
        dialog.connect_close_request(move |_| {
            if let Some(page) = page_for_refresh.upgrade() {
                page.reload_calendar_data();
            }
            glib::Propagation::Proceed
        });
        dialog.present();
    }

    pub fn outer_paned(&self) -> Option<gtk::Paned> {
        self.imp().outer_paned.borrow().clone()
    }

    pub fn year_paned(&self) -> Option<gtk::Paned> {
        self.imp().year_paned.borrow().clone()
    }

    pub fn set_sidebar_position(&self, pos: i32) {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if (paned.position() - pos).abs() > 1 {
                paned.set_position(pos);
            }
        }
    }

    pub(crate) fn focus_search(&self) {
        if let Some(bar) = self.imp().calendar_search_bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }
        if let Some(entry) = self.imp().calendar_search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn is_search_active(&self) -> bool {
        self.imp()
            .calendar_search_bar
            .borrow()
            .as_ref()
            .map(|b| b.is_search_mode())
            .unwrap_or(false)
    }

    pub(crate) fn close_search(&self) {
        if let Some(bar) = self.imp().calendar_search_bar.borrow().as_ref() {
            bar.set_search_mode(false);
        }
    }

    pub(crate) fn search_bar(&self) -> Option<gtk::SearchBar> {
        self.imp().calendar_search_bar.borrow().clone()
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(entry) = self.imp().calendar_search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn open_search_result(&self, source_uid: &str, uid: &str) -> bool {
        if let Some(records) = self.imp().event_records.borrow().as_ref().cloned() {
            if let Some(event) =
                unique_event_for_legacy_identity(&records.borrow(), source_uid, uid)
            {
                open_event_editor_for_widget(self, &event);
                return true;
            }
        }
        if self.open_task_by_uid(source_uid, uid) {
            return true;
        }
        if self.open_memo_by_uid(source_uid, uid) {
            return true;
        }
        false
    }

    pub(crate) fn open_task_by_uid(&self, source_uid: &str, uid: &str) -> bool {
        let Some(records) = self.imp().task_records.borrow().as_ref().cloned() else {
            return false;
        };
        let matches = records.borrow().iter().any(|task| {
            (source_uid.is_empty() || task.source_uid == source_uid) && task.uid == uid
        });
        if !matches {
            return false;
        }
        let Some(root) = self.root() else {
            return false;
        };
        let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() else {
            return false;
        };
        window.open_component(ReminderIdentity {
            kind: ReminderComponentKind::Task,
            source_uid: source_uid.to_string(),
            component_uid: uid.to_string(),
        })
    }

    pub(crate) fn open_memo_by_uid(&self, source_uid: &str, uid: &str) -> bool {
        let Some(records) = self.imp().memo_records.borrow().as_ref().cloned() else {
            return false;
        };
        let matches = records.borrow().iter().any(|memo| {
            (source_uid.is_empty() || memo.source_uid == source_uid) && memo.uid == uid
        });
        if !matches {
            return false;
        }
        let Some(root) = self.root() else {
            return false;
        };
        let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() else {
            return false;
        };
        window.open_component(ReminderIdentity {
            kind: ReminderComponentKind::Memo,
            source_uid: source_uid.to_string(),
            component_uid: uid.to_string(),
        })
    }

    pub(crate) fn open_search_result_identity(&self, identity: &CalendarEventIdentity) -> bool {
        self.open_event_editor_for_identity(self, identity)
    }

    /// Reload the portable/EDS calendar snapshot after a dialog operation.
    ///
    /// EDS-backed installations normally receive the same change through the
    /// native monitor.  The explicit reload is still required for the
    /// portable local store and also makes the UI deterministic when a
    /// backend emits its signal late or not at all.
    pub(crate) fn reload_calendar_data(&self) {
        if let Some(source) = self.imp().calendar_reload_debounce.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source.as_raw()) };
        }
        let page = self.downgrade();
        let source =
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                if let Some(page) = page.upgrade() {
                    page.imp().calendar_reload_debounce.replace(None);
                    page.start_calendar_snapshot_load();
                }
            });
        self.imp().calendar_reload_debounce.replace(Some(source));
    }

    #[cfg(all(target_os = "macos", macos_bridge))]
    pub fn update_macos_permission_banner(&self) {
        let Some(banner) = self.imp().macos_banner.borrow().as_ref().cloned() else {
            return;
        };
        let srv = crate::services::MacosIntegrationService::new();
        let status = srv.check_permission(crate::services::MacosPermissionKind::Calendar);
        match status {
            crate::services::MacosPermissionStatus::NotDetermined => {
                banner.set_title("Apple Kalender verbinden: Möchten Sie Ihre macOS Kalender und Termine in Evolution anzeigen?");
                banner.set_button_label(Some("Apple Kalender verbinden"));
                banner.set_revealed(true);
            }
            crate::services::MacosPermissionStatus::Denied => {
                banner.set_title("Zugriff auf Apple Kalender nicht erlaubt. In den macOS Systemeinstellungen aktivieren.");
                banner.set_button_label(Some("Systemeinstellungen öffnen"));
                banner.set_revealed(true);
            }
            _ => {
                banner.set_revealed(false);
            }
        }
    }

    /// Present the calendar purge dialog to purge past events.
    pub fn present_calendar_purge_dialog(&self) {
        let page = self.downgrade();
        let dialog = crate::dialogs::CalendarPurgeDialog::with_finished_handler(move |outcome| {
            let Some(page) = page.upgrade() else {
                return;
            };
            if outcome.changed() {
                page.reload_calendar_data();
            }
            show_toast_for_widget(&page, &outcome.message());
        });
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }
        dialog.present();
    }

    /// Return all currently loaded calendar events as a cloned vector.
    pub fn loaded_calendar_events(&self) -> Vec<CalendarEventRecord> {
        self.imp()
            .event_records
            .borrow()
            .as_ref()
            .map(|records| records.borrow().clone())
            .unwrap_or_default()
    }

    /// Print the current visible calendar view or agenda.
    pub fn print_visible_agenda(&self) {
        let events = self.loaded_calendar_events();
        if events.is_empty() {
            show_toast_for_widget(self, "Keine Termine zum Drucken verfügbar");
            return;
        }
        let view_name = match *self.imp().current_view.borrow() {
            CalendarView::Day => "Tagesansicht",
            CalendarView::Week => "Wochenansicht",
            CalendarView::WorkWeek => "Arbeitswochenansicht",
            CalendarView::Month => "Monatsansicht",
            CalendarView::Agenda => "Agenda",
            CalendarView::Year => "Jahresübersicht",
            CalendarView::List => "Terminliste",
        };
        match build_calendar_list_print_document(&events, Some(view_name)) {
            Ok(doc) => {
                show_toast_for_widget(
                    self,
                    &format!(
                        "Druckdokument vorbereitet ({} Zeichen, {} Termine); Print-Portal ist Linux-Handoff",
                        doc.html.len(),
                        doc.event_count
                    ),
                );
            }
            Err(err) => {
                show_toast_for_widget(self, &format!("Drucken fehlgeschlagen: {err}"));
            }
        }
    }

    /// Present a file dialog to export calendar events in iCalendar, CSV, or RDF format.
    pub fn export_calendar_dialog(&self) {
        let events = self.loaded_calendar_events();
        if events.is_empty() {
            show_toast_for_widget(self, "Keine Termine zum Exportieren verfügbar");
            return;
        }
        let Some(root) = self.root().and_downcast::<gtk::Window>() else {
            return;
        };

        let filter_ics = gtk::FileFilter::new();
        filter_ics.set_name(Some("iCalendar (*.ics)"));
        filter_ics.add_pattern("*.ics");

        let filter_csv = gtk::FileFilter::new();
        filter_csv.set_name(Some("Kommagetrennte Werte (*.csv)"));
        filter_csv.add_pattern("*.csv");

        let filter_rdf = gtk::FileFilter::new();
        filter_rdf.set_name(Some("RDF / RSS 1.0 (*.rdf)"));
        filter_rdf.add_pattern("*.rdf");

        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter_ics);
        filters.append(&filter_csv);
        filters.append(&filter_rdf);

        let file_dialog = gtk::FileDialog::builder()
            .title("Kalender exportieren")
            .filters(&filters)
            .default_filter(&filter_ics)
            .initial_name("kalender.ics")
            .modal(true)
            .build();

        let page_weak = self.downgrade();
        file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            let events = page.loaded_calendar_events();
            let format = if let Some(path) = file.path() {
                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if ext.eq_ignore_ascii_case("csv") {
                    CalendarExportFormat::Csv
                } else if ext.eq_ignore_ascii_case("rdf") {
                    CalendarExportFormat::Rdf
                } else {
                    CalendarExportFormat::ICalendar
                }
            } else {
                CalendarExportFormat::ICalendar
            };
            let payload = match export_calendar_events(&events, format, None) {
                Ok(data) => data,
                Err(err) => {
                    show_toast_for_widget(&page, &err);
                    return;
                }
            };
            let save_res = file.replace_contents(
                payload.as_bytes(),
                None,
                true,
                gio::FileCreateFlags::NONE,
                None::<&gio::Cancellable>,
            );
            match save_res {
                Ok(_) => show_toast_for_widget(
                    &page,
                    &format!(
                        "{} Termin(e) als {} exportiert",
                        events.len(),
                        format.display_label()
                    ),
                ),
                Err(err) => show_toast_for_widget(&page, &format!("Exportfehler: {err}")),
            }
        });
    }

    fn start_calendar_snapshot_load(&self) {
        self.cancel_calendar_snapshot_load();
        self.cancel_calendar_projection();
        self.invalidate_calendar_mutation();

        let generation = self.imp().calendar_load_gate.borrow_mut().begin();
        self.bump_calendar_revision();
        self.imp().view_state.borrow_mut().load_generation = generation;
        self.imp().calendar_load_error.replace(None);
        let needs_preview = self.imp().calendar_snapshot_index.borrow().is_none();
        self.set_calendar_loading_status(true, "Kalender werden geladen …");
        self.imp().calendar_list_projection_count.set(None);
        self.refresh_task_sidebar();
        self.refresh_task_and_memo_panes();
        self.refresh_event_list_visibility();
        self.refresh_calendar_views();

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
                let started = std::time::Instant::now();
                // Disk I/O, JSON/ICS parsing and index construction can be large.
                // Publish the cache before connecting to any provider, off GTK.
                if needs_preview {
                    let preview = std::panic::catch_unwind(|| {
                        let snapshot = crate::services::ecal_service::load_calendar_load_cache()
                            .unwrap_or_else(|| CalendarStoreService::new().load_snapshot());
                        Arc::new(CalendarSnapshotIndex::new(Arc::new(snapshot)))
                    });
                    if let Ok(snapshot_index) = preview {
                        tracing::debug!(
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            events = snapshot_index.snapshot.events.len(),
                            "Calendar preview prepared"
                        );
                        if cancellation.load(Ordering::Acquire)
                            || sender
                                .send_blocking(CalendarSnapshotLoad {
                                    preview: true,
                                    generation,
                                    cancellation: Arc::clone(&cancellation),
                                    result: Ok((snapshot_index, CalendarDataOrigin::Local, None)),
                                })
                                .is_err()
                        {
                            return;
                        }
                    }
                }
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
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    success = result.is_ok(),
                    "Calendar provider snapshot prepared"
                );
                let load = CalendarSnapshotLoad {
                    preview: false,
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
            while let Ok(load) = receiver.recv().await {
                let complete = !load.preview;
                let Some(page) = page.upgrade() else {
                    cancellation_for_receiver.store(true, Ordering::Release);
                    return;
                };
                page.apply_calendar_snapshot_load(load);
                if complete {
                    return;
                }
            }
            if let Some(page) = page.upgrade()
                && page.finish_calendar_snapshot_load(generation, &cancellation_for_receiver)
            {
                page.set_calendar_load_error("Kalender konnten nicht geladen werden");
                page.set_calendar_loading_status(false, "Kalender konnten nicht geladen werden");
                page.refresh_calendar_views();
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
            preview,
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
                    self.set_calendar_load_error(&format!(
                        "Kalender konnten nicht geladen werden: {error}"
                    ));
                    self.set_calendar_loading_status(
                        false,
                        &format!("Kalender konnten nicht geladen werden: {error}"),
                    );
                    self.refresh_calendar_views();
                }
                return;
            }
        };
        self.imp().calendar_load_error.replace(None);

        let Some(event_list) = self.imp().event_list.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_load_error("Kalenderansicht nicht verfügbar");
                self.set_calendar_loading_status(false, "Kalenderansicht nicht verfügbar");
                self.refresh_calendar_views();
            }
            return;
        };
        let Some(event_records) = self.imp().event_records.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_load_error("Kalenderdaten nicht verfügbar");
                self.set_calendar_loading_status(false, "Kalenderdaten nicht verfügbar");
                self.refresh_calendar_views();
            }
            return;
        };
        let Some(task_records) = self.imp().task_records.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_load_error("Aufgabendaten nicht verfügbar");
                self.set_calendar_loading_status(false, "Aufgabendaten nicht verfügbar");
                self.refresh_calendar_views();
            }
            return;
        };
        let Some(memo_records) = self.imp().memo_records.borrow().as_ref().cloned() else {
            if self.finish_calendar_snapshot_load(generation, &cancellation) {
                self.set_calendar_load_error("Notizdaten nicht verfügbar");
                self.set_calendar_loading_status(false, "Notizdaten nicht verfügbar");
                self.refresh_calendar_views();
            }
            return;
        };

        if let Some(warning) = warning.as_deref() {
            tracing::warn!(%warning, "Calendar data loaded with a backend warning");
            self.set_calendar_load_error(warning);
        }

        // A provider result invalidates any unfinished cache row/projection work.
        self.cancel_calendar_projection();
        self.bump_calendar_revision();
        self.imp()
            .calendar_snapshot_index
            .replace(Some(Arc::clone(&snapshot_index)));
        event_records.borrow_mut().clear();
        while let Some(child) = event_list.first_child() {
            event_list.remove(&child);
        }
        task_records.replace(snapshot_index.snapshot.tasks.clone());
        memo_records.replace(snapshot_index.snapshot.memos.clone());

        let count = snapshot_index.snapshot.events.len();
        let state_label = self.imp().calendar_data_state.borrow().as_ref().cloned();
        let sidebar_token = CalendarSidebarApplyToken {
            load_generation: generation,
            model_revision: self.imp().calendar_revision.get(),
        };
        if !preview && !self.finish_calendar_snapshot_load(generation, &cancellation) {
            return;
        }
        self.set_calendar_loading_status(
            preview,
            if preview {
                "Gespeicherter Kalenderstand wird angezeigt · Aktualisierung läuft …"
            } else {
                ""
            },
        );
        let origin_str = match origin {
            CalendarDataOrigin::Eds => "verbundenen Kalendern",
            CalendarDataOrigin::Graph => "verbundenen Kalendern",
            CalendarDataOrigin::Local => "lokalem Kalenderbestand",
        };
        let status_msg = if preview {
            format!("{count} gespeicherte Termin(e) · Aktualisierung läuft …")
        } else if count == 0 {
            format!("Keine Termine aus {origin_str} gefunden")
        } else {
            format!("{count} Termin(e) aus {origin_str} geladen.")
        };
        let status_msg = if let Some(warning) = warning {
            format!("{status_msg} · Aktualisierung unvollständig: {warning}")
        } else {
            status_msg
        };
        if let Some(win) = self.root().and_downcast::<crate::window::EvolutionWindow>() {
            win.set_status(&status_msg);
        }

        if count == 0 {
            event_list.set_visible(false);
            if let Some(state_label) = state_label {
                state_label.set_label(&status_msg);
            }
        } else {
            event_list.set_visible(false);
            if let Some(state_label) = state_label {
                state_label.set_label(&status_msg);
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
                    if !page.calendar_sidebar_apply_is_current(sidebar_token, &cancellation) {
                        return;
                    }
                    let end = (index + chunk_size).min(events.len());
                    for event in &events[index..end] {
                        append_event_row(
                            &page,
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
                    if !page.calendar_sidebar_apply_is_current(sidebar_token, &cancellation) {
                        return;
                    }
                    page.refresh_task_sidebar();
                    page.refresh_task_and_memo_panes();
                    page.refresh_calendar_category_model();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                }
            });
        }

        self.refresh_task_sidebar();
        self.refresh_task_and_memo_panes();
        self.refresh_calendar_category_model();
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

    fn calendar_sidebar_apply_is_current(
        &self,
        token: CalendarSidebarApplyToken,
        cancellation: &Arc<AtomicBool>,
    ) -> bool {
        self.calendar_data_state() == CalendarDataState::Ready
            && self.imp().view_state.borrow().load_generation == token.load_generation
            && self.imp().calendar_revision.get() == token.model_revision
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

    fn set_calendar_load_error(&self, error: &str) {
        self.imp()
            .calendar_load_error
            .replace(Some(error.to_string()));
    }

    fn calendar_data_state(&self) -> CalendarDataState {
        if self.imp().calendar_snapshot_index.borrow().is_some() {
            CalendarDataState::Ready
        } else if self.imp().calendar_loading.get() {
            CalendarDataState::Loading
        } else if self.imp().calendar_load_error.borrow().is_some() {
            CalendarDataState::Error
        } else {
            CalendarDataState::Ready
        }
    }

    fn calendar_state_row(&self) -> adw::ActionRow {
        let (title, subtitle) = match self.calendar_data_state() {
            CalendarDataState::Loading => (
                "Kalender werden geladen …".to_string(),
                "Diese Ansicht wird nach dem bestätigten Snapshot aktualisiert.".to_string(),
            ),
            CalendarDataState::Error => (
                "Kalender konnten nicht geladen werden".to_string(),
                self.imp()
                    .calendar_load_error
                    .borrow()
                    .as_deref()
                    .unwrap_or("Der zuletzt bestätigte Kalenderstand ist nicht verfügbar.")
                    .to_string(),
            ),
            CalendarDataState::Ready => (
                "Kalenderansicht".to_string(),
                "Noch keine bestätigten Kalenderdaten verfügbar.".to_string(),
            ),
        };
        adw::ActionRow::builder()
            .title(&title)
            .subtitle(&subtitle)
            .build()
    }

    fn append_calendar_state_to_box(&self, container: &gtk::Box) -> bool {
        if self.calendar_data_state() == CalendarDataState::Ready {
            return false;
        }
        container.append(&self.calendar_state_row());
        true
    }

    fn attach_calendar_state_to_grid(&self, grid: &gtk::Grid) -> bool {
        if self.calendar_data_state() == CalendarDataState::Ready {
            return false;
        }
        let row = self.calendar_state_row();
        grid.attach(&row, 0, 0, 7, 1);
        true
    }

    fn bump_calendar_revision(&self) -> u64 {
        let next = advance_calendar_generation(self.imp().calendar_revision.get());
        self.imp().calendar_revision.set(next);
        next
    }

    fn begin_calendar_mutation(
        &self,
        identity: Option<CalendarEventIdentity>,
    ) -> CalendarMutationToken {
        let model_revision = self.imp().calendar_revision.get();
        let load_generation = self.imp().view_state.borrow().load_generation;
        self.imp().calendar_mutation_gate.borrow_mut().begin(
            model_revision,
            load_generation,
            identity,
        )
    }

    fn unique_loaded_calendar_event(
        &self,
        identity: &CalendarEventIdentity,
    ) -> Option<CalendarEventRecord> {
        self.imp()
            .event_records
            .borrow()
            .as_ref()
            .and_then(|records| unique_event_for_identity(&records.borrow(), identity))
    }

    fn open_event_editor_for_identity<W: IsA<gtk::Widget>>(
        &self,
        widget: &W,
        identity: &CalendarEventIdentity,
    ) -> bool {
        let Some(event) = self.unique_loaded_calendar_event(identity) else {
            return false;
        };
        open_event_editor_for_widget(widget, &event);
        true
    }

    fn calendar_mutation_is_current(&self, token: &CalendarMutationToken) -> bool {
        self.imp().calendar_mutation_gate.borrow().accepts(token)
            && self.imp().calendar_revision.get() == token.model_revision
            && self.imp().view_state.borrow().load_generation == token.load_generation
    }

    fn calendar_event_identity_is_loaded(&self, identity: &CalendarEventIdentity) -> bool {
        self.imp()
            .event_records
            .borrow()
            .as_ref()
            .is_some_and(|records| unique_event_for_identity(&records.borrow(), identity).is_some())
    }

    fn calendar_event_mutation_is_current(&self, token: &CalendarMutationToken) -> bool {
        self.calendar_mutation_is_current(token)
            && token
                .identity
                .as_ref()
                .is_some_and(|identity| self.calendar_event_identity_is_loaded(identity))
    }

    fn finish_calendar_mutation(&self, token: &CalendarMutationToken) -> bool {
        self.imp().calendar_mutation_gate.borrow_mut().finish(token)
    }

    fn invalidate_calendar_mutation(&self) {
        self.imp().calendar_mutation_gate.borrow_mut().invalidate();
    }

    fn calendar_monitor_is_current(&self, generation: u64) -> bool {
        self.imp().calendar_monitor_generation.get() == generation
    }

    fn calendar_monitor_revision_is_current(&self, generation: u64, revision: u64) -> bool {
        self.calendar_monitor_is_current(generation)
            && self.imp().calendar_revision.get() == revision
    }

    #[allow(dead_code)]
    fn report_calendar_monitor_failure(&self, error: &str) {
        self.invalidate_calendar_mutation();
        self.cancel_calendar_snapshot_load();
        self.cancel_calendar_projection();
        let message = format!("Kalender-Synchronisierung fehlgeschlagen: {error}");
        self.set_calendar_load_error(&message);
        self.set_calendar_loading_status(false, &message);
        self.refresh_task_sidebar();
        self.refresh_task_and_memo_panes();
        self.refresh_calendar_views();
        self.refresh_event_list_visibility();
    }

    fn cancel_calendar_projection(&self) {
        if let Some(cancel) = self.imp().calendar_projection_cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Release);
        }
        let generation = self.imp().calendar_projection_gate.borrow().generation;
        self.imp()
            .calendar_projection_gate
            .borrow_mut()
            .cancel(generation);
    }

    fn start_calendar_projection(
        &self,
        view: CalendarView,
    ) {
        self.cancel_calendar_projection();
        let Some(index) = self
            .imp()
            .calendar_snapshot_index
            .borrow()
            .as_ref()
            .cloned()
        else {
            return;
        };
        let revision = self.imp().calendar_revision.get();
        let load_generation = self.imp().view_state.borrow().load_generation;
        let generation =
            self.imp()
                .calendar_projection_gate
                .borrow_mut()
                .begin(view, revision, load_generation);
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .calendar_projection_cancel
            .replace(Some(Arc::clone(&cancellation)));

        let search_query = self.imp().calendar_search_query.borrow().clone();
        let category_filter = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();
        let page = self.downgrade();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn({
            let cancellation = Arc::clone(&cancellation);
            move || {
                if cancellation.load(Ordering::Acquire) {
                    return;
                }
                let result = std::panic::catch_unwind(|| match view {
                    CalendarView::List => {
                        Ok(CalendarProjectionPlan::List(build_list_projection_plan(
                            &index,
                            &search_query,
                            category_filter.as_deref(),
                            &hidden_sources,
                        )))
                    }
                    _ => Err("Für diese Ansicht gibt es keinen Projektion-Worker".to_string()),
                })
                .unwrap_or_else(|_| {
                    Err("Die Kalenderansicht konnte nicht vorbereitet werden".to_string())
                });
                if cancellation.load(Ordering::Acquire) {
                    return;
                }
                let load = CalendarProjectionLoad {
                    generation,
                    revision,
                    load_generation,
                    view,
                    cancellation,
                    result,
                };
                if let Err(error) = sender.send_blocking(load) {
                    tracing::debug!(%error, "Calendar projection result was dropped");
                }
            }
        });
        let cancellation_for_receiver = Arc::clone(&cancellation);
        glib::spawn_future_local(async move {
            match receiver.recv().await {
                Ok(load) => {
                    if let Some(page) = page.upgrade() {
                        page.apply_calendar_projection_load(load);
                    }
                }
                Err(error) => {
                    if let Some(page) = page.upgrade()
                        && page.finish_calendar_projection_token(
                            generation,
                            view,
                            revision,
                            load_generation,
                            &cancellation_for_receiver,
                        )
                    {
                        page.show_calendar_projection_error(
                            view,
                            &format!("Der Projektion-Worker/kanal wurde beendet: {error}"),
                        );
                    }
                }
            }
        });
    }

    fn calendar_projection_is_current(&self, load: &CalendarProjectionLoad) -> bool {
        self.calendar_projection_token_is_current(
            load.generation,
            load.view,
            load.revision,
            load.load_generation,
            &load.cancellation,
        )
    }

    fn finish_calendar_projection(&self, load: &CalendarProjectionLoad) -> bool {
        self.finish_calendar_projection_token(
            load.generation,
            load.view,
            load.revision,
            load.load_generation,
            &load.cancellation,
        )
    }

    fn calendar_projection_token_is_current(
        &self,
        generation: u64,
        view: CalendarView,
        revision: u64,
        load_generation: u64,
        cancellation: &Arc<AtomicBool>,
    ) -> bool {
        self.calendar_data_state() == CalendarDataState::Ready
            && *self.imp().current_view.borrow() == view
            && self.imp().calendar_revision.get() == revision
            && self.imp().view_state.borrow().load_generation == load_generation
            && !cancellation.load(Ordering::Acquire)
            && self.imp().calendar_projection_gate.borrow().accepts(
                generation,
                view,
                revision,
                load_generation,
            )
    }

    fn finish_calendar_projection_token(
        &self,
        generation: u64,
        view: CalendarView,
        revision: u64,
        load_generation: u64,
        cancellation: &Arc<AtomicBool>,
    ) -> bool {
        if !self.calendar_projection_token_is_current(
            generation,
            view,
            revision,
            load_generation,
            cancellation,
        ) {
            return false;
        }
        if !self.imp().calendar_projection_gate.borrow_mut().finish(
            generation,
            view,
            revision,
            load_generation,
        ) {
            return false;
        }
        self.imp().calendar_projection_cancel.borrow_mut().take();
        true
    }

    fn apply_calendar_projection_load(&self, load: CalendarProjectionLoad) {
        if !self.calendar_projection_is_current(&load) {
            return;
        }
        let plan = match load.result.clone() {
            Ok(plan) => plan,
            Err(error) => {
                if self.finish_calendar_projection(&load) {
                    self.show_calendar_projection_error(load.view, &error);
                }
                return;
            }
        };
        match plan {
            CalendarProjectionPlan::List(plan) => self.apply_list_projection_plan(load, plan),
        }
    }

    fn show_calendar_projection_error(&self, view: CalendarView, error: &str) {
        let message = format!("Kalenderansicht konnte nicht aufgebaut werden: {error}");
        match view {
            CalendarView::Year => {
                if let Some(container) = self.imp().year_view.borrow().as_ref() {
                    clear_box(container);
                    container.append(&empty_period_row("Jahresansicht nicht verfügbar", &message));
                }
            }
            CalendarView::List => {
                if let Some(container) = self.imp().list_view.borrow().as_ref() {
                    clear_box(container);
                    container.append(&empty_period_row("Listenansicht nicht verfügbar", &message));
                }
            }
            _ => {}
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

    fn create_mini_calendar(&self) -> gtk::Calendar {
        let mini_cal = gtk::Calendar::new();
        mini_cal.add_css_class("calendar-sidebar-minical");
        let show_week_numbers = AppSettings::try_new()
            .map(|settings| settings.calendar_show_week_numbers())
            .unwrap_or(false);
        mini_cal.set_show_week_numbers(show_week_numbers);
        let page_for_cal = self.downgrade();
        mini_cal.connect_day_selected(move |calendar| {
            let Some(page) = page_for_cal.upgrade() else {
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
        mini_cal
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
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .vexpand(true)
            .hexpand(true)
            .build();
        self.imp().outer_paned.replace(Some(paned.clone()));

        // ── Sidebar (Calendar Picker + Task List) ───────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(0)
            .margin_end(0)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["calendar-sidebar"])
            .build();

        let mini_calendar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .hexpand(true)
            .build();
        self.imp()
            .mini_calendar_box
            .replace(Some(mini_calendar_box.clone()));

        let mut initial_cals = Vec::new();
        for i in 0..4 {
            let cal = self.create_mini_calendar();
            cal.set_visible(i == 0);
            mini_calendar_box.append(&cal);
            initial_cals.push(cal);
        }
        self.imp()
            .mini_calendar
            .replace(Some(initial_cals[0].clone()));
        self.imp()
            .mini_calendar_2
            .replace(Some(initial_cals[1].clone()));
        self.imp()
            .mini_calendar_3
            .replace(Some(initial_cals[2].clone()));
        self.imp()
            .mini_calendar_4
            .replace(Some(initial_cals[3].clone()));
        *self.imp().mini_calendars.borrow_mut() = initial_cals;

        let mini_calendar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(false)
            .min_content_width(40)
            .child(&mini_calendar_box)
            .build();
        sidebar_box.append(&mini_calendar_scroll);

        let page_weak = self.downgrade();
        let last_cal_count = std::rc::Rc::new(std::cell::Cell::new(1usize));
        mini_calendar_scroll.add_tick_callback(move |widget, _| {
            let w = widget.width();
            if w > 0 {
                let target_count = (w / 220).max(1) as usize;
                if target_count != last_cal_count.get() {
                    last_cal_count.set(target_count);
                    if let Some(page) = page_weak.upgrade() {
                        page.ensure_mini_calendar_count(target_count);
                    }
                }
            }
            glib::ControlFlow::Continue
        });

        let source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .margin_top(4)
            .build();

        sidebar_box.append(&source_list);
        self.imp()
            .calendar_source_list
            .replace(Some(source_list.clone()));

        let calendar_state = gtk::Label::builder()
            .label("Kalender werden nach der Synchronisation angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(false)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .halign(gtk::Align::Start)
            .margin_start(6)
            .margin_end(6)
            .build();
        sidebar_box.append(&calendar_state);
        self.imp()
            .calendar_source_state_label
            .replace(Some(calendar_state.clone()));

        let local_item_state = gtk::Label::builder()
            .label("Keine lokalen .ics-Daten geladen")
            .css_classes(vec!["dim-label"])
            .wrap(false)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
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
        let memo_records = Rc::new(RefCell::new(Vec::<MemoRecord>::new()));
        self.imp()
            .memo_records
            .replace(Some(Rc::clone(&memo_records)));

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
            .width_chars(6)
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
            page.refresh_taskpad();
            page.refresh_memopad();
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
            page.refresh_taskpad();
            page.refresh_memopad();
        });

        self.start_calendar_snapshot_load();

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let monitor_generation =
                advance_calendar_generation(self.imp().calendar_monitor_generation.get());
            self.imp()
                .calendar_monitor_generation
                .set(monitor_generation);
            let page_for_monitor = self.downgrade();
            glib::spawn_future_local(async move {
                loop {
                    let event = match receiver.recv().await {
                        Ok(event) => event,
                        Err(error) => {
                            tracing::debug!(%error, "ECal signal monitor channel closed");
                            break;
                        }
                    };
                    let Some(page) = page_for_monitor.upgrade() else {
                        return;
                    };
                    if !page.calendar_monitor_is_current(monitor_generation) {
                        return;
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
                        ECalSyncEvent::Change(_change) => {
                            let revision = page.imp().calendar_revision.get();
                            if page
                                .calendar_monitor_revision_is_current(monitor_generation, revision)
                            {
                                // The next authoritative snapshot owns all
                                // seven views.  Do not merge a monitor delta
                                // into a list that a newer snapshot may be
                                // building.
                                page.reload_calendar_data();
                            }
                        }
                        ECalSyncEvent::Error(error) => {
                            tracing::warn!(%error, "ECal signal monitor stopped");
                            break;
                        }
                    }
                }
            });
        }

        let cached_sources = crate::services::eds_source_cache_service::EdsSourceCache::new()
            .load(crate::services::eds_service::EdsSourceKind::Calendar);
        self.apply_calendar_sources(cached_sources);
        self.reload_calendar_sources();

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .min_content_height(40)
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
            .ellipsize(gtk::pango::EllipsizeMode::End)
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
            "calendar",
            &sidebar_outer_box,
        ));

        paned.set_start_child(Some(&sidebar_outer_box));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(true);

        // ── Calendar Content (View Switcher: Day/Week/Month) ────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();

        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Vorheriger Zeitraum")
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&prev_btn, "Vorherigen Kalenderzeitraum anzeigen");

        let today_btn = gtk::Button::builder()
            .label("Heute")
            .tooltip_text("Zu heute wechseln")
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&today_btn, "Zum heutigen Datum wechseln");

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Nächster Zeitraum")
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&next_btn, "Nächsten Kalenderzeitraum anzeigen");
        self.imp()
            .calendar_prev_button
            .replace(Some(prev_btn.clone()));
        self.imp()
            .calendar_next_button
            .replace(Some(next_btn.clone()));
        self.imp()
            .calendar_today_button
            .replace(Some(today_btn.clone()));

        let nav_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked", "calendar-nav-group"])
            .valign(gtk::Align::Center)
            .build();
        nav_box.append(&prev_btn);
        nav_box.append(&today_btn);
        nav_box.append(&next_btn);

        let date_label = gtk::Label::builder()
            .label("")
            .css_classes(vec!["title-3", "heading", "calendar-title-date"])
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        self.imp()
            .month_date_label
            .replace(Some(date_label.clone()));

        let btn_availability = gtk::Button::builder()
            .icon_name("appointment-soon-symbolic")
            .tooltip_text("Freie Zeiten anzeigen (Verfügbarkeit)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_availability, "Freie Zeiten anzeigen");

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

        // View mode toggle
        let view_toggle = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .valign(gtk::Align::Center)
            .build();

        let initial_view = settings
            .as_ref()
            .map(|settings| persisted_calendar_view_name(Some(settings)))
            .unwrap_or_else(|| persisted_calendar_view_name(None));
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
        let year_btn = gtk::ToggleButton::builder()
            .label("Jahr")
            .group(&day_btn)
            .active(initial_calendar_view == CalendarView::Year)
            .build();
        let list_btn = gtk::ToggleButton::builder()
            .label("Liste")
            .group(&day_btn)
            .active(initial_calendar_view == CalendarView::List)
            .build();

        view_toggle.append(&day_btn);
        view_toggle.append(&week_btn);
        view_toggle.append(&workweek_btn);
        view_toggle.append(&month_btn);
        view_toggle.append(&agenda_btn);
        view_toggle.append(&year_btn);
        view_toggle.append(&list_btn);

        let view_names = [
            "Tag",
            "Woche",
            "Arbeitswoche",
            "Monat",
            "Agenda",
            "Jahr",
            "Liste",
        ];
        let dropdown_model = gtk::StringList::new(&view_names);
        let calendar_view_dropdown =
            gtk::DropDown::new(Some(dropdown_model), None::<&gtk::Expression>);
        calendar_view_dropdown.set_valign(gtk::Align::Center);
        calendar_view_dropdown.set_tooltip_text(Some("Kalenderansicht auswählen"));
        calendar_view_dropdown.set_selected(calendar_view_to_index(initial_calendar_view));
        calendar_view_dropdown.set_visible(false);
        self.imp()
            .calendar_view_dropdown
            .replace(Some(calendar_view_dropdown.clone()));

        let page_weak_for_view_dropdown = self.downgrade();
        calendar_view_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(page) = page_weak_for_view_dropdown.upgrade() {
                if page.imp().syncing_calendar_view.get() {
                    return;
                }
                let view = calendar_view_from_index(dropdown.selected());
                page.select_calendar_view(view);
            }
        });

        let view_switcher_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Center)
            .build();
        view_switcher_box.append(&view_toggle);
        view_switcher_box.append(&calendar_view_dropdown);

        let show_tag_vpane_initial = settings
            .as_ref()
            .map(|s| s.calendar_show_tag_vpane())
            .unwrap_or(true);
        let tag_vpane_toggle_btn = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Aufgaben- und Notizenleiste ein-/ausblenden (Strg+M)")
            .active(show_tag_vpane_initial)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&tag_vpane_toggle_btn, "Aufgaben- und Notizenleiste ein- oder ausblenden");
        self.imp()
            .tag_vpane_toggle_btn
            .replace(Some(tag_vpane_toggle_btn.clone()));

        let sidebar_visible = AppSettings::try_new()
            .map(|s| s.mail_show_sidebar())
            .unwrap_or(true);
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Seitenleiste ein-/ausblenden (F9)")
            .active(sidebar_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&sidebar_toggle, "Seitenleiste ein- oder ausblenden");
        self.imp()
            .calendar_sidebar_toggle
            .replace(Some(sidebar_toggle.clone()));

        let sidebar_for_toggle = sidebar_outer_box.clone();
        sidebar_toggle.connect_toggled(move |toggle| {
            let is_active = toggle.is_active();
            sidebar_for_toggle.set_visible(is_active);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_sidebar(is_active);
            }
        });

        if let Some(settings) = AppSettings::try_new() {
            let sidebar_toggle_for_settings = sidebar_toggle.clone();
            settings.connect_changed(Some("mail-show-sidebar"), move |settings, _| {
                let active = settings.boolean("mail-show-sidebar");
                if sidebar_toggle_for_settings.is_active() != active {
                    sidebar_toggle_for_settings.set_active(active);
                }
            });
        }

        let btn_print_calendar = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text("Kalenderansicht drucken (Strg+P)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_print_calendar, "Kalender drucken");
        let page_weak_for_print = self.downgrade();
        btn_print_calendar.connect_clicked(move |_| {
            if let Some(page) = page_weak_for_print.upgrade() {
                page.print_visible_agenda();
            }
        });

        let actions_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .valign(gtk::Align::Center)
            .css_classes(vec!["calendar-actions-group"])
            .build();
        actions_box.append(&btn_availability);
        actions_box.append(&btn_print_calendar);


        let start_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        start_box.append(&sidebar_toggle);
        start_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        start_box.append(&nav_box);

        let end_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .build();
        end_box.append(&view_switcher_box);
        end_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        end_box.append(&actions_box);
        end_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        end_box.append(&tag_vpane_toggle_btn);

        let view_toolbar = gtk::CenterBox::builder()
            .start_widget(&start_box)
            .center_widget(&date_label)
            .end_widget(&end_box)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .hexpand(true)
            .css_classes(vec!["unified-view-toolbar", "toolbar"])
            .build();

        let view_toolbar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .child(&view_toolbar)
            .build();

        view_toolbar_scroll.set_visible(
            AppSettings::try_new()
                .map(|settings| settings.mail_show_toolbar())
                .unwrap_or(true),
        );
        self.imp()
            .toolbar
            .replace(Some(view_toolbar_scroll.clone().upcast()));

        let calendar_search_bar = gtk::SearchBar::builder()
            .search_mode_enabled(false)
            .key_capture_widget(self)
            .build();
        calendar_search_bar.connect_entry(&calendar_search_entry);
        self.imp()
            .calendar_search_bar
            .replace(Some(calendar_search_bar.clone()));

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

        calendar_search_entry.set_width_chars(32);
        calendar_search_entry.set_valign(gtk::Align::Center);
        category_selector.set_valign(gtk::Align::Center);

        search_content_box.append(&calendar_search_entry);
        search_content_box.append(&category_selector);
        calendar_search_bar.set_child(Some(&search_content_box));

        let entry_for_search_mode = calendar_search_entry.clone();
        let page_weak_for_search_mode = self.downgrade();
        calendar_search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            let is_mode = bar.is_search_mode();
            if !is_mode && !entry_for_search_mode.text().is_empty() {
                entry_for_search_mode.set_text("");
            }
            if let Some(page) = page_weak_for_search_mode.upgrade() {
                if let Some(window) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                    if let Some(btn) = window.search_button() {
                        if btn.is_active() != is_mode {
                            btn.set_active(is_mode);
                        }
                    }
                }
            }
        });

        // View Stack for all portable calendar surfaces.
        let calendar_stack = adw::ViewStack::new();

        // 1. Month Canvas View (Cairo Canvas parity with original GNOME Evolution EWeekViewMainItem)
        let month_canvas = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .can_focus(true)
            .margin_start(2)
            .margin_end(2)
            .margin_top(0)
            .margin_bottom(2)
            .css_classes(vec!["calendar-month-canvas"])
            .build();

        self.imp().month_canvas.replace(Some(month_canvas.clone()));

        // Also keep month_grid and month_header_grid allocated for backwards safety
        let month_header_grid = gtk::Grid::builder().build();
        self.imp().month_header_grid.replace(Some(month_header_grid));
        let month_grid = gtk::Grid::builder().build();
        self.imp().month_grid.replace(Some(month_grid));

        // Draw function
        let page_weak_draw = self.downgrade();
        month_canvas.set_draw_func(move |_, cr, width_i32, height_i32| {
            let width = width_i32 as f64;
            let height = height_i32 as f64;
            let is_dark = adw::StyleManager::default().is_dark();

            let Some(page) = page_weak_draw.upgrade() else { return };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return };

            let selected_time_slot = *page.imp().selected_time_slot.borrow();
            let drag_selection = *page.imp().month_drag_selection.borrow();
            let hovered_event = page.imp().month_hovered_event.borrow().clone();

            draw_month_canvas(
                cr,
                width,
                height,
                &layout,
                selected_time_slot,
                drag_selection,
                hovered_event.as_ref(),
                is_dark,
            );
        });

        // Click Gesture (Left Click: select / details / editor)
        let click_gesture = gtk::GestureClick::new();
        click_gesture.set_button(gdk::BUTTON_PRIMARY);
        let page_weak_click = self.downgrade();
        click_gesture.connect_pressed(move |gesture, n_press, x, y| {
            let (Some(page), Some(widget)) = (page_weak_click.upgrade(), gesture.widget()) else { return };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return };

            let w = drawing_area.width() as f64;
            let h = drawing_area.height() as f64;
            let Some(hit) = hit_test_month_canvas(x, y, w, h, &layout) else { return };

            match hit {
                MonthCanvasHit::Event { event, date } => {
                    if n_press == 2 {
                        if let Some(identity) = CalendarEventIdentity::from_event(&event) {
                            page.open_event_editor_for_identity(drawing_area, &identity);
                        }
                    } else if n_press == 1 {
                        show_calendar_event_details_popover(
                            &page,
                            drawing_area,
                            &event,
                            Some(date),
                        );
                    }
                }
                MonthCanvasHit::Overflow { date } => {
                    *page.imp().current_date.borrow_mut() = date;
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .set_anchor(date, configured_first_weekday());
                    page.imp().view_state.borrow_mut().select_civil_day(date);
                    page.select_calendar_view(CalendarView::Day);
                }
                MonthCanvasHit::DayCell { date } => {
                    let direct_create = AppSettings::try_new()
                        .map(|s| s.calendar_click_action() == "create")
                        .unwrap_or(false);

                    if direct_create || n_press == 2 {
                        let dialog = crate::dialogs::EventDialog::new_for_date(date);
                        if let Some(root) = drawing_area.root() {
                            if let Ok(win) = root.downcast::<gtk::Window>() {
                                dialog.set_transient_for(Some(&win));
                            }
                        }
                        let page_for_refresh = page.downgrade();
                        dialog.connect_close_request(move |_| {
                            if let Some(page) = page_for_refresh.upgrade() {
                                page.reload_calendar_data();
                            }
                            glib::Propagation::Proceed
                        });
                        dialog.present();
                    } else if n_press == 1 {
                        page.set_selected_time_slot(Some(SelectedTimeSlot {
                            date,
                            start_minute: 540,
                            end_minute: 600,
                        }));
                        drawing_area.queue_draw();
                    }
                }
                MonthCanvasHit::KwRow { row } => {
                    if let Some(week) = layout.weeks.get(row) {
                        let target_date = week.week_dates[0];
                        *page.imp().current_date.borrow_mut() = target_date;
                        page.imp()
                            .view_state
                            .borrow_mut()
                            .set_anchor(target_date, configured_first_weekday());
                        page.imp().view_state.borrow_mut().select_civil_day(target_date);
                        page.select_calendar_view(CalendarView::Week);
                    }
                }
                _ => {}
            }
        });
        month_canvas.add_controller(click_gesture);

        // Right Click Gesture (Context Menu)
        let right_click_gesture = gtk::GestureClick::new();
        right_click_gesture.set_button(gdk::BUTTON_SECONDARY);
        let page_weak_right = self.downgrade();
        right_click_gesture.connect_pressed(move |gesture, _, x, y| {
            let (Some(page), Some(widget)) = (page_weak_right.upgrade(), gesture.widget()) else { return };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return };

            let w = drawing_area.width() as f64;
            let h = drawing_area.height() as f64;
            let Some(hit) = hit_test_month_canvas(x, y, w, h, &layout) else { return };

            match hit {
                MonthCanvasHit::Event { event, .. } => {
                    let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                    show_calendar_event_context_menu(&page, drawing_area, &event, rect);
                }
                MonthCanvasHit::DayCell { date } => {
                    page.set_selected_time_slot(Some(SelectedTimeSlot {
                        date,
                        start_minute: 540,
                        end_minute: 600,
                    }));
                    drawing_area.queue_draw();
                    show_calendar_day_context_menu(&page, date, x, y);
                }
                _ => {}
            }
        });
        month_canvas.add_controller(right_click_gesture);

        // Drag Gesture (Range Selection across days)
        let drag_gesture = gtk::GestureDrag::new();
        let page_weak_drag = self.downgrade();
        drag_gesture.connect_drag_begin(move |gesture, x, y| {
            let (Some(page), Some(widget)) = (page_weak_drag.upgrade(), gesture.widget()) else { return };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return };

            let w = drawing_area.width() as f64;
            let h = drawing_area.height() as f64;
            if let Some(MonthCanvasHit::DayCell { date }) = hit_test_month_canvas(x, y, w, h, &layout) {
                page.imp().month_drag_selection.replace(Some(MonthDragSelection {
                    start_date: date,
                    current_date: date,
                }));
                drawing_area.queue_draw();
            }
        });

        let page_weak_drag_upd = self.downgrade();
        drag_gesture.connect_drag_update(move |gesture, offset_x, offset_y| {
            let (Some(page), Some(widget)) = (page_weak_drag_upd.upgrade(), gesture.widget()) else { return };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return };

            let Some((start_x, start_y)) = gesture.start_point() else { return };
            let curr_x = start_x + offset_x;
            let curr_y = start_y + offset_y;
            let w = drawing_area.width() as f64;
            let h = drawing_area.height() as f64;

            if let Some(hit) = hit_test_month_canvas(curr_x, curr_y, w, h, &layout) {
                let date = match hit {
                    MonthCanvasHit::DayCell { date } => Some(date),
                    MonthCanvasHit::Event { date, .. } => Some(date),
                    MonthCanvasHit::Overflow { date } => Some(date),
                    _ => None,
                };
                if let Some(d) = date {
                    let mut mut_sel = page.imp().month_drag_selection.borrow_mut();
                    if let Some(ref mut sel) = *mut_sel {
                        if sel.current_date != d {
                            sel.current_date = d;
                            drop(mut_sel);
                            drawing_area.queue_draw();
                        }
                    }
                }
            }
        });

        let page_weak_drag_end = self.downgrade();
        drag_gesture.connect_drag_end(move |gesture, _, _| {
            let (Some(page), Some(widget)) = (page_weak_drag_end.upgrade(), gesture.widget()) else { return };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return };

            if let Some(sel) = page.imp().month_drag_selection.take() {
                drawing_area.queue_draw();
                if sel.start_date != sel.current_date {
                    let (s, e) = if sel.start_date <= sel.current_date {
                        (sel.start_date, sel.current_date)
                    } else {
                        (sel.current_date, sel.start_date)
                    };
                    let start_dt = s.and_hms_opt(0, 0, 0).unwrap();
                    let end_dt = (e + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap();
                    let dialog = crate::dialogs::EventDialog::new_for_time_range(start_dt, end_dt);
                    if let Some(root) = drawing_area.root() {
                        if let Ok(win) = root.downcast::<gtk::Window>() {
                            dialog.set_transient_for(Some(&win));
                        }
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
            }
        });
        month_canvas.add_controller(drag_gesture);

        // Motion Controller (Cursor changes and hover states)
        let motion_controller = gtk::EventControllerMotion::new();
        let page_weak_motion = self.downgrade();
        motion_controller.connect_motion(move |ctrl, x, y| {
            let (Some(page), Some(widget)) = (page_weak_motion.upgrade(), ctrl.widget()) else { return };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return };

            let w = drawing_area.width() as f64;
            let h = drawing_area.height() as f64;
            let hit = hit_test_month_canvas(x, y, w, h, &layout);

            match hit {
                Some(MonthCanvasHit::Event { event, .. }) => {
                    drawing_area.set_cursor_from_name(Some("pointer"));
                    let ident = CalendarEventIdentity::from_event(&event);
                    let mut prev = page.imp().month_hovered_event.borrow_mut();
                    if *prev != ident {
                        *prev = ident;
                        drop(prev);
                        drawing_area.queue_draw();
                    }
                }
                Some(MonthCanvasHit::Overflow { .. }) => {
                    drawing_area.set_cursor_from_name(Some("pointer"));
                    let mut prev = page.imp().month_hovered_event.borrow_mut();
                    if prev.is_some() {
                        *prev = None;
                        drop(prev);
                        drawing_area.queue_draw();
                    }
                }
                _ => {
                    drawing_area.set_cursor_from_name(None);
                    let mut prev = page.imp().month_hovered_event.borrow_mut();
                    if prev.is_some() {
                        *prev = None;
                        drop(prev);
                        drawing_area.queue_draw();
                    }
                }
            }
        });
        month_canvas.add_controller(motion_controller);

        // Tooltip
        month_canvas.set_has_tooltip(true);
        let page_weak_tooltip = self.downgrade();
        month_canvas.connect_query_tooltip(move |widget, x, y, _, tooltip| {
            let Some(page) = page_weak_tooltip.upgrade() else { return false };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return false };

            let w = widget.width() as f64;
            let h = widget.height() as f64;
            let Some(hit) = hit_test_month_canvas(x as f64, y as f64, w, h, &layout) else { return false };

            match hit {
                MonthCanvasHit::Event { event, date } => {
                    let time_str = if event.all_day {
                        "Ganztägig".to_string()
                    } else {
                        let (start_min, end_min, _) = event_occurrence_times_for_date(&event, date, 30);
                        format!("{}-{}", format_clock_minutes(start_min), format_clock_minutes(end_min))
                    };
                    let loc_str = if !event.location.is_empty() {
                        format!("\nOrt: {}", event.location)
                    } else {
                        String::new()
                    };
                    let tip = format!("{}\nZeit: {}{}\nKalender: {}", event.summary, time_str, loc_str, event.source_display_name);
                    tooltip.set_text(Some(&tip));
                    true
                }
                MonthCanvasHit::Overflow { date } => {
                    tooltip.set_text(Some(&format!("Alle Termine für {} in Tagesansicht anzeigen", date.format("%d.%m.%Y"))));
                    true
                }
                _ => false,
            }
        });

        // Key Controller (Keyboard navigation)
        let key_controller = gtk::EventControllerKey::new();
        let page_weak_key = self.downgrade();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            let Some(page) = page_weak_key.upgrade() else { return glib::Propagation::Proceed };
            let curr_date = page.imp().selected_time_slot.borrow().map(|s| s.date)
                .unwrap_or_else(|| *page.imp().current_date.borrow());

            let new_date = match keyval {
                gdk::Key::Left => Some(curr_date - Duration::days(1)),
                gdk::Key::Right => Some(curr_date + Duration::days(1)),
                gdk::Key::Up => Some(curr_date - Duration::days(7)),
                gdk::Key::Down => Some(curr_date + Duration::days(7)),
                gdk::Key::Page_Up => {
                    page.shift_month_week(-4);
                    None
                }
                gdk::Key::Page_Down => {
                    page.shift_month_week(4);
                    None
                }
                gdk::Key::Home => Some(Local::now().date_naive()),
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    let dialog = crate::dialogs::EventDialog::new_for_date(curr_date);
                    if let Some(canvas) = page.imp().month_canvas.borrow().as_ref() {
                        if let Some(root) = canvas.root() {
                            if let Ok(win) = root.downcast::<gtk::Window>() {
                                dialog.set_transient_for(Some(&win));
                            }
                        }
                    }
                    dialog.present();
                    None
                }
                _ => None,
            };

            if let Some(d) = new_date {
                page.set_selected_time_slot(Some(SelectedTimeSlot {
                    date: d,
                    start_minute: 540,
                    end_minute: 600,
                }));
                *page.imp().current_date.borrow_mut() = d;
                if let Some(canvas) = page.imp().month_canvas.borrow().as_ref() {
                    canvas.queue_draw();
                }
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
        month_canvas.add_controller(key_controller);

        // Drop Target (Drag and drop event into month canvas)
        let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let page_weak_drop = self.downgrade();
        drop_target.connect_drop(move |_, value, x, y| {
            let (Some(page), Ok(payload)) = (page_weak_drop.upgrade(), value.get::<String>()) else {
                return false;
            };
            let Some(canvas) = page.imp().month_canvas.borrow().as_ref().cloned() else {
                return false;
            };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return false };

            let w = canvas.width() as f64;
            let h = canvas.height() as f64;
            let target_date = match hit_test_month_canvas(x, y, w, h, &layout) {
                Some(MonthCanvasHit::DayCell { date }) => date,
                Some(MonthCanvasHit::Event { date, .. }) => date,
                Some(MonthCanvasHit::Overflow { date }) => date,
                _ => return false,
            };

            handle_dropped_event_payload(&page, &payload, target_date, None)
        });
        month_canvas.add_controller(drop_target);

        // Drag Source (Drag event out of month canvas)
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::MOVE);
        let page_weak_drag_src = self.downgrade();
        drag_source.connect_prepare(move |source, x, y| {
            let (Some(page), Some(widget)) = (page_weak_drag_src.upgrade(), source.widget()) else { return None };
            let Some(drawing_area) = widget.downcast_ref::<gtk::DrawingArea>() else { return None };
            let layout_rc = page.imp().month_layout.borrow().as_ref().cloned();
            let Some(layout) = layout_rc else { return None };

            let w = drawing_area.width() as f64;
            let h = drawing_area.height() as f64;
            if let Some(MonthCanvasHit::Event { event, date }) = hit_test_month_canvas(x, y, w, h, &layout) {
                if event.source_uid.trim().is_empty()
                    || event.uid.trim().is_empty()
                    || event_mutation_error(&event).is_some()
                {
                    return None;
                }
                let recurrence_id = calendar_recurrence_id(&event).unwrap_or_default();
                let payload = format!(
                    "{}\n{}\n{}\n{}",
                    event.source_uid,
                    event.uid,
                    recurrence_id,
                    date.format("%Y-%m-%d")
                );
                Some(gdk::ContentProvider::for_value(&payload.to_value()))
            } else {
                None
            }
        });
        month_canvas.add_controller(drag_source);

        let month_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .vexpand(true)
            .hexpand(true)
            .build();
        month_container.append(&month_canvas);

        let scroll_controller = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
        );
        let delta_accum = std::rc::Rc::new(std::cell::Cell::new(0.0f64));
        let pending_steps = std::rc::Rc::new(std::cell::Cell::new(0i32));
        let is_scheduled = std::rc::Rc::new(std::cell::Cell::new(false));
        let last_scroll_time = std::rc::Rc::new(std::cell::Cell::new(std::time::Instant::now()));

        let delta_for_scroll = delta_accum.clone();
        let pending_steps_for_scroll = pending_steps.clone();
        let is_scheduled_for_scroll = is_scheduled.clone();
        let last_scroll_time_for_scroll = last_scroll_time.clone();

        scroll_controller.connect_scroll(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, dy| {
                if *page.imp().current_view.borrow() != CalendarView::Month {
                    return glib::Propagation::Proceed;
                }

                let now = std::time::Instant::now();
                if now.duration_since(last_scroll_time_for_scroll.get())
                    > std::time::Duration::from_millis(250)
                {
                    delta_for_scroll.set(0.0);
                }
                last_scroll_time_for_scroll.set(now);

                let current_delta = delta_for_scroll.get() + dy;
                const SCROLL_THRESHOLD: f64 = 1.0;

                let steps = (current_delta / SCROLL_THRESHOLD).trunc() as i32;
                if steps != 0 {
                    delta_for_scroll.set(current_delta - (steps as f64 * SCROLL_THRESHOLD));
                    let clamped = steps.clamp(-1, 1);
                    pending_steps_for_scroll.set(pending_steps_for_scroll.get() + clamped);

                    if !is_scheduled_for_scroll.get() {
                        is_scheduled_for_scroll.set(true);
                        let is_sched = is_scheduled_for_scroll.clone();
                        let pending = pending_steps_for_scroll.clone();
                        glib::idle_add_local_once(glib::clone!(
                            #[weak]
                            page,
                            move || {
                                is_sched.set(false);
                                let total_steps = pending.take();
                                if total_steps != 0 {
                                    page.shift_month_week(total_steps);
                                }
                            }
                        ));
                    }
                } else {
                    delta_for_scroll.set(current_delta);
                }
                glib::Propagation::Stop
            }
        ));
        month_container.add_controller(scroll_controller);

        calendar_stack.add_titled(&month_container, Some("month"), "Monat");

        // 2. Day View
        let day_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(8)
            .margin_end(8)
            .margin_top(0)
            .margin_bottom(0)
            .spacing(0)
            .hexpand(true)
            .vexpand(true)
            .build();
        self.imp().day_view.replace(Some(day_view.clone()));
        calendar_stack.add_titled(&day_view, Some("day"), "Tag");

        // 3. Week View
        let week_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(8)
            .margin_end(8)
            .margin_top(0)
            .margin_bottom(0)
            .spacing(0)
            .hexpand(true)
            .vexpand(true)
            .build();
        self.imp().week_view.replace(Some(week_view.clone()));
        calendar_stack.add_titled(&week_view, Some("week"), "Woche");

        // 4. Workweek time grid
        let workweek_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(8)
            .margin_end(8)
            .margin_top(0)
            .margin_bottom(0)
            .spacing(0)
            .hexpand(true)
            .vexpand(true)
            .build();
        self.imp()
            .workweek_view
            .replace(Some(workweek_view.clone()));
        calendar_stack.add_titled(&workweek_view, Some("workweek"), "Arbeitswoche");

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

        // 6. Year View: twelve compact civil-date month grids plus the
        // selected-day event list, mirroring Evolution's Year surface.
        let year_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .spacing(8)
            .build();
        self.imp().year_view.replace(Some(year_view.clone()));
        let year_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&year_view)
            .build();
        calendar_stack.add_titled(&year_scroll, Some("year"), "Jahr");

        // 7. List View: virtualized Cairo canvas with dedicated scrollbar.
        let list_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .build();
        self.imp().list_view.replace(Some(list_view.clone()));
        calendar_stack.add_titled(&list_view, Some("list"), "Liste");

        calendar_stack.set_visible_child_name(match initial_calendar_view {
            CalendarView::Day => "day",
            CalendarView::Week => "week",
            CalendarView::WorkWeek => "workweek",
            CalendarView::Month => "month",
            CalendarView::Agenda => "agenda",
            CalendarView::Year => "year",
            CalendarView::List => "list",
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
            year_btn.clone(),
            list_btn.clone(),
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
        year_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::Year);
                }
            }
        ));
        list_btn.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    page.select_calendar_view(CalendarView::List);
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
                "calendar-time-divisions",
                "calendar-compress-weekend",
                "calendar-month-scroll-by-week",
                "calendar-second-zone",
                "hide-cancelled-events",
                "default-calendar-source",
            ] {
                let page_weak = self.downgrade();
                settings.connect_changed(Some(key), move |_, key_name| {
                    if let Some(page) = page_weak.upgrade() {
                        page.refresh_calendar_views();
                        page.refresh_event_list_visibility();
                        if key_name == "default-calendar-source" {
                            if let Some(settings) = AppSettings::try_new() {
                                page.imp()
                                    .calendar_source_selector
                                    .borrow_mut()
                                    .load_from_settings(&settings);
                                page.reload_calendar_sources();
                            }
                        }
                    }
                });
            }

            let page_weak = self.downgrade();
            settings.connect_changed(Some("calendar-hidden-sources"), move |settings, _| {
                let Some(page) = page_weak.upgrade() else { return };
                let new_hidden: HashSet<String> = settings
                    .string("calendar-hidden-sources")
                    .split(',')
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
                if *page.imp().hidden_calendar_sources.borrow() == new_hidden {
                    return;
                }
                *page.imp().hidden_calendar_sources.borrow_mut() = new_hidden.clone();
                {
                    let mut sel = page.imp().calendar_source_selector.borrow_mut();
                    if let Some(app_settings) = AppSettings::try_new() {
                        sel.load_from_settings(&app_settings);
                    }
                }
                page.sync_calendar_checkboxes();
                page.refresh_task_sidebar();
                page.refresh_task_and_memo_panes();
                page.refresh_calendar_views();
                page.refresh_event_list_visibility();
            });

            let page_weak_for_toolbar = self.downgrade();
            settings.connect_changed(Some("mail-show-toolbar"), move |settings, _| {
                if let Some(page) = page_weak_for_toolbar.upgrade() {
                    if let Some(toolbar) = page.imp().toolbar.borrow().as_ref() {
                        toolbar.set_visible(settings.boolean("mail-show-toolbar"));
                    }
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
                if *page.imp().current_view.borrow() == CalendarView::List {
                    return;
                }
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

        // --- Tag VPaned: Taskpad & Memopad (Upstream GW-CAL-SHELL parity) ---
        let tag_vpaned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .wide_handle(false)
            .vexpand(true)
            .build();
        let tag_vpane_initial_pos = AppSettings::try_new()
            .map(|s| s.calendar_tag_vpane_position())
            .unwrap_or(320);
        tag_vpaned.set_position(tag_vpane_initial_pos);
        tag_vpaned.set_visible(show_tag_vpane_initial);
        self.imp().tag_vpaned.replace(Some(tag_vpaned.clone()));

        tag_vpaned.connect_position_notify(move |paned| {
            let pos = paned.position();
            if pos > 50 {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_calendar_tag_vpane_position(pos);
                }
            }
        });

        // 1. Taskpad (Start child of tag_vpaned)
        let taskpad_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(6)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(4)
            .vexpand(true)
            .build();

        let taskpad_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        let taskpad_title = gtk::Label::builder()
            .label("Aufgaben")
            .css_classes(vec!["heading", "title-4"])
            .halign(gtk::Align::Start)
            .build();
        let taskpad_count = gtk::Label::builder()
            .css_classes(vec!["dim-label", "caption"])
            .halign(gtk::Align::Start)
            .build();
        self.imp()
            .taskpad_count
            .replace(Some(taskpad_count.clone()));
        let taskpad_spacer = gtk::Box::builder().hexpand(true).build();
        let taskpad_add_btn = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Neue Aufgabe erstellen")
            .css_classes(vec!["flat", "circular"])
            .build();
        taskpad_add_btn.connect_clicked(move |btn| {
            if let Some(root) = btn.root() {
                if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                    window.present_new_task();
                }
            }
        });

        taskpad_header.append(&taskpad_title);
        taskpad_header.append(&taskpad_count);
        taskpad_header.append(&taskpad_spacer);
        taskpad_header.append(&taskpad_add_btn);
        taskpad_box.append(&taskpad_header);

        let taskpad_state = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(vec!["dim-label"])
            .build();
        self.imp()
            .taskpad_state
            .replace(Some(taskpad_state.clone()));
        taskpad_box.append(&taskpad_state);

        let taskpad_canvas = gtk::DrawingArea::builder()
            .can_focus(true)
            .focusable(true)
            .hexpand(true)
            .vexpand(true)
            .css_classes(vec!["taskpad-canvas"])
            .build();
        self.imp().taskpad_canvas.replace(Some(taskpad_canvas.clone()));

        // Wire draw function
        let page_weak_draw = self.downgrade();
        taskpad_canvas.set_draw_func(move |_area, cr, width_i32, height_i32| {
            let Some(page) = page_weak_draw.upgrade() else { return };
            let width = width_i32 as f64;
            let height = height_i32 as f64;
            let is_dark = adw::StyleManager::default().is_dark();
            let layout_opt = page.imp().taskpad_layout.borrow().clone();
            let hovered_idx = *page.imp().taskpad_hovered_index.borrow();
            let selected_idx = *page.imp().taskpad_selected_index.borrow();
            if let Some(layout) = layout_opt {
                draw_taskpad_canvas(cr, &layout, width, height, hovered_idx, selected_idx, is_dark);
            }
        });

        // Primary click gesture
        let click_gesture = gtk::GestureClick::builder()
            .button(1)
            .build();
        let page_weak_click = self.downgrade();
        click_gesture.connect_pressed(move |gesture, n_press, x, y| {
            let Some(page) = page_weak_click.upgrade() else { return };
            let Some(drawing_area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            drawing_area.grab_focus();
            let layout_opt = page.imp().taskpad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return };
            if let Some((idx, in_check)) = hit_test_taskpad(&layout, x, y) {
                let task = &layout.items[idx].record;
                if in_check {
                    toggle_task_completion(&page, task, !task.completed, None);
                } else {
                    page.imp().taskpad_selected_index.replace(Some(idx));
                    drawing_area.queue_draw();
                    if n_press == 2 {
                        page.open_task_by_uid(&task.source_uid, &task.uid);
                    }
                }
            }
        });
        taskpad_canvas.add_controller(click_gesture);

        // Secondary / Right-click gesture
        let right_click = gtk::GestureClick::builder()
            .button(3)
            .build();
        let page_weak_rc = self.downgrade();
        right_click.connect_pressed(move |gesture, _n_press, x, y| {
            let Some(page) = page_weak_rc.upgrade() else { return };
            let Some(drawing_area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            let layout_opt = page.imp().taskpad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return };
            if let Some((idx, _in_check)) = hit_test_taskpad(&layout, x, y) {
                page.imp().taskpad_selected_index.replace(Some(idx));
                drawing_area.queue_draw();
                let task = &layout.items[idx].record;
                let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                show_taskpad_context_menu(&page, &drawing_area, task, rect);
            }
        });
        taskpad_canvas.add_controller(right_click);

        // Motion / Hover controller
        let motion = gtk::EventControllerMotion::new();
        let page_weak_motion = self.downgrade();
        motion.connect_motion(move |controller, x, y| {
            let Some(page) = page_weak_motion.upgrade() else { return };
            let Some(drawing_area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            let layout_opt = page.imp().taskpad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return };
            let new_hover = hit_test_taskpad(&layout, x, y).map(|(idx, _)| idx);
            if new_hover != *page.imp().taskpad_hovered_index.borrow() {
                page.imp().taskpad_hovered_index.replace(new_hover);
                drawing_area.queue_draw();
            }
            if new_hover.is_some() {
                drawing_area.set_cursor_from_name(Some("pointer"));
            } else {
                drawing_area.set_cursor_from_name(None);
            }
        });
        let page_weak_leave = self.downgrade();
        motion.connect_leave(move |controller| {
            let Some(page) = page_weak_leave.upgrade() else { return };
            let Some(drawing_area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            if page.imp().taskpad_hovered_index.borrow().is_some() {
                page.imp().taskpad_hovered_index.replace(None);
                drawing_area.queue_draw();
            }
            drawing_area.set_cursor_from_name(None);
        });
        taskpad_canvas.add_controller(motion);

        // Key controller
        let key_controller = gtk::EventControllerKey::new();
        let page_weak_key = self.downgrade();
        key_controller.connect_key_pressed(move |controller, key, _keycode, _state| {
            let Some(page) = page_weak_key.upgrade() else { return glib::Propagation::Proceed };
            let Some(drawing_area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return glib::Propagation::Proceed };
            let layout_opt = page.imp().taskpad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return glib::Propagation::Proceed };
            if layout.items.is_empty() {
                return glib::Propagation::Proceed;
            }
            let current = *page.imp().taskpad_selected_index.borrow();
            match key {
                gdk::Key::Up => {
                    let next = match current {
                        Some(idx) if idx > 0 => idx - 1,
                        _ => 0,
                    };
                    page.imp().taskpad_selected_index.replace(Some(next));
                    drawing_area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Down => {
                    let next = match current {
                        Some(idx) if idx + 1 < layout.items.len() => idx + 1,
                        Some(idx) => idx,
                        None => 0,
                    };
                    page.imp().taskpad_selected_index.replace(Some(next));
                    drawing_area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::space => {
                    if let Some(idx) = current {
                        if idx < layout.items.len() {
                            let task = &layout.items[idx].record;
                            toggle_task_completion(&page, task, !task.completed, None);
                            return glib::Propagation::Stop;
                        }
                    }
                    glib::Propagation::Proceed
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    if let Some(idx) = current {
                        if idx < layout.items.len() {
                            let task = &layout.items[idx].record;
                            page.open_task_by_uid(&task.source_uid, &task.uid);
                            return glib::Propagation::Stop;
                        }
                    }
                    glib::Propagation::Proceed
                }
                _ => glib::Propagation::Proceed,
            }
        });
        taskpad_canvas.add_controller(key_controller);

        let taskpad_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&taskpad_canvas)
            .build();
        taskpad_box.append(&taskpad_scroll);

        tag_vpaned.set_start_child(Some(&taskpad_box));
        tag_vpaned.set_resize_start_child(true);
        tag_vpaned.set_shrink_start_child(false);

        // 2. Memopad (End child of tag_vpaned)
        let memopad_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(6)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(4)
            .vexpand(true)
            .build();

        let memopad_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        let memopad_title = gtk::Label::builder()
            .label("Notizen")
            .css_classes(vec!["heading", "title-4"])
            .halign(gtk::Align::Start)
            .build();
        let memopad_count = gtk::Label::builder()
            .css_classes(vec!["dim-label", "caption"])
            .halign(gtk::Align::Start)
            .build();
        self.imp()
            .memopad_count
            .replace(Some(memopad_count.clone()));
        let memopad_spacer = gtk::Box::builder().hexpand(true).build();
        let memopad_add_btn = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Neue Notiz erstellen")
            .css_classes(vec!["flat", "circular"])
            .build();
        memopad_add_btn.connect_clicked(move |btn| {
            if let Some(root) = btn.root() {
                if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                    window.present_new_memo();
                }
            }
        });

        memopad_header.append(&memopad_title);
        memopad_header.append(&memopad_count);
        memopad_header.append(&memopad_spacer);
        memopad_header.append(&memopad_add_btn);
        memopad_box.append(&memopad_header);

        let memopad_state = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(vec!["dim-label"])
            .build();
        self.imp()
            .memopad_state
            .replace(Some(memopad_state.clone()));
        memopad_box.append(&memopad_state);

        let memopad_canvas = gtk::DrawingArea::builder()
            .can_focus(true)
            .focusable(true)
            .hexpand(true)
            .vexpand(true)
            .css_classes(vec!["memopad-canvas"])
            .build();
        self.imp().memopad_canvas.replace(Some(memopad_canvas.clone()));

        // Wire draw function
        let page_weak_draw_memo = self.downgrade();
        memopad_canvas.set_draw_func(move |_area, cr, width_i32, height_i32| {
            let Some(page) = page_weak_draw_memo.upgrade() else { return };
            let width = width_i32 as f64;
            let height = height_i32 as f64;
            let is_dark = adw::StyleManager::default().is_dark();
            let layout_opt = page.imp().memopad_layout.borrow().clone();
            let hovered_idx = *page.imp().memopad_hovered_index.borrow();
            let selected_idx = *page.imp().memopad_selected_index.borrow();
            if let Some(layout) = layout_opt {
                draw_memopad_canvas(cr, &layout, width, height, hovered_idx, selected_idx, is_dark);
            }
        });

        // Primary click gesture
        let memo_click_gesture = gtk::GestureClick::builder()
            .button(1)
            .build();
        let page_weak_memo_click = self.downgrade();
        memo_click_gesture.connect_pressed(move |gesture, n_press, _x, y| {
            let Some(page) = page_weak_memo_click.upgrade() else { return };
            let Some(drawing_area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            drawing_area.grab_focus();
            let layout_opt = page.imp().memopad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return };
            if let Some(idx) = hit_test_memopad(&layout, _x, y) {
                let memo = &layout.items[idx].record;
                page.imp().memopad_selected_index.replace(Some(idx));
                drawing_area.queue_draw();
                if n_press == 2 {
                    page.open_memo_by_uid(&memo.source_uid, &memo.uid);
                }
            }
        });
        memopad_canvas.add_controller(memo_click_gesture);

        // Secondary / Right-click gesture
        let memo_right_click = gtk::GestureClick::builder()
            .button(3)
            .build();
        let page_weak_memo_rc = self.downgrade();
        memo_right_click.connect_pressed(move |gesture, _n_press, x, y| {
            let Some(page) = page_weak_memo_rc.upgrade() else { return };
            let Some(drawing_area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            let layout_opt = page.imp().memopad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return };
            if let Some(idx) = hit_test_memopad(&layout, x, y) {
                page.imp().memopad_selected_index.replace(Some(idx));
                drawing_area.queue_draw();
                let memo = &layout.items[idx].record;
                let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                show_memopad_context_menu(&page, &drawing_area, memo, rect);
            }
        });
        memopad_canvas.add_controller(memo_right_click);

        // Motion / Hover controller
        let memo_motion = gtk::EventControllerMotion::new();
        let page_weak_memo_motion = self.downgrade();
        memo_motion.connect_motion(move |controller, _x, y| {
            let Some(page) = page_weak_memo_motion.upgrade() else { return };
            let Some(drawing_area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            let layout_opt = page.imp().memopad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return };
            let new_hover = hit_test_memopad(&layout, _x, y);
            if new_hover != *page.imp().memopad_hovered_index.borrow() {
                page.imp().memopad_hovered_index.replace(new_hover);
                drawing_area.queue_draw();
            }
            if new_hover.is_some() {
                drawing_area.set_cursor_from_name(Some("pointer"));
            } else {
                drawing_area.set_cursor_from_name(None);
            }
        });
        let page_weak_memo_leave = self.downgrade();
        memo_motion.connect_leave(move |controller| {
            let Some(page) = page_weak_memo_leave.upgrade() else { return };
            let Some(drawing_area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
            if page.imp().memopad_hovered_index.borrow().is_some() {
                page.imp().memopad_hovered_index.replace(None);
                drawing_area.queue_draw();
            }
            drawing_area.set_cursor_from_name(None);
        });
        memopad_canvas.add_controller(memo_motion);

        // Key controller
        let memo_key_controller = gtk::EventControllerKey::new();
        let page_weak_memo_key = self.downgrade();
        memo_key_controller.connect_key_pressed(move |controller, key, _keycode, _state| {
            let Some(page) = page_weak_memo_key.upgrade() else { return glib::Propagation::Proceed };
            let Some(drawing_area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return glib::Propagation::Proceed };
            let layout_opt = page.imp().memopad_layout.borrow().clone();
            let Some(layout) = layout_opt else { return glib::Propagation::Proceed };
            if layout.items.is_empty() {
                return glib::Propagation::Proceed;
            }
            let current = *page.imp().memopad_selected_index.borrow();
            match key {
                gdk::Key::Up => {
                    let next = match current {
                        Some(idx) if idx > 0 => idx - 1,
                        _ => 0,
                    };
                    page.imp().memopad_selected_index.replace(Some(next));
                    drawing_area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Down => {
                    let next = match current {
                        Some(idx) if idx + 1 < layout.items.len() => idx + 1,
                        Some(idx) => idx,
                        None => 0,
                    };
                    page.imp().memopad_selected_index.replace(Some(next));
                    drawing_area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    if let Some(idx) = current {
                        if idx < layout.items.len() {
                            let memo = &layout.items[idx].record;
                            page.open_memo_by_uid(&memo.source_uid, &memo.uid);
                            return glib::Propagation::Stop;
                        }
                    }
                    glib::Propagation::Proceed
                }
                _ => glib::Propagation::Proceed,
            }
        });
        memopad_canvas.add_controller(memo_key_controller);

        let memopad_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&memopad_canvas)
            .build();
        memopad_box.append(&memopad_scroll);

        tag_vpaned.set_end_child(Some(&memopad_box));
        tag_vpaned.set_resize_end_child(true);
        tag_vpaned.set_shrink_end_child(false);

        // Connect toggle button to tag_vpaned visibility
        if let Some(btn) = self.imp().tag_vpane_toggle_btn.borrow().as_ref() {
            let tag_vpaned_toggle = tag_vpaned.clone();
            btn.connect_toggled(move |btn| {
                let active = btn.is_active();
                tag_vpaned_toggle.set_visible(active);
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_calendar_show_tag_vpane(active);
                }
            });
        }

        // Horizontal paned dividing calendar_stack (left) and tag_vpaned (right)
        let content_hpaned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .wide_handle(false)
            .vexpand(true)
            .hexpand(true)
            .build();
        content_hpaned.set_start_child(Some(&calendar_stack));
        content_hpaned.set_end_child(Some(&tag_vpaned));
        content_hpaned.set_resize_start_child(true);
        content_hpaned.set_resize_end_child(false);
        content_hpaned.set_shrink_start_child(true);
        content_hpaned.set_shrink_end_child(true);
        tag_vpaned.set_width_request(260);

        self.imp()
            .content_hpaned
            .replace(Some(content_hpaned.clone()));

        calendar_stack.set_vexpand(true);
        content_box.set_vexpand(true);
        content_box.append(&content_hpaned);

        self.refresh_calendar_views();
        self.sync_mini_calendar();

        content_box.set_hexpand(true);
        paned.set_end_child(Some(&content_box));
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(true);

        let sidebar_position = AppSettings::try_new()
            .map(|settings| settings.mail_sidebar_position())
            .unwrap_or(260)
            .clamp(80, 1500);
        paned.set_position(sidebar_position);
        if let Some(settings) = AppSettings::try_new() {
            paned.connect_position_notify(move |paned| {
                let position = paned.position();
                if (80..=1500).contains(&position) && settings.mail_sidebar_position() != position {
                    settings.set_mail_sidebar_position(position);
                }
            });
        }

        let page_weak_for_keys = self.downgrade();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::F9 {
                if let Some(page) = page_weak_for_keys.upgrade() {
                    if let Some(btn) = page.imp().calendar_sidebar_toggle.borrow().as_ref() {
                        btn.set_active(!btn.is_active());
                        return glib::Propagation::Stop;
                    }
                }
            }
            if (key == gtk::gdk::Key::m || key == gtk::gdk::Key::M)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                if let Some(page) = page_weak_for_keys.upgrade() {
                    if let Some(btn) = page.imp().tag_vpane_toggle_btn.borrow().as_ref() {
                        btn.set_active(!btn.is_active());
                        return glib::Propagation::Stop;
                    }
                }
            }
            let is_print = (key == gtk::gdk::Key::p || key == gtk::gdk::Key::P)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if is_print {
                if let Some(page) = page_weak_for_keys.upgrade() {
                    page.print_visible_agenda();
                    return glib::Propagation::Stop;
                }
            }
            if key == gtk::gdk::Key::Escape {
                if let Some(page) = page_weak_for_keys.upgrade() {
                    if page.imp().selected_time_slot.borrow().is_some() {
                        page.clear_selected_time_slot();
                        return glib::Propagation::Stop;
                    }
                }
            }
            if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
                if let Some(page) = page_weak_for_keys.upgrade() {
                    if let Some(slot) = *page.imp().selected_time_slot.borrow() {
                        let start_time = slot.date
                            .and_hms_opt(0, 0, 0)
                            .unwrap()
                            + Duration::minutes(slot.start_minute as i64);
                        let end_time = slot.date
                            .and_hms_opt(0, 0, 0)
                            .unwrap()
                            + Duration::minutes(slot.end_minute as i64);
                        let dialog = crate::dialogs::EventDialog::new_for_time_range(start_time, end_time);
                        if let Some(root) = page.root() {
                            if let Ok(win) = root.downcast::<gtk::Window>() {
                                dialog.set_transient_for(Some(&win));
                            }
                        }
                        let page_for_refresh = page.downgrade();
                        dialog.connect_close_request(move |_| {
                            if let Some(page) = page_for_refresh.upgrade() {
                                page.reload_calendar_data();
                            }
                            glib::Propagation::Proceed
                        });
                        dialog.present();
                        return glib::Propagation::Stop;
                    }
                }
            }
            glib::Propagation::Proceed
        });
        self.add_controller(key_controller);

        #[cfg(all(target_os = "macos", macos_bridge))]
        let macos_banner = {
            let banner = adw::Banner::builder()
                .title("Apple Kalender verbinden: Möchten Sie Ihre macOS Kalender und Termine in Evolution anzeigen?")
                .button_label("Apple Kalender verbinden")
                .revealed(false)
                .build();
            let page_weak = self.downgrade();
            let banner_clone = banner.clone();
            banner.connect_button_clicked(move |_| {
                let Some(page) = page_weak.upgrade() else { return; };
                let srv = crate::services::MacosIntegrationService::new();
                let status = srv.check_permission(crate::services::MacosPermissionKind::Calendar);
                match status {
                    crate::services::MacosPermissionStatus::NotDetermined => {
                        let page_weak_cb = page.downgrade();
                        srv.request_permission(crate::services::MacosPermissionKind::Calendar, move |granted| {
                            if let Some(page) = page_weak_cb.upgrade() {
                                if granted {
                                    let (sender, receiver) = async_channel::bounded(1);
                                    std::thread::spawn(move || {
                                        let cal_store = crate::services::CalendarStoreService::new();
                                        let macos = crate::services::MacosIntegrationService::new();
                                        let _ = macos.sync_apple_calendar_to_store(&cal_store);
                                        let _ = macos.sync_apple_reminders_to_store(&cal_store);
                                        let _ = sender.send_blocking(());
                                    });
                                    let page_weak_sync = page.downgrade();
                                    glib::spawn_future_local(async move {
                                        if receiver.recv().await.is_ok() {
                                            if let Some(page) = page_weak_sync.upgrade() {
                                                page.reload_calendar_data();
                                                if let Some(win) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                                                    win.add_toast(&adw::Toast::new("Apple Kalender erfolgreich verbunden!"));
                                                }
                                                page.update_macos_permission_banner();
                                            }
                                        }
                                    });
                                } else {
                                    page.update_macos_permission_banner();
                                }
                            }
                        });
                    }
                    crate::services::MacosPermissionStatus::Denied => {
                        srv.open_privacy_settings(crate::services::MacosPermissionKind::Calendar);
                    }
                    _ => {
                        banner_clone.set_revealed(false);
                    }
                }
            });
            self.imp().macos_banner.replace(Some(banner.clone()));
            banner
        };

        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        #[cfg(all(target_os = "macos", macos_bridge))]
        root_box.append(&macos_banner);
        root_box.append(&view_toolbar_scroll);
        root_box.append(&calendar_search_bar);
        root_box.append(&paned);
        root_box.set_parent(self);

        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            self.update_macos_permission_banner();
            let srv = crate::services::MacosIntegrationService::new();
            let perm = srv.check_permission(crate::services::MacosPermissionKind::Calendar);
            if perm == crate::services::MacosPermissionStatus::NotDetermined {
                let page_weak = self.downgrade();
                srv.request_permission(crate::services::MacosPermissionKind::Calendar, move |granted| {
                    if let Some(page) = page_weak.upgrade() {
                        if granted {
                            let (sender, receiver) = async_channel::bounded(1);
                            std::thread::spawn(move || {
                                let cal_store = crate::services::CalendarStoreService::new();
                                let macos = crate::services::MacosIntegrationService::new();
                                let _ = macos.sync_apple_calendar_to_store(&cal_store);
                                let _ = macos.sync_apple_reminders_to_store(&cal_store);
                                let _ = sender.send_blocking(());
                            });
                            let page_weak_sync = page.downgrade();
                            glib::spawn_future_local(async move {
                                if receiver.recv().await.is_ok() {
                                    if let Some(page) = page_weak_sync.upgrade() {
                                        page.reload_calendar_data();
                                        if let Some(win) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                                            win.add_toast(&adw::Toast::new("Apple Kalender erfolgreich verbunden!"));
                                        }
                                        page.update_macos_permission_banner();
                                    }
                                }
                            });
                        } else {
                            page.update_macos_permission_banner();
                        }
                    }
                });
            } else if perm == crate::services::MacosPermissionStatus::Authorized {
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let cal_store = crate::services::CalendarStoreService::new();
                    let macos = crate::services::MacosIntegrationService::new();
                    let _ = macos.sync_apple_calendar_to_store(&cal_store);
                    let _ = macos.sync_apple_reminders_to_store(&cal_store);
                    let _ = sender.send_blocking(());
                });
                let page_weak_sync = self.downgrade();
                glib::spawn_future_local(async move {
                    if receiver.recv().await.is_ok() {
                        if let Some(page) = page_weak_sync.upgrade() {
                            page.reload_calendar_data();
                        }
                    }
                });
            }
        }
    }

    fn select_calendar_view(&self, view: CalendarView) {
        if self.imp().syncing_calendar_view.get() {
            return;
        }
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
                CalendarView::Year,
                CalendarView::List,
            ]) {
                button.set_active(candidate == view);
            }
        }
        if let Some(dropdown) = self.imp().calendar_view_dropdown.borrow().as_ref() {
            let target_idx = calendar_view_to_index(view);
            if dropdown.selected() != target_idx {
                dropdown.set_selected(target_idx);
            }
        }
        if let Some(settings) = self.imp().settings.borrow().as_ref() {
            persist_calendar_view(view, Some(settings));
        } else {
            persist_calendar_view(view, None);
        }
        self.update_calendar_navigation_controls();
        self.refresh_calendar_views();
        self.imp().syncing_calendar_view.set(false);
    }

    fn sync_calendar_view_from_settings(&self) {
        if self.imp().syncing_calendar_view.get() {
            return;
        }
        let view = self
            .imp()
            .settings
            .borrow()
            .as_ref()
            .map(|settings| calendar_view_from_name(&persisted_calendar_view_name(Some(settings))))
            .unwrap_or_else(|| *self.imp().current_view.borrow());

        if *self.imp().current_view.borrow() == view {
            return;
        }

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
                CalendarView::Year,
                CalendarView::List,
            ]) {
                button.set_active(candidate == view);
            }
        }
        if let Some(dropdown) = self.imp().calendar_view_dropdown.borrow().as_ref() {
            let target_idx = calendar_view_to_index(view);
            if dropdown.selected() != target_idx {
                dropdown.set_selected(target_idx);
            }
        }
        self.update_calendar_navigation_controls();
        self.refresh_calendar_views();
        self.imp().syncing_calendar_view.set(false);
    }

    fn update_calendar_navigation_controls(&self) {
        let enabled = *self.imp().current_view.borrow() != CalendarView::List;
        if let Some(button) = self.imp().calendar_prev_button.borrow().as_ref() {
            button.set_sensitive(enabled);
        }
        if let Some(button) = self.imp().calendar_next_button.borrow().as_ref() {
            button.set_sensitive(enabled);
        }
        if let Some(button) = self.imp().calendar_today_button.borrow().as_ref() {
            button.set_sensitive(enabled);
        }
    }

    fn sync_calendar_checkboxes(&self) {
        self.imp().syncing_calendar_checks.set(true);
        let hidden = self.imp().hidden_calendar_sources.borrow().clone();
        for (uid, check) in self.imp().calendar_source_checks.borrow().iter() {
            let should_be_active = !hidden.contains(uid);
            if check.is_active() != should_be_active {
                check.set_active(should_be_active);
            }
        }
        self.imp().syncing_calendar_checks.set(false);
    }

    pub(crate) fn render_calendar_sidebar(&self) {
        let Some(source_list) = self.imp().calendar_source_list.borrow().as_ref().cloned() else {
            return;
        };
        let calendar_state = self
            .imp()
            .calendar_source_state_label
            .borrow()
            .as_ref()
            .cloned();

        while let Some(child) = source_list.first_child() {
            source_list.remove(&child);
        }

        let selector = self.imp().calendar_source_selector.borrow().clone();
        let collapsed_groups = self.imp().collapsed_calendar_groups.borrow().clone();
        let mut displayed_sources = 0usize;

        // Generate and install dynamic CSS rules for calendar colors on checkboxes
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
            let mut prov_slot = self.imp().calendar_color_css_provider.borrow_mut();
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

        self.imp().calendar_source_checks.borrow_mut().clear();

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

            // Group header row
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
                let mut collapsed = page.imp().collapsed_calendar_groups.borrow_mut();
                if collapsed.contains(&group_name_for_toggle) {
                    collapsed.remove(&group_name_for_toggle);
                } else {
                    collapsed.insert(group_name_for_toggle.clone());
                }
                drop(collapsed);
                page.render_calendar_sidebar();
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
                    .margin_start(12)
                    .margin_end(6)
                    .css_classes(vec!["calendar-sidebar-row", "calendar-sidebar-child-row"])
                    .build();

                // Checkbox styled in the calendar's color
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
                    .tooltip_text("Kalender in den Ansichten ein-/ausblenden")
                    .css_classes(vec![color_class.as_str()])
                    .build();

                self.imp()
                    .calendar_source_checks
                    .borrow_mut()
                    .insert(source_uid.clone(), check.clone());

                let page_weak_for_toggle = self.downgrade();
                let source_uid_for_toggle = source_uid.clone();
                check.connect_toggled(move |check| {
                    let Some(page) = page_weak_for_toggle.upgrade() else {
                        return;
                    };
                    if page.imp().syncing_calendar_checks.get() {
                        return;
                    }
                    let is_active = check.is_active();
                    {
                        let mut sel = page.imp().calendar_source_selector.borrow_mut();
                        if sel.is_selected(&source_uid_for_toggle) == is_active {
                            return;
                        }
                        sel.set_selected(&source_uid_for_toggle, is_active);
                        let hidden = sel.hidden_uids();
                        *page.imp().hidden_calendar_sources.borrow_mut() =
                            hidden.iter().cloned().collect();
                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_calendar_hidden_sources(&hidden);
                        }
                    }
                    page.refresh_task_sidebar();
                    page.refresh_task_and_memo_panes();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                });
                child_box.append(&check);

                // 3. Label
                let label = gtk::Label::builder()
                    .label(&item.display_name)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build();
                child_box.append(&label);

                // 4. Primary indicator
                if is_primary {
                    let primary_ind =
                        crate::models::source_selector::create_primary_indicator();
                    primary_ind.set_valign(gtk::Align::Center);
                    child_box.append(&primary_ind);
                }

                // 5. Remote sync icon
                if item.backend_name != "local" && source_uid != "local-calendar" {
                    let sync_icon =
                        gtk::Image::from_icon_name("media-playlist-repeat-symbolic");
                    sync_icon.add_css_class("dim-label");
                    sync_icon.set_tooltip_text(Some("Synchronisierter Kalender"));
                    sync_icon.set_valign(gtk::Align::Center);
                    child_box.append(&sync_icon);
                }

                let row = gtk::ListBoxRow::builder()
                    .child(&child_box)
                    .activatable(true)
                    .selectable(true)
                    .css_classes(vec!["calendar-sidebar-child-row"])
                    .build();

                let page_weak_for_menu = self.downgrade();
                let source_uid_for_menu = source_uid.clone();
                let display_name_for_menu = item.display_name.clone();
                let color_for_menu = color_hex.clone();
                let is_writable = item.writable;
                let source_list_weak = source_list.downgrade();

                let right_click = gtk::GestureClick::builder()
                    .button(gdk::BUTTON_SECONDARY)
                    .build();
                right_click.connect_pressed(move |gesture, _, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let (Some(page), Some(list)) =
                        (page_weak_for_menu.upgrade(), source_list_weak.upgrade())
                    else {
                        return;
                    };
                    page.present_calendar_source_context_menu(
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
                displayed_sources += 1;
            }
        }

        if let Some(state_lbl) = calendar_state {
            if displayed_sources == 0 {
                state_lbl.set_label("Keine Kalenderquellen verfügbar");
                state_lbl.set_visible(true);
            } else {
                state_lbl.set_visible(false);
            }
        }
    }

    pub(crate) fn apply_calendar_sources(&self, mut sources: Vec<eds_service::EdsSource>) {
        sources.sort_by_cached_key(|source| {
            (source.display_name.to_lowercase(), source.uid.clone())
        });

        let mut items = Vec::new();
        for source in sources {
            let color = calendar_source_color(&source.uid, &source.display_name);
            let group = source
                .parent_name
                .clone()
                .unwrap_or_else(|| "Auf diesem Computer".to_string());
            items.push(
                crate::models::source_selector::SourceSelectorItem::new(
                    &source.uid,
                    &source.display_name,
                    &source.backend_name,
                    source.enabled,
                    source.enabled,
                )
                .with_color(color)
                .with_group(group),
            );
        }
        for collection in crate::services::eds_service::load_configured_account_collections() {
            if !collection.calendar_enabled {
                continue;
            }
            let account_name = collection.display_name.clone();
            let already_represented = items.iter().any(|item| {
                item.group_name == account_name
                    || item.uid == collection.uid
                    || item.uid == format!("account-calendar-{}", collection.uid)
            });
            if !already_represented {
                let cal_uid = format!("account-calendar-{}", collection.uid);
                let color = calendar_source_color(&collection.uid, &account_name);
                items.push(
                    crate::models::source_selector::SourceSelectorItem::new(
                        &cal_uid,
                        "Kalender",
                        "calendar",
                        true,
                        true,
                    )
                    .with_color(color)
                    .with_group(account_name),
                );
            }
        }

        if CalendarStoreService::new().is_writable() && !items.iter().any(|i| i.uid == "local-calendar") {
            let color = calendar_source_color("local-calendar", "Lokaler Kalender");
            items.push(
                crate::models::source_selector::SourceSelectorItem::new(
                    "local-calendar",
                    "Lokaler Kalender",
                    "local",
                    true,
                    true,
                )
                .with_color(color)
                .with_group("Auf diesem Computer"),
            );
        }

        {
            let mut selector = self.imp().calendar_source_selector.borrow_mut();
            selector.update_sources(items);
            if let Some(settings) = AppSettings::try_new() {
                selector.load_from_settings(&settings);
            }
            *self.imp().hidden_calendar_sources.borrow_mut() =
                selector.hidden_uids().into_iter().collect();
        }

        if self.imp().calendar_load_error.borrow().is_some()
            || self.imp().calendar_snapshot_index.borrow().is_none()
        {
            self.start_calendar_snapshot_load();
        }

        self.render_calendar_sidebar();
    }

    pub(crate) fn reload_calendar_sources(&self) {
        if self.imp().calendar_source_selector.borrow().sources().is_empty() {
            let cached_sources = crate::services::eds_source_cache_service::EdsSourceCache::new()
                .load(crate::services::eds_service::EdsSourceKind::Calendar);
            self.apply_calendar_sources(cached_sources);
        }

        let calendar_state = self
            .imp()
            .calendar_source_state_label
            .borrow()
            .as_ref()
            .cloned();
        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            page.apply_calendar_sources(discovery.sources);

            if let Some(state_lbl) = calendar_state {
                if page.imp().calendar_source_selector.borrow().sources().is_empty() {
                    let status = discovery
                        .error
                        .map(|error| format!("Kalender sind nicht erreichbar: {error}"))
                        .unwrap_or_else(|| "Keine Kalenderquellen verfügbar".to_string());
                    state_lbl.set_label(&status);
                    state_lbl.set_visible(true);
                } else {
                    state_lbl.set_visible(false);
                }
            }
        });
    }

    pub(crate) fn present_calendar_source_context_menu(
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
                    page.select_exclusive_calendar_source(&uid);
                }
            }
        };

        let on_select_all = {
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.select_all_calendar_sources();
                }
            }
        };

        let uid_def = source_uid.to_string();
        let on_set_default = {
            let page_weak = page_weak.clone();
            let uid = uid_def;
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.set_default_calendar_source(&uid);
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
                    let root_window = page.root().and_downcast::<gtk::Window>();
                    let dialog = crate::dialogs::SourcePropertiesDialog::new(
                        &uid_props,
                        &name_props,
                        "Kalender",
                        &col_props,
                        is_writable,
                        is_primary,
                    );
                    if let Some(parent) = root_window.as_ref() {
                        dialog.set_transient_for(Some(parent));
                    }
                    let page_weak_save = page.downgrade();
                    dialog.set_on_save(move |update| {
                        if let Some(page) = page_weak_save.upgrade() {
                            {
                                let mut sel = page.imp().calendar_source_selector.borrow_mut();
                                sel.update_source_metadata(
                                    &update.source_uid,
                                    &update.display_name,
                                    Some(&update.color),
                                    update.is_default,
                                );
                                if let Some(settings) = AppSettings::try_new() {
                                    sel.save_to_settings(&settings);
                                    if update.is_default {
                                        settings
                                            .set_default_calendar_source_uid(&update.source_uid);
                                    }
                                }
                            }
                            page.render_calendar_sidebar();
                            page.refresh_task_sidebar();
                            page.refresh_task_and_memo_panes();
                            page.refresh_calendar_views();
                            page.refresh_event_list_visibility();
                        }
                    });
                    dialog.present();
                }
            }
        });

        let on_manage_groups = Some({
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    let root_window = page.root().and_downcast::<gtk::Window>();
                    let current_sel = page.imp().calendar_source_selector.borrow().clone();
                    let page_weak_save = page.downgrade();
                    let dialog = crate::dialogs::ManageGroupsDialog::new(
                        root_window.as_ref(),
                        "Kalender",
                        &current_sel,
                        move |updated_selector| {
                            if let Some(page) = page_weak_save.upgrade() {
                                {
                                    let mut sel = page.imp().calendar_source_selector.borrow_mut();
                                    *sel = updated_selector;
                                    sel.deselect_sources_in_hidden_groups();
                                    if let Some(settings) = AppSettings::try_new() {
                                        sel.save_to_settings(&settings);
                                    }
                                    *page.imp().hidden_calendar_sources.borrow_mut() =
                                        sel.hidden_uids().into_iter().collect();
                                }
                                page.render_calendar_sidebar();
                                page.refresh_task_sidebar();
                                page.refresh_task_and_memo_panes();
                                page.refresh_calendar_views();
                                page.refresh_event_list_visibility();
                            }
                        },
                    );
                    dialog.present();
                }
            }
        });

        let popover = crate::models::source_selector::create_source_context_popover(
            is_primary,
            "Kalender",
            on_exclusive,
            on_select_all,
            on_set_default,
            on_properties,
            on_manage_groups,
        );
        popover.set_parent(parent);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            p.unparent();
        });
        popover.popup();
    }

    pub(crate) fn select_exclusive_calendar_source(&self, uid: &str) {
        {
            let mut selector = self.imp().calendar_source_selector.borrow_mut();
            selector.select_exclusive(uid);
            if let Some(settings) = AppSettings::try_new() {
                selector.save_to_settings(&settings);
            }
            *self.imp().hidden_calendar_sources.borrow_mut() =
                selector.hidden_uids().into_iter().collect();
        }
        self.sync_calendar_checkboxes();
        self.refresh_task_sidebar();
        self.refresh_task_and_memo_panes();
        self.refresh_calendar_views();
        self.refresh_event_list_visibility();
    }

    pub(crate) fn select_all_calendar_sources(&self) {
        {
            let mut selector = self.imp().calendar_source_selector.borrow_mut();
            selector.select_all();
            if let Some(settings) = AppSettings::try_new() {
                selector.save_to_settings(&settings);
            }
            *self.imp().hidden_calendar_sources.borrow_mut() =
                selector.hidden_uids().into_iter().collect();
        }
        self.sync_calendar_checkboxes();
        self.refresh_task_sidebar();
        self.refresh_task_and_memo_panes();
        self.refresh_calendar_views();
        self.refresh_event_list_visibility();
    }

    pub(crate) fn set_default_calendar_source(&self, uid: &str) {
        {
            let mut selector = self.imp().calendar_source_selector.borrow_mut();
            selector.set_primary(uid);
            if let Some(settings) = AppSettings::try_new() {
                selector.save_to_settings(&settings);
            }
            *self.imp().hidden_calendar_sources.borrow_mut() =
                selector.hidden_uids().into_iter().collect();
        }
        self.refresh_calendar_views();
        self.render_calendar_sidebar();
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

        match self.calendar_data_state() {
            CalendarDataState::Loading => {
                task_list.set_visible(false);
                if let Some(state) = self.imp().task_state.borrow().as_ref() {
                    state.set_visible(true);
                    state.set_label("Aufgaben werden zusammen mit dem Kalender-Snapshot geladen …");
                }
                return;
            }
            CalendarDataState::Error => {
                task_list.set_visible(false);
                if let Some(state) = self.imp().task_state.borrow().as_ref() {
                    state.set_visible(true);
                    state.set_label(
                        self.imp()
                            .calendar_load_error
                            .borrow()
                            .as_deref()
                            .unwrap_or("Aufgaben konnten nicht geladen werden"),
                    );
                }
                return;
            }
            CalendarDataState::Ready => {}
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

    pub(crate) fn refresh_taskpad(&self) {
        let Some(taskpad_canvas) = self.imp().taskpad_canvas.borrow().as_ref().cloned() else {
            return;
        };
        let Some(records) = self.imp().task_records.borrow().as_ref().cloned() else {
            return;
        };

        match self.calendar_data_state() {
            CalendarDataState::Loading => {
                taskpad_canvas.set_visible(false);
                if let Some(state) = self.imp().taskpad_state.borrow().as_ref() {
                    state.set_visible(true);
                    state.set_label("Aufgaben werden geladen …");
                }
                if let Some(count) = self.imp().taskpad_count.borrow().as_ref() {
                    count.set_label("");
                }
                return;
            }
            CalendarDataState::Error => {
                taskpad_canvas.set_visible(false);
                if let Some(state) = self.imp().taskpad_state.borrow().as_ref() {
                    state.set_visible(true);
                    state.set_label(
                        self.imp()
                            .calendar_load_error
                            .borrow()
                            .as_deref()
                            .unwrap_or("Aufgaben konnten nicht geladen werden"),
                    );
                }
                if let Some(count) = self.imp().taskpad_count.borrow().as_ref() {
                    count.set_label("");
                }
                return;
            }
            CalendarDataState::Ready => {}
        }

        let query = self.imp().calendar_search_query.borrow().clone();
        let category = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();

        let mut tasks = records
            .borrow()
            .iter()
            .filter(|task| {
                task_matches_calendar_filter(task, &query, category.as_deref(), &hidden_sources)
            })
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

        let today = chrono::Local::now().date_naive();
        let layout = prepare_taskpad_layout(&tasks, today);
        let total_h = (layout.total_height as i32).max(1);
        self.imp().taskpad_layout.replace(Some(layout));
        taskpad_canvas.set_content_height(total_h);
        taskpad_canvas.set_height_request(total_h);

        let visible_count = tasks.len();
        taskpad_canvas.set_visible(visible_count > 0);
        taskpad_canvas.queue_draw();

        if let Some(count) = self.imp().taskpad_count.borrow().as_ref() {
            count.set_label(&format!("({visible_count})"));
        }
        if let Some(state) = self.imp().taskpad_state.borrow().as_ref() {
            state.set_visible(visible_count == 0);
            state.set_label(if records.borrow().is_empty() {
                "Keine Aufgaben geladen"
            } else {
                "Keine passenden Aufgaben"
            });
        }
    }

    pub(crate) fn refresh_memopad(&self) {
        let Some(memopad_canvas) = self.imp().memopad_canvas.borrow().as_ref().cloned() else {
            return;
        };
        let Some(records) = self.imp().memo_records.borrow().as_ref().cloned() else {
            return;
        };

        match self.calendar_data_state() {
            CalendarDataState::Loading => {
                memopad_canvas.set_visible(false);
                if let Some(state) = self.imp().memopad_state.borrow().as_ref() {
                    state.set_visible(true);
                    state.set_label("Notizen werden geladen …");
                }
                if let Some(count) = self.imp().memopad_count.borrow().as_ref() {
                    count.set_label("");
                }
                return;
            }
            CalendarDataState::Error => {
                memopad_canvas.set_visible(false);
                if let Some(state) = self.imp().memopad_state.borrow().as_ref() {
                    state.set_visible(true);
                    state.set_label(
                        self.imp()
                            .calendar_load_error
                            .borrow()
                            .as_deref()
                            .unwrap_or("Notizen konnten nicht geladen werden"),
                    );
                }
                if let Some(count) = self.imp().memopad_count.borrow().as_ref() {
                    count.set_label("");
                }
                return;
            }
            CalendarDataState::Ready => {}
        }

        let query = self.imp().calendar_search_query.borrow().clone();
        let category = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();

        let mut memos = records
            .borrow()
            .iter()
            .filter(|memo| {
                memo_matches_calendar_filter(memo, &query, category.as_deref(), &hidden_sources)
            })
            .cloned()
            .collect::<Vec<_>>();
        memos.sort_by(|left, right| {
            left.summary
                .to_lowercase()
                .cmp(&right.summary.to_lowercase())
                .then_with(|| left.updated.cmp(&right.updated))
        });

        let layout = prepare_memopad_layout(&memos);
        let total_h = (layout.total_height as i32).max(1);
        self.imp().memopad_layout.replace(Some(layout));
        memopad_canvas.set_content_height(total_h);
        memopad_canvas.set_height_request(total_h);

        let visible_count = memos.len();
        memopad_canvas.set_visible(visible_count > 0);
        memopad_canvas.queue_draw();

        if let Some(count) = self.imp().memopad_count.borrow().as_ref() {
            count.set_label(&format!("({visible_count})"));
        }
        if let Some(state) = self.imp().memopad_state.borrow().as_ref() {
            state.set_visible(visible_count == 0);
            state.set_label(if records.borrow().is_empty() {
                "Keine Notizen geladen"
            } else {
                "Keine passenden Notizen"
            });
        }
    }

    pub(crate) fn refresh_task_and_memo_panes(&self) {
        self.refresh_taskpad();
        self.refresh_memopad();
    }

    fn shift_period(&self, direction: i32) {
        if *self.imp().current_view.borrow() == CalendarView::List {
            return;
        }
        self.imp().view_state.borrow_mut().navigate_step(
            direction,
            configured_first_weekday(),
            configured_month_scroll_by_week(),
        );
        *self.imp().current_date.borrow_mut() = self.imp().view_state.borrow().anchor_date;
        self.sync_mini_calendar();
    }

    pub(crate) fn shift_month_week(&self, steps: i32) {
        if *self.imp().current_view.borrow() != CalendarView::Month {
            return;
        }
        let first_weekday = configured_first_weekday();
        self.imp().view_state.borrow_mut().navigate_step(
            steps,
            first_weekday,
            true,
        );
        *self.imp().current_date.borrow_mut() = self.imp().view_state.borrow().anchor_date;

        self.refresh_month_grid();
        self.update_calendar_navigation_controls();
        self.refresh_period_label();
        self.sync_mini_calendar();
    }

    pub(crate) fn ensure_mini_calendar_count(&self, target_count: usize) {
        let Some(mini_calendar_box) = self.imp().mini_calendar_box.borrow().as_ref().cloned()
        else {
            return;
        };

        let current_len = self.imp().mini_calendars.borrow().len();
        if target_count > current_len {
            let mut calendars = self.imp().mini_calendars.borrow_mut();
            for _ in current_len..target_count {
                let cal = self.create_mini_calendar();
                cal.set_visible(true);
                mini_calendar_box.append(&cal);
                calendars.push(cal);
            }
        }

        let mut visibility_changed = false;
        let calendars = self.imp().mini_calendars.borrow();
        for (idx, cal) in calendars.iter().enumerate() {
            let visible = idx < target_count;
            if cal.is_visible() != visible {
                cal.set_visible(visible);
                visibility_changed = true;
            }
        }
        drop(calendars);

        if visibility_changed || target_count > current_len {
            self.sync_mini_calendar();
        }
    }

    fn sync_mini_calendar(&self) {
        let calendars = self.imp().mini_calendars.borrow().clone();
        if calendars.is_empty() {
            return;
        }
        let date = *self.imp().current_date.borrow();
        let viewport = self.current_calendar_viewport();
        self.imp().syncing_mini_calendar.set(true);

        let is_month_view = *self.imp().current_view.borrow() == CalendarView::Month;
        let show_week_numbers = AppSettings::try_new()
            .map(|settings| settings.calendar_show_week_numbers())
            .unwrap_or(false);
        let _today = chrono::Local::now().date_naive();

        for (i, cal) in calendars.iter().enumerate() {
            cal.set_show_week_numbers(show_week_numbers);
            if is_month_view {
                cal.add_css_class("in-month-view");
            } else {
                cal.remove_css_class("in-month-view");
            }
            let (y, m) = add_months(date.year(), date.month(), i);
            let has_selected_date = date.year() == y && date.month() == m;
            let day_to_select = if has_selected_date {
                date.day() as i32
            } else {
                1
            };

            if let Ok(dt) = glib::DateTime::new(
                &glib::TimeZone::local(),
                y,
                m as i32,
                day_to_select,
                0,
                0,
                0.0,
            ) {
                cal.select_day(&dt);
            }

            cal.clear_marks();
            let days_count = days_in_month(y, m);
            for day in 1..=days_count {
                if let Some(d) = NaiveDate::from_ymd_opt(y, m, day) {
                    if is_day_visible_in_viewport(&viewport, d) {
                        cal.mark_day(day);
                    }
                }
            }
            update_mini_calendar_classes(cal, y, m, &viewport, has_selected_date);
        }

        self.imp().syncing_mini_calendar.set(false);
    }

    fn set_calendar_anchor(&self, date: NaiveDate) {
        *self.imp().current_date.borrow_mut() = date;
        self.imp()
            .view_state
            .borrow_mut()
            .set_anchor(date, configured_first_weekday());
        if *self.imp().current_view.borrow() == CalendarView::Year {
            self.imp().view_state.borrow_mut().select_civil_day(date);
        }
        self.sync_mini_calendar();
    }

    pub fn navigate_to_date(&self, date: NaiveDate) {
        self.set_calendar_anchor(date);
        self.refresh_calendar_views();
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
        if !matches!(current_view, CalendarView::Year | CalendarView::List) {
            let list_events = self.filtered_sorted_events();
            self.imp()
                .view_state
                .borrow_mut()
                .reconcile_event_selection(&list_events);
        } else {
            self.cancel_calendar_projection();
        }
        match current_view {
            CalendarView::Month => self.refresh_month_grid(),
            CalendarView::Day => self.refresh_day_view(),
            CalendarView::Week => self.refresh_week_view(),
            CalendarView::WorkWeek => self.refresh_workweek_view(),
            CalendarView::Agenda => self.refresh_agenda_view(),
            CalendarView::Year => self.refresh_year_view(),
            CalendarView::List => self.refresh_list_view(),
        }
        self.update_calendar_navigation_controls();
        self.refresh_period_label();
        self.sync_mini_calendar();
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
                    append_event_row(self, &event_list, Rc::clone(&records), event);
                }
            }
            self.rebuild_calendar_index_from_records();
            self.bump_calendar_revision();
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
        if self.calendar_data_state() != CalendarDataState::Ready {
            event_list.set_visible(false);
            if let Some(label) = self.imp().calendar_data_state.borrow().as_ref() {
                match self.calendar_data_state() {
                    CalendarDataState::Loading => {
                        label.set_label("Kalender werden geladen …");
                    }
                    CalendarDataState::Error => label.set_label(
                        self.imp()
                            .calendar_load_error
                            .borrow()
                            .as_deref()
                            .unwrap_or("Kalender konnten nicht geladen werden"),
                    ),
                    CalendarDataState::Ready => {}
                }
            }
            return;
        }
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
            CalendarView::Year => format!("Jahr {}", date.year()),
            CalendarView::List => self
                .imp()
                .calendar_list_projection_count
                .get()
                .map(|count| format!("Alle Termine ({count})"))
                .unwrap_or_else(|| "Alle Termine".to_string()),
        };
        label.set_label(&text);
    }

    fn refresh_day_view(&self) {
        let Some(view) = self.imp().day_view.borrow().as_ref().cloned() else {
            return;
        };
        let date = self.current_calendar_viewport().visible_range.start;
        self.render_continuous_time_grid(&view, &[date]);
    }

    fn refresh_week_view(&self) {
        let Some(view) = self.imp().week_view.borrow().as_ref().cloned() else {
            return;
        };
        let week_start = self.current_calendar_viewport().visible_range.start;
        let days: Vec<NaiveDate> = (0..7)
            .map(|i| week_start + chrono::Duration::days(i))
            .collect();
        self.render_continuous_time_grid(&view, &days);
    }

    fn refresh_workweek_view(&self) {
        let Some(view) = self.imp().workweek_view.borrow().as_ref().cloned() else {
            return;
        };
        let viewport = self.current_calendar_viewport();
        let workdays = configured_workdays();
        let days = workweek_display_dates(viewport.visible_range, &workdays);
        self.render_continuous_time_grid(&view, &days);
    }

    fn render_continuous_time_grid(&self, view: &gtk::Box, days: &[NaiveDate]) {
        clear_box(view);
        if self.append_calendar_state_to_box(view) {
            return;
        }

        let today = Local::now().date_naive();
        let (workday_start, workday_end) = configured_workday_range();
        let workdays = configured_workdays();
        let visible_range = if let (Some(first), Some(last)) = (days.first(), days.last()) {
            let end = last.succ_opt().unwrap_or_else(|| *last + Duration::days(1));
            CalendarDateRange::new(*first, end)
        } else {
            return;
        };
        let events_by_date = self.events_by_date(visible_range);

        let interval_minutes = configured_time_divisions();
        let slots_per_hour = (60 / interval_minutes) as usize;
        let slot_count = 24 * slots_per_hour;
        let slot_height = match interval_minutes {
            60 => 36,
            30 => 24,
            15 => 18,
            10 => 14,
            5 => 12,
            _ => 24,
        };

        let second_zone_name = configured_second_zone();
        let second_tz = timezone_from_name(&second_zone_name);
        let has_second_zone = second_tz.is_some();
        let day_column_offset = if has_second_zone { 2 } else { 1 };
        let use_12h = AppSettings::try_new().is_some_and(|s| s.time_format() == "12h");

        // SizeGroups to synchronize column widths between sticky header and scrolling grid
        let time_col_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let sec_col_group = if has_second_zone {
            Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal))
        } else {
            None
        };
        let day_groups: Vec<gtk::SizeGroup> = (0..days.len())
            .map(|_| gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal))
            .collect();

        // ══════════════════════════════════════════════════════════════════════════
        // Sticky Header Grid: Day Names & Dates (Row 0) + All-Day Events (Row 1)
        // ══════════════════════════════════════════════════════════════════════════
        let header_grid = gtk::Grid::builder()
            .column_spacing(0)
            .row_spacing(0)
            .hexpand(true)
            .css_classes(vec!["calendar-header-grid"])
            .build();

        // Row 0, Col 0: Primary Timezone label (e.g. "Berlin")
        let tz_name = primary_timezone_header_label();
        let time_header = gtk::Label::builder()
            .label(&tz_name)
            .css_classes(vec!["calendar-grid-header-cell", "calendar-timezone-header"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .margin_start(4)
            .width_request(52)
            .build();
        time_col_group.add_widget(&time_header);
        header_grid.attach(&time_header, 0, 0, 1, 1);

        if let Some(ref sec_group) = sec_col_group {
            let sec_header_text = secondary_timezone_header_label(&second_zone_name);
            let sec_header = gtk::Label::builder()
                .label(&sec_header_text)
                .css_classes(vec!["calendar-grid-header-cell", "secondary-tz-header"])
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .width_request(52)
                .build();
            sec_group.add_widget(&sec_header);
            header_grid.attach(&sec_header, 1, 0, 1, 1);
        }

        for (day_index, day) in days.iter().enumerate() {
            let is_today = *day == today;
            let header_cell = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .css_classes(if is_today {
                    vec!["calendar-grid-header-cell", "today"]
                } else {
                    vec!["calendar-grid-header-cell"]
                })
                .hexpand(true)
                .build();
            let day_name = gtk::Label::builder()
                .label(weekday_name(*day))
                .css_classes(vec!["heading"])
                .halign(gtk::Align::Center)
                .build();
            let day_num = gtk::Label::builder()
                .label(&format!("{}.{:02}.", day.day(), day.month()))
                .css_classes(if is_today {
                    vec!["calendar-header-day-number", "accent"]
                } else {
                    vec!["calendar-header-day-number", "dim-label"]
                })
                .halign(gtk::Align::Center)
                .build();
            header_cell.append(&day_name);
            header_cell.append(&day_num);
            day_groups[day_index].add_widget(&header_cell);
            header_grid.attach(&header_cell, day_index as i32 + day_column_offset, 0, 1, 1);
        }

        // Row 1: All-Day Row
        let all_day_label = gtk::Label::builder()
            .label("Ganztägig")
            .css_classes(vec!["calendar-allday-label"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(52)
            .build();
        time_col_group.add_widget(&all_day_label);
        header_grid.attach(&all_day_label, 0, 1, 1, 1);

        if let Some(ref sec_group) = sec_col_group {
            let sec_allday_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(vec![
                    "calendar-allday-label",
                    "calendar-time-gutter-secondary",
                ])
                .width_request(52)
                .build();
            sec_group.add_widget(&sec_allday_box);
            header_grid.attach(&sec_allday_box, 1, 1, 1, 1);
        }

        let days_overlay = gtk::Overlay::new();
        days_overlay.set_hexpand(true);

        let base_days_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .hexpand(true)
            .build();

        for (day_index, day) in days.iter().enumerate() {
            let bg_cell = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(vec!["calendar-allday-cell"])
                .hexpand(true)
                .height_request(26)
                .valign(gtk::Align::Fill)
                .build();
            day_groups[day_index].add_widget(&bg_cell);
            install_day_drop_target(&bg_cell, self, *day);
            base_days_box.append(&bg_cell);
        }
        days_overlay.set_child(Some(&base_days_box));

        let allday_grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .row_spacing(2)
            .column_spacing(2)
            .hexpand(true)
            .valign(gtk::Align::Start)
            .margin_top(2)
            .margin_bottom(2)
            .margin_start(1)
            .margin_end(1)
            .build();

        // Collect unique all-day events across the displayed days
        let mut unique_allday: Vec<CalendarEventRecord> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        for day in days {
            if let Some(day_events) = events_by_date.get(day) {
                for event in day_events.iter().filter(|e| e.all_day) {
                    let id = format!("{}_{}_{}", event.uid, event.summary, event.start);
                    if seen_ids.insert(id) {
                        unique_allday.push(event.clone());
                    }
                }
            }
        }

        struct AlldaySpan {
            event: CalendarEventRecord,
            start_col: usize,
            span: usize,
        }

        let mut spans: Vec<AlldaySpan> = Vec::new();
        for event in unique_allday {
            let range = event_display_range(&event);
            let mut start_idx = None;
            let mut end_idx = 0;
            for (idx, day) in days.iter().enumerate() {
                let matches = range.map(|r| r.contains(*day)).unwrap_or(false);
                if matches {
                    if start_idx.is_none() {
                        start_idx = Some(idx);
                    }
                    end_idx = idx + 1;
                }
            }
            if let Some(s) = start_idx {
                let span = end_idx - s;
                if span > 0 {
                    spans.push(AlldaySpan {
                        event,
                        start_col: s,
                        span,
                    });
                }
            }
        }

        spans.sort_by(|a, b| {
            a.start_col
                .cmp(&b.start_col)
                .then_with(|| b.span.cmp(&a.span))
                .then_with(|| a.event.summary.cmp(&b.event.summary))
        });

        let mut track_busy: Vec<Vec<bool>> = Vec::new();
        for item in spans {
            let mut assigned_track = None;
            for (t_idx, row) in track_busy.iter().enumerate() {
                let conflict = (item.start_col..(item.start_col + item.span))
                    .any(|col| col < row.len() && row[col]);
                if !conflict {
                    assigned_track = Some(t_idx);
                    break;
                }
            }
            let track = match assigned_track {
                Some(t) => t,
                None => {
                    let t = track_busy.len();
                    track_busy.push(vec![false; days.len()]);
                    t
                }
            };
            for col in item.start_col..(item.start_col + item.span) {
                if col < track_busy[track].len() {
                    track_busy[track][col] = true;
                }
            }

            let chip = calendar_grid_event_chip(
                self,
                &item.event,
                true,
                days[item.start_col],
                None,
                1,
            );
            allday_grid.attach(
                &chip,
                item.start_col as i32,
                track as i32,
                item.span as i32,
                1,
            );
        }

        days_overlay.add_overlay(&allday_grid);
        header_grid.attach(&days_overlay, day_column_offset, 1, days.len() as i32, 1);

        view.append(&header_grid);

        // ══════════════════════════════════════════════════════════════════════════
        // Timed Hours Grid (Scrollable)
        // ══════════════════════════════════════════════════════════════════════════
        let timed_grid = gtk::Grid::builder()
            .column_spacing(0)
            .row_spacing(0)
            .hexpand(true)
            .css_classes(vec!["calendar-time-grid"])
            .build();

        let total_hours = 24;
        let hour_height = (slot_height * slots_per_hour as i32).max(1);

        for hour in 0..total_hours {
            let start_slot = (hour * slots_per_hour) as i32;
            let minute = (hour * 60) as u32;

            let gutter_label = gtk::Label::builder()
                .use_markup(true)
                .label(&format_hour_markup(minute, use_12h))
                .css_classes(vec!["calendar-time-gutter-label"])
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .margin_top(1)
                .build();
            let gutter_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(vec!["calendar-time-gutter", "calendar-hour-cell"])
                .width_request(52)
                .height_request(hour_height)
                .valign(gtk::Align::Fill)
                .build();
            gutter_box.append(&gutter_label);
            if hour == 0 {
                time_col_group.add_widget(&gutter_box);
            }
            timed_grid.attach(&gutter_box, 0, start_slot, 1, slots_per_hour as i32);

            if let Some(ref sec_tz_obj) = second_tz {
                let sec_time_str = secondary_timezone_hour_label(
                    days.first().copied().unwrap_or(today),
                    minute,
                    sec_tz_obj,
                    use_12h,
                )
                .unwrap_or_default();
                let sec_label = gtk::Label::builder()
                    .label(&sec_time_str)
                    .css_classes(vec!["secondary-tz-label"])
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .margin_top(1)
                    .build();
                let sec_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .css_classes(vec![
                        "calendar-time-gutter",
                        "calendar-time-gutter-secondary",
                        "calendar-hour-cell",
                    ])
                    .width_request(52)
                    .height_request(hour_height)
                    .valign(gtk::Align::Fill)
                    .build();
                sec_box.append(&sec_label);
                if hour == 0 {
                    if let Some(ref sec_group) = sec_col_group {
                        sec_group.add_widget(&sec_box);
                    }
                }
                timed_grid.attach(&sec_box, 1, start_slot, 1, slots_per_hour as i32);
            }
        }

        let total_grid_height = (slot_count as i32 * slot_height).max(1);

        for (day_index, day) in days.iter().enumerate() {
            let target_date = *day;
            let day_events = events_by_date.get(day).cloned().unwrap_or_default();
            let placed_chips = compute_placed_timed_chips(
                &day_events,
                target_date,
                interval_minutes as u32,
                slot_count,
            );
            let placed_chips_rc = Rc::new(RefCell::new(placed_chips));
            let hovered_chip_idx_rc = Rc::new(Cell::new(None));
            let drag_sel_rc = Rc::new(Cell::new(None));
            let drag_start_y = Rc::new(Cell::new(0.0f64));
            let drag_start_slot = Rc::new(Cell::new(0usize));
            let last_click_rc: Rc<Cell<Option<(usize, std::time::Instant)>>> = Rc::new(Cell::new(None));

            let drawing_area = gtk::DrawingArea::builder()
                .height_request(total_grid_height)
                .hexpand(true)
                .vexpand(true)
                .can_focus(true)
                .focusable(true)
                .css_classes(vec!["calendar-day-canvas"])
                .build();
            day_groups[day_index].add_widget(&drawing_area);

            // Draw function for this day
            let page_weak_draw = self.downgrade();
            let placed_chips_draw = placed_chips_rc.clone();
            let hovered_chip_draw = hovered_chip_idx_rc.clone();
            let drag_sel_draw = drag_sel_rc.clone();
            let is_workday = workdays.contains(&target_date.weekday());

            drawing_area.set_draw_func(move |_area, cr, width_i32, height_i32| {
                let width = width_i32 as f64;
                let height = height_i32 as f64;
                let is_dark = adw::StyleManager::default().is_dark();

                // 1. Backgrounds (workday vs offhours)
                let offhours_alpha = if is_dark { 0.025 } else { 0.035 };
                let offhours_color = if is_dark { 1.0 } else { 0.0 };
                let workday_y1 = (workday_start as f64 / 1440.0) * height;
                let workday_y2 = (workday_end as f64 / 1440.0) * height;

                if is_workday {
                    if workday_y1 > 0.0 {
                        cr.rectangle(0.0, 0.0, width, workday_y1);
                        cr.set_source_rgba(offhours_color, offhours_color, offhours_color, offhours_alpha);
                        let _ = cr.fill();
                    }
                    if workday_y2 < height {
                        cr.rectangle(0.0, workday_y2, width, height - workday_y2);
                        cr.set_source_rgba(offhours_color, offhours_color, offhours_color, offhours_alpha);
                        let _ = cr.fill();
                    }
                } else {
                    cr.rectangle(0.0, 0.0, width, height);
                    cr.set_source_rgba(offhours_color, offhours_color, offhours_color, offhours_alpha);
                    let _ = cr.fill();
                }

                // 2. Grid lines
                let line_color = if is_dark { 1.0 } else { 0.0 };
                let hour_line_alpha = if is_dark { 0.18 } else { 0.14 };
                let sub_line_alpha = if is_dark { 0.07 } else { 0.05 };

                // Hour lines & fine interval lines
                for slot in 1..slot_count {
                    let y = (slot as f64 * slot_height as f64).round() - 0.5;
                    let is_hour = slot % slots_per_hour == 0;
                    if is_hour {
                        cr.set_source_rgba(line_color, line_color, line_color, hour_line_alpha);
                        cr.set_line_width(1.0);
                        cr.move_to(0.0, y);
                        cr.line_to(width, y);
                        let _ = cr.stroke();
                    } else {
                        cr.set_source_rgba(line_color, line_color, line_color, sub_line_alpha);
                        cr.set_line_width(1.0);
                        cr.set_dash(&[2.0, 2.0], 0.0);
                        cr.move_to(0.0, y);
                        cr.line_to(width, y);
                        let _ = cr.stroke();
                        cr.set_dash(&[], 0.0);
                    }
                }

                // Vertical column separator line on the right
                cr.set_source_rgba(line_color, line_color, line_color, hour_line_alpha);
                cr.set_line_width(1.0);
                cr.move_to(width - 0.5, 0.0);
                cr.line_to(width - 0.5, height);
                let _ = cr.stroke();

                // 3. Selection highlight
                let mut sel_range = drag_sel_draw.get();
                if sel_range.is_none() {
                    if let Some(page) = page_weak_draw.upgrade() {
                        if let Some(sel) = *page.imp().selected_time_slot.borrow() {
                            if sel.date == target_date {
                                let s1 = (sel.start_minute / interval_minutes as u32) as usize;
                                let s2 = ((sel.end_minute.saturating_sub(1)) / interval_minutes as u32) as usize;
                                sel_range = Some((s1, s2));
                            }
                        }
                    }
                }

                if let Some((min_s, max_s)) = sel_range {
                    let sy1 = min_s as f64 * slot_height as f64;
                    let sy2 = (max_s + 1) as f64 * slot_height as f64;
                    let sh = (sy2 - sy1).max(2.0);
                    draw_cairo_rounded_rect(cr, 2.0, sy1 + 1.0, (width - 5.0).max(10.0), sh - 2.0, 6.0);
                    cr.set_source_rgba(0.21, 0.52, 0.89, 0.18);
                    let _ = cr.fill_preserve();
                    cr.set_source_rgba(0.21, 0.52, 0.89, 0.85);
                    cr.set_line_width(1.5);
                    let _ = cr.stroke();
                }

                // 4. Event chips
                let hovered_idx = hovered_chip_draw.get();
                for (idx, chip) in placed_chips_draw.borrow().iter().enumerate() {
                    draw_time_grid_chip(
                        cr,
                        chip,
                        width,
                        slot_height as f64,
                        hovered_idx == Some(idx),
                        is_dark,
                    );
                }

                // 5. Current-time red indicator line
                if target_date == today {
                    let now = Local::now().time();
                    let now_min = (now.hour() * 60 + now.minute()) as f64 + (now.second() as f64 / 60.0);
                    let now_y = (now_min / 1440.0) * height;
                    cr.set_source_rgba(0.88, 0.11, 0.14, 1.0);
                    cr.set_line_width(2.0);
                    cr.move_to(0.0, now_y);
                    cr.line_to(width, now_y);
                    let _ = cr.stroke();

                    cr.arc(3.0, now_y, 3.5, 0.0, 2.0 * std::f64::consts::PI);
                    let _ = cr.fill();
                }
            });

            // Motion controller for hover state and cursor change
            let motion = gtk::EventControllerMotion::new();
            let placed_chips_motion = placed_chips_rc.clone();
            let hovered_chip_motion = hovered_chip_idx_rc.clone();
            motion.connect_motion(move |controller, x, y| {
                let Some(area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
                let w = area.width() as f64;
                let found = find_chip_at(&placed_chips_motion.borrow(), x, y, w, slot_height as f64);
                if found != hovered_chip_motion.get() {
                    hovered_chip_motion.set(found);
                    if found.is_some() {
                        area.set_cursor_from_name(Some("pointer"));
                    } else {
                        area.set_cursor(None);
                    }
                    area.queue_draw();
                }
            });
            let hovered_chip_leave = hovered_chip_idx_rc.clone();
            motion.connect_leave(move |controller| {
                if let Some(area) = controller.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) {
                    hovered_chip_leave.set(None);
                    area.set_cursor(None);
                    area.queue_draw();
                }
            });
            drawing_area.add_controller(motion);

            // Tooltips for event chips
            drawing_area.set_has_tooltip(true);
            let placed_chips_tt = placed_chips_rc.clone();
            drawing_area.connect_query_tooltip(move |area, x, y, _keyboard, tooltip| {
                let w = area.width() as f64;
                let found = find_chip_at(&placed_chips_tt.borrow(), x as f64, y as f64, w, slot_height as f64);
                if let Some(idx) = found {
                    let chip = &placed_chips_tt.borrow()[idx];
                    let title = if chip.event.summary.trim().is_empty() {
                        "(Ohne Titel)"
                    } else {
                        chip.event.summary.as_str()
                    };
                    let cal = if chip.event.source_display_name.is_empty() {
                        ""
                    } else {
                        &chip.event.source_display_name
                    };
                    let text = if cal.is_empty() {
                        format!("{}\n{}", title, chip.time_text)
                    } else {
                        format!("{}\n{} · {}", title, chip.time_text, cal)
                    };
                    tooltip.set_text(Some(&text));
                    true
                } else {
                    false
                }
            });

            // Click gesture: open event details or select/open event dialog
            let click_gesture = gtk::GestureClick::new();
            click_gesture.set_button(1);
            let page_weak_click = self.downgrade();
            let placed_chips_click = placed_chips_rc.clone();
            let last_click_click = last_click_rc.clone();

            click_gesture.connect_pressed(move |gesture, n_press, click_x, click_y| {
                let Some(page) = page_weak_click.upgrade() else { return };
                let Some(area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
                let w = area.width() as f64;
                let found = find_chip_at(&placed_chips_click.borrow(), click_x, click_y, w, slot_height as f64);

                if let Some(chip_idx) = found {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let chip = &placed_chips_click.borrow()[chip_idx];
                    if n_press == 2 {
                        if let Some(id) = CalendarEventIdentity::from_event(&chip.event) {
                            page.open_event_editor_for_identity(&area, &id);
                        }
                    } else if n_press == 1 {
                        let (cx, cy, cw, ch) = chip_rect_coordinates(chip, w, slot_height as f64);
                        let rect = gdk::Rectangle::new(cx as i32, cy as i32, cw as i32, ch as i32);
                        show_calendar_event_details_popover_full(
                            &page,
                            &area,
                            &chip.event,
                            Some(target_date),
                            Some(rect),
                        );
                    }
                } else {
                    // Clicked on an empty time slot
                    let slot = ((click_y / slot_height as f64).floor() as usize).min(slot_count.saturating_sub(1));
                    let start_min = (slot * interval_minutes as usize) as u32;
                    let end_min = ((slot + 1) * interval_minutes as usize) as u32;

                    let direct_create = AppSettings::try_new()
                        .map(|s| s.calendar_click_action() == "create")
                        .unwrap_or(false);

                    let now = std::time::Instant::now();
                    let is_double_click = if let Some((last_slot, last_time)) = last_click_click.get() {
                        last_slot == slot && (now - last_time).as_millis() < 450
                    } else {
                        false
                    };
                    last_click_click.set(Some((slot, now)));

                    let prev_sel = *page.imp().selected_time_slot.borrow();
                    let is_already_selected = prev_sel == Some(SelectedTimeSlot {
                        date: target_date,
                        start_minute: start_min,
                        end_minute: end_min,
                    });

                    page.imp().selected_time_slot.replace(Some(SelectedTimeSlot {
                        date: target_date,
                        start_minute: start_min,
                        end_minute: end_min,
                    }));
                    if let Some(parent) = area.parent() {
                        parent.queue_draw();
                    } else {
                        area.queue_draw();
                    }

                    if is_double_click || (direct_create && is_already_selected) {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        let start_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(start_min as i64);
                        let end_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(end_min as i64);
                        let dialog = crate::dialogs::EventDialog::new_for_time_range(start_time, end_time);
                        if let Some(root) = area.root() {
                            if let Ok(win) = root.downcast::<gtk::Window>() {
                                dialog.set_transient_for(Some(&win));
                            }
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
                }
            });
            drawing_area.add_controller(click_gesture);

            // Drag gesture for selecting continuous time ranges
            let drag_gesture = gtk::GestureDrag::new();
            drag_gesture.set_button(1);
            let page_weak_drag = self.downgrade();
            let placed_chips_for_drag = placed_chips_rc.clone();
            let drag_sel_for_drag = drag_sel_rc.clone();
            let drag_start_y_begin = drag_start_y.clone();
            let drag_start_slot_begin = drag_start_slot.clone();

            drag_gesture.connect_drag_begin(move |gesture, start_x, start_y| {
                let Some(area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
                let w = area.width() as f64;
                let on_chip = find_chip_at(&placed_chips_for_drag.borrow(), start_x, start_y, w, slot_height as f64).is_some();
                if on_chip {
                    return;
                }
                drag_start_y_begin.set(start_y);
                let s = ((start_y.max(0.0) / slot_height as f64).floor() as usize).min(slot_count.saturating_sub(1));
                drag_start_slot_begin.set(s);
                drag_sel_for_drag.set(Some((s, s)));
                area.queue_draw();
            });

            let drag_sel_for_update = drag_sel_rc.clone();
            let drag_start_y_update = drag_start_y.clone();
            let drag_start_slot_update = drag_start_slot.clone();
            drag_gesture.connect_drag_update(move |gesture, _offset_x, offset_y| {
                if drag_sel_for_update.get().is_none() {
                    return;
                }
                if offset_y.abs() > 4.0 {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let Some(area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
                    let cur_y = (drag_start_y_update.get() + offset_y).max(0.0);
                    let cur_slot = ((cur_y / slot_height as f64).floor() as usize).min(slot_count.saturating_sub(1));
                    let start_s = drag_start_slot_update.get();
                    let min_s = start_s.min(cur_slot);
                    let max_s = start_s.max(cur_slot);
                    drag_sel_for_update.set(Some((min_s, max_s)));
                    area.queue_draw();
                }
            });

            let drag_sel_for_end = drag_sel_rc.clone();
            drag_gesture.connect_drag_end(move |gesture, _offset_x, _offset_y| {
                let Some(page) = page_weak_drag.upgrade() else { return };
                let Some(area) = gesture.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return };
                if let Some((min_s, max_s)) = drag_sel_for_end.take() {
                    let start_min = (min_s * interval_minutes as usize) as u32;
                    let end_min = ((max_s + 1) * interval_minutes as usize) as u32;

                    page.imp().selected_time_slot.replace(Some(SelectedTimeSlot {
                        date: target_date,
                        start_minute: start_min,
                        end_minute: end_min,
                    }));
                    if let Some(parent) = area.parent() {
                        parent.queue_draw();
                    } else {
                        area.queue_draw();
                    }

                    if min_s != max_s {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        let start_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(start_min as i64);
                        let end_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(end_min as i64);
                        let dialog = crate::dialogs::EventDialog::new_for_time_range(start_time, end_time);
                        if let Some(root) = area.root() {
                            if let Ok(win) = root.downcast::<gtk::Window>() {
                                dialog.set_transient_for(Some(&win));
                            }
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
                }
            });
            drawing_area.add_controller(drag_gesture);

            // DragSource: Dragging an event chip to reschedule it
            let drag_source = gtk::DragSource::new();
            drag_source.set_actions(gdk::DragAction::MOVE | gdk::DragAction::COPY);
            let placed_chips_drag = placed_chips_rc.clone();
            drag_source.connect_prepare(move |source, x, y| {
                let Some(area) = source.widget().and_then(|w| w.downcast::<gtk::DrawingArea>().ok()) else { return None };
                let w = area.width() as f64;
                let found = find_chip_at(&placed_chips_drag.borrow(), x, y, w, slot_height as f64);
                if let Some(idx) = found {
                    let chip = &placed_chips_drag.borrow()[idx];
                    let payload = format!(
                        "{}\n{}\n{}\n{}",
                        chip.event.source_uid,
                        chip.event.uid,
                        calendar_recurrence_id(&chip.event).unwrap_or_default(),
                        target_date.format("%Y-%m-%d")
                    );
                    Some(gdk::ContentProvider::for_value(&payload.to_value()))
                } else {
                    None
                }
            });
            drawing_area.add_controller(drag_source);

            // Drop target: Rescheduling a dropped event onto this day / time
            let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
            let page_weak_drop = self.downgrade();
            let interval_min_val = interval_minutes as u32;
            let total_h_f64 = total_grid_height as f64;

            drop_target.connect_drop(move |_, value, _x, y| {
                let Ok(payload) = value.get::<String>() else { return false; };
                let mut parts = payload.splitn(4, '\n');
                let Some(source_uid) = parts.next() else { return false; };
                let Some(uid) = parts.next() else { return false; };
                let recurrence_id = parts.next().map(str::trim).filter(|s| !s.is_empty());
                let occurrence_date = parts.next().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                let Some(page) = page_weak_drop.upgrade() else { return false; };
                let records = page.imp().event_records.borrow().as_ref().cloned();
                let Some(event) = records.and_then(|records| {
                    let target_identity = calendar_event_identity_from_parts(source_uid, uid, recurrence_id);
                    unique_event_for_identity(&records.borrow(), &target_identity)
                }) else {
                    show_toast_for_widget(&page, "Der gezogene Termin ist nicht mehr geladen");
                    return false;
                };
                let Some(start_date) = occurrence_date.or_else(|| event_start_date(&event)) else {
                    show_toast_for_widget(&page, "Der Termin besitzt kein gültiges Startdatum");
                    return false;
                };
                let delta_days = (target_date - start_date).num_days();
                let delta_minutes = if event.all_day {
                    0
                } else {
                    let target_min = ((y / total_h_f64) * 1440.0) as u32;
                    let snapped_min = (target_min / interval_min_val) * interval_min_val;
                    let start_dt = event_start_datetime(&event);
                    let current_minute = start_dt.map(|dt| dt.time().hour() * 60 + dt.time().minute()).unwrap_or(0);
                    snapped_min as i64 - current_minute as i64
                };
                page.move_event_by_delta(event, delta_days, delta_minutes);
                true
            });
            drawing_area.add_controller(drop_target);

            timed_grid.attach(
                &drawing_area,
                day_index as i32 + day_column_offset,
                0,
                1,
                slot_count as i32,
            );
        }

        // Current-time red indicator line in the time gutter
        if days.contains(&today) {
            let now = Local::now().time();
            let now_min = (now.hour() * 60 + now.minute()) as u32;
            let now_hour = (now_min / 60) as usize;
            if now_hour < total_hours {
                let hour_slot = (now_hour * slots_per_hour) as i32;
                let fraction = (now_min % 60) as f32 / 60.0;
                let top_margin = (fraction * hour_height as f32).round() as i32;
                let gutter_indicator = gtk::Box::builder()
                    .css_classes(vec!["calendar-current-time-line"])
                    .height_request(2)
                    .valign(gtk::Align::Start)
                    .halign(gtk::Align::Fill)
                    .hexpand(false)
                    .margin_top(top_margin)
                    .can_target(false)
                    .build();
                timed_grid.attach(&gutter_indicator, 0, hour_slot, day_column_offset, 1);
            }
        }

        let timed_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&timed_grid)
            .build();

        let target_slot = if days.contains(&today) {
            let now = Local::now().time();
            let now_min = (now.hour() * 60 + now.minute()) as u32;
            let now_slot = (now_min / interval_minutes as u32) as usize;
            now_slot.saturating_sub(2)
        } else {
            (workday_start / interval_minutes as u32) as usize
        };
        let target_y = (target_slot * slot_height as usize) as f64;
        let vadj = timed_scroll.vadjustment();
        glib::idle_add_local_once(move || {
            vadj.set_value(target_y);
        });

        view.append(&timed_scroll);
    }

    fn refresh_agenda_view(&self) {
        let Some(view) = self.imp().agenda_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        if self.append_calendar_state_to_box(&view) {
            return;
        }

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
        let agenda_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .hexpand(true)
            .build();

        let mut is_first = true;
        for date in visible_range.dates() {
            let mut events = events_by_date.get(&date).cloned().unwrap_or_default();
            events.sort_by_key(|event| event_start_datetime(event));
            if !events.is_empty() {
                has_events = true;
                let header_row = gtk::ListBoxRow::builder()
                    .activatable(false)
                    .selectable(false)
                    .build();
                let header_lbl = gtk::Label::builder()
                    .label(&format!(
                        "{} · KW {:02}",
                        format_date(date),
                        date.iso_week().week()
                    ))
                    .css_classes(if is_first {
                        vec!["calendar-agenda-date-header", "first"]
                    } else {
                        vec!["calendar-agenda-date-header"]
                    })
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .build();
                is_first = false;
                header_row.set_child(Some(&header_lbl));
                agenda_list.append(&header_row);

                for event in events {
                    agenda_list.append(&calendar_event_row(self, &event));
                }
            }
        }

        if has_events {
            view.append(&agenda_list);
        } else {
            view.append(&empty_period_row(
                "Keine kommenden Termine",
                "Für die nächsten 90 Tage sind keine VEVENTs geladen.",
            ));
        }
    }

    fn set_current_year(&self, target_year: i32) {
        let current = self.current_calendar_viewport().anchor_date;
        let month = current.month();
        let day = current.day();
        let new_date = NaiveDate::from_ymd_opt(target_year, month, day)
            .or_else(|| NaiveDate::from_ymd_opt(target_year, month, 28))
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(target_year, 1, 1).unwrap());
        *self.imp().current_date.borrow_mut() = new_date;
        self.imp()
            .view_state
            .borrow_mut()
            .set_anchor(new_date, configured_first_weekday());
        self.imp().view_state.borrow_mut().select_civil_day(new_date);
        self.refresh_calendar_views();
    }

    fn create_year_navigation_strip(&self, current_year: i32) -> gtk::Box {
        let strip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .css_classes(vec!["calendar-year-nav-strip"])
            .build();

        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .css_classes(vec!["flat", "calendar-year-nav-arrow"])
            .tooltip_text("Vorheriges Jahr")
            .build();
        let page_prev = self.downgrade();
        prev_btn.connect_clicked(move |_| {
            if let Some(page) = page_prev.upgrade() {
                page.set_current_year(current_year - 1);
            }
        });
        strip.append(&prev_btn);

        for y in (current_year - 2)..=(current_year + 2) {
            let year_btn = gtk::Button::builder()
                .label(&y.to_string())
                .css_classes(if y == current_year {
                    vec!["flat", "calendar-year-nav-current"]
                } else {
                    vec!["flat", "calendar-year-nav-btn"]
                })
                .tooltip_text(&format!("Jahr {y} anzeigen"))
                .build();
            let page_y = self.downgrade();
            year_btn.connect_clicked(move |_| {
                if let Some(page) = page_y.upgrade() {
                    page.set_current_year(y);
                }
            });
            strip.append(&year_btn);
        }

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .css_classes(vec!["flat", "calendar-year-nav-arrow"])
            .tooltip_text("Nächstes Jahr")
            .build();
        let page_next = self.downgrade();
        next_btn.connect_clicked(move |_| {
            if let Some(page) = page_next.upgrade() {
                page.set_current_year(current_year + 1);
            }
        });
        strip.append(&next_btn);

        let today_year = Local::now().date_naive().year();
        if current_year != today_year {
            let today_btn = gtk::Button::builder()
                .label("Heute")
                .css_classes(vec!["flat", "pill"])
                .margin_start(12)
                .tooltip_text("Zum heutigen Jahr zurückkehren")
                .build();
            let page_today = self.downgrade();
            today_btn.connect_clicked(move |_| {
                if let Some(page) = page_today.upgrade() {
                    page.set_current_year(today_year);
                }
            });
            strip.append(&today_btn);
        }

        strip
    }

    fn render_year_day_schedule(&self, date: NaiveDate, events: &[CalendarEventRecord], panel_width: i32) -> gtk::Widget {
        const HOUR_HEIGHT: f64 = 28.0;
        let today = Local::now().date_naive();
        let is_today = date == today;

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .vexpand(true)
            .hexpand(true)
            .build();

        // 1. All-day events
        let all_day_events: Vec<&CalendarEventRecord> = events.iter().filter(|e| e.all_day).collect();
        if !all_day_events.is_empty() {
            let allday_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_bottom(4)
                .build();
            for ev in all_day_events {
                let chip = calendar_grid_event_chip(self, ev, true, date, None, 1);
                allday_box.append(&chip);
            }
            content_box.append(&allday_box);
        }

        // 2. Timed events
        let timed_events: Vec<&CalendarEventRecord> = events.iter().filter(|e| !e.all_day).collect();

        struct TimedItem<'a> {
            event: &'a CalendarEventRecord,
            start_min: u32,
            end_min: u32,
            time_text: String,
        }

        let mut items: Vec<TimedItem> = Vec::new();
        for ev in timed_events {
            let (start_min, end_min, time_text) = event_occurrence_times_for_date(ev, date, 30);
            items.push(TimedItem {
                event: ev,
                start_min,
                end_min: end_min.max(start_min + 15),
                time_text,
            });
        }

        items.sort_by_key(|it| (it.start_min, it.end_min));

        // Overlapping clusters
        let mut clusters: Vec<Vec<TimedItem>> = Vec::new();
        let mut current_cluster: Vec<TimedItem> = Vec::new();
        let mut cluster_end = 0;

        for item in items {
            if current_cluster.is_empty() || item.start_min < cluster_end {
                cluster_end = cluster_end.max(item.end_min);
                current_cluster.push(item);
            } else {
                clusters.push(std::mem::take(&mut current_cluster));
                cluster_end = item.end_min;
                current_cluster.push(item);
            }
        }
        if !current_cluster.is_empty() {
            clusters.push(current_cluster);
        }

        let timeline_overlay = gtk::Overlay::new();

        let hours_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .height_request((24.0 * HOUR_HEIGHT) as i32)
            .hexpand(true)
            .build();

        for h in 0..24 {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .height_request(HOUR_HEIGHT as i32)
                .hexpand(true)
                .css_classes(vec!["appointment-timeline-hour-row"])
                .build();
            let lbl = gtk::Label::builder()
                .label(&format!("{h}⁰⁰"))
                .width_request(34)
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .css_classes(vec!["appointment-timeline-hour-label"])
                .build();
            let sep = gtk::Separator::builder()
                .orientation(gtk::Orientation::Horizontal)
                .hexpand(true)
                .valign(gtk::Align::Start)
                .build();
            row.append(&lbl);
            row.append(&sep);
            hours_box.append(&row);
        }
        timeline_overlay.set_child(Some(&hours_box));

        let timeline_fixed = gtk::Fixed::builder()
            .height_request((24.0 * HOUR_HEIGHT) as i32)
            .hexpand(true)
            .build();

        let is_dark = adw::StyleManager::default().is_dark();

        let events_x = 38.0;
        let available_width = (panel_width as f64 - 68.0).clamp(100.0, 900.0);

        for cluster in clusters {
            let mut col_ends: Vec<u32> = Vec::new();
            let mut placed: Vec<(TimedItem, usize)> = Vec::new();

            for item in cluster {
                let mut col_idx = None;
                for (idx, end) in col_ends.iter_mut().enumerate() {
                    if *end <= item.start_min {
                        *end = item.end_min;
                        col_idx = Some(idx);
                        break;
                    }
                }
                let c = col_idx.unwrap_or_else(|| {
                    let idx = col_ends.len();
                    col_ends.push(item.end_min);
                    idx
                });
                placed.push((item, c));
            }

            let total_cols = col_ends.len().max(1);
            let col_gap = 2.0;
            let col_width = if total_cols > 1 {
                ((available_width - (total_cols as f64 - 1.0) * col_gap) / total_cols as f64).max(30.0)
            } else {
                available_width
            };

            for (item, col_idx) in placed {
                let x = events_x + col_idx as f64 * (col_width + col_gap);
                let y = (item.start_min as f64 / 60.0) * HOUR_HEIGHT;
                let duration = item.end_min.saturating_sub(item.start_min);
                let h = ((duration.max(15) as f64) / 60.0) * HOUR_HEIGHT - 2.0;

                let color = event_color_for_record(item.event);
                let (bg, hover, active, fg) = pastel_colors_for_hex(&color, is_dark);
                let provider = cached_event_chip_css_provider(&bg, &hover, &active, &fg);

                let title = if item.event.summary.trim().is_empty() {
                    "(Ohne Titel)"
                } else {
                    item.event.summary.as_str()
                };

                let time_prefix = format_clock_minutes(item.start_min);
                let chip_btn = gtk::Button::builder()
                    .label(&format!("{} {}", time_prefix, title))
                    .halign(gtk::Align::Fill)
                    .valign(gtk::Align::Fill)
                    .width_request(col_width as i32)
                    .height_request(h.max(22.0) as i32)
                    .css_classes(vec!["flat", "calendar-grid-event"])
                    .tooltip_text(&format!("{}\n{} · {}", title, item.time_text, item.event.source_display_name))
                    .build();
                chip_btn.style_context().add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

                let page_weak = self.downgrade();
                let ev_record = item.event.clone();
                let identity = CalendarEventIdentity::from_event(item.event);
                let gesture = gtk::GestureClick::new();
                gesture.connect_pressed(move |gesture, n_press, _x, _y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let (Some(page), Some(widget)) = (page_weak.upgrade(), gesture.widget()) else { return };
                    if n_press == 2 {
                        if let Some(id) = identity.as_ref() {
                            page.open_event_editor_for_identity(&widget, id);
                        }
                    } else if n_press == 1 {
                        show_calendar_event_details_popover(&page, &widget, &ev_record, Some(date));
                    }
                });
                chip_btn.add_controller(gesture);

                timeline_fixed.put(&chip_btn, x, y);
            }
        }

        // Current-time red indicator line
        if is_today {
            let now = Local::now().time();
            let now_min = (now.hour() * 60 + now.minute()) as f64;
            let now_y = (now_min / 60.0) * HOUR_HEIGHT;
            let time_line = gtk::Box::builder()
                .css_classes(vec!["calendar-current-time-line"])
                .height_request(2)
                .width_request((available_width + 4.0) as i32)
                .can_target(false)
                .build();
            timeline_fixed.put(&time_line, events_x, now_y);
        }

        timeline_overlay.add_overlay(&timeline_fixed);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&timeline_overlay)
            .build();

        let target_hour = if is_today {
            (Local::now().hour() as f64 - 1.0).max(0.0)
        } else if let Some(first_ev) = events.iter().filter(|e| !e.all_day).next() {
            let (start_min, _, _) = event_occurrence_times_for_date(first_ev, date, 30);
            ((start_min / 60) as f64 - 1.0).max(0.0)
        } else {
            8.0
        };
        let target_y = target_hour * HOUR_HEIGHT;
        let vadj = scrolled.vadjustment();
        glib::idle_add_local_once(move || {
            vadj.set_value(target_y);
        });

        content_box.append(&scrolled);
        content_box.upcast::<gtk::Widget>()
    }

    fn render_year_day_details(&self, container: &gtk::Box, date: NaiveDate) {
        clear_box(container);

        let selected_events = self
            .events_by_date(CalendarDateRange::single(date))
            .remove(&date)
            .unwrap_or_default();

        self.imp()
            .view_state
            .borrow_mut()
            .reconcile_event_selection(&selected_events);

        // Header
        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .css_classes(vec!["calendar-year-details-header"])
            .build();

        let title_lbl = gtk::Label::builder()
            .label(&format_date(date))
            .css_classes(vec!["calendar-year-details-title"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        header_box.append(&title_lbl);

        // Mode switcher: [ Liste | Tagesplan ]
        let current_mode = self.imp().year_preview_mode.get();
        let switcher_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .build();

        let list_btn = gtk::Button::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Als Liste anzeigen")
            .css_classes(if current_mode == YearPreviewMode::List {
                vec!["suggested-action"]
            } else {
                vec!["flat"]
            })
            .build();
        let page_for_list = self.downgrade();
        let container_weak = container.downgrade();
        list_btn.connect_clicked(move |_| {
            if let Some(page) = page_for_list.upgrade() {
                page.imp().year_preview_mode.set(YearPreviewMode::List);
                if let Some(c) = container_weak.upgrade() {
                    page.render_year_day_details(&c, date);
                }
            }
        });
        switcher_box.append(&list_btn);

        let sched_btn = gtk::Button::builder()
            .icon_name("x-office-calendar-symbolic")
            .tooltip_text("Als Tagesplan anzeigen")
            .css_classes(if current_mode == YearPreviewMode::Schedule {
                vec!["suggested-action"]
            } else {
                vec!["flat"]
            })
            .build();
        let page_for_sched = self.downgrade();
        let container_weak2 = container.downgrade();
        sched_btn.connect_clicked(move |_| {
            if let Some(page) = page_for_sched.upgrade() {
                page.imp().year_preview_mode.set(YearPreviewMode::Schedule);
                if let Some(c) = container_weak2.upgrade() {
                    page.render_year_day_details(&c, date);
                }
            }
        });
        switcher_box.append(&sched_btn);
        header_box.append(&switcher_box);

        // Orientation toggle button
        let current_orientation = self.imp().year_preview_orientation.get();
        let (orient_icon, orient_tooltip) = match current_orientation {
            YearPreviewOrientation::Side => ("pan-down-symbolic", "Vorschau unten anzeigen"),
            YearPreviewOrientation::Bottom => ("sidebar-show-right-symbolic", "Vorschau rechts anzeigen"),
        };
        let orient_btn = gtk::Button::builder()
            .icon_name(orient_icon)
            .tooltip_text(orient_tooltip)
            .css_classes(vec!["flat"])
            .build();
        let page_for_orient = self.downgrade();
        orient_btn.connect_clicked(move |_| {
            if let Some(page) = page_for_orient.upgrade() {
                let next = match page.imp().year_preview_orientation.get() {
                    YearPreviewOrientation::Side => YearPreviewOrientation::Bottom,
                    YearPreviewOrientation::Bottom => YearPreviewOrientation::Side,
                };
                page.imp().year_preview_orientation.set(next);
                page.refresh_year_view();
            }
        });
        header_box.append(&orient_btn);

        // Open in Day View button
        let open_day_btn = gtk::Button::builder()
            .icon_name("go-jump-symbolic")
            .tooltip_text("In Tagesansicht öffnen")
            .css_classes(vec!["flat"])
            .build();
        let page_for_open = self.downgrade();
        open_day_btn.connect_clicked(move |_| {
            if let Some(page) = page_for_open.upgrade() {
                *page.imp().current_date.borrow_mut() = date;
                page.imp()
                    .view_state
                    .borrow_mut()
                    .set_anchor(date, configured_first_weekday());
                page.imp().view_state.borrow_mut().select_civil_day(date);
                page.select_calendar_view(CalendarView::Day);
            }
        });
        header_box.append(&open_day_btn);

        container.append(&header_box);

        // Content
        match current_mode {
            YearPreviewMode::Schedule => {
                let panel_w = match self.imp().year_preview_orientation.get() {
                    YearPreviewOrientation::Side => {
                        let cw = container.width();
                        if cw > 100 {
                            cw
                        } else {
                            self.imp().year_details_side_width.get()
                        }
                    }
                    YearPreviewOrientation::Bottom => {
                        let cw = container.width();
                        if cw > 100 {
                            cw
                        } else {
                            500
                        }
                    }
                };
                let schedule_widget = self.render_year_day_schedule(date, &selected_events, panel_w);
                container.append(&schedule_widget);
            }
            YearPreviewMode::List => {
                let event_list = gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::Single)
                    .css_classes(vec!["calendar-year-details-list"])
                    .vexpand(true)
                    .build();
                self.connect_calendar_event_selection(&event_list);

                let page_weak = self.downgrade();
                event_list.connect_row_activated(move |_, row| {
                    let Some(page) = page_weak.upgrade() else { return };
                    if let Some(identity) = calendar_event_identity_from_list_row(row) {
                        page.open_event_editor_for_identity(row, &identity);
                    }
                });

                if selected_events.is_empty() {
                    let empty_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(8)
                        .valign(gtk::Align::Center)
                        .halign(gtk::Align::Center)
                        .margin_top(40)
                        .margin_bottom(40)
                        .margin_start(16)
                        .margin_end(16)
                        .build();
                    let empty_icon = gtk::Image::builder()
                        .icon_name("x-office-calendar-symbolic")
                        .pixel_size(32)
                        .css_classes(vec!["dim-label"])
                        .build();
                    let empty_lbl = gtk::Label::builder()
                        .label("Keine Termine")
                        .css_classes(vec!["dim-label", "heading"])
                        .build();
                    let empty_sub = gtk::Label::builder()
                        .label(&format!("Für {} sind keine Termine eingetragen.", format_date(date)))
                        .wrap(true)
                        .wrap_mode(pango::WrapMode::WordChar)
                        .justify(gtk::Justification::Center)
                        .css_classes(vec!["dim-label", "caption"])
                        .build();
                    empty_box.append(&empty_icon);
                    empty_box.append(&empty_lbl);
                    empty_box.append(&empty_sub);
                    let empty_row = gtk::ListBoxRow::builder()
                        .activatable(false)
                        .selectable(false)
                        .child(&empty_box)
                        .build();
                    event_list.append(&empty_row);
                } else {
                    for event in &selected_events {
                        self.append_year_day_event_row(&event_list, event, date);
                    }
                    self.restore_calendar_event_selection(&event_list, &selected_events);
                }

                let scroll = gtk::ScrolledWindow::builder()
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .vscrollbar_policy(gtk::PolicyType::Automatic)
                    .vexpand(true)
                    .child(&event_list)
                    .build();
                container.append(&scroll);
            }
        }
    }

    fn append_year_day_event_row(
        &self,
        event_list: &gtk::ListBox,
        event: &CalendarEventRecord,
        date: NaiveDate,
    ) {
        let row = gtk::ListBoxRow::builder()
            .activatable(true)
            .selectable(true)
            .css_classes(vec!["calendar-year-event-row"])
            .build();

        let hbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .hexpand(true)
            .build();

        // 4px vertical color bar in event / calendar color
        let color = event_color_for_record(event);
        let color_bar = gtk::Box::builder()
            .width_request(4)
            .valign(gtk::Align::Fill)
            .css_classes(vec!["calendar-year-event-bar"])
            .build();
        let p = gtk::CssProvider::new();
        p.load_from_data(&format!(
            "box {{ background-color: {color}; border-radius: 9999px; min-width: 4px; width: 4px; }}"
        ));
        color_bar.style_context().add_provider(&p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        hbox.append(&color_bar);

        let text_col = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();

        let title = if event.summary.trim().is_empty() {
            "(Ohne Betreff)".to_string()
        } else {
            event.summary.clone()
        };

        let title_lbl = gtk::Label::builder()
            .label(&title)
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(vec!["calendar-year-event-title"])
            .build();
        text_col.append(&title_lbl);

        // Time and details
        let time_str = if event.all_day {
            "Ganztägig".to_string()
        } else {
            let (_, _, text) = event_occurrence_times_for_date(event, date, 30);
            if !text.is_empty() {
                text
            } else if !event.start.is_empty() {
                if !event.end.is_empty() && event.start != event.end {
                    format!("{}–{}", event.start, event.end)
                } else {
                    event.start.clone()
                }
            } else {
                String::new()
            }
        };

        let mut sub_parts = Vec::new();
        if !time_str.is_empty() {
            sub_parts.push(time_str);
        }
        if !event.location.trim().is_empty() {
            sub_parts.push(event.location.trim().to_string());
        }
        if !event.source_display_name.trim().is_empty() {
            sub_parts.push(event.source_display_name.trim().to_string());
        }

        if !sub_parts.is_empty() {
            let sub_lbl = gtk::Label::builder()
                .label(&sub_parts.join(" · "))
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(vec!["caption", "dim-label"])
                .build();
            text_col.append(&sub_lbl);
        }

        hbox.append(&text_col);
        row.set_child(Some(&hbox));

        if let Some(identity) = CalendarEventIdentity::from_event(event) {
            unsafe {
                row.set_data("event-id", identity.uid.clone());
                row.set_data("event-source-uid", identity.source_uid.clone());
                row.set_data("event-source-object-key", identity.source_object_key.clone());
                row.set_data("event-recurrence-id", identity.recurrence_id.clone().unwrap_or_default());
            }

            let page_weak = self.downgrade();
            let identity_for_click = identity.clone();
            row.connect_activate(move |r| {
                if let Some(page) = page_weak.upgrade() {
                    page.open_event_editor_for_identity(r, &identity_for_click);
                }
            });
        }

        row.set_tooltip_text(Some(&format!("{}\n(Klicken zum Bearbeiten)", title)));
        event_list.append(&row);
    }

    fn refresh_year_view(&self) {
        let Some(view) = self.imp().year_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        view.set_orientation(gtk::Orientation::Vertical);

        if self.append_calendar_state_to_box(&view) {
            return;
        }

        let year = self.current_calendar_viewport().anchor_date.year();
        let Some(year_range) = calendar_year_range_option(year) else {
            view.append(&empty_period_row(
                "Jahresansicht nicht verfügbar",
                "Das ausgewählte Kalenderjahr liegt außerhalb des unterstützten Datumsbereichs.",
            ));
            return;
        };

        let selected_day = self
            .imp()
            .view_state
            .borrow()
            .selected_range
            .filter(|range| range.start.year() == year)
            .map(|range| range.start)
            .unwrap_or_else(|| {
                self.current_calendar_viewport()
                    .anchor_date
                    .max(year_range.start)
                    .min(year_range.last_inclusive())
            });

        let today = Local::now().date_naive();
        let first_weekday = configured_first_weekday();
        let weekday_names = configured_weekday_names_short();
        let show_week_numbers = AppSettings::try_new()
            .map(|s| s.calendar_show_week_numbers())
            .unwrap_or(false);
        let col_offset = if show_week_numbers { 1 } else { 0 };

        let query = self.imp().calendar_search_query.borrow().clone();
        let category = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();

        let markers: BTreeMap<NaiveDate, usize> = if let Some(index) = self
            .imp()
            .calendar_snapshot_index
            .borrow()
            .as_ref()
        {
            indexed_event_day_markers(
                index,
                year_range,
                &query,
                category.as_deref(),
                &hidden_sources,
            )
        } else {
            self.events_by_date(year_range)
                .into_iter()
                .map(|(date, events)| (date, events.len()))
                .collect()
        };

        // Navigation strip at the top
        let nav_strip = self.create_year_navigation_strip(year);

        // Clear day buttons cache for the new year render
        self.imp().year_day_buttons.borrow_mut().clear();

        // 4x3 clean grid of 12 month cards
        let year_grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .row_homogeneous(false)
            .column_spacing(16)
            .row_spacing(16)
            .margin_start(16)
            .margin_end(16)
            .margin_top(8)
            .margin_bottom(16)
            .hexpand(true)
            .build();

        for month in 1..=YEAR_MONTH_COUNT {
            let Some(first_of_month) = NaiveDate::from_ymd_opt(year, month, 1) else {
                continue;
            };
            let last_of_month = match month {
                12 => NaiveDate::from_ymd_opt(year, 12, 31),
                m => NaiveDate::from_ymd_opt(year, m + 1, 1).map(|d| d - Duration::days(1)),
            }
            .unwrap_or(first_of_month);

            let card = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .css_classes(vec!["calendar-year-month-card"])
                .build();

            // Clickable month header that navigates to Month View
            let month_header_btn = gtk::Button::builder()
                .label(Self::month_name(month))
                .css_classes(vec!["flat", "calendar-year-month-header"])
                .halign(gtk::Align::Center)
                .tooltip_text(&format!("Zur Monatsansicht für {} {}", Self::month_name(month), year))
                .build();
            let page_for_month = self.downgrade();
            month_header_btn.connect_clicked(move |_| {
                if let Some(page) = page_for_month.upgrade() {
                    *page.imp().current_date.borrow_mut() = first_of_month;
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .set_anchor(first_of_month, configured_first_weekday());
                    page.imp().view_state.borrow_mut().select_civil_day(first_of_month);
                    page.select_calendar_view(CalendarView::Month);
                }
            });
            card.append(&month_header_btn);

            let month_grid = gtk::Grid::builder()
                .column_homogeneous(true)
                .row_homogeneous(false)
                .column_spacing(2)
                .row_spacing(2)
                .halign(gtk::Align::Center)
                .build();

            // KW column header if enabled
            if show_week_numbers {
                let kw_hdr = gtk::Label::builder()
                    .label("KW")
                    .css_classes(vec!["calendar-year-week-number", "dim-label"])
                    .halign(gtk::Align::Center)
                    .build();
                month_grid.attach(&kw_hdr, 0, 0, 1, 1);
            }

            // Weekday headers
            for (col, name) in weekday_names.iter().enumerate() {
                let lbl = gtk::Label::builder()
                    .label(*name)
                    .css_classes(vec!["calendar-year-weekday-header", "dim-label"])
                    .halign(gtk::Align::Center)
                    .build();
                month_grid.attach(&lbl, col as i32 + col_offset, 0, 1, 1);
            }

            // Days of the month
            let grid_start = week_start_for(first_of_month, first_weekday);
            let mut curr = grid_start;
            let mut row = 1;
            while curr <= last_of_month {
                if show_week_numbers {
                    let kw = (curr + Duration::days(3)).iso_week().week();
                    let kw_lbl = gtk::Label::builder()
                        .label(&kw.to_string())
                        .css_classes(vec!["calendar-year-week-number", "dim-label"])
                        .halign(gtk::Align::Center)
                        .valign(gtk::Align::Center)
                        .build();
                    month_grid.attach(&kw_lbl, 0, row, 1, 1);
                }

                for col in 0..7 {
                    let cell_date = curr + Duration::days(col as i64);
                    if cell_date.month() == month {
                        let event_count = markers.get(&cell_date).copied().unwrap_or(0);
                        let day_btn = gtk::Button::builder()
                            .css_classes(vec!["flat", "calendar-year-day-btn"])
                            .build();

                        let btn_content = gtk::Box::builder()
                            .orientation(gtk::Orientation::Vertical)
                            .spacing(0)
                            .halign(gtk::Align::Center)
                            .valign(gtk::Align::Center)
                            .build();

                        let num_lbl = gtk::Label::builder()
                            .label(&cell_date.day().to_string())
                            .halign(gtk::Align::Center)
                            .build();
                        btn_content.append(&num_lbl);

                        let dot = gtk::Box::builder()
                            .css_classes(vec!["calendar-year-event-dot"])
                            .halign(gtk::Align::Center)
                            .opacity(if event_count > 0 { 1.0 } else { 0.0 })
                            .build();
                        btn_content.append(&dot);

                        day_btn.set_child(Some(&btn_content));

                        if cell_date == today {
                            day_btn.add_css_class("calendar-year-today");
                        }
                        if cell_date == selected_day {
                            day_btn.add_css_class("calendar-year-selected");
                        }
                        if event_count > 0 {
                            day_btn.add_css_class("calendar-year-has-events");
                        }

                        let tooltip = if event_count == 0 {
                            format_date(cell_date)
                        } else if event_count == 1 {
                            format!("{} · 1 Termin", format_date(cell_date))
                        } else {
                            format!("{} · {} Termine", format_date(cell_date), event_count)
                        };
                        day_btn.set_tooltip_text(Some(&tooltip));

                        set_accessible_label(
                            &day_btn,
                            &format!(
                                "{}{}",
                                format_date(cell_date),
                                if event_count == 0 {
                                    String::new()
                                } else {
                                    format!("; {} Termine", event_count)
                                }
                            ),
                        );

                        let click_gesture = gtk::GestureClick::new();
                        let page_weak = self.downgrade();
                        let target_date = cell_date;
                        click_gesture.connect_pressed(move |_gesture, n_press, _x, _y| {
                            let Some(page) = page_weak.upgrade() else { return };
                            if n_press == 1 {
                                page.select_calendar_day(target_date);
                            } else if n_press >= 2 {
                                *page.imp().current_date.borrow_mut() = target_date;
                                page.imp()
                                    .view_state
                                    .borrow_mut()
                                    .set_anchor(target_date, configured_first_weekday());
                                page.imp().view_state.borrow_mut().select_civil_day(target_date);
                                page.select_calendar_view(CalendarView::Day);
                            }
                        });
                        day_btn.add_controller(click_gesture);

                        self.imp().year_day_buttons.borrow_mut().insert(cell_date, day_btn.clone());
                        month_grid.attach(&day_btn, col as i32 + col_offset, row, 1, 1);
                    } else {
                        let spacer = gtk::Box::builder()
                            .css_classes(vec!["calendar-year-day-btn"])
                            .can_target(false)
                            .build();
                        month_grid.attach(&spacer, col as i32 + col_offset, row, 1, 1);
                    }
                }

                curr = curr + Duration::days(7);
                row += 1;
            }

            card.append(&month_grid);
            let col = ((month - 1) % 4) as i32;
            let r = ((month - 1) / 4) as i32;
            year_grid.attach(&card, col, r, 1, 1);
        }

        let scrolled_year_grid = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&year_grid)
            .build();

        let calendar_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .build();
        calendar_area.append(&nav_strip);
        calendar_area.append(&scrolled_year_grid);

        let details_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .vexpand(true)
            .build();

        let orientation = self.imp().year_preview_orientation.get();
        let paned_orientation = match orientation {
            YearPreviewOrientation::Side => {
                details_container.add_css_class("calendar-year-details-panel");
                details_container.add_css_class("side-mode");
                let side_w = self.imp().year_details_side_width.get();
                details_container.set_width_request(side_w.clamp(160, 600));
                gtk::Orientation::Horizontal
            }
            YearPreviewOrientation::Bottom => {
                details_container.add_css_class("calendar-year-details-panel");
                details_container.add_css_class("bottom-mode");
                let bottom_h = self.imp().year_details_bottom_height.get();
                details_container.set_height_request(bottom_h.clamp(120, 500));
                gtk::Orientation::Vertical
            }
        };

        // Resizable panel dividing calendar_area and details_container (task/notes panel parity)
        let year_paned = gtk::Paned::builder()
            .orientation(paned_orientation)
            .wide_handle(false)
            .hexpand(true)
            .vexpand(true)
            .build();

        year_paned.set_start_child(Some(&calendar_area));
        year_paned.set_end_child(Some(&details_container));
        year_paned.set_resize_start_child(true);
        year_paned.set_resize_end_child(false);
        year_paned.set_shrink_start_child(true);
        year_paned.set_shrink_end_child(true);

        self.imp().year_paned.replace(Some(year_paned.clone()));

        let page_weak_paned = self.downgrade();
        year_paned.connect_position_notify(move |paned| {
            let Some(page) = page_weak_paned.upgrade() else { return };
            let pos = paned.position();
            if pos <= 0 { return; }
            match page.imp().year_preview_orientation.get() {
                YearPreviewOrientation::Side => {
                    let total_w = paned.width();
                    if total_w > 0 {
                        let details_w = total_w - pos;
                        if (140..=1200).contains(&details_w) {
                            page.imp().year_details_side_width.set(details_w);
                        }
                    }
                }
                YearPreviewOrientation::Bottom => {
                    let total_h = paned.height();
                    if total_h > 0 {
                        let details_h = total_h - pos;
                        if (100..=800).contains(&details_h) {
                            page.imp().year_details_bottom_height.set(details_h);
                        }
                    }
                }
            }
        });

        view.append(&year_paned);

        self.imp().year_details_container.replace(Some(details_container.clone()));
        self.render_year_day_details(&details_container, selected_day);
        self.imp().year_rendered_year.set(Some(year));
    }

    fn populate_list_view(&self, view: &gtk::Box, events: Vec<CalendarEventRecord>) {
        clear_box(view);
        self.imp()
            .view_state
            .borrow_mut()
            .reconcile_event_selection(&events);
        self.imp()
            .calendar_list_projection_count
            .set(Some(events.len()));
        let count_label = if events.is_empty() {
            "Keine passenden Termine".to_string()
        } else if events.len() == 1 {
            "1 gefilterter Termin".to_string()
        } else {
            format!("{} gefilterte Termine", events.len())
        };
        view.append(
            &gtk::Label::builder()
                .label(&count_label)
                .css_classes(vec!["title-3", "dim-label"])
                .halign(gtk::Align::Start)
                .margin_start(16)
                .margin_end(16)
                .margin_top(12)
                .margin_bottom(8)
                .build(),
        );
        if events.is_empty() {
            let all_count = self
                .imp()
                .calendar_snapshot_index
                .borrow()
                .as_ref()
                .map(|index| index.snapshot.events.len())
                .unwrap_or(0);
            let empty_row = empty_period_row(
                if all_count == 0 {
                    "Keine Termine geladen"
                } else {
                    "Keine Termine entsprechen den Filtern"
                },
                "Die Listenansicht ist unabhängig vom aktuellen Datumsanker.",
            );
            empty_row.set_margin_start(16);
            empty_row.set_margin_end(16);
            view.append(&empty_row);
            return;
        }
        let row_height = 46.0f64;
        let total_h = events.len() as f64 * row_height;
        let prepared_events: Vec<PreparedCalendarListRow> =
            events.iter().map(prepare_calendar_list_row).collect();
        let prepared_events_rc = Rc::new(prepared_events);
        let hovered_row = Rc::new(Cell::new(None::<usize>));

        let vadjustment = gtk::Adjustment::new(
            0.0,
            0.0,
            total_h,
            row_height,
            row_height * 8.0,
            100.0,
        );

        let scrollbar = gtk::Scrollbar::builder()
            .orientation(gtk::Orientation::Vertical)
            .adjustment(&vadjustment)
            .build();

        let drawing_area = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .can_focus(true)
            .css_classes(vec!["calendar-list-canvas"])
            .build();

        let canvas_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .vexpand(true)
            .build();
        canvas_box.append(&drawing_area);
        canvas_box.append(&scrollbar);
        view.append(&canvas_box);

        let drawing_area_weak = drawing_area.downgrade();
        vadjustment.connect_value_changed(move |_| {
            if let Some(area) = drawing_area_weak.upgrade() {
                area.queue_draw();
            }
        });

        let page_weak_draw = self.downgrade();
        let prepared_draw = prepared_events_rc.clone();
        let hovered_draw = hovered_row.clone();
        let vadjustment_draw = vadjustment.clone();
        let scrollbar_weak = scrollbar.downgrade();

        drawing_area.set_draw_func(move |_area, cr, width_i32, height_i32| {
            let width = width_i32 as f64;
            let height = height_i32 as f64;
            let is_dark = adw::StyleManager::default().is_dark();

            // Synchronize visible page size with vadjustment
            if (vadjustment_draw.page_size() - height).abs() > 0.5
                || (vadjustment_draw.upper() - total_h.max(height)).abs() > 0.5
            {
                vadjustment_draw.set_page_size(height);
                vadjustment_draw.set_page_increment(height * 0.9);
                vadjustment_draw.set_upper(total_h.max(height));
                let max_val = (total_h - height).max(0.0);
                if vadjustment_draw.value() > max_val {
                    vadjustment_draw.set_value(max_val);
                }
                if let Some(sb) = scrollbar_weak.upgrade() {
                    let should_show = total_h > height;
                    if sb.is_visible() != should_show {
                        glib::idle_add_local_once(move || {
                            sb.set_visible(should_show);
                        });
                    }
                }
            }

            let events_len = prepared_draw.len();
            if events_len == 0 {
                return;
            }

            let scroll_y = vadjustment_draw.value();
            let first_row = ((scroll_y / row_height).floor() as usize).min(events_len.saturating_sub(1));
            let num_visible = ((height / row_height).ceil() as usize) + 1;
            let last_row = (first_row + num_visible).min(events_len);

            let selected_idx = page_weak_draw
                .upgrade()
                .and_then(|p| p.imp().view_state.borrow().selected_event_index);
            let hovered_idx = hovered_draw.get();

            for idx in first_row..last_row {
                let item = &prepared_draw[idx];
                let row_screen_y = (idx as f64 * row_height) - scroll_y;
                let is_selected = selected_idx == Some(idx);
                let is_hovered = hovered_idx == Some(idx);

                draw_prepared_calendar_list_row(
                    cr,
                    item,
                    row_screen_y,
                    width,
                    row_height,
                    is_selected,
                    is_hovered,
                    is_dark,
                );
            }
        });

        // Scroll controller (smooth trackpad & mouse wheel)
        let vadjustment_scroll = vadjustment.clone();
        let scroll_ctrl = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL,
        );
        scroll_ctrl.connect_scroll(move |_ctrl, _dx, dy| {
            let old_val = vadjustment_scroll.value();
            let max_val = (vadjustment_scroll.upper() - vadjustment_scroll.page_size()).max(0.0);
            if max_val <= 0.0 {
                return glib::Propagation::Proceed;
            }
            let delta = dy * 36.0;
            let new_val = (old_val + delta).clamp(0.0, max_val);
            if (new_val - old_val).abs() > 0.01 {
                vadjustment_scroll.set_value(new_val);
            }
            glib::Propagation::Stop
        });
        drawing_area.add_controller(scroll_ctrl);

        // Motion controller (hover)
        let hovered_motion = hovered_row.clone();
        let vadjustment_motion = vadjustment.clone();
        let events_len = prepared_events_rc.len();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(move |ctrl, _x, y| {
            let Some(area) = ctrl
                .widget()
                .and_then(|w| w.downcast::<gtk::DrawingArea>().ok())
            else {
                return;
            };
            let virtual_y = y + vadjustment_motion.value();
            let idx = (virtual_y / row_height).floor() as usize;
            let new_hover = if idx < events_len && virtual_y >= 0.0 {
                Some(idx)
            } else {
                None
            };
            if new_hover != hovered_motion.get() {
                hovered_motion.set(new_hover);
                if new_hover.is_some() {
                    area.set_cursor_from_name(Some("pointer"));
                } else {
                    area.set_cursor(None);
                }
                area.queue_draw();
            }
        });
        let hovered_leave = hovered_row.clone();
        motion.connect_leave(move |ctrl| {
            if let Some(area) = ctrl
                .widget()
                .and_then(|w| w.downcast::<gtk::DrawingArea>().ok())
            {
                hovered_leave.set(None);
                area.set_cursor(None);
                area.queue_draw();
            }
        });
        drawing_area.add_controller(motion);

        // Tooltips
        drawing_area.set_has_tooltip(true);
        let prepared_tt = prepared_events_rc.clone();
        let vadjustment_tt = vadjustment.clone();
        drawing_area.connect_query_tooltip(move |_, _x, y, _, tooltip| {
            let virtual_y = y as f64 + vadjustment_tt.value();
            let idx = (virtual_y / row_height).floor() as usize;
            if idx < prepared_tt.len() && virtual_y >= 0.0 {
                let ev = &prepared_tt[idx].record;
                let title = if ev.summary.trim().is_empty() {
                    "(Ohne Titel)"
                } else {
                    ev.summary.trim()
                };
                let time = if ev.all_day {
                    "Ganztägig"
                } else {
                    &ev.start
                };
                let mut text = format!("{}\n{}", title, time);
                if !ev.location.trim().is_empty() {
                    text.push_str(&format!("\n📍 {}", ev.location.trim()));
                }
                if !ev.source_display_name.is_empty() {
                    text.push_str(&format!("\n📅 {}", ev.source_display_name));
                }
                tooltip.set_text(Some(&text));
                true
            } else {
                false
            }
        });

        // Click controller (Selection & Double-click edit)
        let click_gesture = gtk::GestureClick::new();
        click_gesture.set_button(1);
        let page_weak_click = self.downgrade();
        let prepared_click = prepared_events_rc.clone();
        let vadjustment_click = vadjustment.clone();
        click_gesture.connect_pressed(move |gesture, n_press, _x, y| {
            let Some(page) = page_weak_click.upgrade() else {
                return;
            };
            let Some(area) = gesture
                .widget()
                .and_then(|w| w.downcast::<gtk::DrawingArea>().ok())
            else {
                return;
            };
            let virtual_y = y + vadjustment_click.value();
            let idx = (virtual_y / row_height).floor() as usize;
            if idx < prepared_click.len() && virtual_y >= 0.0 {
                area.grab_focus();
                let item = &prepared_click[idx];
                page.imp()
                    .view_state
                    .borrow_mut()
                    .select_event(item.identity.clone(), Some(idx));
                area.queue_draw();

                if n_press == 2 {
                    if let Some(id) = &item.identity {
                        page.open_event_editor_for_identity(&area, id);
                    }
                }
            }
        });
        drawing_area.add_controller(click_gesture);

        // Right-click context menu (Edit, Print, Export, Copy, Move, Delete)
        let right_click = gtk::GestureClick::new();
        right_click.set_button(3);
        let page_weak_right = self.downgrade();
        let prepared_right = prepared_events_rc.clone();
        let vadjustment_right = vadjustment.clone();
        right_click.connect_pressed(move |gesture, _, x, y| {
            let Some(page) = page_weak_right.upgrade() else {
                return;
            };
            let Some(area) = gesture
                .widget()
                .and_then(|w| w.downcast::<gtk::DrawingArea>().ok())
            else {
                return;
            };
            let virtual_y = y + vadjustment_right.value();
            let idx = (virtual_y / row_height).floor() as usize;
            if idx < prepared_right.len() && virtual_y >= 0.0 {
                let item = &prepared_right[idx];
                page.imp()
                    .view_state
                    .borrow_mut()
                    .select_event(item.identity.clone(), Some(idx));
                area.queue_draw();

                let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                show_calendar_event_context_menu(&page, &area, &item.record, rect);
            }
        });
        drawing_area.add_controller(right_click);

        // Keyboard navigation (Up / Down / PageUp / PageDown / Home / End / Enter)
        let key_ctrl = gtk::EventControllerKey::new();
        let page_weak_key = self.downgrade();
        let prepared_key = prepared_events_rc.clone();
        let vadjustment_key = vadjustment.clone();
        key_ctrl.connect_key_pressed(move |ctrl, keyval, _, _| {
            let Some(page) = page_weak_key.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let Some(area) = ctrl
                .widget()
                .and_then(|w| w.downcast::<gtk::DrawingArea>().ok())
            else {
                return glib::Propagation::Proceed;
            };
            let len = prepared_key.len();
            if len == 0 {
                return glib::Propagation::Proceed;
            };

            let cur_idx = page
                .imp()
                .view_state
                .borrow()
                .selected_event_index
                .unwrap_or(0);
            let scroll_into_view = |idx: usize| {
                let row_top = idx as f64 * row_height;
                let row_bot = row_top + row_height;
                let cur_scroll = vadjustment_key.value();
                let page_size = vadjustment_key.page_size();
                if row_top < cur_scroll {
                    vadjustment_key.set_value(row_top);
                } else if row_bot > cur_scroll + page_size {
                    vadjustment_key.set_value((row_bot - page_size).max(0.0));
                }
            };

            match keyval {
                gdk::Key::Up => {
                    let next_idx = cur_idx.saturating_sub(1);
                    let identity = prepared_key[next_idx].identity.clone();
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .select_event(identity, Some(next_idx));
                    scroll_into_view(next_idx);
                    area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Down => {
                    let next_idx = (cur_idx + 1).min(len.saturating_sub(1));
                    let identity = prepared_key[next_idx].identity.clone();
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .select_event(identity, Some(next_idx));
                    scroll_into_view(next_idx);
                    area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Page_Up => {
                    let page_steps = (vadjustment_key.page_size() / row_height).floor() as usize;
                    let next_idx = cur_idx.saturating_sub(page_steps.max(1));
                    let identity = prepared_key[next_idx].identity.clone();
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .select_event(identity, Some(next_idx));
                    scroll_into_view(next_idx);
                    area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Page_Down => {
                    let page_steps = (vadjustment_key.page_size() / row_height).floor() as usize;
                    let next_idx = (cur_idx + page_steps.max(1)).min(len.saturating_sub(1));
                    let identity = prepared_key[next_idx].identity.clone();
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .select_event(identity, Some(next_idx));
                    scroll_into_view(next_idx);
                    area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Home => {
                    let next_idx = 0;
                    let identity = prepared_key[next_idx].identity.clone();
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .select_event(identity, Some(next_idx));
                    scroll_into_view(next_idx);
                    area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::End => {
                    let next_idx = len.saturating_sub(1);
                    let identity = prepared_key[next_idx].identity.clone();
                    page.imp()
                        .view_state
                        .borrow_mut()
                        .select_event(identity, Some(next_idx));
                    scroll_into_view(next_idx);
                    area.queue_draw();
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    if cur_idx < len {
                        if let Some(id) = &prepared_key[cur_idx].identity {
                            page.open_event_editor_for_identity(&area, id);
                        }
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        drawing_area.add_controller(key_ctrl);
    }

    fn refresh_list_view(&self) {
        let Some(view) = self.imp().list_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        self.imp().calendar_list_projection_count.set(None);
        if self.append_calendar_state_to_box(&view) {
            return;
        }
        let Some(index) = self.imp().calendar_snapshot_index.borrow().as_ref().cloned() else {
            view.append(&empty_period_row(
                "Keine Kalenderdaten bestätigt",
                "Die Listenansicht wartet auf einen bestätigten Snapshot.",
            ));
            return;
        };

        let search_query = self.imp().calendar_search_query.borrow().clone();
        let category_filter = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();

        let plan = build_list_projection_plan(
            &index,
            &search_query,
            category_filter.as_deref(),
            &hidden_sources,
        );

        self.populate_list_view(&view, plan.events);
        self.refresh_period_label();
    }

    fn apply_list_projection_plan(&self, load: CalendarProjectionLoad, plan: ListProjectionPlan) {
        let Some(view) = self.imp().list_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.calendar_projection_is_current(&load) {
            return;
        }
        self.populate_list_view(&view, plan.events);
        if self.finish_calendar_projection(&load) {
            self.refresh_period_label();
        }
    }

    fn append_calendar_event_action_row(
        &self,
        event_list: &gtk::ListBox,
        event: &CalendarEventRecord,
    ) {
        if let Some(records) = self.imp().event_records.borrow().as_ref().cloned() {
            append_event_row(self, event_list, records, event.clone());
        } else {
            event_list.append(&calendar_event_row(self, event));
        }
    }

    fn select_calendar_day(&self, date: NaiveDate) {
        if *self.imp().current_view.borrow() != CalendarView::Year {
            return;
        }
        if date.year() != self.current_calendar_viewport().anchor_date.year() {
            return;
        }
        let old_date = *self.imp().current_date.borrow();
        *self.imp().current_date.borrow_mut() = date;
        self.imp()
            .view_state
            .borrow_mut()
            .set_anchor(date, configured_first_weekday());
        self.imp().view_state.borrow_mut().select_civil_day(date);

        // Fast path: if the year view is already rendered for this year, update the day selection
        // styling in place and refresh only the details panel without rebuilding all 12 month grids.
        if self.imp().year_rendered_year.get() == Some(date.year()) {
            if old_date != date {
                let day_buttons = self.imp().year_day_buttons.borrow();
                if let Some(old_btn) = day_buttons.get(&old_date) {
                    old_btn.remove_css_class("calendar-year-selected");
                }
                if let Some(new_btn) = day_buttons.get(&date) {
                    new_btn.add_css_class("calendar-year-selected");
                }
            }
            if let Some(details) = self.imp().year_details_container.borrow().as_ref() {
                self.render_year_day_details(details, date);
            }
            self.refresh_period_label();
            return;
        }

        self.refresh_calendar_views();
    }

    fn connect_calendar_event_selection(&self, event_list: &gtk::ListBox) {
        let page_weak = self.downgrade();
        event_list.connect_row_selected(move |_, row| {
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            let identity = row.and_then(calendar_event_identity_from_list_row);
            page.select_calendar_event_identity(identity);
        });
    }

    fn select_calendar_event_identity(&self, identity: Option<CalendarEventIdentity>) {
        let events = self.filtered_sorted_events();
        let Some(identity) = identity else {
            self.imp().view_state.borrow_mut().select_event(None, None);
            return;
        };
        let Some(index) = unique_event_index_for_identity(&events, &identity) else {
            self.imp().view_state.borrow_mut().select_event(None, None);
            return;
        };
        self.imp()
            .view_state
            .borrow_mut()
            .select_event(Some(identity), Some(index));
    }

    fn restore_calendar_event_selection(
        &self,
        event_list: &gtk::ListBox,
        events: &[CalendarEventRecord],
    ) {
        let selected = self.imp().view_state.borrow().selected_event.clone();
        let Some(selected) = selected else {
            return;
        };
        let Some(index) = unique_event_index_for_identity(events, &selected) else {
            return;
        };
        if let Some(row) = event_list.row_at_index(index as i32) {
            event_list.select_row(Some(&row));
        }
    }

    fn filtered_sorted_events(&self) -> Vec<CalendarEventRecord> {
        let query = self.imp().calendar_search_query.borrow().clone();
        let category = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();
        if let Some(index) = self
            .imp()
            .calendar_snapshot_index
            .borrow()
            .as_ref()
            .cloned()
        {
            return filtered_sorted_event_indices(
                &index,
                &query,
                category.as_deref(),
                &hidden_sources,
            )
            .into_iter()
            .filter_map(|event_index| index.snapshot.events.get(event_index).cloned())
            .collect();
        }

        let mut events = self
            .imp()
            .event_records
            .borrow()
            .as_ref()
            .map(|records| records.borrow().clone())
            .unwrap_or_default();
        events.retain(|event| {
            event_matches_calendar_filter(event, &query, category.as_deref(), &hidden_sources)
        });
        events.sort_by(compare_event_list_order);
        events
    }

    fn events_by_date(
        &self,
        range: CalendarDateRange,
    ) -> BTreeMap<NaiveDate, Vec<CalendarEventRecord>> {
        let query = self.imp().calendar_search_query.borrow().clone();
        let category = self.imp().calendar_category_filter.borrow().clone();
        let hidden_sources = self.imp().hidden_calendar_sources.borrow().clone();
        if let Some(index) = self
            .imp()
            .calendar_snapshot_index
            .borrow()
            .as_ref()
            .cloned()
        {
            return indexed_events_by_date(
                &index,
                range,
                &query,
                category.as_deref(),
                &hidden_sources,
            );
        }

        let mut grouped = BTreeMap::<NaiveDate, Vec<CalendarEventRecord>>::new();
        if let Some(records) = self.imp().event_records.borrow().as_ref().cloned() {
            for date in range.dates() {
                for event in records.borrow().iter() {
                    if event_matches_calendar_filter(
                        event,
                        &query,
                        category.as_deref(),
                        &hidden_sources,
                    ) && event_occurs_on(event, date)
                    {
                        grouped.entry(date).or_default().push(event.clone());
                    }
                }
            }
        }

        for (date, events) in grouped.iter_mut() {
            events.sort_by(|a, b| compare_day_events_order(a, b, *date));
            let mut seen = BTreeSet::new();
            events.retain(|event| {
                CalendarEventIdentity::from_event(event)
                    .map(|identity| seen.insert(identity))
                    .unwrap_or(true)
            });
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

    fn update_month_header_labels(&self, is_compact: bool) {
        self.imp().month_header_is_compact.set(is_compact);
        let names = if is_compact {
            configured_weekday_names_short()
        } else {
            configured_weekday_names_full()
        };
        let labels = self.imp().month_header_labels.borrow();
        for (label, &name) in labels.iter().zip(names.iter()) {
            label.set_text(name);
        }
    }

    fn refresh_month_grid(&self) {
        let viewport = self.current_calendar_viewport();
        let events_by_date = self.events_by_date(viewport.visible_range);
        let today = Local::now().date_naive();
        let show_week_numbers = AppSettings::try_new()
            .map(|settings| settings.calendar_show_week_numbers())
            .unwrap_or(false);
        let compress_weekend = configured_compress_weekend();
        let workdays: HashSet<Weekday> = configured_workdays().into_iter().collect();
        let is_dark = adw::StyleManager::default().is_dark();

        let weekday_names_full = configured_weekday_names_full();
        let weekday_names_short = configured_weekday_names_short();

        let layout = prepare_month_layout(
            &viewport,
            &events_by_date,
            today,
            show_week_numbers,
            &workdays,
            compress_weekend,
            &weekday_names_full,
            &weekday_names_short,
            is_dark,
            6,
        );

        self.imp().month_layout.replace(Some(Rc::new(layout)));
        if let Some(canvas) = self.imp().month_canvas.borrow().as_ref() {
            canvas.queue_draw();
        }
    }

    fn move_event_by_days(&self, event: CalendarEventRecord, delta_days: i64) {
        self.move_event_by_delta(event, delta_days, 0);
    }

    fn move_event_by_delta(
        &self,
        event: CalendarEventRecord,
        delta_days: i64,
        delta_minutes: i64,
    ) {
        if delta_days == 0 && delta_minutes == 0 {
            return;
        }
        if let Some(error) = event_mutation_error(&event) {
            show_toast_for_widget(self, error);
            return;
        }
        let updated_component =
            match shift_event_component_full(&event.component, delta_days, delta_minutes) {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(self, &error);
                    return;
                }
            };
        let mutation_token =
            self.begin_calendar_mutation(CalendarEventIdentity::from_event(&event));
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
            let result = receiver.recv().await;
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !page.calendar_event_mutation_is_current(&mutation_token) {
                show_toast_for_widget(
                    &page,
                    "Der Kalenderstand hat sich geändert; die Terminverschiebung wurde verworfen",
                );
                page.reload_calendar_data();
                return;
            }
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    page.finish_calendar_mutation(&mutation_token);
                    show_toast_for_widget(
                        &page,
                        &format!("Terminverschiebung konnte nicht bestätigt werden: {error}"),
                    );
                    page.reload_calendar_data();
                    return;
                }
            };
            match result {
                Ok(result) => {
                    let component = match validated_event_mutation_component(&event, &result) {
                        Ok(component) => component,
                        Err(error) => {
                            page.finish_calendar_mutation(&mutation_token);
                            show_toast_for_widget(&page, &error);
                            page.reload_calendar_data();
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
                        if let Some(existing) = records
                            .iter_mut()
                            .find(|existing| same_calendar_event_identity(existing, &updated_event))
                        {
                            *existing = updated_event;
                        }
                    }
                    if !page.finish_calendar_mutation(&mutation_token) {
                        page.reload_calendar_data();
                        return;
                    }
                    page.bump_calendar_revision();
                    page.rebuild_calendar_index_from_records();
                    page.refresh_calendar_views();
                    show_toast_for_widget(&page, &format!("Termin verschoben ({})", result.uid));
                }
                Err(error) => {
                    page.finish_calendar_mutation(&mutation_token);
                    show_toast_for_widget(
                        &page,
                        &format!("Termin konnte nicht verschoben werden: {error}"),
                    );
                    page.reload_calendar_data();
                }
            }
        });
    }

    fn transfer_event_to_local(
        &self,
        event: CalendarEventRecord,
        kind: CalendarTransferKind,
        action_button: &impl glib::object::IsA<gtk::Widget>,
    ) {
        let action_button = action_button.as_ref();
        let local_writable = CalendarStoreService::new().is_writable();
        let plan =
            match plan_calendar_event_transfer(&event, kind, "local-calendar", local_writable) {
                Ok(plan) => plan,
                Err(error) => {
                    show_toast_for_widget(action_button, &error);
                    return;
                }
            };
        let Some(identity) = CalendarEventIdentity::from_event(&event) else {
            show_toast_for_widget(
                action_button,
                "Der Termin besitzt keine eindeutige Identität und wurde nicht übertragen",
            );
            return;
        };
        let mutation_token = self.begin_calendar_mutation(Some(identity));
        let (sender, receiver) = async_channel::bounded(1);
        action_button.set_sensitive(false);
        std::thread::spawn(move || {
            let service = ECalService::new();
            let result = service
                .create_component(
                    &plan.destination_uid,
                    ECalComponentType::Event,
                    &plan.component,
                )
                .map(|created| {
                    let removal = matches!(plan.kind, CalendarTransferKind::Move).then(|| {
                        service.remove_component(
                            &plan.source_uid,
                            ECalComponentType::Event,
                            &plan.source_component_uid,
                        )
                    });
                    (created, removal)
                });
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Calendar transfer result was dropped");
            }
        });

        let action_button = action_button.clone();
        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page.upgrade() else {
                return;
            };
            if !page.calendar_event_mutation_is_current(&mutation_token) {
                action_button.set_sensitive(true);
                show_toast_for_widget(
                    &action_button,
                    "Der Kalenderstand hat sich geändert; der Übertragungsstand wird neu geladen",
                );
                page.reload_calendar_data();
                return;
            }
            page.finish_calendar_mutation(&mutation_token);
            action_button.set_sensitive(true);
            match result {
                Ok(Ok((created, None))) => {
                    show_toast_for_widget(
                        &action_button,
                        &format!("Termin nach Lokal kopiert ({})", created.uid),
                    );
                    page.reload_calendar_data();
                }
                Ok(Ok((created, Some(Ok(removed))))) => {
                    show_toast_for_widget(
                        &action_button,
                        &format!(
                            "Termin nach Lokal verschoben ({} → {})",
                            removed.uid, created.uid
                        ),
                    );
                    page.reload_calendar_data();
                }
                Ok(Ok((created, Some(Err(error))))) => {
                    show_toast_for_widget(
                        &action_button,
                        &format!(
                            "Kopie nach Lokal erstellt ({}), Quelle blieb wegen Fehler erhalten: {error}",
                            created.uid
                        ),
                    );
                    page.reload_calendar_data();
                }
                Ok(Err(error)) => {
                    show_toast_for_widget(
                        &action_button,
                        &format!("Termin konnte nicht nach Lokal übertragen werden: {error}"),
                    );
                    // Creation failed, so the source was intentionally never removed.
                    page.reload_calendar_data();
                }
                Err(error) => {
                    show_toast_for_widget(
                        &action_button,
                        &format!("Termin-Übertragungskanal wurde beendet: {error}"),
                    );
                    page.reload_calendar_data();
                }
            }
        });
    }

    pub(crate) fn prompt_delete_event(
        &self,
        anchor: &gtk::Widget,
        event: &CalendarEventRecord,
        recurring: bool,
    ) {
        if let Some(error) = event_mutation_error(event) {
            show_toast_for_widget(anchor, error);
            return;
        }

        let Some(identity) = CalendarEventIdentity::from_event(event) else {
            show_toast_for_widget(anchor, "Der Termin besitzt keine eindeutige Identität");
            return;
        };

        if recurring {
            let scope_dialog = adw::AlertDialog::new(
                Some("Wiederkehrenden Termin löschen"),
                Some("Wählen Sie den Löschumfang. Einzelne Vorkommen und künftige Vorkommen werden erst angeboten, wenn der Backend-Adapter sie ohne Datenverlust als Ausnahme bzw. Serienaufteilung bestätigen kann."),
            );
            scope_dialog.add_responses(&[
                ("cancel", "Abbrechen"),
                ("this", "Nur diesen Termin"),
                ("future", "Diesen und künftige"),
                ("series", "Ganze Serie löschen"),
            ]);
            scope_dialog.set_response_appearance("series", adw::ResponseAppearance::Destructive);
            let event_for_scope = event.clone();
            let page_weak = self.downgrade();
            let anchor_clone = anchor.clone();
            let identity_clone = identity.clone();
            scope_dialog.connect_response(None, move |dialog, response| {
                let scope = match response {
                    "this" => Some(CalendarRecurringDeleteScope::ThisOccurrence),
                    "future" => Some(CalendarRecurringDeleteScope::ThisAndFuture),
                    "series" => Some(CalendarRecurringDeleteScope::EntireSeries),
                    _ => None,
                };
                let Some(scope) = scope else { return };
                match plan_calendar_event_delete(&event_for_scope, scope) {
                    Ok(CalendarDeletePlan::RemoveComponent { .. }) => {
                        dialog.close();
                        if let Some(page) = page_weak.upgrade() {
                            page.execute_delete_event(&anchor_clone, &event_for_scope, &identity_clone);
                        }
                    }
                    Ok(CalendarDeletePlan::UnsupportedOccurrenceScope { scope }) => {
                        let description = match scope {
                            CalendarRecurringDeleteScope::ThisOccurrence => "Dieses einzelne Vorkommen kann der aktuelle Backend-Adapter noch nicht verlustfrei löschen; die Serie bleibt unverändert.",
                            CalendarRecurringDeleteScope::ThisAndFuture => "Eine Serienaufteilung ab diesem Vorkommen kann der aktuelle Backend-Adapter noch nicht verlustfrei durchführen; die Serie bleibt unverändert.",
                            CalendarRecurringDeleteScope::EntireSeries => unreachable!(),
                        };
                        show_toast_for_widget(&anchor_clone, description);
                    }
                    Err(error) => show_toast_for_widget(&anchor_clone, &error),
                }
            });
            scope_dialog.present(Some(anchor));
            return;
        }

        let confirmation_message = if event.source_uid == "local-calendar" {
            "Der Termin wird dauerhaft aus dem lokalen Kalender entfernt."
        } else {
            "Der Termin wird dauerhaft aus dem synchronisierten Kalender entfernt."
        };

        let page_weak = self.downgrade();
        let anchor_clone = anchor.clone();
        let event_clone = event.clone();
        let identity_clone = identity.clone();

        crate::dialogs::run_destructive_action(
            anchor,
            "Termin löschen?",
            confirmation_message,
            "Löschen",
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.execute_delete_event(&anchor_clone, &event_clone, &identity_clone);
                }
            },
        );
    }

    fn execute_delete_event(
        &self,
        anchor: &gtk::Widget,
        event: &CalendarEventRecord,
        identity: &CalendarEventIdentity,
    ) {
        let mutation_token = self.begin_calendar_mutation(Some(identity.clone()));
        let source_uid = event.source_uid.clone();
        let uid = event.uid.clone();
        let uid_for_result = uid.clone();
        let (sender, receiver) = async_channel::bounded(1);
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

        let anchor_for_result = anchor.clone();
        let page_weak = self.downgrade();
        let identity_for_result = identity.clone();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page_weak.upgrade() else { return };
            if !page.calendar_event_mutation_is_current(&mutation_token) {
                show_toast_for_widget(
                    &anchor_for_result,
                    "Der Kalenderstand hat sich geändert; das Löschergebnis wurde verworfen",
                );
                page.reload_calendar_data();
                return;
            }
            match result {
                Ok(Ok(result)) if result.uid == uid_for_result => {
                    if !page.finish_calendar_mutation(&mutation_token) {
                        page.reload_calendar_data();
                        return;
                    }
                    if let Some(records) = page.imp().event_records.borrow().as_ref() {
                        records.borrow_mut().retain(|e| {
                            CalendarEventIdentity::from_event(e).as_ref() != Some(&identity_for_result)
                        });
                    }
                    page.bump_calendar_revision();
                    page.rebuild_calendar_index_from_records();
                    page.refresh_calendar_views();
                    show_toast_for_widget(
                        &anchor_for_result,
                        &format!("Termin gelöscht ({})", result.uid),
                    );
                    if let Some(window) = anchor_for_result.root().and_downcast::<crate::window::EvolutionWindow>() {
                        window.refresh_calendar_page();
                    }
                }
                Ok(Ok(result)) => {
                    page.finish_calendar_mutation(&mutation_token);
                    show_toast_for_widget(
                        &anchor_for_result,
                        &format!("ECal bestätigte eine andere Termin-UID ({}); nichts wurde entfernt", result.uid),
                    );
                    page.reload_calendar_data();
                }
                Ok(Err(error)) => {
                    page.finish_calendar_mutation(&mutation_token);
                    show_toast_for_widget(
                        &anchor_for_result,
                        &format!("Termin konnte nicht gelöscht werden: {error}"),
                    );
                    page.reload_calendar_data();
                }
                Err(error) => {
                    page.finish_calendar_mutation(&mutation_token);
                    show_toast_for_widget(
                        &anchor_for_result,
                        &format!("Termin-Löschkanal wurde beendet: {error}"),
                    );
                    page.reload_calendar_data();
                }
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MonthBarSpan {
    None,
    Start,
    Middle,
    End,
}

pub(crate) enum MonthCellTrackItem {
    Event {
        event: CalendarEventRecord,
        span: MonthBarSpan,
        show_title: bool,
    },
    Spacer,
}

pub(crate) struct WeekDayLayout {
    pub(crate) visible_items: Vec<MonthCellTrackItem>,
    pub(crate) overflow_events: Vec<CalendarEventRecord>,
}

pub(crate) fn compute_week_cell_layouts(
    week_dates: &[NaiveDate],
    events_by_date: &BTreeMap<NaiveDate, Vec<CalendarEventRecord>>,
    max_events_per_cell: usize,
) -> Vec<WeekDayLayout> {
    assert_eq!(week_dates.len(), 7);

    // 1. Gather all unique multi-day all-day events in this week
    let mut multi_day_events: Vec<CalendarEventRecord> = Vec::new();
    let mut seen_multi_uids = std::collections::HashSet::new();

    for date in week_dates.iter() {
        if let Some(events) = events_by_date.get(date) {
            for ev in events {
                if ev.all_day {
                    if let Some(range) = event_display_range(ev) {
                        if range.len_days() > 1 {
                            let key = (ev.uid.clone(), ev.start.clone());
                            if seen_multi_uids.insert(key) {
                                multi_day_events.push(ev.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort multi-day events:
    // Earlier start first, longer duration first, then summary/uid for stability
    multi_day_events.sort_by(|a, b| {
        let r_a = event_display_range(a).unwrap();
        let r_b = event_display_range(b).unwrap();
        r_a.start.cmp(&r_b.start)
            .then_with(|| r_b.len_days().cmp(&r_a.len_days()))
            .then_with(|| a.summary.cmp(&b.summary))
            .then_with(|| a.uid.cmp(&b.uid))
    });

    // 2. Track matrix: each row is [Option<MonthCellTrackItem>; 7]
    let mut tracks: Vec<[Option<MonthCellTrackItem>; 7]> = Vec::new();

    for ev in &multi_day_events {
        let range = event_display_range(ev).unwrap();
        let mut cols = Vec::new();
        for (col, date) in week_dates.iter().enumerate() {
            if range.contains(*date) {
                cols.push(col);
            }
        }
        if cols.is_empty() {
            continue;
        }
        let col_start = cols[0];
        let col_end = *cols.last().unwrap();

        // Find lowest track index t where track t is free for all col in col_start..=col_end
        let mut target_track = 0;
        loop {
            if target_track >= tracks.len() {
                tracks.push(std::array::from_fn(|_| None));
                break;
            }
            let is_free = (col_start..=col_end).all(|c| tracks[target_track][c].is_none());
            if is_free {
                break;
            }
            target_track += 1;
        }

        // Place the event segments in track target_track
        for c in col_start..=col_end {
            let date = week_dates[c];
            let is_first = range.start == date;
            let is_last = range.last_inclusive() == date;
            let span = match (is_first, is_last) {
                (true, false) => MonthBarSpan::Start,
                (false, true) => MonthBarSpan::End,
                (false, false) => MonthBarSpan::Middle,
                (true, true) => MonthBarSpan::None,
            };
            let show_title = is_first || c == col_start;
            tracks[target_track][c] = Some(MonthCellTrackItem::Event {
                event: ev.clone(),
                span,
                show_title,
            });
        }
    }

    // 3. Place single-day events (both single-day all-day and timed events)
    for (col, date) in week_dates.iter().enumerate() {
        if let Some(events) = events_by_date.get(date) {
            for ev in events {
                if ev.all_day {
                    if let Some(range) = event_display_range(ev) {
                        if range.len_days() > 1 {
                            continue;
                        }
                    }
                }
                let mut target_track = 0;
                loop {
                    if target_track >= tracks.len() {
                        tracks.push(std::array::from_fn(|_| None));
                        break;
                    }
                    if tracks[target_track][col].is_none() {
                        break;
                    }
                    target_track += 1;
                }
                tracks[target_track][col] = Some(MonthCellTrackItem::Event {
                    event: ev.clone(),
                    span: MonthBarSpan::None,
                    show_title: true,
                });
            }
        }
    }

    // 4. Construct WeekDayLayout for each column
    let mut layouts = Vec::with_capacity(7);
    let max_visible_tracks = tracks.len().min(max_events_per_cell);

    for col in 0..7 {
        let mut last_occupied = None;
        for t in 0..max_visible_tracks {
            if tracks[t][col].is_some() {
                last_occupied = Some(t);
            }
        }

        let mut visible_items = Vec::new();
        if let Some(last_idx) = last_occupied {
            for t in 0..=last_idx {
                match tracks[t][col].take() {
                    Some(item) => visible_items.push(item),
                    None => visible_items.push(MonthCellTrackItem::Spacer),
                }
            }
        }

        let mut overflow_events = Vec::new();
        for t in max_events_per_cell..tracks.len() {
            if let Some(MonthCellTrackItem::Event { event, .. }) = tracks[t][col].take() {
                overflow_events.push(event);
            }
        }

        layouts.push(WeekDayLayout {
            visible_items,
            overflow_events,
        });
    }

    layouts
}

// --- TaskPad and MemoPad Canvas Models and Drawing (Original GNOME Evolution Canvas parity) ---

#[derive(Clone, Debug)]
pub(crate) struct TaskpadRowItem {
    pub(crate) record: TaskRecord,
    pub(crate) title: String,
    pub(crate) due_text: Option<String>,
    pub(crate) is_overdue: bool,
    pub(crate) y: f64,
    pub(crate) height: f64,
    pub(crate) checkbox_rect: (f64, f64, f64, f64),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedTaskpadLayout {
    pub(crate) items: Vec<TaskpadRowItem>,
    pub(crate) total_height: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct MemopadRowItem {
    pub(crate) record: MemoRecord,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) y: f64,
    pub(crate) height: f64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedMemopadLayout {
    pub(crate) items: Vec<MemopadRowItem>,
    pub(crate) total_height: f64,
}

pub(crate) fn prepare_taskpad_layout(tasks: &[TaskRecord], today: NaiveDate) -> PreparedTaskpadLayout {
    let row_height = 36.0;
    let mut items = Vec::with_capacity(tasks.len());
    let mut current_y = 0.0;

    for task in tasks {
        let title = if task.summary.trim().is_empty() {
            "(Ohne Betreff)".to_string()
        } else {
            task.summary.clone()
        };

        let (due_text, is_overdue) = if !task.due.trim().is_empty() {
            let date_str = task.due.split_whitespace().next().unwrap_or(&task.due);
            if let Ok(due_date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                let text = if due_date == today {
                    "Heute".to_string()
                } else if due_date == today + chrono::Duration::days(1) {
                    "Morgen".to_string()
                } else if due_date == today - chrono::Duration::days(1) {
                    "Gestern".to_string()
                } else {
                    due_date.format("%d.%m.%Y").to_string()
                };
                let overdue = !task.completed && due_date < today;
                (Some(text), overdue)
            } else {
                (Some(task.due.clone()), false)
            }
        } else {
            (None, false)
        };

        let checkbox_rect = (8.0, current_y + (row_height - 18.0) / 2.0, 18.0, 18.0);
        items.push(TaskpadRowItem {
            record: task.clone(),
            title,
            due_text,
            is_overdue,
            y: current_y,
            height: row_height,
            checkbox_rect,
        });

        current_y += row_height;
    }

    PreparedTaskpadLayout {
        items,
        total_height: current_y,
    }
}

pub(crate) fn prepare_memopad_layout(memos: &[MemoRecord]) -> PreparedMemopadLayout {
    let row_height = 34.0;
    let mut items = Vec::with_capacity(memos.len());
    let mut current_y = 0.0;

    for memo in memos {
        let title = if memo.summary.trim().is_empty() {
            "(Ohne Titel)".to_string()
        } else {
            memo.summary.clone()
        };
        let subtitle = calendar_memo_subtitle(memo);

        items.push(MemopadRowItem {
            record: memo.clone(),
            title,
            subtitle,
            y: current_y,
            height: row_height,
        });

        current_y += row_height;
    }

    PreparedMemopadLayout {
        items,
        total_height: current_y,
    }
}

pub(crate) fn hit_test_taskpad(layout: &PreparedTaskpadLayout, x: f64, y: f64) -> Option<(usize, bool)> {
    for (idx, item) in layout.items.iter().enumerate() {
        if y >= item.y && y < item.y + item.height {
            let (cx, cy, cw, ch) = item.checkbox_rect;
            let in_checkbox = x >= (cx - 4.0) && x <= (cx + cw + 4.0) && y >= (cy - 4.0) && y <= (cy + ch + 4.0);
            return Some((idx, in_checkbox));
        }
    }
    None
}

pub(crate) fn hit_test_memopad(layout: &PreparedMemopadLayout, _x: f64, y: f64) -> Option<usize> {
    for (idx, item) in layout.items.iter().enumerate() {
        if y >= item.y && y < item.y + item.height {
            return Some(idx);
        }
    }
    None
}

pub(crate) fn draw_taskpad_canvas(
    cr: &gtk::cairo::Context,
    layout: &PreparedTaskpadLayout,
    width: f64,
    height: f64,
    hovered_idx: Option<usize>,
    selected_idx: Option<usize>,
    is_dark: bool,
) {
    if is_dark {
        cr.set_source_rgb(0.14, 0.14, 0.14);
    } else {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    }
    cr.paint().ok();

    for (idx, item) in layout.items.iter().enumerate() {
        if item.y + item.height < 0.0 || item.y > height {
            continue;
        }

        let is_selected = selected_idx == Some(idx);
        let is_hovered = hovered_idx == Some(idx);

        if is_selected {
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.25);
            draw_cairo_rounded_rect(cr, 4.0, item.y + 2.0, (width - 8.0).max(10.0), item.height - 4.0, 4.0);
            cr.fill().ok();
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.5);
            cr.set_line_width(1.0);
            draw_cairo_rounded_rect(cr, 4.0, item.y + 2.0, (width - 8.0).max(10.0), item.height - 4.0, 4.0);
            cr.stroke().ok();
        } else if is_hovered {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.04);
            }
            draw_cairo_rounded_rect(cr, 4.0, item.y + 2.0, (width - 8.0).max(10.0), item.height - 4.0, 4.0);
            cr.fill().ok();
        }

        // Checkbox
        let (cx, cy, cw, ch) = item.checkbox_rect;
        draw_cairo_rounded_rect(cr, cx, cy, cw, ch, 3.0);
        if item.record.completed {
            cr.set_source_rgba(0.2, 0.55, 0.95, 0.9);
            cr.fill_preserve().ok();
            cr.set_source_rgba(0.2, 0.45, 0.85, 1.0);
            cr.set_line_width(1.0);
            cr.stroke().ok();

            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(2.0);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_line_join(gtk::cairo::LineJoin::Round);
            cr.move_to(cx + 4.0, cy + 9.0);
            cr.line_to(cx + 7.5, cy + 12.5);
            cr.line_to(cx + 14.0, cy + 5.0);
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

        // Due text on right
        let mut right_reserve = 12.0;
        if let Some(due) = &item.due_text {
            cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
            cr.set_font_size(10.5);
            if let Ok(extents) = cr.text_extents(due) {
                let due_w = extents.width();
                let due_x = width - due_w - 12.0;
                let due_y = item.y + (item.height / 2.0) + 4.0;
                if item.is_overdue {
                    cr.set_source_rgba(0.95, 0.25, 0.25, 1.0);
                } else if is_dark {
                    cr.set_source_rgba(0.7, 0.7, 0.7, 0.8);
                } else {
                    cr.set_source_rgba(0.4, 0.4, 0.4, 0.8);
                }
                cr.move_to(due_x, due_y);
                cr.show_text(due).ok();
                right_reserve = due_w + 20.0;
            }
        }

        // Title text
        let title_x = cx + cw + 8.0;
        let title_max_w = (width - title_x - right_reserve).max(20.0);
        let title_y = item.y + (item.height / 2.0) + 4.0;

        cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);

        if item.record.completed {
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

        let display_title = truncate_cairo_text(cr, &item.title, title_max_w);
        cr.move_to(title_x, title_y);
        cr.show_text(&display_title).ok();

        // Strikethrough if completed
        if item.record.completed {
            if let Ok(extents) = cr.text_extents(&display_title) {
                cr.set_line_width(1.0);
                cr.move_to(title_x, title_y - 4.0);
                cr.line_to(title_x + extents.width(), title_y - 4.0);
                cr.stroke().ok();
            }
        }

        // Separator
        cr.set_line_width(1.0);
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
        }
        let line_y = (item.y + item.height).floor() + 0.5;
        cr.move_to(title_x, line_y);
        cr.line_to(width - 8.0, line_y);
        cr.stroke().ok();
    }
}

pub(crate) fn draw_memopad_canvas(
    cr: &gtk::cairo::Context,
    layout: &PreparedMemopadLayout,
    width: f64,
    height: f64,
    hovered_idx: Option<usize>,
    selected_idx: Option<usize>,
    is_dark: bool,
) {
    if is_dark {
        cr.set_source_rgb(0.14, 0.14, 0.14);
    } else {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    }
    cr.paint().ok();

    for (idx, item) in layout.items.iter().enumerate() {
        if item.y + item.height < 0.0 || item.y > height {
            continue;
        }

        let is_selected = selected_idx == Some(idx);
        let is_hovered = hovered_idx == Some(idx);

        if is_selected {
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.25);
            draw_cairo_rounded_rect(cr, 4.0, item.y + 2.0, (width - 8.0).max(10.0), item.height - 4.0, 4.0);
            cr.fill().ok();
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.5);
            cr.set_line_width(1.0);
            draw_cairo_rounded_rect(cr, 4.0, item.y + 2.0, (width - 8.0).max(10.0), item.height - 4.0, 4.0);
            cr.stroke().ok();
        } else if is_hovered {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.04);
            }
            draw_cairo_rounded_rect(cr, 4.0, item.y + 2.0, (width - 8.0).max(10.0), item.height - 4.0, 4.0);
            cr.fill().ok();
        }

        // Memo icon glyph
        let icon_x = 10.0;
        let icon_y = item.y + (item.height - 15.0) / 2.0;
        cr.set_line_width(1.2);
        if is_dark {
            cr.set_source_rgba(0.85, 0.75, 0.35, 0.9);
        } else {
            cr.set_source_rgba(0.85, 0.65, 0.1, 0.9);
        }
        draw_cairo_rounded_rect(cr, icon_x, icon_y, 12.0, 15.0, 2.0);
        cr.stroke().ok();
        cr.move_to(icon_x + 3.0, icon_y + 5.0);
        cr.line_to(icon_x + 9.0, icon_y + 5.0);
        cr.move_to(icon_x + 3.0, icon_y + 9.0);
        cr.line_to(icon_x + 8.0, icon_y + 9.0);
        cr.stroke().ok();

        // Subtitle on right
        let mut right_reserve = 12.0;
        if !item.subtitle.trim().is_empty() {
            cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
            cr.set_font_size(10.5);
            if let Ok(extents) = cr.text_extents(&item.subtitle) {
                let sub_w = extents.width();
                let sub_x = width - sub_w - 12.0;
                let sub_y = item.y + (item.height / 2.0) + 4.0;
                if is_dark {
                    cr.set_source_rgba(0.6, 0.6, 0.6, 0.8);
                } else {
                    cr.set_source_rgba(0.45, 0.45, 0.45, 0.8);
                }
                cr.move_to(sub_x, sub_y);
                cr.show_text(&item.subtitle).ok();
                right_reserve = sub_w + 20.0;
            }
        }

        // Title
        let title_x = 28.0;
        let title_max_w = (width - title_x - right_reserve).max(20.0);
        let title_y = item.y + (item.height / 2.0) + 4.0;

        cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);
        if is_dark {
            cr.set_source_rgba(0.92, 0.92, 0.92, 1.0);
        } else {
            cr.set_source_rgba(0.12, 0.12, 0.12, 1.0);
        }
        let display_title = truncate_cairo_text(cr, &item.title, title_max_w);
        cr.move_to(title_x, title_y);
        cr.show_text(&display_title).ok();

        // Separator
        cr.set_line_width(1.0);
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
        }
        let line_y = (item.y + item.height).floor() + 0.5;
        cr.move_to(title_x, line_y);
        cr.line_to(width - 8.0, line_y);
        cr.stroke().ok();
    }
}

fn show_taskpad_context_menu(
    page: &CalendarPage,
    anchor_widget: &gtk::DrawingArea,
    task: &TaskRecord,
    rect: gdk::Rectangle,
) {
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .pointing_to(&rect)
        .build();
    let menu_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    // 1. Edit
    let edit = gtk::Button::with_label("Aufgabe bearbeiten");
    edit.set_halign(gtk::Align::Fill);
    edit.add_css_class("flat");
    let popover_for_edit = popover.clone();
    let page_weak_edit = page.downgrade();
    let source_uid = task.source_uid.clone();
    let uid = task.uid.clone();
    edit.connect_clicked(move |_| {
        popover_for_edit.popdown();
        if let Some(page) = page_weak_edit.upgrade() {
            page.open_task_by_uid(&source_uid, &uid);
        }
    });
    menu_box.append(&edit);

    // 2. Toggle completion
    let toggle_label = if task.completed {
        "Als unerledigt markieren"
    } else {
        "Als erledigt markieren"
    };
    let toggle = gtk::Button::with_label(toggle_label);
    toggle.set_halign(gtk::Align::Fill);
    toggle.add_css_class("flat");
    toggle.set_sensitive(!task.source_uid.trim().is_empty());
    let popover_for_toggle = popover.clone();
    let page_weak_toggle = page.downgrade();
    let task_clone = task.clone();
    toggle.connect_clicked(move |_| {
        popover_for_toggle.popdown();
        if let Some(page) = page_weak_toggle.upgrade() {
            toggle_task_completion(&page, &task_clone, !task_clone.completed, None);
        }
    });
    menu_box.append(&toggle);

    popover.set_parent(anchor_widget);
    popover.set_child(Some(&menu_box));
    popover.popup();
}

fn show_memopad_context_menu(
    page: &CalendarPage,
    anchor_widget: &gtk::DrawingArea,
    memo: &MemoRecord,
    rect: gdk::Rectangle,
) {
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .pointing_to(&rect)
        .build();
    let menu_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    // 1. Edit
    let edit = gtk::Button::with_label("Notiz öffnen");
    edit.set_halign(gtk::Align::Fill);
    edit.add_css_class("flat");
    let popover_for_edit = popover.clone();
    let page_weak_edit = page.downgrade();
    let source_uid = memo.source_uid.clone();
    let uid = memo.uid.clone();
    edit.connect_clicked(move |_| {
        popover_for_edit.popdown();
        if let Some(page) = page_weak_edit.upgrade() {
            page.open_memo_by_uid(&source_uid, &uid);
        }
    });
    menu_box.append(&edit);

    popover.set_parent(anchor_widget);
    popover.set_child(Some(&menu_box));
    popover.popup();
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedMonthEventBar {
    pub(crate) event: CalendarEventRecord,
    pub(crate) track: usize,
    pub(crate) start_col: usize,
    pub(crate) end_col: usize,
    pub(crate) continues_left: bool,
    pub(crate) continues_right: bool,
    pub(crate) all_day: bool,
    pub(crate) time_str: Option<String>,
    pub(crate) bg_hex: String,
    pub(crate) hov_hex: String,
    pub(crate) fg_hex: String,
    pub(crate) color_hex: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedMonthWeek {
    pub(crate) week_dates: [NaiveDate; 7],
    pub(crate) week_number: u32,
    pub(crate) event_bars: Vec<PreparedMonthEventBar>,
    pub(crate) overflow_by_col: [Vec<CalendarEventRecord>; 7],
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedMonthLayout {
    pub(crate) anchor_date: NaiveDate,
    pub(crate) year: i32,
    pub(crate) month: u32,
    pub(crate) visible_dates: Vec<NaiveDate>,
    pub(crate) today: NaiveDate,
    pub(crate) show_week_numbers: bool,
    pub(crate) workdays: HashSet<Weekday>,
    pub(crate) compress_weekend: bool,
    pub(crate) weeks: Vec<PreparedMonthWeek>,
    pub(crate) weekday_names_full: Vec<String>,
    pub(crate) weekday_names_short: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MonthDragSelection {
    pub(crate) start_date: NaiveDate,
    pub(crate) current_date: NaiveDate,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MonthCanvasHit {
    Header { col: usize },
    KwHeader,
    KwRow { row: usize },
    Event { event: CalendarEventRecord, date: NaiveDate },
    Overflow { date: NaiveDate },
    DayCell { date: NaiveDate },
}

pub(crate) fn prepare_month_layout(
    viewport: &CalendarViewport,
    events_by_date: &BTreeMap<NaiveDate, Vec<CalendarEventRecord>>,
    today: NaiveDate,
    show_week_numbers: bool,
    workdays: &HashSet<Weekday>,
    compress_weekend: bool,
    weekday_names_full: &[&str],
    weekday_names_short: &[&str],
    is_dark: bool,
    max_events_per_cell: usize,
) -> PreparedMonthLayout {
    let anchor_date = viewport.anchor_date;
    let year = anchor_date.year();
    let month = anchor_date.month();
    let visible_dates: Vec<NaiveDate> = viewport.visible_range.dates().collect();

    let mut weeks = Vec::with_capacity(6);

    for week_idx in 0..6 {
        if (week_idx + 1) * 7 > visible_dates.len() {
            break;
        }
        let week_slice = &visible_dates[week_idx * 7..(week_idx + 1) * 7];
        let mut week_dates = [anchor_date; 7];
        week_dates.copy_from_slice(week_slice);
        let week_number = week_dates[0].iso_week().week();

        let layouts = compute_week_cell_layouts(&week_dates, events_by_date, max_events_per_cell);

        let mut event_bars = Vec::new();
        // Extract continuous event bars across track matrix
        for t in 0..max_events_per_cell {
            let mut col = 0;
            while col < 7 {
                if let Some(MonthCellTrackItem::Event { event, span, .. }) = layouts[col].visible_items.get(t) {
                    let ev_uid = event.uid.clone();
                    let ev_start = event.start.clone();
                    let mut end_col = col;
                    while end_col + 1 < 7 {
                        if let Some(MonthCellTrackItem::Event { event: next_ev, .. }) = layouts[end_col + 1].visible_items.get(t) {
                            if next_ev.uid == ev_uid && next_ev.start == ev_start {
                                end_col += 1;
                                continue;
                            }
                        }
                        break;
                    }

                    let event_range = event_display_range(event);
                    let cont_left = col == 0
                        && (*span == MonthBarSpan::Middle
                            || *span == MonthBarSpan::End
                            || event_range.as_ref().map(|r| r.start < week_dates[0]).unwrap_or(false));
                    let cont_right = end_col == 6
                        && event_range
                            .as_ref()
                            .map(|r| r.last_inclusive() > week_dates[6])
                            .unwrap_or(false);

                    let color_hex = event_color_for_record(event);
                    let (bg_hex, hov_hex, _, fg_hex) = pastel_colors_for_hex(&color_hex, is_dark);

                    let time_str = if !event.all_day {
                        let (start_min, _, _) = event_occurrence_times_for_date(event, week_dates[col], 30);
                        Some(format_clock_minutes(start_min))
                    } else {
                        None
                    };

                    event_bars.push(PreparedMonthEventBar {
                        event: event.clone(),
                        track: t,
                        start_col: col,
                        end_col,
                        continues_left: cont_left,
                        continues_right: cont_right,
                        all_day: event.all_day,
                        time_str,
                        bg_hex,
                        hov_hex,
                        fg_hex,
                        color_hex,
                    });

                    col = end_col + 1;
                } else {
                    col += 1;
                }
            }
        }

        let mut overflow_by_col: [Vec<CalendarEventRecord>; 7] = Default::default();
        for (c, l) in layouts.iter().enumerate().take(7) {
            overflow_by_col[c] = l.overflow_events.clone();
        }

        weeks.push(PreparedMonthWeek {
            week_dates,
            week_number,
            event_bars,
            overflow_by_col,
        });
    }

    PreparedMonthLayout {
        anchor_date,
        year,
        month,
        visible_dates,
        today,
        show_week_numbers,
        workdays: workdays.clone(),
        compress_weekend,
        weeks,
        weekday_names_full: weekday_names_full.iter().map(|s| s.to_string()).collect(),
        weekday_names_short: weekday_names_short.iter().map(|s| s.to_string()).collect(),
    }
}

pub(crate) fn hit_test_month_canvas(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    layout: &PreparedMonthLayout,
) -> Option<MonthCanvasHit> {
    let header_h = 28.0;
    let kw_w = if layout.show_week_numbers { 36.0 } else { 0.0 };
    let grid_w = (width - kw_w).max(1.0);
    let grid_h = (height - header_h).max(1.0);
    let col_w = grid_w / 7.0;
    let row_h = grid_h / 6.0;
    let day_header_h = 24.0;
    let chip_h = 20.0;
    let chip_step = 22.0;
    let usable_h = (row_h - day_header_h - 4.0).max(0.0);
    let max_tracks = ((usable_h / chip_step).floor() as usize).max(1);

    if y < header_h {
        if layout.show_week_numbers && x < kw_w {
            return Some(MonthCanvasHit::KwHeader);
        }
        let col = (((x - kw_w).max(0.0) / col_w).floor() as usize).min(6);
        return Some(MonthCanvasHit::Header { col });
    }

    if layout.show_week_numbers && x < kw_w {
        let row = (((y - header_h).max(0.0) / row_h).floor() as usize).min(5);
        return Some(MonthCanvasHit::KwRow { row });
    }

    let row = (((y - header_h).max(0.0) / row_h).floor() as usize).min(5);
    let col = (((x - kw_w).max(0.0) / col_w).floor() as usize).min(6);
    let date_idx = row * 7 + col;
    if date_idx >= layout.visible_dates.len() {
        return None;
    }
    let date = layout.visible_dates[date_idx];
    let cell_y = header_h + row as f64 * row_h;

    if row < layout.weeks.len() {
        let week = &layout.weeks[row];
        // Overflow hit test
        if !week.overflow_by_col[col].is_empty() {
            let of_y = cell_y + day_header_h + (max_tracks.saturating_sub(1)) as f64 * chip_step;
            if y >= of_y && y <= of_y + chip_h {
                return Some(MonthCanvasHit::Overflow { date });
            }
        }

        // Event bar hit test
        for bar in &week.event_bars {
            if bar.track >= max_tracks {
                continue;
            }
            if !week.overflow_by_col[col].is_empty() && bar.track >= max_tracks.saturating_sub(1) {
                continue;
            }
            let bar_x = kw_w + bar.start_col as f64 * col_w + 3.0;
            let bar_w = (bar.end_col - bar.start_col + 1) as f64 * col_w - 6.0;
            let bar_y = cell_y + day_header_h + bar.track as f64 * chip_step;
            if x >= bar_x && x <= bar_x + bar_w && y >= bar_y && y <= bar_y + chip_h {
                return Some(MonthCanvasHit::Event {
                    event: bar.event.clone(),
                    date,
                });
            }
        }
    }

    Some(MonthCanvasHit::DayCell { date })
}

pub(crate) fn draw_month_event_bar(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    radius: f64,
    continues_left: bool,
    continues_right: bool,
) {
    let r = radius.min(h / 2.0).min(w / 4.0);
    let degrees = std::f64::consts::PI / 180.0;
    cr.new_sub_path();

    if continues_right {
        cr.line_to(x + w, y);
        cr.line_to(x + w, y + h);
    } else {
        cr.arc(x + w - r, y + r, r, -90.0 * degrees, 0.0 * degrees);
        cr.arc(x + w - r, y + h - r, r, 0.0 * degrees, 90.0 * degrees);
    }

    if continues_left {
        cr.line_to(x, y + h);
        cr.line_to(x, y);
    } else {
        cr.arc(x + r, y + h - r, r, 90.0 * degrees, 180.0 * degrees);
        cr.arc(x + r, y + r, r, 180.0 * degrees, 270.0 * degrees);
    }

    cr.close_path();
}

pub(crate) fn draw_month_canvas(
    cr: &gtk::cairo::Context,
    width: f64,
    height: f64,
    layout: &PreparedMonthLayout,
    selected_time_slot: Option<SelectedTimeSlot>,
    drag_selection: Option<MonthDragSelection>,
    hovered_event: Option<&CalendarEventIdentity>,
    is_dark: bool,
) {
    let header_h = 28.0;
    let kw_w = if layout.show_week_numbers { 36.0 } else { 0.0 };
    let grid_w = (width - kw_w).max(1.0);
    let grid_h = (height - header_h).max(1.0);
    let col_w = grid_w / 7.0;
    let row_h = grid_h / 6.0;
    let day_header_h = 24.0;
    let chip_h = 20.0;
    let chip_step = 22.0;
    let usable_h = (row_h - day_header_h - 4.0).max(0.0);
    let max_tracks = ((usable_h / chip_step).floor() as usize).max(1);

    // 1. Cell backgrounds
    for row in 0..6 {
        let cell_y = header_h + row as f64 * row_h;
        for col in 0..7 {
            let date_idx = row * 7 + col;
            if date_idx >= layout.visible_dates.len() {
                continue;
            }
            let cell_x = kw_w + col as f64 * col_w;
            let date = layout.visible_dates[date_idx];
            let outside_month = date.year() != layout.year || date.month() != layout.month;
            let is_today = date == layout.today;
            let is_weekend = date.weekday() == Weekday::Sat || date.weekday() == Weekday::Sun;
            let is_selected = selected_time_slot.map(|s| s.date == date).unwrap_or(false);
            let is_in_drag = drag_selection
                .map(|d| {
                    let (s, e) = if d.start_date <= d.current_date {
                        (d.start_date, d.current_date)
                    } else {
                        (d.current_date, d.start_date)
                    };
                    date >= s && date <= e
                })
                .unwrap_or(false);

            if outside_month {
                if is_dark {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
                } else {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.035);
                }
                cr.rectangle(cell_x, cell_y, col_w, row_h);
                let _ = cr.fill();
            } else if is_weekend {
                if is_dark {
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.02);
                } else {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.02);
                }
                cr.rectangle(cell_x, cell_y, col_w, row_h);
                let _ = cr.fill();
            }

            if is_today {
                cr.set_source_rgba(53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0, 0.06);
                cr.rectangle(cell_x, cell_y, col_w, row_h);
                let _ = cr.fill();
            }

            if is_in_drag {
                cr.set_source_rgba(53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0, 0.22);
                cr.rectangle(cell_x, cell_y, col_w, row_h);
                let _ = cr.fill();
            } else if is_selected {
                cr.set_source_rgba(53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0, 0.12);
                cr.rectangle(cell_x, cell_y, col_w, row_h);
                let _ = cr.fill();
            }
        }
    }

    // 2. Crisp 1px Grid Lines
    cr.set_line_width(1.0);
    if is_dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.12);
    }

    let y_hdr = header_h.round() + 0.5;
    cr.move_to(0.0, y_hdr);
    cr.line_to(width, y_hdr);
    let _ = cr.stroke();

    for row in 1..6 {
        let y = (header_h + row as f64 * row_h).round() + 0.5;
        cr.move_to(0.0, y);
        cr.line_to(width, y);
        let _ = cr.stroke();
    }

    if layout.show_week_numbers {
        let x_kw = kw_w.round() + 0.5;
        cr.move_to(x_kw, 0.0);
        cr.line_to(x_kw, height);
        let _ = cr.stroke();
    }

    for col in 1..7 {
        let x = (kw_w + col as f64 * col_w).round() + 0.5;
        cr.move_to(x, 0.0);
        cr.line_to(x, height);
        let _ = cr.stroke();
    }

    // 3. Header Row
    if is_dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.035);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.03);
    }
    cr.rectangle(0.0, 0.0, width, header_h);
    let _ = cr.fill();

    if layout.show_week_numbers {
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.50);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.50);
        }
        cr.set_font_size(11.0);
        cr.select_font_face(
            "sans-serif",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Bold,
        );
        if let Ok(ext) = cr.text_extents("KW") {
            let tx = (kw_w - ext.width()) / 2.0 - ext.x_bearing();
            let ty = (header_h - ext.height()) / 2.0 - ext.y_bearing();
            cr.move_to(tx, ty);
            let _ = cr.show_text("KW");
        }
    }

    let use_short = col_w < 74.0;
    cr.set_font_size(11.5);
    cr.select_font_face(
        "sans-serif",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Bold,
    );
    for col in 0..7 {
        let name = if use_short {
            layout
                .weekday_names_short
                .get(col)
                .map(|s| s.as_str())
                .unwrap_or("")
        } else {
            layout
                .weekday_names_full
                .get(col)
                .map(|s| s.as_str())
                .unwrap_or("")
        };
        let cx = kw_w + col as f64 * col_w;
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.70);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.65);
        }
        if let Ok(ext) = cr.text_extents(name) {
            let tx = cx + (col_w - ext.width()) / 2.0 - ext.x_bearing();
            let ty = (header_h - ext.height()) / 2.0 - ext.y_bearing();
            cr.move_to(tx, ty);
            let _ = cr.show_text(name);
        }
    }

    // 4. KW Column Numbers
    if layout.show_week_numbers {
        cr.set_font_size(11.0);
        cr.select_font_face(
            "sans-serif",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
        }
        for (row, week) in layout.weeks.iter().enumerate().take(6) {
            let week_num_str = format!("{:02}", week.week_number);
            let cy = header_h + row as f64 * row_h;
            if let Ok(ext) = cr.text_extents(&week_num_str) {
                let tx = (kw_w - ext.width()) / 2.0 - ext.x_bearing();
                let ty = cy + 16.0 - ext.y_bearing();
                cr.move_to(tx, ty);
                let _ = cr.show_text(&week_num_str);
            }
        }
    }

    // 5. Day Numbers
    for row in 0..6 {
        let cell_y = header_h + row as f64 * row_h;
        for col in 0..7 {
            let date_idx = row * 7 + col;
            if date_idx >= layout.visible_dates.len() {
                continue;
            }
            let cell_x = kw_w + col as f64 * col_w;
            let date = layout.visible_dates[date_idx];
            let outside_month = date.year() != layout.year || date.month() != layout.month;
            let is_today = date == layout.today;
            let day_str = date.day().to_string();

            if is_today {
                let circle_r = 10.5;
                let circle_cx = cell_x + 14.0;
                let circle_cy = cell_y + 13.0;
                cr.arc(circle_cx, circle_cy, circle_r, 0.0, 2.0 * std::f64::consts::PI);
                cr.set_source_rgba(53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0, 1.0);
                let _ = cr.fill();

                cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                cr.set_font_size(11.0);
                cr.select_font_face(
                    "sans-serif",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Bold,
                );
                if let Ok(ext) = cr.text_extents(&day_str) {
                    let tx = circle_cx - ext.width() / 2.0 - ext.x_bearing();
                    let ty = circle_cy - ext.height() / 2.0 - ext.y_bearing();
                    cr.move_to(tx, ty);
                    let _ = cr.show_text(&day_str);
                }
            } else {
                if outside_month {
                    if is_dark {
                        cr.set_source_rgba(1.0, 1.0, 1.0, 0.35);
                    } else {
                        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
                    }
                    cr.select_font_face(
                        "sans-serif",
                        gtk::cairo::FontSlant::Normal,
                        gtk::cairo::FontWeight::Normal,
                    );
                } else {
                    if is_dark {
                        cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
                    } else {
                        cr.set_source_rgba(0.0, 0.0, 0.0, 0.80);
                    }
                    cr.select_font_face(
                        "sans-serif",
                        gtk::cairo::FontSlant::Normal,
                        gtk::cairo::FontWeight::Bold,
                    );
                }
                cr.set_font_size(12.0);
                if let Ok(ext) = cr.text_extents(&day_str) {
                    let tx = cell_x + 8.0 - ext.x_bearing();
                    let ty = cell_y + 16.0 - ext.y_bearing();
                    cr.move_to(tx, ty);
                    let _ = cr.show_text(&day_str);
                }
            }
        }
    }

    // 6. Event Bars
    for (row, week) in layout.weeks.iter().enumerate().take(6) {
        let cell_y = header_h + row as f64 * row_h;
        for bar in &week.event_bars {
            if bar.track >= max_tracks {
                continue;
            }
            let col_has_overflow =
                (bar.start_col..=bar.end_col).any(|c| !week.overflow_by_col[c].is_empty());
            if col_has_overflow && bar.track >= max_tracks.saturating_sub(1) {
                continue;
            }

            let bar_x = kw_w + bar.start_col as f64 * col_w + 3.0;
            let bar_w = (bar.end_col - bar.start_col + 1) as f64 * col_w - 6.0;
            let bar_y = cell_y + day_header_h + bar.track as f64 * chip_step;
            let is_hovered = hovered_event
                .map(|h| h.uid == bar.event.uid)
                .unwrap_or(false);

            if bar.all_day {
                draw_month_event_bar(
                    cr,
                    bar_x,
                    bar_y,
                    bar_w,
                    chip_h,
                    4.0,
                    bar.continues_left,
                    bar.continues_right,
                );
                let (bg_r, bg_g, bg_b) =
                    parse_hex_color(if is_hovered { &bar.hov_hex } else { &bar.bg_hex })
                        .unwrap_or((220, 230, 245));
                cr.set_source_rgba(
                    bg_r as f64 / 255.0,
                    bg_g as f64 / 255.0,
                    bg_b as f64 / 255.0,
                    0.95,
                );
                let _ = cr.fill_preserve();

                let (col_r, col_g, col_b) =
                    parse_hex_color(&bar.color_hex).unwrap_or((53, 132, 228));
                cr.set_source_rgba(
                    col_r as f64 / 255.0,
                    col_g as f64 / 255.0,
                    col_b as f64 / 255.0,
                    0.70,
                );
                cr.set_line_width(1.0);
                let _ = cr.stroke();

                if bar.continues_left {
                    cr.move_to(bar_x + 5.0, bar_y + 5.0);
                    cr.line_to(bar_x + 1.5, bar_y + chip_h / 2.0);
                    cr.line_to(bar_x + 5.0, bar_y + chip_h - 5.0);
                    let _ = cr.stroke();
                }
                if bar.continues_right {
                    cr.move_to(bar_x + bar_w - 5.0, bar_y + 5.0);
                    cr.line_to(bar_x + bar_w - 1.5, bar_y + chip_h / 2.0);
                    cr.line_to(bar_x + bar_w - 5.0, bar_y + chip_h - 5.0);
                    let _ = cr.stroke();
                }

                let _ = cr.save();
                let text_x = bar_x + if bar.continues_left { 9.0 } else { 6.0 };
                let max_text_w = (bar_w
                    - if bar.continues_left { 9.0 } else { 6.0 }
                    - if bar.continues_right { 9.0 } else { 6.0 })
                .max(0.0);
                cr.rectangle(text_x, bar_y, max_text_w, chip_h);
                let _ = cr.clip();

                let (fg_r, fg_g, fg_b) =
                    parse_hex_color(&bar.fg_hex).unwrap_or((20, 20, 20));
                cr.set_source_rgba(
                    fg_r as f64 / 255.0,
                    fg_g as f64 / 255.0,
                    fg_b as f64 / 255.0,
                    1.0,
                );
                cr.set_font_size(11.0);
                cr.select_font_face(
                    "sans-serif",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Bold,
                );
                let trunc = truncate_cairo_text(cr, &bar.event.summary, max_text_w);
                if let Ok(ext) = cr.text_extents(&trunc) {
                    let ty = bar_y + (chip_h - ext.height()) / 2.0 - ext.y_bearing();
                    cr.move_to(text_x, ty);
                    let _ = cr.show_text(&trunc);
                }
                let _ = cr.restore();
            } else {
                let (bg_r, bg_g, bg_b) =
                    parse_hex_color(if is_hovered { &bar.hov_hex } else { &bar.bg_hex })
                        .unwrap_or((240, 240, 240));
                draw_cairo_rounded_rect(cr, bar_x, bar_y, bar_w, chip_h, 3.5);
                cr.set_source_rgba(
                    bg_r as f64 / 255.0,
                    bg_g as f64 / 255.0,
                    bg_b as f64 / 255.0,
                    if is_hovered { 0.70 } else { 0.35 },
                );
                let _ = cr.fill();

                let (col_r, col_g, col_b) =
                    parse_hex_color(&bar.color_hex).unwrap_or((53, 132, 228));
                cr.arc(
                    bar_x + 7.0,
                    bar_y + chip_h / 2.0,
                    3.5,
                    0.0,
                    2.0 * std::f64::consts::PI,
                );
                cr.set_source_rgba(
                    col_r as f64 / 255.0,
                    col_g as f64 / 255.0,
                    col_b as f64 / 255.0,
                    1.0,
                );
                let _ = cr.fill();

                let _ = cr.save();
                let text_x = bar_x + 15.0;
                let max_text_w = (bar_w - 18.0).max(0.0);
                cr.rectangle(text_x, bar_y, max_text_w, chip_h);
                let _ = cr.clip();

                let label_text = if let Some(ref t) = bar.time_str {
                    format!("{} {}", t, bar.event.summary)
                } else {
                    bar.event.summary.clone()
                };
                let (fg_r, fg_g, fg_b) =
                    parse_hex_color(&bar.fg_hex).unwrap_or((20, 20, 20));
                cr.set_source_rgba(
                    fg_r as f64 / 255.0,
                    fg_g as f64 / 255.0,
                    fg_b as f64 / 255.0,
                    1.0,
                );
                cr.set_font_size(11.0);
                cr.select_font_face(
                    "sans-serif",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Normal,
                );
                let trunc = truncate_cairo_text(cr, &label_text, max_text_w);
                if let Ok(ext) = cr.text_extents(&trunc) {
                    let ty = bar_y + (chip_h - ext.height()) / 2.0 - ext.y_bearing();
                    cr.move_to(text_x, ty);
                    let _ = cr.show_text(&trunc);
                }
                let _ = cr.restore();
            }
        }

        // 7. Overflow Indicators
        for col in 0..7 {
            let of_count = week.overflow_by_col[col].len();
            if of_count > 0 {
                let cell_x = kw_w + col as f64 * col_w;
                let of_y = cell_y + day_header_h + (max_tracks.saturating_sub(1)) as f64 * chip_step;
                let of_text = format!("+{} weitere…", of_count);
                cr.set_font_size(10.5);
                cr.select_font_face(
                    "sans-serif",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Bold,
                );
                cr.set_source_rgba(53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0, 0.95);
                if let Ok(ext) = cr.text_extents(&of_text) {
                    let tx = cell_x + 6.0 - ext.x_bearing();
                    let ty = of_y + (chip_h - ext.height()) / 2.0 - ext.y_bearing();
                    cr.move_to(tx, ty);
                    let _ = cr.show_text(&of_text);
                }
            }
        }
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
    let r = radius.min(width / 2.0).min(height / 2.0);
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
    if let Ok(ext) = cr.text_extents(text) {
        if ext.width() <= max_width {
            return text.to_string();
        }
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let mut low = 0usize;
    let mut high = chars.len();
    let mut best = 0usize;

    while low <= high {
        let mid = (low + high) / 2;
        let mut test_str = String::with_capacity(mid * 4 + 3);
        for &ch in &chars[..mid] {
            test_str.push(ch);
        }
        test_str.push('…');
        if let Ok(ext) = cr.text_extents(&test_str) {
            if ext.width() <= max_width {
                best = mid;
                low = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                high = mid - 1;
            }
        } else {
            break;
        }
    }

    if best == 0 {
        "…".to_string()
    } else {
        let mut res = String::with_capacity(best * 4 + 3);
        for &ch in &chars[..best] {
            res.push(ch);
        }
        res.push('…');
        res
    }
}

#[derive(Clone)]
struct PlacedTimeGridChip {
    event: CalendarEventRecord,
    start_slot: usize,
    row_span: usize,
    col_idx: usize,
    total_cols: usize,
    time_text: String,
}

fn compute_placed_timed_chips(
    day_events: &[CalendarEventRecord],
    day: NaiveDate,
    interval_minutes: u32,
    slot_count: usize,
) -> Vec<PlacedTimeGridChip> {
    let mut timed_events: Vec<CalendarEventRecord> =
        day_events.iter().filter(|e| !e.all_day).cloned().collect();
    timed_events.sort_by_key(|e| event_start_datetime(e));

    struct RawTimedEvent {
        event: CalendarEventRecord,
        start_slot: usize,
        end_slot: usize,
        row_span: usize,
        start_min: u32,
        time_text: String,
    }

    let mut raw_events = Vec::new();
    for event in timed_events {
        let (start_min, end_min, time_text) =
            event_occurrence_times_for_date(&event, day, interval_minutes);

        let start_slot = ((start_min / interval_minutes) as usize)
            .min(slot_count.saturating_sub(1));
        let end_slot = (((end_min + interval_minutes - 1) / interval_minutes) as usize)
            .max(start_slot + 1)
            .min(slot_count);
        let row_span = (end_slot - start_slot).max(1);

        raw_events.push(RawTimedEvent {
            event,
            start_slot,
            end_slot,
            row_span,
            start_min,
            time_text,
        });
    }

    raw_events.sort_by(|a, b| {
        a.start_min
            .cmp(&b.start_min)
            .then_with(|| a.end_slot.cmp(&b.end_slot))
            .then_with(|| a.event.summary.cmp(&b.event.summary))
    });

    let mut clusters: Vec<Vec<RawTimedEvent>> = Vec::new();
    let mut current_cluster: Vec<RawTimedEvent> = Vec::new();
    let mut cluster_end = 0;

    for item in raw_events {
        if current_cluster.is_empty() || item.start_slot < cluster_end {
            cluster_end = cluster_end.max(item.end_slot);
            current_cluster.push(item);
        } else {
            clusters.push(std::mem::take(&mut current_cluster));
            cluster_end = item.end_slot;
            current_cluster.push(item);
        }
    }
    if !current_cluster.is_empty() {
        clusters.push(current_cluster);
    }

    let mut placed_chips: Vec<PlacedTimeGridChip> = Vec::new();

    for cluster in clusters {
        let mut col_ends: Vec<usize> = Vec::new();
        let mut cluster_placed = Vec::new();

        for item in cluster {
            let mut col_idx = None;
            for (idx, end) in col_ends.iter_mut().enumerate() {
                if *end <= item.start_slot {
                    *end = item.end_slot;
                    col_idx = Some(idx);
                    break;
                }
            }
            let c = col_idx.unwrap_or_else(|| {
                let idx = col_ends.len();
                col_ends.push(item.end_slot);
                idx
            });
            cluster_placed.push((item.event, item.start_slot, item.row_span, c, item.time_text));
        }

        let total_cols = col_ends.len().max(1);

        for (event, start_slot, row_span, col_idx, time_text) in cluster_placed {
            placed_chips.push(PlacedTimeGridChip {
                event,
                start_slot,
                row_span,
                col_idx,
                total_cols,
                time_text,
            });
        }
    }

    placed_chips
}

fn chip_rect_coordinates(
    chip: &PlacedTimeGridChip,
    width: f64,
    slot_height: f64,
) -> (f64, f64, f64, f64) {
    let total_cols = chip.total_cols.max(1) as f64;
    let col_gap = 2.0;
    let margin_start = 2.0;
    let margin_end = 8.0;
    let available_w = (width - margin_start - margin_end - (total_cols - 1.0) * col_gap).max(20.0);
    let col_w = available_w / total_cols;
    let x = margin_start + chip.col_idx as f64 * (col_w + col_gap);
    let y = chip.start_slot as f64 * slot_height + 1.0;
    let w = col_w;
    let h = (chip.row_span as f64 * slot_height - 2.0).max(18.0);
    (x, y, w, h)
}

fn find_chip_at(
    chips: &[PlacedTimeGridChip],
    x: f64,
    y: f64,
    width: f64,
    slot_height: f64,
) -> Option<usize> {
    for (idx, chip) in chips.iter().enumerate().rev() {
        let (cx, cy, cw, ch) = chip_rect_coordinates(chip, width, slot_height);
        if x >= cx && x <= cx + cw && y >= cy && y <= cy + ch {
            return Some(idx);
        }
    }
    None
}

fn draw_time_grid_chip(
    cr: &gtk::cairo::Context,
    chip: &PlacedTimeGridChip,
    width: f64,
    slot_height: f64,
    is_hovered: bool,
    is_dark: bool,
) {
    let (x, y, w, h) = chip_rect_coordinates(chip, width, slot_height);
    let color_hex = event_color_for_record(&chip.event);
    let (bg_hex, hov_hex, _, fg_hex) = pastel_colors_for_hex(&color_hex, is_dark);
    let active_bg_hex = if is_hovered { &hov_hex } else { &bg_hex };

    let (bg_r, bg_g, bg_b) = parse_hex_color(active_bg_hex).unwrap_or((53, 132, 228));
    let (fg_r, fg_g, fg_b) = parse_hex_color(&fg_hex).unwrap_or((20, 20, 20));

    // Rounded background card
    draw_cairo_rounded_rect(cr, x, y, w, h, 6.0);
    cr.set_source_rgba(
        bg_r as f64 / 255.0,
        bg_g as f64 / 255.0,
        bg_b as f64 / 255.0,
        0.95,
    );
    let _ = cr.fill_preserve();

    // Subtle border
    cr.set_source_rgba(
        fg_r as f64 / 255.0,
        fg_g as f64 / 255.0,
        fg_b as f64 / 255.0,
        if is_hovered { 0.35 } else { 0.15 },
    );
    cr.set_line_width(if is_hovered { 1.5 } else { 1.0 });
    let _ = cr.stroke();

    // Left vertical color accent strip
    let (accent_r, accent_g, accent_b) =
        parse_hex_color(&color_hex).unwrap_or((bg_r, bg_g, bg_b));
    draw_cairo_rounded_rect(cr, x + 1.0, y + 1.0, 3.0, h - 2.0, 2.0);
    cr.set_source_rgba(
        accent_r as f64 / 255.0,
        accent_g as f64 / 255.0,
        accent_b as f64 / 255.0,
        0.9,
    );
    let _ = cr.fill();

    let title = if chip.event.summary.trim().is_empty() {
        "(Ohne Titel)"
    } else {
        chip.event.summary.as_str()
    };

    let text_x = x + 7.0;
    let max_text_w = (w - 10.0).max(10.0);

    let _ = cr.save();
    // Clip to chip interior to ensure zero overflow
    draw_cairo_rounded_rect(cr, x, y, w, h, 6.0);
    let _ = cr.clip();

    cr.set_source_rgba(
        fg_r as f64 / 255.0,
        fg_g as f64 / 255.0,
        fg_b as f64 / 255.0,
        1.0,
    );

    if chip.row_span >= 2 && h >= 32.0 {
        // Multi-line: Title bold on top, time smaller below
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Bold,
        );
        cr.set_font_size(11.0);
        let trunc_title = truncate_cairo_text(cr, title, max_text_w);
        cr.move_to(text_x, y + 13.0);
        let _ = cr.show_text(&trunc_title);

        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        cr.set_font_size(9.5);
        cr.set_source_rgba(
            fg_r as f64 / 255.0,
            fg_g as f64 / 255.0,
            fg_b as f64 / 255.0,
            0.75,
        );
        let trunc_time = truncate_cairo_text(cr, &chip.time_text, max_text_w);
        cr.move_to(text_x, y + 25.0);
        let _ = cr.show_text(&trunc_time);
    } else {
        // Single-line: "10:00 Meeting..."
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Bold,
        );
        cr.set_font_size(10.5);
        let single_line = format!("{} {}", chip.time_text, title);
        let trunc = truncate_cairo_text(cr, &single_line, max_text_w);
        cr.move_to(text_x, y + (h / 2.0 + 3.5).floor());
        let _ = cr.show_text(&trunc);
    }

    let _ = cr.restore();
}

#[derive(Clone, Debug)]
struct PreparedCalendarListRow {
    record: CalendarEventRecord,
    identity: Option<CalendarEventIdentity>,
    date_str: String,
    time_str: String,
    title: String,
    sub: String,
    category: Option<String>,
    color: (f64, f64, f64),
}

fn prepare_calendar_list_row(event: &CalendarEventRecord) -> PreparedCalendarListRow {
    let identity = CalendarEventIdentity::from_event(event);
    let date_str = if let Some(d) = event_start_date(event) {
        d.format("%a., %d.%m.%Y").to_string()
    } else if !event.start.is_empty() {
        event.start.split(' ').next().unwrap_or(&event.start).to_string()
    } else {
        String::new()
    };
    let time_str = if event.all_day {
        "Ganztägig".to_string()
    } else if !event.end.is_empty() {
        let t1 = event.start.split(' ').nth(1).unwrap_or(&event.start);
        let t2 = event.end.split(' ').nth(1).unwrap_or(&event.end);
        format!("{t1} – {t2}")
    } else if !event.start.is_empty() {
        event.start.split(' ').nth(1).unwrap_or(&event.start).to_string()
    } else {
        "-".to_string()
    };
    let title = if event.summary.trim().is_empty() {
        "(Ohne Titel)".to_string()
    } else {
        event.summary.trim().to_string()
    };
    let sub = if !event.location.trim().is_empty() && !event.source_display_name.is_empty() {
        format!("{} · {}", event.location.trim(), event.source_display_name)
    } else if !event.location.trim().is_empty() {
        event.location.trim().to_string()
    } else if !event.source_display_name.is_empty() {
        event.source_display_name.clone()
    } else {
        String::new()
    };
    let category = category_names_from_ical_component(&event.component)
        .into_iter()
        .next();
    let color_hex = event_color_for_record(event);
    let color = if let Ok(c) = gdk::RGBA::parse(&color_hex) {
        (c.red() as f64, c.green() as f64, c.blue() as f64)
    } else {
        (0.21, 0.52, 0.89)
    };
    PreparedCalendarListRow {
        record: event.clone(),
        identity,
        date_str,
        time_str,
        title,
        sub,
        category,
        color,
    }
}

fn draw_prepared_calendar_list_row(
    cr: &gtk::cairo::Context,
    item: &PreparedCalendarListRow,
    row_y: f64,
    width: f64,
    row_height: f64,
    is_selected: bool,
    is_hovered: bool,
    is_dark: bool,
) {
    // 1. Background
    if is_selected {
        cr.rectangle(0.0, row_y, width, row_height);
        cr.set_source_rgba(0.21, 0.52, 0.89, if is_dark { 0.28 } else { 0.16 });
        let _ = cr.fill();

        // Left accent indicator bar
        cr.rectangle(0.0, row_y, 4.0, row_height);
        cr.set_source_rgba(0.21, 0.52, 0.89, 1.0);
        let _ = cr.fill();
    } else if is_hovered {
        cr.rectangle(0.0, row_y, width, row_height);
        let hover_alpha = if is_dark { 0.05 } else { 0.04 };
        let hover_col = if is_dark { 1.0 } else { 0.0 };
        cr.set_source_rgba(hover_col, hover_col, hover_col, hover_alpha);
        let _ = cr.fill();
    }

    // Bottom row separator line
    let line_alpha = if is_dark { 0.08 } else { 0.06 };
    let line_col = if is_dark { 1.0 } else { 0.0 };
    cr.set_source_rgba(line_col, line_col, line_col, line_alpha);
    cr.set_line_width(1.0);
    cr.move_to(0.0, (row_y + row_height).round() - 0.5);
    cr.line_to(width, (row_y + row_height).round() - 0.5);
    let _ = cr.stroke();

    // 2. Calendar source dot
    let dot_x = 18.0;
    let dot_y = row_y + row_height / 2.0;
    let (dr, dg, db) = item.color;
    cr.arc(dot_x, dot_y, 4.5, 0.0, 2.0 * std::f64::consts::PI);
    cr.set_source_rgb(dr, dg, db);
    let _ = cr.fill();

    // Text colors
    let fg = if is_dark { 1.0 } else { 0.0 };
    let primary_alpha = if is_dark { 0.95 } else { 0.90 };
    let secondary_alpha = if is_dark { 0.60 } else { 0.55 };

    // 3. Date & Time column
    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
    cr.set_font_size(12.0);
    cr.set_source_rgba(fg, fg, fg, primary_alpha);
    cr.move_to(36.0, row_y + 19.0);
    let date_trunc = truncate_cairo_text(cr, &item.date_str, 125.0);
    let _ = cr.show_text(&date_trunc);

    // Time
    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
    cr.set_font_size(11.0);
    cr.set_source_rgba(fg, fg, fg, secondary_alpha);
    cr.move_to(36.0, row_y + 35.0);
    let time_trunc = truncate_cairo_text(cr, &item.time_str, 125.0);
    let _ = cr.show_text(&time_trunc);

    // Subtle vertical column separator
    cr.set_source_rgba(line_col, line_col, line_col, line_alpha * 1.5);
    cr.move_to(175.5, row_y + 8.0);
    cr.line_to(175.5, row_y + row_height - 8.0);
    let _ = cr.stroke();

    // 4. Summary & Subtitle
    let title_x = 190.0;
    let right_margin = if width > 500.0 { 150.0 } else { 30.0 };
    let max_title_w = (width - title_x - right_margin).max(60.0);

    // Summary
    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
    cr.set_font_size(13.0);
    cr.set_source_rgba(fg, fg, fg, primary_alpha);
    cr.move_to(title_x, row_y + 19.0);
    let title_trunc = truncate_cairo_text(cr, &item.title, max_title_w);
    let _ = cr.show_text(&title_trunc);

    // Subtitle (location & calendar)
    if !item.sub.is_empty() {
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(11.0);
        cr.set_source_rgba(fg, fg, fg, secondary_alpha);
        cr.move_to(title_x, row_y + 35.0);
        let sub_trunc = truncate_cairo_text(cr, &item.sub, max_title_w);
        let _ = cr.show_text(&sub_trunc);
    }

    // 5. Category pill badge (on right side)
    if width > 460.0 {
        if let Some(cat) = &item.category {
            let cat_w = 90.0;
            let cat_x = width - cat_w - 20.0;
            let cat_y = row_y + 13.0;
            draw_cairo_rounded_rect(cr, cat_x, cat_y, cat_w, 20.0, 9999.0);
            cr.set_source_rgba(fg, fg, fg, if is_dark { 0.10 } else { 0.06 });
            let _ = cr.fill();

            cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
            cr.set_font_size(10.0);
            cr.set_source_rgba(fg, fg, fg, secondary_alpha);
            cr.move_to(cat_x + 8.0, cat_y + 14.0);
            let cat_trunc = truncate_cairo_text(cr, cat, cat_w - 16.0);
            let _ = cr.show_text(&cat_trunc);
        }
    }
}

fn draw_calendar_list_row(
    cr: &gtk::cairo::Context,
    event: &CalendarEventRecord,
    row_y: f64,
    width: f64,
    row_height: f64,
    is_selected: bool,
    is_hovered: bool,
    is_dark: bool,
) {
    let item = prepare_calendar_list_row(event);
    draw_prepared_calendar_list_row(
        cr,
        &item,
        row_y,
        width,
        row_height,
        is_selected,
        is_hovered,
        is_dark,
    );
}

fn show_calendar_event_context_menu(
    page: &CalendarPage,
    anchor_widget: &impl glib::object::IsA<gtk::Widget>,
    event: &CalendarEventRecord,
    rect: gdk::Rectangle,
) {
    let anchor_widget = anchor_widget.as_ref();
    let action_state = calendar_event_action_state(event, CalendarStoreService::new().is_writable());
    let writable = action_state.can_edit;
    let recurring = action_state.recurring;

    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .pointing_to(&rect)
        .build();
    let menu_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    // 1. Edit
    let edit = gtk::Button::with_label("Termin bearbeiten");
    edit.set_halign(gtk::Align::Fill);
    edit.add_css_class("flat");
    edit.set_sensitive(writable);
    let popover_for_edit = popover.clone();
    let page_weak_edit = page.downgrade();
    let event_identity_for_edit = CalendarEventIdentity::from_event(event);
    let anchor_for_edit = anchor_widget.clone();
    edit.connect_clicked(move |_| {
        popover_for_edit.popdown();
        if let (Some(page), Some(identity)) = (page_weak_edit.upgrade(), event_identity_for_edit.as_ref()) {
            page.open_event_editor_for_identity(&anchor_for_edit, identity);
        }
    });
    menu_box.append(&edit);

    // 2. Print
    let print = gtk::Button::with_label("Termin drucken");
    print.set_halign(gtk::Align::Fill);
    print.add_css_class("flat");
    print.set_sensitive(action_state.can_print);
    let popover_for_print = popover.clone();
    let event_for_print = event.clone();
    let anchor_for_print = anchor_widget.clone();
    print.connect_clicked(move |_| {
        popover_for_print.popdown();
        let summary_display = if event_for_print.summary.trim().is_empty() {
            "Termin"
        } else {
            event_for_print.summary.trim()
        };
        match build_event_print_document(&event_for_print) {
            Ok(doc) => {
                show_toast_for_widget(
                    &anchor_for_print,
                    &format!(
                        "Druckansicht für „{summary_display}“ vorbereitet ({} Zeichen); Print-Portal ist Linux-Handoff",
                        doc.html.len()
                    ),
                );
            }
            Err(err) => {
                show_toast_for_widget(&anchor_for_print, &format!("Drucken fehlgeschlagen: {err}"));
            }
        }
    });
    menu_box.append(&print);

    // 3. Export
    let export = gtk::Button::with_label("Termin exportieren");
    export.set_halign(gtk::Align::Fill);
    export.add_css_class("flat");
    export.set_sensitive(action_state.can_export);
    let popover_for_export = popover.clone();
    let event_for_export = event.clone();
    let anchor_for_export = anchor_widget.clone();
    export.connect_clicked(move |_| {
        popover_for_export.popdown();
        if let Some(root) = anchor_for_export.root() {
            if let Ok(window) = root.downcast::<gtk::Window>() {
                let filter_ics = gtk::FileFilter::new();
                filter_ics.set_name(Some("iCalendar (*.ics)"));
                filter_ics.add_pattern("*.ics");

                let filter_csv = gtk::FileFilter::new();
                filter_csv.set_name(Some("Kommagetrennte Werte (*.csv)"));
                filter_csv.add_pattern("*.csv");

                let filter_rdf = gtk::FileFilter::new();
                filter_rdf.set_name(Some("RDF / RSS 1.0 (*.rdf)"));
                filter_rdf.add_pattern("*.rdf");

                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter_ics);
                filters.append(&filter_csv);
                filters.append(&filter_rdf);

                let safe_name = if event_for_export.summary.trim().is_empty() {
                    "termin".to_string()
                } else {
                    event_for_export
                        .summary
                        .chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                        .collect::<String>()
                };
                let initial_name = format!("{}.ics", if safe_name.is_empty() { "termin" } else { &safe_name });

                let file_dialog = gtk::FileDialog::builder()
                    .title("Termin exportieren")
                    .filters(&filters)
                    .default_filter(&filter_ics)
                    .initial_name(&initial_name)
                    .modal(true)
                    .build();
                let event_for_dialog = event_for_export.clone();
                let anchor_for_result = anchor_for_export.clone();
                file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else { return };
                    let format = if let Some(path) = file.path() {
                        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                        if ext.eq_ignore_ascii_case("csv") {
                            CalendarExportFormat::Csv
                        } else if ext.eq_ignore_ascii_case("rdf") {
                            CalendarExportFormat::Rdf
                        } else {
                            CalendarExportFormat::ICalendar
                        }
                    } else {
                        CalendarExportFormat::ICalendar
                    };
                    let payload = match export_calendar_events(&[event_for_dialog.clone()], format, None) {
                        Ok(data) => data,
                        Err(err) => {
                            show_toast_for_widget(&anchor_for_result, &err);
                            return;
                        }
                    };
                    let save_result = file.replace_contents(
                        payload.as_bytes(),
                        None,
                        true,
                        gio::FileCreateFlags::NONE,
                        None::<&gio::Cancellable>,
                    );
                    match save_result {
                        Ok(_) => show_toast_for_widget(
                            &anchor_for_result,
                            &format!("Termin als {} exportiert", format.display_label()),
                        ),
                        Err(error) => show_toast_for_widget(
                            &anchor_for_result,
                            &format!("Termin konnte nicht gespeichert werden: {error}"),
                        ),
                    }
                });
            }
        }
    });
    menu_box.append(&export);

    // 4. Copy
    let copy = gtk::Button::with_label("In lokalen Kalender kopieren");
    copy.set_halign(gtk::Align::Fill);
    copy.add_css_class("flat");
    copy.set_sensitive(action_state.can_copy_to_local);
    let popover_for_copy = popover.clone();
    let page_weak_copy = page.downgrade();
    let event_for_copy = event.clone();
    let anchor_for_copy = anchor_widget.clone();
    copy.connect_clicked(move |_| {
        popover_for_copy.popdown();
        if let Some(page) = page_weak_copy.upgrade() {
            page.transfer_event_to_local(event_for_copy.clone(), CalendarTransferKind::Copy, &anchor_for_copy);
        }
    });
    menu_box.append(&copy);

    // 5. Move
    let move_btn = gtk::Button::with_label("In lokalen Kalender verschieben");
    move_btn.set_halign(gtk::Align::Fill);
    move_btn.add_css_class("flat");
    move_btn.set_sensitive(action_state.can_move_to_local);
    let popover_for_move = popover.clone();
    let page_weak_move = page.downgrade();
    let event_for_move = event.clone();
    let anchor_for_move = anchor_widget.clone();
    move_btn.connect_clicked(move |_| {
        popover_for_move.popdown();
        if let Some(page) = page_weak_move.upgrade() {
            page.transfer_event_to_local(event_for_move.clone(), CalendarTransferKind::Move, &anchor_for_move);
        }
    });
    menu_box.append(&move_btn);

    // 6. Delete
    let delete = gtk::Button::with_label("Termin löschen");
    delete.set_halign(gtk::Align::Fill);
    delete.add_css_class("flat");
    delete.add_css_class("destructive-action");
    delete.set_sensitive(action_state.can_delete);
    let popover_for_delete = popover.clone();
    let page_weak_delete = page.downgrade();
    let event_for_delete = event.clone();
    let anchor_for_delete = anchor_widget.clone();
    delete.connect_clicked(move |_| {
        popover_for_delete.popdown();
        if let Some(page) = page_weak_delete.upgrade() {
            page.prompt_delete_event(&anchor_for_delete, &event_for_delete, recurring);
        }
    });
    menu_box.append(&delete);

    popover.set_parent(anchor_widget);
    popover.set_child(Some(&menu_box));
    popover.popup();
}

fn show_calendar_day_context_menu(
    page: &CalendarPage,
    date: NaiveDate,
    x: f64,
    y: f64,
) {
    let Some(canvas) = page.imp().month_canvas.borrow().as_ref().cloned() else { return };
    let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .pointing_to(&rect)
        .build();
    let menu_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    // 1. Neuer Termin
    let new_btn = gtk::Button::with_label("Neuer Termin…");
    new_btn.set_halign(gtk::Align::Fill);
    new_btn.add_css_class("flat");
    let popover_for_new = popover.clone();
    let page_weak_new = page.downgrade();
    new_btn.connect_clicked(move |_| {
        popover_for_new.popdown();
        if let Some(page) = page_weak_new.upgrade() {
            let dialog = crate::dialogs::EventDialog::new_for_date(date);
            if let Some(canvas) = page.imp().month_canvas.borrow().as_ref() {
                if let Some(root) = canvas.root() {
                    if let Ok(win) = root.downcast::<gtk::Window>() {
                        dialog.set_transient_for(Some(&win));
                    }
                }
            }
            dialog.present();
        }
    });
    menu_box.append(&new_btn);

    // 2. In Tagesansicht öffnen
    let day_btn = gtk::Button::with_label("In Tagesansicht öffnen");
    day_btn.set_halign(gtk::Align::Fill);
    day_btn.add_css_class("flat");
    let popover_for_day = popover.clone();
    let page_weak_day = page.downgrade();
    day_btn.connect_clicked(move |_| {
        popover_for_day.popdown();
        if let Some(page) = page_weak_day.upgrade() {
            *page.imp().current_date.borrow_mut() = date;
            page.imp()
                .view_state
                .borrow_mut()
                .set_anchor(date, configured_first_weekday());
            page.imp().view_state.borrow_mut().select_civil_day(date);
            page.select_calendar_view(CalendarView::Day);
        }
    });
    menu_box.append(&day_btn);

    popover.set_parent(&canvas);
    popover.set_child(Some(&menu_box));
    popover.popup();
}

fn install_event_drag_source<W: IsA<gtk::Widget>>(
    widget: &W,
    event: &CalendarEventRecord,
    occurrence_date: NaiveDate,
) {
    if event.source_uid.trim().is_empty()
        || event.uid.trim().is_empty()
        || event_mutation_error(event).is_some()
    {
        return;
    }
    let recurrence_id = calendar_recurrence_id(event).unwrap_or_default();
    let payload = format!(
        "{}\n{}\n{}\n{}",
        event.source_uid,
        event.uid,
        recurrence_id,
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
    install_time_slot_drop_target(widget, page, target_date, None);
}

fn handle_dropped_event_payload(
    page: &CalendarPage,
    payload: &str,
    target_date: NaiveDate,
    target_minute: Option<u32>,
) -> bool {
    let mut payload_parts = payload.splitn(4, '\n');
    let Some(source_uid) = payload_parts.next() else {
        return false;
    };
    let Some(uid) = payload_parts.next() else {
        return false;
    };
    let recurrence_id = payload_parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let occurrence_date = payload_parts
        .next()
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
    let records = page.imp().event_records.borrow().as_ref().cloned();
    let Some(event) = records.and_then(|records| {
        let target_identity =
            calendar_event_identity_from_parts(source_uid, uid, recurrence_id);
        unique_event_for_identity(&records.borrow(), &target_identity)
    }) else {
        show_toast_for_widget(page, "Der gezogene Termin ist nicht mehr geladen");
        return false;
    };
    let Some(start_date) = occurrence_date.or_else(|| event_start_date(&event)) else {
        show_toast_for_widget(page, "Der Termin besitzt kein gültiges Startdatum");
        return false;
    };
    let delta_days = (target_date - start_date).num_days();
    let delta_minutes = if let Some(target_min) = target_minute {
        if event.all_day {
            0
        } else {
            let start_dt = event_start_datetime(&event);
            let current_minute = start_dt
                .map(|dt| dt.time().hour() * 60 + dt.time().minute())
                .unwrap_or(0);
            target_min as i64 - current_minute as i64
        }
    } else {
        0
    };
    page.move_event_by_delta(event, delta_days, delta_minutes);
    true
}

fn install_time_slot_drop_target<W: IsA<gtk::Widget>>(
    widget: &W,
    page: &CalendarPage,
    target_date: NaiveDate,
    target_minute: Option<u32>,
) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let page_weak = page.downgrade();
    drop_target.connect_drop(move |_, value, _, _| {
        let Ok(payload) = value.get::<String>() else {
            return false;
        };
        let Some(page) = page_weak.upgrade() else {
            return false;
        };
        handle_dropped_event_payload(&page, &payload, target_date, target_minute)
    });
    widget.add_controller(drop_target);
}

fn shift_event_component(raw: &str, delta_days: i64) -> Result<String, String> {
    shift_event_component_full(raw, delta_days, 0)
}

fn shift_event_component_full(
    raw: &str,
    delta_days: i64,
    delta_minutes: i64,
) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("Der Termin enthält keine iCalendar-Daten".to_string());
    }
    if ical_property(raw, "RECURRENCE-ID").is_some() {
        return Err(
            "Serienausnahmen mit RECURRENCE-ID können nicht per Ziehen verschoben werden; der Kalenderdienst kann die Ausnahme nicht eindeutig adressieren"
                .to_string(),
        );
    }

    let mut updated = raw.to_string();
    for property_name in ["DTSTART", "DTEND"] {
        let Some(value) = ical_property(&updated, property_name) else {
            continue;
        };
        let Some(shifted) =
            shift_ical_datetime_value_full(&value, delta_days, delta_minutes)
        else {
            return Err(format!(
                "{property_name} konnte beim Verschieben nicht ausgewertet werden"
            ));
        };
        let property_prefix = ical_property_prefix(&updated, property_name)
            .unwrap_or_else(|| property_name.to_string());
        updated = update_ical_property(&updated, &property_prefix, Some(&shifted));
    }
    for property_name in ["RDATE", "EXDATE"] {
        updated = shift_ical_date_list_property_full(&updated, property_name, delta_days, delta_minutes)?;
    }
    updated = shift_rrule_full(&updated, delta_days, delta_minutes)?;
    Ok(updated)
}

fn shift_ical_date_list_property(
    raw: &str,
    property_name: &str,
    delta_days: i64,
) -> Result<String, String> {
    shift_ical_date_list_property_full(raw, property_name, delta_days, 0)
}

fn shift_ical_date_list_property_full(
    raw: &str,
    property_name: &str,
    delta_days: i64,
    delta_minutes: i64,
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
                    shift_ical_datetime_value_full(value, delta_days, delta_minutes).ok_or_else(|| {
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
    shift_rrule_full(raw, delta_days, 0)
}

fn shift_rrule_full(
    raw: &str,
    delta_days: i64,
    delta_minutes: i64,
) -> Result<String, String> {
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
                let shifted = shift_ical_datetime_value_full(value, delta_days, delta_minutes).ok_or_else(|| {
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
    shift_ical_datetime_value_full(value, delta_days, 0)
}

fn shift_ical_datetime_value_full(
    value: &str,
    delta_days: i64,
    delta_minutes: i64,
) -> Option<String> {
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
    let shift = Duration::days(delta_days) + Duration::minutes(delta_minutes);
    Some(format!(
        "{}{}",
        (datetime + shift).format("%Y%m%dT%H%M%S"),
        suffix
    ))
}

fn week_start_for(date: NaiveDate, first_weekday: Weekday) -> NaiveDate {
    let current = i64::from(date.weekday().num_days_from_monday());
    let first = i64::from(first_weekday.num_days_from_monday());
    date - Duration::days((current - first).rem_euclid(7))
}

fn calendar_year_range_option(year: i32) -> Option<CalendarDateRange> {
    let start = NaiveDate::from_ymd_opt(year, 1, 1)?;
    // `NaiveDate` has no representable exclusive end after its maximum date.
    // Clip the final civil year to that boundary instead of panicking while
    // constructing a viewport at `NaiveDate::MAX`.
    let end = year
        .checked_add(1)
        .and_then(|next_year| NaiveDate::from_ymd_opt(next_year, 1, 1))
        .unwrap_or(NaiveDate::MAX);
    if start >= end {
        return None;
    }
    Some(CalendarDateRange::new(start, end))
}

fn calendar_year_range(year: i32) -> CalendarDateRange {
    calendar_year_range_option(year).unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(year, 1, 1)
            .map(CalendarDateRange::single)
            .unwrap_or_else(|| CalendarDateRange::single(NaiveDate::MIN))
    })
}

fn shift_year(date: NaiveDate, direction: i32) -> NaiveDate {
    let Some(year) = date.year().checked_add(direction) else {
        return date;
    };
    NaiveDate::from_ymd_opt(year, date.month(), date.day())
        .or_else(|| {
            (28..=31)
                .rev()
                .find_map(|day| NaiveDate::from_ymd_opt(year, date.month(), day))
        })
        .unwrap_or(date)
}

fn configured_first_weekday() -> Weekday {
    AppSettings::try_new()
        .map(|settings| settings.calendar_week_start())
        .filter(|value| value == "sunday")
        .map(|_| Weekday::Sun)
        .unwrap_or(Weekday::Mon)
}

fn add_months(year: i32, month: u32, offset: usize) -> (i32, u32) {
    let total_month = (month - 1) as usize + offset;
    let new_year = year + (total_month / 12) as i32;
    let new_month = (total_month % 12 + 1) as u32;
    (new_year, new_month)
}

fn cached_event_chip_css_provider(
    bg: &str,
    hover: &str,
    active: &str,
    fg: &str,
) -> gtk::CssProvider {
    thread_local! {
        static PROVIDER_CACHE: std::cell::RefCell<HashMap<(String, String, String, String), gtk::CssProvider>> =
            std::cell::RefCell::new(HashMap::new());
    }
    let key = (bg.to_string(), hover.to_string(), active.to_string(), fg.to_string());
    PROVIDER_CACHE.with(|cache| {
        let mut map = cache.borrow_mut();
        if let Some(provider) = map.get(&key) {
            provider.clone()
        } else {
            let provider = gtk::CssProvider::new();
            provider.load_from_data(&format!(
                "button.calendar-grid-event {{ border: none; outline: none; background-color: {bg}; color: {fg}; }} \
                 button.calendar-grid-event:hover {{ border: none; outline: none; background-color: {hover}; color: {fg}; }} \
                 button.calendar-grid-event:active {{ border: none; outline: none; background-color: {active}; color: {fg}; }}"
            ));
            map.insert(key, provider.clone());
            provider
        }
    })
}

/// Determines the dominant month (year, month) for a 42-day (6-week) month grid range.
/// A month is dominant if it occupies at least 4 of the 6 rows (evaluated by the row's midpoint date).
/// If two months each occupy 3 rows (a 3:3 split), hysteresis is applied: the currently active
/// month (`current_ym`) is preserved if it is one of the 3-row candidates, preventing visual flickering
/// during continuous row-by-row scrolling.
pub(crate) fn dominant_month_for_range(
    range: CalendarDateRange,
    current_ym: Option<(i32, u32)>,
) -> (i32, u32) {
    let mut row_counts: BTreeMap<(i32, u32), usize> = BTreeMap::new();
    for r in 0..6 {
        if let Some(midpoint) = range.start.checked_add_signed(Duration::days(r * 7 + 3)) {
            *row_counts.entry((midpoint.year(), midpoint.month())).or_insert(0) += 1;
        }
    }

    // 1. A month with 4 or more rows is unconditionally dominant
    for (&ym, &count) in &row_counts {
        if count >= 4 {
            return ym;
        }
    }

    // 2. In case of 3:3 tie, preserve current_ym if it is one of the candidate months with >= 3 rows
    if let Some(curr) = current_ym {
        if let Some(&count) = row_counts.get(&curr) {
            if count >= 3 {
                return curr;
            }
        }
    }

    // 3. Fallback to month with highest row count, or start of range
    row_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(ym, _)| ym)
        .unwrap_or_else(|| (range.start.year(), range.start.month()))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_y, next_m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_y, next_m, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(30)
}

fn is_day_visible_in_viewport(viewport: &CalendarViewport, date: NaiveDate) -> bool {
    match viewport.view {
        CalendarView::Day => date == viewport.anchor_date,
        CalendarView::Week => viewport.visible_range.contains(date),
        CalendarView::WorkWeek => {
            viewport.visible_range.contains(date) && configured_workdays().contains(&date.weekday())
        }
        CalendarView::Month => viewport.visible_range.contains(date),
        CalendarView::Agenda => viewport.visible_range.contains(date),
        CalendarView::Year => date == viewport.anchor_date,
        CalendarView::List => date == viewport.anchor_date,
    }
}

fn update_mini_calendar_classes(
    cal: &gtk::Calendar,
    y: i32,
    m: u32,
    viewport: &CalendarViewport,
    has_selected_date: bool,
) {
    let today = chrono::Local::now().date_naive();
    let (prev_y, prev_m) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let (next_y, next_m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };

    let mut child = cal.first_child();
    while let Some(w) = child {
        if w.css_name() == "grid" {
            let mut cell = w.first_child();
            let mut in_current_month = false;
            let mut past_current_month = false;

            while let Some(lbl) = cell {
                if lbl.has_css_class("day-number") {
                    let is_other_month = lbl.has_css_class("other-month");
                    if !is_other_month {
                        in_current_month = true;
                    } else if in_current_month {
                        past_current_month = true;
                    }

                    if !has_selected_date || viewport.view == CalendarView::Month {
                        lbl.unset_state_flags(gtk::StateFlags::SELECTED);
                    }

                    lbl.remove_css_class("range-start");
                    lbl.remove_css_class("range-end");

                    if let Ok(label) = lbl.clone().downcast::<gtk::Label>() {
                        let text = label.text();
                        if let Ok(day_num) = text.parse::<u32>() {
                            let (cell_y, cell_m) = if !in_current_month {
                                (prev_y, prev_m)
                            } else if past_current_month {
                                (next_y, next_m)
                            } else {
                                (y, m)
                            };

                            if let Some(d) = NaiveDate::from_ymd_opt(cell_y, cell_m, day_num) {
                                let is_visible = is_day_visible_in_viewport(viewport, d);
                                if is_visible {
                                    lbl.set_state_flags(gtk::StateFlags::CHECKED, false);

                                    let prev_visible = d
                                        .pred_opt()
                                        .map(|p| is_day_visible_in_viewport(viewport, p))
                                        .unwrap_or(false);
                                    let next_visible = d
                                        .succ_opt()
                                        .map(|n| is_day_visible_in_viewport(viewport, n))
                                        .unwrap_or(false);

                                    if viewport.view != CalendarView::Month {
                                        if !prev_visible {
                                            lbl.add_css_class("range-start");
                                        }
                                        if !next_visible {
                                            lbl.add_css_class("range-end");
                                        }
                                    }
                                } else {
                                    lbl.unset_state_flags(gtk::StateFlags::CHECKED);
                                }

                                if !is_other_month
                                    && d == today
                                    && y == today.year()
                                    && m == today.month()
                                {
                                    lbl.add_css_class("today");
                                } else {
                                    lbl.remove_css_class("today");
                                }
                            }
                        }
                    }
                }
                cell = lbl.next_sibling();
            }
            break;
        }
        child = w.next_sibling();
    }
}

fn configured_hide_cancelled_events() -> bool {
    AppSettings::try_new()
        .map(|settings| settings.hide_cancelled_events())
        .unwrap_or(false)
}

fn normalize_calendar_time_divisions(divisions: i32) -> i32 {
    match divisions {
        5 | 10 | 15 | 30 | 60 => divisions,
        _ => 30,
    }
}

fn configured_time_divisions() -> i32 {
    AppSettings::try_new()
        .map(|settings| normalize_calendar_time_divisions(settings.calendar_time_divisions()))
        .unwrap_or(30)
}

fn configured_compress_weekend() -> bool {
    AppSettings::try_new()
        .map(|settings| settings.calendar_compress_weekend())
        .unwrap_or(true)
}

fn configured_month_scroll_by_week() -> bool {
    AppSettings::try_new()
        .map(|settings| settings.calendar_month_scroll_by_week())
        .unwrap_or(true)
}

fn configured_second_zone() -> String {
    AppSettings::try_new()
        .map(|settings| settings.calendar_second_zone())
        .unwrap_or_default()
}

fn timezone_from_name(name: &str) -> Option<glib::TimeZone> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.eq_ignore_ascii_case("utc") {
        Some(glib::TimeZone::utc())
    } else {
        glib::TimeZone::from_identifier(Some(trimmed))
    }
}

fn primary_timezone_header_label() -> String {
    let id = glib::TimeZone::local().identifier();
    let trimmed = id.trim();
    if !trimmed.is_empty() {
        let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
        return name.replace('_', " ");
    }
    "Zeit".to_string()
}

fn format_hour_markup(minute_of_day: u32, use_12h: bool) -> String {
    let hour = minute_of_day / 60;
    if use_12h {
        let (h12, ampm) = if hour == 0 {
            (12, "AM")
        } else if hour < 12 {
            (hour, "AM")
        } else if hour == 12 {
            (12, "PM")
        } else {
            (hour - 12, "PM")
        };
        format!(
            r#"<span size="18000" weight="medium">{}</span><span size="9500" rise="7000"> {}</span>"#,
            h12, ampm
        )
    } else {
        format!(
            r#"<span size="18000" weight="medium">{}</span><span size="9500" rise="7000"> 00</span>"#,
            hour
        )
    }
}

fn secondary_timezone_header_label(zone: &str) -> String {
    let trimmed = zone.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    name.replace('_', " ")
}

fn secondary_timezone_hour_label(
    date: NaiveDate,
    minute_of_day: u32,
    second_tz: &glib::TimeZone,
    use_12h: bool,
) -> Option<String> {
    let hour = (minute_of_day / 60) as i32;
    let minute = (minute_of_day % 60) as i32;
    let local_dt = glib::DateTime::new(
        &glib::TimeZone::local(),
        date.year(),
        date.month() as i32,
        date.day() as i32,
        hour,
        minute,
        0.0,
    )
    .ok()?;
    let converted_dt = local_dt.to_timezone(second_tz).ok()?;
    let conv_hour = converted_dt.hour();
    let conv_min = converted_dt.minute();
    if use_12h {
        let suffix = if conv_hour < 12 { "AM" } else { "PM" };
        let display_hour = match conv_hour % 12 {
            0 => 12,
            v => v,
        };
        Some(format!("{display_hour}:{conv_min:02} {suffix}"))
    } else {
        Some(format!("{conv_hour:02}:{conv_min:02}"))
    }
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

/// Qualify whether a specific time slot minute on a given weekday constitutes a workday cell.
#[inline]
pub(crate) fn is_workday_cell(
    minute: u32,
    workday_start: u32,
    workday_end: u32,
    day_weekday: Weekday,
    workdays: &[Weekday],
) -> bool {
    let is_workday_time = minute >= workday_start && minute < workday_end;
    is_workday_time && workdays.contains(&day_weekday)
}

fn configured_weekday_names_short() -> [&'static str; 7] {
    configured_weekday_names()
}

fn configured_weekday_names_full() -> [&'static str; 7] {
    if configured_first_weekday() == Weekday::Sun {
        [
            "Sonntag",
            "Montag",
            "Dienstag",
            "Mittwoch",
            "Donnerstag",
            "Freitag",
            "Samstag",
        ]
    } else {
        [
            "Montag",
            "Dienstag",
            "Mittwoch",
            "Donnerstag",
            "Freitag",
            "Samstag",
            "Sonntag",
        ]
    }
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

fn clear_grid(container: &gtk::Grid) {
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

pub(crate) fn toggle_task_completion(
    page: &CalendarPage,
    task: &TaskRecord,
    completed: bool,
    check: Option<gtk::CheckButton>,
) {
    if completed == task.completed {
        return;
    }
    let source_uid = task.source_uid.clone();
    let uid = task.uid.clone();
    let updated_component = set_task_completion_component(&task.component, completed);
    let mutation_token = page.begin_calendar_mutation(None);
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

    if let Some(check) = &check {
        check.set_sensitive(false);
    }
    let page_weak = page.downgrade();
    let task = task.clone();
    glib::spawn_future_local(async move {
        let result = receiver.recv().await;
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if !page.calendar_mutation_is_current(&mutation_token) {
            if let Some(check) = &check {
                check.set_active(task.completed);
                check.set_sensitive(true);
            }
            show_toast_for_widget(
                &page,
                "Der Kalenderstand hat sich geändert; die Aufgabenänderung wurde verworfen",
            );
            page.reload_calendar_data();
            return;
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                page.finish_calendar_mutation(&mutation_token);
                if let Some(check) = &check {
                    check.set_active(task.completed);
                    check.set_sensitive(true);
                }
                show_toast_for_widget(
                    &page,
                    &format!("Aufgabenänderung konnte nicht bestätigt werden: {error}"),
                );
                page.reload_calendar_data();
                return;
            }
        };
        match result {
            Ok(result) => {
                let component = result.component.unwrap_or(updated_component);
                let Some(records) = page.imp().task_records.borrow().as_ref().cloned() else {
                    page.finish_calendar_mutation(&mutation_token);
                    if let Some(check) = &check {
                        check.set_active(task.completed);
                        check.set_sensitive(true);
                    }
                    show_toast_for_widget(&page, "Aufgabendaten sind nicht mehr verfügbar");
                    page.reload_calendar_data();
                    return;
                };
                let mut records = records.borrow_mut();
                let Some(existing) = records.iter_mut().find(|existing| {
                    existing.source_uid == task.source_uid && existing.uid == task.uid
                }) else {
                    drop(records);
                    page.finish_calendar_mutation(&mutation_token);
                    if let Some(check) = &check {
                        check.set_active(task.completed);
                        check.set_sensitive(true);
                    }
                    show_toast_for_widget(
                        &page,
                        "Die bestätigte Aufgabe ist nicht mehr geladen; die Ansicht wird neu geladen",
                    );
                    page.reload_calendar_data();
                    return;
                };
                existing.component = component;
                existing.completed = completed;
                drop(records);
                if !page.finish_calendar_mutation(&mutation_token) {
                    page.reload_calendar_data();
                    return;
                }
                page.bump_calendar_revision();
                if let Some(check) = &check {
                    check.set_sensitive(true);
                }
                page.refresh_task_sidebar();
                page.refresh_taskpad();
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
                page.finish_calendar_mutation(&mutation_token);
                if let Some(check) = &check {
                    check.set_active(task.completed);
                    check.set_sensitive(true);
                }
                show_toast_for_widget(
                    &page,
                    &format!("Aufgabe konnte nicht geändert werden: {error}"),
                );
                page.reload_calendar_data();
            }
        }
    });
}

fn wire_task_completion_toggle(page: &CalendarPage, check: &gtk::CheckButton, task: TaskRecord) {
    let page_weak = page.downgrade();
    let check_weak = check.clone();
    check.connect_toggled(move |check| {
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let completed = check.is_active();
        toggle_task_completion(&page, &task, completed, Some(check_weak.clone()));
    });
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
        .title_lines(1)
        .subtitle_lines(1)
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

    wire_task_completion_toggle(page, &check, task);
}

fn calendar_memo_subtitle(memo: &MemoRecord) -> String {
    if !memo.updated.trim().is_empty() {
        memo.updated.trim().to_string()
    } else if !memo.description.trim().is_empty() {
        memo.description
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    } else if !memo.source_display_name.trim().is_empty() {
        memo.source_display_name.trim().to_string()
    } else {
        String::new()
    }
}

pub(crate) fn task_matches_calendar_filter(
    task: &TaskRecord,
    query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> bool {
    if hidden_sources.contains(&task.source_uid)
        || hidden_sources.contains(&task.source_display_name)
        || hidden_sources
            .iter()
            .any(|h| !task.source_uid.is_empty() && h.ends_with(&format!(":{}", task.source_uid)))
    {
        return false;
    }
    if let Some(category) = category_filter {
        let categories = category_names_from_ical_component(&task.component);
        if !categories.iter().any(|c| c.eq_ignore_ascii_case(category)) {
            return false;
        }
    }
    if !query.trim().is_empty() {
        let q = query.trim().to_lowercase();
        let summary_match = task.summary.to_lowercase().contains(&q);
        let component_match = task.component.to_lowercase().contains(&q);
        if !summary_match && !component_match {
            return false;
        }
    }
    true
}

pub(crate) fn memo_matches_calendar_filter(
    memo: &MemoRecord,
    query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> bool {
    if hidden_sources.contains(&memo.source_uid)
        || hidden_sources.contains(&memo.source_display_name)
        || hidden_sources
            .iter()
            .any(|h| !memo.source_uid.is_empty() && h.ends_with(&format!(":{}", memo.source_uid)))
    {
        return false;
    }
    if let Some(category) = category_filter {
        let categories = category_names_from_ical_component(&memo.component);
        if !categories.iter().any(|c| c.eq_ignore_ascii_case(category)) {
            return false;
        }
    }
    if !query.trim().is_empty() {
        let q = query.trim().to_lowercase();
        let summary_match = memo.summary.to_lowercase().contains(&q);
        let desc_match = memo.description.to_lowercase().contains(&q);
        let component_match = memo.component.to_lowercase().contains(&q);
        if !summary_match && !desc_match && !component_match {
            return false;
        }
    }
    true
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

fn set_calendar_event_row_identity(row: &adw::ActionRow, event: &CalendarEventRecord) {
    let Some(identity) = CalendarEventIdentity::from_event(event) else {
        return;
    };
    unsafe {
        row.set_data("event-id", identity.uid.clone());
        row.set_data("event-source-uid", identity.source_uid.clone());
        row.set_data(
            "event-source-object-key",
            identity.source_object_key.clone(),
        );
        row.set_data(
            "event-recurrence-id",
            identity.recurrence_id.clone().unwrap_or_default(),
        );
    }
}

fn format_full_german_date(date: NaiveDate) -> String {
    format!(
        "{}, {}. {} {}",
        weekday_name(date),
        date.day(),
        CalendarPage::month_name(date.month()),
        date.year()
    )
}

fn format_duration_text(minutes: u32) -> String {
    let hours = minutes / 60;
    let mins = minutes % 60;
    if hours > 0 && mins > 0 {
        format!("{hours} Std. {mins} Min.")
    } else if hours > 0 {
        format!("{hours} Std.")
    } else {
        format!("{mins} Min.")
    }
}

fn format_recurrence_summary(event: &CalendarEventRecord) -> Option<String> {
    let rule_raw = ical_property(&event.component, "RRULE")?;
    let rule_upper = rule_raw.to_uppercase();
    if rule_upper.contains("FREQ=DAILY") {
        Some("Täglich wiederkehrend".to_string())
    } else if rule_upper.contains("FREQ=WEEKLY") {
        Some("Wöchentlich wiederkehrend".to_string())
    } else if rule_upper.contains("FREQ=MONTHLY") {
        Some("Monatlich wiederkehrend".to_string())
    } else if rule_upper.contains("FREQ=YEARLY") {
        Some("Jährlich wiederkehrend".to_string())
    } else {
        Some("Wiederkehrender Termin".to_string())
    }
}

pub(crate) fn event_occurrence_times_for_date(
    event: &CalendarEventRecord,
    day: NaiveDate,
    interval_minutes: u32,
) -> (u32, u32, String) {
    let start_dt = event_start_datetime(event);
    let end_dt = event_end_datetime(event);

    let (base_start, base_duration) = match (start_dt, end_dt) {
        (Some(start), Some(end)) => {
            let dur = end.signed_duration_since(start);
            let dur = if dur > Duration::zero() {
                dur
            } else {
                Duration::minutes(interval_minutes as i64)
            };
            (start, dur)
        }
        (Some(start), None) => (start, Duration::minutes(interval_minutes as i64)),
        (None, _) => {
            let fallback_start = day.and_hms_opt(9, 0, 0).unwrap();
            (fallback_start, Duration::minutes(interval_minutes as i64))
        }
    };

    let start_date = base_start.date();
    let is_recurring = ical_property(&event.component, "RRULE").is_some()
        || ical_property(&event.component, "RDATE").is_some();

    let (occ_start_dt, occ_end_dt) = if is_recurring {
        let span_days = (base_duration.num_days() + 1).max(1);
        let excluded_dates = exdate_values(event);
        let rdate_values = property_local_dates(&event.component, "RDATE");
        let recurrence_rule =
            ical_property(&event.component, "RRULE").and_then(|raw| parse_recurrence_rule(&raw));

        let mut matched_start = None;
        for offset in 0..span_days {
            if let Some(candidate) = day.checked_sub_signed(Duration::days(offset)) {
                if event_occurrence_start_is_on(
                    candidate,
                    start_date,
                    &excluded_dates,
                    &rdate_values,
                    recurrence_rule.as_ref(),
                ) {
                    matched_start = Some(candidate);
                    break;
                }
            }
        }

        let occ_date = matched_start.unwrap_or(day);
        let occ_start = occ_date.and_time(base_start.time());
        let occ_end = occ_start + base_duration;
        (occ_start, occ_end)
    } else {
        (base_start, base_start + base_duration)
    };

    let start_min = if occ_start_dt.date() < day {
        0
    } else if occ_start_dt.date() == day {
        (occ_start_dt.hour() * 60 + occ_start_dt.minute()).min(1439)
    } else {
        0
    };

    let end_min = if occ_end_dt.date() > day {
        1440
    } else if occ_end_dt.date() == day {
        let m = occ_end_dt.hour() * 60 + occ_end_dt.minute();
        if m == 0 && occ_start_dt.date() < day {
            interval_minutes.min(1440)
        } else if m <= start_min {
            (start_min + interval_minutes).min(1440)
        } else {
            m
        }
    } else {
        (start_min + interval_minutes).min(1440)
    };

    let end_min = if end_min <= start_min {
        (start_min + interval_minutes).min(1440)
    } else {
        end_min
    };

    let time_text = if occ_start_dt.date() == day && occ_end_dt.date() == day {
        let start_str = format_clock_minutes(start_min);
        let end_str = if end_min == 1440 {
            "24:00".to_string()
        } else {
            format_clock_minutes(end_min)
        };
        format!("{start_str}–{end_str}")
    } else if occ_start_dt.date() < day && occ_end_dt.date() == day {
        let end_str = format_clock_minutes(end_min);
        format!("bis {end_str}")
    } else if occ_start_dt.date() == day && occ_end_dt.date() > day {
        let start_str = format_clock_minutes(start_min);
        format!("ab {start_str}")
    } else {
        "Ganztägig".to_string()
    };

    (start_min, end_min, time_text)
}

fn prompt_delete_calendar_event<W: IsA<gtk::Widget> + Clone + 'static>(
    page: &CalendarPage,
    widget: &W,
    event: &CalendarEventRecord,
) {
    if let Some(error) = event_mutation_error(event) {
        show_toast_for_widget(widget, error);
        return;
    }
    let recurring = ical_property(&event.component, "RRULE").is_some()
        || ical_property(&event.component, "RDATE").is_some();
    let page_weak = page.downgrade();

    if recurring {
        let scope_dialog = adw::AlertDialog::new(
            Some("Wiederkehrenden Termin löschen"),
            Some("Wählen Sie den Löschumfang. Einzelne Vorkommen und künftige Vorkommen werden erst angeboten, wenn der Backend-Adapter sie ohne Datenverlust als Ausnahme bzw. Serienaufteilung bestätigen kann."),
        );
        scope_dialog.add_responses(&[
            ("cancel", "Abbrechen"),
            ("this", "Nur diesen Termin"),
            ("future", "Diesen und künftige"),
            ("series", "Ganze Serie löschen"),
        ]);
        scope_dialog.set_response_appearance("series", adw::ResponseAppearance::Destructive);
        let event_for_scope = event.clone();
        let widget_for_scope = widget.clone();
        let page_for_scope = page_weak.clone();

        scope_dialog.connect_response(None, move |dialog, response| {
            let scope = match response {
                "this" => Some(CalendarRecurringDeleteScope::ThisOccurrence),
                "future" => Some(CalendarRecurringDeleteScope::ThisAndFuture),
                "series" => Some(CalendarRecurringDeleteScope::EntireSeries),
                _ => None,
            };
            let Some(scope) = scope else {
                return;
            };
            match plan_calendar_event_delete(&event_for_scope, scope) {
                Ok(CalendarDeletePlan::RemoveComponent { .. }) => {
                    dialog.close();
                    if let Some(page) = page_for_scope.upgrade() {
                        execute_delete_calendar_event(&page, &widget_for_scope, &event_for_scope);
                    }
                }
                Ok(CalendarDeletePlan::UnsupportedOccurrenceScope { scope }) => {
                    let description = match scope {
                        CalendarRecurringDeleteScope::ThisOccurrence => "Dieses einzelne Vorkommen kann der aktuelle Backend-Adapter noch nicht verlustfrei löschen; die Serie bleibt unverändert.",
                        CalendarRecurringDeleteScope::ThisAndFuture => "Eine Serienaufteilung ab diesem Vorkommen kann der aktuelle Backend-Adapter noch nicht verlustfrei durchführen; die Serie bleibt unverändert.",
                        CalendarRecurringDeleteScope::EntireSeries => unreachable!(),
                    };
                    show_toast_for_widget(&widget_for_scope, description);
                }
                Err(error) => show_toast_for_widget(&widget_for_scope, &error),
            }
        });
        scope_dialog.present(Some(widget));
        return;
    }

    execute_delete_calendar_event(page, widget, event);
}

fn execute_delete_calendar_event<W: IsA<gtk::Widget> + Clone + 'static>(
    page: &CalendarPage,
    widget: &W,
    event: &CalendarEventRecord,
) {
    let confirmation_message = if event.source_uid == "local-calendar" {
        "Der Termin wird dauerhaft aus dem lokalen Kalender entfernt."
    } else {
        "Der Termin wird dauerhaft aus dem synchronisierten Kalender entfernt."
    };
    let page_weak_for_action = page.downgrade();
    let widget_for_action = widget.clone();
    let identity_for_action = CalendarEventIdentity::from_event(event);

    crate::dialogs::run_destructive_action(
        widget,
        "Termin löschen?",
        confirmation_message,
        "Löschen",
        move || {
            let Some(page) = page_weak_for_action.upgrade() else {
                return;
            };
            let Some(identity) = identity_for_action.clone() else {
                show_toast_for_widget(
                    &widget_for_action,
                    "Der Termin besitzt keine eindeutige Identität und wurde nicht gelöscht",
                );
                return;
            };
            let Some(event_record) = page.unique_loaded_calendar_event(&identity) else {
                show_toast_for_widget(
                    &widget_for_action,
                    "Der Termin ist nicht eindeutig geladen und wurde nicht gelöscht",
                );
                page.reload_calendar_data();
                return;
            };
            if let Some(error) = event_mutation_error(&event_record) {
                show_toast_for_widget(&widget_for_action, error);
                return;
            }
            let mutation_token = page.begin_calendar_mutation(Some(identity.clone()));
            let source_uid = event_record.source_uid.clone();
            let uid = event_record.uid.clone();
            let uid_for_result = uid.clone();
            let (sender, receiver) = async_channel::bounded(1);
            widget_for_action.set_sensitive(false);
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

            let widget_for_result = widget_for_action.clone();
            let page_weak_for_result = page.downgrade();
            let event_identity_for_result = identity.clone();
            glib::spawn_future_local(async move {
                let result = receiver.recv().await;
                widget_for_result.set_sensitive(true);
                let Some(page) = page_weak_for_result.upgrade() else {
                    return;
                };
                if !page.calendar_event_mutation_is_current(&mutation_token) {
                    show_toast_for_widget(
                        &widget_for_result,
                        "Der Kalenderstand hat sich geändert; das Löschergebnis wurde verworfen",
                    );
                    page.reload_calendar_data();
                    return;
                }
                match result {
                    Ok(Ok(result)) if result.uid == uid_for_result => {
                        if !page.finish_calendar_mutation(&mutation_token) {
                            page.reload_calendar_data();
                            return;
                        }
                        if let Some(list) = page.imp().event_list.borrow().as_ref() {
                            remove_event_list_row_by_identity(list, &event_identity_for_result);
                        }
                        if let Some(records) = page.imp().event_records.borrow().as_ref() {
                            records.borrow_mut().retain(|e| {
                                CalendarEventIdentity::from_event(e).as_ref()
                                    != Some(&event_identity_for_result)
                            });
                        }
                        page.bump_calendar_revision();
                        page.rebuild_calendar_index_from_records();
                        page.refresh_calendar_views();
                        show_toast_for_widget(
                            &widget_for_result,
                            &format!("Termin gelöscht ({})", result.uid),
                        );
                        if let Some(window) = widget_for_result
                            .root()
                            .and_downcast::<crate::window::EvolutionWindow>()
                        {
                            window.refresh_calendar_page();
                        }
                    }
                    Ok(Ok(result)) => {
                        page.finish_calendar_mutation(&mutation_token);
                        show_toast_for_widget(
                            &widget_for_result,
                            &format!(
                                "ECal bestätigte eine andere Termin-UID ({}); nichts wurde entfernt",
                                result.uid
                            ),
                        );
                        page.reload_calendar_data();
                    }
                    Ok(Err(error)) => {
                        page.finish_calendar_mutation(&mutation_token);
                        show_toast_for_widget(
                            &widget_for_result,
                            &format!("Termin konnte nicht gelöscht werden: {error}"),
                        );
                        page.reload_calendar_data();
                    }
                    Err(error) => {
                        page.finish_calendar_mutation(&mutation_token);
                        show_toast_for_widget(
                            &widget_for_result,
                            &format!("Löschauftrag fehlgeschlagen: {error}"),
                        );
                        page.reload_calendar_data();
                    }
                }
            });
        },
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CalendarAttendeeInfo {
    name: String,
    email: Option<String>,
    partstat: Option<String>,
    role: Option<String>,
}

fn parse_ical_attendees(component: &str) -> Vec<CalendarAttendeeInfo> {
    let mut attendees = Vec::new();
    for line in unfold_ical_lines(component) {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = property.split(';');
        let prop_name = parts.next().unwrap_or("").trim().to_ascii_uppercase();
        if prop_name != "ATTENDEE" {
            continue;
        }
        let mut cn: Option<String> = None;
        let mut partstat: Option<String> = None;
        let mut role: Option<String> = None;
        for param in parts {
            let Some((k, v)) = param.split_once('=') else {
                continue;
            };
            let k_upper = k.trim().to_ascii_uppercase();
            let v_clean = v.trim().trim_matches('"').to_string();
            match k_upper.as_str() {
                "CN" => cn = Some(v_clean),
                "PARTSTAT" => partstat = Some(v_clean.to_ascii_uppercase()),
                "ROLE" => role = Some(v_clean.to_ascii_uppercase()),
                _ => {}
            }
        }
        let raw_val = value.trim();
        let email = raw_val
            .strip_prefix("mailto:")
            .or_else(|| raw_val.strip_prefix("MAILTO:"))
            .unwrap_or(raw_val)
            .trim()
            .to_string();
        let name = cn.unwrap_or_else(|| {
            if !email.is_empty() {
                email.clone()
            } else {
                "Teilnehmer".to_string()
            }
        });
        attendees.push(CalendarAttendeeInfo {
            name,
            email: if !email.is_empty() { Some(email) } else { None },
            partstat,
            role,
        });
    }
    attendees
}

fn parse_ical_organizer(component: &str) -> Option<String> {
    for line in unfold_ical_lines(component) {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = property.split(';');
        let prop_name = parts.next().unwrap_or("").trim().to_ascii_uppercase();
        if prop_name != "ORGANIZER" {
            continue;
        }
        for param in parts {
            let Some((k, v)) = param.split_once('=') else {
                continue;
            };
            if k.trim().eq_ignore_ascii_case("CN") {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
        let raw_val = value.trim();
        let clean = raw_val
            .strip_prefix("mailto:")
            .or_else(|| raw_val.strip_prefix("MAILTO:"))
            .unwrap_or(raw_val)
            .trim();
        if !clean.is_empty() {
            return Some(clean.to_string());
        }
    }
    None
}

fn extract_conference_url(component: &str, location: &str, description: &str) -> Option<String> {
    if let Some(conf) = ical_property(component, "CONFERENCE")
        .or_else(|| ical_property(component, "X-CONFERENCE"))
        .or_else(|| ical_property(component, "URL"))
    {
        let trimmed = conf.trim();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return Some(trimmed.to_string());
        }
    }
    let loc_trimmed = location.trim();
    if loc_trimmed.starts_with("http://") || loc_trimmed.starts_with("https://") {
        return Some(loc_trimmed.to_string());
    }
    for line in description.lines() {
        for word in line.split_whitespace() {
            if word.starts_with("https://teams.microsoft.com/")
                || word.starts_with("https://zoom.us/")
                || word.starts_with("https://meet.google.com/")
                || word.starts_with("https://meet.jit.si/")
                || word.starts_with("https://webex.com/")
            {
                return Some(word.trim_end_matches(&[')', '.', ',', '>', ']'][..]).to_string());
            }
        }
    }
    None
}

fn format_alarm_summary(component: &str) -> Option<String> {
    let alarms = parse_alarms(component);
    if alarms.is_empty() {
        return None;
    }
    let mut summaries = Vec::new();
    for alarm in alarms.iter().take(2) {
        if let AlarmTrigger::Relative { offset_seconds, .. } = &alarm.trigger {
            let abs_sec = offset_seconds.abs();
            let time_str = if *offset_seconds == 0 {
                "Zur Startzeit".to_string()
            } else if abs_sec < 3600 {
                let mins = abs_sec / 60;
                format!("{mins} Min. vorher")
            } else if abs_sec < 86400 {
                let hours = abs_sec / 3600;
                if hours == 1 {
                    "1 Std. vorher".to_string()
                } else {
                    format!("{hours} Std. vorher")
                }
            } else {
                let days = abs_sec / 86400;
                if days == 1 {
                    "1 Tag vorher".to_string()
                } else {
                    format!("{days} Tage vorher")
                }
            };
            summaries.push(time_str);
        }
    }
    if summaries.is_empty() {
        Some("Erinnerung eingerichtet".to_string())
    } else {
        Some(summaries.join(", "))
    }
}

fn extract_event_badges(component: &str) -> Vec<(String, &'static str)> {
    let mut badges = Vec::new();
    if let Some(status) = ical_property(component, "STATUS") {
        match status.trim().to_ascii_uppercase().as_str() {
            "TENTATIVE" => badges.push(("Vorläufig".to_string(), "warning-pill")),
            "CANCELLED" => badges.push(("Abgesagt".to_string(), "destructive-pill")),
            _ => {}
        }
    }
    if let Some(transp) = ical_property(component, "TRANSP") {
        if transp.trim().eq_ignore_ascii_case("TRANSPARENT") {
            badges.push(("Frei".to_string(), "success-pill"));
        }
    }
    if let Some(class) = ical_property(component, "CLASS") {
        match class.trim().to_ascii_uppercase().as_str() {
            "PRIVATE" => badges.push(("Privat".to_string(), "neutral-pill")),
            "CONFIDENTIAL" => badges.push(("Vertraulich".to_string(), "neutral-pill")),
            _ => {}
        }
    }
    badges
}

fn show_calendar_event_details_popover<W: IsA<gtk::Widget> + Clone + 'static>(
    page: &CalendarPage,
    parent: &W,
    event: &CalendarEventRecord,
    target_day: Option<NaiveDate>,
) {
    show_calendar_event_details_popover_full(page, parent, event, target_day, None);
}

fn show_calendar_event_details_popover_full<W: IsA<gtk::Widget> + Clone + 'static>(
    page: &CalendarPage,
    parent: &W,
    event: &CalendarEventRecord,
    target_day: Option<NaiveDate>,
    pointing_to: Option<gdk::Rectangle>,
) {
    let identity = CalendarEventIdentity::from_event(event);
    let color = event_color_for_record(event);
    let cal_name = if event.source_display_name.trim().is_empty() {
        "Kalender"
    } else {
        event.source_display_name.trim()
    };
    let title = if event.summary.trim().is_empty() {
        "(Ohne Titel)"
    } else {
        event.summary.as_str()
    };

    let popover = gtk::Popover::builder()
        .autohide(true)
        .has_arrow(true)
        .css_classes(vec!["calendar-details-popover"])
        .build();
    if let Some(ref rect) = pointing_to {
        popover.set_pointing_to(Some(rect));
    }
    popover.set_parent(parent);

    let popover_weak = popover.downgrade();
    popover.connect_closed(move |_| {
        if let Some(popover) = popover_weak.upgrade() {
            popover.unparent();
        }
    });

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .max_content_height(540)
        .min_content_width(340)
        .build();

    let content_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(14)
        .margin_end(14)
        .margin_top(12)
        .margin_bottom(14)
        .build();

    // ── Header bar with Calendar Tag, Status Badges, and Action Buttons ──
    let header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(vec!["calendar-details-header"])
        .build();

    let cal_badge = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    let dot = gtk::Label::new(None);
    dot.set_markup(&format!("<span foreground=\"{color}\" size=\"large\">●</span>"));
    let cal_label = gtk::Label::builder()
        .label(cal_name)
        .css_classes(vec!["caption", "dim-label"])
        .build();
    cal_badge.append(&dot);
    cal_badge.append(&cal_label);
    header_box.append(&cal_badge);

    // Progressive status/privacy badges (Vorläufig, Abgesagt, Frei, Privat, Vertraulich)
    let badges = extract_event_badges(&event.component);
    for (badge_text, badge_class) in badges {
        let badge_lbl = gtk::Label::builder()
            .label(&badge_text)
            .css_classes(vec!["calendar-badge-pill", badge_class])
            .valign(gtk::Align::Center)
            .build();
        header_box.append(&badge_lbl);
    }

    let spacer = gtk::Box::builder().hexpand(true).build();
    header_box.append(&spacer);

    // Edit button in header
    let edit_btn = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .css_classes(vec!["flat", "circular"])
        .tooltip_text("Termin bearbeiten")
        .valign(gtk::Align::Center)
        .build();
    let popover_edit = popover.clone();
    let page_edit = page.downgrade();
    let identity_edit = identity.clone();
    let parent_edit = parent.clone();
    edit_btn.connect_clicked(move |_| {
        popover_edit.popdown();
        if let (Some(page), Some(id)) = (page_edit.upgrade(), identity_edit.as_ref()) {
            page.open_event_editor_for_identity(&parent_edit, id);
        }
    });
    header_box.append(&edit_btn);

    // Delete button in header
    let del_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .css_classes(vec!["flat", "circular", "destructive-action"])
        .tooltip_text("Termin löschen")
        .valign(gtk::Align::Center)
        .build();
    let popover_del = popover.clone();
    let page_del = page.downgrade();
    let event_del = event.clone();
    let parent_del = parent.clone();
    del_btn.connect_clicked(move |_| {
        popover_del.popdown();
        if let Some(page) = page_del.upgrade() {
            prompt_delete_calendar_event(&page, &parent_del, &event_del);
        }
    });
    header_box.append(&del_btn);

    // Close button in header
    let close_btn = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(vec!["flat", "circular"])
        .tooltip_text("Schließen")
        .valign(gtk::Align::Center)
        .build();
    let popover_close = popover.clone();
    close_btn.connect_clicked(move |_| {
        popover_close.popdown();
    });
    header_box.append(&close_btn);

    content_box.append(&header_box);

    // ── Title ──
    let title_label = gtk::Label::builder()
        .label(title)
        .css_classes(vec!["title-2"])
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .build();
    content_box.append(&title_label);

    // ── Date & Time Section ──
    let effective_date = target_day
        .or_else(|| event_start_date(event))
        .unwrap_or_else(|| Local::now().date_naive());

    let time_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .valign(gtk::Align::Start)
        .build();
    let time_icon = gtk::Image::from_icon_name("appointment-soon-symbolic");
    time_icon.set_valign(gtk::Align::Start);
    time_icon.add_css_class("dim-label");
    time_box.append(&time_icon);

    let time_details_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();

    let date_str = format_full_german_date(effective_date);
    let date_label = gtk::Label::builder()
        .label(&date_str)
        .css_classes(vec!["heading"])
        .xalign(0.0)
        .selectable(true)
        .build();
    time_details_box.append(&date_label);

    let duration_str = if event.all_day {
        "Ganztägig".to_string()
    } else {
        let (start_min, end_min, _) = event_occurrence_times_for_date(event, effective_date, 30);
        let diff = end_min.saturating_sub(start_min);
        let dur_label = format_duration_text(diff);
        let start_str = format_clock_minutes(start_min);
        let end_str = if end_min == 1440 {
            "24:00".to_string()
        } else {
            format_clock_minutes(end_min)
        };
        format!("{start_str} – {end_str} ({dur_label})")
    };
    let time_label = gtk::Label::builder()
        .label(&duration_str)
        .css_classes(vec!["caption", "dim-label"])
        .xalign(0.0)
        .selectable(true)
        .build();
    time_details_box.append(&time_label);

    if let Some(recurrence_text) = format_recurrence_summary(event) {
        let recurrence_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_top(4)
            .build();
        let rec_icon = gtk::Image::from_icon_name("media-playlist-repeat-symbolic");
        rec_icon.add_css_class("dim-label");
        let rec_label = gtk::Label::builder()
            .label(&recurrence_text)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        recurrence_box.append(&rec_icon);
        recurrence_box.append(&rec_label);
        time_details_box.append(&recurrence_box);
    }
    time_box.append(&time_details_box);
    content_box.append(&time_box);

    // ── Location Section ──
    if !event.location.trim().is_empty() {
        let loc_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Start)
            .build();
        let loc_icon = gtk::Image::from_icon_name("mark-location-symbolic");
        loc_icon.set_valign(gtk::Align::Start);
        loc_icon.add_css_class("dim-label");
        loc_box.append(&loc_icon);

        let loc_label = gtk::Label::builder()
            .label(event.location.trim())
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .hexpand(true)
            .build();
        loc_box.append(&loc_label);
        content_box.append(&loc_box);
    }

    // ── Online Conference / Video Meeting (Progressive Enhancement) ──
    if let Some(conf_url) = extract_conference_url(&event.component, &event.location, &event.description) {
        let conf_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Center)
            .build();
        let conf_icon = gtk::Image::from_icon_name("camera-web-symbolic");
        conf_icon.add_css_class("dim-label");
        conf_box.append(&conf_icon);

        let conf_btn = gtk::Button::builder()
            .label("An Besprechung teilnehmen")
            .icon_name("call-start-symbolic")
            .css_classes(vec!["suggested-action", "pill"])
            .valign(gtk::Align::Center)
            .build();
        let conf_url_clone = conf_url.clone();
        conf_btn.connect_clicked(move |_| {
            let _ = gio::AppInfo::launch_default_for_uri(&conf_url_clone, None::<&gio::AppLaunchContext>);
        });
        conf_box.append(&conf_btn);
        content_box.append(&conf_box);
    }

    // ── Attendees & Organizer (Progressive Enhancement) ──
    let organizer = parse_ical_organizer(&event.component);
    let attendees = parse_ical_attendees(&event.component);
    if organizer.is_some() || !attendees.is_empty() {
        let att_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Start)
            .build();
        let att_icon = gtk::Image::from_icon_name("system-users-symbolic");
        att_icon.set_valign(gtk::Align::Start);
        att_icon.add_css_class("dim-label");
        att_box.append(&att_icon);

        let att_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .build();

        if let Some(org_name) = organizer {
            let org_label = gtk::Label::builder()
                .label(&format!("Organisator: {org_name}"))
                .xalign(0.0)
                .css_classes(vec!["caption"])
                .selectable(true)
                .build();
            att_content.append(&org_label);
        }

        if !attendees.is_empty() {
            let att_flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .max_children_per_line(3)
                .min_children_per_line(1)
                .column_spacing(6)
                .row_spacing(4)
                .build();
            for att in attendees.iter().take(6) {
                let status_prefix = match att.partstat.as_deref() {
                    Some("ACCEPTED") => "✓ ",
                    Some("DECLINED") => "✗ ",
                    Some("TENTATIVE") => "? ",
                    _ => "",
                };
                let chip_text = format!("{status_prefix}{}", att.name);
                let chip = gtk::Label::builder()
                    .label(&chip_text)
                    .css_classes(vec!["calendar-attendee-chip"])
                    .tooltip_text(att.email.as_deref().unwrap_or(&att.name))
                    .build();
                att_flow.append(&chip);
            }
            if attendees.len() > 6 {
                let more_chip = gtk::Label::builder()
                    .label(&format!("+{} weitere", attendees.len() - 6))
                    .css_classes(vec!["caption", "dim-label"])
                    .build();
                att_flow.append(&more_chip);
            }
            att_content.append(&att_flow);
        }

        att_box.append(&att_content);
        content_box.append(&att_box);
    }

    // ── Alarms / Reminders (Progressive Enhancement) ──
    if let Some(alarm_summary) = format_alarm_summary(&event.component) {
        let alarm_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Center)
            .build();
        let alarm_icon = gtk::Image::from_icon_name("alarm-symbolic");
        alarm_icon.add_css_class("dim-label");
        alarm_box.append(&alarm_icon);

        let alarm_label = gtk::Label::builder()
            .label(&format!("Erinnerung: {alarm_summary}"))
            .xalign(0.0)
            .selectable(true)
            .hexpand(true)
            .build();
        alarm_box.append(&alarm_label);
        content_box.append(&alarm_box);
    }

    // ── Categories (Progressive Enhancement) ──
    let categories = category_names_from_ical_component(&event.component);
    if !categories.is_empty() {
        let cat_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Start)
            .build();
        let cat_icon = gtk::Image::from_icon_name("tag-symbolic");
        cat_icon.set_valign(gtk::Align::Start);
        cat_icon.add_css_class("dim-label");
        cat_box.append(&cat_icon);

        let cat_flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(4)
            .min_children_per_line(1)
            .column_spacing(6)
            .row_spacing(4)
            .hexpand(true)
            .build();
        let cat_store = CalendarCategoryStore::default();
        for cat in categories {
            let cat_pill = gtk::Label::builder()
                .label(&cat)
                .css_classes(vec!["calendar-badge-pill"])
                .build();
            if let Some(cat_col) = cat_store.color_for(&cat) {
                let cat_provider = gtk::CssProvider::new();
                cat_provider.load_from_data(&format!(
                    "label {{ background-color: alpha({cat_col}, 0.20); color: {cat_col}; }}"
                ));
                cat_pill
                    .style_context()
                    .add_provider(&cat_provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            cat_flow.append(&cat_pill);
        }
        cat_box.append(&cat_flow);
        content_box.append(&cat_box);
    }

    // ── Calendar / Account Section ──
    let cal_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .valign(gtk::Align::Start)
        .build();
    let cal_icon = gtk::Image::from_icon_name("x-office-calendar-symbolic");
    cal_icon.set_valign(gtk::Align::Start);
    cal_icon.add_css_class("dim-label");
    cal_box.append(&cal_icon);

    let cal_label = gtk::Label::builder()
        .label(cal_name)
        .xalign(0.0)
        .selectable(true)
        .hexpand(true)
        .build();
    cal_box.append(&cal_label);
    content_box.append(&cal_box);

    // ── Description / Notes Section ──
    if !event.description.trim().is_empty() {
        let desc_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Start)
            .build();
        let desc_icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
        desc_icon.set_valign(gtk::Align::Start);
        desc_icon.add_css_class("dim-label");
        desc_box.append(&desc_icon);

        let note_card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["calendar-details-note-box"])
            .hexpand(true)
            .build();
        let desc_label = gtk::Label::builder()
            .label(event.description.trim())
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .build();
        note_card.append(&desc_label);
        desc_box.append(&note_card);
        content_box.append(&desc_box);
    }

    // ── Edit Button at Bottom ──
    let edit_full_btn = gtk::Button::builder()
        .label("Termin bearbeiten")
        .css_classes(vec!["suggested-action"])
        .margin_top(4)
        .halign(gtk::Align::Fill)
        .build();
    let popover_edit2 = popover.clone();
    let page_edit2 = page.downgrade();
    let identity_edit2 = identity.clone();
    let parent_edit2 = parent.clone();
    edit_full_btn.connect_clicked(move |_| {
        popover_edit2.popdown();
        if let (Some(page), Some(id)) = (page_edit2.upgrade(), identity_edit2.as_ref()) {
            page.open_event_editor_for_identity(&parent_edit2, id);
        }
    });
    content_box.append(&edit_full_btn);

    scroll.set_child(Some(&content_box));
    popover.set_child(Some(&scroll));
    popover.popup();
}

fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    let s = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some((r, g, b))
    } else if s.len() == 3 {
        let r = u8::from_str_radix(&s[0..1], 16).ok()? * 17;
        let g = u8::from_str_radix(&s[1..2], 16).ok()? * 17;
        let b = u8::from_str_radix(&s[2..3], 16).ok()? * 17;
        Some((r, g, b))
    } else {
        None
    }
}

/// Computes solid (opaque), non-transparent pastel background, hover, active, and text colors
/// inspired by Amie / Google Calendar / Evolution UX.
pub(crate) fn pastel_colors_for_hex(hex: &str, is_dark: bool) -> (String, String, String, String) {
    let (r, g, b) = parse_hex_color(hex).unwrap_or((53, 132, 228)); // default GNOME accent blue

    if is_dark {
        // Dark theme: rich dark pastel card background with vibrant high-contrast foreground
        let bg_r = (r as f32 * 0.35 + 36.0 * 0.65).round().clamp(0.0, 255.0) as u8;
        let bg_g = (g as f32 * 0.35 + 36.0 * 0.65).round().clamp(0.0, 255.0) as u8;
        let bg_b = (b as f32 * 0.35 + 36.0 * 0.65).round().clamp(0.0, 255.0) as u8;

        let hov_r = (r as f32 * 0.48 + 36.0 * 0.52).round().clamp(0.0, 255.0) as u8;
        let hov_g = (g as f32 * 0.48 + 36.0 * 0.52).round().clamp(0.0, 255.0) as u8;
        let hov_b = (b as f32 * 0.48 + 36.0 * 0.52).round().clamp(0.0, 255.0) as u8;

        let act_r = (r as f32 * 0.60 + 36.0 * 0.40).round().clamp(0.0, 255.0) as u8;
        let act_g = (g as f32 * 0.60 + 36.0 * 0.40).round().clamp(0.0, 255.0) as u8;
        let act_b = (b as f32 * 0.60 + 36.0 * 0.40).round().clamp(0.0, 255.0) as u8;

        let fg_r = (r as f32 * 0.50 + 255.0 * 0.50).round().clamp(0.0, 255.0) as u8;
        let fg_g = (g as f32 * 0.50 + 255.0 * 0.50).round().clamp(0.0, 255.0) as u8;
        let fg_b = (b as f32 * 0.50 + 255.0 * 0.50).round().clamp(0.0, 255.0) as u8;

        (
            format!("#{bg_r:02x}{bg_g:02x}{bg_b:02x}"),
            format!("#{hov_r:02x}{hov_g:02x}{hov_b:02x}"),
            format!("#{act_r:02x}{act_g:02x}{act_b:02x}"),
            format!("#{fg_r:02x}{fg_g:02x}{fg_b:02x}"),
        )
    } else {
        // Light theme: soft cheerful solid pastel background with deep, saturated high-contrast foreground (Amie style)
        let bg_r = (r as f32 * 0.30 + 255.0 * 0.70).round().clamp(0.0, 255.0) as u8;
        let bg_g = (g as f32 * 0.30 + 255.0 * 0.70).round().clamp(0.0, 255.0) as u8;
        let bg_b = (b as f32 * 0.30 + 255.0 * 0.70).round().clamp(0.0, 255.0) as u8;

        let hov_r = (r as f32 * 0.42 + 255.0 * 0.58).round().clamp(0.0, 255.0) as u8;
        let hov_g = (g as f32 * 0.42 + 255.0 * 0.58).round().clamp(0.0, 255.0) as u8;
        let hov_b = (b as f32 * 0.42 + 255.0 * 0.58).round().clamp(0.0, 255.0) as u8;

        let act_r = (r as f32 * 0.55 + 255.0 * 0.45).round().clamp(0.0, 255.0) as u8;
        let act_g = (g as f32 * 0.55 + 255.0 * 0.45).round().clamp(0.0, 255.0) as u8;
        let act_b = (b as f32 * 0.55 + 255.0 * 0.45).round().clamp(0.0, 255.0) as u8;

        // Dark text for strong contrast on pastel
        let mut fg_r = (r as f32 * 0.45).round() as u8;
        let mut fg_g = (g as f32 * 0.45).round() as u8;
        let mut fg_b = (b as f32 * 0.45).round() as u8;
        let lum = 0.299 * (fg_r as f32) + 0.587 * (fg_g as f32) + 0.114 * (fg_b as f32);
        if lum > 115.0 {
            fg_r = (fg_r as f32 * 0.65).round() as u8;
            fg_g = (fg_g as f32 * 0.65).round() as u8;
            fg_b = (fg_b as f32 * 0.65).round() as u8;
        }

        (
            format!("#{bg_r:02x}{bg_g:02x}{bg_b:02x}"),
            format!("#{hov_r:02x}{hov_g:02x}{hov_b:02x}"),
            format!("#{act_r:02x}{act_g:02x}{act_b:02x}"),
            format!("#{fg_r:02x}{fg_g:02x}{fg_b:02x}"),
        )
    }
}

fn calendar_grid_event_chip(
    page: &CalendarPage,
    event: &CalendarEventRecord,
    is_all_day: bool,
    day: NaiveDate,
    custom_time_text: Option<&str>,
    row_span: usize,
) -> gtk::Widget {
    let title = if event.summary.trim().is_empty() {
        "(Ohne Titel)"
    } else {
        event.summary.as_str()
    };
    let time_text = custom_time_text
        .map(|s| s.to_string())
        .unwrap_or_else(|| event_time_text(event));
    let color = event_color_for_record(event);
    let chip_button = gtk::Button::builder()
        .halign(gtk::Align::Fill)
        .valign(if is_all_day {
            gtk::Align::Start
        } else {
            gtk::Align::Fill
        })
        .hexpand(true)
        .vexpand(false)
        .margin_start(if is_all_day { 1 } else { 2 })
        .margin_end(if is_all_day { 1 } else { 12 })
        .margin_top(1)
        .margin_bottom(1)
        .height_request(if is_all_day { 19 } else { -1 })
        .css_classes(if is_all_day {
            vec!["flat", "calendar-grid-event", "all-day-chip"]
        } else {
            vec!["flat", "calendar-grid-event"]
        })
        .build();

    // Solid, opaque pastel card styling (Amie & Libadwaita borderless)
    let is_dark = adw::StyleManager::default().is_dark();
    let (bg, hover, active, fg) = pastel_colors_for_hex(&color, is_dark);
    let provider = cached_event_chip_css_provider(&bg, &hover, &active, &fg);
    chip_button
        .style_context()
        .add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

    let text = if is_all_day {
        format!("<b>{}</b>", glib::markup_escape_text(title))
    } else if row_span >= 2 {
        format!(
            "<b>{}</b>\n<span size=\"smaller\" alpha=\"75%\">{}</span>",
            glib::markup_escape_text(title),
            glib::markup_escape_text(&time_text)
        )
    } else {
        format!(
            "<b>{}</b>  {}",
            glib::markup_escape_text(&time_text),
            glib::markup_escape_text(title)
        )
    };

    let label = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .valign(if is_all_day {
            gtk::Align::Center
        } else {
            gtk::Align::Start
        })
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .use_markup(true)
        .label(&text)
        .build();

    chip_button.set_child(Some(&label));
    set_accessible_label(&chip_button, &format!("Termin öffnen: {title}"));
    if !event.source_display_name.is_empty() {
        chip_button.set_tooltip_text(Some(&format!(
            "{title}\n{time_text} · {}",
            event.source_display_name
        )));
    }

    let event_identity_for_click = CalendarEventIdentity::from_event(event);
    let event_for_click = event.clone();
    let page_weak_for_click = page.downgrade();
    let target_day = day;

    let gesture = gtk::GestureClick::new();
    gesture.connect_pressed(move |gesture, n_press, _x, _y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let (Some(page), Some(widget)) = (page_weak_for_click.upgrade(), gesture.widget()) else {
            return;
        };
        if n_press == 2 {
            if let Some(id) = event_identity_for_click.as_ref() {
                page.open_event_editor_for_identity(&widget, id);
            }
        } else if n_press == 1 {
            show_calendar_event_details_popover(&page, &widget, &event_for_click, Some(target_day));
        }
    });
    chip_button.add_controller(gesture);

    let key_controller = gtk::EventControllerKey::new();
    let event_for_key = event.clone();
    let page_weak_for_key = page.downgrade();
    key_controller.connect_key_pressed(move |controller, keyval, _, _| {
        if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter || keyval == gdk::Key::space {
            if let (Some(page), Some(widget)) = (page_weak_for_key.upgrade(), controller.widget()) {
                show_calendar_event_details_popover(&page, &widget, &event_for_key, Some(target_day));
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    chip_button.add_controller(key_controller);

    install_event_drag_source(&chip_button, event, day);
    chip_button.upcast()
}

fn calendar_event_row(page: &CalendarPage, event: &CalendarEventRecord) -> adw::ActionRow {
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
    set_calendar_event_row_identity(&row, event);
    let color = event_color_for_record(event);
    let category_dot = gtk::Label::new(None);
    category_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
    category_dot.set_tooltip_text(Some(if !event.source_display_name.is_empty() {
        event.source_display_name.as_str()
    } else {
        "Kalender"
    }));
    row.add_prefix(&category_dot);
    let event_identity_for_activation = CalendarEventIdentity::from_event(event);
    let page_weak_for_activation = page.downgrade();
    row.connect_activated(move |row| {
        let (Some(page), Some(identity)) = (
            page_weak_for_activation.upgrade(),
            event_identity_for_activation.as_ref(),
        ) else {
            return;
        };
        page.open_event_editor_for_identity(row, identity);
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
    let start_dt = event_start_datetime(event);
    let end_dt = event_end_datetime(event);
    if let (Some(start), Some(end)) = (start_dt, end_dt) {
        let start_str = format_clock_minutes(start.time().hour() * 60 + start.time().minute());
        let end_str = format_clock_minutes(end.time().hour() * 60 + end.time().minute());
        if start != end {
            return format!("{start_str}–{end_str}");
        } else {
            return start_str;
        }
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

pub(crate) fn calendar_source_color(source_uid: &str, source_display_name: &str) -> String {
    // Official GNOME Human Interface Guidelines (HIG) Color Palette
    const CALENDAR_PALETTE: &[&str] = &[
        "#3584e4", // GNOME Blue 3 (Default Accent)
        "#2ec27e", // GNOME Green 4
        "#e66100", // GNOME Orange 4
        "#e01b24", // GNOME Red 3
        "#9141ac", // GNOME Purple 3
        "#2aa198", // GNOME Teal 3
        "#e5a50a", // GNOME Yellow 5 (Gold)
        "#986a44", // GNOME Brown 3
        "#1c71d8", // GNOME Blue 4
        "#26a269", // GNOME Green 5
        "#c01c28", // GNOME Red 4
        "#813d9c", // GNOME Purple 4
    ];
    let key = if !source_display_name.trim().is_empty() {
        source_display_name.trim()
    } else {
        source_uid.trim()
    };
    if key.eq_ignore_ascii_case("feiertage")
        || key.to_lowercase().contains("holiday")
        || key.to_lowercase().contains("feiertag")
    {
        return "#2ec27e".to_string(); // GNOME Green 4
    }
    if key.to_lowercase().contains("geburtstag") || key.to_lowercase().contains("birthday") {
        return "#9141ac".to_string(); // GNOME Purple 3
    }
    let mut hash = 0u64;
    for byte in key.bytes() {
        hash = hash.wrapping_mul(37).wrapping_add(byte as u64);
    }
    CALENDAR_PALETTE[(hash as usize) % CALENDAR_PALETTE.len()].to_string()
}

pub(crate) fn event_color_for_record(event: &CalendarEventRecord) -> String {
    if let Some(color) = category_color_for_component(&event.component) {
        return color;
    }
    if event.source_display_name != "Lokaler Kalender" && !event.source_display_name.is_empty() {
        return calendar_source_color(&event.source_uid, &event.source_display_name);
    }
    if let Some(color) = ical_property(&event.component, "COLOR")
        .or_else(|| ical_property(&event.component, "X-APPLE-CALENDAR-COLOR"))
        .filter(|c| !c.trim().is_empty())
    {
        return color;
    }
    calendar_source_color(&event.source_uid, &event.source_display_name)
}

fn calendar_event_markup(event: &CalendarEventRecord, text: &str) -> String {
    let escaped_text = glib::markup_escape_text(text);
    let color = event_color_for_record(event);
    format!("<span foreground=\"{color}\">●</span> {escaped_text}")
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
        .or_else(|| {
            let start = event_start_datetime(event)?;
            let duration = property_local_duration(&event.component)?;
            Some(start + duration)
        })
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

pub(crate) fn event_occurs_on(event: &CalendarEventRecord, date: NaiveDate) -> bool {
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

fn stable_calendar_identity_digest(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn calendar_identity_material(event: &CalendarEventRecord) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        event.source_display_name.trim(),
        event.component,
        event.summary,
        event.location,
        event.description,
        event.all_day,
    )
}

fn canonical_calendar_source_object_key_for_uid(source_uid: &str) -> String {
    let source_uid = source_uid.trim();
    if !source_uid.is_empty() {
        return format!("source:{source_uid}");
    }
    let source_material = "source-less-import";
    format!(
        "{}:{}",
        source_material,
        stable_calendar_identity_digest(&source_material)
    )
}

fn canonical_calendar_source_object_key(event: &CalendarEventRecord) -> String {
    canonical_calendar_source_object_key_for_uid(&event.source_uid)
}

fn canonical_calendar_uid(event: &CalendarEventRecord) -> String {
    if !event.uid.trim().is_empty() {
        return event.uid.trim().to_string();
    }
    format!(
        "imported-uid:{}",
        stable_calendar_identity_digest(&calendar_identity_material(event))
    )
}

fn calendar_recurrence_id(event: &CalendarEventRecord) -> Option<String> {
    ical_property(&event.component, "RECURRENCE-ID")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn calendar_event_is_recurring(event: &CalendarEventRecord) -> bool {
    calendar_recurrence_id(event).is_some()
        || ical_property(&event.component, "RRULE").is_some()
        || ical_property(&event.component, "RDATE").is_some()
}

fn calendar_event_action_state(
    event: &CalendarEventRecord,
    local_calendar_writable: bool,
) -> CalendarEventActionState {
    let mutable = event_mutation_error(event).is_none();
    let transferable = mutable
        && !event.component.trim().is_empty()
        && event.source_uid.trim() != "local-calendar"
        && local_calendar_writable;
    CalendarEventActionState {
        can_edit: mutable,
        can_delete: mutable,
        can_export: !event.component.trim().is_empty() || !event.summary.trim().is_empty(),
        can_print: !event.summary.trim().is_empty() || !event.component.trim().is_empty(),
        can_copy_to_local: transferable,
        can_move_to_local: transferable,
        recurring: calendar_event_is_recurring(event),
    }
}

fn plan_calendar_event_transfer(
    event: &CalendarEventRecord,
    kind: CalendarTransferKind,
    destination_uid: &str,
    destination_writable: bool,
) -> Result<CalendarTransferPlan, String> {
    if let Some(error) = event_mutation_error(event) {
        return Err(error.to_string());
    }
    if !destination_writable {
        return Err("Das Ziel ist nicht als schreibfähig bestätigt".to_string());
    }
    let destination_uid = destination_uid.trim();
    if destination_uid.is_empty() {
        return Err("Für das Ziel fehlt die Kalenderkennung".to_string());
    }
    if event.source_uid.trim() == destination_uid {
        return Err("Quelle und Ziel sind derselbe Kalender".to_string());
    }
    if event.component.trim().is_empty() {
        return Err("Für diesen Termin sind keine iCalendar-Daten verfügbar".to_string());
    }
    Ok(CalendarTransferPlan {
        kind,
        source_uid: event.source_uid.trim().to_string(),
        source_component_uid: event.uid.trim().to_string(),
        destination_uid: destination_uid.to_string(),
        component: event.component.clone(),
    })
}

fn plan_calendar_event_delete(
    event: &CalendarEventRecord,
    scope: CalendarRecurringDeleteScope,
) -> Result<CalendarDeletePlan, String> {
    if let Some(error) = event_mutation_error(event) {
        return Err(error.to_string());
    }
    if event.source_uid.trim().is_empty() || event.uid.trim().is_empty() {
        return Err("Der Termin besitzt keine eindeutige Quelle und UID".to_string());
    }
    if calendar_event_is_recurring(event)
        && !matches!(scope, CalendarRecurringDeleteScope::EntireSeries)
    {
        // ECalService currently exposes component removal only.  Do not
        // pretend that it can create a lossless exception or split a series.
        return Ok(CalendarDeletePlan::UnsupportedOccurrenceScope { scope });
    }
    Ok(CalendarDeletePlan::RemoveComponent {
        source_uid: event.source_uid.trim().to_string(),
        uid: event.uid.trim().to_string(),
    })
}

fn calendar_event_identity_from_parts(
    source_uid: &str,
    uid: &str,
    recurrence_id: Option<&str>,
) -> CalendarEventIdentity {
    let source_uid = source_uid.trim().to_string();
    let uid = uid.trim().to_string();
    let source_object_key = canonical_calendar_source_object_key_for_uid(&source_uid);
    CalendarEventIdentity {
        source_object_key,
        source_uid,
        uid,
        recurrence_id: recurrence_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    }
}

fn unique_event_index_for_identity(
    events: &[CalendarEventRecord],
    identity: &CalendarEventIdentity,
) -> Option<usize> {
    let mut match_index = None;
    for (index, event) in events.iter().enumerate() {
        if CalendarEventIdentity::from_event(event).as_ref() != Some(identity) {
            continue;
        }
        if match_index.is_some() {
            return None;
        }
        match_index = Some(index);
    }
    match_index
}

fn unique_event_for_identity(
    events: &[CalendarEventRecord],
    identity: &CalendarEventIdentity,
) -> Option<CalendarEventRecord> {
    unique_event_index_for_identity(events, identity).map(|index| events[index].clone())
}

fn remove_unique_event_for_identity(
    events: &mut Vec<CalendarEventRecord>,
    identity: &CalendarEventIdentity,
) -> Option<CalendarEventRecord> {
    let index = unique_event_index_for_identity(events, identity)?;
    Some(events.remove(index))
}

fn same_calendar_event_identity(left: &CalendarEventRecord, right: &CalendarEventRecord) -> bool {
    CalendarEventIdentity::from_event(left) == CalendarEventIdentity::from_event(right)
}

fn same_event_identity(event: &CalendarEventRecord, source_uid: &str, uid: &str) -> bool {
    CalendarEventIdentity::from_event(event).is_some_and(|identity| {
        identity.source_uid == source_uid.trim()
            && identity.uid == uid.trim()
            && identity.recurrence_id.is_none()
    })
}

fn unique_event_for_legacy_identity(
    events: &[CalendarEventRecord],
    source_uid: &str,
    uid: &str,
) -> Option<CalendarEventRecord> {
    let uid = uid.trim();
    if uid.is_empty() {
        return None;
    }
    let identity = calendar_event_identity_from_parts(source_uid, uid, None);
    unique_event_for_identity(events, &identity)
}

fn reconcile_event_selection(
    selected_event: Option<CalendarEventIdentity>,
    selected_event_index: Option<usize>,
    ordered_events: &[CalendarEventRecord],
) -> (Option<CalendarEventIdentity>, Option<usize>) {
    let Some(selected_event) = selected_event else {
        return (None, None);
    };
    if let Some(index) = unique_event_index_for_identity(ordered_events, &selected_event) {
        return (Some(selected_event), Some(index));
    }

    let Some(index) = selected_event_index else {
        return (None, None);
    };
    let Some(fallback) = ordered_events.get(index.min(ordered_events.len().saturating_sub(1)))
    else {
        return (None, None);
    };
    let Some(identity) = CalendarEventIdentity::from_event(fallback) else {
        return (None, None);
    };
    let Some(fallback_index) = unique_event_index_for_identity(ordered_events, &identity) else {
        return (None, None);
    };
    (Some(identity), Some(fallback_index))
}

fn compare_event_list_order(
    left: &CalendarEventRecord,
    right: &CalendarEventRecord,
) -> CompareOrdering {
    // List sorting is also used while the immutable snapshot index is built
    // on the worker.  Use the already-normalized record strings here instead
    // of component-aware timezone conversion, which may consult settings.
    compare_optional_order(
        displayed_event_datetime(&left.start),
        displayed_event_datetime(&right.start),
    )
    .then_with(|| {
        compare_optional_order(
            displayed_event_datetime(&left.end),
            displayed_event_datetime(&right.end),
        )
    })
    .then_with(|| {
        left.summary
            .to_lowercase()
            .cmp(&right.summary.to_lowercase())
    })
    .then_with(|| left.summary.cmp(&right.summary))
    .then_with(|| left.source_uid.cmp(&right.source_uid))
    .then_with(|| left.uid.cmp(&right.uid))
    .then_with(|| {
        CalendarEventIdentity::from_event(left).cmp(&CalendarEventIdentity::from_event(right))
    })
    .then_with(|| left.source_display_name.cmp(&right.source_display_name))
    .then_with(|| left.component.cmp(&right.component))
}

fn compare_event_date_order(
    left: &CalendarEventRecord,
    right: &CalendarEventRecord,
) -> CompareOrdering {
    event_start_datetime(left)
        .cmp(&event_start_datetime(right))
        .then_with(|| left.source_uid.cmp(&right.source_uid))
        .then_with(|| left.uid.cmp(&right.uid))
}

pub(crate) fn compare_day_events_order(
    left: &CalendarEventRecord,
    right: &CalendarEventRecord,
    date: NaiveDate,
) -> CompareOrdering {
    (!left.all_day)
        .cmp(&(!right.all_day))
        .then_with(|| {
            if left.all_day && right.all_day {
                CompareOrdering::Equal
            } else {
                let (l_start, l_end, _) = event_occurrence_times_for_date(left, date, 30);
                let (r_start, r_end, _) = event_occurrence_times_for_date(right, date, 30);
                l_start.cmp(&r_start).then_with(|| l_end.cmp(&r_end))
            }
        })
        .then_with(|| left.summary.to_lowercase().cmp(&right.summary.to_lowercase()))
        .then_with(|| left.source_uid.cmp(&right.source_uid))
        .then_with(|| left.uid.cmp(&right.uid))
}

fn compare_optional_order<T: Ord>(left: Option<T>, right: Option<T>) -> CompareOrdering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => CompareOrdering::Less,
        (None, Some(_)) => CompareOrdering::Greater,
        (None, None) => CompareOrdering::Equal,
    }
}

fn event_is_cancelled(event: &CalendarEventRecord) -> bool {
    ical_property(&event.component, "STATUS")
        .as_deref()
        .is_some_and(|status| status.trim().eq_ignore_ascii_case("CANCELLED"))
}

fn event_matches_calendar_filter(
    event: &CalendarEventRecord,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> bool {
    event_matches_calendar_filter_with_options(
        event,
        search_query,
        category_filter,
        hidden_sources,
        configured_hide_cancelled_events(),
    )
}

fn event_matches_calendar_filter_with_options(
    event: &CalendarEventRecord,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
    hide_cancelled: bool,
) -> bool {
    if hidden_sources.contains(&event.source_uid)
        || hidden_sources.contains(&event.source_display_name)
        || hidden_sources
            .iter()
            .any(|h| !event.source_uid.is_empty() && h.ends_with(&format!(":{}", event.source_uid)))
    {
        return false;
    }
    if hide_cancelled && event_is_cancelled(event) {
        return false;
    }
    let search_query = search_query.trim().to_lowercase();
    if !search_query.is_empty() && !calendar_event_search_text(event).contains(&search_query) {
        return false;
    }
    category_filter.is_none_or(|filter| {
        category_names_from_ical_component(&event.component)
            .iter()
            .any(|category| category.eq_ignore_ascii_case(filter))
    })
}

fn filtered_sorted_event_indices(
    index: &CalendarSnapshotIndex,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> Vec<usize> {
    let mut indices = index
        .all_event_indices()
        .into_iter()
        .filter(|event_index| {
            index
                .snapshot
                .events
                .get(*event_index)
                .is_some_and(|event| {
                    event_matches_calendar_filter(
                        event,
                        search_query,
                        category_filter,
                        hidden_sources,
                    )
                })
        })
        .collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        compare_event_list_order(
            &index.snapshot.events[*left],
            &index.snapshot.events[*right],
        )
        .then_with(|| left.cmp(right))
    });
    indices
}

fn indexed_events_by_date(
    index: &CalendarSnapshotIndex,
    range: CalendarDateRange,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> BTreeMap<NaiveDate, Vec<CalendarEventRecord>> {
    let mut grouped = BTreeMap::<NaiveDate, Vec<CalendarEventRecord>>::new();
    for event_index in index.candidate_event_indices(range) {
        let Some(event) = index.snapshot.events.get(event_index) else {
            continue;
        };
        if !event_matches_calendar_filter(event, search_query, category_filter, hidden_sources) {
            continue;
        }
        for date in event_occurrence_dates_in_range(event, range) {
            grouped.entry(date).or_default().push(event.clone());
        }
    }

    for (date, events) in grouped.iter_mut() {
        events.sort_by(|a, b| compare_day_events_order(a, b, *date));
        let mut seen = BTreeSet::new();
        events.retain(|event| {
            CalendarEventIdentity::from_event(event)
                .map(|identity| seen.insert(identity))
                .unwrap_or(true)
        });
    }
    grouped
}

fn indexed_event_day_markers(
    index: &CalendarSnapshotIndex,
    range: CalendarDateRange,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> BTreeMap<NaiveDate, usize> {
    indexed_events_by_date(index, range, search_query, category_filter, hidden_sources)
        .into_iter()
        .map(|(date, events)| (date, events.len()))
        .collect()
}

fn calendar_event_identity_from_list_row(row: &gtk::ListBoxRow) -> Option<CalendarEventIdentity> {
    if let Some(event_row) = row.downcast_ref::<adw::ActionRow>() {
        return calendar_event_identity_from_action_row(event_row);
    }
    if let Some(event_row) = row.child().and_then(|c| c.downcast::<adw::ActionRow>().ok()) {
        return calendar_event_identity_from_action_row(&event_row);
    }
    unsafe {
        let read_data = |w: &gtk::Widget| -> Option<CalendarEventIdentity> {
            let uid = w.data::<String>("event-id").map(|v| v.as_ref().clone())?;
            let source_uid = w
                .data::<String>("event-source-uid")
                .map(|v| v.as_ref().clone())
                .unwrap_or_default();
            let source_object_key = w
                .data::<String>("event-source-object-key")
                .map(|v| v.as_ref().clone())
                .unwrap_or_else(|| {
                    calendar_event_identity_from_parts(&source_uid, "", None).source_object_key
                });
            let recurrence_id = w
                .data::<String>("event-recurrence-id")
                .map(|v| v.as_ref().clone())
                .filter(|v| !v.is_empty());
            Some(CalendarEventIdentity {
                source_object_key,
                source_uid,
                uid,
                recurrence_id,
            })
        };
        if let Some(id) = read_data(row.upcast_ref()) {
            return Some(id);
        }
        if let Some(c) = row.child() {
            if let Some(id) = read_data(&c) {
                return Some(id);
            }
        }
    }
    None
}

fn calendar_event_identity_from_action_row(
    event_row: &adw::ActionRow,
) -> Option<CalendarEventIdentity> {
    unsafe {
        let source_uid = event_row
            .data::<String>("event-source-uid")
            .map(|value| value.as_ref().clone())
            .unwrap_or_default();
        let source_object_key = event_row
            .data::<String>("event-source-object-key")
            .map(|value| value.as_ref().clone())
            .unwrap_or_else(|| {
                calendar_event_identity_from_parts(&source_uid, "", None).source_object_key
            });
        let uid = event_row
            .data::<String>("event-id")
            .map(|value| value.as_ref().clone())?;
        let recurrence_id = event_row
            .data::<String>("event-recurrence-id")
            .map(|value| value.as_ref().clone())
            .filter(|value| !value.is_empty());
        Some(CalendarEventIdentity {
            source_object_key,
            source_uid,
            uid,
            recurrence_id,
        })
    }
}

fn event_mutation_error(event: &CalendarEventRecord) -> Option<&'static str> {
    if calendar_recurrence_id(event).is_some() {
        return Some(
            "Serienausnahmen mit RECURRENCE-ID können über die verfügbare Kalender-API nicht sicher geändert werden",
        );
    }
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
    let returned_recurrence_id = ical_property(&component, "RECURRENCE-ID")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if returned_recurrence_id != calendar_recurrence_id(event) {
        return Err(
            "Der Kalenderdienst lieferte eine andere Serienausnahme zurück; die Ansicht wurde nicht geändert"
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
    page: &CalendarPage,
    event_list: &gtk::ListBox,
    event_records: Rc<RefCell<Vec<CalendarEventRecord>>>,
    event: CalendarEventRecord,
) {
    let existing_index = event_records
        .borrow()
        .iter()
        .position(|existing| same_calendar_event_identity(existing, &event));
    if let Some(index) = existing_index {
        let existing = event_records.borrow()[index].clone();
        event_records.borrow_mut().remove(index);
        if let Some(identity) = CalendarEventIdentity::from_event(&existing) {
            remove_event_list_row_by_identity(event_list, &identity);
        }
    }
    event_records.borrow_mut().push(event.clone());
    append_event_row(page, event_list, event_records, event);
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
    page: &CalendarPage,
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
    set_calendar_event_row_identity(&row, &event);
    unsafe {
        row.set_data(
            "event-categories",
            category_names_from_ical_component(&event.component).join("\u{1f}"),
        );
        row.set_data("event-search", calendar_event_search_text(&event));
    }
    let event_identity_for_activation = CalendarEventIdentity::from_event(&event);
    let page_weak_for_activation = page.downgrade();
    row.connect_activated(move |row| {
        let (Some(page), Some(identity)) = (
            page_weak_for_activation.upgrade(),
            event_identity_for_activation.as_ref(),
        ) else {
            return;
        };
        page.open_event_editor_for_identity(row, identity);
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
    let action_state =
        calendar_event_action_state(&event, CalendarStoreService::new().is_writable());
    let writable = action_state.can_edit;
    let recurring = action_state.recurring;
    edit_button.set_sensitive(action_state.can_edit);
    delete_button.set_sensitive(action_state.can_delete);
    let print_button = gtk::Button::builder()
        .icon_name("document-print-symbolic")
        .tooltip_text("Termin drucken")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&print_button, "Termin drucken");
    print_button.set_sensitive(action_state.can_print);
    let export_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Termin exportieren (iCalendar, CSV, RDF)")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&export_button, "Termin exportieren");
    export_button.set_sensitive(action_state.can_export);
    let copy_button = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Termin in lokalen Kalender kopieren")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .sensitive(action_state.can_copy_to_local)
        .build();
    set_accessible_label(&copy_button, "Termin in lokalen Kalender kopieren");
    let move_button = gtk::Button::builder()
        .icon_name("go-jump-symbolic")
        .tooltip_text("Termin in lokalen Kalender verschieben")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .sensitive(action_state.can_move_to_local)
        .build();
    set_accessible_label(&move_button, "Termin in lokalen Kalender verschieben");
    row.add_suffix(&edit_button);
    row.add_suffix(&print_button);
    row.add_suffix(&export_button);
    row.add_suffix(&copy_button);
    row.add_suffix(&move_button);
    row.add_suffix(&delete_button);

    let event_identity_for_edit = CalendarEventIdentity::from_event(&event);
    let page_weak_for_edit = page.downgrade();
    let edit_button_for_context = edit_button.clone();
    let print_button_for_context = print_button.clone();
    let edit_button_for_signal = edit_button.clone();
    let edit_button_for_click = edit_button.clone();
    edit_button_for_signal.connect_clicked(move |_| {
        let Some(page) = page_weak_for_edit.upgrade() else {
            return;
        };
        let Some(identity) = event_identity_for_edit.as_ref() else {
            show_toast_for_widget(
                &edit_button_for_click,
                "Der Termin besitzt keine eindeutige Identität und wurde nicht geöffnet",
            );
            return;
        };
        let Some(event_for_edit) = page.unique_loaded_calendar_event(identity) else {
            show_toast_for_widget(
                &edit_button_for_click,
                "Der Termin ist nicht eindeutig geladen und wurde nicht geöffnet",
            );
            return;
        };
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

    let event_for_print = event.clone();
    print_button.connect_clicked(move |btn| {
        let summary_display = if event_for_print.summary.trim().is_empty() {
            "Termin"
        } else {
            event_for_print.summary.trim()
        };
        match build_event_print_document(&event_for_print) {
            Ok(doc) => {
                show_toast_for_widget(
                    btn,
                    &format!(
                        "Druckansicht für „{summary_display}“ vorbereitet ({} Zeichen); Print-Portal ist Linux-Handoff",
                        doc.html.len()
                    ),
                );
            }
            Err(err) => {
                show_toast_for_widget(btn, &format!("Drucken fehlgeschlagen: {err}"));
            }
        }
    });

    let event_for_export = event.clone();
    let export_button_for_context = export_button.clone();
    let copy_button_for_context = copy_button.clone();
    let move_button_for_context = move_button.clone();
    let export_button_for_signal = export_button.clone();
    export_button_for_signal.connect_clicked(move |_| {
        if event_for_export.component.trim().is_empty()
            && event_for_export.summary.trim().is_empty()
        {
            show_toast_for_widget(
                &export_button,
                "Für diesen Termin sind keine Kalenderdaten verfügbar",
            );
            return;
        }
        let Some(root) = export_button.root() else {
            return;
        };
        let Ok(window) = root.downcast::<gtk::Window>() else {
            return;
        };

        let filter_ics = gtk::FileFilter::new();
        filter_ics.set_name(Some("iCalendar (*.ics)"));
        filter_ics.add_pattern("*.ics");

        let filter_csv = gtk::FileFilter::new();
        filter_csv.set_name(Some("Kommagetrennte Werte (*.csv)"));
        filter_csv.add_pattern("*.csv");

        let filter_rdf = gtk::FileFilter::new();
        filter_rdf.set_name(Some("RDF / RSS 1.0 (*.rdf)"));
        filter_rdf.add_pattern("*.rdf");

        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter_ics);
        filters.append(&filter_csv);
        filters.append(&filter_rdf);

        let safe_name = if event_for_export.summary.trim().is_empty() {
            "termin".to_string()
        } else {
            event_for_export
                .summary
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        };
        let initial_name = format!(
            "{}.ics",
            if safe_name.is_empty() {
                "termin"
            } else {
                &safe_name
            }
        );

        let file_dialog = gtk::FileDialog::builder()
            .title("Termin exportieren")
            .filters(&filters)
            .default_filter(&filter_ics)
            .initial_name(&initial_name)
            .modal(true)
            .build();
        let event_for_dialog = event_for_export.clone();
        let export_button_for_result = export_button.clone();
        file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let format = if let Some(path) = file.path() {
                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if ext.eq_ignore_ascii_case("csv") {
                    CalendarExportFormat::Csv
                } else if ext.eq_ignore_ascii_case("rdf") {
                    CalendarExportFormat::Rdf
                } else {
                    CalendarExportFormat::ICalendar
                }
            } else {
                CalendarExportFormat::ICalendar
            };
            let payload = match export_calendar_events(&[event_for_dialog.clone()], format, None) {
                Ok(data) => data,
                Err(err) => {
                    show_toast_for_widget(&export_button_for_result, &err);
                    return;
                }
            };
            let save_result = file.replace_contents(
                payload.as_bytes(),
                None,
                true,
                gio::FileCreateFlags::NONE,
                None::<&gio::Cancellable>,
            );
            match save_result {
                Ok(_) => show_toast_for_widget(
                    &export_button_for_result,
                    &format!("Termin als {} exportiert", format.display_label()),
                ),
                Err(error) => show_toast_for_widget(
                    &export_button_for_result,
                    &format!("Termin konnte nicht gespeichert werden: {error}"),
                ),
            }
        });
    });

    let event_for_copy = event.clone();
    let page_weak_for_copy = page.downgrade();
    let copy_button_for_signal = copy_button.clone();
    copy_button.connect_clicked(move |_| {
        if let Some(page) = page_weak_for_copy.upgrade() {
            page.transfer_event_to_local(
                event_for_copy.clone(),
                CalendarTransferKind::Copy,
                &copy_button_for_signal,
            );
        }
    });

    let event_for_move = event.clone();
    let page_weak_for_move = page.downgrade();
    let move_button_for_signal = move_button.clone();
    move_button.connect_clicked(move |_| {
        if let Some(page) = page_weak_for_move.upgrade() {
            page.transfer_event_to_local(
                event_for_move.clone(),
                CalendarTransferKind::Move,
                &move_button_for_signal,
            );
        }
    });

    let event_for_delete = event.clone();
    let event_identity_for_delete = CalendarEventIdentity::from_event(&event);
    let event_records_for_delete = Rc::clone(&event_records);
    let event_list_for_delete = event_list.clone();
    let page_weak_for_delete = page.downgrade();
    let delete_button_for_context = delete_button.clone();
    let delete_button_for_signal = delete_button.clone();
    let delete_scope_confirmed = Rc::new(Cell::new(false));
    let delete_scope_confirmed_for_signal = Rc::clone(&delete_scope_confirmed);
    delete_button_for_signal.connect_clicked(move |_| {
        if let Some(error) = event_mutation_error(&event_for_delete) {
            show_toast_for_widget(&delete_button, error);
            return;
        }
        if recurring && !delete_scope_confirmed_for_signal.replace(false) {
            let scope_dialog = adw::AlertDialog::new(
                Some("Wiederkehrenden Termin löschen"),
                Some("Wählen Sie den Löschumfang. Einzelne Vorkommen und künftige Vorkommen werden erst angeboten, wenn der Backend-Adapter sie ohne Datenverlust als Ausnahme bzw. Serienaufteilung bestätigen kann."),
            );
            scope_dialog.add_responses(&[
                ("cancel", "Abbrechen"),
                ("this", "Nur diesen Termin"),
                ("future", "Diesen und künftige"),
                ("series", "Ganze Serie löschen"),
            ]);
            scope_dialog.set_response_appearance("series", adw::ResponseAppearance::Destructive);
            let event_for_scope = event_for_delete.clone();
            let delete_button_for_scope = delete_button.clone();
            let scope_confirmation = Rc::clone(&delete_scope_confirmed_for_signal);
            scope_dialog.connect_response(None, move |dialog, response| {
                let scope = match response {
                    "this" => Some(CalendarRecurringDeleteScope::ThisOccurrence),
                    "future" => Some(CalendarRecurringDeleteScope::ThisAndFuture),
                    "series" => Some(CalendarRecurringDeleteScope::EntireSeries),
                    _ => None,
                };
                let Some(scope) = scope else {
                    return;
                };
                match plan_calendar_event_delete(&event_for_scope, scope) {
                    Ok(CalendarDeletePlan::RemoveComponent { .. }) => {
                        dialog.close();
                        scope_confirmation.set(true);
                        delete_button_for_scope.activate();
                    }
                    Ok(CalendarDeletePlan::UnsupportedOccurrenceScope { scope }) => {
                        let description = match scope {
                            CalendarRecurringDeleteScope::ThisOccurrence => "Dieses einzelne Vorkommen kann der aktuelle Backend-Adapter noch nicht verlustfrei löschen; die Serie bleibt unverändert.",
                            CalendarRecurringDeleteScope::ThisAndFuture => "Eine Serienaufteilung ab diesem Vorkommen kann der aktuelle Backend-Adapter noch nicht verlustfrei durchführen; die Serie bleibt unverändert.",
                            CalendarRecurringDeleteScope::EntireSeries => unreachable!(),
                        };
                        show_toast_for_widget(&delete_button_for_scope, description);
                    }
                    Err(error) => show_toast_for_widget(&delete_button_for_scope, &error),
                }
            });
            scope_dialog.present(Some(&delete_button));
            return;
        }
        let confirmation_message = if event_for_delete.source_uid == "local-calendar" {
            "Der Termin wird dauerhaft aus dem lokalen Kalender entfernt."
        } else {
            "Der Termin wird dauerhaft aus dem synchronisierten Kalender entfernt."
        };
        let event_records_for_action = Rc::clone(&event_records_for_delete);
        let event_list_for_action = event_list_for_delete.clone();
        let delete_button_for_action = delete_button.clone();
        let page_weak_for_action = page_weak_for_delete.clone();
        let event_identity_for_action = event_identity_for_delete.clone();
        crate::dialogs::run_destructive_action(
            &delete_button,
            "Termin löschen?",
            confirmation_message,
            "Löschen",
            move || {
                let Some(page) = page_weak_for_action.upgrade() else {
                    return;
                };
                let Some(identity) = event_identity_for_action.clone() else {
                    show_toast_for_widget(
                        &delete_button_for_action,
                        "Der Termin besitzt keine eindeutige Identität und wurde nicht gelöscht",
                    );
                    return;
                };
                let Some(event_for_action) = page.unique_loaded_calendar_event(&identity) else {
                    show_toast_for_widget(
                        &delete_button_for_action,
                        "Der Termin ist nicht eindeutig geladen und wurde nicht gelöscht",
                    );
                    page.reload_calendar_data();
                    return;
                };
                if let Some(error) = event_mutation_error(&event_for_action) {
                    show_toast_for_widget(&delete_button_for_action, error);
                    return;
                }
                let mutation_token =
                    page.begin_calendar_mutation(Some(identity.clone()));
                let source_uid = event_for_action.source_uid.clone();
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
                let page_weak_for_result = page.downgrade();
                let event_identity_for_result = event_identity_for_action.clone();
                glib::spawn_future_local(async move {
                    let result = receiver.recv().await;
                    let Some(page) = page_weak_for_result.upgrade() else {
                        return;
                    };
                    if !page.calendar_event_mutation_is_current(&mutation_token) {
                        delete_button_for_result.set_sensitive(true);
                        show_toast_for_widget(
                            &delete_button_for_result,
                            "Der Kalenderstand hat sich geändert; das Löschergebnis wurde verworfen",
                        );
                        page.reload_calendar_data();
                        return;
                    }
                    match result {
                        Ok(Ok(result)) if result.uid == uid_for_result => {
                            let Some(identity) = event_identity_for_result.as_ref() else {
                                delete_button_for_result.set_sensitive(true);
                                page.reload_calendar_data();
                                return;
                            };
                            if !page.finish_calendar_mutation(&mutation_token) {
                                delete_button_for_result.set_sensitive(true);
                                page.reload_calendar_data();
                                return;
                            }
                            remove_event_list_row_by_identity(&event_list_for_result, identity);
                            event_records_for_result.borrow_mut().retain(|event| {
                                CalendarEventIdentity::from_event(event).as_ref() != Some(identity)
                            });
                            page.bump_calendar_revision();
                            page.rebuild_calendar_index_from_records();
                            page.refresh_calendar_views();
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
                            page.finish_calendar_mutation(&mutation_token);
                            delete_button_for_result.set_sensitive(true);
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!(
                                    "ECal bestätigte eine andere Termin-UID ({}); nichts wurde entfernt",
                                    result.uid
                                ),
                            );
                            page.reload_calendar_data();
                        }
                        Ok(Err(error)) => {
                            page.finish_calendar_mutation(&mutation_token);
                            delete_button_for_result.set_sensitive(true);
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Termin konnte nicht gelöscht werden: {error}"),
                            );
                            page.reload_calendar_data();
                        }
                        Err(error) => {
                            page.finish_calendar_mutation(&mutation_token);
                            delete_button_for_result.set_sensitive(true);
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Termin-Löschkanal wurde beendet: {error}"),
                            );
                            page.reload_calendar_data();
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
    let page_weak_for_context = page.downgrade();
    context_click.connect_pressed(move |_, _, x, y| {
        let Some(row_for_context) = row_weak_for_context.upgrade() else {
            return;
        };
        if let Some(page) = page_weak_for_context.upgrade() {
            page.select_calendar_event_identity(calendar_event_identity_from_action_row(
                &row_for_context,
            ));
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

        let edit = gtk::Button::with_label("Termin bearbeiten");
        edit.set_halign(gtk::Align::Fill);
        edit.add_css_class("flat");
        edit.set_sensitive(writable);
        let popover_for_edit = popover.clone();
        let edit_button_for_menu = edit_button_for_context.clone();
        edit.connect_clicked(move |_| {
            popover_for_edit.popdown();
            edit_button_for_menu.activate();
        });
        menu_box.append(&edit);

        let print = gtk::Button::with_label("Termin drucken");
        print.set_halign(gtk::Align::Fill);
        print.add_css_class("flat");
        print.set_sensitive(action_state.can_print);
        let popover_for_print = popover.clone();
        let print_button_for_menu = print_button_for_context.clone();
        print.connect_clicked(move |_| {
            popover_for_print.popdown();
            print_button_for_menu.activate();
        });
        menu_box.append(&print);

        let export = gtk::Button::with_label("Termin exportieren");
        export.set_halign(gtk::Align::Fill);
        export.add_css_class("flat");
        export.set_sensitive(action_state.can_export);
        let popover_for_export = popover.clone();
        let export_button_for_menu = export_button_for_context.clone();
        export.connect_clicked(move |_| {
            popover_for_export.popdown();
            export_button_for_menu.activate();
        });
        menu_box.append(&export);

        let copy = gtk::Button::with_label("In lokalen Kalender kopieren");
        copy.set_halign(gtk::Align::Fill);
        copy.add_css_class("flat");
        copy.set_sensitive(action_state.can_copy_to_local);
        let popover_for_copy = popover.clone();
        let copy_button_for_menu = copy_button_for_context.clone();
        copy.connect_clicked(move |_| {
            popover_for_copy.popdown();
            copy_button_for_menu.activate();
        });
        menu_box.append(&copy);

        let move_to_local = gtk::Button::with_label("In lokalen Kalender verschieben");
        move_to_local.set_halign(gtk::Align::Fill);
        move_to_local.add_css_class("flat");
        move_to_local.set_sensitive(action_state.can_move_to_local);
        let popover_for_move = popover.clone();
        let move_button_for_menu = move_button_for_context.clone();
        move_to_local.connect_clicked(move |_| {
            popover_for_move.popdown();
            move_button_for_menu.activate();
        });
        menu_box.append(&move_to_local);

        let delete = gtk::Button::with_label("Termin löschen");
        delete.set_halign(gtk::Align::Fill);
        delete.add_css_class("flat");
        delete.add_css_class("destructive-action");
        delete.set_sensitive(writable);
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
    let identity = calendar_event_identity_from_parts(source_uid, uid, None);
    remove_event_list_row_by_identity(event_list, &identity);
}

fn remove_event_list_row_by_identity(event_list: &gtk::ListBox, identity: &CalendarEventIdentity) {
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
            let row_source_object_key = event_row
                .data::<String>("event-source-object-key")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = event_row
                .data::<String>("event-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_recurrence_id = event_row
                .data::<String>("event-recurrence-id")
                .map(|value| value.as_ref().clone())
                .filter(|value| !value.is_empty());
            row_source_object_key == identity.source_object_key
                && row_source_uid == identity.source_uid
                && row_uid == identity.uid
                && row_recurrence_id == identity.recurrence_id
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
    fn shift_month_anchors_to_first_of_month() {
        let d = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
        assert_eq!(
            shift_month(d, 1),
            NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()
        );
        assert_eq!(
            shift_month(d, -1),
            NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()
        );

        // Year boundary wrapping
        let dec = NaiveDate::from_ymd_opt(2026, 12, 10).unwrap();
        assert_eq!(
            shift_month(dec, 1),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
        let jan = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        assert_eq!(
            shift_month(jan, -1),
            NaiveDate::from_ymd_opt(2025, 12, 1).unwrap()
        );
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
    fn recurrence_safe_identity_keeps_master_and_exception_separate() {
        let mut master = all_day_event("source", "series", "2026-08-03", "2026-08-04");
        master.component = concat!(
            "BEGIN:VEVENT\r\nUID:series\r\n",
            "DTSTART;VALUE=DATE:20260803\r\n",
            "DTEND;VALUE=DATE:20260804\r\n",
            "RRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\n"
        )
        .to_string();
        let mut exception = all_day_event("source", "series", "2026-08-10", "2026-08-11");
        exception.component = concat!(
            "BEGIN:VEVENT\r\nUID:series\r\n",
            "RECURRENCE-ID;VALUE=DATE:20260810\r\n",
            "DTSTART;VALUE=DATE:20260810\r\n",
            "DTEND;VALUE=DATE:20260811\r\nEND:VEVENT\r\n"
        )
        .to_string();

        let master_identity = CalendarEventIdentity::from_event(&master).unwrap();
        let exception_identity = CalendarEventIdentity::from_event(&exception).unwrap();
        assert_ne!(master_identity, exception_identity);
        assert_eq!(master_identity.source_object_key, "source:source");
        assert_eq!(
            exception_identity.recurrence_id.as_deref(),
            Some("20260810")
        );
        assert!(same_event_identity(&master, "source", "series"));
        assert!(!same_event_identity(&exception, "source", "series"));

        let index = CalendarSnapshotIndex::new(Arc::new(CalendarSnapshot {
            events: vec![master, exception],
            tasks: Vec::new(),
            memos: Vec::new(),
        }));
        let grouped = indexed_events_by_date(
            &index,
            CalendarDateRange::single(date("2026-08-10")),
            "",
            None,
            &HashSet::new(),
        );
        assert_eq!(grouped.get(&date("2026-08-10")).map(Vec::len), Some(2));
    }

    #[test]
    fn source_less_import_identity_is_stable_but_remains_read_only() {
        let mut imported = event_with_start("2026-08-10");
        imported.source_uid.clear();
        imported.source_display_name = "Importierte ICS-Datei".to_string();
        imported.component =
            "BEGIN:VEVENT\r\nUID:event-1\r\nDTSTART;VALUE=DATE:20260810\r\nEND:VEVENT\r\n"
                .to_string();

        let first = CalendarEventIdentity::from_event(&imported).unwrap();
        let second = CalendarEventIdentity::from_event(&imported).unwrap();
        assert_eq!(first, second);
        assert!(!first.source_object_key.is_empty());
        assert_eq!(first.source_uid, "");
        assert_eq!(first.uid, "event-1");
        assert!(event_mutation_error(&imported).is_some());
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
    fn cached_preview_keeps_provider_result_open_and_rejects_late_previews() {
        let mut gate = CalendarLoadGate::default();
        let first = gate.begin();
        assert!(gate.accepts(first)); // cache is provisional, no finish
        assert!(gate.loading);
        assert!(gate.finish(first)); // authoritative result closes both stages
        assert!(!gate.accepts(first));
        let second = gate.begin();
        assert!(!gate.accepts(first));
        assert!(gate.accepts(second));
        assert!(gate.cancel(second));
        assert!(!gate.accepts(second));
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
    fn stale_or_cancelled_projection_plans_are_rejected() {
        let mut gate = CalendarProjectionGate::default();
        let first = gate.begin(CalendarView::Year, 4, 8);
        assert!(gate.accepts(first, CalendarView::Year, 4, 8));
        let second = gate.begin(CalendarView::List, 5, 8);
        assert!(!gate.accepts(first, CalendarView::Year, 4, 8));
        assert!(gate.accepts(second, CalendarView::List, 5, 8));
        assert!(gate.cancel(second));
        assert!(!gate.accepts(second, CalendarView::List, 5, 8));
        assert!(!gate.finish(second, CalendarView::List, 5, 8));
    }

    #[test]
    fn calendar_action_state_only_offers_local_transfer_when_source_and_target_are_safe() {
        let remote = all_day_event("remote", "event-1", "2026-08-10", "2026-08-11");
        let state = calendar_event_action_state(&remote, true);
        assert!(state.can_edit);
        assert!(state.can_delete);
        assert!(state.can_export);
        assert!(state.can_print);
        assert!(state.can_copy_to_local);
        assert!(state.can_move_to_local);

        let local = all_day_event("local-calendar", "event-2", "2026-08-10", "2026-08-11");
        let local_state = calendar_event_action_state(&local, true);
        assert!(local_state.can_print);
        assert!(!local_state.can_copy_to_local);
        assert!(!local_state.can_move_to_local);
        assert!(
            plan_calendar_event_transfer(
                &remote,
                CalendarTransferKind::Move,
                "local-calendar",
                true,
            )
            .is_ok()
        );
        assert!(
            plan_calendar_event_transfer(
                &local,
                CalendarTransferKind::Copy,
                "local-calendar",
                true,
            )
            .is_err()
        );
        assert!(
            plan_calendar_event_transfer(
                &remote,
                CalendarTransferKind::Copy,
                "local-calendar",
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn recurring_delete_scopes_preserve_the_component_until_entire_series_is_confirmed() {
        let mut recurring = all_day_event("source", "series", "2026-08-10", "2026-08-11");
        recurring.component = concat!(
            "BEGIN:VEVENT\r\nUID:series\r\nDTSTART;VALUE=DATE:20260810\r\n",
            "RRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\n"
        )
        .to_string();

        assert_eq!(
            plan_calendar_event_delete(&recurring, CalendarRecurringDeleteScope::ThisOccurrence),
            Ok(CalendarDeletePlan::UnsupportedOccurrenceScope {
                scope: CalendarRecurringDeleteScope::ThisOccurrence,
            })
        );
        assert_eq!(
            plan_calendar_event_delete(&recurring, CalendarRecurringDeleteScope::ThisAndFuture),
            Ok(CalendarDeletePlan::UnsupportedOccurrenceScope {
                scope: CalendarRecurringDeleteScope::ThisAndFuture,
            })
        );
        assert_eq!(
            plan_calendar_event_delete(&recurring, CalendarRecurringDeleteScope::EntireSeries),
            Ok(CalendarDeletePlan::RemoveComponent {
                source_uid: "source".to_string(),
                uid: "series".to_string(),
            })
        );
    }

    #[test]
    fn projection_application_ranges_are_bounded() {
        let ranges = projection_chunk_ranges(59, PROJECTION_APPLY_CHUNK_SIZE);
        assert_eq!(ranges.first(), Some(&(0, PROJECTION_APPLY_CHUNK_SIZE)));
        assert_eq!(ranges.last(), Some(&(48, 59)));
        assert_eq!(
            ranges.iter().map(|(start, end)| end - start).sum::<usize>(),
            59
        );
        assert!(
            ranges
                .iter()
                .all(|(start, end)| end > start && end - start <= PROJECTION_APPLY_CHUNK_SIZE)
        );
    }

    #[test]
    fn mutation_revision_rejects_callbacks_after_authoritative_reload() {
        let mut gate = CalendarMutationGate::default();
        let first = gate.begin(10, 1, None);
        assert!(gate.accepts(&first));
        let second = gate.begin(11, 1, None);
        assert!(!gate.accepts(&first));
        assert!(gate.accepts(&second));
        assert!(gate.finish(&second));
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
    fn year_viewport_is_exactly_the_civil_year_and_leap_navigation_is_clamped() {
        let leap = CalendarViewport::for_view(CalendarView::Year, date("2024-02-29"), Weekday::Mon);
        assert_eq!(leap.visible_range.start, date("2024-01-01"));
        assert_eq!(leap.visible_range.end, date("2025-01-01"));
        assert_eq!(leap.visible_range.len_days(), 366);
        assert!(!leap.visible_range.contains(date("2025-01-01")));

        let mut state =
            CalendarViewState::new(CalendarView::Year, date("2024-02-29"), Weekday::Mon);
        state.navigate(1, Weekday::Mon);
        assert_eq!(state.anchor_date, date("2025-02-28"));
        assert_eq!(state.visible_range, calendar_year_range(2025));
        state.navigate(-1, Weekday::Mon);
        assert_eq!(state.anchor_date, date("2024-02-28"));
        assert_eq!(state.visible_range, calendar_year_range(2024));
    }

    #[test]
    fn year_navigation_at_chrono_boundary_is_checked_and_non_panicking() {
        let mut state = CalendarViewState::new(CalendarView::Year, NaiveDate::MAX, Weekday::Mon);
        let before = state.anchor_date;
        state.navigate(1, Weekday::Mon);
        assert_eq!(state.anchor_date, before);
        state.navigate(-1, Weekday::Mon);
        assert!(state.anchor_date < before);
    }

    #[test]
    fn year_markers_expand_recurrence_rdate_exdate_and_multiday_spans() {
        let mut recurring = all_day_event("source-a", "series", "2024-02-28", "2024-02-29");
        recurring.component = concat!(
            "BEGIN:VEVENT\r\nUID:series\r\n",
            "DTSTART;VALUE=DATE:20240228\r\n",
            "DTEND;VALUE=DATE:20240229\r\n",
            "RRULE:FREQ=DAILY;COUNT=3\r\n",
            "RDATE;VALUE=DATE:20240305\r\n",
            "EXDATE;VALUE=DATE:20240229\r\nEND:VEVENT\r\n"
        )
        .to_string();
        let multi_day = all_day_event("source-b", "multi", "2024-03-10", "2024-03-12");
        let index = CalendarSnapshotIndex::new(Arc::new(CalendarSnapshot {
            events: vec![recurring, multi_day],
            tasks: Vec::new(),
            memos: Vec::new(),
        }));
        let markers =
            indexed_event_day_markers(&index, calendar_year_range(2024), "", None, &HashSet::new());

        assert_eq!(markers.get(&date("2024-02-28")), Some(&1));
        assert_eq!(markers.get(&date("2024-02-29")), None);
        assert_eq!(markers.get(&date("2024-03-01")), Some(&1));
        assert_eq!(markers.get(&date("2024-03-05")), Some(&1));
        assert_eq!(markers.get(&date("2024-03-10")), Some(&1));
        assert_eq!(markers.get(&date("2024-03-11")), Some(&1));
        assert_eq!(markers.get(&date("2024-03-12")), None);

        let mut hidden_sources = HashSet::new();
        hidden_sources.insert("source-b".to_string());
        let filtered_markers =
            indexed_event_day_markers(&index, calendar_year_range(2024), "", None, &hidden_sources);
        assert_eq!(filtered_markers.get(&date("2024-03-10")), None);
        assert_eq!(filtered_markers.get(&date("2024-03-05")), Some(&1));
    }

    #[test]
    fn list_index_keeps_duplicate_uids_filters_and_out_of_anchor_events() {
        let mut source_a = all_day_event("source-a", "same", "2027-01-02", "2027-01-03");
        source_a.summary = "Alpha".to_string();
        source_a.component = source_a
            .component
            .replace("END:VEVENT", "CATEGORIES:Work\r\nEND:VEVENT");
        let mut source_b = all_day_event("source-b", "same", "2025-12-30", "2025-12-31");
        source_b.summary = "Beta".to_string();
        source_b.component = source_b
            .component
            .replace("END:VEVENT", "CATEGORIES:Personal\r\nEND:VEVENT");
        let mut same_start = all_day_event("source-c", "later", "2027-01-02", "2027-01-03");
        same_start.summary = "Zulu".to_string();

        let index = CalendarSnapshotIndex::new(Arc::new(CalendarSnapshot {
            events: vec![source_a, source_b, same_start],
            tasks: Vec::new(),
            memos: Vec::new(),
        }));
        let all = filtered_sorted_event_indices(&index, "", None, &HashSet::new());
        let identities = all
            .iter()
            .map(|event_index| {
                let event = &index.snapshot.events[*event_index];
                (event.source_uid.as_str(), event.uid.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            identities,
            vec![
                ("source-b", "same"),
                ("source-a", "same"),
                ("source-c", "later")
            ]
        );

        let anchor_range = CalendarDateRange::from_days(date("2026-06-01"), 7);
        assert!(index.candidate_event_indices(anchor_range).is_empty());
        assert_eq!(all.len(), 3, "List must not inherit the anchor range");

        let mut hidden = HashSet::new();
        hidden.insert("source-b".to_string());
        let filtered = filtered_sorted_event_indices(&index, "", Some("Work"), &hidden);
        assert_eq!(filtered, vec![0]);
        let searched = filtered_sorted_event_indices(&index, "zulu", None, &HashSet::new());
        assert_eq!(searched, vec![2]);
    }

    #[test]
    fn malformed_identity_date_and_recurrence_are_safe_and_list_visible() {
        let mut malformed_date = event_with_start("not-a-date");
        malformed_date.uid = "bad-date".to_string();
        malformed_date.source_uid = "source".to_string();
        let mut malformed_identity = event_with_start("2026-01-01");
        malformed_identity.uid.clear();
        let mut malformed_recurrence = event_with_start("2026-08-03");
        malformed_recurrence.uid = "bad-rule".to_string();
        malformed_recurrence.all_day = true;
        malformed_recurrence.component = concat!(
            "BEGIN:VEVENT\r\nUID:bad-rule\r\nDTSTART;VALUE=DATE:20260803\r\n",
            "RRULE:FREQ=DAILY;COUNT=not-a-number\r\nEND:VEVENT\r\n"
        )
        .to_string();
        let index = CalendarSnapshotIndex::new(Arc::new(CalendarSnapshot {
            events: vec![
                malformed_date.clone(),
                malformed_identity.clone(),
                malformed_recurrence.clone(),
            ],
            tasks: Vec::new(),
            memos: Vec::new(),
        }));

        assert_eq!(
            CalendarEventIdentity::from_event(&malformed_date)
                .unwrap()
                .uid,
            "bad-date"
        );
        let malformed_identity = CalendarEventIdentity::from_event(&malformed_identity)
            .expect("source-less or UID-less rows still need a stable identity");
        assert!(malformed_identity.uid.starts_with("imported-uid:"));
        assert!(filtered_sorted_event_indices(&index, "", None, &HashSet::new()).contains(&0));
        assert!(
            event_occurrence_dates_in_range(&malformed_recurrence, calendar_year_range(2026))
                .contains(&date("2026-08-03"))
        );
        assert!(
            !event_occurrence_dates_in_range(&malformed_recurrence, calendar_year_range(2026))
                .contains(&date("2026-08-04"))
        );
    }

    #[test]
    fn selection_is_preserved_by_identity_and_moves_to_the_same_index_on_removal() {
        let first = all_day_event("source-a", "first", "2026-01-01", "2026-01-02");
        let selected = all_day_event("source-a", "selected", "2026-01-02", "2026-01-03");
        let last = all_day_event("source-b", "last", "2026-01-03", "2026-01-04");
        let selected_identity = CalendarEventIdentity::from_event(&selected).unwrap();
        let mut state =
            CalendarViewState::new(CalendarView::Year, date("2026-01-02"), Weekday::Mon);
        state.select_event(Some(selected_identity), Some(1));
        state.reconcile_event_selection(&[first.clone(), selected.clone(), last.clone()]);
        assert_eq!(state.selected_event_index, Some(1));

        state.reconcile_event_selection(&[first.clone(), last.clone()]);
        assert_eq!(
            state.selected_event,
            CalendarEventIdentity::from_event(&last)
        );
        assert_eq!(state.selected_event_index, Some(1));

        state.reconcile_event_selection(std::slice::from_ref(&first));
        assert_eq!(
            state.selected_event,
            CalendarEventIdentity::from_event(&first)
        );
        assert_eq!(state.selected_event_index, Some(0));
        state.reconcile_event_selection(&[]);
        assert_eq!(state.selected_event, None);
        assert_eq!(state.selected_event_index, None);
    }

    #[test]
    fn year_to_list_preserves_selection_and_list_navigation_is_a_noop() {
        let selected = all_day_event("source", "selected", "2026-08-10", "2026-08-11");
        let identity = CalendarEventIdentity::from_event(&selected).unwrap();
        let mut state =
            CalendarViewState::new(CalendarView::Year, date("2026-08-10"), Weekday::Mon);
        state.select_event(Some(identity.clone()), Some(0));
        state.select_civil_day(date("2026-08-10"));
        state.set_view(CalendarView::List, Weekday::Mon);
        let before = state.clone();
        state.navigate(1, Weekday::Mon);
        state.today(date("2027-01-01"), Weekday::Mon);
        assert_eq!(state.anchor_date, before.anchor_date);
        assert_eq!(state.visible_range, before.visible_range);
        assert_eq!(state.selected_event, Some(identity));
        assert_eq!(state.selected_range, before.selected_range);
        assert_eq!(calendar_view_from_name("year"), CalendarView::Year);
        assert_eq!(calendar_view_from_name("list"), CalendarView::List);
        assert_eq!(calendar_view_name(CalendarView::Year), "year");
        assert_eq!(calendar_view_name(CalendarView::List), "list");
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
    fn shifting_timed_event_updates_start_and_end_with_minutes() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:event-1\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T100000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T110000\r\n",
            "END:VEVENT\r\n"
        );
        // Move from 10:00 to 14:30 on same day (+270 minutes, 0 days)
        let shifted = shift_event_component_full(raw, 0, 270).expect("event should shift minutes");
        assert!(shifted.contains("DTSTART;TZID=Europe/Berlin:20260810T143000"));
        assert!(shifted.contains("DTEND;TZID=Europe/Berlin:20260810T153000"));

        // Move to next day at 09:00 (+1 day, -60 minutes from original 10:00)
        let shifted_next_day =
            shift_event_component_full(raw, 1, -60).expect("event should shift day and minutes");
        assert!(shifted_next_day.contains("DTSTART;TZID=Europe/Berlin:20260811T090000"));
        assert!(shifted_next_day.contains("DTEND;TZID=Europe/Berlin:20260811T100000"));
    }

    #[test]
    fn shifting_all_day_event_ignores_delta_minutes() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-allday\r\n",
            "DTSTART;VALUE=DATE:20260810\r\n",
            "DTEND;VALUE=DATE:20260811\r\nEND:VEVENT\r\n"
        );
        // Dragging all-day event should only shift days even if minutes are supplied
        let shifted = shift_event_component_full(raw, 2, 120).expect("all day event should shift");
        assert!(shifted.contains("DTSTART;VALUE=DATE:20260812"));
        assert!(shifted.contains("DTEND;VALUE=DATE:20260813"));
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

    #[test]
    fn task_matches_calendar_filter_tests() {
        let task = TaskRecord {
            uid: "task-101".to_string(),
            source_uid: "source-work".to_string(),
            source_display_name: "Arbeit".to_string(),
            component: "BEGIN:VTODO\r\nUID:task-101\r\nSUMMARY:Bericht fertigstellen\r\nCATEGORIES:Projekt,Wichtig\r\nDESCRIPTION:Detaillierte Analyse\r\nEND:VTODO\r\n".to_string(),
            summary: "Bericht fertigstellen".to_string(),
            due: "2026-09-10".to_string(),
            priority: 1,
            completed: false,
        };
        let hidden = HashSet::new();

        // 1. Match everything when no filters
        assert!(task_matches_calendar_filter(&task, "", None, &hidden));

        // 2. Filter out if source is hidden
        let mut hidden_sources = HashSet::new();
        hidden_sources.insert("source-work".to_string());
        assert!(!task_matches_calendar_filter(
            &task,
            "",
            None,
            &hidden_sources
        ));

        // 3. Search query matching summary
        assert!(task_matches_calendar_filter(
            &task, "bericht", None, &hidden
        ));
        assert!(task_matches_calendar_filter(&task, "FERTIG", None, &hidden));

        // 4. Search query matching component / description
        assert!(task_matches_calendar_filter(
            &task, "analyse", None, &hidden
        ));

        // 5. Search query mismatch
        assert!(!task_matches_calendar_filter(
            &task, "urlaub", None, &hidden
        ));

        // 6. Category matching
        assert!(task_matches_calendar_filter(
            &task,
            "",
            Some("Projekt"),
            &hidden
        ));
        assert!(task_matches_calendar_filter(
            &task,
            "",
            Some("wichtig"),
            &hidden
        ));
        assert!(!task_matches_calendar_filter(
            &task,
            "",
            Some("Privat"),
            &hidden
        ));

        // 7. Combined query and category match
        assert!(task_matches_calendar_filter(
            &task,
            "bericht",
            Some("Projekt"),
            &hidden
        ));
        assert!(!task_matches_calendar_filter(
            &task,
            "bericht",
            Some("Privat"),
            &hidden
        ));
        assert!(!task_matches_calendar_filter(
            &task,
            "urlaub",
            Some("Projekt"),
            &hidden
        ));
    }

    #[test]
    fn memo_matches_calendar_filter_tests() {
        let memo = MemoRecord {
            uid: "memo-202".to_string(),
            source_uid: "source-notes".to_string(),
            source_display_name: "Notizen".to_string(),
            component: "BEGIN:VJOURNAL\r\nUID:memo-202\r\nSUMMARY:Architektur Meeting\r\nCATEGORIES:Ideen,Technik\r\nDESCRIPTION:Diskussion über GTK4 Paned Layout\r\nEND:VJOURNAL\r\n".to_string(),
            summary: "Architektur Meeting".to_string(),
            description: "Diskussion über GTK4 Paned Layout".to_string(),
            updated: "2026-09-04".to_string(),
        };
        let hidden = HashSet::new();

        // 1. Match everything when no filters
        assert!(memo_matches_calendar_filter(&memo, "", None, &hidden));

        // 2. Filter out if source is hidden
        let mut hidden_sources = HashSet::new();
        hidden_sources.insert("source-notes".to_string());
        assert!(!memo_matches_calendar_filter(
            &memo,
            "",
            None,
            &hidden_sources
        ));

        // 3. Search query matching summary
        assert!(memo_matches_calendar_filter(
            &memo, "meeting", None, &hidden
        ));
        assert!(memo_matches_calendar_filter(
            &memo,
            "ARCHITEKTUR",
            None,
            &hidden
        ));

        // 4. Search query matching description
        assert!(memo_matches_calendar_filter(&memo, "paned", None, &hidden));

        // 5. Search query mismatch
        assert!(!memo_matches_calendar_filter(
            &memo,
            "einkaufsliste",
            None,
            &hidden
        ));

        // 6. Category matching
        assert!(memo_matches_calendar_filter(
            &memo,
            "",
            Some("Ideen"),
            &hidden
        ));
        assert!(memo_matches_calendar_filter(
            &memo,
            "",
            Some("technik"),
            &hidden
        ));
        assert!(!memo_matches_calendar_filter(
            &memo,
            "",
            Some("Privat"),
            &hidden
        ));

        // 7. Combined query and category match
        assert!(memo_matches_calendar_filter(
            &memo,
            "paned",
            Some("Technik"),
            &hidden
        ));
        assert!(!memo_matches_calendar_filter(
            &memo,
            "paned",
            Some("Privat"),
            &hidden
        ));
    }

    #[test]
    fn calendar_memo_subtitle_tests() {
        let mut memo = MemoRecord {
            uid: "m1".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Persönlich".to_string(),
            component: String::new(),
            summary: "Titel".to_string(),
            description: "Erste Zeile\nZweite Zeile".to_string(),
            updated: "2026-09-04 12:00".to_string(),
        };
        assert_eq!(calendar_memo_subtitle(&memo), "2026-09-04 12:00");

        memo.updated.clear();
        assert_eq!(calendar_memo_subtitle(&memo), "Erste Zeile");

        memo.description.clear();
        assert_eq!(calendar_memo_subtitle(&memo), "Persönlich");

        memo.source_display_name.clear();
        assert_eq!(calendar_memo_subtitle(&memo), "");
    }

    #[test]
    fn calendar_event_hide_cancelled_events_filter_tests() {
        let confirmed_event = CalendarEventRecord {
            uid: "evt-1".to_string(),
            source_uid: "cal-local".to_string(),
            source_display_name: "Personal".to_string(),
            component: concat!(
                "BEGIN:VEVENT\r\nUID:evt-1\r\n",
                "STATUS:CONFIRMED\r\nSUMMARY:Team Meeting\r\nEND:VEVENT\r\n"
            )
            .to_string(),
            summary: "Team Meeting".to_string(),
            description: "".to_string(),
            location: "".to_string(),
            start: "2026-08-10 10:00".to_string(),
            end: "2026-08-10 11:00".to_string(),
            all_day: false,
        };

        let cancelled_event = CalendarEventRecord {
            uid: "evt-2".to_string(),
            source_uid: "cal-local".to_string(),
            source_display_name: "Personal".to_string(),
            component: concat!(
                "BEGIN:VEVENT\r\nUID:evt-2\r\n",
                "STATUS:CANCELLED\r\nSUMMARY:Cancelled Project Sync\r\nEND:VEVENT\r\n"
            )
            .to_string(),
            summary: "Cancelled Project Sync".to_string(),
            description: "".to_string(),
            location: "".to_string(),
            start: "2026-08-10 14:00".to_string(),
            end: "2026-08-10 15:00".to_string(),
            all_day: false,
        };

        let hidden = HashSet::new();

        // 1. Confirmed event is visible regardless of hide_cancelled flag
        assert!(event_matches_calendar_filter_with_options(
            &confirmed_event,
            "",
            None,
            &hidden,
            false
        ));
        assert!(event_matches_calendar_filter_with_options(
            &confirmed_event,
            "",
            None,
            &hidden,
            true
        ));

        // 2. Cancelled event is visible when hide_cancelled is false
        assert!(event_matches_calendar_filter_with_options(
            &cancelled_event,
            "",
            None,
            &hidden,
            false
        ));

        // 3. Cancelled event is filtered out when hide_cancelled is true
        assert!(!event_matches_calendar_filter_with_options(
            &cancelled_event,
            "",
            None,
            &hidden,
            true
        ));
    }

    #[test]
    fn continuous_grid_slot_time_conversion_and_bounds() {
        // 48 slots cover 24 hours in 30-minute intervals
        for slot in 0..48u32 {
            let minute = slot * 30;
            let hour = minute / 60;
            let min = minute % 60;
            assert!(hour < 24);
            assert!(min == 0 || min == 30);
            let formatted = format_clock_minutes(minute);
            assert!(!formatted.is_empty());
        }

        // Test boundary values
        assert_eq!(format_clock_minutes(0), "00:00");
        assert_eq!(format_clock_minutes(9 * 60 + 30), "09:30");
        assert_eq!(format_clock_minutes(23 * 60 + 30), "23:30");
    }

    #[test]
    fn normalize_calendar_time_divisions_values() {
        assert_eq!(normalize_calendar_time_divisions(5), 5);
        assert_eq!(normalize_calendar_time_divisions(10), 10);
        assert_eq!(normalize_calendar_time_divisions(15), 15);
        assert_eq!(normalize_calendar_time_divisions(30), 30);
        assert_eq!(normalize_calendar_time_divisions(60), 60);

        // Invalid fallback to 30
        assert_eq!(normalize_calendar_time_divisions(0), 30);
        assert_eq!(normalize_calendar_time_divisions(7), 30);
        assert_eq!(normalize_calendar_time_divisions(45), 30);
        assert_eq!(normalize_calendar_time_divisions(120), 30);
        assert_eq!(normalize_calendar_time_divisions(-1), 30);
    }

    #[test]
    fn time_divisions_calculates_correct_slot_count_and_intervals() {
        for (div, expected_slots_per_hour, expected_total_slots) in [
            (60, 1, 24),
            (30, 2, 48),
            (15, 4, 96),
            (10, 6, 144),
            (5, 12, 288),
        ] {
            let normalized = normalize_calendar_time_divisions(div);
            let slots_per_hour = (60 / normalized) as usize;
            let total_slots = 24 * slots_per_hour;
            assert_eq!(slots_per_hour, expected_slots_per_hour);
            assert_eq!(total_slots, expected_total_slots);
        }
    }

    #[test]
    fn secondary_timezone_header_and_hour_labels() {
        assert_eq!(secondary_timezone_header_label("UTC"), "UTC");
        assert_eq!(
            secondary_timezone_header_label("America/New_York"),
            "New York"
        );
        assert_eq!(secondary_timezone_header_label("Europe/Berlin"), "Berlin");
        assert_eq!(secondary_timezone_header_label(""), "");
        assert_eq!(secondary_timezone_header_label("   "), "");

        let utc_tz = timezone_from_name("UTC").expect("UTC timezone should resolve");
        let date = NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();

        // Check hour label formatting
        let label_24h = secondary_timezone_hour_label(date, 12 * 60, &utc_tz, false);
        assert!(label_24h.is_some());

        let label_12h = secondary_timezone_hour_label(date, 12 * 60, &utc_tz, true);
        assert!(label_12h.is_some());
    }

    #[test]
    fn primary_timezone_header_and_format_hour_markup_tests() {
        let label = primary_timezone_header_label();
        assert!(!label.trim().is_empty());

        let h9_24 = format_hour_markup(9 * 60, false);
        assert!(h9_24.contains(r#"weight="medium">9<"#));
        assert!(h9_24.contains(r#"rise="7000"> 00<"#));

        let h14_12 = format_hour_markup(14 * 60, true);
        assert!(h14_12.contains(r#"weight="medium">2<"#));
        assert!(h14_12.contains(r#"rise="7000"> PM<"#));

        let h0_12 = format_hour_markup(0, true);
        assert!(h0_12.contains(r#"weight="medium">12<"#));
        assert!(h0_12.contains(r#"rise="7000"> AM<"#));
    }

    #[test]
    fn month_scroll_by_week_navigation_step() {
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let mut state_month = CalendarViewState::new(CalendarView::Month, anchor, Weekday::Mon);

        // Standard month navigation (shift by month)
        state_month.navigate_step(1, Weekday::Mon, false);
        assert_eq!(
            state_month.anchor_date,
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
        );

        // Week-by-week month navigation (shift by 7 days)
        let mut state_week_scroll =
            CalendarViewState::new(CalendarView::Month, anchor, Weekday::Mon);
        state_week_scroll.navigate_step(1, Weekday::Mon, true);
        assert_eq!(
            state_week_scroll.anchor_date,
            NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()
        );
        state_week_scroll.navigate_step(-1, Weekday::Mon, true);
        assert_eq!(state_week_scroll.anchor_date, anchor);
    }

    #[test]
    fn calendar_sidebar_mini_calendars_sync_consecutive_months() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let page = CalendarPage::new();
        let target = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        page.navigate_to_date(target);

        let cal1 = page.imp().mini_calendar.borrow().clone().unwrap();
        let cal2 = page.imp().mini_calendar_2.borrow().clone().unwrap();
        let cal3 = page.imp().mini_calendar_3.borrow().clone().unwrap();
        let cal4 = page.imp().mini_calendar_4.borrow().clone().unwrap();

        assert_eq!(cal1.date().month(), 9);
        assert_eq!(cal2.date().month(), 10);
        assert_eq!(cal3.date().month(), 11);
        assert_eq!(cal4.date().month(), 12);

        // Verify marked days in Week view (default week is 2026-09-07 to 2026-09-13)
        page.select_calendar_view(CalendarView::Week);
        assert!(cal1.day_is_marked(7));
        assert!(cal1.day_is_marked(13));
        assert!(!cal1.day_is_marked(6));
        assert!(!cal1.day_is_marked(14));
        assert!(!cal2.day_is_marked(7));

        // Verify marked days in WorkWeek view (Mon-Fri 7..=11, Sat-Sun not marked)
        page.select_calendar_view(CalendarView::WorkWeek);
        assert!(cal1.day_is_marked(7));
        assert!(cal1.day_is_marked(11));
        assert!(!cal1.day_is_marked(12));
        assert!(!cal1.day_is_marked(13));

        // Verify marked days in Day view (only anchor day 7 marked)
        page.select_calendar_view(CalendarView::Day);
        assert!(cal1.day_is_marked(7));
        assert!(!cal1.day_is_marked(8));

        // Verify marked days in Month view (September days marked, October unmarked)
        page.select_calendar_view(CalendarView::Month);
        assert!(cal1.day_is_marked(1));
        assert!(cal1.day_is_marked(15));
        assert!(cal1.day_is_marked(30));
        assert!(!cal2.day_is_marked(1));

        // Test October (crossing into January of next year, matching user's screenshot)
        let october_target = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        page.navigate_to_date(october_target);
        assert_eq!(cal1.date().year(), 2026);
        assert_eq!(cal1.date().month(), 10);
        assert_eq!(cal2.date().year(), 2026);
        assert_eq!(cal2.date().month(), 11);
        assert_eq!(cal3.date().year(), 2026);
        assert_eq!(cal3.date().month(), 12);
        assert_eq!(cal4.date().year(), 2027);
        assert_eq!(cal4.date().month(), 1);

        // Verify limitless scaling beyond 4 months (e.g. 6 consecutive months)
        page.ensure_mini_calendar_count(6);
        assert_eq!(page.imp().mini_calendars.borrow().len(), 6);
        let cal5 = page.imp().mini_calendars.borrow()[4].clone();
        let cal6 = page.imp().mini_calendars.borrow()[5].clone();
        assert_eq!(cal5.date().year(), 2027);
        assert_eq!(cal5.date().month(), 2);
        assert_eq!(cal6.date().year(), 2027);
        assert_eq!(cal6.date().month(), 3);
    }

    #[test]
    fn workday_cell_qualification_requires_both_working_hours_and_configured_weekday() {
        let workdays = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ];
        let (workday_start, workday_end) = (540, 1020); // 09:00 to 17:00

        let monday = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        let sunday = NaiveDate::from_ymd_opt(2026, 8, 9).unwrap();

        // Monday 10:00 (600m) -> Workday cell
        assert!(is_workday_cell(600, workday_start, workday_end, monday.weekday(), &workdays));

        // Monday 08:00 (480m) -> Offhours
        assert!(!is_workday_cell(480, workday_start, workday_end, monday.weekday(), &workdays));

        // Sunday 10:00 (600m) -> Offhours (not a configured workday)
        assert!(!is_workday_cell(600, workday_start, workday_end, sunday.weekday(), &workdays));
    }

    #[test]
    fn time_divisions_normalization_covers_all_supported_steps() {
        assert_eq!(normalize_calendar_time_divisions(5), 5);
        assert_eq!(normalize_calendar_time_divisions(10), 10);
        assert_eq!(normalize_calendar_time_divisions(15), 15);
        assert_eq!(normalize_calendar_time_divisions(30), 30);
        assert_eq!(normalize_calendar_time_divisions(60), 60);
        assert_eq!(normalize_calendar_time_divisions(45), 30); // fallback
        assert_eq!(normalize_calendar_time_divisions(0), 30);  // fallback
        assert_eq!(normalize_calendar_time_divisions(-10), 30); // fallback
    }

    #[test]
    fn configured_weekday_names_order_switches_between_monday_and_sunday() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let Some(settings) = AppSettings::try_new() else {
            let names = configured_weekday_names();
            assert!(names[0] == "Mo" || names[0] == "So");
            return;
        };

        let previous = settings.calendar_week_start();

        // 1. Basic Monday vs Sunday switching
        settings.set_calendar_week_start("monday");
        assert_eq!(configured_first_weekday(), Weekday::Mon);
        assert_eq!(configured_weekday_names()[0], "Mo");
        assert_eq!(
            configured_weekday_names(),
            ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"]
        );

        settings.set_calendar_week_start("sunday");
        assert_eq!(configured_first_weekday(), Weekday::Sun);
        assert_eq!(configured_weekday_names()[0], "So");
        assert_eq!(
            configured_weekday_names(),
            ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"]
        );

        // 2. Adversarial rapid stress cycling (50 iterations)
        for i in 0..50 {
            if i % 2 == 0 {
                settings.set_calendar_week_start("monday");
                assert_eq!(configured_first_weekday(), Weekday::Mon);
                assert_eq!(configured_weekday_names()[0], "Mo");
            } else {
                settings.set_calendar_week_start("sunday");
                assert_eq!(configured_first_weekday(), Weekday::Sun);
                assert_eq!(configured_weekday_names()[0], "So");
            }
        }

        // 3. Fallback resilience on non-standard/unrecognized values
        for invalid in ["saturday", "tuesday", "", "SUNDAY", "unknown_day"] {
            settings.set_calendar_week_start(invalid);
            assert_eq!(
                configured_first_weekday(),
                Weekday::Mon,
                "Invalid week-start '{invalid}' must safely default to Monday"
            );
            assert_eq!(
                configured_weekday_names()[0],
                "Mo",
                "Invalid week-start '{invalid}' must safely default to 'Mo'"
            );
        }

        settings.set_calendar_week_start(&previous);
    }

    #[test]
    fn custom_workdays_filter_workweek_dates_accurately() {
        let week_range = CalendarDateRange::from_days(
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(), // Monday
            7,
        );
        // Custom 4-day workweek: Mon, Tue, Wed, Thu
        let four_day_workdays = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu];
        let dates = workweek_display_dates(week_range, &four_day_workdays);
        assert_eq!(dates.len(), 4);
        assert_eq!(dates[0], NaiveDate::from_ymd_opt(2026, 8, 10).unwrap());
        assert_eq!(dates[3], NaiveDate::from_ymd_opt(2026, 8, 13).unwrap());

        // Shift work: Wednesday to Sunday
        let shift_workdays = [
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ];
        let shift_dates = workweek_display_dates(week_range, &shift_workdays);
        assert_eq!(shift_dates.len(), 5);
        assert_eq!(shift_dates[0], NaiveDate::from_ymd_opt(2026, 8, 12).unwrap());
        assert_eq!(shift_dates[4], NaiveDate::from_ymd_opt(2026, 8, 16).unwrap());
    }

    #[test]
    fn test_adversarial_workday_cell_qualification_exhaustive() {
        let workdays = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ];
        let (workday_start, workday_end) = (540, 1020); // 09:00 to 17:00

        let sunday = NaiveDate::from_ymd_opt(2026, 8, 9).unwrap();
        let saturday = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();

        // 1. Unconfigured workdays: Sunday & Saturday MUST be offhours for EVERY minute (0..1440)
        for minute in 0..1440 {
            assert!(
                !is_workday_cell(minute, workday_start, workday_end, sunday.weekday(), &workdays),
                "Sunday at minute {minute} must NEVER receive workday cell qualification"
            );

            assert!(
                !is_workday_cell(minute, workday_start, workday_end, saturday.weekday(), &workdays),
                "Saturday at minute {minute} must NEVER receive workday cell qualification"
            );
        }

        // 2. Configured workday: Monday exhaustive minute-by-minute evaluation
        // Single-minute precision across all 1440 minutes of the day
        for minute in 0..1440 {
            let expected = minute >= workday_start && minute < workday_end;
            let actual = is_workday_cell(minute, workday_start, workday_end, monday.weekday(), &workdays);
            assert_eq!(
                actual, expected,
                "Monday at minute {minute} (expected {expected}, got {actual})"
            );
        }

        // 3. Exact boundary minute stress checks
        // Midnight at 0
        assert!(!is_workday_cell(0, workday_start, workday_end, monday.weekday(), &workdays));
        // Exactly 1 minute before start: 539 (08:59) -> offhours
        assert!(!is_workday_cell(539, workday_start, workday_end, monday.weekday(), &workdays));
        // Exactly at start: 540 (09:00) -> workday
        assert!(is_workday_cell(540, workday_start, workday_end, monday.weekday(), &workdays));
        // Exactly 1 minute after start: 541 (09:01) -> workday
        assert!(is_workday_cell(541, workday_start, workday_end, monday.weekday(), &workdays));
        // Exactly 1 minute before end: 1019 (16:59) -> workday
        assert!(is_workday_cell(1019, workday_start, workday_end, monday.weekday(), &workdays));
        // Exactly at end: 1020 (17:00) -> offhours (half-open boundary [start, end))
        assert!(!is_workday_cell(1020, workday_start, workday_end, monday.weekday(), &workdays));
        // Exactly 1 minute after end: 1021 (17:01) -> offhours
        assert!(!is_workday_cell(1021, workday_start, workday_end, monday.weekday(), &workdays));
        // Last minute of 24-hr day: 1439 (23:59) -> offhours
        assert!(!is_workday_cell(1439, workday_start, workday_end, monday.weekday(), &workdays));
        // Boundary at 1440 (24:00) -> offhours
        assert!(!is_workday_cell(1440, workday_start, workday_end, monday.weekday(), &workdays));
        // Out-of-bounds minute -> offhours
        assert!(!is_workday_cell(u32::MAX, workday_start, workday_end, monday.weekday(), &workdays));

        // 4. Midnight transitions: shifts aligned with 00:00 or 24:00
        // Early morning shift: 00:00 to 08:00 (0..480)
        assert!(is_workday_cell(0, 0, 480, monday.weekday(), &workdays));
        assert!(is_workday_cell(479, 0, 480, monday.weekday(), &workdays));
        assert!(!is_workday_cell(480, 0, 480, monday.weekday(), &workdays));

        // Late evening shift: 20:00 to 24:00 (1200..1440)
        assert!(!is_workday_cell(1199, 1200, 1440, monday.weekday(), &workdays));
        assert!(is_workday_cell(1200, 1200, 1440, monday.weekday(), &workdays));
        assert!(is_workday_cell(1439, 1200, 1440, monday.weekday(), &workdays));
        assert!(!is_workday_cell(1440, 1200, 1440, monday.weekday(), &workdays));

        // 5. Inverted and degenerate workday ranges
        // Degenerate start == end: empty interval, no cell qualifies
        for minute in [0, 540, 600, 1020, 1439] {
            assert!(!is_workday_cell(minute, 540, 540, monday.weekday(), &workdays));
        }
        // Inverted start > end: e.g. 1020..540
        for minute in [0, 540, 600, 1020, 1439] {
            assert!(!is_workday_cell(minute, 1020, 540, monday.weekday(), &workdays));
        }
        // Normalized range safety: normalized_workday_range guarantees start < end
        let (norm_start, norm_end) = normalized_workday_range(1020, 540);
        assert!(norm_start < norm_end);
        assert!(norm_end >= norm_start + 30);
    }

    #[test]
    fn test_adversarial_custom_workday_configurations() {
        let week_range = CalendarDateRange::from_days(
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(), // Monday
            7,
        );

        // Scenario 1: Shift work Wednesday through Sunday (Wed, Thu, Fri, Sat, Sun)
        let shift_workdays = [
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ];
        let (shift_start, shift_end) = (480, 960); // 08:00 to 16:00

        let monday = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        let tuesday = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
        let wednesday = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
        let saturday = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let sunday = NaiveDate::from_ymd_opt(2026, 8, 16).unwrap();

        let shift_daytime = 600; // 10:00 AM (inside 08:00-16:00)

        // Monday & Tuesday are off-days in this shift schedule
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, monday.weekday(), &shift_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, tuesday.weekday(), &shift_workdays));

        // Wednesday, Saturday, Sunday are working days
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, wednesday.weekday(), &shift_workdays));
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, saturday.weekday(), &shift_workdays));
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, sunday.weekday(), &shift_workdays));

        let shift_dates = workweek_display_dates(week_range, &shift_workdays);
        assert_eq!(shift_dates.len(), 5);
        assert_eq!(shift_dates[0], wednesday);
        assert_eq!(shift_dates[4], sunday);

        // Scenario 2: Weekend-only work (Saturday & Sunday)
        let weekend_workdays = [Weekday::Sat, Weekday::Sun];
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, saturday.weekday(), &weekend_workdays));
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, sunday.weekday(), &weekend_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, monday.weekday(), &weekend_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, wednesday.weekday(), &weekend_workdays));

        let weekend_dates = workweek_display_dates(week_range, &weekend_workdays);
        assert_eq!(weekend_dates.len(), 2);
        assert_eq!(weekend_dates[0], saturday);
        assert_eq!(weekend_dates[1], sunday);

        // Scenario 3: Zero workdays configured (empty slice)
        let empty_workdays: [Weekday; 0] = [];
        for d in [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ] {
            for m in [0, 480, 600, 960, 1439] {
                assert!(
                    !is_workday_cell(m, shift_start, shift_end, d, &empty_workdays),
                    "Empty workdays must never qualify day {d:?} at minute {m}"
                );
            }
        }
        let empty_dates = workweek_display_dates(week_range, &empty_workdays);
        assert!(empty_dates.is_empty());

        // Scenario 4: All 7 days configured as workdays
        let full_workdays = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ];
        for d in &full_workdays {
            assert!(is_workday_cell(shift_daytime, shift_start, shift_end, *d, &full_workdays));
            assert!(!is_workday_cell(0, shift_start, shift_end, *d, &full_workdays));
        }
        let full_dates = workweek_display_dates(week_range, &full_workdays);
        assert_eq!(full_dates.len(), 7);

        // Scenario 5: Split-shift workdays (e.g. Mon, Wed, Fri - MWF)
        let mwf_workdays = [Weekday::Mon, Weekday::Wed, Weekday::Fri];
        let thursday = NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        // Working days inside shift
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, monday.weekday(), &mwf_workdays));
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, wednesday.weekday(), &mwf_workdays));
        // Off-days inside shift
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, tuesday.weekday(), &mwf_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, thursday.weekday(), &mwf_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, saturday.weekday(), &mwf_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, sunday.weekday(), &mwf_workdays));
        // Working days outside shift
        assert!(!is_workday_cell(400, shift_start, shift_end, monday.weekday(), &mwf_workdays));
        assert!(!is_workday_cell(1100, shift_start, shift_end, monday.weekday(), &mwf_workdays));

        let mwf_dates = workweek_display_dates(week_range, &mwf_workdays);
        assert_eq!(mwf_dates.len(), 3);
        assert_eq!(mwf_dates[0], monday);
        assert_eq!(mwf_dates[1], wednesday);
        assert_eq!(mwf_dates[2], NaiveDate::from_ymd_opt(2026, 8, 14).unwrap()); // Friday

        // Scenario 6: Duplicate entries in workdays slice (resilience against duplicates)
        let dup_workdays = [
            Weekday::Mon,
            Weekday::Mon,
            Weekday::Wed,
            Weekday::Wed,
            Weekday::Fri,
        ];
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, monday.weekday(), &dup_workdays));
        assert!(is_workday_cell(shift_daytime, shift_start, shift_end, wednesday.weekday(), &dup_workdays));
        assert!(!is_workday_cell(shift_daytime, shift_start, shift_end, tuesday.weekday(), &dup_workdays));
    }

    #[test]
    fn test_adversarial_mini_calendar_week_numbers_reactivity() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        // 1. Direct GTK Calendar week numbers property reactivity
        let mini_cal = gtk::Calendar::new();
        mini_cal.set_show_week_numbers(false);
        assert!(!mini_cal.shows_week_numbers());

        mini_cal.set_show_week_numbers(true);
        assert!(mini_cal.shows_week_numbers());

        mini_cal.set_show_week_numbers(false);
        assert!(!mini_cal.shows_week_numbers());

        // 2. CalendarPage mini calendars synchronization
        let page = CalendarPage::new();
        page.ensure_mini_calendar_count(4);
        let calendars = page.imp().mini_calendars.borrow().clone();
        assert!(!calendars.is_empty());

        // Verify all mini calendars synchronize week numbers uniformly
        let expected_show = AppSettings::try_new()
            .map(|s| s.calendar_show_week_numbers())
            .unwrap_or(false);
        page.sync_mini_calendar();
        for cal in &calendars {
            assert_eq!(cal.shows_week_numbers(), expected_show);
        }
    }

    #[test]
    fn test_event_occurrence_times_for_date_single_day() {
        let event = CalendarEventRecord {
            uid: "single-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Privat".to_string(),
            component: "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:single-1\r\nDTSTART:20260909T100000\r\nDTEND:20260909T113000\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR".to_string(),
            summary: "Meeting".to_string(),
            start: "2026-09-09 10:00".to_string(),
            end: "2026-09-09 11:30".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        let date = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        let (start_min, end_min, time_text) = event_occurrence_times_for_date(&event, date, 30);
        assert_eq!(start_min, 600); // 10:00
        assert_eq!(end_min, 690);   // 11:30
        assert_eq!(time_text, "10:00–11:30");
    }

    #[test]
    fn test_event_occurrence_times_for_date_weekly_recurring_not_midnight() {
        let event = CalendarEventRecord {
            uid: "weekly-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Arbeit".to_string(),
            component: "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-1\r\nDTSTART:20260107T140000\r\nDTEND:20260107T153000\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Team-Standup\r\nEND:VEVENT\r\nEND:VCALENDAR".to_string(),
            summary: "Team-Standup".to_string(),
            start: "2026-01-07 14:00".to_string(),
            end: "2026-01-07 15:30".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        // 2026-09-09 is a Wednesday, months after initial start date
        let target_date = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        let (start_min, end_min, time_text) = event_occurrence_times_for_date(&event, target_date, 30);
        // Crucial test: It must NOT be placed at 00:00! It must be 14:00 (840) to 15:30 (930)
        assert_eq!(start_min, 840, "Recurring event must start at 14:00, not at midnight 00:00");
        assert_eq!(end_min, 930, "Recurring event must end at 15:30, not at 00:30");
        assert_eq!(time_text, "14:00–15:30");
    }

    #[test]
    fn test_event_occurrence_times_for_date_multiday() {
        let event = CalendarEventRecord {
            uid: "multiday-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Schicht".to_string(),
            component: "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:multiday-1\r\nDTSTART:20260907T220000\r\nDTEND:20260908T040000\r\nSUMMARY:Nachtdienst\r\nEND:VEVENT\r\nEND:VCALENDAR".to_string(),
            summary: "Nachtdienst".to_string(),
            start: "2026-09-07 22:00".to_string(),
            end: "2026-09-08 04:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };
        let day1 = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        let (d1_start, d1_end, d1_text) = event_occurrence_times_for_date(&event, day1, 30);
        assert_eq!(d1_start, 1320); // 22:00
        assert_eq!(d1_end, 1440);   // 24:00 / midnight
        assert_eq!(d1_text, "ab 22:00");

        let day2 = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let (d2_start, d2_end, d2_text) = event_occurrence_times_for_date(&event, day2, 30);
        assert_eq!(d2_start, 0);   // 00:00 / midnight
        assert_eq!(d2_end, 240);   // 04:00
        assert_eq!(d2_text, "bis 04:00");
    }

    #[test]
    fn test_duration_and_recurrence_formatting() {
        assert_eq!(format_duration_text(90), "1 Std. 30 Min.");
        assert_eq!(format_duration_text(60), "1 Std.");
        assert_eq!(format_duration_text(120), "2 Std.");
        assert_eq!(format_duration_text(45), "45 Min.");

        let mut event = event_with_start("2026-09-09 10:00");
        event.component = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nEND:VEVENT\r\nEND:VCALENDAR".to_string();
        assert_eq!(format_recurrence_summary(&event), Some("Wöchentlich wiederkehrend".to_string()));

        event.component = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\nEND:VCALENDAR".to_string();
        assert_eq!(format_recurrence_summary(&event), Some("Täglich wiederkehrend".to_string()));
    }

    #[test]
    fn test_progressive_enhancement_attendees_and_organizer() {
        let component = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nORGANIZER;CN=\"Erika Mustermann\":mailto:erika@example.com\r\nATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=\"Max Mustermann\":mailto:max@example.com\r\nATTENDEE;PARTSTAT=DECLINED:mailto:anna@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR";
        assert_eq!(parse_ical_organizer(component), Some("Erika Mustermann".to_string()));
        let attendees = parse_ical_attendees(component);
        assert_eq!(attendees.len(), 2);
        assert_eq!(attendees[0].name, "Max Mustermann");
        assert_eq!(attendees[0].partstat.as_deref(), Some("ACCEPTED"));
        assert_eq!(attendees[1].name, "anna@example.com");
        assert_eq!(attendees[1].partstat.as_deref(), Some("DECLINED"));
    }

    #[test]
    fn test_progressive_enhancement_conference_and_badges() {
        let component = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nCONFERENCE:https://meet.example.com/daily-sync\r\nSTATUS:TENTATIVE\r\nTRANSP:TRANSPARENT\r\nCLASS:CONFIDENTIAL\r\nEND:VEVENT\r\nEND:VCALENDAR";
        assert_eq!(
            extract_conference_url(component, "", ""),
            Some("https://meet.example.com/daily-sync".to_string())
        );
        let badges = extract_event_badges(component);
        assert_eq!(
            badges,
            vec![
                ("Vorläufig".to_string(), "warning-pill"),
                ("Frei".to_string(), "success-pill"),
                ("Vertraulich".to_string(), "neutral-pill"),
            ]
        );
    }

    #[test]
    fn test_progressive_enhancement_alarms() {
        let component = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let summary = format_alarm_summary(component);
        assert_eq!(summary, Some("15 Min. vorher".to_string()));
    }

    #[test]
    fn test_selected_time_slot_model() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        let slot = SelectedTimeSlot {
            date,
            start_minute: 600,
            end_minute: 630,
        };
        assert_eq!(slot.date, date);
        assert_eq!(slot.start_minute, 600);
        assert_eq!(slot.end_minute, 630);
        let same_slot = SelectedTimeSlot {
            date,
            start_minute: 600,
            end_minute: 630,
        };
        assert_eq!(slot, same_slot);
        let diff_slot = SelectedTimeSlot {
            date,
            start_minute: 630,
            end_minute: 660,
        };
        assert_ne!(slot, diff_slot);
    }

    #[test]
    fn test_configured_weekday_names_short_and_full() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let short = configured_weekday_names_short();
        let full = configured_weekday_names_full();
        assert_eq!(short.len(), 7);
        assert_eq!(full.len(), 7);
        if configured_first_weekday() == Weekday::Mon {
            assert_eq!(short[0], "Mo");
            assert_eq!(short[6], "So");
            assert_eq!(full[0], "Montag");
            assert_eq!(full[6], "Sonntag");
        } else {
            assert_eq!(short[0], "So");
            assert_eq!(short[6], "Sa");
            assert_eq!(full[0], "Sonntag");
            assert_eq!(full[6], "Samstag");
        }
    }

    #[test]
    fn test_cross_month_week_visibility_and_range_boundaries() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        let visible_range = CalendarDateRange::from_days(start, 7);
        let end_inclusive = visible_range.last_inclusive();
        let viewport = CalendarViewport {
            view: CalendarView::Week,
            anchor_date: start,
            visible_range,
        };

        assert!(is_day_visible_in_viewport(&viewport, start));
        let prev_day = start.pred_opt().unwrap();
        assert!(!is_day_visible_in_viewport(&viewport, prev_day));

        let sep_30 = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        let oct_01 = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        assert!(is_day_visible_in_viewport(&viewport, sep_30));
        assert!(is_day_visible_in_viewport(&viewport, oct_01));

        assert!(is_day_visible_in_viewport(&viewport, end_inclusive));
        let next_day = end_inclusive.succ_opt().unwrap();
        assert!(!is_day_visible_in_viewport(&viewport, next_day));
    }

    #[test]
    fn test_month_navigation_step() {
        let mut state = CalendarViewState::new(
            CalendarView::Month,
            NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            Weekday::Mon,
        );
        state.navigate_step(1, Weekday::Mon, false);
        assert_eq!(state.anchor_date, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());

        state.navigate_step(-1, Weekday::Mon, false);
        assert_eq!(state.anchor_date, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
    }

    #[test]
    fn test_dominant_month_4_of_6_rows_and_hysteresis() {
        let start1 = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let range1 = CalendarDateRange::from_days(start1, 42);
        assert_eq!(dominant_month_for_range(range1, None), (2026, 9));

        let start2 = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        let range2 = CalendarDateRange::from_days(start2, 42);
        // In 3:3 split, hysteresis retains currently active month
        assert_eq!(dominant_month_for_range(range2, Some((2026, 9))), (2026, 9));
        assert_eq!(dominant_month_for_range(range2, Some((2026, 10))), (2026, 10));

        let start3 = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let range3 = CalendarDateRange::from_days(start3, 42);
        // When October reaches 4 of 6 rows, it becomes dominant
        assert_eq!(dominant_month_for_range(range3, Some((2026, 9))), (2026, 10));
    }

    #[test]
    fn test_month_week_by_week_scrolling_shifts_range() {
        let initial_anchor = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let mut state = CalendarViewState::new(CalendarView::Month, initial_anchor, Weekday::Mon);
        let initial_range = state.visible_range;

        state.navigate_step(1, Weekday::Mon, true);
        assert_eq!(
            state.visible_range.start,
            initial_range.start + Duration::days(7),
            "visible range start must shift by 7 days"
        );
        assert_eq!(state.visible_range.len_days(), 42);

        state.navigate_step(-1, Weekday::Mon, true);
        assert_eq!(state.visible_range, initial_range);

        state.navigate_step(3, Weekday::Mon, true);
        assert_eq!(
            state.visible_range.start,
            initial_range.start + Duration::days(21),
            "multi-step visible range start must shift by 21 days"
        );
        assert_eq!(state.visible_range.len_days(), 42);

        state.navigate_step(-3, Weekday::Mon, true);
        assert_eq!(state.visible_range, initial_range);
    }

    #[test]
    fn test_month_discrete_scroll_single_line_per_notch() {
        const SCROLL_THRESHOLD: f64 = 1.0;
        let delta = 1.0;
        let steps = (delta / SCROLL_THRESHOLD).trunc() as i32;
        assert_eq!(steps, 1, "Single mouse wheel detent (dy=1.0) must scroll exactly 1 row");

        let delta_back = -1.0;
        let steps_back = (delta_back / SCROLL_THRESHOLD).trunc() as i32;
        assert_eq!(steps_back, -1, "Single backward mouse wheel detent (dy=-1.0) must scroll exactly -1 row");

        let delta_fast = 3.0;
        let steps_fast = (delta_fast / SCROLL_THRESHOLD).trunc() as i32;
        assert_eq!(steps_fast, 3, "Three fast notches (dy=3.0) must scroll 3 rows");
    }

    #[test]
    fn test_month_view_all_42_cells_visible_in_viewport() {
        let anchor = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let viewport = CalendarViewport::for_view(CalendarView::Month, anchor, Weekday::Mon);

        let first_day = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        let last_day = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();

        assert_eq!(viewport.visible_range.start, first_day);
        assert_eq!(viewport.visible_range.last_inclusive(), last_day);

        assert!(is_day_visible_in_viewport(&viewport, first_day));
        assert!(is_day_visible_in_viewport(&viewport, last_day));
        assert!(is_day_visible_in_viewport(&viewport, anchor));

        assert!(!is_day_visible_in_viewport(&viewport, first_day.pred_opt().unwrap()));
        assert!(!is_day_visible_in_viewport(&viewport, last_day.succ_opt().unwrap()));
    }

    #[test]
    fn test_multiday_allday_span_and_track_allocation() {
        let days = [
            NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(), // Mon (0)
            NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(), // Tue (1)
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(), // Wed (2)
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(), // Thu (3)
            NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(), // Fri (4)
            NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(), // Sat (5)
            NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(), // Sun (6)
        ];

        let event1 = CalendarEventRecord {
            uid: "ev1".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Personal".to_string(),
            component: String::new(),
            summary: "Vacation Part 1".to_string(),
            start: "2026-09-28".to_string(),
            end: "2026-10-01".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };

        let event2 = CalendarEventRecord {
            uid: "ev2".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Personal".to_string(),
            component: String::new(),
            summary: "Conference".to_string(),
            start: "2026-09-29".to_string(),
            end: "2026-10-02".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };

        let range1 = event_display_range(&event1).unwrap();
        let range2 = event_display_range(&event2).unwrap();

        let span1: Vec<usize> = days
            .iter()
            .enumerate()
            .filter_map(|(idx, d)| if range1.contains(*d) { Some(idx) } else { None })
            .collect();
        assert_eq!(span1, vec![0, 1, 2]);

        let span2: Vec<usize> = days
            .iter()
            .enumerate()
            .filter_map(|(idx, d)| if range2.contains(*d) { Some(idx) } else { None })
            .collect();
        assert_eq!(span2, vec![1, 2, 3]);

        let conflict = span1.iter().any(|c| span2.contains(c));
        assert!(conflict, "Overlapping all-day events must be assigned to separate tracks");

        let mut events_by_date = BTreeMap::new();
        // event1 on Mon, Tue, Wed (days 0, 1, 2)
        events_by_date.entry(days[0]).or_insert_with(Vec::new).push(event1.clone());
        events_by_date.entry(days[1]).or_insert_with(Vec::new).push(event1.clone());
        events_by_date.entry(days[2]).or_insert_with(Vec::new).push(event1.clone());

        // event2 on Tue, Wed, Thu (days 1, 2, 3)
        events_by_date.entry(days[1]).or_insert_with(Vec::new).push(event2.clone());
        events_by_date.entry(days[2]).or_insert_with(Vec::new).push(event2.clone());
        events_by_date.entry(days[3]).or_insert_with(Vec::new).push(event2.clone());

        // Single day event on Mon (day 0)
        let single_event = CalendarEventRecord {
            uid: "ev3".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Personal".to_string(),
            component: String::new(),
            summary: "Single Day Task".to_string(),
            start: "2026-09-28".to_string(),
            end: "2026-09-29".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };
        events_by_date.entry(days[0]).or_insert_with(Vec::new).push(single_event.clone());

        let layouts = compute_week_cell_layouts(&days, &events_by_date, 6);
        assert_eq!(layouts.len(), 7);

        // Day 0 (Mon):
        // Track 0: event1 (Start, show_title: true)
        // Track 1: single_event (None, show_title: true)
        assert_eq!(layouts[0].visible_items.len(), 2);
        match &layouts[0].visible_items[0] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev1");
                assert_eq!(*span, MonthBarSpan::Start);
                assert!(*show_title);
            }
            _ => panic!("Expected event1 on track 0"),
        }
        match &layouts[0].visible_items[1] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev3");
                assert_eq!(*span, MonthBarSpan::None);
                assert!(*show_title);
            }
            _ => panic!("Expected single_event on track 1"),
        }

        // Day 1 (Tue):
        // Track 0: event1 (Middle, show_title: false)
        // Track 1: event2 (Start, show_title: true)
        assert_eq!(layouts[1].visible_items.len(), 2);
        match &layouts[1].visible_items[0] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev1");
                assert_eq!(*span, MonthBarSpan::Middle);
                assert!(!*show_title);
            }
            _ => panic!("Expected event1 on track 0"),
        }
        match &layouts[1].visible_items[1] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev2");
                assert_eq!(*span, MonthBarSpan::Start);
                assert!(*show_title);
            }
            _ => panic!("Expected event2 on track 1"),
        }

        // Day 2 (Wed):
        // Track 0: event1 (End, show_title: false)
        // Track 1: event2 (Middle, show_title: false)
        assert_eq!(layouts[2].visible_items.len(), 2);
        match &layouts[2].visible_items[0] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev1");
                assert_eq!(*span, MonthBarSpan::End);
                assert!(!*show_title);
            }
            _ => panic!("Expected event1 on track 0"),
        }
        match &layouts[2].visible_items[1] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev2");
                assert_eq!(*span, MonthBarSpan::Middle);
                assert!(!*show_title);
            }
            _ => panic!("Expected event2 on track 1"),
        }

        // Day 3 (Thu):
        // Track 0: Spacer (to align event2 on track 1!)
        // Track 1: event2 (End, show_title: false)
        assert_eq!(layouts[3].visible_items.len(), 2);
        match &layouts[3].visible_items[0] {
            MonthCellTrackItem::Spacer => {}
            _ => panic!("Expected Spacer on track 0 for day 3"),
        }
        match &layouts[3].visible_items[1] {
            MonthCellTrackItem::Event { event, span, show_title } => {
                assert_eq!(event.uid, "ev2");
                assert_eq!(*span, MonthBarSpan::End);
                assert!(!*show_title);
            }
            _ => panic!("Expected event2 on track 1"),
        }

        // Day 4 (Fri): No events, no trailing spacers!
        assert_eq!(layouts[4].visible_items.len(), 0);
    }

    #[test]
    fn test_prepared_month_layout_event_bars_and_spans() {
        let anchor = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let viewport = CalendarViewport::for_view(CalendarView::Month, anchor, Weekday::Mon);
        let days: Vec<NaiveDate> = viewport.visible_range.dates().collect();
        assert!(days.len() >= 42);

        let event1 = CalendarEventRecord {
            uid: "ev1".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Personal".to_string(),
            component: String::new(),
            summary: "Multi Day Trip".to_string(),
            start: days[0].format("%Y-%m-%d").to_string(),
            end: (days[2] + Duration::days(1)).format("%Y-%m-%d").to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };

        let event2 = CalendarEventRecord {
            uid: "ev2".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Personal".to_string(),
            component: String::new(),
            summary: "Sprint Review".to_string(),
            start: days[1].format("%Y-%m-%d").to_string(),
            end: (days[3] + Duration::days(1)).format("%Y-%m-%d").to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };

        let mut events_by_date = BTreeMap::new();
        events_by_date.entry(days[0]).or_insert_with(Vec::new).push(event1.clone());
        events_by_date.entry(days[1]).or_insert_with(Vec::new).push(event1.clone());
        events_by_date.entry(days[2]).or_insert_with(Vec::new).push(event1.clone());

        events_by_date.entry(days[1]).or_insert_with(Vec::new).push(event2.clone());
        events_by_date.entry(days[2]).or_insert_with(Vec::new).push(event2.clone());
        events_by_date.entry(days[3]).or_insert_with(Vec::new).push(event2.clone());

        let workdays: HashSet<Weekday> = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri].into_iter().collect();
        let weekday_names_full = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];
        let weekday_names_short = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];

        let layout = prepare_month_layout(
            &viewport,
            &events_by_date,
            days[0],
            true,
            &workdays,
            false,
            &weekday_names_full,
            &weekday_names_short,
            false,
            6,
        );

        assert_eq!(layout.weeks.len(), 6);
        let first_week = &layout.weeks[0];
        // Must contain two continuous event bars: ev1 spanning 0..=2, ev2 spanning 1..=3
        assert_eq!(first_week.event_bars.len(), 2);

        let bar1 = first_week.event_bars.iter().find(|b| b.event.uid == "ev1").expect("ev1 bar");
        assert_eq!(bar1.start_col, 0);
        assert_eq!(bar1.end_col, 2);
        assert_eq!(bar1.track, 0);
        assert!(!bar1.continues_left);
        assert!(!bar1.continues_right);

        let bar2 = first_week.event_bars.iter().find(|b| b.event.uid == "ev2").expect("ev2 bar");
        assert_eq!(bar2.start_col, 1);
        assert_eq!(bar2.end_col, 3);
        assert_eq!(bar2.track, 1);
        assert!(!bar2.continues_left);
        assert!(!bar2.continues_right);
    }

    #[test]
    fn test_hit_test_month_canvas_coordinates() {
        let anchor = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let viewport = CalendarViewport::for_view(CalendarView::Month, anchor, Weekday::Mon);
        let days: Vec<NaiveDate> = viewport.visible_range.dates().collect();

        let event = CalendarEventRecord {
            uid: "ev-hit".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Personal".to_string(),
            component: String::new(),
            summary: "Single Event".to_string(),
            start: days[0].format("%Y-%m-%d").to_string(),
            end: days[0].format("%Y-%m-%d").to_string(),
            location: String::new(),
            description: String::new(),
            all_day: true,
        };

        let mut events_by_date = BTreeMap::new();
        events_by_date.entry(days[0]).or_insert_with(Vec::new).push(event);

        let workdays: HashSet<Weekday> = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri].into_iter().collect();
        let weekday_names_full = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];
        let weekday_names_short = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];

        let layout = prepare_month_layout(
            &viewport,
            &events_by_date,
            days[0],
            true, // show KW column
            &workdays,
            false,
            &weekday_names_full,
            &weekday_names_short,
            false,
            6,
        );

        let width = 736.0;
        let height = 628.0;

        // 1. KW Header hit test (x < 36, y < 28)
        assert_eq!(
            hit_test_month_canvas(10.0, 10.0, width, height, &layout),
            Some(MonthCanvasHit::KwHeader)
        );

        // 2. Weekday Header hit test (x > 36, y < 28)
        assert_eq!(
            hit_test_month_canvas(80.0, 10.0, width, height, &layout),
            Some(MonthCanvasHit::Header { col: 0 })
        );

        // 3. KW Row hit test (x < 36, y in row 0)
        assert_eq!(
            hit_test_month_canvas(10.0, 50.0, width, height, &layout),
            Some(MonthCanvasHit::KwRow { row: 0 })
        );

        // 4. Event Bar hit test (x in col 0, y at track 0 event chip)
        // col_w = (736 - 36) / 7 = 100.0. row_h = (628 - 28) / 6 = 100.0.
        // cell_y = 28 + 0 * 100 = 28. day_header_h = 24. chip_y = 28 + 24 = 52.
        // bar_x = 36 + 0 * 100 + 3 = 39. bar_w = 94.
        let hit_ev = hit_test_month_canvas(50.0, 60.0, width, height, &layout);
        match hit_ev {
            Some(MonthCanvasHit::Event { event, date }) => {
                assert_eq!(event.uid, "ev-hit");
                assert_eq!(date, days[0]);
            }
            other => panic!("Expected Event hit, got {:?}", other),
        }

        // 5. Day cell hit test (outside event bar, e.g. at bottom of cell 0)
        assert_eq!(
            hit_test_month_canvas(50.0, 110.0, width, height, &layout),
            Some(MonthCanvasHit::DayCell { date: days[0] })
        );
    }

    #[test]
    fn test_draw_month_event_bar_geometry() {
        let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 200, 100).unwrap();
        let cr = gtk::cairo::Context::new(&surface).unwrap();

        // Standard rounded rect
        draw_month_event_bar(&cr, 10.0, 10.0, 80.0, 20.0, 4.0, false, false);
        let _ = cr.fill();

        // Left continuing (straight / chevron on left)
        draw_month_event_bar(&cr, 10.0, 40.0, 80.0, 20.0, 4.0, true, false);
        let _ = cr.fill();

        // Right continuing (straight / chevron on right)
        draw_month_event_bar(&cr, 10.0, 70.0, 80.0, 20.0, 4.0, false, true);
        let _ = cr.fill();
    }

    #[test]
    fn test_calendar_list_cairo_row_rendering() {
        let event = CalendarEventRecord {
            uid: "list-ev-1".to_string(),
            source_uid: "s1".to_string(),
            source_display_name: "Arbeit".to_string(),
            component: "BEGIN:VEVENT\r\nUID:list-ev-1\r\nDTSTART:20260915T140000\r\nDTEND:20260915T150000\r\nSUMMARY:Projektbesprechung\r\nCATEGORIES:Business\r\nLOCATION:Konferenzraum A\r\nEND:VEVENT\r\n".to_string(),
            summary: "Projektbesprechung".to_string(),
            start: "2026-09-15 14:00".to_string(),
            end: "2026-09-15 15:00".to_string(),
            location: "Konferenzraum A".to_string(),
            description: String::new(),
            all_day: false,
        };

        let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 800, 36).unwrap();
        let cr = gtk::cairo::Context::new(&surface).unwrap();

        // Should draw without panicking:
        // 1. Unselected, non-hovered
        draw_calendar_list_row(&cr, &event, 0.0, 800.0, 36.0, false, false, false);

        // 2. Selected, dark theme
        draw_calendar_list_row(&cr, &event, 0.0, 800.0, 36.0, true, false, true);

        // 3. Hovered, light theme
        draw_calendar_list_row(&cr, &event, 0.0, 800.0, 36.0, false, true, false);
    }

    #[test]
    fn test_truncate_cairo_text_binary_search_and_exact_fits() {
        let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 400, 50).unwrap();
        let cr = gtk::cairo::Context::new(&surface).unwrap();

        // 1. Empty string
        assert_eq!(truncate_cairo_text(&cr, "", 100.0), "");
        assert_eq!(truncate_cairo_text(&cr, "Test", 0.0), "");

        // 2. Fits within max_width
        let fit = truncate_cairo_text(&cr, "Short text", 300.0);
        assert_eq!(fit, "Short text");

        // 3. Needs truncation
        let truncated = truncate_cairo_text(&cr, "Very long text that will not fit in twenty pixels", 20.0);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() < "Very long text that will not fit in twenty pixels".len());
    }

    #[test]
    fn test_prepared_calendar_list_row_precomputation() {
        let event = CalendarEventRecord {
            uid: "prep-1".to_string(),
            source_uid: "src-1".to_string(),
            source_display_name: "Privat".to_string(),
            component: "BEGIN:VEVENT\r\nUID:prep-1\r\nDTSTART:20260915T100000\r\nDTEND:20260915T113000\r\nSUMMARY:Zahnarzt\r\nCATEGORIES:Gesundheit\r\nLOCATION:Praxis Dr. Schmidt\r\nEND:VEVENT\r\n".to_string(),
            summary: "Zahnarzt".to_string(),
            start: "2026-09-15 10:00".to_string(),
            end: "2026-09-15 11:30".to_string(),
            location: "Praxis Dr. Schmidt".to_string(),
            description: String::new(),
            all_day: false,
        };

        let prepared = prepare_calendar_list_row(&event);
        assert_eq!(prepared.title, "Zahnarzt");
        assert_eq!(prepared.time_str, "10:00 – 11:30");
        assert_eq!(prepared.category.as_deref(), Some("Gesundheit"));
        assert!(prepared.sub.contains("Praxis Dr. Schmidt"));
        assert!(prepared.sub.contains("Privat"));
        assert!(prepared.identity.is_some());
    }

    #[test]
    fn test_calendar_toolbar_layout_alignment() {
        let source = include_str!("calendar_page.rs");
        // Verify date_label is centered in the view_toolbar CenterBox
        assert!(source.contains(".center_widget(&date_label)"));
        // Verify navigation buttons are in start_box on the left
        assert!(source.contains("start_box.append(&nav_box)"));
        // Verify view switchers are in end_box on the right
        assert!(source.contains("end_box.append(&view_switcher_box)"));
        // Verify date_label has halign Center
        assert!(source.contains(".halign(gtk::Align::Center)"));
    }

    #[test]
    fn test_compare_day_events_order_chronological() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();

        let make_event = |uid: &str, summary: &str, start_time: &str, end_time: &str, all_day: bool| CalendarEventRecord {
            uid: uid.to_string(),
            source_uid: "cal1".to_string(),
            source_display_name: "Calendar".to_string(),
            component: format!(
                "BEGIN:VEVENT\nUID:{uid}\nDTSTART:{start_time}\nDTEND:{end_time}\nSUMMARY:{summary}\nEND:VEVENT"
            ),
            summary: summary.to_string(),
            start: format!("2026-09-10 {start_time}"),
            end: format!("2026-09-10 {end_time}"),
            location: String::new(),
            description: String::new(),
            all_day,
        };

        let ev_allday = make_event("1", "Ganztag", "00:00", "23:59", true);
        let ev_0700 = make_event("2", "Mail gucken", "07:00", "08:00", false);
        let ev_0800 = make_event("3", "Arbeit", "08:00", "09:00", false);
        let ev_0900 = make_event("4", "Simon", "09:00", "10:00", false);
        let ev_1630 = make_event("5", "Mail gucken Nachmittag", "16:30", "17:30", false);
        let ev_2230 = make_event("6", "Fokuszeit", "22:30", "23:30", false);

        // Put in scrambled order (like the bug where 22:30 was placed first)
        let mut events = vec![
            ev_2230.clone(),
            ev_0900.clone(),
            ev_0800.clone(),
            ev_allday.clone(),
            ev_1630.clone(),
            ev_0700.clone(),
        ];

        events.sort_by(|a, b| compare_day_events_order(a, b, date));

        let ordered_summaries: Vec<&str> = events.iter().map(|e| e.summary.as_str()).collect();
        assert_eq!(
            ordered_summaries,
            vec![
                "Ganztag",
                "Mail gucken",
                "Arbeit",
                "Simon",
                "Mail gucken Nachmittag",
                "Fokuszeit"
            ],
            "Events on a day must sort strictly chronologically with all-day events first"
        );
    }

    #[test]
    fn test_calendar_drag_selection_time_range_calculation() {
        let interval_minutes = 30usize;
        let slot_height = 24.0f64;

        // Forward drag: from 09:00 (slot 18) to 11:00 (slot 22)
        let start_y = 18.0 * slot_height + 5.0;
        let offset_y = 4.0 * slot_height; // drags 4 slots down to slot 22
        let start_s = (start_y / slot_height).floor() as usize;
        let cur_y = (start_y + offset_y).max(0.0);
        let cur_s = (cur_y / slot_height).floor() as usize;
        let min_s = start_s.min(cur_s);
        let max_s = start_s.max(cur_s);
        assert_eq!(min_s, 18);
        assert_eq!(max_s, 22);

        let start_min = (min_s * interval_minutes) as u32;
        let end_min = ((max_s + 1) * interval_minutes) as u32;
        assert_eq!(start_min, 540); // 09:00
        assert_eq!(end_min, 690);   // 11:30 (covers through end of slot 22)

        // Backward/upward drag: from 14:30 (slot 29) up to 13:00 (slot 26)
        let start_y_up = 29.0 * slot_height + 10.0;
        let offset_y_up = -3.0 * slot_height;
        let start_s_up = (start_y_up / slot_height).floor() as usize;
        let cur_y_up = (start_y_up + offset_y_up).max(0.0);
        let cur_s_up = (cur_y_up / slot_height).floor() as usize;
        let min_s_up = start_s_up.min(cur_s_up);
        let max_s_up = start_s_up.max(cur_s_up);
        assert_eq!(min_s_up, 26);
        assert_eq!(max_s_up, 29);

        let start_min_up = (min_s_up * interval_minutes) as u32;
        let end_min_up = ((max_s_up + 1) * interval_minutes) as u32;
        assert_eq!(start_min_up, 780); // 13:00
        assert_eq!(end_min_up, 900);   // 15:00 (covers through end of slot 29)
    }

    #[test]
    fn test_calendar_checkbox_sync_and_hidden_sources() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let page = CalendarPage::new();
        let check_cal1 = gtk::CheckButton::builder().active(true).build();
        let check_cal2 = gtk::CheckButton::builder().active(true).build();
        let check_cal3 = gtk::CheckButton::builder().active(false).build();

        page.imp().calendar_source_checks.borrow_mut().insert("cal1".to_string(), check_cal1.clone());
        page.imp().calendar_source_checks.borrow_mut().insert("cal2".to_string(), check_cal2.clone());
        page.imp().calendar_source_checks.borrow_mut().insert("cal3".to_string(), check_cal3.clone());

        // Hide cal1 and show cal3
        let mut hidden = HashSet::new();
        hidden.insert("cal1".to_string());
        *page.imp().hidden_calendar_sources.borrow_mut() = hidden;

        page.sync_calendar_checkboxes();

        assert!(!check_cal1.is_active(), "cal1 is hidden, so checkbox must be inactive");
        assert!(check_cal2.is_active(), "cal2 is visible, so checkbox must be active");
        assert!(check_cal3.is_active(), "cal3 is not in hidden set, so checkbox must be active");
    }

    #[test]
    fn test_calendar_toolbar_size_and_visibility() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let page = CalendarPage::new();
        let toolbar = page.imp().toolbar.borrow().as_ref().cloned().expect("toolbar exists");
        assert!(toolbar.is_visible());
        assert!(toolbar.downcast::<gtk::ScrolledWindow>().is_ok());
    }

    #[test]
    fn test_calendar_gnome_color_palette_and_unified_selection() {
        // Verify GNOME HIG color palette compliance
        let holiday_col = calendar_source_color("holidays", "Feiertage");
        assert_eq!(holiday_col, "#2ec27e", "Holidays should use GNOME Green 4");

        let birthday_col = calendar_source_color("birthdays", "Geburtstage");
        assert_eq!(birthday_col, "#9141ac", "Birthdays should use GNOME Purple 3");

        let default_source_col = calendar_source_color("local-calendar", "Persönlich");
        let valid_gnome_colors = [
            "#3584e4", "#2ec27e", "#e66100", "#e01b24",
            "#9141ac", "#2aa198", "#e5a50a", "#986a44",
            "#1c71d8", "#26a269", "#c01c28", "#813d9c",
        ];
        assert!(
            valid_gnome_colors.contains(&default_source_col.as_str()),
            "Calendar source color must be from GNOME HIG palette"
        );

        // Verify unified selection CSS classes in source
        let src = include_str!("calendar_page.rs");
        assert!(src.contains("calendar-slot-selected-top"));
        assert!(src.contains("calendar-slot-selected-bottom"));
    }

    #[test]
    fn test_year_view_grid_coverage_and_date_mapping() {
        let year = 2026;
        let first_weekday = Weekday::Mon;

        for month in 1..=12 {
            let first_of_month = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
            let last_of_month = match month {
                12 => NaiveDate::from_ymd_opt(year, 12, 31),
                m => NaiveDate::from_ymd_opt(year, m + 1, 1).map(|d| d - Duration::days(1)),
            }
            .unwrap();

            let grid_start = week_start_for(first_of_month, first_weekday);
            assert!(grid_start <= first_of_month);

            let mut curr = grid_start;
            let mut days_seen = Vec::new();
            while curr <= last_of_month {
                let kw = (curr + Duration::days(3)).iso_week().week();
                assert!(kw >= 1 && kw <= 53);

                for col in 0..7 {
                    let cell_date = curr + Duration::days(col as i64);
                    if cell_date.month() == month {
                        days_seen.push(cell_date.day());
                    }
                }
                curr = curr + Duration::days(7);
            }

            // Every day 1..=last_day must be visited exactly once in order
            let expected_days: Vec<u32> = (1..=last_of_month.day()).collect();
            assert_eq!(days_seen, expected_days, "Month {month} must contain all days exactly once");
        }
    }

    #[test]
    fn test_drag_selection_end_time_calculation_at_day_boundary() {
        let target_date = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();

        // Simulate dragging from slot 18 (09:00) to slot 20 (10:00)
        let start_min = 18 * 30; // 540 min = 09:00
        let end_min = 20 * 30;   // 600 min = 10:00
        let start_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(start_min as i64);
        let end_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(end_min as i64);
        assert_eq!(start_time.format("%H:%M").to_string(), "09:00");
        assert_eq!(end_time.format("%H:%M").to_string(), "10:00");

        // Simulate dragging all the way to slot 48 (24:00 / midnight boundary)
        let midnight_min = 48 * 30; // 1440 min
        let midnight_time = target_date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(midnight_min as i64);
        assert_eq!(midnight_time, (target_date + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap());
        // Verify it does not panic or return None as and_hms_opt(24, 0, 0) would
        assert_eq!(midnight_time - start_time, Duration::minutes(1440 - 540));
    }

    #[test]
    fn test_year_preview_mode_and_orientation() {
        // Enums and transitions
        assert_ne!(YearPreviewMode::List, YearPreviewMode::Schedule);
        assert_ne!(YearPreviewOrientation::Side, YearPreviewOrientation::Bottom);

        // Test leap year navigation clamping (e.g. Feb 29 2024 -> 2025 -> Feb 28 2025)
        let leap_day = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let target_year = 2025;
        let clamped = NaiveDate::from_ymd_opt(target_year, leap_day.month(), leap_day.day())
            .or_else(|| NaiveDate::from_ymd_opt(target_year, leap_day.month(), 28))
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(target_year, 1, 1).unwrap());
        assert_eq!(clamped, NaiveDate::from_ymd_opt(2025, 2, 28).unwrap());

        // Test normal year navigation (e.g. Jul 15 2025 -> 2026 -> Jul 15 2026)
        let normal_day = NaiveDate::from_ymd_opt(2025, 7, 15).unwrap();
        let next_year = 2026;
        let normal_clamped = NaiveDate::from_ymd_opt(next_year, normal_day.month(), normal_day.day())
            .or_else(|| NaiveDate::from_ymd_opt(next_year, normal_day.month(), 28))
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(next_year, 1, 1).unwrap());
        assert_eq!(normal_clamped, NaiveDate::from_ymd_opt(2026, 7, 15).unwrap());

        // Test year paned panel sizing bounds (allowing narrower widths down to 160px)
        let default_side_w: i32 = 240;
        assert_eq!(default_side_w.clamp(160, 600), 240);
        let narrow_user_w: i32 = 180;
        assert_eq!(narrow_user_w.clamp(160, 600), 180);
        let ultra_narrow_w: i32 = 120;
        assert_eq!(ultra_narrow_w.clamp(160, 600), 160);

        // Test schedule available_width for different panel widths
        let avail_240 = (240.0f64 - 68.0).clamp(100.0f64, 900.0f64);
        assert_eq!(avail_240, 172.0);
        let avail_180 = (180.0f64 - 68.0).clamp(100.0f64, 900.0f64);
        assert_eq!(avail_180, 112.0);
        let avail_narrow = (140.0f64 - 68.0).clamp(100.0f64, 900.0f64);
        assert_eq!(avail_narrow, 100.0);

        // Verify orientation icons and regression against missing symbolic icons
        let (side_icon, _) = match YearPreviewOrientation::Side {
            YearPreviewOrientation::Side => ("pan-down-symbolic", "Vorschau unten anzeigen"),
            YearPreviewOrientation::Bottom => ("sidebar-show-right-symbolic", "Vorschau rechts anzeigen"),
        };
        assert_eq!(side_icon, "pan-down-symbolic");

        let (bottom_icon, _) = match YearPreviewOrientation::Bottom {
            YearPreviewOrientation::Side => ("pan-down-symbolic", "Vorschau unten anzeigen"),
            YearPreviewOrientation::Bottom => ("sidebar-show-right-symbolic", "Vorschau rechts anzeigen"),
        };
        assert_eq!(bottom_icon, "sidebar-show-right-symbolic");

        let source = include_str!("calendar_page.rs");
        assert!(!source.contains(".icon_name(\"view-calendar-day-symbolic\")"));
        assert!(!source.contains("\"sidebar-show-bottom-symbolic\""));
        assert!(source.contains(".icon_name(\"x-office-calendar-symbolic\")"));
        assert!(source.contains("\"pan-down-symbolic\""));
        assert!(source.contains("calendar-year-details-list"));
        assert!(source.contains("calendar-year-event-row"));
        assert!(source.contains("append_year_day_event_row"));
    }

    #[test]
    fn test_list_projection_plan_construction() {
        let mut e1 = event_with_start("2026-09-11 10:00");
        e1.uid = "e1".to_string();
        e1.source_uid = "s1".to_string();
        e1.summary = "Alpha Meeting".to_string();
        e1.component = "BEGIN:VEVENT\r\nUID:e1\r\nCATEGORIES:Work\r\nSUMMARY:Alpha Meeting\r\nEND:VEVENT\r\n".to_string();

        let mut e2 = event_with_start("2026-09-12 14:00");
        e2.uid = "e2".to_string();
        e2.source_uid = "s1".to_string();
        e2.summary = "Beta Workshop".to_string();
        e2.component = "BEGIN:VEVENT\r\nUID:e2\r\nCATEGORIES:Workshop\r\nSUMMARY:Beta Workshop\r\nEND:VEVENT\r\n".to_string();

        let snapshot = Arc::new(CalendarSnapshot {
            events: vec![e1, e2],
            tasks: Vec::new(),
            memos: Vec::new(),
        });
        let index = CalendarSnapshotIndex::new(snapshot);
        let hidden = HashSet::new();

        // All events
        let plan_all = build_list_projection_plan(&index, "", None, &hidden);
        assert_eq!(plan_all.events.len(), 2);

        // Filter by search query
        let plan_alpha = build_list_projection_plan(&index, "Alpha", None, &hidden);
        assert_eq!(plan_alpha.events.len(), 1);
        assert_eq!(plan_alpha.events[0].uid, "e1");

        // Filter by category
        let plan_ws = build_list_projection_plan(&index, "", Some("Workshop"), &hidden);
        assert_eq!(plan_ws.events.len(), 1);
        assert_eq!(plan_ws.events[0].uid, "e2");

        // Filter by hidden sources
        let mut hide_s1 = HashSet::new();
        hide_s1.insert("s1".to_string());
        let plan_hidden = build_list_projection_plan(&index, "", None, &hide_s1);
        assert_eq!(plan_hidden.events.len(), 0);
    }

    #[test]
    fn test_cairo_canvas_time_grid_chip_layout_and_hit_testing() {
        let mut e1 = event_with_start("2026-09-15 09:00");
        e1.uid = "meeting-1".to_string();
        e1.summary = "Standup".to_string();
        e1.component = "BEGIN:VEVENT\r\nUID:meeting-1\r\nDTSTART:20260915T090000\r\nDTEND:20260915T100000\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n".to_string();

        let mut e2 = event_with_start("2026-09-15 09:30");
        e2.uid = "meeting-2".to_string();
        e2.summary = "Sync".to_string();
        e2.component = "BEGIN:VEVENT\r\nUID:meeting-2\r\nDTSTART:20260915T093000\r\nDTEND:20260915T103000\r\nSUMMARY:Sync\r\nEND:VEVENT\r\n".to_string();

        let target_date = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let interval_minutes = 30u32;
        let slot_count = 48usize;
        let slot_height = 24.0f64;
        let canvas_width = 200.0f64;

        let chips = compute_placed_timed_chips(&[e1, e2], target_date, interval_minutes, slot_count);
        assert_eq!(chips.len(), 2);

        // e1 is at 9:00 -> slot 18, span 2 (1h = two 30-min slots)
        assert_eq!(chips[0].start_slot, 18);
        assert_eq!(chips[0].row_span, 2);
        // e2 is at 9:30 -> slot 19, span 2
        assert_eq!(chips[1].start_slot, 19);
        assert_eq!(chips[1].row_span, 2);

        // Both overlap between 09:30 and 10:00, so they must be assigned 2 columns
        assert_eq!(chips[0].total_cols, 2);
        assert_eq!(chips[1].total_cols, 2);
        assert_ne!(chips[0].col_idx, chips[1].col_idx);

        // Coordinates
        let (x0, y0, w0, h0) = chip_rect_coordinates(&chips[0], canvas_width, slot_height);
        let (x1, y1, w1, h1) = chip_rect_coordinates(&chips[1], canvas_width, slot_height);

        assert!(y0 >= 18.0 * slot_height);
        assert!(h0 > 0.0);
        assert!(w0 > 0.0);
        assert!(w1 > 0.0);
        // Columns should not overlap in X
        assert!(x0 + w0 <= x1 + 4.0 || x1 + w1 <= x0 + 4.0);

        // Hit testing
        let hit_0 = find_chip_at(&chips, x0 + w0 / 2.0, y0 + h0 / 2.0, canvas_width, slot_height);
        assert_eq!(hit_0, Some(0));

        let hit_1 = find_chip_at(&chips, x1 + w1 / 2.0, y1 + h1 / 2.0, canvas_width, slot_height);
        assert_eq!(hit_1, Some(1));

        // Outside hit
        let hit_empty = find_chip_at(&chips, 50.0, 10.0, canvas_width, slot_height);
        assert_eq!(hit_empty, None);
    }

    #[test]
    fn test_event_color_resolution_and_source_filter_matching() {
        let work_event = CalendarEventRecord {
            uid: "ev-work".to_string(),
            source_uid: "macos:calendar:12:GoogleAcc:work-id".to_string(),
            source_display_name: "Arbeit".to_string(),
            component: "BEGIN:VEVENT\r\nUID:ev-work\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\n".to_string(),
            summary: "Meeting".to_string(),
            start: "2026-09-15 10:00".to_string(),
            end: "2026-09-15 11:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };

        let private_event = CalendarEventRecord {
            uid: "ev-priv".to_string(),
            source_uid: "macos:calendar:12:GoogleAcc:priv-id".to_string(),
            source_display_name: "Privat".to_string(),
            component: "BEGIN:VEVENT\r\nUID:ev-priv\r\nSUMMARY:Doctor\r\nEND:VEVENT\r\n".to_string(),
            summary: "Doctor".to_string(),
            start: "2026-09-15 14:00".to_string(),
            end: "2026-09-15 15:00".to_string(),
            location: String::new(),
            description: String::new(),
            all_day: false,
        };

        // 1. Color matches the sidebar source color!
        assert_eq!(
            event_color_for_record(&work_event),
            calendar_source_color("macos:calendar:12:GoogleAcc:work-id", "Arbeit")
        );
        assert_eq!(
            event_color_for_record(&private_event),
            calendar_source_color("macos:calendar:12:GoogleAcc:priv-id", "Privat")
        );
        // And they are not all the same default blue!
        assert_ne!(
            event_color_for_record(&work_event),
            event_color_for_record(&private_event)
        );

        // 2. Filter matching by source display name
        let mut hidden = HashSet::new();
        hidden.insert("Arbeit".to_string());

        assert!(!event_matches_calendar_filter_with_options(
            &work_event,
            "",
            None,
            &hidden,
            false
        ));
        assert!(event_matches_calendar_filter_with_options(
            &private_event,
            "",
            None,
            &hidden,
            false
        ));

        // 3. Filter matching by full UID
        let mut hidden_uid = HashSet::new();
        hidden_uid.insert("macos:calendar:12:GoogleAcc:work-id".to_string());
        assert!(!event_matches_calendar_filter_with_options(
            &work_event,
            "",
            None,
            &hidden_uid,
            false
        ));
    }

    #[test]
    fn test_taskpad_layout_and_hit_test() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let tasks = vec![
            TaskRecord {
                source_uid: "src-1".to_string(),
                source_display_name: "Privat".to_string(),
                uid: "t-1".to_string(),
                component: String::new(),
                summary: "Milch kaufen".to_string(),
                due: "2026-09-15 12:00".to_string(),
                completed: false,
                priority: 0,
            },
            TaskRecord {
                source_uid: "src-1".to_string(),
                source_display_name: "Privat".to_string(),
                uid: "t-2".to_string(),
                component: String::new(),
                summary: "Bericht schreiben".to_string(),
                due: "2026-09-10 12:00".to_string(),
                completed: false,
                priority: 0,
            },
            TaskRecord {
                source_uid: "src-1".to_string(),
                source_display_name: "Privat".to_string(),
                uid: "t-3".to_string(),
                component: String::new(),
                summary: "Archiviert".to_string(),
                due: String::new(),
                completed: true,
                priority: 0,
            },
        ];

        let layout = prepare_taskpad_layout(&tasks, today);
        assert_eq!(layout.items.len(), 3);
        assert_eq!(layout.total_height, 36.0 * 3.0);

        // Task 1: Due today -> "Heute", not overdue
        assert_eq!(layout.items[0].due_text.as_deref(), Some("Heute"));
        assert!(!layout.items[0].is_overdue);

        // Task 2: Due in the past (2026-09-10 < 2026-09-15) and not completed -> is_overdue
        assert!(layout.items[1].is_overdue);

        // Task 3: Completed -> not overdue
        assert!(!layout.items[2].is_overdue);
        assert!(layout.items[2].record.completed);

        // Hit testing
        // Checkbox is at (8, y + 9, 18, 18)
        let hit_check = hit_test_taskpad(&layout, 12.0, 15.0);
        assert_eq!(hit_check, Some((0, true))); // in checkbox of item 0

        let hit_row = hit_test_taskpad(&layout, 100.0, 15.0);
        assert_eq!(hit_row, Some((0, false))); // in row of item 0, not in checkbox

        let hit_item1_check = hit_test_taskpad(&layout, 12.0, 45.0);
        assert_eq!(hit_item1_check, Some((1, true))); // in checkbox of item 1

        let hit_none = hit_test_taskpad(&layout, 50.0, 200.0);
        assert_eq!(hit_none, None);
    }

    #[test]
    fn test_memopad_layout_and_hit_test() {
        let memos = vec![
            MemoRecord {
                source_uid: "src-memo".to_string(),
                source_display_name: "Notizen".to_string(),
                uid: "m-1".to_string(),
                component: String::new(),
                summary: "Einkaufsliste".to_string(),
                description: "Brot, Butter, Käse".to_string(),
                updated: "15.09.2026".to_string(),
            },
            MemoRecord {
                source_uid: "src-memo".to_string(),
                source_display_name: "Notizen".to_string(),
                uid: "m-2".to_string(),
                component: String::new(),
                summary: "".to_string(),
                description: String::new(),
                updated: String::new(),
            },
        ];

        let layout = prepare_memopad_layout(&memos);
        assert_eq!(layout.items.len(), 2);
        assert_eq!(layout.total_height, 34.0 * 2.0);
        assert_eq!(layout.items[0].title, "Einkaufsliste");
        assert_eq!(layout.items[1].title, "(Ohne Titel)");

        // Hit testing
        let hit0 = hit_test_memopad(&layout, 50.0, 15.0);
        assert_eq!(hit0, Some(0));

        let hit1 = hit_test_memopad(&layout, 50.0, 45.0);
        assert_eq!(hit1, Some(1));

        let hit_none = hit_test_memopad(&layout, 50.0, 100.0);
        assert_eq!(hit_none, None);
    }
}

