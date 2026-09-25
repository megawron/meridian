// SPDX-License-Identifier: GPL-3.0-or-later
//
// QuickAddPopover — Compact, lightweight appointment creation popover.
// Upstream parity with GNOME Calendar's GcalQuickAddPopover and Google Calendar's inline event creator.

use adw::prelude::*;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike, Weekday};
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickAddCalendarItem {
    pub uid: String,
    pub display_name: String,
    pub color: String,
}

#[derive(Debug, Clone)]
pub struct QuickAddEventDraft {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
    pub summary: String,
    pub source_uid: String,
}

pub fn weekday_short_name(date: NaiveDate) -> String {
    match date.weekday() {
        Weekday::Mon => crate::i18n::gettext("Mon"),
        Weekday::Tue => crate::i18n::gettext("Tue"),
        Weekday::Wed => crate::i18n::gettext("Wed"),
        Weekday::Thu => crate::i18n::gettext("Thu"),
        Weekday::Fri => crate::i18n::gettext("Fri"),
        Weekday::Sat => crate::i18n::gettext("Sat"),
        Weekday::Sun => crate::i18n::gettext("Sun"),
    }
}

pub fn weekday_long_name(date: NaiveDate) -> String {
    match date.weekday() {
        Weekday::Mon => crate::i18n::gettext("Monday"),
        Weekday::Tue => crate::i18n::gettext("Tuesday"),
        Weekday::Wed => crate::i18n::gettext("Wednesday"),
        Weekday::Thu => crate::i18n::gettext("Thursday"),
        Weekday::Fri => crate::i18n::gettext("Friday"),
        Weekday::Sat => crate::i18n::gettext("Saturday"),
        Weekday::Sun => crate::i18n::gettext("Sunday"),
    }
}

pub fn month_name(month: u32) -> String {
    match month {
        1 => crate::i18n::gettext("January"),
        2 => crate::i18n::gettext("February"),
        3 => crate::i18n::gettext("March"),
        4 => crate::i18n::gettext("April"),
        5 => crate::i18n::gettext("May"),
        6 => crate::i18n::gettext("June"),
        7 => crate::i18n::gettext("July"),
        8 => crate::i18n::gettext("August"),
        9 => crate::i18n::gettext("September"),
        10 => crate::i18n::gettext("October"),
        11 => crate::i18n::gettext("November"),
        12 => crate::i18n::gettext("December"),
        _ => String::new(),
    }
}

pub fn format_date_human(date: NaiveDate) -> String {
    format!(
        "{}, {}. {}",
        weekday_long_name(date),
        date.day(),
        month_name(date.month())
    )
}

pub fn format_date_short(date: NaiveDate) -> String {
    format!(
        "{}, {}. {}",
        weekday_short_name(date),
        date.day(),
        month_name(date.month())
    )
}

pub fn format_quick_add_date_time(
    start: NaiveDateTime,
    end: NaiveDateTime,
    all_day: bool,
) -> String {
    let s_date = start.date();
    let e_date = end.date();
    if all_day {
        if s_date == e_date
            || (e_date == s_date + chrono::Duration::days(1)
                && end.time() == chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap())
        {
            format!(
                "{} • {}",
                format_date_human(s_date),
                crate::i18n::gettext("All day")
            )
        } else {
            let actual_end = if end.time() == chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
                && e_date > s_date
            {
                e_date - chrono::Duration::days(1)
            } else {
                e_date
            };
            if actual_end == s_date {
                format!(
                    "{} • {}",
                    format_date_human(s_date),
                    crate::i18n::gettext("All day")
                )
            } else {
                format!(
                    "{} – {} • {}",
                    format_date_short(s_date),
                    format_date_short(actual_end),
                    crate::i18n::gettext("All day")
                )
            }
        }
    } else if s_date == e_date {
        format!(
            "{}, {:02}:{:02} – {:02}:{:02}",
            format_date_human(s_date),
            start.hour(),
            start.minute(),
            end.hour(),
            end.minute()
        )
    } else {
        format!(
            "{}, {:02}:{:02} – {}, {:02}:{:02}",
            format_date_short(s_date),
            start.hour(),
            start.minute(),
            format_date_short(e_date),
            end.hour(),
            end.minute()
        )
    }
}

pub fn present_quick_add_popover<W: IsA<gtk::Widget> + Clone + 'static, F, G>(
    parent: &W,
    pointing_to: Option<gtk::gdk::Rectangle>,
    start: NaiveDateTime,
    end: NaiveDateTime,
    all_day: bool,
    sources: Vec<QuickAddCalendarItem>,
    default_source_uid: Option<String>,
    on_save: F,
    on_edit_details: G,
) -> gtk::Popover
where
    F: Fn(QuickAddEventDraft) + 'static,
    G: Fn(QuickAddEventDraft) + 'static,
{
    let popover = gtk::Popover::builder()
        .autohide(true)
        .has_arrow(true)
        .css_classes(vec!["calendar-quick-add-popover"])
        .build();

    if let Some(ref rect) = pointing_to {
        popover.set_pointing_to(Some(rect));
    }
    popover.set_parent(parent);

    popover.connect_closed(move |p| {
        p.unparent();
    });

    let main_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(14)
        .margin_end(14)
        .margin_top(14)
        .margin_bottom(14)
        .width_request(340)
        .css_classes(vec!["calendar-quick-add-content"])
        .build();

    // ── Row 1: Title Entry + Close Button ──
    let top_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();

    let title_entry = gtk::Entry::builder()
        .placeholder_text(&crate::i18n::gettext("Add title"))
        .hexpand(true)
        .css_classes(vec!["quick-add-title-entry"])
        .build();
    top_box.append(&title_entry);

    let btn_close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(vec!["flat", "circular"])
        .tooltip_text(&crate::i18n::gettext("Close"))
        .build();
    let popover_weak_close = popover.downgrade();
    btn_close.connect_clicked(move |_| {
        if let Some(p) = popover_weak_close.upgrade() {
            p.popdown();
        }
    });
    top_box.append(&btn_close);
    main_box.append(&top_box);

    // ── Row 2: Date & Time Display ──
    let time_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .valign(gtk::Align::Center)
        .build();

    let time_icon = gtk::Image::builder()
        .icon_name("appointment-soon-symbolic")
        .pixel_size(16)
        .css_classes(vec!["dim-label"])
        .build();
    time_box.append(&time_icon);

    let formatted_time = format_quick_add_date_time(start, end, all_day);
    let time_label = gtk::Label::builder()
        .label(&formatted_time)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(vec!["quick-add-time-label"])
        .build();
    time_box.append(&time_label);
    main_box.append(&time_box);

    // ── Row 3: Calendar Selector ──
    let cal_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .valign(gtk::Align::Center)
        .build();

    let initial_color = sources
        .iter()
        .find(|s| default_source_uid.as_deref() == Some(&s.uid))
        .map(|s| s.color.clone())
        .or_else(|| sources.first().map(|s| s.color.clone()))
        .unwrap_or_else(|| "#3584e4".to_string());

    let dot_color = Rc::new(RefCell::new(initial_color));
    let dot_color_draw = Rc::clone(&dot_color);

    let dot = gtk::DrawingArea::builder()
        .content_width(12)
        .content_height(12)
        .valign(gtk::Align::Center)
        .css_classes(vec!["calendar-quick-add-color-dot"])
        .build();
    dot.set_draw_func(move |_, cr, width, height| {
        let color_str = dot_color_draw.borrow();
        let rgba = gtk::gdk::RGBA::parse(color_str.as_str())
            .unwrap_or(gtk::gdk::RGBA::new(0.2, 0.5, 0.9, 1.0));
        let cx = width as f64 / 2.0;
        let cy = height as f64 / 2.0;
        let r = (cx.min(cy) - 0.5).max(1.0);
        cr.arc(cx, cy, r, 0.0, 2.0 * std::f64::consts::PI);
        cr.set_source_rgba(
            rgba.red() as f64,
            rgba.green() as f64,
            rgba.blue() as f64,
            rgba.alpha() as f64,
        );
        let _ = cr.fill();
    });
    cal_box.append(&dot);

    let cal_names: Vec<String> = sources.iter().map(|s| s.display_name.clone()).collect();
    let cal_names_refs: Vec<&str> = cal_names.iter().map(String::as_str).collect();
    let cal_model = gtk::StringList::new(&cal_names_refs);

    let cal_dropdown = gtk::DropDown::builder()
        .model(&cal_model)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();

    if let Some(ref def_uid) = default_source_uid {
        if let Some(idx) = sources.iter().position(|s| &s.uid == def_uid) {
            cal_dropdown.set_selected(idx as u32);
        }
    }

    let dot_color_select = Rc::clone(&dot_color);
    let dot_weak = dot.downgrade();
    let sources_for_select = sources.clone();
    cal_dropdown.connect_selected_notify(move |dropdown| {
        let idx = dropdown.selected() as usize;
        if let Some(src) = sources_for_select.get(idx) {
            *dot_color_select.borrow_mut() = src.color.clone();
            if let Some(d) = dot_weak.upgrade() {
                d.queue_draw();
            }
        }
    });

    cal_box.append(&cal_dropdown);
    main_box.append(&cal_box);

    // ── Row 4: Buttons (Edit Details + Save) ──
    let btn_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .valign(gtk::Align::Center)
        .build();

    let btn_edit_details = gtk::Button::builder()
        .label(&crate::i18n::gettext("Edit details…"))
        .css_classes(vec!["flat"])
        .build();
    btn_box.append(&btn_edit_details);

    let spacer = gtk::Box::builder().hexpand(true).build();
    btn_box.append(&spacer);

    let btn_save = gtk::Button::builder()
        .label(&crate::i18n::gettext("Save"))
        .css_classes(vec!["suggested-action"])
        .build();
    btn_box.append(&btn_save);
    main_box.append(&btn_box);

    popover.set_child(Some(&main_box));

    // ── Action Handlers ──
    let save_action = {
        let popover_weak = popover.downgrade();
        let title_entry = title_entry.clone();
        let cal_dropdown = cal_dropdown.clone();
        let sources = sources.clone();
        let on_save = on_save;
        Rc::new(move || {
            let text = title_entry.text().to_string();
            let summary = if text.trim().is_empty() {
                crate::i18n::gettext("New Event")
            } else {
                text.trim().to_string()
            };
            let selected_idx = cal_dropdown.selected() as usize;
            let source_uid = sources
                .get(selected_idx)
                .map(|s| s.uid.clone())
                .unwrap_or_else(|| "local-calendar".to_string());

            let draft = QuickAddEventDraft {
                start,
                end,
                all_day,
                summary,
                source_uid,
            };

            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
            on_save(draft);
        })
    };

    let save_for_entry = Rc::clone(&save_action);
    title_entry.connect_activate(move |_| {
        save_for_entry();
    });

    let save_for_btn = Rc::clone(&save_action);
    btn_save.connect_clicked(move |_| {
        save_for_btn();
    });

    let edit_action = {
        let popover_weak = popover.downgrade();
        let title_entry = title_entry.clone();
        let cal_dropdown = cal_dropdown.clone();
        let sources = sources.clone();
        let on_edit_details = on_edit_details;
        Rc::new(move || {
            let text = title_entry.text().to_string();
            let summary = text.trim().to_string();
            let selected_idx = cal_dropdown.selected() as usize;
            let source_uid = sources
                .get(selected_idx)
                .map(|s| s.uid.clone())
                .unwrap_or_else(|| "local-calendar".to_string());

            let draft = QuickAddEventDraft {
                start,
                end,
                all_day,
                summary,
                source_uid,
            };

            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
            on_edit_details(draft);
        })
    };

    btn_edit_details.connect_clicked(move |_| {
        edit_action();
    });

    // Auto-focus title entry immediately after popover opens
    let title_entry_focus = title_entry.clone();
    glib::idle_add_local_once(move || {
        title_entry_focus.grab_focus();
    });

    popover.popup();
    popover
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_quick_add_date_time_same_day() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 25)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 25)
            .unwrap()
            .and_hms_opt(11, 30, 0)
            .unwrap();
        let formatted = format_quick_add_date_time(start, end, false);
        assert!(formatted.contains("10:00 – 11:30"));
        assert!(formatted.contains("25"));
    }

    #[test]
    fn test_format_quick_add_date_time_all_day_single_day() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 25)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 25)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let formatted = format_quick_add_date_time(start, end, true);
        assert!(formatted.contains("25"));
        assert!(formatted.contains("Ganztägig") || formatted.contains("All day"));
    }

    #[test]
    fn test_format_quick_add_date_time_all_day_multi_day() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 25)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 28)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let formatted = format_quick_add_date_time(start, end, true);
        assert!(formatted.contains("25"));
        assert!(formatted.contains("27") || formatted.contains("28"));
    }
}
