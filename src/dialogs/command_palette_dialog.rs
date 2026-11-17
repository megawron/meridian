// SPDX-License-Identifier: GPL-3.0-or-later
//
// CommandPaletteDialog — VS Code-inspired fuzzy command launcher (`Ctrl+Shift+P`)
// surfacing built-in Meridian actions and active extension commands without
// cluttering the daily interface.

use adw::prelude::*;
use gtk::{gdk, glib};
use std::collections::HashSet;
use std::rc::Rc;

use crate::action_contract::PRIMARY_MENU_ACTIONS;
use crate::i18n::gettext;
use crate::services::extension_service::ExtensionRegistry;
use crate::window::MeridianWindow;

#[derive(Debug, Clone)]
enum PaletteTarget {
    AppOrWinAction(String),
    ExtensionCommand(String),
}

#[derive(Debug, Clone)]
struct PaletteEntry {
    title: String,
    accelerator: String,
    search_key: String,
    target: PaletteTarget,
}

pub fn present_command_palette(window: &MeridianWindow) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Command Palette"))
        .content_width(560)
        .content_height(430)
        .follows_content_size(false)
        .build();

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar_view.add_top_bar(&header);

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_start(14)
        .margin_end(14)
        .margin_top(6)
        .margin_bottom(14)
        .build();

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Type a command or action…"))
        .hexpand(true)
        .build();
    outer.append(&search_entry);

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .css_classes(["boxed-list"])
        .build();

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(true)
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    toolbar_view.set_content(Some(&outer));
    dialog.set_child(Some(&toolbar_view));

    let entries = Rc::new(collect_palette_entries());
    let visible_indices: Rc<std::cell::RefCell<Vec<usize>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));

    let populate = {
        let list_box = list_box.clone();
        let entries = Rc::clone(&entries);
        let visible_indices = Rc::clone(&visible_indices);
        Rc::new(move |query: &str| {
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            visible_indices.borrow_mut().clear();

            let tokens: Vec<String> = query
                .trim()
                .to_ascii_lowercase()
                .split_whitespace()
                .map(String::from)
                .collect();

            for (idx, item) in entries.iter().enumerate() {
                if !tokens.iter().all(|tok| item.search_key.contains(tok)) {
                    continue;
                }
                let row = gtk::ListBoxRow::new();
                let hbox = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(12)
                    .margin_start(12)
                    .margin_end(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .build();
                let lbl = gtk::Label::builder()
                    .label(&item.title)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build();
                hbox.append(&lbl);

                if !item.accelerator.is_empty() {
                    let accel_lbl = gtk::ShortcutLabel::new(&item.accelerator);
                    accel_lbl.set_valign(gtk::Align::Center);
                    hbox.append(&accel_lbl);
                }
                row.set_child(Some(&hbox));
                list_box.append(&row);
                visible_indices.borrow_mut().push(idx);
            }

            if let Some(first) = list_box.row_at_index(0) {
                list_box.select_row(Some(&first));
            }
        })
    };

    populate("");

    let populate_for_search = Rc::clone(&populate);
    search_entry.connect_search_changed(move |entry| {
        populate_for_search(&entry.text());
    });

    let execute_row: Rc<dyn Fn(i32)> = {
        let entries = Rc::clone(&entries);
        let visible_indices = Rc::clone(&visible_indices);
        let win_weak = window.downgrade();
        let dlg_weak = dialog.downgrade();
        Rc::new(move |row_idx: i32| {
            if row_idx < 0 {
                return;
            }
            let Some(&entry_idx) = visible_indices.borrow().get(row_idx as usize) else {
                return;
            };
            let chosen = entries[entry_idx].clone();
            if let Some(d) = dlg_weak.upgrade() {
                d.close();
            }
            let Some(win) = win_weak.upgrade() else {
                return;
            };
            match chosen.target {
                PaletteTarget::AppOrWinAction(action_name) => {
                    if let Some(short) = action_name.strip_prefix("app.") {
                        if let Some(app) = win.application() {
                            gtk::prelude::ActionGroupExt::activate_action(&app, short, None);
                        }
                    } else if let Some(short) = action_name.strip_prefix("win.") {
                        gtk::prelude::WidgetExt::activate_action(&win, &format!("win.{short}"), None)
                            .ok();
                    }
                }
                PaletteTarget::ExtensionCommand(cmd_id) => {
                    win.execute_extension_command(&cmd_id);
                }
            }
        })
    };

    let exec_for_activate = Rc::clone(&execute_row);
    list_box.connect_row_activated(move |_, row| {
        exec_for_activate(row.index());
    });

    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let list_for_keys = list_box.clone();
    let exec_for_keys = Rc::clone(&execute_row);
    key_ctrl.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Down => {
            let next = list_for_keys
                .selected_row()
                .map(|r| r.index() + 1)
                .unwrap_or(0);
            if let Some(row) = list_for_keys.row_at_index(next) {
                list_for_keys.select_row(Some(&row));
                row.grab_focus();
            }
            glib::Propagation::Stop
        }
        gdk::Key::Up => {
            let prev = list_for_keys
                .selected_row()
                .map(|r| (r.index() - 1).max(0))
                .unwrap_or(0);
            if let Some(row) = list_for_keys.row_at_index(prev) {
                list_for_keys.select_row(Some(&row));
            }
            glib::Propagation::Stop
        }
        gdk::Key::Return | gdk::Key::KP_Enter => {
            if let Some(sel) = list_for_keys.selected_row() {
                exec_for_keys(sel.index());
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
        _ => glib::Propagation::Proceed,
    });
    search_entry.add_controller(key_ctrl);

    adw::prelude::AdwDialogExt::set_focus(&dialog, Some(&search_entry));
    dialog.present(Some(window));
}

fn collect_palette_entries() -> Vec<PaletteEntry> {
    let mut seen_actions = HashSet::new();
    let mut out = Vec::new();

    // Extension commands first so installed add-ons feel first-class
    for (ext, cmd) in ExtensionRegistry::default().enabled_commands() {
        let title = if cmd.title.contains(':') {
            cmd.title.clone()
        } else {
            format!("{}: {}", ext.manifest.name, cmd.title)
        };
        let search_key = format!(
            "{} {} {}",
            title.to_ascii_lowercase(),
            ext.manifest.name.to_ascii_lowercase(),
            cmd.id.to_ascii_lowercase()
        );
        out.push(PaletteEntry {
            title,
            accelerator: cmd.shortcut,
            search_key,
            target: PaletteTarget::ExtensionCommand(cmd.id),
        });
    }

    for spec in PRIMARY_MENU_ACTIONS {
        if !seen_actions.insert(spec.action) {
            continue;
        }
        let localized = gettext(spec.label).trim_end_matches('…').trim_end_matches("...").to_string();
        let accel = crate::application::primary_accelerator(spec.action)
            .unwrap_or("")
            .to_string();
        let search_key = format!(
            "{} {} {}",
            localized.to_ascii_lowercase(),
            spec.label.to_ascii_lowercase(),
            spec.action.to_ascii_lowercase()
        );
        out.push(PaletteEntry {
            title: localized,
            accelerator: accel,
            search_key,
            target: PaletteTarget::AppOrWinAction(spec.action.to_string()),
        });
    }

    out
}
