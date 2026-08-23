// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactsPage — Address book view with search filtering and dynamic contact detail view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::contact_search_service::{
    ContactIdentity, ContactOuterFilters, ContactSearchEvaluation, ContactSearchField,
    ContactSearchQuery, ContactSearchSnapshot, LastValidContactSearch, LatestContactSearchWorker,
    LatestGenerationEvaluator, field_values,
};
use crate::services::ebook_service::{
    EBookChangeKind, EBookContactRecord, EBookDataOrigin, EBookService, EBookSyncEvent,
    load_contacts_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;
use crate::services::vcard_service::{VCardContact, VCardListMemberKind};
use crate::views::accessibility::set_accessible_label;

const LOCAL_ADDRESS_BOOK_SOURCE_UID: &str = "local-addressbook";

#[derive(Debug)]
struct ContactPageSearchState {
    visible_identities: RefCell<HashSet<ContactIdentity>>,
    active_query: RefCell<ContactSearchQuery>,
    last_valid: RefCell<LastValidContactSearch>,
    generations: LatestGenerationEvaluator,
    worker: LatestContactSearchWorker,
    quick_field: Cell<ContactSearchField>,
}

impl Default for ContactPageSearchState {
    fn default() -> Self {
        Self {
            visible_identities: RefCell::new(HashSet::new()),
            active_query: RefCell::new(ContactSearchQuery::all()),
            last_valid: RefCell::new(LastValidContactSearch::default()),
            generations: LatestGenerationEvaluator::default(),
            worker: LatestContactSearchWorker::default(),
            quick_field: Cell::new(ContactSearchField::AnyField),
        }
    }
}

impl ContactPageSearchState {
    fn quick_field(&self) -> ContactSearchField {
        self.quick_field.get()
    }

    fn set_quick_field(&self, field: ContactSearchField) {
        self.quick_field.set(field);
    }

    fn active_query(&self) -> ContactSearchQuery {
        self.active_query.borrow().clone()
    }

    fn set_active_query(&self, query: ContactSearchQuery) {
        self.active_query.replace(query);
    }

    fn active_query_with_filters(&self, filters: ContactOuterFilters) -> ContactSearchQuery {
        let mut query = self.active_query();
        query.filters = filters;
        self.set_active_query(query.clone());
        query
    }
}

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ContactsPage {
        pub ebook_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub contact_search_cancel: RefCell<Option<Arc<AtomicBool>>>,
        pub contact_search_field: Cell<ContactSearchField>,
        pub contact_search_status: RefCell<Option<gtk::Label>>,
        pub contact_search_spinner: RefCell<Option<gtk::Spinner>>,
        pub disposed: Cell<bool>,
        pub selected_identity: RefCell<Option<ContactIdentity>>,
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
            self.disposed.set(true);
            if let Some(cancel) = self.ebook_monitor_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(cancel) = self.contact_search_cancel.borrow_mut().take() {
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
        // Global search has an explicit, typed mapping in the service layer:
        // Any Field contains.  Reset the page-local quick mode before setting
        // the entry so a previous Name/Phone quick mode cannot reinterpret a
        // cross-component result.
        self.imp()
            .contact_search_field
            .set(ContactSearchField::AnyField);
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
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(240.0)
            .max_sidebar_width(240.0)
            .sidebar_width_fraction(0.0)
            .build();

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

        let quick_mode = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Alle Felder",
                "Name",
                "E-Mail",
                "Telefon",
                "Kategorie",
            ])),
            None::<&gtk::Expression>,
        );
        quick_mode.set_tooltip_text(Some("Schnellsuchfeld auswählen"));
        quick_mode.set_margin_start(8);
        quick_mode.set_margin_end(8);
        quick_mode.set_margin_bottom(4);
        sidebar_box.append(&quick_mode);

        let advanced_search_button = gtk::Button::builder()
            .label("Erweiterte Suche…")
            .icon_name("system-search-symbolic")
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(6)
            .css_classes(vec!["flat"])
            .build();
        sidebar_box.append(&advanced_search_button);

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

        let selected_book = Rc::new(RefCell::new(String::new()));
        let book_source_uids = Rc::new(RefCell::new(vec![String::new()]));
        let selected_category = Rc::new(RefCell::new(String::new()));
        let duplicate_only = Rc::new(Cell::new(false));
        let unmatched_category = Rc::new(Cell::new(false));

        let duplicate_filter_row = gtk::CheckButton::builder()
            .label("Nur Dubletten")
            .tooltip_text("Gleiche E-Mail, Telefonnummer oder Name ohne Kontaktweg anzeigen")
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&duplicate_filter_row);

        let unmatched_category_row = gtk::CheckButton::builder()
            .label("Nur ohne Kategorie")
            .tooltip_text("Kontakte ohne jegliche Kategorie anzeigen")
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&unmatched_category_row);

        let empty_state = gtk::Label::builder()
            .label("Noch kein synchronisiertes Adressbuch geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let search_spinner = gtk::Spinner::new();
        search_spinner.set_visible(false);
        search_spinner.set_halign(gtk::Align::Start);
        search_spinner.set_margin_start(8);
        sidebar_box.append(&search_spinner);
        sidebar_box.append(&empty_state);
        self.imp()
            .contact_search_status
            .replace(Some(empty_state.clone()));
        self.imp()
            .contact_search_spinner
            .replace(Some(search_spinner.clone()));

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
            // Evolution's address-book actions operate on the whole visible
            // selection.  Keep the detail pane single-contact specific, but
            // let the list retain a 0/1/n selection for batch-safe actions.
            .selection_mode(gtk::SelectionMode::Multiple)
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
        let refresh_generation = Rc::new(Cell::new(0u64));
        let search_state = Rc::new(ContactPageSearchState::default());
        self.imp()
            .contact_search_field
            .set(ContactSearchField::AnyField);

        set_contact_filter(&contact_list, Rc::clone(&search_state));

        refresh_contacts(
            contact_list.clone(),
            Rc::clone(&stored_contacts),
            empty_state.clone(),
            Rc::clone(&selected_contact),
            category_model.clone(),
            category_selector.clone(),
            Rc::clone(&selected_category),
            Rc::clone(&refresh_generation),
            self.downgrade(),
            Rc::clone(&search_state),
        );

        let search_state_for_search = Rc::clone(&search_state);
        let stored_contacts_for_search = Rc::clone(&stored_contacts);
        let page_weak_for_search = self.downgrade();
        let selected_book_for_search = Rc::clone(&selected_book);
        let selected_category_for_search = Rc::clone(&selected_category);
        let duplicate_only_for_search = Rc::clone(&duplicate_only);
        let unmatched_category_for_search = Rc::clone(&unmatched_category);
        search_entry.connect_search_changed(glib::clone!(
            #[strong]
            search_state_for_search,
            #[strong]
            stored_contacts_for_search,
            #[strong]
            page_weak_for_search,
            move |entry| {
                let quick_field = page_weak_for_search
                    .upgrade()
                    .map(|page| page.imp().contact_search_field.get())
                    .unwrap_or_else(|| search_state_for_search.quick_field());
                search_state_for_search.set_quick_field(quick_field);
                let query =
                    quick_query(search_state_for_search.quick_field(), entry.text().as_str());
                let query = with_contact_outer_filters(
                    query,
                    &selected_book_for_search,
                    &selected_category_for_search,
                    unmatched_category_for_search.get(),
                    duplicate_only_for_search.get(),
                );
                start_contact_search(
                    page_weak_for_search.clone(),
                    query,
                    Rc::clone(&search_state_for_search),
                    Rc::clone(&stored_contacts_for_search),
                );
            }
        ));

        let search_entry_for_quick_mode = search_entry.clone();
        let search_state_for_quick_mode = Rc::clone(&search_state);
        let stored_contacts_for_quick_mode = Rc::clone(&stored_contacts);
        let page_weak_for_quick_mode = self.downgrade();
        let selected_book_for_quick_mode = Rc::clone(&selected_book);
        let selected_category_for_quick_mode = Rc::clone(&selected_category);
        let duplicate_only_for_quick_mode = Rc::clone(&duplicate_only);
        let unmatched_category_for_quick_mode = Rc::clone(&unmatched_category);
        quick_mode.connect_selected_notify(move |mode| {
            let field = quick_field_for_index(mode.selected());
            if let Some(page) = page_weak_for_quick_mode.upgrade() {
                page.imp().contact_search_field.set(field);
            }
            search_state_for_quick_mode.set_quick_field(field);
            let query = quick_query(field, search_entry_for_quick_mode.text().as_str());
            let query = with_contact_outer_filters(
                query,
                &selected_book_for_quick_mode,
                &selected_category_for_quick_mode,
                unmatched_category_for_quick_mode.get(),
                duplicate_only_for_quick_mode.get(),
            );
            start_contact_search(
                page_weak_for_quick_mode.clone(),
                query,
                Rc::clone(&search_state_for_quick_mode),
                Rc::clone(&stored_contacts_for_quick_mode),
            );
        });

        let selected_book_for_filter = Rc::clone(&selected_book);
        let selected_category_for_book = Rc::clone(&selected_category);
        let duplicate_only_for_book = Rc::clone(&duplicate_only);
        let unmatched_category_for_book = Rc::clone(&unmatched_category);
        let book_source_uids_for_filter = Rc::clone(&book_source_uids);
        let book_source_list_for_filter = book_source_list.clone();
        let search_state_for_book = Rc::clone(&search_state);
        let stored_contacts_for_book = Rc::clone(&stored_contacts);
        let page_weak_for_book = self.downgrade();
        book_selector.connect_selected_notify(move |selector| {
            let selected = book_source_uids_for_filter
                .borrow()
                .get(selector.selected() as usize)
                .cloned()
                .unwrap_or_default();
            *selected_book_for_filter.borrow_mut() = selected.clone();
            if let Some(settings) = AppSettings::try_new() {
                settings.set_default_address_book_source_uid(&selected);
            }
            if let Some(row) = book_source_list_for_filter.row_at_index(selector.selected() as i32)
            {
                book_source_list_for_filter.select_row(Some(&row));
            }
            let query = search_state_for_book.active_query_with_filters(current_outer_filters(
                &selected_book_for_filter,
                &selected_category_for_book,
                unmatched_category_for_book.get(),
                duplicate_only_for_book.get(),
            ));
            start_contact_search(
                page_weak_for_book.clone(),
                query,
                Rc::clone(&search_state_for_book),
                Rc::clone(&stored_contacts_for_book),
            );
        });

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

        let selected_book_for_duplicates = Rc::clone(&selected_book);
        let selected_category_for_duplicates = Rc::clone(&selected_category);
        let duplicate_only_for_duplicates = Rc::clone(&duplicate_only);
        let unmatched_category_for_duplicates = Rc::clone(&unmatched_category);
        let search_state_for_duplicates = Rc::clone(&search_state);
        let stored_contacts_for_duplicates = Rc::clone(&stored_contacts);
        let page_weak_for_duplicates = self.downgrade();
        duplicate_filter_row.connect_active_notify(move |row| {
            duplicate_only_for_duplicates.set(row.is_active());
            let query =
                search_state_for_duplicates.active_query_with_filters(current_outer_filters(
                    &selected_book_for_duplicates,
                    &selected_category_for_duplicates,
                    unmatched_category_for_duplicates.get(),
                    duplicate_only_for_duplicates.get(),
                ));
            start_contact_search(
                page_weak_for_duplicates.clone(),
                query,
                Rc::clone(&search_state_for_duplicates),
                Rc::clone(&stored_contacts_for_duplicates),
            );
        });

        let selected_book_for_unmatched = Rc::clone(&selected_book);
        let selected_category_for_unmatched = Rc::clone(&selected_category);
        let duplicate_only_for_unmatched = Rc::clone(&duplicate_only);
        let unmatched_category_for_filter = Rc::clone(&unmatched_category);
        let search_state_for_unmatched = Rc::clone(&search_state);
        let stored_contacts_for_unmatched = Rc::clone(&stored_contacts);
        let page_weak_for_unmatched = self.downgrade();
        unmatched_category_row.connect_active_notify(move |row| {
            unmatched_category_for_filter.set(row.is_active());
            let query =
                search_state_for_unmatched.active_query_with_filters(current_outer_filters(
                    &selected_book_for_unmatched,
                    &selected_category_for_unmatched,
                    unmatched_category_for_filter.get(),
                    duplicate_only_for_unmatched.get(),
                ));
            start_contact_search(
                page_weak_for_unmatched.clone(),
                query,
                Rc::clone(&search_state_for_unmatched),
                Rc::clone(&stored_contacts_for_unmatched),
            );
        });

        let selected_category_for_selector = Rc::clone(&selected_category);
        let selected_book_for_category = Rc::clone(&selected_book);
        let duplicate_only_for_category = Rc::clone(&duplicate_only);
        let unmatched_category_for_category = Rc::clone(&unmatched_category);
        let category_model_for_selector = category_model.clone();
        let search_state_for_category = Rc::clone(&search_state);
        let stored_contacts_for_category = Rc::clone(&stored_contacts);
        let page_weak_for_category = self.downgrade();
        category_selector.connect_selected_notify(move |selector| {
            let category = if selector.selected() == 0 {
                String::new()
            } else {
                category_model_for_selector
                    .string(selector.selected())
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            };
            selected_category_for_selector.replace(category);
            let query = search_state_for_category.active_query_with_filters(current_outer_filters(
                &selected_book_for_category,
                &selected_category_for_selector,
                unmatched_category_for_category.get(),
                duplicate_only_for_category.get(),
            ));
            start_contact_search(
                page_weak_for_category.clone(),
                query,
                Rc::clone(&search_state_for_category),
                Rc::clone(&stored_contacts_for_category),
            );
        });

        let search_state_for_advanced = Rc::clone(&search_state);
        let stored_contacts_for_advanced = Rc::clone(&stored_contacts);
        let page_weak_for_advanced = self.downgrade();
        let selected_book_for_advanced = Rc::clone(&selected_book);
        let selected_category_for_advanced = Rc::clone(&selected_category);
        let unmatched_category_for_advanced = Rc::clone(&unmatched_category);
        let duplicate_only_for_advanced = Rc::clone(&duplicate_only);
        advanced_search_button.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let initial = crate::dialogs::contact_search_dialog::seed_query_for_reopen(
                    &search_state_for_advanced.active_query_with_filters(current_outer_filters(
                        &selected_book_for_advanced,
                        &selected_category_for_advanced,
                        unmatched_category_for_advanced.get(),
                        duplicate_only_for_advanced.get(),
                    )),
                );
                let state_for_apply = Rc::clone(&search_state_for_advanced);
                let contacts_for_apply = Rc::clone(&stored_contacts_for_advanced);
                let page_for_apply = page_weak_for_advanced.clone();
                let selected_book_for_apply = Rc::clone(&selected_book_for_advanced);
                let selected_category_for_apply = Rc::clone(&selected_category_for_advanced);
                let unmatched_category_for_apply = Rc::clone(&unmatched_category_for_advanced);
                let duplicate_only_for_apply = Rc::clone(&duplicate_only_for_advanced);
                let dialog =
                    crate::dialogs::ContactSearchDialog::with_query(initial, move |query| {
                        let query = query.with_filters(current_outer_filters(
                            &selected_book_for_apply,
                            &selected_category_for_apply,
                            unmatched_category_for_apply.get(),
                            duplicate_only_for_apply.get(),
                        ));
                        start_contact_search(
                            page_for_apply.clone(),
                            query,
                            Rc::clone(&state_for_apply),
                            Rc::clone(&contacts_for_apply),
                        );
                    });
                if let Some(window) = page.root().and_downcast::<gtk::Window>() {
                    dialog.set_transient_for(Some(&window));
                }
                dialog.present();
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
        let refresh_generation_for_new = Rc::clone(&refresh_generation);
        let category_model_for_new = category_model.clone();
        let category_selector_for_new = category_selector.clone();
        let selected_category_for_new = Rc::clone(&selected_category);
        let search_state_for_new = Rc::clone(&search_state);
        let page_weak_for_new_refresh = self.downgrade();
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
                let refresh_generation_for_close = Rc::clone(&refresh_generation_for_new);
                let search_state_for_close = Rc::clone(&search_state_for_new);
                let page_weak_for_close = page_weak_for_new_refresh.clone();
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                        Rc::clone(&refresh_generation_for_close),
                        page_weak_for_close.clone(),
                        Rc::clone(&search_state_for_close),
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

        let new_list_btn = gtk::Button::builder()
            .label("Neue Kontaktliste")
            .icon_name("view-list-symbolic")
            .css_classes(vec!["suggested-action"])
            .hexpand(true)
            .build();
        let contact_list_for_new_list = contact_list.clone();
        let stored_contacts_for_new_list = Rc::clone(&stored_contacts);
        let empty_state_for_new_list = empty_state.clone();
        let selected_contact_for_new_list = Rc::clone(&selected_contact);
        let refresh_generation_for_new_list = Rc::clone(&refresh_generation);
        let category_model_for_new_list = category_model.clone();
        let category_selector_for_new_list = category_selector.clone();
        let selected_category_for_new_list = Rc::clone(&selected_category);
        let search_state_for_new_list = Rc::clone(&search_state);
        let page_weak_for_new_list_refresh = self.downgrade();
        new_list_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::ContactDialog::new_for_list();
                let root = page.root();
                if let Some(win) = root.and_then(|root| root.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let contact_list_for_close = contact_list_for_new_list.clone();
                let stored_contacts_for_close = Rc::clone(&stored_contacts_for_new_list);
                let empty_state_for_close = empty_state_for_new_list.clone();
                let selected_contact_for_close = Rc::clone(&selected_contact_for_new_list);
                let category_model_for_close = category_model_for_new_list.clone();
                let category_selector_for_close = category_selector_for_new_list.clone();
                let selected_category_for_close = Rc::clone(&selected_category_for_new_list);
                let refresh_generation_for_close = Rc::clone(&refresh_generation_for_new_list);
                let search_state_for_close = Rc::clone(&search_state_for_new_list);
                let page_weak_for_close = page_weak_for_new_list_refresh.clone();
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                        Rc::clone(&refresh_generation_for_close),
                        page_weak_for_close.clone(),
                        Rc::clone(&search_state_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));
        set_accessible_label(&new_list_btn, "Neue Kontaktliste erstellen");
        new_contact_bar.append(&new_list_btn);
        sidebar_box.append(&new_contact_bar);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&sidebar_box)
            .width_request(240)
            .vexpand(true)
            .build();

        let drawer_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(vec!["sidebar-drawer-header"])
            .build();
        let drawer_icon = gtk::Image::from_icon_name("x-office-address-book-symbolic");
        let drawer_title = gtk::Label::builder()
            .label("Kontakte")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();
        sidebar_outer_box.append(&drawer_header);
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "contacts",
            &sidebar_outer_box,
        ));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Kontakte")
            .child(&sidebar_outer_box)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
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

        let contact_list_for_email = contact_list.clone();
        let stored_contacts_for_email = Rc::clone(&stored_contacts);
        email_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_email,
                    &stored_contacts_for_email,
                ) {
                    Ok(selected) => selected,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };
                let recipient =
                    selected_contact_recipients(&selected, &stored_contacts_for_email.borrow());
                if recipient.trim().is_empty() {
                    page.show_toast("Die ausgewählten Kontakte haben keine E-Mail-Adresse");
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
        let refresh_generation_for_edit = Rc::clone(&refresh_generation);
        let search_state_for_edit = Rc::clone(&search_state);
        let page_weak_for_edit_refresh = self.downgrade();
        edit_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(record) = selected_contact_for_edit.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if !contact_source_is_writable(&record.source_uid) {
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
                let refresh_generation_for_close = Rc::clone(&refresh_generation_for_edit);
                let search_state_for_close = Rc::clone(&search_state_for_edit);
                let page_weak_for_close = page_weak_for_edit_refresh.clone();
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                        Rc::clone(&refresh_generation_for_close),
                        page_weak_for_close.clone(),
                        Rc::clone(&search_state_for_close),
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
        let search_state_for_merge = Rc::clone(&search_state);
        let merge_btn_for_result = merge_btn.clone();
        merge_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(primary) = selected_contact_for_merge.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if !contact_source_is_writable(&primary.source_uid) {
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
                let search_state_for_action = Rc::clone(&search_state_for_merge);
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
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        // Remove any automatic GTK row selection before
                                        // rebuilding the primary row.  Otherwise ListBox can
                                        // select an unrelated neighbour and the search
                                        // generation would restore that contact instead.
                                        clear_contact_selection(&page, &contact_list_for_action);
                                    }
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
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        page.imp().selected_identity.replace(Some(
                                            ContactIdentity::new(
                                                merged.source_uid.clone(),
                                                merged.uid.clone(),
                                            ),
                                        ));
                                    }
                                    merge_btn_for_action.set_sensitive(
                                        contact_has_mergeable_duplicate(
                                            &merged,
                                            &stored_contacts_for_action.borrow(),
                                        ),
                                    );
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        reapply_active_contact_search(
                                            page.downgrade(),
                                            Rc::clone(&search_state_for_action),
                                            Rc::clone(&stored_contacts_for_action),
                                        );
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
        let contact_list_for_export = contact_list.clone();
        let stored_contacts_for_export = Rc::clone(&stored_contacts);
        let export_btn_for_export = export_btn.clone();
        export_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_export,
                    &stored_contacts_for_export,
                ) {
                    Ok(selected) => selected,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };
                // Plan the complete output before opening the save dialog.
                // This prevents a partially valid selection from producing a
                // partial export if one cached vCard is malformed.
                let plan = match plan_contact_vcard_export(&selected) {
                    Ok(plan) => plan,
                    Err(error) => {
                        page.show_toast(&error.user_message());
                        return;
                    }
                };
                let Some(root) = page.root().and_downcast::<gtk::Window>() else {
                    page.show_toast("Kein Fenster für den Export verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Kontakte als vCard speichern")
                    .initial_name(&plan.initial_name)
                    .modal(true)
                    .build();
                let export_button = export_btn_for_export.clone();
                file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    save_contact_vcard_export_async(&export_button, file, plan);
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
        let email_btn_for_delete = email_btn.clone();
        let export_btn_for_delete = export_btn.clone();
        let edit_btn_for_delete = edit_btn.clone();
        let merge_btn_for_delete = merge_btn.clone();
        let delete_btn_for_delete = delete_btn.clone();
        let search_state_for_delete = Rc::clone(&search_state);
        delete_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(contact) = selected_contact_for_delete.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if !contact_source_is_writable(&contact.source_uid) {
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
                let email_btn_for_action = email_btn_for_delete.clone();
                let export_btn_for_action = export_btn_for_delete.clone();
                let edit_btn_for_action = edit_btn_for_delete.clone();
                let merge_btn_for_action = merge_btn_for_delete.clone();
                let delete_btn_for_action = delete_btn_for_delete.clone();
                let page_weak_for_action = page.downgrade();
                let search_state_for_action = Rc::clone(&search_state_for_delete);
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
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        clear_contact_selection(&page, &contact_list_for_result);
                                    }
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
                                    update_contact_action_sensitivity(
                                        &email_btn_for_action,
                                        &export_btn_for_action,
                                        &edit_btn_for_action,
                                        &merge_btn_for_action,
                                        &delete_btn_for_action,
                                        None,
                                        &selected_contact_for_result,
                                        &stored_contacts_for_result,
                                    );
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        reapply_active_contact_search(
                                            page.downgrade(),
                                            Rc::clone(&search_state_for_action),
                                            Rc::clone(&stored_contacts_for_result),
                                        );
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
        let selected_contact_for_context = Rc::clone(&selected_contact);
        let stored_contacts_for_context = Rc::clone(&stored_contacts);
        let context_click = gtk::GestureClick::builder().button(3).build();
        let contact_list_weak_for_context = contact_list.downgrade();
        context_click.connect_pressed(move |_, _, x, y| {
            let Some(contact_list_for_context) = contact_list_weak_for_context.upgrade() else {
                return;
            };
            let Some(row) = contact_list_for_context.row_at_y(y as i32) else {
                return;
            };
            // A context menu targets its clicked contact.  Do not leave a
            // stale multi-selection attached to a menu made for one row.
            contact_list_for_context.unselect_all();
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

            // Context actions use the same selection/source capability
            // calculation as the header.  This is deliberately applied only
            // after the row was selected, so imported/read-only records fail
            // closed in both action surfaces.
            let action_state = context_action_state(
                selected_contact_for_context.borrow().as_ref(),
                &stored_contacts_for_context.borrow(),
            );
            apply_contact_action_state(action_state, &email, &export, &edit, &merge, &delete, None);

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
        let stored_contacts_for_copy = Rc::clone(&stored_contacts);
        copy_btn1.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |button| {
                let Some(record) = selected_contact_for_copy.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                let email = if record.contact.is_list {
                    if record.contact.list_show_addresses {
                        contact_recipient_text_with_records(
                            &record.contact,
                            &stored_contacts_for_copy.borrow(),
                        )
                    } else {
                        String::new()
                    }
                } else {
                    contact_email_text(&record.contact)
                };
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

        // The detail pane intentionally describes exactly one contact, while
        // the surrounding actions retain Evolution's 0/1/n selection model.
        // Selection changes rather than row-selected are used because GTK
        // emits row-selected for individual transitions within a multi-select.
        let split_for_selection = split.clone();
        let selected_contact_for_selection = Rc::clone(&selected_contact);
        let stored_contacts_for_selection = Rc::clone(&stored_contacts);
        let page_weak_for_selection = self.downgrade();
        let email_btn_for_selection = email_btn.clone();
        let export_btn_for_selection = export_btn.clone();
        let edit_btn_for_selection = edit_btn.clone();
        let merge_btn_for_selection = merge_btn.clone();
        let delete_btn_for_selection = delete_btn.clone();
        let copy_btn_for_selection = copy_btn1.clone();
        let detail_avatar_for_selection = detail_avatar.clone();
        let detail_name_for_selection = detail_name.clone();
        let detail_title_for_selection = detail_title.clone();
        let detail_extra_for_selection = detail_extra.clone();
        let email_row_for_selection = email_row1.clone();
        contact_list.connect_selected_rows_changed(move |list| {
            let Some(page) = page_weak_for_selection.upgrade() else {
                return;
            };
            sync_contact_selection(
                &page,
                list,
                &stored_contacts_for_selection,
                &selected_contact_for_selection,
                &split_for_selection,
                &detail_avatar_for_selection,
                &detail_name_for_selection,
                &detail_title_for_selection,
                &detail_extra_for_selection,
                &email_row_for_selection,
                &email_btn_for_selection,
                &export_btn_for_selection,
                &edit_btn_for_selection,
                &merge_btn_for_selection,
                &delete_btn_for_selection,
                &copy_btn_for_selection,
            );
        });

        sync_contact_selection(
            self,
            &contact_list,
            &stored_contacts,
            &selected_contact,
            &split,
            &detail_avatar,
            &detail_name,
            &detail_title,
            &detail_extra,
            &email_row1,
            &email_btn,
            &export_btn,
            &edit_btn,
            &merge_btn,
            &delete_btn,
            &copy_btn1,
        );

        let edit_btn_for_activate = edit_btn.clone();
        contact_list.connect_row_activated(move |_, _| {
            edit_btn_for_activate.activate();
        });

        if let Some((cancel, receiver)) = EBookService::new().start_change_monitor() {
            *self.imp().ebook_monitor_cancel.borrow_mut() = Some(cancel);
            let contact_list_for_monitor = contact_list.clone();
            let split_for_monitor = split.clone();
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
            let search_state_for_monitor = Rc::clone(&search_state);
            let page_weak_for_monitor = self.downgrade();
            let email_btn_for_monitor = email_btn.clone();
            let export_btn_for_monitor = export_btn.clone();
            let edit_btn_for_monitor = edit_btn.clone();
            let merge_btn_for_monitor = merge_btn.clone();
            let delete_btn_for_monitor = delete_btn.clone();
            let copy_btn_for_monitor = copy_btn1.clone();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    let Some(page) = page_weak_for_monitor.upgrade() else {
                        break;
                    };
                    if page.imp().disposed.get() {
                        break;
                    }
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
                                    page.imp().selected_identity.replace(None);
                                    contact_list_for_monitor.unselect_all();
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
                    sync_contact_selection(
                        &page,
                        &contact_list_for_monitor,
                        &stored_contacts_for_monitor,
                        &selected_contact_for_monitor,
                        &split_for_monitor,
                        &detail_avatar_for_monitor,
                        &detail_name_for_monitor,
                        &detail_title_for_monitor,
                        &detail_extra_for_monitor,
                        &email_row_for_monitor,
                        &email_btn_for_monitor,
                        &export_btn_for_monitor,
                        &edit_btn_for_monitor,
                        &merge_btn_for_monitor,
                        &delete_btn_for_monitor,
                        &copy_btn_for_monitor,
                    );
                    empty_state_for_monitor
                        .set_visible(stored_contacts_for_monitor.borrow().is_empty());
                    refresh_contact_category_model(
                        &category_model_for_monitor,
                        &category_selector_for_monitor,
                        &stored_contacts_for_monitor,
                        &selected_category_for_monitor,
                    );
                    start_contact_search(
                        page_weak_for_monitor.clone(),
                        search_state_for_monitor.active_query(),
                        Rc::clone(&search_state_for_monitor),
                        Rc::clone(&stored_contacts_for_monitor),
                    );
                }
            });
        }

        let content_page = adw::NavigationPage::builder()
            .title("Kontakt-Details")
            .child(&detail_box)
            .build();

        split.set_content(Some(&content_page));
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
    refresh_generation: Rc<Cell<u64>>,
    page_weak: glib::WeakRef<ContactsPage>,
    search_state: Rc<ContactPageSearchState>,
) {
    let generation = refresh_generation.get().wrapping_add(1);
    refresh_generation.set(generation);
    let selected_identity = selected_contact
        .borrow()
        .as_ref()
        .map(|contact| (contact.source_uid.clone(), contact.uid.clone()));
    while let Some(row) = contact_list.row_at_index(0) {
        contact_list.remove(&row);
    }
    stored_contacts.borrow_mut().clear();
    search_state.visible_identities.borrow_mut().clear();
    *selected_contact.borrow_mut() = None;
    empty_state.set_label("Kontakte werden geladen…");
    empty_state.set_visible(true);
    if let Some(page) = page_weak.upgrade() {
        if page.imp().disposed.get() {
            return;
        }
        page.imp().selected_identity.replace(None);
        let request = search_state.generations.begin();
        page.imp()
            .contact_search_cancel
            .replace(Some(request.cancellation()));
        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
            spinner.start();
            spinner.set_visible(true);
        }
    } else {
        return;
    }

    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = load_contacts_prefer_eds();
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Contact refresh result was dropped");
        }
    });
    glib::spawn_future_local(async move {
        let is_current = || refresh_generation.get() == generation;
        let page_is_alive = || {
            page_weak
                .upgrade()
                .is_some_and(|page| !page.imp().disposed.get())
        };
        match receiver.recv().await {
            Ok(result) => {
                if !is_current() || !page_is_alive() {
                    return;
                }
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
                    start_contact_search(
                        page_weak.clone(),
                        search_state.active_query(),
                        Rc::clone(&search_state),
                        Rc::clone(&stored_contacts),
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
                    if !is_current() || !page_is_alive() {
                        return;
                    }
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
                        if !is_current() || !page_is_alive() {
                            return;
                        }
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
                start_contact_search(
                    page_weak.clone(),
                    search_state.active_query(),
                    Rc::clone(&search_state),
                    Rc::clone(&stored_contacts),
                );
            }
            Err(error) => {
                if !page_is_alive() {
                    return;
                }
                empty_state.set_label(&format!("Kontakte konnten nicht geladen werden: {error}"));
                refresh_contact_category_model(
                    &category_model,
                    &category_selector,
                    &stored_contacts,
                    &selected_category,
                );
                if let Some(page) = page_weak.upgrade() {
                    if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                        spinner.stop();
                        spinner.set_visible(false);
                    }
                }
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
            contact_list.unselect_all();
            contact_list.select_row(Some(&row));
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContactActionState {
    email: bool,
    export: bool,
    edit: bool,
    merge: bool,
    delete: bool,
    copy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContactSelectionActionState {
    selection_count: usize,
    email: bool,
    export: bool,
    edit: bool,
    merge: bool,
    delete: bool,
    copy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContactVCardExportPlan {
    initial_name: String,
    payload: Vec<u8>,
    contact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContactVCardExportError {
    NoSelection,
    InvalidVCard { contact_name: String },
}

impl ContactVCardExportError {
    fn user_message(&self) -> String {
        match self {
            Self::NoSelection => "Keine Kontakte ausgewählt".to_string(),
            Self::InvalidVCard { contact_name } => format!(
                "vCard-Export nicht gestartet: „{contact_name}“ enthält keine vollständige vCard"
            ),
        }
    }
}

fn contact_source_is_writable(source_uid: &str) -> bool {
    let source_uid = source_uid.trim();
    if source_uid.is_empty() {
        return false;
    }
    if source_uid == LOCAL_ADDRESS_BOOK_SOURCE_UID {
        return crate::services::contact_store_service::ContactStoreService::new().is_writable();
    }
    // EBookContactRecord does not carry a fabricated read-only flag.  A
    // non-empty source UID is the capability metadata supplied by the EBook
    // layer for an address-book-backed record; imported records have no UID
    // and remain explicitly read-only.
    true
}

fn contact_action_state(
    selected: Option<&EBookContactRecord>,
    contacts: &[EBookContactRecord],
) -> ContactActionState {
    let Some(record) = selected else {
        return ContactActionState {
            email: false,
            export: false,
            edit: false,
            merge: false,
            delete: false,
            copy: false,
        };
    };
    let recipient = contact_recipient_text_with_records(&record.contact, contacts);
    let copyable = if record.contact.is_list {
        record.contact.list_show_addresses && !recipient.trim().is_empty()
    } else {
        !contact_email_text(&record.contact).trim().is_empty()
    };
    let writable = contact_source_is_writable(&record.source_uid);
    ContactActionState {
        email: !recipient.trim().is_empty(),
        export: true,
        edit: writable,
        merge: writable && contact_has_mergeable_duplicate(record, contacts),
        delete: writable,
        copy: copyable,
    }
}

fn context_action_state(
    selected: Option<&EBookContactRecord>,
    contacts: &[EBookContactRecord],
) -> ContactActionState {
    contact_action_state(selected, contacts)
}

fn contact_selection_action_state(
    selected: &[EBookContactRecord],
    contacts: &[EBookContactRecord],
) -> ContactSelectionActionState {
    let selection_count = selected.len();
    let single = selection_count == 1;
    let single_state = single.then(|| contact_action_state(selected.first(), contacts));
    let recipients = selected_contact_recipients(selected, contacts);
    ContactSelectionActionState {
        selection_count,
        email: !recipients.trim().is_empty(),
        // The export button only becomes enabled once the whole selection has
        // been validated as one payload.  This is the fail-closed half of the
        // atomic save contract; the click handler plans it again from the
        // current selection before the file picker is opened.
        export: plan_contact_vcard_export(selected).is_ok(),
        edit: single_state.is_some_and(|state| state.edit),
        merge: single_state.is_some_and(|state| state.merge),
        // The existing EBook mutation is intentionally a single-contact
        // transaction.  Keep multi-delete visibly unavailable until a
        // backend-atomic batch mutation exists instead of deleting a prefix.
        delete: single_state.is_some_and(|state| state.delete),
        copy: single_state.is_some_and(|state| state.copy),
    }
}

fn update_contact_action_sensitivity(
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    copy_button: Option<&gtk::Button>,
    selected: &Rc<RefCell<Option<EBookContactRecord>>>,
    contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let selected = selected.borrow();
    let contacts = contacts.borrow();
    let state = contact_action_state(selected.as_ref(), &contacts);
    apply_contact_action_state(
        state,
        email_button,
        export_button,
        edit_button,
        merge_button,
        delete_button,
        copy_button,
    );
}

fn apply_contact_action_state(
    state: ContactActionState,
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    copy_button: Option<&gtk::Button>,
) {
    email_button.set_sensitive(state.email);
    export_button.set_sensitive(state.export);
    edit_button.set_sensitive(state.edit);
    merge_button.set_sensitive(state.merge);
    delete_button.set_sensitive(state.delete);
    if let Some(copy_button) = copy_button {
        copy_button.set_sensitive(state.copy);
    }
}

fn apply_contact_selection_action_state(
    state: ContactSelectionActionState,
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    copy_button: &gtk::Button,
) {
    email_button.set_sensitive(state.email);
    export_button.set_sensitive(state.export);
    edit_button.set_sensitive(state.edit);
    merge_button.set_sensitive(state.merge);
    delete_button.set_sensitive(state.delete);
    copy_button.set_sensitive(state.copy);

    let multiple = state.selection_count > 1;
    email_button.set_tooltip_text(Some(if state.email {
        "E-Mail an ausgewählte Kontakte senden"
    } else {
        "Die Auswahl enthält keine E-Mail-Adresse"
    }));
    export_button.set_tooltip_text(Some(if state.export {
        "Ausgewählte Kontakte gemeinsam als vCard exportieren"
    } else if state.selection_count == 0 {
        "Kein Kontakt ausgewählt"
    } else {
        "Die Auswahl enthält keine vollständig exportierbare vCard"
    }));
    edit_button.set_tooltip_text(Some(if multiple {
        "Bearbeiten ist nur für genau einen Kontakt verfügbar"
    } else {
        "Kontakt bearbeiten"
    }));
    merge_button.set_tooltip_text(Some(if multiple {
        "Zusammenführen ist nur für genau einen Kontakt verfügbar"
    } else {
        "Dubletten derselben Quelle zusammenführen"
    }));
    delete_button.set_tooltip_text(Some(if multiple {
        "Mehrere Kontakte können noch nicht atomar gelöscht werden"
    } else {
        "Kontakt löschen"
    }));
}

fn set_contact_filter(contact_list: &gtk::ListBox, search_state: Rc<ContactPageSearchState>) {
    contact_list.set_filter_func(move |row| unsafe {
        let source_uid = row
            .data::<String>("contact-source-uid")
            .map(|value| value.as_ref().clone())
            .unwrap_or_default();
        let uid = row
            .data::<String>("contact-id")
            .map(|value| value.as_ref().clone())
            .unwrap_or_default();
        search_state
            .visible_identities
            .borrow()
            .contains(&ContactIdentity::new(source_uid, uid))
    });
}

fn quick_field_for_index(index: u32) -> ContactSearchField {
    match index {
        1 => ContactSearchField::Name,
        2 => ContactSearchField::Email,
        3 => ContactSearchField::Phone,
        4 => ContactSearchField::Category,
        _ => ContactSearchField::AnyField,
    }
}

fn quick_query(field: ContactSearchField, text: &str) -> ContactSearchQuery {
    if text.trim().is_empty() {
        ContactSearchQuery::all()
    } else if field == ContactSearchField::AnyField {
        ContactSearchQuery::global_search(text)
    } else {
        ContactSearchQuery::contains(field, text)
    }
}

fn current_outer_filters(
    selected_book: &Rc<RefCell<String>>,
    selected_category: &Rc<RefCell<String>>,
    unmatched_category: bool,
    duplicate_only: bool,
) -> crate::services::contact_search_service::ContactOuterFilters {
    let source_uid = selected_book.borrow().trim().to_string();
    let category = selected_category.borrow().trim().to_string();
    crate::services::contact_search_service::ContactOuterFilters {
        source_uid: (!source_uid.is_empty()).then_some(source_uid),
        category: (!category.is_empty()).then_some(category),
        unmatched_category,
        duplicate_only,
    }
}

fn with_contact_outer_filters(
    mut query: ContactSearchQuery,
    selected_book: &Rc<RefCell<String>>,
    selected_category: &Rc<RefCell<String>>,
    unmatched_category: bool,
    duplicate_only: bool,
) -> ContactSearchQuery {
    query.filters = current_outer_filters(
        selected_book,
        selected_category,
        unmatched_category,
        duplicate_only,
    );
    query
}

fn snapshot_records_cancelable(
    records: &[EBookContactRecord],
    cancellation: &AtomicBool,
) -> Option<Vec<ContactSearchSnapshot>> {
    let mut snapshots = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        if index % 16 == 0 && cancellation.load(Ordering::Relaxed) {
            return None;
        }
        snapshots.push(ContactSearchSnapshot::with_source_display_name(
            record.source_uid.clone(),
            record.uid.clone(),
            record.source_display_name.clone(),
            record.contact.clone(),
        ));
    }
    Some(snapshots)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContactSelectionRestore {
    generation: u64,
    identity: ContactIdentity,
}

fn row_contact_identity(row: &gtk::ListBoxRow) -> Option<ContactIdentity> {
    unsafe {
        Some(ContactIdentity::new(
            row.data::<String>("contact-source-uid")?.as_ref().clone(),
            row.data::<String>("contact-id")?.as_ref().clone(),
        ))
    }
}

fn selected_contact_identity(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
) -> Option<ContactIdentity> {
    contact_list
        .selected_row()
        .as_ref()
        .and_then(row_contact_identity)
        .or_else(|| page.imp().selected_identity.borrow().clone())
}

fn selected_contact_records_checked(
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
) -> Result<Vec<EBookContactRecord>, String> {
    let identities = contact_list
        .selected_rows()
        .iter()
        .filter_map(row_contact_identity)
        .collect::<Vec<_>>();
    if identities.is_empty() {
        return Err("Keine Kontakte ausgewählt".to_string());
    }

    let records = stored_contacts.borrow();
    let mut selected = Vec::with_capacity(identities.len());
    for identity in identities {
        let matches = records
            .iter()
            .filter(|record| record.source_uid == identity.source_uid && record.uid == identity.uid)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [record] => selected.push((*record).clone()),
            _ => {
                return Err(
                    "Die Kontaktauswahl ist nicht mehr aktuell; bitte neu auswählen".to_string(),
                );
            }
        }
    }
    Ok(selected)
}

fn selected_contact_recipients(
    selected: &[EBookContactRecord],
    records: &[EBookContactRecord],
) -> String {
    let mut recipients = Vec::new();
    for record in selected {
        for recipient in contact_recipient_text_with_records(&record.contact, records).split(',') {
            push_unique_recipient(&mut recipients, recipient);
        }
    }
    recipients.join(", ")
}

fn plan_contact_vcard_export(
    selected: &[EBookContactRecord],
) -> Result<ContactVCardExportPlan, ContactVCardExportError> {
    if selected.is_empty() {
        return Err(ContactVCardExportError::NoSelection);
    }

    let mut payload = Vec::new();
    for record in selected {
        if VCardContact::parse_vcard_string(&record.vcard).is_none() {
            return Err(ContactVCardExportError::InvalidVCard {
                contact_name: record.contact.fn_name.clone(),
            });
        }
        // Retain every provider-supplied byte except trailing line-breaks;
        // one CRLF is then appended so adjacent cards stay distinct without
        // reserializing vCard 3/4 or extension data through a lossy UI path.
        let vcard = record.vcard.trim_end_matches(['\r', '\n']);
        payload.extend_from_slice(vcard.as_bytes());
        payload.extend_from_slice(b"\r\n");
    }

    Ok(ContactVCardExportPlan {
        initial_name: if selected.len() == 1 {
            "kontakt.vcf".to_string()
        } else {
            "kontakte.vcf".to_string()
        },
        payload,
        contact_count: selected.len(),
    })
}

fn save_contact_vcard_export_async(
    button: &gtk::Button,
    file: gio::File,
    plan: ContactVCardExportPlan,
) {
    let button = button.clone();
    let contact_count = plan.contact_count;
    file.replace_contents_async(
        plan.payload,
        None,
        true,
        gio::FileCreateFlags::NONE,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(_) if contact_count == 1 => {
                show_toast_for_widget(&button, "Kontakt als vCard exportiert")
            }
            Ok(_) => show_toast_for_widget(
                &button,
                &format!("{contact_count} Kontakte als eine vCard-Datei exportiert"),
            ),
            Err((_, error)) => show_toast_for_widget(
                &button,
                &format!("vCard konnte nicht gespeichert werden: {error}"),
            ),
        },
    );
}

fn reset_contact_detail(
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    detail_extra: &gtk::Label,
    email_row: &adw::ActionRow,
) {
    detail_avatar.set_text(Some(""));
    detail_avatar.set_custom_image(None::<&gdk::Texture>);
    detail_name.set_label("Kein Kontakt ausgewählt");
    detail_title.set_label("Adressbuchdaten werden nach der Synchronisation angezeigt");
    detail_extra.set_label("Keine weiteren Kontaktdaten");
    email_row.set_title("Keine E-Mail-Adresse");
    email_row.set_subtitle("Kein Kontakt ausgewählt");
}

#[allow(clippy::too_many_arguments)]
fn sync_contact_selection(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
    selected_contact: &Rc<RefCell<Option<EBookContactRecord>>>,
    split: &adw::NavigationSplitView,
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    detail_extra: &gtk::Label,
    email_row: &adw::ActionRow,
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    copy_button: &gtk::Button,
) {
    let selected = match selected_contact_records_checked(contact_list, stored_contacts) {
        Ok(selected) => selected,
        Err(error) if error == "Keine Kontakte ausgewählt" => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, "Contact selection no longer maps to the loaded records");
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            reset_contact_detail(
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
            split.set_show_content(false);
            apply_contact_selection_action_state(
                ContactSelectionActionState {
                    selection_count: 0,
                    email: false,
                    export: false,
                    edit: false,
                    merge: false,
                    delete: false,
                    copy: false,
                },
                email_button,
                export_button,
                edit_button,
                merge_button,
                delete_button,
                copy_button,
            );
            return;
        }
    };
    let state = contact_selection_action_state(&selected, &stored_contacts.borrow());

    match selected.as_slice() {
        [] => {
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            reset_contact_detail(
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
            split.set_show_content(false);
        }
        [record] => {
            *selected_contact.borrow_mut() = Some(record.clone());
            page.imp()
                .selected_identity
                .replace(Some(ContactIdentity::new(
                    record.source_uid.clone(),
                    record.uid.clone(),
                )));
            update_contact_detail(
                record,
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
            split.set_show_content(true);
        }
        records => {
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            detail_avatar.set_text(Some(""));
            detail_avatar.set_custom_image(None::<&gdk::Texture>);
            detail_name.set_label(&format!("{} Kontakte ausgewählt", records.len()));
            detail_title.set_label(
                "E-Mail und vCard-Export verwenden die vollständige Auswahl; Bearbeiten, Zusammenführen und Löschen benötigen genau einen Kontakt.",
            );
            email_row.set_title(&format!("{} Kontakte ausgewählt", records.len()));
            email_row.set_subtitle(if state.email {
                "E-Mail-Adressen können gemeinsam verwendet werden"
            } else {
                "Keine E-Mail-Adressen in der Auswahl"
            });
            detail_extra.set_label(
                "Mehrfachauswahl: Der Export schreibt alle ausgewählten, geprüften vCards in eine Datei. Mehrfaches Löschen ist noch nicht verfügbar.",
            );
            split.set_show_content(true);
        }
    }
    apply_contact_selection_action_state(
        state,
        email_button,
        export_button,
        edit_button,
        merge_button,
        delete_button,
        copy_button,
    );
}

fn clear_contact_selection(page: &ContactsPage, contact_list: &gtk::ListBox) {
    contact_list.unselect_all();
    page.imp().selected_identity.replace(None);
}

fn should_restore_contact_selection(
    selection_restore: Option<&ContactSelectionRestore>,
    current_identity: &ContactIdentity,
    generation: u64,
) -> bool {
    selection_restore.is_some_and(|target| {
        target.generation == generation && target.identity == *current_identity
    })
}

fn apply_contact_search_selection(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
    selection_restore: Option<&ContactSelectionRestore>,
    identities: &[ContactIdentity],
    generation: u64,
) {
    let selection_before_apply = selected_contact_identity(page, contact_list);
    contact_list.invalidate_filter();

    let Some(current_identity) = selection_before_apply else {
        return;
    };
    if !identities.contains(&current_identity) {
        clear_contact_selection(page, contact_list);
        return;
    }

    if should_restore_contact_selection(selection_restore, &current_identity, generation) {
        let restored = select_contact_row(
            contact_list,
            &current_identity.source_uid,
            &current_identity.uid,
        );
        if !restored {
            clear_contact_selection(page, contact_list);
        }
    }
}

fn start_contact_search(
    page_weak: glib::WeakRef<ContactsPage>,
    query: ContactSearchQuery,
    search_state: Rc<ContactPageSearchState>,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let Some(page) = page_weak.upgrade() else {
        return;
    };
    if page.imp().disposed.get() {
        return;
    }

    // Invalidate the previous generation before validating the new input.
    // Unsupported/invalid input therefore cannot let an older worker publish
    // after the user has already moved on, while the last valid result stays
    // visible below the status message.
    let request = search_state.generations.begin();
    let generation = request.generation();
    page.imp()
        .contact_search_cancel
        .replace(Some(request.cancellation()));

    if let Err(error) = query.validate() {
        if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
            status.set_text(&format!("Suche nicht angewendet: {error}"));
            status.set_visible(true);
        }
        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
            spinner.stop();
            spinner.set_visible(false);
        }
        return;
    }
    search_state.set_active_query(query.clone());

    let selection_restore = page
        .imp()
        .contact_list
        .borrow()
        .as_ref()
        .and_then(|contact_list| {
            selected_contact_identity(&page, contact_list).map(|identity| ContactSelectionRestore {
                generation,
                identity,
            })
        });
    if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
        status.set_text("Kontakte werden gesucht…");
        status.set_visible(true);
    }
    if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
        spinner.start();
        spinner.set_visible(true);
    }

    let snapshots = {
        let contacts = stored_contacts.borrow();
        let Some(snapshots) = snapshot_records_cancelable(&contacts, &request.cancellation())
        else {
            return;
        };
        snapshots
    };
    let receiver = search_state.worker.submit(
        search_state.generations.clone(),
        request.clone(),
        query.clone(),
        snapshots,
    );

    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if page.imp().disposed.get()
            || search_state.generations.current_generation() != generation
            || !request.is_current()
        {
            return;
        }
        let evaluation = match result {
            Ok(evaluation) => evaluation,
            Err(error) => {
                if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
                    status.set_text(&format!("Suche nicht angewendet: {error}"));
                    status.set_visible(true);
                }
                if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                    spinner.stop();
                    spinner.set_visible(false);
                }
                page.imp().contact_search_cancel.borrow_mut().take();
                return;
            }
        };
        let ContactSearchEvaluation::Results { identities, .. } = evaluation else {
            return;
        };
        let identity_set = identities.iter().cloned().collect::<HashSet<_>>();
        search_state.visible_identities.replace(identity_set);
        search_state
            .last_valid
            .borrow_mut()
            .accept_results(query, identities.clone());

        if let Some(contact_list) = page.imp().contact_list.borrow().as_ref() {
            apply_contact_search_selection(
                &page,
                contact_list,
                selection_restore.as_ref(),
                &identities,
                generation,
            );
        }
        if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
            if identities.is_empty() {
                status.set_text("Keine Kontakte entsprechen der Suche");
                status.set_visible(true);
            } else {
                status.set_visible(false);
            }
        }
        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
            spinner.stop();
            spinner.set_visible(false);
        }
        page.imp().contact_search_cancel.borrow_mut().take();
    });
}

fn reapply_active_contact_search(
    page_weak: glib::WeakRef<ContactsPage>,
    search_state: Rc<ContactPageSearchState>,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let query = search_state.active_query();
    start_contact_search(page_weak, query, search_state, stored_contacts);
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
    crate::services::contact_search_service::normalize_search_text(value)
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
    let detail_title_text = contact_detail_title(&record.contact);
    detail_title.set_label(&detail_title_text);
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
    normalize_contact_query(&field_values(&record.contact, ContactSearchField::AnyField).join(" "))
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
    if contact.is_list {
        if contact.list_show_addresses {
            return contact.list_member_emails().join(", ");
        }
        return format!(
            "Kontaktliste · {} Mitglied(e) · Adressen verborgen",
            contact.list_members.len()
        );
    }
    if contact.emails.is_empty() {
        contact.email.clone()
    } else {
        contact.emails.join(", ")
    }
}

fn contact_recipient_text(contact: &VCardContact) -> String {
    contact_recipient_text_with_records(contact, &[])
}

fn contact_recipient_text_with_records(
    contact: &VCardContact,
    records: &[EBookContactRecord],
) -> String {
    if !contact.is_list {
        return contact_email_text(contact);
    }
    let mut recipients = Vec::new();
    append_list_recipients(contact, records, &mut Vec::new(), &mut recipients);
    recipients.join(", ")
}

fn append_list_recipients(
    contact: &VCardContact,
    records: &[EBookContactRecord],
    visited: &mut Vec<String>,
    recipients: &mut Vec<String>,
) {
    for member in &contact.list_members {
        if !member.email.trim().is_empty() {
            push_unique_recipient(recipients, &member.email);
        }
        let Some(member_uid) = member
            .contact_uid
            .trim()
            .strip_prefix("urn:uuid:")
            .or_else(|| {
                (!member.contact_uid.trim().is_empty()).then_some(member.contact_uid.trim())
            })
        else {
            continue;
        };
        if !matches!(
            member.kind,
            VCardListMemberKind::NestedList | VCardListMemberKind::StandardMember
        ) {
            continue;
        }
        if visited.iter().any(|uid| uid == member_uid) {
            continue;
        }
        let Some(record) = records.iter().find(|record| record.uid == member_uid) else {
            continue;
        };
        visited.push(member_uid.to_string());
        if record.contact.is_list {
            append_list_recipients(&record.contact, records, visited, recipients);
        } else {
            for email in if record.contact.emails.is_empty() {
                std::slice::from_ref(&record.contact.email)
            } else {
                record.contact.emails.as_slice()
            } {
                if !email.trim().is_empty() {
                    push_unique_recipient(recipients, email);
                }
            }
        }
        visited.pop();
    }
}

fn push_unique_recipient(recipients: &mut Vec<String>, email: &str) {
    let email = email.trim();
    if !email.is_empty()
        && !recipients
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(email))
    {
        recipients.push(email.to_string());
    }
}

fn contact_phone_text(contact: &VCardContact) -> String {
    if contact.is_list {
        return String::new();
    }
    if contact.phones.is_empty() {
        contact.tel.clone()
    } else {
        contact.phones.join(", ")
    }
}

fn contact_detail_title(contact: &VCardContact) -> String {
    if contact.is_list {
        format!("Kontaktliste · {} Mitglied(e)", contact.list_members.len())
    } else if contact.title.is_empty() {
        contact.org.clone()
    } else {
        contact.title.clone()
    }
}

fn contact_detail_summary(contact: &VCardContact) -> String {
    let mut lines = Vec::new();
    if contact.is_list {
        lines.push(format!(
            "Kontaktliste: {} Mitglied(e)",
            contact.list_members.len()
        ));
        if contact.list_show_addresses {
            let members = contact
                .list_members
                .iter()
                .map(|member| member.display_text(true))
                .collect::<Vec<_>>();
            if !members.is_empty() {
                lines.push(format!("Mitglieder: {}", members.join(" · ")));
            }
        } else {
            lines.push("Mitgliederadressen: verborgen".to_string());
        }
    }
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
        ContactActionState, ContactIdentity, ContactPageSearchState, ContactSelectionRestore,
        ContactVCardExportError, confirmed_contact_uid, contact_action_state,
        contact_category_matches, contact_detail_summary, contact_email_text,
        contact_load_status_message, contact_query_matches, contact_recipient_text,
        contact_recipient_text_with_records, contact_records_are_duplicates,
        contact_records_share_duplicate_key, contact_selection_action_state, contact_sort_key,
        contact_source_label, contact_status_needs_visibility, context_action_state,
        plan_contact_vcard_export, should_restore_contact_selection,
    };
    use crate::services::contact_search_service::{
        ContactClauseGroup, ContactOuterFilters, ContactSearchClause, ContactSearchField,
        ContactSearchOperator, ContactSearchQuery,
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
    fn outer_filters_change_only_the_active_query_filters() {
        let state = ContactPageSearchState::default();
        let advanced = ContactSearchQuery::new(
            ContactClauseGroup::Any,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Contains,
                    "Ada",
                ),
                ContactSearchClause::new(
                    ContactSearchField::AnyField,
                    ContactSearchOperator::Contains,
                    "example.test",
                ),
            ],
        );
        state.set_active_query(advanced.clone());
        let updated = state.active_query_with_filters(
            ContactOuterFilters::default()
                .source("book-a")
                .category("Work")
                .unmatched_category(true)
                .duplicate_only(true),
        );

        assert_eq!(updated.group, advanced.group);
        assert_eq!(updated.clauses, advanced.clauses);
        assert_eq!(
            updated.filters,
            ContactOuterFilters {
                source_uid: Some("book-a".to_string()),
                category: Some("Work".to_string()),
                unmatched_category: true,
                duplicate_only: true,
            }
        );
        assert_eq!(state.active_query(), updated);
    }

    #[test]
    fn selection_restoration_is_scoped_to_the_search_generation() {
        let identity = ContactIdentity::new("book-a", "ada");
        let target = ContactSelectionRestore {
            generation: 7,
            identity: identity.clone(),
        };

        assert!(should_restore_contact_selection(
            Some(&target),
            &identity,
            7
        ));
        assert!(!should_restore_contact_selection(
            Some(&target),
            &identity,
            8
        ));
        assert!(!should_restore_contact_selection(None, &identity, 7));
    }

    #[test]
    fn zero_selection_disables_every_selection_action_and_imports_are_read_only() {
        assert_eq!(
            contact_action_state(None, &[]),
            ContactActionState {
                email: false,
                export: false,
                edit: false,
                merge: false,
                delete: false,
                copy: false,
            }
        );

        let imported = record(
            "",
            "imported",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let state = contact_action_state(Some(&imported), std::slice::from_ref(&imported));
        let context_state = context_action_state(Some(&imported), std::slice::from_ref(&imported));
        assert!(state.email);
        assert!(state.export);
        assert!(!state.edit);
        assert!(!state.merge);
        assert!(!state.delete);
        assert!(state.copy);
        assert_eq!(context_state, state);
    }

    #[test]
    fn multi_selection_keeps_only_batch_safe_actions_enabled() {
        let ada = record(
            "book-a",
            "ada",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let grace = record(
            "book-a",
            "grace",
            VCardContact::new("Grace", "grace@example.test", "", ""),
        );
        let state = contact_selection_action_state(
            &[ada.clone(), grace.clone()],
            &[ada.clone(), grace.clone()],
        );

        assert_eq!(state.selection_count, 2);
        assert!(state.email);
        assert!(state.export);
        assert!(!state.edit);
        assert!(!state.merge);
        assert!(!state.delete);
        assert!(!state.copy);
    }

    #[test]
    fn multi_vcard_export_is_planned_as_one_complete_payload() {
        let ada = record(
            "book-a",
            "ada",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let grace = record(
            "book-b",
            "grace",
            VCardContact::new("Grace", "grace@example.test", "", ""),
        );
        let plan = plan_contact_vcard_export(&[ada.clone(), grace.clone()])
            .expect("both complete vCards should form one export");

        assert_eq!(plan.initial_name, "kontakte.vcf");
        assert_eq!(plan.contact_count, 2);
        let payload = String::from_utf8(plan.payload).expect("vCard output is UTF-8");
        assert_eq!(payload.matches("BEGIN:VCARD").count(), 2);
        assert!(payload.contains(&ada.vcard.trim().to_string()));
        assert!(payload.contains(&grace.vcard.trim().to_string()));

        let mut broken = grace;
        broken.vcard = "not a vCard".to_string();
        assert_eq!(
            plan_contact_vcard_export(&[ada, broken]),
            Err(ContactVCardExportError::InvalidVCard {
                contact_name: "Grace".to_string(),
            })
        );
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

    #[test]
    fn contact_list_display_hides_addresses_but_keeps_recipients_for_mail() {
        let mut list = VCardContact::new_list("Projektteam", false, Some("list-1"));
        list.list_members = vec![
            crate::services::vcard_service::VCardListMember::external_address(
                "external@example.test",
                "Extern",
            ),
            crate::services::vcard_service::VCardListMember::local_contact(
                "contact-1",
                "local-addressbook",
                "Ada Lovelace",
                "ada@example.test",
                Some(0),
            ),
        ];
        assert!(contact_email_text(&list).contains("Adressen verborgen"));
        assert!(!contact_email_text(&list).contains("external@example.test"));
        assert_eq!(
            contact_recipient_text(&list),
            "external@example.test, ada@example.test"
        );
        assert!(contact_detail_summary(&list).contains("Mitgliederadressen: verborgen"));
    }

    #[test]
    fn nested_list_recipients_are_resolved_without_leaking_hidden_display_text() {
        let mut nested = VCardContact::new_list("Unterteam", true, Some("nested-list"));
        nested.list_members = vec![
            crate::services::vcard_service::VCardListMember::external_address(
                "nested@example.test",
                "Unterteam-Adresse",
            ),
        ];
        let mut outer = VCardContact::new_list("Team", false, Some("outer-list"));
        outer.list_members = vec![
            crate::services::vcard_service::VCardListMember::nested_list(
                "nested-list",
                "Unterteam",
                None,
            ),
        ];
        let nested_record = record("book-a", "nested-list", nested);
        assert_eq!(contact_recipient_text(&outer), "");
        assert_eq!(
            contact_recipient_text_with_records(&outer, &[nested_record]),
            "nested@example.test"
        );
        assert!(contact_email_text(&outer).contains("Adressen verborgen"));
        assert!(!contact_email_text(&outer).contains("nested@example.test"));
    }
}
