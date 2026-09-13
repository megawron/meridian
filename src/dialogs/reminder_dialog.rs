// SPDX-License-Identifier: GPL-3.0-or-later
//
// Reusable GTK4 reminder surface.  The dialog owns only GTK state; all
// snapshot, claim and persistence work is supplied by the application through
// callbacks and therefore remains outside the main loop.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::services::reminder_service::{
    ReminderDelivery, ReminderIdentity, deduplicate_deliveries,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderDialogAction {
    Snooze(i64),
    Dismiss,
}

struct ReminderEntry {
    row: gtk::ListBoxRow,
    busy: Rc<Cell<bool>>,
    identity: ReminderIdentity,
    delivery: ReminderDelivery,
    action_target: String,
}

#[derive(Clone)]
pub struct ReminderDialog {
    window: adw::Window,
    list: gtk::ListBox,
    empty_state: gtk::Label,
    error_label: gtk::Label,
    entries: Rc<RefCell<BTreeMap<String, ReminderEntry>>>,
    on_open: Rc<dyn Fn(ReminderDelivery) -> bool>,
    on_action: Rc<dyn Fn(ReminderDelivery, ReminderDialogAction) -> bool>,
}

impl ReminderDialog {
    pub fn new<P, Open, Action>(parent: Option<&P>, on_open: Open, on_action: Action) -> Self
    where
        P: IsA<gtk::Window>,
        Open: Fn(ReminderDelivery) -> bool + 'static,
        Action: Fn(ReminderDelivery, ReminderDialogAction) -> bool + 'static,
    {
        let window = adw::Window::builder()
            .title("Erinnerungen")
            .default_width(720)
            .default_height(420)
            .build();
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
        }

        let header_bar = adw::HeaderBar::new();
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();

        let empty_state = gtk::Label::builder()
            .label("Keine fälligen Erinnerungen")
            .css_classes(vec!["dim-label"])
            .margin_top(24)
            .margin_bottom(24)
            .visible(false)
            .build();
        let error_label = gtk::Label::builder()
            .css_classes(vec!["error"])
            .wrap(true)
            .xalign(0.0)
            .margin_start(16)
            .margin_end(16)
            .margin_bottom(8)
            .visible(false)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&error_label);
        content.append(&empty_state);
        content.append(&scrolled);
        toolbar_view.set_content(Some(&content));
        window.set_content(Some(&toolbar_view));

        Self {
            window,
            list,
            empty_state,
            error_label,
            entries: Rc::new(RefCell::new(BTreeMap::new())),
            on_open: Rc::new(on_open),
            on_action: Rc::new(on_action),
        }
    }

    pub fn present_deliveries(&self, deliveries: &[ReminderDelivery]) {
        self.replace_deliveries(deliveries);
        if self.has_entries() {
            self.window.present();
        }
    }

    pub fn present_existing(&self) {
        if self.has_entries() {
            self.window.present();
        }
    }

    pub fn has_entries(&self) -> bool {
        !self.entries.borrow().is_empty()
    }

    pub fn finish_action(&self, target: &str, success: bool, error: Option<&str>) {
        let key = self
            .entries
            .borrow()
            .iter()
            .find(|(_, entry)| entry.action_target == target)
            .map(|(key, _)| key.clone());
        let Some(key) = key else {
            // A lease may have expired and the same candidate may already
            // have been replaced with a fresh claim.  Never let the stale
            // completion remove or re-enable that newer row.
            return;
        };
        if success {
            if let Some(entry) = self.entries.borrow_mut().remove(&key) {
                entry.row.unparent();
            }
            self.empty_state.set_visible(!self.has_entries());
            self.error_label.set_visible(false);
            if !self.has_entries() {
                self.window.set_visible(false);
            }
            return;
        }

        if let Some(entry) = self.entries.borrow().get(&key) {
            entry.busy.set(false);
            entry.row.set_sensitive(true);
        }
        self.error_label
            .set_label(error.unwrap_or("Reminder-Aktion konnte nicht gespeichert werden"));
        self.error_label.set_visible(true);
    }

    pub fn finish_open(&self, identity: &ReminderIdentity, success: bool, error: Option<&str>) {
        for entry in self
            .entries
            .borrow()
            .values()
            .filter(|entry| &entry.identity == identity)
        {
            let row = entry.row.clone();
            let busy = Rc::clone(&entry.busy);
            busy.set(false);
            row.set_sensitive(true);
        }
        if success {
            self.error_label.set_visible(false);
        } else {
            self.error_label
                .set_label(error.unwrap_or("Komponente konnte nicht geöffnet werden"));
            self.error_label.set_visible(true);
        }
    }

    pub fn show_error(&self, message: &str) {
        self.error_label.set_label(message);
        self.error_label.set_visible(true);
    }

    fn replace_deliveries(&self, deliveries: &[ReminderDelivery]) {
        self.error_label.set_visible(false);

        for delivery in deduplicate_deliveries(deliveries) {
            let Ok(identity) = delivery.identity() else {
                self.show_error("Reminder-Eintrag mit ungültiger Identität wurde verworfen");
                continue;
            };
            let key = delivery.candidate.key.clone();
            let Ok(action_target) = delivery.action_target() else {
                self.show_error("Reminder-Eintrag mit ungültigem Aktionsziel wurde verworfen");
                continue;
            };
            let unchanged = self.entries.borrow().get(&key).is_some_and(|entry| {
                entry.action_target == action_target && entry.delivery == delivery
            });
            if unchanged {
                continue;
            }
            if let Some(entry) = self.entries.borrow_mut().remove(&key) {
                entry.row.unparent();
            }
            let (row, busy) = self.build_row(delivery.clone());
            self.list.append(&row);
            self.entries.borrow_mut().insert(
                key,
                ReminderEntry {
                    row,
                    busy,
                    identity,
                    delivery,
                    action_target,
                },
            );
        }
        self.empty_state.set_visible(!self.has_entries());
    }

    fn build_row(&self, delivery: ReminderDelivery) -> (gtk::ListBoxRow, Rc<Cell<bool>>) {
        let kind_label = delivery
            .identity()
            .map(|identity| match identity.kind {
                crate::services::reminder_service::ReminderComponentKind::Event => "Termin",
                crate::services::reminder_service::ReminderComponentKind::Task => "Aufgabe",
                crate::services::reminder_service::ReminderComponentKind::Memo => "Notiz",
                crate::services::reminder_service::ReminderComponentKind::Contact => "Kontakt",
            })
            .unwrap_or("Reminder");
        let due_label = chrono::DateTime::from_timestamp(delivery.candidate.due_at, 0)
            .map(|timestamp| {
                timestamp
                    .with_timezone(&chrono::Local)
                    .format("%d.%m.%Y %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "Fällig".to_string());
        let action_row = adw::ActionRow::builder()
            .title(&delivery.candidate.title)
            .subtitle(&format!("{kind_label} · {due_label}"))
            .build();
        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .valign(gtk::Align::Center)
            .build();
        let open_button = gtk::Button::with_label("Öffnen");
        let snooze_10_button = gtk::Button::with_label("10 min");
        let snooze_hour_button = gtk::Button::with_label("1 h");
        let dismiss_button = gtk::Button::with_label("Erledigt");
        dismiss_button.add_css_class("destructive-action");
        controls.append(&open_button);
        controls.append(&snooze_10_button);
        controls.append(&snooze_hour_button);
        controls.append(&dismiss_button);
        action_row.add_suffix(&controls);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&action_row));
        let busy = Rc::new(Cell::new(false));

        let row_for_open = row.clone();
        let busy_for_open = Rc::clone(&busy);
        let on_open = Rc::clone(&self.on_open);
        let delivery_for_open = delivery.clone();
        open_button.connect_clicked(move |_| {
            if busy_for_open.replace(true) {
                return;
            }
            row_for_open.set_sensitive(false);
            let accepted = on_open(delivery_for_open.clone());
            if !accepted {
                busy_for_open.set(false);
                row_for_open.set_sensitive(true);
            }
        });

        for (button, action) in [
            (snooze_10_button, ReminderDialogAction::Snooze(10 * 60)),
            (snooze_hour_button, ReminderDialogAction::Snooze(60 * 60)),
            (dismiss_button, ReminderDialogAction::Dismiss),
        ] {
            let row_for_action = row.clone();
            let busy_for_action = Rc::clone(&busy);
            let on_action = Rc::clone(&self.on_action);
            let delivery_for_action = delivery.clone();
            button.connect_clicked(move |_| {
                if busy_for_action.replace(true) {
                    return;
                }
                row_for_action.set_sensitive(false);
                if !on_action(delivery_for_action.clone(), action) {
                    busy_for_action.set(false);
                    row_for_action.set_sensitive(true);
                }
            });
        }

        (row, busy)
    }
}
