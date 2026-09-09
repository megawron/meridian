// SPDX-License-Identifier: GPL-3.0-or-later
//
// Development fallback for platforms without WebKitGTK 6.0.
//
// Linux production builds use the WebKit-backed editor in html_editor.rs.
// macOS can still exercise the complete GTK/libadwaita shell and compose
// workflow with this deliberately small text editor; it never claims to be
// the production HTML renderer.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub text_view: RefCell<Option<gtk::TextView>>,
        pub format_toolbar: RefCell<Option<gtk::Box>>,
        pub plain_text_mode: Cell<bool>,
        pub signature_marks: RefCell<Option<(gtk::TextMark, gtk::TextMark)>>,
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

    pub fn read_html<F>(&self, callback: F)
    where
        F: FnOnce(Result<String, glib::Error>) + 'static,
    {
        let Some(text_view) = self.imp().text_view.borrow().as_ref().cloned() else {
            callback(Err(editor_error("Texteditor ist nicht initialisiert")));
            return;
        };
        let text = buffer_text(&text_view);
        callback(Ok(if self.plain_text_mode() {
            text
        } else {
            text_to_basic_html(&text)
        }));
    }

    pub fn read_content<F>(&self, callback: F)
    where
        F: FnOnce(Result<EditorContent, glib::Error>) + 'static,
    {
        let Some(text_view) = self.imp().text_view.borrow().as_ref().cloned() else {
            callback(Err(editor_error("Texteditor ist nicht initialisiert")));
            return;
        };
        let text = buffer_text(&text_view);
        let plain_text = self.plain_text_mode();
        callback(Ok(EditorContent {
            body: if plain_text {
                text
            } else {
                text_to_basic_html(&text)
            },
            plain_text,
        }));
    }

    pub fn set_html(&self, html: &str) {
        self.clear_signature_marks();
        if let Some(text_view) = self.imp().text_view.borrow().as_ref() {
            text_view.buffer().set_text(&html_to_fallback_text(html));
        }
    }

    pub fn set_text(&self, text: &str) {
        self.clear_signature_marks();
        if let Some(text_view) = self.imp().text_view.borrow().as_ref() {
            text_view.buffer().set_text(text);
        }
    }

    fn clear_signature_marks(&self) {
        if let Some((start, end)) = self.imp().signature_marks.borrow_mut().take()
            && let Some(text_view) = self.imp().text_view.borrow().as_ref()
        {
            let buffer = text_view.buffer();
            buffer.delete_mark(&start);
            buffer.delete_mark(&end);
        }
    }

    /// Track the inserted signature independently from the draft. Text marks
    /// move with edits before/after it, so changing accounts preserves the body.
    pub fn replace_or_insert_signature(&self, html: &str, top_signature: bool) {
        let Some(text_view) = self.imp().text_view.borrow().as_ref().cloned() else {
            return;
        };
        let buffer = text_view.buffer();
        buffer.begin_user_action();
        if let Some((start, end)) = self.imp().signature_marks.borrow_mut().take() {
            buffer.delete(
                &mut buffer.iter_at_mark(&start),
                &mut buffer.iter_at_mark(&end),
            );
            buffer.delete_mark(&start);
            buffer.delete_mark(&end);
        }
        let signature = html_to_fallback_text(html);
        if !signature.is_empty() {
            let text = if buffer.char_count() == 0 {
                signature
            } else if top_signature {
                format!("{signature}\n")
            } else {
                format!("\n{signature}")
            };
            let mut position = if top_signature {
                buffer.start_iter()
            } else {
                buffer.end_iter()
            };
            let offset = position.offset();
            buffer.insert(&mut position, &text);
            // Exclude body text subsequently inserted exactly at the boundaries.
            let start = buffer.create_mark(None, &buffer.iter_at_offset(offset), false);
            let end = buffer.create_mark(None, &position, true);
            self.imp().signature_marks.replace(Some((start, end)));
        }
        buffer.end_user_action();
    }

    pub fn insert_html(&self, html: &str) {
        if let Some(text_view) = self.imp().text_view.borrow().as_ref() {
            text_view
                .buffer()
                .insert_at_cursor(&html_to_fallback_text(html));
        }
    }

    pub fn plain_text_mode(&self) -> bool {
        self.imp().plain_text_mode.get()
    }

    pub fn set_plain_text_mode(&self, plain_text: bool) {
        self.imp().plain_text_mode.set(plain_text);
        if let Some(toolbar) = self.imp().format_toolbar.borrow().as_ref() {
            toolbar.set_sensitive(false);
            toolbar.set_tooltip_text(Some(
                "Rich-Text-Formatierung steht im macOS-Entwicklungsfallback nicht zur Verfügung",
            ));
        }
    }

    fn build_ui(&self) {
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["linked"])
            .build();
        for (icon_name, tooltip) in [
            ("format-text-bold-symbolic", "Fett"),
            ("format-text-italic-symbolic", "Kursiv"),
            ("format-text-underline-symbolic", "Unterstrichen"),
            ("format-list-bulleted-symbolic", "Aufzählung"),
            ("format-list-numbered-symbolic", "Nummerierte Liste"),
            ("format-text-direction-ltr-symbolic", "Blockzitat"),
            ("edit-undo-symbolic", "Rückgängig"),
            ("edit-redo-symbolic", "Wiederholen"),
        ] {
            toolbar.append(
                &gtk::Button::builder()
                    .icon_name(icon_name)
                    .tooltip_text(tooltip)
                    .css_classes(vec!["flat"])
                    .sensitive(false)
                    .build(),
            );
        }
        self.imp().format_toolbar.replace(Some(toolbar.clone()));
        vbox.append(&toolbar);

        vbox.append(
            &gtk::Label::builder()
                .label("macOS-Entwicklungsmodus: einfacher Texteditor; der Linux-Build verwendet WebKitGTK.")
                .css_classes(vec!["caption", "dim-label"])
                .halign(gtk::Align::Start)
                .margin_start(8)
                .margin_end(8)
                .build(),
        );

        let text_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .vexpand(true)
            .hexpand(true)
            .build();
        text_view
            .buffer()
            .set_text("macOS-Entwicklungsmodus: Nachrichtentext hier eingeben.");
        self.imp().text_view.replace(Some(text_view.clone()));
        vbox.append(
            &gtk::ScrolledWindow::builder()
                .child(&text_view)
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .vexpand(true)
                .hexpand(true)
                .build(),
        );
        vbox.set_parent(self);
    }
}

fn buffer_text(text_view: &gtk::TextView) -> String {
    let buffer = text_view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

fn text_to_basic_html(text: &str) -> String {
    text.lines()
        .map(|line| {
            format!(
                "<p>{}</p>",
                line.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn html_to_fallback_text(value: &str) -> String {
    let sanitized = crate::services::html_sanitizer::sanitize_html(value);
    let mut output = String::with_capacity(sanitized.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for character in sanitized.chars() {
        if in_tag {
            if character == '>' {
                let normalized = tag.trim().to_ascii_lowercase();
                if matches!(
                    normalized.as_str(),
                    "br" | "/p" | "/div" | "/li" | "/blockquote" | "/h1" | "/h2" | "/h3"
                ) && !output.ends_with('\n')
                {
                    output.push('\n');
                }
                in_tag = false;
                tag.clear();
            } else {
                tag.push(character);
            }
        } else if character == '<' {
            in_tag = true;
            tag.clear();
        } else {
            output.push(character);
        }
    }
    output
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}

fn editor_error(message: &str) -> glib::Error {
    glib::Error::new(glib::FileError::Failed, message)
}

impl Default for HtmlEditor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_replacement_and_removal_preserve_edited_body() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        editor.set_text("Draft body");
        editor.replace_or_insert_signature("<p>Alice</p>", false);
        let text_view = editor.imp().text_view.borrow().as_ref().unwrap().clone();
        let buffer = text_view.buffer();
        buffer.insert(&mut buffer.start_iter(), "Edited: ");
        editor.replace_or_insert_signature("<p>Bob</p>", false);
        assert_eq!(buffer_text(&text_view), "Edited: Draft body\nBob");
        editor.replace_or_insert_signature("", false);
        assert_eq!(buffer_text(&text_view), "Edited: Draft body");
        editor.replace_or_insert_signature("<p>Alice</p>", true);
        buffer.insert(&mut buffer.end_iter(), "!");
        editor.replace_or_insert_signature("<p>Bob</p>", true);
        assert_eq!(buffer_text(&text_view), "Bob\nEdited: Draft body!");
    }
}
