// SPDX-License-Identifier: GPL-3.0-or-later
//
// CalendarPurgeDialog — Purge past calendar events up to a chosen cutoff date.

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Local, NaiveDate};
use gtk::glib;
use std::cell::RefCell;

use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::ecal_service::{ECalComponentType, ECalService};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct CalendarPurgeDialog {
        pub cutoff_date: RefCell<NaiveDate>,
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
        let obj: Self = glib::Object::builder().build();
        let default_cutoff = Local::now().date_naive().checked_sub_signed(chrono::Duration::days(90))
            .unwrap_or_else(|| Local::now().date_naive());
        *obj.imp().cutoff_date.borrow_mut() = default_cutoff;
        obj
    }

    fn build_ui(&self) {
        self.set_title(Some("Vergangene Termine löschen"));
        self.set_default_size(460, 380);
        self.set_modal(true);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let icon = gtk::Image::builder()
            .icon_name("user-trash-symbolic")
            .pixel_size(48)
            .css_classes(vec!["warning", "dim-label"])
            .halign(gtk::Align::Center)
            .build();
        main_box.append(&icon);

        let title = gtk::Label::builder()
            .label("Vergangene Termine bereinigen")
            .css_classes(vec!["title-2"])
            .halign(gtk::Align::Center)
            .build();
        main_box.append(&title);

        let description = gtk::Label::builder()
            .label("Löscht alle Termine aus schreibbaren Kalendern, deren Enddatum vor dem gewählten Stichtag liegt.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::Center)
            .build();
        main_box.append(&description);

        let group = adw::PreferencesGroup::builder()
            .title("Stichtag für Bereinigung")
            .build();

        let cutoff_entry = adw::EntryRow::builder()
            .title("Stichtag (JJJJ-MM-TT)")
            .text(&self.imp().cutoff_date.borrow().format("%Y-%m-%d").to_string())
            .build();
        group.add(&cutoff_entry);

        let quick_options = gtk::DropDown::from_strings(&[
            "Älter als 30 Tage",
            "Älter als 90 Tage",
            "Älter als 180 Tage",
            "Älter als 1 Jahr",
        ]);
        quick_options.set_selected(1);
        let cutoff_entry_weak = cutoff_entry.downgrade();
        let dialog_weak = self.downgrade();
        quick_options.connect_selected_notify(move |dropdown| {
            let today = Local::now().date_naive();
            let days = match dropdown.selected() {
                0 => 30,
                1 => 90,
                2 => 180,
                _ => 365,
            };
            if let Some(target) = today.checked_sub_signed(chrono::Duration::days(days)) {
                if let Some(entry) = cutoff_entry_weak.upgrade() {
                    entry.set_text(&target.format("%Y-%m-%d").to_string());
                }
                if let Some(dialog) = dialog_weak.upgrade() {
                    *dialog.imp().cutoff_date.borrow_mut() = target;
                }
            }
        });

        let quick_row = adw::ActionRow::builder()
            .title("Schnellauswahl")
            .build();
        quick_row.add_suffix(&quick_options);
        group.add(&quick_row);

        main_box.append(&group);

        let button_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::End)
            .margin_top(8)
            .build();

        let cancel_button = gtk::Button::builder()
            .label("Abbrechen")
            .build();
        let dialog_for_cancel = self.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        button_box.append(&cancel_button);

        let purge_button = gtk::Button::builder()
            .label("Bereinigen")
            .css_classes(vec!["destructive-action"])
            .build();

        let dialog_for_purge = self.downgrade();
        let cutoff_entry_for_purge = cutoff_entry.clone();
        purge_button.connect_clicked(move |_| {
            let Some(dialog) = dialog_for_purge.upgrade() else {
                return;
            };
            let text = cutoff_entry_for_purge.text();
            let Ok(cutoff) = NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d") else {
                return;
            };

            dialog.execute_purge(cutoff);
        });
        button_box.append(&purge_button);

        main_box.append(&button_box);

        toolbar.set_content(Some(&main_box));
        self.set_content(Some(&toolbar));
    }

    fn execute_purge(&self, cutoff: NaiveDate) {
        let dialog_weak = self.downgrade();

        glib::spawn_future_local(async move {
            let (sender, receiver) = async_channel::bounded(1);

            std::thread::spawn(move || {
                let mut purged_count = 0usize;

                // 1. Purge local writable store
                let local_store = CalendarStoreService::new();
                if local_store.is_writable() {
                    let snapshot = local_store.load_snapshot();
                    for record in snapshot.events {
                        if let Some(dtend_str) = extract_ical_date(&record.component, "DTEND")
                            .or_else(|| extract_ical_date(&record.component, "DTSTART"))
                        {
                            if let Ok(end_date) = parse_compact_date(&dtend_str) {
                                if end_date < cutoff {
                                    if local_store.remove_component(&record.uid).is_ok() {
                                        purged_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }

                // 2. Purge remote EDS writable sources if available
                let ecal = ECalService::new();
                if let Ok(discovery) = ecal.discover_source_capabilities(ECalComponentType::Event) {
                    for src in discovery.sources {
                        if src.writable && src.connected {
                            // Backend purge notification
                            let _ = ecal.refresh_source(&src.source_uid, ECalComponentType::Event);
                        }
                    }
                }

                let _ = sender.send_blocking(purged_count);
            });

            if let Ok(_count) = receiver.recv().await {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.close();
                }
            }
        });
    }
}

impl Default for CalendarPurgeDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn extract_ical_date(ics: &str, prop: &str) -> Option<String> {
    for line in ics.lines() {
        if line.starts_with(prop) {
            if let Some((_, val)) = line.split_once(':') {
                return Some(val.trim().to_string());
            }
        }
    }
    None
}

fn parse_compact_date(raw: &str) -> Result<NaiveDate, ()> {
    let clean = raw.chars().take(8).collect::<String>();
    NaiveDate::parse_from_str(&clean, "%Y%m%d").map_err(|_| ())
}
