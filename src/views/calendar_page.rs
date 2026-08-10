// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPage — Calendar view with Day/Week/Month navigation, event creation, and task management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime};
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::calendar_store_service::CalendarEventRecord;
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalendarView {
    Day,
    Week,
    Month,
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
        pub event_records: RefCell<Option<Rc<RefCell<Vec<CalendarEventRecord>>>>>,
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
                event_records: RefCell::new(None),
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
        let split = adw::NavigationSplitView::new();

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

        let calendars: [(&str, &str, bool); 0] = [];

        for (name, _color, active) in calendars {
            let row = adw::ActionRow::builder().title(name).build();

            let check = gtk::CheckButton::builder()
                .active(active)
                .valign(gtk::Align::Center)
                .build();

            row.add_prefix(&check);
            source_list.append(&row);
        }

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
                    }
                }
            });
        }

        let source_list_for_load = source_list.clone();
        let calendar_state_for_load = calendar_state.clone();
        glib::spawn_future_local(async move {
            match eds_service::list_calendar_sources().await {
                Ok(sources) if sources.is_empty() => {
                    calendar_state_for_load.set_label("Keine Kalenderquellen in EDS gefunden.");
                }
                Ok(sources) => {
                    for source in sources {
                        let row = adw::ActionRow::builder()
                            .title(&source.display_name)
                            .subtitle(&format!(
                                "{} · {}",
                                source.backend_name,
                                if source.enabled {
                                    "aktiv"
                                } else {
                                    "deaktiviert"
                                }
                            ))
                            .build();
                        let check = gtk::CheckButton::builder()
                            .active(source.enabled)
                            .sensitive(false)
                            .valign(gtk::Align::Center)
                            .tooltip_text("Kalenderquellen werden über EDS verwaltet")
                            .build();
                        row.add_prefix(&check);
                        source_list_for_load.append(&row);
                    }
                    calendar_state_for_load.set_visible(false);
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load calendar sources from EDS");
                    calendar_state_for_load
                        .set_label(&format!("EDS-Kalender sind nicht erreichbar: {error}"));
                }
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
            .build();

        let sidebar_page = adw::NavigationPage::builder()
            .title("Kalender")
            .child(&sidebar_scroll)
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

        let day_btn = gtk::ToggleButton::builder().label("Tag").build();
        let week_btn = gtk::ToggleButton::builder()
            .label("Woche")
            .group(&day_btn)
            .build();
        let month_btn = gtk::ToggleButton::builder()
            .label("Monat")
            .group(&day_btn)
            .active(true)
            .build();

        view_toggle.append(&day_btn);
        view_toggle.append(&week_btn);
        view_toggle.append(&month_btn);

        view_toolbar.append(&prev_btn);
        view_toolbar.append(&today_btn);
        view_toolbar.append(&next_btn);
        view_toolbar.append(&date_label);
        view_toolbar.append(&btn_add_event);
        view_toolbar.append(&view_toggle);

        content_box.append(&view_toolbar);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // View Stack for Day/Week/Month view switching
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

        calendar_stack.set_visible_child_name("month");

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

        let content_page = adw::NavigationPage::builder()
            .title("Kalenderansicht")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(260.0);
        split.set_max_sidebar_width(360.0);
        split.set_parent(self);
    }

    fn shift_period(&self, direction: i32) {
        let current = *self.imp().current_date.borrow();
        let view = *self.imp().current_view.borrow();
        let next = match view {
            CalendarView::Day => current + Duration::days(i64::from(direction)),
            CalendarView::Week => current + Duration::days(i64::from(direction) * 7),
            CalendarView::Month => shift_month(current, direction),
        };
        *self.imp().current_date.borrow_mut() = next;
    }

    fn refresh_calendar_views(&self) {
        self.refresh_month_grid();
        self.refresh_day_view();
        self.refresh_week_view();
        self.refresh_period_label();
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
                let start = date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
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
            CalendarView::Month => {
                format!("{} {}", Self::month_name(date.month()), date.year())
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
        let week_start = date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
        let week_heading = gtk::Label::builder()
            .label("Termine dieser Woche")
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

    fn events_for_date(&self, date: NaiveDate) -> Vec<CalendarEventRecord> {
        self.imp()
            .event_records
            .borrow()
            .as_ref()
            .map(|records| {
                records
                    .borrow()
                    .iter()
                    .filter(|event| event_occurs_on(event, date))
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
        let first_weekday = month_start.weekday().num_days_from_monday();
        let today = Local::now().date_naive();
        let events = self
            .imp()
            .event_records
            .borrow()
            .as_ref()
            .map(|records| records.borrow().clone())
            .unwrap_or_default();

        while let Some(child) = grid.first_child() {
            grid.remove(&child);
        }

        for (column, weekday) in ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"]
            .iter()
            .enumerate()
        {
            let label = gtk::Label::builder()
                .label(*weekday)
                .css_classes(vec!["heading", "dim-label"])
                .margin_top(4)
                .margin_bottom(4)
                .build();
            grid.attach(&label, column as i32, 0, 1, 1);
        }

        for cell_index in 0..42_i32 {
            let row = cell_index / 7 + 1;
            let column = cell_index % 7;
            let day = cell_index - first_weekday as i32 + 1;
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
                grid.attach(&cell_frame, column, row, 1, 1);
                continue;
            }

            let date = month_start + Duration::days(i64::from(day - 1));
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
                    .label(&format!("• {}", event.summary))
                    .halign(gtk::Align::Start)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(vec!["caption"])
                    .build();
                cell_box.append(&event_label);
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
            grid.attach(&cell_frame, column, row, 1, 1);
        }
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
    adw::ActionRow::builder()
        .title(if event.summary.trim().is_empty() {
            "(Ohne Titel)"
        } else {
            event.summary.as_str()
        })
        .subtitle(&details.join(" · "))
        .build()
}

fn event_time_text(event: &CalendarEventRecord) -> String {
    if event.all_day || event.start.len() <= 10 {
        return "Ganztägig".to_string();
    }
    let start = event.start.get(11..16).unwrap_or(event.start.as_str());
    if event.end.len() > 15 {
        let end = event.end.get(11..16).unwrap_or(event.end.as_str());
        format!("{start}–{end}")
    } else {
        start.to_string()
    }
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
    (start..=end).contains(&date)
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
        if let Some(row) = event_list.row_at_index(index as i32) {
            event_list.remove(&row);
        }
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
    if let Some(row) = event_list.row_at_index(index as i32) {
        event_list.remove(&row);
    }
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
    let row = adw::ActionRow::builder()
        .title(&event.summary)
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
    row.add_suffix(&edit_button);
    row.add_suffix(&delete_button);

    let event_for_edit = event.clone();
    let edit_button_for_signal = edit_button.clone();
    edit_button_for_signal.connect_clicked(move |_| {
        if event_for_edit.source_uid.is_empty() {
            show_toast_for_widget(
                &edit_button,
                "Lokale Termine können ohne EDS-Quelle nicht geändert werden",
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

    let row_for_delete = row.clone();
    let event_for_delete = event.clone();
    let event_records_for_delete = Rc::clone(&event_records);
    let event_list_for_delete = event_list.clone();
    let delete_button_for_signal = delete_button.clone();
    delete_button_for_signal.connect_clicked(move |_| {
        if event_for_delete.source_uid.is_empty() {
            show_toast_for_widget(
                &delete_button,
                "Lokale Termine können ohne EDS-Quelle nicht gelöscht werden",
            );
            return;
        }
        let confirmation = adw::AlertDialog::new(
            Some("Termin löschen?"),
            Some("Der Termin wird dauerhaft aus dem EDS-Kalender entfernt."),
        );
        confirmation.add_responses(&[("cancel", "Abbrechen"), ("delete", "Löschen")]);
        confirmation.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let event_for_response = event_for_delete.clone();
        let event_records_for_response = Rc::clone(&event_records_for_delete);
        let event_list_for_response = event_list_for_delete.clone();
        let row_for_response = row_for_delete.clone();
        let delete_button_for_response = delete_button.clone();
        confirmation.connect_response(None, move |dialog: &adw::AlertDialog, response: &str| {
            if response != "delete" {
                return;
            }
            let source_uid = event_for_response.source_uid.clone();
            let uid = event_for_response.uid.clone();
            let (sender, receiver) = async_channel::bounded(1);
            delete_button_for_response.set_sensitive(false);
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

            let dialog_for_result = dialog.clone();
            let event_records_for_result = Rc::clone(&event_records_for_response);
            let event_list_for_result = event_list_for_response.clone();
            let row_for_result = row_for_response.clone();
            let delete_button_for_result = delete_button_for_response.clone();
            glib::spawn_future_local(async move {
                match receiver.recv().await {
                    Ok(Ok(result)) => {
                        event_list_for_result.remove(&row_for_result);
                        event_records_for_result
                            .borrow_mut()
                            .retain(|event| event.uid != result.uid);
                        show_toast_for_widget(
                            &delete_button_for_result,
                            &format!("Termin gelöscht ({})", result.uid),
                        );
                        dialog_for_result.close();
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
        });
        confirmation.present(Some(&delete_button));
    });

    event_list.append(&row);
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
    fn event_time_text_distinguishes_all_day_and_timed_events() {
        let mut event = event_with_start("2026-08-10 09:30");
        event.end = "2026-08-10 10:45".to_string();
        assert_eq!(event_time_text(&event), "09:30–10:45");
        event.all_day = true;
        assert_eq!(event_time_text(&event), "Ganztägig");
    }
}
