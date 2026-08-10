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

use crate::window::EvolutionWindow;
use crate::{APP_ID, RESOURCE_PATH};

// ─── GObject Subclass Boilerplate ───────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct EvolutionApplication;

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

            // Register global keyboard shortcuts / actions
            app.setup_actions();
            app.setup_accels();
        }

        /// Called when the application is activated (or re-focused).
        fn activate(&self) {
            self.parent_activate();
            let app = self.obj();

            tracing::debug!("Application activate");

            // GTK4-rs/libadwaita is the single production frontend.  EDS and
            // Camel services remain independent of this widget layer.
            let window = if let Some(window) = app.active_window() {
                window
            } else {
                let window = EvolutionWindow::new(&app);
                window.upcast()
            };

            window.present();
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
    }

    /// Set up keyboard accelerators for actions.
    fn setup_accels(&self) {
        self.set_accels_for_action("app.quit", &["<Ctrl>Q"]);
        self.set_accels_for_action("app.compose", &["<Ctrl>N"]);
        self.set_accels_for_action("app.preferences", &["<Ctrl>comma"]);
        self.set_accels_for_action("win.search", &["<Ctrl>F"]);
        self.set_accels_for_action("win.refresh-mail", &["F5"]);
        self.set_accels_for_action("win.undo", &["<Ctrl>Z"]);
        self.set_accels_for_action("win.redo", &["<Ctrl><Shift>Z", "<Ctrl>Y"]);
        self.set_accels_for_action("win.toggle-sidebar", &["F9"]);

        // Module switching: Ctrl+1/2/3
        self.set_accels_for_action("win.switch-mail", &["<Ctrl>1"]);
        self.set_accels_for_action("win.switch-calendar", &["<Ctrl>2"]);
        self.set_accels_for_action("win.switch-contacts", &["<Ctrl>3"]);

        // Mail navigation (vim-style)
        self.set_accels_for_action("win.mail-next", &["J"]);
        self.set_accels_for_action("win.mail-prev", &["K"]);
        self.set_accels_for_action("win.reply", &["<Ctrl>R"]);
        self.set_accels_for_action("win.reply-all", &["<Ctrl><Shift>R"]);
        self.set_accels_for_action("win.forward", &["<Ctrl>L"]);
        self.set_accels_for_action("win.delete", &["Delete"]);
        self.set_accels_for_action("win.mark-unread", &["<Ctrl>U"]);

        tracing::debug!("Keyboard accelerators registered");
    }

    /// Show the About dialog using AdwAboutDialog.
    fn show_about_dialog(&self) {
        if let Some(win) = self.active_window() {
            crate::dialogs::show_about_dialog(&win);
        }
    }
}

impl Default for EvolutionApplication {
    fn default() -> Self {
        Self::new()
    }
}
