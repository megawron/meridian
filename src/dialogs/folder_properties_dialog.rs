// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialog for inspecting and customizing mail folder properties in GTK4/libadwaita.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

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

impl FolderPropertiesSnapshot {
    pub fn customization_key(&self) -> String {
        FolderCustomizationStore::key_for_path(self.is_remote, &self.folder_path, &self.folder_name)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FolderCustomization {
    #[serde(default)]
    pub icon_name: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default = "default_true")]
    pub include_in_unified: bool,
    #[serde(default)]
    pub retention_days: u32,
    #[serde(default = "default_true")]
    pub check_new_mail: bool,
    #[serde(default)]
    pub apply_filters: bool,
}

fn default_true() -> bool {
    true
}

impl Default for FolderCustomization {
    fn default() -> Self {
        Self {
            icon_name: None,
            accent_color: None,
            include_in_unified: true,
            retention_days: 0,
            check_new_mail: true,
            apply_filters: false,
        }
    }
}

pub const FOLDER_ICON_CHOICES: &[(&str, &str)] = &[
    ("folder-symbolic", "folder-symbolic"),
    ("mail-inbox-symbolic", "mail-inbox-symbolic"),
    ("mail-send-symbolic", "mail-send-symbolic"),
    ("mail-outbox-symbolic", "mail-outbox-symbolic"),
    ("mail-drafts-symbolic", "mail-drafts-symbolic"),
    ("mail-mark-junk-symbolic", "mail-mark-junk-symbolic"),
    ("user-trash-symbolic", "user-trash-symbolic"),
    ("mail-unread-symbolic", "mail-unread-symbolic"),
    ("starred-symbolic", "starred-symbolic"),
    ("emblem-important-symbolic", "emblem-important-symbolic"),
    ("package-x-generic-symbolic", "package-x-generic-symbolic"),
    ("emblem-favorite-symbolic", "emblem-favorite-symbolic"),
    ("security-high-symbolic", "security-high-symbolic"),
    ("system-users-symbolic", "system-users-symbolic"),
];

pub const FOLDER_COLOR_CHOICES: &[(&str, &str)] = &[
    ("", ""),
    ("blue", "#3584e4"),
    ("teal", "#2190a4"),
    ("green", "#3a944a"),
    ("yellow", "#c88800"),
    ("orange", "#ed5b00"),
    ("red", "#e62d42"),
    ("pink", "#d56199"),
    ("purple", "#9141ac"),
    ("slate", "#6f8396"),
];

pub struct FolderCustomizationStore {
    path: PathBuf,
}

impl Default for FolderCustomizationStore {
    fn default() -> Self {
        Self {
            path: crate::services::app_data_dir().join("folder-customizations.json"),
        }
    }
}

impl FolderCustomizationStore {
    pub fn key_for_path(is_remote: bool, folder_path: &str, folder_name: &str) -> String {
        let raw = if !folder_path.trim().is_empty() {
            folder_path
                .split_once(" / ")
                .map(|(_, rest)| rest.trim())
                .unwrap_or(folder_path.trim())
        } else {
            folder_name.trim()
        };
        if is_remote {
            format!("remote:{}", folder_path.trim().to_ascii_lowercase())
        } else {
            format!("local:{}", raw.to_ascii_lowercase())
        }
    }

    pub fn load_all(&self) -> HashMap<String, FolderCustomization> {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, key: &str) -> FolderCustomization {
        let normalized = key.trim().to_ascii_lowercase();
        let all = self.load_all();
        if let Some(found) = all.get(&normalized) {
            return found.clone();
        }
        // Fallback by leaf folder suffix for remote folders
        if let Some(leaf) = normalized.strip_prefix("remote:") {
            for (k, v) in &all {
                if k.starts_with("remote:") && (k.ends_with(leaf) || leaf.ends_with(k.trim_start_matches("remote:"))) {
                    return v.clone();
                }
            }
        }
        FolderCustomization::default()
    }

    pub fn set(&self, key: &str, value: FolderCustomization) {
        let normalized = key.trim().to_ascii_lowercase();
        let mut all = self.load_all();
        all.insert(normalized, value);
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(encoded) = serde_json::to_vec_pretty(&all) {
            let _ = fs::write(&self.path, encoded);
        }
    }
}

pub fn ensure_folder_customization_css() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };
        let provider = gtk::CssProvider::new();
        let mut css = String::from(
            ".folder-color-swatch { min-width: 22px; max-width: 22px; min-height: 22px; max-height: 22px; padding: 0; margin: 0; border-radius: 999px; border: 2px solid transparent; }\n\
             .folder-color-swatch > image { -gtk-icon-size: 12px; min-width: 12px; min-height: 12px; }\n\
             .folder-color-swatch.selected-swatch { border-color: @accent_color; box-shadow: 0 0 0 1px @window_bg_color inset; }\n\
             .folder-icon-choice { min-width: 30px; min-height: 30px; padding: 4px; border-radius: 6px; }\n\
             .folder-icon-choice.selected-icon { background-color: alpha(@accent_bg_color, 0.25); color: @accent_color; }\n",
        );
        for (name, hex) in FOLDER_COLOR_CHOICES {
            if !name.is_empty() {
                css.push_str(&format!(
                    ".folder-color-{name} {{ color: {hex}; opacity: 1.0; }}\n\
                     .folder-swatch-{name} {{ background-color: {hex}; }}\n"
                ));
            }
        }
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

pub fn apply_folder_icon_customization(icon: &gtk::Image, key: &str, default_icon: &str) {
    ensure_folder_customization_css();
    let custom = FolderCustomizationStore::default().get(key);
    let chosen_icon = custom
        .icon_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(default_icon);
    icon.set_icon_name(Some(chosen_icon));
    for (name, _) in FOLDER_COLOR_CHOICES {
        if !name.is_empty() {
            icon.remove_css_class(&format!("folder-color-{name}"));
        }
    }
    if let Some(color) = custom.accent_color.as_deref().filter(|s| !s.is_empty()) {
        icon.remove_css_class("dim-label");
        icon.add_css_class(&format!("folder-color-{color}"));
    } else {
        icon.add_css_class("dim-label");
    }
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
                    glib::ParamSpecString::builder("folder-name")
                        .construct()
                        .build(),
                    glib::ParamSpecString::builder("account-name")
                        .construct()
                        .build(),
                    glib::ParamSpecString::builder("folder-path")
                        .construct()
                        .build(),
                    glib::ParamSpecUInt::builder("total-count")
                        .construct()
                        .build(),
                    glib::ParamSpecUInt::builder("unread-count")
                        .construct()
                        .build(),
                    glib::ParamSpecBoolean::builder("is-remote")
                        .construct()
                        .build(),
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

fn make_value_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    let val_label = gtk::Label::builder()
        .label(value)
        .valign(gtk::Align::Center)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .selectable(true)
        .css_classes(["dim-label"])
        .build();
    row.add_suffix(&val_label);
    row
}

fn is_builtin_local_folder(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "inbox"
            | "sent"
            | "drafts"
            | "trash"
            | "archive"
            | "junk"
            | "spam"
            | "posteingang"
            | "gesendet"
            | "entwürfe"
            | "papierkorb"
            | "archiv"
    )
}

impl FolderPropertiesDialog {
    pub fn new(snapshot: FolderPropertiesSnapshot) -> Self {
        let title = if snapshot.folder_name.trim().is_empty() {
            gettext("Folder Properties")
        } else {
            snapshot.folder_name.clone()
        };
        glib::Object::builder()
            .property("title", title)
            .property("content-width", 480)
            .property("content-height", 500)
            .property("folder-name", snapshot.folder_name)
            .property("account-name", snapshot.account_name)
            .property("folder-path", snapshot.folder_path)
            .property("total-count", snapshot.total_count)
            .property("unread-count", snapshot.unread_count)
            .property("is-remote", snapshot.is_remote)
            .build()
    }

    fn build_ui(&self) {
        ensure_folder_customization_css();

        let folder_name = self.imp().folder_name.borrow().clone();
        let account_name = self.imp().account_name.borrow().clone();
        let folder_path = self.imp().folder_path.borrow().clone();
        let total_count = *self.imp().total_count.borrow();
        let unread_count = *self.imp().unread_count.borrow();
        let is_remote = *self.imp().is_remote.borrow();

        let raw_local_id = folder_path
            .split_once(" / ")
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_else(|| folder_name.clone());
        let is_custom_local = !is_remote && !is_builtin_local_folder(&raw_local_id);

        let folder_key = Rc::new(RefCell::new(FolderCustomizationStore::key_for_path(
            is_remote,
            &folder_path,
            &folder_name,
        )));
        let current_custom = Rc::new(RefCell::new(
            FolderCustomizationStore::default().get(&folder_key.borrow()),
        ));

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();

        // ── Group 1: Overview ────────────────────────────────────────────
        let overview_group = adw::PreferencesGroup::new();

        if is_custom_local {
            let row_name = adw::EntryRow::builder()
                .title(gettext("Folder Name"))
                .text(&raw_local_id)
                .show_apply_button(true)
                .use_markup(false)
                .build();
            let current_id = Rc::new(RefCell::new(format!("custom:{raw_local_id}")));
            let folder_key_for_rename = Rc::clone(&folder_key);
            let current_custom_for_rename = Rc::clone(&current_custom);
            let dialog_weak = self.downgrade();
            row_name.connect_apply(move |entry| {
                let new_name = entry.text().trim().to_string();
                if new_name.is_empty() {
                    return;
                }
                let from_id = current_id.borrow().clone();
                match crate::services::MailStoreService::new().rename_local_folder(&from_id, &new_name) {
                    Ok(updated_id) => {
                        let clean = updated_id
                            .strip_prefix("custom:")
                            .unwrap_or(&updated_id)
                            .to_string();
                        let new_key = format!("local:{}", clean.to_ascii_lowercase());
                        FolderCustomizationStore::default()
                            .set(&new_key, current_custom_for_rename.borrow().clone());
                        *folder_key_for_rename.borrow_mut() = new_key;
                        *current_id.borrow_mut() = updated_id;
                        if let Some(dialog) = dialog_weak.upgrade() {
                            dialog.set_title(&clean);
                            dialog.show_toast(&gettext("Folder renamed"));
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = dialog_weak.upgrade() {
                            dialog.show_toast(&format!("{}: {error}", gettext("Could not rename folder")));
                        }
                    }
                }
            });
            overview_group.add(&row_name);
        } else {
            overview_group.add(&make_value_row(&gettext("Folder"), &folder_name));
        }

        let display_account = if is_remote {
            account_name.clone()
        } else {
            gettext("On This Computer")
        };
        overview_group.add(&make_value_row(&gettext("Location"), &display_account));

        let row_path = make_value_row(&gettext("Path"), &folder_path);
        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Copy folder path"))
            .css_classes(["flat", "circular"])
            .build();
        let path_text = folder_path.clone();
        let dialog_weak = self.downgrade();
        copy_button.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&path_text);
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.show_toast(&gettext("Folder path copied"));
                }
            }
        });
        row_path.add_suffix(&copy_button);
        overview_group.add(&row_path);

        let messages_summary = format!(
            "{} ({} {})",
            total_count,
            unread_count,
            gettext("unread")
        );
        overview_group.add(&make_value_row(&gettext("Messages"), &messages_summary));

        page.add(&overview_group);

        // ── Group 2: Appearance (Sidebar Icon & Accent Color) ────────────
        let appearance_group = adw::PreferencesGroup::builder()
            .title(gettext("Appearance"))
            .build();

        let row_icon = adw::ActionRow::builder()
            .title(gettext("Icon"))
            .use_markup(false)
            .build();
        let icon_grid = gtk::Grid::builder()
            .column_spacing(4)
            .row_spacing(4)
            .margin_top(6)
            .margin_bottom(6)
            .valign(gtk::Align::Center)
            .build();
        let icon_buttons: Rc<RefCell<Vec<(String, gtk::Button)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let default_folder_icon = {
            let raw = crate::views::MailPage::folder_icon_name_for(&folder_path, &folder_name);
            match raw {
                "mail-spam-symbolic" => "mail-mark-junk-symbolic",
                "mail-mark-important-symbolic" => "emblem-important-symbolic",
                "mail-read-symbolic" => "mail-unread-symbolic",
                other => other,
            }
            .to_string()
        };
        let active_icon = current_custom
            .borrow()
            .icon_name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_folder_icon.clone());

        for (idx, (value, preview_icon)) in FOLDER_ICON_CHOICES.iter().enumerate() {
            let tip = match *value {
                "folder-symbolic" => gettext("Folder"),
                "mail-inbox-symbolic" => gettext("Inbox"),
                "mail-send-symbolic" => gettext("Sent"),
                "mail-outbox-symbolic" => gettext("Outbox"),
                "mail-drafts-symbolic" => gettext("Drafts"),
                "mail-mark-junk-symbolic" => gettext("Junk"),
                "user-trash-symbolic" => gettext("Trash"),
                "mail-unread-symbolic" => gettext("Unread"),
                "starred-symbolic" => gettext("Starred"),
                "emblem-important-symbolic" => gettext("Important"),
                "package-x-generic-symbolic" => gettext("Archive"),
                "emblem-favorite-symbolic" => gettext("Favorite"),
                "security-high-symbolic" => gettext("Security"),
                "system-users-symbolic" => gettext("Shared"),
                other => other.to_string(),
            };
            let btn = gtk::Button::builder()
                .icon_name(*preview_icon)
                .valign(gtk::Align::Center)
                .css_classes(["flat", "folder-icon-choice"])
                .tooltip_text(tip)
                .build();
            if *value == active_icon.as_str() {
                btn.add_css_class("selected-icon");
            }
            let val_str = (*value).to_string();
            let default_icon_c = default_folder_icon.clone();
            let icon_buttons_c = Rc::clone(&icon_buttons);
            let folder_key_c = Rc::clone(&folder_key);
            let current_custom_c = Rc::clone(&current_custom);
            btn.connect_clicked(move |_| {
                for (v, b) in icon_buttons_c.borrow().iter() {
                    if v == &val_str {
                        b.add_css_class("selected-icon");
                    } else {
                        b.remove_css_class("selected-icon");
                    }
                }
                let mut guard = current_custom_c.borrow_mut();
                guard.icon_name = if val_str == default_icon_c {
                    None
                } else {
                    Some(val_str.clone())
                };
                FolderCustomizationStore::default().set(&folder_key_c.borrow(), guard.clone());
            });
            icon_grid.attach(&btn, (idx % 7) as i32, (idx / 7) as i32, 1, 1);
            icon_buttons.borrow_mut().push(((*value).to_string(), btn));
        }
        row_icon.add_suffix(&icon_grid);
        appearance_group.add(&row_icon);

        let row_color = adw::ActionRow::builder()
            .title(gettext("Color"))
            .use_markup(false)
            .build();
        let color_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let color_buttons: Rc<RefCell<Vec<(String, gtk::Button)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let active_color = current_custom
            .borrow()
            .accent_color
            .clone()
            .unwrap_or_default();

        for (color_name, _) in FOLDER_COLOR_CHOICES {
            let btn = if color_name.is_empty() {
                gtk::Button::builder()
                    .icon_name("window-close-symbolic")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat", "circular", "folder-color-swatch"])
                    .tooltip_text(gettext("Default"))
                    .build()
            } else {
                gtk::Button::builder()
                    .valign(gtk::Align::Center)
                    .css_classes([
                        "flat",
                        "circular",
                        "folder-color-swatch",
                        &format!("folder-swatch-{color_name}"),
                    ])
                    .tooltip_text(*color_name)
                    .build()
            };
            if *color_name == active_color.as_str() {
                btn.add_css_class("selected-swatch");
            }
            let val_str = (*color_name).to_string();
            let color_buttons_c = Rc::clone(&color_buttons);
            let folder_key_c = Rc::clone(&folder_key);
            let current_custom_c = Rc::clone(&current_custom);
            btn.connect_clicked(move |_| {
                for (v, b) in color_buttons_c.borrow().iter() {
                    if v == &val_str {
                        b.add_css_class("selected-swatch");
                    } else {
                        b.remove_css_class("selected-swatch");
                    }
                }
                let mut guard = current_custom_c.borrow_mut();
                guard.accent_color = if val_str.is_empty() {
                    None
                } else {
                    Some(val_str.clone())
                };
                FolderCustomizationStore::default().set(&folder_key_c.borrow(), guard.clone());
            });
            color_box.append(&btn);
            color_buttons
                .borrow_mut()
                .push(((*color_name).to_string(), btn));
        }
        row_color.add_suffix(&color_box);
        appearance_group.add(&row_color);

        page.add(&appearance_group);

        // ── Group 3: Behavior & Local / Sync Options ─────────────────────
        let behavior_group = adw::PreferencesGroup::builder()
            .title(if is_remote {
                gettext("Synchronization")
            } else {
                gettext("Local Folder Options")
            })
            .build();

        let row_unified = adw::SwitchRow::builder()
            .title(gettext("Include in Unified Views & Search"))
            .use_markup(false)
            .active(current_custom.borrow().include_in_unified)
            .build();
        {
            let folder_key_c = Rc::clone(&folder_key);
            let current_custom_c = Rc::clone(&current_custom);
            row_unified.connect_active_notify(move |row| {
                let mut guard = current_custom_c.borrow_mut();
                guard.include_in_unified = row.is_active();
                FolderCustomizationStore::default().set(&folder_key_c.borrow(), guard.clone());
            });
        }
        behavior_group.add(&row_unified);

        if is_remote {
            let row_check = adw::SwitchRow::builder()
                .title(gettext("Check for New Mail"))
                .use_markup(false)
                .active(current_custom.borrow().check_new_mail)
                .build();
            {
                let folder_key_c = Rc::clone(&folder_key);
                let current_custom_c = Rc::clone(&current_custom);
                row_check.connect_active_notify(move |row| {
                    let mut guard = current_custom_c.borrow_mut();
                    guard.check_new_mail = row.is_active();
                    FolderCustomizationStore::default().set(&folder_key_c.borrow(), guard.clone());
                });
            }
            behavior_group.add(&row_check);

            let row_filter = adw::SwitchRow::builder()
                .title(gettext("Apply Incoming Filters"))
                .use_markup(false)
                .active(current_custom.borrow().apply_filters)
                .build();
            {
                let folder_key_c = Rc::clone(&folder_key);
                let current_custom_c = Rc::clone(&current_custom);
                row_filter.connect_active_notify(move |row| {
                    let mut guard = current_custom_c.borrow_mut();
                    guard.apply_filters = row.is_active();
                    FolderCustomizationStore::default().set(&folder_key_c.borrow(), guard.clone());
                });
            }
            behavior_group.add(&row_filter);
        } else {
            let retention_options = [
                gettext("Keep Forever"),
                gettext("Delete After 30 Days"),
                gettext("Delete After 90 Days"),
                gettext("Delete After 1 Year"),
            ];
            let retention_slices: Vec<&str> = retention_options.iter().map(|s| s.as_str()).collect();
            let row_retention = adw::ComboRow::builder()
                .title(gettext("Auto-Clean Old Messages"))
                .use_markup(false)
                .model(&gtk::StringList::new(&retention_slices))
                .build();
            let initial_retention_idx = match current_custom.borrow().retention_days {
                30 => 1,
                90 => 2,
                365 => 3,
                _ => 0,
            };
            row_retention.set_selected(initial_retention_idx);
            {
                let folder_key_c = Rc::clone(&folder_key);
                let current_custom_c = Rc::clone(&current_custom);
                row_retention.connect_selected_notify(move |combo| {
                    let days = match combo.selected() {
                        1 => 30,
                        2 => 90,
                        3 => 365,
                        _ => 0,
                    };
                    let mut guard = current_custom_c.borrow_mut();
                    guard.retention_days = days;
                    FolderCustomizationStore::default().set(&folder_key_c.borrow(), guard.clone());
                });
            }
            behavior_group.add(&row_retention);

            let row_rule = adw::ActionRow::builder()
                .title(gettext("Route Mail to This Folder"))
                .use_markup(false)
                .build();
            let create_rule_btn = gtk::Button::builder()
                .label(gettext("New Filter Rule…"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let target_folder_for_rule = format!("local:{raw_local_id}");
            let folder_display_for_rule = raw_local_id.clone();
            let dialog_for_rule = self.downgrade();
            create_rule_btn.connect_clicked(move |_| {
                let Some(dialog) = dialog_for_rule.upgrade() else {
                    return;
                };
                let template = crate::services::mail_filter_service::MailFilterRule {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: format!("→ {folder_display_for_rule}"),
                    enabled: true,
                    scope: crate::services::mail_filter_service::FilterScope::Incoming,
                    condition: crate::services::mail_filter_service::FilterCondition {
                        field: crate::services::mail_filter_service::FilterField::Sender,
                        value: String::new(),
                        operator: crate::services::mail_filter_service::FilterOperator::Contains,
                    },
                    action: crate::services::mail_filter_service::FilterAction::MoveToFolder(
                        target_folder_for_rule.clone(),
                    ),
                    conditions: Vec::new(),
                    actions: Vec::new(),
                    match_all: true,
                };
                let editor = crate::dialogs::FilterEditorDialog::new_for_edit(&template);
                editor.present(dialog.root().as_ref());
            });
            row_rule.add_suffix(&create_rule_btn);
            row_rule.set_activatable_widget(Some(&create_rule_btn));
            behavior_group.add(&row_rule);
        }

        page.add(&behavior_group);

        toolbar.set_content(Some(&page));
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
