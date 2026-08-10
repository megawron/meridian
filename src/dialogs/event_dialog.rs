// SPDX-License-Identifier: GPL-3.0-or-later
//
// EventDialog — Dialog for creating and editing calendar events.
//
// Features:
// - Summary / Title entry
// - Location & Description
// - Calendar Source dropdown (Persönlich, Arbeit, etc.)
// - All-day toggle
// - Start and End Date/Time selection

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, Utc};
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::eds_service;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct EventDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for EventDialog {
        const NAME: &'static str = "EvolutionEventDialog";
        type Type = super::EventDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for EventDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for EventDialog {}
    impl WindowImpl for EventDialog {}
    impl AdwWindowImpl for EventDialog {}
}

glib::wrapper! {
    pub struct EventDialog(ObjectSubclass<imp::EventDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl EventDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Neuer Termin")
            .property("modal", true)
            .property("default-width", 560)
            .property("default-height", 520)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let btn_save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let content_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        // Title entry
        let entry_title = gtk::Entry::builder()
            .placeholder_text("Titel des Termins…")
            .css_classes(vec!["title-2"])
            .build();
        content_box.append(&entry_title);

        // Group 1: General Details
        let group_details = adw::PreferencesGroup::builder().title("Details").build();

        let calendar_model = gtk::StringList::new(&["EDS-Kalender werden geladen…"]);
        let calendar_source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let row_calendar = adw::ComboRow::builder()
            .title("Kalender")
            .model(&calendar_model)
            .build();
        group_details.add(&row_calendar);

        let row_location = adw::EntryRow::builder().title("Ort").build();
        group_details.add(&row_location);

        let row_allday = adw::SwitchRow::builder()
            .title("Ganztägig")
            .active(false)
            .build();
        group_details.add(&row_allday);

        content_box.append(&group_details);

        // Group 2: Time Selection
        let group_time = adw::PreferencesGroup::builder().title("Zeitraum").build();

        let default_start = Local::now();
        let default_end = default_start + Duration::hours(1);
        let row_start = adw::EntryRow::builder()
            .title("Beginn (YYYY-MM-DD HH:MM)")
            .text(&default_start.format("%Y-%m-%d %H:%M").to_string())
            .build();
        group_time.add(&row_start);

        let row_end = adw::EntryRow::builder()
            .title("Ende (YYYY-MM-DD HH:MM)")
            .text(&default_end.format("%Y-%m-%d %H:%M").to_string())
            .build();
        group_time.add(&row_end);

        content_box.append(&group_time);

        // Description
        let group_desc = adw::PreferencesGroup::builder()
            .title("Beschreibung & Notizen")
            .build();

        let text_desc = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .height_request(80)
            .build();
        group_desc.add(&text_desc);
        content_box.append(&group_desc);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));

        let calendar_model_for_load = calendar_model.clone();
        let calendar_source_uids_for_load = Rc::clone(&calendar_source_uids);
        glib::spawn_future_local(async move {
            match eds_service::list_calendar_sources().await {
                Ok(sources) => {
                    let active_sources: Vec<_> = sources
                        .into_iter()
                        .filter(|source| source.enabled)
                        .collect();
                    let names: Vec<String> = active_sources
                        .iter()
                        .map(|source| {
                            if source.backend_name.is_empty() {
                                source.display_name.clone()
                            } else {
                                format!("{} · {}", source.display_name, source.backend_name)
                            }
                        })
                        .collect();
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    calendar_model_for_load.splice(
                        0,
                        calendar_model_for_load.n_items(),
                        &name_refs,
                    );
                    let mut uids = calendar_source_uids_for_load.borrow_mut();
                    uids.clear();
                    uids.extend(active_sources.into_iter().map(|source| source.uid));
                    if uids.is_empty() {
                        calendar_model_for_load.splice(
                            0,
                            calendar_model_for_load.n_items(),
                            &["Kein aktiver EDS-Kalender verfügbar"],
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load EDS calendars for event dialog");
                    calendar_model_for_load.splice(
                        0,
                        calendar_model_for_load.n_items(),
                        &["EDS-Kalender sind nicht erreichbar"],
                    );
                }
            }
        });

        let calendar_source_uids_for_save = Rc::clone(&calendar_source_uids);
        let dialog_weak = self.downgrade();
        btn_save.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let selected = row_calendar.selected() as usize;
            let Some(source_uid) = calendar_source_uids_for_save
                .borrow()
                .get(selected)
                .cloned()
            else {
                dialog.show_toast("Kein aktiver EDS-Kalender ist ausgewählt");
                return;
            };
            let component = match build_event_component(
                &entry_title.text(),
                &row_location.text(),
                row_allday.is_active(),
                &row_start.text(),
                &row_end.text(),
                &text_desc.buffer().text(
                    &text_desc.buffer().start_iter(),
                    &text_desc.buffer().end_iter(),
                    false,
                ),
            ) {
                Ok(component) => component,
                Err(error) => {
                    dialog.show_toast(&error);
                    return;
                }
            };

            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let result = ECalService::new().create_component(
                    &source_uid,
                    ECalComponentType::Event,
                    &component,
                );
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Event mutation result was dropped");
                }
            });

            let dialog_weak_for_result = dialog.downgrade();
            glib::spawn_future_local(async move {
                let Ok(result) = receiver.recv().await else {
                    return;
                };
                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                    return;
                };
                match result {
                    Ok(result) => {
                        dialog.show_toast(&format!("Termin gespeichert ({})", result.uid));
                        dialog.close();
                    }
                    Err(error) => dialog
                        .show_toast(&format!("Termin konnte nicht gespeichert werden: {error}")),
                }
            });
        });
    }

    fn show_toast(&self, message: &str) {
        let Some(parent) = self.transient_for() else {
            tracing::warn!(%message, "ECal dialog result");
            return;
        };
        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
        } else {
            tracing::warn!(%message, "ECal dialog result");
        }
    }
}

impl Default for EventDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn build_event_component(
    title: &str,
    location: &str,
    all_day: bool,
    start: &str,
    end: &str,
    description: &str,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Ein Termin benötigt einen Titel".to_string());
    }
    let start_value = parse_event_datetime(start, all_day, "Beginn")?;
    let end_value = parse_event_datetime(end, all_day, "Ende")?;
    if !all_day && end_value <= start_value {
        return Err("Das Ende muss nach dem Beginn liegen".to_string());
    }

    let uid = format!("evolution-gtk4-{}", uuid::Uuid::new_v4());
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let (start_line, end_line) = if all_day {
        (
            format!("DTSTART;VALUE=DATE:{start_value}"),
            format!("DTEND;VALUE=DATE:{end_value}"),
        )
    } else {
        (
            format!("DTSTART:{start_value}"),
            format!("DTEND:{end_value}"),
        )
    };

    Ok(format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{stamp}\r\nSUMMARY:{}\r\n{}\
         \r\n{}\r\nLOCATION:{}\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\n",
        escape_ical_text(title),
        start_line,
        end_line,
        escape_ical_text(location.trim()),
        escape_ical_text(description.trim()),
    ))
}

fn parse_event_datetime(value: &str, all_day: bool, field_name: &str) -> Result<String, String> {
    let value = value.trim();
    if all_day {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(|date| date.format("%Y%m%d").to_string())
            .map_err(|_| format!("{field_name}: erwartet YYYY-MM-DD"));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M")
        .map(|date_time| date_time.format("%Y%m%dT%H%M%S").to_string())
        .map_err(|_| format!("{field_name}: erwartet YYYY-MM-DD HH:MM"))
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_component_serialization_is_ical_safe() {
        let component = build_event_component(
            "Team, Sync",
            "Room; 42",
            false,
            "2026-08-10 12:00",
            "2026-08-10 13:00",
            "Line one\nLine two",
        )
        .expect("event should serialize");

        assert!(component.contains("BEGIN:VEVENT"));
        assert!(component.contains("SUMMARY:Team\\, Sync"));
        assert!(component.contains("LOCATION:Room\\; 42"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two"));
        assert!(component.contains("DTSTART:20260810T120000"));
        assert!(component.contains("DTEND:20260810T130000"));
        assert!(component.ends_with("END:VEVENT\r\n"));
    }

    #[test]
    fn event_component_rejects_invalid_or_reversed_times() {
        assert!(
            build_event_component("Title", "", false, "not-a-date", "2026-08-10 13:00", "",)
                .is_err()
        );
        assert!(
            build_event_component(
                "Title",
                "",
                false,
                "2026-08-10 13:00",
                "2026-08-10 12:00",
                "",
            )
            .is_err()
        );
    }
}
