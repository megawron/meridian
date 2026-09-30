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
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use webkit6::prelude::*;

use crate::services::AppSettings;
use crate::services::html_sanitizer::{
    block_remote_images_with_options, html_to_plain_text, is_remote_srcset,
    sanitize_html_with_options, sanitized_html_to_plain_text,
};
use crate::services::inline_content_service::{InlineImage, resolve_cid_images};
use crate::services::mail_metadata_service::RemoteContentStore;
use crate::services::mail_print_service::MailPrintDocument;

const MAX_CACHED_DERIVED_BYTES: usize = 4 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_FIND_MATCH_COUNT: u32 = 10_000;
const RENDER_LOADING_TEXT: &str = "Preparing message…";

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MailViewer {
        pub banner: adw::Banner,
        pub trash_banner: adw::Banner,
        pub junk_banner: adw::Banner,
        pub last_measured_content_height: Cell<Option<i32>>,
        pub adapted_card_height: Cell<Option<i32>>,
        pub last_parent_height: Cell<i32>,
        #[cfg(target_os = "linux")]
        pub webview: RefCell<Option<webkit6::WebView>>,
        #[cfg(target_os = "linux")]
        pub renderer_stack: RefCell<Option<gtk::Stack>>,
        pub fallback_view: RefCell<Option<gtk::TextView>>,
        pub fallback_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        #[cfg(target_os = "linux")]
        pub renderer_error_label: RefCell<Option<gtk::Label>>,
        pub current_message_id: RefCell<Option<String>>,
        pub current_html: RefCell<Option<String>>,
        pub current_inline_images: RefCell<Vec<InlineImage>>,
        pub current_base_uri: RefCell<Option<String>>,
        pub current_fallback_text: RefCell<Option<String>>,
        pub(super) displayed_plaintext: RefCell<Option<DisplayedPlaintext>>,
        pub(super) fallback_search_tag: RefCell<Option<gtk::TextTag>>,
        pub(super) fallback_zoom_tag: RefCell<Option<gtk::TextTag>>,
        pub zoom_level: Cell<f64>,
        pub(super) message_search: RefCell<Option<ActiveMessageSearch>>,
        pub(super) search_generation: Cell<u64>,
        pub remote_images_allowed_for_current: Cell<bool>,
        pub remote_images_blocked_for_current: Cell<bool>,
        pub current_message_force_light: Cell<bool>,
        pub source_generation: Cell<u64>,
        pub render_generation: Cell<u64>,
        pub render_worker_generation: Arc<AtomicU64>,
        pub(super) render_worker_queue: Arc<(Mutex<Option<RenderJob>>, Condvar)>,
        pub render_worker_shutdown: Arc<AtomicBool>,
        pub(super) last_render_key: RefCell<Option<RenderRequestKey>>,
        pub(super) render_cache: RefCell<Option<CachedMailRender>>,
        #[cfg(target_os = "linux")]
        pub active_webview_generation: Cell<u64>,
        #[cfg(target_os = "linux")]
        pub active_webview_uri: RefCell<Option<String>>,
        #[cfg(target_os = "linux")]
        pub(super) find_controller: RefCell<Option<webkit6::FindController>>,
        #[cfg(target_os = "linux")]
        pub(super) webview_ready: Cell<bool>,
        #[cfg(target_os = "linux")]
        pub(super) print_operation: RefCell<Option<webkit6::PrintOperation>>,
        pub settings: RefCell<Option<AppSettings>>,
        pub remote_content_store: RefCell<Option<RemoteContentStore>>,
        pub remote_content_allowed: RefCell<HashSet<String>>,
        pub remote_content_allowed_senders: RefCell<HashSet<String>>,
        pub current_sender: RefCell<Option<String>>,
        pub on_remote_images_blocked: RefCell<Option<Box<dyn Fn(bool) + 'static>>>,
        pub on_scroll_offset: RefCell<Vec<Box<dyn Fn(f64) + 'static>>>,
    }

    impl std::fmt::Debug for MailViewer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MailViewer").finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailViewer {
        const NAME: &'static str = "MeridianMailViewer";
        type Type = super::MailViewer;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for MailViewer {
        fn constructed(&self) {
            self.zoom_level.set(1.0);
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            self.render_worker_shutdown.store(true, Ordering::Release);
            self.render_worker_queue.1.notify_all();
            #[cfg(target_os = "linux")]
            self.print_operation.borrow_mut().take();
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MailViewer {
        fn realize(&self) {
            self.parent_realize();
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (min, nat, min_b, nat_b) = self.parent_measure(orientation, for_size);
            if orientation == gtk::Orientation::Vertical {
                if let Some(target_h) = self.adapted_card_height.get() {
                    return (target_h, target_h, -1, -1);
                }
            }
            (min, nat, min_b, nat_b)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
        }
    }
}

glib::wrapper! {
    pub struct MailViewer(ObjectSubclass<imp::MailViewer>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Disables slide-down/slide-up animations on an `AdwBanner` by configuring
/// its internal `GtkRevealer` with transition duration 0 and transition type None.
/// This prevents layout shifting and content jumping when emails with blocked
/// remote images or warnings are loaded and displayed.
fn disable_banner_animation(banner: &adw::Banner) {
    if let Some(revealer) = banner.first_child().and_then(|c| c.downcast::<gtk::Revealer>().ok()) {
        revealer.set_transition_duration(0);
        revealer.set_transition_type(gtk::RevealerTransitionType::None);
    }
    fn make_shrinkable(widget: &gtk::Widget) {
        if let Some(lbl) = widget.downcast_ref::<gtk::Label>() {
            lbl.set_wrap(false);
            lbl.set_single_line_mode(true);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            lbl.set_width_chars(1);
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            make_shrinkable(&c);
            child = c.next_sibling();
        }
    }
    make_shrinkable(banner.upcast_ref());
}

impl MailViewer {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_current_sender(&self, sender: Option<&str>) {
        self.imp()
            .current_sender
            .replace(sender.map(str::to_string));
    }

    pub fn set_caret_mode(&self, enabled: bool) {
        #[cfg(target_os = "linux")]
        {
            if let Some(webview) = self.imp().webview.borrow().as_ref() {
                if let Some(settings) = webkit6::prelude::WebViewExt::settings(webview) {
                    settings.set_enable_caret_browsing(enabled);
                }
            }
        }
        if let Some(fallback) = self.imp().fallback_view.borrow().as_ref() {
            fallback.set_cursor_visible(enabled);
        }
    }

    fn build_ui(&self) {
        tracing::debug!("MailViewer construction started");
        self.set_overflow(gtk::Overflow::Hidden);
        self.set_hexpand(true);
        // Card styling (.card, .mail-preview-card) is placed on the inner
        // box_container rather than on `self` so that the background,
        // border-radius and box-shadow are drawn at the *constrained*
        // child allocation (see size_allocate above), not at the full
        // height the parent Box/Paned gives us.
        let box_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .overflow(gtk::Overflow::Hidden)
            .hexpand(true)
            .css_classes(vec!["card", "mail-preview-card"])
            .build();

        // ── 1. Privacy Banner for Remote Images ────────────────────────────
        let banner = adw::Banner::builder()
            .title(&crate::i18n::gettext("Remote images blocked"))
            .button_label(&crate::i18n::gettext("Load Images"))
            .revealed(false)
            .visible(false)
            .css_classes(vec!["mail-status-banner"])
            .build();
        disable_banner_animation(&banner);

        banner.connect_button_clicked(glib::clone!(
            #[weak(rename_to = viewer)]
            self,
            move |b| {
                viewer.allow_remote_images();
                b.set_revealed(false);
                b.set_visible(false);
                if let Some(ch) = viewer.imp().last_measured_content_height.get() {
                    viewer.apply_adapted_height(ch);
                }
            }
        ));

        box_container.append(&banner);

        // ── 2. Trash Banner ────────────────────────────────────────────────
        let trash_banner = adw::Banner::builder()
            .title(&crate::i18n::gettext("This message is in the Trash."))
            .button_label(&crate::i18n::gettext("Restore"))
            .revealed(false)
            .visible(false)
            .css_classes(vec!["mail-status-banner"])
            .build();
        disable_banner_animation(&trash_banner);
        box_container.append(&trash_banner);

        // ── 3. Junk Banner ─────────────────────────────────────────────────
        let junk_banner = adw::Banner::builder()
            .title(&crate::i18n::gettext("This message was marked as junk (spam)."))
            .button_label(&crate::i18n::gettext("Not Junk"))
            .revealed(false)
            .visible(false)
            .css_classes(vec!["mail-status-banner"])
            .build();
        disable_banner_animation(&junk_banner);
        box_container.append(&junk_banner);

        let caret_mode = AppSettings::try_new()
            .map(|s| s.mail_caret_mode())
            .unwrap_or(false);
        let fallback_view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(caret_mode)
            .wrap_mode(gtk::WrapMode::WordChar)
            .css_classes(vec!["mail-plain-view"])
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(16)
            .right_margin(16)
            .hexpand(true)
            .vexpand(true)
            .build();

        let fallback_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .min_content_width(0)
            .child(&fallback_view)
            .hexpand(true)
            .vexpand(true)
            .build();
        let viewer_weak_fb = self.downgrade();
        fallback_scroll.vadjustment().connect_value_changed(move |adj| {
            if let Some(viewer) = viewer_weak_fb.upgrade() {
                viewer.notify_scroll_offset(adj.value());
            }
        });
        self.imp()
            .fallback_scroll
            .replace(Some(fallback_scroll.clone()));

        let placeholder = adw::StatusPage::builder()
            .title(&crate::i18n::gettext("No message selected"))
            .description(&crate::i18n::gettext("The secure message viewer will load when opened."))
            .icon_name("mail-read-symbolic")
            .hexpand(true)
            .vexpand(true)
            .build();
        let loading = adw::StatusPage::builder()
            .title(&crate::i18n::gettext("Loading message…"))
            .description(&crate::i18n::gettext("Preparing secure view."))
            .icon_name("view-refresh-symbolic")
            .hexpand(true)
            .vexpand(true)
            .build();
        let error_label = gtk::Label::builder()
            .label(&crate::i18n::gettext("HTML rendering failed. A safe plain text view is displayed."))
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

        let load_error = adw::StatusPage::builder()
            .title(&crate::i18n::gettext("Unable to load message"))
            .description(&crate::i18n::gettext("The message content could not be retrieved from the mail server."))
            .icon_name("dialog-error-symbolic")
            .hexpand(true)
            .vexpand(true)
            .build();

        let renderer_stack = gtk::Stack::builder()
            .hexpand(true)
            .vexpand(true)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        renderer_stack.add_named(&placeholder, Some("placeholder"));
        renderer_stack.add_named(&loading, Some("loading"));
        renderer_stack.add_named(&error_state, Some("error"));
        renderer_stack.add_named(&load_error, Some("load_error"));
        renderer_stack.set_visible_child_name("placeholder");
        box_container.append(&renderer_stack);
        self.imp().renderer_stack.replace(Some(renderer_stack));
        self.imp().renderer_error_label.replace(Some(error_label));


        self.imp().fallback_view.replace(Some(fallback_view));
        let remote_content_store = RemoteContentStore::default();
        self.imp()
            .remote_content_allowed
            .replace(remote_content_store.allowed_message_ids());
        self.imp()
            .remote_content_allowed_senders
            .replace(remote_content_store.allowed_senders());
        self.imp()
            .remote_content_store
            .replace(Some(remote_content_store));
        self.start_render_worker();

        if let Some(settings) = AppSettings::try_new() {
            for key in [
                "auto-load-remote-images",
                "mail-image-loading-policy",
                "mail-preserve-image-layout",
                "render-html",
                "force-dark-mode",
                "mail-dark-mode",
                "mail-font-family",
                "mail-font-size",
                "mail-citation-highlighting",
                "mail-citation-color-1",
                "mail-citation-color-2",
                "mail-citation-color-3",
                "mail-citation-color-4",
                "mail-citation-color-5",
            ] {
                let viewer_weak = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(viewer) = viewer_weak.upgrade() {
                        viewer.reload_current();
                    }
                });
            }
            let viewer_weak_caret = self.downgrade();
            settings.connect_changed(Some("mail-caret-mode"), move |settings, _| {
                if let Some(viewer) = viewer_weak_caret.upgrade() {
                    let caret = settings.boolean("mail-caret-mode");
                    viewer.set_caret_mode(caret);
                }
            });
            self.imp().settings.replace(Some(settings));
        }

        let viewer_weak_dark = self.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(viewer) = viewer_weak_dark.upgrade() {
                viewer.reload_current();
            }
        });

        box_container.set_parent(self);

        // Retain the banner reference.  The target-specific render widget is
        // stored above in its respective interior-mutable field.
        unsafe {
            let imp = self.imp();
            let mut_imp = imp as *const imp::MailViewer as *mut imp::MailViewer;
            (*mut_imp).banner = banner;
            (*mut_imp).trash_banner = trash_banner;
            (*mut_imp).junk_banner = junk_banner;
        }

        // ── 4. Zoom Gestures & Keyboard Shortcuts ──────────────────────────
        // Mouse wheel zoom with Ctrl modifier (Ctrl + Scroll Up/Down)
        let scroll_ctrl = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL,
        );
        scroll_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        let scroll_accum = std::rc::Rc::new(std::cell::Cell::new(0.0f64));
        let viewer_weak_scroll = self.downgrade();
        let accum_clone = scroll_accum.clone();
        scroll_ctrl.connect_scroll(move |controller, _dx, dy| {
            let state = controller.current_event_state();
            let is_ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                && !state.contains(gtk::gdk::ModifierType::SUPER_MASK);
            if is_ctrl {
                if let Some(viewer) = viewer_weak_scroll.upgrade() {
                    let mut val = accum_clone.get() + dy;
                    if val <= -0.5 {
                        viewer.zoom_in();
                        val = 0.0;
                    } else if val >= 0.5 {
                        viewer.zoom_out();
                        val = 0.0;
                    }
                    accum_clone.set(val);
                    return glib::Propagation::Stop;
                }
            } else {
                accum_clone.set(0.0);
            }
            glib::Propagation::Proceed
        });
        self.add_controller(scroll_ctrl);

        // Keyboard zoom shortcuts when focus is inside the viewer (Ctrl++, Ctrl+-, Ctrl+0)
        let key_ctrl = gtk::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        let viewer_weak_key = self.downgrade();
        key_ctrl.connect_key_pressed(move |controller, key, _keycode, _state| {
            let state = controller.current_event_state();
            let is_ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                && !state.contains(gtk::gdk::ModifierType::SUPER_MASK);
            if is_ctrl {
                match key {
                    gtk::gdk::Key::plus
                    | gtk::gdk::Key::KP_Add
                    | gtk::gdk::Key::equal => {
                        if let Some(viewer) = viewer_weak_key.upgrade() {
                            viewer.zoom_in();
                            return glib::Propagation::Stop;
                        }
                    }
                    gtk::gdk::Key::minus
                    | gtk::gdk::Key::KP_Subtract
                    | gtk::gdk::Key::underscore => {
                        if let Some(viewer) = viewer_weak_key.upgrade() {
                            viewer.zoom_out();
                            return glib::Propagation::Stop;
                        }
                    }
                    gtk::gdk::Key::_0
                    | gtk::gdk::Key::KP_0 => {
                        if let Some(viewer) = viewer_weak_key.upgrade() {
                            viewer.zoom_reset();
                            return glib::Propagation::Stop;
                        }
                    }
                    _ => {}
                }
            }
            glib::Propagation::Proceed
        });
        self.add_controller(key_ctrl);

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
        self.load_message_content_with_fallback(
            message_id,
            html_content,
            &[],
            base_uri,
            fallback_text,
        );
    }

    /// Reset the viewer to the empty/placeholder state without flashing
    /// error fallbacks or unformatted plain text.
    pub fn clear(&self) {
        self.invalidate_message_search_context();
        self.imp().remote_images_allowed_for_current.set(false);
        self.imp().remote_images_blocked_for_current.set(false);
        self.imp().current_message_force_light.set(false);
        self.imp().banner.set_revealed(false);
        self.imp().banner.set_visible(false);
        self.notify_remote_images_blocked(false);
        self.imp().current_message_id.take();
        self.imp().current_html.take();
        self.imp().current_inline_images.borrow_mut().clear();
        self.imp().current_base_uri.take();
        self.imp().current_fallback_text.take();
        self.imp().last_render_key.take();
        self.imp().render_cache.take();
        self.imp().displayed_plaintext.take();
        self.set_current_sender(None);
        self.set_trash_banner_visible(false);
        self.set_junk_banner_visible(false);
        self.imp().last_measured_content_height.set(None);
        self.set_vexpand(true);
        self.set_valign(gtk::Align::Fill);
        self.set_height_request(-1);

        #[cfg(target_os = "linux")]
        {
            self.imp().webview_ready.set(false);
            if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
                label.set_visible(false);
            }
            if let Some(webview) = self.imp().webview.borrow().as_ref() {
                webview.set_height_request(-1);
                webview.set_opacity(0.0);
            }
            if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
                stack.set_visible_child_name("placeholder");
            }
        }

        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            view.buffer().set_text("");
        }
        self.notify_scroll_offset(0.0);
    }

    /// Prepare the viewer for asynchronous message body loading.
    /// Clears any previous message state and fades out the previous document so
    /// stale content, preliminary snippets, or empty-state warnings do not flash
    /// before the actual email body arrives.
    pub(crate) fn prepare_for_loading(&self, message_id: Option<&str>) {
        self.notify_scroll_offset(0.0);
        self.invalidate_message_search_context();
        self.imp().remote_images_allowed_for_current.set(false);
        self.imp().remote_images_blocked_for_current.set(false);
        self.imp().current_message_force_light.set(false);
        self.imp().banner.set_revealed(false);
        self.imp().banner.set_visible(false);
        self.notify_remote_images_blocked(false);
        self.imp()
            .current_message_id
            .replace(message_id.map(str::to_string));
        self.imp().current_html.take();
        self.imp().current_inline_images.borrow_mut().clear();
        self.imp().current_base_uri.take();
        self.imp().current_fallback_text.take();
        self.imp().last_render_key.take();
        self.imp().render_cache.take();
        self.imp().displayed_plaintext.take();
        self.set_trash_banner_visible(false);
        self.set_junk_banner_visible(false);
        self.imp().last_measured_content_height.set(None);
        self.imp().adapted_card_height.set(None);
        self.set_vexpand(true);
        self.set_valign(gtk::Align::Fill);
        self.set_height_request(-1);

        #[cfg(target_os = "linux")]
        {
            self.imp().webview_ready.set(false);
            if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
                label.set_visible(false);
            }
            if let Some(webview) = self.imp().webview.borrow().as_ref() {
                webview.set_height_request(-1);
                webview.set_opacity(0.0);
            }
            if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
                if message_id.is_some() {
                    stack.set_visible_child_name("loading");
                } else {
                    stack.set_visible_child_name("placeholder");
                }
            }
        }

        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            view.buffer().set_text("");
        }
    }

    /// Scroll the visible mail viewer viewport up or down by one page.
    /// Returns true if scrolling occurred, or false if already at the boundary.
    pub fn scroll_page(&self, down: bool) -> bool {
        let scroll = self.imp().fallback_scroll.borrow().clone();
        if let Some(scroll) = scroll {
            let vadj = scroll.vadjustment();
            let value = vadj.value();
            let page_size = vadj.page_size();
            let upper = vadj.upper();
            let lower = vadj.lower();
            let max_scroll = (upper - page_size).max(lower);
            if down {
                if value < max_scroll - 2.0 {
                    let next = (value + page_size * 0.9).min(max_scroll);
                    vadj.set_value(next);
                    return true;
                }
            } else if value > lower + 2.0 {
                let next = (value - page_size * 0.9).max(lower);
                vadj.set_value(next);
                return true;
            }
        }
        false
    }

    /// Load a message and resolve inline CID images in the render worker.
    /// Attachment payloads are reference-counted, so rapid selection changes
    /// do not copy or Base64-encode large images on GTK's main context.
    pub(crate) fn load_message_content_with_fallback(
        &self,
        message_id: Option<&str>,
        html_content: &str,
        inline_images: &[InlineImage],
        base_uri: Option<&str>,
        fallback_text: Option<&str>,
    ) {
        let message_changed = self.imp().current_message_id.borrow().as_deref() != message_id;
        let source_changed = {
            let imp = self.imp();
            imp.current_message_id.borrow().as_deref() != message_id
                || imp.current_html.borrow().as_deref() != Some(html_content)
                || !inline_image_sources_match(
                    imp.current_inline_images.borrow().as_slice(),
                    inline_images,
                )
                || imp.current_base_uri.borrow().as_deref() != base_uri
                || imp.current_fallback_text.borrow().as_deref() != fallback_text
        };
        if message_changed {
            self.imp().remote_images_allowed_for_current.set(false);
            self.imp().remote_images_blocked_for_current.set(false);
            self.imp().current_message_force_light.set(false);
            self.imp().banner.set_revealed(false);
            self.imp().banner.set_visible(false);
            self.notify_remote_images_blocked(false);
        }
        if source_changed {
            let source_generation = advance_generation(self.imp().source_generation.get());
            self.imp().source_generation.set(source_generation);
            self.imp().last_render_key.take();
            self.imp().render_cache.take();
        }
        self.imp()
            .current_message_id
            .replace(message_id.map(str::to_string));
        self.imp()
            .current_html
            .replace(Some(html_content.to_string()));
        self.imp()
            .current_inline_images
            .replace(inline_images.to_vec());
        self.imp()
            .current_base_uri
            .replace(base_uri.map(str::to_string));
        self.imp()
            .current_fallback_text
            .replace(fallback_text.map(str::to_string));
        let settings = AppSettings::try_new();
        let image_policy = settings
            .as_ref()
            .map(AppSettings::mail_image_loading_policy)
            .unwrap_or_else(|| "never".to_string());
        let global_remote_images = image_policy == "always"
            || settings
                .as_ref()
                .map(AppSettings::auto_load_remote_images)
                .unwrap_or(false);
        let contacts_remote_images = (image_policy == "sometimes" || image_policy == "contacts")
            && sender_in_contacts(self.imp().current_sender.borrow().as_deref());
        let sender_remote_images = self
            .imp()
            .current_sender
            .borrow()
            .as_deref()
            .map(|sender| {
                let email = extract_email_address(sender);
                !email.is_empty()
                    && self
                        .imp()
                        .remote_content_allowed_senders
                        .borrow()
                        .contains(&email)
            })
            .unwrap_or(false);
        let per_message_remote_images = message_id
            .map(|message_id| {
                self.imp()
                    .remote_content_allowed
                    .borrow()
                    .contains(message_id)
            })
            .unwrap_or(false);
        let remote_images_allowed = global_remote_images
            || contacts_remote_images
            || sender_remote_images
            || per_message_remote_images
            || self.imp().remote_images_allowed_for_current.get();
        let render_html = settings
            .as_ref()
            .map(AppSettings::render_html)
            .unwrap_or(true);
        let mail_font_family = settings
            .as_ref()
            .map(AppSettings::mail_font_family)
            .unwrap_or_else(|| "system-ui, sans-serif".to_string());
        let mail_font_size = settings
            .as_ref()
            .map(AppSettings::mail_font_size)
            .unwrap_or(16);
        let citation_highlighting = settings
            .as_ref()
            .map(AppSettings::mail_citation_highlighting)
            .unwrap_or(true);
        let citation_colors = settings
            .as_ref()
            .map(AppSettings::mail_citation_colors)
            .unwrap_or_else(|| [
                "#729FCF".to_string(),
                "#AD7FA8".to_string(),
                "#8AE234".to_string(),
                "#FCAF3E".to_string(),
                "#E9B96E".to_string(),
            ]);
        let preserve_image_layout = self
            .imp()
            .settings
            .borrow()
            .as_ref()
            .map(AppSettings::mail_preserve_image_layout)
            .unwrap_or(true);
        let render_key = RenderRequestKey {
            source_generation: self.imp().source_generation.get(),
            render_html,
            remote_images_allowed,
            preserve_image_layout,
            force_dark_mode: self.effective_dark_mode(),
            mail_font_family: safe_css_font_family(&mail_font_family),
            mail_font_size: safe_mail_font_size(mail_font_size),
            citation_highlighting,
            citation_colors,
        };
        if self.imp().last_render_key.borrow().as_ref() == Some(&render_key) {
            return;
        }
        // A render request replaces the document that a view-local search
        // may refer to. Clear both the search highlight and its captured
        // message/render identity before the new worker result can arrive.
        self.invalidate_message_search_context();
        self.imp().last_render_key.replace(Some(render_key.clone()));
        let generation = advance_generation(self.imp().render_generation.get());
        self.imp().render_generation.set(generation);
        self.imp()
            .render_worker_generation
            .store(generation, Ordering::Release);

        let decision = {
            let cache = self.imp().render_cache.borrow();
            render_decision(cache.as_ref(), &render_key)
        };
        match decision {
            RenderDecision::UseCachedDocument => {
                let cached = self.imp().render_cache.borrow().as_ref().cloned();
                if let Some(cached) = cached
                    && let Some(document) = cached.document
                {
                    self.apply_rendered_mail(
                        generation,
                        render_key,
                        cached.plan,
                        document.document,
                    );
                    return;
                }
            }
            RenderDecision::RecomposeCachedBody => {
                let plan = self
                    .imp()
                    .render_cache
                    .borrow()
                    .as_ref()
                    .map(|cache| cache.plan.clone());
                if let Some(plan) = plan {
                    self.schedule_render(generation, render_key, RenderWork::Compose { plan });
                    return;
                }
            }
            RenderDecision::PrepareBody => {}
        }
        self.schedule_render(
            generation,
            render_key,
            RenderWork::Prepare {
                html_content: html_content.to_string(),
                inline_images: inline_images.to_vec(),
                fallback_text: fallback_text.map(str::to_string),
            },
        );
    }

    fn schedule_render(&self, generation: u64, key: RenderRequestKey, work: RenderWork) {
        let (queue, wake) = &*self.imp().render_worker_queue;
        queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(RenderJob {
                generation,
                key,
                work,
            });
        wake.notify_one();
    }

    fn start_render_worker(&self) {
        let queue = Arc::clone(&self.imp().render_worker_queue);
        let current_generation = Arc::clone(&self.imp().render_worker_generation);
        let shutdown = Arc::clone(&self.imp().render_worker_shutdown);
        let (sender, receiver) = async_channel::unbounded();
        std::thread::spawn(move || {
            loop {
                let job = {
                    let (pending, wake) = &*queue;
                    let mut pending = pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    while pending.is_none() && !shutdown.load(Ordering::Acquire) {
                        pending = wake
                            .wait(pending)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    if shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    pending.take()
                };
                let Some(job) = job else {
                    continue;
                };
                if let Some(result) = execute_render_job(job, &current_generation)
                    && sender.send_blocking(result).is_err()
                {
                    return;
                }
            }
        });

        let viewer = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok((generation, key, render_plan, document)) = receiver.recv().await {
                let Some(viewer) = viewer.upgrade() else {
                    return;
                };
                viewer.apply_rendered_mail(generation, key, render_plan, document);
            }
        });
    }

    fn apply_rendered_mail(
        &self,
        generation: u64,
        key: RenderRequestKey,
        render_plan: Arc<PreparedMailBody>,
        document: Arc<str>,
    ) {
        if !render_result_is_current(
            self.imp().render_generation.get(),
            self.imp().source_generation.get(),
            self.imp().last_render_key.borrow().as_ref(),
            generation,
            &key,
        ) {
            return;
        }

        self.finish_displayed_message_search();
        self.imp().render_cache.replace(cache_mail_render(
            key.source_generation,
            &key,
            render_plan.clone(),
            document.clone(),
        ));
        let remote_blocked = render_plan.has_remote_images && !key.remote_images_allowed;
        self.imp().remote_images_blocked_for_current.set(remote_blocked);
        self.imp().banner.set_visible(remote_blocked);
        self.imp().banner.set_revealed(remote_blocked);
        self.notify_remote_images_blocked(remote_blocked);
        self.set_fallback_text(&render_plan.visible_text);
        self.imp()
            .displayed_plaintext
            .replace(Some(DisplayedPlaintext {
                source_generation: key.source_generation,
                render_generation: generation,
                message_id: self.imp().current_message_id.borrow().clone(),
                render_plan: render_plan.clone(),
            }));
        tracing::debug!(
            html_bytes = self
                .imp()
                .current_html
                .borrow()
                .as_ref()
                .map_or(0, String::len),
            sanitized_bytes = render_plan.body.len(),
            fallback_used = render_plan.used_fallback,
            render_html = key.render_html,
            "Prepared selected mail content"
        );

        #[cfg(target_os = "linux")]
        {
            let Some(webview) = self.ensure_webkit() else {
                self.show_renderer_error();
                return;
            };
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(&webview) {
                // Keep generic image loading enabled for trusted inline
                // `data:`, `cid:` and `blob:` content. The sanitized body,
                // CSP and policy CSS continue to deny remote sources.
                settings.set_auto_load_images(true);
                let desktop_font = detect_desktop_font_family();
                settings.set_default_font_family(&desktop_font);
                settings.set_sans_serif_font_family(&desktop_font);
            }
            let base_uri = self.imp().current_base_uri.borrow().clone();
            let effective_base_uri =
                effective_mail_base_uri(base_uri.as_deref(), key.remote_images_allowed);
            let content_uri = renderer_content_uri(generation);
            self.imp().active_webview_generation.set(generation);
            self.imp()
                .active_webview_uri
                .replace(Some(content_uri.clone()));
            self.imp().webview_ready.set(false);
            webview.set_opacity(0.0);
            webview.load_alternate_html(&document, &content_uri, effective_base_uri);

            // Timeout backstop (matching Lookout's HTML_REVEAL_TIMEOUT_MS):
            // If WebKit hasn't reported Finished within 400ms (e.g. slow/hung remote fonts or subresources),
            // forcibly reveal the message anyway instead of keeping the view stuck at opacity 0.
            let viewer_weak = self.downgrade();
            let current_gen = generation;
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(400),
                move || {
                    if let Some(viewer) = viewer_weak.upgrade() {
                        if viewer.imp().render_generation.get() == current_gen
                            && !viewer.imp().webview_ready.get()
                        {
                            tracing::debug!("Revealing webview after 400ms timeout backstop");
                            viewer.imp().webview_ready.set(true);
                            if let Some(wv) = viewer.imp().webview.borrow().as_ref() {
                                wv.set_opacity(1.0);
                                viewer.adapt_card_height(wv);
                            }
                            viewer.show_renderer_ready();
                        }
                    }
                },
            );
        }


    }

    fn reload_current(&self) {
        let Some(html) = self.imp().current_html.borrow().clone() else {
            return;
        };
        let message_id = self.imp().current_message_id.borrow().clone();
        let base_uri = self.imp().current_base_uri.borrow().clone();
        let fallback_text = self.imp().current_fallback_text.borrow().clone();
        let inline_images = self.imp().current_inline_images.borrow().clone();
        self.load_message_content_with_fallback(
            message_id.as_deref(),
            &html,
            &inline_images,
            base_uri.as_deref(),
            fallback_text.as_deref(),
        );
    }

    /// Load an already prepared print document without consulting viewer
    /// settings or starting another MIME/HTML preparation pass.  The document
    /// was sanitized on a worker by `mail_print_service`; this method only
    /// hands those bytes to the target renderer and is therefore safe for the
    /// GTK context.
    pub(crate) fn load_print_document(&self, document: &MailPrintDocument) -> Result<u64, String> {
        let source_generation = advance_generation(self.imp().source_generation.get());
        self.imp().source_generation.set(source_generation);
        let render_generation = advance_generation(self.imp().render_generation.get());
        self.imp().render_generation.set(render_generation);
        self.imp()
            .render_worker_generation
            .store(render_generation, Ordering::Release);
        self.imp().last_render_key.take();
        self.imp().render_cache.take();
        self.invalidate_message_search_context();
        self.imp().current_message_id.replace(None);
        self.imp().current_html.replace(Some(document.html.clone()));
        self.imp().current_inline_images.replace(Vec::new());
        self.imp().current_base_uri.replace(None);
        self.imp()
            .current_fallback_text
            .replace(Some(document.plain_text.clone()));
        self.imp().remote_images_allowed_for_current.set(false);
        self.imp().banner.set_revealed(false);
        self.set_fallback_text(&document.plain_text);

        self.imp().webview_ready.set(false);
        let Some(webview) = self.ensure_webkit() else {
            self.show_renderer_error();
            return Err("WebKit-Druckdarsteller ist nicht verfügbar".to_string());
        };
        let content_uri = renderer_content_uri(render_generation);
        self.imp().active_webview_generation.set(render_generation);
        self.imp()
            .active_webview_uri
            .replace(Some(content_uri.clone()));
        webview.load_alternate_html(&document.html, &content_uri, None);

        Ok(render_generation)
    }

    /// Open the native GTK/WebKit print dialog for a worker-prepared document.
    pub(crate) fn print_document<F>(
        &self,
        document: &MailPrintDocument,
        parent: &gtk::Window,
        callback: F,
    ) where
        F: FnOnce(Result<MailPrintDialogResult, String>) + 'static,
    {
        let render_generation = match self.load_print_document(document) {
            Ok(generation) => generation,
            Err(error) => {
                callback(Err(error));
                return;
            }
        };
        let viewer = self.downgrade();
        let parent = parent.clone();
        let attempts = Cell::new(0u16);
        let callback = Rc::new(RefCell::new(Some(callback)));
        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(viewer) = viewer.upgrade() else {
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(Err("Druckdarsteller wurde geschlossen".to_string()));
                }
                return glib::ControlFlow::Break;
            };
            if viewer.imp().render_generation.get() != render_generation {
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(Err(
                        "Druckdokument wurde durch eine neuere Auswahl ersetzt".to_string()
                    ));
                }
                return glib::ControlFlow::Break;
            }
            if !viewer.imp().webview_ready.get() {
                let attempt = attempts.get().saturating_add(1);
                attempts.set(attempt);
                if attempt >= 250 {
                    if let Some(callback) = callback.borrow_mut().take() {
                        callback(Err(
                            "Druckdarstellung wurde nicht rechtzeitig geladen".to_string()
                        ));
                    }
                    return glib::ControlFlow::Break;
                }
                return glib::ControlFlow::Continue;
            }
            let Some(webview) = viewer.imp().webview.borrow().as_ref().cloned() else {
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(Err("WebKit-Druckdarsteller ist nicht verfügbar".to_string()));
                }
                return glib::ControlFlow::Break;
            };

            let print_operation = webkit6::PrintOperation::new(&webview);
            let print_error = Rc::new(RefCell::new(None::<String>));
            let print_error_for_signal = Rc::clone(&print_error);
            print_operation.connect_failed(move |_, error| {
                print_error_for_signal.replace(Some(error.to_string()));
            });
            let callback_for_finished = Rc::clone(&callback);
            let print_error_for_finished = Rc::clone(&print_error);
            let viewer_for_finished = viewer.downgrade();
            print_operation.connect_finished(move |_| {
                if let Some(viewer) = viewer_for_finished.upgrade() {
                    viewer.imp().print_operation.borrow_mut().take();
                }
                let result = print_error_for_finished
                    .borrow_mut()
                    .take()
                    .map_or(Ok(MailPrintDialogResult::Printed), Err);
                if let Some(callback) = callback_for_finished.borrow_mut().take() {
                    callback(result);
                }
            });
            viewer
                .imp()
                .print_operation
                .replace(Some(print_operation.clone()));
            let response = print_operation.run_dialog(Some(&parent));
            match response {
                // The `finished` signal is authoritative for a successful
                // print. It may run inside `run_dialog` or shortly after it
                // returns; the retained operation keeps that signal alive.
                webkit6::PrintOperationResponse::Print => {
                    if let Some(error) = print_error.borrow_mut().take() {
                        viewer.imp().print_operation.borrow_mut().take();
                        if let Some(callback) = callback.borrow_mut().take() {
                            callback(Err(error));
                        }
                    }
                }
                webkit6::PrintOperationResponse::Cancel => {
                    viewer.imp().print_operation.borrow_mut().take();
                    let result = print_error
                        .borrow_mut()
                        .take()
                        .map_or(Ok(MailPrintDialogResult::Cancelled), Err);
                    if let Some(callback) = callback.borrow_mut().take() {
                        callback(result);
                    }
                }
                _ => {
                    viewer.imp().print_operation.borrow_mut().take();
                    if let Some(callback) = callback.borrow_mut().take() {
                        callback(Err("Unbekannte Antwort des Druckdialogs".to_string()));
                    }
                }
            }
            glib::ControlFlow::Break
        });
    }


    /// Increase text/content zoom level in the viewer.
    pub fn zoom_in(&self) {
        let current = self.zoom_level();
        let new_level = ((current + 0.1) * 10.0).round() / 10.0;
        let new_level = new_level.min(3.0);
        self.imp().zoom_level.set(new_level);
        #[cfg(target_os = "linux")]
        if let Some(ref webview) = *self.imp().webview.borrow() {
            webview.set_zoom_level(new_level);
            let viewer_weak = self.downgrade();
            let wv = webview.clone();
            glib::timeout_add_local_once(Duration::from_millis(60), move || {
                if let Some(v) = viewer_weak.upgrade() {
                    v.adapt_card_height(&wv);
                }
            });
        }
        self.apply_fallback_zoom(new_level);
    }

    /// Decrease text/content zoom level in the viewer.
    pub fn zoom_out(&self) {
        let current = self.zoom_level();
        let new_level = ((current - 0.1) * 10.0).round() / 10.0;
        let new_level = new_level.max(0.5);
        self.imp().zoom_level.set(new_level);
        #[cfg(target_os = "linux")]
        if let Some(ref webview) = *self.imp().webview.borrow() {
            webview.set_zoom_level(new_level);
            let viewer_weak = self.downgrade();
            let wv = webview.clone();
            glib::timeout_add_local_once(Duration::from_millis(60), move || {
                if let Some(v) = viewer_weak.upgrade() {
                    v.adapt_card_height(&wv);
                }
            });
        }
        self.apply_fallback_zoom(new_level);
    }

    /// Reset zoom level in the viewer to 100%.
    pub fn zoom_reset(&self) {
        self.imp().zoom_level.set(1.0);
        #[cfg(target_os = "linux")]
        if let Some(ref webview) = *self.imp().webview.borrow() {
            webview.set_zoom_level(1.0);
            let viewer_weak = self.downgrade();
            let wv = webview.clone();
            glib::timeout_add_local_once(Duration::from_millis(60), move || {
                if let Some(v) = viewer_weak.upgrade() {
                    v.adapt_card_height(&wv);
                }
            });
        }
        self.apply_fallback_zoom(1.0);
    }

    /// Return the current zoom level.
    pub fn zoom_level(&self) -> f64 {
        let level = self.imp().zoom_level.get();
        if level <= 0.0 {
            1.0
        } else {
            level
        }
    }

    pub fn set_trash_banner_visible(&self, visible: bool) {
        self.imp().trash_banner.set_visible(visible);
        self.imp().trash_banner.set_revealed(visible);
        if let Some(ch) = self.imp().last_measured_content_height.get() {
            self.apply_adapted_height(ch);
        }
    }

    pub fn set_junk_banner_visible(&self, visible: bool) {
        self.imp().junk_banner.set_visible(visible);
        self.imp().junk_banner.set_revealed(visible);
        if let Some(ch) = self.imp().last_measured_content_height.get() {
            self.apply_adapted_height(ch);
        }
    }

    pub fn set_remote_images_banner_visible(&self, visible: bool) {
        self.imp().banner.set_visible(visible);
        self.imp().banner.set_revealed(visible);
        if let Some(ch) = self.imp().last_measured_content_height.get() {
            self.apply_adapted_height(ch);
        }
    }

    pub fn is_remote_images_banner_revealed(&self) -> bool {
        self.imp().banner.is_revealed()
    }

    pub fn is_remote_images_blocked(&self) -> bool {
        self.imp().remote_images_blocked_for_current.get()
    }

    pub fn has_visible_banner(&self) -> bool {
        self.imp().banner.is_revealed()
            || self.imp().trash_banner.is_revealed()
            || self.imp().junk_banner.is_revealed()
    }

    pub fn connect_trash_restore<F: Fn() + 'static>(&self, callback: F) {
        self.imp().trash_banner.connect_button_clicked(move |_| {
            callback();
        });
    }

    pub fn connect_junk_restore<F: Fn() + 'static>(&self, callback: F) {
        self.imp().junk_banner.connect_button_clicked(move |_| {
            callback();
        });
    }

    pub fn connect_remote_images_blocked<F: Fn(bool) + 'static>(&self, callback: F) {
        self.imp()
            .on_remote_images_blocked
            .replace(Some(Box::new(callback)));
    }

    pub(crate) fn notify_remote_images_blocked(&self, blocked: bool) {
        if let Some(cb) = self.imp().on_remote_images_blocked.borrow().as_ref() {
            cb(blocked);
        }
    }

    pub fn connect_scroll_offset<F: Fn(f64) + 'static>(&self, callback: F) {
        self.imp()
            .on_scroll_offset
            .borrow_mut()
            .push(Box::new(callback));
    }

    pub(crate) fn notify_scroll_offset(&self, offset_y: f64) {
        for cb in self.imp().on_scroll_offset.borrow().iter() {
            cb(offset_y);
        }
    }

    /// Allow remote image loading for the current email.
    pub fn allow_remote_images(&self) {
        if let Some(message_id) = self.imp().current_message_id.borrow().as_deref() {
            let result = self
                .imp()
                .remote_content_store
                .borrow()
                .as_ref()
                .map(|store| store.set_allowed(message_id, true));
            match result {
                Some(Ok(())) => {
                    self.imp()
                        .remote_content_allowed
                        .borrow_mut()
                        .insert(message_id.to_string());
                }
                Some(Err(error)) => {
                    tracing::warn!(%error, "Could not persist remote content approval");
                }
                None => tracing::warn!("Remote content store is unavailable"),
            }
        }
        self.imp().remote_images_allowed_for_current.set(true);
        self.imp().remote_images_blocked_for_current.set(false);
        self.imp().banner.set_revealed(false);
        self.imp().banner.set_visible(false);
        self.notify_remote_images_blocked(false);
        // Re-run the rendering path from the retained original HTML.  A
        // WebKit reload alone cannot restore remote <img> tags that were
        // deliberately removed from the privacy-first document.
        self.reload_current();
    }

    /// Return the currently set sender string, if any.
    pub fn current_sender(&self) -> Option<String> {
        self.imp().current_sender.borrow().clone()
    }

    /// Allow remote image loading for the sender of the current email permanently.
    pub fn allow_remote_images_for_sender(&self) {
        if let Some(sender) = self.imp().current_sender.borrow().as_deref() {
            let email = extract_email_address(sender);
            if !email.is_empty() {
                if let Some(store) = self.imp().remote_content_store.borrow().as_ref() {
                    if let Err(error) = store.set_sender_allowed(&email, true) {
                        tracing::warn!(%error, "Could not persist remote content approval for sender");
                    }
                }
                self.imp()
                    .remote_content_allowed_senders
                    .borrow_mut()
                    .insert(email);
            }
        }
        self.allow_remote_images();
    }

    /// Returns whether the mail should be displayed in dark mode.
    ///
    /// Respects the per-message light mode override, the application's
    /// `mail-dark-mode` setting (default: true), and the system/Libadwaita
    /// theme (`adw::StyleManager::default().is_dark()`), as well as the
    /// legacy `force-dark-mode` switch.
    pub fn effective_dark_mode(&self) -> bool {
        if self.imp().current_message_force_light.get() {
            return false;
        }
        self.is_dark_mode_supported_or_active()
    }

    /// Returns whether dark mode is currently active for the mail environment
    /// (either via system/app dark theme with `mail-dark-mode` enabled, or
    /// via `force-dark-mode`).
    pub fn is_dark_mode_supported_or_active(&self) -> bool {
        let settings = self.imp().settings.borrow();
        let allow_dark = settings
            .as_ref()
            .map(AppSettings::mail_dark_mode)
            .unwrap_or(true);
        let force_dark = settings
            .as_ref()
            .map(AppSettings::force_dark_mode)
            .unwrap_or(false);

        (allow_dark && adw::StyleManager::default().is_dark()) || force_dark
    }

    /// Toggle the per-message light mode override for the currently displayed message.
    /// Returns the new state of the override (`true` if light mode is forced).
    pub fn toggle_light_mode_override(&self) -> bool {
        let current = self.imp().current_message_force_light.get();
        let next = !current;
        self.imp().current_message_force_light.set(next);
        self.reload_current();
        next
    }

    /// Check if the currently displayed message has a light mode override active.
    pub fn is_light_mode_override(&self) -> bool {
        self.imp().current_message_force_light.get()
    }

    /// Set the per-message light mode override explicitly.
    pub fn set_light_mode_override(&self, force_light: bool) {
        if self.imp().current_message_force_light.get() != force_light {
            self.imp().current_message_force_light.set(force_light);
            self.reload_current();
        }
    }

    /// Start a search limited to the message currently owned by this viewer.
    ///
    /// Linux/WebKit builds delegate highlighting and navigation to the
    /// document's `WebKitFindController`. Builds without WebKit, and a
    /// WebKit document which is not ready or has failed, use the rendered
    /// plain-text representation and select the active match in the fallback
    /// `TextView`. The returned generation lets the future window-level
    /// search UI discard a result that it no longer owns.
    pub(crate) fn search_displayed_message(&self, query: &str) -> MessageSearchResult {
        let generation = self.begin_message_search();
        if query.trim().is_empty() {
            return self.replace_empty_search_result(generation, query);
        }

        #[cfg(target_os = "linux")]
        if let Some(result) = self.search_displayed_message_webkit(query, generation) {
            return result;
        }

        self.search_displayed_message_plaintext(query, generation)
    }

    /// Move to the next match in the active displayed-message search.
    pub(crate) fn search_displayed_message_next(&self) -> MessageSearchResult {
        self.advance_displayed_message_search(MessageSearchDirection::Next)
    }

    /// Move to the previous match in the active displayed-message search.
    pub(crate) fn search_displayed_message_previous(&self) -> MessageSearchResult {
        self.advance_displayed_message_search(MessageSearchDirection::Previous)
    }

    pub(crate) fn displayed_message_search_result(&self) -> Option<MessageSearchResult> {
        self.imp()
            .message_search
            .borrow()
            .as_ref()
            .map(|search| search.result.clone())
    }

    /// Stop highlighting/searching without changing the displayed message.
    pub(crate) fn finish_displayed_message_search(&self) {
        #[cfg(target_os = "linux")]
        if let Some(controller) = self.imp().find_controller.borrow().as_ref() {
            controller.search_finish();
        }
        self.clear_fallback_search_highlight();
        self.imp().message_search.take();
        self.imp()
            .search_generation
            .set(advance_generation(self.imp().search_generation.get()));
    }

    fn begin_message_search(&self) -> u64 {
        self.finish_displayed_message_search();
        let generation = advance_generation(self.imp().search_generation.get());
        self.imp().search_generation.set(generation);
        generation
    }

    fn invalidate_message_search_context(&self) {
        self.finish_displayed_message_search();
        self.imp().displayed_plaintext.take();
    }

    fn replace_empty_search_result(&self, generation: u64, query: &str) -> MessageSearchResult {
        let result = MessageSearchResult {
            generation,
            backend: None,
            status: MessageSearchStatus::EmptyQuery,
            query: query.to_string(),
            match_count: 0,
            current_match: None,
        };
        self.imp().message_search.replace(Some(ActiveMessageSearch {
            source_generation: self.imp().source_generation.get(),
            render_generation: self.imp().render_generation.get(),
            message_id: self.imp().current_message_id.borrow().clone(),
            fallback_matches: Vec::new(),
            result: result.clone(),
        }));
        result
    }

    fn search_displayed_message_plaintext(
        &self,
        query: &str,
        generation: u64,
    ) -> MessageSearchResult {
        let Some(displayed) = self.current_displayed_plaintext() else {
            let result = MessageSearchResult {
                generation,
                backend: None,
                status: MessageSearchStatus::NoDisplayedMessage,
                query: query.to_string(),
                match_count: 0,
                current_match: None,
            };
            self.imp().message_search.replace(Some(ActiveMessageSearch {
                source_generation: self.imp().source_generation.get(),
                render_generation: self.imp().render_generation.get(),
                message_id: self.imp().current_message_id.borrow().clone(),
                fallback_matches: Vec::new(),
                result: result.clone(),
            }));
            return result;
        };

        let matches = plaintext_search_matches(&displayed.render_plan.visible_text, query);
        let result = MessageSearchResult {
            generation,
            backend: Some(MessageSearchBackend::PlaintextFallback),
            status: if matches.is_empty() {
                MessageSearchStatus::NotFound
            } else {
                MessageSearchStatus::Found
            },
            query: query.to_string(),
            match_count: matches.len(),
            current_match: (!matches.is_empty()).then_some(1),
        };
        self.imp().message_search.replace(Some(ActiveMessageSearch {
            source_generation: displayed.source_generation,
            render_generation: displayed.render_generation,
            message_id: displayed.message_id,
            fallback_matches: matches.clone(),
            result: result.clone(),
        }));
        if !matches.is_empty() {
            self.apply_fallback_search_matches(&matches, 0);
        }
        result
    }

    fn current_displayed_plaintext(&self) -> Option<DisplayedPlaintext> {
        let displayed = self.imp().displayed_plaintext.borrow().as_ref()?.clone();
        let current_message_id = self.imp().current_message_id.borrow().clone();
        message_search_context_is_current(
            self.imp().source_generation.get(),
            self.imp().render_generation.get(),
            current_message_id.as_deref(),
            displayed.source_generation,
            displayed.render_generation,
            displayed.message_id.as_deref(),
        )
        .then_some(displayed)
    }

    fn advance_displayed_message_search(
        &self,
        direction: MessageSearchDirection,
    ) -> MessageSearchResult {
        let Some(mut search) = self.imp().message_search.take() else {
            return self.search_displayed_message("");
        };
        let current_message_id = self.imp().current_message_id.borrow().clone();
        if !message_search_context_is_current(
            self.imp().source_generation.get(),
            self.imp().render_generation.get(),
            current_message_id.as_deref(),
            search.source_generation,
            search.render_generation,
            search.message_id.as_deref(),
        ) {
            self.clear_fallback_search_highlight();
            return self.search_displayed_message("");
        }

        if search.result.status != MessageSearchStatus::Found {
            let result = search.result.clone();
            self.imp().message_search.replace(Some(search));
            return result;
        }

        let Some(current_match) = search.result.current_match else {
            let result = search.result.clone();
            self.imp().message_search.replace(Some(search));
            return result;
        };
        let match_count = search.result.match_count;
        if match_count == 0 {
            let result = search.result.clone();
            self.imp().message_search.replace(Some(search));
            return result;
        }

        let next_match = match direction {
            MessageSearchDirection::Next => current_match % match_count + 1,
            MessageSearchDirection::Previous => {
                if current_match == 1 {
                    match_count
                } else {
                    current_match - 1
                }
            }
        };

        match search.result.backend {
            Some(MessageSearchBackend::PlaintextFallback) => {
                self.apply_fallback_search_matches(&search.fallback_matches, next_match - 1);
                search.result.current_match = Some(next_match);
            }
            #[cfg(target_os = "linux")]
            Some(MessageSearchBackend::WebKit) => {
                if !self.webkit_search_is_ready() {
                    let query = search.result.query.clone();
                    return self.search_displayed_message(&query);
                }
                if let Some(controller) = self.imp().find_controller.borrow().as_ref() {
                    match direction {
                        MessageSearchDirection::Next => controller.search_next(),
                        MessageSearchDirection::Previous => controller.search_previous(),
                    }
                    search.result.current_match = Some(next_match);
                } else {
                    let query = search.result.query.clone();
                    return self.search_displayed_message(&query);
                }
            }
            None => {}
        }

        let result = search.result.clone();
        self.imp().message_search.replace(Some(search));
        result
    }

    fn clear_fallback_search_highlight(&self) {
        let fallback_view = self.imp().fallback_view.borrow();
        let Some(view) = fallback_view.as_ref() else {
            return;
        };
        let buffer = view.buffer();
        if let Some(tag) = self.imp().fallback_search_tag.borrow().as_ref() {
            let start = buffer.start_iter();
            let end = buffer.end_iter();
            buffer.remove_tag(tag, &start, &end);
        }
        let end = buffer.end_iter();
        buffer.select_range(&end, &end);
    }

    fn apply_fallback_search_matches(&self, matches: &[TextSearchMatch], active: usize) {
        let fallback_view = self.imp().fallback_view.borrow();
        let Some(view) = fallback_view.as_ref() else {
            return;
        };
        let buffer = view.buffer();
        let Some(tag) = self.fallback_search_tag(&buffer) else {
            return;
        };

        let start = buffer.start_iter();
        let end = buffer.end_iter();
        buffer.remove_tag(&tag, &start, &end);
        for matched in matches {
            let start = buffer.iter_at_offset(gtk_text_offset(matched.start));
            let end = buffer.iter_at_offset(gtk_text_offset(matched.end));
            buffer.apply_tag(&tag, &start, &end);
        }
        if let Some(matched) = matches.get(active) {
            let start = buffer.iter_at_offset(gtk_text_offset(matched.start));
            let end = buffer.iter_at_offset(gtk_text_offset(matched.end));
            buffer.select_range(&start, &end);
            let mut scroll = start.clone();
            view.scroll_to_iter(&mut scroll, 0.1, false, 0.0, 0.5);
        }
    }

    fn fallback_search_tag(&self, buffer: &gtk::TextBuffer) -> Option<gtk::TextTag> {
        if let Some(tag) = self.imp().fallback_search_tag.borrow().as_ref() {
            return Some(tag.clone());
        }
        let tag = gtk::TextTag::builder()
            .name("meridian-mail-search-match")
            .background("yellow")
            .foreground("black")
            .build();
        if !buffer.tag_table().add(&tag) {
            return None;
        }
        self.imp().fallback_search_tag.replace(Some(tag.clone()));
        Some(tag)
    }

    #[cfg(target_os = "linux")]
    fn search_displayed_message_webkit(
        &self,
        query: &str,
        generation: u64,
    ) -> Option<MessageSearchResult> {
        if self.current_displayed_plaintext().is_none() || !self.webkit_search_is_ready() {
            return None;
        }
        let controller = self.imp().find_controller.borrow().clone()?;
        let result = MessageSearchResult {
            generation,
            backend: Some(MessageSearchBackend::WebKit),
            status: MessageSearchStatus::Pending,
            query: query.to_string(),
            match_count: 0,
            current_match: None,
        };
        let displayed = self.current_displayed_plaintext()?;
        self.imp().message_search.replace(Some(ActiveMessageSearch {
            source_generation: displayed.source_generation,
            render_generation: displayed.render_generation,
            message_id: displayed.message_id,
            fallback_matches: Vec::new(),
            result: result.clone(),
        }));
        controller.search_finish();
        let options =
            (webkit6::FindOptions::CASE_INSENSITIVE | webkit6::FindOptions::WRAP_AROUND).bits();
        controller.search(query, options, MAX_FIND_MATCH_COUNT);
        Some(result)
    }

    #[cfg(target_os = "linux")]
    fn webkit_search_is_ready(&self) -> bool {
        let Some(webview) = self.imp().webview.borrow().clone() else {
            return false;
        };
        self.imp().webview_ready.get() && self.is_current_webview_load(&webview)
    }

    #[cfg(target_os = "linux")]
    fn webkit_search_event_is_current(&self, controller: &webkit6::FindController) -> bool {
        let binding = self.imp().message_search.borrow();
        let Some(search) = binding.as_ref() else {
            return false;
        };
        let current_message_id = self.imp().current_message_id.borrow().clone();
        if search.result.backend != Some(MessageSearchBackend::WebKit)
            || !message_search_context_is_current(
                self.imp().source_generation.get(),
                self.imp().render_generation.get(),
                current_message_id.as_deref(),
                search.source_generation,
                search.render_generation,
                search.message_id.as_deref(),
            )
            || search.result.query != controller.search_text().as_deref().unwrap_or_default()
        {
            return false;
        }
        self.webkit_search_is_ready()
    }

    #[cfg(target_os = "linux")]
    fn handle_webkit_found_text(&self, controller: &webkit6::FindController, count: u32) {
        if !self.webkit_search_event_is_current(controller) {
            return;
        }
        let mut search = self.imp().message_search.borrow_mut();
        let Some(search) = search.as_mut() else {
            return;
        };
        let count = count as usize;
        search.result.match_count = count;
        search.result.status = if count == 0 {
            MessageSearchStatus::NotFound
        } else {
            MessageSearchStatus::Found
        };
        search.result.current_match = (count > 0).then_some(1);
    }

    #[cfg(target_os = "linux")]
    fn handle_webkit_failed_to_find_text(&self, controller: &webkit6::FindController) {
        if !self.webkit_search_event_is_current(controller) {
            return;
        }
        let mut search = self.imp().message_search.borrow_mut();
        let Some(search) = search.as_mut() else {
            return;
        };
        search.result.status = MessageSearchStatus::NotFound;
        search.result.match_count = 0;
        search.result.current_match = None;
    }

    fn set_fallback_text(&self, text: &str) {
        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            view.buffer().set_text(text);
            self.apply_fallback_zoom(self.zoom_level());
        }
        let line_count = text.lines().count().max(1) as i32;
        let estimated_h = (line_count * 22 + 40).min(2000);
        self.imp().last_measured_content_height.set(Some(estimated_h));
        self.apply_adapted_height(estimated_h);
    }

    fn apply_fallback_zoom(&self, level: f64) {
        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            let buffer = view.buffer();
            let tag = if let Some(tag) = self.imp().fallback_zoom_tag.borrow().as_ref() {
                tag.clone()
            } else {
                let tag = gtk::TextTag::builder()
                    .name("meridian-mail-zoom")
                    .scale(level)
                    .build();
                if buffer.tag_table().add(&tag) {
                    self.imp().fallback_zoom_tag.replace(Some(tag.clone()));
                    tag
                } else {
                    return;
                }
            };
            tag.set_scale(level);
            let (start, end) = buffer.bounds();
            buffer.apply_tag(&tag, &start, &end);
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
        // Enable JavaScript so internal measurement scripts (adapt_card_height) can execute.
        // Document scripts in incoming emails remain strictly blocked by ammonia HTML
        // sanitization and Content-Security-Policy (`script-src 'none'`).
        settings.set_enable_javascript(true);
        let caret_mode = AppSettings::try_new()
            .map(|s| s.mail_caret_mode())
            .unwrap_or(false);
        settings.set_enable_caret_browsing(caret_mode);
        // Remote sources are controlled by the sanitized document and CSP,
        // not by WebKit's global image switch. The latter must stay enabled
        // so trusted inline `data:`, `cid:` and `blob:` images remain visible.
        settings.set_auto_load_images(true);
        settings.set_enable_developer_extras(false);
        settings.set_enable_page_cache(true);
        settings.set_enable_smooth_scrolling(true);
        let desktop_font = detect_desktop_font_family();
        settings.set_default_font_family(&desktop_font);
        settings.set_sans_serif_font_family(&desktop_font);

        let user_content_manager = webkit6::UserContentManager::new();
        let user_style_sheet = webkit6::UserStyleSheet::new(
            USER_MAIL_OVERRIDE_CSS,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        );
        user_content_manager.add_style_sheet(&user_style_sheet);
        let scroll_script = webkit6::UserScript::new(
            "(() => {
                let isCompact = false;
                let ticking = false;
                window.addEventListener('scroll', () => {
                    if (!ticking) {
                        window.requestAnimationFrame(() => {
                            try {
                                const y = window.scrollY || document.documentElement.scrollTop || 0;
                                if (!isCompact && y > 45) {
                                    isCompact = true;
                                    window.webkit.messageHandlers.meridianScroll.postMessage(y);
                                } else if (isCompact && y < 15) {
                                    isCompact = false;
                                    window.webkit.messageHandlers.meridianScroll.postMessage(y);
                                }
                            } catch (e) {}
                            ticking = false;
                        });
                        ticking = true;
                    }
                }, { passive: true });
            })();",
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::End,
            &[],
            &[],
        );
        user_content_manager.add_script(&scroll_script);
        user_content_manager.register_script_message_handler("meridianScroll", None);
        let viewer_weak_scroll = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianScroll",
            false,
            move |values| {
                if let Some(viewer) = viewer_weak_scroll.upgrade() {
                    if let Some(js_val) = values.get(1).and_then(|v| v.get::<javascriptcore6::Value>().ok()) {
                        let offset_y = js_val.to_double();
                        viewer.notify_scroll_offset(offset_y);
                    }
                }
                None
            },
        );

        let webview = webkit6::WebView::builder()
            .web_context(&web_context)
            .settings(&settings)
            .user_content_manager(&user_content_manager)
            .hexpand(true)
            .vexpand(true)
            .build();
        webview.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        webview.set_zoom_level(self.zoom_level());

        let viewer_weak = self.downgrade();
        webview.connect_load_changed(move |webview, event| {
            let Some(viewer) = viewer_weak.upgrade() else {
                return;
            };
            if !viewer.is_current_webview_load(webview) {
                return;
            }
            match event {
                webkit6::LoadEvent::Finished => {
                    viewer.imp().webview_ready.set(true);
                    viewer.show_renderer_ready();
                    viewer.adapt_card_height(webview);
                    let viewer_weak_d1 = viewer.downgrade();
                    let wv_d1 = webview.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
                        if let Some(v) = viewer_weak_d1.upgrade() {
                            v.adapt_card_height(&wv_d1);
                        }
                    });
                    let viewer_weak_d2 = viewer.downgrade();
                    let wv_d2 = webview.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                        if let Some(v) = viewer_weak_d2.upgrade() {
                            v.adapt_card_height(&wv_d2);
                        }
                    });
                }
                _ => {}
            }
        });

        let viewer_weak = self.downgrade();
        webview.connect_load_failed(move |webview, _, failing_uri, error| {
            tracing::warn!(uri = %redact_renderer_uri(failing_uri), %error, "Mail viewer failed to load document");
            if let Some(viewer) = viewer_weak.upgrade() {
                if viewer.is_current_webview_load(webview) {
                    viewer.imp().webview_ready.set(false);
                    viewer.finish_displayed_message_search();
                    viewer.show_renderer_error();
                }
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
                                if !is_internal_renderer_uri(&uri) && !uri.starts_with("data:") {
                                    tracing::info!(
                                        uri = %redact_renderer_uri(&uri),
                                        "Opening external link in browser"
                                    );
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

        if let Some(find_controller) = webview.find_controller() {
            let viewer_weak = self.downgrade();
            find_controller.connect_found_text(move |controller, count| {
                if let Some(viewer) = viewer_weak.upgrade() {
                    viewer.handle_webkit_found_text(controller, count);
                }
            });

            let viewer_weak = self.downgrade();
            find_controller.connect_failed_to_find_text(move |controller| {
                if let Some(viewer) = viewer_weak.upgrade() {
                    viewer.handle_webkit_failed_to_find_text(controller);
                }
            });
            self.imp().find_controller.replace(Some(find_controller));
        }

        stack.add_named(&webview, Some("webkit"));
        stack.set_visible_child_name("webkit");
        self.imp().webview.replace(Some(webview.clone()));
        tracing::debug!("Deferred WebKitGTK context and view created");
        Some(webview)
    }

    #[cfg(target_os = "linux")]
    fn is_current_webview_load(&self, webview: &webkit6::WebView) -> bool {
        let current_uri = webview.uri();
        webview_load_is_current(
            self.imp().active_webview_generation.get(),
            self.imp().render_generation.get(),
            self.imp().active_webview_uri.borrow().as_deref(),
            current_uri.as_deref(),
        )
    }

    #[cfg(target_os = "linux")]
    fn show_renderer_loading(&self) {
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(false);
        }
    }

    #[cfg(target_os = "linux")]
    fn show_renderer_ready(&self) {
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(false);
        }
        if let Some(webview) = self.imp().webview.borrow().as_ref() {
            webview.set_opacity(1.0);
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("webkit");
        }
    }

    #[cfg(target_os = "linux")]
    pub fn adapt_card_height(&self, webview: &webkit6::WebView) {
        let viewer_weak = self.downgrade();
        // Measure the bottom of the lowest element in body or body.offsetHeight.
        // DO NOT use document.documentElement.scrollHeight because in WebKit that is always >= viewport height!
        let script = r#"
(() => {
    let body = document.body;
    if (!body) return 0;
    let maxBottom = 0;
    let children = body.children;
    let scrollY = window.scrollY || window.pageYOffset || 0;
    for (let i = 0; i < children.length; i++) {
        let r = children[i].getBoundingClientRect();
        let bottom = r.bottom + scrollY;
        if (bottom > maxBottom) {
            maxBottom = bottom;
        }
    }
    let rangeBottom = 0;
    try {
        let range = document.createRange();
        range.selectNodeContents(body);
        let rangeRect = range.getBoundingClientRect();
        rangeBottom = rangeRect.bottom + scrollY;
    } catch (e) {}
    let docEl = document.documentElement;
    let computed = window.getComputedStyle(body);
    let padBottom = parseFloat(computed.paddingBottom) || 16;
    let measured = Math.max(
        maxBottom + padBottom,
        rangeBottom + padBottom,
        body.scrollHeight || 0,
        docEl ? (docEl.scrollHeight || 0) : 0
    );
    return Math.ceil(measured);
})()
"#;
        let viewer_weak_fb = viewer_weak.clone();
        let script_clone = script.to_string();
        let webview_clone = webview.clone();
        webview.evaluate_javascript(
            script,
            None,
            Some("meridian-measure"),
            None::<&gio::Cancellable>,
            move |result| {
                match result {
                    Ok(value) => {
                        let measured_h = value.to_double().max(0.0) as i32;
                        let content_height = if measured_h > 0 { measured_h + 8 } else { 500 };
                        if let Some(viewer) = viewer_weak.upgrade() {
                            tracing::debug!(content_height, "Measured email content height (isolated world)");
                            viewer.imp().last_measured_content_height.set(Some(content_height));
                            viewer.apply_adapted_height(content_height);
                        }
                    }
                    Err(err) => {
                        tracing::debug!(?err, "evaluate_javascript in isolated world failed, trying default world");
                        webview_clone.evaluate_javascript(
                            &script_clone,
                            None,
                            None,
                            None::<&gio::Cancellable>,
                            move |result2| {
                                match result2 {
                                    Ok(value) => {
                                        let measured_h = value.to_double().max(0.0) as i32;
                                        let content_height = if measured_h > 0 { measured_h + 8 } else { 500 };
                                        if let Some(viewer) = viewer_weak_fb.upgrade() {
                                            tracing::debug!(content_height, "Measured email content height (default world)");
                                            viewer.imp().last_measured_content_height.set(Some(content_height));
                                            viewer.apply_adapted_height(content_height);
                                        }
                                    }
                                    Err(err2) => {
                                        tracing::warn!(?err2, "evaluate_javascript in default world also failed");
                                    }
                                }
                            },
                        );
                    }
                }
            },
        );
    }

    pub fn apply_adapted_height(&self, content_height: i32) {
        if content_height <= 0 {
            if self.imp().adapted_card_height.get().is_some() {
                self.imp().adapted_card_height.set(None);
                self.set_vexpand(true);
                self.set_valign(gtk::Align::Fill);
                self.set_height_request(-1);
                #[cfg(target_os = "linux")]
                if let Some(wv) = self.imp().webview.borrow().as_ref() {
                    wv.set_vexpand(true);
                    wv.set_valign(gtk::Align::Fill);
                    wv.set_height_request(-1);
                }
                if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
                    stack.set_vexpand(true);
                    stack.set_valign(gtk::Align::Fill);
                    stack.set_height_request(-1);
                }
            }
            return;
        }

        // Available height in parent message pane: determine from the enclosing
        // Paned or Window so that available height does not shrink when the viewer shrinks.
        let available_height = {
            let mut container_h = 0;
            let mut current = self.parent();
            while let Some(w) = current {
                if w.is::<gtk::Paned>() || w.is::<gtk::Window>() {
                    container_h = w.height();
                    break;
                }
                current = w.parent();
            }
            if container_h <= 120 {
                container_h = self.root().map(|r| r.height()).unwrap_or(0);
            }
            if container_h > 120 {
                let other_h = if let Some(parent) = self.parent() {
                    let mut sum = 0;
                    let mut sibling = parent.first_child();
                    while let Some(child) = sibling {
                        if child != *self.upcast_ref::<gtk::Widget>() && child.is_visible() {
                            sum += child.height();
                        }
                        sibling = child.next_sibling();
                    }
                    sum
                } else {
                    120
                };
                let margins = self.margin_top() + self.margin_bottom();
                (container_h - other_h - margins).max(100)
            } else {
                500
            }
        };

        let mut banner_extra = 0;
        if self.imp().banner.is_revealed() || self.imp().banner.is_visible() {
            banner_extra += 48;
        }
        if self.imp().trash_banner.is_revealed() || self.imp().trash_banner.is_visible() {
            banner_extra += 48;
        }
        if self.imp().junk_banner.is_revealed() || self.imp().junk_banner.is_visible() {
            banner_extra += 48;
        }

        let needed_total = (content_height + banner_extra).max(80);

        tracing::debug!(
            content_height,
            needed_total,
            available_height,
            is_short = needed_total < available_height,
            "Applying adapted mail viewer card height"
        );

        if needed_total < available_height {
            if self.imp().adapted_card_height.get() != Some(needed_total) {
                self.imp().adapted_card_height.set(Some(needed_total));
                self.set_vexpand(false);
                self.set_valign(gtk::Align::Start);
                self.set_height_request(needed_total);
            }
        } else if self.imp().adapted_card_height.get().is_some() {
            self.imp().adapted_card_height.set(None);
            self.set_vexpand(true);
            self.set_valign(gtk::Align::Fill);
            self.set_height_request(-1);
        }
    }

    pub(crate) fn show_renderer_error(&self) {
        if self.imp().current_message_id.borrow().is_none() {
            if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
                label.set_visible(false);
            }
            if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
                stack.set_visible_child_name("placeholder");
            }
            return;
        }

        self.imp().webview_ready.set(false);
        self.imp().last_render_key.take();
        if let Some(webview) = self.imp().webview.borrow().as_ref() {
            webview.set_opacity(0.0);
        }
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(true);
        }
        if let Some(view) = self.imp().fallback_view.borrow().as_ref() {
            let buffer = view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            if text.is_empty() || text.as_str() == crate::i18n::gettext("No message selected.") {
                let fallback = self.imp().current_fallback_text.borrow();
                if let Some(ref fb) = *fallback {
                    if !fb.is_empty() {
                        buffer.set_text(fb);
                    } else {
                        buffer.set_text(&crate::i18n::gettext("No plain text version available for this message."));
                    }
                } else {
                    buffer.set_text(&crate::i18n::gettext("Unable to display message content."));
                }
            }
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("error");
        }
    }

    pub(crate) fn show_message_load_error(&self) {
        if self.imp().current_message_id.borrow().is_none() {
            if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
                label.set_visible(false);
            }
            if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
                stack.set_visible_child_name("placeholder");
            }
            return;
        }
        self.imp().webview_ready.set(false);
        self.imp().last_render_key.take();
        if let Some(webview) = self.imp().webview.borrow().as_ref() {
            webview.set_opacity(0.0);
        }
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(false);
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("load_error");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MailPrintDialogResult {
    Printed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageSearchDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageSearchBackend {
    WebKit,
    PlaintextFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageSearchStatus {
    EmptyQuery,
    NoDisplayedMessage,
    Pending,
    Found,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageSearchResult {
    pub(crate) generation: u64,
    pub(crate) backend: Option<MessageSearchBackend>,
    pub(crate) status: MessageSearchStatus,
    pub(crate) query: String,
    /// For WebKit this is zero while the asynchronous FindController result
    /// is pending. Fallback searches know the count immediately.
    pub(crate) match_count: usize,
    /// Match numbers are one-based to map directly to a future "1 of N"
    /// indicator. WebKit fills this after its found-text signal.
    pub(crate) current_match: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayedPlaintext {
    source_generation: u64,
    render_generation: u64,
    message_id: Option<String>,
    render_plan: Arc<PreparedMailBody>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextSearchMatch {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct ActiveMessageSearch {
    source_generation: u64,
    render_generation: u64,
    message_id: Option<String>,
    fallback_matches: Vec<TextSearchMatch>,
    result: MessageSearchResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedMailBody {
    body: String,
    visible_text: String,
    used_fallback: bool,
    has_remote_images: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BodyRenderKey {
    render_html: bool,
    remote_images_allowed: bool,
    preserve_image_layout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderRequestKey {
    source_generation: u64,
    render_html: bool,
    remote_images_allowed: bool,
    preserve_image_layout: bool,
    force_dark_mode: bool,
    mail_font_family: String,
    mail_font_size: u32,
    citation_highlighting: bool,
    citation_colors: [String; 5],
}

impl RenderRequestKey {
    fn body_key(&self) -> BodyRenderKey {
        BodyRenderKey {
            render_html: self.render_html,
            remote_images_allowed: self.remote_images_allowed,
            preserve_image_layout: self.preserve_image_layout,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedDocument {
    key: RenderRequestKey,
    document: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedMailRender {
    source_generation: u64,
    body_key: BodyRenderKey,
    plan: Arc<PreparedMailBody>,
    document: Option<CachedDocument>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderDecision {
    UseCachedDocument,
    RecomposeCachedBody,
    PrepareBody,
}

#[derive(Debug)]
enum RenderWork {
    Prepare {
        html_content: String,
        inline_images: Vec<InlineImage>,
        fallback_text: Option<String>,
    },
    Compose {
        plan: Arc<PreparedMailBody>,
    },
}

#[derive(Debug)]
struct RenderJob {
    generation: u64,
    key: RenderRequestKey,
    work: RenderWork,
}

type RenderResult = (u64, RenderRequestKey, Arc<PreparedMailBody>, Arc<str>);

fn execute_render_job(job: RenderJob, current_generation: &AtomicU64) -> Option<RenderResult> {
    if current_generation.load(Ordering::Acquire) != job.generation {
        return None;
    }
    let generation = job.generation;
    let key = job.key;
    let (render_plan, document) = match job.work {
        RenderWork::Prepare {
            html_content,
            inline_images,
            fallback_text,
        } => {
            let html_content = resolve_cid_images(&html_content, &inline_images);
            if current_generation.load(Ordering::Acquire) != generation {
                return None;
            }
            let render_plan = Arc::new(prepare_mail_body(
                &html_content,
                fallback_text.as_deref(),
                key.render_html,
                key.remote_images_allowed,
                key.preserve_image_layout,
            ));
            if current_generation.load(Ordering::Acquire) != generation {
                return None;
            }
            let document = compose_sanitized_mail_document(
                &render_plan.body,
                key.render_html,
                key.remote_images_allowed,
                key.force_dark_mode,
                &key.mail_font_family,
                key.mail_font_size,
                key.citation_highlighting,
                &key.citation_colors,
            );
            (render_plan, document.into())
        }
        RenderWork::Compose { plan } => {
            let document = compose_sanitized_mail_document(
                &plan.body,
                key.render_html,
                key.remote_images_allowed,
                key.force_dark_mode,
                &key.mail_font_family,
                key.mail_font_size,
                key.citation_highlighting,
                &key.citation_colors,
            );
            (plan, document.into())
        }
    };
    (current_generation.load(Ordering::Acquire) == generation).then_some((
        generation,
        key,
        render_plan,
        document,
    ))
}

fn inline_image_sources_match(left: &[InlineImage], right: &[InlineImage]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.content_id == right.content_id
                && left.mime_type == right.mime_type
                && Arc::ptr_eq(&left.content, &right.content)
        })
}

fn advance_generation(current: u64) -> u64 {
    current.wrapping_add(1).max(1)
}

fn message_search_context_is_current(
    current_source_generation: u64,
    current_render_generation: u64,
    current_message_id: Option<&str>,
    search_source_generation: u64,
    search_render_generation: u64,
    search_message_id: Option<&str>,
) -> bool {
    current_source_generation == search_source_generation
        && current_render_generation == search_render_generation
        && current_message_id == search_message_id
}

fn plaintext_search_matches(text: &str, query: &str) -> Vec<TextSearchMatch> {
    let folded_query: Vec<char> = query
        .chars()
        .flat_map(|character| character.to_lowercase())
        .collect();
    if folded_query.is_empty() {
        return Vec::new();
    }

    let mut folded_text = Vec::new();
    let mut original_character = 0;
    for character in text.chars() {
        let original_range = (original_character, original_character + 1);
        folded_text.extend(
            character
                .to_lowercase()
                .map(|folded| (folded, original_range)),
        );
        original_character += 1;
    }

    let mut matches = Vec::new();
    let mut cursor = 0;
    while cursor + folded_query.len() <= folded_text.len() {
        let end = cursor + folded_query.len();
        if folded_text[cursor..end]
            .iter()
            .map(|(character, _)| *character)
            .eq(folded_query.iter().copied())
        {
            matches.push(TextSearchMatch {
                start: folded_text[cursor].1.0,
                end: folded_text[end - 1].1.1,
            });
            // Match WebKit's normal non-overlapping find behavior.
            cursor = end;
        } else {
            cursor += 1;
        }
    }
    matches
}

fn gtk_text_offset(offset: usize) -> i32 {
    offset.min(i32::MAX as usize) as i32
}

fn render_decision(cache: Option<&CachedMailRender>, key: &RenderRequestKey) -> RenderDecision {
    let Some(cache) = cache else {
        return RenderDecision::PrepareBody;
    };
    if cache.source_generation != key.source_generation || cache.body_key != key.body_key() {
        return RenderDecision::PrepareBody;
    }
    if cache
        .document
        .as_ref()
        .is_some_and(|document| document.key == *key)
    {
        RenderDecision::UseCachedDocument
    } else {
        RenderDecision::RecomposeCachedBody
    }
}

fn render_result_is_current(
    active_generation: u64,
    active_source_generation: u64,
    active_key: Option<&RenderRequestKey>,
    result_generation: u64,
    result_key: &RenderRequestKey,
) -> bool {
    active_generation == result_generation
        && active_source_generation == result_key.source_generation
        && active_key == Some(result_key)
}

fn webview_load_is_current(
    active_webview_generation: u64,
    render_generation: u64,
    expected_uri: Option<&str>,
    current_uri: Option<&str>,
) -> bool {
    if active_webview_generation != render_generation {
        return false;
    }
    match (expected_uri, current_uri) {
        (Some(expected), Some(current)) => current == expected || current == "about:blank",
        (Some(_), None) => false,
        (None, None) => true,
        _ => false,
    }
}

fn cached_plan_bytes(plan: &PreparedMailBody) -> usize {
    plan.body.len().saturating_add(plan.visible_text.len())
}

fn cache_mail_render(
    source_generation: u64,
    key: &RenderRequestKey,
    plan: Arc<PreparedMailBody>,
    document: Arc<str>,
) -> Option<CachedMailRender> {
    let plan_bytes = cached_plan_bytes(&plan);
    if plan_bytes > MAX_CACHED_DERIVED_BYTES {
        return None;
    }
    let document =
        (plan_bytes.saturating_add(document.len()) <= MAX_CACHED_DERIVED_BYTES).then(|| {
            CachedDocument {
                key: key.clone(),
                document,
            }
        });
    Some(CachedMailRender {
        source_generation,
        body_key: key.body_key(),
        plan,
        document,
    })
}

/// Apply the same sanitizer, remote-image policy and empty-body fallback on
/// every platform. Keeping this as a pure composition step makes it possible
/// to verify the privacy boundary without constructing GTK or WebKit objects.
fn prepare_mail_body(
    html_content: &str,
    fallback_text: Option<&str>,
    render_html: bool,
    remote_images_allowed: bool,
    preserve_image_layout: bool,
) -> PreparedMailBody {
    if !render_html {
        let visible_text = html_to_fallback_text(html_content);
        let used_fallback = visible_text.is_empty();
        let visible_text = if visible_text.is_empty() {
            readable_fallback_text(fallback_text)
        } else {
            visible_text
        };
        return PreparedMailBody {
            body: format!("<pre>{}</pre>", escape_html(&visible_text)),
            visible_text,
            used_fallback,
            has_remote_images: false,
        };
    }

    let sanitized_full = sanitize_html_with_options(html_content, true);
    let blocked = block_remote_images_with_options(
        &sanitize_html_with_options(html_content, false),
        preserve_image_layout,
    );
    let has_remote_images = blocked != sanitized_full
        || any_image_tag(&sanitized_full, |tag| {
            image_attribute_value(tag, "xlink:href").is_some_and(is_remote_uri_reference)
                || image_attribute_value(tag, "srcset").is_some_and(is_remote_srcset)
        });
    let sanitized = if remote_images_allowed {
        sanitized_full
    } else {
        blocked
    };
    let visible_text = sanitized_html_to_plain_text(&sanitized);
    let has_renderable_content = !visible_text.is_empty() || contains_renderable_image(&sanitized);

    if !has_renderable_content {
        let visible_text = readable_fallback_text(fallback_text);
        return PreparedMailBody {
            body: format!("<pre>{}</pre>", escape_html(&visible_text)),
            visible_text,
            used_fallback: true,
            has_remote_images,
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
        has_remote_images,
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

pub(crate) fn detect_desktop_font_family() -> String {
    if gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup("org.gnome.desktop.interface", true))
        .is_some()
    {
        let s = gio::Settings::new("org.gnome.desktop.interface");
        let font_name = s.string("font-name");
        if let Some(idx) = font_name.rfind(' ') {
            let (fam, _) = font_name.split_at(idx);
            let fam = fam.trim();
            if !fam.is_empty() {
                return fam.to_string();
            }
        } else if !font_name.trim().is_empty() {
            return font_name.trim().to_string();
        }
    }
    "Cantarell".to_string()
}

pub(crate) fn safe_css_font_family(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 120
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || " ,-_'\"".contains(character)))
    {
        "Cantarell, \"Adwaita Sans\", system-ui, -apple-system, sans-serif".to_string()
    } else if value == "system-ui, sans-serif" || value == "sans-serif" {
        "Cantarell, \"Adwaita Sans\", system-ui, -apple-system, sans-serif".to_string()
    } else if !value.contains("Cantarell") && !value.contains("Adwaita Sans") && !value.contains("sans-serif") {
        format!("{value}, Cantarell, \"Adwaita Sans\", sans-serif")
    } else if !value.contains("Cantarell") && !value.contains("Adwaita Sans") {
        format!("Cantarell, \"Adwaita Sans\", {value}")
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

#[cfg(target_os = "linux")]
fn renderer_content_uri(generation: u64) -> String {
    format!("about:blank#meridian-mail-render-{generation}")
}

#[cfg(target_os = "linux")]
fn is_internal_renderer_uri(value: &str) -> bool {
    value.eq_ignore_ascii_case("about:blank")
        || value.starts_with("about:blank#meridian-mail-render-")
}

pub fn extract_email_address(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.find('<') {
        if let Some(end) = trimmed.rfind('>') {
            if start < end {
                return trimmed[start + 1..end].trim().to_lowercase();
            }
        }
    }
    trimmed.to_lowercase()
}

fn sender_in_contacts(sender: Option<&str>) -> bool {
    let Some(sender) = sender else {
        return false;
    };
    let email_to_check = extract_email_address(sender);
    if email_to_check.is_empty() {
        return false;
    }

    let contacts =
        crate::services::contact_store_service::ContactStoreService::new().load_contacts();
    contacts.into_iter().any(|record| {
        let contact = record.contact;
        if contact.email.trim().eq_ignore_ascii_case(&email_to_check) {
            return true;
        }
        contact
            .emails
            .iter()
            .any(|e| e.trim().eq_ignore_ascii_case(&email_to_check))
    })
}

fn safe_css_color(color: &str, fallback: &str) -> String {
    let trimmed = color.trim();
    if (trimmed.starts_with('#')
        && (trimmed.len() == 4 || trimmed.len() == 7 || trimmed.len() == 9))
        && trimmed[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        trimmed.to_string()
    } else {
        fallback.to_string()
    }
}

fn blockquote_citation_css(citation_highlighting: bool, citation_colors: &[String; 5]) -> String {
    if !citation_highlighting {
        return "blockquote { border-left: 3px solid #888a85; margin: 8px 0; padding-left: 12px; opacity: 0.85; }".to_string();
    }
    let defaults = ["#729fcf", "#ad7fa8", "#8ae234", "#fcaf3e", "#e9b96e"];
    let c0 = safe_css_color(&citation_colors[0], defaults[0]);
    let c1 = safe_css_color(&citation_colors[1], defaults[1]);
    let c2 = safe_css_color(&citation_colors[2], defaults[2]);
    let c3 = safe_css_color(&citation_colors[3], defaults[3]);
    let c4 = safe_css_color(&citation_colors[4], defaults[4]);

    format!(
        "blockquote {{ border-left: 3px solid {c0}; margin: 8px 0; padding-left: 12px; opacity: 0.85; }} \
         blockquote blockquote {{ border-left: 3px solid {c1}; }} \
         blockquote blockquote blockquote {{ border-left: 3px solid {c2}; }} \
         blockquote blockquote blockquote blockquote {{ border-left: 3px solid {c3}; }} \
         blockquote blockquote blockquote blockquote blockquote {{ border-left: 3px solid {c4}; }}"
    )
}

pub(crate) const USER_MAIL_OVERRIDE_CSS: &str = r#"
.preheader, .preview-text, .email-preheader, .email-preview, .mso-hide,
*[class*="preheader" i], *[id*="preheader" i],
*[class*="pre-header" i], *[id*="pre-header" i],
*[class*="preview-text" i], *[id*="preview-text" i],
*[class*="preview_text" i], *[id*="preview_text" i],
*[class*="email-preview" i], *[id*="email-preview" i],
*[class*="mso-hide" i],
div[style*="display:none" i], div[style*="display: none" i],
div[style*="visibility:hidden" i], div[style*="visibility: hidden" i],
div[style*="mso-hide:all" i], div[style*="mso-hide: all" i],
div[style*="font-size:1px" i], div[style*="font-size: 1px" i],
div[style*="max-height:0" i], div[style*="max-height: 0" i],
span[style*="display:none" i], span[style*="display: none" i],
span[style*="visibility:hidden" i], span[style*="visibility: hidden" i],
span[style*="mso-hide:all" i], span[style*="mso-hide: all" i],
span[style*="font-size:1px" i], span[style*="font-size: 1px" i],
span[style*="max-height:0" i], span[style*="max-height: 0" i] {
  display: none !important;
  visibility: hidden !important;
  opacity: 0 !important;
  height: 0 !important;
  max-height: 0 !important;
  min-height: 0 !important;
  width: 0 !important;
  max-width: 0 !important;
  min-width: 0 !important;
  line-height: 0 !important;
  font-size: 0 !important;
  overflow: hidden !important;
  mso-hide: all !important;
  pointer-events: none !important;
  margin: 0 !important;
  padding: 0 !important;
}

* {
  min-width: 0 !important;
}
html {
  width: 100% !important;
  max-width: 100% !important;
  min-width: 0 !important;
  min-height: 0 !important;
  height: auto !important;
  margin: 0 !important;
  padding: 0 !important;
  box-sizing: border-box !important;
  overflow-x: auto !important;
  -webkit-text-size-adjust: 100%;
}
body {
  width: 100% !important;
  max-width: 100% !important;
  min-width: 0 !important;
  min-height: 0 !important;
  height: auto !important;
  margin: 0 !important;
  padding: 12px 16px !important;
  box-sizing: border-box !important;
  overflow-x: auto !important;
  -webkit-text-size-adjust: 100%;
  word-wrap: break-word !important;
}
table, div, p, span, a, td, th, center, article, section, header, footer {
  max-width: 100% !important;
  overflow-wrap: anywhere !important;
  word-break: break-word !important;
}
h1, h2, h3, h4, h5, h6 {
  line-height: 1.25 !important;
}
img, video, iframe, canvas, svg {
  max-width: 100%;
  height: auto;
}
img[data-meridian-placeholder] {
  box-sizing: border-box !important;
  background-color: rgba(127, 127, 127, 0.08) !important;
  border: 1px dashed rgba(127, 127, 127, 0.28) !important;
  border-radius: 6px !important;
  object-fit: scale-down !important;
  object-position: center !important;
  vertical-align: middle !important;
}
img[data-meridian-placeholder]:not([width]):not([style*="width"]) {
  min-width: 32px;
  min-height: 32px;
}
img[data-meridian-placeholder][width="1"],
img[data-meridian-placeholder][height="1"],
img[data-meridian-placeholder][width="0"],
img[data-meridian-placeholder][height="0"] {
  display: none !important;
  width: 0 !important;
  height: 0 !important;
}
table {
  border-collapse: collapse !important;
  table-layout: fixed !important;
  max-width: 100% !important;
  min-width: 0 !important;
}
table[width], table[style*="width"] {
  max-width: 100% !important;
  min-width: 0 !important;
}
td[width], td[style*="width"], th[width], th[style*="width"],
div[style*="width"], div[width] {
  max-width: 100% !important;
  min-width: 0 !important;
}
body > table, body > div, body > center, body > article, body > main,
center > table {
  max-width: 100% !important;
  min-width: 0 !important;
  margin-left: auto !important;
  margin-right: auto !important;
}
@media (max-width: 768px), screen and (max-width: 768px) {
  body > table, body > div, body > center, body > article, body > main,
  center > table {
    margin-left: 0 !important;
    margin-right: 0 !important;
  }
  td[class*="col" i], th[class*="col" i],
  td[class*="stack" i], th[class*="stack" i],
  td[class*="column" i], th[class*="column" i],
  *[class*="mobile" i], *[class*="fluid" i],
  *[class*="responsive" i] {
    display: block !important;
    width: 100% !important;
    max-width: 100% !important;
    min-width: 0 !important;
    float: none !important;
    box-sizing: border-box !important;
  }
}
@media (max-width: 600px), screen and (max-width: 600px) {
  html, body {
    padding: 8px !important;
  }
  center {
    max-width: 100% !important;
    min-width: 0 !important;
  }
}
@media (max-width: 400px), screen and (max-width: 400px) {
  html, body {
    padding: 4px !important;
  }
  table, tbody, tr, td, th, div, p, span, a {
    max-width: 100% !important;
    overflow-wrap: anywhere !important;
    word-break: break-word !important;
  }
}
pre, code {
  white-space: pre-wrap !important;
  word-wrap: break-word !important;
  font-family: inherit !important;
  font-size: inherit;
  margin: 0;
}
a {
  color: #3584e4;
  text-decoration: underline;
}
"#;

fn parse_hex_color(token: &str) -> Option<(u8, u8, u8)> {
    let s = token.strip_prefix('#')?;
    let hex: String = s.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() == 3 {
        let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
        let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
        let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
        Some((r, g, b))
    } else if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some((r, g, b))
    } else {
        None
    }
}

fn color_luminance(r: u8, g: u8, b: u8) -> u32 {
    (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000
}

fn is_light_color_token(token: &str) -> bool {
    let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '#');
    if t.eq_ignore_ascii_case("white")
        || t.eq_ignore_ascii_case("whitesmoke")
        || t.eq_ignore_ascii_case("snow")
        || t.eq_ignore_ascii_case("ghostwhite")
        || t.eq_ignore_ascii_case("aliceblue")
        || t.eq_ignore_ascii_case("ivory")
    {
        return true;
    }
    if let Some((r, g, b)) = parse_hex_color(t) {
        return color_luminance(r, g, b) > 160;
    }
    false
}

fn is_dark_color_token(token: &str) -> bool {
    let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '#');
    if t.eq_ignore_ascii_case("black") {
        return true;
    }
    if let Some((r, g, b)) = parse_hex_color(t) {
        return color_luminance(r, g, b) < 90;
    }
    false
}

fn has_explicit_light_background(html_lower: &str) -> bool {
    let mut search_idx = 0;
    while let Some(pos) = html_lower[search_idx..].find("bgcolor") {
        let abs_pos = search_idx + pos;
        let rest = &html_lower[abs_pos + 7..];
        let rest_trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '=');
        let val = if rest_trimmed.starts_with('"') || rest_trimmed.starts_with('\'') {
            let quote = rest_trimmed.chars().next().unwrap();
            rest_trimmed[1..].split(quote).next().unwrap_or("")
        } else {
            rest_trimmed.split_whitespace().next().unwrap_or("").split('>').next().unwrap_or("")
        };
        if is_light_color_token(val) {
            return true;
        }
        search_idx = abs_pos + 7;
    }

    let mut search_idx = 0;
    while let Some(pos) = html_lower[search_idx..].find("background") {
        let abs_pos = search_idx + pos;
        let mut rest = &html_lower[abs_pos + 10..];
        if rest.starts_with("-color") {
            rest = &rest[6..];
        }
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.starts_with(':') {
            let val_part = rest_trimmed[1..].trim_start();
            let end_pos = val_part
                .find(|c| c == ';' || c == '"' || c == '\'' || c == '}' || c == '>')
                .unwrap_or(val_part.len());
            let prop_val = &val_part[..end_pos];
            for token in prop_val.split_whitespace() {
                if is_light_color_token(token) {
                    return true;
                }
            }
        }
        search_idx = abs_pos + 10;
    }
    false
}

fn dark_media_query_overrides_background(html_lower: &str) -> bool {
    let Some(mq_pos) = html_lower.find("prefers-color-scheme") else {
        return false;
    };
    let block = match html_lower[mq_pos..].find("</style>") {
        Some(end) => &html_lower[mq_pos..mq_pos + end],
        None => {
            let max_len = 2000.min(html_lower.len() - mq_pos);
            &html_lower[mq_pos..mq_pos + max_len]
        }
    };
    let mut search_idx = 0;
    while let Some(pos) = block[search_idx..].find("background") {
        let abs_pos = search_idx + pos;
        let mut rest = &block[abs_pos + 10..];
        if rest.starts_with("-color") {
            rest = &rest[6..];
        }
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.starts_with(':') {
            let val_part = rest_trimmed[1..].trim_start();
            let end_pos = val_part
                .find(|c| c == ';' || c == '"' || c == '\'' || c == '}' || c == '>')
                .unwrap_or(val_part.len());
            let prop_val = &val_part[..end_pos];
            for token in prop_val.split_whitespace() {
                if is_dark_color_token(token) {
                    return true;
                }
            }
        }
        search_idx = abs_pos + 10;
    }
    false
}

fn declares_dark_support(html_lower: &str) -> bool {
    if html_lower.contains("prefers-color-scheme") && html_lower.contains("dark") {
        return true;
    }
    for tag in ["color-scheme", "supported-color-schemes"] {
        let mut search_idx = 0;
        while let Some(pos) = html_lower[search_idx..].find(tag) {
            let abs_pos = search_idx + pos;
            let check_len = 150.min(html_lower.len() - abs_pos);
            let snippet = &html_lower[abs_pos..abs_pos + check_len];
            if snippet.contains("dark") {
                return true;
            }
            search_idx = abs_pos + tag.len();
        }
    }
    false
}

fn has_explicit_dark_background(html_lower: &str) -> bool {
    for tag in ["<body", "<html"] {
        if let Some(pos) = html_lower.find(tag) {
            if let Some(tag_end) = html_lower[pos..].find('>') {
                let snippet = &html_lower[pos..pos + tag_end];
                if let Some(bg_pos) = snippet.find("bgcolor") {
                    let rest = &snippet[bg_pos + 7..];
                    let rest_trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '=');
                    let val = rest_trimmed.trim_matches(|c: char| c == '"' || c == '\'');
                    let token = val.split_whitespace().next().unwrap_or("");
                    if is_dark_color_token(token) {
                        return true;
                    }
                }
                if let Some(bg_pos) = snippet.find("background") {
                    let mut rest = &snippet[bg_pos + 10..];
                    if rest.starts_with("-color") {
                        rest = &rest[6..];
                    }
                    let rest_trimmed = rest.trim_start();
                    if rest_trimmed.starts_with(':') {
                        let val_part = rest_trimmed[1..].trim_start();
                        let end_pos = val_part.find(';').unwrap_or(val_part.len());
                        for token in val_part[..end_pos].split_whitespace() {
                            if is_dark_color_token(token) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

pub(crate) fn has_effective_native_dark_mode_from_lower(lower: &str) -> bool {
    if has_explicit_light_background(lower) {
        dark_media_query_overrides_background(lower)
    } else {
        declares_dark_support(lower) || has_explicit_dark_background(lower)
    }
}

pub(crate) fn has_effective_native_dark_mode(sanitized: &str) -> bool {
    has_effective_native_dark_mode_from_lower(&sanitized.to_ascii_lowercase())
}

fn compose_sanitized_mail_document(
    sanitized: &str,
    render_html: bool,
    remote_images_allowed: bool,
    force_dark_mode: bool,
    mail_font_family: &str,
    mail_font_size: u32,
    citation_highlighting: bool,
    citation_colors: &[String; 5],
) -> String {
    let mail_font_family = safe_css_font_family(mail_font_family);
    let mail_font_size = safe_mail_font_size(mail_font_size);
    let lower = sanitized.to_ascii_lowercase();
    let has_native_dark_mode = has_effective_native_dark_mode_from_lower(&lower);

    let (body_css, dark_inversion_css) = if force_dark_mode {
        if !render_html || has_native_dark_mode {
            (
                r#"
  html {
    color-scheme: dark;
    background-color: transparent !important;
    color: #eeeeec;
  }
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    color: inherit;
    background-color: #1e1e1e !important;
  }
"#,
                "",
            )
        } else {
            (
                r#"
  html {
    color-scheme: dark;
    background-color: #e1e1e1 !important;
    color: #2e3436;
    filter: invert(100%) hue-rotate(180deg);
    -webkit-filter: invert(100%) hue-rotate(180deg);
  }
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    color: inherit;
    background-color: #e1e1e1 !important;
  }
"#,
                r#"
  img, video, canvas, svg, iframe, input[type="image"] {
    filter: invert(100%) hue-rotate(180deg) !important;
    -webkit-filter: invert(100%) hue-rotate(180deg) !important;
  }
"#,
            )
        }
    } else {
        (
            r#"
  html {
    color-scheme: light;
    background-color: transparent !important;
    color: #2e3436;
  }
  body {
    font-family: __FONT_FAMILY__;
    font-size: __FONT_SIZE__px;
    line-height: 1.6;
    color: inherit;
    background-color: #ffffff !important;
  }
"#,
            "",
        )
    };
    let body_css = body_css
        .replace("__FONT_FAMILY__", &mail_font_family)
        .replace("__FONT_SIZE__", &mail_font_size.to_string());
    let citation_css = blockquote_citation_css(citation_highlighting, citation_colors);
    let remote_images_css = if remote_images_allowed {
        ""
    } else {
        " img[src^=\"http://\"], img[src^=\"https://\"], img[src^=\"//\"], img[srcset], source[srcset], img[xlink\\:href^=\"http://\"], img[xlink\\:href^=\"https://\"], img[xlink\\:href^=\"//\"] { display: none !important; }"
    };
    let baseline_style_tag = format!("<style>{body_css}</style>");
    let override_style_tag = format!(
        "<style>{USER_MAIL_OVERRIDE_CSS} {citation_css}{remote_images_css}{dark_inversion_css}</style>"
    );
    let style_tag = format!("{baseline_style_tag}{override_style_tag}");

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
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; base-uri 'none'; form-action 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src {image_sources}; font-src data:; connect-src 'none'; media-src 'none'; frame-src 'none'; object-src 'none'; worker-src 'none'\">"
    );

    let has_html_root = render_html && lower.contains("<html");

    if has_html_root {
        // Put the application baseline before the mail's own <head> rules,
        // and put responsive overrides/preheader suppression at the end of <head>
        // so our rules come AFTER the email's own <style> blocks in document order.
        // This guarantees that the viewer baseline provides default typography
        // without stomping mail colors, while max-width: 100%, preheader suppression,
        // and mobile media queries win in specificity over author fixed widths.
        if let Some(head_close) = lower.find("</head>") {
            let (before_close, after_close) = sanitized.split_at(head_close);
            if let Some(head_start) = lower.find("<head") {
                if let Some(head_end_offset) = lower[head_start..].find('>') {
                    let open_insertion = head_start + head_end_offset + 1;
                    let (head_open, inside_head) = before_close.split_at(open_insertion);
                    return format!("{head_open}{security_tag}{baseline_style_tag}{inside_head}{override_style_tag}{after_close}");
                }
            }
            return format!("{before_close}{security_tag}{baseline_style_tag}{override_style_tag}{after_close}");
        }
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
    let fallback = crate::i18n::gettext("(The message has no displayable content.)");
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or(fallback)
}

/// Return whether a mail body contains an external image source that is
/// subject to the remote-content decision.  Inline `cid:` and `data:` images
/// intentionally do not trigger the privacy banner.
#[cfg(test)]
fn contains_remote_image_reference(value: &str) -> bool {
    let sanitized = crate::services::html_sanitizer::sanitize_html(value);
    crate::services::html_sanitizer::block_remote_images(&sanitized) != sanitized
        || any_image_tag(&sanitized, |tag| {
            image_attribute_value(tag, "xlink:href").is_some_and(is_remote_uri_reference)
        })
}

fn contains_renderable_image(value: &str) -> bool {
    any_image_tag(value, |tag| {
        image_attribute_value(tag, "data-meridian-placeholder").is_none()
            && ["src", "xlink:href"].iter().any(|attribute| {
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
        CachedMailRender, RenderDecision, RenderJob, RenderRequestKey, RenderWork,
        advance_generation, cache_mail_render, compose_sanitized_mail_document,
        contains_remote_image_reference, effective_mail_base_uri, execute_render_job,
        html_to_fallback_text, inline_image_sources_match, message_search_context_is_current,
        plaintext_search_matches, prepare_mail_body, readable_fallback_text, render_decision,
        render_result_is_current, safe_css_font_family, safe_mail_font_size,
        webview_load_is_current, extract_email_address,
    };
    use crate::services::inline_content_service::InlineImage;
    use glib::subclass::prelude::ObjectSubclassIsExt;
    use gtk::prelude::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn test_citation_colors() -> [String; 5] {
        [
            "#729fcf".to_string(),
            "#ad7fa8".to_string(),
            "#8ae234".to_string(),
            "#fcaf3e".to_string(),
            "#e9b96e".to_string(),
        ]
    }

    fn render_key(source_generation: u64) -> RenderRequestKey {
        RenderRequestKey {
            source_generation,
            render_html: true,
            remote_images_allowed: false,
            preserve_image_layout: true,
            force_dark_mode: false,
            mail_font_family: "system-ui, sans-serif".to_string(),
            mail_font_size: 16,
            citation_highlighting: true,
            citation_colors: test_citation_colors(),
        }
    }

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
        let empty = readable_fallback_text(Some("   "));
        assert!(
            empty == "(The message has no displayable content.)"
                || empty == "(Die Nachricht enthält keinen darstellbaren Inhalt.)"
        );
    }

    #[test]
    fn plaintext_message_search_is_case_insensitive_and_uses_character_offsets() {
        let matches = plaintext_search_matches("Hallo hallo HALLO", "hAlLo");
        assert_eq!(
            matches,
            vec![
                super::TextSearchMatch { start: 0, end: 5 },
                super::TextSearchMatch { start: 6, end: 11 },
                super::TextSearchMatch { start: 12, end: 17 },
            ]
        );
    }

    #[test]
    fn plaintext_message_search_handles_unicode_without_byte_offsets() {
        let matches = plaintext_search_matches("Ärger ärger", "ÄRGER");
        assert_eq!(
            matches,
            vec![
                super::TextSearchMatch { start: 0, end: 5 },
                super::TextSearchMatch { start: 6, end: 11 },
            ]
        );
    }

    #[test]
    fn plaintext_message_search_is_non_overlapping() {
        assert_eq!(
            plaintext_search_matches("aaaa", "aa"),
            vec![
                super::TextSearchMatch { start: 0, end: 2 },
                super::TextSearchMatch { start: 2, end: 4 },
            ]
        );
    }

    #[test]
    fn stale_message_search_context_is_rejected_after_selection_or_render_change() {
        assert!(message_search_context_is_current(
            4,
            9,
            Some("message-a"),
            4,
            9,
            Some("message-a"),
        ));
        assert!(!message_search_context_is_current(
            5,
            9,
            Some("message-a"),
            4,
            9,
            Some("message-a"),
        ));
        assert!(!message_search_context_is_current(
            4,
            10,
            Some("message-a"),
            4,
            9,
            Some("message-a"),
        ));
        assert!(!message_search_context_is_current(
            4,
            9,
            Some("message-b"),
            4,
            9,
            Some("message-a"),
        ));
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
            true,
        );

        assert!(!plan.used_fallback);
        assert!(plan.has_remote_images);
        assert_eq!(plan.visible_text, "Hallo");
        assert!(!plan.body.contains("cdn.example"));
        assert!(plan.body.contains("data-meridian-placeholder=\"true\""));
        assert!(plan.body.contains("cid:logo@example.org"));
        assert!(plan.body.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn render_plan_allows_remote_images_when_approved() {
        let plan = prepare_mail_body(
            r#"<p>Hallo</p><img src="//cdn.example/remote.png" srcset="//cdn.example/remote-2x.png 2x"><table background="//cdn.example/bg.png"><tr><td style="background-image: url('//cdn.example/table.png')">Text</td></tr></table>"#,
            Some("Vorschau"),
            true,
            true,
            true,
        );

        assert!(!plan.used_fallback);
        assert!(plan.has_remote_images);
        assert!(plan.body.contains("https://cdn.example/remote.png"));
        assert!(plan.body.contains("srcset=\"https://cdn.example/remote-2x.png 2x\""));
        assert!(plan.body.contains("background=\"https://cdn.example/bg.png\""));
        assert!(plan.body.contains("background-image: url('https://cdn.example/table.png')"));
    }

    #[test]
    fn render_plan_uses_preview_when_html_has_no_renderable_content() {
        let plan = prepare_mail_body(
            r#"<style>.tracking { display: none; }</style><img src="https://cdn.example/only.png">"#,
            Some("  Lesbare Vorschau  "),
            true,
            false,
            true,
        );

        assert!(plan.used_fallback);
        assert!(plan.has_remote_images);
        assert_eq!(plan.visible_text, "Lesbare Vorschau");
        assert!(plan.body.contains("Lesbare Vorschau"));
        assert!(!plan.body.contains("cdn.example"));
    }

    #[test]
    fn render_plan_inserts_placeholder_when_preserving_layout() {
        let plan = prepare_mail_body(
            r#"<p>Text</p><img src="https://cdn.example/photo.png" width="400" height="200" alt="Foto">"#,
            None,
            true,
            false,
            true,
        );
        assert!(!plan.used_fallback);
        assert!(plan.has_remote_images);
        assert!(!plan.body.contains("cdn.example"));
        assert!(plan.body.contains("data-meridian-placeholder=\"true\""));
        assert!(plan.body.contains("width=\"400\""));
        assert!(plan.body.contains("height=\"200\""));
        assert!(plan.body.contains("alt=\"Foto\""));
        assert!(plan.body.contains("title=\"Foto\""));
    }

    #[test]
    fn render_plan_drops_image_when_not_preserving_layout() {
        let plan = prepare_mail_body(
            r#"<p>Text</p><img src="https://cdn.example/photo.png" width="400" height="200" alt="Foto">"#,
            None,
            true,
            false,
            false,
        );
        assert!(!plan.used_fallback);
        assert!(plan.has_remote_images);
        assert!(!plan.body.contains("cdn.example"));
        assert!(!plan.body.contains("data-meridian-placeholder"));
        assert!(!plan.body.contains("<img"));
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
            true,
            &test_citation_colors(),
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
            "Cantarell, \"Adwaita Sans\", system-ui, -apple-system, sans-serif"
        );
        assert_eq!(
            safe_css_font_family("</style><script>bad</script>"),
            "Cantarell, \"Adwaita Sans\", system-ui, -apple-system, sans-serif"
        );
        assert_eq!(safe_mail_font_size(1), 8);
        assert_eq!(safe_mail_font_size(100), 48);
    }

    #[test]
    fn render_generation_never_uses_zero_as_a_live_value() {
        assert_eq!(advance_generation(0), 1);
        assert_eq!(advance_generation(u64::MAX), 1);
    }

    #[test]
    fn inline_payload_identity_is_checked_without_copying_bytes() {
        let bytes: Arc<[u8]> = vec![1, 2, 3].into();
        let first = InlineImage::new("logo", "image/png", bytes.clone());
        let same = first.clone();
        assert!(inline_image_sources_match(&[first], &[same]));

        let left = InlineImage::new("logo", "image/png", vec![1, 2, 3]);
        let right = InlineImage::new("logo", "image/png", vec![1, 2, 3]);
        assert!(!inline_image_sources_match(&[left], &[right]));
    }

    #[test]
    fn render_worker_resolves_cid_content_and_drops_stale_jobs() {
        let generation = AtomicU64::new(2);
        let stale = RenderJob {
            generation: 1,
            key: render_key(1),
            work: RenderWork::Prepare {
                html_content: "<img src=\"cid:logo\">".to_string(),
                inline_images: vec![InlineImage::new("logo", "image/png", b"PNG".to_vec())],
                fallback_text: Some("Logo".to_string()),
            },
        };
        assert!(execute_render_job(stale, &generation).is_none());

        let current = RenderJob {
            generation: 2,
            key: render_key(2),
            work: RenderWork::Prepare {
                html_content: "<img src=\"cid:logo\">".to_string(),
                inline_images: vec![InlineImage::new("logo", "image/png", b"PNG".to_vec())],
                fallback_text: Some("Logo".to_string()),
            },
        };
        let (_, _, plan, document) =
            execute_render_job(current, &generation).expect("current work should finish");
        assert!(plan.body.contains("data:image/png;base64,UE5H"));
        assert!(document.contains("data:image/png;base64,UE5H"));
    }

    #[test]
    fn render_cache_reuses_documents_and_only_recomposes_style_changes() {
        let key = render_key(7);
        let plan = prepare_mail_body("<p>Hallo</p>", Some("Hallo"), true, false, true);
        let cache = cache_mail_render(
            7,
            &key,
            Arc::new(plan),
            Arc::<str>::from("<html>Hallo</html>"),
        )
        .expect("small derived content should be cached");

        assert_eq!(
            render_decision(Some(&cache), &key),
            RenderDecision::UseCachedDocument
        );

        let mut font_change = key.clone();
        font_change.mail_font_size = 18;
        assert_eq!(
            render_decision(Some(&cache), &font_change),
            RenderDecision::RecomposeCachedBody
        );

        let mut policy_change = key.clone();
        policy_change.remote_images_allowed = true;
        assert_eq!(
            render_decision(Some(&cache), &policy_change),
            RenderDecision::PrepareBody
        );
        assert_eq!(
            render_decision(None::<&CachedMailRender>, &key),
            RenderDecision::PrepareBody
        );
    }

    #[test]
    fn stale_render_results_cannot_replace_the_active_message() {
        let active_key = render_key(11);
        assert!(render_result_is_current(
            4,
            11,
            Some(&active_key),
            4,
            &active_key,
        ));
        assert!(!render_result_is_current(
            4,
            11,
            Some(&active_key),
            3,
            &active_key,
        ));

        let stale_source = render_key(10);
        assert!(!render_result_is_current(
            4,
            11,
            Some(&active_key),
            4,
            &stale_source,
        ));
    }

    #[test]
    fn webview_load_requires_the_exact_current_internal_uri() {
        let uri = "about:blank#meridian-mail-render-7";
        assert!(webview_load_is_current(7, 7, Some(uri), Some(uri)));
        assert!(!webview_load_is_current(7, 7, Some(uri), None));
        assert!(!webview_load_is_current(6, 7, Some(uri), Some(uri)));
        assert!(!webview_load_is_current(
            7,
            7,
            Some(uri),
            Some("about:blank#meridian-mail-render-6")
        ));
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
            true,
            &test_citation_colors(),
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

    #[test]
    fn citation_highlighting_css_generates_nested_levels() {
        let colors = [
            "#111111".to_string(),
            "#222222".to_string(),
            "#333333".to_string(),
            "#444444".to_string(),
            "#555555".to_string(),
        ];
        let doc_with_highlighting = compose_sanitized_mail_document(
            "<blockquote>Level 1<blockquote>Level 2</blockquote></blockquote>",
            true,
            false,
            false,
            "sans-serif",
            14,
            true,
            &colors,
        );
        assert!(doc_with_highlighting.contains("blockquote { border-left: 3px solid #111111;"));
        assert!(doc_with_highlighting.contains("blockquote blockquote { border-left: 3px solid #222222; }"));
        assert!(doc_with_highlighting.contains("blockquote blockquote blockquote { border-left: 3px solid #333333; }"));
        assert!(doc_with_highlighting.contains("blockquote blockquote blockquote blockquote { border-left: 3px solid #444444; }"));
        assert!(doc_with_highlighting.contains("blockquote blockquote blockquote blockquote blockquote { border-left: 3px solid #555555; }"));

        let doc_without_highlighting = compose_sanitized_mail_document(
            "<blockquote>Level 1</blockquote>",
            true,
            false,
            false,
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc_without_highlighting.contains("blockquote { border-left: 3px solid #888a85;"));
        assert!(!doc_without_highlighting.contains("blockquote blockquote {"));
    }

    #[test]
    fn citation_highlighting_handles_invalid_colors_safely() {
        let invalid_colors = [
            "red; color: blue".to_string(),
            "".to_string(),
            "invalid".to_string(),
            "#GGG".to_string(),
            "#1234567890".to_string(),
        ];
        let doc = compose_sanitized_mail_document(
            "<blockquote>Level 1</blockquote>",
            true,
            false,
            false,
            "sans-serif",
            14,
            true,
            &invalid_colors,
        );
        // Fallbacks to upstream defaults
        assert!(doc.contains("blockquote { border-left: 3px solid #729fcf;"));
        assert!(doc.contains("blockquote blockquote { border-left: 3px solid #ad7fa8; }"));
        assert!(!doc.contains("red; color: blue"));
    }

    #[test]
    fn mail_composed_document_contains_viewport_and_responsive_css() {
        let colors = test_citation_colors();
        let doc = compose_sanitized_mail_document(
            r#"<html><head></head><body><table width="800"><tr><td>Inhalt</td></tr></table></body></html>"#,
            true,
            false,
            false,
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc.contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">"));
        assert!(doc.contains("overflow-x: auto !important"));
        assert!(doc.contains("max-width: 100% !important"));
        assert!(doc.contains("min-width: 0 !important"));
        assert!(doc.contains("word-break: break-word !important"));
        assert!(doc.contains("table-layout: fixed !important"));
    }

    #[test]
    fn mail_composed_document_contains_preheader_suppression_rules() {
        let colors = test_citation_colors();
        let doc = compose_sanitized_mail_document(
            r#"<html><head><style>.own-rule { color: red; }</style></head><body><div class="preheader" style="display:none;font-size:1px;max-height:0px;">Strava</div><p>Hallo</p></body></html>"#,
            true,
            false,
            false,
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc.contains(".preheader, .preview-text"));
        assert!(doc.contains("div[style*=\"display:none\" i]"));
        assert!(doc.contains("div[style*=\"font-size:1px\" i]"));
        assert!(doc.contains("div[style*=\"max-height:0\" i]"));
        assert!(doc.contains("span[style*=\"display:none\" i]"));
        assert!(doc.contains("span[style*=\"font-size:1px\" i]"));
        assert!(doc.contains("span[style*=\"max-height:0\" i]"));
        assert!(doc.contains("display: none !important"));
        assert!(doc.contains("visibility: hidden !important"));
        assert!(doc.contains("mso-hide: all !important"));
        assert!(!doc.contains("font-size: 0px"), "Must not suppress font-size: 0px which is used by email column layouts");
        assert!(!doc.contains("font-size:0px"), "Must not suppress font-size:0px");
        assert!(!doc.contains("font-size: 0pt"), "Must not suppress font-size: 0pt");
        assert!(!doc.contains("font-size:0pt"), "Must not suppress font-size:0pt");
        assert!(!doc.contains("*[style*=\"max-height:0\""), "Must not use wildcard max-height:0 matching on all elements");
        // Ensure style_tag comes after the mail's own head rules
        let own_rule_pos = doc.find(".own-rule").unwrap();
        let override_style_pos = doc.find(".preheader, .preview-text").unwrap();
        assert!(override_style_pos > own_rule_pos);
    }

    #[test]
    fn mail_composed_document_contains_responsive_container_and_media_queries() {
        let colors = test_citation_colors();
        let doc = compose_sanitized_mail_document(
            r#"<html><body><div class="container"><table class="row"><tr><td class="col">Spalte</td></tr></table></div></body></html>"#,
            true,
            false,
            false,
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc.contains("body > table, body > div, body > center"));
        assert!(doc.contains("min-width: 0 !important"));
        assert!(doc.contains("@media (max-width: 768px), screen and (max-width: 768px)"));
        assert!(doc.contains("td[class*=\"col\" i]"));
    }

    #[test]
    fn test_extract_email_address_helper() {
        assert_eq!(extract_email_address("Alice <alice@example.com>"), "alice@example.com");
        assert_eq!(extract_email_address("<bob@example.com>"), "bob@example.com");
        assert_eq!(extract_email_address("charlie@example.com"), "charlie@example.com");
        assert_eq!(extract_email_address("  David <DAVID@EXAMPLE.COM>  "), "david@example.com");
        assert_eq!(extract_email_address(""), "");
    }

    #[test]
    fn test_mail_viewer_measures_can_shrink() {
        use gtk::prelude::*;
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();
        let (min_w, _, _, _) = viewer.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            min_w <= 160,
            "MailViewer minimum width must be <= 160px so it can fit narrow responsive panels, got {min_w}"
        );
    }

    #[test]
    fn test_mail_viewer_adapted_height() {
        use gtk::prelude::*;
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();

        // Short email: height fits within available parent height
        viewer.apply_adapted_height(120);
        assert_eq!(viewer.valign(), gtk::Align::Start);
        assert!(!viewer.vexpands());
        assert_eq!(viewer.height_request(), 120);

        // Long email: exceeds default available height (500)
        viewer.apply_adapted_height(1200);
        assert_eq!(viewer.valign(), gtk::Align::Fill);
        assert!(viewer.vexpands());
        assert_eq!(viewer.height_request(), -1);

        // Reset/cleared state
        viewer.clear();
        assert_eq!(viewer.valign(), gtk::Align::Fill);
        assert!(viewer.vexpands());
        assert_eq!(viewer.height_request(), -1);
    }


    #[test]
    fn test_mail_viewer_composed_document_dark_mode_css() {
        let colors = test_citation_colors();

        // 1. Standard HTML email in dark mode: Smart Inversion applied
        let doc_dark_inverted = compose_sanitized_mail_document(
            "<p>Hallo Dark</p>",
            true,
            false,
            true, // force_dark_mode = true
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc_dark_inverted.contains("color-scheme: dark"));
        assert!(doc_dark_inverted.contains("filter: invert(100%) hue-rotate(180deg)"));
        assert!(doc_dark_inverted.contains("background-color: #e1e1e1"));
        assert!(doc_dark_inverted.contains("img, video, canvas, svg, iframe, input[type=\"image\"]"));

        // 2. Native dark mode email (contains prefers-color-scheme): Direct dark styling applied, no inversion
        let doc_dark_native = compose_sanitized_mail_document(
            "<style>@media (prefers-color-scheme: dark) { body { color: white; } }</style><p>Hallo Dark</p>",
            true,
            false,
            true,
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc_dark_native.contains("color-scheme: dark"));
        assert!(doc_dark_native.contains("background-color: #1e1e1e"));
        assert!(doc_dark_native.contains("color: #eeeeec"));
        assert!(!doc_dark_native.contains("filter: invert(100%) hue-rotate(180deg)"));

        // 3. Plaintext email in dark mode (render_html = false): Direct dark styling
        let doc_dark_plain = compose_sanitized_mail_document(
            "Plain text content",
            false,
            false,
            true,
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc_dark_plain.contains("color-scheme: dark"));
        assert!(doc_dark_plain.contains("background-color: #1e1e1e"));
        assert!(doc_dark_plain.contains("color: #eeeeec"));
        assert!(!doc_dark_plain.contains("filter: invert(100%) hue-rotate(180deg)"));

        // 4. Light mode email (force_dark_mode = false)
        let doc_light = compose_sanitized_mail_document(
            "<p>Hallo Light</p>",
            true,
            false,
            false, // force_dark_mode = false
            "sans-serif",
            14,
            false,
            &colors,
        );
        assert!(doc_light.contains("color-scheme: light"));
        assert!(doc_light.contains("background-color: #ffffff"));
        assert!(doc_light.contains("color: #2e3436"));
        assert!(!doc_light.contains("filter: invert"));
    }

    #[test]
    fn test_mail_viewer_light_mode_override_and_message_reset() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let viewer = super::MailViewer::new();
        assert!(!viewer.is_light_mode_override());

        // Toggle override
        let toggled = viewer.toggle_light_mode_override();
        assert!(toggled);
        assert!(viewer.is_light_mode_override());
        assert!(!viewer.effective_dark_mode());

        // Explicit set
        viewer.set_light_mode_override(false);
        assert!(!viewer.is_light_mode_override());

        // Toggled again
        viewer.set_light_mode_override(true);
        assert!(viewer.is_light_mode_override());

        // Loading a new message must reset the light mode override
        viewer.load_html_for_message(Some("msg-1"), "<p>First</p>", None);
        // Explicitly set override on msg-1
        viewer.set_light_mode_override(true);
        assert!(viewer.is_light_mode_override());

        // Switch to msg-2
        viewer.load_html_for_message(Some("msg-2"), "<p>Second</p>", None);
        assert!(
            !viewer.is_light_mode_override(),
            "Switching message must reset light mode override"
        );
    }

    #[test]
    fn test_user_mail_override_css_has_card_padding() {
        assert!(
            super::USER_MAIL_OVERRIDE_CSS.contains("padding: 12px 16px !important;"),
            "body must have 12px 16px padding for libadwaita card preview"
        );
        assert!(
            super::USER_MAIL_OVERRIDE_CSS.contains("padding: 0 !important;"),
            "html root must have 0 padding"
        );
    }

    #[test]
    fn test_mail_viewer_clear_resets_state_to_placeholder() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();

        viewer.load_html_for_message(Some("msg-1"), "<p>Hello</p>", None);
        assert_eq!(viewer.imp().current_message_id.borrow().as_deref(), Some("msg-1"));

        viewer.clear();
        assert_eq!(viewer.imp().current_message_id.borrow().as_deref(), None);
        assert_eq!(viewer.imp().current_html.borrow().as_deref(), None);

        #[cfg(target_os = "linux")]
        {
            if let Some(stack) = viewer.imp().renderer_stack.borrow().as_ref() {
                assert_eq!(stack.visible_child_name().as_deref(), Some("placeholder"));
            }
        }
    }

    #[test]
    fn test_mail_viewer_prepare_for_loading_resets_stale_content() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();

        viewer.load_html_for_message(Some("msg-1"), "<p>Hello</p>", None);
        assert_eq!(viewer.imp().current_message_id.borrow().as_deref(), Some("msg-1"));

        viewer.prepare_for_loading(Some("msg-2"));
        assert_eq!(viewer.imp().current_message_id.borrow().as_deref(), Some("msg-2"));
        assert_eq!(viewer.imp().current_html.borrow().as_deref(), None);

        #[cfg(target_os = "linux")]
        {
            assert!(!viewer.imp().webview_ready.get());
        }
    }

    #[test]
    fn test_mail_viewer_error_handling_empty_state_and_load_error() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();

        // 1. Calling show_renderer_error when NO message is selected must not show the error state or label
        viewer.clear();
        viewer.show_renderer_error();
        #[cfg(target_os = "linux")]
        {
            if let Some(stack) = viewer.imp().renderer_stack.borrow().as_ref() {
                assert_eq!(stack.visible_child_name().as_deref(), Some("placeholder"));
            }
            if let Some(label) = viewer.imp().renderer_error_label.borrow().as_ref() {
                assert!(!label.is_visible(), "Error label must not be visible on empty state");
            }
        }

        // 2. When message is selected and show_renderer_error is called, error state is activated
        viewer.prepare_for_loading(Some("msg-err"));
        viewer.show_renderer_error();
        #[cfg(target_os = "linux")]
        {
            if let Some(stack) = viewer.imp().renderer_stack.borrow().as_ref() {
                assert_eq!(stack.visible_child_name().as_deref(), Some("error"));
            }
            if let Some(label) = viewer.imp().renderer_error_label.borrow().as_ref() {
                assert!(label.is_visible(), "Error label should be visible for render error");
            }
        }

        // 3. Clear must reset error label and stack to placeholder
        viewer.clear();
        #[cfg(target_os = "linux")]
        {
            if let Some(stack) = viewer.imp().renderer_stack.borrow().as_ref() {
                assert_eq!(stack.visible_child_name().as_deref(), Some("placeholder"));
            }
            if let Some(label) = viewer.imp().renderer_error_label.borrow().as_ref() {
                assert!(!label.is_visible(), "Error label must be hidden after clear()");
            }
        }

        // 4. show_message_load_error shows load_error status page and hides HTML error label
        viewer.prepare_for_loading(Some("msg-load-fail"));
        viewer.show_message_load_error();
        #[cfg(target_os = "linux")]
        {
            if let Some(stack) = viewer.imp().renderer_stack.borrow().as_ref() {
                assert_eq!(stack.visible_child_name().as_deref(), Some("load_error"));
            }
            if let Some(label) = viewer.imp().renderer_error_label.borrow().as_ref() {
                assert!(!label.is_visible(), "Error label must not be visible on load_error");
            }
        }
    }

    #[test]
    fn test_mail_viewer_zoom_operations_and_clamping() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();

        assert_eq!(viewer.zoom_level(), 1.0);
        viewer.zoom_in();
        assert!((viewer.zoom_level() - 1.1).abs() < 1e-5);
        viewer.zoom_out();
        assert!((viewer.zoom_level() - 1.0).abs() < 1e-5);

        // Clamping to max 3.0
        for _ in 0..30 {
            viewer.zoom_in();
        }
        assert_eq!(viewer.zoom_level(), 3.0);

        // Clamping to min 0.5
        for _ in 0..40 {
            viewer.zoom_out();
        }
        assert_eq!(viewer.zoom_level(), 0.5);

        // Reset to 1.0
        viewer.zoom_reset();
        assert_eq!(viewer.zoom_level(), 1.0);
    }

    #[test]
    fn test_user_mail_override_css_does_not_hide_background_color_transparent() {
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("style*=\"color:transparent"),
            "USER_MAIL_OVERRIDE_CSS must not hide elements matching color:transparent as it hides background-color:transparent"
        );
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("style*=\"color: transparent"),
            "USER_MAIL_OVERRIDE_CSS must not hide elements matching color: transparent"
        );
    }

    #[test]
    fn test_fideapi_html_renders_without_being_hidden() {
        let fide_path = std::path::Path::new("/tmp/meridian-test/fixture-a.eml");
        if let Ok(bytes) = std::fs::read(fide_path) {
            let parsed = crate::services::mail_store_service::parse_rfc822_bytes(
                "fide-test",
                "inbox",
                &bytes,
                false,
            );
            assert!(parsed.is_some(), "fideapi.eml must parse successfully");
            let parsed = parsed.unwrap();
            assert!(!parsed.html_body.is_empty(), "fideapi.eml must have an HTML body");

            let plan = super::prepare_mail_body(&parsed.html_body, None, true, false, true);
            assert!(!plan.used_fallback, "fideapi.eml must not fall back to plain text");
            assert!(!plan.body.is_empty(), "fideapi.eml sanitized body must not be empty");

            let doc = super::compose_sanitized_mail_document(
                &plan.body,
                true,
                false,
                false,
                "system-ui, sans-serif",
                16,
                true,
                &[
                    "#729FCF".to_string(),
                    "#AD7FA8".to_string(),
                    "#8AE234".to_string(),
                    "#FCAF3E".to_string(),
                    "#E9B96E".to_string(),
                ],
            );
            assert!(doc.contains("Sie haben einige Google-Kontodaten mit FideAPI geteilt") || doc.contains("Google-Kontodaten"));
            assert!(!doc.contains("style*=\"color:transparent"), "doc must not contain color:transparent override rule");
        }
    }

    #[test]
    fn test_flink_and_xai_render() {
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("font-size: 0px")
                && !super::USER_MAIL_OVERRIDE_CSS.contains("font-size:0px")
                && !super::USER_MAIL_OVERRIDE_CSS.contains("font-size: 0pt")
                && !super::USER_MAIL_OVERRIDE_CSS.contains("font-size:0pt"),
            "USER_MAIL_OVERRIDE_CSS must not hide elements with font-size: 0 as it destroys column layouts in MJML and email templates"
        );
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("*[style*=\"max-height:0\""),
            "USER_MAIL_OVERRIDE_CSS must not hide elements matching wildcard max-height:0 as it breaks tables"
        );

        for filename in &["flink.eml", "xai.eml"] {
            let path = std::path::Path::new("/tmp/meridian-test").join(filename);
            if let Ok(bytes) = std::fs::read(&path) {
                let parsed = crate::services::mail_store_service::parse_rfc822_bytes(
                    "test",
                    "inbox",
                    &bytes,
                    false,
                );
                assert!(parsed.is_some(), "{filename} must parse");
                let parsed = parsed.unwrap();
                let plan = super::prepare_mail_body(&parsed.html_body, None, true, false, true);
                assert!(!plan.used_fallback, "{filename} must not fall back to plain text");
                assert!(!plan.body.is_empty(), "{filename} body must not be empty");
                let doc = super::compose_sanitized_mail_document(
                    &plan.body,
                    true,
                    false,
                    false,
                    "system-ui, sans-serif",
                    16,
                    true,
                    &[
                        "#729FCF".to_string(),
                        "#AD7FA8".to_string(),
                        "#8AE234".to_string(),
                        "#FCAF3E".to_string(),
                        "#E9B96E".to_string(),
                    ],
                );
                if *filename == "flink.eml" {
                    assert!(
                        doc.contains("Asian Weeks") || doc.contains("Discover Asian Weeks"),
                        "flink.eml doc must contain its content"
                    );
                } else if *filename == "xai.eml" {
                    assert!(
                        doc.contains("Introducing the Voice Agent Builder") || doc.contains("Voice Agent Builder"),
                        "xai.eml doc must contain its content"
                    );
                }
            }
        }
    }

    #[test]
    fn test_karr_dump() {
        let path = std::path::Path::new("/tmp/meridian-test/fixture-d.eml");
        if let Ok(bytes) = std::fs::read(&path) {
            let parsed = crate::services::mail_store_service::parse_rfc822_bytes("test", "inbox", &bytes, false).unwrap();
            let plan = super::prepare_mail_body(&parsed.html_body, None, true, true, true);
            let colors = [
                "#729FCF".to_string(),
                "#AD7FA8".to_string(),
                "#8AE234".to_string(),
                "#FCAF3E".to_string(),
                "#E9B96E".to_string(),
            ];
            let doc = super::compose_sanitized_mail_document(
                &plan.body,
                true,
                false,
                true,
                "system-ui, sans-serif",
                16,
                true,
                &colors,
            );
            std::fs::write("/tmp/karr_rendered.html", &doc).unwrap();
        }
    }

    #[test]
    fn test_mail_viewer_banner_animations_are_disabled() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let viewer = super::MailViewer::new();
        for banner in [&viewer.imp().banner, &viewer.imp().trash_banner, &viewer.imp().junk_banner] {
            let revealer = banner
                .first_child()
                .and_then(|c| c.downcast::<gtk::Revealer>().ok())
                .expect("Banner must have an internal GtkRevealer child");
            assert_eq!(revealer.transition_duration(), 0, "Banner animation duration must be 0ms");
            assert_eq!(revealer.transition_type(), gtk::RevealerTransitionType::None, "Banner transition type must be None");
        }
        assert_eq!(viewer.imp().banner.title().as_str(), "Remote images blocked");
        fn find_labels(widget: &gtk::Widget, labels: &mut Vec<gtk::Label>) {
            if let Some(lbl) = widget.downcast_ref::<gtk::Label>() {
                labels.push(lbl.clone());
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                find_labels(&c, labels);
                child = c.next_sibling();
            }
        }
        let mut labels = Vec::new();
        find_labels(viewer.imp().banner.upcast_ref(), &mut labels);
        for lbl in &labels {
            assert!(!lbl.wraps(), "Banner labels must not wrap");
            assert!(lbl.is_single_line_mode(), "Banner labels must use single-line mode");
        }
    }

    #[test]
    fn test_brevo_mail_renders_without_oversized_images() {
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("table[width], table[style*=\"width\"] {\n  width: 100% !important;"),
            "USER_MAIL_OVERRIDE_CSS must not force fixed-width tables to width: 100% !important as it expands email logos and icons"
        );
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("td[align=\"right\"]"),
            "USER_MAIL_OVERRIDE_CSS must not force aligned table cells to display: block width: 100%"
        );
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("* {\n  box-sizing: border-box !important;"),
            "USER_MAIL_OVERRIDE_CSS must not force box-sizing: border-box on * as it breaks email button height and vertical padding"
        );

        let brevo_path = std::path::Path::new("/tmp/meridian-test/fixture-e.eml");
        if let Ok(bytes) = std::fs::read(brevo_path) {
            let parsed = crate::services::mail_store_service::parse_rfc822_bytes(
                "brevo-test",
                "inbox",
                &bytes,
                false,
            );
            assert!(parsed.is_some(), "brevo.eml must parse successfully");
            let parsed = parsed.unwrap();
            assert!(!parsed.html_body.is_empty(), "brevo.eml must have an HTML body");

            let plan = super::prepare_mail_body(&parsed.html_body, None, true, false, true);
            assert!(!plan.used_fallback, "brevo.eml must not fall back to plain text");
            assert!(!plan.body.is_empty(), "brevo.eml sanitized body must not be empty");

            let doc = super::compose_sanitized_mail_document(
                &plan.body,
                true,
                false,
                false,
                "system-ui, sans-serif",
                16,
                true,
                &[
                    "#729FCF".to_string(),
                    "#AD7FA8".to_string(),
                    "#8AE234".to_string(),
                    "#FCAF3E".to_string(),
                    "#E9B96E".to_string(),
                ],
            );
            assert!(
                doc.contains("width=\"24\"") && doc.contains("width=\"120\""),
                "brevo.eml document must retain fixed-width container tables for eye icon and logo"
            );
        }
    }

    #[test]
    fn test_karr_eml_renders_without_oversized_images() {
        assert!(
            !super::USER_MAIL_OVERRIDE_CSS.contains("img, video, iframe, canvas, svg {\n  max-width: 100% !important;"),
            "USER_MAIL_OVERRIDE_CSS must not force max-width: 100% !important on images as it overrides author inline max-width"
        );
        assert!(
            super::USER_MAIL_OVERRIDE_CSS.contains("h1, h2, h3, h4, h5, h6 {\n  line-height: 1.25 !important;\n}"),
            "USER_MAIL_OVERRIDE_CSS must ensure headings line-height is 1.25"
        );

        let karr_path = std::path::Path::new("/tmp/meridian-test/fixture-d.eml");
        if let Ok(bytes) = std::fs::read(karr_path) {
            let parsed = crate::services::mail_store_service::parse_rfc822_bytes(
                "karr-test",
                "inbox",
                &bytes,
                false,
            );
            assert!(parsed.is_some(), "karr.eml must parse successfully");
            let parsed = parsed.unwrap();
            assert!(!parsed.html_body.is_empty(), "karr.eml must have an HTML body");

            let plan = super::prepare_mail_body(&parsed.html_body, None, true, true, true);
            assert!(!plan.used_fallback, "karr.eml must not fall back to plain text");
            assert!(!plan.body.is_empty(), "karr.eml sanitized body must not be empty");

            let doc = super::compose_sanitized_mail_document(
                &plan.body,
                true,
                true,
                false,
                "system-ui, sans-serif",
                16,
                true,
                &[
                    "#729FCF".to_string(),
                    "#AD7FA8".to_string(),
                    "#8AE234".to_string(),
                    "#FCAF3E".to_string(),
                    "#E9B96E".to_string(),
                ],
            );
            assert!(
                doc.contains("max-width: 126.69px") || doc.contains("max-width: 127.6px") || doc.contains("width=\"126.69\"") || doc.contains("width=\"127\""),
                "karr.eml document must retain author inline max-width constraints on images"
            );
        }
    }

    #[test]
    fn test_google_eml_dark_mode_inversion() {
        let google_path = std::path::Path::new("/tmp/meridian-test/Meridian/google.eml");
        if let Ok(bytes) = std::fs::read(google_path) {
            let parsed = crate::services::mail_store_service::parse_rfc822_bytes("test", "inbox", &bytes, false)
                .expect("google.eml must parse successfully");
            let plan = super::prepare_mail_body(&parsed.html_body, None, true, true, true);
            let colors = test_citation_colors();
            let doc = super::compose_sanitized_mail_document(
                &plan.body,
                true,
                true,
                true,
                "sans-serif",
                14,
                false,
                &colors,
            );
            assert!(doc.contains("color-scheme: dark"));
            assert!(doc.contains("filter: invert(100%) hue-rotate(180deg)"));
            assert!(doc.contains("background-color: #e1e1e1"));
            assert!(doc.contains("img, video, canvas, svg, iframe, input[type=\"image\"]"));
        }
    }

    #[test]
    fn test_eml_files_dark_mode_smart_inversion() {
        for eml_filename in &["1.eml", "2.eml", "3.eml"] {
            let eml_path = std::path::Path::new("/tmp/meridian-test/Meridian").join(eml_filename);
            if let Ok(bytes) = std::fs::read(&eml_path) {
                let parsed = crate::services::mail_store_service::parse_rfc822_bytes("test", "inbox", &bytes, false)
                    .unwrap_or_else(|| panic!("{} must parse successfully", eml_filename));
                let plan = super::prepare_mail_body(&parsed.html_body, None, true, true, true);
                let colors = test_citation_colors();
                let doc = super::compose_sanitized_mail_document(
                    &plan.body,
                    true,
                    true,
                    true,
                    "sans-serif",
                    14,
                    false,
                    &colors,
                );
                assert!(
                    doc.contains("filter: invert(100%) hue-rotate(180deg)"),
                    "{} must have smart inversion applied in dark mode",
                    eml_filename
                );
                assert!(
                    doc.contains("background-color: #e1e1e1"),
                    "{} must have #e1e1e1 background-color in dark mode inversion",
                    eml_filename
                );
            }
        }
    }

    #[test]
    fn test_has_effective_native_dark_mode_classification() {
        // Standard light html: not natively dark
        assert!(!super::has_effective_native_dark_mode("<p>Hallo Light</p>"));

        // Synthetic native dark with prefers-color-scheme and no explicit light background
        assert!(super::has_effective_native_dark_mode(
            "<style>@media (prefers-color-scheme: dark) { body { color: white; } }</style><p>Dark</p>"
        ));

        // Boilerplate with light background: not natively dark
        assert!(!super::has_effective_native_dark_mode(
            "<style>@media (prefers-color-scheme: dark) { a.link { color: blue; } }</style><body style=\"background: #fefefe\"><p>Text</p></body>"
        ));

        // Inherently dark email
        assert!(super::has_effective_native_dark_mode(
            "<body style=\"background-color: #121212; color: #fff;\"><p>Dark</p></body>"
        ));

        // Real dark override in media query
        assert!(super::has_effective_native_dark_mode(
            "<style>@media (prefers-color-scheme: dark) { body { background-color: #1a1a1a !important; } }</style><body style=\"background: #ffffff;\"><p>Dark</p></body>"
        ));
    }
}

impl Default for MailViewer {
    fn default() -> Self {
        Self::new()
    }
}
