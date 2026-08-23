// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for managing remote folder subscriptions (IMAP / Exchange) in GTK4/libadwaita.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct SubscribedFolderItem {
    pub name: String,
    pub path: String,
    pub is_subscribed: bool,
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct FolderSubscriptionsDialog {
        pub account_name: RefCell<String>,
        pub source_uid: RefCell<Option<String>>,
        pub folders: RefCell<Vec<SubscribedFolderItem>>,
        pub subscribed_paths: RefCell<HashSet<String>>,
        pub list_box: RefCell<Option<gtk::ListBox>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderSubscriptionsDialog {
        const NAME: &'static str = "EvolutionFolderSubscriptionsDialog";
        type Type = super::FolderSubscriptionsDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for FolderSubscriptionsDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("account-name").build(),
                    glib::ParamSpecString::builder("source-uid").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "account-name" => {
                    *self.account_name.borrow_mut() = value.get().unwrap_or_default()
                }
                "source-uid" => {
                    let uid: String = value.get().unwrap_or_default();
                    *self.source_uid.borrow_mut() = (!uid.trim().is_empty()).then_some(uid);
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "account-name" => self.account_name.borrow().to_value(),
                "source-uid" => self.source_uid.borrow().as_deref().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for FolderSubscriptionsDialog {}
    impl WindowImpl for FolderSubscriptionsDialog {}
    impl AdwWindowImpl for FolderSubscriptionsDialog {}
}

glib::wrapper! {
    pub struct FolderSubscriptionsDialog(ObjectSubclass<imp::FolderSubscriptionsDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl FolderSubscriptionsDialog {
    pub fn new(
        account_name: &str,
        source_uid: Option<&str>,
        available_folders: Vec<SubscribedFolderItem>,
    ) -> Self {
        let initial_subscribed = available_folders
            .iter()
            .filter(|item| item.is_subscribed)
            .map(|item| item.path.clone())
            .collect::<HashSet<_>>();

        let obj: Self = glib::Object::builder()
            .property("title", &format!("Ordner-Abonnements: {account_name}"))
            .property("modal", true)
            .property("default-width", 540)
            .property("default-height", 420)
            .property("account-name", account_name)
            .property("source-uid", source_uid.unwrap_or_default())
            .build();

        *obj.imp().folders.borrow_mut() = available_folders;
        *obj.imp().subscribed_paths.borrow_mut() = initial_subscribed;
        obj.rebuild_folder_list();
        obj
    }

    fn build_ui(&self) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let close_button = gtk::Button::builder()
            .label("Schließen")
            .css_classes(vec!["suggested-action"])
            .build();
        let dialog_weak = self.downgrade();
        close_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.close();
            }
        });
        header.pack_end(&close_button);
        toolbar.add_top_bar(&header);

        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(16)
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Ordner filtern…")
            .build();
        main_box.append(&search_entry);

        let list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list_box)
            .build();
        main_box.append(&scrolled);

        let dialog_for_search = self.downgrade();
        let list_box_for_search = list_box.clone();
        search_entry.connect_search_changed(move |entry| {
            let filter = entry.text().trim().to_lowercase();
            if let Some(dialog) = dialog_for_search.upgrade() {
                dialog.filter_folder_rows(&list_box_for_search, &filter);
            }
        });

        self.imp().list_box.replace(Some(list_box));

        toolbar.set_content(Some(&main_box));
        self.set_content(Some(&toolbar));
    }

    fn rebuild_folder_list(&self) {
        let Some(list_box) = self.imp().list_box.borrow().as_ref().cloned() else {
            return;
        };

        while let Some(child) = list_box.first_child() {
            list_box.remove(&child);
        }

        let folders = self.imp().folders.borrow().clone();
        if folders.is_empty() {
            let empty_row = adw::ActionRow::builder()
                .title("Keine abonnierbaren Ordner gefunden")
                .subtitle("Alle Remote-Ordner sind aktuell synchronisiert oder das Konto ist offline.")
                .build();
            list_box.append(&empty_row);
            return;
        }

        for folder in &folders {
            let row = adw::ActionRow::builder()
                .title(&folder.name)
                .subtitle(&folder.path)
                .build();

            let is_checked = self.imp().subscribed_paths.borrow().contains(&folder.path);
            let check = gtk::CheckButton::builder()
                .active(is_checked)
                .valign(gtk::Align::Center)
                .tooltip_text("Ordner abonnieren")
                .build();

            let path_for_check = folder.path.clone();
            let dialog_weak = self.downgrade();
            check.connect_toggled(move |btn| {
                let active = btn.is_active();
                if let Some(dialog) = dialog_weak.upgrade() {
                    let mut subscribed = dialog.imp().subscribed_paths.borrow_mut();
                    if active {
                        subscribed.insert(path_for_check.clone());
                    } else {
                        subscribed.remove(&path_for_check);
                    }
                }
            });

            row.add_suffix(&check);
            row.set_activatable_widget(Some(&check));
            unsafe {
                row.set_data("folder-name-filter", folder.name.to_lowercase());
                row.set_data("folder-path-filter", folder.path.to_lowercase());
            }
            list_box.append(&row);
        }
    }

    fn filter_folder_rows(&self, list_box: &gtk::ListBox, filter: &str) {
        for index in 0..list_box.observe_children().n_items() {
            let Some(row) = list_box
                .row_at_index(index as i32)
                .and_downcast::<adw::ActionRow>()
            else {
                continue;
            };
            if filter.is_empty() {
                row.set_visible(true);
                continue;
            }
            let name = unsafe {
                row.data::<String>("folder-name-filter")
                    .map(|val| val.as_ref().clone())
                    .unwrap_or_default()
            };
            let path = unsafe {
                row.data::<String>("folder-path-filter")
                    .map(|val| val.as_ref().clone())
                    .unwrap_or_default()
            };
            let visible = name.contains(filter) || path.contains(filter);
            row.set_visible(visible);
        }
    }
}
