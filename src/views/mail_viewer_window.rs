use crate::i18n::gettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailViewerWindow — Standalone message viewer window (AdwWindow).
//
// Features:
// - Full Libadwaita HeaderBar with message actions: Reply, Reply-All, Forward,
//   Delete, Junk, Mark Read/Unread, Print, View Source (Ctrl+U), Zoom controls.
// - Embedded MailViewer with HTML rendering / plaintext fallback, privacy banner,
//   trash/junk banners, and in-message search bar.
// - Normal headers grid (From, To, Cc, Subject, Date, Avatar) and expandable
//   all-headers revealer.
// - S/MIME & OpenPGP crypto verification badge and ITIP meeting invitation banner.
// - Attachment chips bar with "Save As", "Save All", and "Open" actions.
// - Independent lifecycle tracking and cancellation token support.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::models::mail_message::MailMessageObject;
use crate::services::account_config_service::AccountConfigStore;
use crate::services::camel_mail_service;
use crate::services::crypto_service;
use crate::services::inline_content_service::InlineImage;
use crate::services::itip_service::parse_invitation;
use crate::services::mail_print_service::{
    self, MailPrintAction, MailPrintJobState,
};
use crate::services::mail_store_service::parse_rfc822_bytes;
use crate::services::AppSettings;
use crate::views::accessibility::set_accessible_label;
use crate::views::itip_banner::ItipBanner;
use crate::views::mail_page::{
    configured_normal_header_fields, default_normal_header_configuration,
    export_filename, message_header_fields, message_is_junk, message_is_trash,
    message_source_window_key, prepare_loaded_message_body, print_snapshot_from,
    raw_mime_may_contain_crypto, replace_formatted_header_rows,
    replace_normal_header_rows, sender_header, set_image_icon_with_fallback,
    MailPage,
};
use crate::views::mail_viewer::{
    MailViewer, MessageSearchResult, MessageSearchStatus,
};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MailViewerWindow {
        pub message: RefCell<Option<MailMessageObject>>,
        pub parent_page: RefCell<Option<glib::WeakRef<MailPage>>>,
        pub mail_viewer: RefCell<Option<MailViewer>>,
        pub itip_banner: RefCell<Option<ItipBanner>>,
        pub normal_headers_grid: RefCell<Option<gtk::Grid>>,
        pub all_headers_grid: RefCell<Option<gtk::Grid>>,
        pub all_headers_revealer: RefCell<Option<gtk::Revealer>>,
        pub all_headers_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub crypto_label: RefCell<Option<gtk::Label>>,
        pub lbl_crypto: RefCell<Option<gtk::Label>>,
        pub crypto_chip: RefCell<Option<gtk::Box>>,
        pub crypto_request_generation: Cell<u64>,
        pub message_find_revealer: RefCell<Option<gtk::Revealer>>,
        pub message_find_entry: RefCell<Option<gtk::SearchEntry>>,
        pub message_find_status: RefCell<Option<gtk::Label>>,
        pub find_previous: RefCell<Option<gtk::Button>>,
        pub find_next: RefCell<Option<gtk::Button>>,
        pub attachment_section: RefCell<Option<gtk::Box>>,
        pub attachment_title_box: RefCell<Option<gtk::Box>>,
        pub attachment_summary_box: RefCell<Option<gtk::Box>>,
        pub attachment_summary_label: RefCell<Option<gtk::Label>>,
        pub attachment_chip_box: RefCell<Option<gtk::Box>>,
        pub attachment_chevron: RefCell<Option<gtk::Image>>,
        pub attachment_btn_collapse: RefCell<Option<gtk::Button>>,
        pub attachment_save_all_box: RefCell<Option<gtk::Box>>,
        pub attachment_view_toggle_box: RefCell<Option<gtk::Box>>,
        pub attachment_btn_icons: RefCell<Option<gtk::Button>>,
        pub attachment_btn_list: RefCell<Option<gtk::Button>>,
        pub attachment_stack: RefCell<Option<gtk::Stack>>,
        pub attachment_bar: RefCell<Option<gtk::Box>>,
        pub attachment_scrolled: RefCell<Option<gtk::ScrolledWindow>>,
        pub attachment_listbox: RefCell<Option<gtk::ListBox>>,
        pub attachment_revealer: RefCell<Option<gtk::Revealer>>,
        pub attachment_expanded: Cell<bool>,
        pub print_status_area: RefCell<Option<gtk::Box>>,
        pub print_status_label: RefCell<Option<gtk::Label>>,
        pub print_cancel_button: RefCell<Option<gtk::Button>>,
        pub print_preview_window: RefCell<Option<gtk::Window>>,
        pub print_viewer: RefCell<Option<MailViewer>>,
        pub print_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub print_generation: Cell<u64>,
        pub print_state: Cell<MailPrintJobState>,
        pub toast_overlay: adw::ToastOverlay,
        pub title_widget: RefCell<Option<adw::WindowTitle>>,
        pub btn_mark_read_unread: RefCell<Option<gtk::Button>>,
        pub cancellation: Arc<AtomicBool>,
        pub window_key: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailViewerWindow {
        const NAME: &'static str = "MeridianMailViewerWindow";
        type Type = super::MailViewerWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for MailViewerWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_window();
        }

        fn dispose(&self) {
            self.obj().shutdown();
        }
    }

    impl WidgetImpl for MailViewerWindow {}

    impl WindowImpl for MailViewerWindow {
        fn close_request(&self) -> glib::Propagation {
            self.obj().shutdown();
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for MailViewerWindow {}
    impl AdwApplicationWindowImpl for MailViewerWindow {}
}

glib::wrapper! {
    pub struct MailViewerWindow(ObjectSubclass<imp::MailViewerWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow,
                 gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap,
                    gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native,
                    gtk::Root, gtk::ShortcutManager;
}

impl MailViewerWindow {
    pub fn new(
        app: Option<&gtk::Application>,
        message: &MailMessageObject,
        parent_page: Option<&MailPage>,
    ) -> Self {
        let window: Self = glib::Object::builder()
            .property("title", message.subject())
            .property("default-width", 960)
            .property("default-height", 720)
            .build();

        if let Some(app) = app {
            window.set_application(Some(app));
        }

        let key = message_source_window_key(message);
        *window.imp().window_key.borrow_mut() = key;
        *window.imp().message.borrow_mut() = Some(message.clone());
        if let Some(page) = parent_page {
            *window.imp().parent_page.borrow_mut() = Some(page.downgrade());
        }

        window.build_ui();
        window.setup_actions();
        window.render_message();

        window
    }

    pub fn message(&self) -> Option<MailMessageObject> {
        self.imp().message.borrow().clone()
    }

    pub fn window_key(&self) -> String {
        self.imp().window_key.borrow().clone()
    }

    pub fn cancellation_token(&self) -> Arc<AtomicBool> {
        self.imp().cancellation.clone()
    }

    pub fn cancel_loading(&self) {
        self.imp().cancellation.store(true, Ordering::Relaxed);
        if let Some(c) = self.imp().all_headers_cancellation.borrow().as_ref() {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(c) = self.imp().print_cancellation.borrow_mut().take() {
            c.store(true, Ordering::Release);
        }
    }

    pub fn shutdown(&self) {
        self.cancel_loading();
        if let Some(window) = self.imp().print_preview_window.borrow_mut().take() {
            window.close();
        }
        self.imp().print_viewer.borrow_mut().take();
    }

    pub fn add_toast(&self, toast: &adw::Toast) {
        self.imp().toast_overlay.add_toast(toast.clone());
    }

    pub fn show_toast(&self, message: &str) {
        self.add_toast(&adw::Toast::new(message));
    }

    pub fn show_toast_with_folder_action(&self, message: &str, folder: &std::path::Path) {
        let toast = adw::Toast::new(message);
        toast.set_button_label(Some(&gettext("Open Folder")));
        let folder_path = folder.to_path_buf();
        let win_weak = self.downgrade();
        toast.connect_button_clicked(move |_| {
            let parent = win_weak.upgrade().map(|w| w.upcast::<gtk::Window>());
            let _ = crate::services::quick_preview_service::open_folder_in_file_manager_with_parent(
                &folder_path,
                parent.as_ref(),
            );
        });
        self.add_toast(&toast);
    }

    pub fn zoom_level(&self) -> f64 {
        self.imp()
            .mail_viewer
            .borrow()
            .as_ref()
            .map(|v| v.zoom_level())
            .unwrap_or(1.0)
    }

    pub fn zoom_in(&self) {
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            viewer.zoom_in();
        }
    }

    pub fn zoom_out(&self) {
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            viewer.zoom_out();
        }
    }

    pub fn zoom_reset(&self) {
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            viewer.zoom_reset();
        }
    }

    pub fn allow_remote_images(&self) {
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            viewer.allow_remote_images();
        }
    }

    pub fn remote_images_allowed(&self) -> bool {
        self.imp()
            .mail_viewer
            .borrow()
            .as_ref()
            .map(|v| v.imp().remote_images_allowed_for_current.get())
            .unwrap_or(false)
    }

    fn setup_window(&self) {
        self.set_size_request(480, 360);
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        // ── 1. HeaderBar ───────────────────────────────────────────────────
        let header_bar = adw::HeaderBar::new();

        // Start actions: Reply, Reply-All, Forward
        let btn_reply = gtk::Button::builder()
            .icon_name("mail-reply-sender-symbolic")
            .tooltip_text(gettext("Reply (Ctrl+R)"))
            .action_name("win.reply")
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_reply, &gettext("Reply"));

        let btn_reply_all = gtk::Button::builder()
            .icon_name("mail-reply-all-symbolic")
            .tooltip_text(gettext("Reply all (Ctrl+Shift+R)"))
            .action_name("win.reply-all")
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_reply_all, &gettext("Reply all"));

        let btn_forward = gtk::Button::builder()
            .icon_name("mail-forward-symbolic")
            .tooltip_text(gettext("Forward (Ctrl+L)"))
            .action_name("win.forward")
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_forward, &gettext("Forward"));

        header_bar.pack_start(&btn_reply);
        header_bar.pack_start(&btn_reply_all);
        header_bar.pack_start(&btn_forward);

        // Title widget: Subject + Sender
        let title_widget = adw::WindowTitle::builder()
            .title(gettext("Email"))
            .subtitle("")
            .build();
        header_bar.set_title_widget(Some(&title_widget));
        self.imp().title_widget.replace(Some(title_widget));

        // End actions: Menu, Print, Mark Read/Unread, Junk, Delete
        let menu = gio::Menu::new();
        menu.append(Some(&gettext("Show message source")), Some("win.view-message-source"));
        menu.append(Some(&gettext("Show all headers")), Some("win.toggle-all-headers"));
        menu.append(Some(&gettext("Light style for this email")), Some("win.toggle-mail-light-mode"));

        let zoom_section = gio::Menu::new();
        zoom_section.append(Some(&gettext("Zoom in")), Some("win.zoom-in"));
        zoom_section.append(Some(&gettext("Zoom out")), Some("win.zoom-out"));
        zoom_section.append(Some(&gettext("Normal size")), Some("win.zoom-reset"));
        menu.append_section(Some(&gettext("View")), &zoom_section);

        let search_section = gio::Menu::new();
        search_section.append(Some(&gettext("Find in message…")), Some("win.find-in-message"));
        search_section.append(Some(&gettext("Save as EML…")), Some("win.export"));
        menu.append_section(None, &search_section);

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text(gettext("Main menu"))
            .menu_model(&menu)
            .css_classes(["flat"])
            .build();
        set_accessible_label(&menu_button, &gettext("Main menu"));

        let btn_print = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text(gettext("Print (Ctrl+P)"))
            .action_name("win.mail-print")
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_print, &gettext("Print"));

        let btn_mark_read_unread = gtk::Button::builder()
            .icon_name("mail-mark-read-symbolic")
            .tooltip_text(gettext("Mark as read/unread"))
            .action_name("win.toggle-read-unread")
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_mark_read_unread, &gettext("Mark as read or unread"));
        self.imp().btn_mark_read_unread.replace(Some(btn_mark_read_unread.clone()));

        let btn_junk = gtk::Button::builder()
            .icon_name("mail-mark-junk-symbolic")
            .tooltip_text(gettext("Mark as junk (Ctrl+J)"))
            .action_name("win.mark-junk")
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_junk, &gettext("Mark as junk"));

        let btn_delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(gettext("Delete message (Del)"))
            .action_name("win.delete")
            .css_classes(["flat", "destructive-action"])
            .build();
        set_accessible_label(&btn_delete, &gettext("Delete message"));

        // Pack end in reverse order: Delete, Junk, Mark, Print, Menu
        header_bar.pack_end(&menu_button);
        header_bar.pack_end(&btn_print);
        header_bar.pack_end(&btn_mark_read_unread);
        header_bar.pack_end(&btn_junk);
        header_bar.pack_end(&btn_delete);

        toolbar_view.add_top_bar(&header_bar);

        // ── 2. Message Detail Content ──────────────────────────────────────
        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        // Header area
        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["mail-detail-header"])
            .spacing(2)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(0)
            .build();

        let normal_headers_grid = gtk::Grid::builder()
            .row_spacing(2)
            .column_spacing(8)
            .hexpand(true)
            .css_classes(["mail-normal-headers"])
            .build();
        self.imp().normal_headers_grid.replace(Some(normal_headers_grid.clone()));

        let meta_grid = gtk::Grid::builder()
            .row_spacing(0)
            .column_spacing(6)
            .margin_top(2)
            .build();

        let crypto_label = gtk::Label::builder()
            .label(gettext("Security:"))
            .css_classes(["dim-label", "caption"])
            .halign(gtk::Align::End)
            .visible(false)
            .build();

        let crypto_chip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .css_classes(["badge"])
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        let icon_lock = gtk::Image::new();
        set_image_icon_with_fallback(&icon_lock, "security-high-symbolic", "dialog-information-symbolic");
        let lbl_crypto = gtk::Label::builder()
            .label(gettext("No OpenPGP/S/MIME protection detected"))
            .css_classes(["caption"])
            .build();
        crypto_chip.append(&icon_lock);
        crypto_chip.append(&lbl_crypto);

        meta_grid.attach(&crypto_label, 0, 0, 1, 1);
        meta_grid.attach(&crypto_chip, 1, 0, 1, 1);
        self.imp().crypto_label.replace(Some(crypto_label));
        self.imp().lbl_crypto.replace(Some(lbl_crypto));
        self.imp().crypto_chip.replace(Some(crypto_chip));

        let all_headers_grid = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(10)
            .margin_start(6)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(6)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .css_classes(["mail-all-headers"])
            .build();
        let all_headers_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .overlay_scrolling(false)
            .hexpand(true)
            .min_content_height(80)
            .max_content_height(560)
            .propagate_natural_height(true)
            .child(&all_headers_grid)
            .build();
        let all_headers_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(120)
            .reveal_child(false)
            .child(&all_headers_scroll)
            .build();
        self.imp().all_headers_grid.replace(Some(all_headers_grid));
        self.imp().all_headers_revealer.replace(Some(all_headers_revealer.clone()));

        header_box.append(&normal_headers_grid);
        header_box.append(&meta_grid);
        header_box.append(&all_headers_revealer);

        let header_separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        header_separator.set_visible(false);

        // Find in message bar
        let message_find_entry = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Find in message"))
            .hexpand(true)
            .build();
        let message_find_status = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .width_chars(14)
            .xalign(0.0)
            .build();
        let find_previous = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text(gettext("Previous match"))
            .css_classes(["flat"])
            .sensitive(false)
            .build();
        let find_next = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text(gettext("Next match"))
            .css_classes(["flat"])
            .sensitive(false)
            .build();
        let find_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(gettext("Close search (Esc)"))
            .css_classes(["flat"])
            .build();

        let message_find_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(3)
            .margin_bottom(3)
            .build();
        message_find_bar.append(&message_find_entry);
        message_find_bar.append(&message_find_status);
        message_find_bar.append(&find_previous);
        message_find_bar.append(&find_next);
        message_find_bar.append(&find_close);

        let message_find_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(100)
            .reveal_child(false)
            .child(&message_find_bar)
            .build();

        self.imp().message_find_revealer.replace(Some(message_find_revealer.clone()));
        self.imp().message_find_entry.replace(Some(message_find_entry.clone()));
        self.imp().message_find_status.replace(Some(message_find_status.clone()));
        self.imp().find_previous.replace(Some(find_previous.clone()));
        self.imp().find_next.replace(Some(find_next.clone()));

        // ITIP calendar meeting banner
        let itip_banner = ItipBanner::new();
        let win_weak = self.downgrade();
        itip_banner.connect_action_toast(move |msg| {
            if let Some(win) = win_weak.upgrade() {
                win.show_toast(&msg);
            }
        });
        self.imp().itip_banner.replace(Some(itip_banner.clone()));

        // MailViewer
        let mail_viewer = MailViewer::new();
        mail_viewer.set_vexpand(true);
        mail_viewer.set_margin_start(8);
        mail_viewer.set_margin_end(8);
        mail_viewer.set_margin_top(8);
        mail_viewer.set_margin_bottom(8);
        self.imp().mail_viewer.replace(Some(mail_viewer.clone()));

        // Attachment section container
        let attachment_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .valign(gtk::Align::Start)
            .vexpand(false)
            .css_classes(["attachment-section-container"])
            .visible(false)
            .build();
        let att_sec_for_vexpand = attachment_section.clone();
        mail_viewer.connect_notify_local(Some("vexpand"), move |viewer, _| {
            att_sec_for_vexpand.set_vexpand(false);
            att_sec_for_vexpand.set_valign(gtk::Align::Start);
            if !viewer.vexpands() {
                att_sec_for_vexpand.add_css_class("is-floating-card");
                att_sec_for_vexpand.set_margin_start(8);
                att_sec_for_vexpand.set_margin_end(8);
                att_sec_for_vexpand.set_margin_bottom(8);
            } else {
                att_sec_for_vexpand.remove_css_class("is-floating-card");
                att_sec_for_vexpand.set_margin_start(0);
                att_sec_for_vexpand.set_margin_end(0);
                att_sec_for_vexpand.set_margin_bottom(0);
            }
        });

        let handle_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .margin_top(2)
            .margin_bottom(1)
            .css_classes(["attachment-handle-container"])
            .focusable(false)
            .visible(false)
            .build();
        handle_box.set_cursor_from_name(Some("pointer"));

        let handle_pill = gtk::Box::builder()
            .css_classes(["attachment-bar-handle"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        handle_box.append(&handle_pill);
        attachment_section.append(&handle_box);

        let attachment_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(5)
            .margin_bottom(5)
            .css_classes(["attachment-header-bar"])
            .build();

        let title_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .valign(gtk::Align::Center)
            .hexpand(false)
            .css_classes(["attachment-title-box"])
            .tooltip_text(gettext("Attachments"))
            .focusable(true)
            .build();
        title_box.set_cursor_from_name(Some("pointer"));

        let clip_icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
        clip_icon.set_pixel_size(16);
        clip_icon.set_opacity(0.85);
        clip_icon.set_halign(gtk::Align::Center);
        clip_icon.set_valign(gtk::Align::Center);
        clip_icon.set_hexpand(true);
        title_box.append(&clip_icon);
        attachment_header.append(&title_box);

        let summary_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .css_classes(["attachment-summary-clickable"])
            .focusable(true)
            .visible(false)
            .build();
        summary_box.set_cursor_from_name(Some("pointer"));

        let summary_label = gtk::Label::builder()
            .valign(gtk::Align::Center)
            .use_markup(true)
            .css_classes(["attachment-summary-label"])
            .build();
        summary_box.append(&summary_label);
        attachment_header.append(&summary_box);

        let chip_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .hexpand(false)
            .css_classes(["attachment-bar-chip-container"])
            .visible(true)
            .build();
        attachment_header.append(&chip_box);

        let header_spacer = gtk::Box::builder().hexpand(true).valign(gtk::Align::Fill).build();
        header_spacer.set_cursor_from_name(Some("pointer"));
        attachment_header.append(&header_spacer);

        let save_all_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Center)
            .css_classes(["attachment-save-all-box"])
            .build();
        attachment_header.append(&save_all_box);

        let view_toggle_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["linked", "attachment-view-toggle"])
            .valign(gtk::Align::Center)
            .margin_start(4)
            .visible(false)
            .build();

        let btn_icons = gtk::Button::builder()
            .icon_name("view-grid-symbolic")
            .tooltip_text(gettext("Icon view"))
            .css_classes(["flat", "active", "attachment-view-icons-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_icons, &gettext("Icon view"));

        let btn_list = gtk::Button::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(gettext("List view"))
            .css_classes(["flat", "attachment-view-list-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_list, &gettext("List view"));

        view_toggle_box.append(&btn_icons);
        view_toggle_box.append(&btn_list);
        attachment_header.append(&view_toggle_box);

        let chevron_icon = gtk::Image::from_icon_name("pan-down-symbolic");
        chevron_icon.set_pixel_size(16);
        chevron_icon.set_opacity(1.0);
        chevron_icon.add_css_class("attachment-chevron");
        let btn_collapse = gtk::Button::builder()
            .child(&chevron_icon)
            .tooltip_text(gettext("Expand attachments"))
            .css_classes(["flat", "attachment-collapse-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_collapse, &gettext("Expand attachments"));
        attachment_header.append(&btn_collapse);

        attachment_section.append(&attachment_header);

        let attachment_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(200)
            .reveal_child(false)
            .build();

        let attachment_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .css_classes(["attachment-drawer-stack"])
            .build();

        // Attachment bar (icons view)
        let attachment_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(6)
            .build();
        let attachment_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .propagate_natural_height(true)
            .min_content_width(0)
            .min_content_height(0)
            .max_content_height(168)
            .child(&attachment_bar)
            .visible(true)
            .build();

        attachment_stack.add_named(&attachment_scrolled, Some("icons"));

        // List view container
        let list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_start(12)
            .margin_end(12)
            .margin_top(2)
            .margin_bottom(6)
            .build();

        let list_header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(8)
            .margin_end(8)
            .css_classes(["attachment-list-header"])
            .height_request(26)
            .visible(false)
            .build();

        let icon_spacer = gtk::Box::builder().width_request(22).build();
        list_header_box.append(&icon_spacer);

        let col_name = gtk::Label::builder()
            .label(gettext("Name"))
            .hexpand(true)
            .xalign(0.0)
            .css_classes(["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_name);

        let col_type = gtk::Label::builder()
            .label(gettext("Type"))
            .width_request(140)
            .xalign(0.0)
            .css_classes(["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_type);

        let col_size = gtk::Label::builder()
            .label(gettext("Size"))
            .width_request(85)
            .xalign(1.0)
            .css_classes(["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_size);

        let actions_spacer = gtk::Box::builder().width_request(60).build();
        list_header_box.append(&actions_spacer);
        list_container.append(&list_header_box);

        let attachment_listbox = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["attachment-listbox"])
            .build();

        let list_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .min_content_height(0)
            .max_content_height(160)
            .child(&attachment_listbox)
            .build();
        list_container.append(&list_scrolled);

        attachment_stack.add_named(&list_container, Some("list"));

        attachment_revealer.set_child(Some(&attachment_stack));
        attachment_section.append(&attachment_revealer);

        let win_weak_toggle = self.downgrade();
        let toggle_action = std::rc::Rc::new(move || {
            if let Some(win) = win_weak_toggle.upgrade() {
                win.toggle_attachment_expanded();
            }
        });

        let toggle_btn = toggle_action.clone();
        btn_collapse.connect_clicked(move |_| {
            toggle_btn();
        });

        let toggle_title = toggle_action.clone();
        let title_click = gtk::GestureClick::new();
        title_click.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            toggle_title();
        });
        title_box.add_controller(title_click);

        let toggle_spacer = toggle_action.clone();
        let spacer_click = gtk::GestureClick::new();
        spacer_click.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            toggle_spacer();
        });
        header_spacer.add_controller(spacer_click);

        let toggle_handle = toggle_action.clone();
        let handle_click = gtk::GestureClick::new();
        handle_click.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            toggle_handle();
        });
        handle_box.add_controller(handle_click);

        let toggle_drag = toggle_action.clone();
        let rev_for_drag = attachment_revealer.clone();
        let drag_gesture = gtk::GestureDrag::new();
        drag_gesture.set_button(1);
        drag_gesture.connect_drag_end(move |gesture, _offset_x, offset_y| {
            if offset_y < -8.0 && !rev_for_drag.reveals_child() {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                toggle_drag();
            } else if offset_y > 8.0 && rev_for_drag.reveals_child() {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                toggle_drag();
            }
        });
        handle_box.add_controller(drag_gesture);

        let toggle_summary = toggle_action.clone();
        let click_gesture = gtk::GestureClick::new();
        click_gesture.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            toggle_summary();
        });
        summary_box.add_controller(click_gesture);

        let toggle_key = toggle_action.clone();
        let key_ctrl = gtk::EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::space || keyval == gdk::Key::Return {
                toggle_key();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        summary_box.add_controller(key_ctrl);

        let stack_for_icons = attachment_stack.clone();
        let btn_icons_for_icons = btn_icons.clone();
        let btn_list_for_icons = btn_list.clone();
        let win_for_icons = self.downgrade();
        btn_icons.connect_clicked(move |_| {
            stack_for_icons.set_visible_child_name("icons");
            btn_icons_for_icons.add_css_class("active");
            btn_list_for_icons.remove_css_class("active");
            if let Some(win) = win_for_icons.upgrade() {
                if !win.imp().attachment_expanded.get() {
                    win.set_attachment_expanded(true);
                }
            }
        });

        let stack_for_list = attachment_stack.clone();
        let btn_icons_for_list = btn_icons.clone();
        let btn_list_for_list = btn_list.clone();
        let win_for_list = self.downgrade();
        btn_list.connect_clicked(move |_| {
            stack_for_list.set_visible_child_name("list");
            btn_list_for_list.add_css_class("active");
            btn_icons_for_list.remove_css_class("active");
            if let Some(win) = win_for_list.upgrade() {
                if !win.imp().attachment_expanded.get() {
                    win.set_attachment_expanded(true);
                }
            }
        });

        self.imp().attachment_section.replace(Some(attachment_section));
        self.imp().attachment_title_box.replace(Some(title_box));
        self.imp().attachment_summary_box.replace(Some(summary_box));
        self.imp().attachment_summary_label.replace(Some(summary_label));
        self.imp().attachment_chip_box.replace(Some(chip_box));
        self.imp().attachment_chevron.replace(Some(chevron_icon));
        self.imp().attachment_btn_collapse.replace(Some(btn_collapse));
        self.imp().attachment_save_all_box.replace(Some(save_all_box));
        self.imp().attachment_view_toggle_box.replace(Some(view_toggle_box));
        self.imp().attachment_btn_icons.replace(Some(btn_icons));
        self.imp().attachment_btn_list.replace(Some(btn_list));
        self.imp().attachment_stack.replace(Some(attachment_stack));
        self.imp().attachment_bar.replace(Some(attachment_bar));
        self.imp().attachment_scrolled.replace(Some(attachment_scrolled));
        self.imp().attachment_listbox.replace(Some(attachment_listbox));
        self.imp().attachment_revealer.replace(Some(attachment_revealer));
        self.imp().attachment_expanded.set(false);

        // Print status area
        let print_status_label = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let print_cancel_button = gtk::Button::builder()
            .label(gettext("Cancel"))
            .tooltip_text(gettext("Cancel print preparation"))
            .action_name("win.mail-print-cancel")
            .css_classes(["flat"])
            .visible(false)
            .build();
        let print_status_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_top(3)
            .margin_bottom(3)
            .visible(false)
            .build();
        print_status_area.append(&print_status_label);
        print_status_area.append(&print_cancel_button);
        self.imp().print_status_area.replace(Some(print_status_area.clone()));
        self.imp().print_status_label.replace(Some(print_status_label));
        self.imp().print_cancel_button.replace(Some(print_cancel_button));

        // Connect Find bar signals
        let viewer_for_find = mail_viewer.clone();
        let status_for_find = message_find_status.clone();
        let prev_for_find = find_previous.clone();
        let next_for_find = find_next.clone();
        message_find_entry.connect_search_changed(move |entry| {
            let result = viewer_for_find.search_displayed_message(entry.text().as_str());
            Self::update_search_status(&status_for_find, &prev_for_find, &next_for_find, &result);
        });

        let viewer_for_prev = mail_viewer.clone();
        let status_for_prev = message_find_status.clone();
        let prev_btn = find_previous.clone();
        let next_btn = find_next.clone();
        find_previous.connect_clicked(move |_| {
            let result = viewer_for_prev.search_displayed_message_previous();
            Self::update_search_status(&status_for_prev, &prev_btn, &next_btn, &result);
        });

        let viewer_for_next = mail_viewer.clone();
        let status_for_next = message_find_status.clone();
        let prev_btn2 = find_previous.clone();
        let next_btn2 = find_next.clone();
        find_next.connect_clicked(move |_| {
            let result = viewer_for_next.search_displayed_message_next();
            Self::update_search_status(&status_for_next, &prev_btn2, &next_btn2, &result);
        });

        let viewer_for_close = mail_viewer.clone();
        let entry_for_close = message_find_entry.clone();
        let rev_for_close = message_find_revealer.clone();
        find_close.connect_clicked(move |_| {
            viewer_for_close.finish_displayed_message_search();
            entry_for_close.set_text("");
            rev_for_close.set_reveal_child(false);
        });

        let esc_controller = gtk::EventControllerKey::new();
        let viewer_for_esc = mail_viewer.clone();
        let entry_for_esc = message_find_entry.clone();
        let rev_for_esc = message_find_revealer.clone();
        esc_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                viewer_for_esc.finish_displayed_message_search();
                entry_for_esc.set_text("");
                rev_for_esc.set_reveal_child(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        message_find_entry.add_controller(esc_controller);

        // Pack main box
        main_box.append(&header_box);
        main_box.append(&header_separator);
        main_box.append(&message_find_revealer);
        main_box.append(itip_banner.widget());
        main_box.append(&mail_viewer);
        if let Some(sec) = self.imp().attachment_section.borrow().as_ref() {
            main_box.append(sec);
        }
        main_box.append(&print_status_area);

        self.imp().toast_overlay.set_child(Some(&main_box));
        toolbar_view.set_content(Some(&self.imp().toast_overlay));
        self.set_content(Some(&toolbar_view));
    }

    fn update_search_status(
        status: &gtk::Label,
        prev: &gtk::Button,
        next: &gtk::Button,
        result: &MessageSearchResult,
    ) {
        match result.status {
            MessageSearchStatus::EmptyQuery | MessageSearchStatus::NoDisplayedMessage => {
                status.set_label("");
                prev.set_sensitive(false);
                next.set_sensitive(false);
            }
            MessageSearchStatus::Found => {
                let current = result.current_match.unwrap_or(0);
                status.set_label(&format!("{}/{}", current, result.match_count));
                prev.set_sensitive(result.match_count > 1);
                next.set_sensitive(result.match_count > 1);
            }
            MessageSearchStatus::NotFound => {
                status.set_label("0/0");
                prev.set_sensitive(false);
                next.set_sensitive(false);
            }
            MessageSearchStatus::Pending => {
                status.set_label(&gettext("Searching…"));
                prev.set_sensitive(false);
                next.set_sensitive(false);
            }
        }
    }

    fn setup_actions(&self) {
        let action_reply = gio::SimpleAction::new("reply", None);
        action_reply.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_reply()
        ));
        self.add_action(&action_reply);

        let action_reply_all = gio::SimpleAction::new("reply-all", None);
        action_reply_all.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_reply_all()
        ));
        self.add_action(&action_reply_all);

        let action_forward = gio::SimpleAction::new("forward", None);
        action_forward.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_forward()
        ));
        self.add_action(&action_forward);

        let action_delete = gio::SimpleAction::new("delete", None);
        action_delete.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_delete()
        ));
        self.add_action(&action_delete);

        let action_mark_junk = gio::SimpleAction::new("mark-junk", None);
        action_mark_junk.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_mark_junk()
        ));
        self.add_action(&action_mark_junk);

        let action_mark_not_junk = gio::SimpleAction::new("mark-not-junk", None);
        action_mark_not_junk.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_mark_not_junk()
        ));
        self.add_action(&action_mark_not_junk);

        let action_mark_read = gio::SimpleAction::new("mark-read", None);
        action_mark_read.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.set_unread_state(false)
        ));
        self.add_action(&action_mark_read);

        let action_mark_unread = gio::SimpleAction::new("mark-unread", None);
        action_mark_unread.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.set_unread_state(true)
        ));
        self.add_action(&action_mark_unread);

        let action_toggle_read = gio::SimpleAction::new("toggle-read-unread", None);
        action_toggle_read.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(msg) = win.message() {
                    win.set_unread_state(!msg.is_unread());
                }
            }
        ));
        self.add_action(&action_toggle_read);

        let action_print = gio::SimpleAction::new("mail-print", None);
        action_print.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_print(MailPrintAction::Print)
        ));
        self.add_action(&action_print);

        let action_print_cancel = gio::SimpleAction::new("mail-print-cancel", None);
        action_print_cancel.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.cancel_print()
        ));
        self.add_action(&action_print_cancel);

        let action_source = gio::SimpleAction::new("view-message-source", None);
        action_source.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_view_source()
        ));
        self.add_action(&action_source);

        let action_toggle_light = gio::SimpleAction::new_stateful(
            "toggle-mail-light-mode",
            None,
            &false.to_variant(),
        );
        action_toggle_light.connect_change_state(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |action, target_state| {
                if let Some(state) = target_state {
                    if let Some(force_light) = state.get::<bool>() {
                        action.set_state(&force_light.to_variant());
                        if let Some(viewer) = win.imp().mail_viewer.borrow().as_ref() {
                            viewer.set_light_mode_override(force_light);
                        }
                    }
                }
            }
        ));
        action_toggle_light.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |action, _| {
                let current = action
                    .state()
                    .and_then(|v| v.get::<bool>())
                    .unwrap_or(false);
                let next = !current;
                action.set_state(&next.to_variant());
                if let Some(viewer) = win.imp().mail_viewer.borrow().as_ref() {
                    viewer.set_light_mode_override(next);
                }
            }
        ));
        let is_dark_active = self
            .imp()
            .mail_viewer
            .borrow()
            .as_ref()
            .map(|v| v.is_dark_mode_supported_or_active())
            .unwrap_or_else(|| adw::StyleManager::default().is_dark());
        action_toggle_light.set_enabled(is_dark_active);

        let action_for_dark = action_toggle_light.clone();
        let win_weak_dark = self.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(win) = win_weak_dark.upgrade() {
                let is_dark_active = win
                    .imp()
                    .mail_viewer
                    .borrow()
                    .as_ref()
                    .map(|v| v.is_dark_mode_supported_or_active())
                    .unwrap_or_else(|| adw::StyleManager::default().is_dark());
                action_for_dark.set_enabled(is_dark_active);
            }
        });
        self.add_action(&action_toggle_light);

        let action_find = gio::SimpleAction::new("find-in-message", None);
        action_find.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(rev) = win.imp().message_find_revealer.borrow().as_ref() {
                    rev.set_reveal_child(true);
                }
                if let Some(entry) = win.imp().message_find_entry.borrow().as_ref() {
                    entry.grab_focus();
                }
            }
        ));
        self.add_action(&action_find);

        let action_zoom_in = gio::SimpleAction::new("zoom-in", None);
        action_zoom_in.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.zoom_in()
        ));
        self.add_action(&action_zoom_in);

        let action_zoom_out = gio::SimpleAction::new("zoom-out", None);
        action_zoom_out.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.zoom_out()
        ));
        self.add_action(&action_zoom_out);

        let action_zoom_reset = gio::SimpleAction::new("zoom-reset", None);
        action_zoom_reset.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.zoom_reset()
        ));
        self.add_action(&action_zoom_reset);

        let action_toggle_headers = gio::SimpleAction::new("toggle-all-headers", None);
        action_toggle_headers.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(rev) = win.imp().all_headers_revealer.borrow().as_ref() {
                    rev.set_reveal_child(!rev.reveals_child());
                }
            }
        ));
        self.add_action(&action_toggle_headers);

        let action_export = gio::SimpleAction::new("export", None);
        action_export.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.on_export_eml()
        ));
        self.add_action(&action_export);

        let action_allow_remote = gio::SimpleAction::new("allow-remote-images", None);
        action_allow_remote.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.allow_remote_images()
        ));
        self.add_action(&action_allow_remote);

        let action_close = gio::SimpleAction::new("close", None);
        action_close.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.close()
        ));
        self.add_action(&action_close);
    }

    fn render_message(&self) {
        let Some(message) = self.message() else { return; };

        // Update Title & Subtitle
        let subject = if message.subject().trim().is_empty() {
            gettext("(No Subject)")
        } else {
            message.subject()
        };
        let sender = sender_header(&message);
        self.set_title(Some(&subject));
        if let Some(title_w) = self.imp().title_widget.borrow().as_ref() {
            title_w.set_title(&subject);
            title_w.set_subtitle(&sender);
        }

        // Update Read/Unread button icon
        self.update_read_unread_icon(message.is_unread());

        // Populate Normal Headers
        if let Some(grid) = self.imp().normal_headers_grid.borrow().as_ref() {
            let config = AppSettings::try_new()
                .map(|s| s.mail_normal_headers())
                .unwrap_or_else(default_normal_header_configuration);
            let fields = configured_normal_header_fields(&message_header_fields(&message), &config);
            replace_normal_header_rows(grid, &fields);
        }

        // Populate All Headers
        if let Some(grid) = self.imp().all_headers_grid.borrow().as_ref() {
            replace_formatted_header_rows(grid, &message_header_fields(&message));
        }

        // Populate Attachments Bar
        self.render_attachments(&message);

        // Populate ITIP Meeting Banner
        self.render_itip(&message);

        // Populate Body in MailViewer
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            let attachments = message.attachments();
            let inline_images = attachments
                .iter()
                .filter_map(|att| {
                    att.content_id.as_ref().map(|cid: &String| {
                        InlineImage::new(cid, &att.mime_type, att.content.clone())
                    })
                })
                .collect::<Vec<_>>();
            viewer.set_current_sender(Some(&sender));
            if !message.is_preview_only() {
                viewer.load_message_content_with_fallback(
                    Some(&message.id()),
                    &message.html_body(),
                    &inline_images,
                    None,
                    None,
                );
                viewer.set_trash_banner_visible(message_is_trash(&message));
                viewer.set_junk_banner_visible(message_is_junk(&message));
            } else {
                viewer.prepare_for_loading(Some(&message.id()));
            }
        }

        // Check for Crypto Envelope & Background Load
        self.check_crypto(&message);
        self.check_preview_only(&message);
    }

    pub fn set_attachment_expanded(&self, expanded: bool) {
        self.imp().attachment_expanded.set(expanded);
        if let Some(rev) = self.imp().attachment_revealer.borrow().as_ref() {
            rev.set_reveal_child(expanded);
        }
        if let Some(chev) = self.imp().attachment_chevron.borrow().as_ref() {
            chev.set_icon_name(Some(if expanded {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            }));
        }
        if let Some(btn) = self.imp().attachment_btn_collapse.borrow().as_ref() {
            let tip = if expanded {
                gettext("Collapse attachments")
            } else {
                gettext("Expand attachments")
            };
            btn.set_tooltip_text(Some(&tip));
            set_accessible_label(btn, &tip);
        }
        let has_chips = self.imp().attachment_chip_box.borrow().as_ref()
            .and_then(|cb| cb.first_child())
            .is_some();
        if let Some(summary) = self.imp().attachment_summary_box.borrow().as_ref() {
            summary.set_visible(expanded || !has_chips);
        }
        if let Some(chip_box) = self.imp().attachment_chip_box.borrow().as_ref() {
            chip_box.set_visible(!expanded && has_chips);
        }
        if let Some(view_toggle) = self.imp().attachment_view_toggle_box.borrow().as_ref() {
            view_toggle.set_visible(expanded);
        }
    }

    pub fn toggle_attachment_expanded(&self) {
        let current = self.imp().attachment_revealer.borrow().as_ref().map(|r| r.reveals_child()).unwrap_or(false);
        self.set_attachment_expanded(!current);
    }

    fn render_attachments(&self, message: &MailMessageObject) {
        let Some(bar) = self.imp().attachment_bar.borrow().as_ref().cloned() else { return; };
        let Some(section) = self.imp().attachment_section.borrow().as_ref().cloned() else { return; };

        while let Some(child) = bar.first_child() {
            bar.remove(&child);
        }

        if let Some(listbox) = self.imp().attachment_listbox.borrow().as_ref() {
            while let Some(child) = listbox.first_child() {
                listbox.remove(&child);
            }
        }

        if let Some(save_all_box) = self.imp().attachment_save_all_box.borrow().as_ref() {
            while let Some(child) = save_all_box.first_child() {
                save_all_box.remove(&child);
            }
        }

        if let Some(chip_box) = self.imp().attachment_chip_box.borrow().as_ref() {
            while let Some(child) = chip_box.first_child() {
                chip_box.remove(&child);
            }
        }

        if message.is_preview_only() && message.user_attachments().is_empty() {
            section.set_visible(false);
            self.set_attachment_expanded(false);
            return;
        }

        let has_itip_banner = self.imp().itip_banner.borrow().is_some() && {
            let mut found = message.attachments().iter().any(|att| {
                let is_cal = att
                    .mime_type
                    .split(';')
                    .next()
                    .is_some_and(|m: &str| {
                        let m = m.trim();
                        m.eq_ignore_ascii_case("text/calendar")
                            || m.eq_ignore_ascii_case("application/ics")
                    })
                    || att.filename.to_ascii_lowercase().ends_with(".ics");
                is_cal && parse_invitation(&String::from_utf8_lossy(&att.content)).is_some()
            });
            if !found {
                if let Some(raw) = message.raw_mime().as_deref() {
                    let text = String::from_utf8_lossy(raw);
                    if text.contains("BEGIN:VCALENDAR") {
                        found = parse_invitation(&text).is_some();
                    }
                }
            }
            found
        };

        let attachments: Vec<_> = message
            .user_attachments()
            .into_iter()
            .filter(|att| {
                if has_itip_banner {
                    let is_cal = att
                        .mime_type
                        .split(';')
                        .next()
                        .is_some_and(|m: &str| {
                            let m = m.trim();
                            m.eq_ignore_ascii_case("text/calendar")
                                || m.eq_ignore_ascii_case("application/ics")
                        })
                        || att.filename.to_ascii_lowercase().ends_with(".ics");
                    if is_cal {
                        return false;
                    }
                }
                true
            })
            .collect();
        if attachments.is_empty() {
            section.set_visible(false);
            self.set_attachment_expanded(false);
            return;
        }

        section.set_visible(true);

        let total_size: u64 = attachments.iter().map(|att| att.content.len() as u64).sum();
        let count = attachments.len();
        let count_str = if count == 1 {
            gettext("1 attachment")
        } else {
            gettext("{} attachments").replace("{}", &count.to_string())
        };
        let size_str = glib::format_size(total_size);

        if let Some(lbl) = self.imp().attachment_summary_label.borrow().as_ref() {
            lbl.set_markup(&format!(
                "<b>{}</b> <span alpha='70%'>({})</span>",
                glib::markup_escape_text(&count_str),
                glib::markup_escape_text(&size_str)
            ));
        }

        let win_w = self.width().max(self.default_width());
        let max_chips = if win_w < 550 {
            1
        } else if win_w < 750 {
            2
        } else if win_w < 1000 {
            3
        } else {
            4
        };

        let is_short_mail = {
            let (len, lines) = if let Some(searchable) = message.searchable_body_text() {
                let trimmed = searchable.trim();
                (trimmed.len(), trimmed.lines().count())
            } else {
                let html = message.html_body();
                if html.trim().is_empty() {
                    (0, 0)
                } else {
                    let plain = crate::services::html_sanitizer::html_to_plain_text(&html);
                    let trimmed = plain.trim();
                    (trimmed.len(), trimmed.lines().count())
                }
            };
            len < 800 && lines <= 16
        };

        if let Some(chip_box) = self.imp().attachment_chip_box.borrow().as_ref() {
            if count <= max_chips {
                let max_chip_name_chars = if count == 1 { 50 } else { 26 };
                for att in &attachments {
                    let chip = self.create_attachment_bar_chip(att, max_chip_name_chars);
                    chip_box.append(&chip);
                }
            } else if let Some(summary_box) = self.imp().attachment_summary_box.borrow().as_ref() {
                let tip = format!("{} ({}) – {}", count_str, size_str, gettext("Click to expand"));
                summary_box.set_tooltip_text(Some(&tip));
            }
        }

        // Always default to collapsed state so the attachment bar stays compact and unobtrusive.
        let _ = is_short_mail;
        self.set_attachment_expanded(false);

        if count > 1 {
            let btn_save_all = self.create_save_all_button(&attachments);
            if let Some(save_all_box) = self.imp().attachment_save_all_box.borrow().as_ref() {
                save_all_box.append(&btn_save_all);
            }
        }

        let cards_flowbox = if !attachments.is_empty() {
            let fb = gtk::FlowBox::builder()
                .orientation(gtk::Orientation::Horizontal)
                .selection_mode(gtk::SelectionMode::None)
                .homogeneous(false)
                .row_spacing(6)
                .column_spacing(8)
                .min_children_per_line(1)
                .max_children_per_line(6)
                .valign(gtk::Align::Start)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .css_classes(["attachment-flowbox"])
                .build();
            bar.append(&fb);
            Some(fb)
        } else {
            None
        };

        for att in &attachments {
            let card = self.create_attachment_drawer_card(att);
            if let Some(ref fb) = cards_flowbox {
                let fb_child = gtk::FlowBoxChild::builder()
                    .css_classes(["attachment-flow-child"])
                    .focusable(false)
                    .child(&card)
                    .build();
                fb.append(&fb_child);
            } else {
                bar.append(&card);
            }

            // Also add list view row if listbox exists
            if let Some(listbox) = self.imp().attachment_listbox.borrow().as_ref() {
                let row = gtk::ListBoxRow::new();
                row.set_css_classes(&["attachment-list-row"]);

                let row_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(12)
                    .margin_start(8)
                    .margin_end(8)
                    .margin_top(2)
                    .margin_bottom(2)
                    .focusable(true)
                    .tooltip_text(&format!("{} ({})", att.filename, att.size_formatted))
                    .build();
                row_box.set_cursor_from_name(Some("pointer"));

                let icon_name = att.icon_name();
                let list_icon = gtk::Image::new();
                list_icon.add_css_class("attachment-type-icon");
                set_image_icon_with_fallback(&list_icon, icon_name, "mail-attachment-symbolic");
                list_icon.set_pixel_size(18);
                row_box.append(&list_icon);

                let lbl_name = gtk::Label::builder()
                    .label(&att.filename)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["attachment-list-label"])
                    .hexpand(true)
                    .xalign(0.0)
                    .valign(gtk::Align::Center)
                    .build();
                row_box.append(&lbl_name);

                let type_desc = att.type_description();
                let lbl_type = gtk::Label::builder()
                    .label(&type_desc)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["dim-label", "caption", "attachment-col-type"])
                    .width_request(140)
                    .xalign(0.0)
                    .valign(gtk::Align::Center)
                    .build();
                row_box.append(&lbl_type);

                let lbl_size = gtk::Label::builder()
                    .label(&att.size_formatted)
                    .css_classes(["dim-label", "caption", "attachment-col-size"])
                    .width_request(85)
                    .xalign(1.0)
                    .valign(gtk::Align::Center)
                    .build();
                row_box.append(&lbl_size);

                let actions_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(4)
                    .valign(gtk::Align::Center)
                    .build();

                let save_icon = gtk::Image::from_icon_name("document-save-symbolic");
                save_icon.set_pixel_size(14);
                let btn_save_list = gtk::Button::builder()
                    .child(&save_icon)
                    .tooltip_text(gettext("Save to Downloads (Right-click for options)"))
                    .css_classes(["flat", "circular", "attachment-list-download-btn"])
                    .valign(gtk::Align::Center)
                    .focusable(false)
                    .build();
                set_accessible_label(&btn_save_list, &gettext("Save attachment"));
                let fname_save_list = att.filename.clone();
                let content_save_list = att.content.clone();
                let win_weak_list = self.downgrade();
                btn_save_list.connect_clicked(move |_| {
                    let Some(win) = win_weak_list.upgrade() else { return; };
                    win.save_single_attachment(&fname_save_list, &content_save_list);
                });

                let fname_rc = att.filename.clone();
                let ct_rc = att.content.clone();
                let win_weak_rc = self.downgrade();
                let btn_save_rc = btn_save_list.clone();
                let btn_rc_gesture = gtk::GestureClick::builder().button(3).build();
                btn_rc_gesture.connect_pressed(move |gesture, _, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(win) = win_weak_rc.upgrade() {
                        win.show_attachment_chip_context_menu(&btn_save_rc, x, y, &fname_rc, &ct_rc);
                    }
                });
                btn_save_list.add_controller(btn_rc_gesture);

                actions_box.append(&btn_save_list);

                let fn_row_rc = att.filename.clone();
                let ct_row_rc = att.content.clone();
                let win_weak_row = self.downgrade();
                let row_rc = row.clone();
                let row_rc_gesture = gtk::GestureClick::builder().button(3).build();
                row_rc_gesture.connect_pressed(move |gesture, _, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(win) = win_weak_row.upgrade() {
                        win.show_attachment_chip_context_menu(&row_rc, x, y, &fn_row_rc, &ct_row_rc);
                    }
                });
                row.add_controller(row_rc_gesture);

                row_box.append(&actions_box);

                let fn_open_row = att.filename.clone();
                let ct_open_row = att.content.clone();
                let win_weak_open_row = self.downgrade();
                let row_for_pick = row.clone();
                let btn_save_for_pick = btn_save_list.clone();
                let click = gtk::GestureClick::builder().button(1).build();
                click.connect_released(move |gesture, n_press, x, y| {
                    if n_press >= 1 {
                        if let Some(picked) = row_for_pick.pick(x, y, gtk::PickFlags::DEFAULT) {
                            if picked == btn_save_for_pick || picked.is_ancestor(&btn_save_for_pick) {
                                return;
                            }
                        }
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        let parent = win_weak_open_row.upgrade().map(|w| w.upcast::<gtk::Window>());
                        let _ = crate::services::quick_preview_service::open_attachment_bytes(
                            &fn_open_row,
                            ct_open_row.as_ref(),
                            parent.as_ref(),
                        );
                    }
                });
                row.add_controller(click);

                let fn_key_row = att.filename.clone();
                let ct_key_row = att.content.clone();
                let win_weak_key_row = self.downgrade();
                let key_ctrl = gtk::EventControllerKey::new();
                key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
                    if keyval == gdk::Key::space || keyval == gdk::Key::Return {
                        let parent = win_weak_key_row.upgrade().map(|w| w.upcast::<gtk::Window>());
                        let _ = crate::services::quick_preview_service::open_attachment_bytes(
                            &fn_key_row,
                            ct_key_row.as_ref(),
                            parent.as_ref(),
                        );
                        return glib::Propagation::Stop;
                    }
                    glib::Propagation::Proceed
                });
                row.add_controller(key_ctrl);

                row.set_child(Some(&row_box));
                listbox.append(&row);
            }
        }
    }

    fn save_single_attachment(&self, filename: &str, content: &std::sync::Arc<[u8]>) {
        match crate::services::quick_preview_service::download_attachment_to_downloads(filename, content.as_ref()) {
            Ok(path) => {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(filename);
                let msg = gettext("Saved to Downloads: {}").replace("{}", file_name);
                self.show_toast_with_folder_action(&msg, &path);
            }
            Err(_) => {
                self.prompt_save_attachment_as(filename, content);
            }
        }
    }

    fn prompt_save_attachment_as(&self, filename: &str, content: &std::sync::Arc<[u8]>) {
        let file_dialog = gtk::FileDialog::builder()
            .title(gettext("Save Attachment"))
            .modal(true)
            .initial_name(filename)
            .build();
        let cont = content.clone();
        let f_name = filename.to_string();
        let win_for_toast = self.downgrade();
        file_dialog.save(Some(self), None::<&gio::Cancellable>, move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    if fs::write(&path, cont.as_ref()).is_ok() {
                        if let Some(win) = win_for_toast.upgrade() {
                            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(&f_name);
                            let msg = gettext("Saved: {}").replace("{}", file_name);
                            win.show_toast_with_folder_action(&msg, &path);
                        }
                    }
                }
            }
        });
    }

    fn save_all_attachments(&self, attachments: &[crate::models::mail_message::AttachmentInfo]) {
        match crate::services::quick_preview_service::download_all_attachments_to_downloads(attachments) {
            Ok((saved_count, dir)) => {
                let msg = if saved_count == 1 {
                    gettext("1 attachment saved to Downloads")
                } else {
                    gettext("{} attachments saved to Downloads").replace("{}", &saved_count.to_string())
                };
                self.show_toast_with_folder_action(&msg, &dir);
            }
            Err(_) => {
                self.prompt_save_all_attachments_folder(attachments);
            }
        }
    }

    fn prompt_save_all_attachments_folder(&self, attachments: &[crate::models::mail_message::AttachmentInfo]) {
        let folder_dialog = gtk::FileDialog::builder()
            .title(gettext("Select folder for all attachments"))
            .modal(true)
            .build();
        let atts = attachments.to_vec();
        let win_for_toast = self.downgrade();
        folder_dialog.select_folder(Some(self), None::<&gio::Cancellable>, move |result| {
            if let Ok(folder) = result {
                if let Some(folder_path) = folder.path() {
                    let mut saved_count = 0;
                    for att in &atts {
                        let safe_name = crate::services::quick_preview_service::sanitize_attachment_filename(&att.filename);
                        let target_path = folder_path.join(&safe_name);
                        if std::fs::write(&target_path, att.content.as_ref()).is_ok() {
                            saved_count += 1;
                        }
                    }
                    if let Some(win) = win_for_toast.upgrade() {
                        let msg = gettext("{} attachments saved").replace("{}", &saved_count.to_string());
                        win.show_toast_with_folder_action(&msg, &folder_path);
                    }
                }
            }
        });
    }

    fn create_save_all_button(
        &self,
        attachments: &[crate::models::mail_message::AttachmentInfo],
    ) -> gtk::Button {
        let btn = gtk::Button::builder()
            .css_classes(["attachment-save-all-btn"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Save all attachments to Downloads (Right-click for options)"))
            .build();
        set_accessible_label(&btn, &gettext("Save all attachments"));

        let btn_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();

        let save_icon = gtk::Image::from_icon_name("document-save-symbolic");
        save_icon.set_pixel_size(15);
        let save_label = gtk::Label::builder()
            .label(gettext("Save all…"))
            .css_classes(["attachment-save-all-label"])
            .build();

        btn_box.append(&save_icon);
        btn_box.append(&save_label);
        btn.set_child(Some(&btn_box));

        let atts_click = attachments.to_vec();
        let win_weak = self.downgrade();
        btn.connect_clicked(move |_| {
            let Some(win) = win_weak.upgrade() else { return; };
            win.save_all_attachments(&atts_click);
        });

        let atts_rc = attachments.to_vec();
        let win_weak_rc = self.downgrade();
        let btn_rc = btn.clone();
        let rc_gesture = gtk::GestureClick::builder().button(3).build();
        rc_gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(win) = win_weak_rc.upgrade() {
                win.show_save_all_context_menu(&btn_rc, x, y, &atts_rc);
            }
        });
        btn.add_controller(rc_gesture);

        btn
    }

    fn show_save_all_context_menu(
        &self,
        button: &gtk::Button,
        x: f64,
        y: f64,
        attachments: &[crate::models::mail_message::AttachmentInfo],
    ) {
        let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

        let btn_quick = crate::views::create_context_menu_button(
            Some("folder-download-symbolic"),
            &gettext("Save to Downloads"),
            false,
        );
        let pop_quick = popover.clone();
        let win_weak_quick = self.downgrade();
        let atts_for_quick = attachments.to_vec();
        btn_quick.connect_clicked(move |_| {
            pop_quick.popdown();
            let Some(win) = win_weak_quick.upgrade() else { return; };
            win.save_all_attachments(&atts_for_quick);
        });
        menu_box.append(&btn_quick);

        let btn_choose = crate::views::create_context_menu_button(
            Some("document-save-as-symbolic"),
            &gettext("Choose folder…"),
            false,
        );
        let pop_choose = popover.clone();
        let atts_for_choose = attachments.to_vec();
        let win_weak_choose = self.downgrade();
        btn_choose.connect_clicked(move |_| {
            pop_choose.popdown();
            let Some(win) = win_weak_choose.upgrade() else { return; };
            win.prompt_save_all_attachments_folder(&atts_for_choose);
        });
        menu_box.append(&btn_choose);

        popover.set_parent(button);
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || {
                p.unparent();
            });
        });
        popover.popup();
    }

    fn create_attachment_drawer_card(
        &self,
        att: &crate::models::mail_message::AttachmentInfo,
    ) -> gtk::Widget {
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .css_classes(["attachment-drawer-card"])
            .margin_top(0)
            .margin_bottom(0)
            .focusable(true)
            .tooltip_text(&format!("{} ({})", att.filename, att.size_formatted))
            .build();
        card.set_cursor_from_name(Some("pointer"));

        let icon_name = att.icon_name();
        let icon = gtk::Image::new();
        icon.add_css_class("attachment-type-icon");
        set_image_icon_with_fallback(&icon, icon_name, "mail-attachment-symbolic");
        icon.set_pixel_size(24);
        card.append(&icon);

        let text_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();

        let lbl_name = gtk::Label::builder()
            .label(&att.filename)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .css_classes(["attachment-card-name"])
            .xalign(0.0)
            .build();
        text_box.append(&lbl_name);

        let lbl_meta = gtk::Label::builder()
            .label(&format!("{} • {}", att.size_formatted, att.type_description()))
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .css_classes(["dim-label", "caption", "attachment-card-meta"])
            .xalign(0.0)
            .build();
        text_box.append(&lbl_meta);

        card.append(&text_box);

        let save_icon = gtk::Image::from_icon_name("document-save-symbolic");
        save_icon.set_pixel_size(15);
        let btn_save = gtk::Button::builder()
            .child(&save_icon)
            .tooltip_text(gettext("Save to Downloads (Right-click for options)"))
            .css_classes(["flat", "circular", "attachment-card-download-btn"])
            .valign(gtk::Align::Center)
            .focusable(false)
            .build();
        set_accessible_label(&btn_save, &gettext("Save attachment"));

        let fn_save = att.filename.clone();
        let ct_save = att.content.clone();
        let win_weak_save = self.downgrade();
        btn_save.connect_clicked(move |_| {
            let Some(win) = win_weak_save.upgrade() else { return; };
            win.save_single_attachment(&fn_save, &ct_save);
        });

        let fn_save_rc = att.filename.clone();
        let ct_save_rc = att.content.clone();
        let win_weak_rc = self.downgrade();
        let btn_save_rc = btn_save.clone();
        let btn_rc_gesture = gtk::GestureClick::builder().button(3).build();
        btn_rc_gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(win) = win_weak_rc.upgrade() {
                win.show_attachment_chip_context_menu(&btn_save_rc, x, y, &fn_save_rc, &ct_save_rc);
            }
        });
        btn_save.add_controller(btn_rc_gesture);

        card.append(&btn_save);

        let fn_open = att.filename.clone();
        let ct_open = att.content.clone();
        let win_weak_open = self.downgrade();
        let card_for_pick = card.clone();
        let btn_save_for_pick = btn_save.clone();
        let click = gtk::GestureClick::builder().button(1).build();
        click.connect_released(move |gesture, n_press, x, y| {
            if n_press >= 1 {
                if let Some(picked) = card_for_pick.pick(x, y, gtk::PickFlags::DEFAULT) {
                    if picked == btn_save_for_pick || picked.is_ancestor(&btn_save_for_pick) {
                        return;
                    }
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let parent = win_weak_open.upgrade().map(|w| w.upcast::<gtk::Window>());
                let _ = crate::services::quick_preview_service::open_attachment_bytes(
                    &fn_open,
                    ct_open.as_ref(),
                    parent.as_ref(),
                );
            }
        });
        card.add_controller(click);

        let fn_key = att.filename.clone();
        let ct_key = att.content.clone();
        let win_weak_key = self.downgrade();
        let key_ctrl = gtk::EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::space || keyval == gdk::Key::Return {
                let parent = win_weak_key.upgrade().map(|w| w.upcast::<gtk::Window>());
                let _ = crate::services::quick_preview_service::open_attachment_bytes(
                    &fn_key,
                    ct_key.as_ref(),
                    parent.as_ref(),
                );
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        card.add_controller(key_ctrl);

        let fn_rc = att.filename.clone();
        let ct_rc = att.content.clone();
        let win_weak_rc2 = self.downgrade();
        let card_weak = card.downgrade();
        let rc_gesture = gtk::GestureClick::builder().button(3).build();
        rc_gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let (Some(c), Some(win)) = (card_weak.upgrade(), win_weak_rc2.upgrade()) {
                win.show_attachment_chip_context_menu(&c, x, y, &fn_rc, &ct_rc);
            }
        });
        card.add_controller(rc_gesture);

        card.upcast::<gtk::Widget>()
    }

    fn create_attachment_bar_chip(
        &self,
        att: &crate::models::mail_message::AttachmentInfo,
        max_chars: i32,
    ) -> gtk::Widget {
        let chip_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .css_classes(["attachment-chip"])
            .tooltip_text(&format!("{} ({})", att.filename, att.size_formatted))
            .focusable(true)
            .build();
        chip_box.set_cursor_from_name(Some("pointer"));

        let icon_name = att.icon_name();
        let icon = gtk::Image::new();
        icon.add_css_class("attachment-type-icon");
        set_image_icon_with_fallback(&icon, icon_name, "mail-attachment-symbolic");
        icon.set_pixel_size(15);
        chip_box.append(&icon);

        let lbl = gtk::Label::builder()
            .label(&att.filename)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(max_chars)
            .css_classes(["attachment-chip-label"])
            .valign(gtk::Align::Center)
            .build();
        chip_box.append(&lbl);

        let lbl_size = gtk::Label::builder()
            .label(&format!("· {}", att.size_formatted))
            .css_classes(["dim-label", "caption", "attachment-chip-size"])
            .valign(gtk::Align::Center)
            .build();
        chip_box.append(&lbl_size);

        let save_icon = gtk::Image::from_icon_name("document-save-symbolic");
        save_icon.set_pixel_size(12);
        let btn_save = gtk::Button::builder()
            .child(&save_icon)
            .tooltip_text(gettext("Save to Downloads (Right-click for options)"))
            .css_classes(["flat", "circular", "attachment-chip-download-btn", "attachment-chip-btn"])
            .valign(gtk::Align::Center)
            .focusable(false)
            .build();
        set_accessible_label(&btn_save, &gettext("Save attachment"));

        let filename_for_save = att.filename.clone();
        let content_for_save = att.content.clone();
        let win_weak_save = self.downgrade();
        btn_save.connect_clicked(move |_| {
            let Some(win) = win_weak_save.upgrade() else { return; };
            win.save_single_attachment(&filename_for_save, &content_for_save);
        });

        let fn_save_rc = att.filename.clone();
        let ct_save_rc = att.content.clone();
        let win_weak_rc_btn = self.downgrade();
        let btn_save_rc = btn_save.clone();
        let btn_rc_gesture = gtk::GestureClick::builder().button(3).build();
        btn_rc_gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(win) = win_weak_rc_btn.upgrade() {
                win.show_attachment_chip_context_menu(&btn_save_rc, x, y, &fn_save_rc, &ct_save_rc);
            }
        });
        btn_save.add_controller(btn_rc_gesture);

        chip_box.append(&btn_save);

        let fn_open = att.filename.clone();
        let ct_open = att.content.clone();
        let win_weak_open = self.downgrade();
        let chip_for_pick = chip_box.clone();
        let btn_save_for_pick = btn_save.clone();
        let click = gtk::GestureClick::builder().button(1).build();
        click.connect_released(move |gesture, n_press, x, y| {
            if n_press >= 1 {
                if let Some(picked) = chip_for_pick.pick(x, y, gtk::PickFlags::DEFAULT) {
                    if picked == btn_save_for_pick || picked.is_ancestor(&btn_save_for_pick) {
                        return;
                    }
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let parent = win_weak_open.upgrade().map(|w| w.upcast::<gtk::Window>());
                let _ = crate::services::quick_preview_service::open_attachment_bytes(
                    &fn_open,
                    ct_open.as_ref(),
                    parent.as_ref(),
                );
            }
        });
        chip_box.add_controller(click);

        let fn_key = att.filename.clone();
        let ct_key = att.content.clone();
        let win_weak_key = self.downgrade();
        let key_ctrl = gtk::EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::space || keyval == gdk::Key::Return {
                let parent = win_weak_key.upgrade().map(|w| w.upcast::<gtk::Window>());
                let _ = crate::services::quick_preview_service::open_attachment_bytes(
                    &fn_key,
                    ct_key.as_ref(),
                    parent.as_ref(),
                );
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        chip_box.add_controller(key_ctrl);

        let fn_rc = att.filename.clone();
        let ct_rc = att.content.clone();
        let win_weak_rc = self.downgrade();
        let chip_weak_rc = chip_box.downgrade();
        let rc_gesture = gtk::GestureClick::builder().button(3).build();
        rc_gesture.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let (Some(c), Some(win)) = (chip_weak_rc.upgrade(), win_weak_rc.upgrade()) {
                win.show_attachment_chip_context_menu(&c, x, y, &fn_rc, &ct_rc);
            }
        });
        chip_box.add_controller(rc_gesture);

        chip_box.upcast::<gtk::Widget>()
    }

    fn show_attachment_chip_context_menu<W: IsA<gtk::Widget> + Clone + 'static>(
        &self,
        widget: &W,
        x: f64,
        y: f64,
        filename: &str,
        content: &std::sync::Arc<[u8]>,
    ) {
        let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

        let btn_open = crate::views::create_context_menu_button(
            Some("document-open-symbolic"),
            &gettext("Open Attachment"),
            false,
        );
        let pop_open = popover.clone();
        let fname_open = filename.to_string();
        let cont_open = content.clone();
        let win_weak_open = self.downgrade();
        btn_open.connect_clicked(move |_| {
            pop_open.popdown();
            let parent = win_weak_open.upgrade().map(|w| w.upcast::<gtk::Window>());
            let _ = crate::services::quick_preview_service::open_attachment_bytes(
                &fname_open,
                cont_open.as_ref(),
                parent.as_ref(),
            );
        });
        menu_box.append(&btn_open);

        let btn_quick = crate::views::create_context_menu_button(
            Some("folder-download-symbolic"),
            &gettext("Save to Downloads"),
            false,
        );
        let pop_quick = popover.clone();
        let fname_quick = filename.to_string();
        let cont_quick = content.clone();
        let win_weak_quick = self.downgrade();
        btn_quick.connect_clicked(move |_| {
            pop_quick.popdown();
            let Some(win) = win_weak_quick.upgrade() else { return; };
            win.save_single_attachment(&fname_quick, &cont_quick);
        });
        menu_box.append(&btn_quick);

        let btn_save = crate::views::create_context_menu_button(
            Some("document-save-as-symbolic"),
            &gettext("Save As…"),
            false,
        );
        let pop_save = popover.clone();
        let fname_save = filename.to_string();
        let cont_save = content.clone();
        let win_weak_save = self.downgrade();
        btn_save.connect_clicked(move |_| {
            pop_save.popdown();
            let Some(win) = win_weak_save.upgrade() else { return; };
            win.prompt_save_attachment_as(&fname_save, &cont_save);
        });
        menu_box.append(&btn_save);

        menu_box.append(&crate::views::create_context_menu_separator());

        let btn_copy = crate::views::create_context_menu_button(
            Some("edit-copy-symbolic"),
            &gettext("Copy Filename"),
            false,
        );
        let pop_copy = popover.clone();
        let fname_copy = filename.to_string();
        let w_copy = widget.clone();
        btn_copy.connect_clicked(move |_| {
            pop_copy.popdown();
            w_copy.display().clipboard().set_text(&fname_copy);
        });
        menu_box.append(&btn_copy);

        popover.set_parent(widget);
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || {
                p.unparent();
            });
        });
        popover.popup();
    }

    fn render_itip(&self, message: &MailMessageObject) {
        let Some(banner) = self.imp().itip_banner.borrow().as_ref().cloned() else { return; };
        let mut itip_invitation = None;

        for att in &message.attachments() {
            let is_cal = att.mime_type.split(';').next().is_some_and(|m: &str| m.trim().eq_ignore_ascii_case("text/calendar"))
                || att.filename.to_ascii_lowercase().ends_with(".ics");
            if is_cal {
                if let Some(inv) = parse_invitation(&String::from_utf8_lossy(&att.content)) {
                    itip_invitation = Some(inv);
                    break;
                }
            }
        }

        if itip_invitation.is_none() {
            if let Some(raw) = message.raw_mime().as_deref() {
                let text = String::from_utf8_lossy(raw);
                if text.contains("BEGIN:VCALENDAR") {
                    itip_invitation = parse_invitation(&text);
                }
            }
        }

        let has_itip = itip_invitation.is_some();
        let body_plain = if has_itip {
            let html = message.html_body();
            if !html.trim().is_empty() {
                Some(crate::services::html_sanitizer::html_to_plain_text(&html))
            } else {
                message.searchable_body_text().map(|s| s.to_string())
            }
        } else {
            None
        };

        let profiles = AccountConfigStore::default().load();
        let user_emails: Vec<&str> = profiles.iter().map(|p| p.email.as_str()).collect();
        banner.set_invitation(itip_invitation, &user_emails);
        banner.set_email_body(body_plain.as_deref());
        if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
            viewer.set_suppressed_by_itip(has_itip);
        }
    }

    fn check_crypto(&self, message: &MailMessageObject) {
        let raw_mime: Arc<[u8]> = match message.raw_mime() {
            Some(raw) => raw,
            None => {
                if let Some(chip) = self.imp().crypto_chip.borrow().as_ref() {
                    chip.set_visible(false);
                }
                if let Some(lbl) = self.imp().crypto_label.borrow().as_ref() {
                    lbl.set_visible(false);
                }
                return;
            }
        };
        let has_crypto = raw_mime_may_contain_crypto(&raw_mime);

        if let Some(label) = self.imp().lbl_crypto.borrow().as_ref() {
            let crypto_text = if has_crypto {
                gettext("Cryptography: Verification in progress…")
            } else {
                gettext("No OpenPGP/S/MIME protection detected")
            };
            label.set_label(&crypto_text);
        }
        if let Some(chip) = self.imp().crypto_chip.borrow().as_ref() {
            chip.set_visible(has_crypto);
        }
        if let Some(lbl) = self.imp().crypto_label.borrow().as_ref() {
            lbl.set_visible(has_crypto);
        }

        if !has_crypto {
            return;
        }

        let generation = self.imp().crypto_request_generation.get().wrapping_add(1);
        self.imp().crypto_request_generation.set(generation);

        let (sender, receiver) = async_channel::bounded(1);
        let smime_options = AppSettings::try_new()
            .map(|s| s.smime_options(false, false))
            .unwrap_or_default();
        let msg_id = message.id();
        let folder_id = message.folder_id();
        let unread = message.is_unread();
        let worker_raw: Arc<[u8]> = Arc::clone(&raw_mime);

        std::thread::spawn(move || {
            let res = crypto_service::inspect_crypto_mime(&worker_raw, &smime_options).map(|inspection| {
                let decrypted = inspection.decrypted_mime.as_deref().and_then(|dec| {
                    parse_rfc822_bytes(&msg_id, &folder_id, dec, unread)
                });
                (inspection, decrypted)
            });
            let _ = sender.send_blocking(res);
        });

        let win_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(res) = receiver.recv().await else { return; };
            let Some(win) = win_weak.upgrade() else { return; };
            if win.imp().crypto_request_generation.get() != generation {
                return;
            }

            match res {
                Ok((inspection, decrypted)) => {
                    if let Some(lbl) = win.imp().lbl_crypto.borrow().as_ref() {
                        lbl.set_label(&inspection.details.display_text());
                    }
                    if let Some(dec) = decrypted {
                        if let Some(viewer) = win.imp().mail_viewer.borrow().as_ref() {
                            viewer.load_html(&dec.html_body, None);
                        }
                    }
                }
                Err(err) => {
                    if let Some(lbl) = win.imp().lbl_crypto.borrow().as_ref() {
                        lbl.set_label(&format!("{}: {err}", gettext("Cryptography error")));
                    }
                }
            }
        });
    }

    fn check_preview_only(&self, message: &MailMessageObject) {
        if !message.is_preview_only() {
            return;
        }
        let Some(location) = message.backend_location() else { return; };

        let cancellation = self.imp().cancellation.clone();
        let (sender, receiver) = async_channel::bounded(1);
        let msg_id = message.id();
        let folder_id = message.folder_id();
        let unread = message.is_unread();

        std::thread::spawn(move || {
            let result = camel_mail_service::load_raw_message(&location, &cancellation).and_then(|raw| {
                prepare_loaded_message_body(&msg_id, &folder_id, &raw, unread, &cancellation)
            });
            let _ = sender.send_blocking(result);
        });

        let win_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(res) = receiver.recv().await else { return; };
            let Some(win) = win_weak.upgrade() else { return; };
            if win.imp().cancellation.load(Ordering::Relaxed) {
                return;
            }

            if let Ok((parsed, searchable)) = res {
                if let Some(current_msg) = win.message() {
                    current_msg.set_raw_headers(parsed.raw_headers);
                    current_msg.set_raw_mime(parsed.raw_mime);
                    current_msg.replace_rendered_content(
                        parsed.html_body,
                        parsed.preview_snippet,
                        parsed.attachments,
                        Some(searchable),
                    );
                    win.render_message();
                }
            }
        });
    }

    fn update_read_unread_icon(&self, is_unread: bool) {
        if let Some(btn) = self.imp().btn_mark_read_unread.borrow().as_ref() {
            if is_unread {
                btn.set_icon_name("mail-mark-read-symbolic");
                btn.set_tooltip_text(Some(&gettext("Mark as read (Ctrl+K)")));
            } else {
                btn.set_icon_name("mail-mark-unread-symbolic");
                btn.set_tooltip_text(Some(&gettext("Mark as unread (Ctrl+Shift+K)")));
            }
        }
    }

    fn set_unread_state(&self, unread: bool) {
        let Some(msg) = self.message() else { return; };
        msg.set_unread(unread);
        self.update_read_unread_icon(unread);

        if let Some(page_weak) = self.imp().parent_page.borrow().as_ref() {
            if let Some(page) = page_weak.upgrade() {
                let _ = if unread {
                    page.mark_message_unread(&msg)
                } else {
                    page.mark_message_read(&msg)
                };
            }
        }

        let toast_msg = if unread {
            gettext("Message marked as unread")
        } else {
            gettext("Message marked as read")
        };
        self.show_toast(&toast_msg);
    }

    // ── Action Handlers ────────────────────────────────────────────────────

    fn on_reply(&self) {
        let Some(message) = self.message() else { return; };
        let sender = sender_header(&message);
        let style = AppSettings::try_new()
            .map(|s| s.composer_reply_style())
            .unwrap_or_else(|| "quoted".to_string());

        let composer = crate::dialogs::ComposeDialog::reply_to_message_with_style(
            &sender,
            &message.reply_to(),
            &message.recipient_to(),
            &message.recipient_cc(),
            &message.date_str(),
            &message.subject(),
            &message.resolved_html_body(),
            &style,
        );
        composer.set_transient_for(Some(self));
        composer.present();
    }

    fn on_reply_all(&self) {
        let Some(message) = self.message() else { return; };
        let sender = sender_header(&message);
        let style = AppSettings::try_new()
            .map(|s| s.composer_reply_style())
            .unwrap_or_else(|| "quoted".to_string());

        let composer = crate::dialogs::ComposeDialog::reply_all_message_with_style(
            &sender,
            &message.reply_to(),
            &message.recipient_to(),
            &message.recipient_cc(),
            &message.date_str(),
            &message.subject(),
            &message.resolved_html_body(),
            &style,
        );
        composer.set_transient_for(Some(self));
        composer.present();
    }

    fn on_forward(&self) {
        let Some(message) = self.message() else { return; };
        let sender = sender_header(&message);
        let style = AppSettings::try_new()
            .map(|s| s.composer_forward_style())
            .unwrap_or_else(|| "inline".to_string());
        let resolved_html = message.resolved_html_body();

        let composer = if style == "quoted" {
            crate::dialogs::ComposeDialog::forward_quoted_message(
                &message.subject(),
                &sender,
                &message.recipient_to(),
                &message.recipient_cc(),
                &message.date_str(),
                &resolved_html,
            )
        } else {
            crate::dialogs::ComposeDialog::forward_inline_message(
                &message.subject(),
                &sender,
                &message.recipient_to(),
                &message.recipient_cc(),
                &message.date_str(),
                &resolved_html,
            )
        };
        composer.set_transient_for(Some(self));
        composer.present();
    }

    fn on_delete(&self) {
        let Some(message) = self.message() else { return; };
        let moves_to_trash = crate::services::AppSettings::try_new()
            .map(|s| s.mail_delete_moves_to_trash())
            .unwrap_or(true);
        let in_trash = crate::views::mail_page::message_is_trash(&message);
        if moves_to_trash && !in_trash {
            let mut deleted = false;
            if let Some(page_weak) = self.imp().parent_page.borrow().as_ref() {
                if let Some(page) = page_weak.upgrade() {
                    deleted = page.delete_message(&message).unwrap_or(false);
                }
            }
            if deleted {
                self.close();
            } else {
                self.show_toast(&gettext("Delete failed"));
            }
            return;
        }
        let win = self.clone();

        crate::dialogs::run_destructive_action(
            self,
            &gettext("Delete message?"),
            &gettext("The message will be deleted or moved to trash."),
            &gettext("Delete"),
            move || {
                let mut deleted = false;
                if let Some(page_weak) = win.imp().parent_page.borrow().as_ref() {
                    if let Some(page) = page_weak.upgrade() {
                        deleted = page.delete_message(&message).unwrap_or(false);
                    }
                }
                if deleted {
                    win.close();
                } else {
                    win.show_toast(&gettext("Delete failed"));
                }
            },
        );
    }

    fn on_mark_junk(&self) {
        let Some(message) = self.message() else { return; };
        let mut marked = false;
        if let Some(page_weak) = self.imp().parent_page.borrow().as_ref() {
            if let Some(page) = page_weak.upgrade() {
                marked = page.mark_message_junk(&message).unwrap_or(false);
            }
        }
        if marked {
            self.show_toast(&gettext("Marked as junk"));
            self.close();
        } else {
            self.show_toast(&gettext("Failed to mark as junk"));
        }
    }

    fn on_mark_not_junk(&self) {
        let Some(message) = self.message() else { return; };
        let mut marked = false;
        if let Some(page_weak) = self.imp().parent_page.borrow().as_ref() {
            if let Some(page) = page_weak.upgrade() {
                marked = page.mark_message_not_junk(&message).unwrap_or(false);
            }
        }
        if marked {
            if let Some(viewer) = self.imp().mail_viewer.borrow().as_ref() {
                viewer.set_junk_banner_visible(false);
            }
            self.show_toast(&gettext("Not marked as junk"));
        } else {
            self.show_toast(&gettext("Failed to mark as junk"));
        }
    }

    fn on_view_source(&self) {
        let Some(message) = self.message() else { return; };
        if let Some(page_weak) = self.imp().parent_page.borrow().as_ref() {
            if let Some(page) = page_weak.upgrade() {
                page.present_source_for_message(&message, Some(self.upcast_ref::<gtk::Window>()));
            }
        }
    }

    fn on_export_eml(&self) {
        let Some(message) = self.message() else { return; };
        let filename = export_filename(&message.subject());

        let file_dialog = gtk::FileDialog::builder()
            .title(gettext("Save email as EML"))
            .accept_label(&gettext("Save"))
            .initial_name(&filename)
            .modal(true)
            .build();

        let win_weak = self.downgrade();
        let message_clone = message.clone();
        file_dialog.save(Some(self), None::<&gio::Cancellable>, move |res| {
            let Ok(file) = res else { return; };
            let Some(path) = file.path() else { return; };
            let raw_bytes: Vec<u8> = message_clone
                .raw_mime()
                .map(|b: Arc<[u8]>| b.to_vec())
                .unwrap_or_else(|| message_clone.html_body().into_bytes());
            if fs::write(&path, &raw_bytes).is_ok() {
                if let Some(win) = win_weak.upgrade() {
                    win.show_toast(&gettext("Message exported successfully"));
                }
            }
        });
    }

    fn on_print(&self, action: MailPrintAction) {
        let _ = action;
        let Some(message) = self.message() else { return; };
        let snapshot = print_snapshot_from(&message);
        let generation = self.imp().print_generation.get().wrapping_add(1);
        self.imp().print_generation.set(generation);
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp().print_cancellation.replace(Some(cancellation.clone()));

        if let Some(area) = self.imp().print_status_area.borrow().as_ref() {
            area.set_visible(true);
        }
        if let Some(label) = self.imp().print_status_label.borrow().as_ref() {
            label.set_label(&gettext("Preparing print…"));
        }
        if let Some(btn) = self.imp().print_cancel_button.borrow().as_ref() {
            btn.set_visible(true);
        }

        let (sender, receiver) = async_channel::bounded(1);
        let worker_cancel = cancellation.clone();
        std::thread::spawn(move || {
            let res = mail_print_service::prepare_print_document(&snapshot, &worker_cancel);
            let _ = sender.send_blocking(res);
        });

        let win_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(res) = receiver.recv().await else { return; };
            let Some(win) = win_weak.upgrade() else { return; };
            if win.imp().print_generation.get() != generation {
                return;
            }

            if let Some(area) = win.imp().print_status_area.borrow().as_ref() {
                area.set_visible(false);
            }
            if let Some(btn) = win.imp().print_cancel_button.borrow().as_ref() {
                btn.set_visible(false);
            }

            match res {
                Ok(doc) => {
                    if let Some(viewer) = win.imp().mail_viewer.borrow().as_ref() {
                        let win_weak2 = win.downgrade();
                        viewer.print_document(&doc, win.upcast_ref(), move |result| {
                            if let Some(win) = win_weak2.upgrade() {
                                match result {
                                    Ok(crate::views::mail_viewer::MailPrintDialogResult::Printed) => {
                                        win.show_toast(&gettext("Print job sent to system"));
                                    }
                                    Ok(crate::views::mail_viewer::MailPrintDialogResult::Cancelled) => {
                                        win.show_toast(&gettext("Printing cancelled"));
                                    }
                                    Err(err) => win.show_toast(&format!("{}: {err}", gettext("Print error"))),
                                }
                            }
                        });
                    }
                }
                Err(_) => {
                    win.show_toast(&gettext("Print preparation cancelled or failed"));
                }
            }
        });
    }

    fn cancel_print(&self) {
        if let Some(c) = self.imp().print_cancellation.borrow_mut().take() {
            c.store(true, Ordering::Release);
        }
        if let Some(area) = self.imp().print_status_area.borrow().as_ref() {
            area.set_visible(false);
        }
        if let Some(btn) = self.imp().print_cancel_button.borrow().as_ref() {
            btn.set_visible(false);
        }
        self.show_toast(&gettext("Print job cancelled"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mail_metadata_service::{MailFollowUpMetadata, MailFollowUpState};

    #[test]
    fn test_mail_viewer_window_new_and_actions() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let msg = MailMessageObject::new(
            "msg-test-1",
            "inbox",
            "Alice Sender",
            "alice@example.com",
            "bob@example.com",
            "",
            "",
            "",
            "Test Subject Line",
            Some("<msg-1@example.com>".to_string()),
            vec![],
            vec![],
            "thread-1",
            1,
            0,
            "14:30",
            1024,
            "Preview snippet text",
            "<p>HTML body content</p>",
            true,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            vec![],
            None,
            vec![],
        );

        let page = MailPage::new();
        let window = MailViewerWindow::new(None, &msg, Some(&page));

        assert_eq!(window.title().unwrap().as_str(), "Test Subject Line");
        assert!(window.has_action("reply"));
        assert!(window.has_action("reply-all"));
        assert!(window.has_action("forward"));
        assert!(window.has_action("delete"));
        assert!(window.has_action("mark-junk"));
        assert!(window.has_action("mark-not-junk"));
        assert!(window.has_action("mark-read"));
        assert!(window.has_action("mark-unread"));
        assert!(window.has_action("mail-print"));
        assert!(window.has_action("view-message-source"));
        assert!(window.has_action("find-in-message"));
        assert!(window.has_action("zoom-in"));
        assert!(window.has_action("zoom-out"));
        assert!(window.has_action("zoom-reset"));
        assert!(window.has_action("toggle-all-headers"));
        assert!(window.has_action("toggle-mail-light-mode"));
        assert!(window.has_action("export"));
        assert!(window.has_action("close"));
        assert_eq!(window.zoom_level(), 1.0);
    }

    #[test]
    fn test_mail_viewer_window_cancellation_on_shutdown() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let msg = MailMessageObject::new(
            "msg-test-2",
            "inbox",
            "Alice",
            "alice@example.com",
            "bob@example.com",
            "",
            "",
            "",
            "Subject",
            None,
            vec![],
            vec![],
            "t1",
            1,
            0,
            "12:00",
            512,
            "Snippet",
            "Body",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            vec![],
            None,
            vec![],
        );

        let page = MailPage::new();
        let window = MailViewerWindow::new(None, &msg, Some(&page));
        let cancel = window.cancellation_token();
        assert!(!cancel.load(Ordering::Relaxed));

        window.cancel_loading();
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn test_mail_viewer_window_evolution_attachment_section() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let msg = MailMessageObject::new(
            "msg-test-att",
            "inbox",
            "Alice",
            "alice@example.com",
            "bob@example.com",
            "",
            "",
            "",
            "Subject",
            None,
            vec![],
            vec![],
            "t1",
            1,
            0,
            "12:00",
            512,
            "Snippet",
            &"<p>Long email body line</p>\n".repeat(25),
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            vec![],
            None,
            vec![
                crate::models::mail_message::AttachmentInfo {
                    filename: "report.pdf".to_string(),
                    size_formatted: "100 KB".to_string(),
                    mime_type: "application/pdf".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([1, 2, 3]),
                },
                crate::models::mail_message::AttachmentInfo {
                    filename: "image.png".to_string(),
                    size_formatted: "500 KB".to_string(),
                    mime_type: "image/png".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([4, 5, 6, 7]),
                },
            ],
        );

        let window = MailViewerWindow::new(None, &msg, None);
        window.set_visible(true);
        let sec = window.imp().attachment_section.borrow().clone().expect("attachment_section");
        let rev = window.imp().attachment_revealer.borrow().clone().expect("attachment_revealer");
        let lbl = window.imp().attachment_summary_label.borrow().clone().expect("attachment_summary_label");
        let save_box = window.imp().attachment_save_all_box.borrow().clone().expect("attachment_save_all_box");
        let chip_box = window.imp().attachment_chip_box.borrow().clone().expect("chip_box");
        let summary_box = window.imp().attachment_summary_box.borrow().clone().expect("summary_box");
        let btn_collapse = window.imp().attachment_btn_collapse.borrow().clone().expect("btn_collapse");

        // Section should be visible and revealer should be collapsed by default to save space
        assert!(sec.is_visible());
        assert!(!rev.reveals_child());
        assert!(chip_box.is_visible());
        assert!(chip_box.first_child().is_some());
        assert!(!summary_box.is_visible());

        // View toggle box and stack should be present
        let toggle_box = window.imp().attachment_view_toggle_box.borrow().clone().expect("view_toggle_box");
        let stack = window.imp().attachment_stack.borrow().clone().expect("attachment_stack");
        let listbox = window.imp().attachment_listbox.borrow().clone().expect("attachment_listbox");
        let btn_list = window.imp().attachment_btn_list.borrow().clone().expect("btn_list");
        let btn_icons = window.imp().attachment_btn_icons.borrow().clone().expect("btn_icons");

        assert!(!toggle_box.is_visible());
        assert_eq!(stack.visible_child_name().as_deref(), Some("icons"));
        assert!(listbox.first_child().is_some());

        // Summary label should reflect 2 attachments
        let text = lbl.text();
        assert!(text.contains('2'), "Summary label should contain attachment count 2: {text}");

        fn has_descendant_with_class(root: &impl gtk::prelude::IsA<gtk::Widget>, class: &str) -> bool {
            if root.has_css_class(class) {
                return true;
            }
            let mut child = root.first_child();
            while let Some(c) = child {
                if has_descendant_with_class(&c, class) {
                    return true;
                }
                child = c.next_sibling();
            }
            false
        }

        // Save all button should be styled with attachment-save-all-btn
        let btn = save_box.first_child().and_downcast::<gtk::Button>().expect("save all button");
        assert!(btn.has_css_class("attachment-save-all-btn"));
        assert!(has_descendant_with_class(&btn, "attachment-save-all-label"), "save all button must render a visible label");

        // Compact chip should have a 1-click download button
        let chip = chip_box.first_child().expect("chip in chip_box");
        assert!(has_descendant_with_class(&chip, "attachment-chip-download-btn"), "compact chip must have a 1-click download button");

        // List row should have a 1-click download button
        let list_row = listbox.first_child().expect("list row exists");
        assert!(list_row.has_css_class("attachment-list-row"));
        assert!(has_descendant_with_class(&list_row, "attachment-list-download-btn"), "list row must have a 1-click download button");

        // Attachment bar should contain drawer cards with download button
        let bar = window.imp().attachment_bar.borrow().clone().expect("attachment_bar");
        assert!(
            has_descendant_with_class(&bar, "attachment-drawer-card"),
            "attachment_bar must contain cards with .attachment-drawer-card"
        );
        assert!(
            has_descendant_with_class(&bar, "attachment-card-download-btn"),
            "drawer card must have a 1-click download button"
        );

        // Expanding revealer via btn_collapse
        btn_collapse.emit_clicked();
        assert!(rev.reveals_child());
        assert!(!chip_box.is_visible());
        assert!(summary_box.is_visible());
        assert!(toggle_box.is_visible());

        // Switch to list view
        btn_list.emit_clicked();
        assert_eq!(stack.visible_child_name().as_deref(), Some("list"));

        // Switch back to icons view
        btn_icons.emit_clicked();
        assert_eq!(stack.visible_child_name().as_deref(), Some("icons"));

        // Collapse again
        btn_collapse.emit_clicked();
        assert!(!rev.reveals_child());
        assert!(chip_box.is_visible());
        assert!(!summary_box.is_visible());
        assert!(!toggle_box.is_visible());

        // When opening a short email, drawer is expanded by default to utilize vertical whitespace
        let short_msg = MailMessageObject::new(
            "msg-test-short",
            "inbox",
            "Alice",
            "alice@example.com",
            "bob@example.com",
            "",
            "",
            "",
            "Subject",
            None,
            vec![],
            vec![],
            "t1",
            1,
            0,
            "12:00",
            512,
            "Snippet",
            "Short body",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
            vec![],
            None,
            vec![
                crate::models::mail_message::AttachmentInfo {
                    filename: "quick.pdf".to_string(),
                    size_formatted: "10 KB".to_string(),
                    mime_type: "application/pdf".to_string(),
                    content_id: None,
                    is_inline: false,
                    content: std::sync::Arc::from([1, 2]),
                },
            ],
        );
        let short_win = MailViewerWindow::new(None, &short_msg, None);
        short_win.set_visible(true);
        let short_rev = short_win.imp().attachment_revealer.borrow().clone().expect("attachment_revealer");
        let short_summary = short_win.imp().attachment_summary_box.borrow().clone().expect("summary_box");
        let short_chip = short_win.imp().attachment_chip_box.borrow().clone().expect("chip_box");
        let short_toggle = short_win.imp().attachment_view_toggle_box.borrow().clone().expect("view_toggle_box");

        assert!(short_rev.reveals_child(), "Short email drawer should be expanded by default");
        assert!(short_summary.is_visible());
        assert!(!short_chip.is_visible());
        assert!(short_toggle.is_visible());
    }
}
