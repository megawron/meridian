// SPDX-License-Identifier: GPL-3.0-or-later
//
// Views module — contains all major page views of the application.

pub(crate) mod accessibility;
mod calendar_page;
mod contacts_page;
#[cfg(target_os = "linux")]
mod html_editor;
#[cfg(not(target_os = "linux"))]
#[path = "html_editor_fallback.rs"]
mod html_editor;
pub mod itip_banner;
mod mail_page;
mod mail_viewer;
mod memos_page;
mod tasks_page;

pub use calendar_page::CalendarPage;
pub use contacts_page::ContactsPage;
#[allow(unused_imports)]
pub use html_editor::EditorContent;
pub use html_editor::HtmlEditor;
pub use itip_banner::ItipBanner;
pub use mail_page::MailPage;
#[cfg(target_os = "linux")]
pub(crate) use mail_viewer::MailPrintDialogResult;
pub use mail_viewer::MailViewer;
pub(crate) use mail_viewer::{MessageSearchResult, MessageSearchStatus};
pub use memos_page::MemosPage;
pub use tasks_page::TasksPage;

use gdk::prelude::*;
use gtk::prelude::*;

fn module_icon_widget(name: &str) -> gtk::Image {
    let (icon_name, fallback_name) = match name {
        "mail" => ("evolution-mail", "mail-unread-symbolic"),
        "contacts" => ("x-office-address-book", "contact-new-symbolic"),
        "calendar" => ("x-office-calendar", "x-office-calendar-symbolic"),
        "tasks" => ("evolution-tasks", "checkbox-checked-symbolic"),
        "memos" => ("evolution-memos", "accessory-text-editor-symbolic"),
        _ => ("evolution-mail", "mail-unread-symbolic"),
    };

    let display = gdk::Display::default();
    if let Some(theme) = display.as_ref().map(gtk::IconTheme::for_display) {
        if theme.has_icon(icon_name) {
            return gtk::Image::from_icon_name(icon_name);
        }
    }
    gtk::Image::from_icon_name(fallback_name)
}

pub fn build_sidebar_module_switcher(
    active_view: &str,
    anchor_widget: &impl glib::object::IsA<gtk::Widget>,
) -> gtk::Box {
    let switcher_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .css_classes(vec!["evolution-module-switcher"])
        .build();

    let grid = gtk::Grid::builder()
        .column_spacing(4)
        .row_spacing(4)
        .column_homogeneous(true)
        .row_homogeneous(false)
        .build();

    let modules = [
        ("mail", "Mail"),
        ("contacts", "Contacts"),
        ("calendar", "Calendar"),
        ("tasks", "Tasks"),
        ("memos", "Memos"),
    ];

    let mut buttons_vec = Vec::new();

    for (name, label) in modules {
        let is_active = name == active_view;
        let btn = gtk::Button::builder()
            .child(&{
                let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                bx.set_margin_start(4);
                bx.set_margin_end(4);
                bx.append(&module_icon_widget(name));
                let lbl = gtk::Label::builder()
                    .label(label)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .build();
                bx.append(&lbl);
                bx
            })
            .halign(gtk::Align::Fill)
            .css_classes(if is_active {
                vec!["suggested-action"]
            } else {
                vec!["flat"]
            })
            .build();

        let widget_weak = anchor_widget.downgrade();
        let target_name = name.to_string();
        btn.connect_clicked(move |_| {
            if let Some(w) = widget_weak.upgrade() {
                if let Some(window) = w.root().and_downcast::<crate::window::EvolutionWindow>() {
                    window.set_active_view(&target_name);
                }
            }
        });
        buttons_vec.push(btn);
    }

    let buttons: [gtk::Button; 5] = match buttons_vec.try_into() {
        Ok(arr) => arr,
        Err(_) => unreachable!(),
    };

    let current_mode = std::rc::Rc::new(std::cell::Cell::new(0u8));

    let apply_layout = {
        let grid = grid.clone();
        let buttons = buttons.clone();
        let current_mode = std::rc::Rc::clone(&current_mode);
        move |width: i32| {
            if width <= 0 {
                return;
            }
            // Layout modes:
            // 4: 5 vertical full-width rows (width < 200)
            // 1: 1 top full-width + 2x2 grid (200 <= width < 340)
            // 2: 1 top full-width + 1x4 row (340 <= width < 480)
            // 3: 1x5 horizontal row (width >= 480)
            let new_mode = if width < 200 {
                4
            } else if width < 340 {
                1
            } else if width < 480 {
                2
            } else {
                3
            };

            if current_mode.get() == new_mode {
                return;
            }
            current_mode.set(new_mode);

            while let Some(child) = grid.first_child() {
                grid.remove(&child);
            }

            match new_mode {
                4 => {
                    for (i, btn) in buttons.iter().enumerate() {
                        grid.attach(btn, 0, i as i32, 1, 1);
                    }
                }
                1 => {
                    grid.attach(&buttons[0], 0, 0, 2, 1);
                    grid.attach(&buttons[1], 0, 1, 1, 1);
                    grid.attach(&buttons[2], 1, 1, 1, 1);
                    grid.attach(&buttons[3], 0, 2, 1, 1);
                    grid.attach(&buttons[4], 1, 2, 1, 1);
                }
                2 => {
                    grid.attach(&buttons[0], 0, 0, 4, 1);
                    grid.attach(&buttons[1], 0, 1, 1, 1);
                    grid.attach(&buttons[2], 1, 1, 1, 1);
                    grid.attach(&buttons[3], 2, 1, 1, 1);
                    grid.attach(&buttons[4], 3, 1, 1, 1);
                }
                _ => {
                    for (i, btn) in buttons.iter().enumerate() {
                        grid.attach(btn, i as i32, 0, 1, 1);
                    }
                }
            }
        }
    };

    // Initial default layout: 1 top + 2x2 grid (standard ~240px sidebar)
    apply_layout(240);

    let apply_layout_for_notify = apply_layout.clone();
    switcher_box.connect_notify_local(Some("width"), move |widget, _| {
        let w = widget.width();
        apply_layout_for_notify(w);
    });

    switcher_box.append(&grid);

    if let Some(settings) = crate::services::AppSettings::try_new() {
        let switcher_clone = switcher_box.clone();
        switcher_clone.set_visible(settings.switcher_location() == "bottom-left");
        settings.connect_changed(Some("switcher-location"), move |s, _| {
            switcher_clone.set_visible(s.string("switcher-location") == "bottom-left");
        });
    }

    switcher_box
}
