// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailViewer — Secure, sandboxed WebKitGTK 6.0 email HTML renderer.
//
// Security & Privacy Features:
// - Sandboxed ephemeral WebContext (no cookie/storage persistence)
// - JavaScript disabled (`set_enable_javascript(false)`)
// - Remote images blocked by default with interactive Privacy Banner
// - CSP security policy enforcement (`script-src 'none'`)
// - Navigation blocked (external links dispatched to system xdg-open)
// - Dark mode CSS auto-injection matching AdwStyleManager

use adw::prelude::*;
use adw::subclass::prelude::*;
#[cfg(target_os = "linux")]
use gtk::gio;
use gtk::glib;
use std::cell::RefCell;
#[cfg(target_os = "linux")]
use webkit6::prelude::*;

use crate::services::AppSettings;
use crate::services::html_sanitizer::sanitize_html;
use crate::services::mail_metadata_service::RemoteContentStore;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MailViewer {
        pub banner: adw::Banner,
        #[cfg(target_os = "linux")]
        pub webview: RefCell<Option<webkit6::WebView>>,
        #[cfg(target_os = "linux")]
        pub renderer_stack: RefCell<Option<gtk::Stack>>,
        #[cfg(not(target_os = "linux"))]
        pub fallback_view: RefCell<Option<gtk::TextView>>,
        pub current_message_id: RefCell<Option<String>>,
        pub current_html: RefCell<Option<String>>,
        pub current_base_uri: RefCell<Option<String>>,
        pub settings: RefCell<Option<AppSettings>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailViewer {
        const NAME: &'static str = "EvolutionMailViewer";
        type Type = super::MailViewer;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for MailViewer {
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

    impl WidgetImpl for MailViewer {
        fn realize(&self) {}
    }
}

glib::wrapper! {
    pub struct MailViewer(ObjectSubclass<imp::MailViewer>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MailViewer {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        tracing::debug!("MailViewer construction started");
        let box_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();

        // ── 1. Privacy Banner for Remote Images ────────────────────────────
        let banner = adw::Banner::builder()
            .title("Externe Bilder wurden aus Datenschutzgründen blockiert")
            .button_label("Bilder laden")
            .revealed(
                !AppSettings::try_new()
                    .as_ref()
                    .map(AppSettings::auto_load_remote_images)
                    .unwrap_or(false),
            )
            .build();

        banner.connect_button_clicked(glib::clone!(
            #[weak(rename_to = viewer)]
            self,
            move |b| {
                viewer.allow_remote_images();
                b.set_revealed(false);
            }
        ));

        box_container.append(&banner);

        #[cfg(target_os = "linux")]
        {
            // WebKitGTK is the production renderer on Linux.  It is kept
            // behind the target gate because WebKitGTK 6.0 is not shipped by
            // macOS package managers; the macOS fallback below still allows
            // the GTK/libadwaita shell to be exercised locally.  Creating a
            // WebKit process is deliberately deferred until a message really
            // needs HTML rendering, so a cold EDS start does not compete with
            // the first GTK frame.
            let placeholder = adw::StatusPage::builder()
                .title("Keine Nachricht ausgewählt")
                .description("Der HTML-Renderer wird beim Öffnen einer Nachricht geladen.")
                .icon_name("mail-read-symbolic")
                .hexpand(true)
                .vexpand(true)
                .build();
            let renderer_stack = gtk::Stack::builder()
                .hexpand(true)
                .vexpand(true)
                .transition_type(gtk::StackTransitionType::Crossfade)
                .build();
            renderer_stack.add_named(&placeholder, Some("placeholder"));
            renderer_stack.set_visible_child_name("placeholder");
            box_container.append(&renderer_stack);
            self.imp().renderer_stack.replace(Some(renderer_stack));
        }

        #[cfg(not(target_os = "linux"))]
        {
            let fallback_view = gtk::TextView::builder()
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::WordChar)
                .top_margin(16)
                .bottom_margin(16)
                .left_margin(16)
                .right_margin(16)
                .hexpand(true)
                .vexpand(true)
                .build();
            fallback_view
                .buffer()
                .set_text("macOS-Entwicklungsmodus: Nachrichtentext wird als Klartext angezeigt.");
            let fallback_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .child(&fallback_view)
                .hexpand(true)
                .vexpand(true)
                .build();
            box_container.append(&fallback_scroll);
            self.imp().fallback_view.replace(Some(fallback_view));
        }

        if let Some(settings) = AppSettings::try_new() {
            for key in [
                "auto-load-remote-images",
                "render-html",
                "force-dark-mode",
                "mail-font-family",
                "mail-font-size",
            ] {
                let viewer_weak = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(viewer) = viewer_weak.upgrade() {
                        viewer.reload_current();
                    }
                });
            }
            self.imp().settings.replace(Some(settings));
        }

        box_container.set_parent(self);

        // Retain the banner reference.  The target-specific render widget is
        // stored above in its respective interior-mutable field.
        unsafe {
            let imp = self.imp();
            let mut_imp = imp as *const imp::MailViewer as *mut imp::MailViewer;
            let mut_banner = &mut (*mut_imp).banner;
            *mut_banner = banner;
        }
        tracing::debug!("MailViewer construction finished");
    }

    /// Load HTML content into the WebKitGTK WebView.
    pub fn load_html(&self, html_content: &str, base_uri: Option<&str>) {
        self.load_html_for_message(None, html_content, base_uri);
    }

    /// Load HTML while applying the global and per-message remote-content
    /// policy for the selected message.
    pub fn load_html_for_message(
        &self,
        message_id: Option<&str>,
        html_content: &str,
        base_uri: Option<&str>,
    ) {
        self.imp()
            .current_message_id
            .replace(message_id.map(str::to_string));
        self.imp()
            .current_html
            .replace(Some(html_content.to_string()));
        self.imp()
            .current_base_uri
            .replace(base_uri.map(str::to_string));
        let global_remote_images = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::auto_load_remote_images)
            .unwrap_or(false);
        let per_message_remote_images = message_id
            .map(|message_id| RemoteContentStore::default().is_allowed(message_id))
            .unwrap_or(false);
        let remote_images_allowed = global_remote_images || per_message_remote_images;
        self.imp().banner.set_revealed(!remote_images_allowed);

        #[cfg(target_os = "linux")]
        if let Some(webview) = self.ensure_webkit() {
            let render_html = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::render_html)
                .unwrap_or(true);
            let sanitized = if render_html {
                sanitize_html(html_content)
            } else {
                format!("<pre>{}</pre>", escape_html(html_content))
            };
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(&webview) {
                settings.set_auto_load_images(remote_images_allowed);
            }
            let is_dark_mode = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::force_dark_mode)
                .unwrap_or(true);
            let body_css = if is_dark_mode {
                r#"
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    margin: 16px;
    padding: 0;
    color: #eeeeec;
    background-color: #1e1e1e;
  }
  a { color: #78aeed; }
"#
            } else {
                r#"
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    margin: 16px;
    padding: 0;
    color: #2e3436;
    background-color: #ffffff;
  }
  a { color: #1a73e8; }
"#
            };
            let mail_font_family = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::mail_font_family)
                .unwrap_or_else(|| "system-ui, sans-serif".to_string());
            let mail_font_size = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::mail_font_size)
                .unwrap_or(14);
            let body_css = body_css
                .replace("__FONT_FAMILY__", &mail_font_family)
                .replace("__FONT_SIZE__", &mail_font_size.to_string());

            let style_tag = format!(
                "<style>{} img {{ max-width: 100%; height: auto; }} blockquote {{ border-left: 3px solid #78aeed; margin-left: 0; padding-left: 12px; color: #888888; }}</style>",
                body_css
            );

            let is_full_doc = render_html
                && (sanitized.contains("<html")
                    || sanitized.contains("<body")
                    || sanitized.contains("<!DOCTYPE")
                    || sanitized.contains("<!doctype"));

            let styled_html = if is_full_doc {
                if let Some(pos) = sanitized.find("</head>") {
                    let (head, rest) = sanitized.split_at(pos);
                    format!("{}{}{}", head, style_tag, rest)
                } else if let Some(pos) = sanitized.find("<body") {
                    let (head, rest) = sanitized.split_at(pos);
                    format!("{}{}{}", head, style_tag, rest)
                } else {
                    format!("{}{}", style_tag, sanitized)
                }
            } else {
                format!(
                    r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="script-src 'none';">
{}
</head>
<body>
{}
</body>
</html>"#,
                    style_tag, sanitized
                )
            };

            webview.load_html(&styled_html, base_uri);
        }

        #[cfg(not(target_os = "linux"))]
        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            let text = html_to_fallback_text(html_content);
            view.buffer().set_text(&text);
        }
    }

    fn reload_current(&self) {
        let Some(html) = self.imp().current_html.borrow().clone() else {
            return;
        };
        let message_id = self.imp().current_message_id.borrow().clone();
        let base_uri = self.imp().current_base_uri.borrow().clone();
        self.load_html_for_message(message_id.as_deref(), &html, base_uri.as_deref());
    }

    /// Allow remote image loading for the current email.
    pub fn allow_remote_images(&self) {
        if let Some(message_id) = self.imp().current_message_id.borrow().as_deref() {
            if let Err(error) = RemoteContentStore::default().set_allowed(message_id, true) {
                tracing::warn!(%error, %message_id, "Could not persist remote content approval");
            }
        }
        self.imp().banner.set_revealed(false);
        #[cfg(target_os = "linux")]
        if let Some(webview) = self.imp().webview.borrow().as_ref() {
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(webview) {
                settings.set_auto_load_images(true);
                webview.reload();
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn ensure_webkit(&self) -> Option<webkit6::WebView> {
        if let Some(webview) = self.imp().webview.borrow().as_ref() {
            return Some(webview.clone());
        }

        let stack = self.imp().renderer_stack.borrow().clone()?;
        tracing::debug!("Creating deferred WebKitGTK context and view");
        let web_context = webkit6::WebContext::new();
        let settings = webkit6::Settings::new();
        settings.set_enable_javascript(false);
        settings.set_auto_load_images(
            AppSettings::try_new()
                .as_ref()
                .map(AppSettings::auto_load_remote_images)
                .unwrap_or(false),
        );
        settings.set_enable_developer_extras(false);

        let webview = webkit6::WebView::builder()
            .web_context(&web_context)
            .settings(&settings)
            .hexpand(true)
            .vexpand(true)
            .build();

        let webview_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&webview)
            .hexpand(true)
            .vexpand(true)
            .build();

        // Block external navigation links in WebView, handle via the
        // desktop's default browser.
        webview.connect_decide_policy(|_, decision, decision_type| {
            if decision_type == webkit6::PolicyDecisionType::NavigationAction {
                if let Ok(nav) = decision
                    .clone()
                    .downcast::<webkit6::NavigationPolicyDecision>()
                {
                    if let Some(mut action) = nav.navigation_action() {
                        if let Some(req) = action.request() {
                            if let Some(uri) = req.uri() {
                                if uri != "about:blank" && !uri.starts_with("data:") {
                                    tracing::info!("Opening external link in browser: {}", uri);
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

        stack.add_named(&webview_scroll, Some("webkit"));
        stack.set_visible_child_name("webkit");
        self.imp().webview.replace(Some(webview.clone()));
        tracing::debug!("Deferred WebKitGTK context and view created");
        Some(webview)
    }
}

#[cfg(target_os = "linux")]
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(not(target_os = "linux"))]
fn html_to_fallback_text(value: &str) -> String {
    let sanitized = sanitize_html(value);
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
                ) {
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
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

impl Default for MailViewer {
    fn default() -> Self {
        Self::new()
    }
}
