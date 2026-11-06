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
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::services::contact_store_service::ContactStoreService;
use crate::services::ebook_service::EBookService;
use crate::services::ebook_service::{EBookContactRecord, load_contacts_prefer_eds};
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;
use crate::services::vcard_service::{VCardContact, VCardListMember, VCardListMemberKind};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactDialog {
        pub edit_vcard: RefCell<Option<String>>,
        pub edit_uid: RefCell<Option<String>>,
        pub edit_source_uid: RefCell<Option<String>>,
        pub list_mode: Cell<bool>,
        pub save_in_progress: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContactDialog {
        const NAME: &'static str = "MeridianContactDialog";
        type Type = super::ContactDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for ContactDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("edit-vcard")
                        .construct()
                        .build(),
                    glib::ParamSpecString::builder("edit-uid")
                        .construct()
                        .build(),
                    glib::ParamSpecString::builder("edit-source-uid")
                        .construct()
                        .build(),
                    glib::ParamSpecBoolean::builder("list-mode")
                        .default_value(false)
                        .construct()
                        .build(),
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
                "list-mode" => self.list_mode.set(value.get().expect("boolean")),
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-vcard" => self.edit_vcard.borrow().to_value(),
                "edit-uid" => self.edit_uid.borrow().to_value(),
                "edit-source-uid" => self.edit_source_uid.borrow().to_value(),
                "list-mode" => self.list_mode.get().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for ContactDialog {}
    impl AdwDialogImpl for ContactDialog {}
}

glib::wrapper! {
    pub struct ContactDialog(ObjectSubclass<imp::ContactDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ContactDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "New Contact")
            .property("content-width", 560)
            .property("content-height", 560)
            .build()
    }

    pub(crate) fn new_for_list() -> Self {
        glib::Object::builder()
            .property("title", "New Contact List")
            .property("content-width", 620)
            .property("content-height", 620)
            .property("list-mode", true)
            .build()
    }

    pub(crate) fn new_for_edit(record: &EBookContactRecord) -> Self {
        glib::Object::builder()
            .property("title", "Edit Contact")
            .property("content-width", 560)
            .property("content-height", 600)
            .property("edit-vcard", &record.vcard)
            .property("edit-uid", &record.uid)
            .property("edit-source-uid", &record.source_uid)
            .property("list-mode", record.contact.is_list)
            .build()
    }

    pub(crate) fn new_for_email(name: &str, email: &str) -> Self {
        let seed = VCardContact::new(name.trim(), email.trim(), "", "").to_vcard_string();
        glib::Object::builder()
            .property("title", "Add Sender to Address Book")
            .property("content-width", 560)
            .property("content-height", 560)
            .property("edit-vcard", seed)
            .build()
    }

    fn build_ui(&self) {
        if self.imp().list_mode.get() {
            self.build_list_ui();
            return;
        }
        self.guard_close_while_saving();
        let edit_vcard = self.imp().edit_vcard.borrow().clone();
        let edit_uid = self.imp().edit_uid.borrow().clone();
        let edit_source_uid = self.imp().edit_source_uid.borrow().clone();
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        header_bar.set_show_start_title_buttons(false);
        header_bar.set_show_end_title_buttons(false);
        let btn_cancel = gtk::Button::builder()
            .label("Cancel")
            .build();
        let dialog_weak_for_cancel = self.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header_bar.pack_start(&btn_cancel);

        let btn_save = gtk::Button::builder()
            .label("Save")
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
            .margin_bottom(24)
            .build();

        // Header Avatar & Name
        let name_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(16)
            .halign(gtk::Align::Center)
            .margin_bottom(4)
            .build();

        let avatar = adw::Avatar::builder()
            .size(72)
            .text("New Contact")
            .show_initials(true)
            .build();
        name_box.append(&avatar);
        content_box.append(&name_box);

        // Group 1: General Info (Clean & Focused like GNOME Contacts)
        let group_general = adw::PreferencesGroup::new();

        let row_name = adw::EntryRow::builder().title("Full Name").build();
        let name_details_toggle = gtk::ToggleButton::builder()
            .icon_name("pan-down-symbolic")
            .tooltip_text("Show/hide structured name fields")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        row_name.add_suffix(&name_details_toggle);
        group_general.add(&row_name);

        let row_given_name = adw::EntryRow::builder().title("First Name").build();
        row_given_name.set_visible(false);
        group_general.add(&row_given_name);

        let row_family_name = adw::EntryRow::builder().title("Last Name").build();
        row_family_name.set_visible(false);
        group_general.add(&row_family_name);

        let row_additional_name = adw::EntryRow::builder()
            .title("Middle Name")
            .build();
        row_additional_name.set_visible(false);
        group_general.add(&row_additional_name);

        let row_name_prefix = adw::EntryRow::builder().title("Name Prefix").build();
        row_name_prefix.set_visible(false);
        group_general.add(&row_name_prefix);

        let row_name_suffix = adw::EntryRow::builder().title("Name Suffix").build();
        row_name_suffix.set_visible(false);
        group_general.add(&row_name_suffix);

        {
            let row_given_name = row_given_name.clone();
            let row_family_name = row_family_name.clone();
            let row_additional_name = row_additional_name.clone();
            let row_name_prefix = row_name_prefix.clone();
            let row_name_suffix = row_name_suffix.clone();
            name_details_toggle.connect_toggled(move |btn| {
                let show = btn.is_active();
                btn.set_icon_name(if show {
                    "pan-up-symbolic"
                } else {
                    "pan-down-symbolic"
                });
                row_given_name.set_visible(show);
                row_family_name.set_visible(show);
                row_additional_name.set_visible(show);
                row_name_prefix.set_visible(show);
                row_name_suffix.set_visible(show);
            });
        }

        {
            let avatar = avatar.clone();
            row_name.connect_changed(move |entry| {
                let text = entry.text();
                let trimmed = text.trim();
                avatar.set_text(Some(if trimmed.is_empty() {
                    "New Contact"
                } else {
                    trimmed
                }));
            });
        }

        let source_model = gtk::StringList::new(&["Loading address books…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Address Book")
            .model(&source_model)
            .build();
        source_row.set_sensitive(edit_uid.is_none());
        group_general.add(&source_row);

        let row_org = adw::EntryRow::builder()
            .title("Company / Organization")
            .build();
        group_general.add(&row_org);

        let row_title = adw::EntryRow::builder().title("Job Title").build();
        group_general.add(&row_title);

        content_box.append(&group_general);

        // Group 2: Contact Methods (Progressive Email & Phone rows)
        let group_contact = adw::PreferencesGroup::builder()
            .title("Contact Information")
            .build();

        let row_email1 = adw::EntryRow::builder().title("Email (Personal)").build();
        row_email1.set_input_purpose(gtk::InputPurpose::Email);
        let btn_add_email = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add another email address")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        row_email1.add_suffix(&btn_add_email);
        group_contact.add(&row_email1);

        let row_email2 = adw::EntryRow::builder().title("Email (Work)").build();
        row_email2.set_input_purpose(gtk::InputPurpose::Email);
        row_email2.set_visible(false);
        group_contact.add(&row_email2);

        let row_email3 = adw::EntryRow::builder().title("Email (Other)").build();
        row_email3.set_input_purpose(gtk::InputPurpose::Email);
        row_email3.set_visible(false);
        group_contact.add(&row_email3);

        {
            let row_email2 = row_email2.clone();
            let row_email3 = row_email3.clone();
            btn_add_email.connect_clicked(move |btn| {
                if !row_email2.is_visible() {
                    row_email2.set_visible(true);
                    row_email2.grab_focus();
                    if row_email3.is_visible() {
                        btn.set_visible(false);
                    }
                } else if !row_email3.is_visible() {
                    row_email3.set_visible(true);
                    row_email3.grab_focus();
                    btn.set_visible(false);
                }
            });
        }

        let row_phone = adw::EntryRow::builder().title("Phone (Mobile)").build();
        row_phone.set_input_purpose(gtk::InputPurpose::Phone);
        let btn_add_phone = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add another phone number")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        row_phone.add_suffix(&btn_add_phone);
        group_contact.add(&row_phone);

        let row_phone_work = adw::EntryRow::builder().title("Phone (Work)").build();
        row_phone_work.set_input_purpose(gtk::InputPurpose::Phone);
        row_phone_work.set_visible(false);
        group_contact.add(&row_phone_work);

        let row_phone_home = adw::EntryRow::builder().title("Phone (Home)").build();
        row_phone_home.set_input_purpose(gtk::InputPurpose::Phone);
        row_phone_home.set_visible(false);
        group_contact.add(&row_phone_home);

        let phones_revealed = Rc::new(Cell::new(0u8));
        {
            let row_phone_work = row_phone_work.clone();
            let row_phone_home = row_phone_home.clone();
            let phones_revealed = Rc::clone(&phones_revealed);
            btn_add_phone.connect_clicked(move |btn| {
                if !row_phone_work.is_visible() {
                    phones_revealed.set(phones_revealed.get().max(1));
                    row_phone_work.set_visible(true);
                    row_phone_work.grab_focus();
                    if row_phone_home.is_visible() {
                        btn.set_visible(false);
                    }
                } else if !row_phone_home.is_visible() {
                    phones_revealed.set(2);
                    row_phone_home.set_visible(true);
                    row_phone_home.grab_focus();
                    btn.set_visible(false);
                }
            });
        }

        let row_sip = adw::EntryRow::builder()
            .title("SIP / VoIP Address")
            .build();
        group_contact.add(&row_sip);

        content_box.append(&group_contact);

        // Group 3: Primary Extras (Address, Note, Website, Birthday)
        let group_extra = adw::PreferencesGroup::builder()
            .title("Additional Information")
            .build();

        let row_address = adw::EntryRow::builder().title("Home Address").build();
        let btn_add_address = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add another address")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        row_address.add_suffix(&btn_add_address);
        group_extra.add(&row_address);

        let row_address_work = adw::EntryRow::builder()
            .title("Work Address")
            .build();
        row_address_work.set_visible(false);
        group_extra.add(&row_address_work);

        let row_address_other = adw::EntryRow::builder()
            .title("Other Address")
            .build();
        row_address_other.set_visible(false);
        group_extra.add(&row_address_other);

        let addresses_revealed = Rc::new(Cell::new(0u8));
        {
            let row_address_work = row_address_work.clone();
            let row_address_other = row_address_other.clone();
            let addresses_revealed = Rc::clone(&addresses_revealed);
            btn_add_address.connect_clicked(move |btn| {
                if !row_address_work.is_visible() {
                    addresses_revealed.set(addresses_revealed.get().max(1));
                    row_address_work.set_visible(true);
                    row_address_work.grab_focus();
                    if row_address_other.is_visible() {
                        btn.set_visible(false);
                    }
                } else if !row_address_other.is_visible() {
                    addresses_revealed.set(2);
                    row_address_other.set_visible(true);
                    row_address_other.grab_focus();
                    btn.set_visible(false);
                }
            });
        }

        let row_url = adw::EntryRow::builder().title("Website").build();
        row_url.set_input_purpose(gtk::InputPurpose::Url);
        group_extra.add(&row_url);

        let row_birthday = adw::EntryRow::builder()
            .title("Birthday (YYYY-MM-DD)")
            .build();
        attach_entry_row_date_picker(&row_birthday);
        group_extra.add(&row_birthday);

        let row_note = adw::EntryRow::builder().title("Note").build();
        group_extra.add(&row_note);

        // Progressive Enhancement: Expandable row for secondary & advanced fields
        let expander_more = adw::ExpanderRow::builder()
            .title("More Details")
            .subtitle("Nickname, anniversary, categories, messenger, and advanced fields")
            .show_enable_switch(false)
            .build();

        let row_nickname = adw::EntryRow::builder().title("Nickname").build();
        expander_more.add_row(&row_nickname);

        let row_anniversary = adw::EntryRow::builder()
            .title("Anniversary (YYYY-MM-DD)")
            .build();
        attach_entry_row_date_picker(&row_anniversary);
        expander_more.add_row(&row_anniversary);

        let row_categories = adw::EntryRow::builder()
            .title("Categories (comma-separated)")
            .build();
        expander_more.add_row(&row_categories);

        let row_impp = adw::EntryRow::builder()
            .title("Instant Messaging (IMPP)")
            .build();
        expander_more.add_row(&row_impp);

        let row_role = adw::EntryRow::builder().title("Role").build();
        expander_more.add_row(&row_role);

        let row_evolution_manager = adw::EntryRow::builder().title("Manager").build();
        expander_more.add_row(&row_evolution_manager);

        let row_evolution_assistant = adw::EntryRow::builder().title("Assistant").build();
        expander_more.add_row(&row_evolution_assistant);

        let row_evolution_spouse = adw::EntryRow::builder().title("Spouse / Partner").build();
        expander_more.add_row(&row_evolution_spouse);

        let row_related = adw::EntryRow::builder()
            .title("Related Contacts (comma-separated)")
            .build();
        expander_more.add_row(&row_related);

        let row_gender = adw::EntryRow::builder().title("Gender").build();
        expander_more.add_row(&row_gender);

        let row_timezone = adw::EntryRow::builder().title("Time Zone").build();
        expander_more.add_row(&row_timezone);

        let row_geo = adw::EntryRow::builder()
            .title("Geo Position (lat;lon)")
            .build();
        expander_more.add_row(&row_geo);

        let row_photo = adw::EntryRow::builder()
            .title("Photo (URI or Base64)")
            .build();
        row_photo.set_input_purpose(gtk::InputPurpose::Url);
        expander_more.add_row(&row_photo);

        let row_evolution_blog_url = adw::EntryRow::builder().title("Blog URL").build();
        row_evolution_blog_url.set_input_purpose(gtk::InputPurpose::Url);
        expander_more.add_row(&row_evolution_blog_url);

        let row_evolution_video_url = adw::EntryRow::builder().title("Video URL").build();
        row_evolution_video_url.set_input_purpose(gtk::InputPurpose::Url);
        expander_more.add_row(&row_evolution_video_url);

        let row_evolution_file_as = adw::EntryRow::builder()
            .title("File As / Sort Name")
            .build();
        expander_more.add_row(&row_evolution_file_as);

        let row_evolution_wants_html = adw::SwitchRow::builder()
            .title("Prefer HTML Mail")
            .subtitle("Prefer sending messages to this contact as HTML")
            .build();
        expander_more.add_row(&row_evolution_wants_html);

        let row_certs = adw::EntryRow::builder()
            .title("S/MIME Certificate")
            .build();
        expander_more.add_row(&row_certs);

        group_extra.add(&expander_more);

        let row_evolution_list = adw::SwitchRow::builder()
            .title("Mailing List")
            .subtitle("Treat contact as a list")
            .build();
        let row_evolution_list_name = adw::EntryRow::builder().title("List Name").build();
        let row_evolution_list_show_addresses = adw::SwitchRow::builder()
            .title("Show list addresses")
            .build();
        // List editing has a dedicated member-aware UI. Keep these widgets
        // only as compatibility storage for the existing individual vCard
        // builder tests; they are intentionally not shown here.

        if let Some(vcard) = edit_vcard.as_deref() {
            if let Some(contact) = VCardContact::parse_vcard_string(vcard) {
                row_name.set_text(&contact.fn_name);
                if !contact.fn_name.trim().is_empty() {
                    avatar.set_text(Some(contact.fn_name.trim()));
                }
                row_family_name.set_text(&contact.name.family);
                row_given_name.set_text(&contact.name.given);
                row_additional_name.set_text(&contact.name.additional);
                row_name_prefix.set_text(&contact.name.prefix);
                row_name_suffix.set_text(&contact.name.suffix);
                if !contact.name.additional.trim().is_empty()
                    || !contact.name.prefix.trim().is_empty()
                    || !contact.name.suffix.trim().is_empty()
                {
                    name_details_toggle.set_active(true);
                }
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
                if let Some(sip) = contact
                    .impps
                    .iter()
                    .find(|i| i.to_ascii_lowercase().starts_with("sip:"))
                    .cloned()
                {
                    row_sip.set_text(
                        sip.strip_prefix("sip:")
                            .or_else(|| sip.strip_prefix("SIP:"))
                            .unwrap_or(&sip),
                    );
                }
                if let Some(work_addr) = contact.addresses.get(1) {
                    row_address_work.set_text(&work_addr.replace(';', ", "));
                }
                if let Some(other_addr) = contact.addresses.get(2) {
                    row_address_other.set_text(&other_addr.replace(';', ", "));
                }
                if let Some(cert) = contact
                    .unknown_properties
                    .iter()
                    .find(|p| {
                        p.property.eq_ignore_ascii_case("KEY")
                            || p.property.eq_ignore_ascii_case("USERCERTIFICATE")
                            || p.property.eq_ignore_ascii_case("X-EVOLUTION-CERT")
                    })
                    .map(|p| &p.value)
                {
                    row_certs.set_text(cert);
                }
            }
            if let Some(email) = vcard_property(vcard, "EMAIL", 1) {
                row_email2.set_text(&email);
            }
            if let Some(address) = vcard_property(vcard, "ADR", 0) {
                row_address.set_text(&address.replace(';', ", "));
            }
            if let Some(work_addr) = vcard_property(vcard, "ADR;TYPE=WORK", 0) {
                row_address_work.set_text(&work_addr.replace(';', ", "));
            } else if let Some(work_addr) = vcard_property(vcard, "ADR", 1) {
                if row_address_work.text().trim().is_empty() {
                    row_address_work.set_text(&work_addr.replace(';', ", "));
                }
            }
            if let Some(other_addr) = vcard_property(vcard, "ADR;TYPE=OTHER", 0) {
                row_address_other.set_text(&other_addr.replace(';', ", "));
            } else if let Some(other_addr) = vcard_property(vcard, "ADR", 2) {
                if row_address_other.text().trim().is_empty() {
                    row_address_other.set_text(&other_addr.replace(';', ", "));
                }
            }
            if let Some(cert) = vcard_property(vcard, "KEY", 0).or_else(|| vcard_property(vcard, "USERCERTIFICATE", 0)) {
                if row_certs.text().trim().is_empty() {
                    row_certs.set_text(&cert);
                }
            }
        }

        // Auto-reveal populated secondary email/phone/address rows and expanders
        if !row_email2.text().trim().is_empty() {
            row_email2.set_visible(true);
        }
        if !row_email3.text().trim().is_empty() {
            row_email2.set_visible(true);
            row_email3.set_visible(true);
            btn_add_email.set_visible(false);
        }
        if !row_phone_work.text().trim().is_empty() {
            phones_revealed.set(phones_revealed.get().max(1));
        }
        if !row_phone_home.text().trim().is_empty() {
            phones_revealed.set(2);
        }
        if !row_address_work.text().trim().is_empty() {
            addresses_revealed.set(addresses_revealed.get().max(1));
        }
        if !row_address_other.text().trim().is_empty() {
            addresses_revealed.set(2);
        }
        if [
            &row_nickname,
            &row_anniversary,
            &row_categories,
            &row_impp,
            &row_role,
            &row_evolution_manager,
            &row_evolution_assistant,
            &row_evolution_spouse,
            &row_related,
            &row_gender,
            &row_timezone,
            &row_geo,
            &row_photo,
            &row_evolution_blog_url,
            &row_evolution_video_url,
            &row_evolution_file_as,
            &row_certs,
        ]
        .iter()
        .any(|row| !row.text().trim().is_empty())
            || row_evolution_wants_html.is_active()
        {
            expander_more.set_expanded(true);
        }

        content_box.append(&group_extra);

        let update_section_visibility: Rc<dyn Fn(&AppSettings)> = {
            let row_phone = row_phone.clone();
            let row_phone_work = row_phone_work.clone();
            let row_phone_home = row_phone_home.clone();
            let btn_add_phone = btn_add_phone.clone();
            let phones_revealed = Rc::clone(&phones_revealed);
            let row_sip = row_sip.clone();
            let row_impp = row_impp.clone();
            let row_address = row_address.clone();
            let row_address_work = row_address_work.clone();
            let row_address_other = row_address_other.clone();
            let btn_add_address = btn_add_address.clone();
            let addresses_revealed = Rc::clone(&addresses_revealed);
            let row_url = row_url.clone();
            let row_evolution_blog_url = row_evolution_blog_url.clone();
            let row_evolution_video_url = row_evolution_video_url.clone();
            let row_org = row_org.clone();
            let row_title = row_title.clone();
            let row_role = row_role.clone();
            let row_evolution_manager = row_evolution_manager.clone();
            let row_evolution_assistant = row_evolution_assistant.clone();
            let row_birthday = row_birthday.clone();
            let row_anniversary = row_anniversary.clone();
            let row_nickname = row_nickname.clone();
            let row_gender = row_gender.clone();
            let row_timezone = row_timezone.clone();
            let row_geo = row_geo.clone();
            let row_related = row_related.clone();
            let row_categories = row_categories.clone();
            let row_photo = row_photo.clone();
            let row_evolution_spouse = row_evolution_spouse.clone();
            let row_note = row_note.clone();
            let row_certs = row_certs.clone();

            Rc::new(move |settings: &AppSettings| {
                // 1. Phone
                let has_phone1 = !row_phone.text().trim().is_empty();
                let has_phone_work = !row_phone_work.text().trim().is_empty();
                let has_phone_home = !row_phone_home.text().trim().is_empty();
                let has_phone = has_phone1 || has_phone_work || has_phone_home;
                let show_phone = settings.contact_editor_show_phone() || has_phone;
                row_phone.set_visible(show_phone);
                let show_work_phone = show_phone && (has_phone_work || phones_revealed.get() >= 1);
                let show_home_phone = show_phone && (has_phone_home || phones_revealed.get() >= 2);
                row_phone_work.set_visible(show_work_phone);
                row_phone_home.set_visible(show_home_phone);
                btn_add_phone.set_visible(show_phone && !(show_work_phone && show_home_phone));

                // 2. SIP
                let has_sip = !row_sip.text().trim().is_empty();
                row_sip.set_visible(settings.contact_editor_show_sip() || has_sip);

                // 3. IM
                let has_im = !row_impp.text().trim().is_empty();
                row_impp.set_visible(settings.contact_editor_show_im() || has_im);

                // 4. Home Address
                let has_home = !row_address.text().trim().is_empty();
                let show_home_addr = settings.contact_editor_show_home_address() || has_home;
                row_address.set_visible(show_home_addr);

                // 5. Work Address
                let has_work = !row_address_work.text().trim().is_empty();
                let show_work_addr = (settings.contact_editor_show_work_address()
                    && addresses_revealed.get() >= 1)
                    || has_work;
                row_address_work.set_visible(show_work_addr);

                // 6. Other Address
                let has_other = !row_address_other.text().trim().is_empty();
                let show_other_addr = (settings.contact_editor_show_other_address()
                    && addresses_revealed.get() >= 2)
                    || has_other;
                row_address_other.set_visible(show_other_addr);
                btn_add_address
                    .set_visible(show_home_addr && !(show_work_addr && show_other_addr));

                // 7. Web
                let has_web = !row_url.text().trim().is_empty()
                    || !row_evolution_blog_url.text().trim().is_empty()
                    || !row_evolution_video_url.text().trim().is_empty();
                let show_web = settings.contact_editor_show_web() || has_web;
                row_url.set_visible(show_web);
                row_evolution_blog_url.set_visible(show_web);
                row_evolution_video_url.set_visible(show_web);

                // 8. Job
                let has_job = !row_org.text().trim().is_empty()
                    || !row_title.text().trim().is_empty()
                    || !row_role.text().trim().is_empty()
                    || !row_evolution_manager.text().trim().is_empty()
                    || !row_evolution_assistant.text().trim().is_empty();
                let show_job = settings.contact_editor_show_job() || has_job;
                row_org.set_visible(show_job);
                row_title.set_visible(show_job);
                row_role.set_visible(show_job);
                row_evolution_manager.set_visible(show_job);
                row_evolution_assistant.set_visible(show_job);

                // 9. Dates
                let has_dates = !row_birthday.text().trim().is_empty()
                    || !row_anniversary.text().trim().is_empty();
                let show_dates = settings.contact_editor_show_dates() || has_dates;
                row_birthday.set_visible(show_dates);
                row_anniversary.set_visible(show_dates);

                // 10. Misc
                let has_misc = !row_nickname.text().trim().is_empty()
                    || !row_gender.text().trim().is_empty()
                    || !row_timezone.text().trim().is_empty()
                    || !row_geo.text().trim().is_empty()
                    || !row_related.text().trim().is_empty()
                    || !row_categories.text().trim().is_empty()
                    || !row_photo.text().trim().is_empty()
                    || !row_evolution_spouse.text().trim().is_empty();
                let show_misc = settings.contact_editor_show_misc() || has_misc;
                row_nickname.set_visible(show_misc);
                row_gender.set_visible(show_misc);
                row_timezone.set_visible(show_misc);
                row_geo.set_visible(show_misc);
                row_related.set_visible(show_misc);
                row_categories.set_visible(show_misc);
                row_photo.set_visible(show_misc);
                row_evolution_spouse.set_visible(show_misc);

                // 11. Notes
                let has_notes = !row_note.text().trim().is_empty();
                row_note.set_visible(settings.contact_editor_show_notes() || has_notes);

                // 12. Certs
                let has_certs = !row_certs.text().trim().is_empty();
                row_certs.set_visible(settings.contact_editor_show_certs() || has_certs);
            })
        };

        if let Some(settings) = AppSettings::try_new() {
            update_section_visibility(&settings);

            const SECTION_KEYS: &[&str] = &[
                "contact-editor-show-phone",
                "contact-editor-show-sip",
                "contact-editor-show-im",
                "contact-editor-show-home-address",
                "contact-editor-show-work-address",
                "contact-editor-show-other-address",
                "contact-editor-show-web",
                "contact-editor-show-job",
                "contact-editor-show-dates",
                "contact-editor-show-misc",
                "contact-editor-show-notes",
                "contact-editor-show-certs",
            ];
                for key in SECTION_KEYS {
                let updater = Rc::clone(&update_section_visibility);
                let phones_revealed_for_setting = Rc::clone(&phones_revealed);
                let addresses_revealed_for_setting = Rc::clone(&addresses_revealed);
                let key_name = (*key).to_string();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(settings) = AppSettings::try_new() {
                        if key_name == "contact-editor-show-phone"
                            && settings.contact_editor_show_phone()
                        {
                            phones_revealed_for_setting.set(2);
                        } else if key_name == "contact-editor-show-work-address"
                            && settings.contact_editor_show_work_address()
                        {
                            addresses_revealed_for_setting
                                .set(addresses_revealed_for_setting.get().max(1));
                        } else if key_name == "contact-editor-show-other-address"
                            && settings.contact_editor_show_other_address()
                        {
                            addresses_revealed_for_setting.set(2);
                        }
                        updater(&settings);
                    }
                });
            }
        }

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_child(Some(&toolbar_view));

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

            if let Some(uid) = edit_source_uid_for_load.as_deref().filter(|uid| {
                !uid.trim().is_empty() && !uids.iter().any(|candidate| candidate == *uid)
            }) {
                names.push(format!("Unavailable Source · {uid}"));
                uids.push(uid.to_string());
            }
            if local_addressbook_available {
                names.push("Local Address Book".to_string());
                uids.push("local-addressbook".to_string());
            }
            if names.is_empty() {
                names.push(
                    discovery
                        .error
                        .map(|error| format!("Address books are unreachable: {error}"))
                        .unwrap_or_else(|| "No active address book available".to_string()),
                );
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            source_uids_for_load.replace(uids.clone());
            let target_uid = edit_source_uid_for_load
                .as_deref()
                .or_else(|| preferred_source_uid.as_deref().filter(|uid| uids.iter().any(|c| c == *uid)))
                .or_else(|| {
                    if local_addressbook_available {
                        Some("local-addressbook")
                    } else {
                        uids.first().map(String::as_str)
                    }
                });
            if let Some(uid) = target_uid {
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
                dialog.show_toast("No active address book selected");
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
                sip: row_sip.text().to_string(),
                address_work: row_address_work.text().to_string(),
                address_other: row_address_other.text().to_string(),
                certs: row_certs.text().to_string(),
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
                if let Some(uid) = edit_uid_for_save.as_deref() {
                    crate::services::ebook_service::register_known_ebook_vcard(
                        &source_uid,
                        uid,
                        original,
                    );
                }
            }
            let source_uid_for_worker = source_uid.clone();
            let operation_uid = edit_uid_for_save.clone();
            let vcard_for_worker = vcard.clone();
            let was_edit = edit_uid_for_save.is_some();
            let (sender, receiver) = async_channel::bounded(1);
            dialog.imp().save_in_progress.set(true);
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
                    if let Some(dialog) = dialog_weak_for_result.upgrade() {
                        dialog.imp().save_in_progress.set(false);
                    }
                    save_button_for_result.set_sensitive(true);
                    return;
                };
                if let Some(dialog) = dialog_weak_for_result.upgrade() {
                    dialog.imp().save_in_progress.set(false);
                }
                match result {
                    Ok(result) => {
                        let message = if was_edit {
                            format!("Contact updated ({})", result.uid)
                        } else {
                            format!("Contact saved ({})", result.uid)
                        };
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&message);
                            dialog.close();
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&format!(
                                "Could not save contact: {error}"
                            ));
                        }
                    }
                }
                save_button_for_result.set_sensitive(true);
            });
        });
    }

    fn build_list_ui(&self) {
        self.guard_close_while_saving();
        let edit_vcard = self.imp().edit_vcard.borrow().clone();
        let edit_uid = self.imp().edit_uid.borrow().clone();
        let edit_source_uid = self.imp().edit_source_uid.borrow().clone();
        let original_contact = edit_vcard
            .as_deref()
            .and_then(VCardContact::parse_vcard_string);
        let initial_name = original_contact
            .as_ref()
            .map(|contact| {
                if contact.list_name.trim().is_empty() {
                    contact.fn_name.clone()
                } else {
                    contact.list_name.clone()
                }
            })
            .unwrap_or_default();
        let initial_show_addresses = original_contact
            .as_ref()
            .is_some_and(|contact| contact.list_show_addresses);
        let members = Rc::new(RefCell::new(
            original_contact
                .as_ref()
                .map(|contact| contact.list_members.clone())
                .unwrap_or_default(),
        ));

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        header_bar.set_show_start_title_buttons(false);
        header_bar.set_show_end_title_buttons(false);
        let cancel_button = gtk::Button::builder()
            .label("Cancel")
            .build();
        let dialog_weak_for_cancel = self.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header_bar.pack_start(&cancel_button);
        let save_button = gtk::Button::builder()
            .label("Save")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&save_button);
        toolbar_view.add_top_bar(&header_bar);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        let list_group = adw::PreferencesGroup::builder()
            .title("Contact List")
            .description("Distribution lists can contain local contacts and external addresses.")
            .build();
        let name_row = adw::EntryRow::builder().title("List Name").build();
        name_row.set_text(&initial_name);
        list_group.add(&name_row);

        let source_model = gtk::StringList::new(&["Loading address books…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Address Book")
            .model(&source_model)
            .build();
        source_row.set_sensitive(edit_uid.is_none());
        list_group.add(&source_row);

        let show_addresses = adw::SwitchRow::builder()
            .title("Show member addresses")
            .subtitle("When disabled, recipient addresses in the list are kept private.")
            .active(initial_show_addresses)
            .build();
        list_group.add(&show_addresses);
        content_box.append(&list_group);

        let member_group = adw::PreferencesGroup::builder()
            .title("Members")
            .description("Invalid or unresolvable members remain visible and must be removed or replaced before saving.")
            .build();
        let external_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let external_entry = gtk::Entry::builder()
            .placeholder_text("Name <address@example.org> or address@example.org")
            .hexpand(true)
            .build();
        let add_external = gtk::Button::builder()
            .label("Add External Address")
            .build();
        external_box.append(&external_entry);
        external_box.append(&add_external);
        member_group.add(&external_box);

        let local_model = gtk::StringList::new(&["Loading address book contacts…"]);
        let local_dropdown =
            gtk::DropDown::new(Some(local_model.clone()), None::<&gtk::Expression>);
        local_dropdown.set_hexpand(true);
        let add_local = gtk::Button::builder()
            .label("Add Local Contact")
            .build();
        let local_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(8)
            .build();
        local_box.append(&local_dropdown);
        local_box.append(&add_local);
        member_group.add(&local_box);

        let nested_model = gtk::StringList::new(&["Loading contact lists…"]);
        let nested_dropdown =
            gtk::DropDown::new(Some(nested_model.clone()), None::<&gtk::Expression>);
        nested_dropdown.set_hexpand(true);
        let add_nested = gtk::Button::builder()
            .label("Add Contact List")
            .build();
        let nested_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(8)
            .build();
        nested_box.append(&nested_dropdown);
        nested_box.append(&add_nested);
        member_group.add(&nested_box);

        let member_list = gtk::ListBox::new();
        member_list.set_selection_mode(gtk::SelectionMode::None);
        member_list.add_css_class("boxed-list");
        member_group.add(&member_list);
        content_box.append(&member_group);

        refresh_list_member_rows(&member_list, &members, &show_addresses);
        let members_for_visibility = Rc::clone(&members);
        let member_list_for_visibility = member_list.clone();
        show_addresses.connect_active_notify(move |row| {
            refresh_list_member_rows(&member_list_for_visibility, &members_for_visibility, row);
        });

        let dialog_weak_for_external = self.downgrade();
        let members_for_external = Rc::clone(&members);
        let member_list_for_external = member_list.clone();
        let show_addresses_for_external = show_addresses.clone();
        let external_entry_for_external = external_entry.clone();
        add_external.connect_clicked(move |_| {
            let Some((display_name, email)) =
                parse_list_recipient_input(&external_entry_for_external.text())
            else {
                if let Some(dialog) = dialog_weak_for_external.upgrade() {
                    dialog.show_toast("Please enter a valid external email address");
                }
                return;
            };
            let member = VCardListMember::external_address(&email, &display_name);
            if !member.is_valid() {
                if let Some(dialog) = dialog_weak_for_external.upgrade() {
                    dialog.show_toast("The external address is invalid");
                }
                return;
            }
            if members_for_external
                .borrow()
                .iter()
                .any(|existing| existing.email.eq_ignore_ascii_case(&member.email))
            {
                if let Some(dialog) = dialog_weak_for_external.upgrade() {
                    dialog.show_toast("This address is already a member of the list");
                }
                return;
            }
            members_for_external.borrow_mut().push(member);
            external_entry_for_external.set_text("");
            refresh_list_member_rows(
                &member_list_for_external,
                &members_for_external,
                &show_addresses_for_external,
            );
        });

        let local_contacts = Rc::new(RefCell::new(Vec::<EBookContactRecord>::new()));
        let nested_contacts = Rc::new(RefCell::new(Vec::<EBookContactRecord>::new()));
        let local_contacts_for_add = Rc::clone(&local_contacts);
        let members_for_local = Rc::clone(&members);
        let member_list_for_local = member_list.clone();
        let show_addresses_for_local = show_addresses.clone();
        let dialog_weak_for_local = self.downgrade();
        add_local.connect_clicked(move |_| {
            let Some(record) = local_contacts_for_add
                .borrow()
                .get(local_dropdown.selected() as usize)
                .cloned()
            else {
                if let Some(dialog) = dialog_weak_for_local.upgrade() {
                    dialog.show_toast("No local contact is selected");
                }
                return;
            };
            let Some((email_num, email)) = first_contact_email_with_index(&record.contact) else {
                if let Some(dialog) = dialog_weak_for_local.upgrade() {
                    dialog.show_toast("The local contact has no email address");
                }
                return;
            };
            let member = VCardListMember::local_contact(
                &record.uid,
                &record.source_uid,
                &record.contact.fn_name,
                email,
                Some(email_num),
            );
            if members_for_local.borrow().iter().any(|existing| {
                existing.kind == VCardListMemberKind::LocalContact
                    && existing.contact_uid == member.contact_uid
                    && existing.source_uid == member.source_uid
                    && existing.email_num == member.email_num
            }) {
                if let Some(dialog) = dialog_weak_for_local.upgrade() {
                    dialog.show_toast("This contact is already a member of the list");
                }
                return;
            }
            members_for_local.borrow_mut().push(member);
            refresh_list_member_rows(
                &member_list_for_local,
                &members_for_local,
                &show_addresses_for_local,
            );
        });

        let nested_contacts_for_add = Rc::clone(&nested_contacts);
        let members_for_nested = Rc::clone(&members);
        let member_list_for_nested = member_list.clone();
        let show_addresses_for_nested = show_addresses.clone();
        let dialog_weak_for_nested = self.downgrade();
        let edit_uid_for_nested = edit_uid.clone();
        add_nested.connect_clicked(move |_| {
            let Some(record) = nested_contacts_for_add
                .borrow()
                .get(nested_dropdown.selected() as usize)
                .cloned()
            else {
                if let Some(dialog) = dialog_weak_for_nested.upgrade() {
                    dialog.show_toast("No nestable contact list is selected");
                }
                return;
            };
            if edit_uid_for_nested.as_deref() == Some(record.uid.as_str()) {
                if let Some(dialog) = dialog_weak_for_nested.upgrade() {
                    dialog.show_toast("A contact list cannot contain itself");
                }
                return;
            }
            let member = VCardListMember::nested_list(&record.uid, &record.contact.fn_name, None);
            if members_for_nested.borrow().iter().any(|existing| {
                existing.contact_uid == member.contact_uid
                    && matches!(
                        existing.kind,
                        VCardListMemberKind::NestedList | VCardListMemberKind::StandardMember
                    )
            }) {
                if let Some(dialog) = dialog_weak_for_nested.upgrade() {
                    dialog.show_toast("This contact list is already a member of the list");
                }
                return;
            }
            members_for_nested.borrow_mut().push(member);
            refresh_list_member_rows(
                &member_list_for_nested,
                &members_for_nested,
                &show_addresses_for_nested,
            );
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let edit_source_uid_for_load = edit_source_uid.clone();
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_address_book_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        let local_available = ContactStoreService::new().is_writable();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::AddressBook).await;
            let mut names = Vec::new();
            let mut uids = Vec::new();
            let mut sources = discovery.sources;
            sources.retain(|source| source.enabled);
            sources.sort_by_cached_key(|source| {
                (source.display_name.to_lowercase(), source.uid.clone())
            });
            for source in sources {
                names.push(if source.backend_name.trim().is_empty() {
                    source.display_name
                } else {
                    format!("{} · {}", source.display_name, source.backend_name)
                });
                uids.push(source.uid);
            }
            let preferred = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid.as_deref());
            if let Some(uid) = preferred.filter(|uid| {
                !uid.trim().is_empty() && !uids.iter().any(|candidate| candidate == *uid)
            }) {
                names.push(format!("Unavailable Source · {uid}"));
                uids.push(uid.to_string());
            }
            if local_available {
                names.push("Local Address Book".to_string());
                uids.push("local-addressbook".to_string());
            }
            let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            source_uids_for_load.replace(uids.clone());
            if let Some(preferred) = preferred {
                if let Some(index) = uids.iter().position(|uid| uid == preferred) {
                    source_row_for_load.set_selected(index as u32);
                }
            }
        });

        let local_model_for_load = local_model.clone();
        let local_contacts_for_load = Rc::clone(&local_contacts);
        let nested_model_for_load = nested_model.clone();
        let nested_contacts_for_load = Rc::clone(&nested_contacts);
        let edit_uid_for_nested_load = edit_uid.clone();
        glib::spawn_future_local(async move {
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let result = load_contacts_prefer_eds();
                let _ = sender.send_blocking(result);
            });
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let mut contacts = Vec::new();
            let mut lists = Vec::new();
            for record in result.contacts {
                if record.contact.is_list {
                    if edit_uid_for_nested_load.as_deref() != Some(record.uid.as_str()) {
                        lists.push(record);
                    }
                } else if first_contact_email(&record.contact).is_some() {
                    contacts.push(record);
                }
            }
            let names = contacts
                .iter()
                .map(|record| {
                    format!(
                        "{} · {}",
                        record.contact.fn_name,
                        contact_source_label(&record.source_display_name, &record.source_uid)
                    )
                })
                .collect::<Vec<_>>();
            let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
            local_model_for_load.splice(0, local_model_for_load.n_items(), &name_refs);
            local_contacts_for_load.replace(contacts);
            let list_names = lists
                .iter()
                .map(|record| {
                    format!(
                        "{} · {}",
                        record.contact.fn_name,
                        contact_source_label(&record.source_display_name, &record.source_uid)
                    )
                })
                .collect::<Vec<_>>();
            let list_name_refs = list_names.iter().map(String::as_str).collect::<Vec<_>>();
            nested_model_for_load.splice(0, nested_model_for_load.n_items(), &list_name_refs);
            nested_contacts_for_load.replace(lists);
        });

        let source_uids_for_save = Rc::clone(&source_uids);
        let members_for_save = Rc::clone(&members);
        let nested_contacts_for_save = Rc::clone(&nested_contacts);
        let original_contact_for_save = original_contact.clone();
        let edit_uid_for_save = edit_uid.clone();
        let edit_vcard_for_save = edit_vcard.clone();
        let save_button_for_signal = save_button.clone();
        let save_button_for_closure = save_button.clone();
        let dialog_weak = self.downgrade();
        save_button_for_signal.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let name = name_row.text().trim().to_string();
            if name.is_empty() {
                dialog.show_toast("A contact list requires a name");
                return;
            }
            if let Some((index, error)) = members_for_save
                .borrow()
                .iter()
                .enumerate()
                .find_map(|(index, member)| member.validation_error().map(|error| (index, error)))
            {
                dialog.show_toast(&format!("Member {} is {error}", index + 1));
                return;
            }
            if let Some(current_uid) = edit_uid_for_save.as_deref() {
                let nested_contacts = nested_contacts_for_save.borrow();
                let has_cycle = members_for_save.borrow().iter().any(|member| {
                    matches!(
                        member.kind,
                        VCardListMemberKind::NestedList | VCardListMemberKind::StandardMember
                    ) && !member.contact_uid.trim().is_empty()
                        && nested_list_reaches_uid(
                            &member.contact_uid,
                            current_uid,
                            &nested_contacts,
                            &mut std::collections::HashSet::new(),
                        )
                });
                if has_cycle {
                    dialog.show_toast("Nested contact lists must not form a cycle");
                    return;
                }
            }
            let selected = source_row.selected() as usize;
            let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() else {
                dialog.show_toast("No active address book selected");
                return;
            };
            let mut contact = original_contact_for_save.clone().unwrap_or_else(|| {
                VCardContact::new_list(
                    &name,
                    show_addresses.is_active(),
                    edit_uid_for_save.as_deref(),
                )
            });
            contact.is_list = true;
            contact.fn_name = name.clone();
            contact.name.family.clear();
            contact.name.given = name.clone();
            contact.name.additional.clear();
            contact.name.prefix.clear();
            contact.name.suffix.clear();
            contact.list_name = name.clone();
            contact.list_show_addresses = show_addresses.is_active();
            contact.list_members = members_for_save.borrow().clone();
            contact.email.clear();
            contact.emails.clear();
            contact.email_properties.clear();
            contact.set_extension_value("X-EVOLUTION-FILE-AS", &name);
            if let Some(uid) = edit_uid_for_save.as_deref() {
                contact.set_uid(uid);
            }
            if contact.version.trim().eq_ignore_ascii_case("4.0")
                && contact
                    .list_members
                    .iter()
                    .any(|member| matches!(member.kind, VCardListMemberKind::StandardMember))
                && contact
                    .list_validation_error()
                    .is_some_and(|error| error.contains("KIND:group"))
            {
                contact.ensure_vcard4_group_kind();
            }
            if let Some(error) = contact.list_validation_error() {
                dialog.show_toast(&error);
                return;
            }
            if let (Some(uid), Some(original)) = (
                edit_uid_for_save.as_deref(),
                edit_vcard_for_save.as_deref(),
            ) {
                crate::services::ebook_service::register_known_ebook_vcard(
                    &source_uid,
                    uid,
                    original,
                );
            }
            let vcard = contact.to_vcard_string();
            let source_uid_for_worker = source_uid.clone();
            let operation_uid = edit_uid_for_save.clone();
            let was_edit = edit_vcard_for_save.is_some();
            let (sender, receiver) = async_channel::bounded(1);
            dialog.imp().save_in_progress.set(true);
            save_button_for_closure.set_sensitive(false);
            std::thread::spawn(move || {
                let service = EBookService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => service.modify_contact(&source_uid_for_worker, uid, &vcard),
                    None => service.create_contact(&source_uid_for_worker, &vcard),
                };
                let _ = sender.send_blocking(result);
            });
            let save_button_for_result = save_button_for_closure.clone();
            let dialog_weak_for_result = dialog.downgrade();
            glib::spawn_future_local(async move {
                let Ok(result) = receiver.recv().await else {
                    if let Some(dialog) = dialog_weak_for_result.upgrade() {
                        dialog.imp().save_in_progress.set(false);
                    }
                    save_button_for_result.set_sensitive(true);
                    return;
                };
                if let Some(dialog) = dialog_weak_for_result.upgrade() {
                    dialog.imp().save_in_progress.set(false);
                }
                match result {
                    Ok(result) => {
                        let message = if was_edit {
                            format!("Contact list updated ({})", result.uid)
                        } else {
                            format!("Contact list saved ({})", result.uid)
                        };
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&message);
                            dialog.close();
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&format!(
                                "Could not save contact list: {error}"
                            ));
                        }
                    }
                }
                save_button_for_result.set_sensitive(true);
            });
        });

        let content_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content_box)
            .build();
        toolbar_view.set_content(Some(&content_scroll));
        self.set_child(Some(&toolbar_view));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
        } else {
            tracing::warn!(%message, "EBook dialog result");
        }
    }

    fn guard_close_while_saving(&self) {
        let dialog_weak = self.downgrade();
        self.connect_close_attempt(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                if dialog.imp().save_in_progress.get() {
                    dialog.show_toast("Save in progress – please wait a moment");
                }
            }
        });
    }
}

fn attach_entry_row_date_picker(entry_row: &adw::EntryRow) {
    let popover = gtk::Popover::builder().has_arrow(true).build();
    let pop_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    let cal = gtk::Calendar::new();
    let clear_btn = gtk::Button::builder()
        .label("Clear Date")
        .css_classes(vec!["flat"])
        .build();
    pop_box.append(&cal);
    pop_box.append(&clear_btn);
    popover.set_child(Some(&pop_box));

    let btn = gtk::MenuButton::builder()
        .icon_name("x-office-calendar-symbolic")
        .tooltip_text("Select date in calendar")
        .css_classes(vec!["flat", "circular"])
        .valign(gtk::Align::Center)
        .popover(&popover)
        .build();
    entry_row.add_suffix(&btn);

    let is_syncing = Rc::new(Cell::new(false));
    let is_nav_click = Rc::new(Cell::new(false));
    let cal_weak_click = cal.downgrade();
    let nav_flag_click = Rc::clone(&is_nav_click);
    let click_gesture = gtk::GestureClick::new();
    click_gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    click_gesture.connect_pressed(move |_, _, x, y| {
        let mut clicked_header_btn = false;
        if let Some(cal_widget) = cal_weak_click.upgrade() {
            let mut cur = cal_widget.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(w) = cur {
                if w == *cal_widget.upcast_ref::<gtk::Widget>() {
                    break;
                }
                if w.downcast_ref::<gtk::Button>().is_some() {
                    clicked_header_btn = true;
                    break;
                }
                cur = w.parent();
            }
        }
        nav_flag_click.set(clicked_header_btn);
    });
    cal.add_controller(click_gesture);

    let entry_weak_open = entry_row.downgrade();
    let cal_weak_open = cal.downgrade();
    let is_syncing_open = Rc::clone(&is_syncing);
    popover.connect_show(move |_| {
        if let (Some(row), Some(cal_w)) = (entry_weak_open.upgrade(), cal_weak_open.upgrade()) {
            let text = row.text();
            let parts: Vec<&str> = text.trim().split('-').collect();
            if parts.len() == 3 {
                if let (Ok(y), Ok(m), Ok(d)) = (
                    parts[0].parse::<i32>(),
                    parts[1].parse::<i32>(),
                    parts[2].parse::<i32>(),
                ) {
                    if let Ok(dt) =
                        glib::DateTime::new(&glib::TimeZone::local(), y, m, d, 0, 0, 0.0)
                    {
                        is_syncing_open.set(true);
                        cal_w.select_day(&dt);
                        is_syncing_open.set(false);
                    }
                }
            }
        }
    });

    let entry_weak_sel = entry_row.downgrade();
    let popover_weak_sel = popover.downgrade();
    let is_syncing_sel = Rc::clone(&is_syncing);
    let nav_flag_sel = Rc::clone(&is_nav_click);
    cal.connect_day_selected(move |c| {
        if is_syncing_sel.get() || nav_flag_sel.replace(false) {
            return;
        }
        let dt = c.date();
        if let Some(row) = entry_weak_sel.upgrade() {
            row.set_text(&format!(
                "{:04}-{:02}-{:02}",
                dt.year(),
                dt.month(),
                dt.day_of_month()
            ));
        }
        if let Some(pop) = popover_weak_sel.upgrade() {
            pop.popdown();
        }
    });

    let entry_weak_clear = entry_row.downgrade();
    let popover_weak_clear = popover.downgrade();
    clear_btn.connect_clicked(move |_| {
        if let Some(row) = entry_weak_clear.upgrade() {
            row.set_text("");
        }
        if let Some(pop) = popover_weak_clear.upgrade() {
            pop.popdown();
        }
    });
}

fn refresh_list_member_rows(
    list: &gtk::ListBox,
    members: &Rc<RefCell<Vec<VCardListMember>>>,
    show_addresses: &adw::SwitchRow,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for (index, member) in members.borrow().iter().cloned().enumerate() {
        let subtitle = match member.kind {
            VCardListMemberKind::ExternalAddress => "External Address".to_string(),
            VCardListMemberKind::LocalContact => {
                if member.source_uid.trim().is_empty() {
                    "Local Contact".to_string()
                } else {
                    format!("Local Contact · {}", member.source_uid)
                }
            }
            VCardListMemberKind::NestedList => "Nested Contact List".to_string(),
            VCardListMemberKind::StandardMember => "vCard 4 Member".to_string(),
            VCardListMemberKind::Invalid => "Invalid List Member".to_string(),
        };
        let subtitle = member
            .validation_error()
            .map(|error| format!("{subtitle} · {error}"))
            .unwrap_or(subtitle);
        let row = adw::ActionRow::builder()
            .title(member.display_text(show_addresses.is_active()))
            .subtitle(subtitle)
            .build();
        if !member.is_valid() {
            row.add_css_class("error");
        }
        let remove_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove member")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let move_up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Move member up")
            .css_classes(vec!["flat", "circular"])
            .sensitive(index > 0)
            .valign(gtk::Align::Center)
            .build();
        let move_down = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Move member down")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let member_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .valign(gtk::Align::Center)
            .build();
        member_actions.append(&move_up);
        member_actions.append(&move_down);
        member_actions.append(&remove_button);
        row.add_suffix(&member_actions);
        let move_down_is_last = index + 1 >= members.borrow().len();
        move_down.set_sensitive(!move_down_is_last);
        let members_for_up = Rc::clone(members);
        let list_for_up = list.clone();
        let show_addresses_for_up = show_addresses.clone();
        move_up.connect_clicked(move |_| {
            let mut values = members_for_up.borrow_mut();
            if index > 0 && index < values.len() {
                values.swap(index - 1, index);
            }
            drop(values);
            refresh_list_member_rows(&list_for_up, &members_for_up, &show_addresses_for_up);
        });
        let members_for_down = Rc::clone(members);
        let list_for_down = list.clone();
        let show_addresses_for_down = show_addresses.clone();
        move_down.connect_clicked(move |_| {
            let mut values = members_for_down.borrow_mut();
            if index + 1 < values.len() {
                values.swap(index, index + 1);
            }
            drop(values);
            refresh_list_member_rows(&list_for_down, &members_for_down, &show_addresses_for_down);
        });
        let members_for_remove = Rc::clone(members);
        let list_for_remove = list.clone();
        let show_addresses_for_remove = show_addresses.clone();
        remove_button.connect_clicked(move |_| {
            if index < members_for_remove.borrow().len() {
                members_for_remove.borrow_mut().remove(index);
                refresh_list_member_rows(
                    &list_for_remove,
                    &members_for_remove,
                    &show_addresses_for_remove,
                );
            }
        });
        list.append(&row);
    }
}

fn parse_list_recipient_input(value: &str) -> Option<(String, String)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (display_name, email) = if let Some(start) = value.rfind('<') {
        if value.ends_with('>') {
            (
                value[..start].trim().trim_matches('"').trim().to_string(),
                value[start + 1..value.len() - 1].trim().to_string(),
            )
        } else {
            return None;
        }
    } else if let Some((display, candidate)) = value.rsplit_once(char::is_whitespace) {
        (display.trim().to_string(), candidate.trim().to_string())
    } else {
        (String::new(), value.to_string())
    };
    let member = VCardListMember::external_address(&email, &display_name);
    member.is_valid().then_some((display_name, member.email))
}

fn nested_list_reaches_uid(
    candidate_uid: &str,
    target_uid: &str,
    records: &[EBookContactRecord],
    visited: &mut std::collections::HashSet<String>,
) -> bool {
    if candidate_uid == target_uid {
        return true;
    }
    if !visited.insert(candidate_uid.to_string()) {
        return false;
    }
    let reaches_target = records
        .iter()
        .find(|record| record.uid == candidate_uid && record.contact.is_list)
        .is_some_and(|record| {
            record.contact.list_members.iter().any(|member| {
                matches!(
                    member.kind,
                    VCardListMemberKind::NestedList | VCardListMemberKind::StandardMember
                ) && !member.contact_uid.trim().is_empty()
                    && nested_list_reaches_uid(&member.contact_uid, target_uid, records, visited)
            })
        });
    visited.remove(candidate_uid);
    reaches_target
}

fn first_contact_email(contact: &VCardContact) -> Option<&str> {
    first_contact_email_with_index(contact).map(|(_, email)| email)
}

fn first_contact_email_with_index(contact: &VCardContact) -> Option<(usize, &str)> {
    contact
        .emails
        .iter()
        .enumerate()
        .map(|(index, email)| (index, email.as_str()))
        .chain((!contact.email.trim().is_empty()).then_some((0, contact.email.as_str())))
        .find(|(_, email)| !email.trim().is_empty())
}

fn contact_source_label(display_name: &str, source_uid: &str) -> String {
    if !display_name.trim().is_empty() {
        display_name.trim().to_string()
    } else if source_uid == "local-addressbook" {
        "Local Address Book".to_string()
    } else {
        "Address Book".to_string()
    }
}

fn vcard_property(vcard: &str, name: &str, occurrence: usize) -> Option<String> {
    unfold_vcard_lines(vcard)
        .into_iter()
        .filter_map(|line| {
            let (property, value) = split_vcard_property_value(&line)?;
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
    sip: String,
    address_work: String,
    address_other: String,
    certs: String,
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
        return Err("A contact requires a name".to_string());
    }
    for (label, value) in [
        ("personal email address", email),
        ("work email address", email_work),
        ("additional email address", email_other),
    ] {
        if !value.trim().is_empty() && !is_valid_contact_email(value) {
            return Err(format!("The {label} is invalid"));
        }
    }
    let uid = uid
        .filter(|uid| !uid.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("meridian-{}", uuid::Uuid::new_v4()));
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
    if !fields.sip.trim().is_empty() {
        let sip_val = fields.sip.trim();
        let sip_uri = if sip_val.to_ascii_lowercase().starts_with("sip:") {
            sip_val.to_string()
        } else {
            format!("sip:{sip_val}")
        };
        lines.push(format!("IMPP;TYPE=SIP:{}", escape_vcard_text(&sip_uri)));
    }
    if !fields.address_work.trim().is_empty() {
        lines.push(format!(
            "ADR;TYPE=WORK:;;{};;;;",
            escape_vcard_text(fields.address_work.trim())
        ));
    }
    if !fields.address_other.trim().is_empty() {
        lines.push(format!(
            "ADR;TYPE=OTHER:;;{};;;;",
            escape_vcard_text(fields.address_other.trim())
        ));
    }
    if !fields.certs.trim().is_empty() {
        lines.push(format!(
            "KEY;TYPE=X509:{}",
            escape_vcard_text(fields.certs.trim())
        ));
    }
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
    let folded = lines
        .into_iter()
        .flat_map(fold_dialog_vcard_line)
        .collect::<Vec<_>>();
    Ok(format!("{}\r\n", folded.join("\r\n")))
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
    let generated_photo_count = generated
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter(|line| {
            split_vcard_property_value(line)
                .map(|(property, _)| {
                    property
                        .split(';')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("PHOTO"))
                })
                .unwrap_or(false)
        })
        .count();
    for line in unfold_vcard_lines(original) {
        let Some((property, _)) = split_vcard_property_value(&line) else {
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
                generated_photo_count == 0 || photo_count > generated_photo_count
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
    lines.extend(extras.into_iter().flat_map(fold_dialog_vcard_line));
    lines.push("END:VCARD".to_string());
    format!("{}\r\n", lines.join("\r\n"))
}

fn unfold_vcard_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&line[1..]);
            }
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn fold_dialog_vcard_line(line: String) -> Vec<String> {
    const MAX_OCTETS: usize = 75;
    if line.len() <= MAX_OCTETS {
        return vec![line];
    }
    let mut folded = Vec::new();
    let mut current = String::new();
    for character in line.chars() {
        if current.len() + character.len_utf8() > MAX_OCTETS && !current.is_empty() {
            folded.push(current);
            current = " ".to_string();
        }
        current.push(character);
    }
    if !current.is_empty() {
        folded.push(current);
    }
    folded
}

fn escape_vcard_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn split_vcard_property_value(line: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ':' {
            return Some((&line[..index], &line[index + character.len_utf8()..]));
        }
    }
    None
}

fn is_valid_contact_email(value: &str) -> bool {
    let value = value.trim();
    if value.matches('@').count() != 1
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '<' | '>' | ',' | ';')
        })
    {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
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
        assert_eq!(result, Err("A contact requires a name".to_string()));
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

    #[test]
    fn contact_vcard_rejects_malformed_email_input() {
        let error = build_vcard(
            "Ada Lovelace",
            "",
            "",
            "ada@@example.test",
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
            None,
        )
        .expect_err("malformed email should be rejected");
        assert!(error.contains("email address"));
    }

    #[test]
    fn first_local_email_keeps_the_actual_email_index() {
        let mut contact = VCardContact::new("Ada Lovelace", "", "", "");
        contact.emails = vec![String::new(), "ada.work@example.test".to_string()];
        assert_eq!(
            first_contact_email_with_index(&contact),
            Some((1, "ada.work@example.test"))
        );
    }

    #[test]
    fn nested_list_cycle_detection_stops_recursive_membership() {
        let mut list_a = VCardContact::new_list("A", true, Some("list-a"));
        list_a.list_members = vec![VCardListMember::nested_list("list-b", "B", None)];
        let mut list_b = VCardContact::new_list("B", true, Some("list-b"));
        list_b.list_members = vec![VCardListMember::nested_list("list-a", "A", None)];
        let records = vec![
            EBookContactRecord {
                source_uid: "local-addressbook".to_string(),
                source_display_name: "Lokal".to_string(),
                uid: "list-a".to_string(),
                vcard: list_a.to_vcard_string(),
                contact: list_a,
            },
            EBookContactRecord {
                source_uid: "local-addressbook".to_string(),
                source_display_name: "Lokal".to_string(),
                uid: "list-b".to_string(),
                vcard: list_b.to_vcard_string(),
                contact: list_b,
            },
        ];
        assert!(nested_list_reaches_uid(
            "list-b",
            "list-a",
            &records,
            &mut std::collections::HashSet::new()
        ));
    }

    fn find_entry_by_title(root: &gtk::Widget, target: &str) -> Option<adw::EntryRow> {
        let mut queue = vec![root.clone()];
        while let Some(w) = queue.pop() {
            if let Ok(row) = w.clone().downcast::<adw::EntryRow>() {
                if row.title().contains(target) {
                    return Some(row);
                }
            }
            if let Some(dialog) = w.downcast_ref::<adw::Dialog>() {
                if let Some(c) = dialog.child() {
                    queue.push(c);
                }
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                let next = c.next_sibling();
                queue.push(c);
                child = next;
            }
        }
        None
    }

    #[test]
    fn test_contact_dialog_section_visibility() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else { return; };

        let Some(settings) = AppSettings::try_new() else {
            let dialog = ContactDialog::new();
            let widget = dialog.upcast_ref::<gtk::Widget>();
            let row_phone = find_entry_by_title(widget, "Phone (Mobile)").expect("phone row exists");
            assert!(row_phone.is_visible());
            return;
        };

        // 1. When settings are false, unpopulated rows become hidden
        settings.set_contact_editor_show_phone(false);
        settings.set_contact_editor_show_sip(false);
        settings.set_contact_editor_show_im(false);
        settings.set_contact_editor_show_home_address(false);
        settings.set_contact_editor_show_work_address(false);
        settings.set_contact_editor_show_other_address(false);
        settings.set_contact_editor_show_web(false);
        settings.set_contact_editor_show_job(false);
        settings.set_contact_editor_show_dates(false);
        settings.set_contact_editor_show_misc(false);
        settings.set_contact_editor_show_notes(false);
        settings.set_contact_editor_show_certs(false);

        let dialog = ContactDialog::new();
        while context.iteration(false) {}
        let widget = dialog.upcast_ref::<gtk::Widget>();

        let row_phone = find_entry_by_title(widget, "Phone (Mobile)").expect("phone row exists");
        assert!(!row_phone.is_visible(), "Empty phone row must be hidden when show-phone is false");

        let row_sip = find_entry_by_title(widget, "SIP / VoIP Address").expect("sip row exists");
        assert!(!row_sip.is_visible(), "Empty SIP row must be hidden when show-sip is false");

        let row_note = find_entry_by_title(widget, "Note").expect("note row exists");
        assert!(!row_note.is_visible(), "Empty note row must be hidden when show-notes is false");

        let row_certs = find_entry_by_title(widget, "S/MIME Certificate").expect("certs row exists");
        assert!(!row_certs.is_visible(), "Empty certs row must be hidden when show-certs is false");

        // 2. Dynamic update: re-enabling via GSettings updates row visibility
        settings.set_contact_editor_show_phone(true);
        settings.set_contact_editor_show_sip(true);
        settings.set_contact_editor_show_notes(true);
        while context.iteration(false) {}

        assert!(row_phone.is_visible(), "Phone row should become visible dynamically");
        assert!(row_sip.is_visible(), "SIP row should become visible dynamically");
        assert!(row_note.is_visible(), "Note row should become visible dynamically");

        // 3. Non-lossy display: populated data must ALWAYS remain visible even if preference is false
        settings.set_contact_editor_show_phone(false);
        settings.set_contact_editor_show_notes(false);
        settings.set_contact_editor_show_sip(false);
        settings.set_contact_editor_show_certs(false);

        let mut contact = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        contact.tel = "+44 1234 56789".to_string();
        contact.note = "Pioneer of computing".to_string();
        contact.impps = vec!["sip:ada@sip.example.test".to_string()];
        let record = EBookContactRecord {
            source_uid: "local-addressbook".to_string(),
            source_display_name: "Local".to_string(),
            uid: "ada-1".to_string(),
            vcard: contact.to_vcard_string(),
            contact,
        };

        let edit_dialog = ContactDialog::new_for_edit(&record);
        while context.iteration(false) {}
        let edit_widget = edit_dialog.upcast_ref::<gtk::Widget>();

        let edit_phone = find_entry_by_title(edit_widget, "Phone (Mobile)").expect("edit phone row");
        let edit_note = find_entry_by_title(edit_widget, "Note").expect("edit note row");
        let edit_sip = find_entry_by_title(edit_widget, "SIP / VoIP Address").expect("edit sip row");

        assert!(edit_phone.is_visible(), "Populated phone row must remain visible to prevent data loss");
        assert!(edit_note.is_visible(), "Populated note row must remain visible to prevent data loss");
        assert!(edit_sip.is_visible(), "Populated SIP row must remain visible to prevent data loss");

        // Reset settings back to true
        settings.set_contact_editor_show_phone(true);
        settings.set_contact_editor_show_sip(true);
        settings.set_contact_editor_show_im(true);
        settings.set_contact_editor_show_home_address(true);
        settings.set_contact_editor_show_work_address(true);
        settings.set_contact_editor_show_other_address(true);
        settings.set_contact_editor_show_web(true);
        settings.set_contact_editor_show_job(true);
        settings.set_contact_editor_show_dates(true);
        settings.set_contact_editor_show_misc(true);
        settings.set_contact_editor_show_notes(true);
        settings.set_contact_editor_show_certs(true);
    }

    #[test]
    fn test_adversarial_contact_dialog_all_12_sections_non_lossy_and_dynamic() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else { return; };

        let Some(settings) = AppSettings::try_new() else { return; };

        let set_all_toggles = |val: bool| {
            settings.set_contact_editor_show_phone(val);
            settings.set_contact_editor_show_sip(val);
            settings.set_contact_editor_show_im(val);
            settings.set_contact_editor_show_home_address(val);
            settings.set_contact_editor_show_work_address(val);
            settings.set_contact_editor_show_other_address(val);
            settings.set_contact_editor_show_web(val);
            settings.set_contact_editor_show_job(val);
            settings.set_contact_editor_show_dates(val);
            settings.set_contact_editor_show_misc(val);
            settings.set_contact_editor_show_notes(val);
            settings.set_contact_editor_show_certs(val);
        };

        // --- TEST PART 1: ALL 12 TOGGLES FALSE -> EMPTY CONTACT HIDES ALL 12 SECTIONS ---
        set_all_toggles(false);

        let empty_dialog = ContactDialog::new();
        while context.iteration(false) {}
        let empty_widget = empty_dialog.upcast_ref::<gtk::Widget>();

        let row_phone = find_entry_by_title(empty_widget, "Phone (Mobile)").expect("row_phone");
        let row_phone_work = find_entry_by_title(empty_widget, "Phone (Work)").expect("row_phone_work");
        let row_phone_home = find_entry_by_title(empty_widget, "Phone (Home)").expect("row_phone_home");
        let row_sip = find_entry_by_title(empty_widget, "SIP / VoIP Address").expect("row_sip");
        let row_impp = find_entry_by_title(empty_widget, "Instant Messaging (IMPP)").expect("row_impp");
        let row_address = find_entry_by_title(empty_widget, "Home Address").expect("row_address");
        let row_address_work = find_entry_by_title(empty_widget, "Work Address").expect("row_address_work");
        let row_address_other = find_entry_by_title(empty_widget, "Other Address").expect("row_address_other");
        let row_url = find_entry_by_title(empty_widget, "Website").expect("row_url");
        let row_org = find_entry_by_title(empty_widget, "Company / Organization").expect("row_org");
        let row_birthday = find_entry_by_title(empty_widget, "Birthday (YYYY-MM-DD)").expect("row_birthday");
        let row_anniversary = find_entry_by_title(empty_widget, "Anniversary (YYYY-MM-DD)").expect("row_anniversary");
        let row_nickname = find_entry_by_title(empty_widget, "Nickname").expect("row_nickname");
        let row_note = find_entry_by_title(empty_widget, "Note").expect("row_note");
        let row_certs = find_entry_by_title(empty_widget, "S/MIME Certificate").expect("row_certs");

        assert!(!row_phone.is_visible(), "Empty phone row must be hidden");
        assert!(!row_phone_work.is_visible(), "Empty work phone row must be hidden");
        assert!(!row_phone_home.is_visible(), "Empty home phone row must be hidden");
        assert!(!row_sip.is_visible(), "Empty SIP row must be hidden");
        assert!(!row_impp.is_visible(), "Empty IMPP row must be hidden");
        assert!(!row_address.is_visible(), "Empty home address row must be hidden");
        assert!(!row_address_work.is_visible(), "Empty work address row must be hidden");
        assert!(!row_address_other.is_visible(), "Empty other address row must be hidden");
        assert!(!row_url.is_visible(), "Empty url row must be hidden");
        assert!(!row_org.is_visible(), "Empty org row must be hidden");
        assert!(!row_birthday.is_visible(), "Empty birthday row must be hidden");
        assert!(!row_anniversary.is_visible(), "Empty anniversary row must be hidden");
        assert!(!row_nickname.is_visible(), "Empty nickname row must be hidden");
        assert!(!row_note.is_visible(), "Empty note row must be hidden");
        assert!(!row_certs.is_visible(), "Empty certs row must be hidden");

        let row_name = find_entry_by_title(empty_widget, "Full Name").expect("row_name");
        let row_email1 = find_entry_by_title(empty_widget, "Email (Personal)").expect("row_email1");
        assert!(row_name.is_visible(), "Name row must always be visible");
        assert!(row_email1.is_visible(), "Email row must always be visible");

        // --- TEST PART 2: DYNAMIC LIVE TOGGLING WHILE DIALOG IS OPEN ---
        settings.set_contact_editor_show_phone(true);
        while context.iteration(false) {}
        assert!(row_phone.is_visible(), "Phone should become visible dynamically");

        settings.set_contact_editor_show_sip(true);
        while context.iteration(false) {}
        assert!(row_sip.is_visible(), "SIP should become visible dynamically");

        settings.set_contact_editor_show_im(true);
        while context.iteration(false) {}
        assert!(row_impp.is_visible(), "IMPP should become visible dynamically");

        settings.set_contact_editor_show_home_address(true);
        while context.iteration(false) {}
        assert!(row_address.is_visible(), "Home address should become visible dynamically");

        settings.set_contact_editor_show_work_address(true);
        while context.iteration(false) {}
        assert!(row_address_work.is_visible(), "Work address should become visible dynamically");

        settings.set_contact_editor_show_other_address(true);
        while context.iteration(false) {}
        assert!(row_address_other.is_visible(), "Other address should become visible dynamically");

        settings.set_contact_editor_show_web(true);
        while context.iteration(false) {}
        assert!(row_url.is_visible(), "Web should become visible dynamically");

        settings.set_contact_editor_show_job(true);
        while context.iteration(false) {}
        assert!(row_org.is_visible(), "Job should become visible dynamically");

        settings.set_contact_editor_show_dates(true);
        while context.iteration(false) {}
        assert!(row_birthday.is_visible(), "Dates should become visible dynamically");

        settings.set_contact_editor_show_misc(true);
        while context.iteration(false) {}
        assert!(row_nickname.is_visible(), "Misc should become visible dynamically");

        settings.set_contact_editor_show_notes(true);
        while context.iteration(false) {}
        assert!(row_note.is_visible(), "Notes should become visible dynamically");

        settings.set_contact_editor_show_certs(true);
        while context.iteration(false) {}
        assert!(row_certs.is_visible(), "Certs should become visible dynamically");

        // --- TEST PART 3: CRITICAL NON-LOSSY INVARIANT ---
        set_all_toggles(false);

        let mut populated = VCardContact::new("Grace Hopper", "grace@navy.mil", "", "");
        populated.tel = "+1-555-0100".to_string();
        populated.impps = vec![
            "sip:grace@voip.navy.mil".to_string(),
            "xmpp:grace@chat.navy.mil".to_string(),
        ];
        populated.addresses = vec![
            "100 Navy Pier, Arlington".to_string(),
            "Pentagon Room 2B, Washington DC".to_string(),
            "PO Box 99, Annapolis".to_string(),
        ];
        populated.urls = vec!["https://en.wikipedia.org/wiki/Grace_Hopper".to_string()];
        populated.org = "United States Navy".to_string();
        populated.title = "Rear Admiral".to_string();
        populated.role = "Computer Scientist".to_string();
        populated.birthday = "1906-12-09".to_string();
        populated.anniversary = "1930-06-15".to_string();
        populated.nickname = "Amazing Grace".to_string();
        populated.note = "First compiler developer".to_string();
        populated.unknown_properties.push(crate::services::vcard_service::VCardProperty {
            property: "KEY".to_string(),
            value: "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA".to_string(),
            raw_value: "KEY:MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA".to_string(),
        });

        let full_record = EBookContactRecord {
            source_uid: "local-addressbook".to_string(),
            source_display_name: "Local".to_string(),
            uid: "grace-hopper-1".to_string(),
            vcard: populated.to_vcard_string(),
            contact: populated,
        };

        let edit_dialog = ContactDialog::new_for_edit(&full_record);
        while context.iteration(false) {}
        let edit_widget = edit_dialog.upcast_ref::<gtk::Widget>();

        let e_phone = find_entry_by_title(edit_widget, "Phone (Mobile)").expect("e_phone");
        let e_sip = find_entry_by_title(edit_widget, "SIP / VoIP Address").expect("e_sip");
        let e_impp = find_entry_by_title(edit_widget, "Instant Messaging (IMPP)").expect("e_impp");
        let e_address = find_entry_by_title(edit_widget, "Home Address").expect("e_address");
        let e_address_work = find_entry_by_title(edit_widget, "Work Address").expect("e_address_work");
        let e_address_other = find_entry_by_title(edit_widget, "Other Address").expect("e_address_other");
        let e_url = find_entry_by_title(edit_widget, "Website").expect("e_url");
        let e_org = find_entry_by_title(edit_widget, "Company / Organization").expect("e_org");
        let e_birthday = find_entry_by_title(edit_widget, "Birthday (YYYY-MM-DD)").expect("e_birthday");
        let e_anniversary = find_entry_by_title(edit_widget, "Anniversary (YYYY-MM-DD)").expect("e_anniversary");
        let e_nickname = find_entry_by_title(edit_widget, "Nickname").expect("e_nickname");
        let e_note = find_entry_by_title(edit_widget, "Note").expect("e_note");
        let e_certs = find_entry_by_title(edit_widget, "S/MIME Certificate").expect("e_certs");

        assert!(e_phone.is_visible(), "Populated phone MUST remain visible despite toggle=false");
        assert!(e_sip.is_visible(), "Populated SIP MUST remain visible despite toggle=false");
        assert!(e_impp.is_visible(), "Populated IMPP MUST remain visible despite toggle=false");
        assert!(e_address.is_visible(), "Populated home address MUST remain visible despite toggle=false");
        assert!(e_address_work.is_visible(), "Populated work address MUST remain visible despite toggle=false");
        assert!(e_address_other.is_visible(), "Populated other address MUST remain visible despite toggle=false");
        assert!(e_url.is_visible(), "Populated URL MUST remain visible despite toggle=false");
        assert!(e_org.is_visible(), "Populated Job MUST remain visible despite toggle=false");
        assert!(e_birthday.is_visible(), "Populated Birthday MUST remain visible despite toggle=false");
        assert!(e_anniversary.is_visible(), "Populated Anniversary MUST remain visible despite toggle=false");
        assert!(e_nickname.is_visible(), "Populated Misc MUST remain visible despite toggle=false");
        assert!(e_note.is_visible(), "Populated Note MUST remain visible despite toggle=false");
        assert!(e_certs.is_visible(), "Populated Certs MUST remain visible despite toggle=false");

        // --- TEST PART 4: DYNAMIC RETENTION OF TYPED TEXT ---
        set_all_toggles(true);
        let live_dialog = ContactDialog::new();
        while context.iteration(false) {}
        let live_widget = live_dialog.upcast_ref::<gtk::Widget>();

        let live_sip = find_entry_by_title(live_widget, "SIP / VoIP Address").expect("live_sip");
        let live_notes = find_entry_by_title(live_widget, "Note").expect("live_notes");
        let live_impp = find_entry_by_title(live_widget, "Instant Messaging (IMPP)").expect("live_impp");

        live_sip.set_text("sip:incoming@test.local");
        live_notes.set_text("Don't hide me!");
        assert_eq!(live_impp.text().as_str(), "");

        set_all_toggles(false);
        while context.iteration(false) {}

        assert!(live_sip.is_visible(), "SIP row with entered text MUST remain visible after toggle=false");
        assert!(live_notes.is_visible(), "Notes row with entered text MUST remain visible after toggle=false");
        assert!(!live_impp.is_visible(), "Empty IMPP row MUST become hidden after toggle=false");

        set_all_toggles(true);
    }
}
