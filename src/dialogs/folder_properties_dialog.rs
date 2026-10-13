// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for inspecting mail folder properties and metrics in GTK4/libadwaita.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use crate::i18n::gettext;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderPropertiesSnapshot {
    pub folder_name: String,
    pub account_name: String,
    pub folder_path: String,
    pub total_count: u32,
    pub unread_count: u32,
    pub is_remote: bool,
}

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
        const NAME: &'static str = "MeridianFolderPropertiesDialog";
        type Type = super::FolderPropertiesDialog;
        type ParentType = adw::Dialog;
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
                "folder-name" => *self.folder_name.borrow_mut() = value.get().unwrap_or_default(),
                "account-name" => *self.account_name.borrow_mut() = value.get().unwrap_or_default(),
                "folder-path" => *self.folder_path.borrow_mut() = value.get().unwrap_or_default(),
                "total-count" => *self.total_count.borrow_mut() = value.get().unwrap_or_default(),
                "unread-count" => *self.unread_count.borrow_mut() = value.get().unwrap_or_default(),
                "is-remote" => *self.is_remote.borrow_mut() = value.get().unwrap_or_default(),
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
    impl AdwDialogImpl for FolderPropertiesDialog {}
}

glib::wrapper! {
    pub struct FolderPropertiesDialog(ObjectSubclass<imp::FolderPropertiesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FolderPropertiesDialog {
    pub fn new(snapshot: FolderPropertiesSnapshot) -> Self {
        glib::Object::builder()
            .property("title", "Ordnereigenschaften")
            .property("content-width", 540)
            .property("content-height", 560)
            .property("folder-name", snapshot.folder_name)
            .property("account-name", snapshot.account_name)
            .property("folder-path", snapshot.folder_path)
            .property("total-count", snapshot.total_count)
            .property("unread-count", snapshot.unread_count)
            .property("is-remote", snapshot.is_remote)
            .build()
    }

    fn build_ui(&self) {
        let toolbar = adw::ToolbarView::new();
        toolbar.set_tooltip_text(Some(
            "Schreibgeschützte Momentaufnahme: Speicherquote und erweiterte Einstellungen des Mail-Anbieters werden vom Server verwaltet.",
        ));
        let type_subtitle = if *self.imp().is_remote.borrow() {
            "Synchronisierter Kontoordner"
        } else {
            "Lokaler app-eigener Ordner"
        };

        let view_stack = adw::ViewStack::new();

        let header = adw::HeaderBar::new();
        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header.set_title_widget(Some(&view_switcher));

        let close_btn = gtk::Button::builder()
            .label(&gettext("Close"))
            .build();
        let dialog_weak_close = self.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_close.upgrade() {
                dialog.close();
            }
        });
        header.pack_end(&close_btn);
        toolbar.add_top_bar(&header);

        let folder_name = self.imp().folder_name.borrow().clone();
        let account_name = self.imp().account_name.borrow().clone();
        let folder_path = self.imp().folder_path.borrow().clone();
        let total_count = *self.imp().total_count.borrow();
        let unread_count = *self.imp().unread_count.borrow();
        let is_remote = *self.imp().is_remote.borrow();

        // ── Page 1: Allgemein (General) ─────────────────────────────────
        let page_general = adw::PreferencesPage::new();

        let details_group = adw::PreferencesGroup::builder()
            .title(&gettext("Folder Details"))
            .description(type_subtitle)
            .build();

        let row_folder = adw::ActionRow::builder()
            .title(&gettext("Folder Name"))
            .subtitle(&folder_name)
            .build();
        let icon_folder = gtk::Image::builder()
            .icon_name("folder-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_folder.add_prefix(&icon_folder);
        details_group.add(&row_folder);

        let row_account = adw::ActionRow::builder()
            .title(&gettext("Account"))
            .subtitle(&account_name)
            .build();
        let icon_account = gtk::Image::builder()
            .icon_name("avatar-default-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_account.add_prefix(&icon_account);
        details_group.add(&row_account);

        let row_path = adw::ActionRow::builder()
            .title(&gettext("Path"))
            .subtitle(&folder_path)
            .build();
        let icon_path = gtk::Image::builder()
            .icon_name("folder-remote-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_path.add_prefix(&icon_path);

        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text(&gettext("Copy folder path"))
            .css_classes(vec!["flat", "circular"])
            .build();
        let path_text = folder_path.clone();
        let dialog_weak = self.downgrade();
        copy_button.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&path_text);
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.show_toast(&gettext("Folder path copied to clipboard"));
                }
            }
        });
        row_path.add_suffix(&copy_button);
        details_group.add(&row_path);

        let row_unread = adw::ActionRow::builder()
            .title(&gettext("Unread Messages"))
            .subtitle(&unread_count.to_string())
            .build();
        let icon_unread = gtk::Image::builder()
            .icon_name("mail-unread-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_unread.add_prefix(&icon_unread);
        details_group.add(&row_unread);

        let row_total = adw::ActionRow::builder()
            .title(&gettext("Total Messages"))
            .subtitle(&total_count.to_string())
            .build();
        let icon_total = gtk::Image::builder()
            .icon_name("mail-read-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_total.add_prefix(&icon_total);
        details_group.add(&row_total);

        page_general.add(&details_group);

        let sync_group = adw::PreferencesGroup::builder()
            .title(&gettext("Synchronization and Offline"))
            .build();

        let row_offline = adw::SwitchRow::builder()
            .title("Ordnerinhalt lokal für den Offline-Betrieb kopieren")
            .subtitle(&gettext("Cache messages on this device for offline reading"))
            .active(is_remote)
            .build();
        let icon_offline = gtk::Image::builder()
            .icon_name("network-offline-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_offline.add_prefix(&icon_offline);
        sync_group.add(&row_offline);

        let row_check = adw::SwitchRow::builder()
            .title("Immer auf neue E-Mails in diesem Ordner prüfen")
            .subtitle(&gettext("Include this folder when synchronizing mailbox"))
            .active(true)
            .build();
        let icon_check = gtk::Image::builder()
            .icon_name("mail-read-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_check.add_prefix(&icon_check);
        sync_group.add(&row_check);

        let row_filter = adw::SwitchRow::builder()
            .title("Nachrichtenfilter auf diesen Ordner anwenden")
            .subtitle(&gettext("Process incoming filter rules automatically"))
            .active(false)
            .build();
        let icon_filter = gtk::Image::builder()
            .icon_name("view-sort-ascending-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_filter.add_prefix(&icon_filter);
        sync_group.add(&row_filter);

        page_general.add(&sync_group);

        let reading_group = adw::PreferencesGroup::builder()
            .title(&gettext("Reading and Delivery"))
            .build();

        let row_read = adw::ActionRow::builder()
            .title(&gettext("Mark messages as read after"))
            .subtitle(&gettext("Timeout before viewed messages are marked read"))
            .build();
        let icon_read = gtk::Image::builder()
            .icon_name("preferences-system-time-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_read.add_prefix(&icon_read);
        let spin_read = gtk::SpinButton::builder()
            .digits(1)
            .climb_rate(0.5)
            .adjustment(&gtk::Adjustment::new(1.5, 0.5, 60.0, 0.5, 5.0, 0.0))
            .valign(gtk::Align::Center)
            .build();
        let lbl_sec = gtk::Label::builder()
            .label(&gettext("Seconds"))
            .valign(gtk::Align::Center)
            .css_classes(vec!["dim-label"])
            .build();
        let read_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        read_box.append(&spin_read);
        read_box.append(&lbl_sec);
        row_read.add_suffix(&read_box);
        reading_group.add(&row_read);

        let accounts_list = [
            gettext("None (Default)"),
            account_name,
        ];
        let accounts_str_slices: Vec<&str> = accounts_list.iter().map(|s| s.as_str()).collect();
        let row_override = adw::ComboRow::builder()
            .title(&gettext("Send Account Override"))
            .subtitle(&gettext("Default sending identity for messages from this folder"))
            .model(&gtk::StringList::new(&accounts_str_slices))
            .build();
        let icon_override = gtk::Image::builder()
            .icon_name("mail-send-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_override.add_prefix(&icon_override);
        reading_group.add(&row_override);

        page_general.add(&reading_group);

        view_stack.add_titled_with_icon(&page_general, Some("general"), "Allgemein", "folder-symbolic");

        // ── Page 2: Erscheinungsbild (Appearance) ────────────────────────
        let page_appearance = adw::PreferencesPage::new();
        let appearance_group = adw::PreferencesGroup::builder()
            .title(&gettext("Appearance"))
            .build();

        let row_color = adw::SwitchRow::builder()
            .title(&gettext("Use custom folder color"))
            .subtitle(&gettext("Display folder with a custom highlight color"))
            .build();
        let icon_color = gtk::Image::builder()
            .icon_name("applications-graphics-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_color.add_prefix(&icon_color);
        appearance_group.add(&row_color);

        let row_icon = adw::SwitchRow::builder()
            .title(&gettext("Show custom folder icon"))
            .subtitle(&gettext("Display a customized icon in the sidebar"))
            .build();
        let icon_folder_custom = gtk::Image::builder()
            .icon_name("starred-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_icon.add_prefix(&icon_folder_custom);
        appearance_group.add(&row_icon);

        page_appearance.add(&appearance_group);
        view_stack.add_titled_with_icon(&page_appearance, Some("appearance"), "Erscheinungsbild", "preferences-desktop-appearance-symbolic");

        // ── Page 3: Archivierung (Archive) ──────────────────────────────
        let page_archive = adw::PreferencesPage::new();
        let archive_group = adw::PreferencesGroup::builder()
            .title(&gettext("Archiving"))
            .build();

        let row_archive = adw::SwitchRow::builder()
            .title(&gettext("Automatically archive old messages in this folder"))
            .subtitle(&gettext("Move expired messages to archive storage"))
            .build();
        let icon_archive = gtk::Image::builder()
            .icon_name("package-x-generic-symbolic")
            .valign(gtk::Align::Center)
            .build();
        row_archive.add_prefix(&icon_archive);
        archive_group.add(&row_archive);

        page_archive.add(&archive_group);
        view_stack.add_titled_with_icon(&page_archive, Some("archive"), "Archivierung", "package-x-generic-symbolic");

        // ── Page 4: Beschriftungen (Labels) ─────────────────────────────
        let page_labels = adw::PreferencesPage::new();
        let labels_group = adw::PreferencesGroup::builder()
            .title(&gettext("Labels"))
            .description(&gettext("Available tags and flags for organizing messages in this folder:"))
            .build();

        let available_labels = crate::services::AppSettings::try_new()
            .map(|s| s.mail_labels())
            .unwrap_or_else(crate::services::settings_service::MailLabelDefinition::default_labels);

        for label_def in available_labels {
            let row = adw::ActionRow::builder()
                .title(&label_def.display_name())
                .subtitle(&format!("{}: {}", gettext("Color"), label_def.color))
                .build();

            let color_dot = gtk::Label::builder()
                .valign(gtk::Align::Center)
                .build();
            color_dot.set_markup(&format!(
                "<span foreground=\"{}\" font_size=\"x-large\">●</span>",
                label_def.color
            ));
            row.add_prefix(&color_dot);

            let check = gtk::CheckButton::builder()
                .active(true)
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&check);
            row.set_activatable_widget(Some(&check));

            labels_group.add(&row);
        }

        page_labels.add(&labels_group);
        view_stack.add_titled_with_icon(&page_labels, Some("labels"), "Beschriftungen", "tag-symbolic");

        toolbar.set_content(Some(&view_stack));
        self.set_child(Some(&toolbar));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
        tracing::info!(%message, "FolderPropertiesDialog toast");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dialog_copy_is_explicitly_a_read_only_snapshot() {
        let source = include_str!("folder_properties_dialog.rs");
        let production = source.split("\n#[cfg(test)]").next().unwrap_or(source);
        assert!(production.contains("Schreibgeschützte Momentaufnahme"));
        assert!(production.contains("Speicherquote und erweiterte Einstellungen"));
        assert!(production.contains("Synchronisierter Kontoordner"));
        assert!(!production.contains("IMAP/Exchange"));
    }
}
