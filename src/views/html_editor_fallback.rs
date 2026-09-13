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

use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub text_view: RefCell<Option<gtk::TextView>>,
        pub format_toolbar: RefCell<Option<gtk::Box>>,
        pub plain_text_mode: Cell<bool>,
        pub signature_marks: RefCell<Option<(gtk::TextMark, gtk::TextMark)>>,
        pub spell_checking_enabled: Cell<bool>,
        pub spell_checking_languages: RefCell<Vec<String>>,
        pub spelling_handler_ids: RefCell<Vec<glib::SignalHandlerId>>,
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

    pub fn spell_checking_enabled(&self) -> bool {
        self.imp().spell_checking_enabled.get()
    }

    pub fn set_spell_checking_enabled(&self, enabled: bool) {
        self.imp().spell_checking_enabled.set(enabled);
    }

    pub fn spell_checking_languages(&self) -> Vec<String> {
        self.imp().spell_checking_languages.borrow().clone()
    }

    pub fn set_spell_checking_languages(&self, languages: &[&str]) {
        self.imp()
            .spell_checking_languages
            .replace(languages.iter().map(|s| s.to_string()).collect());
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
        let inline_spelling = AppSettings::try_new()
            .map(|settings| settings.composer_inline_spelling())
            .unwrap_or(true);
        self.imp().spell_checking_enabled.set(inline_spelling);

        let initial_languages = AppSettings::try_new()
            .map(|settings| {
                settings
                    .spellcheck_languages()
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.imp().spell_checking_languages.replace(initial_languages);

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

    #[test]
    fn spellcheck_reactivity_and_getters_work_in_fallback() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // Direct getter/setter
        editor.set_spell_checking_enabled(false);
        assert!(!editor.spell_checking_enabled());
        editor.set_spell_checking_enabled(true);
        assert!(editor.spell_checking_enabled());

        editor.set_spell_checking_languages(&["de-DE", "en-US"]);
        assert_eq!(editor.spell_checking_languages(), vec!["de-DE".to_string(), "en-US".to_string()]);
        editor.set_spell_checking_languages(&[]);
        assert!(editor.spell_checking_languages().is_empty());

        // Reactivity via GSettings connect_changed
        settings.set_composer_inline_spelling(false);
        let context = glib::MainContext::default();
        while context.iteration(false) {}
        assert!(!editor.spell_checking_enabled());

        settings.set_composer_inline_spelling(true);
        while context.iteration(false) {}
        assert!(editor.spell_checking_enabled());

        settings.set_spellcheck_languages("en_US,de_DE");
        while context.iteration(false) {}
        assert_eq!(editor.spell_checking_languages(), vec!["en_US".to_string(), "de_DE".to_string()]);

        settings.set_spellcheck_languages("");
        while context.iteration(false) {}
        assert!(editor.spell_checking_languages().is_empty());
    }

    #[test]
    fn stress_test_scenario_1_color_scheme_rapid_mutation_and_style_manager() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        let initial_scheme = settings.color_scheme();

        // 50 cycles alternating between valid and invalid/adversarial strings
        let test_cases = [
            ("light", adw::ColorScheme::ForceLight),
            ("dark", adw::ColorScheme::ForceDark),
            ("system", adw::ColorScheme::Default),
            ("invalid_scheme", adw::ColorScheme::Default),
            ("", adw::ColorScheme::Default),
            ("LIGHT", adw::ColorScheme::Default),
            ("DARK", adw::ColorScheme::Default),
            ("dark ", adw::ColorScheme::Default),
            (" system ", adw::ColorScheme::Default),
            ("system\0null_byte", adw::ColorScheme::Default),
            ("force-dark", adw::ColorScheme::Default),
            ("'; DROP TABLE settings;--", adw::ColorScheme::Default),
            ("🎨☀️🌙", adw::ColorScheme::Default),
            ("auto", adw::ColorScheme::Default),
            ("default", adw::ColorScheme::Default),
        ];

        let style_manager = adw::StyleManager::default();
        let context = glib::MainContext::default();

        for cycle in 0..50 {
            let (input, expected_scheme) = test_cases[cycle % test_cases.len()];

            // 1. Verify parser mapping
            let parsed = AppSettings::parse_adw_color_scheme(input);
            assert_eq!(parsed, expected_scheme, "Cycle {cycle} failed parser for input {input}");

            // 2. Directly verify AdwStyleManager rapid state transition without panic or leak
            style_manager.set_color_scheme(parsed);
            assert_eq!(style_manager.color_scheme(), parsed);

            // 3. Verify AppSettings setter persistence & sanitization
            settings.set_color_scheme(input);
            while context.iteration(false) {}

            let expected_persisted = match input {
                "light" | "dark" => input,
                _ => "system",
            };
            assert_eq!(settings.color_scheme(), expected_persisted);
            assert_eq!(settings.adw_color_scheme(), expected_scheme);
        }

        // Restore initial scheme
        settings.set_color_scheme(&initial_scheme);
        style_manager.set_color_scheme(AppSettings::parse_adw_color_scheme(&initial_scheme));
        while context.iteration(false) {}
    }

    #[test]
    fn stress_test_scenario_2_rapid_toggling_inline_spelling_multi_editor_lifecycle() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        let initial_spelling = settings.composer_inline_spelling();
        let context = glib::MainContext::default();

        // 1. Instantiate 5 active editors simultaneously
        let mut editors: Vec<HtmlEditor> = (0..5).map(|_| HtmlEditor::new()).collect();

        // Also instantiate a ComposeDialog and verify its html_editor getter
        let compose_dialog = crate::dialogs::ComposeDialog::new();
        let dialog_editor = compose_dialog.html_editor().expect("ComposeDialog must expose its HtmlEditor");

        // 2. Rapid toggling across 50 cycles with all active editors
        for cycle in 0..50 {
            let target = (cycle % 2) == 0;
            settings.set_composer_inline_spelling(target);
            while context.iteration(false) {}

            for (idx, ed) in editors.iter().enumerate() {
                assert_eq!(
                    ed.spell_checking_enabled(),
                    target,
                    "Active editor {idx} desynced on cycle {cycle}"
                );
            }
            assert_eq!(
                dialog_editor.spell_checking_enabled(),
                target,
                "ComposeDialog editor desynced on cycle {cycle}"
            );
        }

        // 3. Drop/dispose 3 of the 5 editors to create dead weak references in the GSettings listener closures
        let dead_editor_1 = editors.remove(0);
        let dead_editor_2 = editors.remove(0);
        let dead_editor_3 = editors.remove(0);

        // Explicitly trigger dispose on dead_editor_1 and dead_editor_2
        unsafe {
            dead_editor_1.run_dispose();
            dead_editor_2.run_dispose();
        }
        drop(dead_editor_1);
        drop(dead_editor_2);
        drop(dead_editor_3); // dropped without manual run_dispose - weak pointer must safely resolve to None

        // 4. Continue rapid toggling for another 50 cycles with dead weak references
        for cycle in 50..100 {
            let target = (cycle % 2) == 0;
            settings.set_composer_inline_spelling(target);
            while context.iteration(false) {}

            // Surviving editors must continue functioning without issue
            for (idx, ed) in editors.iter().enumerate() {
                assert_eq!(
                    ed.spell_checking_enabled(),
                    target,
                    "Surviving editor {idx} desynced on cycle {cycle} after sibling disposal"
                );
            }
            assert_eq!(
                dialog_editor.spell_checking_enabled(),
                target,
                "ComposeDialog editor desynced on cycle {cycle} after sibling disposal"
            );
        }

        // 5. Clean up remaining editors
        for ed in editors.drain(..) {
            unsafe {
                ed.run_dispose();
            }
        }
        unsafe {
            compose_dialog.run_dispose();
        }

        // Restore initial spelling
        settings.set_composer_inline_spelling(initial_spelling);
        while context.iteration(false) {}
    }

    #[test]
    fn stress_test_scenario_3_rapid_mutation_spellcheck_languages_and_sanitization() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        let initial_languages = settings.spellcheck_languages();
        let editor = HtmlEditor::new();
        let context = glib::MainContext::default();

        let adversarial_cases = [
            // Standard POSIX with underscores (Feature 18 fix)
            ("en_US", vec!["en_US"]),
            ("de_DE", vec!["de_DE"]),
            ("en_US,de_DE", vec!["en_US", "de_DE"]),
            ("de_DE,en_US,fr_FR", vec!["de_DE", "en_US", "fr_FR"]),
            // Hyphenated codes
            ("en-US", vec!["en-US"]),
            ("de-DE,en-US", vec!["de-DE", "en-US"]),
            // Mixed underscores and hyphens
            ("en_US,de-DE,pt_BR,es-ES", vec!["en_US", "de-DE", "pt_BR", "es-ES"]),
            // Padding and extraneous whitespace
            ("  en_US  ,  de_DE  ", vec!["en_US", "de_DE"]),
            ("   en_US   ", vec!["en_US"]),
            // Duplicate removal
            ("en_US,de_DE,en_US,de_DE", vec!["en_US", "de_DE"]),
            ("en_US,en_US,en_US", vec!["en_US"]),
            // Empty / whitespace / comma-only
            ("", vec![]),
            ("   ", vec![]),
            (",", vec![]),
            (",,,,", vec![]),
            ("  ,  ,  ", vec![]),
            // Stripping invalid punctuation and special characters
            ("en!US", vec![]),
            ("de.DE", vec![]),
            ("fr/FR", vec![]),
            ("ru#RU,en_US", vec!["en_US"]),
            ("es$ES,it%IT,de_DE", vec!["de_DE"]),
            ("en@US, de_DE , fr;FR", vec!["de_DE"]),
            // Length boundary: 35 chars is allowed, 36 is rejected
            ("abcdefghijklmnopqrstuvwxyz123456789", vec!["abcdefghijklmnopqrstuvwxyz123456789"]),
            ("abcdefghijklmnopqrstuvwxyz1234567890", vec![]),
            // Unicode / emojis rejected
            ("🇬🇧,🇩🇪", vec![]),
            ("日本語,en_US", vec!["en_US"]),
        ];

        // 50 cycles of rapid mutation
        for cycle in 0..50 {
            let (input, expected) = &adversarial_cases[cycle % adversarial_cases.len()];

            settings.set_spellcheck_languages(input);
            while context.iteration(false) {}

            let actual = editor.spell_checking_languages();
            let expected_vec: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                actual, expected_vec,
                "Cycle {cycle} failed for input {input:?}"
            );

            // Also test apply_spellcheck_settings directly
            let manual_editor = HtmlEditor::new();
            manual_editor.apply_spellcheck_settings(&settings);
            assert_eq!(
                manual_editor.spell_checking_languages(),
                expected_vec,
                "Manual apply failed on cycle {cycle} for input {input:?}"
            );
            unsafe {
                manual_editor.run_dispose();
            }
        }

        unsafe {
            editor.run_dispose();
        }

        // Restore initial languages
        settings.set_spellcheck_languages(&initial_languages);
        while context.iteration(false) {}
    }
}

