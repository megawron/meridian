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
use crate::services::reminder_service::{
    self, ReminderComponentKind, ReminderIdentity, ReminderKind, ReminderScheduler, ReminderStore,
    SystemClock, parse_reminder_action_target, parse_reminder_deep_link,
};
use crate::window::EvolutionWindow;
use crate::{APP_ID, RESOURCE_PATH};

const ACCELERATOR_BINDINGS: &[(&str, &[&str])] = &[
    ("app.quit", &["<Ctrl>Q"]),
    ("app.drafts", &["<Ctrl><Shift>D"]),
    ("app.global-search", &["<Ctrl><Shift>G"]),
    ("app.preferences", &["<Ctrl>comma"]),
    ("win.show-help-overlay", &["<Ctrl>question"]),
    ("win.search", &["<Ctrl>F"]),
    ("win.refresh-mail", &["F5"]),
    ("win.mail-send-receive", &["F12"]),
    ("win.undo", &["<Ctrl>Z"]),
    ("win.redo", &["<Ctrl><Shift>Z"]),
    ("win.apply-filters", &["<Ctrl>Y"]),
    ("win.toggle-sidebar", &["F9"]),
    ("win.new", &["<Ctrl>N"]),
    ("win.switch-mail", &["<Ctrl>1"]),
    ("win.switch-calendar", &["<Ctrl>2"]),
    ("win.switch-contacts", &["<Ctrl>3"]),
    ("win.switch-tasks", &["<Ctrl>4"]),
    ("win.switch-memos", &["<Ctrl>5"]),
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
    ("win.delete", &["Delete"]),
    ("win.mark-read", &["<Ctrl>K"]),
    ("win.mark-unread", &["<Ctrl><Shift>K"]),
    ("win.mark-all-read", &["<Ctrl><Shift>M"]),
    ("win.mark-thread-read", &["<Ctrl><Alt>M"]),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal"]),
    ("win.zoom-out", &["<Ctrl>minus"]),
    ("win.zoom-reset", &["<Ctrl>0"]),
    ("win.expand-all-threads", &["<Shift>plus", "<Shift>KP_Add"]),
    ("win.collapse-all-threads", &["<Shift>minus", "<Shift>KP_Subtract"]),
    ("win.toggle-important", &["<Ctrl>I"]),
    ("win.move-message", &["<Ctrl><Shift>V"]),
    ("win.copy-message", &["<Ctrl><Shift>C"]),
    ("win.select-all-mail", &["<Ctrl>A"]),
];

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

    #[derive(Debug, Default)]
    pub struct EvolutionApplication {
        pub reminder_source: RefCell<Option<glib::SourceId>>,
        pub reminder_check_in_progress: Cell<bool>,
        pub reminder_action_in_progress: Cell<bool>,
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
            let window = app.main_window();

            tracing::debug!("Presenting main window after synchronous UI construction");
            window.present();
            window.present_existing_reminders();
            tracing::debug!("Main window presented");
        }

        fn open(&self, files: &[gio::File], hint: &str) {
            self.parent_open(files, hint);
            let app = self.obj();
            for file in files {
                app.route_component_uri(&file.uri().to_string(), None);
            }
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
                    app.show_reminder_error("Ungültiges Reminder-Aktionsziel");
                    return;
                };
                app.route_component_identity(target.identity, None);
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
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                app.apply_reminder_action(target, ReminderAction::Snooze(seconds));
            }
        ));
        self.add_action(&action);
    }

    fn main_window(&self) -> EvolutionWindow {
        if let Some(window) = self
            .active_window()
            .and_then(|window| window.downcast::<EvolutionWindow>().ok())
        {
            return window;
        }
        self.windows()
            .into_iter()
            .find_map(|window| window.downcast::<EvolutionWindow>().ok())
            .unwrap_or_else(|| EvolutionWindow::new(self))
    }

    fn show_reminder_error(&self, message: &str) {
        let window = self.main_window();
        window.present();
        window.add_toast(&adw::Toast::new(message));
    }

    fn route_component_uri(&self, uri: &str, expected_kind: Option<ReminderComponentKind>) -> bool {
        let identity = match parse_reminder_deep_link(uri) {
            Ok(identity) => identity,
            Err(error) => {
                self.show_reminder_error(&format!("Reminder-Link nicht geöffnet: {error}"));
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
            self.show_reminder_error("Reminder-Link passt nicht zur angeforderten Komponente");
            return false;
        }
        let window = self.main_window();
        window.present();
        window.open_component(identity)
    }

    fn set_reminder_action_sensitivity(&self, enabled: bool) {
        for name in [
            "snooze-reminder-10m",
            "snooze-reminder-1h",
            "dismiss-reminder",
        ] {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
    }

    fn apply_reminder_action(&self, target: &str, action: ReminderAction) -> bool {
        let raw_target = target.to_owned();
        let target = match parse_reminder_action_target(target) {
            Ok(target) => target,
            Err(error) => {
                self.show_reminder_error(&format!("Reminder-Aktion abgewiesen: {error}"));
                return false;
            }
        };
        if self.imp().reminder_action_in_progress.replace(true) {
            return false;
        }
        self.set_reminder_action_sensitivity(false);

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
                                "Reminder-Aktion ist veraltet",
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
                                "Reminder-Aktion ist veraltet",
                            ))
                        }
                    })?,
            });
            let _ = sender.send_blocking(result);
        });

        let app = self.downgrade();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            let Some(app) = app.upgrade() else {
                return;
            };
            app.imp().reminder_action_in_progress.set(false);
            app.set_reminder_action_sensitivity(true);
            let window = app.main_window();
            match result {
                Ok(Ok(())) => {
                    window.finish_reminder_action(&raw_target, true, None);
                    app.withdraw_notification(&reminder_notification_id(&target.key));
                }
                Ok(Err(error)) => {
                    let message = error.to_string();
                    window.finish_reminder_action(&raw_target, false, Some(message));
                }
                Err(error) => {
                    let message = format!("Reminder-Aktion abgebrochen: {error}");
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
        let local_calendar_directory =
            AppSettings::try_new().map(|settings| settings.local_calendar_directory());
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            // Calendar files and reminder state can be large and are not part
            // of the first GTK frame.  Keep all filesystem/parsing work off
            // the main context; only the final desktop notifications return
            // to GTK below.
            let scheduler = ReminderScheduler::new(SystemClock);
            let now = scheduler.now();
            let snapshot = reminder_service::calendar_store_for_worker(local_calendar_directory)
                .load_snapshot();
            let candidates = reminder_service::candidates_from_snapshot(&snapshot, now);
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
                        "Reminder-Zustand nicht gespeichert: {error}"
                    ));
                    return;
                }
                Err(error) => {
                    tracing::debug!(%error, "Reminder result channel closed");
                    return;
                }
            };
            let mut delivered = Vec::with_capacity(due.len());
            for delivery in due {
                let Ok(target) = delivery.action_target() else {
                    tracing::warn!(key = %delivery.candidate.key, "Skipping reminder with invalid action target");
                    continue;
                };
                let notification = gio::Notification::new(&delivery.candidate.title);
                let body = match delivery.candidate.kind {
                    ReminderKind::Event => "Kalendertermin ist fällig",
                    ReminderKind::Task => "Aufgabe ist fällig",
                    ReminderKind::Memo => "Notiz ist fällig",
                };
                notification.set_body(Some(body));
                let target = target.to_variant();
                notification.add_button_with_target_value(
                    "Öffnen",
                    "app.open-reminder",
                    Some(&target),
                );
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
                    .set_default_action_and_target_value("app.open-reminder", Some(&target));
                let notification_id = reminder_notification_id(&delivery.candidate.key);
                app.send_notification(Some(&notification_id), &notification);
                delivered.push(delivery);
            }
            if !delivered.is_empty() {
                let window = app.main_window();
                window.present_reminders(delivered.clone());
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

fn reminder_notification_id(key: &str) -> String {
    format!("evolution-reminder-{key}")
}

impl Default for EvolutionApplication {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(accelerators_for("win.mark-all-read"), &["<Ctrl><Shift>M"]);
    }
}
