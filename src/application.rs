// SPDX-License-Identifier: GPL-3.0-or-later
//
// AdwApplication subclass — the root GObject of the application.
//
// Responsibilities:
// - Application lifecycle (startup, activate, shutdown)
// - Action registration (shortcuts, global actions)
// - Window creation and management
// - CSS provider for custom styles

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use crate::i18n::gettext;
use crate::services::AppSettings;
use crate::services::reminder_service::{
    self, ReminderComponentKind, ReminderIdentity, ReminderScheduler, ReminderStore,
    SystemClock, parse_reminder_action_target, parse_reminder_deep_link,
};
use crate::window::MeridianWindow;
use crate::{APP_ID, RESOURCE_PATH};

pub(crate) const ACCELERATOR_BINDINGS: &[(&str, &[&str])] = &[
    ("app.quit", &["<Ctrl>Q"]),
    ("app.global-search", &["<Ctrl><Shift>G"]),
    ("app.preferences", &["<Ctrl>comma"]),
    ("win.show-help-overlay", &["<Ctrl>question", "F1", "<Ctrl>slash"]),
    ("win.search", &["<Ctrl>F"]),
    ("win.refresh-mail", &["F5"]),
    ("win.mail-send-receive", &["F12"]),
    ("win.undo", &["<Ctrl>Z"]),
    ("win.redo", &["<Ctrl><Shift>Z"]),
    ("win.undelete", &["<Ctrl><Shift>D"]),
    ("win.apply-filters", &["<Ctrl>Y"]),
    ("win.toggle-sidebar", &["F9"]),
    ("win.toggle-statusbar", &["F7"]),
    ("win.toggle-debug-console", &["<Ctrl>F12", "<Ctrl><Shift>Y"]),
    ("win.new", &["<Ctrl>N"]),
    ("win.switch-mail", &["<Ctrl>1"]),
    ("win.switch-calendar", &["<Ctrl>2"]),
    ("win.switch-contacts", &["<Ctrl>3"]),
    ("win.switch-tasks", &["<Ctrl>4"]),
    ("win.mail-next", &["<Ctrl>Page_Down", "J"]),
    ("win.mail-prev", &["<Ctrl>Page_Up", "K"]),
    ("win.mail-next-unread", &["<Ctrl>bracketright"]),
    ("win.mail-prev-unread", &["<Ctrl>bracketleft"]),
    ("win.toggle-preview", &["<Ctrl>M"]),
    ("win.toggle-mail-threading", &["<Ctrl>T"]),
    ("win.reply", &["<Ctrl>R"]),
    ("win.reply-all", &["<Ctrl><Shift>R"]),
    ("win.forward", &["<Ctrl>L"]),
    ("win.export", &["<Ctrl><Shift>S"]),
    ("win.mail-print", &["<Ctrl>P"]),
    ("win.view-message-source", &["<Ctrl>U"]),
    ("win.find-in-message", &["<Ctrl><Shift>F"]),
    ("win.purge-calendar", &["<Ctrl>E"]),
    ("win.delete", &["Delete"]),
    ("win.mark-read", &["<Ctrl>K"]),
    ("win.mark-unread", &["<Ctrl><Shift>K"]),
    ("win.mark-all-read", &["<Ctrl><Shift>M"]),
    ("win.mark-thread-read", &["<Ctrl><Alt>M"]),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
    ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
    ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
    ("win.expand-all-threads", &["<Shift>plus", "<Shift>KP_Add"]),
    (
        "win.collapse-all-threads",
        &["<Shift>minus", "<Shift>KP_Subtract"],
    ),
    ("win.toggle-important", &["<Ctrl>I"]),
    ("win.archive", &["<Ctrl><Alt>A"]),
    ("win.mark-junk", &["<Ctrl>J"]),
    ("win.mark-not-junk", &["<Ctrl><Shift>J"]),
    ("win.move-message", &["<Ctrl><Shift>V"]),
    ("win.copy-message", &["<Ctrl><Shift>C"]),
    ("win.select-all-mail", &["<Ctrl>A"]),
];

pub(crate) fn primary_accelerator(action: &str) -> Option<&'static str> {
    ACCELERATOR_BINDINGS
        .iter()
        .find_map(|(candidate, accelerators)| (*candidate == action).then(|| accelerators[0]))
}

pub(crate) fn all_accelerators(action: &str) -> Option<&'static [&'static str]> {
    ACCELERATOR_BINDINGS
        .iter()
        .find_map(|(candidate, accelerators)| (*candidate == action).then_some(*accelerators))
}

#[cfg(test)]
mod action_contract_tests {
    use super::*;
    use crate::action_contract::{ACTIONS, SHORTCUTS};

    #[test]
    fn every_documented_shortcut_has_a_registered_accelerator() {
        for shortcut in SHORTCUTS {
            assert!(
                primary_accelerator(shortcut.action).is_some(),
                "shortcut has no accelerator: {}",
                shortcut.action
            );
        }
    }

    #[test]
    fn every_accelerator_targets_an_exported_action() {
        for (action, accelerators) in ACCELERATOR_BINDINGS {
            assert!(!accelerators.is_empty(), "empty accelerator list: {action}");
            assert!(
                ACTIONS.iter().any(|spec| spec.action == *action),
                "accelerator targets an unknown action: {action}"
            );
        }
    }

    #[test]
    fn application_action_contract_is_registered() {
        let app = MeridianApplication::new();
        app.setup_actions();
        for spec in ACTIONS
            .iter()
            .filter(|spec| spec.action.starts_with("app."))
        {
            let name = spec.action.trim_start_matches("app.");
            assert!(
                app.lookup_action(name).is_some(),
                "missing app action: {name}"
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ReminderAction {
    Snooze(i64),
    Dismiss,
}

// ─── GObject Subclass Boilerplate ───────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::HashSet;

    #[derive(Debug, Default)]
    pub struct MeridianApplication {
        pub reminder_source: RefCell<Option<glib::SourceId>>,
        pub reminder_check_in_progress: Cell<bool>,
        pub reminder_initial_check_done: Cell<bool>,
        pub reminder_actions_in_flight: RefCell<HashSet<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MeridianApplication {
        const NAME: &'static str = "MeridianApplication";
        type Type = super::MeridianApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for MeridianApplication {}

    impl ApplicationImpl for MeridianApplication {
        /// Called once on first activation. Sets up global resources.
        fn startup(&self) {
            self.parent_startup();
            let app = self.obj();

            tracing::debug!("Application startup");

            // Load custom icons & CSS
            app.setup_icons();
            app.setup_desktop_integration();
            app.setup_css();
            app.apply_style_settings();

            // Register global keyboard shortcuts / actions
            app.setup_actions();
            app.setup_accels();
        }

        /// Called when the application is activated (or re-focused).
        fn activate(&self) {
            self.parent_activate();
            let app = self.obj();

            tracing::debug!("Application activate");
            crate::services::account_config_service::ensure_profiles_synced_to_eds();
            app.setup_reminder_scheduler();

            // GTK4-rs/libadwaita is the single production frontend.  EDS and
            // Camel services remain independent of this widget layer.
            let window = app.main_window();

            tracing::debug!("Presenting main window after synchronous UI construction");
            window.present();
            if AppSettings::try_new().is_some_and(|s| !s.reminder_notify_with_tray()) {
                window.present_existing_reminders();
            }
            tracing::debug!("Main window presented");

            if std::env::args().any(|a| a == "--calendar" || a == "--component=calendar") {
                window.switch_to_view("calendar");
            } else if std::env::args().any(|a| a == "--contacts" || a == "--component=contacts") {
                window.switch_to_view("contacts");
            } else if std::env::args().any(|a| a == "--tasks" || a == "--component=tasks" || a == "--component=memos") {
                window.switch_to_view("tasks");
            } else if std::env::args().any(|a| a == "--mail" || a == "--component=mail") {
                window.switch_to_view("mail");
            }

            if std::env::args().any(|a| a == "--compose") {
                let dialog = crate::dialogs::ComposeDialog::new();
                dialog.present_modal(&window);
            }

            let is_preview = std::env::args().any(|a| a.starts_with("--settings-preview") || a.starts_with("--preferences-preview"));
            if is_preview {
                println!("\n╔═══════════════════════════════════════════════════════════════════════════════╗");
                println!("║               MERIDIAN — LIBADWAITA PREFERENCES PREVIEW                     ║");
                println!("║  Das weltbeste Einstellungsmenü (First Principles & GNOME HIG)                ║");
                println!("║  NavigationSplitView · Live Search (<Ctrl>+F) · Responsive Breakpoint (<640sp)║");
                println!("╚═══════════════════════════════════════════════════════════════════════════════╝\n");
                let dialog = crate::dialogs::PreferencesDialog::new();
                for arg in std::env::args() {
                    if let Some(page) = arg.strip_prefix("--settings-preview=").or_else(|| arg.strip_prefix("--preferences-preview=")) {
                        dialog.select_page(page);
                    }
                }
                dialog.present_standalone();
            } else if std::env::args().any(|a| a == "--preferences") {
                let dialog = crate::dialogs::PreferencesDialog::new();
                dialog.present_standalone();
            }

            if std::env::args().any(|a| a == "--account-setup") {
                let configured = crate::services::account_config_service::AccountConfigStore::default().load();
                let dialog = if let Some(profile) = configured.first() {
                    crate::dialogs::AccountSetupDialog::new_for_edit(profile)
                } else {
                    let mut profile = crate::services::account_config_service::AccountProfile::default();
                    profile.protocol = crate::services::account_config_service::AccountProtocol::Exchange;
                    profile.display_name = "Exchange Account".to_string();
                    profile.email = "user@example.com".to_string();
                    profile.username = r"DOMAIN\user".to_string();
                    profile.imap_host = "https://mail.example.com/EWS/Exchange.asmx".to_string();
                    profile.oab_url = "https://mail.example.com/OAB/".to_string();
                    crate::dialogs::AccountSetupDialog::new_for_edit(&profile)
                };
                if std::env::args().any(|a| a == "--receiving") {
                    dialog.select_page("receiving");
                }
                dialog.present(Some(&window));
            }
        }

        fn open(&self, files: &[gio::File], hint: &str) {
            self.parent_open(files, hint);
            let app = self.obj();
            crate::services::account_config_service::ensure_profiles_synced_to_eds();
            app.setup_reminder_scheduler();
            app.handle_external_open(files);
        }
    }

    impl GtkApplicationImpl for MeridianApplication {}
    impl AdwApplicationImpl for MeridianApplication {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    /// The main application object.
    pub struct MeridianApplication(ObjectSubclass<imp::MeridianApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

pub type EvolutionApplication = MeridianApplication;

impl MeridianApplication {
    /// Create a new application instance.
    pub fn new() -> Self {
        Self::with_flags(gio::ApplicationFlags::HANDLES_OPEN)
    }

    /// Create a new application instance with custom flags (e.g. NON_UNIQUE).
    pub fn with_flags(flags: gio::ApplicationFlags) -> Self {
        glib::Object::builder()
            .property("application-id", APP_ID)
            .property("resource-base-path", RESOURCE_PATH)
            .property("flags", flags)
            .build()
    }

    /// Register bundled symbolic icons with the display icon theme.
    fn setup_icons(&self) {
        let display = gdk::Display::default().expect("Could not get default display");
        let icon_theme = gtk::IconTheme::for_display(&display);

        // Ensure application icons directory exists in app data
        let app_icons_dir = crate::services::app_data_dir().join("icons");
        let scalable_dir = app_icons_dir.join("hicolor/scalable/actions");
        let _ = std::fs::create_dir_all(&scalable_dir);

        let icons: &[(&str, &str)] = &[
            (
                "mail-inbox-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/mail-inbox-symbolic.svg"),
            ),
            (
                "mail-outbox-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/mail-outbox-symbolic.svg"),
            ),
            (
                "mail-drafts-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/mail-drafts-symbolic.svg"),
            ),
            (
                "mail-sync-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/mail-sync-symbolic.svg"),
            ),
            (
                "mail-flag-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/mail-flag-symbolic.svg"),
            ),
            (
                "evolution-tasks-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/evolution-tasks-symbolic.svg"),
            ),
            (
                "tasks-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/tasks-symbolic.svg"),
            ),
            (
                "view-cards-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/view-cards-symbolic.svg"),
            ),
            (
                "view-list-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/view-list-symbolic.svg"),
            ),
            (
                "meridian-view-cards-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/meridian-view-cards-symbolic.svg"),
            ),
            (
                "meridian-view-table-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/meridian-view-table-symbolic.svg"),
            ),
            (
                "funnel-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/funnel-symbolic.svg"),
            ),
            (
                "calendar-today-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/calendar-today-symbolic.svg"),
            ),
            (
                "go-today-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/go-today-symbolic.svg"),
            ),
            (
                "layout-sidebar-right-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/layout-sidebar-right-symbolic.svg"),
            ),
            (
                "external-link-symbolic.svg",
                include_str!("../data/icons/hicolor/scalable/actions/external-link-symbolic.svg"),
            ),
        ];

        for (filename, content) in icons {
            let _ = std::fs::write(scalable_dir.join(filename), content);
        }

        // Install embedded application PNG icons across standard hicolor resolutions
        let app_icon_sizes: &[(&str, &[u8])] = &[
            (
                "16x16",
                include_bytes!("../data/icons/hicolor/16x16/apps/com.martingawron.Meridian.png"),
            ),
            (
                "22x22",
                include_bytes!("../data/icons/hicolor/22x22/apps/com.martingawron.Meridian.png"),
            ),
            (
                "24x24",
                include_bytes!("../data/icons/hicolor/24x24/apps/com.martingawron.Meridian.png"),
            ),
            (
                "32x32",
                include_bytes!("../data/icons/hicolor/32x32/apps/com.martingawron.Meridian.png"),
            ),
            (
                "48x48",
                include_bytes!("../data/icons/hicolor/48x48/apps/com.martingawron.Meridian.png"),
            ),
            (
                "64x64",
                include_bytes!("../data/icons/hicolor/64x64/apps/com.martingawron.Meridian.png"),
            ),
            (
                "128x128",
                include_bytes!("../data/icons/hicolor/128x128/apps/com.martingawron.Meridian.png"),
            ),
            (
                "256x256",
                include_bytes!("../data/icons/hicolor/256x256/apps/com.martingawron.Meridian.png"),
            ),
            (
                "512x512",
                include_bytes!("../data/icons/hicolor/512x512/apps/com.martingawron.Meridian.png"),
            ),
            (
                "1024x1024",
                include_bytes!("../data/icons/hicolor/1024x1024/apps/com.martingawron.Meridian.png"),
            ),
        ];

        #[cfg(unix)]
        let user_hicolor_dir = glib::user_data_dir().join("icons/hicolor");
        #[cfg(unix)]
        let mut user_icons_updated = false;

        for (size, bytes) in app_icon_sizes {
            let dir = app_icons_dir.join(format!("hicolor/{size}/apps"));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join(format!("{APP_ID}.png")), bytes);

            #[cfg(unix)]
            if std::env::var("FLATPAK_ID").is_err() {
                let user_dir = user_hicolor_dir.join(format!("{size}/apps"));
                let user_file = user_dir.join(format!("{APP_ID}.png"));
                let needs_write = match std::fs::read(&user_file) {
                    Ok(existing) => existing != *bytes,
                    Err(_) => true,
                };
                if needs_write {
                    let _ = std::fs::create_dir_all(&user_dir);
                    let _ = std::fs::write(&user_file, bytes);
                    user_icons_updated = true;
                }
            }
        }

        #[cfg(unix)]
        if user_icons_updated {
            let _ = std::process::Command::new("gtk-update-icon-cache")
                .args(["-f", "-t"])
                .arg(&user_hicolor_dir)
                .output();
        }

        icon_theme.add_search_path(&app_icons_dir);
        let local_data = std::path::Path::new("data/icons");
        if local_data.exists() {
            icon_theme.add_search_path(local_data);
        }

        tracing::debug!("Application icons registered");
    }

    /// Ensure desktop launcher and executable symlink exist for GNOME Shell integration.
    #[cfg(unix)]
    fn setup_desktop_integration(&self) {
        ensure_desktop_file_installed();
    }

    #[cfg(not(unix))]
    fn setup_desktop_integration(&self) {}

    /// Load the application-wide CSS stylesheet.
    fn setup_css(&self) {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));

        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        tracing::debug!("Custom CSS loaded");
    }

    fn apply_style_settings(&self) {
        if let Some(settings) = AppSettings::try_new() {
            adw::StyleManager::default().set_color_scheme(settings.adw_color_scheme());

            settings.connect_changed(Some("color-scheme"), move |s, _| {
                let scheme = s.string("color-scheme");
                let color_scheme = AppSettings::parse_adw_color_scheme(scheme.as_str());
                adw::StyleManager::default().set_color_scheme(color_scheme);
            });
        } else {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
        }
    }

    /// Register application-wide actions.
    fn setup_actions(&self) {
        // ── Quit ──
        let action_quit = gio::SimpleAction::new("quit", None);
        action_quit.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                tracing::info!("Quit action triggered");
                for window in app.windows() {
                    window.close();
                }
                app.quit();
            }
        ));
        self.add_action(&action_quit);

        // ── About Dialog ──
        let action_about = gio::SimpleAction::new("about", None);
        action_about.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                app.show_about_dialog();
            }
        ));
        self.add_action(&action_about);

        // ── New Mail ──
        let action_compose = gio::SimpleAction::new("compose", None);
        action_compose.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                tracing::info!("Compose action triggered");
                let dialog = crate::dialogs::ComposeDialog::new();
                let parent = app.active_window();
                dialog.set_transient_for(parent.as_ref());
                dialog.present();
            }
        ));
        self.add_action(&action_compose);

        // ── Local Drafts ──
        let action_drafts = gio::SimpleAction::new("drafts", None);
        action_drafts.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                tracing::info!("Draft manager action triggered");
                let dialog = crate::dialogs::DraftsDialog::new();
                let parent = app.active_window();
                dialog.present(parent.as_ref());
            }
        ));
        self.add_action(&action_drafts);

        // ── Portable global search ──
        let action_global_search = gio::SimpleAction::new("global-search", None);
        action_global_search.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::GlobalSearchDialog::new();
                let parent = app.active_window();
                dialog.present(parent.as_ref());
            }
        ));
        self.add_action(&action_global_search);

        // ── Preferences ──
        let action_preferences = gio::SimpleAction::new("preferences", None);
        action_preferences.connect_activate(glib::clone!(
            #[weak(rename_to = _app)]
            self,
            move |_, _| {
                tracing::info!("Preferences action triggered");
                let dialog = crate::dialogs::PreferencesDialog::new();
                dialog.present_standalone();
            }
        ));
        self.add_action(&action_preferences);

        self.setup_reminder_actions();
    }

    fn setup_reminder_actions(&self) {
        self.add_snooze_action("snooze-reminder-5m", 5 * 60);
        self.add_snooze_action("snooze-reminder-10m", 10 * 60);
        self.add_snooze_action("snooze-reminder-15m", 15 * 60);
        self.add_snooze_action("snooze-reminder-30m", 30 * 60);
        self.add_snooze_action("snooze-reminder-1h", 60 * 60);
        self.add_snooze_action("snooze-reminder-1d", 24 * 60 * 60);

        let action = gio::SimpleAction::new("dismiss-reminder", Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                app.apply_reminder_action(target, ReminderAction::Dismiss);
            }
        ));
        self.add_action(&action);

        let action = gio::SimpleAction::new("open-reminder", Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                let Ok(target) = parse_reminder_action_target(target) else {
                    app.show_reminder_error(&gettext("Invalid reminder action target"));
                    return;
                };
                app.route_component_identity(target.identity, None);
            }
        ));
        self.add_action(&action);

        let action = gio::SimpleAction::new("show-reminders", None);
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                let window = app.main_window();
                window.present();
                window.present_existing_reminders();
            }
        ));
        self.add_action(&action);

        let action = gio::SimpleAction::new("open-mail", Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                app.open_mail_from_notification(target);
            }
        ));
        self.add_action(&action);

        let action = gio::SimpleAction::new("mark-mail-read", Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                app.mark_mail_read_from_notification(target);
            }
        ));
        self.add_action(&action);
    }

    fn open_mail_from_notification(&self, target_str: &str) {
        crate::services::mail_notification_service::MailNotificationService::withdraw_notification(self);
        let window = self.main_window();
        window.present();
        window.switch_to_view("mail");
        let Some(target) =
            crate::services::mail_notification_service::NotificationActionTarget::parse(target_str)
        else {
            return;
        };
        if let Some(mail_page) = window.mail_page() {
            mail_page.open_folder_and_select_message(
                &target.source_uid,
                &target.folder_name,
                target.message_uid.as_deref(),
                target.message_id.as_deref(),
            );
        }
    }

    fn mark_mail_read_from_notification(&self, target_str: &str) {
        crate::services::mail_notification_service::MailNotificationService::withdraw_notification(self);
        let Some(target) =
            crate::services::mail_notification_service::NotificationActionTarget::parse(target_str)
        else {
            return;
        };
        let window = self.main_window();
        if let Some(mail_page) = window.mail_page() {
            if let Some(message_uid) = target.message_uid.as_deref() {
                mail_page.mark_message_read_by_location(
                    &target.source_uid,
                    &target.folder_name,
                    message_uid,
                );
            } else if let Some(message_id) = target.message_id.as_deref() {
                mail_page.mark_message_read_by_id(message_id);
            }
        }
    }

    fn add_snooze_action(&self, name: &str, seconds: i64) {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                app.apply_reminder_action(target, ReminderAction::Snooze(seconds));
            }
        ));
        self.add_action(&action);
    }

    fn main_window(&self) -> MeridianWindow {
        if let Some(window) = self
            .active_window()
            .and_then(|window| window.downcast::<MeridianWindow>().ok())
        {
            return window;
        }
        self.windows()
            .into_iter()
            .find_map(|window| window.downcast::<MeridianWindow>().ok())
            .unwrap_or_else(|| MeridianWindow::new(self))
    }

    fn show_reminder_error(&self, message: &str) {
        let window = self.main_window();
        window.present();
        window.add_toast(&adw::Toast::new(message));
    }

    fn handle_external_open(&self, files: &[gio::File]) {
        let window = self.main_window();
        window.present();

        let mut cal_paths = Vec::new();
        let mut vcard_paths = Vec::new();
        let mut mail_paths = Vec::new();

        for file in files {
            let uri = file.uri().to_string();
            let lower_uri = uri.to_ascii_lowercase();

            if lower_uri.starts_with("mailto:") {
                let raw = uri.trim_start_matches("mailto:").trim_start_matches("MAILTO:");
                let recipient = raw.split('?').next().unwrap_or("").trim();
                let dialog = if recipient.is_empty() {
                    crate::dialogs::ComposeDialog::new()
                } else {
                    crate::dialogs::ComposeDialog::to_recipient(recipient)
                };
                dialog.set_transient_for(Some(&window));
                dialog.present();
                continue;
            }

            if lower_uri.starts_with("webcal://") || lower_uri.starts_with("webcals://") {
                window.switch_to_view("calendar");
                continue;
            }

            if let Some(path) = file.path() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match ext.as_str() {
                    "ics" | "ical" | "ifb" | "vcs" => {
                        cal_paths.push(path);
                        continue;
                    }
                    "vcf" | "vcard" => {
                        vcard_paths.push(path);
                        continue;
                    }
                    "eml" | "mbox" | "mbx" => {
                        mail_paths.push(path);
                        continue;
                    }
                    _ => {}
                }
            }

            self.route_component_uri(&uri, None);
        }

        if !cal_paths.is_empty() {
            window.switch_to_view("calendar");
            let svc = crate::services::CalendarStoreService::new();
            if let Ok(count) = svc.import_files(&cal_paths) {
                window.refresh_calendar_page();
                if count > 0 {
                    window.add_toast(&adw::Toast::new(&gettext("Calendar file imported")));
                }
            }
        }
        if !vcard_paths.is_empty() {
            window.switch_to_view("contacts");
            let svc = crate::services::ContactStoreService::new();
            let _ = svc.import_files(&vcard_paths);
        }
        if !mail_paths.is_empty() {
            window.switch_to_view("mail");
            let svc = crate::services::MailStoreService::new();
            if let Ok(count) = svc.import_files(&mail_paths) {
                if count > 0 {
                    window.refresh_mail_page();
                }
            }
        }
    }

    fn route_component_uri(&self, uri: &str, expected_kind: Option<ReminderComponentKind>) -> bool {
        let identity = match parse_reminder_deep_link(uri) {
            Ok(identity) => identity,
            Err(error) => {
                self.show_reminder_error(&format!("{}: {error}", gettext("Could not open reminder link")));
                return false;
            }
        };
        self.route_component_identity(identity, expected_kind)
    }

    fn route_component_identity(
        &self,
        identity: ReminderIdentity,
        expected_kind: Option<ReminderComponentKind>,
    ) -> bool {
        if expected_kind.is_some_and(|kind| identity.kind != kind) {
            self.show_reminder_error(&gettext("Reminder link does not match the requested component"));
            return false;
        }
        let window = self.main_window();
        window.present();
        window.open_component(identity)
    }

    fn apply_reminder_action(&self, target: &str, action: ReminderAction) -> bool {
        let raw_target = target.to_owned();
        let target = match parse_reminder_action_target(target) {
            Ok(target) => target,
            Err(error) => {
                self.show_reminder_error(&format!("{}: {error}", gettext("Reminder action rejected")));
                return false;
            }
        };
        {
            let mut in_flight = self.imp().reminder_actions_in_flight.borrow_mut();
            if in_flight.contains(&target.key) {
                return false;
            }
            in_flight.insert(target.key.clone());
        }

        let key = target.key.clone();
        let claim_token = target.claim_token.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let now = chrono::Utc::now().timestamp();
            let result = ReminderStore::new().and_then(|mut store| match action {
                ReminderAction::Snooze(seconds) => store
                    .snooze_if_current_at(
                        &key,
                        &claim_token,
                        now.saturating_add(seconds.max(0)),
                        now,
                    )
                    .map(|applied| {
                        if applied {
                            Ok(())
                        } else {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "Reminder action is stale",
                            ))
                        }
                    })?,
                ReminderAction::Dismiss => store
                    .dismiss_if_current_at(&key, &claim_token, now)
                    .map(|applied| {
                        if applied {
                            Ok(())
                        } else {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "Reminder action is stale",
                            ))
                        }
                    })?,
            });
            let _ = sender.send_blocking(result);
        });

        let app = self.downgrade();
        let action_key = target.key.clone();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(app) = app.upgrade() else {
                return;
            };
            app.imp().reminder_actions_in_flight.borrow_mut().remove(&action_key);
            let window = app.main_window();
            match result {
                Ok(Ok(())) => {
                    window.finish_reminder_action(&raw_target, true, None);
                    app.withdraw_notification(&reminder_notification_id(&action_key));
                }
                Ok(Err(error)) => {
                    let message = error.to_string();
                    window.finish_reminder_action(&raw_target, false, Some(message));
                }
                Err(error) => {
                    let message = format!("{}: {error}", gettext("Reminder action cancelled"));
                    window.finish_reminder_action(&raw_target, false, Some(message));
                }
            }
        });
        true
    }

    pub(crate) fn apply_reminder_action_from_dialog(
        &self,
        target: &str,
        snooze_seconds: Option<i64>,
    ) -> bool {
        self.apply_reminder_action(
            target,
            snooze_seconds
                .map(ReminderAction::Snooze)
                .unwrap_or(ReminderAction::Dismiss),
        )
    }

    fn setup_reminder_scheduler(&self) {
        if self.imp().reminder_source.borrow().is_some() {
            return;
        }
        self.check_local_reminders();
        let app = self.downgrade();
        let source = glib::timeout_add_seconds_local(60, move || {
            let Some(app) = app.upgrade() else {
                return glib::ControlFlow::Break;
            };
            app.check_local_reminders();
            glib::ControlFlow::Continue
        });
        self.imp().reminder_source.replace(Some(source));
    }

    /// Poll the local calendar store. EDS-backed snapshots use the
    /// same candidate/state API once the native Linux watcher is enabled.
    fn check_local_reminders(&self) {
        if AppSettings::try_new().is_some_and(|settings| !settings.notifications_enabled()) {
            return;
        }
        if self.imp().reminder_check_in_progress.replace(true) {
            return;
        }
        let settings = AppSettings::try_new();
        let local_calendar_directory =
            settings.as_ref().map(|settings| settings.local_calendar_directory());
        let local_contact_directory =
            settings.as_ref().map(|settings| settings.local_contact_directory());
        let use_ba_reminder = settings.as_ref().is_some_and(|s| s.use_ba_reminder());
        let ba_reminder_offset_seconds = settings
            .as_ref()
            .map(|s| s.ba_reminder_offset_seconds())
            .unwrap_or(900);
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            // Calendar and contact data can be large and are not part of the first GTK
            // frame. Keep all filesystem/parsing/EDS D-Bus work off the main context;
            // only the final desktop notifications and dialog presentation return to GTK.
            let scheduler = ReminderScheduler::new(SystemClock);
            let now = scheduler.now();
            let mut snapshot = crate::services::ecal_service::load_snapshot_prefer_eds().snapshot;
            if let Some(ref dir) = local_calendar_directory {
                let local = reminder_service::calendar_store_for_worker(Some(dir.clone())).load_snapshot();
                snapshot.events.extend(local.events);
                snapshot.tasks.extend(local.tasks);
                snapshot.memos.extend(local.memos);
            }
            let mut candidates = reminder_service::candidates_from_snapshot(&snapshot, now);
            if use_ba_reminder {
                let mut contacts: Vec<crate::services::contact_store_service::LocalContactRecord> =
                    crate::services::ebook_service::load_contacts_prefer_eds()
                        .contacts
                        .into_iter()
                        .map(|c| crate::services::contact_store_service::LocalContactRecord {
                            id: c.uid,
                            contact: c.contact,
                            vcard: c.vcard,
                        })
                        .collect();
                if let Some(ref dir) = local_contact_directory {
                    let local = reminder_service::contact_store_for_worker(Some(dir.clone())).load_contacts();
                    contacts.extend(local);
                }
                let mut seen_ids = std::collections::HashSet::new();
                contacts.retain(|c| seen_ids.insert(c.id.clone()));

                let ba_candidates = reminder_service::contact_birthday_candidates(
                    &contacts,
                    now,
                    ba_reminder_offset_seconds,
                );
                candidates.extend(ba_candidates);
                candidates.sort_by(|l, r| l.due_at.cmp(&r.due_at).then_with(|| l.key.cmp(&r.key)));
                candidates.dedup_by(|l, r| l.key == r.key);
            }
            let result = ReminderStore::new()
                .and_then(|mut store| scheduler.claim_due_reminders(&mut store, &candidates));
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Reminder result was dropped before delivery");
            }
        });

        let app = self.downgrade();
        glib::spawn_future_local(async move {
            let Some(app) = app.upgrade() else {
                return;
            };
            app.imp().reminder_check_in_progress.set(false);
            let due = match receiver.recv().await {
                Ok(Ok(due)) => due,
                Ok(Err(error)) => {
                    app.show_reminder_error(&format!(
                        "{}: {error}",
                        gettext("Could not save reminder state")
                    ));
                    return;
                }
                Err(error) => {
                    tracing::debug!(%error, "Reminder result channel closed");
                    return;
                }
            };
            let is_initial_startup_check = !app.imp().reminder_initial_check_done.replace(true);
            let mut delivered = Vec::with_capacity(due.len());
            for delivery in due {
                let Ok(_target) = delivery.action_target() else {
                    tracing::warn!(key = %delivery.candidate.key, "Skipping reminder with invalid action target");
                    continue;
                };
                if !is_initial_startup_check {
                    let notification_title = gettext("Reminders");
                    let notification = gio::Notification::new(&notification_title);
                    let body = delivery.candidate.format_description();
                    notification.set_body(Some(&body));
                    let icon = gio::ThemedIcon::from_names(&[
                        "appointment-soon",
                        "appointment-soon-symbolic",
                        "alarm-symbolic",
                    ]);
                    notification.set_icon(&icon);
                    notification.set_default_action("app.show-reminders");
                    notification.add_button(&notification_title, "app.show-reminders");
                    let notification_id = reminder_notification_id(&delivery.candidate.key);
                    app.send_notification(Some(&notification_id), &notification);
                }
                delivered.push(delivery);
            }
            if !delivered.is_empty() {
                if !is_initial_startup_check
                    && AppSettings::try_new().is_some_and(|s| s.reminder_sound_enabled())
                {
                    crate::services::mail_notification_service::MailNotificationService::play_alarm_sound();
                }
                let show_dialog = AppSettings::try_new()
                    .is_some_and(|s| !s.reminder_notify_with_tray());
                let window = app.main_window();
                window.present_reminders(delivered.clone(), show_dialog);
                std::thread::spawn(move || {
                    let Ok(mut store) = ReminderStore::new() else {
                        tracing::debug!("Portable reminder state is unavailable after delivery");
                        return;
                    };
                    let now = chrono::Utc::now().timestamp();
                    for delivery in delivered {
                        if let Err(error) = store.mark_delivered_if_claimed_at(
                            &delivery.candidate.key,
                            &delivery.claim_token,
                            now,
                        ) {
                            tracing::warn!(%error, "Could not persist delivered reminder");
                        }
                    }
                });
            }
        });
    }

    /// Set up keyboard accelerators for actions.
    fn setup_accels(&self) {
        for (action, accelerators) in ACCELERATOR_BINDINGS {
            self.set_accels_for_action(action, accelerators);
        }

        tracing::debug!("Keyboard accelerators registered");
    }

    /// Show the About dialog using AdwAboutDialog.
    fn show_about_dialog(&self) {
        if let Some(win) = self.active_window() {
            crate::dialogs::show_about_dialog(&win);
        }
    }
}

pub(crate) const DEFAULT_CALENDAR_MIME_TYPES: &[&str] = &[
    "text/calendar",
    "text/x-vcalendar",
    "application/ics",
    "x-scheme-handler/webcal",
    "x-scheme-handler/webcals",
];

pub(crate) const DEFAULT_MAIL_MIME_TYPES: &[&str] = &[
    "x-scheme-handler/mailto",
    "message/rfc822",
];

#[cfg(unix)]
pub(crate) fn ensure_desktop_file_installed() {
    if std::env::var("FLATPAK_ID").is_err() {
        let data_dir = glib::user_data_dir();
        let applications_dir = data_dir.join("applications");
        let desktop_path = applications_dir.join(format!("{APP_ID}.desktop"));
        let desktop_content = include_str!("../data/com.martingawron.Meridian.desktop.in");

        let needs_write = match std::fs::read_to_string(&desktop_path) {
            Ok(existing) => existing != desktop_content,
            Err(_) => true,
        };

        if needs_write {
            let _ = std::fs::create_dir_all(&applications_dir);
            let _ = std::fs::write(&desktop_path, desktop_content);
            let _ = std::process::Command::new("update-desktop-database")
                .arg(&applications_dir)
                .output();
        }

        if let Ok(exe) = std::env::current_exe() {
            let local_bin = glib::home_dir().join(".local/bin");
            let symlink_path = local_bin.join("meridian");
            let should_link = match std::fs::read_link(&symlink_path) {
                Ok(target) => target != exe,
                Err(_) => !symlink_path.exists(),
            };
            if should_link {
                let _ = std::fs::create_dir_all(&local_bin);
                let _ = std::fs::remove_file(&symlink_path);
                let _ = std::os::unix::fs::symlink(&exe, &symlink_path);
            }
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn ensure_desktop_file_installed() {}

pub(crate) fn is_default_app_for_mime(mime_type: &str) -> bool {
    let desktop_id = format!("{APP_ID}.desktop");
    if let Some(info) = gio::AppInfo::default_for_type(mime_type, false) {
        if let Some(id) = info.id() {
            if id.as_str().eq_ignore_ascii_case(&desktop_id)
                || id.as_str().eq_ignore_ascii_case(APP_ID)
            {
                return true;
            }
        }
        if info.name().eq_ignore_ascii_case("Meridian") {
            return true;
        }
    }
    false
}

pub(crate) fn set_as_default_app_for_mimes(mime_types: &[&str]) -> bool {
    ensure_desktop_file_installed();
    let desktop_id = format!("{APP_ID}.desktop");
    let mut any_ok = false;

    #[cfg(unix)]
    {
        if let Some(app_info) = gio::DesktopAppInfo::new(&desktop_id).or_else(|| {
            let path = glib::user_data_dir()
                .join("applications")
                .join(&desktop_id);
            gio::DesktopAppInfo::from_filename(&path)
        }) {
            for mime in mime_types {
                if app_info.set_as_default_for_type(mime).is_ok() {
                    any_ok = true;
                }
            }
        }

        for mime in mime_types {
            if let Ok(status) = std::process::Command::new("xdg-mime")
                .args(["default", &desktop_id, mime])
                .status()
            {
                if status.success() {
                    any_ok = true;
                }
            }
        }
    }

    any_ok
}

fn reminder_notification_id(key: &str) -> String {
    format!("meridian-reminder-{key}")
}

impl Default for MeridianApplication {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use gio::prelude::*;
    use std::collections::HashMap;

    use super::ACCELERATOR_BINDINGS;

    #[test]
    fn accelerator_contract_is_conflict_free() {
        let mut owners = HashMap::new();
        for (action, accelerators) in ACCELERATOR_BINDINGS {
            for accelerator in *accelerators {
                let normalized = accelerator.to_ascii_lowercase();
                assert_eq!(
                    owners.insert(normalized.clone(), *action),
                    None,
                    "{normalized} is assigned to more than one action"
                );
            }
        }
    }

    #[test]
    fn accelerator_contract_keeps_upstream_daily_mail_paths() {
        let accelerators_for = |wanted: &str| {
            ACCELERATOR_BINDINGS
                .iter()
                .find_map(|(action, accelerators)| (*action == wanted).then_some(*accelerators))
                .expect("daily action should have an accelerator")
        };

        assert!(accelerators_for("win.mail-next").contains(&"<Ctrl>Page_Down"));
        assert!(accelerators_for("win.mail-prev").contains(&"<Ctrl>Page_Up"));
        assert_eq!(
            accelerators_for("win.mail-next-unread"),
            &["<Ctrl>bracketright"]
        );
        assert_eq!(
            accelerators_for("win.mail-prev-unread"),
            &["<Ctrl>bracketleft"]
        );
        assert_eq!(accelerators_for("win.toggle-preview"), &["<Ctrl>M"]);
        assert_eq!(accelerators_for("win.toggle-mail-threading"), &["<Ctrl>T"]);
        assert_eq!(accelerators_for("win.mail-print"), &["<Ctrl>P"]);
        assert_eq!(accelerators_for("win.find-in-message"), &["<Ctrl><Shift>F"]);
        assert_eq!(accelerators_for("win.undelete"), &["<Ctrl><Shift>D"]);
        assert_eq!(accelerators_for("win.mark-all-read"), &["<Ctrl><Shift>M"]);
        assert_eq!(accelerators_for("win.purge-calendar"), &["<Ctrl>E"]);
        assert!(accelerators_for("win.show-help-overlay").contains(&"F1"));
        assert!(accelerators_for("win.show-help-overlay").contains(&"<Ctrl>question"));
    }

    #[test]
    fn test_embedded_application_icons_integrity() {
        let app_icon_sizes: &[(&str, &[u8])] = &[
            (
                "16x16",
                include_bytes!("../data/icons/hicolor/16x16/apps/com.martingawron.Meridian.png"),
            ),
            (
                "22x22",
                include_bytes!("../data/icons/hicolor/22x22/apps/com.martingawron.Meridian.png"),
            ),
            (
                "24x24",
                include_bytes!("../data/icons/hicolor/24x24/apps/com.martingawron.Meridian.png"),
            ),
            (
                "32x32",
                include_bytes!("../data/icons/hicolor/32x32/apps/com.martingawron.Meridian.png"),
            ),
            (
                "48x48",
                include_bytes!("../data/icons/hicolor/48x48/apps/com.martingawron.Meridian.png"),
            ),
            (
                "64x64",
                include_bytes!("../data/icons/hicolor/64x64/apps/com.martingawron.Meridian.png"),
            ),
            (
                "128x128",
                include_bytes!("../data/icons/hicolor/128x128/apps/com.martingawron.Meridian.png"),
            ),
            (
                "256x256",
                include_bytes!("../data/icons/hicolor/256x256/apps/com.martingawron.Meridian.png"),
            ),
            (
                "512x512",
                include_bytes!("../data/icons/hicolor/512x512/apps/com.martingawron.Meridian.png"),
            ),
        ];

        let png_magic = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        for (size, bytes) in app_icon_sizes {
            assert!(bytes.len() > 100, "Icon {size} should have reasonable payload");
            assert_eq!(
                &bytes[..8],
                png_magic,
                "Icon {size} must start with standard PNG magic header"
            );
        }
    }

    #[test]
    fn test_mail_notification_actions_registered() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap();
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let app = super::MeridianApplication::new();
        app.setup_actions();
        assert!(app.lookup_action("open-mail").is_some());
        assert!(app.lookup_action("mark-mail-read").is_some());
        assert!(app.lookup_action("show-reminders").is_some());
        assert!(app.lookup_action("open-reminder").is_some());
        assert!(app.lookup_action("dismiss-reminder").is_some());
        assert!(app.lookup_action("snooze-reminder-10m").is_some());
    }
}
