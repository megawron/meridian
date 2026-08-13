// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactsPage — Address book view with search filtering and dynamic contact detail view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::services::ebook_service::{
    EBookChangeKind, EBookContactRecord, EBookDataOrigin, EBookService, EBookSyncEvent,
    load_contacts_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;
use crate::services::vcard_service::VCardContact;
use crate::views::accessibility::set_accessible_label;

const LOCAL_ADDRESS_BOOK_SOURCE_UID: &str = "local-addressbook";

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactsPage {
        pub ebook_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub new_contact_button: RefCell<Option<gtk::Button>>,
        pub contact_list: RefCell<Option<gtk::ListBox>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
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

    impl WidgetImpl for ContactsPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
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

    /// Trigger the same contact editor as the page-local toolbar button.  The
    /// main Evolution window uses this to keep menu, header, and page actions
    /// on one persistent list-refresh path.
    pub fn present_new_contact(&self) {
        if let Some(button) = self.imp().new_contact_button.borrow().as_ref() {
            button.activate();
        }
    }

    pub(crate) fn focus_search(&self) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn select_search_result(&self, source_uid: &str, uid: &str) -> bool {
        let Some(contact_list) = self.imp().contact_list.borrow().as_ref().cloned() else {
            return false;
        };
        select_contact_row(&contact_list, source_uid, uid)
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
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(4)
            .build();
        self.imp().search_entry.replace(Some(search_entry.clone()));
        sidebar_box.append(&search_entry);

        let book_model = gtk::StringList::new(&["Alle Kontakte"]);
        let book_selector = gtk::DropDown::new(Some(book_model.clone()), None::<&gtk::Expression>);
        book_selector.set_tooltip_text(Some(
            "Die Auswahl wird als Standardadressbuch für den nächsten Start gespeichert",
        ));
        book_selector.set_margin_start(8);
        book_selector.set_margin_end(8);
        book_selector.set_margin_bottom(8);
        // The visible source list below is the Evolution-like navigation.
        // Keep the model-backed dropdown only as the internal selection
        // coordinator so settings and keyboard-driven selection share one
        // source of truth without rendering the same list twice.
        book_selector.set_visible(false);
        sidebar_box.append(&book_selector);

        let book_header = gtk::Label::builder()
            .label("Adressbücher")
            .css_classes(vec!["title-5"])
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_top(4)
            .margin_bottom(2)
            .build();
        sidebar_box.append(&book_header);
        let book_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        sidebar_box.append(&book_source_list);
        let book_selector_for_sidebar = book_selector.clone();
        book_source_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                book_selector_for_sidebar.set_selected(row.index() as u32);
            }
        });

        let category_model = gtk::StringList::new(&["Alle Kategorien"]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some("Kontaktkategorie auswählen"));
        category_selector.set_margin_start(8);
        category_selector.set_margin_end(8);
        category_selector.set_margin_bottom(8);
        sidebar_box.append(&category_selector);

        let search_query = Rc::new(RefCell::new(String::new()));
        let selected_book = Rc::new(RefCell::new(String::new()));
        let book_source_uids = Rc::new(RefCell::new(vec![String::new()]));
        let selected_category = Rc::new(RefCell::new(String::new()));
        let duplicate_only = Rc::new(Cell::new(false));

        let duplicate_filter_row = gtk::CheckButton::builder()
            .label("Nur Dubletten")
            .tooltip_text("Gleiche E-Mail, Telefonnummer oder Name ohne Kontaktweg anzeigen")
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&duplicate_filter_row);

        let empty_state = gtk::Label::builder()
            .label("Noch kein synchronisiertes Adressbuch geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        sidebar_box.append(&empty_state);

        let book_model_for_load = book_model.clone();
        let book_source_uids_for_load = Rc::clone(&book_source_uids);
        let book_selector_for_load = book_selector.clone();
        let book_source_list_for_load = book_source_list.clone();
        let empty_state_for_load = empty_state.clone();
        let default_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_address_book_source_uid())
            .unwrap_or_default();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::AddressBook).await;
            let status = discovery.status_text();
            let mut sources = discovery.sources;
            sources.retain(|source| source.enabled);
            sources.sort_by_cached_key(|source| {
                (source.display_name.to_lowercase(), source.uid.clone())
            });

            let mut names = vec!["Alle Kontakte".to_string()];
            let mut uids = vec![String::new()];
            for source in sources {
                names.push(contact_source_label(&source.display_name, &source.uid));
                uids.push(source.uid);
            }
            if !default_source_uid.trim().is_empty()
                && !uids.iter().any(|uid| uid == &default_source_uid)
            {
                names.push("Nicht verfügbare Quelle".to_string());
                uids.push(default_source_uid.clone());
            }
            if crate::services::contact_store_service::ContactStoreService::new().is_writable() {
                names.push("Lokales Adressbuch".to_string());
                uids.push(LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string());
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            book_model_for_load.splice(0, book_model_for_load.n_items(), &name_refs);
            book_source_uids_for_load.replace(uids.clone());
            while let Some(child) = book_source_list_for_load.first_child() {
                book_source_list_for_load.remove(&child);
            }
            for (index, name) in names.iter().enumerate() {
                let escaped = glib::markup_escape_text(name);
                let row = adw::ActionRow::builder()
                    .title(escaped.as_str())
                    .use_markup(true)
                    .build();
                let image = gtk::Image::from_icon_name(if index == 0 {
                    "view-list-symbolic"
                } else {
                    "x-office-address-book-symbolic"
                });
                row.add_prefix(&image);
                book_source_list_for_load.append(&row);
            }
            let selected_index = uids
                .iter()
                .position(|uid| uid == &default_source_uid)
                .unwrap_or(0) as u32;
            book_selector_for_load.set_selected(selected_index);
            if let Some(row) = book_source_list_for_load.row_at_index(selected_index as i32) {
                book_source_list_for_load.select_row(Some(&row));
            }
            empty_state_for_load.set_label(&format!("{status}; Kontakte werden geladen."));
        });

        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let contact_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        contact_list.set_sort_func(|left, right| unsafe {
            let value = |row: &gtk::ListBoxRow, key: &str| {
                row.data::<String>(key)
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default()
            };
            value(left, "contact-sort-key")
                .cmp(&value(right, "contact-sort-key"))
                .then_with(|| value(left, "contact-email").cmp(&value(right, "contact-email")))
                .then_with(|| {
                    value(left, "contact-source-uid").cmp(&value(right, "contact-source-uid"))
                })
                .then_with(|| value(left, "contact-id").cmp(&value(right, "contact-id")))
                .into()
        });
        self.imp().contact_list.replace(Some(contact_list.clone()));

        let stored_contacts = Rc::new(RefCell::new(Vec::<EBookContactRecord>::new()));
        let selected_contact = Rc::new(RefCell::new(None::<EBookContactRecord>));

        set_contact_filter(
            &contact_list,
            Rc::clone(&search_query),
            Rc::clone(&selected_book),
            Rc::clone(&selected_category),
            Rc::clone(&duplicate_only),
        );

        refresh_contacts(
            contact_list.clone(),
            Rc::clone(&stored_contacts),
            empty_state.clone(),
            Rc::clone(&selected_contact),
            category_model.clone(),
            category_selector.clone(),
            Rc::clone(&selected_category),
        );

        let search_query_for_filter = Rc::clone(&search_query);
        let selected_book_for_search = Rc::clone(&selected_book);
        let selected_category_for_search = Rc::clone(&selected_category);
        let duplicate_only_for_search = Rc::clone(&duplicate_only);
        search_entry.connect_search_changed(glib::clone!(
            #[weak]
            contact_list,
            move |entry| {
                *search_query_for_filter.borrow_mut() = normalize_contact_query(&entry.text());
                set_contact_filter(
                    &contact_list,
                    Rc::clone(&search_query_for_filter),
                    Rc::clone(&selected_book_for_search),
                    Rc::clone(&selected_category_for_search),
                    Rc::clone(&duplicate_only_for_search),
                );
            }
        ));

        let selected_book_for_filter = Rc::clone(&selected_book);
        let search_query_for_book = Rc::clone(&search_query);
        let selected_category_for_book = Rc::clone(&selected_category);
        let duplicate_only_for_book = Rc::clone(&duplicate_only);
        let book_source_uids_for_filter = Rc::clone(&book_source_uids);
        let book_source_list_for_filter = book_source_list.clone();
        book_selector.connect_selected_notify(glib::clone!(
            #[weak]
            contact_list,
            move |selector| {
                let selected = book_source_uids_for_filter
                    .borrow()
                    .get(selector.selected() as usize)
                    .cloned()
                    .unwrap_or_default();
                *selected_book_for_filter.borrow_mut() = selected.clone();
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_default_address_book_source_uid(&selected);
                }
                if let Some(row) =
                    book_source_list_for_filter.row_at_index(selector.selected() as i32)
                {
                    book_source_list_for_filter.select_row(Some(&row));
                }
                set_contact_filter(
                    &contact_list,
                    Rc::clone(&search_query_for_book),
                    Rc::clone(&selected_book_for_filter),
                    Rc::clone(&selected_category_for_book),
                    Rc::clone(&duplicate_only_for_book),
                );
            }
        ));

        if let Some(settings) = AppSettings::try_new() {
            let book_selector_for_settings = book_selector.clone();
            let book_source_uids_for_settings = Rc::clone(&book_source_uids);
            settings.connect_changed(Some("default-address-book-source"), move |settings, _| {
                let value = settings.string("default-address-book-source");
                let Some(selected) = book_source_uids_for_settings
                    .borrow()
                    .iter()
                    .position(|uid| uid == value.as_str())
                else {
                    return;
                };
                book_selector_for_settings.set_selected(selected as u32);
            });
        }

        let search_query_for_duplicates = Rc::clone(&search_query);
        let selected_book_for_duplicates = Rc::clone(&selected_book);
        let selected_category_for_duplicates = Rc::clone(&selected_category);
        let duplicate_only_for_duplicates = Rc::clone(&duplicate_only);
        duplicate_filter_row.connect_active_notify(glib::clone!(
            #[weak]
            contact_list,
            move |row| {
                duplicate_only_for_duplicates.set(row.is_active());
                set_contact_filter(
                    &contact_list,
                    Rc::clone(&search_query_for_duplicates),
                    Rc::clone(&selected_book_for_duplicates),
                    Rc::clone(&selected_category_for_duplicates),
                    Rc::clone(&duplicate_only_for_duplicates),
                );
            }
        ));

        let selected_category_for_selector = Rc::clone(&selected_category);
        let search_query_for_category = Rc::clone(&search_query);
        let selected_book_for_category = Rc::clone(&selected_book);
        let duplicate_only_for_category = Rc::clone(&duplicate_only);
        let category_model_for_selector = category_model.clone();
        category_selector.connect_selected_notify(glib::clone!(
            #[weak]
            contact_list,
            move |selector| {
                let category = if selector.selected() == 0 {
                    String::new()
                } else {
                    category_model_for_selector
                        .string(selector.selected())
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                };
                selected_category_for_selector.replace(category);
                set_contact_filter(
                    &contact_list,
                    Rc::clone(&search_query_for_category),
                    Rc::clone(&selected_book_for_category),
                    Rc::clone(&selected_category_for_selector),
                    Rc::clone(&duplicate_only_for_category),
                );
            }
        ));

        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Always)
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
        let category_model_for_new = category_model.clone();
        let category_selector_for_new = category_selector.clone();
        let selected_category_for_new = Rc::clone(&selected_category);
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
                let category_model_for_close = category_model_for_new.clone();
                let category_selector_for_close = category_selector_for_new.clone();
                let selected_category_for_close = Rc::clone(&selected_category_for_new);
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));
        self.imp()
            .new_contact_button
            .replace(Some(new_contact_btn.clone()));

        new_contact_bar.append(&new_contact_btn);
        sidebar_box.append(&new_contact_bar);
        sidebar_box.append(&crate::views::build_sidebar_module_switcher(
            "contacts",
            &sidebar_box,
        ));

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
            .spacing(8)
            .margin_start(20)
            .margin_end(20)
            .margin_top(20)
            .margin_bottom(16)
            .halign(gtk::Align::Center)
            .build();

        let detail_avatar = adw::Avatar::builder()
            .size(72)
            .text("")
            .show_initials(true)
            .build();

        let detail_name = gtk::Label::builder()
            .label("Kein Kontakt ausgewählt")
            .css_classes(vec!["title-1"])
            .build();

        let detail_title = gtk::Label::builder()
            .label("Adressbuchdaten werden nach der Synchronisation angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .build();

        let detail_extra = gtk::Label::builder()
            .label("Keine weiteren Kontaktdaten")
            .halign(gtk::Align::Start)
            .wrap(true)
            .selectable(true)
            .css_classes(vec!["body", "dim-label"])
            .build();

        header_box.append(&detail_avatar);
        header_box.append(&detail_name);
        header_box.append(&detail_title);

        let actions_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_bottom(12)
            .build();

        let email_row1 = adw::ActionRow::builder()
            .title("Keine E-Mail-Adresse")
            .subtitle("Kein Kontakt ausgewählt")
            .build();

        let email_btn = gtk::Button::builder()
            .icon_name("mail-message-new-symbolic")
            .tooltip_text("E-Mail senden")
            .css_classes(vec!["circular", "suggested-action"])
            .build();
        set_accessible_label(&email_btn, "E-Mail an den Kontakt senden");

        let selected_contact_for_email = Rc::clone(&selected_contact);
        email_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(record) = selected_contact_for_email.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                let recipient = contact_email_text(&record.contact);
                if recipient.trim().is_empty() {
                    page.show_toast("Der ausgewählte Kontakt hat keine E-Mail-Adresse");
                    return;
                }
                let dialog = crate::dialogs::ComposeDialog::to_recipient(&recipient);
                if let Some(window) = page.root().and_downcast::<gtk::Window>() {
                    dialog.set_transient_for(Some(&window));
                }
                dialog.present();
            }
        ));

        let edit_btn = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Bearbeiten")
            .css_classes(vec!["circular"])
            .build();
        set_accessible_label(&edit_btn, "Kontakt bearbeiten");

        let selected_contact_for_edit = Rc::clone(&selected_contact);
        let selected_contact_for_edit_refresh = Rc::clone(&selected_contact);
        let contact_list_for_edit = contact_list.clone();
        let stored_contacts_for_edit = Rc::clone(&stored_contacts);
        let empty_state_for_edit = empty_state.clone();
        let category_model_for_edit = category_model.clone();
        let category_selector_for_edit = category_selector.clone();
        let selected_category_for_edit = Rc::clone(&selected_category);
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
                        "Dieser Kontakt stammt aus einer schreibgeschützten importierten Quelle",
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
                let category_model_for_close = category_model_for_edit.clone();
                let category_selector_for_close = category_selector_for_edit.clone();
                let selected_category_for_close = Rc::clone(&selected_category_for_edit);
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));

        let merge_btn = gtk::Button::builder()
            .icon_name("object-select-symbolic")
            .tooltip_text("Dubletten derselben Quelle zusammenführen")
            .css_classes(vec!["circular"])
            .sensitive(false)
            .build();
        set_accessible_label(&merge_btn, "Dubletten des Kontakts zusammenführen");

        let selected_contact_for_merge = Rc::clone(&selected_contact);
        let stored_contacts_for_merge = Rc::clone(&stored_contacts);
        let contact_list_for_merge = contact_list.clone();
        let detail_name_for_merge = detail_name.clone();
        let detail_avatar_for_merge = detail_avatar.clone();
        let detail_title_for_merge = detail_title.clone();
        let detail_extra_for_merge = detail_extra.clone();
        let email_row_for_merge = email_row1.clone();
        let category_model_for_merge = category_model.clone();
        let category_selector_for_merge = category_selector.clone();
        let selected_category_for_merge = Rc::clone(&selected_category);
        let merge_btn_for_result = merge_btn.clone();
        merge_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(primary) = selected_contact_for_merge.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if primary.source_uid.is_empty() {
                    page.show_toast(
                        "Dieser Kontakt stammt aus einer schreibgeschützten importierten Quelle",
                    );
                    return;
                }

                let duplicates = stored_contacts_for_merge
                    .borrow()
                    .iter()
                    .filter(|candidate| contact_records_are_duplicates(&primary, candidate))
                    .cloned()
                    .collect::<Vec<_>>();
                if duplicates.is_empty() {
                    page.show_toast("Für diesen Kontakt wurden keine Dubletten gefunden");
                    merge_btn_for_result.set_sensitive(false);
                    return;
                }

                let confirmation_message = format!(
                    "{} Duplikat(e) werden in „{}“ übernommen. Die Primärkarte bleibt erhalten; die Duplikate werden anschließend dauerhaft aus diesem Adressbuch entfernt.",
                    duplicates.len(),
                    primary.contact.fn_name
                );
                let primary_for_action = primary.clone();
                let duplicates_for_action = duplicates.clone();
                let selected_contact_for_action = Rc::clone(&selected_contact_for_merge);
                let stored_contacts_for_action = Rc::clone(&stored_contacts_for_merge);
                let contact_list_for_action = contact_list_for_merge.clone();
                let detail_avatar_for_action = detail_avatar_for_merge.clone();
                let detail_name_for_action = detail_name_for_merge.clone();
                let detail_title_for_action = detail_title_for_merge.clone();
                let detail_extra_for_action = detail_extra_for_merge.clone();
                let email_row_for_action = email_row_for_merge.clone();
                let category_model_for_action = category_model_for_merge.clone();
                let category_selector_for_action = category_selector_for_merge.clone();
                let selected_category_for_action = Rc::clone(&selected_category_for_merge);
                let merge_btn_for_action = merge_btn_for_result.clone();
                let page_weak_for_action = page.downgrade();
                crate::dialogs::run_destructive_action(
                    &detail_name_for_merge,
                    "Dubletten zusammenführen?",
                    &confirmation_message,
                    "Zusammenführen",
                    move || {
                        let primary_for_worker = primary_for_action.clone();
                        let duplicates_for_worker = duplicates_for_action.clone();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = EBookService::new()
                                .merge_contacts(&primary_for_worker, &duplicates_for_worker);
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Contact merge result was dropped");
                            }
                        });

                        let page_weak_for_result = page_weak_for_action.clone();
                        glib::spawn_future_local(async move {
                            match receiver.recv().await {
                                Ok(Ok(result)) => {
                                    let Some(vcard) = result.vcard else {
                                        show_toast_for_widget(
                                            &detail_name_for_action,
                                            "Zusammenführung ohne bestätigte vCard vom Backend",
                                        );
                                        return;
                                    };
                                    let Some(contact) =
                                        VCardContact::parse_vcard_string(&vcard)
                                    else {
                                        show_toast_for_widget(
                                            &detail_name_for_action,
                                            "Die zusammengeführte vCard konnte nicht gelesen werden",
                                        );
                                        return;
                                    };
                                    let merged = EBookContactRecord {
                                        source_uid: primary_for_action.source_uid.clone(),
                                        source_display_name: primary_for_action
                                            .source_display_name
                                            .clone(),
                                        uid: confirmed_contact_uid(
                                            &result.uid,
                                            &primary_for_action.uid,
                                        ),
                                        vcard,
                                        contact,
                                    };
                                    remove_contact_list_row(
                                        &contact_list_for_action,
                                        &primary_for_action.source_uid,
                                        &primary_for_action.uid,
                                    );
                                    for duplicate in &duplicates_for_action {
                                        remove_contact_list_row(
                                            &contact_list_for_action,
                                            &duplicate.source_uid,
                                            &duplicate.uid,
                                        );
                                    }
                                    {
                                        let mut contacts = stored_contacts_for_action.borrow_mut();
                                        contacts.retain(|candidate| {
                                            candidate.source_uid != primary_for_action.source_uid
                                                || (candidate.uid != primary_for_action.uid
                                                    && !duplicates_for_action.iter().any(|duplicate| {
                                                        duplicate.source_uid == candidate.source_uid
                                                            && duplicate.uid == candidate.uid
                                                    }))
                                        });
                                        contacts.push(merged.clone());
                                    }
                                    append_contact_row(&contact_list_for_action, &merged);
                                    mark_duplicate_rows(
                                        &contact_list_for_action,
                                        &stored_contacts_for_action,
                                    );
                                    refresh_contact_category_model(
                                        &category_model_for_action,
                                        &category_selector_for_action,
                                        &stored_contacts_for_action,
                                        &selected_category_for_action,
                                    );
                                    update_contact_detail(
                                        &merged,
                                        &detail_avatar_for_action,
                                        &detail_name_for_action,
                                        &detail_title_for_action,
                                        &detail_extra_for_action,
                                        &email_row_for_action,
                                    );
                                    *selected_contact_for_action.borrow_mut() = Some(merged.clone());
                                    merge_btn_for_action.set_sensitive(
                                        contact_has_mergeable_duplicate(
                                            &merged,
                                            &stored_contacts_for_action.borrow(),
                                        ),
                                    );
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        page.show_toast(&format!(
                                            "{} Dublette(n) in „{}“ zusammengeführt",
                                            duplicates_for_action.len(),
                                            merged.contact.fn_name
                                        ));
                                    }
                                }
                                Ok(Err(error)) => show_toast_for_widget(
                                    &detail_name_for_action,
                                    &format!("Dubletten konnten nicht zusammengeführt werden: {error}"),
                                ),
                                Err(error) => tracing::debug!(%error, "Contact merge channel closed"),
                            }
                        });
                    },
                );
            }
        ));

        let delete_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Löschen")
            .css_classes(vec!["circular", "destructive-action"])
            .build();
        set_accessible_label(&delete_btn, "Kontakt löschen");

        let export_btn = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Kontakt als vCard exportieren")
            .css_classes(vec!["circular"])
            .build();
        set_accessible_label(&export_btn, "Kontakt als vCard exportieren");
        let selected_contact_for_export = Rc::clone(&selected_contact);
        export_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(record) = selected_contact_for_export.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                let Some(root) = page.root().and_downcast::<gtk::Window>() else {
                    page.show_toast("Kein Fenster für den Export verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Kontakt als vCard speichern")
                    .initial_name("kontakt.vcf")
                    .modal(true)
                    .build();
                let page_weak = page.downgrade();
                file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let save_result = file.replace_contents(
                        record.vcard.as_bytes(),
                        None,
                        true,
                        gio::FileCreateFlags::NONE,
                        None::<&gio::Cancellable>,
                    );
                    let Some(page) = page_weak.upgrade() else {
                        return;
                    };
                    match save_result {
                        Ok(_) => page.show_toast("Kontakt als vCard exportiert"),
                        Err(error) => page
                            .show_toast(&format!("vCard konnte nicht gespeichert werden: {error}")),
                    }
                });
            }
        ));

        let selected_contact_for_delete = Rc::clone(&selected_contact);
        let stored_contacts_for_delete = Rc::clone(&stored_contacts);
        let contact_list_for_delete = contact_list.clone();
        let detail_name_for_delete = detail_name.clone();
        let detail_title_for_delete = detail_title.clone();
        let detail_avatar_for_delete = detail_avatar.clone();
        let detail_extra_for_delete = detail_extra.clone();
        let empty_state_for_delete = empty_state.clone();
        let category_model_for_delete = category_model.clone();
        let category_selector_for_delete = category_selector.clone();
        let selected_category_for_delete = Rc::clone(&selected_category);
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
                        "Dieser Kontakt stammt aus einer schreibgeschützten importierten Quelle",
                    );
                    return;
                }

                let source_name = contact_source_label(
                    &contact.source_display_name,
                    &contact.source_uid,
                );
                let confirmation_message = if contact.source_uid == LOCAL_ADDRESS_BOOK_SOURCE_UID {
                    format!(
                        "Der Kontakt wird dauerhaft aus dem lokalen Adressbuch („{source_name}“) entfernt."
                    )
                } else {
                    format!(
                        "Der Kontakt wird dauerhaft aus dem synchronisierten Adressbuch „{source_name}“ entfernt."
                    )
                };
                let contact_for_action = contact.clone();
                let selected_contact_for_action = Rc::clone(&selected_contact_for_delete);
                let stored_contacts_for_action = Rc::clone(&stored_contacts_for_delete);
                let contact_list_for_action = contact_list_for_delete.clone();
                let detail_name_for_action = detail_name_for_delete.clone();
                let detail_title_for_action = detail_title_for_delete.clone();
                let detail_avatar_for_action = detail_avatar_for_delete.clone();
                let detail_extra_for_action = detail_extra_for_delete.clone();
                let empty_state_for_action = empty_state_for_delete.clone();
                let category_model_for_action = category_model_for_delete.clone();
                let category_selector_for_action = category_selector_for_delete.clone();
                let selected_category_for_action = Rc::clone(&selected_category_for_delete);
                let page_weak_for_action = page.downgrade();
                crate::dialogs::run_destructive_action(
                    &detail_name_for_delete,
                    "Kontakt löschen?",
                    &confirmation_message,
                    "Löschen",
                    move || {
                        let source_uid = contact_for_action.source_uid.clone();
                        let contact_uid = contact_for_action.uid.clone();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result =
                                EBookService::new().remove_contact(&source_uid, &contact_uid);
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Contact deletion result was dropped");
                            }
                        });

                        let contact_for_result = contact_for_action.clone();
                        let selected_contact_for_result = Rc::clone(&selected_contact_for_action);
                        let stored_contacts_for_result = Rc::clone(&stored_contacts_for_action);
                        let contact_list_for_result = contact_list_for_action.clone();
                        let detail_name_for_result = detail_name_for_action.clone();
                        let detail_title_for_result = detail_title_for_action.clone();
                        let detail_avatar_for_result = detail_avatar_for_action.clone();
                        let detail_extra_for_result = detail_extra_for_action.clone();
                        let page_weak_for_result = page_weak_for_action.clone();
                        glib::spawn_future_local(async move {
                            match receiver.recv().await {
                                Ok(Ok(result)) => {
                                    let uid = result.uid;
                                    let uid = confirmed_contact_uid(&uid, &contact_for_result.uid);
                                    let key = (contact_for_result.source_uid.clone(), uid.clone());
                                    if let Some(index) = stored_contacts_for_result
                                        .borrow()
                                        .iter()
                                        .position(|item| {
                                            (item.source_uid.clone(), item.uid.clone()) == key
                                        })
                                    {
                                        remove_contact_list_row(
                                            &contact_list_for_result,
                                            &contact_for_result.source_uid,
                                            &uid,
                                        );
                                        stored_contacts_for_result.borrow_mut().remove(index);
                                    }
                                    mark_duplicate_rows(
                                        &contact_list_for_result,
                                        &stored_contacts_for_result,
                                    );
                                    empty_state_for_action.set_visible(
                                        stored_contacts_for_result.borrow().is_empty(),
                                    );
                                    if stored_contacts_for_result.borrow().is_empty() {
                                        empty_state_for_action
                                            .set_label("Keine Kontakte im Adressbuch geladen");
                                    }
                                    refresh_contact_category_model(
                                        &category_model_for_action,
                                        &category_selector_for_action,
                                        &stored_contacts_for_result,
                                        &selected_category_for_action,
                                    );
                                    *selected_contact_for_result.borrow_mut() = None;
                                    detail_avatar_for_result.set_text(Some(""));
                                    detail_avatar_for_result
                                        .set_custom_image(None::<&gdk::Texture>);
                                    detail_name_for_result.set_label("Kein Kontakt ausgewählt");
                                    detail_title_for_result.set_label(
                                        "Adressbuchdaten werden nach der Synchronisation angezeigt",
                                    );
                                    detail_extra_for_result
                                        .set_label("Keine weiteren Kontaktdaten");
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        page.show_toast(&format!("Kontakt gelöscht ({uid})"));
                                    }
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
            }
        ));

        // Match Evolution's contact-row context menu with the actions already
        // exposed in the detail header.  Selecting the row first also makes
        // the menu useful when the split view is collapsed on a narrow window.
        let email_btn_for_context = email_btn.clone();
        let edit_btn_for_context = edit_btn.clone();
        let export_btn_for_context = export_btn.clone();
        let merge_btn_for_context = merge_btn.clone();
        let delete_btn_for_context = delete_btn.clone();
        let context_click = gtk::GestureClick::builder().button(3).build();
        let contact_list_weak_for_context = contact_list.downgrade();
        context_click.connect_pressed(move |_, _, x, y| {
            let Some(contact_list_for_context) = contact_list_weak_for_context.upgrade() else {
                return;
            };
            let Some(row) = contact_list_for_context.row_at_y(y as i32) else {
                return;
            };
            contact_list_for_context.select_row(Some(&row));

            let popover = gtk::Popover::builder()
                .has_arrow(false)
                .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
                .build();
            let menu_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_start(6)
                .margin_end(6)
                .margin_top(6)
                .margin_bottom(6)
                .build();

            let email = gtk::Button::with_label("E-Mail senden");
            email.set_halign(gtk::Align::Fill);
            email.add_css_class("flat");
            let popover_for_email = popover.clone();
            let email_button_for_menu = email_btn_for_context.clone();
            email.connect_clicked(move |_| {
                popover_for_email.popdown();
                email_button_for_menu.activate();
            });
            menu_box.append(&email);

            let edit = gtk::Button::with_label("Kontakt bearbeiten");
            edit.set_halign(gtk::Align::Fill);
            edit.add_css_class("flat");
            let popover_for_edit = popover.clone();
            let edit_button_for_menu = edit_btn_for_context.clone();
            edit.connect_clicked(move |_| {
                popover_for_edit.popdown();
                edit_button_for_menu.activate();
            });
            menu_box.append(&edit);

            let export = gtk::Button::with_label("Als vCard exportieren");
            export.set_halign(gtk::Align::Fill);
            export.add_css_class("flat");
            let popover_for_export = popover.clone();
            let export_button_for_menu = export_btn_for_context.clone();
            export.connect_clicked(move |_| {
                popover_for_export.popdown();
                export_button_for_menu.activate();
            });
            menu_box.append(&export);

            let merge = gtk::Button::with_label("Dubletten zusammenführen");
            merge.set_halign(gtk::Align::Fill);
            merge.add_css_class("flat");
            let popover_for_merge = popover.clone();
            let merge_button_for_menu = merge_btn_for_context.clone();
            merge.connect_clicked(move |_| {
                popover_for_merge.popdown();
                merge_button_for_menu.activate();
            });
            menu_box.append(&merge);

            let delete = gtk::Button::with_label("Kontakt löschen");
            delete.set_halign(gtk::Align::Fill);
            delete.add_css_class("flat");
            delete.add_css_class("destructive-action");
            let popover_for_delete = popover.clone();
            let delete_button_for_menu = delete_btn_for_context.clone();
            delete.connect_clicked(move |_| {
                popover_for_delete.popdown();
                delete_button_for_menu.activate();
            });
            menu_box.append(&delete);

            popover.set_child(Some(&menu_box));
            popover.set_parent(&contact_list_for_context);
            let popover_weak = popover.downgrade();
            popover.connect_closed(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.unparent();
                }
            });
            popover.popup();
        });
        contact_list.add_controller(context_click);

        actions_box.append(&email_btn);
        actions_box.append(&export_btn);
        actions_box.append(&edit_btn);
        actions_box.append(&merge_btn);
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
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let email_group = adw::PreferencesGroup::builder().title("E-Mail").build();

        let copy_btn1 = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .tooltip_text("Kopieren")
            .build();
        set_accessible_label(&copy_btn1, "E-Mail-Adresse kopieren");
        let selected_contact_for_copy = Rc::clone(&selected_contact);
        copy_btn1.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |button| {
                let Some(record) = selected_contact_for_copy.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                let email = contact_email_text(&record.contact);
                if email.trim().is_empty() {
                    page.show_toast("Keine E-Mail-Adresse zum Kopieren vorhanden");
                    return;
                }
                button.display().clipboard().set_text(&email);
                page.show_toast("E-Mail-Adresse kopiert");
            }
        ));
        email_row1.add_suffix(&copy_btn1);
        email_group.add(&email_row1);

        info_box.append(&email_group);
        info_box.append(&detail_extra);
        info_scroll.set_child(Some(&info_box));
        detail_box.append(&info_scroll);

        // Selection connection
        let split_for_selection = split.clone();
        let selected_contact_for_selection = Rc::clone(&selected_contact);
        let stored_contacts_for_selection = Rc::clone(&stored_contacts);
        contact_list.connect_row_selected(glib::clone!(
            #[weak]
            detail_avatar,
            #[weak]
            detail_name,
            #[weak]
            detail_title,
            #[weak]
            detail_extra,
            #[weak]
            email_row1,
            #[weak]
            merge_btn,
            #[strong]
            selected_contact_for_selection,
            #[strong]
            split_for_selection,
            #[strong]
            stored_contacts_for_selection,
            move |_, row| {
                let Some(row) = row else {
                    *selected_contact_for_selection.borrow_mut() = None;
                    merge_btn.set_sensitive(false);
                    split_for_selection.set_show_content(false);
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
                    set_avatar_photo(&detail_avatar, record.contact.photo.as_ref());
                    detail_name.set_label(&record.contact.fn_name);
                    let email_text = contact_email_text(&record.contact);
                    let phone_text = contact_phone_text(&record.contact);
                    email_row1.set_title(if email_text.is_empty() {
                        "Keine E-Mail-Adresse"
                    } else {
                        email_text.as_str()
                    });
                    email_row1.set_subtitle(if phone_text.is_empty() {
                        "Keine Telefonnummer"
                    } else {
                        phone_text.as_str()
                    });
                    detail_title.set_label(if record.contact.title.is_empty() {
                        &record.contact.org
                    } else {
                        &record.contact.title
                    });
                    detail_extra.set_label(&contact_detail_summary(&record.contact));
                    merge_btn.set_sensitive(
                        !record.source_uid.is_empty()
                            && contact_has_mergeable_duplicate(
                                &record,
                                &stored_contacts_for_selection.borrow(),
                            ),
                    );
                    *selected_contact_for_selection.borrow_mut() = Some(record);
                    split_for_selection.set_show_content(true);
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
            let detail_extra_for_monitor = detail_extra.clone();
            let email_row_for_monitor = email_row1.clone();
            let category_model_for_monitor = category_model.clone();
            let category_selector_for_monitor = category_selector.clone();
            let selected_category_for_monitor = Rc::clone(&selected_category);
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
                                    detail_avatar_for_monitor
                                        .set_custom_image(None::<&gdk::Texture>);
                                    detail_name_for_monitor.set_label("Kein Kontakt ausgewählt");
                                    detail_title_for_monitor.set_label(
                                        "Adressbuchdaten werden nach der Synchronisation angezeigt",
                                    );
                                    detail_extra_for_monitor
                                        .set_label("Keine weiteren Kontaktdaten");
                                    email_row_for_monitor.set_title("Keine E-Mail-Adresse");
                                    email_row_for_monitor.set_subtitle("Kein Kontakt ausgewählt");
                                }
                            }
                            EBookChangeKind::Added | EBookChangeKind::Modified => {
                                if let Some(record) = parse_change_record(&change) {
                                    let was_selected = selected_contact_for_monitor
                                        .borrow()
                                        .as_ref()
                                        .is_some_and(|selected| {
                                            selected.source_uid == record.source_uid
                                                && selected.uid == record.uid
                                        });
                                    upsert_contact_row(
                                        &contact_list_for_monitor,
                                        Rc::clone(&stored_contacts_for_monitor),
                                        record.clone(),
                                    );
                                    if was_selected {
                                        // Re-select after replacing the row.  This keeps the
                                        // detail pane and keyboard navigation attached to the
                                        // same source UID + contact UID instead of a stale row.
                                        select_contact_row(
                                            &contact_list_for_monitor,
                                            &record.source_uid,
                                            &record.uid,
                                        );
                                        *selected_contact_for_monitor.borrow_mut() =
                                            Some(record.clone());
                                        update_contact_detail(
                                            &record,
                                            &detail_avatar_for_monitor,
                                            &detail_name_for_monitor,
                                            &detail_title_for_monitor,
                                            &detail_extra_for_monitor,
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
                    refresh_contact_category_model(
                        &category_model_for_monitor,
                        &category_selector_for_monitor,
                        &stored_contacts_for_monitor,
                        &selected_category_for_monitor,
                    );
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
        let split_for_navigation = split.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            let is_back = key == gtk::gdk::Key::Escape
                || (key == gtk::gdk::Key::Left
                    && modifiers.contains(gtk::gdk::ModifierType::ALT_MASK));
            if is_back
                && split_for_navigation.is_collapsed()
                && split_for_navigation.shows_content()
            {
                split_for_navigation.set_show_content(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        split.add_controller(key_controller);
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
    category_model: gtk::StringList,
    category_selector: gtk::DropDown,
    selected_category: Rc<RefCell<String>>,
) {
    let selected_identity = selected_contact
        .borrow()
        .as_ref()
        .map(|contact| (contact.source_uid.clone(), contact.uid.clone()));
    while let Some(row) = contact_list.row_at_index(0) {
        contact_list.remove(&row);
    }
    stored_contacts.borrow_mut().clear();
    *selected_contact.borrow_mut() = None;
    empty_state.set_label("Kontakte werden geladen…");
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
                let warning = result.warning.as_deref();
                if let Some(warning) = warning {
                    tracing::warn!(%warning, "Contact refresh completed with an EBook warning");
                }
                if result.contacts.is_empty() {
                    empty_state.set_label(&contact_load_status_message(result.origin, 0, warning));
                    empty_state.set_visible(true);
                    refresh_contact_category_model(
                        &category_model,
                        &category_selector,
                        &stored_contacts,
                        &selected_category,
                    );
                    return;
                }
                let contact_count = result.contacts.len();
                empty_state.set_label(&contact_load_status_message(
                    result.origin,
                    contact_count,
                    warning,
                ));
                // A successful clean load should not permanently consume
                // sidebar height. Keep the state visible only when it carries
                // an empty/error/warning message the user needs to see.
                empty_state.set_visible(contact_status_needs_visibility(contact_count, warning));
                let contacts = result.contacts;
                let chunk_size = 40;
                let mut index = 0;
                while index < contacts.len() {
                    let end = (index + chunk_size).min(contacts.len());
                    let mut stored_ref = stored_contacts.borrow_mut();
                    for record in &contacts[index..end] {
                        append_contact_row(&contact_list, record);
                        stored_ref.push(record.clone());
                    }
                    drop(stored_ref);
                    index = end;
                    if index < contacts.len() {
                        glib::timeout_future_with_priority(
                            glib::Priority::LOW,
                            std::time::Duration::from_millis(5),
                        )
                        .await;
                    }
                }
                mark_duplicate_rows(&contact_list, &stored_contacts);
                refresh_contact_category_model(
                    &category_model,
                    &category_selector,
                    &stored_contacts,
                    &selected_category,
                );
                if let Some((source_uid, uid)) = selected_identity.as_ref() {
                    select_contact_row(&contact_list, source_uid, uid);
                }
            }
            Err(error) => {
                empty_state.set_label(&format!("Kontakte konnten nicht geladen werden: {error}"));
                refresh_contact_category_model(
                    &category_model,
                    &category_selector,
                    &stored_contacts,
                    &selected_category,
                );
            }
        }
    });
}

fn contact_source_label(display_name: &str, source_uid: &str) -> String {
    let display_name = display_name.trim();
    if !display_name.is_empty() {
        return display_name.to_string();
    }
    if source_uid == LOCAL_ADDRESS_BOOK_SOURCE_UID {
        "Lokales Adressbuch".to_string()
    } else {
        "Adressbuch".to_string()
    }
}

fn confirmed_contact_uid(backend_uid: &str, requested_uid: &str) -> String {
    if backend_uid.trim().is_empty() {
        requested_uid.to_string()
    } else {
        backend_uid.to_string()
    }
}

fn contact_origin_label(origin: EBookDataOrigin) -> &'static str {
    match origin {
        EBookDataOrigin::Eds => "synchronisiertem Adressbuch",
        EBookDataOrigin::Graph => "externem Adressbuch",
        EBookDataOrigin::Local => "lokalem Adressbuch",
    }
}

fn contact_load_status_message(
    origin: EBookDataOrigin,
    contact_count: usize,
    warning: Option<&str>,
) -> String {
    let message = if contact_count == 0 {
        match origin {
            EBookDataOrigin::Eds => "Keine synchronisierten Kontakte geladen".to_string(),
            EBookDataOrigin::Graph => {
                "Keine Kontakte aus dem externen Adressbuch geladen".to_string()
            }
            EBookDataOrigin::Local => {
                "Keine Kontakte aus dem lokalen Adressbuch geladen".to_string()
            }
        }
    } else {
        format!(
            "{contact_count} Kontakt(e) aus {} geladen.",
            contact_origin_label(origin)
        )
    };
    match warning.map(str::trim).filter(|warning| !warning.is_empty()) {
        Some(warning) => format!("{message} Hinweis: {warning}"),
        None => message,
    }
}

fn contact_status_needs_visibility(contact_count: usize, warning: Option<&str>) -> bool {
    contact_count == 0
        || warning
            .map(str::trim)
            .is_some_and(|warning| !warning.is_empty())
}

fn select_contact_row(contact_list: &gtk::ListBox, source_uid: &str, uid: &str) -> bool {
    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            row.data::<String>("contact-source-uid")
                .is_some_and(|value| value.as_ref() == source_uid)
                && row
                    .data::<String>("contact-id")
                    .is_some_and(|value| value.as_ref() == uid)
        };
        if matches {
            contact_list.select_row(Some(&row));
            return true;
        }
    }
    false
}

fn set_contact_filter(
    contact_list: &gtk::ListBox,
    search_query: Rc<RefCell<String>>,
    selected_book: Rc<RefCell<String>>,
    selected_category: Rc<RefCell<String>>,
    duplicate_only: Rc<Cell<bool>>,
) {
    contact_list.set_filter_func(move |row| unsafe {
        let source_filter = selected_book.borrow().clone();
        let category_filter = selected_category.borrow().clone();
        let query = search_query.borrow().clone();
        let source_matches = source_filter.is_empty()
            || row
                .data::<String>("contact-source-uid")
                .is_some_and(|value| value.as_ref().as_str() == source_filter.as_str());
        let search_matches = query.is_empty()
            || row
                .data::<String>("contact-search")
                .is_some_and(|value| contact_query_matches(value.as_ref(), &query));
        let category_matches = category_filter.is_empty()
            || row
                .data::<String>("contact-categories")
                .is_some_and(|value| {
                    contact_category_matches(value.as_ref(), category_filter.as_str())
                });
        let duplicate_matches = !duplicate_only.get()
            || row
                .data::<bool>("contact-duplicate")
                .is_some_and(|value| *value.as_ref());
        source_matches && search_matches && category_matches && duplicate_matches
    });
}

fn contact_category_matches(categories: &str, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return true;
    }
    categories
        .split('\u{1f}')
        .any(|category| category.trim().eq_ignore_ascii_case(filter))
}

fn normalize_contact_query(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn contact_query_matches(search_text: &str, query: &str) -> bool {
    let normalized_query = normalize_contact_query(query);
    if normalized_query.is_empty() {
        return true;
    }
    let normalized_search_text = normalize_contact_query(search_text);
    normalized_query
        .split_whitespace()
        .all(|term| normalized_search_text.contains(term))
}

fn refresh_contact_category_model(
    category_model: &gtk::StringList,
    category_selector: &gtk::DropDown,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
    selected_category: &Rc<RefCell<String>>,
) {
    let mut categories = stored_contacts
        .borrow()
        .iter()
        .flat_map(|record| record.contact.categories.iter())
        .map(|category| category.trim().to_string())
        .filter(|category| !category.is_empty())
        .collect::<Vec<_>>();
    categories.sort_by_cached_key(|category| category.to_lowercase());
    categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

    let current = selected_category.borrow().clone();
    let selected = current
        .is_empty()
        .then_some(0)
        .or_else(|| {
            categories
                .iter()
                .position(|category| category.eq_ignore_ascii_case(&current))
                .map(|index| index as u32 + 1)
        })
        .unwrap_or(0);
    if !current.is_empty() && selected == 0 {
        selected_category.replace(String::new());
    }

    let mut names = vec!["Alle Kategorien".to_string()];
    names.extend(categories);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    category_model.splice(0, category_model.n_items(), &name_refs);
    if category_selector.selected() != selected {
        category_selector.set_selected(selected);
    }
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
        remove_contact_list_row(contact_list, &record.source_uid, &record.uid);
    }
    append_contact_row(contact_list, &record);
    stored_contacts.borrow_mut().push(record);
    mark_duplicate_rows(contact_list, &stored_contacts);
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
    remove_contact_list_row(contact_list, source_uid, uid);
    mark_duplicate_rows(contact_list, &stored_contacts);
    true
}

fn mark_duplicate_rows(
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let contacts = stored_contacts.borrow();
    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let (source_uid, uid) = unsafe {
            (
                row.data::<String>("contact-source-uid")
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default(),
                row.data::<String>("contact-id")
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default(),
            )
        };
        let duplicate = contacts
            .iter()
            .find(|contact| contact.source_uid == source_uid && contact.uid == uid)
            .is_some_and(|contact| contact_has_duplicate(contact, &contacts));
        unsafe {
            row.set_data("contact-duplicate", duplicate);
        }
        if duplicate {
            row.add_css_class("warning");
        } else {
            row.remove_css_class("warning");
        }
    }
    contact_list.invalidate_filter();
}

fn contact_has_duplicate(contact: &EBookContactRecord, contacts: &[EBookContactRecord]) -> bool {
    contacts
        .iter()
        .any(|other| contact_records_share_duplicate_key(contact, other))
}

fn contact_has_mergeable_duplicate(
    contact: &EBookContactRecord,
    contacts: &[EBookContactRecord],
) -> bool {
    contacts
        .iter()
        .any(|other| contact_records_are_duplicates(contact, other))
}

fn contact_records_are_duplicates(
    primary: &EBookContactRecord,
    candidate: &EBookContactRecord,
) -> bool {
    primary.source_uid == candidate.source_uid
        && contact_records_share_duplicate_key(primary, candidate)
}

fn contact_records_share_duplicate_key(
    primary: &EBookContactRecord,
    candidate: &EBookContactRecord,
) -> bool {
    if primary.source_uid == candidate.source_uid && primary.uid == candidate.uid {
        return false;
    }
    let keys = primary.contact.duplicate_keys();
    let candidate_keys = candidate.contact.duplicate_keys();
    keys.iter().any(|key| candidate_keys.contains(key))
}

fn remove_contact_list_row(contact_list: &gtk::ListBox, source_uid: &str, uid: &str) {
    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = row
                .data::<String>("contact-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = row
                .data::<String>("contact-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_uid == uid
        };
        if matches {
            contact_list.remove(&row);
            return;
        }
    }
}

fn update_contact_detail(
    record: &EBookContactRecord,
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    detail_extra: &gtk::Label,
    email_row: &adw::ActionRow,
) {
    detail_avatar.set_text(Some(&record.contact.fn_name));
    set_avatar_photo(detail_avatar, record.contact.photo.as_ref());
    detail_name.set_label(&record.contact.fn_name);
    let email_text = contact_email_text(&record.contact);
    let phone_text = contact_phone_text(&record.contact);
    email_row.set_title(if email_text.is_empty() {
        "Keine E-Mail-Adresse"
    } else {
        email_text.as_str()
    });
    email_row.set_subtitle(if phone_text.is_empty() {
        "Keine Telefonnummer"
    } else {
        phone_text.as_str()
    });
    detail_title.set_label(if record.contact.title.is_empty() {
        &record.contact.org
    } else {
        &record.contact.title
    });
    detail_extra.set_label(&contact_detail_summary(&record.contact));
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
        let search_text = contact_record_search_text(record);
        row.set_data("contact-name", contact.fn_name.clone());
        row.set_data("contact-sort-key", contact_sort_key(contact));
        row.set_data("contact-email", contact.email.clone());
        row.set_data("contact-title", contact.title.clone());
        row.set_data("contact-search", search_text);
        row.set_data("contact-id", record.uid.clone());
        row.set_data("contact-source-uid", record.source_uid.clone());
        row.set_data("contact-source-name", record.source_display_name.clone());
        row.set_data("contact-vcard", record.vcard.clone());
        row.set_data("contact-categories", contact.categories.join("\u{1f}"));
    }

    let row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(8)
        .margin_end(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let avatar = adw::Avatar::builder()
        .size(32)
        .text(&contact.fn_name)
        .show_initials(true)
        .build();
    set_avatar_photo(&avatar, contact.photo.as_ref());
    row_box.append(&avatar);

    let info_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let name = gtk::Label::builder()
        .label(&contact.fn_name)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(1)
        .width_chars(1)
        .build();
    let email_text = contact_email_text(contact);
    let email = gtk::Label::builder()
        .label(if email_text.is_empty() {
            "Keine E-Mail-Adresse"
        } else {
            email_text.as_str()
        })
        .halign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(vec!["dim-label"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(1)
        .width_chars(1)
        .build();
    info_box.append(&name);
    info_box.append(&email);
    row_box.append(&info_box);
    row.set_child(Some(&row_box));
    contact_list.append(&row);
}

fn contact_record_search_text(record: &EBookContactRecord) -> String {
    normalize_contact_query(&format!(
        "{} {} {}",
        record.source_display_name,
        record.contact.searchable_text(),
        record.vcard
    ))
}

fn contact_sort_key(contact: &VCardContact) -> String {
    let family = contact.name.family.trim();
    let given = contact.name.given.trim();
    let name = if family.is_empty() {
        contact.fn_name.trim().to_string()
    } else if given.is_empty() {
        family.to_string()
    } else {
        format!("{family} {given}")
    };
    name.to_lowercase()
}

fn set_avatar_photo(
    avatar: &adw::Avatar,
    photo: Option<&crate::services::vcard_service::VCardPhoto>,
) {
    let Some(bytes) = photo.and_then(|photo| photo.decoded_bytes()) else {
        avatar.set_custom_image(None::<&gdk::Texture>);
        return;
    };
    let bytes = glib::Bytes::from_owned(bytes);
    match gdk::Texture::from_bytes(&bytes) {
        Ok(texture) => avatar.set_custom_image(Some(&texture)),
        Err(error) => {
            avatar.set_custom_image(None::<&gdk::Texture>);
            tracing::debug!(%error, "Could not decode vCard avatar photo");
        }
    }
}

fn contact_email_text(contact: &VCardContact) -> String {
    if contact.emails.is_empty() {
        contact.email.clone()
    } else {
        contact.emails.join(", ")
    }
}

fn contact_phone_text(contact: &VCardContact) -> String {
    if contact.phones.is_empty() {
        contact.tel.clone()
    } else {
        contact.phones.join(", ")
    }
}

fn contact_detail_summary(contact: &VCardContact) -> String {
    let mut lines = Vec::new();
    if !contact.addresses.is_empty() {
        lines.push(format!("Adresse: {}", contact.addresses.join(" · ")));
    }
    if !contact.categories.is_empty() {
        lines.push(format!("Kategorien: {}", contact.categories.join(", ")));
    }
    if !contact.urls.is_empty() {
        lines.push(format!("Websites: {}", contact.urls.join(", ")));
    }
    if !contact.nickname.trim().is_empty() {
        lines.push(format!("Spitzname: {}", contact.nickname));
    }
    if !contact.birthday.trim().is_empty() {
        lines.push(format!("Geburtstag: {}", contact.birthday));
    }
    if !contact.anniversary.trim().is_empty() {
        lines.push(format!("Jahrestag: {}", contact.anniversary));
    }
    if !contact.role.trim().is_empty() {
        lines.push(format!("Rolle: {}", contact.role));
    }
    if !contact.gender.trim().is_empty() {
        lines.push(format!("Geschlecht: {}", contact.gender));
    }
    if !contact.timezone.trim().is_empty() {
        lines.push(format!("Zeitzone: {}", contact.timezone));
    }
    if !contact.geo.trim().is_empty() {
        lines.push(format!("Position: {}", contact.geo));
    }
    if !contact.impps.is_empty() {
        lines.push(format!("Chat/IMPP: {}", contact.impps.join(", ")));
    }
    if !contact.related.is_empty() {
        lines.push(format!("Verwandt: {}", contact.related.join(", ")));
    }
    if !contact.note.trim().is_empty() {
        lines.push(format!("Notiz: {}", contact.note));
    }
    if lines.is_empty() {
        "Keine weiteren Kontaktdaten".to_string()
    } else {
        lines.join("\n")
    }
}

impl Default for ContactsPage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        confirmed_contact_uid, contact_category_matches, contact_load_status_message,
        contact_query_matches, contact_records_are_duplicates, contact_records_share_duplicate_key,
        contact_sort_key, contact_source_label, contact_status_needs_visibility,
    };
    use crate::services::ebook_service::{EBookContactRecord, EBookDataOrigin};
    use crate::services::vcard_service::VCardContact;

    fn record(source_uid: &str, uid: &str, contact: VCardContact) -> EBookContactRecord {
        EBookContactRecord {
            source_uid: source_uid.to_string(),
            source_display_name: "Testadressbuch".to_string(),
            uid: uid.to_string(),
            vcard: contact.to_vcard_string(),
            contact,
        }
    }

    #[test]
    fn contact_category_filter_is_case_insensitive() {
        assert!(contact_category_matches("Arbeit\u{1f}Familie", "arbeit"));
        assert!(contact_category_matches("Arbeit\u{1f}Familie", "FAMILIE"));
        assert!(!contact_category_matches("Arbeit\u{1f}Familie", "Privat"));
    }

    #[test]
    fn contact_sort_key_prefers_family_and_given_name() {
        let mut contact = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        contact.name.family = "Lovelace".to_string();
        contact.name.given = "Ada".to_string();

        assert_eq!(contact_sort_key(&contact), "lovelace ada");
    }

    #[test]
    fn contact_sort_key_falls_back_to_display_name() {
        let contact = VCardContact::new("Grace Hopper", "grace@example.test", "", "");

        assert_eq!(contact_sort_key(&contact), "grace hopper");
    }

    #[test]
    fn contact_search_matches_all_terms_without_losing_unicode() {
        let search_text = "Ada Lovelace ada@example.test X-EVOLUTION-TEAM";

        assert!(contact_query_matches(search_text, "ADA example.test"));
        assert!(contact_query_matches(
            search_text,
            "  lovelace   ada@example.test "
        ));
        assert!(!contact_query_matches(search_text, "Ada nicht-vorhanden"));
        assert!(contact_query_matches(search_text, ""));
    }

    #[test]
    fn source_labels_are_human_readable_and_do_not_expose_uid() {
        assert_eq!(
            contact_source_label("Privates Adressbuch", "goa-secret-source"),
            "Privates Adressbuch"
        );
        assert_eq!(
            contact_source_label("", "local-addressbook"),
            "Lokales Adressbuch"
        );
        assert_eq!(contact_source_label("", "uid-only-source"), "Adressbuch");
    }

    #[test]
    fn load_status_keeps_fallback_and_warning_honest() {
        assert_eq!(
            contact_load_status_message(EBookDataOrigin::Eds, 4, None),
            "4 Kontakt(e) aus synchronisiertem Adressbuch geladen."
        );
        let fallback = contact_load_status_message(
            EBookDataOrigin::Local,
            2,
            Some("Der Adressbuchdienst ist nicht verfügbar"),
        );
        assert!(fallback.contains("lokalem Adressbuch"));
        assert!(fallback.contains("Hinweis:"));
        assert!(contact_status_needs_visibility(0, None));
        assert!(!contact_status_needs_visibility(2, None));
        assert!(contact_status_needs_visibility(2, Some("Warnung")));
    }

    #[test]
    fn duplicate_detection_separates_global_marking_from_safe_merge() {
        let primary = record(
            "book-a",
            "primary",
            VCardContact::new("Ada Lovelace", "ada@example.test", "", ""),
        );
        let same_book = record(
            "book-a",
            "duplicate",
            VCardContact::new("Ada L.", "ADA@example.test", "", ""),
        );
        let other_book = record(
            "book-b",
            "duplicate-other-book",
            VCardContact::new("Ada L.", "ada@example.test", "", ""),
        );

        assert!(contact_records_share_duplicate_key(&primary, &same_book));
        assert!(contact_records_are_duplicates(&primary, &same_book));
        assert!(contact_records_share_duplicate_key(&primary, &other_book));
        assert!(!contact_records_are_duplicates(&primary, &other_book));
        assert!(!contact_records_share_duplicate_key(&primary, &primary));
    }

    #[test]
    fn backend_uid_fallback_preserves_requested_contact_identity() {
        assert_eq!(confirmed_contact_uid("", "contact-42"), "contact-42");
        assert_eq!(
            confirmed_contact_uid("confirmed-42", "contact-42"),
            "confirmed-42"
        );
    }
}
