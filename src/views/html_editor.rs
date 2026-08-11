// SPDX-License-Identifier: GPL-3.0-or-later
//
// HtmlEditor — WebKitGTK 6.0 based WYSIWYG HTML editor for mail composition.
//
// Features:
// - Contenteditable HTML editor canvas
// - Formatting toolbar (Bold, Italic, Underline, Lists, Blockquote, Links)
// - Sandboxed DOM manipulation via JavaScript

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use webkit6::prelude::*;

use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub web_view: RefCell<Option<webkit6::WebView>>,
        pub format_toolbar: RefCell<Option<gtk::Box>>,
        pub plain_text_mode: Cell<bool>,
        pub web_view_ready: Cell<bool>,
        pub pending_scripts: RefCell<Vec<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HtmlEditor {
        const NAME: &'static str = "EvolutionHtmlEditor";
        type Type = super::HtmlEditor;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for HtmlEditor {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for HtmlEditor {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorContent {
    pub body: String,
    pub plain_text: bool,
}

glib::wrapper! {
    pub struct HtmlEditor(ObjectSubclass<imp::HtmlEditor>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HtmlEditor {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Read the current editor contents asynchronously on the GTK main
    /// context.  WebKit owns the DOM, so callers must not try to access it
    /// synchronously or from a worker thread.
    pub fn read_html<F>(&self, callback: F)
    where
        F: FnOnce(Result<String, glib::Error>) + 'static,
    {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            callback(Err(glib::Error::new(
                glib::FileError::Failed,
                "HTML editor is not initialized",
            )));
            return;
        };

        web_view.evaluate_javascript(
            "document.body.innerHTML",
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| callback(result.map(|value| value.to_str().to_string())),
        );
    }

    /// Read the representation that should be persisted or sent.  Plain-text
    /// mode uses `innerText` so line breaks are not accidentally serialized as
    /// WebKit markup, while rich mode retains the sanitized DOM HTML.
    pub fn read_content<F>(&self, callback: F)
    where
        F: FnOnce(Result<EditorContent, glib::Error>) + 'static,
    {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            callback(Err(glib::Error::new(
                glib::FileError::Failed,
                "HTML editor is not initialized",
            )));
            return;
        };
        let plain_text = self.plain_text_mode();
        let expression = if plain_text {
            "document.body.innerText"
        } else {
            "document.body.innerHTML"
        };
        web_view.evaluate_javascript(
            expression,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                callback(result.map(|value| EditorContent {
                    body: value.to_str().to_string(),
                    plain_text,
                }))
            },
        );
    }

    /// Replace the editable DOM contents.  The HTML is encoded as a JSON
    /// string before it is passed to JavaScript so quoted text from a
    /// reply/forward or a restored draft cannot become executable code.
    pub fn set_html(&self, html: &str) {
        let Ok(encoded) = serde_json::to_string(html) else {
            return;
        };
        let script = if self.plain_text_mode() {
            format!(
                "document.body.innerHTML = {encoded}; document.body.textContent = document.body.innerText;"
            )
        } else {
            format!("document.body.innerHTML = {encoded};")
        };
        self.evaluate_or_queue(script);
    }

    /// Replace the editor with literal text, regardless of the current mode.
    pub fn set_text(&self, text: &str) {
        let Ok(encoded) = serde_json::to_string(text) else {
            return;
        };
        self.evaluate_or_queue(format!("document.body.textContent = {encoded};"));
    }

    /// Insert sanitized HTML at the current selection without replacing the
    /// rest of the message.  In plain-text mode the same fragment is reduced
    /// to literal text inside the DOM before it is inserted.
    pub fn insert_html(&self, html: &str) {
        let safe_html = crate::services::html_sanitizer::sanitize_html(html);
        let Ok(encoded) = serde_json::to_string(&safe_html) else {
            return;
        };
        let script = if self.plain_text_mode() {
            format!(
                "const snippet = document.createElement('div'); snippet.innerHTML = {encoded}; document.execCommand('insertText', false, snippet.innerText);"
            )
        } else {
            format!("document.execCommand('insertHTML', false, {encoded});")
        };
        self.evaluate_or_queue(script);
    }

    pub fn plain_text_mode(&self) -> bool {
        self.imp().plain_text_mode.get()
    }

    /// Switch between rich HTML and literal text while preserving the current
    /// visible content.  The conversion is performed inside the DOM so user
    /// text is never interpolated into JavaScript source.
    pub fn set_plain_text_mode(&self, plain_text: bool) {
        if self.plain_text_mode() == plain_text {
            return;
        }
        self.imp().plain_text_mode.set(plain_text);
        if let Some(toolbar) = self.imp().format_toolbar.borrow().as_ref() {
            toolbar.set_sensitive(!plain_text);
        }
        let script = if plain_text {
            "document.body.textContent = document.body.innerText; document.body.style.whiteSpace = 'pre-wrap';"
                .to_string()
        } else {
            concat!(
                "const text = document.body.innerText;",
                "document.body.innerHTML = '';",
                "const lines = text.split(/\\n/);",
                "for (const line of lines) { const p = document.createElement('p'); ",
                "p.textContent = line || '\\u00a0'; document.body.appendChild(p); }",
                "document.body.style.whiteSpace = 'normal';"
            )
            .to_string()
        };
        self.evaluate_or_queue(script);
    }

    fn evaluate_or_queue(&self, script: String) {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.imp().web_view_ready.get() {
            self.imp().pending_scripts.borrow_mut().push(script);
            return;
        }
        web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
    }

    fn flush_pending_scripts(&self) {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        self.imp().web_view_ready.set(true);
        let scripts = std::mem::take(&mut *self.imp().pending_scripts.borrow_mut());
        for script in scripts {
            web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
        }
    }

    fn build_ui(&self) {
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        // WYSIWYG Toolbar
        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["linked"])
            .build();

        let btn_bold = gtk::Button::builder()
            .icon_name("format-text-bold-symbolic")
            .tooltip_text("Fett (Ctrl+B)")
            .css_classes(vec!["flat"])
            .build();

        let btn_italic = gtk::Button::builder()
            .icon_name("format-text-italic-symbolic")
            .tooltip_text("Kursiv (Ctrl+I)")
            .css_classes(vec!["flat"])
            .build();

        let btn_underline = gtk::Button::builder()
            .icon_name("format-text-underline-symbolic")
            .tooltip_text("Unterstrichen (Ctrl+U)")
            .css_classes(vec!["flat"])
            .build();

        let btn_list = gtk::Button::builder()
            .icon_name("format-list-bulleted-symbolic")
            .tooltip_text("Aufzählung")
            .css_classes(vec!["flat"])
            .build();

        let btn_ordered_list = gtk::Button::builder()
            .icon_name("format-list-numbered-symbolic")
            .tooltip_text("Nummerierte Liste")
            .css_classes(vec!["flat"])
            .build();

        let btn_blockquote = gtk::Button::builder()
            .icon_name("format-text-direction-ltr-symbolic")
            .tooltip_text("Blockzitat")
            .css_classes(vec!["flat"])
            .build();

        let btn_undo = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text("Rückgängig (Ctrl+Z)")
            .css_classes(vec!["flat"])
            .build();

        let btn_redo = gtk::Button::builder()
            .icon_name("edit-redo-symbolic")
            .tooltip_text("Wiederholen (Ctrl+Shift+Z)")
            .css_classes(vec!["flat"])
            .build();

        toolbar.append(&btn_bold);
        toolbar.append(&btn_italic);
        toolbar.append(&btn_underline);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_list);
        toolbar.append(&btn_ordered_list);
        toolbar.append(&btn_blockquote);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_undo);
        toolbar.append(&btn_redo);

        self.imp().format_toolbar.replace(Some(toolbar.clone()));

        vbox.append(&toolbar);
        vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // WebKit6 WebView Canvas
        let web_context = webkit6::WebContext::new();
        web_context.set_spell_checking_enabled(true);
        let web_view = webkit6::WebView::builder()
            .web_context(&web_context)
            .vexpand(true)
            .hexpand(true)
            .build();

        let font_family = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_family)
            .unwrap_or_else(|| "system-ui, sans-serif".to_string());
        let font_size = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_size)
            .unwrap_or(14);
        let initial_html = format!(
            "<!DOCTYPE html><html><head><style>body {{ font-family: {}; font-size: {}px; line-height: 1.5; color: #222; margin: 12px; }} @media (prefers-color-scheme: dark) {{ body {{ color: #eee; background-color: #1e1e1e; }} }}</style></head><body contenteditable='true'><p><br></p></body></html>",
            font_family, font_size
        );

        *self.imp().web_view.borrow_mut() = Some(web_view.clone());
        let editor_weak = self.downgrade();
        web_view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.flush_pending_scripts();
                }
            }
        });
        web_view.load_html(&initial_html, None);

        vbox.append(&web_view);
        vbox.set_parent(self);

        // Toolbar action wiring
        btn_bold.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('bold', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_italic.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('italic', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_underline.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('underline', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_list.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('insertUnorderedList', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_ordered_list.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('insertOrderedList', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_blockquote.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('formatBlock', false, 'blockquote');",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_undo.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('undo', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_redo.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('redo', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));
    }
}

impl Default for HtmlEditor {
    fn default() -> Self {
        Self::new()
    }
}
