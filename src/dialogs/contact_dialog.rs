// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactDialog — Dialog for creating and editing address book contacts.
//
// Features:
// - Full Name, Organization, Title
// - Multiple Email addresses (Personal, Work)
// - Phone numbers (Mobile, Work, Home)
// - Postal Address & Notes

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::contact_store_service::ContactStoreService;
use crate::services::ebook_service::EBookContactRecord;
use crate::services::ebook_service::EBookService;
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;
use crate::services::vcard_service::VCardContact;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactDialog {
        pub edit_vcard: RefCell<Option<String>>,
        pub edit_uid: RefCell<Option<String>>,
        pub edit_source_uid: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContactDialog {
        const NAME: &'static str = "EvolutionContactDialog";
        type Type = super::ContactDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for ContactDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("edit-vcard").build(),
                    glib::ParamSpecString::builder("edit-uid").build(),
                    glib::ParamSpecString::builder("edit-source-uid").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "edit-vcard" => *self.edit_vcard.borrow_mut() = value.get().expect("string"),
                "edit-uid" => *self.edit_uid.borrow_mut() = value.get().expect("string"),
                "edit-source-uid" => {
                    *self.edit_source_uid.borrow_mut() = value.get().expect("string")
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-vcard" => self.edit_vcard.borrow().to_value(),
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

    impl WidgetImpl for ContactDialog {}
    impl WindowImpl for ContactDialog {}
    impl AdwWindowImpl for ContactDialog {}
}

glib::wrapper! {
    pub struct ContactDialog(ObjectSubclass<imp::ContactDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl ContactDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Neuer Kontakt")
            .property("modal", true)
            .property("default-width", 560)
            .property("default-height", 560)
            .build()
    }

    pub(crate) fn new_for_edit(record: &EBookContactRecord) -> Self {
        glib::Object::builder()
            .property("title", "Kontakt bearbeiten")
            .property("modal", true)
            .property("default-width", 560)
            .property("default-height", 600)
            .property("edit-vcard", &record.vcard)
            .property("edit-uid", &record.uid)
            .property("edit-source-uid", &record.source_uid)
            .build()
    }

    fn build_ui(&self) {
        let edit_vcard = self.imp().edit_vcard.borrow().clone();
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

        // Header Avatar & Name
        let name_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(16)
            .halign(gtk::Align::Center)
            .margin_bottom(8)
            .build();

        let avatar = adw::Avatar::builder()
            .size(64)
            .text("Neuer Kontakt")
            .show_initials(true)
            .build();
        name_box.append(&avatar);

        content_box.append(&name_box);

        // Group 1: General Info
        let group_general = adw::PreferencesGroup::builder()
            .title("Persönliche Angaben")
            .build();

        let row_name = adw::EntryRow::builder().title("Vollständiger Name").build();
        group_general.add(&row_name);

        let row_family_name = adw::EntryRow::builder().title("Nachname (N)").build();
        group_general.add(&row_family_name);

        let row_given_name = adw::EntryRow::builder().title("Vorname (N)").build();
        group_general.add(&row_given_name);

        let row_additional_name = adw::EntryRow::builder()
            .title("Weitere Vornamen (N)")
            .build();
        group_general.add(&row_additional_name);

        let row_name_prefix = adw::EntryRow::builder().title("Namenspräfix (N)").build();
        group_general.add(&row_name_prefix);

        let row_name_suffix = adw::EntryRow::builder().title("Namenssuffix (N)").build();
        group_general.add(&row_name_suffix);

        let row_org = adw::EntryRow::builder()
            .title("Firma / Organisation")
            .build();
        group_general.add(&row_org);

        let row_title = adw::EntryRow::builder().title("Position / Titel").build();
        group_general.add(&row_title);

        let source_model = gtk::StringList::new(&["Adressbücher werden geladen…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Adressbuch")
            .model(&source_model)
            .build();
        group_general.add(&source_row);
        let settings_for_default_address_book = AppSettings::try_new();
        let source_uids_for_selection = Rc::clone(&source_uids);
        source_row.connect_selected_notify(move |row| {
            let Some(settings) = settings_for_default_address_book.as_ref() else {
                return;
            };
            let Some(source_uid) = source_uids_for_selection
                .borrow()
                .get(row.selected() as usize)
                .cloned()
            else {
                return;
            };
            settings.set_default_address_book_source_uid(&source_uid);
        });

        content_box.append(&group_general);

        // Group 2: Contact Methods
        let group_contact = adw::PreferencesGroup::builder()
            .title("Kontaktdaten")
            .build();

        let row_email1 = adw::EntryRow::builder().title("E-Mail (Privat)").build();
        group_contact.add(&row_email1);

        let row_email2 = adw::EntryRow::builder().title("E-Mail (Arbeit)").build();
        group_contact.add(&row_email2);

        let row_email3 = adw::EntryRow::builder().title("E-Mail (Weitere)").build();
        group_contact.add(&row_email3);

        let row_phone = adw::EntryRow::builder().title("Telefon (Mobil)").build();
        group_contact.add(&row_phone);

        let row_phone_work = adw::EntryRow::builder().title("Telefon (Arbeit)").build();
        group_contact.add(&row_phone_work);

        let row_phone_home = adw::EntryRow::builder().title("Telefon (Privat)").build();
        group_contact.add(&row_phone_home);

        content_box.append(&group_contact);

        // Group 3: Address & Notes
        let group_extra = adw::PreferencesGroup::builder()
            .title("Adresse & Notizen")
            .build();

        let row_address = adw::EntryRow::builder().title("Postadresse").build();
        group_extra.add(&row_address);

        let row_note = adw::EntryRow::builder().title("Notiz").build();
        group_extra.add(&row_note);

        let row_url = adw::EntryRow::builder().title("Website").build();
        group_extra.add(&row_url);

        let row_nickname = adw::EntryRow::builder().title("Spitzname").build();
        group_extra.add(&row_nickname);

        let row_birthday = adw::EntryRow::builder()
            .title("Geburtstag (YYYY-MM-DD)")
            .build();
        group_extra.add(&row_birthday);

        let row_categories = adw::EntryRow::builder()
            .title("Kategorien (durch Komma getrennt)")
            .build();
        group_extra.add(&row_categories);

        let row_anniversary = adw::EntryRow::builder()
            .title("Jahrestag (YYYY-MM-DD)")
            .build();
        group_extra.add(&row_anniversary);

        let row_role = adw::EntryRow::builder().title("Rolle").build();
        group_extra.add(&row_role);

        let row_gender = adw::EntryRow::builder().title("Geschlecht").build();
        group_extra.add(&row_gender);

        let row_timezone = adw::EntryRow::builder().title("Zeitzone").build();
        group_extra.add(&row_timezone);

        let row_geo = adw::EntryRow::builder()
            .title("Geoposition (Breite;Länge)")
            .build();
        group_extra.add(&row_geo);

        let row_impp = adw::EntryRow::builder()
            .title("Chat/IMPP (Komma getrennt)")
            .build();
        group_extra.add(&row_impp);

        let row_related = adw::EntryRow::builder()
            .title("Verwandte Kontakte (Komma getrennt)")
            .build();
        group_extra.add(&row_related);

        let row_photo = adw::EntryRow::builder()
            .title("Foto (URI oder Base64)")
            .build();
        row_photo.set_input_purpose(gtk::InputPurpose::Url);
        group_extra.add(&row_photo);

        // Evolution-specific address-book properties. They are kept in the
        // vCard even when a provider does not expose them as first-class
        // fields, and can be edited explicitly on the portable path.
        let group_evolution = adw::PreferencesGroup::builder()
            .title("Evolution-Felder")
            .description("Zusätzliche Eigenschaften des Evolution-Adressbuchs")
            .build();
        let row_evolution_file_as = adw::EntryRow::builder()
            .title("Sortier-/Anzeigename")
            .build();
        group_evolution.add(&row_evolution_file_as);
        let row_evolution_manager = adw::EntryRow::builder().title("Vorgesetzter").build();
        group_evolution.add(&row_evolution_manager);
        let row_evolution_assistant = adw::EntryRow::builder().title("Assistenz").build();
        group_evolution.add(&row_evolution_assistant);
        let row_evolution_spouse = adw::EntryRow::builder().title("Partner/in").build();
        group_evolution.add(&row_evolution_spouse);
        let row_evolution_blog_url = adw::EntryRow::builder().title("Blog-URL").build();
        row_evolution_blog_url.set_input_purpose(gtk::InputPurpose::Url);
        group_evolution.add(&row_evolution_blog_url);
        let row_evolution_video_url = adw::EntryRow::builder().title("Video-URL").build();
        row_evolution_video_url.set_input_purpose(gtk::InputPurpose::Url);
        group_evolution.add(&row_evolution_video_url);
        let row_evolution_wants_html = adw::SwitchRow::builder()
            .title("HTML-Mail an diesen Kontakt")
            .subtitle("Evolution-Präferenz X-EVOLUTION-WANTS-HTML")
            .build();
        group_evolution.add(&row_evolution_wants_html);
        let row_evolution_list = adw::SwitchRow::builder()
            .title("Mailingliste")
            .subtitle("Kontakt als Liste behandeln")
            .build();
        group_evolution.add(&row_evolution_list);
        let row_evolution_list_name = adw::EntryRow::builder().title("Listenname").build();
        group_evolution.add(&row_evolution_list_name);
        let row_evolution_list_show_addresses = adw::SwitchRow::builder()
            .title("Listenadressen anzeigen")
            .build();
        group_evolution.add(&row_evolution_list_show_addresses);

        if let Some(vcard) = edit_vcard.as_deref() {
            if let Some(contact) = VCardContact::parse_vcard_string(vcard) {
                row_name.set_text(&contact.fn_name);
                row_family_name.set_text(&contact.name.family);
                row_given_name.set_text(&contact.name.given);
                row_additional_name.set_text(&contact.name.additional);
                row_name_prefix.set_text(&contact.name.prefix);
                row_name_suffix.set_text(&contact.name.suffix);
                row_org.set_text(&contact.org);
                row_title.set_text(&contact.title);
                row_email1.set_text(&contact.email);
                row_phone.set_text(&contact.tel);
                row_note.set_text(&contact.note);
                if let Some(email) = contact.emails.get(2) {
                    row_email3.set_text(email);
                }
                if let Some(phone) = contact.phones.get(1) {
                    row_phone_work.set_text(phone);
                }
                if let Some(phone) = contact.phones.get(2) {
                    row_phone_home.set_text(phone);
                }
                row_url.set_text(contact.urls.first().map(String::as_str).unwrap_or_default());
                row_nickname.set_text(&contact.nickname);
                row_birthday.set_text(&contact.birthday);
                row_categories.set_text(&contact.categories.join(", "));
                row_anniversary.set_text(&contact.anniversary);
                row_role.set_text(&contact.role);
                row_gender.set_text(&contact.gender);
                row_timezone.set_text(&contact.timezone);
                row_geo.set_text(&contact.geo);
                row_impp.set_text(&contact.impps.join(", "));
                row_related.set_text(&contact.related.join(", "));
                if let Some(photo) = contact.photo.as_ref() {
                    if photo
                        .encoding
                        .as_deref()
                        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("b"))
                    {
                        row_photo
                            .set_text(&format!("data:{};base64,{}", photo.media_type, photo.value));
                    } else {
                        row_photo.set_text(&photo.value);
                    }
                }
                row_evolution_file_as.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-FILE-AS")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_manager.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-MANAGER")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_assistant.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-ASSISTANT")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_spouse.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-SPOUSE")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_blog_url.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-BLOG-URL")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_video_url.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-VIDEO-URL")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_wants_html.set_active(
                    contact
                        .extension_values("X-EVOLUTION-WANTS-HTML")
                        .first()
                        .is_some_and(|value| parse_vcard_bool(value)),
                );
                row_evolution_list.set_active(
                    contact
                        .extension_values("X-EVOLUTION-LIST")
                        .first()
                        .is_some_and(|value| parse_vcard_bool(value)),
                );
                row_evolution_list_name.set_text(
                    &contact
                        .extension_values("X-EVOLUTION-LIST-NAME")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
                row_evolution_list_show_addresses.set_active(
                    contact
                        .extension_values("X-EVOLUTION-LIST-SHOW-ADDRESSES")
                        .first()
                        .is_some_and(|value| parse_vcard_bool(value)),
                );
            }
            if let Some(email) = vcard_property(vcard, "EMAIL", 1) {
                row_email2.set_text(&email);
            }
            if let Some(address) = vcard_property(vcard, "ADR", 0) {
                row_address.set_text(&address.replace(';', ", "));
            }
        }

        content_box.append(&group_extra);
        content_box.append(&group_evolution);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let edit_source_uid_for_load = edit_source_uid.clone();
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_address_book_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        let local_addressbook_available = ContactStoreService::new().is_writable();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::AddressBook).await;
            let mut sources = discovery.sources;
            sources.retain(|source| source.enabled);
            sources.sort_by_cached_key(|source| {
                (source.display_name.to_lowercase(), source.uid.clone())
            });

            let mut names = Vec::new();
            let mut uids = Vec::new();
            for source in sources {
                names.push(if source.backend_name.trim().is_empty() {
                    source.display_name
                } else {
                    format!("{} · {}", source.display_name, source.backend_name)
                });
                uids.push(source.uid);
            }

            let preferred_uid = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid.as_deref());
            if let Some(uid) = preferred_uid.filter(|uid| {
                !uid.trim().is_empty() && !uids.iter().any(|candidate| candidate == *uid)
            }) {
                names.push(format!("Nicht verfügbare Quelle · {uid}"));
                uids.push(uid.to_string());
            }
            if local_addressbook_available {
                names.push("Lokales Adressbuch".to_string());
                uids.push("local-addressbook".to_string());
            }
            if names.is_empty() {
                names.push(
                    discovery
                        .error
                        .map(|error| format!("Adressbücher sind nicht erreichbar: {error}"))
                        .unwrap_or_else(|| "Kein aktives Adressbuch verfügbar".to_string()),
                );
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            source_uids_for_load.replace(uids.clone());
            if let Some(uid) = preferred_uid {
                if let Some(index) = uids.iter().position(|candidate| candidate == uid) {
                    source_row_for_load.set_selected(index as u32);
                }
            }
        });

        let source_uids_for_save = Rc::clone(&source_uids);
        let edit_uid_for_save = edit_uid.clone();
        let edit_vcard_for_save = edit_vcard.clone();
        let dialog_weak = self.downgrade();
        let save_button_for_signal = btn_save.clone();
        let save_button_for_closure = btn_save.clone();
        save_button_for_signal.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let selected = source_row.selected() as usize;
            let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() else {
                dialog.show_toast("Kein aktives Adressbuch ist ausgewählt");
                return;
            };
            let extended_fields = ContactVCardFields {
                family_name: row_family_name.text().to_string(),
                given_name: row_given_name.text().to_string(),
                additional_name: row_additional_name.text().to_string(),
                name_prefix: row_name_prefix.text().to_string(),
                name_suffix: row_name_suffix.text().to_string(),
                anniversary: row_anniversary.text().to_string(),
                role: row_role.text().to_string(),
                gender: row_gender.text().to_string(),
                timezone: row_timezone.text().to_string(),
                geo: row_geo.text().to_string(),
                impps: row_impp.text().to_string(),
                related: row_related.text().to_string(),
                photo: row_photo.text().to_string(),
                evolution_file_as: row_evolution_file_as.text().to_string(),
                evolution_manager: row_evolution_manager.text().to_string(),
                evolution_assistant: row_evolution_assistant.text().to_string(),
                evolution_spouse: row_evolution_spouse.text().to_string(),
                evolution_blog_url: row_evolution_blog_url.text().to_string(),
                evolution_video_url: row_evolution_video_url.text().to_string(),
                evolution_wants_html: row_evolution_wants_html.is_active(),
                evolution_list: row_evolution_list.is_active(),
                evolution_list_name: row_evolution_list_name.text().to_string(),
                evolution_list_show_addresses: row_evolution_list_show_addresses.is_active(),
            };
            let mut vcard = match build_vcard_with_fields(
                &row_name.text(),
                &row_org.text(),
                &row_title.text(),
                &row_email1.text(),
                &row_email2.text(),
                &row_email3.text(),
                &row_phone.text(),
                &row_phone_work.text(),
                &row_phone_home.text(),
                &row_address.text(),
                &row_note.text(),
                &row_url.text(),
                &row_nickname.text(),
                &row_birthday.text(),
                &row_categories.text(),
                &extended_fields,
                edit_uid_for_save.as_deref(),
            ) {
                Ok(vcard) => vcard,
                Err(error) => {
                    dialog.show_toast(&error);
                    return;
                }
            };
            if let Some(original) = edit_vcard_for_save.as_deref() {
                vcard = preserve_vcard_extensions(&vcard, original);
            }
            let source_uid_for_worker = source_uid.clone();
            let operation_uid = edit_uid_for_save.clone();
            let vcard_for_worker = vcard.clone();
            let was_edit = edit_vcard_for_save.is_some();
            let (sender, receiver) = async_channel::bounded(1);
            save_button_for_closure.set_sensitive(false);
            std::thread::spawn(move || {
                let service = EBookService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => {
                        service.modify_contact(&source_uid_for_worker, uid, &vcard_for_worker)
                    }
                    None => service.create_contact(&source_uid_for_worker, &vcard_for_worker),
                };
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Contact mutation result was dropped");
                }
            });
            let save_button_for_result = save_button_for_closure.clone();
            let dialog_weak_for_result = dialog.downgrade();
            glib::spawn_future_local(async move {
                let Ok(result) = receiver.recv().await else {
                    save_button_for_result.set_sensitive(true);
                    return;
                };
                match result {
                    Ok(result) => {
                        let message = if was_edit {
                            format!("Kontakt aktualisiert ({})", result.uid)
                        } else {
                            format!("Kontakt gespeichert ({})", result.uid)
                        };
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&message);
                            dialog.close();
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&format!(
                                "Kontakt konnte nicht gespeichert werden: {error}"
                            ));
                        }
                    }
                }
                save_button_for_result.set_sensitive(true);
            });
        });
    }

    fn show_toast(&self, message: &str) {
        let Some(parent) = self.transient_for() else {
            tracing::warn!(%message, "EBook dialog result");
            return;
        };
        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
        } else {
            tracing::warn!(%message, "EBook dialog result");
        }
    }
}

fn vcard_property(vcard: &str, name: &str, occurrence: usize) -> Option<String> {
    vcard
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter_map(|line| {
            let (property, value) = line.split_once(':')?;
            let property_name = property.split(';').next().unwrap_or(property);
            property_name.eq_ignore_ascii_case(name).then(|| {
                value
                    .replace("\\n", "\n")
                    .replace("\\N", "\n")
                    .replace("\\,", ",")
                    .replace("\\;", ";")
                    .replace("\\\\", "\\")
            })
        })
        .nth(occurrence)
}

fn parse_vcard_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "1" | "on"
    )
}

#[derive(Debug, Default)]
struct ContactVCardFields {
    family_name: String,
    given_name: String,
    additional_name: String,
    name_prefix: String,
    name_suffix: String,
    anniversary: String,
    role: String,
    gender: String,
    timezone: String,
    geo: String,
    impps: String,
    related: String,
    photo: String,
    evolution_file_as: String,
    evolution_manager: String,
    evolution_assistant: String,
    evolution_spouse: String,
    evolution_blog_url: String,
    evolution_video_url: String,
    evolution_wants_html: bool,
    evolution_list: bool,
    evolution_list_name: String,
    evolution_list_show_addresses: bool,
}

#[cfg(test)]
fn build_vcard(
    name: &str,
    organization: &str,
    title: &str,
    email: &str,
    email_work: &str,
    email_other: &str,
    phone: &str,
    phone_work: &str,
    phone_home: &str,
    address: &str,
    note: &str,
    url: &str,
    nickname: &str,
    birthday: &str,
    categories: &str,
    uid: Option<&str>,
) -> Result<String, String> {
    build_vcard_with_fields(
        name,
        organization,
        title,
        email,
        email_work,
        email_other,
        phone,
        phone_work,
        phone_home,
        address,
        note,
        url,
        nickname,
        birthday,
        categories,
        &ContactVCardFields::default(),
        uid,
    )
}

fn build_vcard_with_fields(
    name: &str,
    organization: &str,
    title: &str,
    email: &str,
    email_work: &str,
    email_other: &str,
    phone: &str,
    phone_work: &str,
    phone_home: &str,
    address: &str,
    note: &str,
    url: &str,
    nickname: &str,
    birthday: &str,
    categories: &str,
    fields: &ContactVCardFields,
    uid: Option<&str>,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Ein Kontakt benötigt einen Namen".to_string());
    }
    let uid = uid
        .filter(|uid| !uid.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("evolution-gtk4-{}", uuid::Uuid::new_v4()));
    let mut lines = vec![
        "BEGIN:VCARD".to_string(),
        "VERSION:3.0".to_string(),
        format!("UID:{uid}"),
        format!("FN:{}", escape_vcard_text(name)),
        format!(
            "N:{};{};{};{};{}",
            escape_vcard_text(if fields.family_name.trim().is_empty() {
                name
            } else {
                fields.family_name.trim()
            }),
            escape_vcard_text(fields.given_name.trim()),
            escape_vcard_text(fields.additional_name.trim()),
            escape_vcard_text(fields.name_prefix.trim()),
            escape_vcard_text(fields.name_suffix.trim())
        ),
    ];
    if !organization.trim().is_empty() {
        lines.push(format!("ORG:{}", escape_vcard_text(organization.trim())));
    }
    if !title.trim().is_empty() {
        lines.push(format!("TITLE:{}", escape_vcard_text(title.trim())));
    }
    if !email.trim().is_empty() {
        lines.push(format!(
            "EMAIL;TYPE=INTERNET:{}",
            escape_vcard_text(email.trim())
        ));
    }
    if !email_work.trim().is_empty() {
        lines.push(format!(
            "EMAIL;TYPE=WORK:{}",
            escape_vcard_text(email_work.trim())
        ));
    }
    if !email_other.trim().is_empty() {
        lines.push(format!(
            "EMAIL;TYPE=OTHER:{}",
            escape_vcard_text(email_other.trim())
        ));
    }
    if !phone.trim().is_empty() {
        lines.push(format!("TEL;TYPE=CELL:{}", escape_vcard_text(phone.trim())));
    }
    if !phone_work.trim().is_empty() {
        lines.push(format!(
            "TEL;TYPE=WORK,VOICE:{}",
            escape_vcard_text(phone_work.trim())
        ));
    }
    if !phone_home.trim().is_empty() {
        lines.push(format!(
            "TEL;TYPE=HOME,VOICE:{}",
            escape_vcard_text(phone_home.trim())
        ));
    }
    if !address.trim().is_empty() {
        lines.push(format!(
            "ADR;TYPE=HOME:;;{};;;;",
            escape_vcard_text(address.trim())
        ));
    }
    if !note.trim().is_empty() {
        lines.push(format!("NOTE:{}", escape_vcard_text(note.trim())));
    }
    if !url.trim().is_empty() {
        lines.push(format!("URL:{}", escape_vcard_text(url.trim())));
    }
    if !nickname.trim().is_empty() {
        lines.push(format!("NICKNAME:{}", escape_vcard_text(nickname.trim())));
    }
    if !birthday.trim().is_empty() {
        lines.push(format!("BDAY:{}", escape_vcard_text(birthday.trim())));
    }
    if !categories.trim().is_empty() {
        let categories = categories
            .split(',')
            .map(str::trim)
            .filter(|category| !category.is_empty())
            .map(escape_vcard_text)
            .collect::<Vec<_>>()
            .join(",");
        if !categories.is_empty() {
            lines.push(format!("CATEGORIES:{categories}"));
        }
    }
    append_optional_vcard_value(&mut lines, "ANNIVERSARY", &fields.anniversary);
    append_optional_vcard_value(&mut lines, "ROLE", &fields.role);
    append_optional_vcard_value(&mut lines, "GENDER", &fields.gender);
    append_optional_vcard_value(&mut lines, "TZ", &fields.timezone);
    append_optional_vcard_value(&mut lines, "GEO", &fields.geo);
    append_comma_separated_vcard_values(&mut lines, "IMPP", &fields.impps);
    append_comma_separated_vcard_values(&mut lines, "RELATED", &fields.related);
    append_photo_value(&mut lines, &fields.photo);
    append_optional_vcard_value(&mut lines, "X-EVOLUTION-FILE-AS", &fields.evolution_file_as);
    append_optional_vcard_value(&mut lines, "X-EVOLUTION-MANAGER", &fields.evolution_manager);
    append_optional_vcard_value(
        &mut lines,
        "X-EVOLUTION-ASSISTANT",
        &fields.evolution_assistant,
    );
    append_optional_vcard_value(&mut lines, "X-EVOLUTION-SPOUSE", &fields.evolution_spouse);
    append_optional_vcard_value(
        &mut lines,
        "X-EVOLUTION-BLOG-URL",
        &fields.evolution_blog_url,
    );
    append_optional_vcard_value(
        &mut lines,
        "X-EVOLUTION-VIDEO-URL",
        &fields.evolution_video_url,
    );
    append_vcard_bool(
        &mut lines,
        "X-EVOLUTION-WANTS-HTML",
        fields.evolution_wants_html,
    );
    append_vcard_bool(&mut lines, "X-EVOLUTION-LIST", fields.evolution_list);
    append_optional_vcard_value(
        &mut lines,
        "X-EVOLUTION-LIST-NAME",
        &fields.evolution_list_name,
    );
    append_vcard_bool(
        &mut lines,
        "X-EVOLUTION-LIST-SHOW-ADDRESSES",
        fields.evolution_list_show_addresses,
    );
    lines.push("END:VCARD".to_string());
    Ok(format!("{}\r\n", lines.join("\r\n")))
}

fn append_optional_vcard_value(lines: &mut Vec<String>, property: &str, value: &str) {
    if !value.trim().is_empty() {
        lines.push(format!("{property}:{}", escape_vcard_text(value.trim())));
    }
}

fn append_vcard_bool(lines: &mut Vec<String>, property: &str, value: bool) {
    lines.push(format!(
        "{property}:{}",
        if value { "TRUE" } else { "FALSE" }
    ));
}

fn append_comma_separated_vcard_values(lines: &mut Vec<String>, property: &str, values: &str) {
    for value in values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        append_optional_vcard_value(lines, property, value);
    }
}

fn append_photo_value(lines: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if value.starts_with("data:") || value.starts_with("http://") || value.starts_with("https://") {
        lines.push(format!("PHOTO:{value}"));
    } else {
        let compact = value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        lines.push(format!("PHOTO;ENCODING=b;TYPE=JPEG:{compact}"));
    }
}

/// Preserve vCard properties that the compact editor does not expose yet.
/// This keeps photos, categories, URLs, custom fields, and additional contact
/// methods intact when an existing EDS contact is edited.
fn preserve_vcard_extensions(generated: &str, original: &str) -> String {
    let mut extras = Vec::new();
    let mut email_count = 0;
    let mut phone_count = 0;
    let mut address_count = 0;
    let mut photo_count = 0;
    let mut impp_count = 0;
    let mut related_count = 0;
    for line in unfold_vcard_lines(original) {
        let Some((property, _)) = line.split_once(':') else {
            continue;
        };
        let name = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        let preserve = match name.as_str() {
            "EMAIL" => {
                email_count += 1;
                email_count > 3
            }
            "TEL" => {
                phone_count += 1;
                phone_count > 3
            }
            "ADR" => {
                address_count += 1;
                address_count > 1
            }
            "PHOTO" => {
                photo_count += 1;
                photo_count >= 1
            }
            "IMPP" => {
                impp_count += 1;
                impp_count > 1
            }
            "RELATED" => {
                related_count += 1;
                related_count > 1
            }
            "BEGIN"
            | "END"
            | "VERSION"
            | "UID"
            | "FN"
            | "N"
            | "ORG"
            | "TITLE"
            | "NOTE"
            | "URL"
            | "NICKNAME"
            | "BDAY"
            | "CATEGORIES"
            | "ANNIVERSARY"
            | "ROLE"
            | "GENDER"
            | "TZ"
            | "GEO"
            | "X-EVOLUTION-FILE-AS"
            | "X-EVOLUTION-MANAGER"
            | "X-EVOLUTION-ASSISTANT"
            | "X-EVOLUTION-SPOUSE"
            | "X-EVOLUTION-BLOG-URL"
            | "X-EVOLUTION-VIDEO-URL"
            | "X-EVOLUTION-WANTS-HTML"
            | "X-EVOLUTION-LIST"
            | "X-EVOLUTION-LIST-NAME"
            | "X-EVOLUTION-LIST-SHOW-ADDRESSES" => false,
            _ => true,
        };
        if preserve {
            extras.push(line);
        }
    }
    if extras.is_empty() {
        return generated.to_string();
    }

    let mut lines = generated
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter(|line| !line.eq_ignore_ascii_case("END:VCARD"))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    lines.extend(extras);
    lines.push("END:VCARD".to_string());
    format!("{}\r\n", lines.join("\r\n"))
}

fn unfold_vcard_lines(raw: &str) -> Vec<String> {
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

fn escape_vcard_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

impl Default for ContactDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_vcard_requires_a_name() {
        let result = build_vcard(
            "  ", "", "", "", "", "", "", "", "", "", "", "", "", "", "", None,
        );
        assert_eq!(result, Err("Ein Kontakt benötigt einen Namen".to_string()));
    }

    #[test]
    fn contact_vcard_escapes_fields_and_roundtrips() {
        let vcard = build_vcard(
            "Ada, Lovelace",
            "Analytical; Engines",
            "Mathematician",
            "ada@example.com",
            "ada.work@example.com",
            "",
            "+44, 123",
            "",
            "",
            "London; UK",
            "Line one\nLine two",
            "",
            "",
            "",
            "",
            Some("contact-1"),
        )
        .expect("contact vCard should serialize");
        assert!(vcard.contains("UID:contact-1"));
        assert!(vcard.contains("FN:Ada\\, Lovelace"));
        assert!(vcard.contains("ORG:Analytical\\; Engines"));
        let parsed = VCardContact::parse_vcard_string(&vcard).expect("vCard should parse");
        assert_eq!(parsed.fn_name, "Ada, Lovelace");
        assert_eq!(parsed.org, "Analytical; Engines");
        assert_eq!(parsed.note, "Line one\nLine two");
    }

    #[test]
    fn editing_preserves_unexposed_vcard_properties() {
        let generated = build_vcard(
            "Ada Lovelace",
            "Analytical Engines",
            "Mathematician",
            "ada@example.com",
            "ada.work@example.com",
            "ada.other@example.com",
            "+44 123",
            "",
            "",
            "London",
            "Note",
            "",
            "",
            "",
            "Friends,Important",
            Some("contact-1"),
        )
        .expect("contact vCard should serialize");
        let original = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact-1\r\n",
            "FN:Ada Lovelace\r\nPHOTO;ENCODING=b:YWJj\r\n",
            "EMAIL:ada@example.com\r\nEMAIL;TYPE=WORK:ada.work@example.com\r\n",
            "EMAIL;TYPE=OTHER:ada.other@example.com\r\n",
            "EMAIL;TYPE=HOME:ada.legacy@example.com\r\n",
            "CATEGORIES:Friends,Important\r\nEND:VCARD\r\n"
        );
        let merged = preserve_vcard_extensions(&generated, original);
        assert!(merged.contains("PHOTO;ENCODING=b:YWJj"));
        assert!(merged.contains("EMAIL;TYPE=OTHER:ada.other@example.com"));
        assert!(merged.contains("EMAIL;TYPE=HOME:ada.legacy@example.com"));
        assert!(merged.contains("CATEGORIES:Friends,Important"));
    }

    #[test]
    fn evolution_contact_fields_are_serialized_and_toggleable() {
        let fields = ContactVCardFields {
            evolution_file_as: "Lovelace, Ada".to_string(),
            evolution_manager: "Grace Hopper".to_string(),
            evolution_wants_html: true,
            evolution_list: false,
            evolution_list_show_addresses: true,
            ..Default::default()
        };
        let vcard = build_vcard_with_fields(
            "Ada Lovelace",
            "Analytical Engines",
            "Mathematician",
            "ada@example.com",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &fields,
            Some("contact-2"),
        )
        .expect("contact vCard should serialize");
        assert!(vcard.contains("X-EVOLUTION-FILE-AS:Lovelace\\, Ada"));
        assert!(vcard.contains("X-EVOLUTION-WANTS-HTML:TRUE"));
        assert!(vcard.contains("X-EVOLUTION-LIST:FALSE"));
        assert!(vcard.contains("X-EVOLUTION-LIST-SHOW-ADDRESSES:TRUE"));
        assert!(parse_vcard_bool("yes"));
        assert!(!parse_vcard_bool("FALSE"));
    }
}
