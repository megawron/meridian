// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright 2026 megawron
//
// Meridian — A modern GTK4/libadwaita mail & groupware client
//
// Application entrypoint. Initializes logging, GTK4, libadwaita,
// and launches the AdwApplication main loop.

#![allow(dead_code)]

mod action_contract;
mod application;
mod dialogs;
pub mod i18n;
mod models;
mod services;
mod views;
mod window;

use application::MeridianApplication;
use gio::prelude::*;
use glib::ExitCode;
use gtk::{gio, glib};

/// Application ID following GNOME reverse-DNS convention.
pub const APP_ID: &str = "com.martingawron.Meridian";

/// Resource base path for GResource lookups.
pub const RESOURCE_PATH: &str = "/com/martingawron/Meridian";

fn main() -> ExitCode {

    glib::set_prgname(Some(APP_ID));
    glib::set_application_name("Meridian");

    // Initialize GNU gettext internationalization
    i18n::init_i18n();

    // Initialize structured logging with tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "meridian=debug,warn".into()),
        )
        .with_target(true)
        .with_writer(|| services::log_buffer::LogBufferWriter)
        .init();

    tracing::info!("Starting Meridian v{}", env!("CARGO_PKG_VERSION"));

    // Create and run the AdwApplication
    let is_preview = std::env::args().any(|a| a.starts_with("--settings-preview") || a.starts_with("--preferences-preview"));
    let non_unique = cfg!(debug_assertions) || is_preview || std::env::args().any(|a| a == "--non-unique");
    let flags = if non_unique {
        gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE
    } else {
        gio::ApplicationFlags::HANDLES_OPEN
    };
    let app = MeridianApplication::with_flags(flags);
    let filtered: Vec<String> = std::env::args()
        .enumerate()
        .filter_map(|(idx, a)| {
            if idx == 0 {
                return Some(a);
            }
            if a == "--calendar" || a == "--component=calendar" {
                return Some("meridian://open/calendar".to_string());
            }
            if a == "--contacts" || a == "--component=contacts" {
                return Some("meridian://open/contacts".to_string());
            }
            if a == "--tasks" || a == "--component=tasks" || a == "--component=memos" {
                return Some("meridian://open/tasks".to_string());
            }
            if a == "--mail" || a == "--component=mail" {
                return Some("meridian://open/mail".to_string());
            }
            if a == "--compose" {
                return Some("meridian://open/compose".to_string());
            }
            if a.starts_with("--settings-preview")
                || a.starts_with("--preferences-preview")
                || a.starts_with("--component=")
                || a == "--non-unique"
                || a == "--preferences"
                || a == "--account-setup"
                || a == "--receiving"
            {
                return None;
            }
            Some(a)
        })
        .collect();
    app.run_with_args(&filtered)
}
