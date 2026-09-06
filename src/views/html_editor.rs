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

    impl WidgetImpl for HtmlEditor {
        fn realize(&self) {
            self.parent_realize();
        }
    }
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

        let btn_strikethrough = gtk::Button::builder()
            .icon_name("format-text-strikethrough-symbolic")
            .tooltip_text("Durchgestrichen")
            .css_classes(vec!["flat"])
            .build();

        let btn_clear = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Formatierung entfernen")
            .css_classes(vec!["flat"])
            .build();

        let dropdown_format = gtk::DropDown::from_strings(&[
            "Normal",
            "Überschrift 1",
            "Überschrift 2",
            "Überschrift 3",
            "Vorformatiert",
        ]);
        dropdown_format.set_valign(gtk::Align::Center);

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

        let link_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .margin_bottom(8)
            .width_request(260)
            .build();
        link_popover_box.append(
            &gtk::Label::builder()
                .label("Hyperlink einfügen")
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .build(),
        );
        let entry_url = gtk::Entry::builder()
            .placeholder_text("https://example.com")
            .build();
        let entry_label = gtk::Entry::builder()
            .placeholder_text("Link-Text (optional)")
            .build();
        let btn_insert_link_confirm = gtk::Button::builder()
            .label("Einfügen")
            .css_classes(vec!["suggested-action"])
            .build();
        link_popover_box.append(&entry_url);
        link_popover_box.append(&entry_label);
        link_popover_box.append(&btn_insert_link_confirm);

        let link_popover = gtk::Popover::builder().child(&link_popover_box).build();
        let btn_link = gtk::MenuButton::builder()
            .icon_name("insert-link-symbolic")
            .tooltip_text("Hyperlink einfügen")
            .popover(&link_popover)
            .css_classes(vec!["flat"])
            .build();

        let btn_rule = gtk::Button::builder()
            .icon_name("insert-object-symbolic")
            .tooltip_text("Horizontale Linie einfügen")
            .css_classes(vec!["flat"])
            .build();

        let btn_image = gtk::Button::builder()
            .icon_name("insert-image-symbolic")
            .tooltip_text("Bild einfügen")
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

        toolbar.append(&dropdown_format);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_bold);
        toolbar.append(&btn_italic);
        toolbar.append(&btn_underline);
        toolbar.append(&btn_strikethrough);
        toolbar.append(&btn_clear);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_list);
        toolbar.append(&btn_ordered_list);
        toolbar.append(&btn_blockquote);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_link);
        toolbar.append(&btn_image);
        toolbar.append(&btn_rule);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_undo);
        toolbar.append(&btn_redo);

        self.imp().format_toolbar.replace(Some(toolbar.clone()));

        vbox.append(&toolbar);
        vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // WebKit6 WebView Canvas
        let web_context = webkit6::WebContext::new();
        let inline_spelling = AppSettings::try_new()
            .map(|settings| settings.composer_inline_spelling())
            .unwrap_or(true);
        web_context.set_spell_checking_enabled(inline_spelling);
        if let Some(settings) = AppSettings::try_new() {
            let language_setting = settings.spellcheck_languages();
            let languages = language_setting
                .split(',')
                .filter(|language| !language.is_empty())
                .collect::<Vec<_>>();
            if !languages.is_empty() {
                web_context.set_spell_checking_languages(&languages);
            }
        }
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

        let web_view_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&web_view)
            .hexpand(true)
            .vexpand(true)
            .build();

        vbox.append(&web_view_scroll);
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

        btn_strikethrough.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('strikethrough', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_clear.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('removeFormat', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        dropdown_format.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let tag = match dropdown.selected() {
                    1 => "h1",
                    2 => "h2",
                    3 => "h3",
                    4 => "pre",
                    _ => "p",
                };
                let script = format!("document.execCommand('formatBlock', false, '{tag}');");
                web_view.evaluate_javascript(
                    &script,
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

        let web_view_for_link = web_view.clone();
        let link_popover_weak = link_popover.downgrade();
        let entry_url_weak = entry_url.downgrade();
        let entry_label_weak = entry_label.downgrade();
        btn_insert_link_confirm.connect_clicked(move |_| {
            let Some(entry_url) = entry_url_weak.upgrade() else {
                return;
            };
            let Some(entry_label) = entry_label_weak.upgrade() else {
                return;
            };
            let url = entry_url.text().trim().to_string();
            let label = entry_label.text().trim().to_string();
            if !url.is_empty() {
                let js = if label.is_empty() {
                    format!(
                        "document.execCommand('createLink', false, '{}');",
                        url.replace('\'', "\\'")
                    )
                } else {
                    format!(
                        "document.execCommand('insertHTML', false, '<a href=\"{}\">{}</a>');",
                        url.replace('\"', "\\\""),
                        label.replace('<', "&lt;").replace('>', "&gt;")
                    )
                };
                web_view_for_link.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
                entry_url.set_text("");
                entry_label.set_text("");
            }
            if let Some(popover) = link_popover_weak.upgrade() {
                popover.popdown();
            }
        });

        btn_rule.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('insertHorizontalRule', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let web_view_for_image = web_view.clone();
        let editor_for_image = self.downgrade();
        btn_image.connect_clicked(move |_| {
            let Some(editor) = editor_for_image.upgrade() else {
                return;
            };
            let parent = editor.root().and_downcast::<gtk::Window>();
            let file_dialog = gtk::FileDialog::builder()
                .title("Bild zum Einfügen auswählen")
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Bilder (*.png, *.jpg, *.jpeg, *.gif, *.svg, *.webp)"));
            filter.add_mime_type("image/png");
            filter.add_mime_type("image/jpeg");
            filter.add_mime_type("image/gif");
            filter.add_mime_type("image/svg+xml");
            filter.add_mime_type("image/webp");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            file_dialog.set_filters(Some(&filters));

            let web_view = web_view_for_image.clone();
            file_dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        if let Ok(bytes) = std::fs::read(&path) {
                            let mime = match path
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .unwrap_or("")
                                .to_ascii_lowercase()
                                .as_str()
                            {
                                "png" => "image/png",
                                "jpg" | "jpeg" => "image/jpeg",
                                "gif" => "image/gif",
                                "svg" => "image/svg+xml",
                                "webp" => "image/webp",
                                _ => "image/png",
                            };
                            let b64 = encode_base64(&bytes);
                            let data_uri = format!("data:{mime};base64,{b64}");
                            let js = format!(
                                "document.execCommand('insertImage', false, '{data_uri}');"
                            );
                            web_view.evaluate_javascript(
                                &js,
                                None,
                                None,
                                None::<&gio::Cancellable>,
                                |_| {},
                            );
                        }
                    }
                }
            });
        });

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

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(
            TABLE[(((first & 0x03) << 4) | second.map_or(0, |value| value >> 4)) as usize] as char,
        );
        encoded.push(second.map_or('=', |value| {
            TABLE[(((value & 0x0f) << 2) | third.map_or(0, |third| third >> 6)) as usize] as char
        }));
        encoded.push(third.map_or('=', |value| TABLE[(value & 0x3f) as usize] as char));
    }

    encoded
}
