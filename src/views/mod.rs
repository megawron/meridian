// SPDX-License-Identifier: GPL-3.0-or-later
//
// Views module — contains all major page views of the application.

pub(crate) mod accessibility;
pub(crate) mod calendar_page;
pub(crate) mod contacts_page;
pub(crate) mod context_menu;
mod html_editor;
pub mod itip_banner;
mod mail_page;
mod mail_viewer;
mod mail_viewer_window;
mod memos_page;
pub mod rich_text_field;
mod tasks_page;

pub(crate) use context_menu::*;

pub use calendar_page::CalendarPage;
#[allow(unused_imports)]
pub(crate) use calendar_page::CalendarView;
pub use contacts_page::ContactsPage;
#[allow(unused_imports)]
pub(crate) use contacts_page::ContactTableColumn;
pub use html_editor::EditorContent;
pub use html_editor::HtmlEditor;
pub use rich_text_field::RichTextField;
pub use itip_banner::ItipBanner;
pub use mail_page::MailPage;
pub(crate) use mail_page::MailDrillDownLevel;
pub(crate) use mail_page::clean_name_for_avatar;
pub(crate) use mail_page::parse_flexible_datetime;
pub(crate) use mail_page::set_button_icon_with_fallback;
pub(crate) use mail_viewer::MailPrintDialogResult;
pub use mail_viewer::MailViewer;
pub use mail_viewer_window::MailViewerWindow;
pub(crate) use mail_viewer::{MessageSearchResult, MessageSearchStatus, extract_email_address};
pub use memos_page::MemosPage;
pub use tasks_page::TasksPage;

use gdk::prelude::*;
use gtk::prelude::*;

fn module_icon_widget(name: &str) -> gtk::Image {
    let (icon_name, fallback_name) = match name {
        "mail" => ("evolution-mail", "mail-unread-symbolic"),
        "contacts" => ("x-office-address-book", "x-office-address-book-symbolic"),
        "calendar" => ("x-office-calendar", "x-office-calendar-symbolic"),
        "tasks" => ("view-list-bullet-symbolic", "tasks-symbolic"),
        "memos" => ("accessories-text-editor", "accessories-text-editor-symbolic"),
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
        .margin_start(0)
        .margin_end(0)
        .margin_top(0)
        .margin_bottom(0)
        .valign(gtk::Align::End)
        .vexpand(false)
        .css_classes(vec!["meridian-module-switcher"])
        .build();

    let grid = gtk::Grid::builder()
        .column_spacing(4)
        .row_spacing(4)
        .column_homogeneous(true)
        .row_homogeneous(false)
        .build();

    let modules: [(&str, String); 4] = [
        ("mail", crate::i18n::gettext("Mail")),
        ("contacts", crate::i18n::gettext("Contacts")),
        ("calendar", crate::i18n::gettext("Calendar")),
        ("tasks", crate::i18n::gettext("Tasks")),
    ];

    let mut buttons_vec = Vec::new();
    let mut labels_vec = Vec::new();

    for (name, label) in &modules {
        let is_active = *name == active_view;
        let lbl = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        labels_vec.push(lbl.clone());

        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bx.set_margin_start(4);
        bx.set_margin_end(4);
        bx.append(&module_icon_widget(name));
        bx.append(&lbl);

        let btn = gtk::Button::builder()
            .child(&bx)
            .halign(gtk::Align::Fill)
            .tooltip_text(label)
            .css_classes(if is_active {
                vec!["suggested-action"]
            } else {
                Vec::new()
            })
            .build();

        let widget_weak = anchor_widget.downgrade();
        let target_name = name.to_string();
        btn.connect_clicked(move |_| {
            if let Some(w) = widget_weak.upgrade() {
                if let Some(window) = w.root().and_downcast::<crate::window::MeridianWindow>() {
                    window.set_active_view(&target_name);
                }
            }
        });
        buttons_vec.push(btn);
    }

    let buttons: [gtk::Button; 4] = match buttons_vec.try_into() {
        Ok(arr) => arr,
        Err(_) => unreachable!(),
    };
    let labels: [gtk::Label; 4] = match labels_vec.try_into() {
        Ok(arr) => arr,
        Err(_) => unreachable!(),
    };

    let current_mode = std::rc::Rc::new(std::cell::Cell::new(0u8));

    let apply_layout = {
        let grid = grid.clone();
        let buttons = buttons.clone();
        let labels = labels.clone();
        let current_mode = std::rc::Rc::clone(&current_mode);
        move |width: i32| {
            if width <= 0 {
                return;
            }
            // Layout modes:
            // 4: 4 vertical full-width rows (width < 180)
            // 1: 2x2 symmetric grid (180 <= width < 540)
            // 3: 1x4 horizontal row (width >= 540)
            let new_mode = if width < 180 {
                4
            } else if width < 540 {
                1
            } else {
                3
            };

            // All button labels remain visible across modes to prevent text truncation
            for lbl in &labels {
                lbl.set_visible(true);
            }

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
                    grid.attach(&buttons[0], 0, 0, 1, 1);
                    grid.attach(&buttons[1], 1, 0, 1, 1);
                    grid.attach(&buttons[2], 0, 1, 1, 1);
                    grid.attach(&buttons[3], 1, 1, 1, 1);
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

    let last_w = std::rc::Rc::new(std::cell::Cell::new(-1i32));
    let apply_layout_for_tick = apply_layout.clone();
    let last_w_clone = std::rc::Rc::clone(&last_w);
    switcher_box.add_tick_callback(move |widget, _| {
        let w = widget.width();
        if w > 0 && w != last_w_clone.get() {
            last_w_clone.set(w);
            apply_layout_for_tick(w);
        }
        glib::ControlFlow::Continue
    });

    switcher_box.append(&grid);

    if let Some(settings) = crate::services::AppSettings::try_new() {
        let is_bottom = settings.switcher_location() != "top";
        switcher_box.set_visible(is_bottom);
        settings.connect_changed(
            Some("switcher-location"),
            glib::clone!(
                #[weak]
                switcher_box,
                move |s, _| {
                    let loc = s.string("switcher-location");
                    switcher_box.set_visible(loc.as_str() != "top");
                }
            ),
        );
    } else {
        switcher_box.set_visible(false);
    }

    switcher_box
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_sidebar_module_switcher() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let anchor = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let switcher = build_sidebar_module_switcher("mail", &anchor);
        assert!(switcher.has_css_class("meridian-module-switcher"));
        let grid = switcher.first_child().and_downcast::<gtk::Grid>().expect("Grid");
        let mut count = 0;
        let mut child = grid.first_child();
        while let Some(c) = child {
            count += 1;
            child = c.next_sibling();
        }
        assert_eq!(count, 4, "Expected 4 buttons in switcher grid");
    }

    #[test]
    fn test_sidebar_module_switcher_labels_remain_visible() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let anchor = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let switcher = build_sidebar_module_switcher("mail", &anchor);
        let grid = switcher.first_child().and_downcast::<gtk::Grid>().expect("Grid");
        let mut child = grid.first_child();
        while let Some(c) = child {
            if let Some(btn) = c.downcast_ref::<gtk::Button>() {
                if let Some(bx) = btn.child().and_downcast::<gtk::Box>() {
                    // Second child is the label
                    if let Some(lbl_widget) = bx.last_child() {
                        assert!(lbl_widget.is_visible(), "Module label should be visible");
                    }
                }
            }
            child = c.next_sibling();
        }
    }

    fn drain_main_context(context: &gtk::glib::MainContext) {
        for _ in 0..32 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
    }

    #[test]
    fn test_sidebar_module_switcher_visibility_tracks_setting() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = crate::services::AppSettings::try_new() else {
            return;
        };
        let original = settings.switcher_location();

        settings.set_switcher_location("bottom-left");
        drain_main_context(&context);
        let anchor = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let switcher = build_sidebar_module_switcher("mail", &anchor);
        assert!(switcher.is_visible());

        settings.set_switcher_location("top");
        drain_main_context(&context);
        assert!(!switcher.is_visible());

        settings.set_switcher_location("bottom-left");
        drain_main_context(&context);
        assert!(switcher.is_visible());

        settings.set_switcher_location(&original);
    }

    #[test]
    fn test_sidebar_module_switcher_anchors_to_bottom() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let anchor = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let switcher = build_sidebar_module_switcher("mail", &anchor);
        assert_eq!(switcher.valign(), gtk::Align::End);
        assert!(!switcher.vexpands());
    }

    #[test]
    fn test_contacts_module_icon_name() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let img = module_icon_widget("contacts");
        let icon_name = img.icon_name().map(|g| g.to_string()).unwrap_or_default();
        assert!(
            icon_name == "x-office-address-book" || icon_name == "x-office-address-book-symbolic",
            "Expected address book icon for contacts module, got: {icon_name}"
        );
    }
}

