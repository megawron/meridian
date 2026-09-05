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
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
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
use crate::services::reminder_service::{
    Alarm, AlarmAction, AlarmDuration, AlarmRelation, AlarmTrigger, parse_alarms,
    replace_component_alarms,
};
use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct EventDialog {
        pub edit_component: RefCell<Option<String>>,
        pub edit_uid: RefCell<Option<String>>,
        pub edit_source_uid: RefCell<Option<String>>,
        pub initial_date: RefCell<Option<String>>,
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
                    glib::ParamSpecString::builder("initial-date").build(),
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
                "initial-date" => {
                    *self.initial_date.borrow_mut() = value.get().expect("string property")
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-component" => self.edit_component.borrow().to_value(),
                "edit-uid" => self.edit_uid.borrow().to_value(),
                "edit-source-uid" => self.edit_source_uid.borrow().to_value(),
                "initial-date" => self.initial_date.borrow().to_value(),
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

    pub fn new_for_date(date: NaiveDate) -> Self {
        glib::Object::builder()
            .property("title", "Neuer Termin")
            .property("modal", true)
            .property("default-width", 560)
            .property("default-height", 520)
            .property("initial-date", date.format("%Y-%m-%d").to_string())
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

        let calendar_model = gtk::StringList::new(&["Kalender werden geladen…"]);
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

        let now = Local::now();
        let initial_date = self
            .imp()
            .initial_date
            .borrow()
            .as_deref()
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
        let default_start = initial_date
            .and_then(|date| date.and_hms_opt(now.hour(), now.minute(), 0))
            .unwrap_or_else(|| now.naive_local());
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
            .title("Wiederholung und Teilnehmer")
            .description("RFC5545-Werte werden unverändert an den Kalenderdienst weitergegeben")
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
        let alarm_editor = AlarmEditor::new(edit_component.as_deref(), "");
        if edit_component.is_none() {
            if let Some(settings) = AppSettings::try_new() {
                if settings.use_default_reminder() {
                    let default_minutes = settings.default_reminder_total_minutes();
                    alarm_editor.add_default_display_if_empty(default_minutes);
                }
            }
        }
        group_recurrence.add(alarm_editor.widget());
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
            row_private.set_active(
                ical_property(component, "CLASS")
                    .map(|value| value.eq_ignore_ascii_case("PRIVATE"))
                    .unwrap_or(false),
            );
        }

        content_box.append(&group_time);

        // Description
        let group_desc = adw::PreferencesGroup::builder()
            .title("Beschreibung und Notizen")
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
            let mut uids: Vec<String> = active_sources
                .into_iter()
                .map(|source| source.uid)
                .collect();
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
                    "Kalender sind nicht erreichbar".to_string()
                } else {
                    "Kein aktiver Kalender verfügbar".to_string()
                });
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            calendar_model_for_load.splice(0, calendar_model_for_load.n_items(), &name_refs);

            // Replace the stored uids completely so no borrow is open during set_selected
            calendar_source_uids_for_load.replace(uids.clone());

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
                    dialog.show_toast("Kein aktiver Kalender ist ausgewählt");
                    return;
                };
                source_uid
            };
            let attachments = attachment_buffer.text(
                &attachment_buffer.start_iter(),
                &attachment_buffer.end_iter(),
                false,
            );
            alarm_editor.set_summary(&entry_title.text());
            let alarms = alarm_editor.alarms();
            let status = match row_status.selected() {
                1 => "TENTATIVE",
                2 => "CANCELLED",
                _ => "CONFIRMED",
            };
            let description = text_desc.buffer().text(
                &text_desc.buffer().start_iter(),
                &text_desc.buffer().end_iter(),
                false,
            );
            let component = match (
                edit_component_for_save.as_deref(),
                edit_uid_for_save.as_deref(),
            ) {
                (Some(original), Some(uid)) => update_existing_event_component(
                    original,
                    uid,
                    &entry_title.text(),
                    &row_location.text(),
                    &row_conference.text(),
                    status,
                    row_transparency.selected() == 1,
                    row_allday.is_active(),
                    &row_start.text(),
                    &row_end.text(),
                    &description,
                    &row_recurrence.text(),
                    &row_rdates.text(),
                    &row_exdates.text(),
                    &row_attendees.text(),
                    &row_categories.text(),
                    &attachments,
                    &alarms,
                    row_private.is_active(),
                ),
                _ => build_event_component(
                    &entry_title.text(),
                    &row_location.text(),
                    &row_conference.text(),
                    status,
                    row_transparency.selected() == 1,
                    row_allday.is_active(),
                    &row_start.text(),
                    &row_end.text(),
                    &description,
                    &row_recurrence.text(),
                    &row_rdates.text(),
                    &row_exdates.text(),
                    &row_attendees.text(),
                    &row_categories.text(),
                    &attachments,
                    &alarms,
                    row_private.is_active(),
                ),
            };
            let component = match component {
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

/// Shared GTK4 reminder page used by event, task and memo editors.  The
/// service owns the VALARM value model; this small widget owns only staged
/// ordering and editor state.  Every visible control below changes that
/// staged list or opens a real editor, so imported non-DISPLAY alarms are
/// never represented by a decorative, non-functional button.
#[derive(Clone)]
pub(crate) struct AlarmEditor {
    root: gtk::Box,
    alarms: Rc<RefCell<Vec<Alarm>>>,
    list: gtk::ListBox,
    summary: Rc<RefCell<String>>,
}

impl AlarmEditor {
    pub(crate) fn new(component: Option<&str>, summary: &str) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        let heading = gtk::Label::builder()
            .label("Erinnerungen")
            .halign(gtk::Align::Start)
            .css_classes(vec!["heading"])
            .build();
        root.append(&heading);
        let description = gtk::Label::builder()
            .label("Mehrere VALARM-Einträge; fremde Eigenschaften bleiben erhalten")
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(vec!["dim-label"])
            .build();
        root.append(&description);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_vexpand(false);
        list.add_css_class("boxed-list");
        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(84)
            .max_content_height(220)
            .child(&list)
            .build();
        root.append(&list_scroll);

        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .build();
        let add_button = gtk::Button::with_label("Hinzufügen");
        let edit_button = gtk::Button::with_label("Bearbeiten");
        let remove_button = gtk::Button::with_label("Entfernen");
        remove_button.add_css_class("destructive-action");
        let up_button = gtk::Button::with_label("Nach oben");
        let down_button = gtk::Button::with_label("Nach unten");
        for button in [
            &add_button,
            &edit_button,
            &remove_button,
            &up_button,
            &down_button,
        ] {
            button.set_valign(gtk::Align::Center);
            controls.append(button);
        }
        root.append(&controls);

        let editor = Self {
            root,
            alarms: Rc::new(RefCell::new(
                component.map(parse_alarms).unwrap_or_default(),
            )),
            list,
            summary: Rc::new(RefCell::new(summary.to_string())),
        };

        let editor_for_add = editor.clone();
        add_button.connect_clicked(move |_| editor_for_add.present_alarm_dialog(None));
        let editor_for_edit = editor.clone();
        edit_button.connect_clicked(move |_| {
            if let Some(index) = editor_for_edit.selected_index() {
                editor_for_edit.present_alarm_dialog(Some(index));
            }
        });
        let editor_for_remove = editor.clone();
        remove_button.connect_clicked(move |_| {
            let Some(index) = editor_for_remove.selected_index() else {
                return;
            };
            editor_for_remove.alarms.borrow_mut().remove(index);
            editor_for_remove.rebuild(Some(index.saturating_sub(1)));
        });
        let editor_for_up = editor.clone();
        up_button.connect_clicked(move |_| editor_for_up.move_selected(-1));
        let editor_for_down = editor.clone();
        down_button.connect_clicked(move |_| editor_for_down.move_selected(1));

        let edit_button_for_selection = edit_button.clone();
        let remove_button_for_selection = remove_button.clone();
        let up_button_for_selection = up_button.clone();
        let down_button_for_selection = down_button.clone();
        let list_for_selection = editor.list.clone();
        editor.list.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                edit_button_for_selection.set_sensitive(false);
                remove_button_for_selection.set_sensitive(false);
                up_button_for_selection.set_sensitive(false);
                down_button_for_selection.set_sensitive(false);
                return;
            };
            let index = row.index();
            let last = list_for_selection.observe_children().n_items() as i32 - 1;
            edit_button_for_selection.set_sensitive(true);
            remove_button_for_selection.set_sensitive(true);
            up_button_for_selection.set_sensitive(index > 0);
            down_button_for_selection.set_sensitive(index >= 0 && index < last);
        });

        editor.rebuild(None);
        editor
    }

    pub(crate) fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub(crate) fn alarms(&self) -> Vec<Alarm> {
        self.alarms.borrow().clone()
    }

    pub(crate) fn set_component(&self, component: Option<&str>, summary: &str) {
        self.alarms
            .replace(component.map(parse_alarms).unwrap_or_default());
        self.summary.replace(summary.to_string());
        self.rebuild(None);
    }

    pub(crate) fn set_summary(&self, summary: &str) {
        self.summary.replace(summary.to_string());
    }

    pub(crate) fn add_default_display_if_empty(&self, minutes: u32) {
        if minutes == 0 || !self.alarms.borrow().is_empty() {
            return;
        }
        self.alarms.borrow_mut().push(Alarm::new_display_relative(
            AlarmRelation::Start,
            -i64::from(minutes) * 60,
            None,
        ));
        self.rebuild(Some(0));
    }

    fn selected_index(&self) -> Option<usize> {
        self.list
            .selected_row()
            .map(|row| row.index())
            .filter(|index| *index >= 0)
            .map(|index| index as usize)
    }

    fn move_selected(&self, direction: isize) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let target = index as isize + direction;
        if target < 0 || target as usize >= self.alarms.borrow().len() {
            return;
        }
        self.alarms.borrow_mut().swap(index, target as usize);
        self.rebuild(Some(target as usize));
    }

    fn rebuild(&self, select_index: Option<usize>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for alarm in self.alarms.borrow().iter() {
            let subtitle = if alarm.is_safe_to_notify() {
                "Desktop-Benachrichtigung"
            } else {
                "Nur angezeigt und bewahrt; wird niemals automatisch ausgeführt"
            };
            let row = adw::ActionRow::builder()
                .title(alarm.label())
                .subtitle(subtitle)
                .build();
            self.list.append(&row);
        }
        if let Some(index) = select_index {
            if let Some(row) = self.list.row_at_index(index as i32) {
                self.list.select_row(Some(&row));
            }
        }
    }

    fn present_alarm_dialog(&self, index: Option<usize>) {
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let existing = index.and_then(|index| self.alarms.borrow().get(index).cloned());
        let initial = existing
            .clone()
            .unwrap_or_else(|| Alarm::new_display_relative(AlarmRelation::Start, -15 * 60, None));
        let window = adw::Window::builder()
            .title(if index.is_some() {
                "Erinnerung bearbeiten"
            } else {
                "Erinnerung hinzufügen"
            })
            .modal(true)
            .default_width(480)
            .default_height(620)
            .transient_for(&parent)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let save = gtk::Button::builder()
            .label("Übernehmen")
            .css_classes(vec!["suggested-action"])
            .build();
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(18)
            .margin_end(18)
            .margin_top(14)
            .margin_bottom(18)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content)
            .build();
        toolbar.set_content(Some(&scroll));
        window.set_content(Some(&toolbar));

        let action_values = Rc::new({
            let mut values = vec![
                AlarmAction::Display,
                AlarmAction::Audio,
                AlarmAction::Email,
                AlarmAction::Procedure,
            ];
            if let AlarmAction::Unknown(value) = &initial.action {
                values.push(AlarmAction::Unknown(value.clone()));
            }
            values
        });
        let action_labels = action_values
            .iter()
            .map(AlarmAction::label)
            .collect::<Vec<_>>();
        let action_label_refs = action_labels.iter().map(String::as_str).collect::<Vec<_>>();
        let action_model = gtk::StringList::new(&action_label_refs);
        let action_row = adw::ComboRow::builder()
            .title("Aktion")
            .model(&action_model)
            .build();
        let action_index = action_values
            .iter()
            .position(|value| value == &initial.action)
            .unwrap_or(0);
        action_row.set_selected(action_index as u32);
        content.append(&action_row);

        let safety_note = gtk::Label::builder()
            .label(
                "Ton, E-Mail und Prozedur werden aus Sicherheitsgründen nur angezeigt und bewahrt.",
            )
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(vec!["warning", "dim-label"])
            .build();
        safety_note.set_visible(!initial.action.is_safe_to_notify());
        content.append(&safety_note);
        let safety_note_for_action = safety_note.clone();
        let action_values_for_signal = Rc::clone(&action_values);
        action_row.connect_selected_notify(move |row| {
            let unsafe_action = action_values_for_signal
                .get(row.selected() as usize)
                .is_some_and(|action| !action.is_safe_to_notify());
            safety_note_for_action.set_visible(unsafe_action);
        });

        let trigger_values = Rc::new({
            let mut values = vec![0_u8, 1_u8];
            if matches!(initial.trigger, AlarmTrigger::Unknown { .. }) {
                values.push(2);
            }
            values
        });
        let trigger_model = gtk::StringList::new(
            &[
                "Relativ zu Beginn/Ende",
                "Absoluter Zeitpunkt",
                "Unbekannter Trigger (bewahren)",
            ][..trigger_values.len()],
        );
        let trigger_row = adw::ComboRow::builder()
            .title("Trigger-Typ")
            .model(&trigger_model)
            .build();
        let trigger_index = match initial.trigger {
            AlarmTrigger::Relative { .. } => 0,
            AlarmTrigger::Absolute { .. } => 1,
            AlarmTrigger::Unknown { .. } => 2,
        };
        trigger_row.set_selected(trigger_index);
        content.append(&trigger_row);

        let relative_group = adw::PreferencesGroup::builder()
            .title("Relativer Trigger")
            .build();
        let relative_to_model = gtk::StringList::new(&["Beginn", "Ende"]);
        let relative_to_row = adw::ComboRow::builder()
            .title("Bezugspunkt")
            .model(&relative_to_model)
            .build();
        let direction_model = gtk::StringList::new(&["Vorher", "Nachher"]);
        let direction_row = adw::ComboRow::builder()
            .title("Richtung")
            .model(&direction_model)
            .build();
        let offset_row = adw::SpinRow::with_range(0.0, 10_000_000.0, 1.0);
        offset_row.set_title("Abstand");
        offset_row.set_digits(0);
        offset_row.set_snap_to_ticks(true);
        let unit_model = gtk::StringList::new(&["Minuten", "Stunden", "Tage"]);
        let unit_row = adw::ComboRow::builder()
            .title("Einheit")
            .model(&unit_model)
            .build();
        relative_group.add(&relative_to_row);
        relative_group.add(&direction_row);
        relative_group.add(&offset_row);
        relative_group.add(&unit_row);
        content.append(&relative_group);

        let absolute_entry = adw::EntryRow::builder()
            .title("Absoluter Zeitpunkt")
            .build();
        absolute_entry.set_tooltip_text(Some("YYYYMMDDTHHMMSSZ oder YYYYMMDDTHHMMSS"));
        content.append(&absolute_entry);
        let unknown_trigger_label = gtk::Label::builder()
            .label("Der unbekannte Trigger bleibt beim Speichern unverändert.")
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&unknown_trigger_label);

        let (relation, offset) = match initial.trigger {
            AlarmTrigger::Relative {
                relation,
                offset_seconds,
                ..
            } => (relation, offset_seconds),
            _ => (AlarmRelation::Start, -15 * 60),
        };
        relative_to_row.set_selected(match relation {
            AlarmRelation::Start => 0,
            AlarmRelation::End => 1,
        });
        direction_row.set_selected((offset >= 0) as u32);
        let (offset_value, offset_unit) = duration_editor_parts(offset.unsigned_abs());
        offset_row.set_value(offset_value as f64);
        unit_row.set_selected(offset_unit);
        if let AlarmTrigger::Absolute { value, .. } = &initial.trigger {
            absolute_entry.set_text(value);
        }

        let relative_group_for_trigger = relative_group.clone();
        let absolute_entry_for_trigger = absolute_entry.clone();
        let unknown_trigger_for_trigger = unknown_trigger_label.clone();
        trigger_row.connect_selected_notify(move |row| {
            let selected = row.selected();
            relative_group_for_trigger.set_visible(selected == 0);
            absolute_entry_for_trigger.set_visible(selected == 1);
            unknown_trigger_for_trigger.set_visible(selected == 2);
        });
        relative_group.set_visible(trigger_index == 0);
        absolute_entry.set_visible(trigger_index == 1);
        unknown_trigger_label.set_visible(trigger_index == 2);

        let repeat_group = adw::PreferencesGroup::builder()
            .title("Wiederholung")
            .build();
        let repeat_row = adw::SpinRow::with_range(0.0, 4_096.0, 1.0);
        repeat_row.set_title("Wiederholungen nach dem ersten Alarm");
        repeat_row.set_digits(0);
        repeat_row.set_snap_to_ticks(true);
        repeat_row.set_value(initial.repeat.unwrap_or(0) as f64);
        let repeat_interval_row = adw::SpinRow::with_range(0.0, 10_000_000.0, 1.0);
        repeat_interval_row.set_title("Abstand der Wiederholungen");
        repeat_interval_row.set_digits(0);
        repeat_interval_row.set_snap_to_ticks(true);
        let repeat_unit_model = gtk::StringList::new(&["Minuten", "Stunden", "Tage"]);
        let repeat_unit_row = adw::ComboRow::builder()
            .title("Einheit")
            .model(&repeat_unit_model)
            .build();
        if let Some(duration) = &initial.duration {
            let (value, unit) = duration_editor_parts(duration.seconds.unsigned_abs());
            repeat_interval_row.set_value(value as f64);
            repeat_unit_row.set_selected(unit);
        } else {
            repeat_interval_row.set_value(5.0);
        }
        repeat_group.add(&repeat_row);
        repeat_group.add(&repeat_interval_row);
        repeat_group.add(&repeat_unit_row);
        content.append(&repeat_group);

        let options = adw::PreferencesGroup::builder()
            .title("Nachricht und Aktionsdaten")
            .build();
        let message_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(86)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        if let Some(message) = initial.message.as_deref() {
            message_view.buffer().set_text(message);
        }
        options.add(
            &adw::ActionRow::builder()
                .title("Benutzerdefinierte Nachricht")
                .build(),
        );
        options.add(&message_view);
        let attachments_entry = adw::EntryRow::builder()
            .title("Sound/Prozedur-Anhänge (eine URI pro Zeile)")
            .build();
        attachments_entry.set_text(&initial.attachments.join("\n"));
        options.add(&attachments_entry);
        let attendees_entry = adw::EntryRow::builder()
            .title("E-Mail-Empfänger (eine Adresse pro Zeile)")
            .build();
        attendees_entry.set_text(&initial.attendees.join("\n"));
        options.add(&attendees_entry);
        content.append(&options);

        let editor = self.clone();
        let initial_for_save = initial.clone();
        let action_values_for_save = Rc::clone(&action_values);
        let trigger_values_for_save = Rc::clone(&trigger_values);
        let window_for_save = window.clone();
        save.connect_clicked(move |_| {
            let action = action_values_for_save
                .get(action_row.selected() as usize)
                .cloned()
                .unwrap_or(AlarmAction::Display);
            let trigger = match trigger_values_for_save
                .get(trigger_row.selected() as usize)
                .copied()
                .unwrap_or(0)
            {
                0 => {
                    let unit_seconds = alarm_unit_seconds(unit_row.selected());
                    let magnitude = offset_row.value().round() as i64;
                    let signed = magnitude.saturating_mul(unit_seconds);
                    let signed = if direction_row.selected() == 0 {
                        signed.saturating_neg()
                    } else {
                        signed
                    };
                    AlarmTrigger::relative(
                        if relative_to_row.selected() == 1 {
                            AlarmRelation::End
                        } else {
                            AlarmRelation::Start
                        },
                        signed,
                    )
                }
                1 => {
                    let value = absolute_entry.text().trim().to_string();
                    if !is_valid_absolute_alarm_value(&value) {
                        show_alarm_error(
                            &window_for_save,
                            "Der absolute Trigger muss YYYYMMDDTHHMMSS[Z] entsprechen.",
                        );
                        return;
                    }
                    AlarmTrigger::absolute(value)
                }
                _ => initial_for_save.trigger.clone(),
            };
            let repeat = (repeat_row.value().round() as u32).min(4_096);
            let repeat_interval = repeat_interval_row.value().round() as i64;
            let duration = (repeat_interval > 0).then(|| {
                AlarmDuration::new(repeat_interval * alarm_unit_seconds(repeat_unit_row.selected()))
            });
            let message = message_view
                .buffer()
                .text(
                    &message_view.buffer().start_iter(),
                    &message_view.buffer().end_iter(),
                    false,
                )
                .to_string();
            let attachments = attachments_entry
                .text()
                .lines()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let attendees = attendees_entry
                .text()
                .lines()
                .flat_map(|line| line.split(',').map(str::trim))
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let mut alarm = initial_for_save.clone();
            alarm.action = action;
            alarm.trigger = trigger;
            alarm.repeat = (repeat > 0).then_some(repeat);
            alarm.duration = duration;
            alarm.message = (!message.trim().is_empty()).then_some(message);
            alarm.attachments = attachments;
            alarm.attendees = attendees;
            if let Some(index) = index {
                if let Some(existing) = editor.alarms.borrow_mut().get_mut(index) {
                    *existing = alarm;
                }
                editor.rebuild(Some(index));
            } else {
                editor.alarms.borrow_mut().push(alarm);
                let index = editor.alarms.borrow().len().saturating_sub(1);
                editor.rebuild(Some(index));
            }
            window_for_save.close();
        });

        window.present();
    }
}

fn duration_editor_parts(seconds: u64) -> (u64, u32) {
    if seconds != 0 && seconds % (24 * 60 * 60) == 0 {
        (seconds / (24 * 60 * 60), 2)
    } else if seconds != 0 && seconds % (60 * 60) == 0 {
        (seconds / (60 * 60), 1)
    } else {
        (seconds / 60, 0)
    }
}

fn alarm_unit_seconds(index: u32) -> i64 {
    match index {
        1 => 60 * 60,
        2 => 24 * 60 * 60,
        _ => 60,
    }
}

fn is_valid_absolute_alarm_value(value: &str) -> bool {
    let value = value.trim();
    if value.len() != 15 && value.len() != 16 {
        return false;
    }
    if value.as_bytes().get(8) != Some(&b'T')
        || !value[..8]
            .chars()
            .all(|character| character.is_ascii_digit())
        || !value[9..]
            .chars()
            .all(|character| character.is_ascii_digit() || character == 'Z')
    {
        return false;
    }
    chrono::NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").is_ok()
}

fn show_alarm_error(parent: &adw::Window, message: &str) {
    let dialog = adw::AlertDialog::new(
        Some("Erinnerung konnte nicht übernommen werden"),
        Some(message),
    );
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.present(Some(parent));
}

fn ical_property(raw: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw).into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let property_name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (property_name == target).then(|| unescape_ical_text(value))
    })
}

fn ical_properties(raw: &str, name: &str) -> Vec<String> {
    let target = name.to_ascii_uppercase();
    unfold_ical_lines(raw)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let property_name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            (property_name == target).then(|| unescape_ical_text(value))
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
    unfold_ical_lines(raw).into_iter().any(|line| {
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
    if !value.is_ascii() {
        return value.to_string();
    }
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
    alarms: &[Alarm],
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
    let component = format!(
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
        "",
    );
    Ok(replace_component_alarms(&component, alarms))
}

/// Update an existing event component while preserving unknown properties, parameters,
/// and vendor-specific fields (e.g., ORGANIZER, GEO, URL, PRIORITY, X-*, and custom alarms).
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_existing_event_component(
    original: &str,
    uid: &str,
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
    alarms: &[Alarm],
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

    let start_property = if all_day {
        "DTSTART;VALUE=DATE".to_string()
    } else {
        ical_datetime_property_prefix(original, "DTSTART").unwrap_or_else(|| "DTSTART".to_string())
    };
    let end_property = if all_day {
        "DTEND;VALUE=DATE".to_string()
    } else {
        ical_datetime_property_prefix(original, "DTEND").unwrap_or_else(|| "DTEND".to_string())
    };
    let mut updated = update_ical_property(original, "UID", Some(&escape_ical_text(uid)));
    updated = update_ical_property(&updated, "SUMMARY", Some(&escape_ical_text(title)));
    updated = update_ical_property(
        &updated,
        "LOCATION",
        Some(&escape_ical_text(location.trim())),
    );
    let conference_value =
        (!conference.trim().is_empty()).then(|| escape_ical_text(conference.trim()));
    updated = update_ical_property(&updated, "CONFERENCE", conference_value.as_deref());
    updated = update_ical_property(&updated, "STATUS", Some(status));
    updated = update_ical_property(
        &updated,
        "TRANSP",
        Some(if transparent { "TRANSPARENT" } else { "OPAQUE" }),
    );
    let original_has_date_start = component_property_has_date_value(original, "DTSTART");
    let original_has_date_end = component_property_has_date_value(original, "DTEND");
    if original_has_date_start != all_day {
        updated = update_ical_property(&updated, "DTSTART", None);
    }
    updated = update_ical_property(&updated, &start_property, Some(&start_value));

    if original_has_date_end != all_day {
        updated = update_ical_property(&updated, "DTEND", None);
    }
    updated = update_ical_property(&updated, &end_property, Some(&end_value));

    updated = update_ical_property(
        &updated,
        "DESCRIPTION",
        Some(&escape_ical_text(description.trim())),
    );
    let recurrence_trimmed = recurrence.trim();
    updated = update_ical_property(
        &updated,
        "RRULE",
        (!recurrence_trimmed.is_empty()).then_some(recurrence_trimmed),
    );
    let rdate_prefix = ical_recurrence_property_prefix(original, "RDATE", all_day)
        .unwrap_or_else(|| default_recurrence_property_prefix("RDATE", all_day));
    let rdates_serialized = serialize_recurrence_values(rdates);
    let original_has_date_rdate = component_property_has_date_value(original, "RDATE");
    if original_has_date_rdate != all_day {
        updated = update_ical_property(&updated, "RDATE", None);
    }
    updated = update_ical_property(
        &updated,
        &rdate_prefix,
        (!rdates_serialized.is_empty()).then_some(rdates_serialized.as_str()),
    );
    let exdate_prefix = ical_recurrence_property_prefix(original, "EXDATE", all_day)
        .unwrap_or_else(|| default_recurrence_property_prefix("EXDATE", all_day));
    let exdates_serialized = serialize_recurrence_values(exdates);
    let original_has_date_exdate = component_property_has_date_value(original, "EXDATE");
    if original_has_date_exdate != all_day {
        updated = update_ical_property(&updated, "EXDATE", None);
    }
    updated = update_ical_property(
        &updated,
        &exdate_prefix,
        (!exdates_serialized.is_empty()).then_some(exdates_serialized.as_str()),
    );
    let categories_serialized = serialize_categories(categories);
    updated = update_ical_property(
        &updated,
        "CATEGORIES",
        (!categories_serialized.is_empty()).then_some(categories_serialized.as_str()),
    );
    updated = update_ical_property(
        &updated,
        "CLASS",
        Some(if private { "PRIVATE" } else { "PUBLIC" }),
    );
    let attendees = parse_attendees(attendees);
    updated = replace_ical_attendees(&updated, &attendees);
    let attachments = parse_attachments(attachments);
    updated = replace_ical_attachments(&updated, &attachments);
    updated = replace_component_alarms(&updated, alarms);
    Ok(updated)
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
    for raw_line in raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split_terminator('\n')
    {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.get(1..).unwrap_or_default());
            }
        } else if !raw_line.is_empty() || !lines.is_empty() {
            lines.push(raw_line.to_string());
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
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            match character {
                'n' | 'N' => result.push('\n'),
                ',' | ';' | '\\' => result.push(character),
                other => {
                    result.push('\\');
                    result.push(other);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
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
            &[Alarm::new_display_relative(
                AlarmRelation::Start,
                -15 * 60,
                None,
            )],
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
                &[],
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
                &[],
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
            &[],
            false,
        )
        .expect("event with attachments should serialize");
        assert!(ical_properties(&component, "ATTACH").contains(
            &"https://calendar.example.test/agenda.ics?tags=a,b;kind=agenda".to_string()
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
        let alarms = parse_alarms(raw);
        assert_eq!(alarms.len(), 2);
        assert!(matches!(alarms[0].action, AlarmAction::Email));
        assert_eq!(alarms[1].trigger.offset_seconds(), Some(-15 * 60));

        let removed = replace_component_alarms(raw, &alarms[..1]);
        assert!(removed.contains("ACTION:EMAIL"));
        assert!(removed.contains("TRIGGER:-P1D"));
        assert!(!removed.contains("TRIGGER;RELATED=START:-PT15M"));

        let updated_alarm = Alarm::new_display_relative(
            AlarmRelation::Start,
            -30 * 60,
            Some("Reminder".to_string()),
        );
        let updated = replace_component_alarms(raw, &[alarms[0].clone(), updated_alarm]);
        assert!(updated.contains("ACTION:EMAIL"));
        assert!(updated.contains("TRIGGER:-P1D"));
        assert!(updated.contains("TRIGGER:-PT30M"));
        assert!(!updated.contains("TRIGGER;RELATED=START:-PT15M"));
    }

    #[test]
    fn alarm_editor_model_roundtrips_all_actions_repeat_duration_and_unknowns() {
        let raw = concat!(
            "BEGIN:VEVENT\r\nUID:event-alarms\r\n",
            "X-COMPONENT-STATE:keep-me\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "TRIGGER;RELATED=END;X-TRIGGER=keep:-PT30M\r\n",
            "REPEAT:2\r\nDURATION:PT15M\r\n",
            "DESCRIPTION:Custom\\, message\\;\r\n",
            "X-ALARM-PROPERTY:preserve\r\n",
            "END:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:AUDIO\r\n",
            "TRIGGER;VALUE=DATE-TIME:20261025T013000Z\r\n",
            "ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg\r\n",
            "END:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-P1D\r\n",
            "ATTENDEE;CN=Ops:mailto:ops@example.test\r\nEND:VALARM\r\n",
            "BEGIN:VALARM\r\nACTION:PROCEDURE\r\nTRIGGER:-PT5M\r\n",
            "X-EXECUTABLE:do-not-run\r\nEND:VALARM\r\n",
            "END:VEVENT\r\n"
        );
        let alarms = parse_alarms(raw);
        assert_eq!(alarms.len(), 4);
        assert_eq!(alarms[0].repeat, Some(2));
        assert_eq!(
            alarms[0].duration.as_ref().map(|value| value.seconds),
            Some(15 * 60)
        );
        assert_eq!(alarms[0].message.as_deref(), Some("Custom, message;"));
        assert!(!alarms[1].is_safe_to_notify());
        assert!(!alarms[2].is_safe_to_notify());
        assert!(!alarms[3].is_safe_to_notify());

        let roundtrip = replace_component_alarms(raw, &alarms);
        assert!(roundtrip.contains("X-COMPONENT-STATE:keep-me"));
        assert!(roundtrip.contains("X-ALARM-PROPERTY:preserve"));
        assert!(
            roundtrip.contains("ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg")
        );
        assert!(roundtrip.contains("ATTENDEE;CN=Ops:mailto:ops@example.test"));
        assert_eq!(parse_alarms(&roundtrip), alarms);
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

    #[test]
    fn update_existing_event_preserves_unknown_properties_and_parameters() {
        let raw = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:upstream-event-123\r\n",
            "DTSTAMP:20260101T000000Z\r\n",
            "DTSTART;TZID=Europe/Berlin:20260810T090000\r\n",
            "DTEND;TZID=Europe/Berlin:20260810T100000\r\n",
            "SUMMARY:Initial Strategy Meeting\r\n",
            "ORGANIZER;CN=Alice Boss:mailto:alice@example.com\r\n",
            "GEO:48.85299;2.36885\r\n",
            "URL:https://intranet.example.com/meeting/123\r\n",
            "PRIORITY:1\r\n",
            "CLASS:CONFIDENTIAL\r\n",
            "X-EVOLUTION-MOVE-CALENDAR:1\r\n",
            "X-CUSTOM-FLAG:TRUE\r\n",
            "ATTENDEE;CN=Bob Colleague;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n",
            "ATTENDEE;CN=Charlie Remote;ROLE=OPT-PARTICIPANT:mailto:charlie@example.com\r\n",
            "RDATE;TZID=Europe/Berlin:20260810T090000\r\n",
            "ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://example.com/spec.pdf\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:AUDIO\r\n",
            "TRIGGER;VALUE=DATE-TIME:20260810T083000Z\r\n",
            "ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg\r\n",
            "END:VALARM\r\n",
            "END:VEVENT\r\n"
        );

        let mut alarms = parse_alarms(raw);
        alarms.push(Alarm::new_display_relative(
            AlarmRelation::Start,
            -15 * 60,
            Some("Meeting starts in 15m".to_string()),
        ));

        let updated = update_existing_event_component(
            raw,
            "upstream-event-123",
            "Updated Strategy Meeting",
            "Room 101",
            "https://meet.example.com/abc",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 10:00",
            "2026-08-10 11:30",
            "Discuss Q3 goals and roadmap.",
            "FREQ=WEEKLY;COUNT=5",
            "20260817T100000,20260824T100000",
            "20260831T100000",
            "mailto:bob@example.com, mailto:dana@example.com",
            r"Work,Strategy\, Planning",
            "https://example.com/spec.pdf\nhttps://example.com/agenda.pdf",
            &alarms,
            true,
        )
        .expect("update_existing_event_component should succeed");

        let unfolded = unfold_ical_lines(&updated);

        // Verify edited fields
        assert!(unfolded.contains(&"SUMMARY:Updated Strategy Meeting".to_string()));
        assert!(unfolded.contains(&"LOCATION:Room 101".to_string()));
        assert!(unfolded.contains(&"CONFERENCE:https://meet.example.com/abc".to_string()));
        assert!(unfolded.contains(&"STATUS:CONFIRMED".to_string()));
        assert!(unfolded.contains(&"TRANSP:OPAQUE".to_string()));
        assert!(unfolded.contains(&"DTSTART;TZID=Europe/Berlin:20260810T100000".to_string()));
        assert!(unfolded.contains(&"DTEND;TZID=Europe/Berlin:20260810T113000".to_string()));
        assert!(unfolded.contains(&"DESCRIPTION:Discuss Q3 goals and roadmap.".to_string()));
        assert!(unfolded.contains(&"RRULE:FREQ=WEEKLY;COUNT=5".to_string()));
        assert!(
            unfolded
                .contains(&"RDATE;TZID=Europe/Berlin:20260817T100000,20260824T100000".to_string())
        );
        assert!(unfolded.contains(&"EXDATE:20260831T100000".to_string()));
        assert!(unfolded.contains(&r"CATEGORIES:Work,Strategy\, Planning".to_string()));
        assert!(unfolded.contains(&"CLASS:PRIVATE".to_string()));

        // Verify preserved unknown/vendor properties
        assert!(unfolded.contains(&"ORGANIZER;CN=Alice Boss:mailto:alice@example.com".to_string()));
        assert!(unfolded.contains(&"GEO:48.85299;2.36885".to_string()));
        assert!(unfolded.contains(&"URL:https://intranet.example.com/meeting/123".to_string()));
        assert!(unfolded.contains(&"PRIORITY:1".to_string()));
        assert!(unfolded.contains(&"X-EVOLUTION-MOVE-CALENDAR:1".to_string()));
        assert!(unfolded.contains(&"X-CUSTOM-FLAG:TRUE".to_string()));

        // Verify preserved attendee parameters and added/removed attendees
        assert!(unfolded.contains(
            &"ATTENDEE;CN=Bob Colleague;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.com".to_string()
        ));
        assert!(unfolded.contains(&"ATTENDEE:mailto:dana@example.com".to_string()));
        assert!(!updated.contains("charlie@example.com"));

        // Verify preserved attachment FMTTYPE and newly added attachment
        assert!(unfolded.contains(
            &"ATTACH;FMTTYPE=application/pdf;VALUE=URI:https://example.com/spec.pdf".to_string()
        ));
        assert!(unfolded.contains(&"ATTACH;VALUE=URI:https://example.com/agenda.pdf".to_string()));

        // Verify alarms: audio alarm from raw preserved plus newly added display alarm
        assert!(unfolded.contains(&"ACTION:AUDIO".to_string()));
        assert!(unfolded.contains(
            &"ATTACH;FMTTYPE=audio/ogg;VALUE=URI:https://example.test/alarm.ogg".to_string()
        ));
        assert!(unfolded.contains(&"ACTION:DISPLAY".to_string()));
        assert!(unfolded.contains(&"TRIGGER:-PT15M".to_string()));
    }

    #[test]
    fn update_existing_event_validates_inputs() {
        let raw = "BEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20260810T090000\r\nDTEND:20260810T100000\r\nSUMMARY:Test\r\nEND:VEVENT\r\n";

        // Empty title
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "   ",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 10:00",
                "2026-08-10 11:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false
            )
            .is_err()
        );

        // End before start
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "Meeting",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 12:00",
                "2026-08-10 11:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false
            )
            .is_err()
        );

        // Invalid status
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "Meeting",
                "",
                "",
                "INVALID_STATUS",
                false,
                false,
                "2026-08-10 10:00",
                "2026-08-10 11:00",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                &[],
                false
            )
            .is_err()
        );

        // Newline injection in recurrence
        assert!(
            update_existing_event_component(
                raw,
                "event-1",
                "Meeting",
                "",
                "",
                "CONFIRMED",
                false,
                false,
                "2026-08-10 10:00",
                "2026-08-10 11:00",
                "",
                "FREQ=DAILY\r\nX-INJECTED:bad",
                "",
                "",
                "",
                "",
                "",
                &[],
                false
            )
            .is_err()
        );
    }

    #[test]
    fn update_existing_event_switches_all_day_mode() {
        let timed = "BEGIN:VEVENT\r\nUID:event-timed\r\nDTSTART:20260810T090000\r\nDTEND:20260810T100000\r\nSUMMARY:Timed\r\nEND:VEVENT\r\n";

        // Switch timed -> all-day
        let all_day = update_existing_event_component(
            timed,
            "event-timed",
            "All-Day Event",
            "",
            "",
            "CONFIRMED",
            false,
            true,
            "2026-08-10",
            "2026-08-11",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
        )
        .expect("should convert to all-day");

        assert!(all_day.contains("DTSTART;VALUE=DATE:20260810"));
        assert!(all_day.contains("DTEND;VALUE=DATE:20260811"));
        assert!(!all_day.contains("DTSTART:20260810\r\n"));

        // Switch all-day -> timed
        let back_to_timed = update_existing_event_component(
            &all_day,
            "event-timed",
            "Back to Timed",
            "",
            "",
            "CONFIRMED",
            false,
            false,
            "2026-08-10 14:00",
            "2026-08-10 15:00",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
        )
        .expect("should convert to timed");

        assert!(back_to_timed.contains("DTSTART:20260810T140000"));
        assert!(back_to_timed.contains("DTEND:20260810T150000"));
        assert!(!back_to_timed.contains("VALUE=DATE"));
    }
}
