// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPurgeDialog — fail-closed portable purge for the app-owned calendar.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::Local;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::calendar_store_service::{CalendarPurgeResult, CalendarStoreService};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CalendarPurgeOutcome {
    pub(crate) confirmed: CalendarPurgeResult,
    pub(crate) error: Option<String>,
    pub(crate) cancelled: bool,
}

impl CalendarPurgeOutcome {
    pub(crate) fn changed(&self) -> bool {
        self.confirmed.removed_count > 0
    }

    pub(crate) fn message(&self) -> String {
        let mut parts = vec![match self.confirmed.removed_count {
            0 => "Keine alten lokalen Termine gelöscht".to_string(),
            1 => "Ein alter lokaler Termin wurde gelöscht".to_string(),
            count => format!("{count} alte lokale Termine wurden gelöscht"),
        }];
        if self.confirmed.skipped_recurring_count > 0 {
            parts.push(format!(
                "{} Serien/Instanzen oder mehrdeutige UIDs wurden sicherheitshalber beibehalten",
                self.confirmed.skipped_recurring_count
            ));
        }
        if self.confirmed.skipped_invalid_count > 0 {
            parts.push(format!(
                "{} Termine ohne sicher auswertbares Ende wurden beibehalten",
                self.confirmed.skipped_invalid_count
            ));
        }
        if self.cancelled {
            parts.push("Bereinigung wurde abgebrochen".to_string());
        } else if let Some(error) = self.error.as_deref() {
            parts.push(format!("Fehler: {error}"));
        }
        parts.join(". ")
    }
}

type FinishedHandler = Box<dyn Fn(CalendarPurgeOutcome) + 'static>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CalendarPurgeDialog {
        pub(super) running: Cell<bool>,
        pub(super) cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub(super) purge_button: RefCell<Option<gtk::Button>>,
        pub(super) cancel_button: RefCell<Option<gtk::Button>>,
        pub(super) status: RefCell<Option<gtk::Label>>,
        pub(super) on_finished: RefCell<Option<FinishedHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarPurgeDialog {
        const NAME: &'static str = "EvolutionCalendarPurgeDialog";
        type Type = super::CalendarPurgeDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for CalendarPurgeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for CalendarPurgeDialog {}
    impl WindowImpl for CalendarPurgeDialog {}
    impl AdwWindowImpl for CalendarPurgeDialog {}
}

glib::wrapper! {
    pub struct CalendarPurgeDialog(ObjectSubclass<imp::CalendarPurgeDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl CalendarPurgeDialog {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub(crate) fn with_finished_handler<F>(on_finished: F) -> Self
    where
        F: Fn(CalendarPurgeOutcome) + 'static,
    {
        let dialog = Self::new();
        dialog
            .imp()
            .on_finished
            .replace(Some(Box::new(on_finished)));
        dialog
    }

    fn build_ui(&self) {
        self.set_title(Some("Vergangene Termine löschen"));
        self.set_default_size(460, 360);
        self.set_modal(true);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());

        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        main_box.append(
            &gtk::Image::builder()
                .icon_name("user-trash-symbolic")
                .pixel_size(48)
                .css_classes(vec!["warning", "dim-label"])
                .halign(gtk::Align::Center)
                .build(),
        );
        main_box.append(
            &gtk::Label::builder()
                .label("Vergangene Termine bereinigen")
                .css_classes(vec!["title-2"])
                .halign(gtk::Align::Center)
                .build(),
        );
        main_box.append(
            &gtk::Label::builder()
                .label("Diese Aktion löscht endgültig alte Einzeltermine aus dem app-eigenen lokalen Kalender. Synchronisierte Kalender und Terminserien werden in diesem portablen Pfad nicht verändert.")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(vec!["dim-label"])
                .halign(gtk::Align::Center)
                .build(),
        );

        let days = gtk::SpinButton::with_range(0.0, 1000.0, 1.0);
        days.set_value(60.0);
        let days_row = adw::ActionRow::builder()
            .title("Termine löschen, die älter sind als")
            .subtitle("Tage")
            .build();
        days_row.add_suffix(&days);
        let group = adw::PreferencesGroup::new();
        group.add(&days_row);
        main_box.append(&group);

        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(vec!["dim-label"])
            .build();
        main_box.append(&status);
        self.imp().status.replace(Some(status));

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::End)
            .margin_top(8)
            .build();
        let cancel = gtk::Button::with_label("Abbrechen");
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |button| {
                if dialog.imp().running.get() {
                    if let Some(cancellation) = dialog.imp().cancellation.borrow().as_ref() {
                        cancellation.store(true, Ordering::Relaxed);
                    }
                    button.set_sensitive(false);
                    dialog.set_status("Bereinigung wird abgebrochen …", false);
                } else {
                    dialog.close();
                }
            }
        ));
        buttons.append(&cancel);
        self.imp().cancel_button.replace(Some(cancel));

        let purge = gtk::Button::builder()
            .label("Endgültig löschen")
            .css_classes(vec!["destructive-action"])
            .build();
        purge.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            days,
            move |_| {
                let Some(cutoff) = purge_cutoff(Local::now().timestamp(), days.value_as_int())
                else {
                    dialog.set_status("Der gewählte Zeitraum ist ungültig", true);
                    return;
                };
                dialog.execute_purge(cutoff);
            }
        ));
        buttons.append(&purge);
        self.imp().purge_button.replace(Some(purge));
        main_box.append(&buttons);

        self.connect_close_request(|dialog| {
            if !dialog.imp().running.get() {
                return glib::Propagation::Proceed;
            }
            if let Some(cancellation) = dialog.imp().cancellation.borrow().as_ref() {
                cancellation.store(true, Ordering::Relaxed);
            }
            dialog.set_status("Bereinigung wird abgebrochen …", false);
            glib::Propagation::Stop
        });

        toolbar.set_content(Some(&main_box));
        self.set_content(Some(&toolbar));
    }

    fn execute_purge(&self, cutoff_timestamp: i64) {
        if self.imp().running.replace(true) {
            return;
        }
        self.set_deletable(false);
        if let Some(button) = self.imp().purge_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = self.imp().cancel_button.borrow().as_ref() {
            button.set_sensitive(true);
            button.set_label("Abbrechen");
        }
        self.set_status("Lokale Termine werden geprüft …", false);

        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .cancellation
            .replace(Some(Arc::clone(&cancellation)));
        let dialog = self.downgrade();
        glib::spawn_future_local(async move {
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let outcome = match CalendarStoreService::new()
                    .purge_events_before(cutoff_timestamp, &cancellation)
                {
                    Ok(confirmed) => CalendarPurgeOutcome {
                        confirmed,
                        error: None,
                        cancelled: false,
                    },
                    Err(failure) => CalendarPurgeOutcome {
                        confirmed: failure.confirmed,
                        cancelled: failure.error.kind() == std::io::ErrorKind::Interrupted,
                        error: (failure.error.kind() != std::io::ErrorKind::Interrupted)
                            .then(|| failure.error.to_string()),
                    },
                };
                let _ = sender.send_blocking(outcome);
            });

            let Ok(outcome) = receiver.recv().await else {
                return;
            };
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            dialog.imp().running.set(false);
            dialog.imp().cancellation.replace(None);
            dialog.set_deletable(true);
            if let Some(button) = dialog.imp().purge_button.borrow().as_ref() {
                button.set_sensitive(true);
            }
            if let Some(button) = dialog.imp().cancel_button.borrow().as_ref() {
                button.set_sensitive(true);
                button.set_label("Schließen");
            }
            let message = outcome.message();
            dialog.set_status(&message, outcome.error.is_some());
            if let Some(on_finished) = dialog.imp().on_finished.borrow().as_ref() {
                on_finished(outcome);
                dialog.close();
            }
        });
    }

    fn set_status(&self, message: &str, error: bool) {
        if let Some(status) = self.imp().status.borrow().as_ref() {
            status.set_text(message);
            if error {
                status.remove_css_class("dim-label");
                status.add_css_class("error");
            } else {
                status.remove_css_class("error");
                status.add_css_class("dim-label");
            }
        }
    }
}

fn purge_cutoff(now_timestamp: i64, days: i32) -> Option<i64> {
    if !(0..=1000).contains(&days) {
        return None;
    }
    let seconds = i64::from(days).checked_mul(24 * 60 * 60)?;
    now_timestamp.checked_sub(seconds)
}

impl Default for CalendarPurgeDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_cutoff_matches_upstream_absolute_day_range() {
        let now = 1_800_000_000_i64;
        assert_eq!(purge_cutoff(now, 60), Some(now - 60 * 24 * 60 * 60));
        assert_eq!(purge_cutoff(now, 0), Some(now));
        assert!(purge_cutoff(now, 1000).is_some());
        assert!(purge_cutoff(now, -1).is_none());
        assert!(purge_cutoff(now, 1001).is_none());
    }

    #[test]
    fn outcome_message_never_claims_skipped_series_were_deleted() {
        let outcome = CalendarPurgeOutcome {
            confirmed: CalendarPurgeResult {
                removed_count: 1,
                skipped_recurring_count: 2,
                skipped_invalid_count: 1,
            },
            error: None,
            cancelled: false,
        };
        let message = outcome.message();
        assert!(message.contains("Ein alter lokaler Termin wurde gelöscht"));
        assert!(message.contains(
            "2 Serien/Instanzen oder mehrdeutige UIDs wurden sicherheitshalber beibehalten"
        ));
        assert!(message.contains("1 Termine ohne sicher auswertbares Ende"));
        assert!(outcome.changed());
    }
}
