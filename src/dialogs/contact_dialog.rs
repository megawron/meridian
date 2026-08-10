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

use crate::services::ebook_service::EBookContactRecord;
use crate::services::ebook_service::EBookService;
use crate::services::eds_service;
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
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-vcard" => self.edit_vcard.borrow().to_value(),
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

        let row_org = adw::EntryRow::builder()
            .title("Firma / Organisation")
            .build();
        group_general.add(&row_org);

        let row_title = adw::EntryRow::builder().title("Position / Titel").build();
        group_general.add(&row_title);

        let source_model = gtk::StringList::new(&["EDS-Adressbücher werden geladen…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Adressbuch")
            .model(&source_model)
            .build();
        group_general.add(&source_row);

        content_box.append(&group_general);

        // Group 2: Contact Methods
        let group_contact = adw::PreferencesGroup::builder()
            .title("Kontaktdaten")
            .build();

        let row_email1 = adw::EntryRow::builder().title("E-Mail (Privat)").build();
        group_contact.add(&row_email1);

        let row_email2 = adw::EntryRow::builder().title("E-Mail (Arbeit)").build();
        group_contact.add(&row_email2);

        let row_phone = adw::EntryRow::builder().title("Telefon (Mobil)").build();
        group_contact.add(&row_phone);

        content_box.append(&group_contact);

        // Group 3: Address & Notes
        let group_extra = adw::PreferencesGroup::builder()
            .title("Adresse & Notizen")
            .build();

        let row_address = adw::EntryRow::builder().title("Postadresse").build();
        group_extra.add(&row_address);

        let row_note = adw::EntryRow::builder().title("Notiz").build();
        group_extra.add(&row_note);

        if let Some(vcard) = edit_vcard.as_deref() {
            if let Some(contact) = VCardContact::parse_vcard_string(vcard) {
                row_name.set_text(&contact.fn_name);
                row_org.set_text(&contact.org);
                row_title.set_text(&contact.title);
                row_email1.set_text(&contact.email);
                row_phone.set_text(&contact.tel);
                row_note.set_text(&contact.note);
            }
            if let Some(email) = vcard_property(vcard, "EMAIL", 1) {
                row_email2.set_text(&email);
            }
            if let Some(address) = vcard_property(vcard, "ADR", 0) {
                row_address.set_text(&address.replace(';', ", "));
            }
        }

        content_box.append(&group_extra);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let edit_source_uid_for_load = edit_source_uid.clone();
        glib::spawn_future_local(async move {
            match eds_service::list_address_book_sources().await {
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
                    source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
                    let mut uids = source_uids_for_load.borrow_mut();
                    uids.clear();
                    uids.extend(active_sources.into_iter().map(|source| source.uid));
                    if let Some(edit_source_uid) = edit_source_uid_for_load.as_deref() {
                        if let Some(index) = uids.iter().position(|uid| uid == edit_source_uid) {
                            source_row_for_load.set_selected(index as u32);
                        }
                    }
                    if uids.is_empty() {
                        source_model_for_load.splice(
                            0,
                            source_model_for_load.n_items(),
                            &["Kein aktives EDS-Adressbuch verfügbar"],
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load EDS address books for contact dialog");
                    source_model_for_load.splice(
                        0,
                        source_model_for_load.n_items(),
                        &["EDS-Adressbücher sind nicht erreichbar"],
                    );
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
                dialog.show_toast("Kein aktives EDS-Adressbuch ist ausgewählt");
                return;
            };
            let vcard = match build_vcard(
                &row_name.text(),
                &row_org.text(),
                &row_title.text(),
                &row_email1.text(),
                &row_email2.text(),
                &row_phone.text(),
                &row_address.text(),
                &row_note.text(),
                edit_uid_for_save.as_deref(),
            ) {
                Ok(vcard) => vcard,
                Err(error) => {
                    dialog.show_toast(&error);
                    return;
                }
            };
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

fn build_vcard(
    name: &str,
    organization: &str,
    title: &str,
    email: &str,
    email_work: &str,
    phone: &str,
    address: &str,
    note: &str,
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
        format!("N:{};;;;", escape_vcard_text(name)),
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
    if !phone.trim().is_empty() {
        lines.push(format!("TEL;TYPE=CELL:{}", escape_vcard_text(phone.trim())));
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
    lines.push("END:VCARD".to_string());
    Ok(format!("{}\r\n", lines.join("\r\n")))
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
        let result = build_vcard("  ", "", "", "", "", "", "", "", None);
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
            "+44, 123",
            "London; UK",
            "Line one\nLine two",
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
}
