// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactsPage — Address book view with search filtering and dynamic contact detail view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::contact_store_service::{ContactStoreService, LocalContactRecord};
use crate::services::eds_service;
use crate::services::vcard_service::VCardContact;

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactsPage;

    #[glib::object_subclass]
    impl ObjectSubclass for ContactsPage {
        const NAME: &'static str = "EvolutionContactsPage";
        type Type = super::ContactsPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for ContactsPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ContactsPage {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct ContactsPage(ObjectSubclass<imp::ContactsPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ContactsPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        let split = adw::NavigationSplitView::new();

        // ── Contact List (Sidebar) ──────────────────────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Kontakte durchsuchen…")
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        sidebar_box.append(&search_entry);

        let book_model = gtk::StringList::new(&["Alle Kontakte"]);
        let book_selector = gtk::DropDown::new(Some(book_model.clone()), None::<&gtk::Expression>);
        book_selector.set_margin_start(8);
        book_selector.set_margin_end(8);
        book_selector.set_margin_bottom(8);
        sidebar_box.append(&book_selector);

        let empty_state = gtk::Label::builder()
            .label("Noch kein Adressbuch aus EDS geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        sidebar_box.append(&empty_state);

        let book_model_for_load = book_model.clone();
        let empty_state_for_load = empty_state.clone();
        glib::spawn_future_local(async move {
            match eds_service::list_address_book_sources().await {
                Ok(sources) if sources.is_empty() => {
                    empty_state_for_load.set_label("Keine Adressbuchquellen in EDS gefunden.");
                }
                Ok(sources) => {
                    let source_count = sources.len();
                    for source in sources {
                        book_model_for_load.append(&source.display_name);
                    }
                    empty_state_for_load.set_label(&format!(
                        "{source_count} Adressbuchquelle(n) aus EDS gefunden; Kontakte werden nach dem EBook-Stream geladen."
                    ));
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load address book sources from EDS");
                    empty_state_for_load
                        .set_label(&format!("EDS-Adressbücher sind nicht erreichbar: {error}"));
                }
            }
        });

        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let contact_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        let local_contacts = Rc::new(RefCell::new(Vec::<LocalContactRecord>::new()));

        contact_list.set_filter_func(move |_row| true);

        search_entry.connect_search_changed(glib::clone!(
            #[weak]
            contact_list,
            move |entry| {
                let query = entry.text().to_lowercase();
                if query.is_empty() {
                    contact_list.set_filter_func(|_| true);
                } else {
                    contact_list.set_filter_func(move |row| {
                        if let Some(child) = row.child() {
                            if let Ok(hbox) = child.downcast::<gtk::Box>() {
                                if let Some(info_box) = hbox.last_child().and_downcast::<gtk::Box>()
                                {
                                    if let Some(name_lbl) =
                                        info_box.first_child().and_downcast::<gtk::Label>()
                                    {
                                        if name_lbl.label().to_lowercase().contains(&query) {
                                            return true;
                                        }
                                    }
                                    if let Some(email_lbl) =
                                        info_box.last_child().and_downcast::<gtk::Label>()
                                    {
                                        if email_lbl.label().to_lowercase().contains(&query) {
                                            return true;
                                        }
                                    }
                                }
                            }
                        }
                        false
                    });
                }
            }
        ));

        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&contact_list)
            .vexpand(true)
            .build();

        sidebar_box.append(&list_scroll);

        let contact_list_for_load = contact_list.clone();
        let empty_state_for_contact_load = empty_state.clone();
        let local_contacts_for_load = Rc::clone(&local_contacts);
        let (contact_sender, contact_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let contacts = ContactStoreService::new().load_contacts();
            if let Err(error) = contact_sender.send_blocking(contacts) {
                tracing::debug!(%error, "Contact snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match contact_receiver.recv().await {
                Ok(contacts) => {
                    if contacts.is_empty() {
                        return;
                    }
                    empty_state_for_contact_load.set_label(&format!(
                        "{} lokale(r) Kontakt(e) aus vCard geladen; EBook-Quellen folgen.",
                        contacts.len()
                    ));
                    let mut stored_contacts = local_contacts_for_load.borrow_mut();
                    for record in contacts {
                        append_contact_row(&contact_list_for_load, &record.contact, &record.id);
                        stored_contacts.push(record);
                    }
                }
                Err(error) => tracing::debug!(%error, "Local contact snapshot unavailable"),
            }
        });

        let new_contact_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let new_contact_btn = gtk::Button::builder()
            .label("Neuer Kontakt")
            .icon_name("contact-new-symbolic")
            .css_classes(vec!["suggested-action"])
            .hexpand(true)
            .build();

        new_contact_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::ContactDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                dialog.present();
            }
        ));

        new_contact_bar.append(&new_contact_btn);
        sidebar_box.append(&new_contact_bar);

        let sidebar_page = adw::NavigationPage::builder()
            .title("Kontakte")
            .child(&sidebar_box)
            .build();

        split.set_sidebar(Some(&sidebar_page));

        // ── Contact Detail (Content) ────────────────────────────────────

        let detail_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(24)
            .margin_end(24)
            .margin_top(32)
            .margin_bottom(24)
            .halign(gtk::Align::Center)
            .build();

        let detail_avatar = adw::Avatar::builder()
            .size(96)
            .text("")
            .show_initials(true)
            .build();

        let detail_name = gtk::Label::builder()
            .label("Kein Kontakt ausgewählt")
            .css_classes(vec!["title-1"])
            .build();

        let detail_title = gtk::Label::builder()
            .label("Adressbuchdaten werden nach der EDS-Anbindung angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .build();

        header_box.append(&detail_avatar);
        header_box.append(&detail_name);
        header_box.append(&detail_title);

        let actions_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Center)
            .margin_bottom(16)
            .build();

        let email_btn = gtk::Button::builder()
            .icon_name("mail-message-new-symbolic")
            .tooltip_text("E-Mail senden")
            .action_name("app.compose")
            .css_classes(vec!["circular", "suggested-action"])
            .build();

        let edit_btn = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Bearbeiten")
            .css_classes(vec!["circular"])
            .build();

        edit_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::ContactDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                dialog.present();
            }
        ));

        actions_box.append(&email_btn);
        actions_box.append(&edit_btn);

        header_box.append(&actions_box);
        detail_box.append(&header_box);
        detail_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let info_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let info_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_start(24)
            .margin_end(24)
            .margin_top(24)
            .margin_bottom(24)
            .build();

        let email_group = adw::PreferencesGroup::builder().title("E-Mail").build();

        let email_row1 = adw::ActionRow::builder()
            .title("Keine E-Mail-Adresse")
            .subtitle("Kein Kontakt ausgewählt")
            .build();
        let _copy_btn1 = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .tooltip_text("Kopieren")
            .build();
        email_row1.add_suffix(&_copy_btn1);
        email_group.add(&email_row1);

        info_box.append(&email_group);
        info_scroll.set_child(Some(&info_box));
        detail_box.append(&info_scroll);

        // Selection connection
        contact_list.connect_row_selected(glib::clone!(
            #[weak]
            detail_avatar,
            #[weak]
            detail_name,
            #[weak]
            detail_title,
            #[weak]
            email_row1,
            move |_, row| {
                if let Some(row) = row {
                    unsafe {
                        if let Some(name) = row.data::<String>("contact-name") {
                            detail_avatar.set_text(Some(name.as_ref()));
                            detail_name.set_label(name.as_ref());
                        }
                        if let Some(email) = row.data::<String>("contact-email") {
                            email_row1.set_title(email.as_ref());
                        }
                        if let Some(title) = row.data::<String>("contact-title") {
                            detail_title.set_label(title.as_ref());
                        }
                    }
                }
            }
        ));

        let content_page = adw::NavigationPage::builder()
            .title("Kontakt-Details")
            .child(&detail_box)
            .build();

        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(280.0);
        split.set_max_sidebar_width(400.0);
        split.set_parent(self);
    }
}

fn append_contact_row(contact_list: &gtk::ListBox, contact: &VCardContact, id: &str) {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    unsafe {
        row.set_data("contact-name", contact.fn_name.clone());
        row.set_data("contact-email", contact.email.clone());
        row.set_data("contact-title", contact.title.clone());
        row.set_data("contact-id", id.to_string());
    }

    let row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let avatar = adw::Avatar::builder()
        .size(40)
        .text(&contact.fn_name)
        .show_initials(true)
        .build();
    row_box.append(&avatar);

    let info_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .build();
    let name = gtk::Label::builder()
        .label(&contact.fn_name)
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let email = gtk::Label::builder()
        .label(if contact.email.is_empty() {
            "Keine E-Mail-Adresse"
        } else {
            &contact.email
        })
        .halign(gtk::Align::Start)
        .css_classes(vec!["dim-label"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    info_box.append(&name);
    info_box.append(&email);
    row_box.append(&info_box);
    row.set_child(Some(&row_box));
    contact_list.append(&row);
}

impl Default for ContactsPage {
    fn default() -> Self {
        Self::new()
    }
}
