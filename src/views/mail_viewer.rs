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
use gtk::{gio, glib};
use std::cell::RefCell;
use webkit6::prelude::*;

use crate::services::AppSettings;
use crate::services::html_sanitizer::sanitize_html;
use crate::services::mail_metadata_service::RemoteContentStore;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MailViewer {
        pub banner: adw::Banner,
        pub webview: Option<webkit6::WebView>,
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

    impl WidgetImpl for MailViewer {}
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

        // ── 2. WebKitGTK 6.0 WebView ───────────────────────────────────────
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

        // Block external navigation links in WebView, handle via xdg-open
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

        box_container.append(&webview);
        box_container.set_parent(self);

        // Retain webview reference
        unsafe {
            let imp = self.imp();
            let mut_imp = imp as *const imp::MailViewer as *mut imp::MailViewer;
            (*mut_imp).webview = Some(webview);
            let mut_banner = &mut (*mut_imp).banner;
            *mut_banner = banner;
        }
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

        if let Some(ref webview) = self.imp().webview {
            let render_html = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::render_html)
                .unwrap_or(true);
            let body = if render_html {
                sanitize_html(html_content)
            } else {
                format!("<pre>{}</pre>", escape_html(html_content))
            };
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(webview) {
                settings.set_auto_load_images(remote_images_allowed);
            }
            let dark_mode_css = if AppSettings::try_new()
                .as_ref()
                .map(AppSettings::force_dark_mode)
                .unwrap_or(true)
            {
                r#"
  @media (prefers-color-scheme: dark) {
    body {
      color: #eeeeec;
      background-color: #1e1e1e;
    }
    a { color: #78aeed; }
  }
"#
            } else {
                ""
            };
            let mail_font_family = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::mail_font_family)
                .unwrap_or_else(|| "system-ui, sans-serif".to_string());
            let mail_font_size = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::mail_font_size)
                .unwrap_or(14);
            let styled_html = format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="script-src 'none';">
<style>
  body {{
    font-family: {};
    font-size: {}px;
    line-height: 1.6;
    margin: 16px;
    padding: 0;
    color: #2e3436;
    background-color: #ffffff;
  }}
{}
  img {{ max-width: 100%; height: auto; }}
  blockquote {{
    border-left: 3px solid #78aeed;
    margin-left: 0;
    padding-left: 12px;
    color: #555753;
  }}
</style>
</head>
<body>
{}
</body>
</html>"#,
                mail_font_family, mail_font_size, dark_mode_css, body
            );

            webview.load_html(&styled_html, base_uri);
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
        if let Some(ref webview) = self.imp().webview {
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(webview) {
                settings.set_auto_load_images(true);
                webview.reload();
            }
        }
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl Default for MailViewer {
    fn default() -> Self {
        Self::new()
    }
}
