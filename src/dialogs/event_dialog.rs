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
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::{
    CalendarEventRecord, CalendarStoreService, replace_ical_attachments,
    serialize_ical_attachment_value, update_ical_property,
};
use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;

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
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-component" => self.edit_component.borrow().to_value(),
                "edit-uid" => self.edit_uid.borrow().to_value(),
                "edit-source-uid" => self.edit_source_uid.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
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
        row_calendar.set_sensitive(edit_uid.is_none());
        let settings_for_default_calendar = AppSettings::try_new();
        let calendar_source_uids_for_selection = Rc::clone(&calendar_source_uids);
        row_calendar.connect_selected_notify(move |row| {
            let Some(settings) = settings_for_default_calendar.as_ref() else {
                return;
            };
            let Some(source_uid) = calendar_source_uids_for_selection
                .borrow()
                .get(row.selected() as usize)
                .cloned()
            else {
                return;
            };
            settings.set_default_calendar_source_uid(&source_uid);
        });
        group_details.add(&row_calendar);

        let row_location = adw::EntryRow::builder().title("Ort").build();
        group_details.add(&row_location);

        let row_conference = adw::EntryRow::builder().title("Konferenz-URL").build();
        row_conference.set_tooltip_text(Some(
            "z. B. https://meet.example.org/raum — wird als RFC-7986 CONFERENCE gespeichert",
        ));
        group_details.add(&row_conference);

        let row_status = adw::ComboRow::builder()
            .title("Terminstatus")
            .model(&gtk::StringList::new(&[
                "Bestätigt",
                "Vorläufig",
                "Abgesagt",
            ]))
            .build();
        group_details.add(&row_status);
        let row_transparency = adw::ComboRow::builder()
            .title("Verfügbarkeit")
            .subtitle("Ob der Zeitraum als belegt gilt")
            .model(&gtk::StringList::new(&["Beschäftigt", "Frei"]))
            .build();
        group_details.add(&row_transparency);

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

        let group_recurrence = adw::PreferencesGroup::builder()
            .title("Wiederholung & Teilnehmer")
            .description("RFC5545-Werte werden unverändert an ECal/Camel weitergegeben")
            .build();
        let row_recurrence = adw::EntryRow::builder()
            .title("Wiederholung (RRULE)")
            .build();
        row_recurrence.set_text("");
        row_recurrence.set_tooltip_text(Some("z. B. FREQ=WEEKLY;BYDAY=MO;COUNT=10"));
        group_recurrence.add(&row_recurrence);
        let row_rdates = adw::EntryRow::builder()
            .title("Zusätzliche Vorkommen (RDATE, kommasepariert)")
            .build();
        row_rdates.set_tooltip_text(Some("z. B. 20260812,20260819 oder 20260812T120000Z"));
        group_recurrence.add(&row_rdates);
        let row_exdates = adw::EntryRow::builder()
            .title("Ausnahmen (EXDATE, kommasepariert)")
            .build();
        row_exdates.set_tooltip_text(Some("z. B. 20260815,20260822"));
        group_recurrence.add(&row_exdates);
        let row_attendees = adw::EntryRow::builder()
            .title("Teilnehmer (durch Komma getrennt)")
            .build();
        group_recurrence.add(&row_attendees);
        let row_categories = adw::EntryRow::builder().title("Kategorien").build();
        group_recurrence.add(&row_categories);
        let attachment_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::None)
            .height_request(72)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let attachment_buffer = attachment_view.buffer();
        let attachment_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(72)
            .max_content_height(160)
            .build();
        attachment_scroll.set_child(Some(&attachment_view));
        let attachment_picker = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Dateien als Terminanhänge auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        let attachment_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(6)
            .margin_top(6)
            .build();
        let attachment_title = gtk::Label::builder()
            .label("Anhänge (eine URI oder Datei pro Zeile)")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        attachment_header.append(&attachment_title);
        attachment_header.append(&attachment_picker);
        let attachment_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        attachment_box.append(&attachment_header);
        attachment_box.append(&attachment_scroll);
        group_recurrence.add(&attachment_box);
        let row_alarm = adw::SpinRow::with_range(0.0, 10_080.0, 5.0);
        row_alarm.set_title("Erinnerung vorher (Minuten)");
        row_alarm.set_digits(0);
        row_alarm.set_snap_to_ticks(true);
        row_alarm.set_value(
            AppSettings::try_new()
                .map(|settings| settings.calendar_default_reminder_minutes())
                .unwrap_or(15) as f64,
        );
        group_recurrence.add(&row_alarm);
        let row_private = adw::SwitchRow::builder()
            .title("Privater Termin")
            .subtitle("Andere Personen sehen den Termin als privat markiert")
            .build();
        group_recurrence.add(&row_private);
        content_box.append(&group_recurrence);

        let attachment_buffer_for_picker = attachment_buffer.clone();
        attachment_picker.connect_clicked(move |button| {
            let Some(root) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let file_dialog = gtk::FileDialog::builder()
                .title("Terminanhänge auswählen")
                .modal(true)
                .build();
            let attachment_buffer_for_result = attachment_buffer_for_picker.clone();
            file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
                let Ok(files) = result else {
                    return;
                };
                let mut values = attachment_buffer_for_result
                    .text(
                        &attachment_buffer_for_result.start_iter(),
                        &attachment_buffer_for_result.end_iter(),
                        false,
                    )
                    .lines()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                for index in 0..files.n_items() {
                    let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                        continue;
                    };
                    let uri = file.uri().to_string();
                    if !uri.is_empty() && !values.iter().any(|value| value == &uri) {
                        values.push(uri);
                    }
                }
                attachment_buffer_for_result.set_text(&values.join("\n"));
            });
        });

        if let Some(component) = edit_component.as_deref() {
            entry_title.set_text(ical_property(component, "SUMMARY").as_deref().unwrap_or(""));
            row_location.set_text(
                ical_property(component, "LOCATION")
                    .as_deref()
                    .unwrap_or(""),
            );
            row_conference.set_text(
                ical_property(component, "CONFERENCE")
                    .or_else(|| ical_property(component, "X-CONFERENCE"))
                    .as_deref()
                    .unwrap_or(""),
            );
            row_status.set_selected(
                match ical_property(component, "STATUS")
                    .unwrap_or_default()
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "TENTATIVE" => 1,
                    "CANCELLED" => 2,
                    _ => 0,
                },
            );
            row_transparency.set_selected(
                (ical_property(component, "TRANSP")
                    .map(|value| value.eq_ignore_ascii_case("TRANSPARENT"))
                    .unwrap_or(false)) as u32,
            );
            let all_day = component_property_has_date_value(component, "DTSTART");
            row_allday.set_active(all_day);
            if let Some(start) = ical_property(component, "DTSTART") {
                row_start.set_text(&format_event_datetime_for_editor(&start, all_day));
            }
            if let Some(end) = ical_property(component, "DTEND") {
                row_end.set_text(&format_event_datetime_for_editor(&end, all_day));
            }
            row_recurrence.set_text(&ical_property(component, "RRULE").unwrap_or_default());
            row_rdates.set_text(&ical_properties(component, "RDATE").join(","));
            row_exdates.set_text(&ical_properties(component, "EXDATE").join(","));
            row_attendees.set_text(&ical_properties(component, "ATTENDEE").join(", "));
            row_categories.set_text(&ical_property(component, "CATEGORIES").unwrap_or_default());
            attachment_buffer.set_text(&ical_properties(component, "ATTACH").join("\n"));
            row_alarm.set_value(ical_alarm_minutes(component).unwrap_or(0) as f64);
            row_private.set_active(
                ical_property(component, "CLASS")
                    .map(|value| value.eq_ignore_ascii_case("PRIVATE"))
                    .unwrap_or(false),
            );
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
        let local_calendar_available = CalendarStoreService::new().is_writable();
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_calendar_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
            if let Some(error) = discovery.error.as_deref() {
                tracing::warn!(%error, "Could not reach EDS calendars for event dialog");
            }
            let active_sources: Vec<_> = discovery
                .sources
                .into_iter()
                .filter(|source| source.enabled)
                .collect();
            let mut names: Vec<String> = active_sources
                .iter()
                .map(|source| {
                    if source.backend_name.is_empty() {
                        source.display_name.clone()
                    } else {
                        format!("{} · {}", source.display_name, source.backend_name)
                    }
                })
                .collect();
            let mut uids = calendar_source_uids_for_load.borrow_mut();
            uids.clear();
            uids.extend(active_sources.into_iter().map(|source| source.uid));
            let preserved_source_uid = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid.as_deref())
                .filter(|uid| {
                    *uid != "local-calendar" && !uids.iter().any(|candidate| candidate == uid)
                });
            if let Some(uid) = preserved_source_uid {
                names.push(format!("Nicht verfügbare Quelle · {uid}"));
                uids.push(uid.to_string());
            }
            if local_calendar_available {
                names.push("Lokaler Kalender".to_string());
                uids.push("local-calendar".to_string());
            }
            if names.is_empty() {
                names.push(if discovery.error.is_some() {
                    "EDS-Kalender sind nicht erreichbar".to_string()
                } else {
                    "Kein aktiver EDS-Kalender verfügbar".to_string()
                });
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            calendar_model_for_load.splice(0, calendar_model_for_load.n_items(), &name_refs);
            if let Some(edit_source_uid) = edit_source_uid_for_load.as_deref() {
                if let Some(index) = uids.iter().position(|uid| uid == edit_source_uid) {
                    row_calendar_for_load.set_selected(index as u32);
                }
            } else if let Some(preferred_source_uid) = preferred_source_uid.as_deref() {
                if let Some(index) = uids.iter().position(|uid| uid == preferred_source_uid) {
                    row_calendar_for_load.set_selected(index as u32);
                }
            }
        });

        let calendar_source_uids_for_save = Rc::clone(&calendar_source_uids);
        let edit_component_for_save = edit_component.clone();
        let edit_uid_for_save = edit_uid.clone();
        let edit_source_uid_for_save = edit_source_uid.clone();
        let dialog_weak = self.downgrade();
        btn_save.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let source_uid = if let Some(source_uid) = edit_source_uid_for_save.clone() {
                source_uid
            } else {
                let selected = row_calendar.selected() as usize;
                let Some(source_uid) = calendar_source_uids_for_save
                    .borrow()
                    .get(selected)
                    .cloned()
                else {
                    dialog.show_toast("Kein aktiver EDS-Kalender ist ausgewählt");
                    return;
                };
                source_uid
            };
            let attachments = attachment_buffer.text(
                &attachment_buffer.start_iter(),
                &attachment_buffer.end_iter(),
                false,
            );
            let new_component = match build_event_component(
                &entry_title.text(),
                &row_location.text(),
                &row_conference.text(),
                match row_status.selected() {
                    1 => "TENTATIVE",
                    2 => "CANCELLED",
                    _ => "CONFIRMED",
                },
                row_transparency.selected() == 1,
                row_allday.is_active(),
                &row_start.text(),
                &row_end.text(),
                &text_desc.buffer().text(
                    &text_desc.buffer().start_iter(),
                    &text_desc.buffer().end_iter(),
                    false,
                ),
                &row_recurrence.text(),
                &row_rdates.text(),
                &row_exdates.text(),
                &row_attendees.text(),
                &row_categories.text(),
                &attachments,
                row_alarm.value().round() as u32,
                row_private.is_active(),
            ) {
                Ok(component) => component,
                Err(error) => {
                    dialog.show_toast(&error);
                    return;
                }
            };

            let category_names = split_ical_category_names(&row_categories.text());
            if let Err(error) = CalendarCategoryStore::default().ensure_names(category_names) {
                tracing::warn!(%error, "Could not persist calendar category metadata");
            }

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
                    "DTSTART;VALUE=DATE".to_string()
                } else {
                    ical_datetime_property_prefix(original, "DTSTART")
                        .unwrap_or_else(|| "DTSTART".to_string())
                };
                let end_property = if all_day {
                    "DTEND;VALUE=DATE".to_string()
                } else {
                    ical_datetime_property_prefix(original, "DTEND")
                        .unwrap_or_else(|| "DTEND".to_string())
                };
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
                let conference = row_conference.text();
                let conference_value =
                    (!conference.trim().is_empty()).then(|| escape_ical_text(conference.trim()));
                updated = update_ical_property(&updated, "CONFERENCE", conference_value.as_deref());
                updated = update_ical_property(
                    &updated,
                    "STATUS",
                    Some(match row_status.selected() {
                        1 => "TENTATIVE",
                        2 => "CANCELLED",
                        _ => "CONFIRMED",
                    }),
                );
                updated = update_ical_property(
                    &updated,
                    "TRANSP",
                    Some(if row_transparency.selected() == 1 {
                        "TRANSPARENT"
                    } else {
                        "OPAQUE"
                    }),
                );
                updated = update_ical_property(&updated, &start_property, Some(&start_value));
                updated = update_ical_property(&updated, &end_property, Some(&end_value));
                updated = update_ical_property(
                    &updated,
                    "DESCRIPTION",
                    Some(&escape_ical_text(&text_desc.buffer().text(
                        &text_desc.buffer().start_iter(),
                        &text_desc.buffer().end_iter(),
                        false,
                    ))),
                );
                let recurrence = row_recurrence.text();
                updated = update_ical_property(
                    &updated,
                    "RRULE",
                    (!recurrence.trim().is_empty()).then_some(recurrence.as_str()),
                );
                let rdate_prefix = ical_recurrence_property_prefix(original, "RDATE", all_day)
                    .unwrap_or_else(|| default_recurrence_property_prefix("RDATE", all_day));
                let rdates = serialize_recurrence_values(&row_rdates.text());
                updated = update_ical_property(
                    &updated,
                    &rdate_prefix,
                    (!rdates.is_empty()).then_some(rdates.as_str()),
                );
                let exdate_prefix = ical_recurrence_property_prefix(original, "EXDATE", all_day)
                    .unwrap_or_else(|| default_recurrence_property_prefix("EXDATE", all_day));
                let exdates = serialize_recurrence_values(&row_exdates.text());
                updated = update_ical_property(
                    &updated,
                    &exdate_prefix,
                    (!exdates.is_empty()).then_some(exdates.as_str()),
                );
                let categories = serialize_categories(&row_categories.text());
                updated = update_ical_property(
                    &updated,
                    "CATEGORIES",
                    (!categories.is_empty()).then_some(categories.as_str()),
                );
                updated = update_ical_property(
                    &updated,
                    "CLASS",
                    Some(if row_private.is_active() {
                        "PRIVATE"
                    } else {
                        "PUBLIC"
                    }),
                );
                let attendees = parse_attendees(&row_attendees.text());
                updated = replace_ical_attendees(&updated, &attendees);
                let attachments = parse_attachments(&attachment_buffer.text(
                    &attachment_buffer.start_iter(),
                    &attachment_buffer.end_iter(),
                    false,
                ));
                updated = replace_ical_attachments(&updated, &attachments);
                updated = replace_ical_alarm(&updated, row_alarm.value().round() as u32);
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

fn ical_properties(raw: &str, name: &str) -> Vec<String> {
    let target = name.to_ascii_uppercase();
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter_map(|line| {
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
        .collect()
}

fn split_ical_category_names(value: &str) -> Vec<String> {
    let mut categories = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
        } else if character == ',' {
            if !current.trim().is_empty() {
                categories.push(current.trim().to_string());
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.trim().is_empty() {
        categories.push(current.trim().to_string());
    }
    categories
}

fn serialize_categories(value: &str) -> String {
    split_ical_category_names(value)
        .into_iter()
        .map(|category| escape_ical_text(&category))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_recurrence_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn serialize_recurrence_values(value: &str) -> String {
    parse_recurrence_values(value).join(",")
}

fn default_recurrence_property_prefix(name: &str, all_day: bool) -> String {
    if all_day {
        format!("{name};VALUE=DATE")
    } else {
        name.to_string()
    }
}

fn build_recurrence_property_line(name: &str, value: &str, all_day: bool) -> String {
    let values = serialize_recurrence_values(value);
    if values.is_empty() {
        String::new()
    } else {
        format!(
            "{}:{}\r\n",
            default_recurrence_property_prefix(name, all_day),
            values
        )
    }
}

fn ical_recurrence_property_prefix(raw: &str, name: &str, all_day: bool) -> Option<String> {
    let target = name.to_ascii_uppercase();
    let mut depth = 0usize;
    for line in unfold_ical_lines(raw) {
        let property = line
            .split_once(':')
            .map(|(property, _)| property)
            .unwrap_or(line.as_str());
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        let is_date_value = property.to_ascii_uppercase().contains("VALUE=DATE");
        if property_name == target && depth == 1 && is_date_value == all_day {
            return Some(property.to_string());
        }
        if property_name == "BEGIN" {
            depth = depth.saturating_add(1);
        } else if property_name == "END" {
            depth = depth.saturating_sub(1);
        }
    }
    None
}

fn ical_alarm_minutes(raw: &str) -> Option<u32> {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .find_map(|line| {
            let (property, value) = line.split_once(':')?;
            if !property
                .split(';')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("TRIGGER"))
            {
                return None;
            }
            let value = value.strip_prefix("-PT")?.strip_suffix('M')?;
            value.parse::<u32>().ok()
        })
}

fn parse_attendees(value: &str) -> Vec<String> {
    value
        .split(|character| character == ',' || character == ';')
        .map(str::trim)
        .filter(|attendee| !attendee.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_attachments(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|attachment| !attachment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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

fn ical_datetime_property_prefix(raw: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, _) = line.split_once(':')?;
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (property_name == target && !property.to_ascii_uppercase().contains("VALUE=DATE"))
            .then(|| property.to_string())
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
    conference: &str,
    status: &str,
    transparent: bool,
    all_day: bool,
    start: &str,
    end: &str,
    description: &str,
    recurrence: &str,
    rdates: &str,
    exdates: &str,
    attendees: &str,
    categories: &str,
    attachments: &str,
    alarm_minutes: u32,
    private: bool,
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
    let status = match status.to_ascii_uppercase().as_str() {
        "TENTATIVE" => "TENTATIVE",
        "CANCELLED" => "CANCELLED",
        "CONFIRMED" => "CONFIRMED",
        _ => return Err("Der Terminstatus ist ungültig".to_string()),
    };
    for (field, value) in [
        ("RRULE", recurrence),
        ("RDATE", rdates),
        ("EXDATE", exdates),
        ("Teilnehmer", attendees),
        ("Konferenz-URL", conference),
    ] {
        if value
            .chars()
            .any(|character| character == '\r' || character == '\n')
        {
            return Err(format!("{field} enthält ungültige Zeilenumbrüche"));
        }
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

    let recurrence_line = if recurrence.trim().is_empty() {
        String::new()
    } else {
        format!("RRULE:{}\r\n", recurrence.trim())
    };
    let status_line = format!("STATUS:{status}\r\n");
    let transparency_line = if transparent {
        "TRANSP:TRANSPARENT\r\n"
    } else {
        "TRANSP:OPAQUE\r\n"
    };
    let rdate_line = build_recurrence_property_line("RDATE", rdates, all_day);
    let exdate_line = build_recurrence_property_line("EXDATE", exdates, all_day);
    let attendee_lines = attendees
        .split(|character| character == ',' || character == ';')
        .map(str::trim)
        .filter(|attendee| !attendee.is_empty())
        .map(|attendee| format!("ATTENDEE:{}\r\n", escape_ical_text(attendee)))
        .collect::<String>();
    let categories_line = if categories.trim().is_empty() {
        String::new()
    } else {
        format!("CATEGORIES:{}\r\n", serialize_categories(categories.trim()))
    };
    let attachment_lines = parse_attachments(attachments)
        .into_iter()
        .map(|attachment| {
            format!(
                "ATTACH;VALUE=URI:{}\r\n",
                serialize_ical_attachment_value(&attachment)
            )
        })
        .collect::<String>();
    let conference_line = if conference.trim().is_empty() {
        String::new()
    } else {
        format!("CONFERENCE:{}\r\n", escape_ical_text(conference.trim()))
    };
    let class_line = if private { "CLASS:PRIVATE\r\n" } else { "" };
    let alarm = if alarm_minutes > 0 {
        format!(
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:{}\r\nTRIGGER:-PT{}M\r\nEND:VALARM\r\n",
            escape_ical_text(title),
            alarm_minutes
        )
    } else {
        String::new()
    };

    Ok(format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{stamp}\r\nSUMMARY:{}\r\n{}\
         \r\n{}\r\nLOCATION:{}\r\nDESCRIPTION:{}\r\n{}{}{}{}{}{}{}{}{}{}{}END:VEVENT\r\n",
        escape_ical_text(title),
        start_line,
        end_line,
        escape_ical_text(location.trim()),
        escape_ical_text(description.trim()),
        status_line,
        transparency_line,
        recurrence_line,
        rdate_line,
        exdate_line,
        attendee_lines,
        conference_line,
        categories_line,
        attachment_lines,
        class_line,
        alarm,
    ))
}

fn replace_ical_attendees(raw: &str, values: &[String]) -> String {
    let existing = existing_attendee_properties(raw);
    let mut lines = Vec::new();
    let mut replaced = false;
    let mut depth = 0usize;
    for line in unfold_ical_lines(raw) {
        let property_part = line
            .split_once(':')
            .map(|(property, _)| property)
            .unwrap_or(line.as_str());
        let property = property_part
            .split(';')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if property == "ATTENDEE" && depth <= 1 {
            if !replaced {
                let mut used = vec![false; existing.len()];
                for value in values {
                    let matching_existing =
                        existing
                            .iter()
                            .enumerate()
                            .find(|(index, (existing_value, _))| {
                                !used[*index] && existing_value.eq_ignore_ascii_case(value)
                            });
                    let line = if let Some((index, (_, property_prefix))) = matching_existing {
                        used[index] = true;
                        format!("{property_prefix}:{}", escape_ical_text(value))
                    } else {
                        format!("ATTENDEE:{}", escape_ical_text(value))
                    };
                    lines.push(line);
                }
                replaced = true;
            }
            continue;
        }
        lines.push(line);
        if property == "BEGIN" {
            depth = depth.saturating_add(1);
        } else if property == "END" {
            depth = depth.saturating_sub(1);
        }
    }
    if !replaced && !values.is_empty() {
        insert_before_event_end(
            &mut lines,
            values
                .iter()
                .map(|value| format!("ATTENDEE:{}", escape_ical_text(value))),
        );
    }
    format!("{}\r\n", lines.join("\r\n"))
}

fn existing_attendee_properties(raw: &str) -> Vec<(String, String)> {
    let mut attendees = Vec::new();
    let mut depth = 0usize;
    for line in unfold_ical_lines(raw) {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        if name == "ATTENDEE" && depth == 1 {
            attendees.push((unescape_ical_text(value), property.to_string()));
        }
        if name == "BEGIN" {
            depth = depth.saturating_add(1);
        } else if name == "END" {
            depth = depth.saturating_sub(1);
        }
    }
    attendees
}

fn replace_ical_alarm(raw: &str, alarm_minutes: u32) -> String {
    let source = unfold_ical_lines(raw);
    let mut lines = Vec::new();
    let mut depth = 0usize;
    let mut replaced = false;
    let mut index = 0usize;
    while index < source.len() {
        let line = &source[index];
        if line.eq_ignore_ascii_case("BEGIN:VALARM") && depth == 1 {
            let end = source
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, line)| line.eq_ignore_ascii_case("END:VALARM"))
                .map(|(index, _)| index);
            let Some(end) = end else {
                lines.push(line.clone());
                index += 1;
                continue;
            };
            let alarm = &source[index..=end];
            if is_editable_display_alarm(alarm) {
                if alarm_minutes > 0 && !replaced {
                    lines.extend(display_alarm_lines(alarm_minutes));
                    replaced = true;
                }
            } else {
                lines.extend(alarm.iter().cloned());
            }
            index = end + 1;
            continue;
        }
        lines.push(line.clone());
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            depth = depth.saturating_add(1);
        } else if line.eq_ignore_ascii_case("END:VEVENT") {
            depth = depth.saturating_sub(1);
        }
        index += 1;
    }
    if alarm_minutes > 0 && !replaced {
        insert_before_event_end(&mut lines, display_alarm_lines(alarm_minutes));
    }
    format!("{}\r\n", lines.join("\r\n"))
}

fn is_editable_display_alarm(lines: &[String]) -> bool {
    let mut display = false;
    let mut relative_minutes = false;
    for line in lines {
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        if name == "ACTION" && value.eq_ignore_ascii_case("DISPLAY") {
            display = true;
        } else if name == "TRIGGER" {
            relative_minutes = value
                .strip_prefix("-PT")
                .and_then(|value| value.strip_suffix('M'))
                .is_some_and(|value| value.parse::<u32>().is_ok());
        }
    }
    display && relative_minutes
}

fn display_alarm_lines(alarm_minutes: u32) -> Vec<String> {
    vec![
        "BEGIN:VALARM".to_string(),
        "ACTION:DISPLAY".to_string(),
        "DESCRIPTION:Reminder".to_string(),
        format!("TRIGGER:-PT{alarm_minutes}M"),
        "END:VALARM".to_string(),
    ]
}

fn insert_before_event_end<I>(lines: &mut Vec<String>, values: I)
where
    I: IntoIterator<Item = String>,
{
    let index = lines
        .iter()
        .position(|line| line.eq_ignore_ascii_case("END:VEVENT"))
        .unwrap_or(lines.len());
    lines.splice(index..index, values);
}

fn unfold_ical_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(line.trim_start());
            }
        } else {
            lines.push(line.to_string());
        }
    }
    lines
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

fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_component_serialization_is_ical_safe() {
        let component = build_event_component(
            "Team, Sync",
            "Room; 42",
            "https://meet.example.test/room?x=1&y=2",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 12:00",
            "2026-08-10 13:00",
            "Line one\nLine two",
            "FREQ=WEEKLY;BYDAY=MO",
            "20260812T120000Z,20260819T120000Z",
            "20260815T120000Z",
            "mailto:person@example.org",
            "Work",
            "",
            15,
            true,
        )
        .expect("event should serialize");

        assert!(component.contains("BEGIN:VEVENT"));
        assert!(component.contains("SUMMARY:Team\\, Sync"));
        assert!(component.contains("LOCATION:Room\\; 42"));
        assert!(component.contains("CONFERENCE:https://meet.example.test/room?x=1&y=2"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two"));
        assert!(component.contains("DTSTART:20260810T120000"));
        assert!(component.contains("DTEND:20260810T130000"));
        assert!(component.contains("STATUS:CONFIRMED"));
        assert!(component.contains("TRANSP:OPAQUE"));
        assert!(component.contains("RRULE:FREQ=WEEKLY;BYDAY=MO"));
        assert!(component.contains("RDATE:20260812T120000Z,20260819T120000Z"));
        assert!(component.contains("EXDATE:20260815T120000Z"));
        assert!(component.contains("ATTENDEE:mailto:person@example.org"));
        assert!(component.contains("CATEGORIES:Work"));
        assert!(component.contains("TRIGGER:-PT15M"));
        assert!(component.contains("CLASS:PRIVATE"));
        assert!(component.ends_with("END:VEVENT\r\n"));
    }

    #[test]
    fn event_component_rejects_invalid_or_reversed_times() {
        assert!(
            build_event_component(
                "Title",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "not-a-date",
                "2026-08-10 13:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                0,
                false,
            )
            .is_err()
        );
        assert!(
            build_event_component(
                "Title",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 13:00",
                "2026-08-10 12:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                0,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn attendee_updates_preserve_existing_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "ATTENDEE;CN=Bob Example;ROLE=REQ-PARTICIPANT:mailto:bob@example.test\r\n",
            "ATTENDEE;CN=Alice Example:mailto:alice@example.test\r\n",
            "END:VEVENT\r\n"
        );
        let updated = replace_ical_attendees(
            raw,
            &[
                "mailto:bob@example.test".to_string(),
                "mailto:new@example.test".to_string(),
            ],
        );
        assert!(
            updated
                .contains("ATTENDEE;CN=Bob Example;ROLE=REQ-PARTICIPANT:mailto:bob@example.test")
        );
        assert!(updated.contains("ATTENDEE:mailto:new@example.test"));
        assert!(!updated.contains("alice@example.test"));
    }

    #[test]
    fn event_attachments_roundtrip_and_preserve_existing_parameters() {
        let component = build_event_component(
            "Planning",
            "Room",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 12:00",
            "2026-08-10 13:00",
            "",
            "",
            "",
            "",
            "",
            "",
            "https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda\nfile:///tmp/notes.pdf",
            0,
            false,
        )
        .expect("event with attachments should serialize");
        assert!(component.contains(
            "ATTACH;VALUE=URI:https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda"
        ));
        assert!(component.contains("ATTACH;VALUE=URI:file:///tmp/notes.pdf"));

        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda\r\n",
            "ATTACH;VALUE=URI:https://calendar.example.test/old.ics\r\n",
            "END:VEVENT\r\n"
        );
        let updated = replace_ical_attachments(
            raw,
            &[
                "https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda".to_string(),
                "https://calendar.example.test/new.ics".to_string(),
            ],
        );
        assert!(updated.contains(
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda"
        ));
        assert!(updated.contains("ATTACH;VALUE=URI:https://calendar.example.test/new.ics"));
        assert!(!updated.contains("https://calendar.example.test/old.ics"));
    }

    #[test]
    fn editing_preserves_timezone_property_prefix() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T120000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T130000\r\n",
            "END:VEVENT\r\n"
        );

        assert_eq!(
            ical_datetime_property_prefix(raw, "DTSTART").as_deref(),
            Some("DTSTART;TZID=Europe/Berlin")
        );
        assert_eq!(
            ical_datetime_property_prefix(raw, "DTEND").as_deref(),
            Some("DTEND;TZID=Europe/Berlin")
        );
    }

    #[test]
    fn recurrence_date_prefixes_preserve_date_and_timezone_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "RDATE;TZID=Europe/Berlin:20260812T120000,20260819T120000\r\n",
            "EXDATE;VALUE=DATE:20260815\r\n",
            "END:VEVENT\r\n"
        );
        assert_eq!(
            ical_recurrence_property_prefix(raw, "RDATE", false).as_deref(),
            Some("RDATE;TZID=Europe/Berlin")
        );
        assert_eq!(
            ical_recurrence_property_prefix(raw, "EXDATE", true).as_deref(),
            Some("EXDATE;VALUE=DATE")
        );
        assert_eq!(
            build_recurrence_property_line("RDATE", "20260812,20260819", true),
            "RDATE;VALUE=DATE:20260812,20260819\r\n"
        );
    }

    #[test]
    fn editing_simple_alarm_does_not_delete_unsupported_alarm_types() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-1\r\n",
            "BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-P1D\r\n",
            "ATTENDEE:mailto:notify@example.test\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=START:-PT15M\r\n",
            "DESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\n"
        );
        assert_eq!(ical_alarm_minutes(raw), Some(15));
        let removed = replace_ical_alarm(raw, 0);
        assert!(removed.contains("ACTION:EMAIL"));
        assert!(removed.contains("TRIGGER:-P1D"));
        assert!(!removed.contains("TRIGGER;RELATED=START:-PT15M"));

        let updated = replace_ical_alarm(raw, 30);
        assert!(updated.contains("ACTION:EMAIL"));
        assert!(updated.contains("TRIGGER:-P1D"));
        assert!(updated.contains("TRIGGER:-PT30M"));
        assert!(!updated.contains("TRIGGER;RELATED=START:-PT15M"));
    }

    #[test]
    fn category_names_split_only_on_unescaped_commas() {
        assert_eq!(
            split_ical_category_names(r"Arbeit,Privat\, Familie,Reise"),
            vec![
                "Arbeit".to_string(),
                "Privat, Familie".to_string(),
                "Reise".to_string()
            ]
        );
    }

    #[test]
    fn category_serialization_keeps_category_boundaries() {
        assert_eq!(
            serialize_categories(r"Arbeit,Privat\, Familie,Reise"),
            r"Arbeit,Privat\, Familie,Reise"
        );
    }
}
