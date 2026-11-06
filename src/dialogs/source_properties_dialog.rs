// SPDX-License-Identifier: GPL-3.0-or-later
//
// SourcePropertiesDialog — Unified dialog for viewing and editing Groupware account/source properties.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use crate::i18n::gettext;

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
        const NAME: &'static str = "MeridianSourcePropertiesDialog";
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
        let source_kind = self.imp().source_kind.borrow().clone();
        let color = self.imp().color.borrow().clone();
        let is_writable = *self.imp().is_writable.borrow();
        let is_default = *self.imp().is_default.borrow();

        self.set_title(&format!("Properties of \"{display_name}\""));
        self.set_content_width(460);
        self.set_content_height(380);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();

        let cancel_btn = gtk::Button::builder()
            .label(&gettext("Cancel"))
            .build();
        let dialog_for_cancel = self.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header.pack_start(&cancel_btn);

        let page = adw::PreferencesPage::new();

        // 1. General group
        let general_group = adw::PreferencesGroup::builder().title("General").build();

        let name_row = adw::EntryRow::builder()
            .title("Name")
            .text(&display_name)
            .build();
        general_group.add(&name_row);

        let type_subtitle = if is_writable {
            format!("{source_kind} · Writable (Read/Write access)")
        } else {
            format!("{source_kind} · Read-only")
        };
        let type_row = adw::ActionRow::builder()
            .title("Source Type & Write Access")
            .subtitle(&type_subtitle)
            .build();
        let type_icon = gtk::Image::builder()
            .icon_name("folder-remote-symbolic")
            .valign(gtk::Align::Center)
            .build();
        type_row.add_prefix(&type_icon);
        general_group.add(&type_row);

        page.add(&general_group);

        // 2. Visuals and Behavior
        let visual_group = adw::PreferencesGroup::builder()
            .title("Appearance and Behavior")
            .build();

        let color_row = adw::ActionRow::builder()
            .title("Color")
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
            .title("Select Source Color")
            .build();
        let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
        if let Ok(rgba) = gtk::gdk::RGBA::parse(&color) {
            color_button.set_rgba(&rgba);
        }
        color_button.set_valign(gtk::Align::Center);
        color_button.set_tooltip_text(Some("Customize color"));

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

        let default_row = adw::SwitchRow::builder()
            .title("Use as default source")
            .active(is_default)
            .build();
        let default_icon = gtk::Image::builder()
            .icon_name("starred-symbolic")
            .valign(gtk::Align::Center)
            .build();
        default_row.add_prefix(&default_icon);
        visual_group.add(&default_row);

        page.add(&visual_group);

        // Save button in HeaderBar
        let save_btn = gtk::Button::builder()
            .label("Save")
            .css_classes(vec!["suggested-action"])
            .build();
        let dialog_for_save = self.downgrade();
        let name_row_weak = name_row.downgrade();
        let default_row_weak = default_row.downgrade();
        save_btn.connect_clicked(move |_| {
            let Some(dialog) = dialog_for_save.upgrade() else {
                return;
            };
            let updated_name = name_row_weak
                .upgrade()
                .map(|r| r.text().trim().to_string())
                .unwrap_or_default();
            let is_def = default_row_weak
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
        toolbar.set_content(Some(&page));
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
}
