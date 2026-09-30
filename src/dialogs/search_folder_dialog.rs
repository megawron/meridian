// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for saving a portable local search definition.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};

use crate::services::search_folder_service::{
    SavedSearch, SavedSearchFieldMode, SavedSearchScope, SearchFolderService,
};

const FIELD_LABELS: &[&str] = &[
    "Betreff oder Adressen",
    "Betreff",
    "Absender",
    "Empfänger",
    "Nachricht",
    "Nachrichtentext",
    "Freier Ausdruck",
];

const SCOPE_LABELS: &[&str] = &[
    "Aktueller Ordner",
    "Aktueller Ordner und Unterordner",
    "Aktuelles Konto",
    "Alle Konten",
];

fn field_key(field: SavedSearchFieldMode) -> &'static str {
    match field {
        SavedSearchFieldMode::SubjectOrAddresses => "subject_or_addresses",
        SavedSearchFieldMode::Subject => "subject",
        SavedSearchFieldMode::Sender => "sender",
        SavedSearchFieldMode::Recipients => "recipients",
        SavedSearchFieldMode::Message => "message",
        SavedSearchFieldMode::Body => "body",
        SavedSearchFieldMode::FreeForm => "free_form",
    }
}

fn field_from_key(key: &str) -> SavedSearchFieldMode {
    match key {
        "subject_or_addresses" => SavedSearchFieldMode::SubjectOrAddresses,
        "subject" => SavedSearchFieldMode::Subject,
        "sender" => SavedSearchFieldMode::Sender,
        "recipients" => SavedSearchFieldMode::Recipients,
        "message" => SavedSearchFieldMode::Message,
        "body" => SavedSearchFieldMode::Body,
        "free_form" => SavedSearchFieldMode::FreeForm,
        _ => SavedSearchFieldMode::FreeForm,
    }
}

fn field_index(field: SavedSearchFieldMode) -> u32 {
    match field {
        SavedSearchFieldMode::SubjectOrAddresses => 0,
        SavedSearchFieldMode::Subject => 1,
        SavedSearchFieldMode::Sender => 2,
        SavedSearchFieldMode::Recipients => 3,
        SavedSearchFieldMode::Message => 4,
        SavedSearchFieldMode::Body => 5,
        SavedSearchFieldMode::FreeForm => 6,
    }
}

fn field_from_index(index: u32) -> SavedSearchFieldMode {
    match index {
        0 => SavedSearchFieldMode::SubjectOrAddresses,
        1 => SavedSearchFieldMode::Subject,
        2 => SavedSearchFieldMode::Sender,
        3 => SavedSearchFieldMode::Recipients,
        4 => SavedSearchFieldMode::Message,
        5 => SavedSearchFieldMode::Body,
        _ => SavedSearchFieldMode::FreeForm,
    }
}

fn scope_key(scope: SavedSearchScope) -> &'static str {
    match scope {
        SavedSearchScope::CurrentFolder => "current_folder",
        SavedSearchScope::CurrentFolderAndSubfolders => "current_folder_and_subfolders",
        SavedSearchScope::CurrentAccount => "current_account",
        SavedSearchScope::AllAccounts => "all_accounts",
    }
}

fn scope_from_key(key: &str) -> SavedSearchScope {
    match key {
        "current_folder" => SavedSearchScope::CurrentFolder,
        "current_folder_and_subfolders" => SavedSearchScope::CurrentFolderAndSubfolders,
        "current_account" => SavedSearchScope::CurrentAccount,
        "all_accounts" => SavedSearchScope::AllAccounts,
        _ => SavedSearchScope::AllAccounts,
    }
}

fn scope_index(scope: SavedSearchScope) -> u32 {
    match scope {
        SavedSearchScope::CurrentFolder => 0,
        SavedSearchScope::CurrentFolderAndSubfolders => 1,
        SavedSearchScope::CurrentAccount => 2,
        SavedSearchScope::AllAccounts => 3,
    }
}

fn scope_from_index(index: u32) -> SavedSearchScope {
    match index {
        0 => SavedSearchScope::CurrentFolder,
        1 => SavedSearchScope::CurrentFolderAndSubfolders,
        2 => SavedSearchScope::CurrentAccount,
        _ => SavedSearchScope::AllAccounts,
    }
}

fn target_description(
    scope: SavedSearchScope,
    folder_id: Option<&str>,
    source_uid: Option<&str>,
    folder_name: Option<&str>,
) -> String {
    match scope {
        SavedSearchScope::AllAccounts => "Alle Konten".to_string(),
        SavedSearchScope::CurrentAccount => source_uid
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("Konto: {value}"))
            .unwrap_or_else(|| "Kein Konto ausgewählt".to_string()),
        SavedSearchScope::CurrentFolder | SavedSearchScope::CurrentFolderAndSubfolders => {
            match (source_uid, folder_name, folder_id) {
                (Some(source_uid), Some(folder_name), _) => {
                    format!("Konto {source_uid} / Ordner {folder_name}")
                }
                (None, None, Some(folder_id)) => format!("Lokaler Ordner: {folder_id}"),
                _ => "Kein Ordner ausgewählt".to_string(),
            }
        }
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct SearchFolderDialog {
        pub initial_name: RefCell<String>,
        pub initial_query: RefCell<String>,
        pub search_id: RefCell<Option<String>>,
        pub field: Cell<SavedSearchFieldMode>,
        pub scope: Cell<SavedSearchScope>,
        pub folder_id: RefCell<Option<String>>,
        pub source_uid: RefCell<Option<String>>,
        pub folder_name: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchFolderDialog {
        const NAME: &'static str = "MeridianSearchFolderDialog";
        type Type = super::SearchFolderDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for SearchFolderDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("initial-name").build(),
                    glib::ParamSpecString::builder("initial-query").build(),
                    glib::ParamSpecString::builder("search-id").build(),
                    glib::ParamSpecString::builder("field").build(),
                    glib::ParamSpecString::builder("scope").build(),
                    glib::ParamSpecString::builder("folder-id").build(),
                    glib::ParamSpecString::builder("source-uid").build(),
                    glib::ParamSpecString::builder("folder-name").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "initial-name" => {
                    *self.initial_name.borrow_mut() = value.get().expect("string property")
                }
                "initial-query" => {
                    *self.initial_query.borrow_mut() = value.get().expect("string property")
                }
                "search-id" => {
                    let id: String = value.get().expect("string property");
                    *self.search_id.borrow_mut() = (!id.trim().is_empty()).then_some(id);
                }
                "field" => {
                    let field: String = value.get().expect("string property");
                    self.field.set(field_from_key(&field));
                }
                "scope" => {
                    let scope: String = value.get().expect("string property");
                    self.scope.set(scope_from_key(&scope));
                }
                "folder-id" => {
                    let folder_id: String = value.get().expect("string property");
                    *self.folder_id.borrow_mut() =
                        (!folder_id.trim().is_empty()).then_some(folder_id);
                }
                "source-uid" => {
                    let source_uid: String = value.get().expect("string property");
                    *self.source_uid.borrow_mut() =
                        (!source_uid.trim().is_empty()).then_some(source_uid);
                }
                "folder-name" => {
                    let folder_name: String = value.get().expect("string property");
                    *self.folder_name.borrow_mut() =
                        (!folder_name.trim().is_empty()).then_some(folder_name);
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "initial-name" => self.initial_name.borrow().to_value(),
                "initial-query" => self.initial_query.borrow().to_value(),
                "search-id" => self.search_id.borrow().as_deref().to_value(),
                "field" => field_key(self.field.get()).to_value(),
                "scope" => scope_key(self.scope.get()).to_value(),
                "folder-id" => self.folder_id.borrow().as_deref().to_value(),
                "source-uid" => self.source_uid.borrow().as_deref().to_value(),
                "folder-name" => self.folder_name.borrow().as_deref().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for SearchFolderDialog {}
    impl AdwDialogImpl for SearchFolderDialog {}
}

glib::wrapper! {
    pub struct SearchFolderDialog(ObjectSubclass<imp::SearchFolderDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SearchFolderDialog {
    pub fn new(initial_query: &str) -> Self {
        Self::new_with_metadata(
            initial_query,
            SavedSearchFieldMode::FreeForm,
            SavedSearchScope::AllAccounts,
            None,
            None,
            None,
        )
    }

    /// Open a new saved-search dialog with the complete current UI context.
    /// The identity values are copied as owned strings before any GTK signal
    /// handler is connected, so saving never has to infer a target later.
    pub fn new_with_metadata(
        initial_query: &str,
        field: SavedSearchFieldMode,
        scope: SavedSearchScope,
        folder_id: Option<String>,
        source_uid: Option<String>,
        folder_name: Option<String>,
    ) -> Self {
        glib::Object::builder()
            .property("title", "Lokalen Suchordner speichern")
            .property("content-width", 520)
            .property("content-height", 360)
            .property("initial-name", "")
            .property("initial-query", initial_query)
            .property("search-id", "")
            .property("field", field_key(field))
            .property("scope", scope_key(scope))
            .property("folder-id", folder_id.as_deref().unwrap_or_default())
            .property("source-uid", source_uid.as_deref().unwrap_or_default())
            .property("folder-name", folder_name.as_deref().unwrap_or_default())
            .build()
    }

    pub fn new_for_edit(search: &SavedSearch) -> Self {
        glib::Object::builder()
            .property("title", "Lokalen Suchordner bearbeiten")
            .property("content-width", 520)
            .property("content-height", 360)
            .property("initial-name", &search.name)
            .property("initial-query", &search.query)
            .property("search-id", &search.id)
            .property("field", field_key(search.field))
            .property("scope", scope_key(search.scope))
            .property("folder-id", search.folder_id.as_deref().unwrap_or_default())
            .property(
                "source-uid",
                search.source_uid.as_deref().unwrap_or_default(),
            )
            .property(
                "folder-name",
                search.folder_name.as_deref().unwrap_or_default(),
            )
            .build()
    }

    fn build_ui(&self) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(20)
            .margin_end(20)
            .margin_top(20)
            .margin_bottom(20)
            .build();
        let name = adw::EntryRow::builder().title("Name").build();
        name.set_tooltip_text(Some("z. B. Rechnungen"));
        name.set_text(&self.imp().initial_name.borrow());
        let initial_query = self.imp().initial_query.borrow().clone();
        let query = adw::EntryRow::builder()
            .title("Suchabfrage")
            .text(initial_query)
            .build();
        query.set_tooltip_text(Some("z. B. from:billing@example.org is:unread"));

        let field_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let field_label = gtk::Label::builder()
            .label("Suchfeld")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        let field_selector = gtk::DropDown::new(
            Some(gtk::StringList::new(FIELD_LABELS)),
            None::<&gtk::Expression>,
        );
        field_selector.set_selected(field_index(self.imp().field.get()));
        field_selector.set_hexpand(false);
        field_selector.set_tooltip_text(Some("Suchfeld für unqualifizierte Begriffe"));
        field_row.append(&field_label);
        field_row.append(&field_selector);

        let scope_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let scope_label = gtk::Label::builder()
            .label("Suchbereich")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        let scope_selector = gtk::DropDown::new(
            Some(gtk::StringList::new(SCOPE_LABELS)),
            None::<&gtk::Expression>,
        );
        scope_selector.set_selected(scope_index(self.imp().scope.get()));
        scope_selector.set_hexpand(false);
        scope_selector.set_tooltip_text(Some("Ordner- oder Kontobereich der Suche"));
        scope_row.append(&scope_label);
        scope_row.append(&scope_selector);

        let target_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let target_label = gtk::Label::builder()
            .label("Ziel")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        let target_value = gtk::Label::builder()
            .halign(gtk::Align::End)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(60)
            .css_classes(vec!["dim-label"])
            .build();
        target_value.set_label(&target_description(
            self.imp().scope.get(),
            self.imp().folder_id.borrow().as_deref(),
            self.imp().source_uid.borrow().as_deref(),
            self.imp().folder_name.borrow().as_deref(),
        ));
        target_row.append(&target_label);
        target_row.append(&target_value);

        let target_value_for_scope = target_value.clone();
        let folder_id_for_scope = self.imp().folder_id.borrow().clone();
        let source_uid_for_scope = self.imp().source_uid.borrow().clone();
        let folder_name_for_scope = self.imp().folder_name.borrow().clone();
        scope_selector.connect_selected_notify(move |selector| {
            let scope = scope_from_index(selector.selected());
            target_value_for_scope.set_label(&target_description(
                scope,
                folder_id_for_scope.as_deref(),
                source_uid_for_scope.as_deref(),
                folder_name_for_scope.as_deref(),
            ));
        });

        content.append(&name);
        content.append(&query);
        content.append(&field_row);
        content.append(&scope_row);
        content.append(&target_row);
        toolbar.set_content(Some(&content));
        self.set_child(Some(&toolbar));

        save.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let name_value = name.text().trim().to_string();
                let query_value = query.text().trim().to_string();
                if name_value.is_empty() || query_value.is_empty() {
                    dialog.show_toast("Name und Suchabfrage sind erforderlich");
                    return;
                }
                let scope = scope_from_index(scope_selector.selected());
                let (folder_id, source_uid, folder_name) = match scope {
                    SavedSearchScope::AllAccounts => (None, None, None),
                    SavedSearchScope::CurrentAccount => {
                        (None, dialog.imp().source_uid.borrow().clone(), None)
                    }
                    SavedSearchScope::CurrentFolder
                    | SavedSearchScope::CurrentFolderAndSubfolders => (
                        dialog.imp().folder_id.borrow().clone(),
                        dialog.imp().source_uid.borrow().clone(),
                        dialog.imp().folder_name.borrow().clone(),
                    ),
                };
                let field = field_from_index(field_selector.selected());
                let id = dialog
                    .imp()
                    .search_id
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let saved = SavedSearch::with_metadata(
                    id,
                    name_value,
                    query_value,
                    field,
                    scope,
                    folder_id,
                    source_uid,
                    folder_name,
                );
                match saved.and_then(|saved| SearchFolderService::default().save_saved(saved)) {
                    Ok(()) => {
                        dialog.show_toast("Suchordner gespeichert");
                        dialog.close();
                    }
                    Err(error) => dialog.show_toast(&format!(
                        "Suchordner konnte nicht gespeichert werden: {error}"
                    )),
                }
            }
        ));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
        tracing::warn!(%message, "Search folder dialog result");
    }
}

impl Default for SearchFolderDialog {
    fn default() -> Self {
        Self::new("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_storage_keys_roundtrip_without_falling_back() {
        let fields = [
            SavedSearchFieldMode::SubjectOrAddresses,
            SavedSearchFieldMode::Subject,
            SavedSearchFieldMode::Sender,
            SavedSearchFieldMode::Recipients,
            SavedSearchFieldMode::Message,
            SavedSearchFieldMode::Body,
            SavedSearchFieldMode::FreeForm,
        ];
        for field in fields {
            assert_eq!(field_from_key(field_key(field)), field);
            assert_eq!(field_from_index(field_index(field)), field);
        }
    }

    #[test]
    fn scope_storage_keys_roundtrip_without_falling_back() {
        let scopes = [
            SavedSearchScope::CurrentFolder,
            SavedSearchScope::CurrentFolderAndSubfolders,
            SavedSearchScope::CurrentAccount,
            SavedSearchScope::AllAccounts,
        ];
        for scope in scopes {
            assert_eq!(scope_from_key(scope_key(scope)), scope);
            assert_eq!(scope_from_index(scope_index(scope)), scope);
        }
    }

    #[test]
    fn target_description_keeps_local_and_remote_identity_distinct() {
        assert_eq!(
            target_description(
                SavedSearchScope::CurrentFolder,
                Some("custom:Projects"),
                None,
                None,
            ),
            "Lokaler Ordner: custom:Projects"
        );
        assert_eq!(
            target_description(
                SavedSearchScope::CurrentFolder,
                Some("account\u{0}Inbox"),
                Some("account"),
                Some("Inbox"),
            ),
            "Konto account / Ordner Inbox"
        );
        assert_eq!(
            target_description(
                SavedSearchScope::CurrentAccount,
                None,
                Some("account"),
                None,
            ),
            "Konto: account"
        );
    }

    #[test]
    fn dialog_metadata_builder_recomputes_ast_without_losing_target() {
        let search = SavedSearch::with_metadata(
            "saved-1",
            "Inbox projects",
            "needle",
            SavedSearchFieldMode::Subject,
            SavedSearchScope::CurrentFolder,
            Some("account\u{0}Inbox".to_string()),
            Some("account".to_string()),
            Some("Inbox".to_string()),
        )
        .expect("metadata should be valid");
        assert_eq!(search.field, SavedSearchFieldMode::Subject);
        assert_eq!(search.scope, SavedSearchScope::CurrentFolder);
        assert_eq!(search.folder_id.as_deref(), Some("account\u{0}Inbox"));
        assert_eq!(search.source_uid.as_deref(), Some("account"));
        assert_eq!(search.folder_name.as_deref(), Some("Inbox"));
        assert!(!search.ast.is_empty());
    }
}
