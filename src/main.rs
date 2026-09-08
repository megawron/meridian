// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright 2026 megawron
//
// evolution-gtk4 — A modern GTK4/libadwaita mail & groupware client
//
// Application entrypoint. Initializes logging, GTK4, libadwaita,
// and launches the AdwApplication main loop.

mod action_contract;
mod application;
mod dialogs;
mod models;
mod services;
mod views;
mod window;

use application::EvolutionApplication;
use gio::prelude::*;
use glib::ExitCode;
use gtk::{gio, glib};

/// Application ID following GNOME reverse-DNS convention.
/// TODO: Update when project name is finalized.
pub const APP_ID: &str = "org.gnome.Evolution4";

/// Resource base path for GResource lookups.
pub const RESOURCE_PATH: &str = "/org/gnome/Evolution4";

fn main() -> ExitCode {
    // Initialize structured logging with tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "evolution_gtk4=debug,warn".into()),
        )
        .with_target(true)
        .init();

    tracing::info!("Starting Evolution GTK4 v{}", env!("CARGO_PKG_VERSION"));

    // Create and run the AdwApplication
    let non_unique = std::env::args().any(|a| a == "--non-unique");
    let flags = if non_unique {
        gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE
    } else {
        gio::ApplicationFlags::HANDLES_OPEN
    };
    let app = EvolutionApplication::with_flags(flags);
    if non_unique {
        let filtered: Vec<String> = std::env::args()
            .filter(|a| a != "--non-unique" && a != "--compose" && a != "--preferences" && a != "--account-setup")
            .collect();
        app.run_with_args(&filtered)
    } else {
        let filtered: Vec<String> = std::env::args()
            .filter(|a| a != "--compose" && a != "--preferences" && a != "--account-setup")
            .collect();
        app.run_with_args(&filtered)
    }
}
