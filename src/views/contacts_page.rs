// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactsPage — Address book view with search filtering and dynamic contact detail view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::ebook_service::{
    EBookChangeKind, EBookContactRecord, EBookDataOrigin, EBookService, EBookSyncEvent,
    load_contacts_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::vcard_service::VCardContact;

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactsPage {
        pub ebook_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    }

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
            if let Some(cancel) = self.ebook_monitor_cancel.borrow_mut().take() {
                use std::sync::atomic::Ordering;
                cancel.store(true, Ordering::Relaxed);
            }
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

        let stored_contacts = Rc::new(RefCell::new(Vec::<EBookContactRecord>::new()));
        let selected_contact = Rc::new(RefCell::new(None::<EBookContactRecord>));

        contact_list.set_filter_func(move |_row| true);

        refresh_contacts(
            contact_list.clone(),
            Rc::clone(&stored_contacts),
            empty_state.clone(),
            Rc::clone(&selected_contact),
        );

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

        let contact_list_for_new = contact_list.clone();
        let stored_contacts_for_new = Rc::clone(&stored_contacts);
        let empty_state_for_new = empty_state.clone();
        let selected_contact_for_new = Rc::clone(&selected_contact);
        new_contact_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::ContactDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let contact_list_for_close = contact_list_for_new.clone();
                let stored_contacts_for_close = Rc::clone(&stored_contacts_for_new);
                let empty_state_for_close = empty_state_for_new.clone();
                let selected_contact_for_close = Rc::clone(&selected_contact_for_new);
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                    );
                    glib::Propagation::Proceed
                });
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

        let selected_contact_for_edit = Rc::clone(&selected_contact);
        let selected_contact_for_edit_refresh = Rc::clone(&selected_contact);
        let contact_list_for_edit = contact_list.clone();
        let stored_contacts_for_edit = Rc::clone(&stored_contacts);
        let empty_state_for_edit = empty_state.clone();
        edit_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(record) = selected_contact_for_edit.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if record.source_uid.is_empty() {
                    page.show_toast(
                        "Lokale vCards können ohne aktives EDS-Adressbuch nicht geändert werden",
                    );
                    return;
                }
                let dialog = crate::dialogs::ContactDialog::new_for_edit(&record);
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let contact_list_for_close = contact_list_for_edit.clone();
                let stored_contacts_for_close = Rc::clone(&stored_contacts_for_edit);
                let empty_state_for_close = empty_state_for_edit.clone();
                let selected_contact_for_close = Rc::clone(&selected_contact_for_edit_refresh);
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));

        let delete_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Löschen")
            .css_classes(vec!["circular", "destructive-action"])
            .build();

        let selected_contact_for_delete = Rc::clone(&selected_contact);
        let stored_contacts_for_delete = Rc::clone(&stored_contacts);
        let contact_list_for_delete = contact_list.clone();
        let detail_name_for_delete = detail_name.clone();
        let detail_title_for_delete = detail_title.clone();
        let detail_avatar_for_delete = detail_avatar.clone();
        delete_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(contact) = selected_contact_for_delete.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if contact.source_uid.is_empty() {
                    page.show_toast(
                        "Lokale vCards können ohne aktives EDS-Adressbuch nicht gelöscht werden",
                    );
                    return;
                }

                let confirmation = adw::AlertDialog::new(
                    Some("Kontakt löschen?"),
                    Some("Der Kontakt wird dauerhaft aus dem EDS-Adressbuch entfernt."),
                );
                confirmation.add_responses(&[("cancel", "Abbrechen"), ("delete", "Löschen")]);
                confirmation
                    .set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                let confirmation_parent = detail_name_for_delete.clone();
                let selected_contact_for_response = Rc::clone(&selected_contact_for_delete);
                let stored_contacts_for_response = Rc::clone(&stored_contacts_for_delete);
                let contact_list_for_response = contact_list_for_delete.clone();
                let detail_name_for_response = detail_name_for_delete.clone();
                let detail_title_for_response = detail_title_for_delete.clone();
                let detail_avatar_for_response = detail_avatar_for_delete.clone();
                let page_weak_for_response = page.downgrade();
                confirmation.connect_response(
                    None,
                    move |dialog: &adw::AlertDialog, response: &str| {
                        if response != "delete" {
                            return;
                        }
                        let source_uid = contact.source_uid.clone();
                        let contact_uid = contact.uid.clone();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result =
                                EBookService::new().remove_contact(&source_uid, &contact_uid);
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Contact deletion result was dropped");
                            }
                        });

                        let dialog_for_result = dialog.clone();
                        let contact_for_result = contact.clone();
                        let selected_contact_for_result = Rc::clone(&selected_contact_for_response);
                        let stored_contacts_for_result = Rc::clone(&stored_contacts_for_response);
                        let contact_list_for_result = contact_list_for_response.clone();
                        let detail_name_for_result = detail_name_for_response.clone();
                        let detail_title_for_result = detail_title_for_response.clone();
                        let detail_avatar_for_result = detail_avatar_for_response.clone();
                        let page_weak_for_result = page_weak_for_response.clone();
                        glib::spawn_future_local(async move {
                            match receiver.recv().await {
                                Ok(Ok(result)) => {
                                    let uid = result.uid;
                                    let key = (contact_for_result.source_uid.clone(), uid.clone());
                                    if let Some(index) = stored_contacts_for_result
                                        .borrow()
                                        .iter()
                                        .position(|item| {
                                            (item.source_uid.clone(), item.uid.clone()) == key
                                        })
                                    {
                                        if let Some(row) =
                                            contact_list_for_result.row_at_index(index as i32)
                                        {
                                            contact_list_for_result.remove(&row);
                                        }
                                        stored_contacts_for_result.borrow_mut().remove(index);
                                    }
                                    *selected_contact_for_result.borrow_mut() = None;
                                    detail_avatar_for_result.set_text(Some(""));
                                    detail_name_for_result.set_label("Kein Kontakt ausgewählt");
                                    detail_title_for_result.set_label(
                                        "Adressbuchdaten werden nach der EDS-Anbindung angezeigt",
                                    );
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        page.show_toast(&format!("Kontakt gelöscht ({uid})"));
                                    }
                                    dialog_for_result.close();
                                }
                                Ok(Err(error)) => {
                                    show_toast_for_widget(
                                        &detail_name_for_result,
                                        &format!("Kontakt konnte nicht gelöscht werden: {error}"),
                                    );
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "Contact deletion channel closed");
                                }
                            }
                        });
                    },
                );
                confirmation.present(Some(&confirmation_parent));
            }
        ));

        actions_box.append(&email_btn);
        actions_box.append(&edit_btn);
        actions_box.append(&delete_btn);

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
        let selected_contact_for_selection = Rc::clone(&selected_contact);
        contact_list.connect_row_selected(glib::clone!(
            #[weak]
            detail_avatar,
            #[weak]
            detail_name,
            #[weak]
            detail_title,
            #[weak]
            email_row1,
            #[strong]
            selected_contact_for_selection,
            move |_, row| {
                let Some(row) = row else {
                    *selected_contact_for_selection.borrow_mut() = None;
                    return;
                };
                unsafe {
                    let read_data =
                        |key: &str| row.data::<String>(key).map(|value| value.as_ref().clone());
                    let Some(vcard) = read_data("contact-vcard") else {
                        return;
                    };
                    let Some(contact) = VCardContact::parse_vcard_string(&vcard) else {
                        return;
                    };
                    let record = EBookContactRecord {
                        source_uid: read_data("contact-source-uid").unwrap_or_default(),
                        source_display_name: read_data("contact-source-name").unwrap_or_default(),
                        uid: read_data("contact-id").unwrap_or_default(),
                        vcard,
                        contact,
                    };
                    detail_avatar.set_text(Some(&record.contact.fn_name));
                    detail_name.set_label(&record.contact.fn_name);
                    email_row1.set_title(if record.contact.email.is_empty() {
                        "Keine E-Mail-Adresse"
                    } else {
                        &record.contact.email
                    });
                    email_row1.set_subtitle(if record.contact.tel.is_empty() {
                        "Keine Telefonnummer"
                    } else {
                        &record.contact.tel
                    });
                    detail_title.set_label(if record.contact.title.is_empty() {
                        &record.contact.org
                    } else {
                        &record.contact.title
                    });
                    *selected_contact_for_selection.borrow_mut() = Some(record);
                }
            }
        ));

        if let Some((cancel, receiver)) = EBookService::new().start_change_monitor() {
            *self.imp().ebook_monitor_cancel.borrow_mut() = Some(cancel);
            let contact_list_for_monitor = contact_list.clone();
            let stored_contacts_for_monitor = Rc::clone(&stored_contacts);
            let empty_state_for_monitor = empty_state.clone();
            let selected_contact_for_monitor = Rc::clone(&selected_contact);
            let detail_avatar_for_monitor = detail_avatar.clone();
            let detail_name_for_monitor = detail_name.clone();
            let detail_title_for_monitor = detail_title.clone();
            let email_row_for_monitor = email_row1.clone();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        EBookSyncEvent::Ready(status) => {
                            if let Some(warning) = status.warning {
                                tracing::warn!(
                                    configured = status.configured_sources,
                                    active = status.active_sources,
                                    failed = status.failed_sources,
                                    %warning,
                                    "EBook signal monitor started with source warnings"
                                );
                            }
                        }
                        EBookSyncEvent::Change(change) => match change.kind {
                            EBookChangeKind::Removed => {
                                let removed = remove_contact_row(
                                    &contact_list_for_monitor,
                                    Rc::clone(&stored_contacts_for_monitor),
                                    &change.source_uid,
                                    &change.uid,
                                );
                                if removed
                                    && selected_contact_for_monitor.borrow().as_ref().is_some_and(
                                        |contact| {
                                            contact.source_uid == change.source_uid
                                                && contact.uid == change.uid
                                        },
                                    )
                                {
                                    *selected_contact_for_monitor.borrow_mut() = None;
                                    detail_avatar_for_monitor.set_text(Some(""));
                                    detail_name_for_monitor.set_label("Kein Kontakt ausgewählt");
                                    detail_title_for_monitor.set_label(
                                        "Adressbuchdaten werden nach der EDS-Anbindung angezeigt",
                                    );
                                    email_row_for_monitor.set_title("Keine E-Mail-Adresse");
                                    email_row_for_monitor.set_subtitle("Kein Kontakt ausgewählt");
                                }
                            }
                            EBookChangeKind::Added | EBookChangeKind::Modified => {
                                if let Some(record) = parse_change_record(&change) {
                                    upsert_contact_row(
                                        &contact_list_for_monitor,
                                        Rc::clone(&stored_contacts_for_monitor),
                                        record.clone(),
                                    );
                                    if selected_contact_for_monitor.borrow().as_ref().is_some_and(
                                        |selected| {
                                            selected.source_uid == record.source_uid
                                                && selected.uid == record.uid
                                        },
                                    ) {
                                        *selected_contact_for_monitor.borrow_mut() =
                                            Some(record.clone());
                                        update_contact_detail(
                                            &record,
                                            &detail_avatar_for_monitor,
                                            &detail_name_for_monitor,
                                            &detail_title_for_monitor,
                                            &email_row_for_monitor,
                                        );
                                    }
                                }
                            }
                        },
                        EBookSyncEvent::Error(error) => {
                            tracing::warn!(%error, "EBook signal monitor stopped");
                            break;
                        }
                    }
                    empty_state_for_monitor
                        .set_visible(stored_contacts_for_monitor.borrow().is_empty());
                }
            });
        }

        let content_page = adw::NavigationPage::builder()
            .title("Kontakt-Details")
            .child(&detail_box)
            .build();

        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(280.0);
        split.set_max_sidebar_width(400.0);
        split.set_parent(self);
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Contact page result");
    }
}

fn refresh_contacts(
    contact_list: gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    empty_state: gtk::Label,
    selected_contact: Rc<RefCell<Option<EBookContactRecord>>>,
) {
    while let Some(row) = contact_list.row_at_index(0) {
        contact_list.remove(&row);
    }
    stored_contacts.borrow_mut().clear();
    *selected_contact.borrow_mut() = None;
    empty_state.set_label("Kontakte werden aus EDS geladen…");
    empty_state.set_visible(true);

    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = load_contacts_prefer_eds();
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Contact refresh result was dropped");
        }
    });
    glib::spawn_future_local(async move {
        match receiver.recv().await {
            Ok(result) => {
                if let Some(warning) = result.warning {
                    tracing::warn!(%warning, "Contact refresh completed with an EBook warning");
                }
                if result.contacts.is_empty() {
                    empty_state.set_label(match result.origin {
                        EBookDataOrigin::Eds => "Keine Kontakte aus EDS geladen",
                        EBookDataOrigin::Local => "Keine lokalen vCards geladen",
                    });
                    return;
                }
                let origin = match result.origin {
                    EBookDataOrigin::Eds => "EDS",
                    EBookDataOrigin::Local => "lokalen vCards",
                };
                empty_state.set_label(&format!(
                    "{} Kontakt(e) aus {origin} geladen.",
                    result.contacts.len()
                ));
                for record in result.contacts {
                    upsert_contact_row(&contact_list, Rc::clone(&stored_contacts), record);
                }
            }
            Err(error) => {
                empty_state.set_label(&format!("Kontakte konnten nicht geladen werden: {error}"));
            }
        }
    });
}

fn upsert_contact_row(
    contact_list: &gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    record: EBookContactRecord,
) {
    let existing_index = stored_contacts.borrow().iter().position(|existing| {
        existing.source_uid == record.source_uid && existing.uid == record.uid
    });
    if let Some(index) = existing_index {
        stored_contacts.borrow_mut().remove(index);
        if let Some(row) = contact_list.row_at_index(index as i32) {
            contact_list.remove(&row);
        }
    }
    append_contact_row(contact_list, &record);
    stored_contacts.borrow_mut().push(record);
}

fn remove_contact_row(
    contact_list: &gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    source_uid: &str,
    uid: &str,
) -> bool {
    let Some(index) = stored_contacts
        .borrow()
        .iter()
        .position(|contact| contact.source_uid == source_uid && contact.uid == uid)
    else {
        return false;
    };
    stored_contacts.borrow_mut().remove(index);
    if let Some(row) = contact_list.row_at_index(index as i32) {
        contact_list.remove(&row);
    }
    true
}

fn update_contact_detail(
    record: &EBookContactRecord,
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    email_row: &adw::ActionRow,
) {
    detail_avatar.set_text(Some(&record.contact.fn_name));
    detail_name.set_label(&record.contact.fn_name);
    email_row.set_title(if record.contact.email.is_empty() {
        "Keine E-Mail-Adresse"
    } else {
        &record.contact.email
    });
    email_row.set_subtitle(if record.contact.tel.is_empty() {
        "Keine Telefonnummer"
    } else {
        &record.contact.tel
    });
    detail_title.set_label(if record.contact.title.is_empty() {
        &record.contact.org
    } else {
        &record.contact.title
    });
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Contact UI result");
}

fn append_contact_row(contact_list: &gtk::ListBox, record: &EBookContactRecord) {
    let contact = &record.contact;
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    unsafe {
        row.set_data("contact-name", contact.fn_name.clone());
        row.set_data("contact-email", contact.email.clone());
        row.set_data("contact-title", contact.title.clone());
        row.set_data("contact-id", record.uid.clone());
        row.set_data("contact-source-uid", record.source_uid.clone());
        row.set_data("contact-source-name", record.source_display_name.clone());
        row.set_data("contact-vcard", record.vcard.clone());
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
