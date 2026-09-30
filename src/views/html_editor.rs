use crate::i18n::gettext;
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
use gtk::gio;
use gtk::glib;
use webkit6::prelude::*;

use crate::services::settings_service::AppSettings;
use crate::views::accessibility::set_accessible_label;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub web_view: RefCell<Option<webkit6::WebView>>,
        pub web_context: RefCell<Option<webkit6::WebContext>>,
        pub format_toolbar: RefCell<Option<gtk::Box>>,
        pub plain_text_mode: Cell<bool>,
        pub web_view_ready: Cell<bool>,
        pub pending_scripts: RefCell<Vec<String>>,
        pub spell_checking_enabled: Cell<bool>,
        pub spell_checking_languages: RefCell<Vec<String>>,
        pub spelling_handler_ids: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HtmlEditor {
        const NAME: &'static str = "MeridianHtmlEditor";
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
            if let Some(settings) = AppSettings::try_new() {
                for id in self.spelling_handler_ids.borrow_mut().drain(..) {
                    settings.disconnect(id);
                }
            }
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

    /// Replace an existing signature in the DOM or insert a new one.
    /// If `signature_html` is empty, any existing signature element is removed.
    pub fn replace_or_insert_signature(&self, signature_html: &str, top_signature: bool) {
        let trimmed = signature_html.trim();
        let is_empty = trimmed.is_empty();
        let safe_html = if is_empty {
            String::new()
        } else {
            crate::services::html_sanitizer::sanitize_html(trimmed)
        };
        let Ok(encoded) = serde_json::to_string(&safe_html) else {
            return;
        };
        let script = format!(
            r#"(function() {{
    const el = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');
    const isEmpty = {is_empty};
    if (isEmpty) {{
        if (el) {{
            el.remove();
        }}
        return;
    }}
    const temp = document.createElement('div');
    temp.innerHTML = {encoded};
    const newSig = temp.firstElementChild || temp;
    if (el) {{
        el.replaceWith(newSig);
    }} else {{
        if ({top_signature}) {{
            document.body.prepend(newSig);
        }} else {{
            const br = document.createElement('p');
            br.innerHTML = '<br>';
            document.body.appendChild(br);
            document.body.appendChild(newSig);
        }}
    }}
}})();"#
        );
        self.evaluate_or_queue(script);
    }

    pub fn spell_checking_enabled(&self) -> bool {
        self.imp().spell_checking_enabled.get()
    }

    pub fn set_spell_checking_enabled(&self, enabled: bool) {
        self.imp().spell_checking_enabled.set(enabled);
        if let Some(web_context) = self.imp().web_context.borrow().as_ref() {
            web_context.set_spell_checking_enabled(enabled);
        }
    }

    pub fn spell_checking_languages(&self) -> Vec<String> {
        self.imp().spell_checking_languages.borrow().clone()
    }

    pub fn set_spell_checking_languages(&self, languages: &[&str]) {
        self.imp()
            .spell_checking_languages
            .replace(languages.iter().map(|s| s.to_string()).collect());
        if let Some(web_context) = self.imp().web_context.borrow().as_ref() {
            web_context.set_spell_checking_languages(languages);
        }
    }

    pub fn apply_spellcheck_settings(&self, settings: &AppSettings) {
        self.set_spell_checking_enabled(settings.composer_inline_spelling());
        let raw = settings.spellcheck_languages();
        let languages: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.set_spell_checking_languages(&languages);
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
            .tooltip_text(&gettext("Bold (Ctrl+B)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_bold, &gettext("Bold"));

        let btn_italic = gtk::Button::builder()
            .icon_name("format-text-italic-symbolic")
            .tooltip_text(&gettext("Italic (Ctrl+I)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_italic, &gettext("Italic"));

        let btn_underline = gtk::Button::builder()
            .icon_name("format-text-underline-symbolic")
            .tooltip_text(&gettext("Underline (Ctrl+U)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_underline, &gettext("Underline"));

        let btn_strikethrough = gtk::Button::builder()
            .icon_name("format-text-strikethrough-symbolic")
            .tooltip_text(&gettext("Strikethrough"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_strikethrough, &gettext("Strikethrough"));

        let btn_clear = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text(&gettext("Clear formatting"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_clear, &gettext("Clear formatting"));

        let format_labels = [
            gettext("Normal"),
            gettext("Heading 1"),
            gettext("Heading 2"),
            gettext("Heading 3"),
            gettext("Preformatted"),
        ];
        let format_strs: Vec<&str> = format_labels.iter().map(|s| s.as_str()).collect();
        let dropdown_format = gtk::DropDown::from_strings(&format_strs);
        dropdown_format.set_valign(gtk::Align::Center);
        set_accessible_label(&dropdown_format, &gettext("Paragraph format"));

        let font_size_labels = [
            gettext("Standardgröße"),
            gettext("10 px (Sehr klein)"),
            gettext("12 px (Klein)"),
            gettext("14 px (Normal)"),
            gettext("16 px (Mittel)"),
            gettext("18 px (Groß)"),
            gettext("24 px (Sehr groß)"),
            gettext("32 px (Riesig)"),
        ];
        let font_size_strs: Vec<&str> = font_size_labels.iter().map(|s| s.as_str()).collect();
        let dropdown_font_size = gtk::DropDown::from_strings(&font_size_strs);
        dropdown_font_size.set_valign(gtk::Align::Center);
        set_accessible_label(&dropdown_font_size, &gettext("Font size"));

        let btn_list = gtk::Button::builder()
            .icon_name("view-list-bullet-symbolic")
            .tooltip_text(&gettext("Bullet list"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_list, &gettext("Bullet list"));

        let btn_ordered_list = gtk::Button::builder()
            .icon_name("view-list-ordered-symbolic")
            .tooltip_text(&gettext("Numbered list"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_ordered_list, &gettext("Numbered list"));

        let btn_blockquote = gtk::Button::builder()
            .icon_name("format-text-direction-ltr-symbolic")
            .tooltip_text(&gettext("Block quote"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_blockquote, &gettext("Block quote"));

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
                .label(&gettext("Insert hyperlink"))
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .build(),
        );
        let entry_url = gtk::Entry::builder()
            .placeholder_text("https://example.com")
            .build();
        let entry_label = gtk::Entry::builder()
            .placeholder_text(&gettext("Link text (optional)"))
            .build();
        let btn_insert_link_confirm = gtk::Button::builder()
            .label(&gettext("Insert"))
            .css_classes(vec!["suggested-action"])
            .build();
        link_popover_box.append(&entry_url);
        link_popover_box.append(&entry_label);
        link_popover_box.append(&btn_insert_link_confirm);

        let btn_confirm_url = btn_insert_link_confirm.clone();
        entry_url.connect_activate(move |_| {
            btn_confirm_url.emit_clicked();
        });
        let btn_confirm_label = btn_insert_link_confirm.clone();
        entry_label.connect_activate(move |_| {
            btn_confirm_label.emit_clicked();
        });

        let link_popover = gtk::Popover::builder().child(&link_popover_box).build();
        let btn_link = gtk::MenuButton::builder()
            .icon_name("insert-link-symbolic")
            .tooltip_text(&gettext("Insert hyperlink"))
            .popover(&link_popover)
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_link, &gettext("Insert hyperlink"));

        let btn_rule = gtk::Button::builder()
            .icon_name("insert-object-symbolic")
            .tooltip_text(&gettext("Insert horizontal rule"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_rule, &gettext("Insert horizontal rule"));

        let btn_image = gtk::Button::builder()
            .icon_name("insert-image-symbolic")
            .tooltip_text(&gettext("Insert image"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_image, &gettext("Insert image"));

        let btn_undo = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text(&gettext("Undo (Ctrl+Z)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_undo, &gettext("Undo"));

        let btn_redo = gtk::Button::builder()
            .icon_name("edit-redo-symbolic")
            .tooltip_text(&gettext("Redo (Ctrl+Shift+Z)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_redo, &gettext("Redo"));

        toolbar.append(&dropdown_format);
        toolbar.append(&dropdown_font_size);
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
        self.imp().spell_checking_enabled.set(inline_spelling);

        let initial_languages = AppSettings::try_new()
            .map(|settings| {
                settings
                    .spellcheck_languages()
                    .split(',')
                    .map(str::trim)
                    .filter(|language| !language.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let initial_lang_refs: Vec<&str> = initial_languages.iter().map(String::as_str).collect();
        web_context.set_spell_checking_languages(&initial_lang_refs);
        self.imp().spell_checking_languages.replace(initial_languages);

        self.imp().web_context.replace(Some(web_context.clone()));

        if let Some(settings) = AppSettings::try_new() {
            let editor_weak = self.downgrade();
            let id_spelling = settings.connect_changed(Some("composer-inline-spelling"), move |s, _| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_spell_checking_enabled(s.boolean("composer-inline-spelling"));
                }
            });
            let editor_weak = self.downgrade();
            let id_languages = settings.connect_changed(Some("spellcheck-languages"), move |s, _| {
                if let Some(editor) = editor_weak.upgrade() {
                    let raw = s.string("spellcheck-languages");
                    let langs: Vec<&str> = raw
                        .split(',')
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .collect();
                    editor.set_spell_checking_languages(&langs);
                }
            });
            self.imp().spelling_handler_ids.borrow_mut().push(id_spelling);
            self.imp().spelling_handler_ids.borrow_mut().push(id_languages);
        }

        let web_view = webkit6::WebView::builder()
            .web_context(&web_context)
            .vexpand(true)
            .hexpand(true)
            .build();
        web_view.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        set_accessible_label(&web_view, &gettext("Email message body"));

        if let Some(settings) = webkit6::prelude::WebViewExt::settings(&web_view) {
            let desktop_font = crate::views::mail_viewer::detect_desktop_font_family();
            settings.set_default_font_family(&desktop_font);
            settings.set_sans_serif_font_family(&desktop_font);
            settings.set_enable_javascript(true);
            settings.set_auto_load_images(true);
            settings.set_enable_developer_extras(false);
            settings.set_allow_file_access_from_file_urls(false);
            settings.set_allow_universal_access_from_file_urls(false);
            settings.set_enable_webgl(false);
            settings.set_enable_webrtc(false);
            settings.set_enable_mediasource(false);
            settings.set_enable_media_stream(false);
            settings.set_enable_mock_capture_devices(false);
            settings.set_enable_html5_local_storage(false);
            settings.set_enable_html5_database(false);
            settings.set_enable_offline_web_application_cache(false);
        }

        let font_family_raw = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_family)
            .unwrap_or_else(|| "system-ui, sans-serif".to_string());
        let font_family = crate::views::mail_viewer::safe_css_font_family(&font_family_raw);
        let font_size = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_size)
            .unwrap_or(14);
        let initial_html = format!(
            concat!(
                "<!DOCTYPE html><html><head><style>",
                "html, body {{ font-family: {}; font-size: {}px; line-height: 1.5; color: #222; margin: 12px; background-color: transparent; }}",
                "img {{ max-width: 100%; height: auto; }}",
                "a {{ color: #1a73e8; text-decoration: underline; cursor: pointer; }}",
                "@media (prefers-color-scheme: dark) {{ html, body {{ color: #eee; background-color: transparent; }} a {{ color: #78aeed; }} }}",
                "</style>",
                "<script>",
                "document.addEventListener('mouseover', function(e) {{",
                "    const link = e.target.closest('a');",
                "    if (link && link.href && !link.title) {{",
                "        link.title = link.href + ' (Strg+Klick zum Öffnen)';",
                "    }}",
                "}});",
                "document.addEventListener('click', function(e) {{",
                "    const link = e.target.closest('a');",
                "    if (link && link.href && (e.ctrlKey || e.metaKey)) {{",
                "        e.preventDefault();",
                "        window.open(link.href, '_blank');",
                "    }}",
                "}});",
                "document.addEventListener('paste', function(e) {{",
                "    if (!e.clipboardData) return;",
                "    const types = Array.from(e.clipboardData.types || []);",
                "    if (types.includes('text/html')) return;",
                "    const items = e.clipboardData.items;",
                "    if (items) {{",
                "        for (let i = 0; i < items.length; i++) {{",
                "            if (items[i].type.indexOf('image') !== -1) {{",
                "                const file = items[i].getAsFile();",
                "                if (file) {{",
                "                    e.preventDefault();",
                "                    const reader = new FileReader();",
                "                    reader.onload = function(evt) {{",
                "                        document.execCommand('insertImage', false, evt.target.result);",
                "                    }};",
                "                    reader.readAsDataURL(file);",
                "                    return;",
                "                }}",
                "            }}",
                "        }}",
                "    }}",
                "    const text = e.clipboardData.getData('text/plain');",
                "    if (text) {{",
                "        const urlRegex = /(https?:\\/\\/[^\\s<]+|mailto:[^\\s<]+|www\\.[a-zA-Z0-9.\\-]+\\.[a-zA-Z]{{2,}}[^\\s<]*|[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{{2,}})/g;",
                "        if (urlRegex.test(text)) {{",
                "            e.preventDefault();",
                "            const escaped = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');",
                "            const htmlWithLinks = escaped.replace(urlRegex, function(match) {{",
                "                let cleanMatch = match;",
                "                let trailing = '';",
                "                while (/[.,;:)]$/.test(cleanMatch)) {{",
                "                    trailing = cleanMatch.slice(-1) + trailing;",
                "                    cleanMatch = cleanMatch.slice(0, -1);",
                "                }}",
                "                let href = cleanMatch;",
                "                if (cleanMatch.startsWith('www.')) {{",
                "                    href = 'https://' + cleanMatch;",
                "                }} else if (cleanMatch.includes('@') && !cleanMatch.startsWith('mailto:') && !cleanMatch.startsWith('http')) {{",
                "                    href = 'mailto:' + cleanMatch;",
                "                }}",
                "                return '<a href=\"' + href + '\">' + cleanMatch + '</a>' + trailing;",
                "            }}).replace(/\\r?\\n/g, '<br>');",
                "            document.execCommand('insertHTML', false, htmlWithLinks);",
                "        }}",
                "    }}",
                "}});",
                "document.addEventListener('dragover', function(e) {{ e.preventDefault(); }});",
                "document.addEventListener('drop', function(e) {{",
                "    if (!e.dataTransfer) return;",
                "    const files = e.dataTransfer.files;",
                "    if (files && files.length > 0) {{",
                "        for (let i = 0; i < files.length; i++) {{",
                "            if (files[i].type.indexOf('image') !== -1) {{",
                "                e.preventDefault();",
                "                const reader = new FileReader();",
                "                reader.onload = function(evt) {{",
                "                    document.execCommand('insertImage', false, evt.target.result);",
                "                }};",
                "                reader.readAsDataURL(files[i]);",
                "                return;",
                "            }}",
                "        }}",
                "    }}",
                "}});",
                "</script></head><body contenteditable='true'><p><br></p></body></html>"
            ),
            font_family, font_size
        );

        web_view.connect_decide_policy(|_, decision, decision_type| {
            if decision_type == webkit6::PolicyDecisionType::NavigationAction
                || decision_type == webkit6::PolicyDecisionType::NewWindowAction
            {
                if let Ok(nav) = decision
                    .clone()
                    .downcast::<webkit6::NavigationPolicyDecision>()
                {
                    if let Some(mut action) = nav.navigation_action() {
                        if let Some(req) = action.request() {
                            if let Some(uri) = req.uri() {
                                if uri.starts_with("http://")
                                    || uri.starts_with("https://")
                                    || uri.starts_with("mailto:")
                                {
                                    let _ = gio::AppInfo::launch_default_for_uri(
                                        &uri,
                                        None::<&gio::AppLaunchContext>,
                                    );
                                    decision.ignore();
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            false
        });

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

        dropdown_font_size.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let size = match dropdown.selected() {
                    1 => 10,
                    2 => 12,
                    3 => 14,
                    4 => 16,
                    5 => 18,
                    6 => 24,
                    7 => 32,
                    _ => 0,
                };
                let script = if size == 0 {
                    "document.execCommand('styleWithCSS', false, true); document.execCommand('fontSize', false, '3');".to_string()
                } else {
                    format!(
                        concat!(
                            "document.execCommand('styleWithCSS', false, true);",
                            "document.execCommand('fontSize', false, '7');",
                            "(function() {{",
                            "    var spans = document.querySelectorAll('span[style*=\"-webkit-xxx-large\"], font[size=\"7\"]');",
                            "    for (var i = 0; i < spans.length; i++) {{",
                            "        spans[i].removeAttribute('size');",
                            "        spans[i].style.fontSize = '{size}px';",
                            "    }}",
                            "}})();"
                        ),
                        size = size
                    )
                };
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

        let entry_url_weak_for_open = entry_url.downgrade();
        let entry_label_weak_for_open = entry_label.downgrade();
        let web_view_for_open = web_view.clone();
        link_popover.connect_notify_local(Some("visible"), move |popover, _| {
            if !popover.is_visible() {
                return;
            }
            let Some(entry_url) = entry_url_weak_for_open.upgrade() else {
                return;
            };
            let Some(entry_label) = entry_label_weak_for_open.upgrade() else {
                return;
            };
            let entry_u = entry_url.clone();
            let entry_l = entry_label.clone();
            web_view_for_open.evaluate_javascript(
                r#"(() => {
    let sel = window.getSelection();
    let text = sel ? sel.toString().trim() : '';
    let href = '';
    if (sel && sel.rangeCount > 0) {
        let node = sel.anchorNode;
        if (node && node.nodeType === 3) node = node.parentElement;
        let a = node ? node.closest('a') : null;
        if (a) {
            href = a.getAttribute('href') || a.href || '';
            if (!text) text = a.textContent ? a.textContent.trim() : '';
        }
    }
    return JSON.stringify({ text, href });
})()"#,
                None,
                None,
                None::<&gio::Cancellable>,
                move |result| {
                    if let Ok(val) = result {
                        let json_str = val.to_str();
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let href = parsed.get("href").and_then(|v| v.as_str()).unwrap_or("");
                            entry_u.set_text(href);
                            entry_l.set_text(text);
                            if href.is_empty() {
                                entry_u.grab_focus();
                            } else {
                                entry_l.grab_focus();
                            }
                        }
                    }
                },
            );
        });

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
            let mut url = entry_url.text().trim().to_string();
            let label = entry_label.text().trim().to_string();
            if !url.is_empty() {
                if !url.contains("://") && !url.starts_with("mailto:") && !url.starts_with('#') {
                    if url.contains('@') && !url.contains('/') {
                        url = format!("mailto:{url}");
                    } else {
                        url = format!("https://{url}");
                    }
                }

                let url_json = serde_json::to_string(&url).unwrap_or_else(|_| format!("\"{}\"", url));
                let label_json = serde_json::to_string(&label).unwrap_or_else(|_| format!("\"{}\"", label));

                let js = format!(
                    r#"(() => {{
    const url = {url_json};
    const label = {label_json};
    const sel = window.getSelection();
    let existingA = null;
    if (sel && sel.rangeCount > 0) {{
        let node = sel.anchorNode;
        if (node && node.nodeType === 3) node = node.parentElement;
        existingA = node ? node.closest('a') : null;
    }}
    if (existingA) {{
        existingA.href = url;
        if (label) existingA.textContent = label;
    }} else if (label) {{
        const a = document.createElement('a');
        a.href = url;
        a.textContent = label;
        if (sel && sel.rangeCount > 0) {{
            const range = sel.getRangeAt(0);
            range.deleteContents();
            range.insertNode(a);
            range.setStartAfter(a);
            range.setEndAfter(a);
            sel.removeAllRanges();
            sel.addRange(range);
        }} else {{
            document.body.appendChild(a);
        }}
    }} else {{
        if (sel && !sel.isCollapsed) {{
            document.execCommand('createLink', false, url);
        }} else {{
            const a = document.createElement('a');
            a.href = url;
            a.textContent = url;
            if (sel && sel.rangeCount > 0) {{
                const range = sel.getRangeAt(0);
                range.deleteContents();
                range.insertNode(a);
                range.setStartAfter(a);
                range.setEndAfter(a);
                sel.removeAllRanges();
                sel.addRange(range);
            }} else {{
                document.body.appendChild(a);
            }}
        }}
    }}
}})();"#
                );

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
                .title(&gettext("Select image to insert"))
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&gettext("Images (*.png, *.jpg, *.jpeg, *.gif, *.svg, *.webp)")));
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
