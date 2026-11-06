// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable local free/busy view for the calendar.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Local, NaiveDate};
use gtk::glib;
use std::cell::RefCell;

use crate::services::availability_service::{
    FreeSlot, busy_intervals_for_snapshot, find_free_slots,
};
use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct AvailabilityDialog {
        pub date: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AvailabilityDialog {
        const NAME: &'static str = "MeridianAvailabilityDialog";
        type Type = super::AvailabilityDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for AvailabilityDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| vec![glib::ParamSpecString::builder("date").build()])
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "date" {
                *self.date.borrow_mut() = value.get().expect("date property");
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "date" => self.date.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for AvailabilityDialog {}
    impl AdwDialogImpl for AvailabilityDialog {}
}

glib::wrapper! {
    pub struct AvailabilityDialog(ObjectSubclass<imp::AvailabilityDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AvailabilityDialog {
    pub fn new(date: NaiveDate) -> Self {
        glib::Object::builder()
            .property("title", "Free/Busy")
            .property("content-width", 520)
            .property("content-height", 420)
            .property("date", date.format("%Y-%m-%d").to_string())
            .build()
    }

    fn build_ui(&self) {
        let date = self
            .imp()
            .date
            .borrow()
            .parse::<NaiveDate>()
            .unwrap_or_else(|_| Local::now().date_naive());
        let settings = AppSettings::try_new();
        let workday_start = settings
            .as_ref()
            .map(|settings| settings.calendar_workday_start_minutes())
            .unwrap_or(9 * 60);
        let workday_end = settings
            .as_ref()
            .map(|settings| settings.calendar_workday_end_minutes())
            .unwrap_or(17 * 60);
        let minimum_duration = 30;

        let snapshot = CalendarStoreService::new().load_snapshot();
        let busy = busy_intervals_for_snapshot(&snapshot, date);
        let slots = find_free_slots(date, &busy, workday_start, workday_end, minimum_duration);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        let close = gtk::Button::builder()
            .label("Close")
            .build();
        header.pack_start(&close);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(20)
            .margin_end(20)
            .margin_top(20)
            .margin_bottom(20)
            .build();
        content.append(
            &gtk::Label::builder()
                .label(&format!(
                    "Available slots on {} · Working hours {}–{}",
                    date.format("%Y-%m-%d"),
                    format_minutes(workday_start),
                    format_minutes(workday_end)
                ))
                .css_classes(vec!["title-3"])
                .halign(gtk::Align::Start)
                .wrap(true)
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label("Calculated from your local and synced calendars.")
                .css_classes(vec!["dim-label"])
                .halign(gtk::Align::Start)
                .wrap(true)
                .build(),
        );

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        if slots.is_empty() {
            list.append(
                &adw::ActionRow::builder()
                    .title("No available time slots")
                    .subtitle("No free slot of at least 30 minutes was found during working hours.")
                    .build(),
            );
        } else {
            for slot in slots {
                list.append(&slot_row(slot));
            }
        }
        content.append(&list);
        toolbar.set_content(Some(&content));
        self.set_child(Some(&toolbar));

        close.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
    }
}

fn slot_row(slot: FreeSlot) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(format!(
            "{}–{}",
            slot.start.format("%H:%M"),
            slot.end.format("%H:%M")
        ))
        .subtitle(format!("{} minutes available", slot.duration_minutes()))
        .build()
}

fn format_minutes(value: u32) -> String {
    format!("{:02}:{:02}", value / 60, value % 60)
}

impl Default for AvailabilityDialog {
    fn default() -> Self {
        Self::new(Local::now().date_naive())
    }
}
