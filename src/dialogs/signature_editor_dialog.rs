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
    }

    impl std::fmt::Debug for SignatureEditorDialog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SignatureEditorDialog")
                .field("edit_signature_json", &self.edit_signature_json)
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
                vec![glib::ParamSpecString::builder("edit-signature-json").build()]
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
            .property("title", gettext("Neue Signatur"))
            .property("content-width", 700)
            .property("content-height", 580)
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
            .property("title", gettext("Signatur bearbeiten"))
            .property("content-width", 700)
            .property("content-height", 580)
            .property("edit-signature-json", encoded)
            .build();
        if let Some(cb) = on_saved {
            *obj.imp().on_saved.borrow_mut() = Some(Rc::new(cb));
        }
        obj
    }

    fn build_ui(&self) {
        let edit_profile = self
            .imp()
            .edit_signature_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<SignatureProfile>(raw).ok());

        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let title_str = if edit_profile.is_some() {
            gettext("Signatur bearbeiten")
        } else {
            gettext("Neue Signatur")
        };
        let title_widget = adw::WindowTitle::new(&title_str, "");
        header_bar.set_title_widget(Some(&title_widget));

        let btn_cancel = gtk::Button::builder()
            .label(&gettext("Abbrechen"))
            .build();
        btn_cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        header_bar.pack_start(&btn_cancel);

        let btn_save = gtk::Button::builder()
            .label(&gettext("Speichern"))
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

        // 1. Signature Name
        let group_name = adw::PreferencesGroup::new();
        let entry_name = adw::EntryRow::builder()
            .title(&gettext("Signaturname"))
            .text(edit_profile.as_ref().map(|p| p.name.as_str()).unwrap_or(""))
            .build();
        group_name.add(&entry_name);
        content_box.append(&group_name);

        // 2. Signature Content (WYSIWYG Editor)
        let group_editor = adw::PreferencesGroup::builder()
            .title(&gettext("Signatur-Inhalt"))
            .description(&gettext(
                "Unterstützt Textformatierung, Links und Grafiken (Bilder können kopiert &amp; eingefügt oder über die Symbolleiste gewählt werden).",
            ))
            .vexpand(true)
            .build();

        let editor = HtmlEditor::new();
        editor.set_vexpand(true);
        editor.set_hexpand(true);

        if let Some(profile) = &edit_profile {
            if profile.is_html() {
                editor.set_html(&profile.text);
            } else {
                editor.set_text(&profile.text);
            }
        }

        let editor_frame = gtk::Frame::builder()
            .css_classes(vec!["card"])
            .vexpand(true)
            .hexpand(true)
            .height_request(340)
            .child(&editor)
            .build();

        group_editor.add(&editor_frame);
        content_box.append(&group_editor);

        // Save Action wiring
        let existing_id = edit_profile.as_ref().map(|p| p.id.clone());
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
                gettext("Signatur")
            } else {
                name_raw
            };

            let sig_id = existing_id
                .clone()
                .unwrap_or_else(|| format!("sig-{}", uuid::Uuid::new_v4()));

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
                        body = "<p><br></p>".to_string();
                    }

                    let profile = SignatureProfile {
                        id: sig_id,
                        name,
                        text: body,
                        format: "html".to_string(),
                    };

                    let store = SignatureStore::from_environment();
                    if let Err(e) = store.upsert(profile.clone()) {
                        tracing::error!("Failed to upsert signature into store: {e}");
                    }

                    if let Some(cb) = dialog.imp().on_saved.borrow().as_ref() {
                        cb(profile);
                    }

                    dialog.close();
                }
            ));
        });

        toolbar_view.set_content(Some(&content_box));
        self.set_child(Some(&toolbar_view));
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
}
