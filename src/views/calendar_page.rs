// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPage — Calendar view with Day/Week/Month navigation, event creation, and task management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Weekday};
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::{
    CalendarEventRecord, CalendarStoreService, export_ical_component, update_ical_property,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::recurrence_service::{
    RecurrenceByDay, parse_extended_weekday_token, parse_ical_date, parse_recurrence_rule,
    recurrence_matches_without_count, recurrence_occurs_on as recurrence_rule_occurs_on,
};
use crate::services::settings_service::AppSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalendarView {
    Day,
    Week,
    WorkWeek,
    Month,
    Agenda,
}

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug)]
    pub struct CalendarPage {
        pub current_date: RefCell<NaiveDate>,
        pub(crate) current_view: RefCell<CalendarView>,
        pub month_grid: RefCell<Option<gtk::Grid>>,
        pub month_date_label: RefCell<Option<gtk::Label>>,
        pub day_view: RefCell<Option<gtk::Box>>,
        pub week_view: RefCell<Option<gtk::Box>>,
        pub workweek_view: RefCell<Option<gtk::Box>>,
        pub agenda_view: RefCell<Option<gtk::Box>>,
        pub event_records: RefCell<Option<Rc<RefCell<Vec<CalendarEventRecord>>>>>,
        pub event_list: RefCell<Option<gtk::ListBox>>,
        pub hidden_calendar_sources: RefCell<HashSet<String>>,
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    }

    impl Default for CalendarPage {
        fn default() -> Self {
            Self {
                current_date: RefCell::new(Local::now().date_naive()),
                current_view: RefCell::new(CalendarView::Month),
                month_grid: RefCell::new(None),
                month_date_label: RefCell::new(None),
                day_view: RefCell::new(None),
                week_view: RefCell::new(None),
                workweek_view: RefCell::new(None),
                agenda_view: RefCell::new(None),
                event_records: RefCell::new(None),
                event_list: RefCell::new(None),
                hidden_calendar_sources: RefCell::new(HashSet::new()),
                ecal_monitor_cancel: RefCell::new(None),
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
            if let Some(cancel) = self.ecal_monitor_cancel.borrow_mut().take() {
                use std::sync::atomic::Ordering;
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CalendarPage {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct CalendarPage(ObjectSubclass<imp::CalendarPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CalendarPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
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
        if let Some(settings) = AppSettings::try_new() {
            self.imp()
                .hidden_calendar_sources
                .replace(settings.calendar_hidden_sources().into_iter().collect());
        }
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(false);
        paned.set_position(280);

        // ── Sidebar (Calendar Picker + Task List) ───────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let mini_calendar = gtk::Calendar::new();
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
            .label("Kalender werden nach der ECal-Anbindung angezeigt")
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

        let local_item_state_for_load = local_item_state.clone();
        let event_list_for_load = event_list.clone();
        let event_records_for_load = Rc::clone(&event_records);
        let page_for_load = self.downgrade();
        let (local_sender, local_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = load_snapshot_prefer_eds();
            if let Err(error) = local_sender.send_blocking(result) {
                tracing::debug!(%error, "Calendar snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match local_receiver.recv().await {
                Ok(result) => {
                    let count = result.snapshot.events.len();
                    if let Some(warning) = result.warning {
                        tracing::warn!(%warning, "Calendar data loaded with a backend warning");
                    }
                    if count == 0 {
                        event_list_for_load.set_visible(false);
                        match result.origin {
                            CalendarDataOrigin::Eds => {
                                local_item_state_for_load
                                    .set_label("Keine VEVENT-Daten aus EDS gefunden");
                            }
                            CalendarDataOrigin::Local => {
                                local_item_state_for_load
                                    .set_label("Keine lokalen VEVENT-Daten gefunden");
                            }
                        }
                    } else {
                        event_list_for_load.set_visible(true);
                        let origin = match result.origin {
                            CalendarDataOrigin::Eds => "EDS",
                            CalendarDataOrigin::Local => "lokalem iCalendar",
                        };
                        local_item_state_for_load
                            .set_label(&format!("{count} Termin(e) aus {origin} geladen."));
                        for event in result.snapshot.events.into_iter().take(25) {
                            upsert_event_row(
                                &event_list_for_load,
                                Rc::clone(&event_records_for_load),
                                event,
                            );
                        }
                        if let Some(page) = page_for_load.upgrade() {
                            page.refresh_calendar_views();
                            page.refresh_event_list_visibility();
                        }
                    }
                }
                Err(error) => tracing::debug!(%error, "Calendar snapshot unavailable"),
            }
        });

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let event_list_for_monitor = event_list.clone();
            let event_records_for_monitor = Rc::clone(&event_records);
            let local_item_state_for_monitor = local_item_state.clone();
            let page_for_monitor = self.downgrade();
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
                                    "ECal signal monitor started with source warnings"
                                );
                            }
                        }
                        ECalSyncEvent::Change(change) => match change.kind {
                            ECalChangeKind::Removed => {
                                remove_event_row(
                                    &event_list_for_monitor,
                                    Rc::clone(&event_records_for_monitor),
                                    &change.source_uid,
                                    &change.uid,
                                );
                            }
                            ECalChangeKind::Added | ECalChangeKind::Modified => {
                                if let Some(ECalRecord::Event(event)) = parse_change_record(&change)
                                {
                                    upsert_event_row(
                                        &event_list_for_monitor,
                                        Rc::clone(&event_records_for_monitor),
                                        event,
                                    );
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
                            .set_label(&format!("{count} Termin(e) aus EDS synchronisiert."));
                    }
                    if let Some(page) = page_for_monitor.upgrade() {
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
                let subtitle = if source.backend_name.trim().is_empty() {
                    if source.enabled {
                        "aktiv".to_string()
                    } else {
                        "deaktiviert".to_string()
                    }
                } else {
                    format!(
                        "{} · {}",
                        source.backend_name,
                        if source.enabled {
                            "aktiv"
                        } else {
                            "deaktiviert"
                        }
                    )
                };
                let row = adw::ActionRow::builder()
                    .title(escaped_title.as_str())
                    .subtitle(&subtitle)
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
                    .map(|error| format!("EDS-Kalender sind nicht erreichbar: {error}"))
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

        let tasks: [(&str, &str); 0] = [];

        for (title, due) in tasks {
            let row = adw::ActionRow::builder().title(title).subtitle(due).build();

            let check = gtk::CheckButton::builder()
                .valign(gtk::Align::Center)
                .build();
            row.add_prefix(&check);

            task_list.append(&row);
        }

        sidebar_box.append(&task_list);

        let task_state = gtk::Label::builder()
            .label("Aufgaben werden nach der ECal-Anbindung angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .halign(gtk::Align::Start)
            .build();
        sidebar_box.append(&task_state);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar_box)
            .vexpand(true)
            .build();
        paned.set_start_child(Some(&sidebar_scroll));

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

        let today_btn = gtk::Button::builder()
            .label("Heute")
            .css_classes(vec!["flat"])
            .tooltip_text("Zu heute wechseln")
            .build();

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .css_classes(vec!["flat"])
            .tooltip_text("Nächster Zeitraum")
            .build();

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
                let dialog = crate::dialogs::EventDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                dialog.present();
            }
        ));

        // View mode toggle
        let view_toggle = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .build();

        let initial_view = AppSettings::try_new()
            .map(|settings| settings.calendar_view())
            .unwrap_or_else(|| "month".to_string());
        let initial_calendar_view = match initial_view.as_str() {
            "day" => CalendarView::Day,
            "week" => CalendarView::Week,
            "workweek" => CalendarView::WorkWeek,
            "agenda" => CalendarView::Agenda,
            _ => CalendarView::Month,
        };
        *self.imp().current_view.borrow_mut() = initial_calendar_view;

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

        // View toggle handler
        day_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("day");
                    *page.imp().current_view.borrow_mut() = CalendarView::Day;
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_view("day");
                    }
                    page.refresh_calendar_views();
                }
            }
        ));
        week_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("week");
                    *page.imp().current_view.borrow_mut() = CalendarView::Week;
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_view("week");
                    }
                    page.refresh_calendar_views();
                }
            }
        ));
        workweek_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("workweek");
                    *page.imp().current_view.borrow_mut() = CalendarView::WorkWeek;
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_view("workweek");
                    }
                    page.refresh_calendar_views();
                }
            }
        ));
        month_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("month");
                    *page.imp().current_view.borrow_mut() = CalendarView::Month;
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_view("month");
                    }
                    page.refresh_calendar_views();
                }
            }
        ));
        agenda_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            #[weak(rename_to = page)]
            self,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("agenda");
                    *page.imp().current_view.borrow_mut() = CalendarView::Agenda;
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_calendar_view("agenda");
                    }
                    page.refresh_calendar_views();
                }
            }
        ));

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
                *page.imp().current_date.borrow_mut() = Local::now().date_naive();
                page.refresh_calendar_views();
            }
        ));

        calendar_stack.set_vexpand(true);
        content_box.set_vexpand(true);
        content_box.append(&calendar_stack);

        self.refresh_calendar_views();

        paned.set_end_child(Some(&content_box));
        paned.set_parent(self);
    }

    fn shift_period(&self, direction: i32) {
        let current = *self.imp().current_date.borrow();
        let view = *self.imp().current_view.borrow();
        let next = match view {
            CalendarView::Day => current + Duration::days(i64::from(direction)),
            CalendarView::Week => current + Duration::days(i64::from(direction) * 7),
            CalendarView::WorkWeek => current + Duration::days(i64::from(direction) * 7),
            CalendarView::Month => shift_month(current, direction),
            CalendarView::Agenda => current + Duration::days(i64::from(direction) * 30),
        };
        *self.imp().current_date.borrow_mut() = next;
    }

    fn refresh_calendar_views(&self) {
        self.refresh_month_grid();
        self.refresh_day_view();
        self.refresh_week_view();
        self.refresh_workweek_view();
        self.refresh_agenda_view();
        self.refresh_period_label();
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
            let source_uid = unsafe {
                row.data::<String>("event-source-uid")
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default()
            };
            let visible = self.calendar_source_is_visible(&source_uid);
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
        let date = *self.imp().current_date.borrow();
        let view = *self.imp().current_view.borrow();
        let text = match view {
            CalendarView::Day => format!(
                "{}, {}. {} {}",
                weekday_name(date),
                date.day(),
                Self::month_name(date.month()),
                date.year()
            ),
            CalendarView::Week => {
                let start = configured_week_start(date);
                let end = start + Duration::days(6);
                if start.month() == end.month() {
                    format!(
                        "{}.\u{2013}{}. {} {}",
                        start.day(),
                        end.day(),
                        Self::month_name(start.month()),
                        start.year()
                    )
                } else {
                    format!(
                        "{}. {}\u{2013}{}. {} {}",
                        start.day(),
                        Self::month_name(start.month()),
                        end.day(),
                        Self::month_name(end.month()),
                        end.year()
                    )
                }
            }
            CalendarView::WorkWeek => {
                let start = workweek_start(date);
                let end = start + Duration::days(4);
                if start.month() == end.month() {
                    format!(
                        "Arbeitswoche {}.\u{2013}{}. {} {}",
                        start.day(),
                        end.day(),
                        Self::month_name(start.month()),
                        start.year()
                    )
                } else {
                    format!(
                        "Arbeitswoche {}. {}\u{2013}{}. {} {}",
                        start.day(),
                        Self::month_name(start.month()),
                        end.day(),
                        Self::month_name(end.month()),
                        end.year()
                    )
                }
            }
            CalendarView::Month => {
                format!("{} {}", Self::month_name(date.month()), date.year())
            }
            CalendarView::Agenda => {
                format!(
                    "Agenda ab {}. {} {}",
                    date.day(),
                    Self::month_name(date.month()),
                    date.year()
                )
            }
        };
        label.set_label(&text);
    }

    fn refresh_day_view(&self) {
        let Some(view) = self.imp().day_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);
        let date = *self.imp().current_date.borrow();
        let heading = gtk::Label::builder()
            .label(&format!(
                "{}, {}. {} {}",
                weekday_name(date),
                date.day(),
                Self::month_name(date.month()),
                date.year()
            ))
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
        let mut events = self.events_for_date(date);
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
        let date = *self.imp().current_date.borrow();
        let week_start = configured_week_start(date);
        let week_heading = gtk::Label::builder()
            .label(&format!(
                "Termine dieser Woche · Arbeitszeit: {}",
                configured_workday_text()
            ))
            .css_classes(vec!["title-3"])
            .halign(gtk::Align::Start)
            .build();
        view.append(&week_heading);

        for offset in 0..7_i64 {
            let day = week_start + Duration::days(offset);
            let day_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .build();
            day_box.add_css_class("card");
            let heading = gtk::Label::builder()
                .label(&format!(
                    "{}, {}. {}",
                    weekday_name(day),
                    day.day(),
                    Self::month_name(day.month())
                ))
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
            let mut events = self.events_for_date(day);
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

        let current_date = *self.imp().current_date.borrow();
        let start_date = workweek_start(current_date);
        let workdays = configured_workdays();
        let days = workdays
            .iter()
            .map(|weekday| {
                let offset = (i64::from(weekday.num_days_from_monday())
                    - i64::from(start_date.weekday().num_days_from_monday()))
                .rem_euclid(7);
                start_date + Duration::days(offset)
            })
            .collect::<Vec<_>>();
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
            let mut events = self.events_for_date(*day);
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
                let event_label = gtk::Label::builder()
                    .halign(gtk::Align::Fill)
                    .valign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .margin_start(4)
                    .margin_end(4)
                    .margin_top(3)
                    .margin_bottom(3)
                    .css_classes(vec!["card"])
                    .build();
                event_label.set_markup(&calendar_event_markup(
                    &event.component,
                    &format!("{} · {}", event_time_text(&event), title),
                ));
                install_event_drag_source(&event_label, &event, *day);
                cells[day_index][slot as usize].append(&event_label);
            }
        }

        view.append(&grid);
    }

    fn refresh_agenda_view(&self) {
        let Some(view) = self.imp().agenda_view.borrow().as_ref().cloned() else {
            return;
        };
        clear_box(&view);

        let start_date = *self.imp().current_date.borrow();
        let end_date = start_date + Duration::days(90);
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

        let mut has_events = false;
        let mut date = start_date;
        while date <= end_date {
            let mut events = self.events_for_date(date);
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
            date += Duration::days(1);
        }

        if !has_events {
            view.append(&empty_period_row(
                "Keine kommenden Termine",
                "Für die nächsten 90 Tage sind keine VEVENTs geladen.",
            ));
        }
    }

    fn events_for_date(&self, date: NaiveDate) -> Vec<CalendarEventRecord> {
        self.imp()
            .event_records
            .borrow()
            .as_ref()
            .map(|records| {
                records
                    .borrow()
                    .iter()
                    .filter(|event| {
                        self.calendar_source_is_visible(&event.source_uid)
                            && event_occurs_on(event, date)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn calendar_source_is_visible(&self, source_uid: &str) -> bool {
        !self
            .imp()
            .hidden_calendar_sources
            .borrow()
            .contains(source_uid)
    }

    fn visible_event_records(&self) -> Vec<CalendarEventRecord> {
        self.imp()
            .event_records
            .borrow()
            .as_ref()
            .map(|records| {
                records
                    .borrow()
                    .iter()
                    .filter(|event| self.calendar_source_is_visible(&event.source_uid))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refresh_month_grid(&self) {
        let Some(grid) = self.imp().month_grid.borrow().as_ref().cloned() else {
            return;
        };
        let current_date = *self.imp().current_date.borrow();
        let year = current_date.year();
        let month = current_date.month();
        let Some(month_start) = NaiveDate::from_ymd_opt(year, month, 1) else {
            return;
        };
        let next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        };
        let Some(next_month) = next_month else {
            return;
        };
        let days_in_month = (next_month - Duration::days(1)).day();
        let first_weekday = configured_weekday_offset(month_start);
        let today = Local::now().date_naive();
        let show_week_numbers = AppSettings::try_new()
            .map(|settings| settings.calendar_show_week_numbers())
            .unwrap_or(false);
        let column_offset = if show_week_numbers { 1 } else { 0 };
        let events = self.visible_event_records();

        while let Some(child) = grid.first_child() {
            grid.remove(&child);
        }

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

        for cell_index in 0..42_i32 {
            let row = cell_index / 7 + 1;
            let column = cell_index % 7;
            let day = cell_index - first_weekday + 1;
            let cell_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_start(4)
                .margin_end(4)
                .margin_top(4)
                .margin_bottom(4)
                .build();

            let cell_frame = gtk::Frame::builder().child(&cell_box).build();
            if !(1..=days_in_month as i32).contains(&day) {
                cell_frame.add_css_class("dim-label");
                grid.attach(&cell_frame, column + column_offset, row, 1, 1);
                continue;
            }

            let date = month_start + Duration::days(i64::from(day - 1));
            install_day_drop_target(&cell_frame, self, date);
            let day_label = gtk::Label::builder()
                .label(&day.to_string())
                .halign(gtk::Align::Start)
                .css_classes(vec!["caption"])
                .build();
            if date == today {
                day_label.add_css_class("accent");
                day_label.add_css_class("title-4");
            }
            cell_box.append(&day_label);

            let day_events = events
                .iter()
                .filter(|event| event_occurs_on(event, date))
                .collect::<Vec<_>>();
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
                let event_for_click = (*event).clone();
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
                let week_date =
                    month_start + Duration::days(i64::from(row * 7) - i64::from(first_weekday));
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
        if event.source_uid.trim().is_empty() {
            show_toast_for_widget(
                self,
                "Dieser Termin stammt aus einer schreibgeschützten importierten Quelle",
            );
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
                    let component = result.component.unwrap_or_else(|| {
                        shift_event_component(&event.component, delta_days)
                            .unwrap_or_else(|_| event.component.clone())
                    });
                    let mut updated_event = event.clone();
                    updated_event.component = component.clone();
                    updated_event.start = display_ical_datetime(
                        &ical_property(&component, "DTSTART").unwrap_or_default(),
                    );
                    updated_event.end = ical_property(&component, "DTEND")
                        .map(|value| display_ical_datetime(&value))
                        .unwrap_or_default();
                    let records = page.imp().event_records.borrow().as_ref().cloned();
                    if let Some(records) = records {
                        let mut records = records.borrow_mut();
                        if let Some(existing) = records.iter_mut().find(|existing| {
                            existing.source_uid == updated_event.source_uid
                                && existing.uid == updated_event.uid
                        }) {
                            *existing = updated_event;
                        }
                    }
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
    updated = shift_rrule_until(&updated, delta_days)?;
    Ok(updated)
}

fn shift_ical_date_list_property(
    raw: &str,
    property_name: &str,
    delta_days: i64,
) -> Result<String, String> {
    let Some(property_prefix) = ical_property_prefix(raw, property_name) else {
        return Ok(raw.to_string());
    };
    let shifted_values = ical_properties(raw, property_name)
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| {
            shift_ical_datetime_value(value, delta_days).ok_or_else(|| {
                format!("{property_name} konnte beim Verschieben nicht ausgewertet werden")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(update_ical_property(
        raw,
        &property_prefix,
        Some(&shifted_values.join(",")),
    ))
}

fn shift_rrule_until(raw: &str, delta_days: i64) -> Result<String, String> {
    let Some(rule) = ical_property(raw, "RRULE") else {
        return Ok(raw.to_string());
    };
    let mut changed = false;
    let shifted_rule = rule
        .split(';')
        .map(|part| {
            let Some((key, value)) = part.split_once('=') else {
                return Ok(part.to_string());
            };
            if !key.eq_ignore_ascii_case("UNTIL") {
                return Ok(part.to_string());
            }
            let shifted = shift_ical_datetime_value(value, delta_days)
                .ok_or_else(|| "RRULE-UNTIL konnte beim Verschieben nicht ausgewertet werden")?;
            changed = true;
            Ok(format!("{key}={shifted}"))
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
    let datetime = NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()?;
    let suffix = &value[15..];
    Some(format!(
        "{}{}",
        (datetime + Duration::days(delta_days)).format("%Y%m%dT%H%M%S"),
        suffix
    ))
}

fn display_ical_datetime(value: &str) -> String {
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

fn configured_week_start(date: NaiveDate) -> NaiveDate {
    let week_start = configured_first_weekday();
    let current = i64::from(date.weekday().num_days_from_monday());
    let first = i64::from(week_start.num_days_from_monday());
    date - Duration::days((current - first).rem_euclid(7))
}

fn workweek_start(date: NaiveDate) -> NaiveDate {
    configured_week_start(date)
}

fn configured_weekday_offset(date: NaiveDate) -> i32 {
    let current = i32::try_from(date.weekday().num_days_from_monday()).unwrap_or_default();
    let first =
        i32::try_from(configured_first_weekday().num_days_from_monday()).unwrap_or_default();
    (current - first).rem_euclid(7)
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
    let start = settings
        .as_ref()
        .map(|settings| settings.calendar_workday_start_minutes())
        .unwrap_or(540)
        .min(1_410);
    let configured_end = settings
        .as_ref()
        .map(|settings| settings.calendar_workday_end_minutes())
        .unwrap_or(1_020);
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

fn calendar_event_row(event: &CalendarEventRecord) -> adw::ActionRow {
    let mut details = vec![event_time_text(event)];
    if !event.location.trim().is_empty() {
        details.push(event.location.trim().to_string());
    }
    if !event.source_display_name.trim().is_empty() {
        details.push(event.source_display_name.trim().to_string());
    }
    let row = adw::ActionRow::builder()
        .title(if event.summary.trim().is_empty() {
            "(Ohne Titel)"
        } else {
            event.summary.as_str()
        })
        .subtitle(&details.join(" · "))
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
    if event.source_uid.trim().is_empty() || event.component.trim().is_empty() {
        show_toast_for_widget(
            widget,
            "Dieser Termin stammt aus einer schreibgeschützten importierten Quelle",
        );
        return;
    }
    let dialog = crate::dialogs::EventDialog::new_for_edit(event);
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&window));
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
    NaiveDate::parse_from_str(event.start.get(..10)?, "%Y-%m-%d").ok()
}

fn event_end_date(event: &CalendarEventRecord) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(event.end.get(..10)?, "%Y-%m-%d").ok()
}

fn event_start_datetime(event: &CalendarEventRecord) -> Option<NaiveDateTime> {
    let date = event_start_date(event)?;
    if let Some(value) = event.start.get(..16) {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M") {
            return Some(datetime);
        }
    }
    date.and_hms_opt(0, 0, 0)
}

fn event_occurs_on(event: &CalendarEventRecord, date: NaiveDate) -> bool {
    let Some(start) = event_start_date(event) else {
        return false;
    };
    let mut end = event_end_date(event).unwrap_or(start);
    if event.all_day && end > start {
        end -= Duration::days(1);
    }
    if end < start {
        end = start;
    }
    if has_exdate(event, date) {
        return false;
    }
    if has_rdate(event, date) {
        return true;
    }
    if (start..=end).contains(&date) {
        // EXDATE identifies an occurrence start.  Keep the complete
        // multi-day span hidden when that start is excluded.
        return !has_exdate(event, start);
    }

    let span_days = (end - start).num_days().max(0);
    (0..=span_days).any(|offset| {
        let occurrence_start = date - Duration::days(offset);
        occurrence_start > end && recurrence_occurs_on(event, start, occurrence_start)
    })
}

fn recurrence_occurs_on(
    event: &CalendarEventRecord,
    initial_start: NaiveDate,
    date: NaiveDate,
) -> bool {
    if date < initial_start || event.component.trim().is_empty() {
        return false;
    }
    let Some(rule) = ical_property(&event.component, "RRULE") else {
        return false;
    };

    let Some(rule) = parse_recurrence_rule(&rule) else {
        return false;
    };
    let excluded_dates = exdate_values(event);
    if excluded_dates.contains(&date) {
        return false;
    }
    recurrence_rule_occurs_on(&rule, initial_start, date, &excluded_dates)
}

fn has_exdate(event: &CalendarEventRecord, date: NaiveDate) -> bool {
    exdate_values(event).contains(&date)
}

fn exdate_values(event: &CalendarEventRecord) -> Vec<NaiveDate> {
    ical_properties(&event.component, "EXDATE")
        .iter()
        .flat_map(|value| value.split(','))
        .filter_map(parse_ical_date)
        .collect()
}

fn has_rdate(event: &CalendarEventRecord, date: NaiveDate) -> bool {
    ical_properties(&event.component, "RDATE")
        .iter()
        .flat_map(|value| value.split(','))
        .filter_map(parse_ical_date)
        .any(|included| included == date)
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

fn ical_properties(raw: &str, target: &str) -> Vec<String> {
    unfold_ical_lines(raw)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            (name == target.to_ascii_uppercase()).then(|| unescape_ical_text(value))
        })
        .collect()
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
        .position(|existing| existing.source_uid == event.source_uid && existing.uid == event.uid);
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
        .position(|event| event.source_uid == source_uid && event.uid == uid)
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
    let row = adw::ActionRow::builder()
        .title(title.as_str())
        .subtitle(&format!(
            "{}{}",
            if event.end.is_empty() {
                event.start.clone()
            } else {
                format!("{}–{}", event.start, event.end)
            },
            source
        ))
        .build();
    unsafe {
        row.set_data("event-id", event.uid.clone());
        row.set_data("event-source-uid", event.source_uid.clone());
    }

    let edit_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Termin bearbeiten")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Termin löschen")
        .css_classes(vec!["flat", "destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    let export_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Termin als iCalendar exportieren")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&edit_button);
    row.add_suffix(&export_button);
    row.add_suffix(&delete_button);

    let event_for_edit = event.clone();
    let edit_button_for_signal = edit_button.clone();
    edit_button_for_signal.connect_clicked(move |_| {
        if event_for_edit.source_uid.is_empty() {
            show_toast_for_widget(
                &edit_button,
                "Dieser Termin stammt aus einer schreibgeschützten importierten Quelle",
            );
            return;
        }
        let dialog = crate::dialogs::EventDialog::new_for_edit(&event_for_edit);
        if let Some(root) = edit_button.root() {
            if let Ok(window) = root.downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&window));
            }
        }
        dialog.present();
    });

    let event_for_export = event.clone();
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

    let row_for_delete = row.clone();
    let event_for_delete = event.clone();
    let event_records_for_delete = Rc::clone(&event_records);
    let event_list_for_delete = event_list.clone();
    let delete_button_for_signal = delete_button.clone();
    delete_button_for_signal.connect_clicked(move |_| {
        if event_for_delete.source_uid.is_empty() {
            show_toast_for_widget(
                &delete_button,
                "Dieser Termin stammt aus einer schreibgeschützten importierten Quelle",
            );
            return;
        }
        let confirmation_message = if event_for_delete.source_uid == "local-calendar" {
            "Der Termin wird dauerhaft aus dem lokalen Kalender entfernt."
        } else {
            "Der Termin wird dauerhaft aus dem EDS-Kalender entfernt."
        };
        let event_for_action = event_for_delete.clone();
        let event_records_for_action = Rc::clone(&event_records_for_delete);
        let event_list_for_action = event_list_for_delete.clone();
        let row_for_action = row_for_delete.clone();
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
                let row_for_result = row_for_action.clone();
                let delete_button_for_result = delete_button_for_action.clone();
                glib::spawn_future_local(async move {
                    match receiver.recv().await {
                        Ok(Ok(result)) => {
                            event_list_for_result.remove(&row_for_result);
                            event_records_for_result.borrow_mut().retain(|event| {
                                event.source_uid != source_uid_for_result
                                    || event.uid != uid_for_result
                            });
                            show_toast_for_widget(
                                &delete_button_for_result,
                                &format!("Termin gelöscht ({})", result.uid),
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

    event_list.append(&row);
}

fn remove_event_list_row(event_list: &gtk::ListBox, source_uid: &str, uid: &str) {
    let mut child = event_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = row
                .data::<String>("event-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = row
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
    fn workweek_always_starts_on_monday_and_contains_five_days() {
        let sunday = NaiveDate::from_ymd_opt(2026, 8, 9).unwrap();
        let start = workweek_start(sunday);
        assert_eq!(start.weekday(), Weekday::Mon);
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 8, 3).unwrap());
        assert_eq!(
            start + Duration::days(4),
            NaiveDate::from_ymd_opt(2026, 8, 7).unwrap()
        );
    }

    #[test]
    fn workday_range_keeps_at_least_one_half_hour_slot() {
        let (start, end) = configured_workday_range();
        assert!(end > start);
        assert!(end - start >= 30);
        assert!(end <= 1_440);
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
}
