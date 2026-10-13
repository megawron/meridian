// SPDX-License-Identifier: GPL-3.0-or-later
//
// SignatureEditorDialog — WYSIWYG HTML & rich text editor for email signatures.
//
// Features:
// - Live WebKitGTK-based HTML editor with full formatting toolbar
// - Direct copy-paste and file insertion of images (Base64 data URLs / remote URLs)
// - GNOME HIG compliant AdwDialog presentation

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::signature_store_service::{SignatureProfile, SignatureStore};
use crate::views::HtmlEditor;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SignatureEditorDialog {
        pub edit_signature_json: RefCell<Option<String>>,
        pub on_saved: RefCell<Option<Rc<dyn Fn(SignatureProfile)>>>,
        pub existing_id: RefCell<Option<String>>,
        pub entry_name: RefCell<Option<adw::EntryRow>>,
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
            self.obj().build_ui();
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
            .property("content-width", 780)
            .property("content-height", 680)
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
            .property("content-width", 780)
            .property("content-height", 680)
            .property("edit-signature-json", encoded)
            .build();
        if let Some(cb) = on_saved {
            *obj.imp().on_saved.borrow_mut() = Some(Rc::new(cb));
        }
        obj.load_profile(profile);
        obj
    }

    pub fn load_profile(&self, profile: &SignatureProfile) {
        *self.imp().existing_id.borrow_mut() = Some(profile.id.clone());
        if let Some(title) = self.imp().title_widget.borrow().as_ref() {
            title.set_title(&gettext("Edit Signature"));
        }
        if let Some(entry) = self.imp().entry_name.borrow().as_ref() {
            entry.set_text(&profile.name);
        }
        let is_html = profile.is_html();
        if let Some(row_format) = self.imp().row_format.borrow().as_ref() {
            row_format.set_active(is_html);
        }
        if let Some(btn_format) = self.imp().btn_format.borrow().as_ref() {
            btn_format.set_sensitive(is_html);
            btn_format.set_active(is_html);
        }
        if let Some(editor) = self.imp().editor.borrow().as_ref() {
            if is_html {
                editor.set_plain_text_mode(false);
                editor.set_html(&profile.text);
                editor.set_format_toolbar_visible(true);
            } else {
                editor.set_plain_text_mode(true);
                editor.set_text(&profile.text);
                editor.set_format_toolbar_visible(false);
            }
        }
    }

    fn build_ui(&self) {
        let edit_profile = self
            .imp()
            .edit_signature_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<SignatureProfile>(raw).ok());

        if let Some(profile) = &edit_profile {
            *self.imp().existing_id.borrow_mut() = Some(profile.id.clone());
        }

        let is_html = edit_profile.as_ref().map(|p| p.is_html()).unwrap_or(true);

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

        let btn_format = gtk::ToggleButton::builder()
            .icon_name("format-text-bold-symbolic")
            .tooltip_text(&gettext("Toggle formatting toolbar"))
            .active(is_html)
            .sensitive(is_html)
            .css_classes(vec!["flat"])
            .build();
        self.imp().btn_format.replace(Some(btn_format.clone()));
        header_bar.pack_end(&btn_format);

        let btn_save = gtk::Button::builder()
            .label(&gettext("Save"))
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(16)
            .vexpand(true)
            .build();

        // 1. Signature Name & Format (Native Libadwaita Boxed List)
        let group_name = adw::PreferencesGroup::new();
        let entry_name = adw::EntryRow::builder()
            .title(&gettext("Signature Name"))
            .text(edit_profile.as_ref().map(|p| p.name.as_str()).unwrap_or(""))
            .show_apply_button(false)
            .build();
        entry_name.add_prefix(&gtk::Image::from_icon_name("document-edit-symbolic"));
        entry_name.connect_changed(move |row| {
            row.remove_css_class("error");
        });
        let btn_save_for_apply = btn_save.clone();
        entry_name.connect_apply(move |_| {
            btn_save_for_apply.emit_clicked();
        });
        self.imp().entry_name.replace(Some(entry_name.clone()));
        group_name.add(&entry_name);

        let row_format = adw::SwitchRow::builder()
            .title(&gettext("HTML and Rich Text"))
            .subtitle(&gettext("Enable formatting (bold, italic, lists, links) and images or logos"))
            .active(is_html)
            .build();
        row_format.add_prefix(&gtk::Image::from_icon_name("format-text-bold-symbolic"));
        self.imp().row_format.replace(Some(row_format.clone()));
        group_name.add(&row_format);

        let store_for_init = SignatureStore::from_environment();
        let is_default = edit_profile
            .as_ref()
            .map(|p| store_for_init.active_id().as_deref() == Some(&p.id))
            .unwrap_or_else(|| store_for_init.load().is_empty());

        let row_default = adw::SwitchRow::builder()
            .title(&gettext("Set as Standard Signature"))
            .subtitle(&gettext("Automatically insert into new messages"))
            .active(is_default)
            .build();
        row_default.add_prefix(&gtk::Image::from_icon_name("starred-symbolic"));
        self.imp().row_default.replace(Some(row_default.clone()));
        group_name.add(&row_default);
        content_box.append(&group_name);

        // 2. Signature Editor (.card frame directly below with full formatting toolbar)
        let editor = HtmlEditor::new();
        editor.set_vexpand(true);
        editor.set_hexpand(true);
        self.imp().editor.replace(Some(editor.clone()));

        if is_html {
            editor.set_format_toolbar_visible(true);
        } else {
            editor.set_plain_text_mode(true);
            editor.set_format_toolbar_visible(false);
        }

        if let Some(profile) = &edit_profile {
            if profile.is_html() {
                editor.set_html(&profile.text);
            } else {
                editor.set_text(&profile.text);
            }
        }

        btn_format.connect_toggled(glib::clone!(
            #[weak]
            editor,
            move |btn| {
                editor.set_format_toolbar_visible(btn.is_active());
            }
        ));

        let btn_format_weak = btn_format.downgrade();
        row_format.connect_active_notify(glib::clone!(
            #[weak]
            editor,
            move |row| {
                let active = row.is_active();
                editor.set_plain_text_mode(!active);
                if let Some(btn) = btn_format_weak.upgrade() {
                    btn.set_sensitive(active);
                    btn.set_active(active);
                }
                editor.set_format_toolbar_visible(active);
            }
        ));

        let editor_frame = gtk::Frame::builder()
            .css_classes(vec!["card"])
            .vexpand(true)
            .hexpand(true)
            .child(&editor)
            .build();
        content_box.append(&editor_frame);

        // Save Action wiring
        let dialog_weak = self.downgrade();
        let entry_name_weak = entry_name.downgrade();
        let editor_weak = editor.downgrade();

        btn_save.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let Some(entry_name) = entry_name_weak.upgrade() else {
                return;
            };
            let Some(editor) = editor_weak.upgrade() else {
                return;
            };

            let name_raw = entry_name.text().trim().to_string();
            let name = if name_raw.is_empty() {
                gettext("Signature")
            } else {
                name_raw
            };

            let existing_id_opt = dialog.imp().existing_id.borrow().clone();
            let store = SignatureStore::from_environment();

            if store.has_name(&name, existing_id_opt.as_deref()) {
                entry_name.add_css_class("error");
                if let Some(overlay) = dialog.imp().toast_overlay.borrow().as_ref() {
                    overlay.add_toast(adw::Toast::new(&gettext(
                        "A signature with this name already exists.",
                    )));
                }
                return;
            }

            let sig_id = existing_id_opt
                .unwrap_or_else(|| format!("sig-{}", uuid::Uuid::new_v4()));

            let is_html_active = dialog
                .imp()
                .row_format
                .borrow()
                .as_ref()
                .map(|r| r.is_active())
                .unwrap_or(true);
            let is_default_active = dialog
                .imp()
                .row_default
                .borrow()
                .as_ref()
                .map(|r| r.is_active())
                .unwrap_or(false);
            let format_str = if is_html_active { "html" } else { "text" }.to_string();

            editor.read_content(glib::clone!(
                #[weak]
                dialog,
                move |result| {
                    let mut body = match result {
                        Ok(content) => content.body,
                        Err(e) => {
                            tracing::warn!("Failed to read HTML editor content: {e}");
                            String::new()
                        }
                    };

                    if body.trim().is_empty() {
                        body = if is_html_active {
                            "<p><br></p>".to_string()
                        } else {
                            String::new()
                        };
                    }

                    let profile = SignatureProfile {
                        id: sig_id.clone(),
                        name,
                        text: body.clone(),
                        format: format_str,
                    };

                    let store = SignatureStore::from_environment();
                    if let Err(e) = store.upsert(profile.clone()) {
                        tracing::error!("Failed to upsert signature into store: {e}");
                    }

                    if is_default_active {
                        let _ = store.set_active(Some(&sig_id));
                        if let Some(settings) = crate::services::AppSettings::try_new() {
                            settings.set_signature_text(&body);
                        }
                    } else if store.active_id().as_deref() == Some(&sig_id) {
                        let _ = store.set_active(None);
                    }

                    if let Some(cb) = dialog.imp().on_saved.borrow().as_ref() {
                        cb(profile);
                    }

                    dialog.close();
                }
            ));
        });

        toolbar_view.set_content(Some(&content_box));

        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&toolbar_view));
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
        assert!(row_default.title().contains("Standard"));
    }
}
