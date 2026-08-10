// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPage — Calendar view with Day/Week/Month navigation, event creation, and task management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::Datelike;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::calendar_store_service::CalendarEventRecord;
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug)]
    pub struct CalendarPage {
        pub current_year: RefCell<i32>,
        pub current_month: RefCell<u32>,
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    }

    impl Default for CalendarPage {
        fn default() -> Self {
            Self {
                current_year: RefCell::new(2026),
                current_month: RefCell::new(8),
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

        let local_item_state_for_load = local_item_state.clone();
        let event_list_for_load = event_list.clone();
        let event_records_for_load = Rc::clone(&event_records);
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
            .tooltip_text("Vorheriger Monat")
            .build();

        let today_btn = gtk::Button::builder()
            .label("Heute")
            .css_classes(vec!["flat"])
            .tooltip_text("Zu heute wechseln")
            .build();

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .css_classes(vec!["flat"])
            .tooltip_text("Nächster Monat")
            .build();

        let date_label = gtk::Label::builder()
            .label("August 2026")
            .css_classes(vec!["title-3"])
            .hexpand(true)
            .build();

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

        let weekdays = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];
        for (col, day) in weekdays.iter().enumerate() {
            let label = gtk::Label::builder()
                .label(*day)
                .css_classes(vec!["heading", "dim-label"])
                .margin_top(4)
                .margin_bottom(4)
                .build();
            month_grid.attach(&label, col as i32, 0, 1, 1);
        }

        Self::populate_month_grid(&month_grid);

        let month_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&month_grid)
            .build();

        calendar_stack.add_titled(&month_scroll, Some("month"), "Monat");

        // 2. Day View Placeholder
        let day_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();

        let day_card = adw::PreferencesGroup::builder()
            .title("Tagesansicht — noch keine Daten")
            .build();

        let day_state = adw::ActionRow::builder()
            .title("Keine Termine geladen")
            .subtitle("ECal-Synchronisation ist noch nicht verbunden")
            .build();
        let day_icon = gtk::Image::from_icon_name("x-office-calendar-symbolic");
        day_state.add_prefix(&day_icon);
        day_card.add(&day_state);

        day_box.append(&day_card);
        calendar_stack.add_titled(&day_box, Some("day"), "Tag");

        // 3. Week View Placeholder
        let week_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();

        let week_card = adw::PreferencesGroup::builder()
            .title("Wochenansicht — noch keine Daten")
            .build();
        let week_row = adw::ActionRow::builder()
            .title("Keine Termine geladen")
            .subtitle("ECal-Synchronisation ist noch nicht verbunden")
            .build();
        week_card.add(&week_row);

        week_box.append(&week_card);
        calendar_stack.add_titled(&week_box, Some("week"), "Woche");

        calendar_stack.set_visible_child_name("month");

        // View toggle handler
        calendar_stack.set_visible_child_name("month");
        day_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("day");
                }
            }
        ));
        week_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("week");
                }
            }
        ));
        month_btn.connect_toggled(glib::clone!(
            #[weak]
            calendar_stack,
            move |btn| {
                if btn.is_active() {
                    calendar_stack.set_visible_child_name("month");
                }
            }
        ));

        // Date navigation handlers
        prev_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            date_label,
            move |_| {
                let mut m = *page.imp().current_month.borrow();
                let mut y = *page.imp().current_year.borrow();
                if m == 1 {
                    m = 12;
                    y -= 1;
                } else {
                    m -= 1;
                }
                *page.imp().current_month.borrow_mut() = m;
                *page.imp().current_year.borrow_mut() = y;
                date_label.set_label(&format!("{} {}", Self::month_name(m), y));
            }
        ));

        next_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            date_label,
            move |_| {
                let mut m = *page.imp().current_month.borrow();
                let mut y = *page.imp().current_year.borrow();
                if m == 12 {
                    m = 1;
                    y += 1;
                } else {
                    m += 1;
                }
                *page.imp().current_month.borrow_mut() = m;
                *page.imp().current_year.borrow_mut() = y;
                date_label.set_label(&format!("{} {}", Self::month_name(m), y));
            }
        ));

        today_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            date_label,
            move |_| {
                let now = chrono::Local::now();
                let month = now.month();
                let year = now.year();
                *page.imp().current_month.borrow_mut() = month;
                *page.imp().current_year.borrow_mut() = year;
                date_label.set_label(&format!("{} {}", Self::month_name(month), year));
            }
        ));

        calendar_stack.set_vexpand(true);
        content_box.set_vexpand(true);
        content_box.append(&calendar_stack);

        let content_page = adw::NavigationPage::builder()
            .title("Kalenderansicht")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(260.0);
        split.set_max_sidebar_width(360.0);
        split.set_parent(self);
    }

    fn populate_month_grid(grid: &gtk::Grid) {
        let mut day_num = 1;
        for row in 1..=5 {
            for col in 0..7 {
                if row == 1 && col < 5 {
                    // Previous month padding
                    let pad_cell = gtk::Frame::builder()
                        .css_classes(vec!["card", "dim-label"])
                        .build();
                    grid.attach(&pad_cell, col, row, 1, 1);
                    continue;
                }
                if day_num > 31 {
                    break;
                }

                let cell_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(2)
                    .margin_start(4)
                    .margin_end(4)
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();

                let day_lbl = gtk::Label::builder()
                    .label(&day_num.to_string())
                    .halign(gtk::Align::Start)
                    .css_classes(vec!["caption"])
                    .build();
                cell_box.append(&day_lbl);

                let cell_frame = gtk::Frame::builder()
                    .child(&cell_box)
                    .css_classes(vec!["card"])
                    .build();

                grid.attach(&cell_frame, col, row, 1, 1);
                day_num += 1;
            }
        }
    }
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
