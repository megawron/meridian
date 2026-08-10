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

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for ContactDialog {
        const NAME: &'static str = "EvolutionContactDialog";
        type Type = super::ContactDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for ContactDialog {
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

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let btn_save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        btn_save.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                tracing::warn!(
                    "Contact save requested, but the EBook/EDS backend is not connected"
                );
                let toast = adw::Toast::new(
                    "Speichern ist noch nicht verbunden — der Kontakt bleibt geöffnet",
                );
                if let Some(parent) = dialog.transient_for() {
                    if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                        window.add_toast(&toast);
                    }
                }
            }
        ));
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

        content_box.append(&group_extra);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));
    }
}

impl Default for ContactDialog {
    fn default() -> Self {
        Self::new()
    }
}
