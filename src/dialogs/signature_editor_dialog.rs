// SPDX-License-Identifier: GPL-3.0-or-later
//
// SignatureEditorDialog — WYSIWYG HTML & rich text editor for email signatures.
//
// Features:
// - Reusable RichTextField editor with formatting toolbar
// - Direct copy-paste and file insertion of images (Base64 data URLs / remote URLs)
// - GNOME HIG compliant AdwDialog presentation

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::glib;
use std::cell::RefCell;
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
            obj.set_content_width(760);
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
            .property("content-width", 760)
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
            .property("content-width", 760)
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
        if let Some(rich) = self.imp().rich_text.borrow().as_ref() {
            rich.set_is_html(is_html);
            if is_html {
                rich.set_html(&profile.text);
            } else {
                rich.set_plain_text(&profile.text);
            }
        }
    }

    fn build_ui(&self) {
        self.set_content_width(760);
        self.set_content_height(560);
        self.set_follows_content_size(false);
        self.add_css_class("composer-dialog");

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
            .icon_name("format-text-underline-symbolic")
            .tooltip_text(&gettext("Toggle formatting (Ctrl+Shift+F)"))
            .css_classes(vec!["flat"])
            .active(true)
            .build();
        set_accessible_label(&btn_format, &gettext("Formatting"));
        header_bar.pack_start(&btn_format);
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

        let store_for_init = SignatureStore::from_environment();
        let is_default = edit_profile
            .as_ref()
            .map(|p| store_for_init.active_id().as_deref() == Some(&p.id))
            .unwrap_or_else(|| store_for_init.load().is_empty());

        let row_default = adw::SwitchRow::builder()
            .title(&gettext("Default Signature"))
            .active(is_default)
            .visible(false)
            .build();
        self.imp().row_default.replace(Some(row_default.clone()));

        // Flush vertical box layout:
        // 1. Flush "Name" entry line styled like the composer's Subject field (full width, separator below)
        // 2. Formatting toolbar (full width, one line, flush)
        // 3. Editor filling the rest edge-to-edge (no card margins, no rounded inset)
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

        let initial_name = edit_profile.as_ref().map(|p| p.name.as_str()).unwrap_or("");
        let entry_name = gtk::Entry::builder()
            .hexpand(true)
            .text(initial_name)
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

        entry_name.connect_changed(move |row| {
            row.remove_css_class("error");
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
        if let Some(profile) = &edit_profile {
            if profile.is_html() {
                rich_text.set_html(&profile.text);
            } else {
                rich_text.set_plain_text(&profile.text);
            }
        }

        let rich_text_for_btn = rich_text.clone();
        let row_format_for_btn = row_format.clone();
        btn_format.connect_toggled(move |btn| {
            let is_active = btn.is_active();
            let is_html_active = row_format_for_btn.is_active();
            rich_text_for_btn.editor().set_format_toolbar_visible(is_active && is_html_active);
        });

        let rich_text_weak = rich_text.downgrade();
        let btn_format_weak = btn_format.downgrade();
        row_format.connect_active_notify(move |row| {
            let is_active = row.is_active();
            if let Some(rich) = rich_text_weak.upgrade() {
                rich.set_is_html(is_active);
                if let Some(btn) = btn_format_weak.upgrade() {
                    btn.set_sensitive(is_active);
                    rich.editor().set_format_toolbar_visible(is_active && btn.is_active());
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

        // Save Action wiring (unchanged saving logic)
        let dialog_weak = self.downgrade();
        let entry_name_weak = entry_name.downgrade();
        let rich_text_weak2 = rich_text.downgrade();

        btn_save.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let Some(entry_name) = entry_name_weak.upgrade() else {
                return;
            };
            let Some(rich_text) = rich_text_weak2.upgrade() else {
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

            if store.load().len() == 1 && store.active_id().is_none() {
                let _ = store.set_active(Some(&sig_id));
                if let Some(settings) = crate::services::AppSettings::try_new() {
                    settings.set_signature_text(&body);
                }
            }

            if let Some(cb) = dialog.imp().on_saved.borrow().as_ref() {
                cb(profile);
            }

            dialog.close();
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
        assert_eq!(dialog.content_width(), 760);
        assert_eq!(dialog.content_height(), 560);
        assert!(!dialog.follows_content_size());
        assert!(dialog.has_css_class("composer-dialog"));

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
        assert_eq!(edit_dialog.content_width(), 760);
        assert_eq!(edit_dialog.content_height(), 560);
        assert_eq!(edit_rich.plain_text(), "Hello\nWorld");
        assert!(edit_rich.html().contains("Hello"));
        assert!(edit_rich.html().contains("World"));
    }
}
