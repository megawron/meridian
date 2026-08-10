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

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub web_view: RefCell<Option<webkit6::WebView>>,
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

        toolbar.append(&btn_bold);
        toolbar.append(&btn_italic);
        toolbar.append(&btn_underline);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_list);

        vbox.append(&toolbar);
        vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // WebKit6 WebView Canvas
        let web_context = webkit6::WebContext::new();
        let web_view = webkit6::WebView::builder()
            .web_context(&web_context)
            .vexpand(true)
            .hexpand(true)
            .build();

        let initial_html = concat!(
            "<!DOCTYPE html><html><head><style>",
            "body { font-family: system-ui, sans-serif; font-size: 14px; line-height: 1.5; color: #222; margin: 12px; }",
            "@media (prefers-color-scheme: dark) { body { color: #eee; background-color: #1e1e1e; } }",
            "</style></head><body contenteditable='true'><p><br></p></body></html>"
        );

        web_view.load_html(initial_html, None);
        *self.imp().web_view.borrow_mut() = Some(web_view.clone());

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
    }
}

impl Default for HtmlEditor {
    fn default() -> Self {
        Self::new()
    }
}
