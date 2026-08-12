// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable backup/restore for application-owned state.
//
// This service deliberately does not touch the original Evolution Data Server
// profile, Camel stores, Secret Service, or user-selected external mail,
// calendar, and contact directories.  It snapshots the JSON state, app-owned
// local mail/ICS/vCard stores, and the non-secret GSettings values we expose.

use chrono::Utc;
use glib::{Checksum, ChecksumType};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::settings_service::AppSettings;

const CURRENT_VERSION: u32 = 3;

fn default_calendar_view() -> String {
    "month".to_string()
}

fn default_calendar_week_start() -> String {
    "monday".to_string()
}

fn default_calendar_workdays() -> String {
    "monday,tuesday,wednesday,thursday,friday".to_string()
}

fn default_calendar_hidden_sources() -> String {
    String::new()
}

fn default_mail_sort_field() -> String {
    "date".to_string()
}

fn default_calendar_time_zone() -> String {
    "system".to_string()
}

fn default_task_filter() -> String {
    "all".to_string()
}

fn default_task_sort_field() -> String {
    "due".to_string()
}

fn default_task_source_filter() -> String {
    String::new()
}

fn default_task_sort_descending() -> bool {
    true
}

fn default_memo_sort_field() -> String {
    "updated".to_string()
}

fn default_memo_source_filter() -> String {
    String::new()
}

fn default_memo_sort_descending() -> bool {
    true
}

const APP_FILES: &[&str] = &[
    "accounts.json",
    "drafts.json",
    "eds-sources.json",
    "mail-filters.json",
    "mail-state.json",
    "mail-undo.json",
    "remote-content.json",
    "reminders.json",
    "search-folders.json",
    "signatures.json",
    "mail-snippets.json",
    "mail-templates.json",
    "calendar-categories.json",
    "out-of-office.json",
    "ical/evolution-gtk4-local.ics",
    "vcards/evolution-gtk4-local.vcf",
];

const APP_OWNED_DIRECTORIES: &[&str] = &["ical", "vcards", "mail"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BackupFile {
    relative_path: String,
    contents: Vec<u8>,
    /// Optional for backwards compatibility with version-3 backups created
    /// before integrity metadata was added.
    #[serde(default)]
    checksum_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BackupSettings {
    #[serde(default = "default_window_width")]
    window_width: i32,
    #[serde(default = "default_window_height")]
    window_height: i32,
    #[serde(default)]
    is_maximized: bool,
    #[serde(default = "default_active_view")]
    active_view: String,
    #[serde(default)]
    default_account: String,
    #[serde(default)]
    default_calendar_source: String,
    #[serde(default)]
    default_address_book_source: String,
    #[serde(default)]
    default_task_source: String,
    #[serde(default)]
    default_memo_source: String,
    #[serde(default)]
    auto_load_remote_images: bool,
    #[serde(default = "default_force_dark_mode")]
    force_dark_mode: bool,
    #[serde(default = "default_render_html")]
    render_html: bool,
    #[serde(default = "default_composer_html")]
    composer_html: bool,
    #[serde(default)]
    request_read_receipt: bool,
    #[serde(default)]
    spellcheck_languages: String,
    #[serde(default = "default_mail_font_family")]
    mail_font_family: String,
    #[serde(default = "default_mail_font_size")]
    mail_font_size: u32,
    #[serde(default = "default_mark_read_on_selection")]
    mark_read_on_selection: bool,
    #[serde(default = "default_show_message_preview")]
    show_message_preview: bool,
    #[serde(default = "default_mail_default_folder")]
    mail_default_folder: String,
    #[serde(default = "default_mail_layout")]
    mail_layout: String,
    #[serde(default = "default_mail_show_toolbar")]
    mail_show_toolbar: bool,
    #[serde(default = "default_mail_show_sidebar")]
    mail_show_sidebar: bool,
    #[serde(default = "default_mail_sidebar_position")]
    mail_sidebar_position: i32,
    #[serde(default = "default_mail_modern_preview_position")]
    mail_modern_preview_position: i32,
    #[serde(default = "default_mail_classic_preview_position")]
    mail_classic_preview_position: i32,
    #[serde(default = "default_mail_row_density")]
    mail_row_density: String,
    #[serde(default = "default_mail_show_avatar")]
    mail_show_avatar: bool,
    #[serde(default = "default_mail_show_sender")]
    mail_show_sender: bool,
    #[serde(default = "default_mail_show_recipient")]
    mail_show_recipient: bool,
    #[serde(default = "default_mail_show_date")]
    mail_show_date: bool,
    #[serde(default = "default_mail_show_attachment")]
    mail_show_attachment: bool,
    #[serde(default = "default_mail_show_preview_text")]
    mail_show_preview_text: bool,
    #[serde(default = "default_mail_show_labels")]
    mail_show_labels: bool,
    #[serde(default = "default_color_scheme")]
    color_scheme: String,
    #[serde(default)]
    sign_new_messages: bool,
    #[serde(default)]
    default_pgp_key: String,
    #[serde(default = "default_crypto_protocol")]
    crypto_protocol: String,
    #[serde(default)]
    smime_signer_certificate: String,
    #[serde(default)]
    smime_private_key: String,
    #[serde(default)]
    smime_recipient_certificates: String,
    #[serde(default)]
    smime_ca_file: String,
    #[serde(default = "default_mail_sync_interval")]
    mail_sync_interval: u32,
    #[serde(default = "default_draft_autosave_interval")]
    draft_autosave_interval: u32,
    #[serde(default)]
    append_signature: bool,
    #[serde(default)]
    signature_text: String,
    #[serde(default)]
    local_mail_directory: String,
    #[serde(default)]
    local_calendar_directory: String,
    #[serde(default)]
    local_contact_directory: String,
    #[serde(default = "default_quote_original")]
    quote_original: bool,
    #[serde(default = "default_quote_style")]
    quote_style: String,
    #[serde(default = "default_calendar_view")]
    calendar_view: String,
    #[serde(default = "default_calendar_week_start")]
    calendar_week_start: String,
    #[serde(default = "default_calendar_workdays")]
    calendar_workdays: String,
    #[serde(default = "default_workday_start")]
    calendar_workday_start: u32,
    #[serde(default = "default_workday_end")]
    calendar_workday_end: u32,
    #[serde(default = "default_reminder")]
    calendar_default_reminder: u32,
    #[serde(default = "default_notifications_enabled")]
    notifications_enabled: bool,
    #[serde(default = "default_confirm_destructive_actions")]
    confirm_destructive_actions: bool,
    #[serde(default)]
    calendar_show_week_numbers: bool,
    #[serde(default = "default_calendar_hidden_sources")]
    calendar_hidden_sources: String,
    #[serde(default = "default_date_format")]
    date_format: String,
    #[serde(default = "default_time_format")]
    time_format: String,
    #[serde(default = "default_mail_sort_field")]
    mail_sort_field: String,
    #[serde(default = "default_mail_sort_descending")]
    mail_sort_descending: bool,
    #[serde(default = "default_mail_threading")]
    mail_threading: bool,
    #[serde(default = "default_calendar_time_zone")]
    calendar_time_zone: String,
    #[serde(default = "default_task_filter")]
    task_filter: String,
    #[serde(default = "default_task_source_filter")]
    task_source_filter: String,
    #[serde(default = "default_task_sort_field")]
    task_sort_field: String,
    #[serde(default = "default_task_sort_descending")]
    task_sort_descending: bool,
    #[serde(default = "default_memo_sort_field")]
    memo_sort_field: String,
    #[serde(default = "default_memo_source_filter")]
    memo_source_filter: String,
    #[serde(default = "default_memo_sort_descending")]
    memo_sort_descending: bool,
}

fn default_window_width() -> i32 {
    1_100
}

fn default_window_height() -> i32 {
    700
}

fn default_active_view() -> String {
    "mail".to_string()
}

fn default_force_dark_mode() -> bool {
    true
}

fn default_render_html() -> bool {
    true
}

fn default_color_scheme() -> String {
    "system".to_string()
}

fn default_mail_sync_interval() -> u32 {
    30
}

fn default_crypto_protocol() -> String {
    "openpgp".to_string()
}

fn default_draft_autosave_interval() -> u32 {
    30
}

fn default_quote_original() -> bool {
    true
}

fn default_quote_style() -> String {
    "blockquote".to_string()
}

fn default_workday_start() -> u32 {
    540
}

fn default_workday_end() -> u32 {
    1_020
}

fn default_reminder() -> u32 {
    15
}

fn default_notifications_enabled() -> bool {
    true
}

fn default_confirm_destructive_actions() -> bool {
    true
}

fn default_mark_read_on_selection() -> bool {
    true
}

fn default_show_message_preview() -> bool {
    true
}

fn default_mail_default_folder() -> String {
    "inbox".to_string()
}

fn default_mail_row_density() -> String {
    "comfortable".to_string()
}

fn default_mail_show_avatar() -> bool {
    true
}

fn default_mail_show_sender() -> bool {
    true
}

fn default_mail_show_recipient() -> bool {
    false
}

fn default_mail_show_date() -> bool {
    true
}

fn default_mail_show_attachment() -> bool {
    true
}

fn default_mail_show_preview_text() -> bool {
    true
}

fn default_mail_show_labels() -> bool {
    true
}

fn default_mail_layout() -> String {
    "classic".to_string()
}

fn default_mail_show_toolbar() -> bool {
    true
}

fn default_mail_show_sidebar() -> bool {
    true
}

fn default_mail_sidebar_position() -> i32 {
    220
}

fn default_mail_modern_preview_position() -> i32 {
    420
}

fn default_mail_classic_preview_position() -> i32 {
    360
}

fn default_composer_html() -> bool {
    true
}

fn default_mail_font_family() -> String {
    "system-ui, sans-serif".to_string()
}

fn default_mail_font_size() -> u32 {
    14
}

fn default_date_format() -> String {
    "german".to_string()
}

fn default_time_format() -> String {
    "24h".to_string()
}

fn default_mail_sort_descending() -> bool {
    true
}

fn default_mail_threading() -> bool {
    true
}

impl BackupSettings {
    fn from_app(settings: &AppSettings) -> Self {
        Self {
            window_width: settings.window_width(),
            window_height: settings.window_height(),
            is_maximized: settings.is_maximized(),
            active_view: settings.active_view(),
            default_account: settings.default_account_id(),
            default_calendar_source: settings.default_calendar_source_uid(),
            default_address_book_source: settings.default_address_book_source_uid(),
            default_task_source: settings.default_task_source_uid(),
            default_memo_source: settings.default_memo_source_uid(),
            auto_load_remote_images: settings.auto_load_remote_images(),
            force_dark_mode: settings.force_dark_mode(),
            render_html: settings.render_html(),
            composer_html: settings.composer_html(),
            request_read_receipt: settings.request_read_receipt(),
            spellcheck_languages: settings.spellcheck_languages(),
            mail_font_family: settings.mail_font_family(),
            mail_font_size: settings.mail_font_size(),
            mark_read_on_selection: settings.mark_read_on_selection(),
            show_message_preview: settings.show_message_preview(),
            mail_default_folder: settings.mail_default_folder(),
            mail_layout: settings.mail_layout(),
            mail_show_toolbar: settings.mail_show_toolbar(),
            mail_show_sidebar: settings.mail_show_sidebar(),
            mail_sidebar_position: settings.mail_sidebar_position(),
            mail_modern_preview_position: settings.mail_preview_position("modern"),
            mail_classic_preview_position: settings.mail_preview_position("classic"),
            mail_row_density: settings.mail_row_density(),
            mail_show_avatar: settings.mail_show_avatar(),
            mail_show_sender: settings.mail_show_sender(),
            mail_show_recipient: settings.mail_show_recipient(),
            mail_show_date: settings.mail_show_date(),
            mail_show_attachment: settings.mail_show_attachment(),
            mail_show_preview_text: settings.mail_show_preview_text(),
            mail_show_labels: settings.mail_show_labels(),
            color_scheme: settings.color_scheme(),
            sign_new_messages: settings.sign_new_messages(),
            default_pgp_key: settings.default_pgp_key(),
            crypto_protocol: settings.crypto_protocol(),
            smime_signer_certificate: settings.smime_signer_certificate(),
            smime_private_key: settings.smime_private_key(),
            smime_recipient_certificates: settings.smime_recipient_certificates().join("\n"),
            smime_ca_file: settings.smime_ca_file(),
            mail_sync_interval: settings.mail_sync_interval_seconds(),
            draft_autosave_interval: settings.draft_autosave_interval_seconds(),
            append_signature: settings.append_signature(),
            signature_text: settings.signature_text(),
            local_mail_directory: settings.local_mail_directory(),
            local_calendar_directory: settings.local_calendar_directory(),
            local_contact_directory: settings.local_contact_directory(),
            quote_original: settings.quote_original(),
            quote_style: settings.quote_style(),
            calendar_view: settings.calendar_view(),
            calendar_week_start: settings.calendar_week_start(),
            calendar_workdays: settings.calendar_workdays(),
            calendar_workday_start: settings.calendar_workday_start_minutes(),
            calendar_workday_end: settings.calendar_workday_end_minutes(),
            calendar_default_reminder: settings.calendar_default_reminder_minutes(),
            notifications_enabled: settings.notifications_enabled(),
            confirm_destructive_actions: settings.confirm_destructive_actions(),
            calendar_show_week_numbers: settings.calendar_show_week_numbers(),
            calendar_hidden_sources: settings.calendar_hidden_sources().join(","),
            date_format: settings.date_format(),
            time_format: settings.time_format(),
            mail_sort_field: settings.mail_sort_field(),
            mail_sort_descending: settings.mail_sort_descending(),
            mail_threading: settings.mail_threading(),
            calendar_time_zone: settings.calendar_time_zone(),
            task_filter: settings.task_filter(),
            task_source_filter: settings.task_source_filter(),
            task_sort_field: settings.task_sort_field(),
            task_sort_descending: settings.task_sort_descending(),
            memo_sort_field: settings.memo_sort_field(),
            memo_source_filter: settings.memo_source_filter(),
            memo_sort_descending: settings.memo_sort_descending(),
        }
    }

    fn apply_to(&self, settings: &AppSettings) {
        settings.set_window_width(self.window_width);
        settings.set_window_height(self.window_height);
        settings.set_is_maximized(self.is_maximized);
        settings.set_active_view(&self.active_view);
        settings.set_default_account_id(&self.default_account);
        settings.set_default_calendar_source_uid(&self.default_calendar_source);
        settings.set_default_address_book_source_uid(&self.default_address_book_source);
        settings.set_default_task_source_uid(&self.default_task_source);
        settings.set_default_memo_source_uid(&self.default_memo_source);
        settings.set_auto_load_remote_images(self.auto_load_remote_images);
        settings.set_force_dark_mode(self.force_dark_mode);
        settings.set_render_html(self.render_html);
        settings.set_composer_html(self.composer_html);
        settings.set_request_read_receipt(self.request_read_receipt);
        settings.set_spellcheck_languages(&self.spellcheck_languages);
        settings.set_mail_font_family(&self.mail_font_family);
        settings.set_mail_font_size(self.mail_font_size);
        settings.set_mark_read_on_selection(self.mark_read_on_selection);
        settings.set_show_message_preview(self.show_message_preview);
        settings.set_mail_default_folder(&self.mail_default_folder);
        settings.set_mail_layout(&self.mail_layout);
        settings.set_mail_show_toolbar(self.mail_show_toolbar);
        settings.set_mail_show_sidebar(self.mail_show_sidebar);
        settings.set_mail_sidebar_position(self.mail_sidebar_position);
        settings.set_mail_preview_position("modern", self.mail_modern_preview_position);
        settings.set_mail_preview_position("classic", self.mail_classic_preview_position);
        settings.set_mail_row_density(&self.mail_row_density);
        settings.set_mail_show_avatar(self.mail_show_avatar);
        settings.set_mail_show_sender(self.mail_show_sender);
        settings.set_mail_show_recipient(self.mail_show_recipient);
        settings.set_mail_show_date(self.mail_show_date);
        settings.set_mail_show_attachment(self.mail_show_attachment);
        settings.set_mail_show_preview_text(self.mail_show_preview_text);
        settings.set_mail_show_labels(self.mail_show_labels);
        settings.set_color_scheme(&self.color_scheme);
        settings.set_sign_new_messages(self.sign_new_messages);
        settings.set_default_pgp_key(&self.default_pgp_key);
        settings.set_crypto_protocol(&self.crypto_protocol);
        settings.set_smime_signer_certificate(&self.smime_signer_certificate);
        settings.set_smime_private_key(&self.smime_private_key);
        settings.set_smime_recipient_certificates(
            &self
                .smime_recipient_certificates
                .split('\n')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_smime_ca_file(&self.smime_ca_file);
        settings.set_mail_sync_interval_seconds(self.mail_sync_interval);
        settings.set_draft_autosave_interval_seconds(self.draft_autosave_interval);
        settings.set_append_signature(self.append_signature);
        settings.set_signature_text(&self.signature_text);
        settings.set_local_mail_directory(&self.local_mail_directory);
        settings.set_local_calendar_directory(&self.local_calendar_directory);
        settings.set_local_contact_directory(&self.local_contact_directory);
        settings.set_quote_original(self.quote_original);
        settings.set_quote_style(&self.quote_style);
        settings.set_calendar_view(&self.calendar_view);
        settings.set_calendar_week_start(&self.calendar_week_start);
        settings.set_calendar_workdays(&self.calendar_workdays);
        settings.set_calendar_workday_start_minutes(self.calendar_workday_start);
        settings.set_calendar_workday_end_minutes(self.calendar_workday_end);
        settings.set_calendar_default_reminder_minutes(self.calendar_default_reminder);
        settings.set_notifications_enabled(self.notifications_enabled);
        settings.set_confirm_destructive_actions(self.confirm_destructive_actions);
        settings.set_calendar_show_week_numbers(self.calendar_show_week_numbers);
        settings.set_calendar_hidden_sources(
            &self
                .calendar_hidden_sources
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_date_format(&self.date_format);
        settings.set_time_format(&self.time_format);
        settings.set_mail_sort_field(&self.mail_sort_field);
        settings.set_mail_sort_descending(self.mail_sort_descending);
        settings.set_mail_threading(self.mail_threading);
        settings.set_calendar_time_zone(&self.calendar_time_zone);
        settings.set_task_filter(&self.task_filter);
        settings.set_task_source_filter(&self.task_source_filter);
        settings.set_task_sort_field(&self.task_sort_field);
        settings.set_task_sort_descending(self.task_sort_descending);
        settings.set_memo_sort_field(&self.memo_sort_field);
        settings.set_memo_source_filter(&self.memo_source_filter);
        settings.set_memo_sort_descending(self.memo_sort_descending);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BackupBundle {
    version: u32,
    created_at: String,
    #[serde(default)]
    settings: Option<BackupSettings>,
    #[serde(default)]
    files: Vec<BackupFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub files_restored: usize,
    pub settings_restored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInspection {
    pub version: u32,
    pub files: usize,
    pub settings_present: bool,
    pub checksummed_files: usize,
    pub legacy_files_without_checksum: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReport {
    pub files_removed: usize,
    pub settings_reset: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DiagnosticFile {
    relative_path: String,
    present: bool,
    size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DiagnosticBundle {
    version: u32,
    generated_at: String,
    app_files: Vec<DiagnosticFile>,
    settings_available: bool,
}

/// Create a JSON backup of application-owned state.
pub fn create_backup() -> io::Result<Vec<u8>> {
    let root = app_data_root();
    let relative_paths = collect_app_owned_paths(&root)?;
    let files = relative_paths
        .iter()
        .filter_map(|relative_path| {
            let path = root.join(relative_path);
            if let Err(error) = ensure_safe_app_path(&root, relative_path) {
                return Some(Err(error));
            }
            match fs::read(&path) {
                Ok(contents) => Some(Ok(BackupFile {
                    relative_path: relative_path.clone(),
                    checksum_sha256: sha256(&contents),
                    contents,
                })),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<io::Result<Vec<_>>>()?;
    let settings = AppSettings::try_new()
        .as_ref()
        .map(BackupSettings::from_app);
    let bundle = BackupBundle {
        version: CURRENT_VERSION,
        created_at: Utc::now().to_rfc3339(),
        settings,
        files,
    };
    serde_json::to_vec_pretty(&bundle)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Create a metadata-only diagnostic package.
///
/// The package contains file names and sizes, not file contents, settings
/// values, mail, contact data, credentials, or tokens.
pub fn create_diagnostic_package() -> io::Result<Vec<u8>> {
    let bundle = diagnostic_bundle(&app_data_root(), AppSettings::try_new().is_some())?;
    serde_json::to_vec_pretty(&bundle)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Restore a previously created backup into the current app-owned data root.
/// Existing files listed in the backup are replaced atomically; files not
/// present in the backup are intentionally left untouched.
pub fn restore_backup(raw: &[u8]) -> io::Result<RestoreReport> {
    let bundle = serde_json::from_slice::<BackupBundle>(raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    restore_bundle(bundle, &app_data_root(), AppSettings::try_new().as_ref())
}

/// Validate a backup without touching the application data directory.
///
/// This is used by the UI before showing a destructive restore confirmation,
/// so malformed paths and corrupted file contents are reported early.
pub fn inspect_backup(raw: &[u8]) -> io::Result<BackupInspection> {
    let bundle = serde_json::from_slice::<BackupBundle>(raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    validate_bundle(&bundle)?;
    let checksummed_files = bundle
        .files
        .iter()
        .filter(|file| !file.checksum_sha256.is_empty())
        .count();
    Ok(BackupInspection {
        version: bundle.version,
        files: bundle.files.len(),
        settings_present: bundle.settings.is_some(),
        checksummed_files,
        legacy_files_without_checksum: bundle.files.len() - checksummed_files,
    })
}

/// Validate the complete bundle before writing anything and roll back files
/// already replaced if a later atomic write fails.  This keeps a malformed or
/// interrupted restore from leaving a half-restored app-owned data set.
fn restore_bundle(
    bundle: BackupBundle,
    root: &Path,
    app_settings: Option<&AppSettings>,
) -> io::Result<RestoreReport> {
    validate_bundle(&bundle)?;

    if fs::symlink_metadata(root)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "App-Datenordner darf kein symbolischer Link sein",
        ));
    }
    fs::create_dir_all(&root)?;

    let mut relative_paths = HashSet::new();
    let mut entries = Vec::with_capacity(bundle.files.len());
    for file in &bundle.files {
        relative_paths.insert(file.relative_path.as_str());
        let path = safe_app_path(root, &file.relative_path)?;
        let previous = match fs::read(&path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        entries.push(RestoreEntry {
            path,
            contents: file.contents.clone(),
            previous,
        });
    }

    let mut written = Vec::with_capacity(entries.len());
    for entry in &entries {
        if let Err(error) = write_atomic(&entry.path, &entry.contents) {
            restore_entries(&written);
            restore_entry(entry);
            return Err(error);
        }
        written.push(entry);
    }

    let settings_restored = bundle.settings.is_some() && app_settings.is_some();
    if let (Some(settings_snapshot), Some(settings)) = (bundle.settings.as_ref(), app_settings) {
        settings_snapshot.apply_to(&settings);
    }

    Ok(RestoreReport {
        files_restored: entries.len(),
        settings_restored,
    })
}

fn validate_bundle(bundle: &BackupBundle) -> io::Result<()> {
    if bundle.version == 0 || bundle.version > CURRENT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Nicht unterstützte Sicherungsversion: {} (erwartet {})",
                bundle.version, CURRENT_VERSION
            ),
        ));
    }

    let mut relative_paths = HashSet::new();
    for file in &bundle.files {
        validate_relative_path(&file.relative_path)?;
        if !is_app_owned_path(&file.relative_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unbekannte app-eigene Sicherungsdatei: {}",
                    file.relative_path
                ),
            ));
        }
        if !file.checksum_sha256.is_empty() && file.checksum_sha256 != sha256(&file.contents) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Integritätsprüfung der Sicherungsdatei fehlgeschlagen: {}",
                    file.relative_path
                ),
            ));
        }
        if !relative_paths.insert(file.relative_path.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Doppelte Sicherungsdatei: {}", file.relative_path),
            ));
        }
    }
    Ok(())
}

struct RestoreEntry {
    path: PathBuf,
    contents: Vec<u8>,
    previous: Option<Vec<u8>>,
}

fn restore_entries(entries: &[&RestoreEntry]) {
    for entry in entries.iter().rev() {
        restore_entry(entry);
    }
}

fn restore_entry(entry: &RestoreEntry) {
    let result = match entry.previous.as_deref() {
        Some(contents) => write_atomic(&entry.path, contents),
        None => match fs::remove_file(&entry.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
    };
    if let Err(error) = result {
        tracing::error!(path = %entry.path.display(), %error, "Could not roll back backup restore");
    }
}

/// Delete only files and settings owned by this application.
///
/// The original Evolution/EDS profile, Secret Service entries, and
/// user-selected external mail/calendar/contact directories are preserved.
pub fn delete_app_data() -> io::Result<DeleteReport> {
    delete_app_data_at(&app_data_root(), AppSettings::try_new().as_ref())
}

fn delete_app_data_at(root: &Path, settings: Option<&AppSettings>) -> io::Result<DeleteReport> {
    let mut files_removed = 0;
    for relative_path in collect_app_owned_paths(root)? {
        let path = safe_app_path(root, &relative_path)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || metadata.is_file() {
                    fs::remove_file(path)?;
                    files_removed += 1;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("App-Datei ist kein reguläres File: {relative_path}"),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(settings) = settings {
        settings.reset_to_defaults();
    }
    Ok(DeleteReport {
        files_removed,
        settings_reset: settings.is_some(),
    })
}

fn diagnostic_bundle(root: &Path, settings_available: bool) -> io::Result<DiagnosticBundle> {
    let relative_paths = collect_app_owned_paths(root)?;
    let mut app_files = Vec::with_capacity(relative_paths.len());
    for relative_path in relative_paths {
        let path = safe_app_path(root, &relative_path)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("App-Datei darf kein symbolischer Link sein: {relative_path}"),
                ));
            }
            Ok(metadata) => app_files.push(DiagnosticFile {
                relative_path,
                present: true,
                size_bytes: metadata.len(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                app_files.push(DiagnosticFile {
                    relative_path,
                    present: false,
                    size_bytes: 0,
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok(DiagnosticBundle {
        version: CURRENT_VERSION,
        generated_at: Utc::now().to_rfc3339(),
        app_files,
        settings_available,
    })
}

fn app_data_root() -> PathBuf {
    crate::services::app_data_dir()
}

fn sha256(contents: &[u8]) -> String {
    let mut checksum = Checksum::new(ChecksumType::Sha256)
        .expect("GLib must provide the SHA-256 checksum implementation");
    checksum.update(contents);
    checksum
        .string()
        .expect("GLib must return a SHA-256 checksum string")
}

fn is_app_owned_path(relative_path: &str) -> bool {
    APP_FILES.contains(&relative_path)
        || APP_OWNED_DIRECTORIES.iter().any(|directory| {
            relative_path
                .strip_prefix(directory)
                .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
        })
}

fn collect_app_owned_paths(root: &Path) -> io::Result<Vec<String>> {
    let mut paths = APP_FILES
        .iter()
        .map(|relative_path| (*relative_path).to_string())
        .collect::<Vec<_>>();
    for directory in APP_OWNED_DIRECTORIES {
        collect_owned_files(root, Path::new(directory), &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_owned_files(
    root: &Path,
    relative_directory: &Path,
    paths: &mut Vec<String>,
) -> io::Result<()> {
    let directory = root.join(relative_directory);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "App-eigenes Verzeichnis ist kein reguläres Verzeichnis: {}",
                relative_directory.display()
            ),
        ));
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let relative_path = relative_directory.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "App-eigener Pfad darf kein symbolischer Link sein: {}",
                    relative_path.display()
                ),
            ));
        }
        if file_type.is_dir() {
            collect_owned_files(root, &relative_path, paths)?;
        } else if file_type.is_file() {
            paths.push(relative_path.to_string_lossy().replace('\\', "/"));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "App-eigener Pfad ist kein reguläres File: {}",
                    relative_path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> io::Result<()> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültiger Sicherungspfad",
        ));
    }
    Ok(())
}

fn safe_app_path(root: &Path, relative_path: &str) -> io::Result<PathBuf> {
    validate_relative_path(relative_path)?;
    ensure_safe_app_path(root, relative_path)?;
    Ok(root.join(relative_path))
}

fn ensure_safe_app_path(root: &Path, relative_path: &str) -> io::Result<()> {
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if root_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "App-Datenordner darf kein symbolischer Link sein",
        ));
    }

    let mut current = root.to_path_buf();
    for component in Path::new(relative_path).components() {
        if let Component::Normal(name) = component {
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("App-Pfad enthält einen symbolischen Link: {relative_path}"),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary_path = path.with_extension("restore.tmp");
    fs::write(&temporary_path, contents)?;
    fs::rename(temporary_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_and_parent_backup_paths() {
        assert!(validate_relative_path("/tmp/evil").is_err());
        assert!(validate_relative_path("../evil").is_err());
        assert!(validate_relative_path("drafts.json").is_ok());
    }

    #[test]
    fn backup_bundle_roundtrips_without_secrets() {
        let bundle = BackupBundle {
            version: CURRENT_VERSION,
            created_at: "2026-08-10T12:00:00Z".to_string(),
            settings: None,
            files: vec![BackupFile {
                relative_path: "drafts.json".to_string(),
                contents: br#"{"version":1}"#.to_vec(),
                checksum_sha256: sha256(br#"{"version":1}"#),
            }],
        };
        let raw = serde_json::to_vec(&bundle).expect("bundle should serialize");
        let decoded: BackupBundle = serde_json::from_slice(&raw).expect("bundle should parse");
        assert_eq!(decoded, bundle);
        assert!(!String::from_utf8_lossy(&raw).contains("password"));
    }

    #[test]
    fn rejects_backup_file_with_modified_contents() {
        let root = temporary_directory("restore-checksum");
        let bundle = BackupBundle {
            version: CURRENT_VERSION,
            created_at: "2026-08-11T12:00:00Z".to_string(),
            settings: None,
            files: vec![BackupFile {
                relative_path: "accounts.json".to_string(),
                contents: b"modified".to_vec(),
                checksum_sha256: sha256(b"original"),
            }],
        };

        let error = restore_bundle(bundle, &root, None).expect_err("checksum must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("Integritätsprüfung"));
        assert!(!root.join("accounts.json").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backup_manifest_includes_portable_reminder_state() {
        assert!(APP_FILES.contains(&"reminders.json"));
        assert!(APP_FILES.contains(&"mail-snippets.json"));
        assert!(APP_FILES.contains(&"calendar-categories.json"));
    }

    #[test]
    fn imported_calendar_and_contact_files_are_discovered_and_restorable() {
        let root = temporary_directory("dynamic-owned-files");
        fs::create_dir_all(root.join("ical/imported")).expect("calendar directory should exist");
        fs::create_dir_all(root.join("vcards")).expect("contact directory should exist");
        fs::create_dir_all(root.join("mail")).expect("mail directory should exist");
        fs::write(root.join("ical/imported/event.ics"), b"event")
            .expect("calendar fixture should be written");
        fs::write(root.join("vcards/imported.vcf"), b"contact")
            .expect("contact fixture should be written");

        let paths = collect_app_owned_paths(&root).expect("owned files should be discovered");
        assert!(paths.iter().any(|path| path == "ical/imported/event.ics"));
        assert!(paths.iter().any(|path| path == "vcards/imported.vcf"));
        assert!(is_app_owned_path("ical/imported/event.ics"));
        assert!(is_app_owned_path("vcards/imported.vcf"));
        assert!(is_app_owned_path("mail/imported/message.eml"));
        assert!(!is_app_owned_path("external/event.ics"));

        let bundle = BackupBundle {
            version: CURRENT_VERSION,
            created_at: "2026-08-11T12:00:00Z".to_string(),
            settings: None,
            files: vec![
                BackupFile {
                    relative_path: "ical/imported/event.ics".to_string(),
                    contents: b"restored event".to_vec(),
                    checksum_sha256: String::new(),
                },
                BackupFile {
                    relative_path: "vcards/imported.vcf".to_string(),
                    contents: b"restored contact".to_vec(),
                    checksum_sha256: String::new(),
                },
            ],
        };
        let report = restore_bundle(bundle, &root, None).expect("owned files should restore");
        assert_eq!(report.files_restored, 2);
        assert_eq!(
            fs::read(root.join("ical/imported/event.ics")).unwrap(),
            b"restored event"
        );
        assert_eq!(
            fs::read(root.join("vcards/imported.vcf")).unwrap(),
            b"restored contact"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_validates_the_complete_bundle_before_replacing_files() {
        let root = temporary_directory("restore-preflight");
        let original = root.join("accounts.json");
        fs::write(&original, b"original").expect("original account data should be written");
        let bundle = BackupBundle {
            version: CURRENT_VERSION,
            created_at: "2026-08-11T12:00:00Z".to_string(),
            settings: None,
            files: vec![
                BackupFile {
                    relative_path: "accounts.json".to_string(),
                    contents: b"replacement".to_vec(),
                    checksum_sha256: String::new(),
                },
                BackupFile {
                    relative_path: "../outside.json".to_string(),
                    contents: b"must not be written".to_vec(),
                    checksum_sha256: String::new(),
                },
            ],
        };
        let raw = serde_json::to_vec(&bundle).expect("bundle should serialize");

        let decoded = serde_json::from_slice::<BackupBundle>(&raw).expect("bundle should parse");
        assert!(restore_bundle(decoded, &root, None).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"original");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_settings_get_defaults_for_new_calendar_fields() {
        let legacy = serde_json::json!({
            "window_width": 1100,
            "window_height": 700,
            "is_maximized": false,
            "active_view": "mail",
            "auto_load_remote_images": false,
            "force_dark_mode": false,
            "render_html": true,
            "color_scheme": "system",
            "sign_new_messages": false,
            "default_pgp_key": "",
            "mail_sync_interval": 300,
            "draft_autosave_interval": 60,
            "append_signature": false,
            "signature_text": "",
            "local_mail_directory": "",
            "local_calendar_directory": "",
            "local_contact_directory": "",
            "quote_original": true,
            "quote_style": "blockquote"
        });
        let settings: BackupSettings =
            serde_json::from_value(legacy).expect("legacy settings should remain readable");

        assert_eq!(settings.calendar_view, "month");
        assert!(settings.default_address_book_source.is_empty());
        assert!(settings.default_task_source.is_empty());
        assert!(settings.default_memo_source.is_empty());
        assert_eq!(settings.calendar_week_start, "monday");
        assert_eq!(
            settings.calendar_workdays,
            "monday,tuesday,wednesday,thursday,friday"
        );
        assert_eq!(settings.calendar_workday_start, 540);
        assert_eq!(settings.calendar_workday_end, 1_020);
        assert_eq!(settings.calendar_default_reminder, 15);
        assert!(settings.notifications_enabled);
        assert!(settings.mark_read_on_selection);
        assert!(settings.show_message_preview);
        assert_eq!(settings.mail_default_folder, "inbox");
        assert!(settings.spellcheck_languages.is_empty());
        assert_eq!(settings.mail_layout, "classic");
        assert!(settings.mail_show_toolbar);
        assert!(settings.mail_show_sidebar);
        assert_eq!(settings.calendar_time_zone, "system");
        assert_eq!(settings.mail_sidebar_position, 220);
        assert_eq!(settings.mail_modern_preview_position, 420);
        assert_eq!(settings.mail_classic_preview_position, 360);
        assert_eq!(settings.mail_row_density, "comfortable");
        assert!(settings.mail_show_avatar);
        assert!(settings.mail_show_sender);
        assert!(!settings.mail_show_recipient);
        assert!(settings.mail_show_date);
        assert!(settings.mail_show_attachment);
        assert!(settings.mail_show_preview_text);
        assert!(settings.mail_show_labels);
        assert!(!settings.calendar_show_week_numbers);
        assert!(settings.calendar_hidden_sources.is_empty());
        assert_eq!(settings.mail_sort_field, "date");
        assert!(settings.mail_sort_descending);
        assert!(settings.mail_threading);
        assert!(settings.task_source_filter.is_empty());
        assert!(settings.task_sort_descending);
        assert!(settings.memo_source_filter.is_empty());
        assert!(settings.memo_sort_descending);
    }

    #[test]
    fn missing_settings_use_the_installed_schema_defaults() {
        let settings: BackupSettings =
            serde_json::from_value(serde_json::json!({})).expect("empty settings should parse");

        assert_eq!(settings.window_width, 1_100);
        assert_eq!(settings.window_height, 700);
        assert!(!settings.is_maximized);
        assert_eq!(settings.active_view, "mail");
        assert!(!settings.auto_load_remote_images);
        assert!(settings.force_dark_mode);
        assert!(settings.render_html);
        assert!(settings.composer_html);
        assert!(!settings.request_read_receipt);
        assert!(settings.spellcheck_languages.is_empty());
        assert_eq!(settings.color_scheme, "system");
        assert!(!settings.sign_new_messages);
        assert!(settings.default_pgp_key.is_empty());
        assert_eq!(settings.mail_sync_interval, 30);
        assert_eq!(settings.draft_autosave_interval, 30);
        assert!(!settings.append_signature);
        assert!(settings.signature_text.is_empty());
        assert!(settings.local_mail_directory.is_empty());
        assert!(settings.local_calendar_directory.is_empty());
        assert!(settings.local_contact_directory.is_empty());
        assert!(settings.quote_original);
        assert_eq!(settings.quote_style, "blockquote");
    }

    #[test]
    fn diagnostic_package_contains_metadata_but_no_file_contents() {
        let root = temporary_directory("diagnostic");
        fs::write(root.join("drafts.json"), br#"{"password":"secret"}"#)
            .expect("diagnostic fixture should be written");

        let bundle = diagnostic_bundle(&root, false).expect("diagnostic bundle should build");
        let raw = serde_json::to_vec(&bundle).expect("diagnostic bundle should serialize");
        assert!(String::from_utf8_lossy(&raw).contains("drafts.json"));
        assert!(String::from_utf8_lossy(&raw).contains("size_bytes"));
        assert!(!String::from_utf8_lossy(&raw).contains("secret"));
        assert!(!bundle.settings_available);
        assert_eq!(
            bundle
                .app_files
                .iter()
                .find(|file| file.relative_path == "drafts.json")
                .map(|file| file.size_bytes),
            Some(br#"{"password":"secret"}"#.len() as u64)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_data_deletion_preserves_external_files() {
        let root = temporary_directory("delete");
        fs::write(root.join("accounts.json"), b"owned").expect("owned fixture should be written");
        fs::create_dir_all(root.join("ical")).expect("owned directory should be created");
        fs::write(
            root.join("ical/evolution-gtk4-local.ics"),
            b"BEGIN:VCALENDAR",
        )
        .expect("calendar fixture should be written");
        fs::write(root.join("external.txt"), b"keep").expect("external fixture should be written");

        let report = delete_app_data_at(&root, None).expect("app data should be deleted");
        assert_eq!(report.files_removed, 2);
        assert!(!root.join("accounts.json").exists());
        assert!(!root.join("ical/evolution-gtk4-local.ics").exists());
        assert_eq!(
            fs::read(root.join("external.txt")).expect("external data should remain"),
            b"keep"
        );

        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
