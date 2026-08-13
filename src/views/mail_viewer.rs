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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(target_os = "linux")]
use webkit6::prelude::*;

use crate::services::AppSettings;
use crate::services::html_sanitizer::{
    block_remote_images, html_to_plain_text, sanitize_html, sanitized_html_to_plain_text,
};
use crate::services::inline_content_service::{InlineImage, resolve_cid_images};
use crate::services::mail_metadata_service::RemoteContentStore;

const MAX_CACHED_DERIVED_BYTES: usize = 4 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_FIND_MATCH_COUNT: u32 = 10_000;
const RENDER_LOADING_TEXT: &str = "Nachricht wird vorbereitet …";

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
        pub current_inline_images: RefCell<Vec<InlineImage>>,
        pub current_base_uri: RefCell<Option<String>>,
        pub current_fallback_text: RefCell<Option<String>>,
        pub(super) displayed_plaintext: RefCell<Option<DisplayedPlaintext>>,
        pub(super) fallback_search_tag: RefCell<Option<gtk::TextTag>>,
        pub(super) message_search: RefCell<Option<ActiveMessageSearch>>,
        pub(super) search_generation: Cell<u64>,
        pub remote_images_allowed_for_current: Cell<bool>,
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
        pub settings: RefCell<Option<AppSettings>>,
        pub remote_content_store: RefCell<Option<RemoteContentStore>>,
        pub remote_content_allowed: RefCell<HashSet<String>>,
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
            self.render_worker_shutdown.store(true, Ordering::Release);
            self.render_worker_queue.1.notify_all();
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
                .icon_name("view-refresh-symbolic")
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
        let remote_content_store = RemoteContentStore::default();
        self.imp()
            .remote_content_allowed
            .replace(remote_content_store.allowed_message_ids());
        self.imp()
            .remote_content_store
            .replace(Some(remote_content_store));
        self.start_render_worker();

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
        self.load_message_content_with_fallback(
            message_id,
            html_content,
            &[],
            base_uri,
            fallback_text,
        );
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
        let global_remote_images = settings
            .as_ref()
            .map(AppSettings::auto_load_remote_images)
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
        let render_key = RenderRequestKey {
            source_generation: self.imp().source_generation.get(),
            render_html,
            remote_images_allowed,
            force_dark_mode: settings
                .as_ref()
                .map(AppSettings::force_dark_mode)
                .unwrap_or(false),
            mail_font_family: safe_css_font_family(&mail_font_family),
            mail_font_size: safe_mail_font_size(mail_font_size),
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
        self.imp().banner.set_revealed(false);
        self.set_fallback_text(RENDER_LOADING_TEXT);
        #[cfg(target_os = "linux")]
        self.show_renderer_loading();

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
        self.imp()
            .banner
            .set_revealed(render_plan.has_remote_images && !key.remote_images_allowed);
        self.set_fallback_text(&render_plan.visible_text);
        self.imp()
            .displayed_plaintext
            .replace(Some(DisplayedPlaintext {
                source_generation: key.source_generation,
                render_generation: generation,
                message_id: self.imp().current_message_id.borrow().clone(),
                text: render_plan.visible_text.clone(),
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
            }
            let base_uri = self.imp().current_base_uri.borrow().clone();
            let effective_base_uri =
                effective_mail_base_uri(base_uri.as_deref(), key.remote_images_allowed);
            let content_uri = renderer_content_uri(generation);
            self.imp().active_webview_generation.set(generation);
            self.imp()
                .active_webview_uri
                .replace(Some(content_uri.clone()));
            webview.load_alternate_html(&document, &content_uri, effective_base_uri);
        }

        #[cfg(not(target_os = "linux"))]
        {
            // The non-Linux fallback is intentionally plain text. It uses
            // the same sanitized/fallback decision as Linux.
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
        let inline_images = self.imp().current_inline_images.borrow().clone();
        self.load_message_content_with_fallback(
            message_id.as_deref(),
            &html,
            &inline_images,
            base_uri.as_deref(),
            fallback_text.as_deref(),
        );
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
        self.imp().banner.set_revealed(false);
        // Re-run the rendering path from the retained original HTML.  A
        // WebKit reload alone cannot restore remote <img> tags that were
        // deliberately removed from the privacy-first document.
        self.reload_current();
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

        let matches = plaintext_search_matches(&displayed.text, query);
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
            #[cfg(not(target_os = "linux"))]
            Some(MessageSearchBackend::WebKit) => {}
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
            .name("evolution-mail-search-match")
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
        let Some(search) = self.imp().message_search.borrow().as_ref() else {
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
        webview.connect_load_changed(move |webview, event| {
            let Some(viewer) = viewer_weak.upgrade() else {
                return;
            };
            if !viewer.is_current_webview_load(webview) {
                return;
            }
            match event {
                webkit6::LoadEvent::Started | webkit6::LoadEvent::Redirected => {
                    viewer.imp().webview_ready.set(false);
                    viewer.show_renderer_loading();
                }
                webkit6::LoadEvent::Finished => {
                    viewer.imp().webview_ready.set(true);
                    viewer.show_renderer_ready();
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

        stack.add_named(&webview_scroll, Some("webkit"));
        stack.set_visible_child_name("loading");
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
        self.imp().webview_ready.set(false);
        self.imp().last_render_key.take();
        if let Some(label) = self.imp().renderer_error_label.borrow().as_ref() {
            label.set_visible(true);
        }
        if let Some(stack) = self.imp().renderer_stack.borrow().as_ref() {
            stack.set_visible_child_name("error");
        }
    }
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
    text: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderRequestKey {
    source_generation: u64,
    render_html: bool,
    remote_images_allowed: bool,
    force_dark_mode: bool,
    mail_font_family: String,
    mail_font_size: u32,
}

impl RenderRequestKey {
    fn body_key(&self) -> BodyRenderKey {
        BodyRenderKey {
            render_html: self.render_html,
            remote_images_allowed: self.remote_images_allowed,
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
    active_webview_generation == render_generation
        && expected_uri.is_some()
        && expected_uri == current_uri
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

    let sanitized = sanitize_html(html_content);
    let blocked = block_remote_images(&sanitized);
    let has_remote_images = blocked != sanitized
        || any_image_tag(&sanitized, |tag| {
            image_attribute_value(tag, "xlink:href").is_some_and(is_remote_uri_reference)
        });
    let sanitized = if remote_images_allowed {
        sanitized
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

#[cfg(target_os = "linux")]
fn renderer_content_uri(generation: u64) -> String {
    format!("about:blank#evolution-mail-render-{generation}")
}

#[cfg(target_os = "linux")]
fn is_internal_renderer_uri(value: &str) -> bool {
    value.eq_ignore_ascii_case("about:blank")
        || value.starts_with("about:blank#evolution-mail-render-")
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
        CachedMailRender, RenderDecision, RenderJob, RenderRequestKey, RenderWork,
        advance_generation, cache_mail_render, compose_sanitized_mail_document,
        contains_remote_image_reference, effective_mail_base_uri, execute_render_job,
        html_to_fallback_text, inline_image_sources_match, message_search_context_is_current,
        plaintext_search_matches, prepare_mail_body, readable_fallback_text, render_decision,
        render_result_is_current, safe_css_font_family, safe_mail_font_size,
        webview_load_is_current,
    };
    use crate::services::inline_content_service::InlineImage;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn render_key(source_generation: u64) -> RenderRequestKey {
        RenderRequestKey {
            source_generation,
            render_html: true,
            remote_images_allowed: false,
            force_dark_mode: false,
            mail_font_family: "system-ui, sans-serif".to_string(),
            mail_font_size: 16,
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
        assert_eq!(
            readable_fallback_text(Some("   ")),
            "(Die Nachricht enthält keinen darstellbaren Inhalt.)"
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
        );

        assert!(!plan.used_fallback);
        assert!(plan.has_remote_images);
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
        assert!(plan.has_remote_images);
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
        let plan = prepare_mail_body("<p>Hallo</p>", Some("Hallo"), true, false);
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
        let uri = "about:blank#evolution-mail-render-7";
        assert!(webview_load_is_current(7, 7, Some(uri), Some(uri)));
        assert!(!webview_load_is_current(7, 7, Some(uri), None));
        assert!(!webview_load_is_current(6, 7, Some(uri), Some(uri)));
        assert!(!webview_load_is_current(
            7,
            7,
            Some(uri),
            Some("about:blank#evolution-mail-render-6")
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
