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

use crate::i18n::gettext;
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
            0 => "No old local events deleted".to_string(),
            1 => "1 old local event deleted".to_string(),
            count => format!("{count} old local events deleted"),
        }];
        if self.confirmed.skipped_recurring_count > 0 {
            parts.push(format!(
                "{} recurring series/instances or ambiguous UIDs were kept for safety",
                self.confirmed.skipped_recurring_count
            ));
        }
        if self.confirmed.skipped_invalid_count > 0 {
            parts.push(format!(
                "{} events without a safely evaluable end time were kept",
                self.confirmed.skipped_invalid_count
            ));
        }
        if self.cancelled {
            parts.push("Purge cancelled".to_string());
        } else if let Some(error) = self.error.as_deref() {
            parts.push(format!("Error: {error}"));
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
        const NAME: &'static str = "MeridianCalendarPurgeDialog";
        type Type = super::CalendarPurgeDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for CalendarPurgeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for CalendarPurgeDialog {}
    impl AdwDialogImpl for CalendarPurgeDialog {}
}

glib::wrapper! {
    pub struct CalendarPurgeDialog(ObjectSubclass<imp::CalendarPurgeDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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
        self.set_title(&gettext("Purge Past Events"));
        self.set_content_width(480);
        self.set_content_height(340);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);

        let cancel = gtk::Button::with_label(&gettext("Cancel"));
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |button| {
                if dialog.imp().running.get() {
                    if let Some(cancellation) = dialog.imp().cancellation.borrow().as_ref() {
                        cancellation.store(true, Ordering::Relaxed);
                    }
                    button.set_sensitive(false);
                    dialog.set_status(&gettext("Canceling purge…"), false);
                } else {
                    dialog.close();
                }
            }
        ));
        header.pack_start(&cancel);
        self.imp().cancel_button.replace(Some(cancel));

        let days_row = adw::SpinRow::with_range(0.0, 1000.0, 1.0);
        days_row.set_title(&gettext("Delete events older than"));
        days_row.set_subtitle(&gettext("Days"));
        days_row.set_value(60.0);
        let days_icon = gtk::Image::builder()
            .icon_name("preferences-system-time-symbolic")
            .valign(gtk::Align::Center)
            .build();
        days_row.add_prefix(&days_icon);

        let purge = gtk::Button::builder()
            .label(gettext("Permanently Delete"))
            .css_classes(["destructive-action"])
            .build();
        purge.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            days_row,
            move |_| {
                let Some(cutoff) = purge_cutoff(Local::now().timestamp(), days_row.value() as i32)
                else {
                    dialog.set_status(&gettext("The selected time range is invalid"), true);
                    return;
                };
                dialog.execute_purge(cutoff);
            }
        ));
        header.pack_end(&purge);
        self.imp().purge_button.replace(Some(purge));
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Purge Past Events"))
            .description(gettext("This action permanently deletes old single events from the app-owned local calendar. Synchronized calendars and recurring event series are not modified in this portable path."))
            .build();
        group.add(&days_row);

        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .margin_top(8)
            .css_classes(["dim-label"])
            .build();
        group.add(&status);
        self.imp().status.replace(Some(status));
        page.add(&group);

        self.connect_close_attempt(|dialog| {
            if !dialog.imp().running.get() {
                dialog.set_can_close(true);
                dialog.close();
                return;
            }
            if let Some(cancellation) = dialog.imp().cancellation.borrow().as_ref() {
                cancellation.store(true, Ordering::Relaxed);
            }
            dialog.set_status(&gettext("Canceling purge…"), false);
        });

        toolbar.set_content(Some(&page));
        self.set_child(Some(&toolbar));
    }

    fn execute_purge(&self, cutoff_timestamp: i64) {
        if self.imp().running.replace(true) {
            return;
        }
        self.set_can_close(false);
        if let Some(button) = self.imp().purge_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = self.imp().cancel_button.borrow().as_ref() {
            button.set_sensitive(true);
            button.set_label(&gettext("Cancel"));
        }
        self.set_status(&gettext("Checking local events…"), false);

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
            dialog.set_can_close(true);
            if let Some(button) = dialog.imp().purge_button.borrow().as_ref() {
                button.set_sensitive(true);
            }
            if let Some(button) = dialog.imp().cancel_button.borrow().as_ref() {
                button.set_sensitive(true);
                button.set_label(&gettext("Close"));
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
        assert!(message.contains("1 old local event deleted"));
        assert!(message.contains(
            "2 recurring series/instances or ambiguous UIDs were kept for safety"
        ));
        assert!(message.contains("1 events without a safely evaluable end time"));
        assert!(outcome.changed());
    }
}
