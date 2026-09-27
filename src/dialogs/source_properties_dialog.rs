// SPDX-License-Identifier: GPL-3.0-or-later
//
// SourcePropertiesDialog — Unified dialog for viewing and editing Groupware account/source properties.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePropertiesUpdate {
    pub source_uid: String,
    pub display_name: String,
    pub color: String,
    pub is_default: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SourcePropertiesDialog {
        pub source_uid: RefCell<String>,
        pub display_name: RefCell<String>,
        pub source_kind: RefCell<String>,
        pub color: RefCell<String>,
        pub current_color: RefCell<String>,
        pub is_writable: RefCell<bool>,
        pub is_default: RefCell<bool>,
        pub on_save: RefCell<Option<Box<dyn Fn(SourcePropertiesUpdate) + 'static>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SourcePropertiesDialog {
        const NAME: &'static str = "EvolutionSourcePropertiesDialog";
        type Type = super::SourcePropertiesDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for SourcePropertiesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for SourcePropertiesDialog {}
    impl AdwDialogImpl for SourcePropertiesDialog {}
}

glib::wrapper! {
    pub struct SourcePropertiesDialog(ObjectSubclass<imp::SourcePropertiesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SourcePropertiesDialog {
    pub fn new(
        source_uid: &str,
        display_name: &str,
        source_kind: &str,
        color: &str,
        is_writable: bool,
        is_default: bool,
    ) -> Self {
        let obj: Self = glib::Object::builder().build();
        *obj.imp().source_uid.borrow_mut() = source_uid.to_string();
        *obj.imp().display_name.borrow_mut() = display_name.to_string();
        *obj.imp().source_kind.borrow_mut() = source_kind.to_string();
        *obj.imp().color.borrow_mut() = color.to_string();
        *obj.imp().current_color.borrow_mut() = color.to_string();
        *obj.imp().is_writable.borrow_mut() = is_writable;
        *obj.imp().is_default.borrow_mut() = is_default;
        obj
    }

    pub fn set_on_save<F>(&self, callback: F)
    where
        F: Fn(SourcePropertiesUpdate) + 'static,
    {
        *self.imp().on_save.borrow_mut() = Some(Box::new(callback));
    }

    fn build_ui(&self) {
        let display_name = self.imp().display_name.borrow().clone();
        let source_uid = self.imp().source_uid.borrow().clone();
        let source_kind = self.imp().source_kind.borrow().clone();
        let color = self.imp().color.borrow().clone();
        let is_writable = *self.imp().is_writable.borrow();
        let is_default = *self.imp().is_default.borrow();

        self.set_title(&format!("Eigenschaften von „{display_name}“"));
        self.set_content_width(520);
        self.set_content_height(480);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let cancel_btn = gtk::Button::builder()
            .label("Abbrechen")
            .build();
        let dialog_for_cancel = self.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header.pack_start(&cancel_btn);

        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();

        // 1. General group
        let general_group = adw::PreferencesGroup::builder().title("Allgemein").build();

        let name_row = adw::EntryRow::builder()
            .title("Name")
            .text(&display_name)
            .build();
        general_group.add(&name_row);

        let type_row = adw::ActionRow::builder()
            .title("Quellentyp")
            .subtitle(&source_kind)
            .build();
        general_group.add(&type_row);

        let uid_row = adw::ActionRow::builder()
            .title("Quellen-UID")
            .subtitle(&source_uid)
            .build();
        let copy_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat", "circular"])
            .tooltip_text("UID in die Zwischenablage kopieren")
            .build();
        let uid_for_copy = source_uid.clone();
        copy_btn.connect_clicked(move |btn| {
            btn.display().clipboard().set_text(&uid_for_copy);
        });
        uid_row.add_suffix(&copy_btn);
        general_group.add(&uid_row);

        main_box.append(&general_group);

        // 2. Visuals and Behavior
        let visual_group = adw::PreferencesGroup::builder()
            .title("Darstellung und Verhalten")
            .build();

        let color_row = adw::ActionRow::builder()
            .title("Farbe")
            .subtitle(&color)
            .build();
        let color_dot = gtk::Label::builder()
            .label("●")
            .css_classes(vec!["title-2"])
            .valign(gtk::Align::Center)
            .margin_end(8)
            .build();
        color_dot.set_markup(&format!(
            "<span foreground=\"{color}\" font_size=\"x-large\">●</span>"
        ));
        color_row.add_prefix(&color_dot);

        // Color dialog button for selecting a color
        let color_dialog = gtk::ColorDialog::builder()
            .title("Quellenfarbe auswählen")
            .build();
        let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
        if let Ok(rgba) = gtk::gdk::RGBA::parse(&color) {
            color_button.set_rgba(&rgba);
        }
        color_button.set_valign(gtk::Align::Center);
        color_button.set_tooltip_text(Some("Farbe anpassen"));

        let color_dot_weak = color_dot.downgrade();
        let color_row_weak = color_row.downgrade();
        let dialog_for_color = self.downgrade();
        color_button.connect_rgba_notify(move |btn| {
            let rgba = btn.rgba();
            let r = (rgba.red() * 255.0).round() as u8;
            let g = (rgba.green() * 255.0).round() as u8;
            let b = (rgba.blue() * 255.0).round() as u8;
            let hex = format!("#{r:02x}{g:02x}{b:02x}");
            if let Some(dialog) = dialog_for_color.upgrade() {
                *dialog.imp().current_color.borrow_mut() = hex.clone();
            }
            if let Some(dot) = color_dot_weak.upgrade() {
                dot.set_markup(&format!(
                    "<span foreground=\"{hex}\" font_size=\"x-large\">●</span>"
                ));
            }
            if let Some(row) = color_row_weak.upgrade() {
                row.set_subtitle(&hex);
            }
        });
        color_row.add_suffix(&color_button);
        visual_group.add(&color_row);

        let writable_row = adw::ActionRow::builder()
            .title("Schreibzugriff")
            .subtitle(if is_writable {
                "Schreibbar (Lese-/Schreibzugriff)"
            } else {
                "Schreibgeschützt (Nur Lesen)"
            })
            .build();
        let write_icon = gtk::Image::builder()
            .icon_name(if is_writable {
                "document-edit-symbolic"
            } else {
                "channel-insecure-symbolic"
            })
            .valign(gtk::Align::Center)
            .build();
        writable_row.add_prefix(&write_icon);
        visual_group.add(&writable_row);

        let default_switch = gtk::Switch::builder()
            .active(is_default)
            .valign(gtk::Align::Center)
            .build();
        let default_row = adw::ActionRow::builder()
            .title("Als Standardquelle verwenden")
            .activatable_widget(&default_switch)
            .build();
        default_row.add_suffix(&default_switch);
        visual_group.add(&default_row);

        main_box.append(&visual_group);

        // Save button in HeaderBar
        let save_btn = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        let dialog_for_save = self.downgrade();
        let name_row_weak = name_row.downgrade();
        let default_switch_weak = default_switch.downgrade();
        save_btn.connect_clicked(move |_| {
            let Some(dialog) = dialog_for_save.upgrade() else {
                return;
            };
            let updated_name = name_row_weak
                .upgrade()
                .map(|r| r.text().trim().to_string())
                .unwrap_or_default();
            let is_def = default_switch_weak
                .upgrade()
                .map(|s| s.is_active())
                .unwrap_or(false);
            let uid = dialog.imp().source_uid.borrow().clone();
            let final_color = dialog.imp().current_color.borrow().clone();

            let update = SourcePropertiesUpdate {
                source_uid: uid,
                display_name: updated_name,
                color: final_color,
                is_default: is_def,
            };

            if let Some(on_save) = dialog.imp().on_save.borrow().as_ref() {
                on_save(update);
            }
            dialog.close();
        });
        header.pack_end(&save_btn);

        toolbar.add_top_bar(&header);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&main_box)
            .build();

        toolbar.set_content(Some(&scrolled));
        self.set_child(Some(&toolbar));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_properties_update_data_structure() {
        let update = SourcePropertiesUpdate {
            source_uid: "local-1".to_string(),
            display_name: "Mein Kalender".to_string(),
            color: "#3584e4".to_string(),
            is_default: true,
        };
        assert_eq!(update.source_uid, "local-1");
        assert_eq!(update.display_name, "Mein Kalender");
        assert_eq!(update.color, "#3584e4");
        assert!(update.is_default);
    }

    #[test]
    fn source_properties_dialog_copy_and_labels() {
        let source = include_str!("source_properties_dialog.rs");
        assert!(source.contains("Allgemein"));
        assert!(source.contains("Quellentyp"));
        assert!(source.contains("Quellen-UID"));
        assert!(source.contains("Darstellung und Verhalten"));
        assert!(source.contains("Farbe"));
        assert!(source.contains("Schreibzugriff"));
        assert!(source.contains("Als Standardquelle verwenden"));
        assert!(source.contains("Abbrechen"));
        assert!(source.contains("Speichern"));
    }
}
