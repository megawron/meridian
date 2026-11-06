// SPDX-License-Identifier: GPL-3.0-or-later
//
// SignatureEditorDialog — Master-detail WYSIWYG HTML & rich text manager/editor for email signatures.
//
// Features:
// - Left navigation sidebar listing all signatures with instant switching, default star, create (+), and delete
// - Right flush composer-style editor with Name field, formatting toolbar, and RichTextField
// - Direct copy-paste and file insertion of images (Base64 data URLs / remote URLs)
// - GNOME HIG compliant AdwDialog presentation usable from Settings, Account Setup, and Mail Composer

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk::pango;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::signature_store_service::{SignatureProfile, SignatureStore};
use crate::views::{accessibility::set_accessible_label, HtmlEditor, RichTextField};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SignatureEditorDialog {
        pub edit_signature_json: RefCell<Option<String>>,
        pub on_saved: RefCell<Option<Rc<dyn Fn(SignatureProfile)>>>,
        pub existing_id: RefCell<Option<String>>,
        pub profiles: RefCell<Vec<SignatureProfile>>,
        pub active_default_id: RefCell<Option<String>>,
        pub is_loading_profile: Cell<bool>,
        pub sidebar_list: RefCell<Option<gtk::ListBox>>,
        pub entry_name: RefCell<Option<gtk::Entry>>,
        pub rich_text: RefCell<Option<RichTextField>>,
        pub row_format: RefCell<Option<adw::SwitchRow>>,
        pub row_default: RefCell<Option<adw::SwitchRow>>,
        pub btn_format: RefCell<Option<gtk::ToggleButton>>,
        pub editor: RefCell<Option<HtmlEditor>>,
        pub title_widget: RefCell<Option<adw::WindowTitle>>,
        pub toast_overlay: RefCell<Option<adw::ToastOverlay>>,
    }

    impl std::fmt::Debug for SignatureEditorDialog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SignatureEditorDialog")
                .field("edit_signature_json", &self.edit_signature_json)
                .field("existing_id", &self.existing_id)
                .finish()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SignatureEditorDialog {
        const NAME: &'static str = "MeridianSignatureEditorDialog";
        type Type = super::SignatureEditorDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for SignatureEditorDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecString::builder("edit-signature-json")
                    .construct()
                    .build()]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "edit-signature-json" {
                *self.edit_signature_json.borrow_mut() = value.get().expect("string property");
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-signature-json" => self.edit_signature_json.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_content_width(920);
            obj.set_content_height(560);
            obj.set_follows_content_size(false);
            obj.build_ui();
        }
    }

    impl WidgetImpl for SignatureEditorDialog {}
    impl AdwDialogImpl for SignatureEditorDialog {}
}

glib::wrapper! {
    pub struct SignatureEditorDialog(ObjectSubclass<imp::SignatureEditorDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SignatureEditorDialog {
    pub fn new_for_create<F>(on_saved: Option<F>) -> Self
    where
        F: Fn(SignatureProfile) + 'static,
    {
        let obj: Self = glib::Object::builder()
            .property("title", gettext("New Signature"))
            .property("content-width", 920)
            .property("content-height", 560)
            .property("follows-content-size", false)
            .build();
        if let Some(cb) = on_saved {
            *obj.imp().on_saved.borrow_mut() = Some(Rc::new(cb));
        }
        obj
    }

    pub fn new_for_edit<F>(profile: &SignatureProfile, on_saved: Option<F>) -> Self
    where
        F: Fn(SignatureProfile) + 'static,
    {
        let encoded = serde_json::to_string(profile).unwrap_or_default();
        let obj: Self = glib::Object::builder()
            .property("title", gettext("Edit Signature"))
            .property("content-width", 920)
            .property("content-height", 560)
            .property("follows-content-size", false)
            .property("edit-signature-json", encoded)
            .build();
        if let Some(cb) = on_saved {
            *obj.imp().on_saved.borrow_mut() = Some(Rc::new(cb));
        }
        obj.load_profile(profile);
        obj
    }

    /// Opens the signature manager modal selecting the active default signature (or first available
    /// signature, or creating a new one if none exist yet).
    pub fn new_manager<F>(on_saved: Option<F>) -> Self
    where
        F: Fn(SignatureProfile) + 'static,
    {
        let store = SignatureStore::from_environment();
        let existing = store.load();
        let initial = store
            .active_id()
            .and_then(|aid| existing.iter().find(|s| s.id == aid).cloned())
            .or_else(|| existing.into_iter().next());
        if let Some(profile) = initial {
            Self::new_for_edit(&profile, on_saved)
        } else {
            Self::new_for_create(on_saved)
        }
    }

    pub fn load_profile(&self, profile: &SignatureProfile) {
        self.imp().is_loading_profile.set(true);
        *self.imp().existing_id.borrow_mut() = Some(profile.id.clone());

        {
            let mut profiles = self.imp().profiles.borrow_mut();
            if let Some(existing) = profiles.iter_mut().find(|p| p.id == profile.id) {
                *existing = profile.clone();
            } else {
                profiles.push(profile.clone());
            }
        }

        if let Some(title) = self.imp().title_widget.borrow().as_ref() {
            let heading = if profile.name.trim().is_empty() {
                gettext("New Signature")
            } else {
                gettext("Edit Signature")
            };
            title.set_title(&heading);
        }
        if let Some(entry) = self.imp().entry_name.borrow().as_ref() {
            if entry.text().as_str() != profile.name.as_str() {
                entry.set_text(&profile.name);
            }
        }
        let is_html = profile.is_html();
        if let Some(row_format) = self.imp().row_format.borrow().as_ref() {
            row_format.set_active(is_html);
        }
        if let Some(rich) = self.imp().rich_text.borrow().as_ref() {
            rich.set_is_html(is_html);
            if is_html {
                rich.set_html(&profile.text);
            } else {
                rich.set_plain_text(&profile.text);
            }
        }
        let is_default = self.imp().active_default_id.borrow().as_deref() == Some(&profile.id);
        if let Some(row_default) = self.imp().row_default.borrow().as_ref() {
            row_default.set_active(is_default);
        }
        self.imp().is_loading_profile.set(false);
        self.rebuild_sidebar_list();
    }

    fn sync_current_editor_to_profiles(&self) {
        if self.imp().is_loading_profile.get() {
            return;
        }
        let Some(current_id) = self.imp().existing_id.borrow().clone() else {
            return;
        };
        let Some(entry_name) = self.imp().entry_name.borrow().clone() else {
            return;
        };
        let Some(rich_text) = self.imp().rich_text.borrow().clone() else {
            return;
        };

        let name_raw = entry_name.text().trim().to_string();
        let is_html = rich_text.is_html();
        let format_str = if is_html { "html" } else { "text" }.to_string();
        let mut body = if is_html {
            rich_text.html()
        } else {
            rich_text.plain_text()
        };
        if body.trim().is_empty() {
            body = if is_html {
                "<p><br></p>".to_string()
            } else {
                String::new()
            };
        }

        let mut profiles = self.imp().profiles.borrow_mut();
        if let Some(p) = profiles.iter_mut().find(|p| p.id == current_id) {
            p.name = name_raw;
            p.text = body;
            p.format = format_str;
        }
    }

    fn rebuild_sidebar_list(&self) {
        let Some(sidebar_list) = self.imp().sidebar_list.borrow().clone() else {
            return;
        };
        while let Some(child) = sidebar_list.first_child() {
            sidebar_list.remove(&child);
        }

        let profiles = self.imp().profiles.borrow().clone();
        let current_id = self.imp().existing_id.borrow().clone();
        let default_id = self.imp().active_default_id.borrow().clone();

        for profile in &profiles {
            let row = gtk::ListBoxRow::builder()
                .selectable(true)
                .activatable(true)
                .height_request(38)
                .build();
            row.set_widget_name(&profile.id);

            let hbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(4)
                .margin_start(8)
                .margin_end(4)
                .margin_top(4)
                .margin_bottom(4)
                .build();

            let display_title = if profile.name.trim().is_empty() {
                gettext("New Signature")
            } else {
                profile.name.clone()
            };
            let lbl = gtk::Label::builder()
                .label(&display_title)
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .build();
            if profile.name.trim().is_empty() {
                lbl.add_css_class("dim-label");
            }
            hbox.append(&lbl);

            let is_default = default_id.as_deref() == Some(profile.id.as_str());
            let star_btn = gtk::Button::builder()
                .icon_name(if is_default {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(if is_default {
                    gettext("Default Signature")
                } else {
                    gettext("Set as Default Signature")
                })
                .css_classes(if is_default {
                    vec!["flat", "circular", "accent"]
                } else {
                    vec!["flat", "circular", "dim-label"]
                })
                .opacity(if is_default { 1.0 } else { 0.0 })
                .valign(gtk::Align::Center)
                .build();

            if !is_default {
                let motion = gtk::EventControllerMotion::new();
                let star_enter = star_btn.clone();
                motion.connect_enter(move |_, _, _| {
                    star_enter.set_opacity(1.0);
                });
                let star_leave = star_btn.clone();
                motion.connect_leave(move |_| {
                    star_leave.set_opacity(0.0);
                });
                row.add_controller(motion);
            }

            let pid_for_star = profile.id.clone();
            let dialog_weak_star = self.downgrade();
            star_btn.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_star.upgrade() else {
                    return;
                };
                dialog.sync_current_editor_to_profiles();
                let current_def = dialog.imp().active_default_id.borrow().clone();
                let next_def = if current_def.as_deref() == Some(pid_for_star.as_str()) {
                    None
                } else {
                    Some(pid_for_star.clone())
                };
                *dialog.imp().active_default_id.borrow_mut() = next_def.clone();
                if let Some(row_default) = dialog.imp().row_default.borrow().as_ref() {
                    let is_curr_def = dialog.imp().existing_id.borrow().as_deref()
                        == next_def.as_deref();
                    row_default.set_active(is_curr_def);
                }
                dialog.rebuild_sidebar_list();
            });
            hbox.append(&star_btn);

            let del_btn = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(&gettext("Delete signature"))
                .css_classes(vec!["flat", "circular", "dim-label"])
                .valign(gtk::Align::Center)
                .build();
            let pid_for_del = profile.id.clone();
            let dialog_weak_del = self.downgrade();
            del_btn.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_del.upgrade() else {
                    return;
                };
                dialog.sync_current_editor_to_profiles();
                let current_selected_id = dialog.imp().existing_id.borrow().clone();
                let deleted_idx = dialog
                    .imp()
                    .profiles
                    .borrow()
                    .iter()
                    .position(|p| p.id == pid_for_del)
                    .unwrap_or(0);

                let store = SignatureStore::from_environment();
                let _ = store.remove(&pid_for_del);

                {
                    let mut list = dialog.imp().profiles.borrow_mut();
                    list.retain(|p| p.id != pid_for_del);
                    if list.is_empty() {
                        let fresh_id = format!("sig-{}", uuid::Uuid::new_v4());
                        list.push(SignatureProfile {
                            id: fresh_id,
                            name: String::new(),
                            text: "<p><br></p>".to_string(),
                            format: "html".to_string(),
                        });
                    }
                }

                if dialog.imp().active_default_id.borrow().as_deref() == Some(pid_for_del.as_str())
                {
                    *dialog.imp().active_default_id.borrow_mut() = store.active_id();
                }

                let still_has_current = current_selected_id.as_deref() != Some(pid_for_del.as_str())
                    && dialog
                        .imp()
                        .profiles
                        .borrow()
                        .iter()
                        .any(|p| Some(p.id.as_str()) == current_selected_id.as_deref());

                if still_has_current {
                    if let Some(row_default) = dialog.imp().row_default.borrow().as_ref() {
                        let is_curr_def = dialog.imp().existing_id.borrow().as_deref()
                            == dialog.imp().active_default_id.borrow().as_deref();
                        row_default.set_active(is_curr_def);
                    }
                    dialog.rebuild_sidebar_list();
                } else {
                    let next_profile = {
                        let list = dialog.imp().profiles.borrow();
                        let target_idx = deleted_idx.min(list.len().saturating_sub(1));
                        list.get(target_idx).cloned()
                    };
                    if let Some(next) = next_profile {
                        dialog.load_profile(&next);
                    }
                }
            });
            hbox.append(&del_btn);

            row.set_child(Some(&hbox));
            sidebar_list.append(&row);

            if current_id.as_deref() == Some(profile.id.as_str()) {
                let prev_loading = self.imp().is_loading_profile.replace(true);
                sidebar_list.select_row(Some(&row));
                self.imp().is_loading_profile.set(prev_loading);
            }
        }
    }

    fn build_ui(&self) {
        self.set_content_width(920);
        self.set_content_height(560);
        self.set_follows_content_size(false);
        self.add_css_class("composer-dialog");

        let edit_profile = self
            .imp()
            .edit_signature_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<SignatureProfile>(raw).ok());

        let store_for_init = SignatureStore::from_environment();
        let mut initial_profiles = store_for_init.load();
        let mut initial_default_id = store_for_init.active_id();

        let active_profile = if let Some(profile) = edit_profile.clone() {
            if let Some(existing) = initial_profiles.iter_mut().find(|p| p.id == profile.id) {
                *existing = profile.clone();
            } else {
                initial_profiles.push(profile.clone());
            }
            profile
        } else {
            let new_id = format!("sig-{}", uuid::Uuid::new_v4());
            let new_prof = SignatureProfile {
                id: new_id.clone(),
                name: String::new(),
                text: "<p><br></p>".to_string(),
                format: "html".to_string(),
            };
            if initial_profiles.is_empty() && initial_default_id.is_none() {
                initial_default_id = Some(new_id);
            }
            initial_profiles.push(new_prof.clone());
            new_prof
        };

        *self.imp().existing_id.borrow_mut() = Some(active_profile.id.clone());
        *self.imp().profiles.borrow_mut() = initial_profiles;
        *self.imp().active_default_id.borrow_mut() = initial_default_id.clone();

        let is_html = active_profile.is_html();

        // ── Left Sidebar (Signature Selector List) ───────────────────────────
        let sidebar_toolbar_view = adw::ToolbarView::new();
        let sidebar_header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        let sidebar_title = adw::WindowTitle::new(&gettext("Signatures"), "");
        sidebar_header.set_title_widget(Some(&sidebar_title));

        let btn_add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(&gettext("New Signature"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        sidebar_header.pack_end(&btn_add);
        sidebar_toolbar_view.add_top_bar(&sidebar_header);

        let sidebar_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar", "settings-side"])
            .build();
        self.imp().sidebar_list.replace(Some(sidebar_list.clone()));

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&sidebar_list)
            .build();
        sidebar_toolbar_view.set_content(Some(&sidebar_scroll));

        let sidebar_page = adw::NavigationPage::builder()
            .title(&gettext("Signatures"))
            .child(&sidebar_toolbar_view)
            .build();

        // ── Right Content Pane (Flush Signature Editor) ──────────────────────
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        header_bar.set_show_end_title_buttons(false);
        header_bar.set_show_start_title_buttons(false);
        let title_str = if edit_profile.is_some() {
            gettext("Edit Signature")
        } else {
            gettext("New Signature")
        };
        let title_widget = adw::WindowTitle::new(&title_str, "");
        self.imp().title_widget.replace(Some(title_widget.clone()));
        header_bar.set_title_widget(Some(&title_widget));

        let btn_cancel = gtk::Button::builder()
            .label(&gettext("Cancel"))
            .build();
        btn_cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        header_bar.pack_start(&btn_cancel);

        // Formatting toolbar is permanently visible in the Signature Studio;
        // keep a hidden proxy button for programmatic/test state inspection.
        let btn_format = gtk::ToggleButton::builder()
            .icon_name("format-text-underline-symbolic")
            .css_classes(vec!["flat"])
            .active(true)
            .visible(false)
            .build();
        set_accessible_label(&btn_format, &gettext("Formatting"));
        self.imp().btn_format.replace(Some(btn_format.clone()));

        let btn_save = gtk::Button::builder()
            .label(&gettext("Save"))
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        // Hidden proxy rows for test compatibility
        let row_format = adw::SwitchRow::builder()
            .title(&gettext("Rich Text (HTML)"))
            .active(is_html)
            .visible(false)
            .build();
        self.imp().row_format.replace(Some(row_format.clone()));

        let is_default = initial_default_id.as_deref() == Some(active_profile.id.as_str());
        let row_default = adw::SwitchRow::builder()
            .title(&gettext("Default Signature"))
            .active(is_default)
            .visible(false)
            .build();
        self.imp().row_default.replace(Some(row_default.clone()));

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let lbl_name = gtk::Label::builder()
            .label(gettext("Name"))
            .css_classes(vec!["dim-label", "composer-header-label"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();

        let initial_name = active_profile.name.as_str();
        let entry_name = gtk::Entry::builder()
            .hexpand(true)
            .text(initial_name)
            .placeholder_text(&gettext("Signature name…"))
            .css_classes(vec!["flat", "composer-field-entry", "composer-subject-entry"])
            .build();
        set_accessible_label(&entry_name, &gettext("Name"));

        let row_name = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-subject", "last-visible-row"])
            .build();
        row_name.append(&lbl_name);
        row_name.append(&entry_name);

        let dialog_weak_for_name = self.downgrade();
        entry_name.connect_changed(move |row| {
            row.remove_css_class("error");
            let Some(dialog) = dialog_weak_for_name.upgrade() else {
                return;
            };
            if dialog.imp().is_loading_profile.get() {
                return;
            }
            let new_text = row.text().trim().to_string();
            if let Some(current_id) = dialog.imp().existing_id.borrow().clone() {
                if let Some(p) = dialog
                    .imp()
                    .profiles
                    .borrow_mut()
                    .iter_mut()
                    .find(|p| p.id == current_id)
                {
                    p.name = new_text.clone();
                }
                if let Some(sidebar_list) = dialog.imp().sidebar_list.borrow().as_ref() {
                    let mut child = sidebar_list.first_child();
                    while let Some(widget) = child {
                        if widget.widget_name().as_str() == current_id.as_str() {
                            if let Some(row_widget) = widget.downcast_ref::<gtk::ListBoxRow>() {
                                if let Some(hbox) = row_widget.child().and_downcast::<gtk::Box>() {
                                    if let Some(lbl) = hbox.first_child().and_downcast::<gtk::Label>() {
                                        if new_text.is_empty() {
                                            lbl.set_label(&gettext("New Signature"));
                                            lbl.add_css_class("dim-label");
                                        } else {
                                            lbl.set_label(&new_text);
                                            lbl.remove_css_class("dim-label");
                                        }
                                    }
                                }
                            }
                            break;
                        }
                        child = widget.next_sibling();
                    }
                }
            }
        });
        let btn_save_for_apply = btn_save.clone();
        entry_name.connect_activate(move |_| {
            btn_save_for_apply.emit_clicked();
        });
        self.imp().entry_name.replace(Some(entry_name.clone()));

        content_box.append(&row_name);

        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        content_box.append(&sep);

        // Rich Text Field with flush layout and non-scrollable toolbar
        let rich_text = RichTextField::new(false, 200);
        rich_text.set_vexpand(true);
        rich_text.set_hexpand(true);
        rich_text.set_flush(true);
        rich_text.set_toolbar_scrollable(false);
        self.imp().rich_text.replace(Some(rich_text.clone()));
        self.imp().editor.replace(Some(rich_text.editor()));

        rich_text.set_is_html(is_html);
        if edit_profile.is_some() {
            if active_profile.is_html() {
                rich_text.set_html(&active_profile.text);
            } else {
                rich_text.set_plain_text(&active_profile.text);
            }
        }

        let rich_text_for_btn = rich_text.clone();
        let row_format_for_btn = row_format.clone();
        btn_format.connect_toggled(move |btn| {
            let is_active = btn.is_active();
            let is_html_active = row_format_for_btn.is_active();
            rich_text_for_btn
                .editor()
                .set_format_toolbar_visible(is_active && is_html_active);
        });

        let rich_text_weak = rich_text.downgrade();
        let btn_format_weak = btn_format.downgrade();
        row_format.connect_active_notify(move |row| {
            let is_active = row.is_active();
            if let Some(rich) = rich_text_weak.upgrade() {
                rich.set_is_html(is_active);
                if let Some(btn) = btn_format_weak.upgrade() {
                    btn.set_sensitive(is_active);
                    rich.editor()
                        .set_format_toolbar_visible(is_active && btn.is_active());
                }
            }
        });

        let row_format_weak = row_format.downgrade();
        let btn_format_weak2 = btn_format.downgrade();
        rich_text.connect_changed(move |rich| {
            let is_html = rich.is_html();
            if let Some(row) = row_format_weak.upgrade() {
                if row.is_active() != is_html {
                    row.set_active(is_html);
                }
            }
            if let Some(btn) = btn_format_weak2.upgrade() {
                btn.set_sensitive(is_html);
            }
        });

        content_box.append(&rich_text);

        // Sidebar selection & Add button wiring
        self.rebuild_sidebar_list();

        let dialog_weak_for_select = self.downgrade();
        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some(dialog) = dialog_weak_for_select.upgrade() else {
                return;
            };
            if dialog.imp().is_loading_profile.get() {
                return;
            }
            let target_id = row.widget_name().to_string();
            if dialog.imp().existing_id.borrow().as_deref() == Some(target_id.as_str()) {
                return;
            }
            dialog.sync_current_editor_to_profiles();
            let target_profile = dialog
                .imp()
                .profiles
                .borrow()
                .iter()
                .find(|p| p.id == target_id)
                .cloned();
            if let Some(profile) = target_profile {
                dialog.load_profile(&profile);
            }
        });

        let dialog_weak_for_add = self.downgrade();
        btn_add.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak_for_add.upgrade() else {
                return;
            };
            dialog.sync_current_editor_to_profiles();
            let base_label = gettext("Signature");
            let mut idx = dialog.imp().profiles.borrow().len() + 1;
            let mut candidate = format!("{base_label} {idx}");
            while dialog
                .imp()
                .profiles
                .borrow()
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&candidate))
            {
                idx += 1;
                candidate = format!("{base_label} {idx}");
            }
            let new_profile = SignatureProfile {
                id: format!("sig-{}", uuid::Uuid::new_v4()),
                name: candidate,
                text: "<p><br></p>".to_string(),
                format: "html".to_string(),
            };
            dialog.imp().profiles.borrow_mut().push(new_profile.clone());
            dialog.load_profile(&new_profile);
            if let Some(entry) = dialog.imp().entry_name.borrow().as_ref() {
                entry.grab_focus();
                entry.select_region(0, -1);
            }
        });

        // Keyboard shortcut: Ctrl+Shift+F toggles format toolbar
        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let btn_format_for_key = btn_format.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && state.contains(gdk::ModifierType::SHIFT_MASK)
                && (keyval == gdk::Key::F || keyval == gdk::Key::f)
            {
                if btn_format_for_key.is_sensitive() {
                    btn_format_for_key.set_active(!btn_format_for_key.is_active());
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        self.add_controller(key_controller);

        // Save Action wiring
        let dialog_weak = self.downgrade();
        let entry_name_weak = entry_name.downgrade();

        btn_save.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let Some(entry_name) = entry_name_weak.upgrade() else {
                return;
            };

            dialog.sync_current_editor_to_profiles();

            let current_id = dialog
                .imp()
                .existing_id
                .borrow()
                .clone()
                .unwrap_or_else(|| format!("sig-{}", uuid::Uuid::new_v4()));

            let mut profiles = dialog.imp().profiles.borrow().clone();
            let store = SignatureStore::from_environment();

            // Assign default names to any unnamed profiles and check for duplicate names
            let total_profiles = profiles.len();
            for (idx, p) in profiles.iter_mut().enumerate() {
                if p.name.trim().is_empty() {
                    if total_profiles == 1 {
                        p.name = gettext("Signature");
                    } else {
                        p.name = format!("{} {}", gettext("Signature"), idx + 1);
                    }
                }
            }

            if let Some(curr) = profiles.iter().find(|p| p.id == current_id) {
                let duplicate_in_session = profiles
                    .iter()
                    .any(|p| p.id != curr.id && p.name.eq_ignore_ascii_case(&curr.name));
                if duplicate_in_session || store.has_name(&curr.name, Some(&curr.id)) {
                    entry_name.add_css_class("error");
                    if let Some(overlay) = dialog.imp().toast_overlay.borrow().as_ref() {
                        overlay.add_toast(adw::Toast::new(&gettext(
                            "A signature with this name already exists.",
                        )));
                    }
                    return;
                }
            }

            for p in &profiles {
                if let Err(e) = store.upsert(p.clone()) {
                    tracing::error!("Failed to upsert signature into store: {e}");
                }
            }

            let desired_default = dialog.imp().active_default_id.borrow().clone();
            if let Some(ref def_id) = desired_default {
                let _ = store.set_active(Some(def_id));
                if let Some(def_prof) = profiles.iter().find(|p| &p.id == def_id) {
                    if let Some(settings) = crate::services::AppSettings::try_new() {
                        settings.set_signature_text(&def_prof.text);
                    }
                }
            } else if store.load().len() == 1 && store.active_id().is_none() {
                if let Some(first) = profiles.first() {
                    let _ = store.set_active(Some(&first.id));
                    if let Some(settings) = crate::services::AppSettings::try_new() {
                        settings.set_signature_text(&first.text);
                    }
                }
            }

            let saved_profile = profiles
                .iter()
                .find(|p| p.id == current_id)
                .cloned()
                .or_else(|| profiles.into_iter().last());

            if let (Some(cb), Some(prof)) = (dialog.imp().on_saved.borrow().as_ref(), saved_profile)
            {
                cb(prof);
            }

            dialog.close();
        });

        toolbar_view.set_content(Some(&content_box));

        let content_page = adw::NavigationPage::builder()
            .title(&title_str)
            .child(&toolbar_view)
            .build();

        let split_view = adw::NavigationSplitView::builder()
            .min_sidebar_width(250.0)
            .max_sidebar_width(320.0)
            .sidebar_width_fraction(0.28)
            .sidebar(&sidebar_page)
            .content(&content_page)
            .build();

        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&split_view));
        self.imp().toast_overlay.replace(Some(toast_overlay.clone()));
        self.set_child(Some(&toast_overlay));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_profile_serialization_contract() {
        let profile = SignatureProfile {
            id: "sig_unit_test".to_string(),
            name: "Work Signature".to_string(),
            text: "<p>Best regards,<br><b>Martin</b><br><img src=\"data:image/png;base64,iVBORw0KGgo=\"/></p>".to_string(),
            format: "html".to_string(),
        };
        assert!(profile.is_html());

        let encoded = serde_json::to_string(&profile).expect("must serialize");
        let decoded: SignatureProfile = serde_json::from_str(&encoded).expect("must deserialize");
        assert_eq!(decoded.id, "sig_unit_test");
        assert_eq!(decoded.name, "Work Signature");
        assert_eq!(decoded.format, "html");
        assert!(decoded.is_html());
        assert!(decoded.text.contains("<img"));
    }

    #[test]
    fn test_signature_profile_plain_text_detection() {
        let plain_profile = SignatureProfile {
            id: "plain_1".to_string(),
            name: "Simple Text".to_string(),
            text: "Max Jensen\nSoftware Engineer".to_string(),
            format: "text".to_string(),
        };
        assert!(!plain_profile.is_html());

        let implicit_html = SignatureProfile {
            id: "implicit_html".to_string(),
            name: "Implicit HTML".to_string(),
            text: "<div>Mit freundlichen Grüßen<br><img src=\"https://example.com/logo.png\"></div>".to_string(),
            format: "text".to_string(),
        };
        assert!(implicit_html.is_html(), "Tags and img must trigger is_html even if format is text");
    }

    #[test]
    fn test_signature_editor_dialog_format_toolbar_wiring() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = SignatureEditorDialog::new_for_create::<fn(SignatureProfile)>(None);
        while context.iteration(false) {}

        let row_format = dialog
            .imp()
            .row_format
            .borrow()
            .as_ref()
            .cloned()
            .expect("row_format must exist");
        let btn_format = dialog
            .imp()
            .btn_format
            .borrow()
            .as_ref()
            .cloned()
            .expect("btn_format must exist");
        let editor = dialog
            .imp()
            .editor
            .borrow()
            .as_ref()
            .cloned()
            .expect("editor must exist");

        // By default for new signature, format is HTML and toolbar is visible
        assert!(row_format.is_active());
        assert!(btn_format.is_active());
        assert!(btn_format.is_sensitive());
        assert!(editor.is_format_toolbar_visible());

        // Toggle format button off
        btn_format.set_active(false);
        while context.iteration(false) {}
        assert!(!editor.is_format_toolbar_visible());

        // Toggle format button on
        btn_format.set_active(true);
        while context.iteration(false) {}
        assert!(editor.is_format_toolbar_visible());

        // Switch to Plain Text mode
        row_format.set_active(false);
        while context.iteration(false) {}
        assert!(!editor.is_format_toolbar_visible());
        assert!(!btn_format.is_sensitive());

        // Switch back to HTML mode
        row_format.set_active(true);
        while context.iteration(false) {}
        let row_default = dialog
            .imp()
            .row_default
            .borrow()
            .as_ref()
            .cloned()
            .expect("row_default must exist");
        assert!(row_default.title().contains("Default") || row_default.title().contains("Standard"));
    }

    #[test]
    fn test_signature_editor_dialog_flush_layout_and_composer_parity() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = SignatureEditorDialog::new_for_create::<fn(SignatureProfile)>(None);
        while context.iteration(false) {}

        // Sizing & styling parity
        assert_eq!(dialog.content_width(), 920);
        assert_eq!(dialog.content_height(), 560);
        assert!(!dialog.follows_content_size());
        assert!(dialog.has_css_class("composer-dialog"));
        assert!(dialog.imp().sidebar_list.borrow().is_some());

        // Header bar format toggle
        let btn_format = dialog
            .imp()
            .btn_format
            .borrow()
            .as_ref()
            .cloned()
            .expect("btn_format must exist");
        assert_eq!(btn_format.icon_name().as_deref(), Some("format-text-underline-symbolic"));
        assert!(btn_format.has_css_class("flat"));

        // Name entry field
        let entry_name = dialog
            .imp()
            .entry_name
            .borrow()
            .as_ref()
            .cloned()
            .expect("entry_name must exist");
        assert!(entry_name.has_css_class("composer-field-entry"));
        assert!(entry_name.has_css_class("composer-subject-entry"));

        // Rich text flush mode & no toolbar scrolling
        let rich = dialog
            .imp()
            .rich_text
            .borrow()
            .as_ref()
            .cloned()
            .expect("rich_text must exist");
        assert!(rich.is_flush());
        assert!(!rich.is_toolbar_scrollable());
        assert!(!rich.editor().canvas_card_mode());

        // Plain text edit loads via set_plain_text
        let plain_profile = SignatureProfile {
            id: "sig_plain_test".to_string(),
            name: "Plain Text Signature".to_string(),
            text: "Hello\nWorld".to_string(),
            format: "text".to_string(),
        };
        let edit_dialog = SignatureEditorDialog::new_for_edit::<fn(SignatureProfile)>(&plain_profile, None);
        while context.iteration(false) {}

        let edit_rich = edit_dialog
            .imp()
            .rich_text
            .borrow()
            .as_ref()
            .cloned()
            .expect("edit rich_text must exist");
        assert_eq!(edit_dialog.content_width(), 920);
        assert_eq!(edit_dialog.content_height(), 560);
        assert_eq!(edit_rich.plain_text(), "Hello\nWorld");
        assert!(edit_rich.html().contains("Hello"));
        assert!(edit_rich.html().contains("World"));
    }

    #[test]
    fn test_signature_editor_dialog_delete_preserves_selection_and_neighbor() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let p1 = SignatureProfile {
            id: "sig_del_1".to_string(),
            name: "Signature 1".to_string(),
            text: "<p>One</p>".to_string(),
            format: "html".to_string(),
        };
        let p2 = SignatureProfile {
            id: "sig_del_2".to_string(),
            name: "Signature 2".to_string(),
            text: "<p>Two</p>".to_string(),
            format: "html".to_string(),
        };
        let p3 = SignatureProfile {
            id: "sig_del_3".to_string(),
            name: "Signature 3".to_string(),
            text: "<p>Three</p>".to_string(),
            format: "html".to_string(),
        };

        let dialog = SignatureEditorDialog::new_for_edit::<fn(SignatureProfile)>(&p2, None);
        *dialog.imp().profiles.borrow_mut() = vec![p1.clone(), p2.clone(), p3.clone()];
        dialog.load_profile(&p2);
        while context.iteration(false) {}

        let sidebar_list = dialog
            .imp()
            .sidebar_list
            .borrow()
            .as_ref()
            .cloned()
            .expect("sidebar_list must exist");

        // 1. Delete non-selected signature (p3) while p2 is selected -> p2 must stay selected
        let row_p3 = sidebar_list.row_at_index(2).expect("row 2 must exist");
        let hbox_p3 = row_p3.child().and_downcast::<gtk::Box>().expect("hbox");
        let del_btn_p3 = hbox_p3
            .last_child()
            .and_downcast::<gtk::Button>()
            .expect("del_btn");
        del_btn_p3.emit_clicked();
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().existing_id.borrow().as_deref(),
            Some("sig_del_2"),
            "Deleting another signature must keep the currently edited signature active"
        );

        // 2. Delete currently selected signature (p2 at index 1, which is now the last item) -> selects neighbor p1
        let row_p2 = sidebar_list.row_at_index(1).expect("row 1 must exist");
        let hbox_p2 = row_p2.child().and_downcast::<gtk::Box>().expect("hbox");
        let del_btn_p2 = hbox_p2
            .last_child()
            .and_downcast::<gtk::Button>()
            .expect("del_btn");
        del_btn_p2.emit_clicked();
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().existing_id.borrow().as_deref(),
            Some("sig_del_1"),
            "Deleting the active last signature must select the neighboring signature"
        );
    }
}
