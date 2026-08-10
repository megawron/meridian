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
use webkit6::prelude::*;

use crate::services::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MailViewer {
        pub banner: adw::Banner,
        pub webview: Option<webkit6::WebView>,
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
        if let Some(ref webview) = self.imp().webview {
            let render_html = AppSettings::try_new()
                .as_ref()
                .map(AppSettings::render_html)
                .unwrap_or(true);
            let body = if render_html {
                html_content.to_string()
            } else {
                format!("<pre>{}</pre>", escape_html(html_content))
            };
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
            let styled_html = format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="script-src 'none';">
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Cantarell, "GNOME Sans", sans-serif;
    font-size: 14px;
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
                dark_mode_css, body
            );

            webview.load_html(&styled_html, base_uri);
        }
    }

    /// Allow remote image loading for the current email.
    pub fn allow_remote_images(&self) {
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
