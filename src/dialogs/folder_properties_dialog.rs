// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for inspecting mail folder properties and metrics in GTK4/libadwaita.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct FolderPropertiesDialog {
        pub folder_name: RefCell<String>,
        pub account_name: RefCell<String>,
        pub folder_path: RefCell<String>,
        pub total_count: RefCell<u32>,
        pub unread_count: RefCell<u32>,
        pub is_remote: RefCell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderPropertiesDialog {
        const NAME: &'static str = "EvolutionFolderPropertiesDialog";
        type Type = super::FolderPropertiesDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for FolderPropertiesDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("folder-name").build(),
                    glib::ParamSpecString::builder("account-name").build(),
                    glib::ParamSpecString::builder("folder-path").build(),
                    glib::ParamSpecUInt::builder("total-count").build(),
                    glib::ParamSpecUInt::builder("unread-count").build(),
                    glib::ParamSpecBoolean::builder("is-remote").build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "folder-name" => {
                    *self.folder_name.borrow_mut() = value.get().unwrap_or_default()
                }
                "account-name" => {
                    *self.account_name.borrow_mut() = value.get().unwrap_or_default()
                }
                "folder-path" => {
                    *self.folder_path.borrow_mut() = value.get().unwrap_or_default()
                }
                "total-count" => {
                    *self.total_count.borrow_mut() = value.get().unwrap_or_default()
                }
                "unread-count" => {
                    *self.unread_count.borrow_mut() = value.get().unwrap_or_default()
                }
                "is-remote" => {
                    *self.is_remote.borrow_mut() = value.get().unwrap_or_default()
                }
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "folder-name" => self.folder_name.borrow().to_value(),
                "account-name" => self.account_name.borrow().to_value(),
                "folder-path" => self.folder_path.borrow().to_value(),
                "total-count" => self.total_count.borrow().to_value(),
                "unread-count" => self.unread_count.borrow().to_value(),
                "is-remote" => self.is_remote.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for FolderPropertiesDialog {}
    impl WindowImpl for FolderPropertiesDialog {}
    impl AdwWindowImpl for FolderPropertiesDialog {}
}

glib::wrapper! {
    pub struct FolderPropertiesDialog(ObjectSubclass<imp::FolderPropertiesDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl FolderPropertiesDialog {
    pub fn new(
        folder_name: &str,
        account_name: &str,
        folder_path: &str,
        total_count: u32,
        unread_count: u32,
        is_remote: bool,
    ) -> Self {
        glib::Object::builder()
            .property("title", &format!("Eigenschaften: {folder_name}"))
            .property("modal", true)
            .property("default-width", 480)
            .property("default-height", 340)
            .property("folder-name", folder_name)
            .property("account-name", account_name)
            .property("folder-path", folder_path)
            .property("total-count", total_count)
            .property("unread-count", unread_count)
            .property("is-remote", is_remote)
            .build()
    }

    fn build_ui(&self) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(24)
            .margin_end(24)
            .margin_top(16)
            .margin_bottom(24)
            .build();

        let general_group = adw::PreferencesGroup::builder()
            .title("Allgemeine Informationen")
            .build();

        let folder_name_row = adw::ActionRow::builder()
            .title("Ordnername")
            .subtitle(self.imp().folder_name.borrow().as_str())
            .build();
        general_group.add(&folder_name_row);

        let account_row = adw::ActionRow::builder()
            .title("Konto / Quelle")
            .subtitle(self.imp().account_name.borrow().as_str())
            .build();
        general_group.add(&account_row);

        let type_label = if *self.imp().is_remote.borrow() {
            "Synchronisierter Remote-Ordner (IMAP/Exchange)"
        } else {
            "Lokaler app-eigener Ordner"
        };
        let type_row = adw::ActionRow::builder()
            .title("Ordnertyp")
            .subtitle(type_label)
            .build();
        general_group.add(&type_row);

        let path_row = adw::ActionRow::builder()
            .title("Speicherpfad / URI")
            .subtitle(self.imp().folder_path.borrow().as_str())
            .build();

        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Pfad in Zwischenablage kopieren")
            .css_classes(vec!["flat", "circular"])
            .build();
        let path_text = self.imp().folder_path.borrow().clone();
        let dialog_weak = self.downgrade();
        copy_button.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&path_text);
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.show_toast("Ordnerpfad in Zwischenablage kopiert");
                }
            }
        });
        path_row.add_suffix(&copy_button);
        general_group.add(&path_row);

        let metrics_group = adw::PreferencesGroup::builder()
            .title("Nachrichtenbestand")
            .build();

        let total_row = adw::ActionRow::builder()
            .title("Gesamte Nachrichten")
            .subtitle(&format!("{} Nachrichten", self.imp().total_count.borrow()))
            .build();
        metrics_group.add(&total_row);

        let unread_row = adw::ActionRow::builder()
            .title("Ungelesene Nachrichten")
            .subtitle(&format!("{} ungelesen", self.imp().unread_count.borrow()))
            .build();
        metrics_group.add(&unread_row);

        content.append(&general_group);
        content.append(&metrics_group);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build();

        toolbar.set_content(Some(&scrolled));
        self.set_content(Some(&toolbar));
    }

    fn show_toast(&self, message: &str) {
        if let Some(parent) = self.transient_for() {
            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::info!(%message, "FolderPropertiesDialog toast");
    }
}
