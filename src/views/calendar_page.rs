// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPage — Calendar view with Day/Week/Month navigation, event creation, and task management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc, Weekday};
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering as CompareOrdering;
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
    CalendarDataOrigin, ECalComponentType, ECalMutationResult, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds,
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
    Year,
    List,
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

    pub(crate) fn navigate(&mut self, direction: i32, first_weekday: Weekday) {
        if self.view == CalendarView::List {
            return;
        }
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
struct YearDayProjection {
    date: NaiveDate,
    event_count: usize,
}

#[derive(Debug, Clone)]
struct YearMonthProjection {
    month: u32,
    cells: Vec<Option<YearDayProjection>>,
}

#[derive(Debug, Clone)]
struct YearProjectionPlan {
    year: i32,
    selected_day: NaiveDate,
    today: NaiveDate,
    months: Vec<YearMonthProjection>,
    selected_events: Vec<CalendarEventRecord>,
}

#[derive(Debug, Clone)]
struct ListProjectionPlan {
    events: Vec<CalendarEventRecord>,
}

#[derive(Debug, Clone)]
enum CalendarProjectionPlan {
    Year(YearProjectionPlan),
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

fn build_year_projection_plan(
    index: &CalendarSnapshotIndex,
    year: i32,
    selected_day: NaiveDate,
    today: NaiveDate,
    first_weekday: Weekday,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> Result<YearProjectionPlan, String> {
    let year_range = calendar_year_range_option(year).ok_or_else(|| {
        "Das ausgewählte Kalenderjahr liegt außerhalb des unterstützten Datumsbereichs".to_string()
    })?;
    let markers = indexed_event_day_markers(
        index,
        year_range,
        search_query,
        category_filter,
        hidden_sources,
    );
    let selected_events = indexed_events_by_date(
        index,
        CalendarDateRange::single(selected_day),
        search_query,
        category_filter,
        hidden_sources,
    )
    .remove(&selected_day)
    .unwrap_or_default();

    let mut months = Vec::with_capacity(YEAR_MONTH_COUNT as usize);
    for month in 1..=YEAR_MONTH_COUNT {
        let Some(month_start) = NaiveDate::from_ymd_opt(year, month, 1) else {
            continue;
        };
        let grid_start = week_start_for(month_start, first_weekday);
        let cells = (0..MONTH_GRID_CELLS)
            .filter_map(|offset| {
                let date = grid_start.checked_add_signed(Duration::days(offset))?;
                if date.month() != month {
                    return Some(None);
                }
                Some(Some(YearDayProjection {
                    date,
                    event_count: markers.get(&date).copied().unwrap_or(0),
                }))
            })
            .collect::<Vec<_>>();
        months.push(YearMonthProjection { month, cells });
    }

    Ok(YearProjectionPlan {
        year,
        selected_day,
        today,
        months,
        selected_events,
    })
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
    raw_calendar_view_setting()
        .or_else(|| settings.map(|settings| settings.calendar_view()))
        .unwrap_or_else(|| "month".to_string())
}

fn persist_calendar_view(view: CalendarView, settings: Option<&AppSettings>) {
    let value = calendar_view_name(view);
    if !matches!(view, CalendarView::Year | CalendarView::List) {
        if let Some(settings) = settings {
            settings.set_calendar_view(value);
            return;
        }
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
        pub syncing_calendar_view: Cell<bool>,
        pub month_grid: RefCell<Option<gtk::Grid>>,
        pub month_date_label: RefCell<Option<gtk::Label>>,
        pub day_view: RefCell<Option<gtk::Box>>,
        pub week_view: RefCell<Option<gtk::Box>>,
        pub workweek_view: RefCell<Option<gtk::Box>>,
        pub agenda_view: RefCell<Option<gtk::Box>>,
        pub year_view: RefCell<Option<gtk::Box>>,
        pub list_view: RefCell<Option<gtk::Box>>,
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
                syncing_calendar_view: Cell::new(false),
                month_grid: RefCell::new(None),
                month_date_label: RefCell::new(None),
                day_view: RefCell::new(None),
                week_view: RefCell::new(None),
                workweek_view: RefCell::new(None),
                agenda_view: RefCell::new(None),
                year_view: RefCell::new(None),
                list_view: RefCell::new(None),
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
                calendar_load_error: RefCell::new(None),
                calendar_snapshot_index: RefCell::new(None),
                calendar_revision: Cell::new(0),
                calendar_monitor_generation: Cell::new(0),
                calendar_projection_cancel: RefCell::new(None),
                calendar_projection_gate: RefCell::new(CalendarProjectionGate::default()),
                calendar_mutation_gate: RefCell::new(CalendarMutationGate::default()),
                calendar_list_projection_count: Cell::new(None),
                calendar_reload_debounce: RefCell::new(None),
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
        let Some(event) = unique_event_for_legacy_identity(&records.borrow(), source_uid, uid)
        else {
            return false;
        };
        open_event_editor_for_widget(self, &event);
        true
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

    fn start_calendar_snapshot_load(&self) {
        self.cancel_calendar_snapshot_load();
        self.cancel_calendar_projection();
        self.invalidate_calendar_mutation();

        let generation = self.imp().calendar_load_gate.borrow_mut().begin();
        self.bump_calendar_revision();
        self.imp().view_state.borrow_mut().load_generation = generation;
        self.imp().calendar_load_error.replace(None);
        if self.imp().calendar_snapshot_index.borrow().is_none() {
            let local_service = CalendarStoreService::new();
            let local_snapshot = local_service.load_snapshot();
            if !local_snapshot.events.is_empty()
                || !local_snapshot.tasks.is_empty()
                || !local_snapshot.memos.is_empty()
            {
                let snapshot_index = Arc::new(CalendarSnapshotIndex::new(Arc::new(local_snapshot)));
                self.imp()
                    .calendar_snapshot_index
                    .replace(Some(snapshot_index));
            }
        }
        self.set_calendar_loading_status(true, "Kalender werden geladen …");
        self.imp().calendar_list_projection_count.set(None);
        self.refresh_task_sidebar();
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
                        page.set_calendar_load_error("Kalender konnten nicht geladen werden");
                        page.set_calendar_loading_status(
                            false,
                            "Kalender konnten nicht geladen werden",
                        );
                        page.refresh_calendar_views();
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
        let sidebar_token = CalendarSidebarApplyToken {
            load_generation: generation,
            model_revision: self.imp().calendar_revision.get(),
        };
        if !self.finish_calendar_snapshot_load(generation, &cancellation) {
            return;
        }
        self.set_calendar_loading_status(false, "");
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

    fn report_calendar_monitor_failure(&self, error: &str) {
        self.invalidate_calendar_mutation();
        self.cancel_calendar_snapshot_load();
        self.cancel_calendar_projection();
        let message = format!("Kalender-Synchronisierung fehlgeschlagen: {error}");
        self.set_calendar_load_error(&message);
        self.set_calendar_loading_status(false, &message);
        self.refresh_task_sidebar();
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
        year: Option<i32>,
        selected_day: Option<NaiveDate>,
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
        let first_weekday = configured_first_weekday();
        let today = Local::now().date_naive();
        let page = self.downgrade();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn({
            let cancellation = Arc::clone(&cancellation);
            move || {
                if cancellation.load(Ordering::Acquire) {
                    return;
                }
                let result = std::panic::catch_unwind(|| match view {
                    CalendarView::Year => {
                        let year =
                            year.ok_or_else(|| "Jahresansicht ist nicht verfügbar".to_string())?;
                        let selected_day = selected_day.ok_or_else(|| {
                            "Kein ausgewählter Tag für die Jahresansicht".to_string()
                        })?;
                        build_year_projection_plan(
                            &index,
                            year,
                            selected_day,
                            today,
                            first_weekday,
                            &search_query,
                            category_filter.as_deref(),
                            &hidden_sources,
                        )
                        .map(CalendarProjectionPlan::Year)
                    }
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
            CalendarProjectionPlan::Year(plan) => self.apply_year_projection_plan(load, plan),
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
                            if let Some(page) = page_for_monitor.upgrade()
                                && page.calendar_monitor_is_current(monitor_generation)
                            {
                                page.report_calendar_monitor_failure(&error.to_string());
                            }
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
                            page.report_calendar_monitor_failure(&error);
                            break;
                        }
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
                    let hidden_sources_vec = {
                        let mut hidden_sources = page.imp().hidden_calendar_sources.borrow_mut();
                        if check.is_active() {
                            hidden_sources.remove(&source_uid_for_toggle);
                        } else {
                            hidden_sources.insert(source_uid_for_toggle.clone());
                        }
                        hidden_sources.iter().cloned().collect::<Vec<_>>()
                    };
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_hidden_sources(&hidden_sources_vec);
                    }
                    page.refresh_task_sidebar();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                });
                let color = calendar_source_color(&source.uid, &source.display_name);
                let color_dot = gtk::Label::new(None);
                color_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
                color_dot.set_margin_start(4);
                color_dot.set_margin_end(2);
                row.add_prefix(&color_dot);
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
                    let hidden_sources_vec = {
                        let mut hidden_sources = page.imp().hidden_calendar_sources.borrow_mut();
                        if check.is_active() {
                            hidden_sources.remove("local-calendar");
                        } else {
                            hidden_sources.insert("local-calendar".to_string());
                        }
                        hidden_sources.iter().cloned().collect::<Vec<_>>()
                    };
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_hidden_sources(&hidden_sources_vec);
                    }
                    page.refresh_task_sidebar();
                    page.refresh_calendar_views();
                    page.refresh_event_list_visibility();
                });
                let color = calendar_source_color("local-calendar", "Lokaler Kalender");
                let color_dot = gtk::Label::new(None);
                color_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
                color_dot.set_margin_start(4);
                color_dot.set_margin_end(2);
                row.add_prefix(&color_dot);
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
        self.imp()
            .calendar_prev_button
            .replace(Some(prev_btn.clone()));
        self.imp()
            .calendar_next_button
            .replace(Some(next_btn.clone()));
        self.imp()
            .calendar_today_button
            .replace(Some(today_btn.clone()));

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

        // View Stack for all portable calendar surfaces.
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

        // 7. List View: one stable, filtered row per loaded component.
        let list_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .spacing(8)
            .build();
        self.imp().list_view.replace(Some(list_view.clone()));
        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&list_view)
            .build();
        calendar_stack.add_titled(&list_scroll, Some("list"), "Liste");

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
            persist_calendar_view(view, Some(settings));
        } else {
            persist_calendar_view(view, None);
        }
        self.update_calendar_navigation_controls();
        self.refresh_calendar_views();
    }

    fn sync_calendar_view_from_settings(&self) {
        let view = self
            .imp()
            .settings
            .borrow()
            .as_ref()
            .map(|settings| calendar_view_from_name(&persisted_calendar_view_name(Some(settings))))
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
                CalendarView::Year,
                CalendarView::List,
            ]) {
                button.set_active(candidate == view);
            }
        }
        self.imp().syncing_calendar_view.set(false);
        self.update_calendar_navigation_controls();
        self.refresh_calendar_views();
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

    fn shift_period(&self, direction: i32) {
        if *self.imp().current_view.borrow() == CalendarView::List {
            return;
        }
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
        if *self.imp().current_view.borrow() == CalendarView::Year {
            self.imp().view_state.borrow_mut().select_civil_day(date);
        }
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
        self.refresh_task_sidebar();
        self.refresh_calendar_category_model();
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
        clear_box(&view);
        if self.append_calendar_state_to_box(&view) {
            return;
        }
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
                list.append(&calendar_event_row(self, &event));
            }
        }
        view.append(&list);
    }

    fn refresh_week_view(&self) {
        let Some(view) = self.imp().week_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        if self.append_calendar_state_to_box(&view) {
            return;
        }
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
                    list.append(&calendar_event_row(self, &event));
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
        if self.append_calendar_state_to_box(&view) {
            return;
        }

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
                    all_day_cells[day_index].append(&calendar_event_row(self, &event));
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
                    &event,
                    &format!("{} · {}", event_time_text(&event), title),
                ));
                event_button.set_child(Some(&event_label));
                set_accessible_label(&event_button, &format!("Termin öffnen: {title}"));
                let event_identity_for_click = CalendarEventIdentity::from_event(&event);
                let page_weak_for_click = self.downgrade();
                event_button.connect_clicked(move |button| {
                    let (Some(page), Some(identity)) = (
                        page_weak_for_click.upgrade(),
                        event_identity_for_click.as_ref(),
                    ) else {
                        return;
                    };
                    page.open_event_editor_for_identity(button, identity);
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
                    list.append(&calendar_event_row(self, &event));
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

    fn refresh_year_view(&self) {
        let Some(view) = self.imp().year_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);

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

        if self.imp().calendar_snapshot_index.borrow().is_none() {
            view.append(&empty_period_row(
                "Keine Kalenderdaten bestätigt",
                "Die Jahresansicht wartet auf einen bestätigten Snapshot.",
            ));
            return;
        }
        view.append(&empty_period_row(
            "Jahresansicht wird vorbereitet …",
            "Die Jahresprojektion wird außerhalb des GTK-Hauptlaufs berechnet.",
        ));
        self.start_calendar_projection(CalendarView::Year, Some(year), Some(selected_day));
        return;
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
        if self.imp().calendar_snapshot_index.borrow().is_none() {
            view.append(&empty_period_row(
                "Keine Kalenderdaten bestätigt",
                "Die Listenansicht wartet auf einen bestätigten Snapshot.",
            ));
            return;
        }
        view.append(&empty_period_row(
            "Liste wird vorbereitet …",
            "Die gefilterte Projektion wird außerhalb des GTK-Hauptlaufs berechnet.",
        ));
        self.start_calendar_projection(CalendarView::List, None, None);
    }

    fn apply_year_projection_plan(&self, load: CalendarProjectionLoad, plan: YearProjectionPlan) {
        let Some(view) = self.imp().year_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.calendar_projection_is_current(&load) {
            return;
        }
        clear_box(&view);
        let year_grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .row_homogeneous(true)
            .column_spacing(10)
            .row_spacing(10)
            .hexpand(true)
            .build();
        view.append(&year_grid);
        let weekday_names = configured_weekday_names();
        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            for (month_index, month) in plan.months.iter().enumerate() {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                if !page.calendar_projection_is_current(&load) {
                    return;
                }
                let month_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(4)
                    .margin_start(4)
                    .margin_end(4)
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();
                month_box.add_css_class("card");
                month_box.append(
                    &gtk::Label::builder()
                        .label(Self::month_name(month.month))
                        .css_classes(vec!["title-4"])
                        .halign(gtk::Align::Center)
                        .build(),
                );
                let month_grid = gtk::Grid::builder()
                    .column_homogeneous(true)
                    .row_spacing(1)
                    .column_spacing(1)
                    .hexpand(true)
                    .build();
                for (column, weekday) in weekday_names.iter().enumerate() {
                    month_grid.attach(
                        &gtk::Label::builder()
                            .label(*weekday)
                            .css_classes(vec!["caption", "dim-label"])
                            .halign(gtk::Align::Center)
                            .build(),
                        column as i32,
                        0,
                        1,
                        1,
                    );
                }
                for (start, end) in
                    projection_chunk_ranges(month.cells.len(), PROJECTION_APPLY_CHUNK_SIZE)
                {
                    let Some(page) = page_weak.upgrade() else {
                        return;
                    };
                    if !page.calendar_projection_is_current(&load) {
                        return;
                    }
                    for (cell_index, cell) in month.cells[start..end].iter().enumerate() {
                        let cell_index = start + cell_index;
                        let row = (cell_index / 7) as i32 + 1;
                        let column = (cell_index % 7) as i32;
                        let Some(cell) = cell else {
                            month_grid.attach(&gtk::Label::new(None), column, row, 1, 1);
                            continue;
                        };
                        let date = cell.date;
                        let event_count = cell.event_count;
                        let label = if event_count == 0 {
                            date.day().to_string()
                        } else {
                            format!("{}\n• {}", date.day(), event_count)
                        };
                        let day_button = gtk::Button::builder()
                            .label(&label)
                            .css_classes(vec!["flat", "caption"])
                            .hexpand(true)
                            .vexpand(true)
                            .build();
                        let tooltip = if event_count == 0 {
                            format_date(date)
                        } else if event_count == 1 {
                            format!("{} · 1 Termin", format_date(date))
                        } else {
                            format!("{} · {} Termine", format_date(date), event_count)
                        };
                        day_button.set_tooltip_text(Some(&tooltip));
                        if event_count > 0 {
                            day_button.add_css_class("calendar-event-day");
                        }
                        if date == plan.selected_day {
                            day_button.add_css_class("accent");
                        }
                        if date == plan.today {
                            day_button.add_css_class("title-4");
                        }
                        set_accessible_label(
                            &day_button,
                            &format!(
                                "{}{}",
                                format_date(date),
                                if event_count == 0 {
                                    String::new()
                                } else {
                                    format!("; {} Termine", event_count)
                                }
                            ),
                        );
                        let page_for_click = page_weak.clone();
                        day_button.connect_clicked(move |_| {
                            if let Some(page) = page_for_click.upgrade() {
                                page.select_calendar_day(date);
                            }
                        });
                        month_grid.attach(&day_button, column, row, 1, 1);
                    }
                    if end < month.cells.len() {
                        glib::timeout_future_with_priority(
                            glib::Priority::LOW,
                            std::time::Duration::from_millis(1),
                        )
                        .await;
                    }
                }
                month_box.append(&month_grid);
                year_grid.attach(
                    &month_box,
                    (month.month.saturating_sub(1) % 3) as i32,
                    (month.month.saturating_sub(1) / 3) as i32,
                    1,
                    1,
                );
                if month_index + 1 < plan.months.len() {
                    glib::timeout_future_with_priority(
                        glib::Priority::LOW,
                        std::time::Duration::from_millis(1),
                    )
                    .await;
                }
            }

            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !page.calendar_projection_is_current(&load) {
                return;
            }
            page.imp()
                .view_state
                .borrow_mut()
                .reconcile_event_selection(&plan.selected_events);
            view.append(
                &gtk::Label::builder()
                    .label(&format!("Termine für {}", format_date(plan.selected_day)))
                    .css_classes(vec!["title-3"])
                    .halign(gtk::Align::Start)
                    .margin_top(8)
                    .build(),
            );
            let event_list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .css_classes(vec!["boxed-list"])
                .build();
            page.connect_calendar_event_selection(&event_list);
            if plan.selected_events.is_empty() {
                event_list.append(&empty_period_row(
                    "Keine passenden Termine",
                    "Für den ausgewählten zivilen Tag sind keine gefilterten VEVENTs geladen.",
                ));
            } else {
                for (start, end) in
                    projection_chunk_ranges(plan.selected_events.len(), PROJECTION_APPLY_CHUNK_SIZE)
                {
                    let Some(page) = page_weak.upgrade() else {
                        return;
                    };
                    if !page.calendar_projection_is_current(&load) {
                        return;
                    }
                    for event in &plan.selected_events[start..end] {
                        page.append_calendar_event_action_row(&event_list, event);
                    }
                    if end < plan.selected_events.len() {
                        glib::timeout_future_with_priority(
                            glib::Priority::LOW,
                            std::time::Duration::from_millis(1),
                        )
                        .await;
                    }
                }
                page.restore_calendar_event_selection(&event_list, &plan.selected_events);
            }
            view.append(&event_list);
            if page.finish_calendar_projection(&load) {
                page.refresh_period_label();
            }
        });
    }

    fn apply_list_projection_plan(&self, load: CalendarProjectionLoad, plan: ListProjectionPlan) {
        let Some(view) = self.imp().list_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.calendar_projection_is_current(&load) {
            return;
        }
        clear_box(&view);
        let events = plan.events;
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
            view.append(&empty_period_row(
                if all_count == 0 {
                    "Keine Termine geladen"
                } else {
                    "Keine Termine entsprechen den Filtern"
                },
                "Die Listenansicht ist unabhängig vom aktuellen Datumsanker.",
            ));
            if self.finish_calendar_projection(&load) {
                self.refresh_period_label();
            }
            return;
        }
        let event_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["boxed-list"])
            .build();
        self.connect_calendar_event_selection(&event_list);
        view.append(&event_list);
        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            for (start, end) in projection_chunk_ranges(events.len(), PROJECTION_APPLY_CHUNK_SIZE) {
                let Some(page) = page_weak.upgrade() else {
                    return;
                };
                if !page.calendar_projection_is_current(&load) {
                    return;
                }
                for event in &events[start..end] {
                    page.append_calendar_event_action_row(&event_list, event);
                }
                if end < events.len() {
                    glib::timeout_future_with_priority(
                        glib::Priority::LOW,
                        std::time::Duration::from_millis(1),
                    )
                    .await;
                }
            }
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !page.calendar_projection_is_current(&load) {
                return;
            }
            page.restore_calendar_event_selection(&event_list, &events);
            if page.finish_calendar_projection(&load) {
                page.refresh_period_label();
            }
        });
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
        *self.imp().current_date.borrow_mut() = date;
        self.imp()
            .view_state
            .borrow_mut()
            .set_anchor(date, configured_first_weekday());
        self.imp().view_state.borrow_mut().select_civil_day(date);
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

        for events in grouped.values_mut() {
            events.sort_by(compare_event_date_order);
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

    fn refresh_month_grid(&self) {
        let Some(grid) = self.imp().month_grid.borrow().as_ref().cloned() else {
            return;
        };
        clear_grid(&grid);
        if self.attach_calendar_state_to_grid(&grid) {
            return;
        }
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
                event_label.set_markup(&calendar_event_markup(event, &event.summary));
                let event_button = gtk::Button::builder()
                    .halign(gtk::Align::Fill)
                    .hexpand(true)
                    .css_classes(vec!["flat", "caption"])
                    .child(&event_label)
                    .build();
                set_accessible_label(&event_button, &format!("Termin öffnen: {}", event.summary));
                let event_identity_for_click = CalendarEventIdentity::from_event(event);
                let page_weak_for_click = self.downgrade();
                event_button.connect_clicked(move |button| {
                    let (Some(page), Some(identity)) = (
                        page_weak_for_click.upgrade(),
                        event_identity_for_click.as_ref(),
                    ) else {
                        return;
                    };
                    page.open_event_editor_for_identity(button, identity);
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
        action_button: &gtk::Button,
    ) {
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
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let page_weak = page.downgrade();
    drop_target.connect_drop(move |_, value, _, _| {
        let Ok(payload) = value.get::<String>() else {
            return false;
        };
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
        let Some(page) = page_weak.upgrade() else {
            return false;
        };
        let records = page.imp().event_records.borrow().as_ref().cloned();
        let Some(event) = records.and_then(|records| {
            let target_identity =
                calendar_event_identity_from_parts(source_uid, uid, recurrence_id);
            unique_event_for_identity(&records.borrow(), &target_identity)
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
    let page_weak_for_toggle = page_weak.clone();
    check.connect_toggled(move |check| {
        let Some(page) = page_weak_for_toggle.upgrade() else {
            return;
        };
        let completed = check.is_active();
        let task = task.clone();
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

        check.set_sensitive(false);
        let check_for_result = check.clone();
        let page_weak = page.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !page.calendar_mutation_is_current(&mutation_token) {
                check_for_result.set_active(task.completed);
                check_for_result.set_sensitive(true);
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
                    check_for_result.set_active(task.completed);
                    check_for_result.set_sensitive(true);
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
                        check_for_result.set_active(task.completed);
                        check_for_result.set_sensitive(true);
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
                        check_for_result.set_active(task.completed);
                        check_for_result.set_sensitive(true);
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
                    page.finish_calendar_mutation(&mutation_token);
                    check_for_result.set_active(task.completed);
                    check_for_result.set_sensitive(true);
                    show_toast_for_widget(
                        &page,
                        &format!("Aufgabe konnte nicht geändert werden: {error}"),
                    );
                    page.reload_calendar_data();
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
    const CALENDAR_PALETTE: &[&str] = &[
        "#1a73e8", // Google Blue
        "#0d904f", // Google Green
        "#e37400", // Google Orange
        "#d93025", // Google Red
        "#8430ce", // Google Purple
        "#007b83", // Google Teal
        "#b06000", // Google Ochre
        "#188038", // Forest Green
        "#c5221f", // Crimson
        "#681da8", // Deep Purple
        "#e91e63", // Pink
        "#3f51b5", // Indigo
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
        return "#0d904f".to_string();
    }
    if key.to_lowercase().contains("geburtstag") || key.to_lowercase().contains("birthday") {
        return "#8430ce".to_string();
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
        can_export: !event.component.trim().is_empty(),
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

fn compare_optional_order<T: Ord>(left: Option<T>, right: Option<T>) -> CompareOrdering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => CompareOrdering::Less,
        (None, Some(_)) => CompareOrdering::Greater,
        (None, None) => CompareOrdering::Equal,
    }
}

fn event_matches_calendar_filter(
    event: &CalendarEventRecord,
    search_query: &str,
    category_filter: Option<&str>,
    hidden_sources: &HashSet<String>,
) -> bool {
    if hidden_sources.contains(&event.source_uid) {
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

    for events in grouped.values_mut() {
        events.sort_by(compare_event_date_order);
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
    let event_row = row.child()?.downcast::<adw::ActionRow>().ok()?;
    calendar_event_identity_from_action_row(&event_row)
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
    let export_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Termin als iCalendar exportieren")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    set_accessible_label(&export_button, "Termin als iCalendar exportieren");
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
    row.add_suffix(&export_button);
    row.add_suffix(&copy_button);
    row.add_suffix(&move_button);
    row.add_suffix(&delete_button);

    let event_identity_for_edit = CalendarEventIdentity::from_event(&event);
    let page_weak_for_edit = page.downgrade();
    let edit_button_for_context = edit_button.clone();
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

    let event_for_export = event.clone();
    let export_button_for_context = export_button.clone();
    let copy_button_for_context = copy_button.clone();
    let move_button_for_context = move_button.clone();
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

        let export = gtk::Button::with_label("Als iCalendar exportieren");
        export.set_halign(gtk::Align::Fill);
        export.add_css_class("flat");
        export.set_sensitive(!event.component.trim().is_empty());
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
        assert!(state.can_copy_to_local);
        assert!(state.can_move_to_local);

        let local = all_day_event("local-calendar", "event-2", "2026-08-10", "2026-08-11");
        let local_state = calendar_event_action_state(&local, true);
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
