use crate::i18n::ngettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// EventDialog — Dialog for creating and editing calendar events.
//
// Features:
// - Summary / Title entry
// - Location & Description
// - Calendar Source dropdown (Personal, Work, etc.)
// - All-day toggle
// - Start and End Date/Time selection

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::{
    CalendarEventRecord, replace_ical_attachments,
    serialize_ical_attachment_value, update_ical_property,
};
use crate::services::ecal_service::{
    ECalComponentType, ECalService, load_snapshot_prefer_eds,
};
use crate::services::eds_service;
use crate::services::reminder_service::{
    Alarm, AlarmAction, AlarmDuration, AlarmRelation, AlarmTrigger, parse_alarms,
    replace_component_alarms,
};
use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct EventDialog {
        pub edit_component: RefCell<Option<String>>,
        pub edit_uid: RefCell<Option<String>>,
        pub edit_source_uid: RefCell<Option<String>>,
        pub initial_date: RefCell<Option<String>>,
        pub initial_start: RefCell<Option<String>>,
        pub initial_end: RefCell<Option<String>>,
        pub initial_allday: Cell<bool>,
        pub initial_summary: RefCell<Option<String>>,
        pub is_modal: Cell<bool>,
        pub stored_parent: RefCell<Option<gtk::Widget>>,
        pub btn_open_in_new_window: RefCell<Option<gtk::Button>>,
        pub split_handler: RefCell<Option<Rc<dyn Fn()>>>,
        pub close_confirmed: Cell<bool>,
    }

    impl std::fmt::Debug for EventDialog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("EventDialog")
                .field("edit_component", &self.edit_component)
                .field("edit_uid", &self.edit_uid)
                .field("edit_source_uid", &self.edit_source_uid)
                .field("is_modal", &self.is_modal.get())
                .finish()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EventDialog {
        const NAME: &'static str = "MeridianEventDialog";
        type Type = super::EventDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for EventDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("edit-component").build(),
                    glib::ParamSpecString::builder("edit-uid").build(),
                    glib::ParamSpecString::builder("edit-source-uid").build(),
                    glib::ParamSpecString::builder("initial-date").build(),
                    glib::ParamSpecString::builder("initial-start").build(),
                    glib::ParamSpecString::builder("initial-end").build(),
                    glib::ParamSpecBoolean::builder("initial-allday").build(),
                    glib::ParamSpecString::builder("initial-summary").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "edit-component" => {
                    *self.edit_component.borrow_mut() = value.get().expect("string property")
                }
                "edit-uid" => *self.edit_uid.borrow_mut() = value.get().expect("string property"),
                "edit-source-uid" => {
                    *self.edit_source_uid.borrow_mut() = value.get().expect("string property")
                }
                "initial-date" => {
                    *self.initial_date.borrow_mut() = value.get().expect("string property")
                }
                "initial-start" => {
                    *self.initial_start.borrow_mut() = value.get().expect("string property")
                }
                "initial-end" => {
                    *self.initial_end.borrow_mut() = value.get().expect("string property")
                }
                "initial-allday" => {
                    self.initial_allday.set(value.get().unwrap_or(false))
                }
                "initial-summary" => {
                    *self.initial_summary.borrow_mut() = value.get().expect("string property")
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-component" => self.edit_component.borrow().to_value(),
                "edit-uid" => self.edit_uid.borrow().to_value(),
                "edit-source-uid" => self.edit_source_uid.borrow().to_value(),
                "initial-date" => self.initial_date.borrow().to_value(),
                "initial-start" => self.initial_start.borrow().to_value(),
                "initial-end" => self.initial_end.borrow().to_value(),
                "initial-allday" => self.initial_allday.get().to_value(),
                "initial-summary" => self.initial_summary.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for EventDialog {}
    impl AdwDialogImpl for EventDialog {}
}

glib::wrapper! {
    pub struct EventDialog(ObjectSubclass<imp::EventDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

const HOUR_HEIGHT: f64 = 34.0;
const TIMELINE_EVENTS_X: f64 = 44.0;
const TIMELINE_EVENTS_WIDTH: f64 = 178.0;

fn format_clock_minutes(minutes: u32) -> String {
    let h = (minutes / 60) % 24;
    let m = minutes % 60;
    format!("{h:02}:{m:02}")
}

fn parse_time_to_minutes(text: &str) -> Option<u32> {
    let s = text.trim();
    if s.is_empty() {
        return None;
    }
    let upper = s.to_ascii_uppercase();
    let is_pm = upper.ends_with("PM");
    let is_am = upper.ends_with("AM");
    let clean = s
        .trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace())
        .trim();

    let (h_str, m_str) = if let Some((h, m)) = clean.split_once(':') {
        (h, Some(m))
    } else if let Some((h, m)) = clean.split_once('.') {
        (h, Some(m))
    } else {
        (clean, None)
    };

    let mut h: u32 = h_str.trim().parse().ok()?;
    let m: u32 = if let Some(m_part) = m_str {
        m_part.trim().parse().ok()?
    } else {
        0
    };

    if is_pm && h < 12 {
        h += 12;
    } else if is_am && h == 12 {
        h = 0;
    }

    if h < 24 && m < 60 {
        Some(h * 60 + m)
    } else {
        None
    }
}

pub(crate) fn parse_date_lenient(text: &str) -> Option<NaiveDate> {
    let s = text.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%d.%m.%Y") {
        return Some(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%d.%m.%y") {
        return Some(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%m/%d/%Y") {
        return Some(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%m/%d/%y") {
        return Some(d);
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() == 3 {
        if let (Ok(day), Ok(month), Ok(year)) = (
            parts[0].trim().parse::<u32>(),
            parts[1].trim().parse::<u32>(),
            parts[2].trim().parse::<i32>(),
        ) {
            let year = if year < 100 { 2000 + year } else { year };
            return NaiveDate::from_ymd_opt(year, month, day);
        }
    }
    let slash_parts: Vec<&str> = s.split('/').collect();
    if slash_parts.len() == 3 {
        if let (Ok(month), Ok(day), Ok(year)) = (
            slash_parts[0].trim().parse::<u32>(),
            slash_parts[1].trim().parse::<u32>(),
            slash_parts[2].trim().parse::<i32>(),
        ) {
            let year = if year < 100 { 2000 + year } else { year };
            return NaiveDate::from_ymd_opt(year, month, day);
        }
    }
    None
}

pub(crate) fn format_date_for_locale_entry(date: NaiveDate) -> String {
    let resolved = AppSettings::try_new()
        .map(|s| s.resolved_date_format())
        .unwrap_or_else(|| AppSettings::system_locale_date_format().to_string());
    match resolved.as_str() {
        "german" => date.format("%d.%m.%Y").to_string(),
        "us" => date.format("%m/%d/%Y").to_string(),
        _ => date.format("%Y-%m-%d").to_string(),
    }
}

pub(crate) fn date_to_iso(text: &str) -> Option<String> {
    parse_date_lenient(text).map(|d| d.format("%Y-%m-%d").to_string())
}

pub(crate) fn split_datetime_for_entries(datetime_str: &str) -> (String, String) {
    let s = datetime_str.trim();
    if let Some((d, t)) = s.split_once(' ') {
        (d.to_string(), t.to_string())
    } else if let Some((d, t)) = s.split_once('T') {
        let time_part = if t.len() >= 5 {
            format!("{}:{}", &t[..2], &t[2..4])
        } else {
            t.to_string()
        };
        (d.to_string(), time_part)
    } else {
        (s.to_string(), "09:00".to_string())
    }
}

pub(crate) fn clean_calendar_display_name(display_name: &str, group: &str) -> String {
    let trimmed = display_name.trim();
    let group_trimmed = group.trim();
    if !group_trimmed.is_empty() {
        let suffix = format!(" ({group_trimmed})");
        if let Some(stripped) = trimmed.strip_suffix(&suffix) {
            return stripped.trim().to_string();
        }
        let prefix = format!("{group_trimmed} – ");
        if let Some(stripped) = trimmed.strip_prefix(&prefix) {
            return stripped.trim().to_string();
        }
    }
    if let Some(idx) = trimmed.rfind(" (") {
        if trimmed.ends_with(')') {
            let inner = &trimmed[idx + 2..trimmed.len() - 1];
            if inner.contains('@') || inner.eq_ignore_ascii_case(group_trimmed) {
                return trimmed[..idx].trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

pub(crate) fn calendar_group_sort_key(group: &str) -> (u8, &str) {
    let lower = group.to_lowercase();
    if lower == "on this computer" || lower == "auf diesem rechner" || lower == "lokal" {
        (0, group)
    } else {
        (1, group)
    }
}

fn create_date_picker_popover(entry: &gtk::Entry) -> (gtk::Button, gtk::Popover) {
    let btn = gtk::Button::builder()
        .icon_name("x-office-calendar-symbolic")
        .tooltip_text(gettext("Select date in calendar"))
        .css_classes(["datetime-picker-button"])
        .build();
    let popover = gtk::Popover::builder().has_arrow(true).build();
    let cal = gtk::Calendar::new();

    let is_syncing = std::rc::Rc::new(std::cell::Cell::new(false));
    let is_nav_button_click = std::rc::Rc::new(std::cell::Cell::new(false));

    if let Some(date) = parse_date_lenient(&entry.text()) {
        if let Ok(dt) = glib::DateTime::new(
            &glib::TimeZone::local(),
            date.year(),
            date.month() as i32,
            date.day() as i32,
            0,
            0,
            0.0,
        ) {
            is_syncing.set(true);
            cal.select_day(&dt);
            is_syncing.set(false);
        }
    }

    let nav_flag_click = is_nav_button_click.clone();
    let cal_weak_click = cal.downgrade();
    let click_gesture = gtk::GestureClick::new();
    click_gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    click_gesture.connect_pressed(move |_, _, x, y| {
        let mut clicked_header_btn = false;
        if let Some(cal_widget) = cal_weak_click.upgrade() {
            let mut cur = cal_widget.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(w) = cur {
                if w == *cal_widget.upcast_ref::<gtk::Widget>() {
                    break;
                }
                if w.downcast_ref::<gtk::Button>().is_some() {
                    clicked_header_btn = true;
                    break;
                }
                cur = w.parent();
            }
        }
        nav_flag_click.set(clicked_header_btn);
    });
    cal.add_controller(click_gesture);

    let entry_weak = entry.downgrade();
    let popover_weak = popover.downgrade();
    let is_syncing_sel = is_syncing.clone();
    let nav_flag_sel = is_nav_button_click.clone();
    cal.connect_day_selected(move |c| {
        if is_syncing_sel.get() {
            return;
        }
        if nav_flag_sel.replace(false) {
            return;
        }
        let dt = c.date();
        let formatted = NaiveDate::from_ymd_opt(dt.year(), dt.month() as u32, dt.day_of_month() as u32)
            .map(format_date_for_locale_entry)
            .unwrap_or_else(|| format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day_of_month()));
        if let Some(entry) = entry_weak.upgrade() {
            entry.set_text(&formatted);
        }
        if let Some(pop) = popover_weak.upgrade() {
            pop.popdown();
        }
    });

    popover.set_child(Some(&cal));
    popover.set_parent(&btn);
    let popover_clone = popover.clone();
    let entry_weak_open = entry.downgrade();
    let cal_weak_open = cal.downgrade();
    let is_syncing_open = is_syncing.clone();
    btn.connect_clicked(move |_| {
        if let (Some(entry_widget), Some(cal_widget)) = (entry_weak_open.upgrade(), cal_weak_open.upgrade()) {
            if let Some(date) = parse_date_lenient(&entry_widget.text()) {
                if let Ok(dt) = glib::DateTime::new(
                    &glib::TimeZone::local(),
                    date.year(),
                    date.month() as i32,
                    date.day() as i32,
                    0,
                    0,
                    0.0,
                ) {
                    is_syncing_open.set(true);
                    cal_widget.select_day(&dt);
                    is_syncing_open.set(false);
                }
            }
        }
        popover_clone.popup();
    });

    (btn, popover)
}

fn create_time_picker_popover(entry: &gtk::Entry) -> (gtk::Button, gtk::Popover) {
    let btn = gtk::Button::builder()
        .icon_name("alarm-symbolic")
        .tooltip_text(gettext("Select time"))
        .css_classes(["datetime-picker-button"])
        .build();
    let popover = gtk::Popover::builder().has_arrow(true).build();
    let scroll = gtk::ScrolledWindow::builder()
        .max_content_height(260)
        .min_content_width(120)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .width_request(116)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    scroll.set_child(Some(&list));
    popover.set_child(Some(&scroll));
    popover.set_parent(&btn);

    let popover_clone = popover.clone();
    let list_clone = list.clone();
    let entry_weak = entry.downgrade();
    let populated = Rc::new(Cell::new(false));
    btn.connect_clicked(move |_| {
        if !populated.replace(true) {
            for m in (0..1440).step_by(30) {
                let time_str = format_clock_minutes(m);
                let time_btn = gtk::Button::builder()
                    .label(&time_str)
                    .css_classes(["flat"])
                    .halign(gtk::Align::Fill)
                    .build();
                let entry_weak_row = entry_weak.clone();
                let popover_weak = popover_clone.downgrade();
                let time_val = time_str.clone();
                time_btn.connect_clicked(move |_| {
                    if let Some(entry) = entry_weak_row.upgrade() {
                        entry.set_text(&time_val);
                    }
                    if let Some(pop) = popover_weak.upgrade() {
                        pop.popdown();
                    }
                });
                list_clone.append(&time_btn);
            }
        }
        popover_clone.popup();
    });

    (btn, popover)
}

fn create_timezone_picker_popover<E: IsA<gtk::Editable> + IsA<gtk::Widget>>(
    entry: &E,
) -> (gtk::Button, gtk::Popover) {
    let btn = gtk::Button::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text(gettext("Select timezone"))
        .css_classes(["flat"])
        .build();
    let popover = gtk::Popover::builder().has_arrow(true).build();
    let pop_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .margin_bottom(8)
        .width_request(280)
        .build();
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search timezone…"))
        .hexpand(true)
        .build();
    pop_box.append(&search_entry);

    let scroll = gtk::ScrolledWindow::builder()
        .max_content_height(260)
        .min_content_height(180)
        .min_content_width(280)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .width_request(272)
        .build();

    let rows: Rc<RefCell<Vec<(String, gtk::Button)>>> = Rc::new(RefCell::new(Vec::new()));
    let rows_for_search = Rc::clone(&rows);
    search_entry.connect_search_changed(move |se| {
        let q = se.text().trim().to_lowercase();
        for (haystack, row_btn) in rows_for_search.borrow().iter() {
            row_btn.set_visible(q.is_empty() || haystack.contains(&q));
        }
    });

    scroll.set_child(Some(&list));
    pop_box.append(&scroll);
    popover.set_child(Some(&pop_box));
    popover.set_parent(&btn);

    let popover_clone = popover.clone();
    let search_clone = search_entry.clone();
    let list_clone = list.clone();
    let rows_for_click = Rc::clone(&rows);
    let entry_weak = entry.downgrade();
    btn.connect_clicked(move |_| {
        if rows_for_click.borrow().is_empty() {
            let timezones: &[(&str, &str)] = &[
                ("Europe/Berlin", "Europe/Berlin (CET/CEST)"),
                ("Europe/Vienna", "Europe/Vienna"),
                ("Europe/Zurich", "Europe/Zurich"),
                ("Europe/London", "Europe/London (GMT/BST)"),
                ("Europe/Paris", "Europe/Paris"),
                ("Europe/Madrid", "Europe/Madrid"),
                ("Europe/Rome", "Europe/Rome"),
                ("Europe/Amsterdam", "Europe/Amsterdam"),
                ("Europe/Warsaw", "Europe/Warsaw"),
                ("Europe/Stockholm", "Europe/Stockholm"),
                ("Europe/Athens", "Europe/Athens (EET)"),
                ("Europe/Istanbul", "Europe/Istanbul"),
                ("America/New_York", "America/New_York (ET)"),
                ("America/Chicago", "America/Chicago (CT)"),
                ("America/Denver", "America/Denver (MT)"),
                ("America/Los_Angeles", "America/Los_Angeles (PT)"),
                ("America/Toronto", "America/Toronto"),
                ("America/Vancouver", "America/Vancouver"),
                ("America/Sao_Paulo", "America/Sao_Paulo"),
                ("Asia/Dubai", "Asia/Dubai (GST)"),
                ("Asia/Kolkata", "Asia/Kolkata (IST)"),
                ("Asia/Bangkok", "Asia/Bangkok"),
                ("Asia/Singapore", "Asia/Singapore (SGT)"),
                ("Asia/Shanghai", "Asia/Shanghai (CST)"),
                ("Asia/Hong_Kong", "Asia/Hong_Kong"),
                ("Asia/Seoul", "Asia/Seoul (KST)"),
                ("Asia/Tokyo", "Asia/Tokyo (JST)"),
                ("Australia/Sydney", "Australia/Sydney (AEST)"),
                ("Pacific/Auckland", "Pacific/Auckland (NZST)"),
                ("UTC", "UTC (Coordinated Universal Time)"),
                ("", "Floating (no fixed timezone)"),
            ];
            let mut built = Vec::with_capacity(timezones.len());
            for (tz_id, tz_label) in timezones {
                let tz_btn = gtk::Button::builder()
                    .label(*tz_label)
                    .css_classes(["flat"])
                    .halign(gtk::Align::Fill)
                    .build();
                if let Some(lbl) = tz_btn.child().and_downcast::<gtk::Label>() {
                    lbl.set_xalign(0.0);
                }
                let entry_weak_row = entry_weak.clone();
                let popover_weak = popover_clone.downgrade();
                let tz_str = (*tz_id).to_string();
                tz_btn.connect_clicked(move |_| {
                    if let Some(entry) = entry_weak_row.upgrade() {
                        entry.set_text(&tz_str);
                    }
                    if let Some(pop) = popover_weak.upgrade() {
                        pop.popdown();
                    }
                });
                list_clone.append(&tz_btn);
                built.push((
                    format!("{} {}", tz_id.to_lowercase(), tz_label.to_lowercase()),
                    tz_btn,
                ));
            }
            *rows_for_click.borrow_mut() = built;
        }
        search_clone.set_text("");
        popover_clone.popup();
    });

    (btn, popover)
}

fn create_categories_picker_popover<E: IsA<gtk::Editable> + IsA<gtk::Widget>>(
    entry: &E,
) -> (gtk::Button, gtk::Popover) {
    let btn = gtk::Button::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text(gettext("Select categories"))
        .css_classes(["flat"])
        .build();
    let popover = gtk::Popover::builder().has_arrow(true).build();
    let scroll = gtk::ScrolledWindow::builder()
        .max_content_height(260)
        .min_content_width(240)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .width_request(230)
        .margin_start(10)
        .margin_end(10)
        .margin_top(8)
        .margin_bottom(8)
        .build();

    let check_boxes = Rc::new(RefCell::new(Vec::<(String, gtk::CheckButton)>::new()));

    scroll.set_child(Some(&list));
    popover.set_child(Some(&scroll));
    popover.set_parent(&btn);

    let entry_weak = entry.downgrade();
    let check_boxes_for_click = Rc::clone(&check_boxes);
    let list_for_click = list.clone();
    let popover_clone = popover.clone();
    let populated = Rc::new(Cell::new(false));
    btn.connect_clicked(move |_| {
        if !populated.replace(true) {
            let store = CalendarCategoryStore::default();
            let cats = store.load();
            if cats.is_empty() {
                let empty_lbl = gtk::Label::builder()
                    .label(gettext("No saved categories"))
                    .xalign(0.0)
                    .css_classes(["dim-label"])
                    .build();
                list_for_click.append(&empty_lbl);
            } else {
                for cat in cats {
                    let chk = gtk::CheckButton::builder().label(&cat.name).build();
                    list_for_click.append(&chk);
                    check_boxes_for_click.borrow_mut().push((cat.name, chk));
                }
            }
        }
        if let Some(entry) = entry_weak.upgrade() {
            let current_tags: Vec<String> = entry
                .text()
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            for (name, chk) in check_boxes_for_click.borrow().iter() {
                chk.set_active(current_tags.contains(&name.to_lowercase()));
            }
        }
        popover_clone.popup();
    });

    let entry_weak_close = entry.downgrade();
    let check_boxes_for_close = Rc::clone(&check_boxes);
    popover.connect_closed(move |_| {
        if let Some(entry) = entry_weak_close.upgrade() {
            let selected: Vec<String> = check_boxes_for_close
                .borrow()
                .iter()
                .filter(|(_, chk)| chk.is_active())
                .map(|(name, _)| name.clone())
                .collect();
            if !selected.is_empty() {
                entry.set_text(&selected.join(", "));
            }
        }
    });

    (btn, popover)
}

fn create_room_picker_popover<E: IsA<gtk::Editable> + IsA<gtk::Widget>>(
    entry: &E,
    location_row: &adw::EntryRow,
    events_cache: Rc<RefCell<Vec<CalendarEventRecord>>>,
) -> (gtk::Button, gtk::Popover) {
    let btn = gtk::Button::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text(gettext("Select meeting room"))
        .css_classes(["flat"])
        .build();
    let popover = gtk::Popover::builder().has_arrow(true).build();
    let scroll = gtk::ScrolledWindow::builder()
        .max_content_height(300)
        .min_content_width(360)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let box_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .width_request(350)
        .margin_start(10)
        .margin_end(10)
        .margin_top(10)
        .margin_bottom(10)
        .build();

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .width_request(340)
        .hexpand(true)
        .css_classes(["boxed-list"])
        .build();

    box_content.append(&list_box);
    scroll.set_child(Some(&box_content));
    popover.set_child(Some(&scroll));
    popover.set_parent(&btn);

    let popover_clone = popover.clone();
    let list_box_clone = list_box.clone();
    let entry_weak = entry.downgrade();
    let loc_weak = location_row.downgrade();
    let populated = Rc::new(Cell::new(false));
    btn.connect_clicked(move |_| {
        if !populated.replace(true) {
            let mut available_rooms: Vec<(String, String, String)> = vec![
                (
                    "Conference Room Spree".to_string(),
                    "spree-meeting@company.local".to_string(),
                    gettext("12 seats · Projector, Video Conferencing"),
                ),
                (
                    "Meeting Room Havel".to_string(),
                    "havel-meeting@company.local".to_string(),
                    gettext("6 seats · Display, Whiteboard"),
                ),
                (
                    "Meeting Room 1".to_string(),
                    "room-1@company.local".to_string(),
                    gettext("4 seats · TV, Speakerphone"),
                ),
                (
                    "Creative Lounge".to_string(),
                    "lounge@company.local".to_string(),
                    gettext("8 seats · Lounge seating, Mobile board"),
                ),
                (
                    "Focus Pod".to_string(),
                    "focus-pod@company.local".to_string(),
                    gettext("1 seat · Soundproof booth"),
                ),
            ];

            for event in events_cache.borrow().iter() {
                let parsed = parse_component_attendees(&event.component);
                for room in parsed.rooms {
                    let email = room.trim_start_matches("mailto:").to_string();
                    if !available_rooms
                        .iter()
                        .any(|(_, e, _)| e.eq_ignore_ascii_case(&email))
                    {
                        let name = if !event.location.is_empty() {
                            event.location.clone()
                        } else {
                            email.clone()
                        };
                        available_rooms.push((name, email, gettext("Calendar resource")));
                    }
                }
            }

            for (name, email, details) in available_rooms {
                let row = adw::ActionRow::builder()
                    .title(&name)
                    .subtitle(&format!("{email} · {details}"))
                    .activatable(true)
                    .build();

                let icon = gtk::Image::builder()
                    .icon_name("x-office-calendar-symbolic")
                    .build();
                row.add_prefix(&icon);

                let entry_weak_row = entry_weak.clone();
                let loc_weak_row = loc_weak.clone();
                let popover_weak = popover_clone.downgrade();
                let room_name = name.clone();
                let room_email = email.clone();

                row.connect_activated(move |_| {
                    if let Some(entry) = entry_weak_row.upgrade() {
                        entry.set_text(&format!("mailto:{room_email}"));
                    }
                    if let Some(loc) = loc_weak_row.upgrade() {
                        if loc.text().trim().is_empty() {
                            loc.set_text(&room_name);
                        }
                    }
                    if let Some(pop) = popover_weak.upgrade() {
                        pop.popdown();
                    }
                });

                list_box_clone.append(&row);
            }
        }
        popover_clone.popup();
    });

    (btn, popover)
}

#[derive(Clone)]
struct TimelinePreviewSlot {
    label: gtk::Label,
    is_all_day: bool,
    start_min: u32,
    end_min: u32,
    location: String,
    height: f64,
    col_width: f64,
}

#[derive(Clone, Copy, Debug)]
struct PreviewTimelineRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    start_min: u32,
    end_min: u32,
}

#[derive(Clone, Debug)]
struct TimelineTooltipRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    tooltip: String,
}

#[derive(Default)]
struct TimelineInteractiveState {
    preview_rect: Option<PreviewTimelineRect>,
    tooltips: Vec<TimelineTooltipRect>,
    is_dragging: bool,
}

#[derive(Clone, Copy, Debug)]
enum TimelineDragMode {
    None,
    Move { orig_start_min: u32, orig_end_min: u32 },
    ResizeTop { orig_start_min: u32, orig_end_min: u32 },
    ResizeBottom { orig_start_min: u32, orig_end_min: u32 },
    GridSweep { start_y: f64, anchor_min: u32, swept: bool },
}

fn format_timeline_timed_event_markup(
    title: &str,
    start_min: u32,
    end_min: u32,
    location: &str,
    height: f64,
    col_width: f64,
) -> (String, i32, bool) {
    let escaped_title = glib::markup_escape_text(title);
    let start_str = format_clock_minutes(start_min);
    let end_str = format_clock_minutes(end_min);
    let time_display = if col_width >= 82.0 {
        format!("{start_str}–{end_str}")
    } else {
        start_str.clone()
    };

    if height >= 48.0 && !location.trim().is_empty() {
        let escaped_loc = glib::markup_escape_text(location.trim());
        let max_lines = (((height - 6.0) / 14.0).floor() as i32).max(3);
        (
            format!(
                "<span size=\"small\" alpha=\"88%\">{time_display}</span>\n<b>{escaped_title}</b>\n<span size=\"x-small\" alpha=\"80%\">{escaped_loc}</span>"
            ),
            max_lines,
            true,
        )
    } else if height >= 28.0 {
        let max_lines = (((height - 4.0) / 14.0).floor() as i32).max(2);
        (
            format!("<span size=\"small\" alpha=\"88%\">{time_display}</span>\n<b>{escaped_title}</b>"),
            max_lines,
            true,
        )
    } else {
        (
            format!("<span size=\"small\">{start_str} <b>{escaped_title}</b></span>"),
            1,
            false,
        )
    }
}

fn update_day_schedule_timeline(
    all_day_box: &gtk::Box,
    timeline_fixed: &gtk::Fixed,
    timeline_window_title: &adw::WindowTitle,
    conflict_badge: &gtk::Label,
    preview_slot: &Rc<RefCell<Option<TimelinePreviewSlot>>>,
    interactive_state: &Rc<RefCell<TimelineInteractiveState>>,
    events_cache: &[CalendarEventRecord],
    date_str: &str,
    start_time_str: &str,
    end_time_str: &str,
    all_day: bool,
    summary: &str,
    preview_location: &str,
    edit_uid: Option<&str>,
    selected_calendar_color: Option<&str>,
) {
    preview_slot.replace(None);
    {
        let mut st = interactive_state.borrow_mut();
        st.preview_rect = None;
        st.tooltips.clear();
    }

    while let Some(child) = all_day_box.first_child() {
        all_day_box.remove(&child);
    }
    while let Some(child) = timeline_fixed.first_child() {
        timeline_fixed.remove(&child);
    }

    let Some(current_date) = parse_date_lenient(date_str) else {
        timeline_window_title.set_subtitle(&gettext("Invalid date"));
        all_day_box.set_visible(false);
        conflict_badge.set_visible(false);
        return;
    };

    timeline_window_title.set_subtitle(&current_date.format("%A, %b %d, %Y").to_string());

    let current_start_min = parse_time_to_minutes(start_time_str).unwrap_or(9 * 60);
    let current_end_min = parse_time_to_minutes(end_time_str).unwrap_or(current_start_min + 60);
    let current_end_min = if current_end_min <= current_start_min {
        current_start_min + 30
    } else {
        current_end_min
    };

    let default_title = gettext("New Event");
    let display_title = if summary.trim().is_empty() {
        &default_title
    } else {
        summary.trim()
    };

    let preview_color = selected_calendar_color.unwrap_or("#3584e4").to_string();

    // 1. All-day events
    let mut all_day_items: Vec<(bool, String, String, String, String)> = Vec::new(); // (is_preview, title, color, location, description)
    if all_day {
        all_day_items.push((
            true,
            display_title.to_string(),
            preview_color.clone(),
            preview_location.to_string(),
            String::new(),
        ));
    }
    for ev in events_cache {
        if Some(ev.uid.as_str()) == edit_uid {
            continue;
        }
        if crate::views::calendar_page::event_occurs_on(ev, current_date) && ev.all_day {
            let ev_title = if ev.summary.trim().is_empty() {
                gettext("(Untitled)")
            } else {
                ev.summary.trim().to_string()
            };
            let color = crate::views::calendar_page::event_color_for_record(ev);
            all_day_items.push((false, ev_title, color, ev.location.clone(), ev.description.clone()));
        }
    }

    fn ensure_timeline_event_css(color: &str, is_preview: bool, is_all_day: bool, is_dark: bool) -> String {
        let clean = color.trim_start_matches('#').to_lowercase();
        let mode = if is_dark { "dark" } else { "light" };
        let preview_tag = if is_preview { "prev" } else { "norm" };
        let type_tag = if is_all_day { "allday" } else { "timed" };
        let class_name = format!("timeline-ev-{clean}-{preview_tag}-{type_tag}-{mode}");

        thread_local! {
            static REGISTERED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
        }

        let already_registered = REGISTERED.with(|r| r.borrow().contains(&class_name));
        if !already_registered {
            if let Some(display) = gtk::gdk::Display::default() {
                let (bg, _, _, fg) = crate::views::calendar_page::pastel_colors_for_hex(color, is_dark);
                let border_rule = if is_preview {
                    format!("border-left: 4px solid {color}; box-shadow: 0 0 0 1.5px {color}; font-weight: 600;")
                } else {
                    format!("border-left: 4px solid {color}; font-weight: 500;")
                };
                let (radius, padding) = if is_all_day {
                    ("5px", "3px 8px")
                } else {
                    ("6px", "3px 6px 3px 7px")
                };
                let css = format!(
                    "label.{class_name} {{ background-color: {bg}; color: {fg}; {border_rule} border-radius: {radius}; padding: {padding}; }}"
                );
                let p = gtk::CssProvider::new();
                p.load_from_string(&css);
                gtk::style_context_add_provider_for_display(
                    &display,
                    &p,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                REGISTERED.with(|r| r.borrow_mut().insert(class_name.clone()));
            }
        }

        class_name
    }

    if all_day_items.is_empty() {
        all_day_box.set_visible(false);
    } else {
        all_day_box.set_visible(true);
        for (is_preview, title, color, loc, desc) in &all_day_items {
            let is_dark = adw::StyleManager::default().is_dark();
            let color_class = ensure_timeline_event_css(color, *is_preview, true, is_dark);
            let mut classes = vec!["appointment-timeline-event", color_class.as_str()];
            if *is_preview {
                classes.push("appointment-timeline-preview");
            }
            let display_title = if title.trim().is_empty() {
                gettext("(Untitled)")
            } else {
                title.to_string()
            };
            let mut tooltip = format!("{display_title}\n{}", gettext("All Day"));
            if *is_preview {
                tooltip.push_str(&format!("\n({})", gettext("Click or drag on the timeline below to set a time")));
            }
            if !loc.trim().is_empty() {
                tooltip.push_str(&format!("\n📍 {}: {}", gettext("Location"), loc.trim()));
            }
            if !desc.trim().is_empty() {
                let desc_trimmed = desc.trim();
                let short_desc = if desc_trimmed.chars().count() > 180 {
                    let truncated: String = desc_trimmed.chars().take(177).collect();
                    format!("{truncated}...")
                } else {
                    desc_trimmed.to_string()
                };
                tooltip.push_str(&format!("\n\n📝 {}:\n{short_desc}", gettext("Notes")));
            }
            let ev_label = gtk::Label::builder()
                .label(&display_title)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .xalign(0.0)
                .halign(gtk::Align::Fill)
                .valign(gtk::Align::Start)
                .width_request(TIMELINE_EVENTS_WIDTH as i32)
                .height_request(24)
                .css_classes(classes)
                .tooltip_text(&tooltip)
                .build();
            if *is_preview {
                preview_slot.replace(Some(TimelinePreviewSlot {
                    label: ev_label.clone(),
                    is_all_day: true,
                    start_min: current_start_min,
                    end_min: current_end_min,
                    location: loc.clone(),
                    height: 24.0,
                    col_width: TIMELINE_EVENTS_WIDTH,
                }));
            }
            all_day_box.append(&ev_label);
        }
    }

    // 2. Timed events (existing + preview)
    struct DayTimelineTimedItem {
        is_preview: bool,
        start_min: u32,
        end_min: u32,
        title: String,
        color: String,
        uid: Option<String>,
        location: String,
        description: String,
    }

    let mut timed_items: Vec<DayTimelineTimedItem> = Vec::new();

    for ev in events_cache {
        if Some(ev.uid.as_str()) == edit_uid || ev.all_day {
            continue;
        }
        if crate::views::calendar_page::event_occurs_on(ev, current_date) {
            let (ev_start, ev_end, _ev_time_text) =
                crate::views::calendar_page::event_occurrence_times_for_date(ev, current_date, 30);
            let ev_title = if ev.summary.trim().is_empty() {
                gettext("(Untitled)")
            } else {
                ev.summary.trim().to_string()
            };
            let color = crate::views::calendar_page::event_color_for_record(ev);
            timed_items.push(DayTimelineTimedItem {
                is_preview: false,
                start_min: ev_start,
                end_min: ev_end.max(ev_start + 15),
                title: ev_title,
                color,
                uid: Some(ev.uid.clone()),
                location: ev.location.clone(),
                description: ev.description.clone(),
            });
        }
    }

    if !all_day {
        timed_items.push(DayTimelineTimedItem {
            is_preview: true,
            start_min: current_start_min,
            end_min: current_end_min.max(current_start_min + 15),
            title: display_title.to_string(),
            color: preview_color,
            uid: None,
            location: preview_location.to_string(),
            description: String::new(),
        });
    }

    // Sort chronologically
    timed_items.sort_by(|a, b| {
        a.start_min
            .cmp(&b.start_min)
            .then_with(|| a.end_min.cmp(&b.end_min))
            .then_with(|| a.is_preview.cmp(&b.is_preview))
    });

    // Check for overlaps (conflict detection)
    let mut has_conflict = false;
    let mut conflicting_uids = std::collections::HashSet::new();
    if !all_day {
        for item in &timed_items {
            if item.is_preview {
                for other in &timed_items {
                    if !other.is_preview {
                        if item.start_min < other.end_min && item.end_min > other.start_min {
                            has_conflict = true;
                            if let Some(ref uid) = other.uid {
                                conflicting_uids.insert(uid.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    if conflicting_uids.is_empty() {
        conflict_badge.set_visible(false);
    } else {
        let count = conflicting_uids.len();
        let text = if count == 1 {
            format!("⚠️ {}", gettext("1 conflict"))
        } else {
            format!(
                "⚠️ {}",
                ngettext("{count} conflict", "{count} conflicts", count as u32)
                    .replace("{count}", &count.to_string())
            )
        };
        conflict_badge.set_label(&text);
        conflict_badge.set_visible(true);
    }

    // Overlapping clusters
    let mut clusters: Vec<Vec<DayTimelineTimedItem>> = Vec::new();
    let mut current_cluster: Vec<DayTimelineTimedItem> = Vec::new();
    let mut cluster_end = 0;

    for item in timed_items {
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

    // Assign columns in each cluster
    for cluster in clusters {
        let mut col_ends: Vec<u32> = Vec::new();
        let mut placed: Vec<(DayTimelineTimedItem, usize)> = Vec::new();

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
        let total_width = TIMELINE_EVENTS_WIDTH;
        let col_width = if total_cols > 1 {
            ((total_width - (total_cols as f64 - 1.0) * col_gap) / total_cols as f64).max(36.0)
        } else {
            total_width
        };

        for (item, col_idx) in placed {
            let x = TIMELINE_EVENTS_X + col_idx as f64 * (col_width + col_gap);
            let y = (item.start_min as f64 / 60.0) * HOUR_HEIGHT;
            let duration = item.end_min.saturating_sub(item.start_min);
            let h = (((duration.max(20) as f64) / 60.0) * HOUR_HEIGHT - 2.0).max(20.0);

            let is_conflict = item
                .uid
                .as_ref()
                .map(|u| conflicting_uids.contains(u))
                .unwrap_or(false)
                || (item.is_preview && has_conflict);

            let is_dark = adw::StyleManager::default().is_dark();

            let conflict_msg = format!("\n⚠️ {}", gettext("Overlaps with another event"));
            let drag_msg = format!("\n↕ {}", gettext("Drag to move · Drag top/bottom edge to resize"));
            let mut tooltip = format!(
                "{}\n{}–{}{}",
                item.title,
                format_clock_minutes(item.start_min),
                format_clock_minutes(item.end_min),
                if is_conflict {
                    conflict_msg.as_str()
                } else if item.is_preview {
                    drag_msg.as_str()
                } else {
                    ""
                }
            );
            if item.is_preview && is_conflict {
                tooltip.push_str(&drag_msg);
            }
            if !item.location.trim().is_empty() {
                tooltip.push_str(&format!("\n📍 {}: {}", gettext("Location"), item.location.trim()));
            }
            if !item.description.trim().is_empty() {
                let desc_trimmed = item.description.trim();
                let short_desc = if desc_trimmed.chars().count() > 180 {
                    let truncated: String = desc_trimmed.chars().take(177).collect();
                    format!("{truncated}...")
                } else {
                    desc_trimmed.to_string()
                };
                tooltip.push_str(&format!("\n\n📝 {}:\n{short_desc}", gettext("Notes")));
            }

            let mut css_classes = vec!["appointment-timeline-event"];
            if item.is_preview {
                css_classes.push("appointment-timeline-preview");
            }
            if is_conflict {
                css_classes.push("appointment-timeline-conflict");
            }

            let color_class = ensure_timeline_event_css(&item.color, item.is_preview, false, is_dark);
            css_classes.push(&color_class);

            let (markup_text, max_lines, is_multiline) = format_timeline_timed_event_markup(
                &item.title,
                item.start_min,
                item.end_min,
                &item.location,
                h,
                col_width,
            );

            let widget = gtk::Label::builder()
                .use_markup(true)
                .label(&markup_text)
                .wrap(is_multiline)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .single_line_mode(!is_multiline)
                .lines(max_lines)
                .xalign(0.0)
                .yalign(0.0)
                .halign(gtk::Align::Fill)
                .valign(gtk::Align::Fill)
                .width_request(col_width as i32)
                .height_request(h as i32)
                .can_target(false)
                .css_classes(css_classes)
                .build();

            timeline_fixed.put(&widget, x, y);

            if item.is_preview {
                // Subtle top & bottom resize grip bars on the preview event
                if h >= 24.0 && col_width >= 44.0 {
                    let grip_w = 22.0;
                    let grip_x = x + ((col_width - grip_w) * 0.5).max(4.0);
                    let top_grip = gtk::Box::builder()
                        .width_request(grip_w as i32)
                        .height_request(2)
                        .can_target(false)
                        .css_classes(["appointment-timeline-resize-grip"])
                        .build();
                    let bottom_grip = gtk::Box::builder()
                        .width_request(grip_w as i32)
                        .height_request(2)
                        .can_target(false)
                        .css_classes(["appointment-timeline-resize-grip"])
                        .build();
                    timeline_fixed.put(&top_grip, grip_x, y + 2.0);
                    timeline_fixed.put(&bottom_grip, grip_x, (y + h - 4.0).max(y + 4.0));
                }

                preview_slot.replace(Some(TimelinePreviewSlot {
                    label: widget.clone(),
                    is_all_day: false,
                    start_min: item.start_min,
                    end_min: item.end_min,
                    location: item.location.clone(),
                    height: h,
                    col_width,
                }));
                interactive_state.borrow_mut().preview_rect = Some(PreviewTimelineRect {
                    x,
                    y,
                    width: col_width,
                    height: h,
                    start_min: item.start_min,
                    end_min: item.end_min,
                });
            }

            interactive_state.borrow_mut().tooltips.push(TimelineTooltipRect {
                x,
                y,
                width: col_width,
                height: h,
                tooltip,
            });
        }
    }
}

impl EventDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", gettext("Event — No Summary"))
            .property("content-width", 880)
            .property("content-height", 680)
            .build()
    }

    pub fn new_for_date(date: NaiveDate) -> Self {
        glib::Object::builder()
            .property("title", gettext("Event — No Summary"))
            .property("content-width", 880)
            .property("content-height", 680)
            .property("initial-date", date.format("%Y-%m-%d").to_string())
            .build()
    }

    pub fn new_all_day_for_date(date: NaiveDate) -> Self {
        Self::new_all_day_for_date_with_summary(date, None, None)
    }

    pub fn new_all_day_for_date_range(start_date: NaiveDate, end_date: NaiveDate) -> Self {
        Self::new_all_day_for_date_range_with_summary(start_date, end_date, None, None)
    }

    pub fn new_all_day_for_date_with_summary(
        date: NaiveDate,
        summary: Option<&str>,
        source_uid: Option<&str>,
    ) -> Self {
        Self::new_all_day_for_date_range_with_summary(date, date, summary, source_uid)
    }

    pub fn new_all_day_for_date_range_with_summary(
        start_date: NaiveDate,
        end_date: NaiveDate,
        summary: Option<&str>,
        source_uid: Option<&str>,
    ) -> Self {
        let (s_d, e_d) = if start_date <= end_date {
            (start_date, end_date)
        } else {
            (end_date, start_date)
        };
        let title_text = summary
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("{} — {s}", gettext("Event")))
            .unwrap_or_else(|| gettext("Event — No Summary"));
        let mut builder = glib::Object::builder()
            .property("title", title_text)
            .property("content-width", 880)
            .property("content-height", 680)
            .property("initial-date", s_d.format("%Y-%m-%d").to_string())
            .property("initial-start", format!("{} 09:00", s_d.format("%Y-%m-%d")))
            .property("initial-end", format!("{} 10:00", e_d.format("%Y-%m-%d")))
            .property("initial-allday", true);
        if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
            builder = builder.property("initial-summary", s);
        }
        if let Some(src) = source_uid.filter(|s| !s.trim().is_empty()) {
            builder = builder.property("edit-source-uid", src);
        }
        builder.build()
    }

    pub fn new_for_time_range(start: NaiveDateTime, end: NaiveDateTime) -> Self {
        Self::new_for_time_range_with_summary(start, end, None, None)
    }

    pub fn new_for_time_range_with_summary(
        start: NaiveDateTime,
        end: NaiveDateTime,
        summary: Option<&str>,
        source_uid: Option<&str>,
    ) -> Self {
        let title_text = summary
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("{} — {s}", gettext("Event")))
            .unwrap_or_else(|| gettext("Event — No Summary"));
        let mut builder = glib::Object::builder()
            .property("title", title_text)
            .property("content-width", 880)
            .property("content-height", 680)
            .property("initial-start", start.format("%Y-%m-%d %H:%M").to_string())
            .property("initial-end", end.format("%Y-%m-%d %H:%M").to_string())
            .property("initial-date", start.date().format("%Y-%m-%d").to_string());
        if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
            builder = builder.property("initial-summary", s);
        }
        if let Some(src) = source_uid.filter(|s| !s.trim().is_empty()) {
            builder = builder.property("edit-source-uid", src);
        }
        builder.build()
    }

    pub fn new_for_edit(event: &CalendarEventRecord) -> Self {
        let summary = ical_property(&event.component, "SUMMARY")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| gettext("No Summary"));
        glib::Object::builder()
            .property("title", &format!("{} — {summary}", gettext("Event")))
            .property("content-width", 880)
            .property("content-height", 680)
            .property("edit-component", &event.component)
            .property("edit-uid", &event.uid)
            .property("edit-source-uid", &event.source_uid)
            .build()
    }

    pub fn set_transient_for<W: IsA<gtk::Window> + IsA<gtk::Widget>>(&self, parent: Option<&W>) {
        if let Some(p) = parent {
            self.imp().stored_parent.replace(Some(p.clone().upcast()));
        } else {
            self.imp().stored_parent.borrow_mut().take();
        }
    }

    pub fn is_modal(&self) -> bool {
        self.imp().is_modal.get()
    }

    pub fn present_modal(&self, parent: &impl IsA<gtk::Widget>) {
        self.imp().is_modal.set(true);
        self.imp().stored_parent.replace(Some(parent.upcast_ref::<gtk::Widget>().clone()));
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_visible(true);
        }
        adw::prelude::AdwDialogExt::present(self, Some(parent));
    }

    pub fn present_standalone(&self) {
        self.imp().is_modal.set(false);
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_visible(false);
        }
        adw::prelude::AdwDialogExt::present(self, None::<&gtk::Widget>);
    }

    pub fn present_adaptive(&self, parent: &impl IsA<gtk::Widget>) {
        if let Some(win) = crate::window::find_meridian_window(parent) {
            self.set_transient_for(Some(&win));
            if win.is_mobile() || win.is_tablet() {
                self.present_modal(parent);
                return;
            }
        }
        self.present_standalone();
    }

    pub fn present(&self) {
        if self.imp().is_modal.get() {
            if let Some(parent) = self.imp().stored_parent.borrow().as_ref() {
                self.present_modal(parent);
                return;
            }
        }
        self.present_standalone();
    }

    pub fn split_to_standalone_window(&self) {
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_sensitive(false);
        }
        if let Some(handler) = self.imp().split_handler.borrow().as_ref() {
            handler();
        } else {
            self.imp().close_confirmed.set(true);
            self.close();
            self.present_standalone();
        }
    }

    fn build_ui(&self) {
        let edit_component = self.imp().edit_component.borrow().clone();
        let edit_uid = self.imp().edit_uid.borrow().clone();
        let edit_source_uid = self.imp().edit_source_uid.borrow().clone();
        let form_toolbar_view = adw::ToolbarView::new();
        form_toolbar_view.set_hexpand(true);
        form_toolbar_view.set_vexpand(true);

        let form_header_bar = adw::HeaderBar::new();
        form_header_bar.set_show_end_title_buttons(false);
        form_header_bar.set_show_start_title_buttons(false);

        let btn_open_in_new_window = gtk::Button::builder()
            .icon_name("window-new-symbolic")
            .tooltip_text(gettext("Open in new window"))
            .css_classes(["flat"])
            .visible(self.imp().is_modal.get())
            .build();
        btn_open_in_new_window.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.split_to_standalone_window();
            }
        ));
        form_header_bar.pack_start(&btn_open_in_new_window);
        self.imp().btn_open_in_new_window.replace(Some(btn_open_in_new_window));

        let btn_cancel = gtk::Button::builder()
            .label(gettext("Cancel"))
            .build();
        let dialog_weak_cancel = self.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_cancel.upgrade() {
                dialog.close();
            }
        });
        form_header_bar.pack_start(&btn_cancel);

        let btn_save = gtk::Button::builder()
            .label(&if edit_uid.is_some() {
                gettext("Save")
            } else {
                gettext("Create")
            })
            .css_classes(["suggested-action"])
            .build();
        form_header_bar.pack_end(&btn_save);

        let window_title = adw::WindowTitle::builder()
            .title(&if edit_uid.is_some() {
                gettext("Edit Event")
            } else {
                gettext("New Event")
            })
            .build();
        form_header_bar.set_title_widget(Some(&window_title));
        form_toolbar_view.add_top_bar(&form_header_bar);

        let scroll_general = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .build();

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .css_classes(["appointment-form-grid", "appointment-form-page"])
            .build();

        let initial_events = crate::services::ecal_service::load_calendar_snapshot_fast().events;
        let events_cache = Rc::new(RefCell::new(initial_events));

        // ── Group 1: Basic Event Info ──
        let group_basic = adw::PreferencesGroup::new();

        let entry_title = adw::EntryRow::builder()
            .title(gettext("Title"))
            .show_apply_button(false)
            .build();
        group_basic.add(&entry_title);

        let row_location = adw::EntryRow::builder()
            .title(gettext("Location"))
            .show_apply_button(false)
            .build();
        group_basic.add(&row_location);

        let selected_source_uid = Rc::new(RefCell::new(String::new()));
        let selected_source_color = Rc::new(RefCell::new(String::new()));
        let selected_source_name = Rc::new(RefCell::new(String::new()));

        let calendar_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();

        let calendar_scrolled = gtk::ScrolledWindow::builder()
            .max_content_height(360)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&calendar_popover_box)
            .build();

        let calendar_popover = gtk::Popover::builder()
            .css_classes(["calendar-picker-popover"])
            .child(&calendar_scrolled)
            .has_arrow(true)
            .autohide(true)
            .build();

        let btn_calendar_chip = gtk::Box::builder()
            .css_classes(["calendar-picker-color-chip"])
            .valign(gtk::Align::Center)
            .build();

        let btn_calendar_label = gtk::Label::builder()
            .label(gettext("Local Calendar"))
            .xalign(0.0)
            .build();

        let btn_calendar_arrow = gtk::Image::from_icon_name("pan-down-symbolic");

        let btn_calendar_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        btn_calendar_content.append(&btn_calendar_chip);
        btn_calendar_content.append(&btn_calendar_label);
        btn_calendar_content.append(&btn_calendar_arrow);

        let btn_calendar = gtk::MenuButton::builder()
            .child(&btn_calendar_content)
            .popover(&calendar_popover)
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        btn_calendar.set_sensitive(edit_uid.is_none());

        let row_calendar_action = adw::ActionRow::builder()
            .title(gettext("Calendar"))
            .activatable_widget(&btn_calendar)
            .build();
        row_calendar_action.add_suffix(&btn_calendar);
        group_basic.add(&row_calendar_action);

        content_box.append(&group_basic);

        // Default start/end
        let (default_start, default_end) = {
            let initial_start = self
                .imp()
                .initial_start
                .borrow()
                .as_deref()
                .and_then(|value| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M").ok());
            let initial_end = self
                .imp()
                .initial_end
                .borrow()
                .as_deref()
                .and_then(|value| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M").ok());
            match (initial_start, initial_end) {
                (Some(start), Some(end)) => (start, end),
                (Some(start), None) => (start, start + Duration::hours(1)),
                _ => {
                    let now = Local::now();
                    let initial_date = self
                        .imp()
                        .initial_date
                        .borrow()
                        .as_deref()
                        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
                    let default_start = initial_date
                        .and_then(|date| {
                            if date == now.date_naive() {
                                let next_min = ((now.minute() / 30) + 1) * 30;
                                let add_hours = (next_min / 60) as u32;
                                let min = (next_min % 60) as u32;
                                let hour = (now.hour() + add_hours).min(23);
                                date.and_hms_opt(hour, min, 0)
                            } else {
                                date.and_hms_opt(9, 0, 0)
                            }
                        })
                        .unwrap_or_else(|| now.naive_local());
                    let default_end = default_start + Duration::hours(1);
                    (default_start, default_end)
                }
            }
        };

        // ── Group 2: Date & Time, Recurrence & Reminders ──
        let group_time = adw::PreferencesGroup::builder()
            .title(gettext("Date & Time"))
            .build();

        let row_allday = adw::SwitchRow::builder()
            .title(gettext("All Day"))
            .active(false)
            .build();
        group_time.add(&row_allday);

        let row_start = adw::ActionRow::builder()
            .title(gettext("Starts"))
            .build();

        let entry_start_date = gtk::Entry::builder()
            .width_chars(10)
            .max_width_chars(10)
            .text(&format_date_for_locale_entry(default_start.date()))
            .tooltip_text(gettext("Date (DD.MM.YYYY, MM/DD/YYYY, or YYYY-MM-DD)"))
            .valign(gtk::Align::Center)
            .build();
        let (btn_start_cal, _pop_start_cal) = create_date_picker_popover(&entry_start_date);
        btn_start_cal.set_valign(gtk::Align::Center);
        let start_date_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Center)
            .css_classes(["linked", "datetime-input-group"])
            .build();
        start_date_box.append(&entry_start_date);
        start_date_box.append(&btn_start_cal);

        let entry_start_time = gtk::Entry::builder()
            .width_chars(5)
            .max_width_chars(5)
            .text(&default_start.format("%H:%M").to_string())
            .tooltip_text(gettext("Time (HH:MM)"))
            .valign(gtk::Align::Center)
            .build();
        let (btn_start_time, _pop_start_time) = create_time_picker_popover(&entry_start_time);
        btn_start_time.set_valign(gtk::Align::Center);
        let start_time_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Center)
            .margin_start(6)
            .css_classes(["linked", "datetime-input-group"])
            .build();
        start_time_box.append(&entry_start_time);
        start_time_box.append(&btn_start_time);

        row_start.add_suffix(&start_date_box);
        row_start.add_suffix(&start_time_box);
        group_time.add(&row_start);

        let row_end = adw::ActionRow::builder()
            .title(gettext("Ends"))
            .build();

        let entry_end_date = gtk::Entry::builder()
            .width_chars(10)
            .max_width_chars(10)
            .text(&format_date_for_locale_entry(default_end.date()))
            .tooltip_text(gettext("Date (DD.MM.YYYY, MM/DD/YYYY, or YYYY-MM-DD)"))
            .valign(gtk::Align::Center)
            .build();
        let (btn_end_cal, _pop_end_cal) = create_date_picker_popover(&entry_end_date);
        btn_end_cal.set_valign(gtk::Align::Center);
        let end_date_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Center)
            .css_classes(["linked", "datetime-input-group"])
            .build();
        end_date_box.append(&entry_end_date);
        end_date_box.append(&btn_end_cal);

        let entry_end_time = gtk::Entry::builder()
            .width_chars(5)
            .max_width_chars(5)
            .text(&default_end.format("%H:%M").to_string())
            .tooltip_text(gettext("Time (HH:MM)"))
            .valign(gtk::Align::Center)
            .build();
        let (btn_end_time, _pop_end_time) = create_time_picker_popover(&entry_end_time);
        btn_end_time.set_valign(gtk::Align::Center);
        let end_time_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Center)
            .margin_start(6)
            .css_classes(["linked", "datetime-input-group"])
            .build();
        end_time_box.append(&entry_end_time);
        end_time_box.append(&btn_end_time);

        row_end.add_suffix(&end_date_box);
        row_end.add_suffix(&end_time_box);
        group_time.add(&row_end);

        let recurrence_preset_model = gtk::StringList::new(&[
            &gettext("None"),
            &gettext("Daily"),
            &gettext("Weekly"),
            &gettext("Biweekly"),
            &gettext("Monthly"),
            &gettext("Yearly"),
            &gettext("Custom…"),
        ]);
        let row_recurrence_preset = adw::ComboRow::builder()
            .title(gettext("Repeat"))
            .model(&recurrence_preset_model)
            .selected(0)
            .build();
        group_time.add(&row_recurrence_preset);

        let row_recurrence = adw::EntryRow::builder()
            .title(gettext("Recurrence Rule (RRULE)"))
            .show_apply_button(false)
            .visible(false)
            .build();
        group_time.add(&row_recurrence);
        let row_rdates = adw::EntryRow::builder()
            .title(gettext("Additional Dates (RDATE)"))
            .show_apply_button(false)
            .visible(false)
            .build();
        group_time.add(&row_rdates);
        let row_exdates = adw::EntryRow::builder()
            .title(gettext("Excluded Dates (EXDATE)"))
            .show_apply_button(false)
            .visible(false)
            .build();
        group_time.add(&row_exdates);

        {
            let rrule_syncing = Rc::new(Cell::new(false));
            let row_rec_for_preset = row_recurrence.clone();
            let row_rdates_for_preset = row_rdates.clone();
            let row_exdates_for_preset = row_exdates.clone();
            let syncing_for_preset = Rc::clone(&rrule_syncing);
            row_recurrence_preset.connect_selected_notify(move |combo| {
                if syncing_for_preset.get() {
                    return;
                }
                syncing_for_preset.set(true);
                match combo.selected() {
                    0 => {
                        row_rec_for_preset.set_text("");
                        row_rec_for_preset.set_visible(false);
                        row_rdates_for_preset.set_visible(!row_rdates_for_preset.text().trim().is_empty());
                        row_exdates_for_preset.set_visible(!row_exdates_for_preset.text().trim().is_empty());
                    }
                    1 => {
                        row_rec_for_preset.set_text("FREQ=DAILY");
                        row_rec_for_preset.set_visible(false);
                    }
                    2 => {
                        row_rec_for_preset.set_text("FREQ=WEEKLY");
                        row_rec_for_preset.set_visible(false);
                    }
                    3 => {
                        row_rec_for_preset.set_text("FREQ=WEEKLY;INTERVAL=2");
                        row_rec_for_preset.set_visible(false);
                    }
                    4 => {
                        row_rec_for_preset.set_text("FREQ=MONTHLY");
                        row_rec_for_preset.set_visible(false);
                    }
                    5 => {
                        row_rec_for_preset.set_text("FREQ=YEARLY");
                        row_rec_for_preset.set_visible(false);
                    }
                    _ => {
                        row_rec_for_preset.set_visible(true);
                        row_rdates_for_preset.set_visible(true);
                        row_exdates_for_preset.set_visible(true);
                    }
                }
                syncing_for_preset.set(false);
            });

            let preset_for_rrule = row_recurrence_preset.clone();
            let row_rec_vis = row_recurrence.clone();
            let syncing_for_rrule = Rc::clone(&rrule_syncing);
            row_recurrence.connect_changed(move |entry| {
                if syncing_for_rrule.get() {
                    return;
                }
                syncing_for_rrule.set(true);
                let val = entry.text().trim().to_ascii_uppercase();
                let idx = match val.as_str() {
                    "" => 0,
                    "FREQ=DAILY" => 1,
                    "FREQ=WEEKLY" => 2,
                    "FREQ=WEEKLY;INTERVAL=2" => 3,
                    "FREQ=MONTHLY" => 4,
                    "FREQ=YEARLY" => 5,
                    _ => {
                        row_rec_vis.set_visible(true);
                        6
                    }
                };
                preset_for_rrule.set_selected(idx);
                syncing_for_rrule.set(false);
            });
        }

        let alarm_editor = AlarmEditor::new(edit_component.as_deref(), "");
        if edit_component.is_none() {
            if let Some(settings) = AppSettings::try_new() {
                if settings.use_default_reminder() {
                    let default_minutes = settings.default_reminder_total_minutes();
                    alarm_editor.add_default_display_if_empty(default_minutes);
                }
            }
        }
        let expander_alarms = adw::ExpanderRow::builder()
            .title(gettext("Reminders"))
            .subtitle(&alarm_editor.summary_label())
            .use_markup(false)
            .show_enable_switch(false)
            .build();
        let alarm_prefs_row = adw::PreferencesRow::builder()
            .activatable(false)
            .css_classes(["alarm-expander-container-row"])
            .child(alarm_editor.widget())
            .build();
        expander_alarms.add_row(&alarm_prefs_row);
        let expander_alarms_for_cb = expander_alarms.clone();
        let alarm_editor_for_cb = alarm_editor.clone();
        alarm_editor.connect_changed(move |_| {
            expander_alarms_for_cb.set_subtitle(&alarm_editor_for_cb.summary_label());
        });
        group_time.add(&expander_alarms);

        content_box.append(&group_time);

        // ── Group 3: Details & Options (Progressive Disclosure) ──
        let group_options = adw::PreferencesGroup::builder()
            .title(gettext("Details"))
            .build();

        let row_attendees = adw::EntryRow::builder()
            .title(gettext("Attendees"))
            .show_apply_button(false)
            .build();
        let btn_room_availability = gtk::Button::builder()
            .icon_name("x-office-calendar-symbolic")
            .tooltip_text(gettext("Check Free/Busy Availability"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        let entry_start_date_for_avail = entry_start_date.clone();
        btn_room_availability.connect_clicked(move |btn| {
            let date_str = entry_start_date_for_avail.text();
            let date = parse_date_lenient(date_str.trim())
                .unwrap_or_else(|| chrono::Local::now().date_naive());
            let avail_dialog = crate::dialogs::AvailabilityDialog::new(date);
            avail_dialog.present(Some(btn));
        });
        row_attendees.add_suffix(&btn_room_availability);
        group_options.add(&row_attendees);

        let row_conference = adw::EntryRow::builder()
            .title(gettext("Meeting Link"))
            .show_apply_button(false)
            .build();
        let btn_open_web = gtk::Button::builder()
            .icon_name("web-browser-symbolic")
            .tooltip_text(gettext("Open link in browser"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        let row_conference_weak = row_conference.downgrade();
        btn_open_web.connect_clicked(move |_| {
            if let Some(entry) = row_conference_weak.upgrade() {
                let url = entry.text().trim().to_string();
                if !url.is_empty() {
                    let uri = if url.starts_with("http://") || url.starts_with("https://") {
                        url
                    } else {
                        format!("https://{url}")
                    };
                    let _ = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
                }
            }
        });
        row_conference.add_suffix(&btn_open_web);
        group_options.add(&row_conference);

        let expander_advanced = adw::ExpanderRow::builder()
            .title(gettext("More Options"))
            .subtitle(gettext("Status, timezone, room, categories, and privacy"))
            .use_markup(false)
            .show_enable_switch(false)
            .build();

        let status_model = gtk::StringList::new(&[
            &gettext("None"),
            &gettext("Confirmed"),
            &gettext("Tentative"),
            &gettext("Cancelled"),
        ]);
        let dropdown_status = adw::ComboRow::builder()
            .title(gettext("Status"))
            .model(&status_model)
            .selected(1)
            .build();
        expander_advanced.add_row(&dropdown_status);

        let transparency_model = gtk::StringList::new(&[&gettext("Busy"), &gettext("Free")]);
        let dropdown_transparency = adw::ComboRow::builder()
            .title(gettext("Availability"))
            .model(&transparency_model)
            .selected(0)
            .build();
        expander_advanced.add_row(&dropdown_transparency);

        let default_tz = crate::services::ical_time_service::default_calendar_timezone_id();
        let entry_tz = adw::EntryRow::builder()
            .title(gettext("Timezone"))
            .text(&default_tz)
            .show_apply_button(false)
            .build();
        let (btn_tz, _pop_tz) = create_timezone_picker_popover(&entry_tz);
        btn_tz.set_valign(gtk::Align::Center);
        let btn_diff_end_tz = gtk::ToggleButton::builder()
            .icon_name("airplane-mode-symbolic")
            .tooltip_text(gettext("Different end timezone (e.g. for flights across timezones)"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        entry_tz.add_suffix(&btn_tz);
        entry_tz.add_suffix(&btn_diff_end_tz);
        let row_tz = entry_tz.clone();
        expander_advanced.add_row(&entry_tz);

        let entry_end_tz = adw::EntryRow::builder()
            .title(gettext("Arrival Timezone"))
            .text(&default_tz)
            .show_apply_button(false)
            .visible(false)
            .build();
        let (btn_end_tz, _pop_end_tz) = create_timezone_picker_popover(&entry_end_tz);
        btn_end_tz.set_valign(gtk::Align::Center);
        entry_end_tz.add_suffix(&btn_end_tz);
        let row_end_tz = entry_end_tz.clone();
        expander_advanced.add_row(&entry_end_tz);

        let row_end_tz_for_toggle = row_end_tz.clone();
        let entry_end_tz_for_toggle = entry_end_tz.clone();
        let entry_tz_for_toggle = entry_tz.clone();
        let row_allday_for_tz_toggle = row_allday.clone();
        btn_diff_end_tz.connect_toggled(move |btn| {
            let active = btn.is_active() && !row_allday_for_tz_toggle.is_active();
            row_end_tz_for_toggle.set_visible(active);
            if active && entry_end_tz_for_toggle.text().trim().is_empty() {
                entry_end_tz_for_toggle.set_text(&entry_tz_for_toggle.text());
            }
        });

        let entry_room = adw::EntryRow::builder()
            .title(gettext("Meeting Room"))
            .show_apply_button(false)
            .build();
        let (btn_room, _pop_room) = create_room_picker_popover(
            &entry_room,
            &row_location,
            Rc::clone(&events_cache),
        );
        btn_room.set_valign(gtk::Align::Center);
        entry_room.add_suffix(&btn_room);
        expander_advanced.add_row(&entry_room);

        let entry_categories = adw::EntryRow::builder()
            .title(gettext("Categories"))
            .show_apply_button(false)
            .build();
        let (btn_categories, _pop_categories) = create_categories_picker_popover(&entry_categories);
        btn_categories.set_valign(gtk::Align::Center);
        entry_categories.add_suffix(&btn_categories);
        expander_advanced.add_row(&entry_categories);

        let row_private = adw::SwitchRow::builder()
            .title(gettext("Private Event"))
            .subtitle(gettext("Mark as private"))
            .build();
        expander_advanced.add_row(&row_private);

        group_options.add(&expander_advanced);
        content_box.append(&group_options);

        // ── Group 4: Notes ──
        let group_desc = adw::PreferencesGroup::builder()
            .title(gettext("Notes"))
            .build();

        let desc_overlay = gtk::Overlay::builder()
            .css_classes(["card", "appointment-notes-card"])
            .hexpand(true)
            .vexpand(true)
            .build();
        let scroll_desc = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(104)
            .hexpand(true)
            .vexpand(true)
            .build();
        let text_desc = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(14)
            .right_margin(14)
            .top_margin(12)
            .bottom_margin(12)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["appointment-notes-view"])
            .build();
        let desc_placeholder = gtk::Label::builder()
            .label(gettext("Add notes or description…"))
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start(14)
            .margin_top(12)
            .can_target(false)
            .css_classes(["dim-label"])
            .build();
        let desc_placeholder_for_buf = desc_placeholder.clone();
        text_desc.buffer().connect_changed(move |buf| {
            desc_placeholder_for_buf.set_visible(buf.char_count() == 0);
        });
        scroll_desc.set_child(Some(&text_desc));
        desc_overlay.set_child(Some(&scroll_desc));
        desc_overlay.add_overlay(&desc_placeholder);
        group_desc.add(&desc_overlay);

        content_box.append(&group_desc);

        let clamp = adw::Clamp::builder()
            .maximum_size(780)
            .tightening_threshold(600)
            .child(&content_box)
            .build();
        scroll_general.set_child(Some(&clamp));
        form_toolbar_view.set_content(Some(&scroll_general));

        // Right: Day Schedule Timeline inside sidebar panel
        let timeline_window_title = adw::WindowTitle::builder()
            .title(gettext("Day Schedule"))
            .subtitle(&default_start.format("%A, %b %d, %Y").to_string())
            .build();

        let timeline_header_bar = adw::HeaderBar::new();
        timeline_header_bar.set_show_end_title_buttons(false);
        timeline_header_bar.set_show_start_title_buttons(false);
        timeline_header_bar.set_title_widget(Some(&timeline_window_title));

        let timeline_toolbar_view = adw::ToolbarView::new();
        timeline_toolbar_view.add_top_bar(&timeline_header_bar);
        timeline_toolbar_view.set_hexpand(false);
        timeline_toolbar_view.set_vexpand(true);
        timeline_toolbar_view.set_width_request(260);
        timeline_toolbar_view.add_css_class("appointment-timeline-container");

        let conflict_badge = gtk::Label::builder()
            .css_classes(["appointment-timeline-conflict-badge"])
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(4)
            .visible(false)
            .build();

        let all_day_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .visible(false)
            .build();

        let scrolled_timeline = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .width_request(260)
            .hexpand(false)
            .vexpand(true)
            .build();

        let timeline_overlay = gtk::Overlay::builder()
            .can_target(true)
            .has_tooltip(true)
            .build();

        let hours_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .height_request((24.0 * HOUR_HEIGHT) as i32)
            .width_request(250)
            .hexpand(false)
            .can_target(false)
            .build();

        for h in 0..24 {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .height_request(HOUR_HEIGHT as i32)
                .width_request(250)
                .hexpand(false)
                .can_target(false)
                .css_classes(["appointment-timeline-hour-row"])
                .build();
            let lbl = gtk::Label::builder()
                .label(&format!("{h}⁰⁰"))
                .width_request(38)
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .can_target(false)
                .css_classes(["appointment-timeline-hour-label"])
                .build();
            let sep = gtk::Separator::builder()
                .orientation(gtk::Orientation::Horizontal)
                .width_request(200)
                .hexpand(false)
                .valign(gtk::Align::Start)
                .can_target(false)
                .build();
            row.append(&lbl);
            row.append(&sep);
            hours_box.append(&row);
        }
        timeline_overlay.set_child(Some(&hours_box));

        let timeline_fixed = gtk::Fixed::builder()
            .height_request((24.0 * HOUR_HEIGHT) as i32)
            .width_request(250)
            .hexpand(false)
            .can_target(false)
            .build();
        timeline_overlay.add_overlay(&timeline_fixed);

        scrolled_timeline.set_child(Some(&timeline_overlay));

        let timeline_content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .hexpand(false)
            .vexpand(true)
            .build();
        timeline_content_box.append(&conflict_badge);
        timeline_content_box.append(&all_day_box);
        timeline_content_box.append(&scrolled_timeline);
        timeline_toolbar_view.set_content(Some(&timeline_content_box));

        let split_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .hexpand(true)
            .vexpand(true)
            .build();

        split_box.append(&form_toolbar_view);
        let pane_sep = gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        split_box.append(&pane_sep);
        split_box.append(&timeline_toolbar_view);

        self.set_child(Some(&split_box));

        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 680sp").unwrap());
        bp.add_setter(&timeline_toolbar_view, "visible", Some(&false.to_value()));
        bp.add_setter(&pane_sep, "visible", Some(&false.to_value()));
        self.add_breakpoint(bp);

        // Timeline live updater
        let color_badge_provider = Rc::new(RefCell::new(None::<gtk::CssProvider>));
        let selected_source_color_for_timeline = Rc::clone(&selected_source_color);
        let preview_slot = Rc::new(RefCell::new(None::<TimelinePreviewSlot>));
        let interactive_state = Rc::new(RefCell::new(TimelineInteractiveState::default()));
        let suppress_timeline_update = Rc::new(Cell::new(false));
        let update_timeline = {
            let all_day_box = all_day_box.clone();
            let timeline_fixed = timeline_fixed.clone();
            let timeline_window_title = timeline_window_title.clone();
            let conflict_badge = conflict_badge.clone();
            let preview_slot = Rc::clone(&preview_slot);
            let interactive_state = Rc::clone(&interactive_state);
            let suppress_timeline_update = Rc::clone(&suppress_timeline_update);
            let events_cache = Rc::clone(&events_cache);
            let entry_start_date = entry_start_date.clone();
            let entry_start_time = entry_start_time.clone();
            let entry_end_time = entry_end_time.clone();
            let row_allday = row_allday.clone();
            let entry_title = entry_title.clone();
            let row_location = row_location.clone();
            let edit_uid = edit_uid.clone();
            let selected_source_color = Rc::clone(&selected_source_color_for_timeline);
            let color_badge_provider = Rc::clone(&color_badge_provider);
            Rc::new(move || {
                if suppress_timeline_update.get() {
                    return;
                }
                let color_val = selected_source_color.borrow().clone();
                let cal_color = if color_val.is_empty() { None } else { Some(color_val) };

                if let Some(ref color) = cal_color {
                    if let Some(display) = gtk::gdk::Display::default() {
                        let mut prov_slot = color_badge_provider.borrow_mut();
                        let css = format!("box.appointment-color-badge {{ background-color: {color}; min-width: 14px; min-height: 14px; border-radius: 9999px; margin-end: 6px; }}");
                        if let Some(ref p) = *prov_slot {
                            p.load_from_string(&css);
                        } else {
                            let p = gtk::CssProvider::new();
                            p.load_from_string(&css);
                            gtk::style_context_add_provider_for_display(
                                &display,
                                &p,
                                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                            );
                            *prov_slot = Some(p);
                        }
                    }
                }

                update_day_schedule_timeline(
                    &all_day_box,
                    &timeline_fixed,
                    &timeline_window_title,
                    &conflict_badge,
                    &preview_slot,
                    &interactive_state,
                    &events_cache.borrow(),
                    &entry_start_date.text(),
                    &entry_start_time.text(),
                    &entry_end_time.text(),
                    row_allday.is_active(),
                    &entry_title.text(),
                    &row_location.text(),
                    edit_uid.as_deref(),
                    cal_color.as_deref(),
                );
            })
        };

        // Interactive Day Schedule: dynamic tooltips, hover cursors, drag-to-move, drag-to-resize, and click/sweep
        {
            let state_for_tooltip = Rc::clone(&interactive_state);
            timeline_overlay.connect_query_tooltip(move |_, x, y, _keyboard, tooltip| {
                let st = state_for_tooltip.borrow();
                if st.is_dragging {
                    return false;
                }
                let fx = x as f64;
                let fy = y as f64;
                for item in st.tooltips.iter().rev() {
                    if fx >= item.x && fx <= item.x + item.width && fy >= item.y && fy <= item.y + item.height {
                        tooltip.set_text(Some(&item.tooltip));
                        return true;
                    }
                }
                false
            });
        }

        {
            let state_for_motion = Rc::clone(&interactive_state);
            let overlay_weak_motion = timeline_overlay.downgrade();
            let motion_ctrl = gtk::EventControllerMotion::new();
            motion_ctrl.connect_motion(move |_, x, y| {
                let Some(overlay) = overlay_weak_motion.upgrade() else {
                    return;
                };
                let st = state_for_motion.borrow();
                if st.is_dragging {
                    return;
                }
                if let Some(rect) = st.preview_rect {
                    if x >= rect.x - 4.0 && x <= rect.x + rect.width + 4.0 {
                        let edge = (rect.height * 0.25).clamp(5.0, 8.0);
                        if (y - rect.y).abs() <= edge || (y - (rect.y + rect.height)).abs() <= edge {
                            overlay.set_cursor_from_name(Some("ns-resize"));
                            return;
                        } else if y >= rect.y && y <= rect.y + rect.height {
                            overlay.set_cursor_from_name(Some("grab"));
                            return;
                        }
                    }
                }
                if x >= TIMELINE_EVENTS_X - 6.0 {
                    overlay.set_cursor_from_name(Some("pointer"));
                } else {
                    overlay.set_cursor_from_name(None);
                }
            });
            let state_for_leave = Rc::clone(&interactive_state);
            let overlay_weak_leave = timeline_overlay.downgrade();
            motion_ctrl.connect_leave(move |_| {
                if !state_for_leave.borrow().is_dragging {
                    if let Some(overlay) = overlay_weak_leave.upgrade() {
                        overlay.set_cursor_from_name(None);
                    }
                }
            });
            timeline_overlay.add_controller(motion_ctrl);
        }

        {
            let drag_mode = Rc::new(Cell::new(TimelineDragMode::None));
            let apply_timeline_minutes = {
                let entry_start_time = entry_start_time.clone();
                let entry_end_time = entry_end_time.clone();
                let suppress = Rc::clone(&suppress_timeline_update);
                let update_tl = update_timeline.clone();
                Rc::new(move |new_start: u32, new_end: u32| {
                    let start_clamped = new_start.min(23 * 60 + 45);
                    let end_clamped = new_end.max(start_clamped + 15).min(23 * 60 + 59);
                    let s_txt = format_clock_minutes(start_clamped);
                    let e_txt = format_clock_minutes(end_clamped);
                    let changed_s = entry_start_time.text().as_str() != s_txt;
                    let changed_e = entry_end_time.text().as_str() != e_txt;
                    if !changed_s && !changed_e {
                        return;
                    }
                    suppress.set(true);
                    if changed_s {
                        entry_start_time.set_text(&s_txt);
                    }
                    if changed_e {
                        entry_end_time.set_text(&e_txt);
                    }
                    suppress.set(false);
                    update_tl();
                })
            };

            let drag_gesture = gtk::GestureDrag::new();
            let state_begin = Rc::clone(&interactive_state);
            let mode_begin = Rc::clone(&drag_mode);
            let overlay_weak_begin = timeline_overlay.downgrade();
            let entry_start_begin = entry_start_time.clone();
            let entry_end_begin = entry_end_time.clone();
            let row_allday_begin = row_allday.clone();
            let apply_begin = Rc::clone(&apply_timeline_minutes);
            drag_gesture.connect_drag_begin(move |gesture, start_x, start_y| {
                if start_x < TIMELINE_EVENTS_X - 10.0 {
                    mode_begin.set(TimelineDragMode::None);
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let preview_rect = {
                    let mut st = state_begin.borrow_mut();
                    st.is_dragging = true;
                    st.preview_rect
                };
                if let Some(rect) = preview_rect {
                    if start_x >= rect.x - 6.0 && start_x <= rect.x + rect.width + 6.0 {
                        let edge = (rect.height * 0.25).clamp(5.0, 8.0);
                        if (start_y - rect.y).abs() <= edge {
                            mode_begin.set(TimelineDragMode::ResizeTop {
                                orig_start_min: rect.start_min,
                                orig_end_min: rect.end_min,
                            });
                            if let Some(overlay) = overlay_weak_begin.upgrade() {
                                overlay.set_cursor_from_name(Some("ns-resize"));
                            }
                            return;
                        } else if (start_y - (rect.y + rect.height)).abs() <= edge {
                            mode_begin.set(TimelineDragMode::ResizeBottom {
                                orig_start_min: rect.start_min,
                                orig_end_min: rect.end_min,
                            });
                            if let Some(overlay) = overlay_weak_begin.upgrade() {
                                overlay.set_cursor_from_name(Some("ns-resize"));
                            }
                            return;
                        } else if start_y >= rect.y && start_y <= rect.y + rect.height {
                            mode_begin.set(TimelineDragMode::Move {
                                orig_start_min: rect.start_min,
                                orig_end_min: rect.end_min,
                            });
                            if let Some(overlay) = overlay_weak_begin.upgrade() {
                                overlay.set_cursor_from_name(Some("grabbing"));
                            }
                            return;
                        }
                    }
                }

                // Clicked outside preview block: jump event to clicked 15-min slot (and allow sweeping a new range)
                let cur_start = parse_time_to_minutes(&entry_start_begin.text()).unwrap_or(9 * 60);
                let cur_end = parse_time_to_minutes(&entry_end_begin.text()).unwrap_or(cur_start + 60);
                let duration = cur_end.saturating_sub(cur_start).max(15);
                let raw_min = ((start_y / HOUR_HEIGHT) * 60.0).clamp(0.0, (24 * 60 - 15) as f64);
                let anchor_min = (((raw_min / 15.0).floor() as u32) * 15).min(23 * 60 + 45);
                if row_allday_begin.is_active() {
                    row_allday_begin.set_active(false);
                }
                let max_start = (23 * 60 + 45).min(1440u32.saturating_sub(duration));
                let new_start = anchor_min.min(max_start);
                let new_end = (new_start + duration).min(23 * 60 + 59);
                apply_begin(new_start, new_end);
                mode_begin.set(TimelineDragMode::GridSweep {
                    start_y,
                    anchor_min,
                    swept: false,
                });
                if let Some(overlay) = overlay_weak_begin.upgrade() {
                    overlay.set_cursor_from_name(Some("ns-resize"));
                }
            });

            let mode_update = Rc::clone(&drag_mode);
            let apply_update = Rc::clone(&apply_timeline_minutes);
            drag_gesture.connect_drag_update(move |_, _offset_x, offset_y| {
                match mode_update.get() {
                    TimelineDragMode::None => {}
                    TimelineDragMode::Move {
                        orig_start_min,
                        orig_end_min,
                    } => {
                        let duration = orig_end_min.saturating_sub(orig_start_min).max(15);
                        let delta_steps = ((offset_y / HOUR_HEIGHT) * 4.0).round() as i32;
                        let delta_min = delta_steps * 15;
                        let max_start = (23 * 60 + 45).min(1440u32.saturating_sub(duration)) as i32;
                        let new_start = (orig_start_min as i32 + delta_min).clamp(0, max_start.max(0)) as u32;
                        let new_end = (new_start + duration).min(23 * 60 + 59);
                        apply_update(new_start, new_end);
                    }
                    TimelineDragMode::ResizeBottom {
                        orig_start_min,
                        orig_end_min,
                    } => {
                        let delta_steps = ((offset_y / HOUR_HEIGHT) * 4.0).round() as i32;
                        let delta_min = delta_steps * 15;
                        let min_end = (orig_start_min + 15) as i32;
                        let new_end = (orig_end_min as i32 + delta_min).clamp(min_end, 23 * 60 + 45) as u32;
                        apply_update(orig_start_min, new_end);
                    }
                    TimelineDragMode::ResizeTop {
                        orig_start_min,
                        orig_end_min,
                    } => {
                        let delta_steps = ((offset_y / HOUR_HEIGHT) * 4.0).round() as i32;
                        let delta_min = delta_steps * 15;
                        let max_start = orig_end_min.saturating_sub(15) as i32;
                        let new_start = (orig_start_min as i32 + delta_min).clamp(0, max_start.max(0)) as u32;
                        apply_update(new_start, orig_end_min);
                    }
                    TimelineDragMode::GridSweep {
                        start_y,
                        anchor_min,
                        swept,
                    } => {
                        if offset_y.abs() >= (HOUR_HEIGHT * 0.22) || swept {
                            mode_update.set(TimelineDragMode::GridSweep {
                                start_y,
                                anchor_min,
                                swept: true,
                            });
                            let raw_min = (((start_y + offset_y) / HOUR_HEIGHT) * 60.0).clamp(0.0, 24.0 * 60.0);
                            let cur_min = (((raw_min / 15.0).round() as u32) * 15).min(23 * 60 + 45);
                            let (new_start, new_end) = if cur_min >= anchor_min {
                                (anchor_min, cur_min.max(anchor_min + 15))
                            } else {
                                (cur_min, anchor_min.max(cur_min + 15))
                            };
                            apply_update(new_start, new_end);
                        }
                    }
                }
            });

            let state_end = Rc::clone(&interactive_state);
            let mode_end = Rc::clone(&drag_mode);
            let overlay_weak_end = timeline_overlay.downgrade();
            drag_gesture.connect_drag_end(move |_, _offset_x, _offset_y| {
                state_end.borrow_mut().is_dragging = false;
                mode_end.set(TimelineDragMode::None);
                if let Some(overlay) = overlay_weak_end.upgrade() {
                    overlay.set_cursor_from_name(None);
                }
            });

            timeline_overlay.add_controller(drag_gesture);
        }

        let update_tl_sdate = update_timeline.clone();
        let entry_end_date_for_sdate = entry_end_date.clone();
        let prev_start_date_cell = Rc::new(Cell::new(default_start.date()));
        let prev_start_for_sdate = Rc::clone(&prev_start_date_cell);
        entry_start_date.connect_changed(move |entry| {
            let s = entry.text();
            if let Some(new_start) = parse_date_lenient(&s) {
                let prev_start = prev_start_for_sdate.replace(new_start);
                let current_end = parse_date_lenient(&entry_end_date_for_sdate.text());
                let new_end = match current_end {
                    Some(end_d) => {
                        let span_days = (end_d - prev_start).num_days().max(0);
                        new_start + Duration::days(span_days)
                    }
                    None => new_start,
                };
                entry_end_date_for_sdate.set_text(&format_date_for_locale_entry(new_end));
            }
            update_tl_sdate();
        });

        let update_tl_edate = update_timeline.clone();
        entry_end_date.connect_changed(move |_| update_tl_edate());

        let update_tl_stime = update_timeline.clone();
        entry_start_time.connect_changed(move |_| update_tl_stime());

        let update_tl_etime = update_timeline.clone();
        entry_end_time.connect_changed(move |_| update_tl_etime());

        let update_tl_loc = update_timeline.clone();
        row_location.connect_changed(move |_| update_tl_loc());

        let update_tl_allday = update_timeline.clone();
        let start_time_box_allday = start_time_box.clone();
        let end_time_box_allday = end_time_box.clone();
        let row_tz_allday = row_tz.clone();
        let row_end_tz_allday = row_end_tz.clone();
        let btn_diff_end_tz_allday = btn_diff_end_tz.clone();
        row_allday.connect_active_notify(move |chk| {
            let active = chk.is_active();
            start_time_box_allday.set_visible(!active);
            end_time_box_allday.set_visible(!active);
            row_tz_allday.set_sensitive(!active);
            row_end_tz_allday.set_sensitive(!active);
            row_end_tz_allday.set_visible(!active && btn_diff_end_tz_allday.is_active());
            update_tl_allday();
        });
        if self.imp().initial_allday.get() {
            row_allday.set_active(true);
        }

        let window_title_clone = window_title.clone();
        let dialog_weak_title = self.downgrade();
        let preview_slot_for_title = Rc::clone(&preview_slot);
        entry_title.connect_changed(move |entry| {
            let text = entry.text();
            let trimmed = text.trim();
            if let Some(dialog) = dialog_weak_title.upgrade() {
                if trimmed.is_empty() {
                    dialog.set_title(&gettext("Event — No Summary"));
                    window_title_clone.set_subtitle("");
                } else {
                    dialog.set_title(&format!("{} — {trimmed}", gettext("Event")));
                    window_title_clone.set_subtitle(trimmed);
                }
            }
            if let Some(slot) = preview_slot_for_title.borrow().as_ref() {
                let default_display = gettext("New Event");
                let display = if trimmed.is_empty() {
                    &default_display
                } else {
                    trimmed
                };
                if slot.is_all_day {
                    slot.label.set_label(display);
                    slot.label.set_tooltip_text(Some(&format!("{display}\n{}", gettext("All Day"))));
                } else {
                    let (markup, _, _) = format_timeline_timed_event_markup(
                        display,
                        slot.start_min,
                        slot.end_min,
                        &slot.location,
                        slot.height,
                        slot.col_width,
                    );
                    slot.label.set_markup(&markup);
                }
            }
        });

        // Auto-scroll timeline to start hour
        let scroll_adj = scrolled_timeline.vadjustment();
        let target_y = ((default_start.hour() as f64 - 1.0).max(0.0)) * HOUR_HEIGHT;
        let scrolled_flag = Rc::new(Cell::new(false));
        let scrolled_notify = scrolled_flag.clone();
        scroll_adj.connect_upper_notify(move |adj| {
            if !scrolled_notify.get() && adj.upper() > 100.0 {
                scrolled_notify.set(true);
                adj.set_value(target_y);
            }
        });
        let scroll_adj_idle = scroll_adj.clone();
        let scrolled_idle = scrolled_flag.clone();
        glib::idle_add_local_once(move || {
            if !scrolled_idle.get() && scroll_adj_idle.upper() > 0.0 {
                scrolled_idle.set(true);
                scroll_adj_idle.set_value(target_y);
            }
        });

        if let Some(component) = edit_component.as_deref() {
            let summary = ical_property(component, "SUMMARY").unwrap_or_default();
            let trimmed = summary.trim();
            if trimmed.is_empty() {
                entry_title.set_text("");
                self.set_title(&gettext("Event — No Summary"));
                window_title.set_subtitle("");
            } else {
                entry_title.set_text(trimmed);
                self.set_title(&format!("{} — {trimmed}", gettext("Event")));
                window_title.set_subtitle(trimmed);
            }
            row_location.set_text(
                ical_property(component, "LOCATION")
                    .as_deref()
                    .unwrap_or(""),
            );
            row_conference.set_text(
                ical_property(component, "CONFERENCE")
                    .or_else(|| ical_property(component, "X-CONFERENCE"))
                    .as_deref()
                    .unwrap_or(""),
            );
            let status_idx = match ical_property(component, "STATUS")
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str()
            {
                "CONFIRMED" => 1,
                "TENTATIVE" => 2,
                "CANCELLED" => 3,
                _ => 0,
            };
            dropdown_status.set_selected(status_idx);
            let transp_idx = (ical_property(component, "TRANSP")
                .map(|value| value.eq_ignore_ascii_case("TRANSPARENT"))
                .unwrap_or(false)) as u32;
            dropdown_transparency.set_selected(transp_idx);
            let all_day = component_property_has_date_value(component, "DTSTART");
            row_allday.set_active(all_day);
            let start_explicit_tz = crate::services::ical_time_service::property_explicit_tzid(component, "DTSTART");
            let end_explicit_tz = crate::services::ical_time_service::property_explicit_tzid(component, "DTEND");
            if let Some(ref stz) = start_explicit_tz {
                entry_tz.set_text(stz);
                entry_end_tz.set_text(stz);
            }
            if let Some(ref etz) = end_explicit_tz {
                entry_end_tz.set_text(etz);
                if start_explicit_tz.as_deref() != Some(etz.as_str()) {
                    btn_diff_end_tz.set_active(true);
                }
            }
            let start_display_opt = if start_explicit_tz.is_some() {
                crate::services::ical_time_service::property_wallclock_display(component, "DTSTART", all_day)
                    .or_else(|| crate::services::ical_time_service::property_local_display(component, "DTSTART", all_day))
            } else {
                crate::services::ical_time_service::property_local_display(component, "DTSTART", all_day)
            };
            if let Some(start_disp) = start_display_opt {
                let (d, t) = split_datetime_for_entries(&start_disp);
                let d_fmt = parse_date_lenient(&d).map(|dt| {
                    prev_start_date_cell.set(dt);
                    format_date_for_locale_entry(dt)
                }).unwrap_or(d);
                entry_start_date.set_text(&d_fmt);
                entry_start_time.set_text(&t);
            } else if let Some(start) = ical_property(component, "DTSTART") {
                let formatted = format_event_datetime_for_editor(&start, all_day);
                let (d, t) = split_datetime_for_entries(&formatted);
                let d_fmt = parse_date_lenient(&d).map(|dt| {
                    prev_start_date_cell.set(dt);
                    format_date_for_locale_entry(dt)
                }).unwrap_or(d);
                entry_start_date.set_text(&d_fmt);
                entry_start_time.set_text(&t);
            }
            let end_display_opt = if end_explicit_tz.is_some() {
                crate::services::ical_time_service::property_wallclock_display(component, "DTEND", all_day)
                    .or_else(|| crate::services::ical_time_service::property_local_display(component, "DTEND", all_day))
            } else {
                crate::services::ical_time_service::property_local_display(component, "DTEND", all_day)
            };
            if let Some(end_disp) = end_display_opt {
                let (d, t) = split_datetime_for_entries(&end_disp);
                let d_fmt = parse_date_lenient(&d).map(format_date_for_locale_entry).unwrap_or(d);
                entry_end_date.set_text(&d_fmt);
                entry_end_time.set_text(&t);
            } else if let Some(end) = ical_property(component, "DTEND") {
                let formatted = format_event_datetime_for_editor(&end, all_day);
                let (d, t) = split_datetime_for_entries(&formatted);
                let d_fmt = parse_date_lenient(&d).map(format_date_for_locale_entry).unwrap_or(d);
                entry_end_date.set_text(&d_fmt);
                entry_end_time.set_text(&t);
            }
            let rdates_str = ical_properties(component, "RDATE").join(",");
            let exdates_str = ical_properties(component, "EXDATE").join(",");
            row_rdates.set_text(&rdates_str);
            row_exdates.set_text(&exdates_str);
            row_recurrence.set_text(&ical_property(component, "RRULE").unwrap_or_default());
            if !rdates_str.trim().is_empty() || !exdates_str.trim().is_empty() {
                row_recurrence_preset.set_selected(6);
                row_recurrence.set_visible(true);
                row_rdates.set_visible(true);
                row_exdates.set_visible(true);
            }
            let parsed_attendees = parse_component_attendees(component);
            row_attendees.set_text(&parsed_attendees.individuals.join(", "));
            let rooms_str = parsed_attendees.rooms.join(", ");
            entry_room.set_text(&rooms_str);
            let categories_str = ical_property(component, "CATEGORIES").unwrap_or_default();
            entry_categories.set_text(&categories_str);
            let is_private = ical_property(component, "CLASS")
                .map(|value| value.eq_ignore_ascii_case("PRIVATE"))
                .unwrap_or(false);
            row_private.set_active(is_private);
            if status_idx != 1
                || transp_idx != 0
                || btn_diff_end_tz.is_active()
                || !rooms_str.is_empty()
                || !categories_str.is_empty()
                || is_private
            {
                expander_advanced.set_expanded(true);
            }
            if let Some(description) = ical_property(component, "DESCRIPTION") {
                text_desc.buffer().set_text(&description);
            }
        } else if let Some(summary) = self.imp().initial_summary.borrow().as_deref() {
            let trimmed = summary.trim();
            if !trimmed.is_empty() {
                entry_title.set_text(trimmed);
                self.set_title(&format!("{} — {trimmed}", gettext("Event")));
                window_title.set_subtitle(trimmed);
            }
        }

        let populate_calendar_picker = {
            let selected_source_uid = Rc::clone(&selected_source_uid);
            let selected_source_color = Rc::clone(&selected_source_color);
            let selected_source_name = Rc::clone(&selected_source_name);
            let btn_calendar_label = btn_calendar_label.clone();
            let btn_calendar_chip = btn_calendar_chip.clone();
            let calendar_popover = calendar_popover.clone();
            let calendar_popover_box = calendar_popover_box.clone();
            let color_badge_provider = Rc::clone(&color_badge_provider);
            let edit_source_uid = edit_source_uid.clone();
            let update_timeline = update_timeline.clone();
            Rc::new(move |raw_sources: Vec<eds_service::EdsSource>| {
                let active_sources: Vec<_> = raw_sources
                    .into_iter()
                    .filter(|s| {
                        let is_edit_target = edit_source_uid
                            .as_deref()
                            .map(|uid| uid == s.uid)
                            .unwrap_or(false);
                        (s.enabled && s.writable && s.backend_name != "contacts" && s.uid != "birthdays")
                            || is_edit_target
                    })
                    .collect();

                struct TempItem {
                    uid: String,
                    clean_name: String,
                    group: String,
                    color: String,
                }

                let mut temp_items: Vec<TempItem> = Vec::new();
                for source in &active_sources {
                    let color = crate::views::calendar_page::calendar_source_color(&source.uid, &source.display_name);
                    let group = if let Some(parent) = source.parent_name.as_deref().filter(|s| !s.trim().is_empty()) {
                        parent.trim().to_string()
                    } else if source.uid == "system-calendar" || source.backend_name == "local" {
                        "On This Computer".to_string()
                    } else {
                        "Other".to_string()
                    };
                    let clean_name = clean_calendar_display_name(&source.display_name, &group);
                    temp_items.push(TempItem {
                        uid: source.uid.clone(),
                        clean_name,
                        group,
                        color,
                    });
                }

                if active_sources.is_empty() {
                    for collection in crate::services::eds_service::load_configured_account_collections() {
                        if !collection.calendar_enabled {
                            continue;
                        }
                        let account_name = collection.display_name.clone();
                        let cal_uid = format!("account-calendar-{}", collection.uid);
                        let color = crate::views::calendar_page::calendar_source_color(&collection.uid, &account_name);
                        let group = if !account_name.is_empty() {
                            account_name
                        } else {
                            "Accounts".to_string()
                        };
                        temp_items.push(TempItem {
                            uid: cal_uid,
                            clean_name: "Calendar".to_string(),
                            group,
                            color,
                        });
                    }

                    if crate::services::calendar_store_service::CalendarStoreService::new().is_writable() {
                        let color = crate::views::calendar_page::calendar_source_color("local-calendar", "Local Calendar");
                        temp_items.push(TempItem {
                            uid: "local-calendar".to_string(),
                            clean_name: "Local Calendar".to_string(),
                            group: "On This Computer".to_string(),
                            color,
                        });
                    }
                }

                let mut name_counts = std::collections::HashMap::new();
                for it in &temp_items {
                    *name_counts.entry(it.clean_name.clone()).or_insert(0) += 1;
                }

                #[derive(Clone)]
                struct CalendarPickerItem {
                    uid: String,
                    display_name: String,
                    picker_label: String,
                    group: String,
                    color: String,
                }

                let mut items: Vec<CalendarPickerItem> = temp_items
                    .into_iter()
                    .map(|it| {
                        let picker_label = if name_counts.get(&it.clean_name).copied().unwrap_or(0) > 1 {
                            format!("{} ({})", it.clean_name, it.group)
                        } else {
                            it.clean_name.clone()
                        };
                        CalendarPickerItem {
                            uid: it.uid,
                            display_name: it.clean_name,
                            picker_label,
                            group: it.group,
                            color: it.color,
                        }
                    })
                    .collect();

                items.sort_by(|a, b| {
                    calendar_group_sort_key(&a.group)
                        .cmp(&calendar_group_sort_key(&b.group))
                        .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
                        .then_with(|| a.uid.cmp(&b.uid))
                });

                let current_sel = selected_source_uid.borrow().clone();
                let selected_idx = if !current_sel.is_empty() {
                    items.iter().position(|it| it.uid == current_sel).unwrap_or(0)
                } else if let Some(edit_source_uid) = edit_source_uid.as_deref() {
                    items.iter().position(|it| it.uid == edit_source_uid).unwrap_or(0)
                } else if let Some(settings) = AppSettings::try_new() {
                    let default_uid = settings.default_calendar_source_uid();
                    if !default_uid.is_empty() {
                        items.iter().position(|it| it.uid == default_uid).unwrap_or(0)
                    } else {
                        0
                    }
                } else {
                    0
                };

                if let Some(selected_item) = items.get(selected_idx).cloned() {
                    selected_source_uid.replace(selected_item.uid.clone());
                    selected_source_color.replace(selected_item.color.clone());
                    selected_source_name.replace(selected_item.picker_label.clone());
                    btn_calendar_label.set_text(&selected_item.picker_label);

                    let chip_css = format!(
                        "box {{ background-color: {}; min-width: 12px; min-height: 12px; border-radius: 2px; }}",
                        selected_item.color
                    );
                    let prov = gtk::CssProvider::new();
                    prov.load_from_string(&chip_css);
                    #[allow(deprecated)]
                    btn_calendar_chip.style_context().add_provider(&prov, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
                }

                while let Some(child) = calendar_popover_box.first_child() {
                    calendar_popover_box.remove(&child);
                }

                let check_icons: Rc<RefCell<Vec<(String, gtk::Image)>>> = Rc::new(RefCell::new(Vec::new()));
                let mut current_group = String::new();
                let mut is_first_group = true;

                for item in &items {
                    if item.group != current_group {
                        current_group = item.group.clone();
                        let header_label = gtk::Label::builder()
                            .label(&current_group)
                            .xalign(0.0)
                            .css_classes(["calendar-picker-group-label"])
                            .margin_start(10)
                            .margin_end(10)
                            .margin_top(if is_first_group { 4 } else { 10 })
                            .margin_bottom(2)
                            .build();
                        calendar_popover_box.append(&header_label);
                        is_first_group = false;
                    }

                    let item_btn = gtk::Button::builder()
                        .css_classes(["flat", "calendar-picker-item-button"])
                        .build();

                    let row_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(8)
                        .margin_start(4)
                        .margin_end(4)
                        .build();

                    let chip = gtk::Box::builder()
                        .css_classes(["calendar-picker-color-chip"])
                        .valign(gtk::Align::Center)
                        .build();
                    let prov = gtk::CssProvider::new();
                    prov.load_from_string(&format!(
                        "box {{ background-color: {}; min-width: 12px; min-height: 12px; border-radius: 2px; }}",
                        item.color
                    ));
                    #[allow(deprecated)]
                    chip.style_context().add_provider(&prov, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
                    row_box.append(&chip);

                    let title_label = gtk::Label::builder()
                        .label(&item.display_name)
                        .xalign(0.0)
                        .hexpand(true)
                        .build();
                    row_box.append(&title_label);

                    let is_selected = selected_source_uid.borrow().as_str() == item.uid;
                    let check_icon = gtk::Image::builder()
                        .icon_name("object-select-symbolic")
                        .visible(is_selected)
                        .build();
                    row_box.append(&check_icon);
                    check_icons.borrow_mut().push((item.uid.clone(), check_icon));

                    item_btn.set_child(Some(&row_box));

                    let item_uid = item.uid.clone();
                    let item_color = item.color.clone();
                    let item_picker_label = item.picker_label.clone();
                    let sel_uid = Rc::clone(&selected_source_uid);
                    let sel_color = Rc::clone(&selected_source_color);
                    let sel_name = Rc::clone(&selected_source_name);
                    let btn_lbl = btn_calendar_label.clone();
                    let btn_chip = btn_calendar_chip.clone();
                    let color_prov = Rc::clone(&color_badge_provider);
                    let icons_ref = Rc::clone(&check_icons);
                    let pop = calendar_popover.clone();
                    let update_tl = update_timeline.clone();

                    item_btn.connect_clicked(move |_| {
                        sel_uid.replace(item_uid.clone());
                        sel_color.replace(item_color.clone());
                        sel_name.replace(item_picker_label.clone());
                        btn_lbl.set_text(&item_picker_label);

                        let chip_css = format!(
                            "box {{ background-color: {}; min-width: 12px; min-height: 12px; border-radius: 2px; }}",
                            item_color
                        );
                        let prov = gtk::CssProvider::new();
                        prov.load_from_string(&chip_css);
                        #[allow(deprecated)]
                        btn_chip.style_context().add_provider(&prov, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

                        if let Some(display) = gtk::gdk::Display::default() {
                            let mut prov_slot = color_prov.borrow_mut();
                            let css = format!(
                                "box.appointment-color-badge {{ background-color: {}; min-width: 14px; min-height: 14px; border-radius: 9999px; margin-end: 6px; }}",
                                item_color
                            );
                            if let Some(ref p) = *prov_slot {
                                p.load_from_string(&css);
                            } else {
                                let p = gtk::CssProvider::new();
                                p.load_from_string(&css);
                                gtk::style_context_add_provider_for_display(
                                    &display,
                                    &p,
                                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                                );
                                *prov_slot = Some(p);
                            }
                        }

                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_default_calendar_source_uid(&item_uid);
                        }

                        for (uid, icon) in icons_ref.borrow().iter() {
                            icon.set_visible(uid == &item_uid);
                        }

                        update_tl();
                        pop.popdown();
                    });

                    calendar_popover_box.append(&item_btn);
                }
            })
        };

        // Frame 0 instant population from fast cache + immediate timeline render
        let cached_sources = eds_service::load_cached_sources_fast(eds_service::EdsSourceKind::Calendar);
        populate_calendar_picker(cached_sources.clone());
        update_timeline();

        // Only query live sources/events asynchronously if cache was cold
        if cached_sources.is_empty() {
            let populate_for_discovery = Rc::clone(&populate_calendar_picker);
            let update_timeline_for_discovery = update_timeline.clone();
            glib::spawn_future_local(async move {
                let discovery = eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
                populate_for_discovery(discovery.sources);
                update_timeline_for_discovery();
            });
        }

        if events_cache.borrow().is_empty() {
            let events_cache_for_worker = Rc::clone(&events_cache);
            let update_timeline_for_worker = update_timeline.clone();
            let (events_sender, events_receiver) = async_channel::bounded(1);
            gio::spawn_blocking(move || {
                let result = load_snapshot_prefer_eds();
                let _ = events_sender.send_blocking(result);
            });
            glib::spawn_future_local(async move {
                if let Ok(result) = events_receiver.recv().await {
                    *events_cache_for_worker.borrow_mut() = result.snapshot.events;
                    update_timeline_for_worker();
                }
            });
        }

        let perform_save = {
            let selected_source_uid = Rc::clone(&selected_source_uid);
            let dialog_weak = self.downgrade();
            let entry_title = entry_title.clone();
            let row_location = row_location.clone();
            let row_conference = row_conference.clone();
            let dropdown_status = dropdown_status.clone();
            let dropdown_transparency = dropdown_transparency.clone();
            let row_allday = row_allday.clone();
            let entry_start_date = entry_start_date.clone();
            let entry_start_time = entry_start_time.clone();
            let entry_end_date = entry_end_date.clone();
            let entry_end_time = entry_end_time.clone();
            let text_desc = text_desc.clone();
            let row_recurrence = row_recurrence.clone();
            let row_rdates = row_rdates.clone();
            let row_exdates = row_exdates.clone();
            let row_attendees = row_attendees.clone();
            let entry_room = entry_room.clone();
            let entry_categories = entry_categories.clone();
            let alarm_editor = alarm_editor.clone();
            let row_private = row_private.clone();
            let entry_tz = entry_tz.clone();
            let entry_end_tz = entry_end_tz.clone();
            let btn_diff_end_tz = btn_diff_end_tz.clone();

            Rc::new(move |close_after: bool| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let source_uid = if dialog.imp().edit_uid.borrow().is_some() {
                    dialog.imp().edit_source_uid.borrow().clone().unwrap_or_else(|| {
                        selected_source_uid.borrow().clone()
                    })
                } else {
                    let source_uid = selected_source_uid.borrow().clone();
                    if source_uid.is_empty() {
                        dialog.show_toast(&gettext("No active calendar is selected"));
                        return;
                    }
                    source_uid
                };

                let all_day = row_allday.is_active();
                let start_date_iso = date_to_iso(&entry_start_date.text())
                    .unwrap_or_else(|| entry_start_date.text().to_string());
                let end_date_iso = date_to_iso(&entry_end_date.text())
                    .unwrap_or_else(|| entry_end_date.text().to_string());
                let (start_val, end_val) = if all_day {
                    (start_date_iso, end_date_iso)
                } else {
                    let s_time = entry_start_time.text();
                    let e_time = entry_end_time.text();
                    (
                        format!("{start_date_iso} {}", s_time.trim()),
                        format!("{end_date_iso} {}", e_time.trim()),
                    )
                };
                let start_tz_str = entry_tz.text().to_string();
                let end_tz_str = entry_end_tz.text().to_string();
                let end_tz_opt = btn_diff_end_tz.is_active().then_some(end_tz_str.as_str());
                let end_val_for_builder = match prepare_end_val_for_timezones(
                    all_day,
                    &start_val,
                    &end_val,
                    &start_tz_str,
                    end_tz_opt,
                ) {
                    Ok(v) => v,
                    Err(error) => {
                        dialog.show_toast(&error);
                        return;
                    }
                };

                alarm_editor.set_summary(&entry_title.text());
                let alarms = alarm_editor.alarms();
                let status = match dropdown_status.selected() {
                    1 => "CONFIRMED",
                    2 => "TENTATIVE",
                    3 => "CANCELLED",
                    _ => "CONFIRMED",
                };
                let description = text_desc.buffer().text(
                    &text_desc.buffer().start_iter(),
                    &text_desc.buffer().end_iter(),
                    false,
                );

                let current_edit_component = dialog.imp().edit_component.borrow().clone();
                let current_edit_uid = dialog.imp().edit_uid.borrow().clone();

                let component = match (
                    current_edit_component.as_deref(),
                    current_edit_uid.as_deref(),
                ) {
                    (Some(original), Some(uid)) => update_existing_event_component(
                        original,
                        uid,
                        &entry_title.text(),
                        &row_location.text(),
                        &row_conference.text(),
                        status,
                        dropdown_transparency.selected() == 1,
                        all_day,
                        &start_val,
                        &end_val_for_builder,
                        &description,
                        &row_recurrence.text(),
                        &row_rdates.text(),
                        &row_exdates.text(),
                        &row_attendees.text(),
                        &entry_categories.text(),
                        "",
                        &alarms,
                        row_private.is_active(),
                        &entry_room.text(),
                    ),
                    _ => build_event_component(
                        &entry_title.text(),
                        &row_location.text(),
                        &row_conference.text(),
                        status,
                        dropdown_transparency.selected() == 1,
                        all_day,
                        &start_val,
                        &end_val_for_builder,
                        &description,
                        &row_recurrence.text(),
                        &row_rdates.text(),
                        &row_exdates.text(),
                        &row_attendees.text(),
                        &entry_categories.text(),
                        "",
                        &alarms,
                        row_private.is_active(),
                        &entry_room.text(),
                    ),
                };

                let component = match component.and_then(|comp| {
                    apply_event_timezones_to_component(
                        &comp,
                        all_day,
                        &start_val,
                        &end_val,
                        &start_tz_str,
                        end_tz_opt,
                    )
                }) {
                    Ok(component) => component,
                    Err(error) => {
                        dialog.show_toast(&error);
                        return;
                    }
                };

                let category_names = split_ical_category_names(&entry_categories.text());
                if let Err(error) = CalendarCategoryStore::default().ensure_names(category_names) {
                    tracing::warn!(%error, "Could not persist calendar category metadata");
                }

                if let (Some(uid), Some(original)) = (
                    current_edit_uid.as_deref(),
                    current_edit_component.as_deref(),
                ) {
                    crate::services::ecal_service::register_known_ecal_component(
                        ECalComponentType::Event,
                        &source_uid,
                        uid,
                        original,
                    );
                }
                let source_uid_for_worker = source_uid.clone();
                let operation_uid = current_edit_uid.clone();
                let was_edit = current_edit_uid.is_some();
                let component_for_worker = component.clone();
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let service = ECalService::new();
                    let result = match operation_uid.as_deref() {
                        Some(uid) => service.modify_component(
                            &source_uid_for_worker,
                            ECalComponentType::Event,
                            uid,
                            &component_for_worker,
                        ),
                        None => service.create_component(
                            &source_uid_for_worker,
                            ECalComponentType::Event,
                            &component_for_worker,
                        ),
                    };
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Event mutation result was dropped");
                    }
                });

                let dialog_weak_for_result = dialog.downgrade();
                let source_uid_for_state = source_uid.clone();
                glib::spawn_future_local(async move {
                    let Ok(result) = receiver.recv().await else {
                        return;
                    };
                    let Some(dialog) = dialog_weak_for_result.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(result) => {
                            let message = if was_edit {
                                format!("{} ({})", gettext("Event updated"), result.uid)
                            } else {
                                format!("{} ({})", gettext("Event saved"), result.uid)
                            };
                            dialog.show_toast(&message);
                            dialog.imp().edit_uid.replace(Some(result.uid.clone()));
                            dialog
                                .imp()
                                .edit_source_uid
                                .replace(Some(source_uid_for_state));
                            if close_after {
                                dialog.close();
                            }
                        }
                        Err(error) => dialog
                            .show_toast(&format!("{}: {error}", gettext("Could not save event"))),
                    }
                });
            })
        };

        let save_fn = Rc::clone(&perform_save);
        btn_save.connect_clicked(move |_| save_fn(true));

        let split_dialog_weak = self.downgrade();
        let split_entry_title = entry_title.clone();
        let split_row_location = row_location.clone();
        let split_row_conference = row_conference.clone();
        let split_dropdown_status = dropdown_status.clone();
        let split_dropdown_transparency = dropdown_transparency.clone();
        let split_row_allday = row_allday.clone();
        let split_entry_start_date = entry_start_date.clone();
        let split_entry_start_time = entry_start_time.clone();
        let split_entry_end_date = entry_end_date.clone();
        let split_entry_end_time = entry_end_time.clone();
        let split_text_desc = text_desc.clone();
        let split_row_recurrence = row_recurrence.clone();
        let split_row_rdates = row_rdates.clone();
        let split_row_exdates = row_exdates.clone();
        let split_row_attendees = row_attendees.clone();
        let split_entry_categories = entry_categories.clone();
        let split_alarm_editor = alarm_editor.clone();
        let split_row_private = row_private.clone();
        let split_entry_room = entry_room.clone();
        let split_entry_tz = entry_tz.clone();
        let split_entry_end_tz = entry_end_tz.clone();
        let split_btn_diff_end_tz = btn_diff_end_tz.clone();
        let split_selected_source_uid = Rc::clone(&selected_source_uid);

        let split_fn: Rc<dyn Fn()> = Rc::new(move || {
            let Some(dialog) = split_dialog_weak.upgrade() else {
                return;
            };

            let source_uid = if dialog.imp().edit_uid.borrow().is_some() {
                dialog.imp().edit_source_uid.borrow().clone().unwrap_or_else(|| {
                    split_selected_source_uid.borrow().clone()
                })
            } else {
                split_selected_source_uid.borrow().clone()
            };

            let all_day = split_row_allday.is_active();
            let start_date_iso = date_to_iso(&split_entry_start_date.text())
                .unwrap_or_else(|| split_entry_start_date.text().to_string());
            let end_date_iso = date_to_iso(&split_entry_end_date.text())
                .unwrap_or_else(|| split_entry_end_date.text().to_string());
            let (start_val, end_val) = if all_day {
                (start_date_iso.clone(), end_date_iso.clone())
            } else {
                let s_time = split_entry_start_time.text();
                let e_time = split_entry_end_time.text();
                (
                    format!("{start_date_iso} {}", s_time.trim()),
                    format!("{end_date_iso} {}", e_time.trim()),
                )
            };
            let start_tz_str = split_entry_tz.text().to_string();
            let end_tz_str = split_entry_end_tz.text().to_string();
            let end_tz_opt = split_btn_diff_end_tz.is_active().then_some(end_tz_str.as_str());
            let end_val_for_builder = prepare_end_val_for_timezones(
                all_day,
                &start_val,
                &end_val,
                &start_tz_str,
                end_tz_opt,
            )
            .unwrap_or_else(|_| end_val.clone());

            split_alarm_editor.set_summary(&split_entry_title.text());
            let alarms = split_alarm_editor.alarms();
            let status = match split_dropdown_status.selected() {
                1 => "CONFIRMED",
                2 => "TENTATIVE",
                3 => "CANCELLED",
                _ => "CONFIRMED",
            };
            let description = split_text_desc.buffer().text(
                &split_text_desc.buffer().start_iter(),
                &split_text_desc.buffer().end_iter(),
                false,
            );

            let current_edit_component = dialog.imp().edit_component.borrow().clone();
            let current_edit_uid = dialog.imp().edit_uid.borrow().clone();

            let raw_title = split_entry_title.text();
            let has_title = !raw_title.trim().is_empty();
            let title_for_split = if has_title {
                raw_title.as_str()
            } else {
                " "
            };

            let component_res = match (
                current_edit_component.as_deref(),
                current_edit_uid.as_deref(),
            ) {
                (Some(original), Some(uid)) => update_existing_event_component(
                    original,
                    uid,
                    title_for_split,
                    &split_row_location.text(),
                    &split_row_conference.text(),
                    status,
                    split_dropdown_transparency.selected() == 1,
                    all_day,
                    &start_val,
                    &end_val_for_builder,
                    &description,
                    &split_row_recurrence.text(),
                    &split_row_rdates.text(),
                    &split_row_exdates.text(),
                    &split_row_attendees.text(),
                    &split_entry_categories.text(),
                    "",
                    &alarms,
                    split_row_private.is_active(),
                    &split_entry_room.text(),
                ),
                _ => build_event_component(
                    title_for_split,
                    &split_row_location.text(),
                    &split_row_conference.text(),
                    status,
                    split_dropdown_transparency.selected() == 1,
                    all_day,
                    &start_val,
                    &end_val_for_builder,
                    &description,
                    &split_row_recurrence.text(),
                    &split_row_rdates.text(),
                    &split_row_exdates.text(),
                    &split_row_attendees.text(),
                    &split_entry_categories.text(),
                    "",
                    &alarms,
                    split_row_private.is_active(),
                    &split_entry_room.text(),
                ),
            }
            .and_then(|comp| {
                apply_event_timezones_to_component(
                    &comp,
                    all_day,
                    &start_val,
                    &end_val,
                    &start_tz_str,
                    end_tz_opt,
                )
            });

            let stored_parent = dialog.imp().stored_parent.borrow().clone();
            dialog.imp().close_confirmed.set(true);
            dialog.close();

            let standalone: EventDialog = if let Ok(component) = component_res {
                let summary = if has_title {
                    raw_title.trim().to_string()
                } else {
                    gettext("No Summary")
                };

                let mut builder = glib::Object::builder()
                    .property("title", format!("{} — {summary}", gettext("Event")))
                    .property("content-width", 880)
                    .property("content-height", 680)
                    .property("edit-component", &component);
                if !source_uid.is_empty() {
                    builder = builder.property("edit-source-uid", &source_uid);
                }
                if let Some(uid) = current_edit_uid.as_deref() {
                    builder = builder.property("edit-uid", uid);
                }
                builder.build()
            } else {
                let mut builder = glib::Object::builder()
                    .property("title", gettext("Event — No Summary"))
                    .property("content-width", 880)
                    .property("content-height", 680)
                    .property("initial-start", format!("{start_date_iso} {}", split_entry_start_time.text().trim()))
                    .property("initial-end", format!("{end_date_iso} {}", split_entry_end_time.text().trim()))
                    .property("initial-date", start_date_iso)
                    .property("initial-allday", all_day);
                if !source_uid.is_empty() {
                    builder = builder.property("edit-source-uid", &source_uid);
                }
                builder.build()
            };

            if let Some(parent) = stored_parent.as_ref() {
                if let Some(win) = crate::window::find_meridian_window(parent) {
                    standalone.set_transient_for(Some(&win));
                    let win_downgrade = win.downgrade();
                    standalone.connect_closed(move |_| {
                        if let Some(w) = win_downgrade.upgrade() {
                            w.refresh_calendar_page();
                        }
                    });
                }
            }

            standalone.present_standalone();
        });

        self.imp().split_handler.replace(Some(split_fn));
    }

    fn show_toast(&self, message: &str) {
        if let Some(parent) = self.imp().stored_parent.borrow().as_ref() {
            if let Some(win) = crate::window::find_meridian_window(parent) {
                win.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
        } else {
            tracing::warn!(%message, "ECal dialog result");
        }
    }
}

impl Default for EventDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn format_alarm_display_label(alarm: &Alarm) -> String {
    let trigger_text = match &alarm.trigger {
        AlarmTrigger::Relative {
            relation,
            offset_seconds,
            ..
        } => {
            if *offset_seconds == 0 {
                match relation {
                    AlarmRelation::Start => gettext("At time of event"),
                    AlarmRelation::End => gettext("At end of event"),
                }
            } else {
                let raw = alarm.trigger.label();
                match relation {
                    AlarmRelation::Start => raw.strip_suffix(" start").unwrap_or(&raw).to_string(),
                    AlarmRelation::End => format!("{raw} {}", gettext("of event")),
                }
            }
        }
        AlarmTrigger::Absolute { value, .. } => format!("{} {value}", gettext("On")),
        AlarmTrigger::Unknown { .. } => gettext("Custom trigger"),
    };
    match &alarm.action {
        AlarmAction::Display => trigger_text,
        other => format!("{}: {}", other.label(), trigger_text),
    }
}

/// Shared GTK4 reminder page used by event, task and memo editors.  The
/// service owns the VALARM value model; this small widget owns only staged
/// ordering and editor state.  Every visible control below changes that
/// staged list or opens a real editor, so imported non-DISPLAY alarms are
/// never represented by a decorative, non-functional button.
#[derive(Clone)]
pub(crate) struct AlarmEditor {
    root: gtk::Box,
    alarms: Rc<RefCell<Vec<Alarm>>>,
    list: gtk::ListBox,
    summary: Rc<RefCell<String>>,
    on_changed: Rc<RefCell<Vec<Box<dyn Fn(&[Alarm])>>>>,
}

impl AlarmEditor {
    pub(crate) fn new(component: Option<&str>, summary: &str) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_vexpand(false);
        list.add_css_class("alarm-editor-list");
        root.append(&list);

        let editor = Self {
            root,
            alarms: Rc::new(RefCell::new(
                component.map(parse_alarms).unwrap_or_default(),
            )),
            list,
            summary: Rc::new(RefCell::new(summary.to_string())),
            on_changed: Rc::new(RefCell::new(Vec::new())),
        };

        editor.rebuild(None);
        editor
    }

    pub(crate) fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub(crate) fn alarms(&self) -> Vec<Alarm> {
        self.alarms.borrow().clone()
    }

    pub(crate) fn summary_label(&self) -> String {
        let alarms = self.alarms.borrow();
        match alarms.len() {
            0 => gettext("None"),
            1 => format_alarm_display_label(&alarms[0]),
            n => format!(
                "{} (+{} {})",
                format_alarm_display_label(&alarms[0]),
                n - 1,
                gettext("more")
            ),
        }
    }

    pub(crate) fn connect_changed(&self, f: impl Fn(&[Alarm]) + 'static) {
        self.on_changed.borrow_mut().push(Box::new(f));
    }

    pub(crate) fn set_component(&self, component: Option<&str>, summary: &str) {
        self.alarms
            .replace(component.map(parse_alarms).unwrap_or_default());
        self.summary.replace(summary.to_string());
        self.rebuild(None);
    }

    pub(crate) fn set_summary(&self, summary: &str) {
        self.summary.replace(summary.to_string());
    }

    pub(crate) fn add_default_display_if_empty(&self, minutes: u32) {
        if minutes == 0 || !self.alarms.borrow().is_empty() {
            return;
        }
        self.alarms.borrow_mut().push(Alarm::new_display_relative(
            AlarmRelation::Start,
            -i64::from(minutes) * 60,
            None,
        ));
        self.rebuild(Some(0));
    }

    fn rebuild(&self, _select_index: Option<usize>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let len = self.alarms.borrow().len();
        let current_alarms = self.alarms.borrow().clone();
        for (idx, alarm) in current_alarms.iter().enumerate() {
            let subtitle = if alarm.is_safe_to_notify() {
                alarm
                    .message
                    .as_deref()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or("")
                    .to_string()
            } else {
                gettext("Preserved from calendar; not executed automatically")
            };
            let row = adw::ActionRow::builder()
                .title(format_alarm_display_label(alarm))
                .subtitle(&subtitle)
                .activatable(true)
                .build();
            let bell_icon = gtk::Image::builder()
                .icon_name("alarm-symbolic")
                .css_classes(["dim-label"])
                .build();
            row.add_prefix(&bell_icon);

            let editor_for_edit = self.clone();
            row.connect_activated(move |_| {
                editor_for_edit.present_alarm_dialog(Some(idx));
            });

            if len > 1 {
                let up_btn = gtk::Button::builder()
                    .icon_name("go-up-symbolic")
                    .tooltip_text(gettext("Move up"))
                    .css_classes(["flat"])
                    .valign(gtk::Align::Center)
                    .sensitive(idx > 0)
                    .build();
                let editor_for_up = self.clone();
                up_btn.connect_clicked(move |_| {
                    if idx > 0 {
                        editor_for_up.alarms.borrow_mut().swap(idx, idx - 1);
                        editor_for_up.rebuild(Some(idx - 1));
                    }
                });
                row.add_suffix(&up_btn);

                let down_btn = gtk::Button::builder()
                    .icon_name("go-down-symbolic")
                    .tooltip_text(gettext("Move down"))
                    .css_classes(["flat"])
                    .valign(gtk::Align::Center)
                    .sensitive(idx + 1 < len)
                    .build();
                let editor_for_down = self.clone();
                down_btn.connect_clicked(move |_| {
                    if idx + 1 < editor_for_down.alarms.borrow().len() {
                        editor_for_down.alarms.borrow_mut().swap(idx, idx + 1);
                        editor_for_down.rebuild(Some(idx + 1));
                    }
                });
                row.add_suffix(&down_btn);
            }

            let remove_btn = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(gettext("Remove reminder"))
                .css_classes(["flat"])
                .valign(gtk::Align::Center)
                .build();
            let editor_for_remove = self.clone();
            remove_btn.connect_clicked(move |_| {
                if idx < editor_for_remove.alarms.borrow().len() {
                    editor_for_remove.alarms.borrow_mut().remove(idx);
                    editor_for_remove.rebuild(None);
                }
            });
            row.add_suffix(&remove_btn);

            self.list.append(&row);
        }

        let add_row = adw::ActionRow::builder()
            .title(gettext("Add Reminder…"))
            .activatable(true)
            .build();
        let add_icon = gtk::Image::from_icon_name("list-add-symbolic");
        add_row.add_prefix(&add_icon);
        let editor_for_add = self.clone();
        add_row.connect_activated(move |_| {
            editor_for_add.present_alarm_dialog(None);
        });
        self.list.append(&add_row);

        let snapshot = self.alarms.borrow().clone();
        for cb in self.on_changed.borrow().iter() {
            cb(&snapshot);
        }
    }

    fn present_alarm_dialog(&self, index: Option<usize>) {
        let existing = index.and_then(|index| self.alarms.borrow().get(index).cloned());
        let initial = existing
            .clone()
            .unwrap_or_else(|| Alarm::new_display_relative(AlarmRelation::Start, -15 * 60, None));
        let window = adw::Dialog::builder()
            .title(&if index.is_some() {
                gettext("Edit Reminder")
            } else {
                gettext("Add Reminder")
            })
            .content_width(440)
            .content_height(460)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);

        let cancel_btn = gtk::Button::builder()
            .label(gettext("Cancel"))
            .build();
        let window_for_cancel = window.clone();
        cancel_btn.connect_clicked(move |_| {
            window_for_cancel.close();
        });
        header.pack_start(&cancel_btn);

        let save = gtk::Button::builder()
            .label(gettext("Save"))
            .css_classes(["suggested-action"])
            .build();
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(18)
            .margin_end(18)
            .margin_top(14)
            .margin_bottom(18)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content)
            .build();
        toolbar.set_content(Some(&scroll));
        window.set_child(Some(&toolbar));

        // ── Group 1: Main Reminder Settings (inside PreferencesGroup so all ComboRows work!) ──
        let general_group = adw::PreferencesGroup::new();

        const PRESET_OFFSETS: &[Option<i64>] = &[
            Some(0),
            Some(-5 * 60),
            Some(-10 * 60),
            Some(-15 * 60),
            Some(-30 * 60),
            Some(-60 * 60),
            Some(-2 * 60 * 60),
            Some(-24 * 60 * 60),
            Some(-2 * 24 * 60 * 60),
            Some(-7 * 24 * 60 * 60),
            None, // Custom
        ];
        let preset_model = gtk::StringList::new(&[
            &gettext("At time of event"),
            &gettext("5 minutes before"),
            &gettext("10 minutes before"),
            &gettext("15 minutes before"),
            &gettext("30 minutes before"),
            &gettext("1 hour before"),
            &gettext("2 hours before"),
            &gettext("1 day before"),
            &gettext("2 days before"),
            &gettext("1 week before"),
            &gettext("Custom…"),
        ]);
        let preset_row = adw::ComboRow::builder()
            .title(gettext("When"))
            .model(&preset_model)
            .build();
        let initial_preset_idx = match &initial.trigger {
            AlarmTrigger::Relative {
                relation: AlarmRelation::Start,
                offset_seconds,
                ..
            } => PRESET_OFFSETS
                .iter()
                .position(|candidate| *candidate == Some(*offset_seconds))
                .unwrap_or(10) as u32,
            _ => 10,
        };
        preset_row.set_selected(initial_preset_idx);
        general_group.add(&preset_row);

        let action_values = Rc::new({
            let mut values = vec![
                AlarmAction::Display,
                AlarmAction::Audio,
                AlarmAction::Email,
                AlarmAction::Procedure,
            ];
            if let AlarmAction::Unknown(value) = &initial.action {
                values.push(AlarmAction::Unknown(value.clone()));
            }
            values
        });
        let action_labels = action_values
            .iter()
            .map(|act| match act {
                AlarmAction::Display => gettext("Notification"),
                AlarmAction::Audio => gettext("Sound"),
                AlarmAction::Email => gettext("Email"),
                AlarmAction::Procedure => gettext("Custom Command"),
                AlarmAction::Unknown(v) => format!("{}: {v}", gettext("Custom")),
            })
            .collect::<Vec<_>>();
        let action_label_refs = action_labels.iter().map(String::as_str).collect::<Vec<_>>();
        let action_model = gtk::StringList::new(&action_label_refs);
        let action_row = adw::ComboRow::builder()
            .title(gettext("Type"))
            .model(&action_model)
            .build();
        let action_index = action_values
            .iter()
            .position(|value| value == &initial.action)
            .unwrap_or(0);
        action_row.set_selected(action_index as u32);
        general_group.add(&action_row);

        let trigger_values = Rc::new({
            let mut values = vec![0_u8, 1_u8];
            if matches!(initial.trigger, AlarmTrigger::Unknown { .. }) {
                values.push(2);
            }
            values
        });
        let trigger_strings = [
            gettext("Relative to event"),
            gettext("Specific date & time"),
            gettext("Preserved trigger"),
        ];
        let trigger_labels = trigger_strings
            .iter()
            .take(trigger_values.len())
            .map(|s| s.as_str())
            .collect::<Vec<_>>();
        let trigger_model = gtk::StringList::new(&trigger_labels);
        let trigger_row = adw::ComboRow::builder()
            .title(gettext("Trigger Mode"))
            .model(&trigger_model)
            .build();
        let trigger_index = match initial.trigger {
            AlarmTrigger::Relative { .. } => 0,
            AlarmTrigger::Absolute { .. } => 1,
            AlarmTrigger::Unknown { .. } => 2,
        };
        trigger_row.set_selected(trigger_index);
        trigger_row.set_visible(initial_preset_idx == 10);
        general_group.add(&trigger_row);

        content.append(&general_group);

        let safety_note = gtk::Label::builder()
            .label(gettext("Sound, Email, and Custom Command reminders are preserved in the calendar file but not executed automatically."),
            )
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["warning", "dim-label"])
            .build();
        safety_note.set_visible(!initial.action.is_safe_to_notify());
        content.append(&safety_note);

        // ── Group 2: Custom Relative or Absolute Timing ──
        let relative_group = adw::PreferencesGroup::builder()
            .title(gettext("Custom Timing"))
            .build();
        let offset_row = adw::SpinRow::with_range(0.0, 10_000_000.0, 1.0);
        offset_row.set_title(&gettext("Offset"));
        offset_row.set_digits(0);
        offset_row.set_snap_to_ticks(true);
        let unit_model = gtk::StringList::new(&[&gettext("Minutes"), &gettext("Hours"), &gettext("Days")]);
        let unit_row = adw::ComboRow::builder()
            .title(gettext("Unit"))
            .model(&unit_model)
            .build();
        let direction_model = gtk::StringList::new(&[&gettext("Before"), &gettext("After")]);
        let direction_row = adw::ComboRow::builder()
            .title(gettext("Direction"))
            .model(&direction_model)
            .build();
        let relative_to_model = gtk::StringList::new(&[&gettext("Event start"), &gettext("Event end")]);
        let relative_to_row = adw::ComboRow::builder()
            .title(gettext("Relative to"))
            .model(&relative_to_model)
            .build();
        relative_group.add(&offset_row);
        relative_group.add(&unit_row);
        relative_group.add(&direction_row);
        relative_group.add(&relative_to_row);
        content.append(&relative_group);

        let absolute_group = adw::PreferencesGroup::builder()
            .title(gettext("Specific Date & Time"))
            .build();
        let absolute_entry = adw::EntryRow::builder()
            .title(gettext("Timestamp (YYYYMMDDTHHMMSSZ)"))
            .build();
        absolute_entry.set_tooltip_text(Some(&gettext("YYYYMMDDTHHMMSSZ or YYYYMMDDTHHMMSS")));
        absolute_group.add(&absolute_entry);
        content.append(&absolute_group);

        let unknown_trigger_label = gtk::Label::builder()
            .label(gettext("The imported trigger will be preserved unchanged when saving."))
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        content.append(&unknown_trigger_label);

        let (relation, offset) = match initial.trigger {
            AlarmTrigger::Relative {
                relation,
                offset_seconds,
                ..
            } => (relation, offset_seconds),
            _ => (AlarmRelation::Start, -15 * 60),
        };
        relative_to_row.set_selected(match relation {
            AlarmRelation::Start => 0,
            AlarmRelation::End => 1,
        });
        direction_row.set_selected((offset > 0) as u32);
        let (offset_value, offset_unit) = duration_editor_parts(offset.unsigned_abs());
        offset_row.set_value(offset_value as f64);
        unit_row.set_selected(offset_unit);
        if let AlarmTrigger::Absolute { value, .. } = &initial.trigger {
            absolute_entry.set_text(value);
        }

        let is_custom = initial_preset_idx == 10;
        relative_group.set_visible(is_custom && trigger_index == 0);
        absolute_group.set_visible(is_custom && trigger_index == 1);
        unknown_trigger_label.set_visible(is_custom && trigger_index == 2);

        let syncing_preset = Rc::new(Cell::new(false));
        {
            let syncing_preset = Rc::clone(&syncing_preset);
            let trigger_row = trigger_row.clone();
            let relative_group = relative_group.clone();
            let absolute_group = absolute_group.clone();
            let unknown_trigger_label = unknown_trigger_label.clone();
            let offset_row = offset_row.clone();
            let unit_row = unit_row.clone();
            let direction_row = direction_row.clone();
            let relative_to_row = relative_to_row.clone();
            preset_row.connect_selected_notify(move |row| {
                if syncing_preset.get() {
                    return;
                }
                syncing_preset.set(true);
                let sel = row.selected() as usize;
                if let Some(Some(preset_sec)) = PRESET_OFFSETS.get(sel) {
                    trigger_row.set_selected(0);
                    trigger_row.set_visible(false);
                    relative_group.set_visible(false);
                    absolute_group.set_visible(false);
                    unknown_trigger_label.set_visible(false);
                    relative_to_row.set_selected(0);
                    direction_row.set_selected(0);
                    let (val, unit) = duration_editor_parts(preset_sec.unsigned_abs());
                    offset_row.set_value(val as f64);
                    unit_row.set_selected(unit);
                } else {
                    trigger_row.set_visible(true);
                    let trig_sel = trigger_row.selected();
                    relative_group.set_visible(trig_sel == 0);
                    absolute_group.set_visible(trig_sel == 1);
                    unknown_trigger_label.set_visible(trig_sel == 2);
                }
                syncing_preset.set(false);
            });
        }

        let relative_group_for_trigger = relative_group.clone();
        let absolute_group_for_trigger = absolute_group.clone();
        let unknown_trigger_for_trigger = unknown_trigger_label.clone();
        let preset_for_trigger = preset_row.clone();
        trigger_row.connect_selected_notify(move |row| {
            let is_custom_mode = preset_for_trigger.selected() == 10;
            let selected = row.selected();
            relative_group_for_trigger.set_visible(is_custom_mode && selected == 0);
            absolute_group_for_trigger.set_visible(is_custom_mode && selected == 1);
            unknown_trigger_for_trigger.set_visible(is_custom_mode && selected == 2);
        });

        // ── Group 3: Advanced / Repeat & Message ──
        let options_group = adw::PreferencesGroup::new();
        let has_advanced_initial = initial.repeat.unwrap_or(0) > 0
            || initial.message.as_deref().is_some_and(|m| !m.trim().is_empty())
            || !initial.attachments.is_empty()
            || !initial.attendees.is_empty();
        let advanced_expander = adw::ExpanderRow::builder()
            .title(gettext("More Options"))
            .subtitle(gettext("Custom message and repeat interval"))
            .show_enable_switch(false)
            .expanded(has_advanced_initial)
            .build();

        let message_entry = adw::EntryRow::builder()
            .title(gettext("Custom Message"))
            .show_apply_button(false)
            .build();
        if let Some(message) = initial.message.as_deref() {
            message_entry.set_text(message);
        }
        advanced_expander.add_row(&message_entry);

        let repeat_row = adw::SpinRow::with_range(0.0, 4_096.0, 1.0);
        repeat_row.set_title(&gettext("Repeat Count"));
        repeat_row.set_digits(0);
        repeat_row.set_snap_to_ticks(true);
        repeat_row.set_value(initial.repeat.unwrap_or(0) as f64);
        advanced_expander.add_row(&repeat_row);

        let repeat_interval_row = adw::SpinRow::with_range(0.0, 10_000_000.0, 1.0);
        repeat_interval_row.set_title(&gettext("Repeat Interval"));
        repeat_interval_row.set_digits(0);
        repeat_interval_row.set_snap_to_ticks(true);
        let repeat_unit_model = gtk::StringList::new(&[&gettext("Minutes"), &gettext("Hours"), &gettext("Days")]);
        let repeat_unit_row = adw::ComboRow::builder()
            .title(gettext("Interval Unit"))
            .model(&repeat_unit_model)
            .build();
        if let Some(duration) = &initial.duration {
            let (value, unit) = duration_editor_parts(duration.seconds.unsigned_abs());
            repeat_interval_row.set_value(value as f64);
            repeat_unit_row.set_selected(unit);
        } else {
            repeat_interval_row.set_value(5.0);
        }
        advanced_expander.add_row(&repeat_interval_row);
        advanced_expander.add_row(&repeat_unit_row);

        let attachments_entry = adw::EntryRow::builder()
            .title(gettext("Sound / Action URI"))
            .show_apply_button(false)
            .visible(
                matches!(initial.action, AlarmAction::Audio | AlarmAction::Procedure)
                    || !initial.attachments.is_empty(),
            )
            .build();
        attachments_entry.set_text(&initial.attachments.join("\n"));
        advanced_expander.add_row(&attachments_entry);

        let attendees_entry = adw::EntryRow::builder()
            .title(gettext("Email Recipients"))
            .show_apply_button(false)
            .visible(matches!(initial.action, AlarmAction::Email) || !initial.attendees.is_empty())
            .build();
        attendees_entry.set_text(&initial.attendees.join("\n"));
        advanced_expander.add_row(&attendees_entry);

        options_group.add(&advanced_expander);
        content.append(&options_group);

        let safety_note_for_action = safety_note.clone();
        let attachments_for_action = attachments_entry.clone();
        let attendees_for_action = attendees_entry.clone();
        let advanced_for_action = advanced_expander.clone();
        let action_values_for_signal = Rc::clone(&action_values);
        action_row.connect_selected_notify(move |row| {
            let selected_action = action_values_for_signal.get(row.selected() as usize);
            let unsafe_action = selected_action.is_some_and(|action| !action.is_safe_to_notify());
            safety_note_for_action.set_visible(unsafe_action);
            let show_attach = selected_action.is_some_and(|a| {
                matches!(a, AlarmAction::Audio | AlarmAction::Procedure)
            }) || !attachments_for_action.text().trim().is_empty();
            let show_attendees = selected_action.is_some_and(|a| matches!(a, AlarmAction::Email))
                || !attendees_for_action.text().trim().is_empty();
            attachments_for_action.set_visible(show_attach);
            attendees_for_action.set_visible(show_attendees);
            if show_attach || show_attendees {
                advanced_for_action.set_expanded(true);
            }
        });

        let editor = self.clone();
        let initial_for_save = initial.clone();
        let action_values_for_save = Rc::clone(&action_values);
        let trigger_values_for_save = Rc::clone(&trigger_values);
        let window_for_save = window.clone();
        save.connect_clicked(move |_| {
            let action = action_values_for_save
                .get(action_row.selected() as usize)
                .cloned()
                .unwrap_or(AlarmAction::Display);
            let trigger = match trigger_values_for_save
                .get(trigger_row.selected() as usize)
                .copied()
                .unwrap_or(0)
            {
                0 => {
                    let unit_seconds = alarm_unit_seconds(unit_row.selected());
                    let magnitude = offset_row.value().round() as i64;
                    let signed = magnitude.saturating_mul(unit_seconds);
                    let signed = if direction_row.selected() == 0 {
                        signed.saturating_neg()
                    } else {
                        signed
                    };
                    AlarmTrigger::relative(
                        if relative_to_row.selected() == 1 {
                            AlarmRelation::End
                        } else {
                            AlarmRelation::Start
                        },
                        signed,
                    )
                }
                1 => {
                    let value = absolute_entry.text().trim().to_string();
                    if !is_valid_absolute_alarm_value(&value) {
                        show_alarm_error(
                            &window_for_save,
                            "The timestamp must match YYYYMMDDTHHMMSS[Z].",
                        );
                        return;
                    }
                    AlarmTrigger::absolute(value)
                }
                _ => initial_for_save.trigger.clone(),
            };
            let repeat = (repeat_row.value().round() as u32).min(4_096);
            let repeat_interval = repeat_interval_row.value().round() as i64;
            let duration = (repeat_interval > 0).then(|| {
                AlarmDuration::new(repeat_interval * alarm_unit_seconds(repeat_unit_row.selected()))
            });
            let message = message_entry.text().to_string();
            let attachments = attachments_entry
                .text()
                .lines()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let attendees = attendees_entry
                .text()
                .lines()
                .flat_map(|line| line.split(',').map(str::trim))
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let mut alarm = initial_for_save.clone();
            alarm.action = action;
            alarm.trigger = trigger;
            alarm.repeat = (repeat > 0).then_some(repeat);
            alarm.duration = duration;
            alarm.message = (!message.trim().is_empty()).then_some(message);
            alarm.attachments = attachments;
            alarm.attendees = attendees;
            if let Some(index) = index {
                if let Some(existing) = editor.alarms.borrow_mut().get_mut(index) {
                    *existing = alarm;
                }
                editor.rebuild(Some(index));
            } else {
                editor.alarms.borrow_mut().push(alarm);
                let index = editor.alarms.borrow().len().saturating_sub(1);
                editor.rebuild(Some(index));
            }
            window_for_save.close();
        });

        window.present(Some(&self.root));
    }
}

fn duration_editor_parts(seconds: u64) -> (u64, u32) {
    if seconds != 0 && seconds % (24 * 60 * 60) == 0 {
        (seconds / (24 * 60 * 60), 2)
    } else if seconds != 0 && seconds % (60 * 60) == 0 {
        (seconds / (60 * 60), 1)
    } else {
        (seconds / 60, 0)
    }
}

fn alarm_unit_seconds(index: u32) -> i64 {
    match index {
        1 => 60 * 60,
        2 => 24 * 60 * 60,
        _ => 60,
    }
}

fn is_valid_absolute_alarm_value(value: &str) -> bool {
    let value = value.trim();
    if value.len() != 15 && value.len() != 16 {
        return false;
    }
    if value.as_bytes().get(8) != Some(&b'T')
        || !value[..8]
            .chars()
            .all(|character| character.is_ascii_digit())
        || !value[9..]
            .chars()
            .all(|character| character.is_ascii_digit() || character == 'Z')
    {
        return false;
    }
    chrono::NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").is_ok()
}

fn show_alarm_error(parent: &impl IsA<gtk::Widget>, message: &str) {
    let dialog = adw::AlertDialog::new(
        Some("Could not save reminder"),
        Some(message),
    );
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.present(Some(parent));
}

fn ical_property(raw: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (property_name == target).then(|| unescape_ical_text(value))
    })
}

pub(crate) fn ical_properties(raw: &str, name: &str) -> Vec<String> {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let property_name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            (property_name == target).then(|| unescape_ical_text(value))
        })
        .collect()
}

fn split_ical_category_names(value: &str) -> Vec<String> {
    let mut categories = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
        } else if character == ',' {
            if !current.trim().is_empty() {
                categories.push(current.trim().to_string());
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.trim().is_empty() {
        categories.push(current.trim().to_string());
    }
    categories
}

fn serialize_categories(value: &str) -> String {
    split_ical_category_names(value)
        .into_iter()
        .map(|category| escape_ical_text(&category))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_recurrence_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn serialize_recurrence_values(value: &str) -> String {
    parse_recurrence_values(value).join(",")
}

fn default_recurrence_property_prefix(name: &str, all_day: bool) -> String {
    if all_day {
        format!("{name};VALUE=DATE")
    } else {
        name.to_string()
    }
}

fn build_recurrence_property_line(name: &str, value: &str, all_day: bool) -> String {
    let values = serialize_recurrence_values(value);
    if values.is_empty() {
        String::new()
    } else {
        format!(
            "{}:{}\r\n",
            default_recurrence_property_prefix(name, all_day),
            values
        )
    }
}

fn ical_recurrence_property_prefix(raw: &str, name: &str, all_day: bool) -> Option<String> {
    let target = name.to_ascii_uppercase();
    let mut depth = 0usize;
    for line in unfold_ical_lines(raw) {
        let property = line
            .split_once(':')
            .map(|(property, _)| property)
            .unwrap_or(line.as_str());
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        let is_date_value = property.to_ascii_uppercase().contains("VALUE=DATE");
        if property_name == target && depth == 1 && is_date_value == all_day {
            return Some(property.to_string());
        }
        if property_name == "BEGIN" {
            depth = depth.saturating_add(1);
        } else if property_name == "END" {
            depth = depth.saturating_sub(1);
        }
    }
    None
}

fn parse_attendees(value: &str) -> Vec<String> {
    value
        .split(|character| character == ',' || character == ';')
        .map(str::trim)
        .filter(|attendee| !attendee.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_attachments(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|attachment| !attachment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(crate) fn component_property_has_date_value(raw: &str, name: &str) -> bool {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().any(|line| {
        line.split_once(':')
            .map(|(property, _)| {
                property
                    .split(';')
                    .next()
                    .unwrap_or(property)
                    .eq_ignore_ascii_case(&target)
                    && property.to_ascii_uppercase().contains("VALUE=DATE")
            })
            .unwrap_or(false)
    })
}

fn ical_datetime_property_prefix(raw: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, _) = line.split_once(':')?;
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (property_name == target && !property.to_ascii_uppercase().contains("VALUE=DATE"))
            .then(|| property.to_string())
    })
}

pub(crate) fn format_event_datetime_for_editor(value: &str, all_day: bool) -> String {
    let value = value.trim_end_matches('Z');
    if !value.is_ascii() {
        return value.to_string();
    }
    if all_day && value.len() >= 8 {
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

pub(crate) fn build_event_component(
    title: &str,
    location: &str,
    conference: &str,
    status: &str,
    transparent: bool,
    all_day: bool,
    start: &str,
    end: &str,
    description: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    attendees: &str,
    categories: &str,
    attachments: &str,
    alarms: &[Alarm],
    private: bool,
    rooms: &str,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(gettext("An event requires a title"));
    }
    let start_value = parse_event_datetime(start, all_day, "Starts")?;
    let end_value = parse_event_datetime(end, all_day, "Ends")?;
    if !all_day && end_value <= start_value {
        return Err(gettext("End time must be after start time"));
    }
    let status = match status.to_ascii_uppercase().as_str() {
        "TENTATIVE" => "TENTATIVE",
        "CANCELLED" => "CANCELLED",
        "CONFIRMED" => "CONFIRMED",
        _ => return Err(gettext("Invalid event status")),
    };
    for (field, value) in [
        ("RRULE", recurrence),
        ("RDATE", rdates),
        ("EXDATE", exdates),
        ("Attendees", attendees),
        ("Rooms", rooms),
        ("Conference URL", conference),
    ] {
        if value
            .chars()
            .any(|character| character == '\r' || character == '\n')
        {
            return Err(format!("{field}: {}", gettext("contains invalid line breaks")));
        }
    }

    let uid = format!("meridian-{}", uuid::Uuid::new_v4());
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let (start_line, end_line) = if all_day {
        (
            format!("DTSTART;VALUE=DATE:{start_value}"),
            format!("DTEND;VALUE=DATE:{end_value}"),
        )
    } else {
        (
            format!("DTSTART:{start_value}"),
            format!("DTEND:{end_value}"),
        )
    };

    let recurrence_line = if recurrence.trim().is_empty() {
        String::new()
    } else {
        format!("RRULE:{}\r\n", recurrence.trim())
    };
    let status_line = format!("STATUS:{status}\r\n");
    let transparency_line = if transparent {
        "TRANSP:TRANSPARENT\r\n"
    } else {
        "TRANSP:OPAQUE\r\n"
    };
    let rdate_line = build_recurrence_property_line("RDATE", rdates, all_day);
    let exdate_line = build_recurrence_property_line("EXDATE", exdates, all_day);
    let mut attendee_lines = attendees
        .split(|character| character == ',' || character == ';')
        .map(str::trim)
        .filter(|attendee| !attendee.is_empty())
        .map(|attendee| {
            if attendee.starts_with("ATTENDEE") {
                format!("{}\r\n", attendee)
            } else {
                format!("ATTENDEE:{}\r\n", escape_ical_text(attendee))
            }
        })
        .collect::<String>();
    for room in parse_attendees(rooms) {
        let (room_mailto, prefix) = format_room_attendee(&room);
        if !room_mailto.is_empty() {
            attendee_lines.push_str(&format!("{prefix}:{}\r\n", escape_ical_text(&room_mailto)));
        }
    }
    let categories_line = if categories.trim().is_empty() {
        String::new()
    } else {
        format!("CATEGORIES:{}\r\n", serialize_categories(categories.trim()))
    };
    let attachment_lines = parse_attachments(attachments)
        .into_iter()
        .map(|attachment| {
            format!(
                "ATTACH;VALUE=URI:{}\r\n",
                serialize_ical_attachment_value(&attachment)
            )
        })
        .collect::<String>();
    let conference_line = if conference.trim().is_empty() {
        String::new()
    } else {
        format!("CONFERENCE:{}\r\n", escape_ical_text(conference.trim()))
    };
    let class_line = if private { "CLASS:PRIVATE\r\n" } else { "" };
    let component = format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{stamp}\r\nSUMMARY:{}\r\n{}\
         \r\n{}\r\nLOCATION:{}\r\nDESCRIPTION:{}\r\n{}{}{}{}{}{}{}{}{}{}{}END:VEVENT\r\n",
        escape_ical_text(title),
        start_line,
        end_line,
        escape_ical_text(location.trim()),
        escape_ical_text(description.trim()),
        status_line,
        transparency_line,
        recurrence_line,
        rdate_line,
        exdate_line,
        attendee_lines,
        conference_line,
        categories_line,
        attachment_lines,
        class_line,
        "",
    );
    Ok(replace_component_alarms(&component, alarms))
}

/// Update an existing event component while preserving unknown properties, parameters,
/// and vendor-specific fields (e.g., ORGANIZER, GEO, URL, PRIORITY, X-*, and custom alarms).
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_existing_event_component(
    original: &str,
    uid: &str,
    title: &str,
    location: &str,
    conference: &str,
    status: &str,
    transparent: bool,
    all_day: bool,
    start: &str,
    end: &str,
    description: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    attendees: &str,
    categories: &str,
    attachments: &str,
    alarms: &[Alarm],
    private: bool,
    rooms: &str,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(gettext("An event requires a title"));
    }
    let start_value = parse_event_datetime(start, all_day, "Starts")?;
    let end_value = parse_event_datetime(end, all_day, "Ends")?;
    if !all_day && end_value <= start_value {
        return Err(gettext("End time must be after start time"));
    }
    let status = match status.to_ascii_uppercase().as_str() {
        "TENTATIVE" => "TENTATIVE",
        "CANCELLED" => "CANCELLED",
        "CONFIRMED" => "CONFIRMED",
        _ => return Err(gettext("Invalid event status")),
    };
    for (field, value) in [
        ("RRULE", recurrence),
        ("RDATE", rdates),
        ("EXDATE", exdates),
        ("Attendees", attendees),
        ("Rooms", rooms),
        ("Conference URL", conference),
    ] {
        if value
            .chars()
            .any(|character| character == '\r' || character == '\n')
        {
            return Err(format!("{field}: {}", gettext("contains invalid line breaks")));
        }
    }

    let start_property = if all_day {
        "DTSTART;VALUE=DATE".to_string()
    } else {
        ical_datetime_property_prefix(original, "DTSTART").unwrap_or_else(|| "DTSTART".to_string())
    };
    let end_property = if all_day {
        "DTEND;VALUE=DATE".to_string()
    } else {
        ical_datetime_property_prefix(original, "DTEND").unwrap_or_else(|| "DTEND".to_string())
    };
    let mut updated = update_ical_property(original, "UID", Some(&escape_ical_text(uid)));
    updated = update_ical_property(&updated, "SUMMARY", Some(&escape_ical_text(title)));
    updated = update_ical_property(
        &updated,
        "LOCATION",
        Some(&escape_ical_text(location.trim())),
    );
    let conference_value =
        (!conference.trim().is_empty()).then(|| escape_ical_text(conference.trim()));
    updated = update_ical_property(&updated, "CONFERENCE", conference_value.as_deref());
    updated = update_ical_property(&updated, "STATUS", Some(status));
    updated = update_ical_property(
        &updated,
        "TRANSP",
        Some(if transparent { "TRANSPARENT" } else { "OPAQUE" }),
    );
    let original_has_date_start = component_property_has_date_value(original, "DTSTART");
    let original_has_date_end = component_property_has_date_value(original, "DTEND");
    if original_has_date_start != all_day {
        updated = update_ical_property(&updated, "DTSTART", None);
    }
    updated = update_ical_property(&updated, &start_property, Some(&start_value));

    if original_has_date_end != all_day {
        updated = update_ical_property(&updated, "DTEND", None);
    }
    updated = update_ical_property(&updated, &end_property, Some(&end_value));

    updated = update_ical_property(
        &updated,
        "DESCRIPTION",
        Some(&escape_ical_text(description.trim())),
    );
    let recurrence_trimmed = recurrence.trim();
    updated = update_ical_property(
        &updated,
        "RRULE",
        (!recurrence_trimmed.is_empty()).then_some(recurrence_trimmed),
    );
    let rdate_prefix = ical_recurrence_property_prefix(original, "RDATE", all_day)
        .unwrap_or_else(|| default_recurrence_property_prefix("RDATE", all_day));
    let rdates_serialized = serialize_recurrence_values(rdates);
    let original_has_date_rdate = component_property_has_date_value(original, "RDATE");
    if original_has_date_rdate != all_day {
        updated = update_ical_property(&updated, "RDATE", None);
    }
    updated = update_ical_property(
        &updated,
        &rdate_prefix,
        (!rdates_serialized.is_empty()).then_some(rdates_serialized.as_str()),
    );
    let exdate_prefix = ical_recurrence_property_prefix(original, "EXDATE", all_day)
        .unwrap_or_else(|| default_recurrence_property_prefix("EXDATE", all_day));
    let exdates_serialized = serialize_recurrence_values(exdates);
    let original_has_date_exdate = component_property_has_date_value(original, "EXDATE");
    if original_has_date_exdate != all_day {
        updated = update_ical_property(&updated, "EXDATE", None);
    }
    updated = update_ical_property(
        &updated,
        &exdate_prefix,
        (!exdates_serialized.is_empty()).then_some(exdates_serialized.as_str()),
    );
    let categories_serialized = serialize_categories(categories);
    updated = update_ical_property(
        &updated,
        "CATEGORIES",
        (!categories_serialized.is_empty()).then_some(categories_serialized.as_str()),
    );
    updated = update_ical_property(
        &updated,
        "CLASS",
        Some(if private { "PRIVATE" } else { "PUBLIC" }),
    );
    let attendees = parse_attendees(attendees);
    let rooms = parse_attendees(rooms);
    updated = replace_ical_attendees_with_rooms(&updated, &attendees, &rooms);
    let attachments = parse_attachments(attachments);
    updated = replace_ical_attachments(&updated, &attachments);
    updated = replace_component_alarms(&updated, alarms);
    Ok(updated)
}

pub(crate) fn replace_ical_attendees_with_rooms(
    raw: &str,
    individual_values: &[String],
    room_values: &[String],
) -> String {
    let existing = existing_attendee_properties(raw);
    let mut lines = Vec::new();
    let mut replaced = false;
    let mut depth = 0usize;

    let mut attendee_lines = Vec::new();
    let mut used = vec![false; existing.len()];

    for value in individual_values {
        let matching_existing = existing
            .iter()
            .enumerate()
            .find(|(index, (existing_value, prop_prefix))| {
                !used[*index]
                    && !prop_prefix.to_ascii_uppercase().contains("CUTYPE=ROOM")
                    && !prop_prefix.to_ascii_uppercase().contains("CUTYPE=RESOURCE")
                    && existing_value.eq_ignore_ascii_case(value)
            });
        let line = if let Some((index, (_, property_prefix))) = matching_existing {
            used[index] = true;
            format!("{property_prefix}:{}", escape_ical_text(value))
        } else {
            let mailto = if value.starts_with("mailto:") || !value.contains('@') {
                value.clone()
            } else {
                format!("mailto:{value}")
            };
            format!("ATTENDEE:{}", escape_ical_text(&mailto))
        };
        attendee_lines.push(line);
    }

    for room in room_values {
        let (room_mailto, default_prefix) = format_room_attendee(room);
        if room_mailto.is_empty() {
            continue;
        }
        let matching_existing = existing
            .iter()
            .enumerate()
            .find(|(index, (existing_value, _))| {
                !used[*index] && existing_value.eq_ignore_ascii_case(&room_mailto)
            });
        let line = if let Some((index, (_, property_prefix))) = matching_existing {
            used[index] = true;
            let final_prefix = if property_prefix.to_ascii_uppercase().contains("CUTYPE=ROOM")
                || property_prefix.to_ascii_uppercase().contains("CUTYPE=RESOURCE")
            {
                property_prefix.clone()
            } else {
                format!("{property_prefix};CUTYPE=ROOM")
            };
            format!("{final_prefix}:{}", escape_ical_text(&room_mailto))
        } else {
            format!("{default_prefix}:{}", escape_ical_text(&room_mailto))
        };
        attendee_lines.push(line);
    }

    for line in unfold_ical_lines(raw) {
        let property_part = line
            .split_once(':')
            .map(|(property, _)| property)
            .unwrap_or(line.as_str());
        let property = property_part
            .split(';')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if property == "ATTENDEE" && depth <= 1 {
            if !replaced {
                lines.extend(attendee_lines.clone());
                replaced = true;
            }
            continue;
        }
        lines.push(line);
        if property == "BEGIN" {
            depth = depth.saturating_add(1);
        } else if property == "END" {
            depth = depth.saturating_sub(1);
        }
    }

    if !replaced && !attendee_lines.is_empty() {
        insert_before_event_end(&mut lines, attendee_lines);
    }
    format!("{}\r\n", lines.join("\r\n"))
}

pub(crate) fn replace_ical_attendees(raw: &str, values: &[String]) -> String {
    replace_ical_attendees_with_rooms(raw, values, &[])
}

pub(crate) fn existing_attendee_properties(raw: &str) -> Vec<(String, String)> {
    let mut attendees = Vec::new();
    let mut depth = 0usize;
    for line in unfold_ical_lines(raw) {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        if name == "ATTENDEE" && depth == 1 {
            attendees.push((unescape_ical_text(value), property.to_string()));
        }
        if name == "BEGIN" {
            depth = depth.saturating_add(1);
        } else if name == "END" {
            depth = depth.saturating_sub(1);
        }
    }
    attendees
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ParsedAttendees {
    pub individuals: Vec<String>,
    pub rooms: Vec<String>,
}

pub(crate) fn parse_component_attendees(raw: &str) -> ParsedAttendees {
    let mut individuals = Vec::new();
    let mut rooms = Vec::new();
    for (value, prop_prefix) in existing_attendee_properties(raw) {
        let upper = prop_prefix.to_ascii_uppercase();
        if upper.contains("CUTYPE=ROOM") || upper.contains("CUTYPE=RESOURCE") {
            rooms.push(value);
        } else {
            individuals.push(value);
        }
    }
    ParsedAttendees { individuals, rooms }
}

pub(crate) fn format_room_attendee(raw_room: &str) -> (String, String) {
    let trimmed = raw_room.trim();
    if trimmed.is_empty() {
        return (String::new(), String::new());
    }
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.find('>')) {
        if start < end {
            let name = trimmed[..start].trim().trim_matches('"');
            let email = trimmed[start + 1..end].trim();
            let mailto = if email.starts_with("mailto:") {
                email.to_string()
            } else {
                format!("mailto:{email}")
            };
            let prefix = if name.is_empty() {
                "ATTENDEE;CUTYPE=ROOM;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE".to_string()
            } else {
                format!("ATTENDEE;CUTYPE=ROOM;CN={name};ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE")
            };
            return (mailto, prefix);
        }
    }
    let mailto = if trimmed.starts_with("mailto:") {
        trimmed.to_string()
    } else if trimmed.contains('@') {
        format!("mailto:{trimmed}")
    } else {
        let slug = trimmed.to_lowercase().replace(' ', "-");
        let mailto = format!("mailto:{slug}@local");
        let prefix = format!("ATTENDEE;CUTYPE=ROOM;CN={trimmed};ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE");
        return (mailto, prefix);
    };
    (
        mailto,
        "ATTENDEE;CUTYPE=ROOM;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE".to_string(),
    )
}

fn insert_before_event_end<I>(lines: &mut Vec<String>, values: I)
where
    I: IntoIterator<Item = String>,
{
    let index = lines
        .iter()
        .position(|line| line.eq_ignore_ascii_case("END:VEVENT"))
        .unwrap_or(lines.len());
    lines.splice(index..index, values);
}

fn unfold_ical_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split_terminator('\n')
    {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.get(1..).unwrap_or_default());
            }
        } else if !raw_line.is_empty() || !lines.is_empty() {
            lines.push(raw_line.to_string());
        }
    }
    lines
}

fn parse_event_datetime(value: &str, all_day: bool, field_name: &str) -> Result<String, String> {
    let value = value.trim();
    if all_day {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(|date| date.format("%Y%m%d").to_string())
            .map_err(|_| format!("{field_name}: {}", gettext("expected YYYY-MM-DD")));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M")
        .map(|date_time| date_time.format("%Y%m%dT%H%M%S").to_string())
        .map_err(|_| format!("{field_name}: {}", gettext("expected YYYY-MM-DD HH:MM")))
}

fn normalize_timezone_input(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("floating")
        || trimmed.eq_ignore_ascii_case("fließend")
        || trimmed.eq_ignore_ascii_case("fliessend")
        || trimmed.eq_ignore_ascii_case("local")
        || trimmed.eq_ignore_ascii_case("lokal")
    {
        return Ok(None);
    }
    if trimmed.eq_ignore_ascii_case("utc") {
        return Ok(Some("UTC".to_string()));
    }
    if glib::TimeZone::from_identifier(Some(trimmed)).is_none() {
        return Err(format!("{}: {trimmed}", gettext("Invalid timezone")));
    }
    Ok(Some(trimmed.to_string()))
}

fn prepare_end_val_for_timezones(
    all_day: bool,
    start_val: &str,
    end_val: &str,
    start_tz: &str,
    end_tz: Option<&str>,
) -> Result<String, String> {
    if all_day {
        return Ok(end_val.to_string());
    }
    let start_ical = parse_event_datetime(start_val, false, "Starts")?;
    let end_ical = parse_event_datetime(end_val, false, "Ends")?;
    let stz = normalize_timezone_input(start_tz)?;
    let etz = match end_tz {
        Some(e) => normalize_timezone_input(e)?,
        None => stz.clone(),
    };
    if stz != etz {
        let start_prop = match &stz {
            Some(tz) => format!("DTSTART;TZID={tz}"),
            None => "DTSTART".to_string(),
        };
        let end_prop = match &etz {
            Some(tz) => format!("DTEND;TZID={tz}"),
            None => "DTEND".to_string(),
        };
        if let (Some(s_ts), Some(e_ts)) = (
            crate::services::ical_time_service::property_local_timestamp(&start_prop, &start_ical, 0),
            crate::services::ical_time_service::property_local_timestamp(&end_prop, &end_ical, 0),
        ) {
            if e_ts <= s_ts {
                return Err(gettext("End time (including timezone) must be after start time"));
            }
            if end_ical <= start_ical {
                if let Ok(dt) = NaiveDateTime::parse_from_str(start_val.trim(), "%Y-%m-%d %H:%M") {
                    return Ok((dt + Duration::hours(1)).format("%Y-%m-%d %H:%M").to_string());
                }
            }
        }
    }
    Ok(end_val.to_string())
}

pub(crate) fn apply_event_timezones_to_component(
    component: &str,
    all_day: bool,
    start_val: &str,
    end_val: &str,
    start_tz: &str,
    end_tz: Option<&str>,
) -> Result<String, String> {
    if all_day {
        return Ok(component.to_string());
    }
    let stz = normalize_timezone_input(start_tz)?;
    let etz = match end_tz {
        Some(e) => normalize_timezone_input(e)?,
        None => stz.clone(),
    };
    let start_ical = parse_event_datetime(start_val, false, "Starts")?;
    let end_ical = parse_event_datetime(end_val, false, "Ends")?;
    let start_prop = match &stz {
        Some(tz) => format!("DTSTART;TZID={tz}"),
        None => "DTSTART".to_string(),
    };
    let end_prop = match &etz {
        Some(tz) => format!("DTEND;TZID={tz}"),
        None => "DTEND".to_string(),
    };
    let mut updated = update_ical_property(component, "DTSTART", None);
    updated = update_ical_property(&updated, &start_prop, Some(&start_ical));
    updated = update_ical_property(&updated, "DTEND", None);
    updated = update_ical_property(&updated, &end_prop, Some(&end_ical));
    Ok(updated)
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

fn unescape_ical_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            match character {
                'n' | 'N' => result.push('\n'),
                ',' | ';' | '\\' => result.push(character),
                other => {
                    result.push('\\');
                    result.push(other);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_component_serialization_is_ical_safe() {
        let component = build_event_component(
            "Team, Sync",
            "Room; 42",
            "https://meet.example.test/room?x=1&y=2",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 12:00",
            "2026-08-10 13:00",
            "Line one\nLine two",
            "FREQ=WEEKLY;BYDAY=MO",
            "20260812T120000Z,20260819T120000Z",
            "20260815T120000Z",
            "mailto:person@example.org",
            "Work",
            "",
            &[Alarm::new_display_relative(
                AlarmRelation::Start,
                -15 * 60,
                None,
            )],
            true,
            "",
        )
        .expect("event should serialize");

        assert!(component.contains("BEGIN:VEVENT"));
        assert!(component.contains("SUMMARY:Team\\, Sync"));
        assert!(component.contains("LOCATION:Room\\; 42"));
        assert!(component.contains("CONFERENCE:https://meet.example.test/room?x=1&y=2"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two"));
        assert!(component.contains("DTSTART:20260810T120000"));
        assert!(component.contains("DTEND:20260810T130000"));
        assert!(component.contains("STATUS:CONFIRMED"));
        assert!(component.contains("TRANSP:OPAQUE"));
        assert!(component.contains("RRULE:FREQ=WEEKLY;BYDAY=MO"));
        assert!(component.contains("RDATE:20260812T120000Z,20260819T120000Z"));
        assert!(component.contains("EXDATE:20260815T120000Z"));
        assert!(component.contains("ATTENDEE:mailto:person@example.org"));
        assert!(component.contains("CATEGORIES:Work"));
        assert!(component.contains("TRIGGER:-PT15M"));
        assert!(component.contains("CLASS:PRIVATE"));
        assert!(component.ends_with("END:VEVENT\r\n"));
    }

    #[test]
    fn event_component_rejects_invalid_or_reversed_times() {
        assert!(
            build_event_component(
                "Title",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "not-a-date",
                "2026-08-10 13:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false,
                "",
            )
            .is_err()
        );
        assert!(
            build_event_component(
                "Title",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 13:00",
                "2026-08-10 12:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false,
                "",
            )
            .is_err()
        );
    }

    #[test]
    fn attendee_updates_preserve_existing_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "ATTENDEE;CN=Bob Example;ROLE=REQ-PARTICIPANT:mailto:bob@example.test\r\n",
            "ATTENDEE;CN=Alice Example:mailto:alice@example.test\r\n",
            "END:VEVENT\r\n"
        );
        let updated = replace_ical_attendees(
            raw,
            &[
                "mailto:bob@example.test".to_string(),
                "mailto:new@example.test".to_string(),
            ],
        );
        assert!(
            updated
                .contains("ATTENDEE;CN=Bob Example;ROLE=REQ-PARTICIPANT:mailto:bob@example.test")
        );
        assert!(updated.contains("ATTENDEE:mailto:new@example.test"));
        assert!(!updated.contains("alice@example.test"));
    }

    #[test]
    fn event_attachments_roundtrip_and_preserve_existing_parameters() {
        let component = build_event_component(
            "Planning",
            "Room",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 12:00",
            "2026-08-10 13:00",
            "",
            "",
            "",
            "",
            "",
            "",
            "https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda\nfile:///tmp/notes.pdf",
            &[],
            false,
            "",
        )
        .expect("event with attachments should serialize");
        assert!(ical_properties(&component, "ATTACH").contains(
            &"https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda".to_string()
        ));
        assert!(component.contains("ATTACH;VALUE=URI:file:///tmp/notes.pdf"));

        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda\r\n",
            "ATTACH;VALUE=URI:https://calendar.example.test/old.ics\r\n",
            "END:VEVENT\r\n"
        );
        let updated = replace_ical_attachments(
            raw,
            &[
                "https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda".to_string(),
                "https://calendar.example.test/new.ics".to_string(),
            ],
        );
        assert!(updated.contains(
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda"
        ));
        assert!(updated.contains("ATTACH;VALUE=URI:https://calendar.example.test/new.ics"));
        assert!(!updated.contains("https://calendar.example.test/old.ics"));
    }

    #[test]
    fn editing_preserves_timezone_property_prefix() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T130000\r\n",
            "END:VEVENT\r\n"
        );

        assert_eq!(
            ical_datetime_property_prefix(raw, "DTSTART").as_deref(),
            Some("DTSTART;TZID=Europe/Berlin")
        );
        assert_eq!(
            ical_datetime_property_prefix(raw, "DTEND").as_deref(),
            Some("DTEND;TZID=Europe/Berlin")
        );
    }

    #[test]
    fn recurrence_date_prefixes_preserve_date_and_timezone_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "RDATE;TZID=Europe/Berlin:20260812T120000,20260819T120000\r\n",
            "EXDATE;VALUE=DATE:20260815\r\n",
            "END:VEVENT\r\n"
        );
        assert_eq!(
            ical_recurrence_property_prefix(raw, "RDATE", false).as_deref(),
            Some("RDATE;TZID=Europe/Berlin")
        );
        assert_eq!(
            ical_recurrence_property_prefix(raw, "EXDATE", true).as_deref(),
            Some("EXDATE;VALUE=DATE")
        );
        assert_eq!(
            build_recurrence_property_line("RDATE", "20260812,20260819", true),
            "RDATE;VALUE=DATE:20260812,20260819\r\n"
        );
    }

    #[test]
    fn editing_simple_alarm_does_not_delete_unsupported_alarm_types() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-P1D\r\n",
            "ATTENDEE:mailto:notify@example.test\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=START:-PT15M\r\n",
            "DESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\n"
        );
        let alarms = parse_alarms(raw);
        assert_eq!(alarms.len(), 2);
        assert!(matches!(alarms[0].action, AlarmAction::Email));
        assert_eq!(alarms[1].trigger.offset_seconds(), Some(-15 * 60));

        let removed = replace_component_alarms(raw, &alarms[..1]);
        assert!(removed.contains("ACTION:EMAIL"));
        assert!(removed.contains("TRIGGER:-P1D"));
        assert!(!removed.contains("TRIGGER;RELATED=START:-PT15M"));

        let updated_alarm = Alarm::new_display_relative(
            AlarmRelation::Start,
            -30 * 60,
            Some("Reminder".to_string()),
        );
        let updated = replace_component_alarms(raw, &[alarms[0].clone(), updated_alarm]);
        assert!(updated.contains("ACTION:EMAIL"));
        assert!(updated.contains("TRIGGER:-P1D"));
        assert!(updated.contains("TRIGGER:-PT30M"));
        assert!(!updated.contains("TRIGGER;RELATED=START:-PT15M"));
    }

    #[test]
    fn alarm_editor_model_roundtrips_all_actions_repeat_duration_and_unknowns() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-alarms\r\n",
            "X-COMPONENT-STATE:keep-me\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "TRIGGER;RELATED=END;X-TRIGGER=keep:-PT30M\r\n",
            "REPEAT:2\r\nDURATION:PT15M\r\n",
            "DESCRIPTION:Custom\\, message\\;\r\n",
            "X-ALARM-PROPERTY:preserve\r\n",
            "END:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:AUDIO\r\n",
            "TRIGGER;VALUE=DATE-TIME:20261025T013000Z\r\n",
            "ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg\r\n",
            "END:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-P1D\r\n",
            "ATTENDEE;CN=Ops:mailto:ops@example.test\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:PROCEDURE\r\nTRIGGER:-PT5M\r\n",
            "X-EXECUTABLE:do-not-run\r\nEND:VALARM\r\n",
            "END:VEVENT\r\n"
        );
        let alarms = parse_alarms(raw);
        assert_eq!(alarms.len(), 4);
        assert_eq!(alarms[0].repeat, Some(2));
        assert_eq!(
            alarms[0].duration.as_ref().map(|value| value.seconds),
            Some(15 * 60)
        );
        assert_eq!(alarms[0].message.as_deref(), Some("Custom, message;"));
        assert!(!alarms[1].is_safe_to_notify());
        assert!(!alarms[2].is_safe_to_notify());
        assert!(!alarms[3].is_safe_to_notify());

        let roundtrip = replace_component_alarms(raw, &alarms);
        assert!(roundtrip.contains("X-COMPONENT-STATE:keep-me"));
        assert!(roundtrip.contains("X-ALARM-PROPERTY:preserve"));
        assert!(
            roundtrip.contains("ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg")
        );
        assert!(roundtrip.contains("ATTENDEE;CN=Ops:mailto:ops@example.test"));
        assert_eq!(parse_alarms(&roundtrip), alarms);
    }

    #[test]
    fn category_names_split_only_on_unescaped_commas() {
        assert_eq!(
            split_ical_category_names(r"Arbeit,Privat\, Familie,Reise"),
            vec![
                "Arbeit".to_string(),
                "Privat, Familie".to_string(),
                "Reise".to_string()
            ]
        );
    }

    #[test]
    fn category_serialization_keeps_category_boundaries() {
        assert_eq!(
            serialize_categories(r"Arbeit,Privat\, Familie,Reise"),
            r"Arbeit,Privat\, Familie,Reise"
        );
    }

    #[test]
    fn update_existing_event_preserves_unknown_properties_and_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:upstream-event-123\r\n",
            "DTSTAMP:20260101T000000Z\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T090000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T100000\r\n",
            "SUMMARY:Initial Strategy Meeting\r\n",
            "ORGANIZER;CN=Alice Boss:mailto:alice@example.com\r\n",
            "GEO:48.85299;2.36885\r\n",
            "URL:https://intranet.example.com/meeting/123\r\n",
            "PRIORITY:1\r\n",
            "CLASS:CONFIDENTIAL\r\n",
            "X-EVOLUTION-MOVE-CALENDAR:1\r\n",
            "X-CUSTOM-FLAG:TRUE\r\n",
            "ATTENDEE;CN=Bob Colleague;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n",
            "ATTENDEE;CN=Charlie Remote;ROLE=OPT-PARTICIPANT:mailto:charlie@example.com\r\n",
            "RDATE;TZID=Europe/Berlin:20260810T090000\r\n",
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://example.com/spec.pdf\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:AUDIO\r\n",
            "TRIGGER;VALUE=DATE-TIME:20260810T083000Z\r\n",
            "ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg\r\n",
            "END:VALARM\r\n",
            "END:VEVENT\r\n"
        );

        let mut alarms = parse_alarms(raw);
        alarms.push(Alarm::new_display_relative(
            AlarmRelation::Start,
            -15 * 60,
            Some("Meeting starts in 15m".to_string()),
        ));

        let updated = update_existing_event_component(
            raw,
            "upstream-event-123",
            "Updated Strategy Meeting",
            "Room 101",
            "https://meet.example.com/abc",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 10:00",
            "2026-08-10 11:30",
            "Discuss Q3 goals and roadmap.",
            "FREQ=WEEKLY;COUNT=5",
            "20260817T100000,20260824T100000",
            "20260831T100000",
            "mailto:bob@example.com, mailto:dana@example.com",
            r"Work,Strategy\, Planning",
            "https://example.com/spec.pdf\nhttps://example.com/agenda.pdf",
            &alarms,
            true,
            "",
        )
        .expect("update_existing_event_component should succeed");

        let unfolded = unfold_ical_lines(&updated);

        // Verify edited fields
        assert!(unfolded.contains(&"SUMMARY:Updated Strategy Meeting".to_string()));
        assert!(unfolded.contains(&"LOCATION:Room 101".to_string()));
        assert!(unfolded.contains(&"CONFERENCE:https://meet.example.com/abc".to_string()));
        assert!(unfolded.contains(&"STATUS:CONFIRMED".to_string()));
        assert!(unfolded.contains(&"TRANSP:OPAQUE".to_string()));
        assert!(unfolded.contains(&"DTSTART;TZID=Europe/Berlin:20260810T100000".to_string()));
        assert!(unfolded.contains(&"DTEND;TZID=Europe/Berlin:20260810T113000".to_string()));
        assert!(unfolded.contains(&"DESCRIPTION:Discuss Q3 goals and roadmap.".to_string()));
        assert!(unfolded.contains(&"RRULE:FREQ=WEEKLY;COUNT=5".to_string()));
        assert!(
            unfolded
                .contains(&"RDATE;TZID=Europe/Berlin:20260817T100000,20260824T100000".to_string())
        );
        assert!(unfolded.contains(&"EXDATE:20260831T100000".to_string()));
        assert!(unfolded.contains(&r"CATEGORIES:Work,Strategy\, Planning".to_string()));
        assert!(unfolded.contains(&"CLASS:PRIVATE".to_string()));

        // Verify preserved unknown/vendor properties
        assert!(unfolded.contains(&"ORGANIZER;CN=Alice Boss:mailto:alice@example.com".to_string()));
        assert!(unfolded.contains(&"GEO:48.85299;2.36885".to_string()));
        assert!(unfolded.contains(&"URL:https://intranet.example.com/meeting/123".to_string()));
        assert!(unfolded.contains(&"PRIORITY:1".to_string()));
        assert!(unfolded.contains(&"X-EVOLUTION-MOVE-CALENDAR:1".to_string()));
        assert!(unfolded.contains(&"X-CUSTOM-FLAG:TRUE".to_string()));

        // Verify preserved attendee parameters and added/removed attendees
        assert!(unfolded.contains(
            &"ATTENDEE;CN=Bob Colleague;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.com".to_string()
        ));
        assert!(unfolded.contains(&"ATTENDEE:mailto:dana@example.com".to_string()));
        assert!(!updated.contains("charlie@example.com"));

        // Verify preserved attachment FMTTYPE and newly added attachment
        assert!(unfolded.contains(
            &"ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://example.com/spec.pdf".to_string()
        ));
        assert!(unfolded.contains(&"ATTACH;VALUE=URI:https://example.com/agenda.pdf".to_string()));

        // Verify alarms: audio alarm from raw preserved plus newly added display alarm
        assert!(unfolded.contains(&"ACTION:AUDIO".to_string()));
        assert!(unfolded.contains(
            &"ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg".to_string()
        ));
        assert!(unfolded.contains(&"ACTION:DISPLAY".to_string()));
        assert!(unfolded.contains(&"TRIGGER:-PT15M".to_string()));
    }

    #[test]
    fn update_existing_event_validates_inputs() {
        let raw = "BEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20260810T090000\r\nDTEND:20260810T100000\r\nSUMMARY:Test\r\nEND:VEVENT\r\n";

        // Empty title
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "   ",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 10:00",
                "2026-08-10 11:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false,
                "",
            )
            .is_err()
        );

        // End before start
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "Meeting",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 12:00",
                "2026-08-10 11:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false,
                "",
            )
            .is_err()
        );

        // Invalid status
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "Meeting",
                "",
                "",
                "INVALID_STATUS",
                false,
                false,
                "2026-08-10 10:00",
                "2026-08-10 11:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false,
                "",
            )
            .is_err()
        );

        // Newline injection in recurrence
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "Meeting",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 10:00",
                "2026-08-10 11:00",
                "",
                "FREQ=DAILY\r\nX-INJECTED:bad",
                "",
                "",
                "",
                "",
                "",
                &[],
                false,
                "",
            )
            .is_err()
        );
    }

    #[test]
    fn update_existing_event_switches_all_day_mode() {
        let timed = "BEGIN:VEVENT\r\nUID:event-timed\r\nDTSTART:20260810T090000\r\nDTEND:20260810T100000\r\nSUMMARY:Timed\r\nEND:VEVENT\r\n";

        // Switch timed -> all-day
        let all_day = update_existing_event_component(
            timed,
            "event-timed",
            "All-Day Event",
            "",
            "",
            "CONFIRMED",
            false,
            true,
            "2026-08-10",
            "2026-08-11",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            "",
        )
        .expect("should convert to all-day");

        assert!(all_day.contains("DTSTART;VALUE=DATE:20260810"));
        assert!(all_day.contains("DTEND;VALUE=DATE:20260811"));
        assert!(!all_day.contains("DTSTART:20260810\r\n"));

        // Switch all-day -> timed
        let back_to_timed = update_existing_event_component(
            &all_day,
            "event-timed",
            "Back to Timed",
            "",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 14:00",
            "2026-08-10 15:00",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            "",
        )
        .expect("should convert to timed");

        assert!(back_to_timed.contains("DTSTART:20260810T140000"));
        assert!(back_to_timed.contains("DTEND:20260810T150000"));
        assert!(!back_to_timed.contains("VALUE=DATE"));
    }

    #[test]
    fn test_room_attendee_serialization_with_cutype_room() {
        let component = build_event_component(
            "Quarterly Review",
            "Konferenzraum Spree",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 14:00",
            "2026-08-10 15:00",
            "Review session",
            "",
            "",
            "",
            "mailto:colleague@example.com",
            "",
            "",
            &[],
            false,
            "spree-meeting@firma.de",
        )
        .expect("should build event with room");

        let unfolded = unfold_ical_lines(&component);
        assert!(unfolded.contains(&"ATTENDEE:mailto:colleague@example.com".to_string()));
        assert!(unfolded.iter().any(|line| line.contains("CUTYPE=ROOM") && line.contains("mailto:spree-meeting@firma.de")));
    }

    #[test]
    fn test_parse_component_attendees_separates_rooms_and_individuals() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-room-test\r\n",
            "ATTENDEE;CN=Alice;ROLE=REQ-PARTICIPANT:mailto:alice@example.com\r\n",
            "ATTENDEE;CUTYPE=ROOM;CN=Konferenzraum 1;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:raum1@firma.de\r\n",
            "ATTENDEE;CN=Bob:mailto:bob@example.com\r\n",
            "END:VEVENT\r\n"
        );
        let parsed = parse_component_attendees(raw);
        assert_eq!(parsed.individuals, vec!["mailto:alice@example.com", "mailto:bob@example.com"]);
        assert_eq!(parsed.rooms, vec!["mailto:raum1@firma.de"]);
    }

    #[test]
    fn test_update_existing_event_component_preserves_room_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-room-preserve\r\n",
            "ATTENDEE;CN=Alice:mailto:alice@example.com\r\n",
            "ATTENDEE;CUTYPE=ROOM;CN=Konferenzraum 1;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:raum1@firma.de\r\n",
            "END:VEVENT\r\n"
        );
        let updated = update_existing_event_component(
            raw,
            "event-room-preserve",
            "Updated Title",
            "",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 14:00",
            "2026-08-10 15:00",
            "",
            "",
            "",
            "",
            "mailto:alice@example.com",
            "",
            "",
            &[],
            false,
            "mailto:raum1@firma.de",
        )
        .expect("should update preserving room");

        let unfolded = unfold_ical_lines(&updated);
        assert!(unfolded.contains(&"ATTENDEE;CUTYPE=ROOM;CN=Konferenzraum 1;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:raum1@firma.de".to_string()));
    }

    #[test]
    fn test_event_dialog_evolution_layout_and_timeline_helpers() {
        // Date parsing: lenient support for ISO and DD.MM.YYYY
        assert_eq!(
            date_to_iso("2026-09-10"),
            Some("2026-09-10".to_string())
        );
        assert_eq!(
            date_to_iso("10.09.2026"),
            Some("2026-09-10".to_string())
        );
        assert_eq!(
            date_to_iso("1.9.2026"),
            Some("2026-09-01".to_string())
        );
        assert_eq!(date_to_iso("invalid-date"), None);

        // Time parsing & formatting
        assert_eq!(parse_time_to_minutes("00:00"), Some(0));
        assert_eq!(parse_time_to_minutes("09:30"), Some(9 * 60 + 30));
        assert_eq!(parse_time_to_minutes("15:45"), Some(15 * 60 + 45));
        assert_eq!(parse_time_to_minutes("23:59"), Some(23 * 60 + 59));
        assert_eq!(parse_time_to_minutes("25:00"), None);
        // Robust 12h, period, single-hour and suffix formats
        assert_eq!(parse_time_to_minutes("2:30 PM"), Some(14 * 60 + 30));
        assert_eq!(parse_time_to_minutes("9:00 AM"), Some(9 * 60));
        assert_eq!(parse_time_to_minutes("12:00 PM"), Some(12 * 60));
        assert_eq!(parse_time_to_minutes("12:00 AM"), Some(0));
        assert_eq!(parse_time_to_minutes("14.30"), Some(14 * 60 + 30));
        assert_eq!(parse_time_to_minutes("14:00 Uhr"), Some(14 * 60));
        assert_eq!(parse_time_to_minutes("9"), Some(9 * 60));

        assert_eq!(format_clock_minutes(0), "00:00");
        assert_eq!(format_clock_minutes(570), "09:30");
        assert_eq!(format_clock_minutes(1439), "23:59");

        // Split datetime for entries
        let (d1, t1) = split_datetime_for_entries("2026-09-10 14:30");
        assert_eq!(d1, "2026-09-10");
        assert_eq!(t1, "14:30");

        let (d2, t2) = split_datetime_for_entries("2026-09-10");
        assert_eq!(d2, "2026-09-10");
        assert_eq!(t2, "09:00");
    }

    #[test]
    fn test_calendar_cleaning_and_grouping() {
        assert_eq!(
            clean_calendar_display_name("Kalender (EDV)", "EDV"),
            "Kalender"
        );
        assert_eq!(
            clean_calendar_display_name("Kalender (edv@acme-corp.example)", "EDV"),
            "Kalender"
        );
        assert_eq!(
            clean_calendar_display_name("EDV – Kalender", "EDV"),
            "Kalender"
        );
        assert_eq!(
            clean_calendar_display_name("Personal", "On This Computer"),
            "Personal"
        );
        assert_eq!(
            clean_calendar_display_name("Geburtstage", "mjensen@acme-corp.example"),
            "Geburtstage"
        );
        assert_eq!(
            clean_calendar_display_name("Kalender", "mjensen@acme-corp.example"),
            "Kalender"
        );

        // Sorting: "On This Computer" first, then alphabetical
        let groups = vec![
            "mjensen@acme-corp.example",
            "On This Computer",
            "EDV",
        ];
        let mut sorted = groups.clone();
        sorted.sort_by_key(|g| calendar_group_sort_key(g));
        assert_eq!(
            sorted,
            vec!["On This Computer", "EDV", "mjensen@acme-corp.example"]
        );
    }

    #[test]
    fn test_event_dialog_calendar_picker_sources_filtering() {
        use crate::services::eds_service::EdsSource;

        let raw_sources = vec![
            EdsSource {
                uid: "birthdays".to_string(),
                display_name: "Birthdays & Anniversaries".to_string(),
                enabled: true,
                writable: false,
                backend_name: "contacts".to_string(),
                parent_name: Some("Contacts".to_string()),
                color: None,
                foreign_mail: None,
            },
            EdsSource {
                uid: "system-calendar".to_string(),
                display_name: "Personal".to_string(),
                enabled: true,
                writable: true,
                backend_name: "local".to_string(),
                parent_name: Some("On This Computer".to_string()),
                color: None,
                foreign_mail: None,
            },
            EdsSource {
                uid: "shared-calendar".to_string(),
                display_name: "Kalender (EDV)".to_string(),
                enabled: true,
                writable: true,
                backend_name: "ews".to_string(),
                parent_name: Some("EDV".to_string()),
                color: None,
                foreign_mail: Some("edv@acme-corp.example".to_string()),
            },
            EdsSource {
                uid: "c3357329b452452d6ff3a15da95d084318369067".to_string(),
                display_name: "Kalender".to_string(),
                enabled: true,
                writable: true,
                backend_name: "ews".to_string(),
                parent_name: Some("mjensen@acme-corp.example".to_string()),
                color: None,
                foreign_mail: None,
            },
            EdsSource {
                uid: "d2faf98d06319a88428c7716feb89a52bda451bf".to_string(),
                display_name: "Geburtstage".to_string(),
                enabled: true,
                writable: true,
                backend_name: "ews".to_string(),
                parent_name: Some("mjensen@acme-corp.example".to_string()),
                color: None,
                foreign_mail: None,
            },
        ];

        // 1. Filter: writable && non-contacts && not birthdays
        let active_sources: Vec<_> = raw_sources
            .into_iter()
            .filter(|s| s.enabled && s.writable && s.backend_name != "contacts" && s.uid != "birthdays")
            .collect();

        assert_eq!(active_sources.len(), 4, "Should have exactly 4 writable calendars (matching Evolution)");

        // 2. Clean names & grouping
        struct TestItem {
            uid: String,
            clean_name: String,
            group: String,
        }

        let mut temp_items: Vec<TestItem> = Vec::new();
        for source in &active_sources {
            let group = source.parent_name.clone().unwrap_or_else(|| "Andere".to_string());
            let clean_name = clean_calendar_display_name(&source.display_name, &group);
            temp_items.push(TestItem {
                uid: source.uid.clone(),
                clean_name,
                group,
            });
        }

        // Count duplicates
        let mut name_counts = std::collections::HashMap::new();
        for it in &temp_items {
            *name_counts.entry(it.clean_name.clone()).or_insert(0) += 1;
        }

        // Build items
        struct FinalItem {
            display_name: String,
            picker_label: String,
            group: String,
        }

        let mut items: Vec<FinalItem> = temp_items
            .into_iter()
            .map(|it| {
                let picker_label = if name_counts.get(&it.clean_name).copied().unwrap_or(0) > 1 {
                    format!("{} ({})", it.clean_name, it.group)
                } else {
                    it.clean_name.clone()
                };
                FinalItem {
                    display_name: it.clean_name,
                    picker_label,
                    group: it.group,
                }
            })
            .collect();

        items.sort_by(|a, b| {
            calendar_group_sort_key(&a.group)
                .cmp(&calendar_group_sort_key(&b.group))
                .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
        });

        assert_eq!(items[0].group, "On This Computer");
        assert_eq!(items[0].display_name, "Personal");
        assert_eq!(items[0].picker_label, "Personal");

        assert_eq!(items[1].group, "EDV");
        assert_eq!(items[1].display_name, "Kalender");
        assert_eq!(items[1].picker_label, "Kalender (EDV)");

        assert_eq!(items[2].group, "mjensen@acme-corp.example");
        assert_eq!(items[2].display_name, "Geburtstage");
        assert_eq!(items[2].picker_label, "Geburtstage");

        assert_eq!(items[3].group, "mjensen@acme-corp.example");
        assert_eq!(items[3].display_name, "Kalender");
        assert_eq!(items[3].picker_label, "Kalender (mjensen@acme-corp.example)");
    }

    #[test]
    fn test_event_dialog_quick_add_escalation_contract() {
        // Verify build_event_component works with basic quick-add parameters
        let component = build_event_component(
            "Quick Add Meeting",
            "",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-09-25 10:00",
            "2026-09-25 11:30",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            "",
        )
        .expect("build_event_component should succeed");
        assert!(component.contains("SUMMARY:Quick Add Meeting"));
        assert!(component.contains("DTSTART:20260925T100000"));
        assert!(component.contains("DTEND:20260925T113000"));
        assert!(component.contains("STATUS:CONFIRMED"));

        // All-day quick add event
        let allday_component = build_event_component(
            "Quick All Day Vacation",
            "",
            "",
            "CONFIRMED",
            false,
            true,
            "2026-09-25",
            "2026-09-25",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            "",
        )
        .expect("build_event_component all-day should succeed");
        assert!(allday_component.contains("SUMMARY:Quick All Day Vacation"));
        assert!(allday_component.contains("DTSTART;VALUE=DATE:20260925"));
        assert!(allday_component.contains("DTEND;VALUE=DATE:20260925"));
    }
}
