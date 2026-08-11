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
pub mod inline_content_service;
pub mod itip_service;
pub mod mail_composer_service;
pub mod mail_filter_service;
pub mod mail_metadata_service;
pub mod mail_operations_service;
pub mod mail_store_service;
pub mod mail_thread_service;
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

/// Root for state owned by this application.  Keeping the override in one
/// place makes tests, backup/restore, and all portable stores address the same
/// data tree without changing the original Evolution/EDS directories.
pub(crate) fn app_data_dir() -> std::path::PathBuf {
    std::env::var_os("EVOLUTION_GTK4_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4"))
}
