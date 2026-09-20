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
pub mod i18n;
mod models;
mod services;
mod views;
mod window;

use application::EvolutionApplication;
use gio::prelude::*;
use glib::ExitCode;
use gtk::{gio, glib};

/// Application ID following GNOME reverse-DNS convention.
pub const APP_ID: &str = "com.martingawron.Meridian";

/// Resource base path for GResource lookups.
pub const RESOURCE_PATH: &str = "/com/martingawron/Meridian";

#[cfg(target_os = "macos")]
fn setup_platform_environment() {
    let mut xdg_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let brew_shares = ["/opt/homebrew/share", "/usr/local/share"];
    for share in &brew_shares {
        if std::path::Path::new(share).exists() && !xdg_dirs.contains(share) {
            if !xdg_dirs.is_empty() {
                xdg_dirs = format!("{share}:{xdg_dirs}");
            } else {
                xdg_dirs = share.to_string();
            }
        }
    }
    if !xdg_dirs.is_empty() {
        unsafe {
            std::env::set_var("XDG_DATA_DIRS", &xdg_dirs);
        }
    }

    // GTK 4's default 'ngl' (New OpenGL) renderer on macOS Quartz backend suffers from
    // damage-rect scissor clipping bugs with Libadwaita dialogs/sheets, leaving transparent holes.
    // Default to 'cairo' on macOS for rock-solid, flicker-free rendering unless overridden.
    if std::env::var("GSK_RENDERER").is_err() {
        unsafe {
            std::env::set_var("GSK_RENDERER", "cairo");
        }
    }

    // If running inside a macOS .app bundle, locate bundled GSettings schemas
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bundle_res) = exe.parent().and_then(|p| p.parent()).map(|p| p.join("Resources/schemas")) {
            if bundle_res.is_dir() && std::env::var("GSETTINGS_SCHEMA_DIR").is_err() {
                unsafe {
                    std::env::set_var("GSETTINGS_SCHEMA_DIR", bundle_res);
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn setup_platform_environment() {}

fn main() -> ExitCode {
    setup_platform_environment();

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
    let non_unique = std::env::args().any(|a| a == "--non-unique");
    let flags = if non_unique {
        gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE
    } else {
        gio::ApplicationFlags::HANDLES_OPEN
    };
    let app = EvolutionApplication::with_flags(flags);
    if non_unique {
        let filtered: Vec<String> = std::env::args()
            .filter(|a| a != "--non-unique" && a != "--compose" && a != "--preferences" && a != "--account-setup" && a != "--receiving")
            .collect();
        app.run_with_args(&filtered)
    } else {
        let filtered: Vec<String> = std::env::args()
            .filter(|a| a != "--compose" && a != "--preferences" && a != "--account-setup" && a != "--receiving")
            .collect();
        app.run_with_args(&filtered)
    }
}
