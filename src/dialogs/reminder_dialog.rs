// SPDX-License-Identifier: GPL-3.0-or-later
//
// Reusable GTK4 reminder surface.  The dialog owns only GTK state; all
// snapshot, claim and persistence work is supplied by the application through
// callbacks and therefore remains outside the main loop.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::i18n::gettext;
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
    dismiss_all_button: gtk::Button,
    snooze_all_button: gtk::MenuButton,
    entries: Rc<RefCell<BTreeMap<String, ReminderEntry>>>,
    on_open: Rc<dyn Fn(ReminderDelivery) -> bool>,
    on_action: Rc<dyn Fn(ReminderDelivery, ReminderDialogAction) -> bool>,
}

fn create_snooze_popover<F>(on_select_duration: F) -> gtk::Popover
where
    F: Fn(i64) + 'static,
{
    let popover = gtk::Popover::new();
    let box_widget = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(4)
        .margin_end(4)
        .margin_top(4)
        .margin_bottom(4)
        .build();

    let on_select = Rc::new(on_select_duration);
    for (label, seconds) in [
        (gettext("5 minutes"), 5 * 60),
        (gettext("10 minutes"), 10 * 60),
        (gettext("15 minutes"), 15 * 60),
        (gettext("30 minutes"), 30 * 60),
        (gettext("1 hour"), 60 * 60),
        (gettext("1 day"), 24 * 60 * 60),
    ] {
        let btn = gtk::Button::builder()
            .label(&label)
            .css_classes(vec!["flat"])
            .build();
        let pop_weak = popover.downgrade();
        let on_select = Rc::clone(&on_select);
        btn.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            on_select(seconds);
        });
        box_widget.append(&btn);
    }
    popover.set_child(Some(&box_widget));
    popover
}

impl ReminderDialog {
    pub fn new<P, Open, Action>(parent: Option<&P>, on_open: Open, on_action: Action) -> Self
    where
        P: IsA<gtk::Window>,
        Open: Fn(ReminderDelivery) -> bool + 'static,
        Action: Fn(ReminderDelivery, ReminderDialogAction) -> bool + 'static,
    {
        let window = adw::Window::builder()
            .title(&gettext("Reminders"))
            .default_width(720)
            .default_height(420)
            .build();
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
        }

        let header_bar = adw::HeaderBar::new();
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);

        let entries: Rc<RefCell<BTreeMap<String, ReminderEntry>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let on_open = Rc::new(on_open);
        let on_action = Rc::new(on_action);

        let dismiss_all_button = gtk::Button::builder()
            .label(&gettext("Dismiss All"))
            .css_classes(vec!["destructive-action"])
            .sensitive(false)
            .build();

        let snooze_all_button = gtk::MenuButton::builder()
            .label(&gettext("Snooze All"))
            .sensitive(false)
            .build();
        let snooze_all_popover = create_snooze_popover({
            let entries = Rc::clone(&entries);
            let on_action = Rc::clone(&on_action);
            move |seconds| {
                let to_snooze: Vec<(Rc<Cell<bool>>, gtk::ListBoxRow, ReminderDelivery)> = entries
                    .borrow()
                    .values()
                    .filter(|e| !e.busy.get())
                    .map(|e| {
                        e.busy.set(true);
                        e.row.set_sensitive(false);
                        (Rc::clone(&e.busy), e.row.clone(), e.delivery.clone())
                    })
                    .collect();
                for (busy, row, delivery) in to_snooze {
                    if !on_action(delivery, ReminderDialogAction::Snooze(seconds)) {
                        busy.set(false);
                        row.set_sensitive(true);
                    }
                }
            }
        });
        snooze_all_button.set_popover(Some(&snooze_all_popover));

        {
            let entries = Rc::clone(&entries);
            let on_action = Rc::clone(&on_action);
            dismiss_all_button.connect_clicked(move |_| {
                let to_dismiss: Vec<(Rc<Cell<bool>>, gtk::ListBoxRow, ReminderDelivery)> = entries
                    .borrow()
                    .values()
                    .filter(|e| !e.busy.get())
                    .map(|e| {
                        e.busy.set(true);
                        e.row.set_sensitive(false);
                        (Rc::clone(&e.busy), e.row.clone(), e.delivery.clone())
                    })
                    .collect();
                for (busy, row, delivery) in to_dismiss {
                    if !on_action(delivery, ReminderDialogAction::Dismiss) {
                        busy.set(false);
                        row.set_sensitive(true);
                    }
                }
            });
        }

        header_bar.pack_start(&dismiss_all_button);
        header_bar.pack_end(&snooze_all_button);

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
            .label(&gettext("No pending reminders"))
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
            dismiss_all_button,
            snooze_all_button,
            entries,
            on_open,
            on_action,
        }
    }

    pub fn update_deliveries(&self, deliveries: &[ReminderDelivery], present_if_hidden: bool) {
        self.replace_deliveries(deliveries);
        if self.has_entries() && (present_if_hidden || self.window.is_visible()) {
            self.window.present();
        }
    }

    pub fn present_deliveries(&self, deliveries: &[ReminderDelivery]) {
        self.update_deliveries(deliveries, true);
    }

    pub fn present_existing(&self) {
        if self.has_entries() {
            self.window.present();
        }
    }

    pub fn has_entries(&self) -> bool {
        !self.entries.borrow().is_empty()
    }

    fn update_action_sensitivity(&self) {
        let has = self.has_entries();
        self.empty_state.set_visible(!has);
        self.dismiss_all_button.set_sensitive(has);
        self.snooze_all_button.set_sensitive(has);
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
            self.update_action_sensitivity();
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
        let fallback = gettext("Could not save reminder action");
        self.error_label.set_label(error.unwrap_or(&fallback));
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
            let fallback = gettext("Could not open component");
            self.error_label.set_label(error.unwrap_or(&fallback));
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
                self.show_error(&gettext("Reminder entry with invalid identity was discarded"));
                continue;
            };
            let key = delivery.candidate.key.clone();
            let Ok(action_target) = delivery.action_target() else {
                self.show_error(&gettext("Reminder entry with invalid action target was discarded"));
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
        self.update_action_sensitivity();
    }

    fn build_row(&self, delivery: ReminderDelivery) -> (gtk::ListBoxRow, Rc<Cell<bool>>) {
        let kind_label = delivery
            .identity()
            .map(|identity| match identity.kind {
                crate::services::reminder_service::ReminderComponentKind::Event => gettext("Event"),
                crate::services::reminder_service::ReminderComponentKind::Task => gettext("Task"),
                crate::services::reminder_service::ReminderComponentKind::Memo => gettext("Memo"),
                crate::services::reminder_service::ReminderComponentKind::Contact => gettext("Contact"),
            })
            .unwrap_or_else(|_| gettext("Reminder"));
        let due_label = chrono::DateTime::from_timestamp(delivery.candidate.due_at, 0)
            .map(|timestamp| {
                timestamp
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| gettext("Due"));
        let action_row = adw::ActionRow::builder()
            .title(&delivery.candidate.title)
            .subtitle(&format!("{kind_label} · {due_label}"))
            .build();
        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .valign(gtk::Align::Center)
            .build();
        let open_button = gtk::Button::with_label(&gettext("Open"));
        let snooze_10_button = gtk::Button::with_label(&gettext("10 min"));
        let snooze_menu_button = gtk::MenuButton::builder()
            .label(&gettext("Snooze…"))
            .build();
        let dismiss_button = gtk::Button::with_label(&gettext("Dismiss"));
        dismiss_button.add_css_class("destructive-action");
        controls.append(&open_button);
        controls.append(&snooze_10_button);
        controls.append(&snooze_menu_button);
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

        // Quick 10m snooze
        {
            let row_for_action = row.clone();
            let busy_for_action = Rc::clone(&busy);
            let on_action = Rc::clone(&self.on_action);
            let delivery_for_action = delivery.clone();
            snooze_10_button.connect_clicked(move |_| {
                if busy_for_action.replace(true) {
                    return;
                }
                row_for_action.set_sensitive(false);
                if !on_action(delivery_for_action.clone(), ReminderDialogAction::Snooze(10 * 60)) {
                    busy_for_action.set(false);
                    row_for_action.set_sensitive(true);
                }
            });
        }

        // Comprehensive snooze durations popover
        {
            let row_for_action = row.clone();
            let busy_for_action = Rc::clone(&busy);
            let on_action = Rc::clone(&self.on_action);
            let delivery_for_action = delivery.clone();
            let popover = create_snooze_popover(move |seconds| {
                if busy_for_action.replace(true) {
                    return;
                }
                row_for_action.set_sensitive(false);
                if !on_action(delivery_for_action.clone(), ReminderDialogAction::Snooze(seconds)) {
                    busy_for_action.set(false);
                    row_for_action.set_sensitive(true);
                }
            });
            snooze_menu_button.set_popover(Some(&popover));
        }

        // Dismiss
        {
            let row_for_action = row.clone();
            let busy_for_action = Rc::clone(&busy);
            let on_action = Rc::clone(&self.on_action);
            let delivery_for_action = delivery.clone();
            dismiss_button.connect_clicked(move |_| {
                if busy_for_action.replace(true) {
                    return;
                }
                row_for_action.set_sensitive(false);
                if !on_action(delivery_for_action.clone(), ReminderDialogAction::Dismiss) {
                    busy_for_action.set(false);
                    row_for_action.set_sensitive(true);
                }
            });
        }

        (row, busy)
    }
}
