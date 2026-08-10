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

use crate::services::calendar_store_service::{CalendarEventRecord, update_ical_property};
use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::eds_service;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct EventDialog {
        pub edit_component: RefCell<Option<String>>,
        pub edit_uid: RefCell<Option<String>>,
        pub edit_source_uid: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EventDialog {
        const NAME: &'static str = "EvolutionEventDialog";
        type Type = super::EventDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for EventDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("edit-component").build(),
                    glib::ParamSpecString::builder("edit-uid").build(),
                    glib::ParamSpecString::builder("edit-source-uid").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "edit-component" => {
                    *self.edit_component.borrow_mut() = value.get().expect("string property")
                }
                "edit-uid" => *self.edit_uid.borrow_mut() = value.get().expect("string property"),
                "edit-source-uid" => {
                    *self.edit_source_uid.borrow_mut() = value.get().expect("string property")
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-component" => self.edit_component.borrow().to_value(),
                "edit-uid" => self.edit_uid.borrow().to_value(),
                "edit-source-uid" => self.edit_source_uid.borrow().to_value(),
                _ => unimplemented!(),
            }
        }

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

    pub fn new_for_edit(event: &CalendarEventRecord) -> Self {
        glib::Object::builder()
            .property("title", "Termin bearbeiten")
            .property("modal", true)
            .property("default-width", 560)
            .property("default-height", 520)
            .property("edit-component", &event.component)
            .property("edit-uid", &event.uid)
            .property("edit-source-uid", &event.source_uid)
            .build()
    }

    fn build_ui(&self) {
        let edit_component = self.imp().edit_component.borrow().clone();
        let edit_uid = self.imp().edit_uid.borrow().clone();
        let edit_source_uid = self.imp().edit_source_uid.borrow().clone();
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

        if let Some(component) = edit_component.as_deref() {
            entry_title.set_text(ical_property(component, "SUMMARY").as_deref().unwrap_or(""));
            row_location.set_text(
                ical_property(component, "LOCATION")
                    .as_deref()
                    .unwrap_or(""),
            );
            let all_day = component_property_has_date_value(component, "DTSTART");
            row_allday.set_active(all_day);
            if let Some(start) = ical_property(component, "DTSTART") {
                row_start.set_text(&format_event_datetime_for_editor(&start, all_day));
            }
            if let Some(end) = ical_property(component, "DTEND") {
                row_end.set_text(&format_event_datetime_for_editor(&end, all_day));
            }
        }

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
        if let Some(component) = edit_component.as_deref() {
            if let Some(description) = ical_property(component, "DESCRIPTION") {
                text_desc.buffer().set_text(&description);
            }
        }
        group_desc.add(&text_desc);
        content_box.append(&group_desc);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));

        let calendar_model_for_load = calendar_model.clone();
        let calendar_source_uids_for_load = Rc::clone(&calendar_source_uids);
        let row_calendar_for_load = row_calendar.clone();
        let edit_source_uid_for_load = edit_source_uid.clone();
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
                    if let Some(edit_source_uid) = edit_source_uid_for_load.as_deref() {
                        if let Some(index) = uids.iter().position(|uid| uid == edit_source_uid) {
                            row_calendar_for_load.set_selected(index as u32);
                        }
                    }
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
        let edit_component_for_save = edit_component.clone();
        let edit_uid_for_save = edit_uid.clone();
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
            let new_component = match build_event_component(
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

            let component = if let (Some(original), Some(uid)) = (
                edit_component_for_save.as_deref(),
                edit_uid_for_save.as_deref(),
            ) {
                let all_day = row_allday.is_active();
                let start_value =
                    parse_event_datetime(&row_start.text(), all_day, "Beginn").unwrap_or_default();
                let end_value =
                    parse_event_datetime(&row_end.text(), all_day, "Ende").unwrap_or_default();
                let start_property = if all_day {
                    "DTSTART;VALUE=DATE"
                } else {
                    "DTSTART"
                };
                let end_property = if all_day { "DTEND;VALUE=DATE" } else { "DTEND" };
                let mut updated =
                    update_ical_property(original, "UID", Some(&escape_ical_text(uid)));
                updated = update_ical_property(
                    &updated,
                    "SUMMARY",
                    Some(&escape_ical_text(entry_title.text().trim())),
                );
                updated = update_ical_property(
                    &updated,
                    "LOCATION",
                    Some(&escape_ical_text(row_location.text().trim())),
                );
                updated = update_ical_property(&updated, start_property, Some(&start_value));
                updated = update_ical_property(&updated, end_property, Some(&end_value));
                updated = update_ical_property(
                    &updated,
                    "DESCRIPTION",
                    Some(&escape_ical_text(&text_desc.buffer().text(
                        &text_desc.buffer().start_iter(),
                        &text_desc.buffer().end_iter(),
                        false,
                    ))),
                );
                updated
            } else {
                new_component
            };
            let source_uid_for_worker = source_uid.clone();
            let operation_uid = edit_uid_for_save.clone();
            let was_edit = edit_uid_for_save.is_some();
            let component_for_worker = component.clone();
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let service = ECalService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => service.modify_component(
                        &source_uid_for_worker,
                        ECalComponentType::Event,
                        uid,
                        &component_for_worker,
                    ),
                    None => service.create_component(
                        &source_uid_for_worker,
                        ECalComponentType::Event,
                        &component_for_worker,
                    ),
                };
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
                        let message = if was_edit {
                            format!("Termin aktualisiert ({})", result.uid)
                        } else {
                            format!("Termin gespeichert ({})", result.uid)
                        };
                        dialog.show_toast(&message);
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

fn ical_property(raw: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_uppercase();
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .find_map(|line| {
            let (property, value) = line.split_once(':')?;
            let property_name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            (property_name == target).then(|| {
                value
                    .replace("\\n", "\n")
                    .replace("\\N", "\n")
                    .replace("\\,", ",")
                    .replace("\\;", ";")
                    .replace("\\\\", "\\")
            })
        })
}

fn component_property_has_date_value(raw: &str, name: &str) -> bool {
    let target = name.to_ascii_uppercase();
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .any(|line| {
            line.split_once(':')
                .map(|(property, _)| {
                    property
                        .split(';')
                        .next()
                        .unwrap_or(property)
                        .eq_ignore_ascii_case(&target)
                        && property.to_ascii_uppercase().contains("VALUE=DATE")
                })
                .unwrap_or(false)
        })
}

fn format_event_datetime_for_editor(value: &str, all_day: bool) -> String {
    let value = value.trim_end_matches('Z');
    if all_day && value.len() >= 8 {
        return format!("{}-{}-{}", &value[..4], &value[4..6], &value[6..8]);
    }
    if value.len() >= 15 && value.as_bytes().get(8) == Some(&b'T') {
        return format!(
            "{}-{}-{} {}:{}",
            &value[..4],
            &value[4..6],
            &value[6..8],
            &value[9..11],
            &value[11..13]
        );
    }
    value.to_string()
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
