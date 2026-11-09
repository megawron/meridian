// SPDX-License-Identifier: GPL-3.0-or-later
//
// RichTextField — Reusable rich-text and HTML editor widget wrapping HtmlEditor.
//
// Features:
// - Compact formatting toolbar (Bold, Italic, Underline, Link, Lists, Image)
// - Optional HTML / Plain Text mode toggle
// - Libadwaita .card canvas wrapping HtmlEditor
// - Synchronous accessors for HTML and plain-text representation
// - Reactive change notification

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::gettext;
use crate::views::HtmlEditor;

/// Escape characters for safe embedding inside HTML tags.
pub fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Convert plain text into HTML paragraphs, escaping entities and converting
/// newlines into `<p>` elements.
pub fn plain_text_to_paragraphs(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "<p><br></p>".to_string();
    }
    let mut html = String::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            html.push_str("<p><br></p>");
        } else {
            html.push_str("<p>");
            html.push_str(&escape_html(line));
            html.push_str("</p>");
        }
    }
    if html.is_empty() {
        "<p><br></p>".to_string()
    } else {
        html
    }
}

/// Helper function to detect whether text contains HTML markup.
pub fn is_html_content(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("<html")
        || lower.contains("<body")
        || lower.contains("<p")
        || lower.contains("<div")
        || lower.contains("<br")
        || lower.contains("<span")
        || lower.contains("<table")
        || lower.contains("<b")
        || lower.contains("<i")
        || lower.contains("<u")
        || lower.contains("<img")
        || lower.contains("<a ")
        || lower.contains("<a>")
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RichTextField {
        pub editor: RefCell<Option<HtmlEditor>>,
        pub btn_format: RefCell<Option<gtk::ToggleButton>>,
        pub html_content: RefCell<String>,
        pub plain_text_content: RefCell<String>,
        pub is_html: Cell<bool>,
        pub show_mode_toggle: Cell<bool>,
        pub min_height: Cell<i32>,
        pub change_callbacks: RefCell<Vec<Rc<dyn Fn(&super::RichTextField) + 'static>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RichTextField {
        const NAME: &'static str = "MeridianRichTextField";
        type Type = super::RichTextField;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for RichTextField {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_spacing(6);
        }
    }

    impl WidgetImpl for RichTextField {}
    impl BoxImpl for RichTextField {}
}

glib::wrapper! {
    pub struct RichTextField(ObjectSubclass<imp::RichTextField>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RichTextField {
    pub fn new(show_mode_toggle: bool, min_height: i32) -> Self {
        let obj: Self = glib::Object::builder().build();
        obj.init_field(show_mode_toggle, min_height);
        obj
    }

    fn init_field(&self, show_mode_toggle: bool, min_height: i32) {
        self.imp().show_mode_toggle.set(show_mode_toggle);
        self.imp().min_height.set(min_height);
        self.imp().is_html.set(true);

        let editor = HtmlEditor::new();
        editor.set_vexpand(true);
        editor.set_hexpand(true);
        editor.set_canvas_card_mode(true);

        if min_height > 0 {
            editor.set_height_request(min_height);
            editor.set_canvas_min_height(min_height);
        }

        editor.set_html_mode(true);
        editor.set_format_toolbar_visible(true);

        let btn_format = if show_mode_toggle {
            editor.format_toggle_button().set_visible(true);
            editor.mode_switcher().set_visible(true);

            let btn = gtk::ToggleButton::builder()
                .label("HTML")
                .icon_name("format-text-bold-symbolic")
                .tooltip_text(gettext("Toggle formatting toolbar"))
                .active(true)
                .sensitive(true)
                .css_classes(["flat"])
                .build();

            let sep = gtk::Separator::new(gtk::Orientation::Vertical);
            editor.prepend_top_bar_widget(&sep);
            editor.prepend_top_bar_widget(&btn);

            btn.connect_toggled(glib::clone!(
                #[weak]
                editor,
                move |b| {
                    editor.set_format_toolbar_visible(b.is_active());
                }
            ));

            Some(btn)
        } else {
            editor.format_toggle_button().set_visible(false);
            editor.mode_switcher().set_visible(false);
            None
        };

        // Connect mode change
        let self_weak = self.downgrade();
        let btn_format_weak = btn_format.as_ref().map(|b| b.downgrade());
        editor.connect_plain_text_mode_changed(move |is_plain| {
            let is_html = !is_plain;
            if let Some(field) = self_weak.upgrade() {
                field.imp().is_html.set(is_html);
                if let Some(ref btn_weak) = btn_format_weak {
                    if let Some(btn) = btn_weak.upgrade() {
                        btn.set_sensitive(is_html);
                        if !is_html {
                            btn.set_active(false);
                        } else if !btn.is_active() {
                            btn.set_active(true);
                        }
                    }
                }
                field.notify_changed();
            }
        });

        // Connect content changed from WebKit
        let self_weak2 = self.downgrade();
        editor.connect_content_changed(move |html, text| {
            if let Some(field) = self_weak2.upgrade() {
                *field.imp().html_content.borrow_mut() = html.to_string();
                *field.imp().plain_text_content.borrow_mut() = text.to_string();
                field.notify_changed();
            }
        });

        self.append(&editor);
        self.imp().editor.replace(Some(editor));
        self.imp().btn_format.replace(btn_format);
    }

    /// Access the underlying HtmlEditor instance.
    pub fn editor(&self) -> HtmlEditor {
        self.imp()
            .editor
            .borrow()
            .as_ref()
            .cloned()
            .expect("editor must be initialized")
    }

    /// Access the formatting toggle button if show_mode_toggle is true.
    pub fn format_button(&self) -> Option<gtk::ToggleButton> {
        self.imp().btn_format.borrow().clone()
    }

    /// Sets the HTML content of the editor.
    pub fn set_html(&self, html: &str) {
        *self.imp().html_content.borrow_mut() = html.to_string();
        *self.imp().plain_text_content.borrow_mut() =
            crate::services::html_sanitizer::html_to_plain_text(html);
        if let Some(editor) = self.imp().editor.borrow().as_ref() {
            editor.set_html(html);
        }
        self.notify_changed();
    }

    /// Returns the current HTML string.
    pub fn html(&self) -> String {
        let content = self.imp().html_content.borrow().clone();
        if content.trim().is_empty() {
            if self.is_html() {
                "<p><br></p>".to_string()
            } else {
                String::new()
            }
        } else {
            content
        }
    }

    /// Escapes plain text, wraps lines into HTML paragraphs, and updates the editor.
    pub fn set_plain_text(&self, text: &str) {
        let html = plain_text_to_paragraphs(text);
        *self.imp().plain_text_content.borrow_mut() = text.to_string();
        *self.imp().html_content.borrow_mut() = html.clone();
        if let Some(editor) = self.imp().editor.borrow().as_ref() {
            editor.set_html(&html);
        }
        self.notify_changed();
    }

    /// Returns the current plain text string.
    pub fn plain_text(&self) -> String {
        self.imp().plain_text_content.borrow().clone()
    }

    /// Returns whether the editor is in HTML mode.
    pub fn is_html(&self) -> bool {
        self.imp().is_html.get()
    }

    /// Switches between HTML and Plain Text mode.
    pub fn set_is_html(&self, is_html: bool) {
        if self.imp().is_html.get() == is_html {
            return;
        }
        self.imp().is_html.set(is_html);
        if let Some(editor) = self.imp().editor.borrow().as_ref() {
            editor.set_plain_text_mode(!is_html);
            editor.set_format_toolbar_visible(is_html);
        }
        if let Some(btn) = self.imp().btn_format.borrow().as_ref() {
            btn.set_sensitive(is_html);
            btn.set_active(is_html);
        }
        self.notify_changed();
    }

    /// Sets whether the rich text field is in flush layout mode (no card background,
    /// no extra spacing, edge-to-edge canvas, and non-scrollable toolbar).
    pub fn set_flush(&self, flush: bool) {
        let editor = self.editor();
        editor.set_canvas_card_mode(!flush);
        editor.set_toolbar_scrollable(!flush);
        if flush {
            self.set_spacing(0);
        } else {
            self.set_spacing(6);
        }
    }

    /// Returns whether the rich text field is in flush layout mode.
    pub fn is_flush(&self) -> bool {
        !self.editor().canvas_card_mode()
    }

    /// Sets whether the formatting toolbar is wrapped in a horizontal ScrolledWindow.
    pub fn set_toolbar_scrollable(&self, scrollable: bool) {
        self.editor().set_toolbar_scrollable(scrollable);
    }

    /// Returns whether the formatting toolbar is wrapped in a horizontal ScrolledWindow.
    pub fn is_toolbar_scrollable(&self) -> bool {
        self.editor().is_toolbar_scrollable()
    }

    /// Connects a listener callback to content or mode changes.
    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) {
        self.imp().change_callbacks.borrow_mut().push(Rc::new(f));
    }

    fn notify_changed(&self) {
        let callbacks = self.imp().change_callbacks.borrow().clone();
        for cb in callbacks {
            cb(self);
        }
    }
}

impl Default for RichTextField {
    fn default() -> Self {
        Self::new(false, -1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_text_to_paragraphs_escaping() {
        assert_eq!(plain_text_to_paragraphs(""), "<p><br></p>");
        assert_eq!(
            plain_text_to_paragraphs("Hello World"),
            "<p>Hello World</p>"
        );
        assert_eq!(
            plain_text_to_paragraphs("Tom & Jerry <friends>"),
            "<p>Tom &amp; Jerry &lt;friends&gt;</p>"
        );
        assert_eq!(
            plain_text_to_paragraphs("Line 1\nLine 2"),
            "<p>Line 1</p><p>Line 2</p>"
        );
        assert_eq!(
            plain_text_to_paragraphs("Line 1\r\nLine 2"),
            "<p>Line 1</p><p>Line 2</p>"
        );
        assert_eq!(
            plain_text_to_paragraphs("Line 1\n\nLine 2"),
            "<p>Line 1</p><p><br></p><p>Line 2</p>"
        );
    }

    #[test]
    fn test_is_html_content_detection() {
        assert!(!is_html_content(""));
        assert!(!is_html_content("Just a plain text sentence."));
        assert!(!is_html_content("Two lines\nof plain text."));
        assert!(is_html_content("<p>Paragraph</p>"));
        assert!(is_html_content("<div>Div block</div>"));
        assert!(is_html_content("Hello<br>world"));
        assert!(is_html_content("<b>Bold text</b>"));
        assert!(is_html_content("<img src=\"test.png\">"));
        assert!(is_html_content("<a href=\"https://example.com\">Link</a>"));
    }

    #[test]
    fn test_rich_text_field_instantiation_and_accessors() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let field = RichTextField::new(true, 180);
        assert!(field.is_html());
        assert!(field.format_button().is_some());
        assert!(field.editor().canvas_card_mode());

        field.set_plain_text("Hello\nWorld");
        assert_eq!(field.plain_text(), "Hello\nWorld");
        assert_eq!(field.html(), "<p>Hello</p><p>World</p>");

        field.set_html("<p><b>Rich</b> text</p>");
        assert_eq!(field.html(), "<p><b>Rich</b> text</p>");

        let changed_called = std::rc::Rc::new(std::cell::Cell::new(false));
        let changed_clone = changed_called.clone();
        field.connect_changed(move |_| {
            changed_clone.set(true);
        });

        field.set_is_html(false);
        assert!(!field.is_html());
        assert!(changed_called.get());
    }

    #[test]
    fn test_rich_text_field_without_mode_toggle() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let field = RichTextField::new(false, 140);
        assert!(field.is_html());
        assert!(field.format_button().is_none());
        assert!(!field.editor().mode_switcher().is_visible());
    }

    #[test]
    fn test_rich_text_field_flush_mode() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let field = RichTextField::new(false, 150);
        assert!(!field.is_flush());
        assert!(field.is_toolbar_scrollable());

        field.set_flush(true);
        assert!(field.is_flush());
        assert!(!field.is_toolbar_scrollable());
        assert!(!field.editor().canvas_card_mode());

        field.set_flush(false);
        assert!(!field.is_flush());
        assert!(field.is_toolbar_scrollable());
        assert!(field.editor().canvas_card_mode());
    }
}
