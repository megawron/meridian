// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for saving a portable local search definition.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

use crate::services::search_folder_service::{SavedSearch, SearchFolderService};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct SearchFolderDialog {
        pub initial_query: RefCell<String>,
        pub search_id: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchFolderDialog {
        const NAME: &'static str = "EvolutionSearchFolderDialog";
        type Type = super::SearchFolderDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for SearchFolderDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("initial-query").build(),
                    glib::ParamSpecString::builder("search-id").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "initial-query" => {
                    *self.initial_query.borrow_mut() = value.get().expect("string property")
                }
                "search-id" => {
                    let id: String = value.get().expect("string property");
                    *self.search_id.borrow_mut() = (!id.trim().is_empty()).then_some(id);
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "initial-query" => self.initial_query.borrow().to_value(),
                "search-id" => self.search_id.borrow().as_deref().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for SearchFolderDialog {}
    impl WindowImpl for SearchFolderDialog {}
    impl AdwWindowImpl for SearchFolderDialog {}
}

glib::wrapper! {
    pub struct SearchFolderDialog(ObjectSubclass<imp::SearchFolderDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl SearchFolderDialog {
    pub fn new(initial_query: &str) -> Self {
        glib::Object::builder()
            .property("title", "Lokalen Suchordner speichern")
            .property("modal", true)
            .property("default-width", 520)
            .property("default-height", 260)
            .property("initial-query", initial_query)
            .property("search-id", "")
            .build()
    }

    pub fn new_for_edit(search: &SavedSearch) -> Self {
        glib::Object::builder()
            .property("title", "Lokalen Suchordner bearbeiten")
            .property("modal", true)
            .property("default-width", 520)
            .property("default-height", 260)
            .property("initial-query", &search.query)
            .property("search-id", &search.id)
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
        if let Some(search_id) = self.imp().search_id.borrow().as_deref() {
            if let Some(search) = SearchFolderService::default()
                .load_saved()
                .into_iter()
                .find(|search| search.id == search_id)
            {
                name.set_text(&search.name);
            }
        }
        let initial_query = self.imp().initial_query.borrow().clone();
        let query = adw::EntryRow::builder()
            .title("Suchabfrage")
            .text(initial_query)
            .build();
        query.set_tooltip_text(Some("z. B. from:billing@example.org is:unread"));
        content.append(&name);
        content.append(&query);
        toolbar.set_content(Some(&content));
        self.set_content(Some(&toolbar));

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
                match SearchFolderService::default().save_saved(SavedSearch {
                    id: dialog
                        .imp()
                        .search_id
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name: name_value,
                    query: query_value,
                }) {
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
        if let Some(parent) = self.transient_for() {
            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Search folder dialog result");
    }
}

impl Default for SearchFolderDialog {
    fn default() -> Self {
        Self::new("")
    }
}
