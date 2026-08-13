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
mod mail_page;
mod mail_viewer;
mod memos_page;
mod tasks_page;

pub use calendar_page::CalendarPage;
pub use contacts_page::ContactsPage;
#[allow(unused_imports)]
pub use html_editor::EditorContent;
pub use html_editor::HtmlEditor;
pub use mail_page::MailPage;
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
        .row_homogeneous(true)
        .build();

    // Static fixed module layout — position and order never change.
    let modules = [
        ("mail", "Mail", 0, 0, 1),
        ("contacts", "Kontakte", 1, 0, 1),
        ("calendar", "Kalender", 0, 1, 1),
        ("tasks", "Aufgaben", 1, 1, 1),
        ("memos", "Memos", 0, 2, 2),
    ];

    for (name, label, col, row, width) in modules {
        let is_active = name == active_view;
        let btn = gtk::Button::builder()
            .child(&{
                let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                bx.append(&module_icon_widget(name));
                bx.append(&gtk::Label::new(Some(label)));
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
        grid.attach(&btn, col, row, width, 1);
    }

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
