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
use glib::prelude::ToVariant;
use gtk::{gdk, gio, glib};

use crate::services::AppSettings;
use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::reminder_service::{self, ReminderKind, ReminderStore};
use crate::window::EvolutionWindow;
use crate::{APP_ID, RESOURCE_PATH};

// ─── GObject Subclass Boilerplate ───────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::Cell;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct EvolutionApplication {
        pub reminder_source: RefCell<Option<glib::SourceId>>,
        pub reminder_check_in_progress: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EvolutionApplication {
        const NAME: &'static str = "EvolutionApplication";
        type Type = super::EvolutionApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for EvolutionApplication {}

    impl ApplicationImpl for EvolutionApplication {
        /// Called once on first activation. Sets up global resources.
        fn startup(&self) {
            self.parent_startup();
            let app = self.obj();

            tracing::debug!("Application startup");

            // Load custom CSS
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
            app.setup_reminder_scheduler();

            // GTK4-rs/libadwaita is the single production frontend.  EDS and
            // Camel services remain independent of this widget layer.
            let window = if let Some(window) = app.active_window() {
                window
            } else {
                let window = EvolutionWindow::new(&app);
                window.upcast()
            };

            tracing::debug!("Presenting main window after synchronous UI construction");
            window.present();
            tracing::debug!("Main window presented");
        }
    }

    impl GtkApplicationImpl for EvolutionApplication {}
    impl AdwApplicationImpl for EvolutionApplication {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    /// The main application object.
    pub struct EvolutionApplication(ObjectSubclass<imp::EvolutionApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl EvolutionApplication {
    /// Create a new application instance.
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", APP_ID)
            .property("resource-base-path", RESOURCE_PATH)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .build()
    }

    /// Load the application-wide CSS stylesheet.
    fn setup_css(&self) {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));

        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("Could not get default display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        tracing::debug!("Custom CSS loaded");
    }

    fn apply_style_settings(&self) {
        let scheme = AppSettings::try_new()
            .map(|settings| settings.color_scheme())
            .unwrap_or_else(|| "system".to_string());
        let color_scheme = match scheme.as_str() {
            "light" => adw::ColorScheme::ForceLight,
            "dark" => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        };
        adw::StyleManager::default().set_color_scheme(color_scheme);
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
                dialog.set_transient_for(parent.as_ref());
                dialog.present();
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
                dialog.set_transient_for(parent.as_ref());
                dialog.present();
            }
        ));
        self.add_action(&action_global_search);

        // ── Preferences ──
        let action_preferences = gio::SimpleAction::new("preferences", None);
        action_preferences.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, _| {
                tracing::info!("Preferences action triggered");
                let dialog = crate::dialogs::PreferencesDialog::new();
                let parent = app.active_window();
                dialog.present(parent.as_ref());
            }
        ));
        self.add_action(&action_preferences);

        self.setup_reminder_actions();
    }

    fn setup_reminder_actions(&self) {
        self.add_snooze_action("snooze-reminder-10m", 10 * 60);
        self.add_snooze_action("snooze-reminder-1h", 60 * 60);

        let action = gio::SimpleAction::new("dismiss-reminder", Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(key) = parameter.and_then(|value| value.str()).map(str::to_owned) else {
                    return;
                };
                if let Ok(mut store) = ReminderStore::new() {
                    if let Err(error) = store.dismiss(&key) {
                        tracing::warn!(%error, "Could not persist dismissed reminder");
                    }
                }
                app.withdraw_notification(&reminder_notification_id(&key));
            }
        ));
        self.add_action(&action);
    }

    fn add_snooze_action(&self, name: &str, seconds: i64) {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        action.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, parameter| {
                let Some(key) = parameter.and_then(|value| value.str()).map(str::to_owned) else {
                    return;
                };
                let until = chrono::Utc::now().timestamp().saturating_add(seconds);
                if let Ok(mut store) = ReminderStore::new() {
                    if let Err(error) = store.snooze_until(&key, until) {
                        tracing::warn!(%error, "Could not persist snoozed reminder");
                    }
                }
                app.withdraw_notification(&reminder_notification_id(&key));
            }
        ));
        self.add_action(&action);
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

    /// Poll the portable local calendar store.  EDS-backed snapshots use the
    /// same candidate/state API once the native Linux watcher is enabled; the
    /// macOS development path intentionally never tries to fake that bridge.
    fn check_local_reminders(&self) {
        if AppSettings::try_new().is_some_and(|settings| !settings.notifications_enabled()) {
            return;
        }
        if self.imp().reminder_check_in_progress.replace(true) {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            // Calendar files and reminder state can be large and are not part
            // of the first GTK frame.  Keep all filesystem/parsing work off
            // the main context; only the final desktop notifications return
            // to GTK below.
            let snapshot = CalendarStoreService::new().load_snapshot();
            let candidates = reminder_service::candidates_from_snapshot(&snapshot, now);
            let Ok(mut store) = ReminderStore::new() else {
                tracing::debug!("Portable reminder state is unavailable");
                let _ = sender.send_blocking(Vec::new());
                return;
            };
            let due = store.due_reminders(&candidates, now);
            // Delivery is marked only after the GTK notification has been
            // handed to the desktop, so a failed startup cannot silently
            // consume a reminder.
            drop(store);
            if let Err(error) = sender.send_blocking(due) {
                tracing::debug!(%error, "Reminder result was dropped before delivery");
            }
        });

        let app = self.downgrade();
        glib::spawn_future_local(async move {
            let due = receiver.recv().await.unwrap_or_default();
            let Some(app) = app.upgrade() else {
                return;
            };
            app.imp().reminder_check_in_progress.set(false);
            let mut delivered_keys = Vec::with_capacity(due.len());
            for candidate in due {
                let notification = gio::Notification::new(&candidate.title);
                let body = match candidate.kind {
                    ReminderKind::Event => "Kalendertermin ist fällig",
                    ReminderKind::Task => "Aufgabe ist fällig",
                };
                notification.set_body(Some(body));
                let target = candidate.key.to_variant();
                notification.add_button_with_target_value(
                    "10 Minuten snoozen",
                    "app.snooze-reminder-10m",
                    Some(&target),
                );
                notification.add_button_with_target_value(
                    "1 Stunde snoozen",
                    "app.snooze-reminder-1h",
                    Some(&target),
                );
                notification.add_button_with_target_value(
                    "Erledigt",
                    "app.dismiss-reminder",
                    Some(&target),
                );
                notification
                    .set_default_action_and_target_value("app.dismiss-reminder", Some(&target));
                let notification_id = reminder_notification_id(&candidate.key);
                app.send_notification(Some(&notification_id), &notification);
                delivered_keys.push(candidate.key);
            }
            if !delivered_keys.is_empty() {
                std::thread::spawn(move || {
                    let Ok(mut store) = ReminderStore::new() else {
                        tracing::debug!("Portable reminder state is unavailable after delivery");
                        return;
                    };
                    for key in delivered_keys {
                        if let Err(error) = store.mark_delivered(&key) {
                            tracing::warn!(%error, "Could not persist delivered reminder");
                        }
                    }
                });
            }
        });
    }

    /// Set up keyboard accelerators for actions.
    fn setup_accels(&self) {
        self.set_accels_for_action("app.quit", &["<Ctrl>Q"]);
        self.set_accels_for_action("app.compose", &["<Ctrl>N"]);
        self.set_accels_for_action("app.drafts", &["<Ctrl><Shift>D"]);
        self.set_accels_for_action("app.global-search", &["<Ctrl><Shift>F"]);
        self.set_accels_for_action("app.preferences", &["<Ctrl>comma"]);
        self.set_accels_for_action("win.show-help-overlay", &["<Ctrl>question"]);
        self.set_accels_for_action("win.search", &["<Ctrl>F"]);
        self.set_accels_for_action("win.refresh-mail", &["F5"]);
        self.set_accels_for_action("win.undo", &["<Ctrl>Z"]);
        self.set_accels_for_action("win.redo", &["<Ctrl><Shift>Z", "<Ctrl>Y"]);
        self.set_accels_for_action("win.toggle-sidebar", &["F9"]);

        // Module switching: Ctrl+1…5
        self.set_accels_for_action("win.switch-mail", &["<Ctrl>1"]);
        self.set_accels_for_action("win.switch-calendar", &["<Ctrl>2"]);
        self.set_accels_for_action("win.switch-contacts", &["<Ctrl>3"]);
        self.set_accels_for_action("win.switch-tasks", &["<Ctrl>4"]);
        self.set_accels_for_action("win.switch-memos", &["<Ctrl>5"]);

        // Mail navigation (vim-style)
        self.set_accels_for_action("win.mail-next", &["J"]);
        self.set_accels_for_action("win.mail-prev", &["K"]);
        self.set_accels_for_action("win.reply", &["<Ctrl>R"]);
        self.set_accels_for_action("win.reply-all", &["<Ctrl><Shift>R"]);
        self.set_accels_for_action("win.forward", &["<Ctrl>L"]);
        self.set_accels_for_action("win.export", &["<Ctrl><Shift>S"]);
        self.set_accels_for_action("win.delete", &["Delete"]);
        self.set_accels_for_action("win.mark-unread", &["<Ctrl>U"]);
        self.set_accels_for_action("win.toggle-important", &["<Ctrl>I"]);
        self.set_accels_for_action("win.select-all-mail", &["<Ctrl>A"]);

        tracing::debug!("Keyboard accelerators registered");
    }

    /// Show the About dialog using AdwAboutDialog.
    fn show_about_dialog(&self) {
        if let Some(win) = self.active_window() {
            crate::dialogs::show_about_dialog(&win);
        }
    }
}

fn reminder_notification_id(key: &str) -> String {
    format!("evolution-reminder-{key}")
}

impl Default for EvolutionApplication {
    fn default() -> Self {
        Self::new()
    }
}
