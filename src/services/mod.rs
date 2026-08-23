// SPDX-License-Identifier: GPL-3.0-or-later
//
// Services module — backend service layer for EDS communication.

pub mod account_config_service;
pub mod availability_service;
pub mod backup_service;
pub mod calendar_category_service;
pub mod calendar_store_service;
pub mod camel_mail_service;
pub mod contact_store_service;
pub mod crypto_service;
pub mod draft_store_service;
pub mod ebook_service;
pub mod ecal_service;
pub mod eds_service;
pub mod eds_source_cache_service;
pub mod exchange_service;
pub mod html_sanitizer;
pub mod ical_time_service;
pub mod inline_content_service;
pub mod itip_service;
pub mod mail_composer_service;
pub mod mail_filter_service;
pub mod mail_metadata_service;
pub mod mail_operations_service;
pub mod mail_print_service;
pub mod mail_store_service;
pub mod mail_thread_service;
pub mod mail_transport_service;
pub mod memo_filter_service;
pub mod memo_surface_service;
pub mod migration_service;
pub mod out_of_office_service;
pub mod recurrence_service;
pub mod reminder_service;
pub mod search_engine;
pub mod search_folder_service;
pub mod settings_service;
pub mod signature_store_service;
pub mod snippet_store_service;
pub mod template_store_service;
pub mod vcard_service;

pub use calendar_store_service::CalendarStoreService;
pub use contact_store_service::ContactStoreService;
pub use mail_store_service::MailStoreService;
pub use settings_service::AppSettings;

fn non_empty_env_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

fn home_dir() -> std::path::PathBuf {
    non_empty_env_path("HOME")
        .or_else(|| non_empty_env_path("USERPROFILE"))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Root for state owned by this application.  Keeping the override in one
/// place makes tests, backup/restore, and all portable stores address the same
/// data tree without changing the original Evolution/EDS directories.
///
/// This deliberately uses only process/environment APIs.  Service workers can
/// resolve account and cache files without touching GTK, GObject or GSettings;
/// the platform defaults mirror GLib's user-data locations on the supported
/// desktop platforms.
pub(crate) fn app_data_dir() -> std::path::PathBuf {
    if let Some(path) = non_empty_env_path("EVOLUTION_GTK4_DATA_DIR") {
        return path;
    }

    #[cfg(target_os = "macos")]
    let base = home_dir().join("Library/Application Support");
    #[cfg(target_os = "windows")]
    let base =
        non_empty_env_path("LOCALAPPDATA").unwrap_or_else(|| home_dir().join("AppData/Local"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base =
        non_empty_env_path("XDG_DATA_HOME").unwrap_or_else(|| home_dir().join(".local/share"));

    base.join("evolution-gtk4")
}

/// Application cache root with the same worker-safe platform policy as
/// [`app_data_dir`].
pub(crate) fn app_cache_dir() -> std::path::PathBuf {
    if let Some(path) = non_empty_env_path("EVOLUTION_GTK4_CACHE_DIR") {
        return path;
    }

    #[cfg(target_os = "macos")]
    let base = home_dir().join("Library/Caches");
    #[cfg(target_os = "windows")]
    let base =
        non_empty_env_path("LOCALAPPDATA").unwrap_or_else(|| home_dir().join("AppData/Local"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = non_empty_env_path("XDG_CACHE_HOME").unwrap_or_else(|| home_dir().join(".cache"));

    base.join("evolution-gtk4")
}

/// Return whether a configured local store is inside the application's own
/// data tree.  Paths selected outside that tree are intentionally treated as
/// read-only import sources; app-owned paths remain editable after a restart
/// or a profile migration has persisted their location in GSettings.
pub(crate) fn is_app_owned_store_path(path: &std::path::Path) -> bool {
    path.starts_with(app_data_dir())
}

#[cfg(test)]
mod tests {
    use super::{app_data_dir, is_app_owned_store_path};

    #[test]
    fn app_owned_store_path_is_component_bounded() {
        let root = app_data_dir();
        assert!(is_app_owned_store_path(&root.join("ical")));

        let sibling = root.with_file_name(format!(
            "{}-external",
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("evolution-gtk4")
        ));
        assert!(!is_app_owned_store_path(&sibling.join("ical")));
    }
}
