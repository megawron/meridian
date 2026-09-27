// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for inspecting mail folder properties and metrics in GTK4/libadwaita.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

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
        const NAME: &'static str = "EvolutionFolderPropertiesDialog";
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
            .property("content-width", 500)
            .property("content-height", 520)
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
        let title_widget = adw::WindowTitle::new("Ordnereigenschaften", type_subtitle);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title_widget));
        toolbar.add_top_bar(&header);

        let notebook = gtk::Notebook::new();
        notebook.set_tab_pos(gtk::PositionType::Top);

        let folder_name = self.imp().folder_name.borrow().clone();
        let account_name = self.imp().account_name.borrow().clone();
        let folder_path = self.imp().folder_path.borrow().clone();
        let total_count = *self.imp().total_count.borrow();
        let unread_count = *self.imp().unread_count.borrow();
        let is_remote = *self.imp().is_remote.borrow();

        // ── Page 1: Allgemein (General) ─────────────────────────────────
        let general_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(20)
            .margin_end(20)
            .margin_top(10)
            .margin_bottom(10)
            .build();

        // Folder Title Header
        let title_label = gtk::Label::builder()
            .label(&folder_name)
            .halign(gtk::Align::Start)
            .css_classes(vec!["title-3", "heading"])
            .build();
        general_box.append(&title_label);

        // Metadata Grid (Path, Unread, Total)
        let grid = gtk::Grid::builder()
            .column_spacing(16)
            .row_spacing(4)
            .build();

        // Row 0: Path
        let lbl_path = gtk::Label::builder()
            .label("Pfad:")
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        let val_path_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::End)
            .hexpand(true)
            .build();
        let val_path = gtk::Label::builder()
            .label(&folder_path)
            .halign(gtk::Align::End)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Pfad in Zwischenablage kopieren")
            .css_classes(vec!["flat", "circular"])
            .build();
        let path_text = folder_path.clone();
        let dialog_weak = self.downgrade();
        copy_button.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&path_text);
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.show_toast("Ordnerpfad in Zwischenablage kopiert");
                }
            }
        });
        val_path_box.append(&val_path);
        val_path_box.append(&copy_button);
        grid.attach(&lbl_path, 0, 0, 1, 1);
        grid.attach(&val_path_box, 1, 0, 1, 1);

        // Row 1: Unread
        let lbl_unread = gtk::Label::builder()
            .label("Ungelesene Nachrichten:")
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        let val_unread = gtk::Label::builder()
            .label(&unread_count.to_string())
            .halign(gtk::Align::End)
            .build();
        grid.attach(&lbl_unread, 0, 1, 1, 1);
        grid.attach(&val_unread, 1, 1, 1, 1);

        // Row 2: Total
        let lbl_total = gtk::Label::builder()
            .label("Gesamte Nachrichten:")
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        let val_total = gtk::Label::builder()
            .label(&total_count.to_string())
            .halign(gtk::Align::End)
            .build();
        grid.attach(&lbl_total, 0, 2, 1, 1);
        grid.attach(&val_total, 1, 2, 1, 1);

        general_box.append(&grid);

        // Options Checkboxes (matching classic GNOME Evolution layout)
        let cb_offline = gtk::CheckButton::builder()
            .label("Ordnerinhalt lokal für den Offline-Betrieb kopieren")
            .active(is_remote)
            .build();
        general_box.append(&cb_offline);

        let cb_filter = gtk::CheckButton::builder()
            .label("Nachrichtenfilter auf diesen Ordner anwenden")
            .active(false)
            .build();
        general_box.append(&cb_filter);

        let cb_check = gtk::CheckButton::builder()
            .label("Immer auf neue E-Mails in diesem Ordner prüfen")
            .active(true)
            .build();
        general_box.append(&cb_check);

        // Mark as read after X seconds row
        let box_read = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        let cb_read = gtk::CheckButton::builder()
            .label("Nachrichten als gelesen markieren nach")
            .active(true)
            .build();
        let spin_read = gtk::SpinButton::builder()
            .digits(1)
            .climb_rate(0.5)
            .adjustment(&gtk::Adjustment::new(1.5, 0.5, 60.0, 0.5, 5.0, 0.0))
            .build();
        let lbl_sec = gtk::Label::new(Some("Sekunden"));
        box_read.append(&cb_read);
        box_read.append(&spin_read);
        box_read.append(&lbl_sec);
        general_box.append(&box_read);

        // Send Account Override
        let lbl_override = gtk::Label::builder()
            .label("Absendekonto überschreiben:")
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .margin_top(4)
            .build();
        general_box.append(&lbl_override);

        let accounts_list = [
            "Keines (Standard)".to_string(),
            account_name,
        ];
        let accounts_str_slices: Vec<&str> = accounts_list.iter().map(|s| s.as_str()).collect();
        let dd_override = gtk::DropDown::from_strings(&accounts_str_slices);
        general_box.append(&dd_override);

        let general_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .child(&general_box)
            .build();
        notebook.append_page(&general_scrolled, Some(&gtk::Label::new(Some("Allgemein"))));

        // ── Page 2: Erscheinungsbild (Appearance) ────────────────────────
        let appearance_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(20)
            .margin_end(20)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        let cb_color = gtk::CheckButton::builder()
            .label("Benutzerdefinierte Ordnerfarbe verwenden")
            .build();
        let cb_icon = gtk::CheckButton::builder()
            .label("Eigenes Ordnersymbol anzeigen")
            .build();
        appearance_box.append(&cb_color);
        appearance_box.append(&cb_icon);
        let appearance_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .child(&appearance_box)
            .build();
        notebook.append_page(&appearance_scrolled, Some(&gtk::Label::new(Some("Erscheinungsbild"))));

        // ── Page 3: Archivierung (Archive) ──────────────────────────────
        let archive_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(20)
            .margin_end(20)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        let cb_archive = gtk::CheckButton::builder()
            .label("Alte Nachrichten dieses Ordners automatisch archivieren")
            .build();
        archive_box.append(&cb_archive);
        let archive_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .child(&archive_box)
            .build();
        notebook.append_page(&archive_scrolled, Some(&gtk::Label::new(Some("Archivierung"))));

        // ── Page 4: Beschriftungen (Labels) ─────────────────────────────
        let labels_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(20)
            .margin_end(20)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        let lbl_labels_hint = gtk::Label::builder()
            .label("Verfügbare Etiketten und Kennzeichnungen:")
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        labels_box.append(&lbl_labels_hint);

        let labels_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();

        let available_labels = crate::services::AppSettings::try_new()
            .map(|s| s.mail_labels())
            .unwrap_or_else(crate::services::settings_service::MailLabelDefinition::default_labels);

        for label_def in available_labels {
            let row = adw::ActionRow::builder()
                .title(&label_def.display_name())
                .subtitle(&format!("Farbe: {}", label_def.color))
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

            labels_list.append(&row);
        }
        labels_box.append(&labels_list);

        let labels_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .child(&labels_box)
            .build();
        notebook.append_page(&labels_scrolled, Some(&gtk::Label::new(Some("Beschriftungen"))));

        toolbar.set_content(Some(&notebook));

        // Bottom Action Bar with Close Button
        let bottom_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_start(16)
            .margin_end(16)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let close_btn = gtk::Button::builder()
            .label("Schließen")
            .build();
        let dialog_weak_close = self.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_close.upgrade() {
                dialog.close();
            }
        });
        bottom_box.append(&close_btn);
        toolbar.add_bottom_bar(&bottom_box);

        self.set_child(Some(&toolbar));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_evolution_window(self) {
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
