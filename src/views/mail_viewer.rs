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
// - Optional dark mode CSS injection (light mail is the default)

use adw::prelude::*;
use adw::subclass::prelude::*;
#[cfg(target_os = "linux")]
use gtk::gio;
use gtk::glib;
use std::cell::{Cell, RefCell};
#[cfg(target_os = "linux")]
use webkit6::prelude::*;

use crate::services::AppSettings;
use crate::services::html_sanitizer::{block_remote_images, html_to_plain_text, sanitize_html};
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
        pub fallback_view: RefCell<Option<gtk::TextView>>,
        #[cfg(target_os = "linux")]
        pub renderer_error_label: RefCell<Option<gtk::Label>>,
        pub current_message_id: RefCell<Option<String>>,
        pub current_html: RefCell<Option<String>>,
        pub current_base_uri: RefCell<Option<String>>,
        pub current_fallback_text: RefCell<Option<String>>,
        pub remote_images_allowed_for_current: Cell<bool>,
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
        fn realize(&self) {
            self.parent_realize();
        }
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
            .title("Externe Bilder blockiert – eingebettete Bilder bleiben sichtbar")
            .button_label("Bilder laden")
            .revealed(false)
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

        let fallback_view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .css_classes(vec!["mail-plain-view"])
            .top_margin(16)
            .bottom_margin(16)
            .left_margin(16)
            .right_margin(16)
            .hexpand(true)
            .vexpand(true)
            .build();
        fallback_view
            .buffer()
            .set_text("Keine Nachricht ausgewählt.");
        let fallback_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&fallback_view)
            .hexpand(true)
            .vexpand(true)
            .build();

        #[cfg(target_os = "linux")]
        {
            // WebKitGTK is the production renderer on Linux. It is kept
            // behind the target gate because WebKitGTK 6.0 is not shipped by
            // macOS package managers; the GTK text view remains a local
            // development and load-failure fallback. Creating the WebKit
            // process is deliberately deferred until a message really needs
            // HTML rendering, so a cold EDS start does not compete with the
            // first GTK frame.
            let placeholder = adw::StatusPage::builder()
                .title("Keine Nachricht ausgewählt")
                .description("Der sichere Nachrichtendarsteller wird beim Öffnen geladen.")
                .icon_name("mail-read-symbolic")
                .hexpand(true)
                .vexpand(true)
                .build();
            let loading = adw::StatusPage::builder()
                .title("Nachricht wird geladen …")
                .description("Die sichere Darstellung wird vorbereitet.")
                .icon_name("content-loading-symbolic")
                .hexpand(true)
                .vexpand(true)
                .build();
            let error_label = gtk::Label::builder()
                .label("Die HTML-Darstellung ist fehlgeschlagen. Eine sichere Klartextansicht wird angezeigt.")
                .wrap(true)
                .xalign(0.0)
                .css_classes(vec!["error"])
                .margin_top(8)
                .margin_start(12)
                .margin_end(12)
                .margin_bottom(4)
                .visible(false)
                .build();
            let error_state = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .hexpand(true)
                .vexpand(true)
                .build();
            error_state.append(&error_label);
            error_state.append(&fallback_scroll);

            let renderer_stack = gtk::Stack::builder()
                .hexpand(true)
                .vexpand(true)
                .transition_type(gtk::StackTransitionType::Crossfade)
                .build();
            renderer_stack.add_named(&placeholder, Some("placeholder"));
            renderer_stack.add_named(&loading, Some("loading"));
            renderer_stack.add_named(&error_state, Some("error"));
            renderer_stack.set_visible_child_name("placeholder");
            box_container.append(&renderer_stack);
            self.imp().renderer_stack.replace(Some(renderer_stack));
            self.imp().renderer_error_label.replace(Some(error_label));
        }

        #[cfg(not(target_os = "linux"))]
        {
            // WebKitGTK is intentionally not emulated on macOS. The plain
            // text view keeps the GTK/libadwaita shell testable while Linux
            // remains the platform for the production HTML renderer.
            box_container.append(&fallback_scroll);
        }
        self.imp().fallback_view.replace(Some(fallback_view));

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
        self.load_html_for_message_with_fallback(message_id, html_content, base_uri, None);
    }

    /// Load a message body and keep a readable fallback for malformed or
    /// entirely blocked HTML.  Real-world mail can contain an incomplete
    /// `<style>`/`<head>` section or a provider can return only a short body
    /// preview.  In both cases a blank reader is worse than showing the
    /// available text, so the fallback is deliberately rendered as escaped
    /// plain text.
    pub fn load_html_for_message_with_fallback(
        &self,
        message_id: Option<&str>,
        html_content: &str,
        base_uri: Option<&str>,
        fallback_text: Option<&str>,
    ) {
        let message_changed = self.imp().current_message_id.borrow().as_deref() != message_id;
        if message_changed {
            self.imp().remote_images_allowed_for_current.set(false);
        }
        self.imp()
            .current_message_id
            .replace(message_id.map(str::to_string));
        self.imp()
            .current_html
            .replace(Some(html_content.to_string()));
        self.imp()
            .current_base_uri
            .replace(base_uri.map(str::to_string));
        self.imp()
            .current_fallback_text
            .replace(fallback_text.map(str::to_string));
        let settings = AppSettings::try_new();
        let global_remote_images = settings
            .as_ref()
            .map(AppSettings::auto_load_remote_images)
            .unwrap_or(false);
        let per_message_remote_images = message_id
            .map(|message_id| RemoteContentStore::default().is_allowed(message_id))
            .unwrap_or(false);
        let remote_images_allowed = global_remote_images
            || per_message_remote_images
            || self.imp().remote_images_allowed_for_current.get();
        let render_html = settings
            .as_ref()
            .map(AppSettings::render_html)
            .unwrap_or(true);
        let has_remote_images = render_html && contains_remote_image_reference(html_content);
        self.imp()
            .banner
            .set_revealed(has_remote_images && !remote_images_allowed);
        let render_plan = prepare_mail_body(
            html_content,
            fallback_text,
            render_html,
            remote_images_allowed,
        );
        self.set_fallback_text(&render_plan.visible_text);

        #[cfg(target_os = "linux")]
        {
            let Some(webview) = self.ensure_webkit() else {
                self.show_renderer_error();
                return;
            };
            self.show_renderer_loading();
            tracing::debug!(
                message_id = ?message_id,
                html_bytes = html_content.len(),
                sanitized_bytes = render_plan.body.len(),
                fallback_used = render_plan.used_fallback,
                render_html,
                "Rendering selected mail content"
            );
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(&webview) {
                // The document policy below blocks network image schemes
                // deterministically. Keeping WebKit's generic image loading
                // enabled is necessary for inline `data:`, `cid:` and `blob:`
                // content to render while remote images remain disallowed.
                settings.set_auto_load_images(true);
            }
            let is_dark_mode = settings
                .as_ref()
                .map(AppSettings::force_dark_mode)
                .unwrap_or(false);
            let mail_font_family = settings
                .as_ref()
                .map(AppSettings::mail_font_family)
                .unwrap_or_else(|| "system-ui, sans-serif".to_string());
            let mail_font_size = settings
                .as_ref()
                .map(AppSettings::mail_font_size)
                .unwrap_or(14);
            let styled_html = compose_sanitized_mail_document(
                &render_plan.body,
                render_html,
                remote_images_allowed,
                is_dark_mode,
                &mail_font_family,
                mail_font_size,
            );

            let effective_base_uri = effective_mail_base_uri(base_uri, remote_images_allowed);
            webview.load_html(&styled_html, effective_base_uri);
        }

        #[cfg(not(target_os = "linux"))]
        {
            // The non-Linux fallback is intentionally plain text. It still
            // uses the exact same sanitized/fallback decision as Linux so
            // local development cannot hide empty-message or unsafe-content
            // bugs behind a platform-specific code path.
            if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
                view.buffer().set_text(&render_plan.visible_text);
            }
        }
    }

    fn reload_current(&self) {
        let Some(html) = self.imp().current_html.borrow().clone() else {
            return;
        };
        let message_id = self.imp().current_message_id.borrow().clone();
        let base_uri = self.imp().current_base_uri.borrow().clone();
        let fallback_text = self.imp().current_fallback_text.borrow().clone();
        self.load_html_for_message_with_fallback(
            message_id.as_deref(),
            &html,
            base_uri.as_deref(),
            fallback_text.as_deref(),
        );
    }

    /// Allow remote image loading for the current email.
    pub fn allow_remote_images(&self) {
        if let Some(message_id) = self.imp().current_message_id.borrow().as_deref() {
            if let Err(error) = RemoteContentStore::default().set_allowed(message_id, true) {
                tracing::warn!(%error, %message_id, "Could not persist remote content approval");
            }
        }
        self.imp().remote_images_allowed_for_current.set(true);
        self.imp().banner.set_revealed(false);
        // Re-run the rendering path from the retained original HTML.  A
        // WebKit reload alone cannot restore remote <img> tags that were
        // deliberately removed from the privacy-first document.
        self.reload_current();
    }

    fn set_fallback_text(&self, text: &str) {
        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            view.buffer().set_text(text);
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
        // Remote sources are controlled by the sanitized document and CSP,
        // not by WebKit's global image switch. The latter must stay enabled
        // so trusted inline `data:`, `cid:` and `blob:` images remain visible.
        settings.set_auto_load_images(true);
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

        let viewer_weak = self.downgrade();
        webview.connect_load_changed(move |_, event| {
            let Some(viewer) = viewer_weak.upgrade() else {
                return;
            };
            match event {
                webkit6::LoadEvent::Started | webkit6::LoadEvent::Redirected => {
                    viewer.show_renderer_loading();
                }
                webkit6::LoadEvent::Finished => viewer.show_renderer_ready(),
                _ => {}
            }
        });

        let viewer_weak = self.downgrade();
        webview.connect_load_failed(move |_, _, failing_uri, error| {
            tracing::warn!(uri = %redact_renderer_uri(failing_uri), %error, "Mail viewer failed to load document");
            if let Some(viewer) = viewer_weak.upgrade() {
                viewer.show_renderer_error();
            }
            true
        });

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
        stack.set_visible_child_name("loading");
        self.imp().webview.replace(Some(webview.clone()));
        tracing::debug!("Deferred WebKitGTK context and view created");
        Some(webview)
    }

    #[cfg(target_os = "linux")]
    fn show_renderer_loading(&self) {
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(false);
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("loading");
        }
    }

    #[cfg(target_os = "linux")]
    fn show_renderer_ready(&self) {
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(false);
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("webkit");
        }
    }

    #[cfg(target_os = "linux")]
    fn show_renderer_error(&self) {
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(true);
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("error");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedMailBody {
    body: String,
    visible_text: String,
    used_fallback: bool,
}

/// Apply the same sanitizer, remote-image policy and empty-body fallback on
/// every platform. Keeping this as a pure composition step makes it possible
/// to verify the privacy boundary without constructing GTK or WebKit objects.
fn prepare_mail_body(
    html_content: &str,
    fallback_text: Option<&str>,
    render_html: bool,
    remote_images_allowed: bool,
) -> PreparedMailBody {
    if !render_html {
        let visible_text = html_to_fallback_text(html_content);
        let visible_text = if visible_text.is_empty() {
            readable_fallback_text(fallback_text)
        } else {
            visible_text
        };
        return PreparedMailBody {
            body: format!("<pre>{}</pre>", escape_html(&visible_text)),
            visible_text,
            used_fallback: html_to_fallback_text(html_content).is_empty(),
        };
    }

    let sanitized = sanitize_html(html_content);
    let sanitized = if remote_images_allowed {
        sanitized
    } else {
        block_remote_images(&sanitized)
    };
    let visible_text = html_to_fallback_text(&sanitized);
    let has_renderable_content = !visible_text.is_empty() || contains_renderable_image(&sanitized);

    if !has_renderable_content {
        let visible_text = readable_fallback_text(fallback_text);
        return PreparedMailBody {
            body: format!("<pre>{}</pre>", escape_html(&visible_text)),
            visible_text,
            used_fallback: true,
        };
    }

    PreparedMailBody {
        body: sanitized,
        visible_text: if visible_text.is_empty() {
            readable_fallback_text(fallback_text)
        } else {
            visible_text
        },
        used_fallback: false,
    }
}

fn effective_mail_base_uri(base_uri: Option<&str>, remote_images_allowed: bool) -> Option<&str> {
    let base_uri = base_uri.map(str::trim).filter(|uri| !uri.is_empty())?;
    if !remote_images_allowed {
        return None;
    }

    let lower = base_uri.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then_some(base_uri)
}

fn safe_css_font_family(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 120
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || " ,-_'\"".contains(character)))
    {
        "system-ui, sans-serif".to_string()
    } else {
        value.to_string()
    }
}

fn safe_mail_font_size(value: u32) -> u32 {
    value.clamp(8, 48)
}

#[cfg(target_os = "linux")]
fn redact_renderer_uri(value: &str) -> &'static str {
    if value.eq_ignore_ascii_case("about:blank") {
        "about:blank"
    } else {
        "redacted resource"
    }
}

fn compose_sanitized_mail_document(
    sanitized: &str,
    render_html: bool,
    remote_images_allowed: bool,
    force_dark_mode: bool,
    mail_font_family: &str,
    mail_font_size: u32,
) -> String {
    let mail_font_family = safe_css_font_family(mail_font_family);
    let mail_font_size = safe_mail_font_size(mail_font_size);
    let body_css = if force_dark_mode {
        r#"
  html {
    color-scheme: dark;
    background-color: #1e1e1e;
    color: #eeeeec;
  }
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    margin: 16px;
    min-height: calc(100vh - 32px);
    padding: 0;
    color: inherit;
    background-color: transparent;
  }
"#
    } else {
        r#"
  html {
    color-scheme: light;
    background-color: #ffffff;
    color: #2e3436;
  }
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    margin: 16px;
    min-height: calc(100vh - 32px);
    padding: 0;
    color: inherit;
    background-color: transparent;
  }
"#
    };
    let body_css = body_css
        .replace("__FONT_FAMILY__", &mail_font_family)
        .replace("__FONT_SIZE__", &mail_font_size.to_string());
    let style_tag = format!(
        "<style>{} img {{ max-width: 100%; height: auto; }} body > table, body > div, body > center, body > article, body > main {{ max-width: 100%; }}{}</style>",
        body_css,
        if remote_images_allowed {
            ""
        } else {
            " img[src^=\"http://\"], img[src^=\"https://\"], img[src^=\"//\"], img[xlink\\:href^=\"http://\"], img[xlink\\:href^=\"https://\"], img[xlink\\:href^=\"//\"] { display: none !important; }"
        }
    );

    // WebKit's image preference is the first line of defense. Keep the
    // policy in the document as well, so a provider cannot make remote
    // images appear through a second URL-bearing element or cached
    // navigation. Inline `cid:`/`data:` images remain usable before the
    // user explicitly allows remote content.
    let image_sources = if remote_images_allowed {
        "http: https: data: cid: blob:"
    } else {
        "data: cid: blob:"
    };
    let security_tag = format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; base-uri 'none'; form-action 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src {image_sources}; font-src data:; connect-src 'none'; media-src 'none'; frame-src 'none'; object-src 'none'; worker-src 'none'\">"
    );

    let lower = sanitized.to_ascii_lowercase();
    let has_html_root = render_html && lower.contains("<html");

    if has_html_root {
        // Put the application baseline before the mail's own <head> rules.
        // This keeps the configured color scheme and safe typography
        // available without overriding explicit colors, backgrounds, link
        // styles, or table layout from the message.
        if let Some(head_start) = lower.find("<head") {
            if let Some(head_end_offset) = lower[head_start..].find('>') {
                let insertion = head_start + head_end_offset + 1;
                let (head, rest) = sanitized.split_at(insertion);
                return format!("{}{}{}{}", head, security_tag, style_tag, rest);
            }
        }
        if let Some(html_start) = lower.find("<html") {
            if let Some(html_end_offset) = lower[html_start..].find('>') {
                let insertion = html_start + html_end_offset + 1;
                let (root, rest) = sanitized.split_at(insertion);
                return format!("{}<head>{}{}</head>{}", root, security_tag, style_tag, rest);
            }
        }
        format!("{}{}{}", security_tag, style_tag, sanitized)
    } else if render_html && lower.contains("<body") {
        format!(
            r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
{}
{}
</head>
{}
</html>"#,
            security_tag, style_tag, sanitized
        )
    } else {
        format!(
            r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
{}
{}
</head>
<body>
{}
</body>
</html>"#,
            security_tag, style_tag, sanitized
        )
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn html_to_fallback_text(value: &str) -> String {
    html_to_plain_text(value)
}

fn readable_fallback_text(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("(Die Nachricht enthält keinen darstellbaren Inhalt.)")
        .to_string()
}

/// Return whether a mail body contains an external image source that is
/// subject to the remote-content decision.  Inline `cid:` and `data:` images
/// intentionally do not trigger the privacy banner.
fn contains_remote_image_reference(value: &str) -> bool {
    let sanitized = sanitize_html(value);
    block_remote_images(&sanitized) != sanitized
        || any_image_tag(&sanitized, |tag| {
            image_attribute_value(tag, "xlink:href").is_some_and(is_remote_uri_reference)
        })
}

fn contains_renderable_image(value: &str) -> bool {
    any_image_tag(value, |tag| {
        ["src", "xlink:href"].iter().any(|attribute| {
            image_attribute_value(tag, attribute).is_some_and(|value| !value.trim().is_empty())
        })
    })
}

fn any_image_tag(value: &str, mut predicate: impl FnMut(&str) -> bool) -> bool {
    let lower = value.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find("<img") {
        let start = cursor + relative_start;
        let after_name = start + 4;
        if lower
            .get(after_name..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|character| character.is_ascii_alphanumeric())
        {
            cursor = after_name;
            continue;
        }
        let end = lower[after_name..]
            .find('>')
            .map(|offset| after_name + offset)
            .unwrap_or(lower.len());
        if predicate(&value[after_name..end]) {
            return true;
        }
        if end >= lower.len() {
            break;
        }
        cursor = end + 1;
    }
    false
}

fn image_attribute_value<'a>(tag: &'a str, attribute: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find(attribute) {
        let start = cursor + relative_start;
        let before_is_name = start == 0
            || !lower[..start].chars().next_back().is_some_and(|character| {
                character.is_ascii_alphanumeric() || "_:-".contains(character)
            });
        let after_attribute = start + attribute.len();
        let after_is_name = lower
            .get(after_attribute..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|character| {
                !(character.is_ascii_alphanumeric() || "_:-".contains(character))
            });
        if !before_is_name || !after_is_name {
            cursor = after_attribute;
            continue;
        }

        let mut value_start = after_attribute;
        while lower
            .get(value_start..)
            .and_then(|rest| rest.as_bytes().first())
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        if lower.as_bytes().get(value_start) != Some(&b'=') {
            cursor = after_attribute;
            continue;
        }
        value_start += 1;
        while lower
            .get(value_start..)
            .and_then(|rest| rest.as_bytes().first())
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        let Some(first) = lower.as_bytes().get(value_start).copied() else {
            return None;
        };
        if first == b'"' || first == b'\'' {
            let value_start = value_start + 1;
            let end = lower[value_start..]
                .find(first as char)
                .map(|offset| value_start + offset)
                .unwrap_or(lower.len());
            return Some(&tag[value_start..end]);
        }
        let end = lower[value_start..]
            .find(|character: char| character.is_ascii_whitespace())
            .map(|offset| value_start + offset)
            .unwrap_or(lower.len());
        return Some(&tag[value_start..end]);
    }
    None
}

fn is_remote_uri_reference(value: &str) -> bool {
    let normalized = normalize_uri_prefix(value);
    normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.starts_with("//")
}

fn normalize_uri_prefix(value: &str) -> String {
    let value = decode_html_attribute_entities(value);
    let bytes = value.as_bytes();
    let mut normalized = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < bytes.len() && normalized.len() < 128 {
        let byte = bytes[cursor];
        if byte.is_ascii_whitespace() || byte.is_ascii_control() {
            cursor += 1;
            continue;
        }
        if byte == b'%' && cursor + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[cursor + 1]), hex_digit(bytes[cursor + 2]))
            {
                normalized.push((high * 16 + low) as char);
                cursor += 3;
                continue;
            }
        }
        normalized.push(byte.to_ascii_lowercase() as char);
        cursor += 1;
    }
    normalized
}

fn decode_html_attribute_entities(value: &str) -> String {
    let mut decoded = value.to_string();
    for _ in 0..2 {
        let mut output = String::with_capacity(decoded.len());
        let mut cursor = 0;
        while cursor < decoded.len() {
            let Some(relative_start) = decoded[cursor..].find('&') else {
                output.push_str(&decoded[cursor..]);
                break;
            };
            let start = cursor + relative_start;
            output.push_str(&decoded[cursor..start]);
            let Some(relative_end) = decoded[start + 1..].find(';') else {
                output.push_str(&decoded[start..]);
                break;
            };
            let end = start + 1 + relative_end;
            let entity = &decoded[start + 1..end];
            let replacement = match entity.to_ascii_lowercase().as_str() {
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "lt" => Some('<'),
                "gt" => Some('>'),
                _ if entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .is_some() =>
                {
                    entity[2..]
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..]
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32),
                _ => None,
            };
            if let Some(replacement) = replacement {
                output.push(replacement);
            } else {
                output.push_str(&decoded[start..=end]);
            }
            cursor = end + 1;
        }
        decoded = output;
    }
    decoded
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compose_sanitized_mail_document, contains_remote_image_reference, effective_mail_base_uri,
        html_to_fallback_text, prepare_mail_body, readable_fallback_text, safe_css_font_family,
        safe_mail_font_size,
    };

    #[test]
    fn fallback_text_preserves_mail_content_and_line_breaks() {
        assert_eq!(
            html_to_fallback_text("<div>Hello<br>world</div><p>Next</p>"),
            "Hello\nworld\nNext"
        );
    }

    #[test]
    fn blocked_only_markup_has_no_visible_fallback_text() {
        assert!(
            html_to_fallback_text("<style>body{display:none}</style><meta charset=utf-8>")
                .is_empty()
        );
    }

    #[test]
    fn fallback_text_is_stable_when_settings_reload_the_reader() {
        assert_eq!(
            readable_fallback_text(Some("  Vorschau aus dem Nachrichtentext  ")),
            "Vorschau aus dem Nachrichtentext"
        );
        assert_eq!(
            readable_fallback_text(Some("   ")),
            "(Die Nachricht enthält keinen darstellbaren Inhalt.)"
        );
    }

    #[test]
    fn remote_image_banner_only_matches_external_image_sources() {
        assert!(contains_remote_image_reference(
            r#"<p><img src="https://cdn.example/image.png"></p>"#
        ));
        assert!(contains_remote_image_reference(
            r#"<img src='//cdn.example/image.png'>"#
        ));
        assert!(!contains_remote_image_reference(
            r#"<img src="cid:logo@example.org"><img src="data:image/png;base64,AAAA">"#
        ));
        assert!(!contains_remote_image_reference(
            r#"<imgdata src="https://cdn.example/not-an-img-tag.png">"#
        ));
        assert!(contains_remote_image_reference(
            r#"<img xlink:href="https%3A%2F%2Fcdn.example/logo.png">"#
        ));
    }

    #[test]
    fn render_plan_blocks_remote_images_but_keeps_inline_content() {
        let plan = prepare_mail_body(
            r#"<p>Hallo</p><img src="https://cdn.example/remote.png"><img src="cid:logo@example.org"><img src="data:image/png;base64,AAAA">"#,
            Some("Vorschau"),
            true,
            false,
        );

        assert!(!plan.used_fallback);
        assert_eq!(plan.visible_text, "Hallo");
        assert!(!plan.body.contains("cdn.example"));
        assert!(plan.body.contains("cid:logo@example.org"));
        assert!(plan.body.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn render_plan_uses_preview_when_html_has_no_renderable_content() {
        let plan = prepare_mail_body(
            r#"<style>.tracking { display: none; }</style><img src="https://cdn.example/only.png">"#,
            Some("  Lesbare Vorschau  "),
            true,
            false,
        );

        assert!(plan.used_fallback);
        assert_eq!(plan.visible_text, "Lesbare Vorschau");
        assert!(plan.body.contains("Lesbare Vorschau"));
        assert!(!plan.body.contains("cdn.example"));
    }

    #[test]
    fn remote_policy_drops_untrusted_base_uris() {
        assert_eq!(
            effective_mail_base_uri(Some("https://mail.example/message/1"), false),
            None
        );
        assert_eq!(
            effective_mail_base_uri(Some("file:///Users/private/message"), true),
            None
        );
        assert_eq!(
            effective_mail_base_uri(Some("https://mail.example/message/1"), true),
            Some("https://mail.example/message/1")
        );
    }

    #[test]
    fn viewer_composition_keeps_inline_sources_and_stable_message_backgrounds() {
        let document = compose_sanitized_mail_document(
            r#"<p style="background-color:#fedcba">Hallo</p><img src="cid:logo@example.org">"#,
            true,
            false,
            false,
            "system-ui, sans-serif",
            14,
        );

        assert!(document.contains("img-src data: cid: blob:"));
        assert!(!document.contains("img-src http:"));
        assert!(document.contains("background-color:#fedcba"));
        assert!(document.contains("src=\"cid:logo@example.org\""));
    }

    #[test]
    fn viewer_font_inputs_have_safe_bounds() {
        assert_eq!(
            safe_css_font_family("system-ui, sans-serif"),
            "system-ui, sans-serif"
        );
        assert_eq!(
            safe_css_font_family("</style><script>bad</script>"),
            "system-ui, sans-serif"
        );
        assert_eq!(safe_mail_font_size(1), 8);
        assert_eq!(safe_mail_font_size(100), 48);
    }

    #[test]
    fn mail_baseline_precedes_message_css_without_overriding_mail_colors() {
        let document = compose_sanitized_mail_document(
            r#"<html><head><style>body { background-color: #123456; color: #abcdef; }</style></head><body style="background-color:#fedcba"><p>Hallo</p></body></html>"#,
            true,
            false,
            false,
            "system-ui, sans-serif",
            14,
        );
        let baseline = document
            .find("color-scheme: light")
            .expect("the viewer baseline should be present");
        let message_css = document
            .find("background-color: #123456")
            .expect("the message CSS should be retained");
        assert!(baseline < message_css);
        assert!(document.contains("style=\"background-color:#fedcba\""));
        assert!(
            document.contains("img[src^=\"http://\"], img[src^=\"https://\"], img[src^=\"//\"]")
        );
    }
}

impl Default for MailViewer {
    fn default() -> Self {
        Self::new()
    }
}
