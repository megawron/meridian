// SPDX-License-Identifier: GPL-3.0-or-later
//
// Services module — backend service layer for EDS communication.

pub mod account_config_service;
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
pub mod mail_filter_service;
pub mod mail_metadata_service;
pub mod mail_operations_service;
pub mod mail_store_service;
pub mod search_engine;
pub mod search_folder_service;
pub mod settings_service;
pub mod vcard_service;

pub use mail_store_service::MailStoreService;
pub use settings_service::AppSettings;
