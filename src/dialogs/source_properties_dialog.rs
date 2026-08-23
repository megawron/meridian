// SPDX-License-Identifier: GPL-3.0-or-later
//
// SourcePropertiesDialog — Unified dialog for viewing and editing Groupware account/source properties.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct SourcePropertiesDialog {
        pub source_uid: RefCell<String>,
        pub display_name: RefCell<String>,
        pub source_kind: RefCell<String>,
        pub color: RefCell<String>,
        pub is_writable: RefCell<bool>,
        pub is_default: RefCell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SourcePropertiesDialog {
        const NAME: &'static str = "EvolutionSourcePropertiesDialog";
        type Type = super::SourcePropertiesDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for SourcePropertiesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for SourcePropertiesDialog {}
    impl WindowImpl for SourcePropertiesDialog {}
    impl AdwWindowImpl for SourcePropertiesDialog {}
}

glib::wrapper! {
    pub struct SourcePropertiesDialog(ObjectSubclass<imp::SourcePropertiesDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
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
        *obj.imp().is_writable.borrow_mut() = is_writable;
        *obj.imp().is_default.borrow_mut() = is_default;
        obj
    }

    fn build_ui(&self) {
        let display_name = self.imp().display_name.borrow().clone();
        let source_uid = self.imp().source_uid.borrow().clone();
        let source_kind = self.imp().source_kind.borrow().clone();
        let color = self.imp().color.borrow().clone();
        let is_writable = *self.imp().is_writable.borrow();
        let is_default = *self.imp().is_default.borrow();

        self.set_title(Some(&format!("Eigenschaften von „{display_name}“")));
        self.set_default_size(500, 420);
        self.set_modal(true);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();

        // 1. General group
        let general_group = adw::PreferencesGroup::builder()
            .title("Allgemein")
            .build();

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
        color_dot.set_markup(&format!("<span foreground=\"{color}\" font_size=\"x-large\">●</span>"));
        color_row.add_prefix(&color_dot);
        visual_group.add(&color_row);

        let writable_row = adw::ActionRow::builder()
            .title("Schreibzugriff")
            .subtitle(if is_writable { "Schreibbar (Lese-/Schreibzugriff)" } else { "Schreibgeschützt (Nur Lesen)" })
            .build();
        let write_icon = gtk::Image::builder()
            .icon_name(if is_writable { "document-edit-symbolic" } else { "channel-insecure-symbolic" })
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

        // 3. Action buttons
        let button_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::End)
            .margin_top(8)
            .build();

        let close_btn = gtk::Button::builder()
            .label("Schließen")
            .css_classes(vec!["suggested-action"])
            .build();
        let dialog_for_close = self.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_close.upgrade() {
                dialog.close();
            }
        });
        button_box.append(&close_btn);
        main_box.append(&button_box);

        toolbar.set_content(Some(&main_box));
        self.set_content(Some(&toolbar));
    }
}
