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
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::settings_service::{AppSettings, ReminderDurationUnit};

const CURRENT_VERSION: u32 = 4;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

fn default_mail_folder_sort_order() -> String {
    "{}".to_string()
}

fn default_calendar_time_zone() -> String {
    "system".to_string()
}

fn default_task_filter() -> String {
    "all".to_string()
}

fn default_task_category_filter() -> String {
    String::new()
}

fn default_task_search_mode() -> String {
    "any-field".to_string()
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
    /// Optional for backwards compatibility with version-3 and older backups
    /// before integrity metadata was added.
    #[serde(default)]
    checksum_sha256: String,
    /// Number of JSON fields deliberately removed by the safe backup
    /// redactor. Older formats do not have this metadata.
    #[serde(default)]
    redacted_fields: u32,
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
    #[serde(default)]
    offline_mode: bool,
    #[serde(default = "default_force_dark_mode")]
    force_dark_mode: bool,
    #[serde(default = "default_render_html")]
    render_html: bool,
    #[serde(default = "default_composer_html")]
    composer_html: bool,
    #[serde(default = "default_composer_charset")]
    composer_charset: String,
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
    #[serde(default)]
    mail_show_security_details: bool,
    #[serde(default)]
    mail_show_all_headers: bool,
    #[serde(default = "default_mail_normal_headers")]
    mail_normal_headers: Vec<(String, bool)>,
    #[serde(default = "default_mail_show_statusbar")]
    mail_show_statusbar: bool,
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
    #[serde(default)]
    mail_show_size: bool,
    #[serde(default)]
    mail_show_follow_up_due: bool,
    #[serde(default)]
    mail_show_location: bool,
    #[serde(default = "default_mail_sender_column_width")]
    mail_sender_column_width: i32,
    #[serde(default = "default_mail_recipient_column_width")]
    mail_recipient_column_width: i32,
    #[serde(default = "default_mail_date_column_width")]
    mail_date_column_width: i32,
    #[serde(default = "default_mail_list_view_profile")]
    mail_list_view_profile: String,
    #[serde(default = "default_mail_list_column_order")]
    mail_list_column_order: String,
    #[serde(default = "default_color_scheme")]
    color_scheme: String,
    #[serde(default = "default_switcher_location")]
    switcher_location: String,
    #[serde(default)]
    sign_new_messages: bool,
    #[serde(default)]
    default_pgp_key: String,
    #[serde(default = "default_crypto_protocol")]
    crypto_protocol: String,
    #[serde(default)]
    smime_signer_certificate: String,
    /// Private key material is native crypto state and is never included in
    /// portable backups. `skip` also keeps legacy JSON fields from being
    /// re-emitted during a round-trip.
    #[serde(skip)]
    smime_private_key: String,
    #[serde(default)]
    smime_recipient_certificates: String,
    #[serde(default)]
    smime_ca_file: String,
    #[serde(default = "default_mail_sync_interval")]
    mail_sync_interval: u32,
    #[serde(default = "default_mail_sync_message_limit")]
    mail_sync_message_limit: u32,
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
    #[serde(default)]
    use_default_reminder: bool,
    #[serde(default = "default_default_reminder_interval")]
    default_reminder_interval: u32,
    #[serde(default = "default_default_reminder_units")]
    default_reminder_units: String,
    #[serde(default)]
    use_ba_reminder: bool,
    #[serde(default = "default_default_reminder_interval")]
    ba_reminder_interval: u32,
    #[serde(default = "default_default_reminder_units")]
    ba_reminder_units: String,
    #[serde(default)]
    hide_completed_tasks: bool,
    #[serde(default = "default_hide_completed_tasks_value")]
    hide_completed_tasks_value: u32,
    #[serde(default = "default_hide_completed_tasks_units")]
    hide_completed_tasks_units: String,
    #[serde(default)]
    hide_cancelled_tasks: bool,
    #[serde(default)]
    hide_cancelled_events: bool,
    #[serde(default = "default_notifications_enabled")]
    notifications_enabled: bool,
    #[serde(default = "default_confirm_destructive_actions")]
    confirm_destructive_actions: bool,
    #[serde(default)]
    calendar_show_week_numbers: bool,
    #[serde(default = "default_calendar_time_divisions")]
    calendar_time_divisions: i32,
    #[serde(default = "default_calendar_compress_weekend")]
    calendar_compress_weekend: bool,
    #[serde(default = "default_calendar_month_scroll_by_week")]
    calendar_month_scroll_by_week: bool,
    #[serde(default = "default_calendar_second_zone")]
    calendar_second_zone: String,
    #[serde(default = "default_calendar_second_zones")]
    calendar_second_zones: String,
    #[serde(default = "default_calendar_hidden_sources")]
    calendar_hidden_sources: String,
    #[serde(default = "default_calendar_hidden_sources")]
    task_hidden_sources: String,
    #[serde(default = "default_calendar_hidden_sources")]
    memo_hidden_sources: String,
    #[serde(default = "default_calendar_hidden_sources")]
    addressbook_hidden_sources: String,
    #[serde(default = "default_calendar_hidden_sources")]
    calendar_hidden_groups: String,
    #[serde(default = "default_calendar_hidden_sources")]
    calendar_groups_order: String,
    #[serde(default = "default_calendar_hidden_sources")]
    task_hidden_groups: String,
    #[serde(default = "default_calendar_hidden_sources")]
    task_groups_order: String,
    #[serde(default = "default_calendar_hidden_sources")]
    memo_hidden_groups: String,
    #[serde(default = "default_calendar_hidden_sources")]
    memo_groups_order: String,
    #[serde(default = "default_calendar_hidden_sources")]
    addressbook_hidden_groups: String,
    #[serde(default = "default_calendar_hidden_sources")]
    addressbook_groups_order: String,
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
    #[serde(default = "default_mail_folder_sort_order")]
    mail_folder_sort_order: String,
    #[serde(default = "default_calendar_time_zone")]
    calendar_time_zone: String,
    #[serde(default = "default_task_filter")]
    task_filter: String,
    #[serde(default = "default_task_category_filter")]
    task_category_filter: String,
    #[serde(default = "default_task_search_mode")]
    task_search_mode: String,
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
    #[serde(default = "default_contact_completion_minimum_query_length")]
    contact_completion_minimum_query_length: u32,
    #[serde(default)]
    contact_completion_show_address: bool,
    #[serde(default)]
    contact_completion_book_sources: String,
    #[serde(default)]
    contact_address_formatting: bool,
    #[serde(default)]
    contact_preview_show_maps: bool,
    #[serde(default)]
    contact_preview_home_before_work: bool,
    #[serde(default = "default_contact_open_map_target")]
    contact_open_map_target: String,
    #[serde(default = "default_contact_open_map_prefer_local")]
    contact_open_map_prefer_local: bool,
    #[serde(default = "default_contact_layout")]
    contact_layout: String,
    #[serde(default = "default_contact_preview_visible")]
    contact_preview_visible: bool,
    #[serde(default = "default_contact_hpane_position")]
    contact_hpane_position: i32,
    #[serde(default = "default_contact_vpane_position")]
    contact_vpane_position: i32,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_phone: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_sip: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_im: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_home_address: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_work_address: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_other_address: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_web: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_job: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_dates: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_misc: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_notes: bool,
    #[serde(default = "default_contact_editor_show_section")]
    contact_editor_show_certs: bool,
    #[serde(default)]
    filters_log_actions: bool,
    #[serde(default)]
    filters_log_file: String,
    #[serde(default = "default_flush_outbox")]
    flush_outbox: bool,
    #[serde(default = "default_mail_labels")]
    mail_labels: Vec<String>,
    #[serde(default = "default_junk_check_incoming")]
    junk_check_incoming: bool,
    #[serde(default = "default_junk_empty_on_exit")]
    junk_empty_on_exit: bool,
    #[serde(default = "default_junk_empty_on_exit_days")]
    junk_empty_on_exit_days: i32,
    #[serde(default = "default_junk_lookup_addressbook")]
    junk_lookup_addressbook: bool,
    #[serde(default = "default_junk_lookup_addressbook_local_only")]
    junk_lookup_addressbook_local_only: bool,
    #[serde(default = "default_junk_default_plugin")]
    junk_default_plugin: String,
    #[serde(default = "default_junk_custom_header")]
    junk_custom_header: String,
    #[serde(default = "default_junk_custom_header_value")]
    junk_custom_header_value: String,
    #[serde(default = "default_composer_show_from")]
    composer_show_from: bool,
    #[serde(default = "default_composer_show_cc")]
    composer_show_cc: bool,
    #[serde(default = "default_composer_show_bcc")]
    composer_show_bcc: bool,
    #[serde(default = "default_composer_show_reply_to")]
    composer_show_reply_to: bool,
    #[serde(default = "default_composer_prompt_empty_subject")]
    composer_prompt_empty_subject: bool,
    #[serde(default = "default_composer_prompt_only_bcc")]
    composer_prompt_only_bcc: bool,
    #[serde(default = "default_composer_inline_spelling")]
    composer_inline_spelling: bool,
    #[serde(default = "default_composer_reply_style")]
    composer_reply_style: String,
    #[serde(default = "default_composer_forward_style")]
    composer_forward_style: String,
    #[serde(default = "default_composer_top_signature")]
    composer_top_signature: bool,
    #[serde(default = "default_composer_signature_outline")]
    composer_signature_outline: bool,
    #[serde(default = "default_mail_image_loading_policy")]
    mail_image_loading_policy: String,
    #[serde(default = "default_mail_mark_seen")]
    mail_mark_seen: bool,
    #[serde(default = "default_mail_mark_seen_timeout")]
    mail_mark_seen_timeout: u32,
    #[serde(default = "default_mail_caret_mode")]
    mail_caret_mode: bool,
    #[serde(default = "default_mail_magic_spacebar")]
    mail_magic_spacebar: bool,
    #[serde(default = "default_mail_citation_highlighting")]
    mail_citation_highlighting: bool,
    #[serde(default = "default_mail_citation_color_1")]
    mail_citation_color_1: String,
    #[serde(default = "default_mail_citation_color_2")]
    mail_citation_color_2: String,
    #[serde(default = "default_mail_citation_color_3")]
    mail_citation_color_3: String,
    #[serde(default = "default_mail_citation_color_4")]
    mail_citation_color_4: String,
    #[serde(default = "default_mail_citation_color_5")]
    mail_citation_color_5: String,
    #[serde(default = "default_mail_headers_collapsed")]
    mail_headers_collapsed: bool,
    #[serde(default = "default_mail_show_real_date")]
    mail_show_real_date: bool,
}

fn default_composer_show_from() -> bool {
    true
}

fn default_composer_show_cc() -> bool {
    true
}

fn default_composer_show_bcc() -> bool {
    false
}

fn default_composer_show_reply_to() -> bool {
    false
}

fn default_composer_prompt_empty_subject() -> bool {
    true
}

fn default_composer_prompt_only_bcc() -> bool {
    true
}

fn default_composer_inline_spelling() -> bool {
    true
}

fn default_composer_reply_style() -> String {
    "quoted".to_string()
}

fn default_composer_forward_style() -> String {
    "attached".to_string()
}

fn default_composer_top_signature() -> bool {
    false
}

fn default_composer_signature_outline() -> bool {
    true
}

fn default_mail_image_loading_policy() -> String {
    "never".to_string()
}

fn default_mail_mark_seen() -> bool {
    true
}

fn default_mail_mark_seen_timeout() -> u32 {
    1500
}

fn default_mail_caret_mode() -> bool {
    false
}

fn default_mail_magic_spacebar() -> bool {
    true
}

fn default_mail_citation_highlighting() -> bool {
    true
}

fn default_mail_citation_color_1() -> String {
    "#729FCF".to_string()
}

fn default_mail_citation_color_2() -> String {
    "#AD7FA8".to_string()
}

fn default_mail_citation_color_3() -> String {
    "#8AE234".to_string()
}

fn default_mail_citation_color_4() -> String {
    "#FCAF3E".to_string()
}

fn default_mail_citation_color_5() -> String {
    "#E9B96E".to_string()
}

fn default_mail_headers_collapsed() -> bool {
    false
}

fn default_mail_show_real_date() -> bool {
    true
}

fn default_mail_labels() -> Vec<String> {
    crate::services::settings_service::MailLabelDefinition::default_labels()
        .into_iter()
        .map(|l| l.to_setting_string())
        .collect()
}

fn default_junk_check_incoming() -> bool {
    true
}

fn default_junk_empty_on_exit() -> bool {
    false
}

fn default_junk_empty_on_exit_days() -> i32 {
    0
}

fn default_junk_lookup_addressbook() -> bool {
    false
}

fn default_junk_lookup_addressbook_local_only() -> bool {
    false
}

fn default_junk_default_plugin() -> String {
    "Bogofilter".to_string()
}

fn default_junk_custom_header() -> String {
    String::new()
}

fn default_junk_custom_header_value() -> String {
    String::new()
}

fn default_flush_outbox() -> bool {
    true
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
    false
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

fn default_mail_sync_message_limit() -> u32 {
    100
}

fn default_composer_charset() -> String {
    "UTF-8".to_string()
}

fn default_switcher_location() -> String {
    "bottom-left".to_string()
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

fn default_calendar_time_divisions() -> i32 {
    30
}

fn default_calendar_compress_weekend() -> bool {
    true
}

fn default_calendar_month_scroll_by_week() -> bool {
    true
}

fn default_calendar_second_zone() -> String {
    String::new()
}

fn default_calendar_second_zones() -> String {
    String::new()
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

fn default_mail_normal_headers() -> Vec<(String, bool)> {
    [
        ("From", true),
        ("Reply-To", true),
        ("To", true),
        ("Cc", true),
        ("Bcc", true),
        ("Subject", true),
        ("Date", true),
        ("Newsgroups", true),
        ("Face", true),
        ("x-evolution-mailer", false),
    ]
    .into_iter()
    .map(|(name, visible)| (name.to_string(), visible))
    .collect()
}

fn default_mail_show_statusbar() -> bool {
    true
}

fn default_mail_default_folder() -> String {
    "inbox".to_string()
}

fn default_mail_row_density() -> String {
    "compact".to_string()
}

fn default_mail_show_avatar() -> bool {
    false
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
    false
}

fn default_mail_show_labels() -> bool {
    true
}

fn default_mail_sender_column_width() -> i32 {
    180
}

fn default_mail_recipient_column_width() -> i32 {
    180
}

fn default_mail_date_column_width() -> i32 {
    130
}

fn default_mail_list_view_profile() -> String {
    "messages".to_string()
}

fn default_mail_list_column_order() -> String {
    crate::services::settings_service::MAIL_LIST_COLUMN_IDS.join(",")
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
    240
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
    16
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

fn default_default_reminder_interval() -> u32 {
    15
}

fn default_default_reminder_units() -> String {
    "minutes".to_string()
}

fn default_hide_completed_tasks_value() -> u32 {
    1
}

fn default_hide_completed_tasks_units() -> String {
    "days".to_string()
}

fn default_contact_completion_minimum_query_length() -> u32 {
    3
}

fn default_contact_open_map_target() -> String {
    "openstreetmap".to_string()
}

fn default_contact_open_map_prefer_local() -> bool {
    true
}

fn default_contact_layout() -> String {
    "classic".to_string()
}

fn default_contact_preview_visible() -> bool {
    true
}

fn default_contact_hpane_position() -> i32 {
    200
}

fn default_contact_vpane_position() -> i32 {
    400
}

fn default_contact_editor_show_section() -> bool {
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
            offline_mode: settings.offline_mode(),
            force_dark_mode: settings.force_dark_mode(),
            render_html: settings.render_html(),
            composer_html: settings.composer_html(),
            composer_charset: settings.composer_charset(),
            request_read_receipt: settings.request_read_receipt(),
            spellcheck_languages: settings.spellcheck_languages(),
            mail_font_family: settings.mail_font_family(),
            mail_font_size: settings.mail_font_size(),
            mark_read_on_selection: settings.mark_read_on_selection(),
            show_message_preview: settings.show_message_preview(),
            mail_show_security_details: settings.mail_show_security_details(),
            mail_show_all_headers: settings.mail_show_all_headers(),
            mail_normal_headers: settings.mail_normal_headers(),
            mail_show_statusbar: settings.mail_show_statusbar(),
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
            mail_show_size: settings.mail_show_size(),
            mail_show_follow_up_due: settings.mail_show_follow_up_due(),
            mail_show_location: settings.mail_show_location(),
            mail_sender_column_width: settings.mail_sender_column_width(),
            mail_recipient_column_width: settings.mail_recipient_column_width(),
            mail_date_column_width: settings.mail_date_column_width(),
            mail_list_view_profile: settings.mail_list_view_profile(),
            mail_list_column_order: settings.mail_list_column_order().join(","),
            color_scheme: settings.color_scheme(),
            switcher_location: settings.switcher_location(),
            sign_new_messages: settings.sign_new_messages(),
            default_pgp_key: settings.default_pgp_key(),
            crypto_protocol: settings.crypto_protocol(),
            smime_signer_certificate: settings.smime_signer_certificate(),
            smime_private_key: String::new(),
            smime_recipient_certificates: settings.smime_recipient_certificates().join("\n"),
            smime_ca_file: settings.smime_ca_file(),
            mail_sync_interval: settings.mail_sync_interval_seconds(),
            mail_sync_message_limit: settings.mail_sync_message_limit(),
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
            use_default_reminder: settings.use_default_reminder(),
            default_reminder_interval: settings.default_reminder_interval(),
            default_reminder_units: settings.default_reminder_units().as_str().to_string(),
            use_ba_reminder: settings.use_ba_reminder(),
            ba_reminder_interval: settings.ba_reminder_interval(),
            ba_reminder_units: settings.ba_reminder_units().as_str().to_string(),
            hide_completed_tasks: settings.hide_completed_tasks(),
            hide_completed_tasks_value: settings.hide_completed_tasks_value(),
            hide_completed_tasks_units: settings.hide_completed_tasks_units().as_str().to_string(),
            hide_cancelled_tasks: settings.hide_cancelled_tasks(),
            hide_cancelled_events: settings.hide_cancelled_events(),
            notifications_enabled: settings.notifications_enabled(),
            confirm_destructive_actions: settings.confirm_destructive_actions(),
            calendar_show_week_numbers: settings.calendar_show_week_numbers(),
            calendar_time_divisions: settings.calendar_time_divisions(),
            calendar_compress_weekend: settings.calendar_compress_weekend(),
            calendar_month_scroll_by_week: settings.calendar_month_scroll_by_week(),
            calendar_second_zone: settings.calendar_second_zone(),
            calendar_second_zones: settings.calendar_second_zones().join(","),
            calendar_hidden_sources: settings.calendar_hidden_sources().join(","),
            task_hidden_sources: settings.task_hidden_sources().join(","),
            memo_hidden_sources: settings.memo_hidden_sources().join(","),
            addressbook_hidden_sources: settings.addressbook_hidden_sources().join(","),
            calendar_hidden_groups: settings.calendar_hidden_groups().join(","),
            calendar_groups_order: settings.calendar_groups_order().join(","),
            task_hidden_groups: settings.task_hidden_groups().join(","),
            task_groups_order: settings.task_groups_order().join(","),
            memo_hidden_groups: settings.memo_hidden_groups().join(","),
            memo_groups_order: settings.memo_groups_order().join(","),
            addressbook_hidden_groups: settings.addressbook_hidden_groups().join(","),
            addressbook_groups_order: settings.addressbook_groups_order().join(","),
            date_format: settings.date_format(),
            time_format: settings.time_format(),
            mail_sort_field: settings.mail_sort_field(),
            mail_sort_descending: settings.mail_sort_descending(),
            mail_threading: settings.mail_threading(),
            mail_folder_sort_order: settings.mail_folder_sort_order(),
            calendar_time_zone: settings.calendar_time_zone(),
            task_filter: settings.task_filter(),
            task_category_filter: settings.task_category_filter(),
            task_search_mode: settings.task_search_mode(),
            task_source_filter: settings.task_source_filter(),
            task_sort_field: settings.task_sort_field(),
            task_sort_descending: settings.task_sort_descending(),
            memo_sort_field: settings.memo_sort_field(),
            memo_source_filter: settings.memo_source_filter(),
            memo_sort_descending: settings.memo_sort_descending(),
            contact_completion_minimum_query_length: settings
                .contact_completion_minimum_query_length(),
            contact_completion_show_address: settings.contact_completion_show_address(),
            contact_completion_book_sources: settings.contact_completion_book_sources(),
            contact_address_formatting: settings.contact_address_formatting(),
            contact_preview_show_maps: settings.contact_preview_show_maps(),
            contact_preview_home_before_work: settings.contact_preview_home_before_work(),
            contact_open_map_target: settings.contact_open_map_target(),
            contact_open_map_prefer_local: settings.contact_open_map_prefer_local(),
            contact_layout: settings.contact_layout(),
            contact_preview_visible: settings.contact_preview_visible(),
            contact_hpane_position: settings.contact_hpane_position(),
            contact_vpane_position: settings.contact_vpane_position(),
            contact_editor_show_phone: settings.contact_editor_show_phone(),
            contact_editor_show_sip: settings.contact_editor_show_sip(),
            contact_editor_show_im: settings.contact_editor_show_im(),
            contact_editor_show_home_address: settings.contact_editor_show_home_address(),
            contact_editor_show_work_address: settings.contact_editor_show_work_address(),
            contact_editor_show_other_address: settings.contact_editor_show_other_address(),
            contact_editor_show_web: settings.contact_editor_show_web(),
            contact_editor_show_job: settings.contact_editor_show_job(),
            contact_editor_show_dates: settings.contact_editor_show_dates(),
            contact_editor_show_misc: settings.contact_editor_show_misc(),
            contact_editor_show_notes: settings.contact_editor_show_notes(),
            contact_editor_show_certs: settings.contact_editor_show_certs(),
            filters_log_actions: settings.filters_log_actions(),
            filters_log_file: settings.filters_log_file(),
            flush_outbox: settings.flush_outbox(),
            mail_labels: settings
                .mail_labels()
                .into_iter()
                .map(|l| l.to_setting_string())
                .collect(),
            junk_check_incoming: settings.junk_check_incoming(),
            junk_empty_on_exit: settings.junk_empty_on_exit(),
            junk_empty_on_exit_days: settings.junk_empty_on_exit_days(),
            junk_lookup_addressbook: settings.junk_lookup_addressbook(),
            junk_lookup_addressbook_local_only: settings.junk_lookup_addressbook_local_only(),
            junk_default_plugin: settings.junk_default_plugin(),
            junk_custom_header: settings.junk_custom_header(),
            junk_custom_header_value: settings.junk_custom_header_value(),
            composer_show_from: settings.composer_show_from(),
            composer_show_cc: settings.composer_show_cc(),
            composer_show_bcc: settings.composer_show_bcc(),
            composer_show_reply_to: settings.composer_show_reply_to(),
            composer_prompt_empty_subject: settings.composer_prompt_empty_subject(),
            composer_prompt_only_bcc: settings.composer_prompt_only_bcc(),
            composer_inline_spelling: settings.composer_inline_spelling(),
            composer_reply_style: settings.composer_reply_style(),
            composer_forward_style: settings.composer_forward_style(),
            composer_top_signature: settings.composer_top_signature(),
            composer_signature_outline: settings.composer_signature_outline(),
            mail_image_loading_policy: settings.mail_image_loading_policy(),
            mail_mark_seen: settings.mail_mark_seen(),
            mail_mark_seen_timeout: settings.mail_mark_seen_timeout(),
            mail_caret_mode: settings.mail_caret_mode(),
            mail_magic_spacebar: settings.mail_magic_spacebar(),
            mail_citation_highlighting: settings.mail_citation_highlighting(),
            mail_citation_color_1: settings.mail_citation_color_1(),
            mail_citation_color_2: settings.mail_citation_color_2(),
            mail_citation_color_3: settings.mail_citation_color_3(),
            mail_citation_color_4: settings.mail_citation_color_4(),
            mail_citation_color_5: settings.mail_citation_color_5(),
            mail_headers_collapsed: settings.mail_headers_collapsed(),
            mail_show_real_date: settings.mail_show_real_date(),
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
        settings.set_offline_mode(self.offline_mode);
        settings.set_force_dark_mode(self.force_dark_mode);
        settings.set_render_html(self.render_html);
        settings.set_composer_html(self.composer_html);
        settings.set_composer_charset(&self.composer_charset);
        settings.set_request_read_receipt(self.request_read_receipt);
        settings.set_spellcheck_languages(&self.spellcheck_languages);
        settings.set_mail_font_family(&self.mail_font_family);
        settings.set_mail_font_size(self.mail_font_size);
        settings.set_mark_read_on_selection(self.mark_read_on_selection);
        settings.set_show_message_preview(self.show_message_preview);
        settings.set_mail_show_security_details(self.mail_show_security_details);
        settings.set_mail_show_all_headers(self.mail_show_all_headers);
        settings.set_mail_normal_headers(&self.mail_normal_headers);
        settings.set_mail_show_statusbar(self.mail_show_statusbar);
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
        settings.set_mail_show_size(self.mail_show_size);
        settings.set_mail_show_follow_up_due(self.mail_show_follow_up_due);
        settings.set_mail_show_location(self.mail_show_location);
        settings.set_mail_sender_column_width(self.mail_sender_column_width);
        settings.set_mail_recipient_column_width(self.mail_recipient_column_width);
        settings.set_mail_date_column_width(self.mail_date_column_width);
        if self.mail_list_view_profile == "custom" {
            let order = crate::services::settings_service::normalize_mail_list_column_order(
                &self.mail_list_column_order,
            );
            settings.set_mail_list_column_order(&order);
        } else {
            settings.apply_mail_list_view_profile(&self.mail_list_view_profile);
        }
        settings.set_color_scheme(&self.color_scheme);
        settings.set_switcher_location(&self.switcher_location);
        settings.set_sign_new_messages(self.sign_new_messages);
        settings.set_default_pgp_key(&self.default_pgp_key);
        settings.set_crypto_protocol(&self.crypto_protocol);
        settings.set_smime_signer_certificate(&self.smime_signer_certificate);
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
        settings.set_mail_sync_message_limit(self.mail_sync_message_limit);
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
        settings.set_use_default_reminder(self.use_default_reminder);
        settings.set_default_reminder_interval(self.default_reminder_interval);
        settings.set_default_reminder_units(ReminderDurationUnit::from_str(
            &self.default_reminder_units,
        ));
        settings.set_use_ba_reminder(self.use_ba_reminder);
        settings.set_ba_reminder_interval(self.ba_reminder_interval);
        settings.set_ba_reminder_units(ReminderDurationUnit::from_str(&self.ba_reminder_units));
        settings.set_hide_completed_tasks(self.hide_completed_tasks);
        settings.set_hide_completed_tasks_value(self.hide_completed_tasks_value);
        settings.set_hide_completed_tasks_units(ReminderDurationUnit::from_str(
            &self.hide_completed_tasks_units,
        ));
        settings.set_hide_cancelled_tasks(self.hide_cancelled_tasks);
        settings.set_hide_cancelled_events(self.hide_cancelled_events);
        settings.set_notifications_enabled(self.notifications_enabled);
        settings.set_confirm_destructive_actions(self.confirm_destructive_actions);
        settings.set_calendar_show_week_numbers(self.calendar_show_week_numbers);
        settings.set_calendar_time_divisions(self.calendar_time_divisions);
        settings.set_calendar_compress_weekend(self.calendar_compress_weekend);
        settings.set_calendar_month_scroll_by_week(self.calendar_month_scroll_by_week);
        settings.set_calendar_second_zone(&self.calendar_second_zone);
        settings.set_calendar_second_zones(
            &self
                .calendar_second_zones
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_calendar_hidden_sources(
            &self
                .calendar_hidden_sources
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_task_hidden_sources(
            &self
                .task_hidden_sources
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_memo_hidden_sources(
            &self
                .memo_hidden_sources
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_addressbook_hidden_sources(
            &self
                .addressbook_hidden_sources
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_calendar_hidden_groups(
            &self
                .calendar_hidden_groups
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_calendar_groups_order(
            &self
                .calendar_groups_order
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_task_hidden_groups(
            &self
                .task_hidden_groups
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_task_groups_order(
            &self
                .task_groups_order
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_memo_hidden_groups(
            &self
                .memo_hidden_groups
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_memo_groups_order(
            &self
                .memo_groups_order
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_addressbook_hidden_groups(
            &self
                .addressbook_hidden_groups
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_addressbook_groups_order(
            &self
                .addressbook_groups_order
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        settings.set_date_format(&self.date_format);
        settings.set_time_format(&self.time_format);
        settings.set_mail_sort_field(&self.mail_sort_field);
        settings.set_mail_sort_descending(self.mail_sort_descending);
        settings.set_mail_threading(self.mail_threading);
        settings.set_mail_folder_sort_order(&self.mail_folder_sort_order);
        settings.set_calendar_time_zone(&self.calendar_time_zone);
        settings.set_task_filter(&self.task_filter);
        settings.set_task_category_filter(&self.task_category_filter);
        settings.set_task_search_mode(&self.task_search_mode);
        settings.set_task_source_filter(&self.task_source_filter);
        settings.set_task_sort_field(&self.task_sort_field);
        settings.set_task_sort_descending(self.task_sort_descending);
        settings.set_memo_sort_field(&self.memo_sort_field);
        settings.set_memo_source_filter(&self.memo_source_filter);
        settings.set_memo_sort_descending(self.memo_sort_descending);
        settings.set_contact_completion_minimum_query_length(
            self.contact_completion_minimum_query_length,
        );
        settings.set_contact_completion_show_address(self.contact_completion_show_address);
        settings.set_contact_completion_book_sources(&self.contact_completion_book_sources);
        settings.set_contact_address_formatting(self.contact_address_formatting);
        settings.set_contact_preview_show_maps(self.contact_preview_show_maps);
        settings.set_contact_preview_home_before_work(self.contact_preview_home_before_work);
        settings.set_contact_open_map_target(&self.contact_open_map_target);
        settings.set_contact_open_map_prefer_local(self.contact_open_map_prefer_local);
        settings.set_contact_layout(&self.contact_layout);
        settings.set_contact_preview_visible(self.contact_preview_visible);
        settings.set_contact_hpane_position(self.contact_hpane_position);
        settings.set_contact_vpane_position(self.contact_vpane_position);
        settings.set_contact_editor_show_phone(self.contact_editor_show_phone);
        settings.set_contact_editor_show_sip(self.contact_editor_show_sip);
        settings.set_contact_editor_show_im(self.contact_editor_show_im);
        settings.set_contact_editor_show_home_address(self.contact_editor_show_home_address);
        settings.set_contact_editor_show_work_address(self.contact_editor_show_work_address);
        settings.set_contact_editor_show_other_address(self.contact_editor_show_other_address);
        settings.set_contact_editor_show_web(self.contact_editor_show_web);
        settings.set_contact_editor_show_job(self.contact_editor_show_job);
        settings.set_contact_editor_show_dates(self.contact_editor_show_dates);
        settings.set_contact_editor_show_misc(self.contact_editor_show_misc);
        settings.set_contact_editor_show_notes(self.contact_editor_show_notes);
        settings.set_contact_editor_show_certs(self.contact_editor_show_certs);
        settings.set_filters_log_actions(self.filters_log_actions);
        settings.set_filters_log_file(&self.filters_log_file);
        settings.set_flush_outbox(self.flush_outbox);
        let label_defs: Vec<crate::services::settings_service::MailLabelDefinition> = self
            .mail_labels
            .iter()
            .filter_map(|s| crate::services::settings_service::MailLabelDefinition::parse(s))
            .collect();
        settings.set_mail_labels(&label_defs);
        settings.set_junk_check_incoming(self.junk_check_incoming);
        settings.set_junk_empty_on_exit(self.junk_empty_on_exit);
        settings.set_junk_empty_on_exit_days(self.junk_empty_on_exit_days);
        settings.set_junk_lookup_addressbook(self.junk_lookup_addressbook);
        settings.set_junk_lookup_addressbook_local_only(self.junk_lookup_addressbook_local_only);
        settings.set_junk_default_plugin(&self.junk_default_plugin);
        settings.set_junk_custom_header(&self.junk_custom_header);
        settings.set_junk_custom_header_value(&self.junk_custom_header_value);
        settings.set_composer_show_from(self.composer_show_from);
        settings.set_composer_show_cc(self.composer_show_cc);
        settings.set_composer_show_bcc(self.composer_show_bcc);
        settings.set_composer_show_reply_to(self.composer_show_reply_to);
        settings.set_composer_prompt_empty_subject(self.composer_prompt_empty_subject);
        settings.set_composer_prompt_only_bcc(self.composer_prompt_only_bcc);
        settings.set_composer_inline_spelling(self.composer_inline_spelling);
        settings.set_composer_reply_style(&self.composer_reply_style);
        settings.set_composer_forward_style(&self.composer_forward_style);
        settings.set_composer_top_signature(self.composer_top_signature);
        settings.set_composer_signature_outline(self.composer_signature_outline);
        settings.set_mail_image_loading_policy(&self.mail_image_loading_policy);
        settings.set_mail_mark_seen(self.mail_mark_seen);
        settings.set_mail_mark_seen_timeout(self.mail_mark_seen_timeout);
        settings.set_mail_caret_mode(self.mail_caret_mode);
        settings.set_mail_magic_spacebar(self.mail_magic_spacebar);
        settings.set_mail_citation_highlighting(self.mail_citation_highlighting);
        settings.set_mail_citation_color_1(&self.mail_citation_color_1);
        settings.set_mail_citation_color_2(&self.mail_citation_color_2);
        settings.set_mail_citation_color_3(&self.mail_citation_color_3);
        settings.set_mail_citation_color_4(&self.mail_citation_color_4);
        settings.set_mail_citation_color_5(&self.mail_citation_color_5);
        settings.set_mail_headers_collapsed(self.mail_headers_collapsed);
        settings.set_mail_show_real_date(self.mail_show_real_date);
    }

    fn validate(&self) -> io::Result<()> {
        validate_i32_setting("window_width", self.window_width, 360, i32::MAX)?;
        validate_i32_setting("window_height", self.window_height, 480, i32::MAX)?;
        validate_choice(
            "active_view",
            &self.active_view,
            &["mail", "calendar", "contacts", "tasks", "memos"],
        )?;
        validate_choice(
            "composer_charset",
            &self.composer_charset,
            &[
                "UTF-8",
                "ISO-8859-1",
                "ISO-8859-15",
                "Windows-1252",
                "US-ASCII",
            ],
        )?;
        validate_choice(
            "composer_reply_style",
            &self.composer_reply_style,
            &["quoted", "inline", "attach"],
        )?;
        validate_choice(
            "composer_forward_style",
            &self.composer_forward_style,
            &["attached", "inline", "quoted"],
        )?;
        validate_choice(
            "switcher_location",
            &self.switcher_location,
            &["top", "bottom-left"],
        )?;
        validate_i32_setting(
            "mail_sidebar_position",
            self.mail_sidebar_position,
            0,
            2_000,
        )?;
        validate_i32_setting(
            "mail_modern_preview_position",
            self.mail_modern_preview_position,
            120,
            4_096,
        )?;
        validate_i32_setting(
            "mail_classic_preview_position",
            self.mail_classic_preview_position,
            120,
            4_096,
        )?;
        validate_i32_setting(
            "mail_sender_column_width",
            self.mail_sender_column_width,
            80,
            360,
        )?;
        validate_i32_setting(
            "mail_recipient_column_width",
            self.mail_recipient_column_width,
            80,
            360,
        )?;
        validate_i32_setting(
            "mail_date_column_width",
            self.mail_date_column_width,
            60,
            300,
        )?;
        validate_choice(
            "mail_list_view_profile",
            &self.mail_list_view_profile,
            &["messages", "wide", "custom"],
        )?;
        if crate::services::settings_service::normalize_mail_list_column_order(
            &self.mail_list_column_order,
        )
        .join(",")
            != self.mail_list_column_order
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mail_list_column_order ist nicht kanonisch",
            ));
        }
        validate_u32_setting("mail_font_size", self.mail_font_size, 8, 48)?;
        validate_u32_setting(
            "calendar_workday_start",
            self.calendar_workday_start,
            0,
            1_439,
        )?;
        validate_u32_setting("calendar_workday_end", self.calendar_workday_end, 0, 1_439)?;
        validate_u32_setting(
            "calendar_default_reminder",
            self.calendar_default_reminder,
            0,
            10_080,
        )?;
        validate_u32_setting(
            "default_reminder_interval",
            self.default_reminder_interval,
            0,
            100_000,
        )?;
        validate_u32_setting(
            "ba_reminder_interval",
            self.ba_reminder_interval,
            0,
            100_000,
        )?;
        validate_u32_setting(
            "hide_completed_tasks_value",
            self.hide_completed_tasks_value,
            0,
            100_000,
        )?;
        validate_u32_setting("mail_sync_interval", self.mail_sync_interval, 0, 86_400)?;
        validate_u32_setting(
            "mail_sync_message_limit",
            self.mail_sync_message_limit,
            25,
            1_000,
        )?;
        validate_u32_setting(
            "draft_autosave_interval",
            self.draft_autosave_interval,
            0,
            3_600,
        )?;

        validate_choice(
            "calendar_view",
            &self.calendar_view,
            &["day", "week", "workweek", "month", "agenda", "year", "list"],
        )?;
        if !matches!(self.calendar_time_divisions, 5 | 10 | 15 | 30 | 60) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "calendar_time_divisions ist keine gültige Zeiteinteilung",
            ));
        }
        validate_choice(
            "calendar_week_start",
            &self.calendar_week_start,
            &["monday", "sunday"],
        )?;
        validate_choice("date_format", &self.date_format, &["german", "iso", "us"])?;
        validate_choice("time_format", &self.time_format, &["12h", "24h"])?;
        validate_choice(
            "mail_default_folder",
            &self.mail_default_folder,
            &["all", "inbox", "sent", "drafts", "trash", "archive", "spam"],
        )?;
        validate_choice(
            "mail_sort_field",
            &self.mail_sort_field,
            &[
                "subject",
                "sender",
                "date",
                "unread",
                "flagged",
                "important",
                "attachments",
            ],
        )?;
        let folder_sort_order = serde_json::from_str::<BTreeMap<String, u32>>(
            &self.mail_folder_sort_order,
        )
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ungültige Ordnersortierung"))?;
        if folder_sort_order
            .iter()
            .any(|(folder_id, rank)| folder_id.trim().is_empty() || *rank == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ungültige Ordnersortierung",
            ));
        }
        validate_choice("mail_layout", &self.mail_layout, &["classic", "modern"])?;
        validate_choice(
            "mail_row_density",
            &self.mail_row_density,
            &["compact", "comfortable", "spacious"],
        )?;
        validate_choice(
            "color_scheme",
            &self.color_scheme,
            &["system", "light", "dark"],
        )?;
        validate_choice(
            "crypto_protocol",
            &self.crypto_protocol,
            &["openpgp", "smime"],
        )?;
        validate_choice("quote_style", &self.quote_style, &["prefix", "blockquote"])?;
        validate_choice(
            "default_reminder_units",
            &self.default_reminder_units,
            &["minutes", "hours", "days"],
        )?;
        validate_choice(
            "ba_reminder_units",
            &self.ba_reminder_units,
            &["minutes", "hours", "days"],
        )?;
        validate_choice(
            "hide_completed_tasks_units",
            &self.hide_completed_tasks_units,
            &["minutes", "hours", "days"],
        )?;
        validate_choice(
            "task_search_mode",
            &self.task_search_mode,
            &["any-field", "summary", "description", "advanced"],
        )?;
        validate_choice(
            "task_sort_field",
            &self.task_sort_field,
            &["due", "title", "priority"],
        )?;
        validate_choice(
            "memo_sort_field",
            &self.memo_sort_field,
            &["updated", "title", "source"],
        )?;
        validate_task_filter(&self.task_filter)?;
        validate_string(
            "task_category_filter",
            &self.task_category_filter,
            128,
            true,
        )?;
        validate_mail_normal_headers(&self.mail_normal_headers)?;
        validate_font_family(&self.mail_font_family)?;
        validate_spellcheck_languages(&self.spellcheck_languages)?;
        validate_calendar_workdays(&self.calendar_workdays)?;
        validate_calendar_hidden_sources(&self.calendar_hidden_sources)?;
        validate_source_or_group_list("task_hidden_sources", &self.task_hidden_sources)?;
        validate_source_or_group_list("memo_hidden_sources", &self.memo_hidden_sources)?;
        validate_source_or_group_list(
            "addressbook_hidden_sources",
            &self.addressbook_hidden_sources,
        )?;
        validate_source_or_group_list("calendar_hidden_groups", &self.calendar_hidden_groups)?;
        validate_source_or_group_list("calendar_groups_order", &self.calendar_groups_order)?;
        validate_source_or_group_list("task_hidden_groups", &self.task_hidden_groups)?;
        validate_source_or_group_list("task_groups_order", &self.task_groups_order)?;
        validate_source_or_group_list("memo_hidden_groups", &self.memo_hidden_groups)?;
        validate_source_or_group_list("memo_groups_order", &self.memo_groups_order)?;
        validate_source_or_group_list(
            "addressbook_hidden_groups",
            &self.addressbook_hidden_groups,
        )?;
        validate_source_or_group_list("addressbook_groups_order", &self.addressbook_groups_order)?;
        validate_u32_setting(
            "contact_completion_minimum_query_length",
            self.contact_completion_minimum_query_length,
            1,
            100,
        )?;
        validate_choice(
            "contact_open_map_target",
            &self.contact_open_map_target,
            &["openstreetmap", "google"],
        )?;
        validate_choice(
            "contact_layout",
            &self.contact_layout,
            &["classic", "vertical"],
        )?;
        validate_i32_setting(
            "contact_hpane_position",
            self.contact_hpane_position,
            80,
            4_096,
        )?;
        validate_i32_setting(
            "contact_vpane_position",
            self.contact_vpane_position,
            80,
            4_096,
        )?;
        validate_source_or_group_list(
            "contact_completion_book_sources",
            &self.contact_completion_book_sources,
        )?;
        for (name, value) in [
            ("default_account", &self.default_account),
            ("default_calendar_source", &self.default_calendar_source),
            (
                "default_address_book_source",
                &self.default_address_book_source,
            ),
            ("default_task_source", &self.default_task_source),
            ("default_memo_source", &self.default_memo_source),
            ("calendar_workdays", &self.calendar_workdays),
            ("calendar_time_zone", &self.calendar_time_zone),
            ("local_mail_directory", &self.local_mail_directory),
            ("local_calendar_directory", &self.local_calendar_directory),
            ("local_contact_directory", &self.local_contact_directory),
            ("default_pgp_key", &self.default_pgp_key),
            ("smime_signer_certificate", &self.smime_signer_certificate),
            ("smime_ca_file", &self.smime_ca_file),
            ("task_source_filter", &self.task_source_filter),
            ("memo_source_filter", &self.memo_source_filter),
        ] {
            validate_string(name, value, 4_096, true)?;
        }
        for certificate in self
            .smime_recipient_certificates
            .split('\n')
            .filter(|certificate| !certificate.trim().is_empty())
        {
            validate_string("smime_recipient_certificates", certificate, 4_096, false)?;
        }
        validate_i32_setting("junk_empty_on_exit_days", self.junk_empty_on_exit_days, 0, 365)?;
        validate_string("junk_default_plugin", &self.junk_default_plugin, 128, true)?;
        validate_string("junk_custom_header", &self.junk_custom_header, 256, true)?;
        validate_string("junk_custom_header_value", &self.junk_custom_header_value, 256, true)?;
        for label_str in &self.mail_labels {
            validate_string("mail_labels", label_str, 256, false)?;
        }
        validate_choice(
            "mail_image_loading_policy",
            &self.mail_image_loading_policy,
            &["never", "always", "sometimes"],
        )?;
        validate_u32_setting(
            "mail_mark_seen_timeout",
            self.mail_mark_seen_timeout,
            0,
            60_000,
        )?;
        validate_color_setting("mail_citation_color_1", &self.mail_citation_color_1)?;
        validate_color_setting("mail_citation_color_2", &self.mail_citation_color_2)?;
        validate_color_setting("mail_citation_color_3", &self.mail_citation_color_3)?;
        validate_color_setting("mail_citation_color_4", &self.mail_citation_color_4)?;
        validate_color_setting("mail_citation_color_5", &self.mail_citation_color_5)?;
        Ok(())
    }
}

fn validation_error(field: &str, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Ungültiger Sicherungswert für {field}: {reason}"),
    )
}

fn validate_color_setting(field: &str, value: &str) -> io::Result<()> {
    validate_string(field, value, 32, false)?;
    let trimmed = value.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if (hex.len() == 3 || hex.len() == 6 || hex.len() == 8)
        && hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(validation_error(field, "ungültiger Farbwert"))
    }
}

fn validate_i32_setting(field: &str, value: i32, minimum: i32, maximum: i32) -> io::Result<()> {
    if !(minimum..=maximum).contains(&value) {
        return Err(validation_error(
            field,
            "außerhalb des portablen Wertebereichs",
        ));
    }
    Ok(())
}

fn validate_u32_setting(field: &str, value: u32, minimum: u32, maximum: u32) -> io::Result<()> {
    if !(minimum..=maximum).contains(&value) {
        return Err(validation_error(
            field,
            "außerhalb des portablen Wertebereichs",
        ));
    }
    Ok(())
}

fn validate_choice(field: &str, value: &str, choices: &[&str]) -> io::Result<()> {
    if choices.contains(&value) {
        Ok(())
    } else {
        Err(validation_error(field, "nicht unterstützt"))
    }
}

fn validate_string(
    field: &str,
    value: &str,
    maximum_bytes: usize,
    allow_empty: bool,
) -> io::Result<()> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(validation_error(field, "ungültiger Textwert"));
    }
    Ok(())
}

fn validate_task_filter(value: &str) -> io::Result<()> {
    validate_choice(
        "task_filter",
        value,
        &[
            "all",
            "today",
            "next-7",
            "next-30",
            "next-365",
            "important",
            "overdue",
            "in-progress",
            "cancelled",
            "completed",
            "uncompleted",
            "scheduled",
            "no-due",
            "with-attachments",
            "started",
            "without-category",
        ],
    )
}

fn validate_mail_normal_headers(headers: &[(String, bool)]) -> io::Result<()> {
    let mut seen = HashSet::with_capacity(headers.len());
    if headers.len() > 64 {
        return Err(validation_error(
            "mail_normal_headers",
            "ungültige Headerliste",
        ));
    }
    for (name, _) in headers {
        let name = super::settings_service::validate_mail_normal_header_name(name)
            .map_err(|_| validation_error("mail_normal_headers", "ungültige Headerliste"))?;
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(validation_error(
                "mail_normal_headers",
                "doppelte Headernamen",
            ));
        }
    }
    Ok(())
}

fn validate_font_family(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 120
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || " ,-_'\"".contains(character)))
    {
        return Err(validation_error(
            "mail_font_family",
            "keine portable Schriftfamilie",
        ));
    }
    Ok(())
}

fn validate_spellcheck_languages(value: &str) -> io::Result<()> {
    if value
        .split(',')
        .filter(|language| !language.trim().is_empty())
        .any(|language| {
            let language = language.trim();
            language.len() > 35
                || language
                    .chars()
                    .any(|character| !(character.is_ascii_alphanumeric() || character == '-'))
        })
    {
        return Err(validation_error(
            "spellcheck_languages",
            "keine portable Sprachliste",
        ));
    }
    Ok(())
}

fn validate_calendar_workdays(value: &str) -> io::Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for day in value.split(',').map(str::trim) {
        if !matches!(
            day,
            "monday" | "tuesday" | "wednesday" | "thursday" | "friday" | "saturday" | "sunday"
        ) || !seen.insert(day)
        {
            return Err(validation_error(
                "calendar_workdays",
                "keine eindeutige portable Wochentagsliste",
            ));
        }
    }
    Ok(())
}

fn validate_source_or_group_list(field: &'static str, value: &str) -> io::Result<()> {
    if value.is_empty() || value.len() > 4_096 {
        if value.is_empty() {
            return Ok(());
        }
        return Err(validation_error(field, "ungültige Quellenliste"));
    }

    let mut seen = HashSet::new();
    for item in value.split(',').map(str::trim) {
        if item.is_empty() || item.chars().any(char::is_control) || !seen.insert(item) {
            return Err(validation_error(
                field,
                "keine eindeutige portable Quellenliste",
            ));
        }
    }
    Ok(())
}

fn validate_calendar_hidden_sources(value: &str) -> io::Result<()> {
    validate_source_or_group_list("calendar_hidden_sources", value)
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
    /// Safe, value-free diagnostics for fields that were not portable or
    /// were intentionally redacted.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInspection {
    pub version: u32,
    pub files: usize,
    pub settings_present: bool,
    pub checksummed_files: usize,
    pub legacy_files_without_checksum: usize,
    pub warnings: Vec<String>,
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

const BACKUP_BUNDLE_FIELDS: &[&str] = &["version", "created_at", "settings", "files"];
const BACKUP_FILE_FIELDS: &[&str] = &[
    "relative_path",
    "contents",
    "checksum_sha256",
    "redacted_fields",
];
const BACKUP_SETTINGS_FIELDS: &[&str] = &[
    "window_width",
    "window_height",
    "is_maximized",
    "active_view",
    "default_account",
    "default_calendar_source",
    "default_address_book_source",
    "default_task_source",
    "default_memo_source",
    "auto_load_remote_images",
    "offline_mode",
    "force_dark_mode",
    "render_html",
    "composer_html",
    "composer_charset",
    "request_read_receipt",
    "spellcheck_languages",
    "mail_font_family",
    "mail_font_size",
    "mark_read_on_selection",
    "show_message_preview",
    "mail_show_security_details",
    "mail_show_all_headers",
    "mail_normal_headers",
    "mail_show_statusbar",
    "mail_default_folder",
    "mail_layout",
    "mail_show_toolbar",
    "mail_show_sidebar",
    "mail_sidebar_position",
    "mail_modern_preview_position",
    "mail_classic_preview_position",
    "mail_row_density",
    "mail_show_avatar",
    "mail_show_sender",
    "mail_show_recipient",
    "mail_show_date",
    "mail_show_attachment",
    "mail_show_preview_text",
    "mail_show_labels",
    "mail_show_size",
    "mail_show_follow_up_due",
    "mail_show_location",
    "mail_sender_column_width",
    "mail_recipient_column_width",
    "mail_date_column_width",
    "mail_list_view_profile",
    "mail_list_column_order",
    "color_scheme",
    "switcher_location",
    "sign_new_messages",
    "default_pgp_key",
    "crypto_protocol",
    "smime_signer_certificate",
    "smime_recipient_certificates",
    "smime_ca_file",
    "mail_sync_interval",
    "mail_sync_message_limit",
    "draft_autosave_interval",
    "append_signature",
    "signature_text",
    "local_mail_directory",
    "local_calendar_directory",
    "local_contact_directory",
    "quote_original",
    "quote_style",
    "calendar_view",
    "calendar_week_start",
    "calendar_workdays",
    "calendar_workday_start",
    "calendar_workday_end",
    "calendar_default_reminder",
    "use_default_reminder",
    "default_reminder_interval",
    "default_reminder_units",
    "use_ba_reminder",
    "ba_reminder_interval",
    "ba_reminder_units",
    "hide_completed_tasks",
    "hide_completed_tasks_value",
    "hide_completed_tasks_units",
    "hide_cancelled_tasks",
    "hide_cancelled_events",
    "notifications_enabled",
    "confirm_destructive_actions",
    "calendar_show_week_numbers",
    "calendar_time_divisions",
    "calendar_compress_weekend",
    "calendar_month_scroll_by_week",
    "calendar_second_zone",
    "calendar_second_zones",
    "calendar_hidden_sources",
    "task_hidden_sources",
    "memo_hidden_sources",
    "addressbook_hidden_sources",
    "calendar_hidden_groups",
    "calendar_groups_order",
    "task_hidden_groups",
    "task_groups_order",
    "memo_hidden_groups",
    "memo_groups_order",
    "addressbook_hidden_groups",
    "addressbook_groups_order",
    "date_format",
    "time_format",
    "mail_sort_field",
    "mail_sort_descending",
    "mail_threading",
    "mail_folder_sort_order",
    "calendar_time_zone",
    "task_filter",
    "task_category_filter",
    "task_search_mode",
    "task_source_filter",
    "task_sort_field",
    "task_sort_descending",
    "memo_sort_field",
    "memo_source_filter",
    "memo_sort_descending",
    "contact_completion_minimum_query_length",
    "contact_completion_show_address",
    "contact_completion_book_sources",
    "contact_address_formatting",
    "contact_preview_show_maps",
    "contact_preview_home_before_work",
    "contact_open_map_target",
    "contact_open_map_prefer_local",
    "contact_layout",
    "contact_preview_visible",
    "contact_hpane_position",
    "contact_vpane_position",
    "contact_editor_show_phone",
    "contact_editor_show_sip",
    "contact_editor_show_im",
    "contact_editor_show_home_address",
    "contact_editor_show_work_address",
    "contact_editor_show_other_address",
    "contact_editor_show_web",
    "contact_editor_show_job",
    "contact_editor_show_dates",
    "contact_editor_show_misc",
    "contact_editor_show_notes",
    "contact_editor_show_certs",
    "filters_log_actions",
    "filters_log_file",
    "flush_outbox",
    "mail_labels",
    "junk_check_incoming",
    "junk_empty_on_exit",
    "junk_empty_on_exit_days",
    "junk_lookup_addressbook",
    "junk_lookup_addressbook_local_only",
    "junk_default_plugin",
    "junk_custom_header",
    "junk_custom_header_value",
    "composer_show_from",
    "composer_show_cc",
    "composer_show_bcc",
    "composer_show_reply_to",
    "composer_prompt_empty_subject",
    "composer_prompt_only_bcc",
    "composer_inline_spelling",
    "composer_reply_style",
    "composer_forward_style",
    "composer_top_signature",
    "composer_signature_outline",
    "mail_image_loading_policy",
    "mail_mark_seen",
    "mail_mark_seen_timeout",
    "mail_caret_mode",
    "mail_magic_spacebar",
    "mail_citation_highlighting",
    "mail_citation_color_1",
    "mail_citation_color_2",
    "mail_citation_color_3",
    "mail_citation_color_4",
    "mail_citation_color_5",
    "mail_headers_collapsed",
    "mail_show_real_date",
];

fn is_sensitive_backup_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('_', "-");
    [
        "password",
        "passwd",
        "token",
        "secret",
        "credential",
        "passphrase",
        "private-key",
        "privatekey",
        "oauth",
        "keyring",
        "api-key",
        "apikey",
        "client-key",
        "key-password",
        "certificate-key",
        "cert-key",
        "certificate",
        "certificates",
        "ca-file",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn safe_backup_field_name(value: &str) -> String {
    if value.len() <= 80
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        value.to_string()
    } else {
        "nicht-öffentlicher Schlüssel".to_string()
    }
}

fn append_unknown_field_warnings(
    warnings: &mut Vec<String>,
    scope: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    known_fields: &[&str],
) {
    let mut unknown = object
        .keys()
        .filter(|key| !known_fields.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    for key in unknown {
        if is_sensitive_backup_key(&key) {
            warnings.push(format!(
                "{scope}: ein nicht-portables oder geheimes Feld wurde verworfen"
            ));
        } else {
            warnings.push(format!(
                "{scope}: unbekanntes Feld „{}“ wurde nicht übernommen",
                safe_backup_field_name(&key)
            ));
        }
    }
}

fn parse_backup_bundle(raw: &[u8]) -> io::Result<(BackupBundle, Vec<String>)> {
    reject_duplicate_json_keys(raw)?;
    let value = serde_json::from_slice::<serde_json::Value>(raw).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültiges JSON-Sicherungsformat",
        )
    })?;
    let mut warnings = Vec::new();
    if let Some(object) = value.as_object() {
        append_unknown_field_warnings(
            &mut warnings,
            "Sicherungscontainer",
            object,
            BACKUP_BUNDLE_FIELDS,
        );
        if let Some(settings) = object
            .get("settings")
            .and_then(serde_json::Value::as_object)
        {
            append_unknown_field_warnings(
                &mut warnings,
                "Sicherungseinstellungen",
                settings,
                BACKUP_SETTINGS_FIELDS,
            );
        }
        if let Some(files) = object.get("files").and_then(serde_json::Value::as_array) {
            for file in files {
                if let Some(file) = file.as_object() {
                    append_unknown_field_warnings(
                        &mut warnings,
                        "Sicherungsdatei",
                        file,
                        BACKUP_FILE_FIELDS,
                    );
                }
            }
        }
    }
    let mut bundle = serde_json::from_value::<BackupBundle>(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige Typen oder Felder im Sicherungsformat",
        )
    })?;
    sanitize_backup_file_payloads(&mut bundle)?;
    for file in &bundle.files {
        if file.redacted_fields > 0 {
            warnings.push(format!(
                "Sicherungsdatei enthält {} redigierte geheime JSON-Felder",
                file.redacted_fields
            ));
        }
    }
    Ok((bundle, warnings))
}

fn sanitize_backup_file_payloads(bundle: &mut BackupBundle) -> io::Result<()> {
    for file in &mut bundle.files {
        if !file.relative_path.ends_with(".json") {
            continue;
        }
        let (contents, redacted_fields) = redact_backup_json(&file.contents)?;
        if redacted_fields == 0 {
            continue;
        }
        if !file.checksum_sha256.is_empty() && file.checksum_sha256 != sha256(&contents) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Sicherungsdatei enthält nicht redigierte geheime JSON-Felder",
            ));
        }
        file.contents = contents;
        if !file.checksum_sha256.is_empty() {
            file.checksum_sha256 = sha256(&file.contents);
        }
        file.redacted_fields = file.redacted_fields.saturating_add(redacted_fields);
    }
    Ok(())
}

struct DuplicateJsonValueSeed;

impl<'de> DeserializeSeed<'de> for DuplicateJsonValueSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateJsonValueVisitor)
    }
}

struct DuplicateJsonValueVisitor;

impl<'de> Visitor<'de> for DuplicateJsonValueVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("einen JSON-Wert ohne doppelte Objektschlüssel")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(DuplicateJsonValueSeed)?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("doppelter JSON-Schlüssel"));
            }
            map.next_value_seed(DuplicateJsonValueSeed)?;
        }
        Ok(())
    }
}

fn reject_duplicate_json_keys(raw: &[u8]) -> io::Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    deserializer
        .deserialize_any(DuplicateJsonValueVisitor)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

fn redact_backup_json(contents: &[u8]) -> io::Result<(Vec<u8>, u32)> {
    reject_duplicate_json_keys(contents)?;
    let mut value = serde_json::from_slice::<serde_json::Value>(contents).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "App-eigene JSON-Datei konnte nicht sicher redigiert werden",
        )
    })?;
    let redacted = redact_json_value(&mut value);
    if redacted == 0 {
        return Ok((contents.to_vec(), 0));
    }
    let encoded = serde_json::to_vec_pretty(&value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok((encoded, redacted))
}

fn redact_json_value(value: &mut serde_json::Value) -> u32 {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().map(redact_json_value).sum(),
        serde_json::Value::Object(object) => {
            let secret_keys = object
                .keys()
                .filter(|key| is_sensitive_backup_key(key))
                .cloned()
                .collect::<Vec<_>>();
            let mut redacted = 0;
            for key in secret_keys {
                if object.remove(&key).is_some() {
                    redacted += 1;
                }
            }
            redacted + object.values_mut().map(redact_json_value).sum::<u32>()
        }
        _ => 0,
    }
}

/// Create a JSON backup of application-owned state.
pub fn create_backup() -> io::Result<Vec<u8>> {
    let root = app_data_root();
    let relative_paths = collect_app_owned_paths(&root)?;
    let mut files = Vec::new();
    for relative_path in relative_paths {
        let path = root.join(&relative_path);
        ensure_safe_app_path(&root, &relative_path)?;
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let (contents, redacted_fields) = if relative_path.ends_with(".json") {
            redact_backup_json(&contents)?
        } else {
            (contents, 0)
        };
        files.push(BackupFile {
            relative_path,
            checksum_sha256: sha256(&contents),
            redacted_fields,
            contents,
        });
    }
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
    let (bundle, warnings) = parse_backup_bundle(raw)?;
    restore_bundle_with_warnings(
        bundle,
        &app_data_root(),
        AppSettings::try_new().as_ref(),
        warnings,
    )
}

/// Validate a backup without touching the application data directory.
///
/// This is used by the UI before showing a destructive restore confirmation,
/// so malformed paths and corrupted file contents are reported early.
pub fn inspect_backup(raw: &[u8]) -> io::Result<BackupInspection> {
    let (bundle, warnings) = parse_backup_bundle(raw)?;
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
        warnings,
    })
}

/// Validate the complete bundle before writing anything and roll back files
/// already replaced if a later atomic write fails.  This keeps a malformed or
/// interrupted restore from leaving a half-restored app-owned data set.
#[cfg(test)]
fn restore_bundle(
    bundle: BackupBundle,
    root: &Path,
    app_settings: Option<&AppSettings>,
) -> io::Result<RestoreReport> {
    restore_bundle_with_warnings(bundle, root, app_settings, Vec::new())
}

fn restore_bundle_with_warnings(
    bundle: BackupBundle,
    root: &Path,
    app_settings: Option<&AppSettings>,
    warnings: Vec<String>,
) -> io::Result<RestoreReport> {
    restore_bundle_with_writer(bundle, root, app_settings, warnings, write_atomic)
}

fn restore_bundle_with_writer<F>(
    bundle: BackupBundle,
    root: &Path,
    app_settings: Option<&AppSettings>,
    warnings: Vec<String>,
    writer: F,
) -> io::Result<RestoreReport>
where
    F: Fn(&Path, &[u8]) -> io::Result<()> + Copy,
{
    let mut bundle = bundle;
    sanitize_backup_file_payloads(&mut bundle)?;
    validate_bundle(&bundle)?;
    let mut warnings = warnings;
    if bundle.settings.is_some() && app_settings.is_none() {
        warnings.push(
            "Portable Einstellungen wurden nicht angewendet, weil das GSettings-Schema nicht verfügbar ist"
                .to_string(),
        );
    }

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
        if let Err(error) = writer(&entry.path, &entry.contents) {
            restore_entries(&written, writer);
            restore_entry(entry, writer);
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
        warnings,
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

    if let Some(settings) = bundle.settings.as_ref() {
        settings.validate()?;
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

fn restore_entries<F>(entries: &[&RestoreEntry], writer: F)
where
    F: Fn(&Path, &[u8]) -> io::Result<()> + Copy,
{
    for entry in entries.iter().rev() {
        restore_entry(entry, writer);
    }
}

fn restore_entry<F>(entry: &RestoreEntry, writer: F)
where
    F: Fn(&Path, &[u8]) -> io::Result<()> + Copy,
{
    let result = match entry.previous.as_deref() {
        Some(contents) => writer(&entry.path, contents),
        None => match fs::remove_file(&entry.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
    };
    if let Err(error) = result {
        tracing::error!(%error, "Could not roll back backup restore");
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
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Atomarer Sicherungspfad besitzt keinen Elternordner",
        )
    })?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Atomarer Sicherungspfad besitzt keinen gültigen Dateinamen",
            )
        })?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary_path = parent.join(format!(
        ".{file_name}.restore-{}-{sequence}.tmp",
        std::process::id()
    ));

    let result = (|| {
        let mut temporary = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        temporary.write_all(contents)?;
        temporary.sync_all()?;
        drop(temporary);
        fs::rename(&temporary_path, path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
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
                redacted_fields: 0,
            }],
        };
        let raw = serde_json::to_vec(&bundle).expect("bundle should serialize");
        let decoded: BackupBundle = serde_json::from_slice(&raw).expect("bundle should parse");
        assert_eq!(decoded, bundle);
        assert!(!String::from_utf8_lossy(&raw).contains("password"));
    }

    #[test]
    fn current_backup_roundtrip_preserves_set01_fields_deterministically() {
        let raw = serde_json::to_vec(&serde_json::json!({
            "version": CURRENT_VERSION,
            "created_at": "2026-08-22T12:00:00Z",
            "settings": {
                "composer_charset": "ISO-8859-15",
                "switcher_location": "top",
                "mail_sync_message_limit": 250,
                "mail_sidebar_position": 240,
                "mail_show_size": true,
                "mail_show_follow_up_due": true,
                "mail_show_location": true,
                "mail_sender_column_width": 245,
                "mail_recipient_column_width": 210,
                "mail_date_column_width": 175,
                "mail_list_view_profile": "custom",
                "mail_list_column_order": "subject,sender,recipient,date,status,attachment,flagged,follow-up,important,avatar,preview,size,follow-up-due,location,messages"
            },
            "files": []
        }))
        .expect("current fixture should serialize");

        let (bundle, warnings) = parse_backup_bundle(&raw).expect("current fixture should parse");
        assert!(warnings.is_empty());
        let settings = bundle
            .settings
            .as_ref()
            .expect("settings should be present");
        assert_eq!(settings.composer_charset, "ISO-8859-15");
        assert_eq!(settings.switcher_location, "top");
        assert_eq!(settings.mail_sync_message_limit, 250);
        assert_eq!(settings.mail_sidebar_position, 240);
        assert!(settings.mail_show_size);
        assert!(settings.mail_show_follow_up_due);
        assert!(settings.mail_show_location);
        assert_eq!(settings.mail_sender_column_width, 245);
        assert_eq!(settings.mail_recipient_column_width, 210);
        assert_eq!(settings.mail_date_column_width, 175);
        assert_eq!(settings.mail_list_view_profile, "custom");
        assert!(
            settings
                .mail_list_column_order
                .starts_with("subject,sender,recipient,date")
        );
        validate_bundle(&bundle).expect("current fixture should validate");

        let encoded = serde_json::to_vec(&bundle).expect("current fixture should round-trip");
        let decoded = serde_json::from_slice::<BackupBundle>(&encoded)
            .expect("round-tripped current fixture should parse");
        assert_eq!(decoded, bundle);
        assert_eq!(
            encoded,
            serde_json::to_vec(&decoded).expect("encoding should be stable")
        );
    }

    #[test]
    fn backup_restores_custom_mail_column_contract_and_both_widths() {
        let source_backend = gtk::gio::memory_settings_backend_new();
        let source = AppSettings::try_new_with_backend(&source_backend)
            .expect("the application settings schema should be available in tests");
        source.set_mail_sender_column_width(245);
        source.set_mail_recipient_column_width(210);
        source.set_mail_date_column_width(175);
        source.set_mail_list_column_order(&[
            "subject".to_string(),
            "sender".to_string(),
            "recipient".to_string(),
            "date".to_string(),
        ]);

        let encoded = serde_json::to_vec(&BackupSettings::from_app(&source))
            .expect("mail list backup settings should serialize");
        let snapshot: BackupSettings =
            serde_json::from_slice(&encoded).expect("mail list backup settings should deserialize");
        snapshot
            .validate()
            .expect("mail list backup settings should validate");

        let target_backend = gtk::gio::memory_settings_backend_new();
        let target = AppSettings::try_new_with_backend(&target_backend)
            .expect("the application settings schema should be available in tests");
        snapshot.apply_to(&target);

        assert_eq!(target.mail_sender_column_width(), 245);
        assert_eq!(target.mail_recipient_column_width(), 210);
        assert_eq!(target.mail_date_column_width(), 175);
        assert_eq!(target.mail_list_view_profile(), "custom");
        assert_eq!(
            &target.mail_list_column_order()[..4],
            &["subject", "sender", "recipient", "date"]
        );
    }

    #[test]
    fn older_backup_versions_keep_legacy_files_and_use_safe_new_defaults() {
        for version in [1, 2, 3] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "version": version,
                "created_at": "2026-08-22T12:00:00Z",
                "settings": {
                    "window_width": 1100,
                    "window_height": 700,
                    "active_view": "mail",
                    "mail_sidebar_position": 220,
                    "smime_private_key": "PRIVATE-KEY-MUST-NOT-ROUNDTRIP"
                },
                "files": [{
                    "relative_path": "drafts.json",
                    "contents": [123, 34, 118, 101, 114, 115, 105, 111, 110, 34, 58, 49, 125]
                }]
            }))
            .expect("legacy fixture should serialize");

            let (bundle, warnings) =
                parse_backup_bundle(&raw).expect("legacy fixture should parse");
            validate_bundle(&bundle).expect("legacy fixture should validate");
            let settings = bundle
                .settings
                .as_ref()
                .expect("legacy settings should be present");
            assert_eq!(settings.composer_charset, "UTF-8");
            assert_eq!(settings.switcher_location, "bottom-left");
            assert_eq!(settings.mail_sync_message_limit, 100);
            assert_eq!(settings.mail_sidebar_position, 220);
            assert!(settings.smime_private_key.is_empty());
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("geheimes Feld"))
            );

            let encoded = serde_json::to_vec(&bundle).expect("legacy fixture should round-trip");
            let text = String::from_utf8_lossy(&encoded);
            assert!(!text.contains("smime_private_key"));
            assert!(!text.contains("PRIVATE-KEY-MUST-NOT-ROUNDTRIP"));
            let decoded = serde_json::from_slice::<BackupBundle>(&encoded)
                .expect("round-tripped legacy fixture should parse");
            assert_eq!(decoded.version, version);
            assert!(decoded.files[0].checksum_sha256.is_empty());
            assert_eq!(decoded.files[0].redacted_fields, 0);
        }
    }

    #[test]
    fn backup_rejects_malformed_types_ranges_and_unknown_future_values() {
        for raw in [b"{\"version\":4".as_slice(), b"not-json".as_slice()] {
            let error = inspect_backup(raw).expect_err("malformed backup must be rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }

        let raw = serde_json::to_vec(&serde_json::json!({
            "version": CURRENT_VERSION,
            "created_at": "2026-08-22T12:00:00Z",
            "settings": {"mail_sync_message_limit": "PASSWORD-MUST-NOT-APPEAR"},
            "files": []
        }))
        .expect("secret type-error fixture should serialize");
        let error = inspect_backup(&raw).expect_err("secret type mismatch must be rejected");
        assert!(!error.to_string().contains("PASSWORD-MUST-NOT-APPEAR"));

        let fixtures = [
            serde_json::json!({"mail_sync_message_limit": "100"}),
            serde_json::json!({"mail_sync_message_limit": 24}),
            serde_json::json!({"mail_sender_column_width": 79}),
            serde_json::json!({"mail_recipient_column_width": 79}),
            serde_json::json!({"mail_date_column_width": 59}),
            serde_json::json!({"mail_list_view_profile": "invalid"}),
            serde_json::json!({"mail_list_column_order": "date,date"}),
            serde_json::json!({"switcher_location": "sidebar"}),
            serde_json::json!({"composer_charset": "X-UNKNOWN"}),
            serde_json::json!({"calendar_hidden_sources": "source-a,source-a"}),
            serde_json::json!({
                "mail_normal_headers": [["From", true], ["from", false]]
            }),
        ];
        for settings in fixtures {
            let raw = serde_json::to_vec(&serde_json::json!({
                "version": CURRENT_VERSION,
                "created_at": "2026-08-22T12:00:00Z",
                "settings": settings,
                "files": []
            }))
            .expect("invalid fixture should serialize");
            let error = inspect_backup(&raw).expect_err("invalid setting must be rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("X-UNKNOWN"));
        }

        let raw = serde_json::to_vec(&serde_json::json!({
            "version": CURRENT_VERSION,
            "created_at": "2026-08-22T12:00:00Z",
            "settings": {
                "future-setting": "new-value",
                "api_token": "MUST-NOT-APPEAR"
            },
            "files": []
        }))
        .expect("unknown-field fixture should serialize");
        let (bundle, warnings) = parse_backup_bundle(&raw).expect("unknown fields remain readable");
        assert!(bundle.settings.is_some());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("future-setting"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("geheimes Feld"))
        );
        let warning_text = warnings.join(" ");
        assert!(!warning_text.contains("MUST-NOT-APPEAR"));
        assert!(!warning_text.contains("api_token"));
    }

    #[test]
    fn backup_rejects_duplicate_json_keys_without_exposing_values() {
        let raw = br#"{
            "version": 4,
            "created_at": "2026-08-22T12:00:00Z",
            "settings": {
                "api_key": "FIRST-SECRET",
                "api_key": "SECOND-SECRET"
            },
            "files": []
        }"#;
        let error = inspect_backup(raw).expect_err("duplicate JSON keys must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("doppelter JSON-Schlüssel"));
        assert!(!error.to_string().contains("FIRST-SECRET"));
        assert!(!error.to_string().contains("SECOND-SECRET"));
    }

    #[test]
    fn restore_redacts_secret_values_in_legacy_json_payloads_before_writing() {
        let secret_contents = serde_json::to_vec(&serde_json::json!({
            "private_key": "LEGACY-PRIVATE-KEY"
        }))
        .expect("legacy JSON contents should serialize");
        let raw = serde_json::to_vec(&serde_json::json!({
            "version": 3,
            "created_at": "2026-08-22T12:00:00Z",
            "settings": null,
            "files": [{
                "relative_path": "drafts.json",
                "contents": secret_contents.clone()
            }]
        }))
        .expect("legacy secret fixture should serialize");
        let (bundle, warnings) =
            parse_backup_bundle(&raw).expect("checksum-free legacy JSON should be safely redacted");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("redigierte"))
        );
        let contents = String::from_utf8(bundle.files[0].contents.clone())
            .expect("redacted legacy JSON should remain UTF-8");
        assert!(!contents.contains("LEGACY-PRIVATE-KEY"));
        assert_eq!(bundle.files[0].redacted_fields, 1);

        let inconsistent = serde_json::to_vec(&serde_json::json!({
            "version": 4,
            "created_at": "2026-08-22T12:00:00Z",
            "settings": null,
            "files": [{
                "relative_path": "drafts.json",
                "contents": secret_contents,
                "checksum_sha256": sha256(&secret_contents)
            }]
        }))
        .expect("inconsistent checksum fixture should serialize");
        let error = parse_backup_bundle(&inconsistent)
            .expect_err("a checksum for unredacted content must not be rewritten");
        assert!(
            error
                .to_string()
                .contains("nicht redigierte geheime JSON-Felder")
        );
        assert!(!error.to_string().contains("LEGACY-PRIVATE-KEY"));

        let root = temporary_directory("restore-legacy-redaction");
        restore_bundle(bundle, &root, None).expect("redacted legacy restore should succeed");
        let restored = fs::read_to_string(root.join("drafts.json"))
            .expect("restored draft JSON should be readable");
        assert!(!restored.contains("LEGACY-PRIVATE-KEY"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backup_redaction_removes_nested_secret_values_but_keeps_portable_data() {
        let contents = br#"{
            "user": "alice",
            "password": "PASSWORD-MUST-NOT-APPEAR",
            "access_token": "TOKEN-MUST-NOT-APPEAR",
            "apiKey": "API-KEY-MUST-NOT-APPEAR",
            "nested": {
                "privateKey": "PRIVATE-KEY-MUST-NOT-APPEAR",
                "certificate": "CERTIFICATE-MUST-NOT-APPEAR",
                "keep": "portable"
            },
            "items": [{"oauth_token": "OAUTH-MUST-NOT-APPEAR"}]
        }"#;
        let (redacted, count) = redact_backup_json(contents).expect("JSON should be redacted");
        let text = String::from_utf8(redacted).expect("redacted JSON should remain UTF-8");
        assert_eq!(count, 6);
        assert!(text.contains("alice"));
        assert!(text.contains("portable"));
        for secret in [
            "PASSWORD-MUST-NOT-APPEAR",
            "TOKEN-MUST-NOT-APPEAR",
            "PRIVATE-KEY-MUST-NOT-APPEAR",
            "CERTIFICATE-MUST-NOT-APPEAR",
            "OAUTH-MUST-NOT-APPEAR",
            "API-KEY-MUST-NOT-APPEAR",
        ] {
            assert!(!text.contains(secret));
        }

        let malformed = br#"{"password":"SECRET-TRUNCATED""#;
        let error = redact_backup_json(malformed)
            .expect_err("malformed app-owned JSON must fail closed before backup");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!error.to_string().contains("SECRET-TRUNCATED"));
    }

    #[test]
    fn atomic_write_replaces_file_without_leaking_temporary_files() {
        let root = temporary_directory("atomic-write");
        let path = root.join("nested/state.json");
        fs::create_dir_all(path.parent().expect("parent should exist"))
            .expect("parent should exist");
        fs::write(&path, b"old").expect("old contents should be written");

        write_atomic(&path, b"new").expect("atomic write should succeed");
        assert_eq!(
            fs::read(&path).expect("new contents should be readable"),
            b"new"
        );
        assert_eq!(
            fs::read_dir(path.parent().expect("parent should exist"))
                .expect("parent should be readable")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".restore-"))
                .count(),
            0
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_cleans_temporary_file_when_rename_fails() {
        let root = temporary_directory("atomic-rename-failure");
        let target = root.join("target-directory");
        fs::create_dir_all(&target).expect("target directory should exist");

        let error = write_atomic(&target, b"must-not-replace-directory")
            .expect_err("renaming a file over a directory must fail");
        assert!(matches!(
            error.kind(),
            io::ErrorKind::IsADirectory | io::ErrorKind::AlreadyExists | io::ErrorKind::Other
        ));
        assert_eq!(
            fs::read_dir(&root)
                .expect("root should be readable")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".restore-"))
                .count(),
            0
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_rolls_back_previous_files_after_a_later_write_failure() {
        let root = temporary_directory("restore-rollback");
        fs::write(root.join("accounts.json"), br#"{"state":"old-account"}"#)
            .expect("old account contents should be written");
        fs::write(root.join("drafts.json"), br#"{"state":"old-draft"}"#)
            .expect("old draft contents should be written");

        let bundle = BackupBundle {
            version: CURRENT_VERSION,
            created_at: "2026-08-22T12:00:00Z".to_string(),
            settings: None,
            files: vec![
                BackupFile {
                    relative_path: "accounts.json".to_string(),
                    contents: br#"{"state":"new-account"}"#.to_vec(),
                    checksum_sha256: sha256(br#"{"state":"new-account"}"#),
                    redacted_fields: 0,
                },
                BackupFile {
                    relative_path: "drafts.json".to_string(),
                    contents: br#"{"state":"new-draft"}"#.to_vec(),
                    checksum_sha256: sha256(br#"{"state":"new-draft"}"#),
                    redacted_fields: 0,
                },
            ],
        };
        let writer = |path: &Path, contents: &[u8]| {
            if path.file_name().is_some_and(|name| name == "drafts.json") {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected rename failure",
                ))
            } else {
                write_atomic(path, contents)
            }
        };

        let error = restore_bundle_with_writer(bundle, &root, None, Vec::new(), writer)
            .expect_err("a later write failure must abort the restore");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(
            fs::read(root.join("accounts.json")).unwrap(),
            br#"{"state":"old-account"}"#
        );
        assert_eq!(
            fs::read(root.join("drafts.json")).unwrap(),
            br#"{"state":"old-draft"}"#
        );

        let _ = fs::remove_dir_all(root);
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
                contents: br#"{"state":"modified"}"#.to_vec(),
                checksum_sha256: sha256(br#"{"state":"original"}"#),
                redacted_fields: 0,
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
                    redacted_fields: 0,
                },
                BackupFile {
                    relative_path: "vcards/imported.vcf".to_string(),
                    contents: b"restored contact".to_vec(),
                    checksum_sha256: String::new(),
                    redacted_fields: 0,
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
                    contents: br#"{"state":"replacement"}"#.to_vec(),
                    checksum_sha256: String::new(),
                    redacted_fields: 0,
                },
                BackupFile {
                    relative_path: "../outside.json".to_string(),
                    contents: br#"{"state":"must-not-be-written"}"#.to_vec(),
                    checksum_sha256: String::new(),
                    redacted_fields: 0,
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
        assert!(!settings.use_default_reminder);
        assert_eq!(settings.default_reminder_interval, 15);
        assert_eq!(settings.default_reminder_units, "minutes");
        assert!(!settings.use_ba_reminder);
        assert_eq!(settings.ba_reminder_interval, 15);
        assert_eq!(settings.ba_reminder_units, "minutes");
        assert!(!settings.hide_completed_tasks);
        assert_eq!(settings.hide_completed_tasks_value, 1);
        assert_eq!(settings.hide_completed_tasks_units, "days");
        assert!(!settings.hide_cancelled_tasks);
        assert!(!settings.hide_cancelled_events);
        assert!(settings.notifications_enabled);
        assert!(settings.mark_read_on_selection);
        assert!(settings.show_message_preview);
        assert_eq!(settings.composer_charset, "UTF-8");
        assert!(!settings.mail_show_security_details);
        assert!(!settings.mail_show_all_headers);
        assert_eq!(settings.mail_normal_headers, default_mail_normal_headers());
        assert!(settings.mail_show_statusbar);
        assert_eq!(settings.mail_font_size, 16);
        assert_eq!(settings.mail_default_folder, "inbox");
        assert!(settings.spellcheck_languages.is_empty());
        assert_eq!(settings.mail_font_size, 16);
        assert_eq!(settings.mail_layout, "classic");
        assert!(settings.mail_show_toolbar);
        assert!(settings.mail_show_sidebar);
        assert_eq!(settings.calendar_time_zone, "system");
        assert_eq!(settings.calendar_time_divisions, 30);
        assert!(settings.calendar_compress_weekend);
        assert!(settings.calendar_month_scroll_by_week);
        assert_eq!(settings.calendar_second_zone, "");
        assert_eq!(settings.calendar_second_zones, "");
        assert_eq!(settings.mail_sidebar_position, 240);
        assert_eq!(settings.mail_modern_preview_position, 420);
        assert_eq!(settings.mail_classic_preview_position, 360);
        assert_eq!(settings.mail_row_density, "compact");
        assert!(!settings.mail_show_avatar);
        assert!(settings.mail_show_sender);
        assert!(!settings.mail_show_recipient);
        assert!(settings.mail_show_date);
        assert!(settings.mail_show_attachment);
        assert!(!settings.mail_show_preview_text);
        assert!(settings.mail_show_labels);
        assert!(!settings.mail_show_size);
        assert!(!settings.mail_show_follow_up_due);
        assert!(!settings.mail_show_location);
        assert!(!settings.calendar_show_week_numbers);
        assert!(settings.calendar_hidden_sources.is_empty());
        assert!(settings.task_hidden_sources.is_empty());
        assert!(settings.memo_hidden_sources.is_empty());
        assert!(settings.addressbook_hidden_sources.is_empty());
        assert!(settings.calendar_hidden_groups.is_empty());
        assert!(settings.calendar_groups_order.is_empty());
        assert!(settings.task_hidden_groups.is_empty());
        assert!(settings.task_groups_order.is_empty());
        assert!(settings.memo_hidden_groups.is_empty());
        assert!(settings.memo_groups_order.is_empty());
        assert!(settings.addressbook_hidden_groups.is_empty());
        assert!(settings.addressbook_groups_order.is_empty());
        assert_eq!(settings.mail_sort_field, "date");
        assert!(settings.mail_sort_descending);
        assert!(settings.mail_threading);
        assert_eq!(settings.mail_folder_sort_order, "{}");
        assert!(settings.task_source_filter.is_empty());
        assert!(settings.task_sort_descending);
        assert_eq!(settings.task_filter, "all");
        assert!(settings.task_category_filter.is_empty());
        assert_eq!(settings.task_search_mode, "any-field");
        assert!(settings.memo_source_filter.is_empty());
        assert!(settings.memo_sort_descending);
        assert_eq!(settings.contact_completion_minimum_query_length, 3);
        assert!(!settings.contact_completion_show_address);
        assert!(settings.contact_completion_book_sources.is_empty());
        assert!(!settings.contact_address_formatting);
        assert!(!settings.contact_preview_show_maps);
        assert!(!settings.contact_preview_home_before_work);
        assert_eq!(settings.contact_open_map_target, "openstreetmap");
        assert!(settings.contact_open_map_prefer_local);
        assert_eq!(settings.contact_layout, "classic");
        assert!(settings.contact_preview_visible);
        assert_eq!(settings.contact_hpane_position, 200);
        assert_eq!(settings.contact_vpane_position, 400);
        assert!(settings.contact_editor_show_phone);
        assert!(settings.contact_editor_show_sip);
        assert!(settings.contact_editor_show_im);
        assert!(settings.contact_editor_show_home_address);
        assert!(settings.contact_editor_show_work_address);
        assert!(settings.contact_editor_show_other_address);
        assert!(settings.contact_editor_show_web);
        assert!(settings.contact_editor_show_job);
        assert!(settings.contact_editor_show_dates);
        assert!(settings.contact_editor_show_misc);
        assert!(settings.contact_editor_show_notes);
        assert!(settings.contact_editor_show_certs);
    }

    #[test]
    fn contact_settings_backup_validation_and_roundtrip() {
        let json = serde_json::json!({
            "contact_completion_minimum_query_length": 5,
            "contact_completion_show_address": true,
            "contact_completion_book_sources": "addr-1,addr-2",
            "contact_address_formatting": true,
            "contact_preview_show_maps": true,
            "contact_preview_home_before_work": true,
            "contact_open_map_target": "google",
            "contact_open_map_prefer_local": false,
            "contact_layout": "vertical",
            "contact_preview_visible": false,
            "contact_hpane_position": 250,
            "contact_vpane_position": 450,
            "contact_editor_show_phone": false,
            "contact_editor_show_sip": false,
            "contact_editor_show_im": false,
            "contact_editor_show_home_address": false,
            "contact_editor_show_work_address": false,
            "contact_editor_show_other_address": false,
            "contact_editor_show_web": false,
            "contact_editor_show_job": false,
            "contact_editor_show_dates": false,
            "contact_editor_show_misc": false,
            "contact_editor_show_notes": false,
            "contact_editor_show_certs": false
        });
        let settings: BackupSettings =
            serde_json::from_value(json).expect("contact settings should parse");
        assert_eq!(settings.contact_completion_minimum_query_length, 5);
        assert!(settings.contact_completion_show_address);
        assert_eq!(settings.contact_completion_book_sources, "addr-1,addr-2");
        assert!(settings.contact_address_formatting);
        assert!(settings.contact_preview_show_maps);
        assert!(settings.contact_preview_home_before_work);
        assert_eq!(settings.contact_open_map_target, "google");
        assert!(!settings.contact_open_map_prefer_local);
        assert_eq!(settings.contact_layout, "vertical");
        assert!(!settings.contact_preview_visible);
        assert_eq!(settings.contact_hpane_position, 250);
        assert_eq!(settings.contact_vpane_position, 450);
        assert!(!settings.contact_editor_show_phone);
        assert!(!settings.contact_editor_show_sip);
        assert!(!settings.contact_editor_show_im);
        assert!(!settings.contact_editor_show_home_address);
        assert!(!settings.contact_editor_show_work_address);
        assert!(!settings.contact_editor_show_other_address);
        assert!(!settings.contact_editor_show_web);
        assert!(!settings.contact_editor_show_job);
        assert!(!settings.contact_editor_show_dates);
        assert!(!settings.contact_editor_show_misc);
        assert!(!settings.contact_editor_show_notes);
        assert!(!settings.contact_editor_show_certs);
        assert!(settings.validate().is_ok());

        let mut invalid_target = settings.clone();
        invalid_target.contact_open_map_target = "bing".to_string();
        assert!(invalid_target.validate().is_err());

        let mut invalid_layout = settings.clone();
        invalid_layout.contact_layout = "compact".to_string();
        assert!(invalid_layout.validate().is_err());

        let mut invalid_query_len = settings.clone();
        invalid_query_len.contact_completion_minimum_query_length = 0;
        assert!(invalid_query_len.validate().is_err());
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
        assert!(!settings.force_dark_mode);
        assert!(settings.render_html);
        assert!(settings.composer_html);
        assert_eq!(settings.composer_charset, "UTF-8");
        assert!(!settings.request_read_receipt);
        assert!(settings.spellcheck_languages.is_empty());
        assert_eq!(settings.color_scheme, "system");
        assert!(!settings.sign_new_messages);
        assert!(settings.default_pgp_key.is_empty());
        assert_eq!(settings.mail_sync_interval, 30);
        assert_eq!(settings.mail_sync_message_limit, 100);
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
    fn task_filter_settings_survive_backup_json_roundtrip() {
        let settings: BackupSettings = serde_json::from_value(serde_json::json!({
            "task_filter": "next-30",
            "task_category_filter": "Work Team",
            "task_search_mode": "advanced"
        }))
        .expect("task settings should parse");
        let raw = serde_json::to_vec(&settings).expect("task settings should serialize");
        let restored: BackupSettings =
            serde_json::from_slice(&raw).expect("task settings should round-trip");

        assert_eq!(restored.task_filter, "next-30");
        assert_eq!(restored.task_category_filter, "Work Team");
        assert_eq!(restored.task_search_mode, "advanced");
    }

    #[test]
    fn folder_sort_order_settings_survive_backup_json_roundtrip() {
        let settings: BackupSettings = serde_json::from_value(serde_json::json!({
            "mail_folder_sort_order": "{\"custom:Projects\":1,\"account\\u0000Archive\":2}"
        }))
        .expect("folder sort order should parse");
        settings
            .validate()
            .expect("folder sort order should validate");
        let raw = serde_json::to_vec(&settings).expect("settings should serialize");
        let restored: BackupSettings =
            serde_json::from_slice(&raw).expect("settings should round-trip");
        assert_eq!(
            restored.mail_folder_sort_order,
            settings.mail_folder_sort_order
        );
        assert!(BACKUP_SETTINGS_FIELDS.contains(&"mail_folder_sort_order"));
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

    #[test]
    fn calendar_view_and_timezone_settings_backup_validation_and_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let app_settings =
            AppSettings::try_new_with_backend(&backend).expect("settings should load");

        app_settings.set_calendar_view("year");
        app_settings.set_calendar_time_divisions(15);
        app_settings.set_calendar_compress_weekend(false);
        app_settings.set_calendar_month_scroll_by_week(false);
        app_settings.set_calendar_second_zone("Europe/London");
        app_settings.set_calendar_second_zones(&[
            "Europe/London".to_string(),
            "America/New_York".to_string(),
        ]);

        let backup = BackupSettings::from_app(&app_settings);
        backup.validate().expect("backup settings should validate");

        assert_eq!(backup.calendar_view, "year");
        assert_eq!(backup.calendar_time_divisions, 15);
        assert!(!backup.calendar_compress_weekend);
        assert!(!backup.calendar_month_scroll_by_week);
        assert_eq!(backup.calendar_second_zone, "Europe/London");
        assert_eq!(
            backup.calendar_second_zones,
            "Europe/London,America/New_York"
        );

        // List view also validates
        let mut list_backup = backup.clone();
        list_backup.calendar_view = "list".to_string();
        list_backup.validate().expect("list view should validate");

        // Invalid calendar view rejected
        let mut invalid_view = backup.clone();
        invalid_view.calendar_view = "century".to_string();
        assert!(invalid_view.validate().is_err());

        // Invalid time divisions rejected
        let mut invalid_div = backup.clone();
        invalid_div.calendar_time_divisions = 25;
        assert!(invalid_div.validate().is_err());

        // Roundtrip via JSON serialization
        let serialized = serde_json::to_string(&backup).expect("should serialize");
        let deserialized: BackupSettings =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert_eq!(deserialized.calendar_view, "year");
        assert_eq!(deserialized.calendar_time_divisions, 15);
        assert!(!deserialized.calendar_compress_weekend);
        assert!(!deserialized.calendar_month_scroll_by_week);
        assert_eq!(deserialized.calendar_second_zone, "Europe/London");
        assert_eq!(
            deserialized.calendar_second_zones,
            "Europe/London,America/New_York"
        );
    }

    #[test]
    fn source_and_group_settings_backup_validation_and_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = AppSettings::try_new_with_backend(&backend).expect("settings should load");

        settings.set_task_hidden_sources(&["task-1".to_string(), "task-2".to_string()]);
        settings.set_memo_hidden_sources(&["memo-1".to_string()]);
        settings.set_addressbook_hidden_sources(&["addr-1".to_string()]);
        settings.set_calendar_hidden_groups(&["cal-grp-1".to_string()]);
        settings.set_calendar_groups_order(&["grp-b".to_string(), "grp-a".to_string()]);
        settings.set_task_hidden_groups(&["task-grp-1".to_string()]);
        settings.set_task_groups_order(&["task-ord-1".to_string()]);
        settings.set_memo_hidden_groups(&["memo-grp-1".to_string()]);
        settings.set_memo_groups_order(&["memo-ord-1".to_string()]);
        settings.set_addressbook_hidden_groups(&["book-grp-1".to_string()]);
        settings.set_addressbook_groups_order(&["book-ord-1".to_string()]);

        let backup = BackupSettings::from_app(&settings);

        assert_eq!(backup.task_hidden_sources, "task-1,task-2");
        assert_eq!(backup.memo_hidden_sources, "memo-1");
        assert_eq!(backup.addressbook_hidden_sources, "addr-1");
        assert_eq!(backup.calendar_hidden_groups, "cal-grp-1");
        assert_eq!(backup.calendar_groups_order, "grp-b,grp-a");
        assert_eq!(backup.task_hidden_groups, "task-grp-1");
        assert_eq!(backup.task_groups_order, "task-ord-1");
        assert_eq!(backup.memo_hidden_groups, "memo-grp-1");
        assert_eq!(backup.memo_groups_order, "memo-ord-1");
        assert_eq!(backup.addressbook_hidden_groups, "book-grp-1");
        assert_eq!(backup.addressbook_groups_order, "book-ord-1");

        // Validate succeeds on valid settings
        backup.validate().expect("backup should validate");

        // Invalid (duplicate UID in list)
        let mut invalid_dupe = backup.clone();
        invalid_dupe.task_hidden_sources = "task-1,task-1".to_string();
        assert!(invalid_dupe.validate().is_err());

        // Invalid (control character in group order)
        let mut invalid_ctrl = backup.clone();
        invalid_ctrl.calendar_groups_order = "grp-a\ngrp-b".to_string();
        assert!(invalid_ctrl.validate().is_err());

        // JSON serialization round-trip
        let serialized = serde_json::to_string(&backup).expect("should serialize");
        let deserialized: BackupSettings =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert_eq!(deserialized.task_hidden_sources, "task-1,task-2");
        assert_eq!(deserialized.memo_hidden_sources, "memo-1");
        assert_eq!(deserialized.addressbook_hidden_sources, "addr-1");
        assert_eq!(deserialized.calendar_groups_order, "grp-b,grp-a");

        // Restoration into fresh settings
        let fresh_backend = gio::memory_settings_backend_new();
        let fresh_settings =
            AppSettings::try_new_with_backend(&fresh_backend).expect("settings should load");
        deserialized.apply_to(&fresh_settings);

        assert_eq!(
            fresh_settings.task_hidden_sources(),
            vec!["task-1", "task-2"]
        );
        assert_eq!(fresh_settings.memo_hidden_sources(), vec!["memo-1"]);
        assert_eq!(fresh_settings.addressbook_hidden_sources(), vec!["addr-1"]);
        assert_eq!(
            fresh_settings.calendar_groups_order(),
            vec!["grp-b", "grp-a"]
        );
    }

    #[test]
    fn reminder_and_task_filter_settings_backup_validation_and_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = AppSettings::try_new_with_backend(&backend).expect("settings should load");

        settings.set_use_default_reminder(true);
        settings.set_default_reminder_interval(45);
        settings.set_default_reminder_units(ReminderDurationUnit::Minutes);
        settings.set_use_ba_reminder(true);
        settings.set_ba_reminder_interval(2);
        settings.set_ba_reminder_units(ReminderDurationUnit::Days);
        settings.set_hide_completed_tasks(true);
        settings.set_hide_completed_tasks_value(3);
        settings.set_hide_completed_tasks_units(ReminderDurationUnit::Hours);
        settings.set_hide_cancelled_tasks(true);
        settings.set_hide_cancelled_events(true);

        let backup = BackupSettings::from_app(&settings);

        assert!(backup.use_default_reminder);
        assert_eq!(backup.default_reminder_interval, 45);
        assert_eq!(backup.default_reminder_units, "minutes");
        assert!(backup.use_ba_reminder);
        assert_eq!(backup.ba_reminder_interval, 2);
        assert_eq!(backup.ba_reminder_units, "days");
        assert!(backup.hide_completed_tasks);
        assert_eq!(backup.hide_completed_tasks_value, 3);
        assert_eq!(backup.hide_completed_tasks_units, "hours");
        assert!(backup.hide_cancelled_tasks);
        assert!(backup.hide_cancelled_events);

        backup.validate().expect("backup should validate");

        // Validate rejects invalid unit
        let mut invalid_units = backup.clone();
        invalid_units.default_reminder_units = "weeks".to_string();
        assert!(invalid_units.validate().is_err());

        let mut invalid_ba_units = backup.clone();
        invalid_ba_units.ba_reminder_units = "seconds".to_string();
        assert!(invalid_ba_units.validate().is_err());

        let mut invalid_task_units = backup.clone();
        invalid_task_units.hide_completed_tasks_units = "months".to_string();
        assert!(invalid_task_units.validate().is_err());

        // Validate rejects out of bounds interval
        let mut invalid_interval = backup.clone();
        invalid_interval.default_reminder_interval = 200_000;
        assert!(invalid_interval.validate().is_err());

        // JSON serialization round-trip
        let serialized = serde_json::to_string(&backup).expect("should serialize");
        let deserialized: BackupSettings =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert!(deserialized.use_default_reminder);
        assert_eq!(deserialized.default_reminder_interval, 45);
        assert_eq!(deserialized.default_reminder_units, "minutes");
        assert!(deserialized.use_ba_reminder);
        assert_eq!(deserialized.ba_reminder_interval, 2);
        assert_eq!(deserialized.ba_reminder_units, "days");
        assert!(deserialized.hide_completed_tasks);
        assert_eq!(deserialized.hide_completed_tasks_value, 3);
        assert_eq!(deserialized.hide_completed_tasks_units, "hours");
        assert!(deserialized.hide_cancelled_tasks);
        assert!(deserialized.hide_cancelled_events);

        // Restoration into fresh settings
        let fresh_backend = gio::memory_settings_backend_new();
        let fresh_settings =
            AppSettings::try_new_with_backend(&fresh_backend).expect("settings should load");
        deserialized.apply_to(&fresh_settings);

        assert!(fresh_settings.use_default_reminder());
        assert_eq!(fresh_settings.default_reminder_interval(), 45);
        assert_eq!(
            fresh_settings.default_reminder_units(),
            ReminderDurationUnit::Minutes
        );
        assert!(fresh_settings.use_ba_reminder());
        assert_eq!(fresh_settings.ba_reminder_interval(), 2);
        assert_eq!(
            fresh_settings.ba_reminder_units(),
            ReminderDurationUnit::Days
        );
        assert!(fresh_settings.hide_completed_tasks());
        assert_eq!(fresh_settings.hide_completed_tasks_value(), 3);
        assert_eq!(
            fresh_settings.hide_completed_tasks_units(),
            ReminderDurationUnit::Hours
        );
        assert!(fresh_settings.hide_cancelled_tasks());
        assert!(fresh_settings.hide_cancelled_events());
    }

    #[test]
    fn mail_labels_and_junk_settings_backup_validation_and_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = AppSettings::try_new_with_backend(&backend).expect("settings should load");

        let custom_labels = vec![
            crate::services::settings_service::MailLabelDefinition::new(
                "Urgent",
                "#FF0000",
                Some("$Labelurgent".to_string()),
            ),
            crate::services::settings_service::MailLabelDefinition::new(
                "Project Alpha",
                "#00FF00",
                None,
            ),
        ];
        settings.set_mail_labels(&custom_labels);
        settings.set_junk_check_incoming(false);
        settings.set_junk_empty_on_exit(true);
        settings.set_junk_empty_on_exit_days(14);
        settings.set_junk_lookup_addressbook(true);
        settings.set_junk_lookup_addressbook_local_only(true);
        settings.set_junk_default_plugin("SpamAssassin");
        settings.set_junk_custom_header("X-Spam-Flag");
        settings.set_junk_custom_header_value("YES");

        let backup = BackupSettings::from_app(&settings);

        assert_eq!(backup.mail_labels.len(), 2);
        assert_eq!(backup.mail_labels[0], "Urgent:#FF0000|$Labelurgent");
        assert_eq!(backup.mail_labels[1], "Project Alpha:#00FF00|project_alpha");
        assert!(!backup.junk_check_incoming);
        assert!(backup.junk_empty_on_exit);
        assert_eq!(backup.junk_empty_on_exit_days, 14);
        assert!(backup.junk_lookup_addressbook);
        assert!(backup.junk_lookup_addressbook_local_only);
        assert_eq!(backup.junk_default_plugin, "SpamAssassin");
        assert_eq!(backup.junk_custom_header, "X-Spam-Flag");
        assert_eq!(backup.junk_custom_header_value, "YES");

        backup.validate().expect("backup should validate");

        // Validate rejects out of bounds empty on exit days
        let mut invalid_days = backup.clone();
        invalid_days.junk_empty_on_exit_days = 400;
        assert!(invalid_days.validate().is_err());

        // JSON serialization round-trip
        let serialized = serde_json::to_string(&backup).expect("should serialize");
        let deserialized: BackupSettings =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert_eq!(deserialized.mail_labels.len(), 2);
        assert_eq!(deserialized.mail_labels[0], "Urgent:#FF0000|$Labelurgent");
        assert!(!deserialized.junk_check_incoming);
        assert!(deserialized.junk_empty_on_exit);
        assert_eq!(deserialized.junk_empty_on_exit_days, 14);
        assert!(deserialized.junk_lookup_addressbook);
        assert!(deserialized.junk_lookup_addressbook_local_only);
        assert_eq!(deserialized.junk_default_plugin, "SpamAssassin");
        assert_eq!(deserialized.junk_custom_header, "X-Spam-Flag");
        assert_eq!(deserialized.junk_custom_header_value, "YES");

        // Restoration into fresh settings
        let fresh_backend = gio::memory_settings_backend_new();
        let fresh_settings =
            AppSettings::try_new_with_backend(&fresh_backend).expect("settings should load");
        deserialized.apply_to(&fresh_settings);

        let restored_labels = fresh_settings.mail_labels();
        assert_eq!(restored_labels.len(), 2);
        assert_eq!(restored_labels[0].name, "Urgent");
        assert_eq!(restored_labels[0].color, "#FF0000");
        assert_eq!(restored_labels[0].tag, "$Labelurgent");
        assert_eq!(restored_labels[1].name, "Project Alpha");
        assert_eq!(restored_labels[1].color, "#00FF00");
        assert_eq!(restored_labels[1].tag, "project_alpha");

        assert!(!fresh_settings.junk_check_incoming());
        assert!(fresh_settings.junk_empty_on_exit());
        assert_eq!(fresh_settings.junk_empty_on_exit_days(), 14);
        assert!(fresh_settings.junk_lookup_addressbook());
        assert!(fresh_settings.junk_lookup_addressbook_local_only());
        assert_eq!(fresh_settings.junk_default_plugin(), "SpamAssassin");
        assert_eq!(fresh_settings.junk_custom_header(), "X-Spam-Flag");
        assert_eq!(fresh_settings.junk_custom_header_value(), "YES");
    }

    #[test]
    fn composer_settings_backup_validation_and_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings = AppSettings::try_new_with_backend(&backend).expect("settings should load");

        settings.set_composer_show_from(false);
        settings.set_composer_show_cc(false);
        settings.set_composer_show_bcc(true);
        settings.set_composer_show_reply_to(true);
        settings.set_composer_prompt_empty_subject(false);
        settings.set_composer_prompt_only_bcc(false);
        settings.set_composer_inline_spelling(false);
        settings.set_composer_reply_style("inline");
        settings.set_composer_forward_style("quoted");
        settings.set_composer_top_signature(true);
        settings.set_composer_signature_outline(false);

        let backup = BackupSettings::from_app(&settings);
        assert!(!backup.composer_show_from);
        assert!(!backup.composer_show_cc);
        assert!(backup.composer_show_bcc);
        assert!(backup.composer_show_reply_to);
        assert!(!backup.composer_prompt_empty_subject);
        assert!(!backup.composer_prompt_only_bcc);
        assert!(!backup.composer_inline_spelling);
        assert_eq!(backup.composer_reply_style, "inline");
        assert_eq!(backup.composer_forward_style, "quoted");
        assert!(backup.composer_top_signature);
        assert!(!backup.composer_signature_outline);

        backup.validate().expect("backup should validate");

        // Validate rejects invalid styles
        let mut invalid_reply = backup.clone();
        invalid_reply.composer_reply_style = "invalid_style".to_string();
        assert!(invalid_reply.validate().is_err());

        let mut invalid_forward = backup.clone();
        invalid_forward.composer_forward_style = "invalid_style".to_string();
        assert!(invalid_forward.validate().is_err());

        // JSON serialization round-trip
        let serialized = serde_json::to_string(&backup).expect("should serialize");
        let deserialized: BackupSettings =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert!(!deserialized.composer_show_from);
        assert!(!deserialized.composer_show_cc);
        assert!(deserialized.composer_show_bcc);
        assert!(deserialized.composer_show_reply_to);
        assert!(!deserialized.composer_prompt_empty_subject);
        assert!(!deserialized.composer_prompt_only_bcc);
        assert!(!deserialized.composer_inline_spelling);
        assert_eq!(deserialized.composer_reply_style, "inline");
        assert_eq!(deserialized.composer_forward_style, "quoted");
        assert!(deserialized.composer_top_signature);
        assert!(!deserialized.composer_signature_outline);

        // Restoration into fresh settings
        let fresh_backend = gio::memory_settings_backend_new();
        let fresh_settings =
            AppSettings::try_new_with_backend(&fresh_backend).expect("settings should load");
        deserialized.apply_to(&fresh_settings);

        assert!(!fresh_settings.composer_show_from());
        assert!(!fresh_settings.composer_show_cc());
        assert!(fresh_settings.composer_show_bcc());
        assert!(fresh_settings.composer_show_reply_to());
        assert!(!fresh_settings.composer_prompt_empty_subject());
        assert!(!fresh_settings.composer_prompt_only_bcc());
        assert!(!fresh_settings.composer_inline_spelling());
        assert_eq!(fresh_settings.composer_reply_style(), "inline");
        assert_eq!(fresh_settings.composer_forward_style(), "quoted");
        assert!(fresh_settings.composer_top_signature());
        assert!(!fresh_settings.composer_signature_outline());
    }

    #[test]
    fn mail_reading_policies_backup_validation_and_roundtrip() {
        let backend = gio::memory_settings_backend_new();
        let settings =
            AppSettings::try_new_with_backend(&backend).expect("settings should load");

        settings.set_mail_image_loading_policy("sometimes");
        settings.set_mail_mark_seen(false);
        settings.set_mail_mark_seen_timeout(5000);
        settings.set_mail_caret_mode(true);
        settings.set_mail_magic_spacebar(false);
        settings.set_mail_citation_highlighting(false);
        settings.set_mail_citation_color_1("#112233");
        settings.set_mail_citation_color_2("#223344");
        settings.set_mail_citation_color_3("#334455");
        settings.set_mail_citation_color_4("#445566");
        settings.set_mail_citation_color_5("#556677");
        settings.set_mail_headers_collapsed(true);
        settings.set_mail_show_real_date(false);

        let backup = BackupSettings::from_app(&settings);
        assert_eq!(backup.mail_image_loading_policy, "sometimes");
        assert!(!backup.mail_mark_seen);
        assert_eq!(backup.mail_mark_seen_timeout, 5000);
        assert!(backup.mail_caret_mode);
        assert!(!backup.mail_magic_spacebar);
        assert!(!backup.mail_citation_highlighting);
        assert_eq!(backup.mail_citation_color_1, "#112233");
        assert_eq!(backup.mail_citation_color_2, "#223344");
        assert_eq!(backup.mail_citation_color_3, "#334455");
        assert_eq!(backup.mail_citation_color_4, "#445566");
        assert_eq!(backup.mail_citation_color_5, "#556677");
        assert!(backup.mail_headers_collapsed);
        assert!(!backup.mail_show_real_date);

        backup.validate().expect("backup should validate");

        // Validate rejects invalid image loading policy
        let mut invalid_policy = backup.clone();
        invalid_policy.mail_image_loading_policy = "unsupported".to_string();
        assert!(invalid_policy.validate().is_err());

        // Validate rejects invalid timeout
        let mut invalid_timeout = backup.clone();
        invalid_timeout.mail_mark_seen_timeout = 70_000;
        assert!(invalid_timeout.validate().is_err());

        // Validate rejects invalid color
        let mut invalid_color = backup.clone();
        invalid_color.mail_citation_color_1 = "not-a-color".to_string();
        assert!(invalid_color.validate().is_err());

        // JSON serialization round-trip
        let serialized = serde_json::to_string(&backup).expect("should serialize");
        let deserialized: BackupSettings =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert_eq!(deserialized.mail_image_loading_policy, "sometimes");
        assert!(!deserialized.mail_mark_seen);
        assert_eq!(deserialized.mail_mark_seen_timeout, 5000);
        assert!(deserialized.mail_caret_mode);
        assert!(!deserialized.mail_magic_spacebar);
        assert!(!deserialized.mail_citation_highlighting);
        assert_eq!(deserialized.mail_citation_color_1, "#112233");
        assert_eq!(deserialized.mail_citation_color_2, "#223344");
        assert_eq!(deserialized.mail_citation_color_3, "#334455");
        assert_eq!(deserialized.mail_citation_color_4, "#445566");
        assert_eq!(deserialized.mail_citation_color_5, "#556677");
        assert!(deserialized.mail_headers_collapsed);
        assert!(!deserialized.mail_show_real_date);

        // Restoration into fresh settings
        let fresh_backend = gio::memory_settings_backend_new();
        let fresh_settings =
            AppSettings::try_new_with_backend(&fresh_backend).expect("settings should load");
        deserialized.apply_to(&fresh_settings);

        assert_eq!(fresh_settings.mail_image_loading_policy(), "sometimes");
        assert!(!fresh_settings.mail_mark_seen());
        assert_eq!(fresh_settings.mail_mark_seen_timeout(), 5000);
        assert!(fresh_settings.mail_caret_mode());
        assert!(!fresh_settings.mail_magic_spacebar());
        assert!(!fresh_settings.mail_citation_highlighting());
        assert_eq!(fresh_settings.mail_citation_color_1(), "#112233");
        assert_eq!(fresh_settings.mail_citation_color_2(), "#223344");
        assert_eq!(fresh_settings.mail_citation_color_3(), "#334455");
        assert_eq!(fresh_settings.mail_citation_color_4(), "#445566");
        assert_eq!(fresh_settings.mail_citation_color_5(), "#556677");
        assert!(fresh_settings.mail_headers_collapsed());
        assert!(!fresh_settings.mail_show_real_date());
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
