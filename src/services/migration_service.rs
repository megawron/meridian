// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable Evolution profile migration.
//
// Evolution stores ESource definitions as key files.  This importer reads
// only non-secret identity and endpoint metadata and converts mail sources
// into the app-owned account profile format.  It never writes to the EDS
// registry and deliberately ignores password, token, credential, and private
// key fields.  Creating/updating the real EDS sources remains a Linux-side
// integration task documented in ROADMAP_LINUX.md.

use super::account_config_service::{AccountConfigStore, AccountProfile, AccountProtocol};
use super::calendar_store_service::CalendarStoreService;
use super::contact_store_service::ContactStoreService;
use super::eds_service::{EdsSource, EdsSourceKind};
use super::eds_source_cache_service::EdsSourceCache;
use super::html_sanitizer::sanitize_html;
use super::mail_filter_service::{
    FilterAction, FilterCondition, FilterField, FilterOperator, FilterScope, MailFilterRule,
    MailFilterStore,
};
use super::mail_store_service::MailStoreService;
use super::settings_service::{
    MAX_TASK_CATEGORY_FILTER_BYTES, normalize_composer_charset,
    normalize_memo_preview_orientation_value, normalize_memo_search_mode_value,
    normalize_memo_source_filter_value, normalize_task_preview_orientation_value,
    parse_mail_normal_headers_value, validate_memo_category_filter_value,
};
use super::signature_store_service::{SignatureProfile, SignatureStore};
use glib::prelude::StaticVariantType;
use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub discovered_sources: usize,
    pub imported_accounts: usize,
    pub cached_mail_sources: usize,
    pub cached_calendar_sources: usize,
    pub cached_address_book_sources: usize,
    pub skipped_sources: usize,
    pub ignored_secret_fields: usize,
    pub discovered_filter_files: usize,
    pub imported_filters: usize,
    pub imported_signatures: usize,
    pub discovered_calendar_files: usize,
    pub imported_calendar_files: usize,
    pub discovered_contact_files: usize,
    pub imported_contact_files: usize,
    pub discovered_mail_files: usize,
    pub imported_mail_files: usize,
    pub imported_mail_folders: usize,
    pub discovered_settings_files: usize,
    pub imported_settings: usize,
    /// Normalized non-secret settings waiting to be applied by the GTK main
    /// thread after the profile scan finishes.
    pub settings: Vec<ImportedSetting>,
    pub signature_text: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSetting {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalDataMigrationReport {
    pub discovered_calendar_files: usize,
    pub imported_calendar_files: usize,
    pub discovered_contact_files: usize,
    pub imported_contact_files: usize,
    pub discovered_mail_files: usize,
    pub imported_mail_files: usize,
    pub imported_mail_folders: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct SourceKeyFile {
    groups: BTreeMap<String, BTreeMap<String, String>>,
    ignored_secret_keys: Vec<String>,
    warnings: Vec<String>,
}

impl SourceKeyFile {
    fn get(&self, groups: &[&str], keys: &[&str]) -> Option<String> {
        groups.iter().find_map(|group| {
            let entries = self
                .groups
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(group))
                .map(|(_, entries)| entries)?;
            keys.iter().find_map(|key| {
                entries
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(key))
                    .map(|(_, value)| value.clone())
            })
        })
    }

    fn has_group(&self, group: &str) -> bool {
        self.groups
            .keys()
            .any(|name| name.eq_ignore_ascii_case(group))
    }

    fn kinds(&self) -> Vec<EdsSourceKind> {
        let mut kinds = Vec::new();
        if self.has_group("Mail Account")
            || self.has_group("Mail Identity")
            || self.has_group("Mail Submission")
        {
            kinds.push(EdsSourceKind::Mail);
        }
        if self.has_group("Calendar") {
            kinds.push(EdsSourceKind::Calendar);
        }
        if self.has_group("Address Book") {
            kinds.push(EdsSourceKind::AddressBook);
        }
        kinds
    }
}

/// Import non-secret mail metadata from Evolution source files below root.
///
/// The source directory may be an extracted copy of an Evolution profile.  It
/// is intentionally supplied by the user instead of guessed automatically so
/// that this portable importer cannot accidentally modify or scan a live EDS
/// profile without an explicit action.
#[allow(dead_code)]
pub fn import_evolution_sources(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
) -> io::Result<MigrationReport> {
    let filter_store = MailFilterStore::default();
    let signature_store = SignatureStore::default();
    import_evolution_sources_with_stores(root, store, &filter_store, &signature_store)
}

/// Import source metadata and the portable subset of Evolution's filter XML.
///
/// The explicit filter store parameter keeps this operation deterministic in
/// tests and lets callers choose the app-owned destination.  The public
/// convenience function above uses the normal app filter store.
#[allow(dead_code)]
pub fn import_evolution_sources_with_filter_store(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
    filter_store: &MailFilterStore,
) -> io::Result<MigrationReport> {
    let signature_store = SignatureStore::default();
    import_evolution_sources_with_stores(root, store, filter_store, &signature_store)
}

/// Import Evolution source metadata and update the portable, non-secret EDS
/// source cache.  The cache is only used as an offline display fallback; it
/// never becomes authoritative for EDS mutations or mail/calendar data.
pub fn import_evolution_sources_with_cache(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
    cache: &EdsSourceCache,
) -> io::Result<MigrationReport> {
    let filter_store = MailFilterStore::default();
    let signature_store = SignatureStore::default();
    import_evolution_sources_with_stores_and_cache(
        root,
        store,
        &filter_store,
        &signature_store,
        cache,
    )
}

/// Import Evolution's non-secret profile metadata and copy valid local
/// iCalendar/vCard files into explicitly supplied app-owned stores.
///
/// The destination services should point at the app-owned stores.  This
/// function never writes to the selected Evolution profile or to an external
/// read-only import directory; creating real EDS sources and migrating Camel
/// mail stores remain Linux-side work.
pub fn import_evolution_profile_with_cache_and_local_data(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
    cache: &EdsSourceCache,
    calendar_store: &CalendarStoreService,
    contact_store: &ContactStoreService,
    mail_store: &MailStoreService,
) -> io::Result<MigrationReport> {
    let root = root.as_ref();
    let mut report = import_evolution_sources_with_cache(root, store, cache)?;
    let local = import_evolution_local_data(root, calendar_store, contact_store, mail_store)?;
    report.discovered_calendar_files = local.discovered_calendar_files;
    report.imported_calendar_files = local.imported_calendar_files;
    report.discovered_contact_files = local.discovered_contact_files;
    report.imported_contact_files = local.imported_contact_files;
    report.discovered_mail_files = local.discovered_mail_files;
    report.imported_mail_files = local.imported_mail_files;
    report.imported_mail_folders = local.imported_mail_folders;
    report.warnings.extend(local.warnings);
    Ok(report)
}

/// Discover and copy valid local mail, iCalendar and vCard files from an
/// explicitly selected Evolution profile copy.  The source files remain
/// untouched and destinations use each store's non-overwriting import
/// behavior.
pub fn import_evolution_local_data(
    root: impl AsRef<Path>,
    calendar_store: &CalendarStoreService,
    contact_store: &ContactStoreService,
    mail_store: &MailStoreService,
) -> io::Result<LocalDataMigrationReport> {
    let root = root.as_ref();
    let mut candidates = Vec::new();
    collect_local_data_files(root, &mut candidates)?;

    let mut calendar_files = Vec::new();
    let mut contact_files = Vec::new();
    let mut mail_files = Vec::new();
    let mut mail_layout_paths = Vec::new();
    let mut warnings = Vec::new();
    for path in candidates {
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) => {
                warnings.push(format!(
                    "{}: lokale Profildatei konnte nicht gelesen werden: {error}",
                    migration_source_label(&path)
                ));
                continue;
            }
        };
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(extension.as_str(), "ics" | "vcs")
            && raw
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case("BEGIN:VCALENDAR"))
        {
            calendar_files.push(path);
        } else if matches!(extension.as_str(), "vcf" | "vcard")
            && raw
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case("BEGIN:VCARD"))
        {
            contact_files.push(path);
        }
    }

    collect_local_mail_files(root, &mut mail_files)?;
    collect_local_mail_layout_paths(root, &mut mail_layout_paths)?;

    let discovered_calendar_files = calendar_files.len();
    let discovered_contact_files = contact_files.len();
    let imported_calendar_files = if discovered_calendar_files == 0 {
        0
    } else if !calendar_store.is_writable() {
        warnings.push(
            "Lokale iCalendar-Dateien wurden nicht übernommen: Zielkalender ist schreibgeschützt"
                .to_string(),
        );
        0
    } else {
        calendar_store.import_files(&calendar_files)?
    };
    let imported_contact_files = if discovered_contact_files == 0 {
        0
    } else if !contact_store.is_writable() {
        warnings.push(
            "Lokale vCard-Dateien wurden nicht übernommen: Zieladressbuch ist schreibgeschützt"
                .to_string(),
        );
        0
    } else {
        contact_store.import_files(&contact_files)?
    };
    let discovered_mail_files = mail_files.len();
    let imported_mail_files = if discovered_mail_files == 0 {
        0
    } else {
        mail_store.import_files_from_root(root, &mail_files)?
    };
    let imported_mail_folders = if mail_layout_paths.is_empty() {
        0
    } else {
        mail_store.register_folder_layout_from_root(root, &mail_layout_paths)?
    };

    Ok(LocalDataMigrationReport {
        discovered_calendar_files,
        imported_calendar_files,
        discovered_contact_files,
        imported_contact_files,
        discovered_mail_files,
        imported_mail_files,
        imported_mail_folders,
        warnings,
    })
}

/// Import source metadata, portable filters, and all safe text signatures into
/// explicitly supplied app-owned stores.
#[allow(dead_code)]
pub fn import_evolution_sources_with_stores(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
    filter_store: &MailFilterStore,
    signature_store: &SignatureStore,
) -> io::Result<MigrationReport> {
    import_evolution_sources_inner(root, store, filter_store, signature_store, None)
}

/// Import source metadata, portable filters, signatures, and non-secret EDS
/// source metadata into explicitly supplied stores.
pub fn import_evolution_sources_with_stores_and_cache(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
    filter_store: &MailFilterStore,
    signature_store: &SignatureStore,
    cache: &EdsSourceCache,
) -> io::Result<MigrationReport> {
    import_evolution_sources_inner(root, store, filter_store, signature_store, Some(cache))
}

fn import_evolution_sources_inner(
    root: impl AsRef<Path>,
    store: &AccountConfigStore,
    filter_store: &MailFilterStore,
    signature_store: &SignatureStore,
    cache: Option<&EdsSourceCache>,
) -> io::Result<MigrationReport> {
    let root = root.as_ref();
    let mut paths = Vec::new();
    collect_source_files(root, &mut paths)?;
    paths.sort();

    let mut report = MigrationReport::default();
    let mut cached_mail_sources = Vec::new();
    let mut cached_calendar_sources = Vec::new();
    let mut cached_address_book_sources = Vec::new();
    for path in paths {
        report.discovered_sources += 1;
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) => {
                report.skipped_sources += 1;
                report.warnings.push(format!(
                    "{}: Datei konnte nicht gelesen werden: {error}",
                    migration_source_label(&path)
                ));
                continue;
            }
        };
        let source = match parse_source(&raw) {
            Ok(source) => source,
            Err(error) => {
                report.skipped_sources += 1;
                report
                    .warnings
                    .push(format!("{}: {error}", migration_source_label(&path)));
                continue;
            }
        };
        report.ignored_secret_fields += source.ignored_secret_keys.len();
        report.warnings.extend(
            source
                .warnings
                .iter()
                .map(|warning| format!("{}: {warning}", migration_source_label(&path))),
        );
        for (kind, eds_source) in eds_sources_from_keyfile(&path, &source) {
            match kind {
                EdsSourceKind::Mail => cached_mail_sources.push(eds_source),
                EdsSourceKind::Calendar => cached_calendar_sources.push(eds_source),
                EdsSourceKind::AddressBook => cached_address_book_sources.push(eds_source),
            }
        }

        match profile_from_source_with_warnings(&path, &source) {
            Ok((profile, warnings)) => {
                report.warnings.extend(
                    warnings
                        .into_iter()
                        .map(|warning| format!("{}: {warning}", migration_source_label(&path))),
                );
                store.upsert(profile).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "Profil aus {} konnte nicht gespeichert werden: {error}",
                            migration_source_label(&path)
                        ),
                    )
                })?;
                report.imported_accounts += 1;
            }
            Err(reason) => {
                report.skipped_sources += 1;
                report
                    .warnings
                    .push(format!("{}: {reason}", migration_source_label(&path)));
            }
        }
    }

    normalize_cached_sources(&mut cached_mail_sources);
    normalize_cached_sources(&mut cached_calendar_sources);
    normalize_cached_sources(&mut cached_address_book_sources);
    report.cached_mail_sources = cached_mail_sources.len();
    report.cached_calendar_sources = cached_calendar_sources.len();
    report.cached_address_book_sources = cached_address_book_sources.len();
    if let Some(cache) = cache {
        save_imported_sources(
            cache,
            EdsSourceKind::Mail,
            &cached_mail_sources,
            &mut report.warnings,
        );
        save_imported_sources(
            cache,
            EdsSourceKind::Calendar,
            &cached_calendar_sources,
            &mut report.warnings,
        );
        save_imported_sources(
            cache,
            EdsSourceKind::AddressBook,
            &cached_address_book_sources,
            &mut report.warnings,
        );
    }

    let mut settings_paths = Vec::new();
    collect_evolution_settings_files(root, &mut settings_paths)?;
    settings_paths.sort();
    settings_paths.dedup();
    report.discovered_settings_files = settings_paths.len();
    let mut imported_settings = BTreeMap::<String, String>::new();
    for path in settings_paths {
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) => {
                report.warnings.push(format!(
                    "{}: Einstellungsdatei konnte nicht gelesen werden: {error}",
                    migration_source_label(&path)
                ));
                continue;
            }
        };
        let parsed = parse_evolution_settings(&raw, &path);
        report.ignored_secret_fields += parsed.ignored_secret_fields;
        report.warnings.extend(parsed.warnings);
        for setting in parsed.settings {
            if imported_settings
                .insert(setting.key.clone(), setting.value)
                .is_some()
            {
                report.warnings.push(format!(
                    "{}: doppelter Einstellungswert für „{}“ wurde durch den späteren Eintrag ersetzt",
                    migration_source_label(&path),
                    setting.key
                ));
            }
        }
    }
    report.settings = imported_settings
        .into_iter()
        .map(|(key, value)| ImportedSetting { key, value })
        .collect();
    report.imported_settings = report.settings.len();

    let mut filter_paths = Vec::new();
    collect_named_files(root, "filters.xml", &mut filter_paths)?;
    filter_paths.sort();
    report.discovered_filter_files = filter_paths.len();
    for path in filter_paths {
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) => {
                report.warnings.push(format!(
                    "{}: Filterdatei konnte nicht gelesen werden: {error}",
                    migration_source_label(&path)
                ));
                continue;
            }
        };
        let rules = match parse_evolution_filters(&raw, &path) {
            Ok(rules) => rules,
            Err(reason) => {
                report.warnings.push(format!(
                    "{}: Filter konnten nicht importiert werden: {reason}",
                    migration_source_label(&path)
                ));
                continue;
            }
        };
        report
            .warnings
            .extend(unsupported_filter_warnings(&raw, &path));
        for rule in rules {
            match filter_store.upsert(rule) {
                Ok(()) => report.imported_filters += 1,
                Err(error) => report.warnings.push(format!(
                    "{}: Filter konnte nicht gespeichert werden: {error}",
                    migration_source_label(&path)
                )),
            }
        }
    }

    let mut signature_paths = Vec::new();
    collect_named_files(root, "signature-list.xml", &mut signature_paths)?;
    for list_path in signature_paths {
        let raw = match fs::read_to_string(&list_path) {
            Ok(raw) => raw,
            Err(error) => {
                report.warnings.push(format!(
                    "{}: Signaturliste konnte nicht gelesen werden: {error}",
                    migration_source_label(&list_path)
                ));
                continue;
            }
        };
        for (signature_index, (signature_tag, signature_body)) in
            extract_xml_blocks(&raw, "signature")
                .into_iter()
                .enumerate()
        {
            let Some(relative_path) = attribute_value(&signature_tag, "filename")
                .or_else(|| attribute_value(&signature_tag, "file"))
                .or_else(|| attribute_value(&signature_tag, "path"))
                .or_else(|| first_xml_text(&signature_body, "filename"))
                .or_else(|| first_xml_text(&signature_body, "file"))
                .or_else(|| first_xml_text(&signature_body, "path"))
            else {
                continue;
            };
            let Some(signature_path) =
                safe_profile_path(root, list_path.parent().unwrap_or(root), &relative_path)
            else {
                report.warnings.push(format!(
                    "{}: Unsicherer Signaturpfad wurde ignoriert",
                    migration_source_label(&list_path)
                ));
                continue;
            };
            let contents = match fs::read_to_string(&signature_path) {
                Ok(contents) => contents,
                Err(error) => {
                    report.warnings.push(format!(
                        "{}: Signatur konnte nicht gelesen werden: {error}",
                        migration_source_label(&signature_path)
                    ));
                    continue;
                }
            };
            let format = attribute_value(&signature_tag, "format")
                .or_else(|| first_xml_text(&signature_body, "format"))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let text = if format.contains("html") {
                // Preserve formatting for the portable composer, but never
                // import active content from a profile file.  The composer
                // sanitizes the selected profile again when it is inserted.
                sanitize_html(&contents)
            } else {
                contents
            };
            if text.trim().is_empty() {
                continue;
            }
            let name = attribute_value(&signature_tag, "name")
                .or_else(|| first_xml_text(&signature_body, "name"))
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| {
                    format!("Importierte Evolution-Signatur {}", signature_index + 1)
                });
            let sig_key = format!(
                "{}:{}:{}:{}",
                list_path.to_string_lossy(),
                signature_index,
                relative_path,
                name
            );
            let mut hasher: u64 = 0xcbf29ce484222325;
            for byte in sig_key.as_bytes() {
                hasher ^= *byte as u64;
                hasher = hasher.wrapping_mul(0x100000001b3);
            }
            let signature = SignatureProfile {
                id: format!("evolution-signature-{:016x}", hasher),
                name,
                text: text.trim_end().to_string(),
                format: if format.is_empty() {
                    "text/plain".to_string()
                } else {
                    format
                },
            };
            match signature_store.upsert(signature.clone()) {
                Ok(()) => {
                    report.imported_signatures += 1;
                    if report.signature_text.is_none() {
                        report.signature_text = Some(signature.text);
                    }
                }
                Err(error) => report.warnings.push(format!(
                    "{}: Signatur konnte nicht gespeichert werden: {error}",
                    migration_source_label(&signature_path)
                )),
            }
        }
    }

    Ok(report)
}

fn collect_source_files(root: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Der ausgewählte Evolution-Profilpfad ist kein Ordner",
        ));
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_source_files(&path, result)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("source"))
        {
            result.push(path);
        }
    }
    Ok(())
}

fn eds_sources_from_keyfile(
    path: &Path,
    source: &SourceKeyFile,
) -> Vec<(EdsSourceKind, EdsSource)> {
    let kinds = source.kinds();
    if kinds.is_empty() {
        return Vec::new();
    }
    let uid = first_value(
        source,
        &[
            "Data Source",
            "Source",
            "Mail Account",
            "Calendar",
            "Address Book",
        ],
        &["UID", "Uid", "Id", "ID"],
    )
    .or_else(|| {
        path.file_stem()
            .map(|value| value.to_string_lossy().into_owned())
    });
    let Some(uid) = uid else {
        return Vec::new();
    };
    let display_name = first_value(
        source,
        &[
            "Data Source",
            "Mail Account",
            "Calendar",
            "Address Book",
            "Mail Identity",
            "Identity",
        ],
        &["DisplayName", "Name", "Label"],
    )
    .unwrap_or_else(|| uid.clone());
    let enabled = first_value(source, &["Data Source", "Source"], &["Enabled", "Active"])
        .map(|value| parse_bool(&value))
        .unwrap_or(true);

    kinds
        .into_iter()
        .map(|kind| {
            let section = match kind {
                EdsSourceKind::Mail => "Mail Account",
                EdsSourceKind::Calendar => "Calendar",
                EdsSourceKind::AddressBook => "Address Book",
            };
            let backend_name = first_value(
                source,
                &[section, "Data Source", "Source", "Backend", "Camel"],
                &["BackendName", "Backend", "BackendType", "Type"],
            )
            .unwrap_or_default();
            (
                kind,
                EdsSource {
                    uid: uid.clone(),
                    display_name: display_name.clone(),
                    backend_name,
                    enabled,
                    writable: true,
                    parent_name: None,
                    color: None,
                    foreign_mail: None,
                },
            )
        })
        .collect()
}

fn normalize_cached_sources(sources: &mut Vec<EdsSource>) {
    sources.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.uid.cmp(&right.uid))
    });
    sources.dedup_by(|left, right| left.uid == right.uid);
}

fn save_imported_sources(
    cache: &EdsSourceCache,
    kind: EdsSourceKind,
    sources: &[EdsSource],
    warnings: &mut Vec<String>,
) {
    if sources.is_empty() {
        return;
    }
    if let Err(error) = cache.save(kind, sources) {
        warnings.push(format!(
            "{} EDS-Quellen konnten nicht im Offline-Cache gespeichert werden: {error}",
            match kind {
                EdsSourceKind::Mail => "Mail",
                EdsSourceKind::Calendar => "Kalender",
                EdsSourceKind::AddressBook => "Adressbuch",
            }
        ));
    }
}

#[derive(Debug, Default)]
struct ParsedEvolutionSettings {
    settings: Vec<ImportedSetting>,
    ignored_secret_fields: usize,
    warnings: Vec<String>,
}

/// Find an explicitly exported Evolution settings file without treating
/// arbitrary profile data as configuration.  Evolution's GSettings values
/// normally live in dconf rather than beside the `.source` files, so the
/// importer accepts the common `dconf dump`/INI export names as well.
fn collect_evolution_settings_files(root: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_evolution_settings_files(&path, result)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let known_name = matches!(
            name.as_str(),
            "settings.ini"
                | "evolution-settings.ini"
                | "evolution-settings.conf"
                | "gsettings.ini"
                | "gsettings.dump"
                | "dconf.dump"
                | "evolution.dconf"
                | "evolution.gsettings"
        );
        let named_export = name.starts_with("evolution")
            && matches!(extension.as_str(), "ini" | "conf" | "dconf" | "dump");
        if known_name || named_export {
            result.push(path);
        }
    }
    Ok(())
}

fn parse_evolution_settings(raw: &str, path: &Path) -> ParsedEvolutionSettings {
    let mut result = ParsedEvolutionSettings::default();
    let mut group = String::new();
    for (line_index, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                result.warnings.push(format!(
                    "{}: unvollständiger Abschnitt in Zeile {} wurde ignoriert",
                    migration_source_label(path),
                    line_index + 1
                ));
                group.clear();
                continue;
            }
            group = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=').or_else(|| line.split_once(':'))
        else {
            result.warnings.push(format!(
                "{}: unvollständige Einstellungszeile {} wurde ignoriert",
                migration_source_label(path),
                line_index + 1
            ));
            continue;
        };
        let key = normalize_setting_key(raw_key);
        if key.is_empty() {
            continue;
        }
        if is_secret_setting_key(&key) {
            result.ignored_secret_fields += 1;
            continue;
        }
        let (canonical_key, value) = match canonical_setting(&group, &key, raw_value) {
            Ok(Some(setting)) => setting,
            Ok(None) => {
                result.warnings.push(format!(
                    "{}: unbekannte oder nicht unterstützte Einstellung „{}“ wurde nicht übernommen",
                    migration_source_label(path),
                    safe_warning_key(&key)
                ));
                continue;
            }
            Err(reason) => {
                result.warnings.push(format!(
                    "{}: Wert für Einstellung „{}“ ist {} und wurde nicht übernommen",
                    migration_source_label(path),
                    safe_warning_key(&key),
                    reason
                ));
                continue;
            }
        };
        let value = match normalize_imported_setting_value(&canonical_key, &value) {
            Ok(value) => value,
            Err(reason) => {
                result.warnings.push(format!(
                    "{}: Wert für Einstellung „{}“ ist {} und wurde nicht übernommen",
                    migration_source_label(path),
                    safe_warning_key(&canonical_key),
                    reason
                ));
                continue;
            }
        };
        result.settings.push(ImportedSetting {
            key: canonical_key,
            value,
        });
    }
    if result.settings.is_empty() && !raw.trim().is_empty() {
        result.warnings.push(format!(
            "{}: keine unterstützten nicht-geheimen Evolution-Einstellungen gefunden",
            migration_source_label(path)
        ));
    }
    result
}

/// Diagnostics must identify the import source without disclosing the
/// user-selected absolute profile path.  Relative setting/filter keys remain
/// useful context and are already passed through the safe label helpers.
fn migration_source_label(_path: &Path) -> &'static str {
    "Evolution-Profil"
}

fn normalize_setting_key(value: &str) -> String {
    value
        .trim()
        .trim_matches(['"', '\''])
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .replace('_', "-")
        .to_ascii_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingValueType {
    String,
    Boolean,
    Int32,
    Uint32,
    StringArray,
    HeaderArray,
}

fn canonical_setting(
    group: &str,
    key: &str,
    raw_value: &str,
) -> Result<Option<(String, String)>, &'static str> {
    let group = group.to_ascii_lowercase();
    let canonical = match key {
        "show-preview" if group.contains("addressbook") || group.contains("contact") => {
            "contact-preview-visible"
        }
        "show-preview" if group.contains("memo") => "memo-preview-visible",
        "show-preview" if group.contains("calendar") || group.contains("task") => {
            "task-preview-visible"
        }
        "show-preview" | "message-preview" | "show-message-preview" => "show-message-preview",
        "show-headers" => "mail-normal-headers",
        "show-week-numbers" => "calendar-show-week-numbers",
        "show-tag-vpane" => "calendar-show-tag-vpane",
        "tag-vpane-position" => "calendar-tag-vpane-position",
        "week-start" | "first-weekday" => "calendar-week-start",
        "time-divisions" => "calendar-time-divisions",
        "compress-weekend" => "calendar-compress-weekend",
        "month-scroll-by-week" => "calendar-month-scroll-by-week",
        "day-second-zone" | "second-zone" => "calendar-second-zone",
        "day-second-zones" | "second-zones" => "calendar-second-zones",
        "default-calendar" => "default-calendar-source",
        "default-address-book" => "default-address-book-source",
        "default-task-list" => "default-task-source",
        "default-memo-list" => "default-memo-source",
        "task-hidden-sources" | "tasks-hidden-sources" => "task-hidden-sources",
        "memo-hidden-sources" | "memos-hidden-sources" => "memo-hidden-sources",
        "addressbook-hidden-sources" | "contacts-hidden-sources" => "addressbook-hidden-sources",
        "calendar-hidden-groups" => "calendar-hidden-groups",
        "calendar-groups-order" => "calendar-groups-order",
        "task-list-hidden-groups"
        | "task list-hidden-groups"
        | "tasks-hidden-groups"
        | "task-hidden-groups" => "task-hidden-groups",
        "task-list-groups-order"
        | "task list-groups-order"
        | "tasks-groups-order"
        | "task-groups-order" => "task-groups-order",
        "memo-list-hidden-groups"
        | "memo list-hidden-groups"
        | "memos-hidden-groups"
        | "memo-hidden-groups" => "memo-hidden-groups",
        "memo-list-groups-order"
        | "memo list-groups-order"
        | "memos-groups-order"
        | "memo-groups-order" => "memo-groups-order",
        "address-book-hidden-groups"
        | "address book-hidden-groups"
        | "contacts-hidden-groups"
        | "addressbook-hidden-groups" => "addressbook-hidden-groups",
        "address-book-groups-order"
        | "address book-groups-order"
        | "contacts-groups-order"
        | "addressbook-groups-order" => "addressbook-groups-order",
        "use-default-reminder" | "default-reminder" => "use-default-reminder",
        "default-reminder-interval" => "default-reminder-interval",
        "default-reminder-units" => "default-reminder-units",
        "use-ba-reminder" | "ba-reminder" => "use-ba-reminder",
        "ba-reminder-interval" => "ba-reminder-interval",
        "ba-reminder-units" => "ba-reminder-units",
        "hide-completed-tasks" | "hide-completed" => "hide-completed-tasks",
        "hide-completed-tasks-value" | "hide-completed-value" => "hide-completed-tasks-value",
        "hide-completed-tasks-units" | "hide-completed-units" => "hide-completed-tasks-units",
        "hide-cancelled-tasks" => "hide-cancelled-tasks",
        "hide-cancelled-events" => "hide-cancelled-events",
        "primary-addressbook" => "default-address-book-source",
        "address-formatting" => "contact-address-formatting",
        "completion-minimum-query-length" => "contact-completion-minimum-query-length",
        "completion-show-address" => "contact-completion-show-address",
        "completion-book-sources" | "completion-sources" => "contact-completion-book-sources",
        "preview-show-maps" => "contact-preview-show-maps",
        "preview-home-before-work" => "contact-preview-home-before-work",
        "open-map-target" => "contact-open-map-target",
        "open-map-prefer-local" => "contact-open-map-prefer-local",
        "layout" if group.contains("addressbook") || group.contains("contact") => "contact-layout",
        "hpane-position" if group.contains("addressbook") || group.contains("contact") => {
            "contact-hpane-position"
        }
        "vpane-position" if group.contains("addressbook") || group.contains("contact") => {
            "contact-vpane-position"
        }
        "editor-show-contact-phone" | "editor-show-phone" => "contact-editor-show-phone",
        "editor-show-contact-sip" | "editor-show-sip" => "contact-editor-show-sip",
        "editor-show-contact-im" | "editor-show-im" => "contact-editor-show-im",
        "editor-show-mailing-home" | "editor-show-home-address" => {
            "contact-editor-show-home-address"
        }
        "editor-show-mailing-work" | "editor-show-work-address" => {
            "contact-editor-show-work-address"
        }
        "editor-show-mailing-other" | "editor-show-other-address" => {
            "contact-editor-show-other-address"
        }
        "editor-show-personal-web" | "editor-show-web" => "contact-editor-show-web",
        "editor-show-personal-job" | "editor-show-job" => "contact-editor-show-job",
        "editor-show-personal-dates" | "editor-show-dates" => "contact-editor-show-dates",
        "editor-show-personal-misc" | "editor-show-misc" => "contact-editor-show-misc",
        "editor-show-notes" => "contact-editor-show-notes",
        "editor-show-certs" => "contact-editor-show-certs",
        "use-12-hour-format" | "use-12-hour-clock" => "time-format",
        "show-toolbar" if group.contains("mail") => "mail-show-toolbar",
        "show-toolbar" if group.contains("addressbook") || group.contains("contact") => "contact-show-toolbar",
        "show-toolbar" if group.contains("task") => "task-show-toolbar",
        "show-toolbar" if group.contains("memo") => "memo-show-toolbar",
        "show-sidebar" if group.contains("mail") => "mail-show-sidebar",
        "show-from" | "show-sender" if group.contains("mail") => "mail-show-sender",
        "show-to" | "show-recipient" if group.contains("mail") => "mail-show-recipient",
        "show-date" if group.contains("mail") => "mail-show-date",
        "show-status" if group.contains("mail") => "mail-show-status",
        "show-flagged" | "show-star" | "show-starred" if group.contains("mail") => "mail-show-flagged",
        "show-attachment" | "show-attachments" if group.contains("mail") => "mail-show-attachment",
        "show-labels" | "show-tags" if group.contains("mail") => "mail-show-labels",
        "show-preview-text" if group.contains("mail") => "mail-show-preview-text",
        "show-security-details" if group.contains("mail") => "mail-show-security-details",
        "show-all-headers" if group.contains("mail") => "mail-show-all-headers",
        "statusbar-visible" | "statusbar-visible-sub" if group.contains("shell") => {
            "mail-show-statusbar"
        }
        "layout" if group.contains("mail") => "mail-layout",
        "threading" | "thread-list" if group.contains("mail") => "mail-threading",
        "default-folder" if group.contains("mail") => "mail-default-folder",
        "sort-field" if group.contains("mail") => "mail-sort-field",
        "sort-descending" if group.contains("mail") => "mail-sort-descending",
        "show-task-preview" if group.contains("calendar") => "task-preview-visible",
        "task-layout" if group.contains("calendar") => "task-preview-orientation",
        "offline-mode" => "offline-mode",
        value if is_supported_migration_key(value) => value,
        _ => return Ok(None),
    };
    let value_type = if matches!(key, "use-12-hour-format" | "use-12-hour-clock") {
        SettingValueType::Boolean
    } else {
        migration_value_type(canonical)
    };
    let mut value = decode_setting_value(raw_value, value_type)?;
    if key == "use-12-hour-format" {
        return parse_setting_bool(&value)
            .map(|enabled| {
                (
                    "time-format".to_string(),
                    if enabled { "12h" } else { "24h" }.to_string(),
                )
            })
            .ok_or("kein gültiger boolescher Wert")
            .map(Some);
    }
    if key == "use-12-hour-clock" {
        return parse_setting_bool(&value)
            .map(|enabled| {
                (
                    "time-format".to_string(),
                    if enabled { "12h" } else { "24h" }.to_string(),
                )
            })
            .ok_or("kein gültiger boolescher Wert")
            .map(Some);
    }
    if canonical == "calendar-week-start" {
        value = match value.to_ascii_lowercase().as_str() {
            "0" | "monday" | "mon" => "monday".to_string(),
            "1" | "sunday" | "sun" => "sunday".to_string(),
            _ => value,
        };
    }
    if key == "task-layout" && canonical == "task-preview-orientation" {
        value = match value.to_ascii_lowercase().as_str() {
            "0" | "classic" => "classic".to_string(),
            "1" | "vertical" => "vertical".to_string(),
            _ => return Err("keine unterstützte Aufgaben-Vorschauausrichtung"),
        };
    }
    Ok(Some((canonical.to_string(), value)))
}

fn migration_value_type(key: &str) -> SettingValueType {
    if is_boolean_migration_key(key) {
        return SettingValueType::Boolean;
    }
    if matches!(
        key,
        "window-width"
            | "window-height"
            | "mail-sidebar-position"
            | "mail-modern-preview-position"
            | "mail-classic-preview-position"
            | "mail-sender-column-width"
            | "mail-font-size"
            | "calendar-workday-start"
            | "calendar-workday-end"
            | "calendar-default-reminder"
            | "default-reminder-interval"
            | "ba-reminder-interval"
            | "hide-completed-tasks-value"
            | "calendar-time-divisions"
            | "mail-sync-interval"
            | "mail-sync-message-limit"
            | "draft-autosave-interval"
            | "task-hpane-position"
            | "task-vpane-position"
            | "memo-hpane-position"
            | "memo-vpane-position"
            | "contact-completion-minimum-query-length"
            | "contact-hpane-position"
            | "contact-vpane-position"
    ) {
        return SettingValueType::Int32;
    }
    if matches!(
        key,
        "calendar-hidden-sources"
            | "task-hidden-sources"
            | "memo-hidden-sources"
            | "addressbook-hidden-sources"
            | "calendar-hidden-groups"
            | "calendar-groups-order"
            | "task-hidden-groups"
            | "task-groups-order"
            | "memo-hidden-groups"
            | "memo-groups-order"
            | "addressbook-hidden-groups"
            | "addressbook-groups-order"
            | "calendar-second-zones"
    ) {
        return SettingValueType::StringArray;
    }
    if key == "mail-normal-headers" {
        return SettingValueType::HeaderArray;
    }
    SettingValueType::String
}

fn is_supported_migration_key(key: &str) -> bool {
    matches!(
        key,
        "window-width"
            | "window-height"
            | "is-maximized"
            | "active-view"
            | "default-account"
            | "default-calendar-source"
            | "default-task-source"
            | "default-memo-source"
            | "default-address-book-source"
            | "calendar-view"
            | "calendar-week-start"
            | "calendar-workdays"
            | "calendar-workday-start"
            | "calendar-workday-end"
            | "calendar-default-reminder"
            | "use-default-reminder"
            | "default-reminder-interval"
            | "default-reminder-units"
            | "use-ba-reminder"
            | "ba-reminder-interval"
            | "ba-reminder-units"
            | "hide-completed-tasks"
            | "hide-completed-tasks-value"
            | "hide-completed-tasks-units"
            | "hide-cancelled-tasks"
            | "hide-cancelled-events"
            | "calendar-time-divisions"
            | "calendar-compress-weekend"
            | "calendar-month-scroll-by-week"
            | "calendar-second-zone"
            | "calendar-second-zones"
            | "notifications-enabled"
            | "confirm-destructive-actions"
            | "calendar-show-week-numbers"
            | "calendar-show-tag-vpane"
            | "calendar-tag-vpane-position"
            | "calendar-hidden-sources"
            | "task-hidden-sources"
            | "memo-hidden-sources"
            | "addressbook-hidden-sources"
            | "calendar-hidden-groups"
            | "calendar-groups-order"
            | "task-hidden-groups"
            | "task-groups-order"
            | "memo-hidden-groups"
            | "memo-groups-order"
            | "addressbook-hidden-groups"
            | "addressbook-groups-order"
            | "date-format"
            | "time-format"
            | "calendar-time-zone"
            | "auto-load-remote-images"
            | "force-dark-mode"
            | "render-html"
            | "composer-html"
            | "composer-charset"
            | "request-read-receipt"
            | "spellcheck-languages"
            | "mail-font-family"
            | "mail-font-size"
            | "mark-read-on-selection"
            | "show-message-preview"
            | "mail-normal-headers"
            | "mail-default-folder"
            | "mail-layout"
            | "mail-show-toolbar"
            | "mail-show-sidebar"
            | "mail-sidebar-position"
            | "mail-modern-preview-position"
            | "mail-classic-preview-position"
            | "mail-sender-column-width"
            | "mail-row-density"
            | "mail-show-avatar"
            | "mail-show-sender"
            | "mail-show-recipient"
            | "mail-show-date"
            | "mail-show-status"
            | "mail-show-flagged"
            | "mail-show-attachment"
            | "mail-show-preview-text"
            | "mail-show-labels"
            | "mail-show-security-details"
            | "mail-show-all-headers"
            | "mail-show-statusbar"
            | "color-scheme"
            | "switcher-location"
            | "sign-new-messages"
            | "default-pgp-key"
            | "crypto-protocol"
            | "smime-signer-certificate"
            | "smime-private-key"
            | "smime-recipient-certificates"
            | "smime-ca-file"
            | "mail-sync-interval"
            | "mail-sync-message-limit"
            | "draft-autosave-interval"
            | "append-signature"
            | "signature-text"
            | "local-mail-directory"
            | "local-calendar-directory"
            | "local-contact-directory"
            | "quote-original"
            | "quote-style"
            | "mail-sort-field"
            | "mail-sort-descending"
            | "mail-threading"
            | "task-filter"
            | "task-category-filter"
            | "task-search-mode"
            | "task-source-filter"
            | "task-sort-field"
            | "task-sort-descending"
            | "task-preview-visible"
            | "task-show-toolbar"
            | "task-preview-orientation"
            | "task-hpane-position"
            | "task-vpane-position"
            | "memo-sort-field"
            | "memo-source-filter"
            | "memo-sort-descending"
            | "memo-search-mode"
            | "memo-category-filter"
            | "memo-preview-visible"
            | "memo-show-toolbar"
            | "memo-preview-orientation"
            | "memo-hpane-position"
            | "memo-vpane-position"
            | "offline-mode"
            | "contact-completion-minimum-query-length"
            | "contact-completion-show-address"
            | "contact-completion-book-sources"
            | "contact-address-formatting"
            | "contact-preview-show-maps"
            | "contact-preview-home-before-work"
            | "contact-open-map-target"
            | "contact-open-map-prefer-local"
            | "contact-layout"
            | "contact-preview-visible"
            | "contact-show-toolbar"
            | "contact-hpane-position"
            | "contact-vpane-position"
            | "contact-editor-show-phone"
            | "contact-editor-show-sip"
            | "contact-editor-show-im"
            | "contact-editor-show-home-address"
            | "contact-editor-show-work-address"
            | "contact-editor-show-other-address"
            | "contact-editor-show-web"
            | "contact-editor-show-job"
            | "contact-editor-show-dates"
            | "contact-editor-show-misc"
            | "contact-editor-show-notes"
            | "contact-editor-show-certs"
            | "composer-show-from"
            | "composer-show-cc"
            | "composer-show-bcc"
            | "composer-show-reply-to"
            | "composer-prompt-empty-subject"
            | "composer-prompt-only-bcc"
            | "composer-inline-spelling"
            | "composer-reply-style"
            | "composer-forward-style"
            | "composer-top-signature"
            | "composer-signature-outline"
            | "mail-image-loading-policy"
            | "mail-mark-seen"
            | "mail-mark-seen-timeout"
            | "mail-caret-mode"
            | "mail-magic-spacebar"
            | "mail-citation-highlighting"
            | "mail-citation-color-1"
            | "mail-citation-color-2"
            | "mail-citation-color-3"
            | "mail-citation-color-4"
            | "mail-citation-color-5"
            | "mail-headers-collapsed"
            | "mail-show-real-date"
    )
}

fn normalize_imported_setting_value(key: &str, value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if is_boolean_migration_key(key) {
        return parse_setting_bool(value)
            .map(|value| value.to_string())
            .ok_or("kein gültiger boolescher Wert");
    }

    match key {
        "window-width" => normalize_i32_setting(value, 360, i32::MAX),
        "window-height" => normalize_i32_setting(value, 480, i32::MAX),
        "switcher-location" => match value {
            "top" | "bottom-left" => Ok(value.to_string()),
            _ => Err("keine unterstützte Position des Modulumschalters"),
        },
        "mail-sidebar-position" => normalize_i32_setting(value, 0, 2_000),
        "mail-modern-preview-position" | "mail-classic-preview-position" => {
            normalize_i32_setting(value, 120, 4_096)
        }
        "mail-sender-column-width" => normalize_i32_setting(value, 80, 360),
        "mail-font-size" => normalize_u32_setting(value, 8, 48),
        "calendar-workday-start" | "calendar-workday-end" => normalize_u32_setting(value, 0, 1_439),
        "calendar-default-reminder" => normalize_u32_setting(value, 0, 10_080),
        "default-reminder-interval" | "ba-reminder-interval" | "hide-completed-tasks-value" => {
            normalize_u32_setting(value, 0, 100_000)
        }
        "default-reminder-units" | "ba-reminder-units" | "hide-completed-tasks-units" => {
            match value.trim().to_ascii_lowercase().as_str() {
                "minutes" | "min" | "0" => Ok("minutes".to_string()),
                "hours" | "hour" | "1" => Ok("hours".to_string()),
                "days" | "day" | "2" => Ok("days".to_string()),
                _ => Err("keine unterstützte Zeiteinheit"),
            }
        }
        "mail-sync-interval" => normalize_u32_setting(value, 0, 86_400),
        "mail-sync-message-limit" => normalize_u32_setting(value, 25, 1_000),
        "draft-autosave-interval" => normalize_u32_setting(value, 0, 3_600),
        "task-hpane-position"
        | "task-vpane-position"
        | "memo-hpane-position"
        | "memo-vpane-position" => normalize_i32_setting(value, 80, 4_096),
        "contact-completion-minimum-query-length" => normalize_u32_setting(value, 1, 100),
        "contact-hpane-position" | "contact-vpane-position" => {
            normalize_i32_setting(value, 80, 4_096)
        }
        "contact-open-map-target" => match value.trim().to_ascii_lowercase().as_str() {
            "openstreetmap" | "osm" => Ok("openstreetmap".to_string()),
            "google" | "googlemaps" | "google-maps" => Ok("google".to_string()),
            _ => Err("kein unterstützter Kartendienst"),
        },
        "contact-layout" => match value.trim().to_ascii_lowercase().as_str() {
            "0" | "classic" => Ok("classic".to_string()),
            "1" | "vertical" => Ok("vertical".to_string()),
            _ => Err("kein unterstütztes Kontaktlayout"),
        },
        "contact-completion-book-sources" => {
            let uids = value
                .split([',', ';'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<_>>();
            if uids.iter().any(|s| s.chars().any(char::is_control)) {
                Err("ungültige Quellenliste")
            } else {
                Ok(crate::services::settings_service::sanitize_source_list(&uids).join(","))
            }
        }
        "calendar-tag-vpane-position" => normalize_i32_setting(value, 100, 2_000),
        "composer-charset" => normalize_composer_charset(value)
            .map(str::to_string)
            .ok_or("keine unterstützte Zeichenkodierung"),
        "composer-reply-style" => {
            crate::services::settings_service::normalize_composer_reply_style(value)
                .map(str::to_string)
                .ok_or("kein unterstützter Antwort-Stil")
        }
        "composer-forward-style" => {
            crate::services::settings_service::normalize_composer_forward_style(value)
                .map(str::to_string)
                .ok_or("kein unterstützter Weiterleitungs-Stil")
        }
        "mail-image-loading-policy" => {
            crate::services::settings_service::normalize_image_loading_policy(value)
                .map(str::to_string)
                .ok_or("keine unterstützte Bildladerichtlinie")
        }
        "mail-mark-seen-timeout" => normalize_u32_setting(value, 0, 60_000),
        "mail-citation-color-1"
        | "mail-citation-color-2"
        | "mail-citation-color-3"
        | "mail-citation-color-4"
        | "mail-citation-color-5" => {
            Ok(crate::services::settings_service::normalize_hex_color(value))
        }
        "active-view" => match value.to_ascii_lowercase().as_str() {
            "mail" | "calendar" | "contacts" | "addressbook" | "tasks" | "memos" => {
                Ok(if value.eq_ignore_ascii_case("addressbook") {
                    "contacts".to_string()
                } else {
                    value.to_ascii_lowercase()
                })
            }
            _ => Err("kein unterstützter Bereich"),
        },
        "calendar-view" => match value.to_ascii_lowercase().as_str() {
            "day" | "week" | "workweek" | "work-week" | "month" | "agenda" | "year" | "list" => {
                Ok(if value.eq_ignore_ascii_case("work-week") {
                    "workweek".to_string()
                } else {
                    value.to_ascii_lowercase()
                })
            }
            _ => Err("keine unterstützte Kalenderansicht"),
        },
        "calendar-time-divisions" => match value.trim().parse::<i32>() {
            Ok(5 | 10 | 15 | 30 | 60) => Ok(value.trim().to_string()),
            _ => Err("keine unterstützte Zeiteinteilung"),
        },
        "calendar-second-zone" => {
            let trimmed = value.trim();
            if trimmed.is_empty()
                || trimmed == "system"
                || trimmed
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
            {
                Ok(trimmed.to_string())
            } else {
                Err("keine gültige Zeitzone")
            }
        }
        "calendar-second-zones" => {
            let zones = value
                .split([',', ';'])
                .map(str::trim)
                .filter(|z| !z.is_empty())
                .collect::<Vec<_>>();
            if zones.iter().any(|z| {
                !z.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
            }) {
                Err("ungültige Zeitzonenliste")
            } else {
                Ok(zones.join(","))
            }
        }
        "calendar-hidden-sources"
        | "task-hidden-sources"
        | "memo-hidden-sources"
        | "addressbook-hidden-sources"
        | "calendar-hidden-groups"
        | "task-hidden-groups"
        | "memo-hidden-groups"
        | "addressbook-hidden-groups" => {
            let items = value
                .split([',', ';'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if items.iter().any(|s| s.chars().any(char::is_control)) {
                Err("ungültige Quellenliste")
            } else {
                let mut ordered = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for item in items {
                    if seen.insert(item) {
                        ordered.push(item);
                    }
                }
                Ok(ordered.join(","))
            }
        }
        "calendar-groups-order"
        | "task-groups-order"
        | "memo-groups-order"
        | "addressbook-groups-order" => {
            let items = value
                .split([',', ';'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if items.iter().any(|s| s.chars().any(char::is_control)) {
                Err("ungültige Gruppenreihenfolge")
            } else {
                let mut ordered = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for item in items {
                    if seen.insert(item) {
                        ordered.push(item);
                    }
                }
                Ok(ordered.join(","))
            }
        }
        "calendar-week-start" => match value.to_ascii_lowercase().as_str() {
            "0" | "monday" | "mon" => Ok("monday".to_string()),
            "1" | "sunday" | "sun" => Ok("sunday".to_string()),
            _ => Err("kein unterstützter Wochenbeginn"),
        },
        "calendar-workdays" => normalize_workdays_setting(value),
        "date-format" => match value.to_ascii_lowercase().as_str() {
            "german" | "de" | "de-de" => Ok("german".to_string()),
            "iso" | "yyyy-mm-dd" => Ok("iso".to_string()),
            "us" | "american" | "en-us" => Ok("us".to_string()),
            _ => Err("kein unterstütztes Datumsformat"),
        },
        "time-format" => match value.to_ascii_lowercase().as_str() {
            "12h" | "12-hour" | "12-hour-clock" => Ok("12h".to_string()),
            "24h" | "24-hour" | "24-hour-clock" => Ok("24h".to_string()),
            _ => Err("kein unterstütztes Zeitformat"),
        },
        "mail-default-folder" => match value.to_ascii_lowercase().as_str() {
            "all" | "inbox" | "sent" | "drafts" | "trash" | "archive" => {
                Ok(value.to_ascii_lowercase())
            }
            "spam" | "junk" => Ok("spam".to_string()),
            _ => Err("kein unterstützter Standardordner"),
        },
        "mail-layout" => match value.to_ascii_lowercase().as_str() {
            "classic" | "modern" | "horizontal" | "vertical" => Ok(value.to_ascii_lowercase()),
            _ => Err("kein unterstütztes Mail-Layout"),
        },
        "mail-normal-headers" => parse_mail_normal_headers_value(value)
            .map(|_| value.to_string())
            .map_err(|_| "keine gültige Headerliste"),
        "local-mail-directory" | "local-calendar-directory" | "local-contact-directory" => {
            if value.len() > 4_096 || value.chars().any(char::is_control) {
                Err("kein sicherer portabler Verzeichnispfad")
            } else {
                Ok(value.to_string())
            }
        }
        "mail-row-density" => match value.to_ascii_lowercase().as_str() {
            "compact" | "comfortable" | "spacious" => Ok(value.to_ascii_lowercase()),
            _ => Err("keine unterstützte Mail-Dichte"),
        },
        "mail-sort-field" => match value.to_ascii_lowercase().as_str() {
            "subject" => Ok("subject".to_string()),
            "sender" | "from" => Ok("sender".to_string()),
            "date" | "received" | "date-received" => Ok("date".to_string()),
            "unread" => Ok("unread".to_string()),
            "flagged" | "starred" => Ok("flagged".to_string()),
            "important" => Ok("important".to_string()),
            "attachment" | "attachments" => Ok("attachments".to_string()),
            _ => Err("kein unterstütztes Mail-Sortierfeld"),
        },
        "color-scheme" => match value.to_ascii_lowercase().as_str() {
            "system" | "light" | "dark" => Ok(value.to_ascii_lowercase()),
            _ => Err("kein unterstütztes Farbschema"),
        },
        "crypto-protocol" => match value.to_ascii_lowercase().as_str() {
            "openpgp" | "pgp" => Ok("openpgp".to_string()),
            "smime" | "s/mime" => Ok("smime".to_string()),
            _ => Err("kein unterstütztes Kryptoprotokoll"),
        },
        "quote-style" => match value.to_ascii_lowercase().as_str() {
            "prefix" | "blockquote" => Ok(value.to_ascii_lowercase()),
            _ => Err("kein unterstützter Zitierstil"),
        },
        "task-filter" => match value.to_ascii_lowercase().as_str() {
            "all" | "today" | "next-7" | "next-30" | "next-365" | "active" | "important"
            | "overdue" | "in-progress" | "cancelled" | "completed" | "uncompleted"
            | "scheduled" | "no-due" | "with-attachments" | "started" | "without-category" => {
                Ok(match value.to_ascii_lowercase().as_str() {
                    "active" => "next-365".to_string(),
                    normalized => normalized.to_string(),
                })
            }
            _ => Err("kein unterstützter Aufgabenfilter"),
        },
        "task-category-filter" => {
            if value.len() > MAX_TASK_CATEGORY_FILTER_BYTES || value.chars().any(char::is_control) {
                Err("ungültiger Aufgaben-Kategoriefilter")
            } else {
                Ok(value.to_string())
            }
        }
        "task-search-mode" => match value.to_ascii_lowercase().as_str() {
            "any-field" | "summary" | "description" | "advanced" => Ok(value.to_ascii_lowercase()),
            _ => Err("kein unterstützter Aufgaben-Suchmodus"),
        },
        "task-sort-field" => match value.to_ascii_lowercase().as_str() {
            "due" | "title" | "priority" => Ok(value.to_ascii_lowercase()),
            _ => Err("kein unterstütztes Aufgaben-Sortierfeld"),
        },
        "task-preview-orientation" => normalize_task_preview_orientation_value(value)
            .map_err(|_| "keine unterstützte Aufgaben-Vorschauausrichtung"),
        "memo-search-mode" => {
            normalize_memo_search_mode_value(value).map_err(|_| "kein unterstützter Memo-Suchmodus")
        }
        "memo-category-filter" => validate_memo_category_filter_value(value)
            .map_err(|_| "ungültiger Memo-Kategoriefilter"),
        "memo-source-filter" => {
            normalize_memo_source_filter_value(value).map_err(|_| "ungültiger Memo-Quellenfilter")
        }
        "memo-sort-field" => match value.to_ascii_lowercase().as_str() {
            "updated" | "title" | "source" => Ok(value.to_ascii_lowercase()),
            _ => Err("kein unterstütztes Memo-Sortierfeld"),
        },
        "memo-preview-orientation" => normalize_memo_preview_orientation_value(value)
            .map_err(|_| "keine unterstützte Memo-Vorschauausrichtung"),
        "mail-font-family" => {
            if value.is_empty()
                || value.len() > 120
                || value.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || " ,-_'\"".contains(character))
                })
            {
                Err("keine portable Schriftfamilie")
            } else {
                Ok(value.to_string())
            }
        }
        "spellcheck-languages" => {
            if value
                .split(',')
                .filter(|language| !language.trim().is_empty())
                .all(|language| {
                    let language = language.trim();
                    language.len() <= 35
                        && language
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric() || character == '-')
                })
            {
                Ok(value.to_string())
            } else {
                Err("keine portable Sprachliste")
            }
        }
        "junk-empty-on-exit-days" => normalize_i32_setting(value, 0, 365),
        "junk-default-plugin" => {
            if value.len() > 128 || value.chars().any(char::is_control) {
                Err("ungültiges Junk-Plugin")
            } else {
                Ok(value.to_string())
            }
        }
        "junk-custom-header" | "junk-custom-header-value" => {
            if value.len() > 256 || value.chars().any(char::is_control) {
                Err("ungültiger Junk-Header")
            } else {
                Ok(value.to_string())
            }
        }
        _ => Ok(value.to_string()),
    }
}

fn is_boolean_migration_key(key: &str) -> bool {
    matches!(
        key,
        "is-maximized"
            | "notifications-enabled"
            | "confirm-destructive-actions"
            | "calendar-show-week-numbers"
            | "calendar-show-tag-vpane"
            | "calendar-compress-weekend"
            | "calendar-month-scroll-by-week"
            | "use-default-reminder"
            | "use-ba-reminder"
            | "hide-completed-tasks"
            | "hide-cancelled-tasks"
            | "hide-cancelled-events"
            | "auto-load-remote-images"
            | "offline-mode"
            | "force-dark-mode"
            | "render-html"
            | "composer-html"
            | "composer-show-from"
            | "composer-show-cc"
            | "composer-show-bcc"
            | "composer-show-reply-to"
            | "composer-prompt-empty-subject"
            | "composer-prompt-only-bcc"
            | "composer-inline-spelling"
            | "composer-top-signature"
            | "composer-signature-outline"
            | "mail-mark-seen"
            | "mail-caret-mode"
            | "mail-magic-spacebar"
            | "mail-citation-highlighting"
            | "mail-headers-collapsed"
            | "mail-show-real-date"
            | "request-read-receipt"
            | "mark-read-on-selection"
            | "show-message-preview"
            | "mail-show-toolbar"
            | "mail-show-sidebar"
            | "mail-show-security-details"
            | "mail-show-all-headers"
            | "mail-show-statusbar"
            | "mail-show-avatar"
            | "mail-show-sender"
            | "mail-show-recipient"
            | "mail-show-date"
            | "mail-show-attachment"
            | "mail-show-preview-text"
            | "mail-show-labels"
            | "junk-check-incoming"
            | "junk-empty-on-exit"
            | "junk-lookup-addressbook"
            | "junk-lookup-addressbook-local-only"
            | "sign-new-messages"
            | "append-signature"
            | "quote-original"
            | "mail-sort-descending"
            | "mail-threading"
            | "task-sort-descending"
            | "task-preview-visible"
            | "task-show-toolbar"
            | "memo-preview-visible"
            | "memo-show-toolbar"
            | "memo-sort-descending"
            | "contact-completion-show-address"
            | "contact-address-formatting"
            | "contact-preview-show-maps"
            | "contact-preview-home-before-work"
            | "contact-open-map-prefer-local"
            | "contact-preview-visible"
            | "contact-show-toolbar"
            | "contact-editor-show-phone"
            | "contact-editor-show-sip"
            | "contact-editor-show-im"
            | "contact-editor-show-home-address"
            | "contact-editor-show-work-address"
            | "contact-editor-show-other-address"
            | "contact-editor-show-web"
            | "contact-editor-show-job"
            | "contact-editor-show-dates"
            | "contact-editor-show-misc"
            | "contact-editor-show-notes"
            | "contact-editor-show-certs"
    )
}

fn normalize_i32_setting(value: &str, minimum: i32, maximum: i32) -> Result<String, &'static str> {
    let value = value.parse::<i32>().map_err(|_| "keine gültige Ganzzahl")?;
    if value < minimum || value > maximum {
        return Err("außerhalb des portablen Wertebereichs");
    }
    Ok(value.to_string())
}

fn normalize_u32_setting(value: &str, minimum: u32, maximum: u32) -> Result<String, &'static str> {
    let value = value
        .parse::<u32>()
        .map_err(|_| "keine gültige positive Ganzzahl")?;
    if value < minimum || value > maximum {
        return Err("außerhalb des portablen Wertebereichs");
    }
    Ok(value.to_string())
}

fn normalize_workdays_setting(value: &str) -> Result<String, &'static str> {
    const DAYS: [&str; 7] = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    let mut normalized = Vec::new();
    for candidate in value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter(|candidate| !candidate.trim().is_empty())
    {
        let candidate = candidate.to_ascii_lowercase();
        let candidate = match candidate.as_str() {
            "mon" => "monday".to_string(),
            "tue" | "tues" => "tuesday".to_string(),
            "wed" => "wednesday".to_string(),
            "thu" | "thur" | "thurs" => "thursday".to_string(),
            "fri" => "friday".to_string(),
            "sat" => "saturday".to_string(),
            "sun" => "sunday".to_string(),
            value if DAYS.contains(&value) => value.to_string(),
            _ => return Err("enthält einen unbekannten Arbeitstag"),
        };
        if !normalized.contains(&candidate) {
            normalized.push(candidate);
        }
    }
    Ok(normalized.join(","))
}

fn safe_warning_key(value: &str) -> String {
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

fn is_secret_setting_key(key: &str) -> bool {
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
        "certificate-key",
        "cert-key",
        "key-password",
    ]
    .iter()
    .any(|secret| key.contains(secret))
}

fn decode_setting_value(
    raw_value: &str,
    expected_type: SettingValueType,
) -> Result<String, &'static str> {
    let (explicit_type, value) = split_setting_type_prefix(raw_value)?;
    if let Some(explicit_type) = explicit_type
        && !setting_types_compatible(expected_type, explicit_type)
    {
        return Err("besitzt einen falschen Typ");
    }

    match explicit_type {
        Some(SettingValueType::StringArray) => decode_gvariant_string_list(value),
        Some(SettingValueType::String) => parse_setting_string(value),
        Some(SettingValueType::HeaderArray) => {
            if value.trim().is_empty() {
                Err("besitzt eine leere Headerliste")
            } else {
                Ok(value.trim().to_string())
            }
        }
        _ => match expected_type {
            SettingValueType::String
                if value.trim().starts_with('\'') || value.trim().starts_with('"') =>
            {
                parse_setting_string(value)
            }
            SettingValueType::StringArray if value.trim().starts_with('[') => {
                decode_gvariant_string_list(value)
            }
            SettingValueType::StringArray
                if value.trim().starts_with('\'') || value.trim().starts_with('"') =>
            {
                parse_setting_string(value)
            }
            _ => Ok(value.trim().to_string()),
        },
    }
}

fn split_setting_type_prefix(
    raw_value: &str,
) -> Result<(Option<SettingValueType>, &str), &'static str> {
    let value = raw_value.trim();
    let Some((prefix, rest)) = value.split_once(char::is_whitespace) else {
        if value.starts_with('@') {
            return Err("besitzt einen unvollständigen Typpräfix");
        }
        return Ok((None, value));
    };
    let explicit_type = match prefix {
        "@s" | "string" => Some(SettingValueType::String),
        "@b" | "boolean" => Some(SettingValueType::Boolean),
        "@i" | "int32" => Some(SettingValueType::Int32),
        "@u" | "uint32" => Some(SettingValueType::Uint32),
        "@as" | "as" => Some(SettingValueType::StringArray),
        "@a(sb)" | "a(sb)" => Some(SettingValueType::HeaderArray),
        "@d" => return Err("besitzt einen nicht unterstützten Gleitkommatyp"),
        _ if prefix.starts_with('@') => return Err("besitzt einen unbekannten Typpräfix"),
        _ => {
            // A plain untyped string is allowed to contain whitespace.  Only
            // a recognized type token consumes the first word; otherwise the
            // complete value must be retained (for example a font family,
            // path, task category, or GVariant header array).
            return Ok((None, value));
        }
    };
    if explicit_type.is_some() && rest.trim().is_empty() {
        return Err("besitzt keinen Wert");
    }
    Ok((explicit_type, rest.trim()))
}

fn setting_types_compatible(expected: SettingValueType, actual: SettingValueType) -> bool {
    match expected {
        SettingValueType::String => actual == SettingValueType::String,
        SettingValueType::Boolean => actual == SettingValueType::Boolean,
        SettingValueType::Int32 => actual == SettingValueType::Int32,
        SettingValueType::Uint32 => actual == SettingValueType::Uint32,
        SettingValueType::StringArray => {
            matches!(
                actual,
                SettingValueType::String | SettingValueType::StringArray
            )
        }
        SettingValueType::HeaderArray => actual == SettingValueType::HeaderArray,
    }
}

fn parse_setting_string(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    let Some(quote) = value.chars().next() else {
        return Ok(String::new());
    };
    if !matches!(quote, '\'' | '"') {
        return Ok(value.to_string());
    }

    if let Ok(variant) = glib::Variant::parse(
        Some(<String as StaticVariantType>::static_variant_type().as_ref()),
        value,
    ) {
        if let Some(value) = variant.get::<String>() {
            return Ok(value);
        }
    }

    let mut raw = String::new();
    let mut characters = value[quote.len_utf8()..].chars();
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if escaped {
            raw.push('\\');
            raw.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            if !characters.all(char::is_whitespace) {
                return Err("besitzt Text nach dem Stringabschluss");
            }
            if escaped {
                return Err("besitzt einen unvollständigen Escape");
            }
            return Ok(unescape_setting_value(&raw));
        } else {
            raw.push(character);
        }
    }
    Err("besitzt einen nicht abgeschlossenen String")
}

fn unescape_setting_value(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let Some(escaped) = characters.next() else {
            output.push('\\');
            break;
        };
        match escaped {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            's' => output.push(' '),
            '\\' => output.push('\\'),
            '\'' => output.push('\''),
            '"' => output.push('"'),
            'u' | 'U' => {
                let digits = if escaped == 'u' { 4 } else { 8 };
                let mut hex = String::with_capacity(digits);
                for _ in 0..digits {
                    if let Some(digit) = characters.next() {
                        hex.push(digit);
                    }
                }
                if hex.len() == digits
                    && u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .is_some()
                {
                    output.push(
                        char::from_u32(u32::from_str_radix(&hex, 16).unwrap())
                            .expect("validated Unicode escape"),
                    );
                } else {
                    output.push('\\');
                    output.push(escaped);
                    output.push_str(&hex);
                }
            }
            'x' => {
                let first = characters.next();
                let second = characters.next();
                match (first, second) {
                    (Some(first), Some(second)) => {
                        let mut hex = String::with_capacity(2);
                        hex.push(first);
                        hex.push(second);
                        if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                            output.push(byte as char);
                        } else {
                            output.push('\\');
                            output.push('x');
                            output.push(first);
                            output.push(second);
                        }
                    }
                    (first, second) => {
                        output.push('\\');
                        output.push('x');
                        if let Some(first) = first {
                            output.push(first);
                        }
                        if let Some(second) = second {
                            output.push(second);
                        }
                    }
                }
            }
            other => {
                // Unknown escapes are retained instead of being silently
                // transformed.  This keeps imported portable settings
                // loss-aware and avoids making an invalid value look valid.
                output.push('\\');
                output.push(other);
            }
        }
    }
    output
}

fn decode_gvariant_string_list(value: &str) -> Result<String, &'static str> {
    let value = value.trim();

    if let Ok(variant) = glib::Variant::parse(
        Some(<Vec<String> as StaticVariantType>::static_variant_type().as_ref()),
        value,
    ) {
        if let Some(values) = variant.get::<Vec<String>>() {
            if values
                .iter()
                .any(|value| value.contains(',') || value.chars().any(char::is_control))
            {
                return Err("besitzt ein nicht-portables Stringlistenelement");
            }
            return Ok(values.join(","));
        }
    }

    let Some(body) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err("besitzt keine gültige Stringliste");
    };
    if body.trim().is_empty() {
        return Ok(String::new());
    }
    let mut result = Vec::new();
    let mut characters = body.chars().peekable();
    let mut trailing_comma = false;
    loop {
        while characters
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            characters.next();
        }
        if characters.peek().is_none() {
            return if trailing_comma {
                Err("besitzt ein fehlendes Stringlistenelement")
            } else {
                Ok(result.join(","))
            };
        }
        let Some(quote @ ('\'' | '"')) = characters.next() else {
            return Err("besitzt ein ungültiges Stringlistenelement");
        };
        let mut raw_item = String::new();
        let mut escaped = false;
        let mut closed = false;
        while let Some(character) = characters.next() {
            if escaped {
                raw_item.push('\\');
                raw_item.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                closed = true;
                break;
            } else {
                raw_item.push(character);
            }
        }
        if escaped || !closed {
            return Err("besitzt ein nicht abgeschlossenes Stringlistenelement");
        }
        let item = unescape_setting_value(&raw_item);
        if item.contains(',') || item.chars().any(char::is_control) {
            return Err("besitzt ein nicht-portables Stringlistenelement");
        }
        result.push(item);
        while characters
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            characters.next();
        }
        match characters.next() {
            None => break,
            Some(',') => trailing_comma = true,
            Some(_) => return Err("besitzt ein ungültiges Trennzeichen in der Stringliste"),
        }
    }
    Ok(result.join(","))
}

fn parse_setting_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn collect_named_files(root: &Path, file_name: &str, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Der ausgewählte Evolution-Profilpfad ist kein Ordner",
        ));
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_named_files(&path, file_name, result)?;
        } else if file_type.is_file()
            && path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
        {
            result.push(path);
        }
    }
    Ok(())
}

fn collect_local_data_files(root: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Der ausgewählte Evolution-Profilpfad ist kein Ordner",
        ));
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_local_data_files(&path, result)?;
        } else if file_type.is_file()
            && path.extension().is_some_and(|extension| {
                matches!(
                    extension.to_string_lossy().to_ascii_lowercase().as_str(),
                    "ics" | "vcs" | "vcf" | "vcard"
                )
            })
        {
            result.push(path);
        }
    }
    Ok(())
}

fn collect_local_mail_files(root: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Der ausgewählte Evolution-Profilpfad ist kein Ordner",
        ));
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_local_mail_files(&path, result)?;
        } else if file_type.is_file() && is_local_mail_candidate(&path) {
            result.push(path);
        }
    }
    Ok(())
}

fn collect_local_mail_layout_paths(root: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut local_roots = Vec::new();
    collect_evolution_local_mail_roots(root, &mut local_roots)?;
    local_roots.sort();
    local_roots.dedup();
    for local_root in local_roots {
        collect_local_mail_layout_entries(&local_root, &local_root, result)?;
    }
    result.sort();
    result.dedup();
    Ok(())
}

fn collect_evolution_local_mail_roots(root: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Ok(());
    }

    let is_local_root = root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("local"))
        && root
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("mail"));
    if is_local_root {
        result.push(root.to_path_buf());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        collect_evolution_local_mail_roots(&entry.path(), result)?;
    }
    Ok(())
}

fn collect_local_mail_layout_entries(
    root: &Path,
    current: &Path,
    result: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.eq_ignore_ascii_case("cur")
                || name.eq_ignore_ascii_case("new")
                || name.eq_ignore_ascii_case("tmp")
            {
                continue;
            }

            let has_maildir_children = directory_has_maildir_children(&path);
            let hidden_folder = name.starts_with('.') && name != "." && name != "..";
            let sbd_folder = name
                .to_ascii_lowercase()
                .strip_suffix(".sbd")
                .is_some_and(|name| !name.is_empty());
            if has_maildir_children || hidden_folder || sbd_folder {
                result.push(path.clone());
            }
            collect_local_mail_layout_entries(root, &path, result)?;
        } else if file_type.is_file() && is_local_mail_layout_file(root, &path) {
            result.push(path);
        }
    }
    Ok(())
}

fn directory_has_maildir_children(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        file_type.is_dir()
            && (entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("cur")
                || entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("new"))
    })
}

fn is_local_mail_layout_file(root: &Path, path: &Path) -> bool {
    let parent_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if parent_name.eq_ignore_ascii_case("cur")
        || parent_name.eq_ignore_ascii_case("new")
        || parent_name.eq_ignore_ascii_case("tmp")
    {
        return false;
    }

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    if matches!(extension.as_deref(), Some("eml" | "mbox")) {
        return true;
    }
    if extension.is_some() {
        return false;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if file_name.starts_with('.') {
        return false;
    }
    path.parent().is_some_and(|parent| {
        parent == root
            || parent
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.to_ascii_lowercase()
                        .strip_suffix(".sbd")
                        .is_some_and(|name| !name.is_empty())
                })
    })
}

fn is_local_mail_candidate(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    if matches!(extension.as_deref(), Some("eml" | "mbox")) {
        return true;
    }
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "cur" | "new"))
    {
        return true;
    }
    if extension.is_some() {
        return false;
    }
    let Ok(raw) = fs::read(path) else {
        return false;
    };
    let text = String::from_utf8_lossy(&raw);
    let mut lines = text.lines();
    let looks_like_mbox = lines.next().is_some_and(|line| line.starts_with("From "))
        && lines.any(|line| line.starts_with("From: "));
    looks_like_mbox
        || text.starts_with("From:")
        || text.contains("\nFrom:")
        || text.starts_with("Subject:")
        || text.contains("\nSubject:")
}

fn safe_profile_path(root: &Path, parent: &Path, path_value: &str) -> Option<PathBuf> {
    let path_value = path_value
        .trim()
        .strip_prefix("file://")
        .unwrap_or(path_value.trim());
    let value = Path::new(path_value);
    let candidate = if value.is_absolute() {
        value.to_path_buf()
    } else {
        parent.join(value)
    };
    let canonical_root = fs::canonicalize(root).ok()?;
    let canonical_candidate = fs::canonicalize(candidate).ok()?;
    canonical_candidate.strip_prefix(canonical_root).ok()?;
    Some(canonical_candidate)
}

fn parse_evolution_filters(raw: &str, path: &Path) -> Result<Vec<MailFilterRule>, String> {
    let mut rules = Vec::new();
    for (index, (opening_tag, body)) in extract_xml_blocks(raw, "rule").into_iter().enumerate() {
        let enabled = attribute_value(&opening_tag, "enabled")
            .as_deref()
            .map(parse_bool)
            .unwrap_or(true);
        let scope = match attribute_value(&opening_tag, "source")
            .or_else(|| attribute_value(&opening_tag, "scope"))
            .unwrap_or_else(|| "incoming".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "outgoing" | "outbox" | "sent" => FilterScope::Outgoing,
            "manual" | "addressbook" => FilterScope::Manual,
            _ => FilterScope::Incoming,
        };

        let mut conditions = Vec::new();
        for (part_tag, part_body) in extract_xml_blocks(&body, "part") {
            let Some(field) = attribute_value(&part_tag, "name")
                .as_deref()
                .and_then(filter_field_from_evolution_name)
            else {
                continue;
            };
            let Some((value_tag, value_body)) =
                extract_xml_blocks(&part_body, "value").into_iter().next()
            else {
                continue;
            };
            let operator = attribute_value(&value_tag, "name")
                .as_deref()
                .and_then(filter_operator_from_evolution_name)
                .unwrap_or_default();
            let value = decode_xml_entities(strip_xml_tags(&value_body).trim());
            if !value.is_empty() || operator.allows_empty_value(field) {
                conditions.push(FilterCondition {
                    field,
                    value,
                    operator,
                });
            }
        }
        let Some(condition) = conditions.first().cloned() else {
            continue;
        };

        let mut actions = Vec::new();
        for (action_tag, action_body) in extract_xml_blocks(&body, "action") {
            let action_name = attribute_value(&action_tag, "name")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let target = extract_xml_blocks(&action_body, "value")
                .into_iter()
                .next()
                .map(|(_, body)| decode_xml_entities(strip_xml_tags(&body).trim()))
                .filter(|target| !target.is_empty())
                .unwrap_or_default();
            if let Some(action) = filter_action_from_evolution_name(&action_name, target) {
                actions.push(action);
            }
        }
        let Some(action) = actions.first().cloned() else {
            continue;
        };

        let name = attribute_value(&opening_tag, "name")
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("Importierte Evolution-Regel {}", index + 1));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.to_string_lossy().hash(&mut hasher);
        index.hash(&mut hasher);
        name.hash(&mut hasher);
        for condition in &conditions {
            condition.field.hash(&mut hasher);
            condition.value.hash(&mut hasher);
            condition.operator.hash(&mut hasher);
        }
        for action in &actions {
            action.hash(&mut hasher);
        }
        let id = format!("evolution-filter-{:016x}", hasher.finish());
        let match_all = attribute_value(&opening_tag, "match")
            .or_else(|| attribute_value(&opening_tag, "match-type"))
            .map(|value| !value.eq_ignore_ascii_case("any"))
            .unwrap_or(true);
        rules.push(MailFilterRule {
            id,
            name,
            enabled,
            scope,
            condition,
            action,
            conditions,
            actions,
            match_all,
        });
    }

    Ok(rules)
}

fn unsupported_filter_warnings(raw: &str, path: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    for (index, (opening_tag, body)) in extract_xml_blocks(raw, "rule").into_iter().enumerate() {
        let rule_name =
            filter_warning_label(attribute_value(&opening_tag, "name").as_deref(), index + 1);
        if let Some(enabled) = attribute_value(&opening_tag, "enabled")
            && parse_setting_bool(&enabled).is_none()
        {
            warnings.push(format!(
                "{}: Aktivierungswert in „{}“ ist nicht unterstützt und wird als aktiviert behandelt",
                migration_source_label(path),
                rule_name
            ));
        }
        if let Some(scope) = attribute_value(&opening_tag, "source")
            .or_else(|| attribute_value(&opening_tag, "scope"))
            && filter_scope_from_evolution_name(&scope).is_none()
        {
            warnings.push(format!(
                "{}: Filterbereich „{}“ in „{}“ wird als Eingang verarbeitet",
                migration_source_label(path),
                safe_warning_key(&scope),
                rule_name
            ));
        }
        if let Some(match_type) = attribute_value(&opening_tag, "match")
            .or_else(|| attribute_value(&opening_tag, "match-type"))
            && !matches!(match_type.to_ascii_lowercase().as_str(), "all" | "any")
        {
            warnings.push(format!(
                "{}: Verknüpfung „{}“ in „{}“ wird als UND verarbeitet",
                migration_source_label(path),
                safe_warning_key(&match_type),
                rule_name
            ));
        }

        let mut portable_condition = false;
        for (part_index, (part_tag, part_body)) in
            extract_xml_blocks(&body, "part").into_iter().enumerate()
        {
            let name = attribute_value(&part_tag, "name").unwrap_or_default();
            let Some(field) = filter_field_from_evolution_name(&name) else {
                if name.trim().is_empty() {
                    warnings.push(format!(
                        "{}: Bedingung {} in „{}“ besitzt kein unterstütztes Feld",
                        migration_source_label(path),
                        part_index + 1,
                        rule_name
                    ));
                } else {
                    warnings.push(format!(
                        "{}: Bedingung „{}“ in „{}“ wird portabel nicht ausgewertet",
                        migration_source_label(path),
                        safe_warning_key(&name),
                        rule_name
                    ));
                }
                continue;
            };

            let Some((value_tag, value_body)) =
                extract_xml_blocks(&part_body, "value").into_iter().next()
            else {
                warnings.push(format!(
                    "{}: Wert für Bedingung „{}“ in „{}“ fehlt und wird nicht importiert",
                    migration_source_label(path),
                    safe_warning_key(&name),
                    rule_name
                ));
                continue;
            };

            let mut condition_imported = true;
            if let Some(value_type) = attribute_value(&value_tag, "type")
                && !is_supported_filter_value_type(&value_type)
            {
                warnings.push(format!(
                    "{}: Werttyp „{}“ für Bedingung „{}“ in „{}“ wird portabel als Text verarbeitet",
                    migration_source_label(path),
                    safe_warning_key(&value_type),
                    safe_warning_key(&name),
                    rule_name
                ));
            }
            let operator_name = attribute_value(&value_tag, "name").unwrap_or_default();
            if operator_name.trim().is_empty() {
                warnings.push(format!(
                    "{}: Vergleich für Bedingung „{}“ in „{}“ fehlt und wird als „enthält“ importiert",
                    migration_source_label(path),
                    safe_warning_key(&name),
                    rule_name
                ));
            } else if filter_operator_from_evolution_name(&operator_name).is_none() {
                warnings.push(format!(
                    "{}: Vergleich „{}“ in „{}“ wird portabel als „enthält“ importiert",
                    migration_source_label(path),
                    safe_warning_key(&operator_name),
                    rule_name
                ));
            }
            let operator = filter_operator_from_evolution_name(&operator_name).unwrap_or_default();
            let value = decode_xml_entities(strip_xml_tags(&value_body).trim());
            if value.trim().is_empty() && !operator.allows_empty_value(field) {
                condition_imported = false;
                warnings.push(format!(
                    "{}: Leerer Wert für Bedingung „{}“ in „{}“ wird nicht importiert",
                    migration_source_label(path),
                    safe_warning_key(&name),
                    rule_name
                ));
            }
            portable_condition |= condition_imported;
        }

        let mut portable_action = false;
        for (action_index, (action_tag, action_body)) in
            extract_xml_blocks(&body, "action").into_iter().enumerate()
        {
            let name = attribute_value(&action_tag, "name").unwrap_or_default();
            let target = extract_xml_blocks(&action_body, "value")
                .into_iter()
                .next()
                .map(|(_, body)| decode_xml_entities(strip_xml_tags(&body).trim()))
                .unwrap_or_default();
            if name.trim().is_empty() {
                warnings.push(format!(
                    "{}: Aktion {} in „{}“ besitzt keinen unterstützten Namen",
                    migration_source_label(path),
                    action_index + 1,
                    rule_name
                ));
                continue;
            }
            if filter_action_requires_target(&name) && target.trim().is_empty() {
                warnings.push(format!(
                    "{}: Aktion „{}“ in „{}“ besitzt kein Ziel und wird nicht importiert",
                    migration_source_label(path),
                    safe_warning_key(&name),
                    rule_name
                ));
                continue;
            }
            let recognition_target = if matches!(
                name.trim().to_ascii_lowercase().as_str(),
                "score" | "set-score" | "assign-score" | "adj-score" | "adjust-score"
            ) {
                "0".to_string()
            } else if filter_action_requires_target(&name) {
                "portable-target".to_string()
            } else {
                String::new()
            };
            if filter_action_from_evolution_name(&name, recognition_target).is_none() {
                warnings.push(format!(
                    "{}: Aktion „{}“ in „{}“ wird portabel nicht ausgeführt",
                    migration_source_label(path),
                    safe_warning_key(&name),
                    rule_name
                ));
            } else {
                portable_action = true;
            }
        }
        if !portable_condition {
            warnings.push(format!(
                "{}: Regel „{}“ besitzt keine importierbare Bedingung",
                migration_source_label(path),
                rule_name
            ));
        }
        if !portable_action {
            warnings.push(format!(
                "{}: Regel „{}“ besitzt keine importierbare Aktion",
                migration_source_label(path),
                rule_name
            ));
        }
    }
    warnings
}

fn filter_warning_label(name: Option<&str>, index: usize) -> String {
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Regel");
    let label = name.replace(['\r', '\n'], " ");
    let label = if label.chars().count() > 80 {
        let mut shortened = label.chars().take(80).collect::<String>();
        shortened.push('…');
        shortened
    } else {
        label
    };
    if label == "Regel" {
        format!("Regel {index}")
    } else {
        label
    }
}

fn filter_scope_from_evolution_name(name: &str) -> Option<FilterScope> {
    match name.trim().to_ascii_lowercase().as_str() {
        "incoming" | "inbox" => Some(FilterScope::Incoming),
        "outgoing" | "outbox" | "sent" => Some(FilterScope::Outgoing),
        "manual" | "addressbook" => Some(FilterScope::Manual),
        _ => None,
    }
}

fn filter_action_requires_target(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "move"
            | "move-to"
            | "move-to-folder"
            | "fileinto"
            | "copy"
            | "copy-to"
            | "copy-to-folder"
            | "set-label"
            | "label"
            | "add-label"
            | "tag"
            | "clear-label"
            | "remove-label"
            | "remove-tag"
            | "set-color"
            | "color"
            | "colour"
            | "assign-color"
            | "set-score"
            | "score"
            | "assign-score"
            | "adj-score"
            | "adjust-score"
            | "play-sound"
            | "sound"
            | "shell"
            | "run-program"
            | "exec"
            | "pipe"
            | "pipe-message"
            | "pipe-to-program"
            | "forward"
            | "forward-to"
    )
}

fn is_supported_filter_value_type(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "string" | "boolean" | "bool" | "date" | "datetime" | "folder" | "integer"
    )
}

fn filter_field_from_evolution_name(name: &str) -> Option<FilterField> {
    match name.trim().to_ascii_lowercase().as_str() {
        "subject" | "title" => Some(FilterField::Subject),
        "from" | "sender" | "mail-from" => Some(FilterField::Sender),
        "to" | "cc" | "bcc" | "recipient" | "mail-to" => Some(FilterField::Recipient),
        "body" | "content" | "message" => Some(FilterField::Body),
        "header" | "header-exists" => Some(FilterField::Header),
        "label" | "tag" | "keyword" => Some(FilterField::Label),
        "unread" | "is-unread" | "read-status" => Some(FilterField::Unread),
        "flagged" | "is-flagged" | "starred" | "is-starred" => Some(FilterField::Flagged),
        "important" | "is-important" | "high-priority" => Some(FilterField::Important),
        "attachment" | "attachments" | "has-attachment" => Some(FilterField::Attachment),
        "date" | "sent-date" | "received-date" | "date-received" => Some(FilterField::Date),
        "folder" | "mail-folder" | "in-folder" => Some(FilterField::Folder),
        "junk" | "spam" | "is-junk" | "is-spam" => Some(FilterField::Junk),
        _ => None,
    }
}

fn filter_operator_from_evolution_name(name: &str) -> Option<FilterOperator> {
    match name.trim().to_ascii_lowercase().as_str() {
        "contains" | "contains-word" | "contains-text" => Some(FilterOperator::Contains),
        "does-not-contain" | "doesnt-contain" | "not-contains" | "not-containing" => {
            Some(FilterOperator::NotContains)
        }
        "is" | "equals" | "equal" => Some(FilterOperator::Equals),
        "is-not" | "isnt" | "not-equal" | "not-equals" => Some(FilterOperator::NotEquals),
        "begins-with" | "starts-with" => Some(FilterOperator::StartsWith),
        "ends-with" => Some(FilterOperator::EndsWith),
        "before" | "earlier-than" | "older-than" => Some(FilterOperator::Before),
        "after" | "later-than" | "newer-than" => Some(FilterOperator::After),
        "exists" | "header-exists" => Some(FilterOperator::Exists),
        _ => None,
    }
}

fn filter_action_from_evolution_name(name: &str, target: String) -> Option<FilterAction> {
    let name = name.trim().to_ascii_lowercase();
    match name.as_str() {
        "move" | "move-to" | "move-to-folder" | "fileinto" => {
            (!target.is_empty()).then_some(FilterAction::MoveToFolder(target))
        }
        "copy" | "copy-to" | "copy-to-folder" => {
            (!target.is_empty()).then_some(FilterAction::CopyToFolder(target))
        }
        "mark-read" | "markread" | "seen" => Some(FilterAction::MarkRead),
        "mark-unread" | "markunread" | "unseen" => Some(FilterAction::MarkUnread),
        "set-flagged" | "flag" | "starred" | "set-starred" => Some(FilterAction::SetFlagged),
        "clear-flagged" | "unflag" | "unstarred" | "clear-starred" => {
            Some(FilterAction::ClearFlagged)
        }
        "important" | "set-important" | "mark-important" | "high-priority" => {
            Some(FilterAction::SetImportant)
        }
        "not-important" | "clear-important" | "unmark-important" | "normal-priority" => {
            Some(FilterAction::ClearImportant)
        }
        "delete" | "discard" | "trash" => Some(FilterAction::Delete),
        "stop" | "stop-processing" | "stop-processing-filters" | "stop-processing-rule" => {
            Some(FilterAction::StopProcessing)
        }
        "junk" | "mark-junk" | "spam" | "mark-spam" => Some(FilterAction::SetJunk),
        "clear-junk" | "not-junk" | "not-spam" | "ham" => Some(FilterAction::ClearJunk),
        "set-label" | "label" | "add-label" | "tag" => {
            (!target.is_empty()).then_some(FilterAction::SetLabel(target))
        }
        "clear-label" | "remove-label" | "remove-tag" => {
            (!target.is_empty()).then_some(FilterAction::ClearLabel(target))
        }
        "set-color" | "color" | "colour" | "assign-color" => {
            (!target.trim().is_empty()).then_some(FilterAction::AssignColor(target.trim().to_string()))
        }
        "unset-color" | "clear-color" => Some(FilterAction::UnsetColor),
        "set-score" | "score" | "assign-score" => {
            target.trim().parse::<i32>().ok().map(FilterAction::AssignScore)
        }
        "adj-score" | "adjust-score" => {
            target.trim().parse::<i32>().ok().map(FilterAction::AdjustScore)
        }
        "beep" => Some(FilterAction::Beep),
        "play-sound" | "sound" => {
            (!target.trim().is_empty()).then_some(FilterAction::PlaySound(target.trim().to_string()))
        }
        "shell" | "run-program" | "exec" => {
            (!target.trim().is_empty()).then_some(FilterAction::RunProgram(target.trim().to_string()))
        }
        "pipe" | "pipe-message" | "pipe-to-program" => {
            (!target.trim().is_empty()).then_some(FilterAction::PipeToProgram(target.trim().to_string()))
        }
        "forward" | "forward-to" => {
            (!target.trim().is_empty()).then_some(FilterAction::ForwardTo(target.trim().to_string()))
        }
        _ => None,
    }
}

fn extract_xml_blocks(raw: &str, element: &str) -> Vec<(String, String)> {
    let opening = format!("<{element}");
    let closing = format!("</{element}>");
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = find_ascii_case_insensitive(&raw[cursor..], &opening) {
        let start = cursor + relative_start;
        let after_name = start + opening.len();
        if raw
            .as_bytes()
            .get(after_name)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            cursor = after_name;
            continue;
        }
        let Some(tag_end_relative) = raw[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_end_relative;
        let opening_tag = raw[start..=tag_end].to_string();
        if opening_tag.trim_end().ends_with("/>") {
            result.push((opening_tag, String::new()));
            cursor = tag_end + 1;
            continue;
        }
        let body_start = tag_end + 1;
        let Some(relative_close) = find_ascii_case_insensitive(&raw[body_start..], &closing) else {
            break;
        };
        let close_start = body_start + relative_close;
        result.push((opening_tag, raw[body_start..close_start].to_string()));
        cursor = close_start + closing.len();
    }
    result
}

fn attribute_value(tag: &str, attribute: &str) -> Option<String> {
    let marker = format!("{attribute}=");
    let mut cursor = 0;
    while let Some(relative_start) = find_ascii_case_insensitive(&tag[cursor..], &marker) {
        let start = cursor + relative_start;
        if start > 0
            && tag
                .as_bytes()
                .get(start - 1)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            cursor = start + marker.len();
            continue;
        }
        let mut value_start = start + marker.len();
        while tag
            .as_bytes()
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        let first = *tag.as_bytes().get(value_start)?;
        let value_end = if first == b'\'' || first == b'"' {
            tag[value_start + 1..]
                .find(first as char)
                .map(|offset| value_start + 1 + offset)?
        } else {
            tag[value_start..]
                .find(|character: char| character.is_ascii_whitespace() || character == '>')
                .map(|offset| value_start + offset)
                .unwrap_or(tag.len())
        };
        let value = if first == b'\'' || first == b'"' {
            &tag[value_start + 1..value_end]
        } else {
            &tag[value_start..value_end]
        };
        return Some(decode_xml_entities(value.trim()).to_string());
    }
    None
}

fn strip_xml_tags(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut inside_tag = false;
    for character in raw.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            character if !inside_tag => result.push(character),
            _ => {}
        }
    }
    result
}

fn first_xml_text(raw: &str, element: &str) -> Option<String> {
    extract_xml_blocks(raw, element)
        .into_iter()
        .next()
        .map(|(_, body)| decode_xml_entities(strip_xml_tags(&body).trim()))
        .filter(|value| !value.is_empty())
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn parse_source(raw: &str) -> Result<SourceKeyFile, &'static str> {
    let mut source = SourceKeyFile::default();
    let mut current_group = None::<String>;

    for (line_index, line) in raw.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(group) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let group = group.trim();
            if group.is_empty() {
                return Err("leere Keyfile-Gruppe");
            }
            current_group = Some(group.to_string());
            source.groups.entry(group.to_string()).or_default();
            continue;
        }

        let Some(group) = current_group.as_ref() else {
            source.warnings.push(format!(
                "Zeile {line_number}: Eintrag außerhalb einer Keyfile-Gruppe wurde ignoriert"
            ));
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            source.warnings.push(format!(
                "Zeile {line_number}: nicht unterstützter Keyfile-Eintrag wurde ignoriert"
            ));
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        if is_secret_key(key) {
            source.ignored_secret_keys.push(key.to_string());
            continue;
        }
        if !is_supported_source_key(group, key) {
            source.warnings.push(format!(
                "Zeile {line_number}: unbekanntes oder nicht unterstütztes Feld „{}“ wurde nicht übernommen",
                safe_warning_key(key)
            ));
        }
        source
            .groups
            .entry(group.clone())
            .or_default()
            .insert(key.to_string(), unescape_keyfile_value(value.trim()));
    }

    if source.groups.is_empty() {
        return Err("keine gültige Keyfile-Gruppe gefunden");
    }
    Ok(source)
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "credential",
        "privatekey",
        "private-key",
        "oauth",
        "keyring",
        "authdata",
        "auth-data",
        "api-key",
        "apikey",
        "client-key",
        "certificate-key",
        "cert-key",
        "key-password",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn is_supported_source_key(group: &str, key: &str) -> bool {
    let group = group.to_ascii_lowercase();
    let key = key.to_ascii_lowercase().replace('_', "-");
    let common = matches!(
        key.as_str(),
        "uid"
            | "id"
            | "displayname"
            | "display-name"
            | "name"
            | "label"
            | "enabled"
            | "active"
            | "backendname"
            | "backend"
            | "backendtype"
            | "type"
            | "host"
            | "server"
            | "hostname"
            | "servername"
            | "port"
            | "portnumber"
            | "user"
            | "username"
            | "login"
            | "method"
            | "authentication"
            | "authmethod"
            | "mechanism"
            | "tls"
            | "encryption"
            | "securitymethod"
            | "usessl"
            | "imapport"
            | "smtpport"
            | "parent"
            | "address"
            | "email"
            | "emailaddress"
            | "replyto"
            | "reply-to"
            | "replyaddress"
            | "organization"
            | "organisation"
            | "aliases"
            | "alias"
            | "emailaliases"
            | "emailalias"
            | "inboxfolder"
            | "inbox"
            | "inboxpath"
            | "draftsfolder"
            | "drafts"
            | "draftfolder"
            | "sentfolder"
            | "sent"
            | "sentfolderpath"
            | "archivefolder"
            | "archive"
            | "archivefolderpath"
            | "trashfolder"
            | "trash"
            | "junkfolder"
            | "junk"
            | "spamfolder"
            | "spam"
    );
    if common {
        return true;
    }
    matches!(
        group.as_str(),
        "data source"
            | "source"
            | "mail account"
            | "mail identity"
            | "identity"
            | "authentication"
            | "mail submission"
            | "submission"
            | "smtp"
            | "transport"
            | "camel smtp"
            | "imap"
            | "camel"
            | "security"
            | "collection"
            | "calendar"
            | "address book"
            | "backend"
    ) && matches!(key.as_str(), "readonly" | "writable" | "removable" | "icon")
}

fn unescape_keyfile_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        let Some(escaped) = characters.next() else {
            result.push('\\');
            break;
        };
        match escaped {
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            's' => result.push(' '),
            '\\' => result.push('\\'),
            'u' | 'U' => {
                let digits = if escaped == 'u' { 4 } else { 8 };
                let mut hex = String::with_capacity(digits);
                for _ in 0..digits {
                    if let Some(digit) = characters.next() {
                        hex.push(digit);
                    }
                }
                if hex.len() == digits
                    && let Ok(value) = u32::from_str_radix(&hex, 16)
                    && let Some(value) = char::from_u32(value)
                {
                    result.push(value);
                } else {
                    result.push('\\');
                    result.push(escaped);
                    result.push_str(&hex);
                }
            }
            other => {
                result.push('\\');
                result.push(other);
            }
        }
    }
    result
}

#[allow(dead_code)]
fn profile_from_source(path: &Path, source: &SourceKeyFile) -> Result<AccountProfile, String> {
    profile_from_source_with_warnings(path, source).map(|(profile, _)| profile)
}

fn profile_from_source_with_warnings(
    path: &Path,
    source: &SourceKeyFile,
) -> Result<(AccountProfile, Vec<String>), String> {
    let mut warnings = Vec::new();
    let email = first_value(
        source,
        &["Mail Identity", "Mail Account", "Identity", "Data Source"],
        &["Address", "EmailAddress", "Email"],
    )
    .or_else(|| {
        first_value(
            source,
            &["Authentication", "Mail Account"],
            &["User", "Username", "Login"],
        )
    })
    .and_then(normalize_email)
    .ok_or_else(|| "keine Mailadresse gefunden (kein Mailkonto)".to_string())?;

    let uid = first_value(
        source,
        &["Data Source", "Source", "Mail Account"],
        &["UID", "Uid", "Id", "ID"],
    )
    .filter(|value| !value.trim().is_empty())
    .or_else(|| {
        path.file_stem()
            .map(|value| value.to_string_lossy().into_owned())
    })
    .unwrap_or_else(|| email.clone());
    let display_name = first_value(
        source,
        &["Data Source", "Mail Identity", "Mail Account"],
        &["DisplayName", "Name", "Label"],
    )
    .filter(|value| !value.trim().is_empty())
    .unwrap_or_else(|| email.clone());

    let backend = first_value(
        source,
        &["Mail Account", "Collection", "Data Source", "Source"],
        &["BackendName", "Backend", "BackendType", "Type"],
    )
    .unwrap_or_default();
    let protocol = protocol_from_backend(&backend);
    if !backend.trim().is_empty() && !is_known_mail_backend(&backend) {
        warnings.push(
            "nicht unterstütztes Mail-Backend wurde als portables IMAP/SMTP-Konto übernommen"
                .to_string(),
        );
    }
    let imap_host = first_value(
        source,
        &["Mail Account", "Authentication", "IMAP", "Camel"],
        &["Host", "Server", "Hostname", "ServerName"],
    )
    .unwrap_or_default();
    let smtp_host = first_value(
        source,
        &[
            "Mail Submission",
            "Submission",
            "SMTP",
            "Transport",
            "Camel SMTP",
        ],
        &["Host", "Server", "Hostname", "ServerName"],
    )
    .unwrap_or_default();
    let tls_value = first_value(
        source,
        &[
            "Security",
            "Mail Account",
            "Mail Submission",
            "SMTP",
            "Authentication",
        ],
        &["Method", "TLS", "Encryption", "SecurityMethod", "UseSSL"],
    );
    let (tls_mode, tls_value_supported) = tls_mode_with_status(tls_value.as_deref());
    if tls_value.is_some() && !tls_value_supported {
        warnings.push(
            "nicht unterstützte Mail-Verschlüsselungsart wurde als TLS-Fallback übernommen"
                .to_string(),
        );
    }
    let username = first_value(
        source,
        &["Authentication", "Mail Account", "IMAP", "Camel"],
        &["User", "Username", "Login", "UserName"],
    )
    .unwrap_or_default();
    let imap_port_value = first_value(
        source,
        &["Mail Account", "Authentication", "IMAP", "Camel"],
        &["Port", "PortNumber", "IMAPPort"],
    );
    let imap_port = parse_source_port(
        imap_port_value.as_deref(),
        if tls_mode == "starttls" { 143 } else { 993 },
        "IMAP",
        &mut warnings,
    );
    let smtp_port_value = first_value(
        source,
        &[
            "Mail Submission",
            "Submission",
            "SMTP",
            "Transport",
            "Camel SMTP",
        ],
        &["Port", "PortNumber", "SMTPPort"],
    );
    let smtp_port = parse_source_port(
        smtp_port_value.as_deref(),
        if tls_mode == "starttls" { 587 } else { 465 },
        "SMTP",
        &mut warnings,
    );
    let auth_method = first_value(
        source,
        &["Authentication", "Mail Account", "SMTP", "Transport"],
        &[
            "AuthMethod",
            "Authentication",
            "Mechanism",
            "Method",
            "Type",
        ],
    )
    .unwrap_or_default();
    let reply_to = first_value(
        source,
        &["Mail Identity", "Identity"],
        &["ReplyTo", "Reply-To", "ReplyAddress"],
    )
    .unwrap_or_default();
    let organization = first_value(
        source,
        &["Mail Identity", "Identity"],
        &["Organization", "Organisation"],
    )
    .unwrap_or_default();
    let aliases = first_value(
        source,
        &["Mail Identity", "Identity"],
        &["Aliases", "Alias", "EmailAliases", "EmailAlias"],
    )
    .map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let folder_value = |keys: &[&str]| {
        first_value(
            source,
            &["Mail Account", "Mail Identity", "Camel", "Data Source"],
            keys,
        )
        .unwrap_or_default()
    };
    let inbox_folder = folder_value(&["InboxFolder", "Inbox", "InboxPath"]);
    let drafts_folder = folder_value(&["DraftsFolder", "Drafts", "DraftFolder"]);
    let sent_folder = folder_value(&["SentFolder", "Sent", "SentFolderPath"]);
    let archive_folder = folder_value(&["ArchiveFolder", "Archive", "ArchiveFolderPath"]);
    let trash_folder = folder_value(&["TrashFolder", "Trash", "TrashFolderPath"]);
    let junk_folder = folder_value(&["JunkFolder", "Junk", "SpamFolder", "Spam"]);
    let enabled = match first_value(source, &["Data Source", "Source"], &["Enabled", "Active"]) {
        None => true,
        Some(value) => match parse_setting_bool(&value) {
            Some(enabled) => enabled,
            None => {
                warnings
                    .push("ungültiger Aktivierungswert wurde als aktiviert übernommen".to_string());
                true
            }
        },
    };

    Ok((
        AccountProfile {
            id: format!("evolution-source:{uid}"),
            display_name,
            email,
            protocol,
            imap_host,
            smtp_host,
            tls_mode,
            username,
            imap_port,
            smtp_port,
            auth_method,
            reply_to,
            organization,
            aliases,
            inbox_folder,
            drafts_folder,
            sent_folder,
            archive_folder,
            trash_folder,
            junk_folder,
            enabled,
            ..Default::default()
        },
        warnings,
    ))
}

fn is_known_mail_backend(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    [
        "imapx",
        "imap",
        "imap4",
        "pop",
        "pop3",
        "exchange",
        "ews",
        "eas",
        "goa",
        "online-account",
        "onlineaccount",
    ]
    .iter()
    .any(|backend| value == *backend || value.contains(backend))
}

fn tls_mode_with_status(value: Option<&str>) -> (String, bool) {
    let Some(value) = value else {
        return ("tls".to_string(), true);
    };
    let normalized = value.trim().to_ascii_lowercase();
    let mode = tls_mode_from_value(value);
    let supported = matches!(
        normalized.as_str(),
        "starttls"
            | "start-tls"
            | "tls"
            | "ssl"
            | "ssl-on-alternate-port"
            | "true"
            | "false"
            | "none"
            | "no encryption"
            | "unencrypted"
    );
    (mode, supported)
}

fn parse_source_port(
    value: Option<&str>,
    fallback: u16,
    label: &str,
    warnings: &mut Vec<String>,
) -> u16 {
    let Some(value) = value else {
        return fallback;
    };
    match value.trim().parse::<u16>() {
        Ok(port) if port > 0 => port,
        _ => {
            warnings.push(format!(
                "ungültiger {label}-Port wurde durch den portablen Standard ersetzt"
            ));
            fallback
        }
    }
}

fn first_value(source: &SourceKeyFile, groups: &[&str], keys: &[&str]) -> Option<String> {
    source.get(groups, keys).and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn normalize_email(value: String) -> Option<String> {
    let value = value.trim();
    let value = value
        .rsplit_once('<')
        .and_then(|(_, value)| value.strip_suffix('>'))
        .unwrap_or(value)
        .trim();
    (value.contains('@') && !value.chars().any(char::is_whitespace)).then_some(value.to_string())
}

fn protocol_from_backend(backend: &str) -> AccountProtocol {
    let backend = backend.to_ascii_lowercase();
    if backend.contains("exchange") || backend.contains("ews") || backend.contains("eas") {
        AccountProtocol::Exchange
    } else if backend.contains("pop") {
        AccountProtocol::Pop3
    } else if backend.contains("goa") || backend.contains("online-account") {
        AccountProtocol::Goa
    } else {
        AccountProtocol::ImapSmtp
    }
}

fn tls_mode_from_value(value: &str) -> String {
    let value = value.to_ascii_lowercase();
    if value.contains("starttls") || value.contains("start-tls") {
        "starttls".to_string()
    } else if value == "false"
        || value == "none"
        || value.contains("no encryption")
        || value.contains("unencrypted")
    {
        "none".to_string()
    } else {
        "tls".to_string()
    }
}

fn parse_bool(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "0" | "no" | "disabled"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set01_migration_gaps_have_positive_and_negative_fixtures() {
        let positive = [
            ("switcher-location", "'bottom-left'", "bottom-left"),
            ("mail-sender-column-width", "@i 240", "240"),
            ("mail-show-security-details", "@b true", "true"),
            ("mail-show-all-headers", "@b true", "true"),
            ("mail-show-statusbar", "@b false", "false"),
            (
                "local-mail-directory",
                "@s '/portable/mail'",
                "/portable/mail",
            ),
            (
                "local-calendar-directory",
                "@s '/portable/calendar'",
                "/portable/calendar",
            ),
            (
                "local-contact-directory",
                "@s '/portable/contacts'",
                "/portable/contacts",
            ),
        ];
        for (key, raw_value, expected) in positive {
            let parsed = parse_evolution_settings(
                &format!("[org/gnome/evolution]\n{key}={raw_value}\n"),
                Path::new("set-01-positive.ini"),
            );
            assert_eq!(
                parsed
                    .settings
                    .iter()
                    .find(|setting| setting.key == key)
                    .map(|setting| setting.value.as_str()),
                Some(expected),
                "positive fixture for {key}"
            );
        }
        let upstream_aliases = parse_evolution_settings(
            "[org/gnome/evolution/mail]\nshow-security-details=@b true\nshow-all-headers=@b true\n[org/gnome/evolution/shell]\nstatusbar-visible=@b false\n",
            Path::new("set-01-upstream-aliases.ini"),
        );
        assert!(upstream_aliases.settings.contains(&ImportedSetting {
            key: "mail-show-security-details".to_string(),
            value: "true".to_string(),
        }));
        assert!(upstream_aliases.settings.contains(&ImportedSetting {
            key: "mail-show-all-headers".to_string(),
            value: "true".to_string(),
        }));
        assert!(upstream_aliases.settings.contains(&ImportedSetting {
            key: "mail-show-statusbar".to_string(),
            value: "false".to_string(),
        }));

        let negative = [
            ("switcher-location", "'sidebar'"),
            ("mail-sender-column-width", "@i 79"),
            ("mail-show-security-details", "@s 'true'"),
            ("mail-show-all-headers", "@s 'true'"),
            ("mail-show-statusbar", "@s 'false'"),
            ("local-mail-directory", "@i 7"),
            ("local-calendar-directory", "@i 7"),
            ("local-contact-directory", "@i 7"),
        ];
        for (key, raw_value) in negative {
            let parsed = parse_evolution_settings(
                &format!("[org/gnome/evolution]\n{key}={raw_value}\n"),
                Path::new("set-01-negative.ini"),
            );
            assert!(!parsed.settings.iter().any(|setting| setting.key == key));
            assert!(
                parsed.warnings.iter().any(|warning| warning.contains(key)),
                "negative fixture for {key} should be diagnosable"
            );
        }
    }

    #[test]
    fn migration_parser_rejects_truncated_values_and_reports_type_errors_without_values() {
        let parsed = parse_evolution_settings(
            "[org/gnome/evolution]\nmail-show-statusbar=@b\nlocal-mail-directory=@s '/tmp/unterminated\nmail-sender-column-width=@s '240'\n",
            Path::new("set-01-malformed.ini"),
        );
        assert!(parsed.settings.is_empty());
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("mail-show-statusbar"))
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("local-mail-directory"))
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("mail-sender-column-width"))
        );
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("unterminated"))
        );
    }

    #[test]
    fn migration_parser_preserves_untyped_text_unicode_and_hides_profile_paths() {
        let profile_path = Path::new("/private/secret-profile/with credentials/evolution.ini");
        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/tasks]
task-category-filter=Work Team
task-category-filter='Gr\u00fcne Gruppe'
unknown-setting='value'
"#,
            profile_path,
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-category-filter".to_string(),
            value: "Work Team".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-category-filter".to_string(),
            value: "Grüne Gruppe".to_string(),
        }));
        let warning_text = parsed.warnings.join(" ");
        assert!(warning_text.contains("Evolution-Profil"));
        assert!(!warning_text.contains("secret-profile"));
        assert!(!warning_text.contains("with credentials"));
    }

    #[test]
    fn parses_exported_settings_without_retaining_secrets() {
        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/mail]
mail-layout='classic'
show-preview=true
threading=false
show-headers=[('From', true), ('X-Project', false)]

[org/gnome/evolution/calendar]
use-12-hour-format=true
show-week-numbers=true
show-tag-vpane=false
tag-vpane-position=380

[org/gnome/evolution/account]
password='must-not-be-retained'
oauth2-access-token='must-not-be-retained'
unknown-setting='ignored'
"#,
            Path::new("evolution-settings.ini"),
        );
        assert_eq!(parsed.ignored_secret_fields, 2);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("unknown-setting"))
        );
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("ignored")
                    || warning.contains("must-not-be-retained"))
        );
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "mail-layout".to_string(),
            value: "classic".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "show-message-preview".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "mail-threading".to_string(),
            value: "false".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "mail-normal-headers".to_string(),
            value: "[('From', true), ('X-Project', false)]".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "time-format".to_string(),
            value: "12h".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-show-tag-vpane".to_string(),
            value: "false".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-tag-vpane-position".to_string(),
            value: "380".to_string(),
        }));
        assert!(
            !parsed
                .settings
                .iter()
                .any(|setting| setting.key == "password")
        );
    }

    #[test]
    fn calendar_view_and_timezone_upstream_migration_parities() {
        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/calendar]
calendar-view='year'
time-divisions=@i 15
compress-weekend=@b false
month-scroll-by-week=@b false
day-second-zone='America/New_York'
day-second-zones=['Europe/Berlin', 'UTC']
"#,
            Path::new("evolution-calendar-settings.ini"),
        );
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-view".to_string(),
            value: "year".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-time-divisions".to_string(),
            value: "15".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-compress-weekend".to_string(),
            value: "false".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-month-scroll-by-week".to_string(),
            value: "false".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-second-zone".to_string(),
            value: "America/New_York".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-second-zones".to_string(),
            value: "Europe/Berlin,UTC".to_string(),
        }));

        let list_view_parsed = parse_evolution_settings(
            "[org/gnome/evolution/calendar]\ncalendar-view='list'\n",
            Path::new("calendar-list-view.ini"),
        );
        assert!(list_view_parsed.settings.contains(&ImportedSetting {
            key: "calendar-view".to_string(),
            value: "list".to_string(),
        }));

        let negative = parse_evolution_settings(
            r#"
[org/gnome/evolution/calendar]
time-divisions=@i 25
calendar-view='decade'
day-second-zone='bad zone; danger'
"#,
            Path::new("calendar-negative.ini"),
        );
        assert!(
            !negative
                .settings
                .iter()
                .any(|s| s.key == "calendar-time-divisions")
        );
        assert!(!negative.settings.iter().any(|s| s.key == "calendar-view"));
        assert_eq!(negative.warnings.len(), 4);
        assert!(negative.warnings[0].contains("calendar-time-divisions"));
        assert!(negative.warnings[1].contains("calendar-view"));
        assert!(negative.warnings[2].contains("calendar-second-zone"));
        assert!(negative.warnings[3].contains("keine unterstützten nicht-geheimen"));
    }

    #[test]
    fn source_and_group_settings_upstream_migration_parities() {
        let parsed = parse_evolution_settings(
            r#"
[SourceSelector]
Calendar-hidden-groups=@as ['Group X', 'Group Y']
Calendar-groups-order=['Group 1', 'Group 2', 'Group 3']
Task List-hidden-groups='TaskGrp1,TaskGrp2'
Task List-groups-order=['TaskOrderA', 'TaskOrderB']
Memo List-hidden-groups=@as ['MemoGrp1']
Memo List-groups-order=['MemoOrderA']
Address Book-hidden-groups=['BookGrp1']
Address Book-groups-order=['BookOrder1', 'BookOrder2']

[org/gnome/evolution/calendar]
task-hidden-sources=@as ['task-src-1', 'task-src-2']
memo-hidden-sources='memo-src-1,memo-src-2'
addressbook-hidden-sources=['book-src-1']
default-calendar='cal-def'
default-task-list='task-def'
default-memo-list='memo-def'
default-address-book='book-def'
"#,
            Path::new("evolution-sources-setup.ini"),
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-hidden-groups".to_string(),
            value: "Group X,Group Y".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-groups-order".to_string(),
            value: "Group 1,Group 2,Group 3".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-hidden-groups".to_string(),
            value: "TaskGrp1,TaskGrp2".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-groups-order".to_string(),
            value: "TaskOrderA,TaskOrderB".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "memo-hidden-groups".to_string(),
            value: "MemoGrp1".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "memo-groups-order".to_string(),
            value: "MemoOrderA".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "addressbook-hidden-groups".to_string(),
            value: "BookGrp1".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "addressbook-groups-order".to_string(),
            value: "BookOrder1,BookOrder2".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-hidden-sources".to_string(),
            value: "task-src-1,task-src-2".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "memo-hidden-sources".to_string(),
            value: "memo-src-1,memo-src-2".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "addressbook-hidden-sources".to_string(),
            value: "book-src-1".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-calendar-source".to_string(),
            value: "cal-def".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-task-source".to_string(),
            value: "task-def".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-memo-source".to_string(),
            value: "memo-def".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-address-book-source".to_string(),
            value: "book-def".to_string(),
        }));
    }

    #[test]
    fn reminder_and_task_filter_settings_upstream_migration_parities() {
        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/calendar]
use-default-reminder=true
default-reminder-interval=@i 30
default-reminder-units='hours'
use-ba-reminder=true
ba-reminder-interval=2
ba-reminder-units='days'
hide-completed-tasks=true
hide-completed-tasks-value=@i 5
hide-completed-tasks-units='days'
hide-cancelled-tasks=true
hide-cancelled-events=true
"#,
            Path::new("evolution-reminders-setup.ini"),
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "use-default-reminder".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-reminder-interval".to_string(),
            value: "30".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-reminder-units".to_string(),
            value: "hours".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "use-ba-reminder".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "ba-reminder-interval".to_string(),
            value: "2".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "ba-reminder-units".to_string(),
            value: "days".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "hide-completed-tasks".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "hide-completed-tasks-value".to_string(),
            value: "5".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "hide-completed-tasks-units".to_string(),
            value: "days".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "hide-cancelled-tasks".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "hide-cancelled-events".to_string(),
            value: "true".to_string(),
        }));
    }

    #[test]
    fn addressbook_settings_upstream_migration_parities() {
        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/addressbook]
primary-addressbook='addr-src-1'
address-formatting=true
completion-minimum-query-length=@i 4
completion-show-address=true
completion-book-sources='addr-src-1,addr-src-2'
preview-show-maps=true
preview-home-before-work=true
open-map-target='google'
open-map-prefer-local=false
show-preview=true
layout=1
hpane-position=@i 250
vpane-position=@i 450
editor-show-contact-phone=true
editor-show-contact-sip=true
editor-show-contact-im=true
editor-show-mailing-home=true
editor-show-mailing-work=true
editor-show-mailing-other=true
editor-show-personal-web=true
editor-show-personal-job=true
editor-show-personal-dates=true
editor-show-personal-misc=true
editor-show-notes=true
editor-show-certs=true
"#,
            Path::new("evolution-addressbook-setup.ini"),
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "default-address-book-source".to_string(),
            value: "addr-src-1".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-address-formatting".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-completion-minimum-query-length".to_string(),
            value: "4".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-completion-show-address".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-completion-book-sources".to_string(),
            value: "addr-src-1,addr-src-2".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-preview-show-maps".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-preview-home-before-work".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-open-map-target".to_string(),
            value: "google".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-open-map-prefer-local".to_string(),
            value: "false".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-preview-visible".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-layout".to_string(),
            value: "vertical".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-hpane-position".to_string(),
            value: "250".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-vpane-position".to_string(),
            value: "450".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-phone".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-sip".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-im".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-home-address".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-work-address".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-other-address".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-web".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-job".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-dates".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-misc".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-notes".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "contact-editor-show-certs".to_string(),
            value: "true".to_string(),
        }));
    }

    #[test]
    fn normalizes_portable_setting_types_and_reports_invalid_values() {
        let parsed = parse_evolution_settings(
            r#"
[/org/gnome/evolution/mail]
show-from=@b true
mail-sort-field='received'
mail-layout='classic'
mail-sync-message-limit=@i 100
window-width=@i 40
mail-threading='not-a-bool'
calendar-hidden-sources=@as ['calendar-work', 'calendar-home']
"#,
            Path::new("dconf.dump"),
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "mail-show-sender".to_string(),
            value: "true".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "mail-sort-field".to_string(),
            value: "date".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "mail-sync-message-limit".to_string(),
            value: "100".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "calendar-hidden-sources".to_string(),
            value: "calendar-work,calendar-home".to_string(),
        }));
        assert!(
            !parsed
                .settings
                .iter()
                .any(|setting| setting.key == "window-width" || setting.key == "mail-threading")
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("window-width"))
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("mail-threading"))
        );
    }

    #[test]
    fn migrates_task_filter_values_and_rejects_unsupported_modes() {
        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/tasks]
task-filter='active'
task-category-filter='  Work Team  '
task-search-mode='ADVANCED'
task-search-mode='bogus'
"#,
            Path::new("evolution-task-settings.ini"),
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-filter".to_string(),
            value: "next-365".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-category-filter".to_string(),
            value: "Work Team".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-search-mode".to_string(),
            value: "advanced".to_string(),
        }));
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("task-search-mode"))
        );
    }

    #[test]
    fn migrates_upstream_task_preview_visibility_and_layout() {
        for key in ["task-preview-visible", "task-preview-orientation"] {
            assert!(is_supported_migration_key(key), "{key}");
        }

        let parsed = parse_evolution_settings(
            r#"
[org/gnome/evolution/calendar]
show-task-preview=false
task-layout=1
task-hpane-position=275
task-vpane-position=525
task-layout=7
"#,
            Path::new("evolution-calendar-settings.ini"),
        );

        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-preview-visible".to_string(),
            value: "false".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-preview-orientation".to_string(),
            value: "vertical".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-hpane-position".to_string(),
            value: "275".to_string(),
        }));
        assert!(parsed.settings.contains(&ImportedSetting {
            key: "task-vpane-position".to_string(),
            value: "525".to_string(),
        }));
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("task-layout"))
        );
        assert!(normalize_imported_setting_value("task-preview-visible", "maybe").is_err());
        assert!(normalize_imported_setting_value("task-preview-orientation", "diagonal").is_err());
        assert!(normalize_imported_setting_value("task-hpane-position", "50").is_err());
    }

    #[test]
    fn memo_surface_migration_values_are_strict_and_canonical() {
        for key in [
            "memo-search-mode",
            "memo-category-filter",
            "memo-source-filter",
            "memo-preview-visible",
            "memo-preview-orientation",
        ] {
            assert!(is_supported_migration_key(key), "{key}");
        }
        assert_eq!(
            normalize_imported_setting_value("memo-search-mode", " DESCRIPTION ").unwrap(),
            "description"
        );
        assert_eq!(
            normalize_imported_setting_value("memo-category-filter", " without ").unwrap(),
            "without"
        );
        assert_eq!(
            normalize_imported_setting_value("memo-source-filter", " source-a ").unwrap(),
            "source-a"
        );
        assert_eq!(
            normalize_imported_setting_value("memo-preview-orientation", "VERTICAL").unwrap(),
            "vertical"
        );
        assert!(normalize_imported_setting_value("memo-category-filter", "").is_err());
        assert!(normalize_imported_setting_value("memo-category-filter", "\"quoted\"").is_err());
        assert!(normalize_imported_setting_value("memo-source-filter", "source a").is_err());
        assert!(normalize_imported_setting_value("memo-preview-visible", "maybe").is_err());
    }

    #[test]
    fn mail_reading_policies_migration_normalization() {
        assert_eq!(
            normalize_imported_setting_value("mail-image-loading-policy", "always").unwrap(),
            "always"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-image-loading-policy", "CONTACTS").unwrap(),
            "sometimes"
        );
        assert!(normalize_imported_setting_value("mail-image-loading-policy", "invalid").is_err());

        assert_eq!(
            normalize_imported_setting_value("mail-mark-seen", "true").unwrap(),
            "true"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-mark-seen-timeout", "2500").unwrap(),
            "2500"
        );
        assert!(normalize_imported_setting_value("mail-mark-seen-timeout", "90000").is_err());

        assert_eq!(
            normalize_imported_setting_value("mail-caret-mode", "false").unwrap(),
            "false"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-magic-spacebar", "1").unwrap(),
            "true"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-citation-highlighting", "yes").unwrap(),
            "true"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-citation-color-1", "#abcdef").unwrap(),
            "#ABCDEF"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-citation-color-2", "#123").unwrap(),
            "#112233"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-headers-collapsed", "0").unwrap(),
            "false"
        );
        assert_eq!(
            normalize_imported_setting_value("mail-show-real-date", "true").unwrap(),
            "true"
        );
    }

    #[test]
    fn parses_mail_source_without_retaining_secret_keys() {
        let source = parse_source(
            r#"
[Data Source]
UID=mail-source-1
DisplayName=Gr\u00fcne\sMailbox
Enabled=true

[Mail Identity]
Name=Person
Address=Person <person@example.org>

[Mail Account]
BackendName=imapx

[Authentication]
Host=imap.example.org
User=person@example.org
Port=143
AuthMethod=plain
Password=must-not-be-imported
ApiKey=must-not-be-imported

[Mail Submission]
Host=smtp.example.org
Port=587

[Security]
Method=STARTTLS
OAuth2AccessToken=must-not-be-imported

[Mail Identity]
ReplyTo=reply@example.org
Organization=Example Org
Aliases=alias@example.org, second@example.org

[Mail Account]
InboxFolder=INBOX
DraftsFolder=Drafts
SentFolder=Sent
ArchiveFolder=Archive
TrashFolder=Trash
JunkFolder=Junk
"#,
        )
        .expect("source should parse");
        assert!(source.get(&["Authentication"], &["Password"]).is_none());
        assert!(source.get(&["Security"], &["OAuth2AccessToken"]).is_none());
        assert_eq!(source.ignored_secret_keys.len(), 3);

        let profile = profile_from_source(Path::new("mail-source-1.source"), &source)
            .expect("mail profile should be detected");
        assert_eq!(profile.id, "evolution-source:mail-source-1");
        assert_eq!(profile.display_name, "Grüne Mailbox");
        assert_eq!(profile.email, "person@example.org");
        assert_eq!(profile.imap_host, "imap.example.org");
        assert_eq!(profile.smtp_host, "smtp.example.org");
        assert_eq!(profile.tls_mode, "starttls");
        assert_eq!(profile.username, "person@example.org");
        assert_eq!(profile.imap_port, 143);
        assert_eq!(profile.smtp_port, 587);
        assert_eq!(profile.auth_method, "plain");
        assert_eq!(profile.reply_to, "reply@example.org");
        assert_eq!(profile.organization, "Example Org");
        assert_eq!(
            profile.aliases,
            vec!["alias@example.org", "second@example.org"]
        );
        assert_eq!(profile.inbox_folder, "INBOX");
        assert_eq!(profile.drafts_folder, "Drafts");
        assert_eq!(profile.sent_folder, "Sent");
        assert_eq!(profile.archive_folder, "Archive");
        assert_eq!(profile.trash_folder, "Trash");
        assert_eq!(profile.junk_folder, "Junk");
        assert!(profile.enabled);
    }

    #[test]
    fn reports_account_fallbacks_without_exposing_source_values() {
        let source = parse_source(
            r#"
[Data Source]
UID=mail-source-unsupported
Enabled=maybe

[Mail Identity]
Address=person@example.org

[Mail Account]
BackendName=unsupported-backend

[Authentication]
Host=imap.example.org
Port=not-a-port

[Security]
Method=unknown-encryption
Password=must-not-be-retained
"#,
        )
        .expect("source should parse");
        let (profile, warnings) =
            profile_from_source_with_warnings(Path::new("mail-source-unsupported.source"), &source)
                .expect("account identity should remain importable");

        assert_eq!(profile.protocol, AccountProtocol::ImapSmtp);
        assert_eq!(profile.imap_port, 993);
        assert_eq!(profile.smtp_port, 465);
        assert!(warnings.iter().any(|warning| warning.contains("Backend")));
        assert!(warnings.iter().any(|warning| warning.contains("IMAP-Port")));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Verschlüsselungsart"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Aktivierungswert"))
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("must-not-be-retained"))
        );
        assert!(
            !source
                .warnings
                .iter()
                .any(|warning| warning.contains("must-not-be-retained"))
        );
    }

    #[test]
    fn ignores_non_mail_sources_and_maps_protocols() {
        let calendar = parse_source(
            "[Data Source]\nUID=calendar-1\nDisplayName=Calendar\nBackendName=caldav\n",
        )
        .expect("calendar source should parse");
        assert!(profile_from_source(Path::new("calendar.source"), &calendar).is_err());

        let exchange = parse_source(
            "[Data Source]\nUID=exchange-1\nDisplayName=Work\nBackendName=exchange\n[Mail Identity]\nAddress=work@example.org\n",
        )
        .expect("exchange source should parse");
        assert_eq!(
            profile_from_source(Path::new("exchange.source"), &exchange)
                .expect("exchange profile should be detected")
                .protocol,
            AccountProtocol::Exchange
        );
    }

    #[test]
    fn a_combined_source_is_cached_for_each_supported_extension() {
        let source = parse_source(
            "[Data Source]\nUID=combined-1\nDisplayName=Arbeitskonto\n[Mail Account]\nBackendName=imapx\n[Calendar]\nBackendName=caldav\n[Address Book]\nBackendName=ldap\n",
        )
        .expect("combined source should parse");
        let cached = eds_sources_from_keyfile(Path::new("combined.source"), &source);

        assert_eq!(cached.len(), 3);
        assert!(cached.iter().any(|(kind, source)| {
            *kind == EdsSourceKind::Mail && source.backend_name == "imapx"
        }));
        assert!(cached.iter().any(|(kind, source)| {
            *kind == EdsSourceKind::Calendar && source.backend_name == "caldav"
        }));
        assert!(cached.iter().any(|(kind, source)| {
            *kind == EdsSourceKind::AddressBook && source.backend_name == "ldap"
        }));
    }

    #[test]
    fn imports_nested_sources_into_the_app_owned_store() {
        let root = temporary_directory("migration");
        let profile_root = root.join("profile").join("sources");
        fs::create_dir_all(&profile_root).expect("source directory should be created");
        fs::write(
            profile_root.join("mail.source"),
            "[Data Source]\nUID=mail-1\nDisplayName=Mail\n[Mail Identity]\nAddress=person@example.org\n",
        )
        .expect("mail source should be written");
        fs::write(
            profile_root.join("calendar.source"),
            "[Data Source]\nUID=calendar-1\nDisplayName=Calendar\nBackendName=caldav\n",
        )
        .expect("calendar source should be written");

        let store = AccountConfigStore::new(root.join("accounts.json"));
        let report = import_evolution_sources(root.join("profile"), &store)
            .expect("profile import should succeed");
        assert_eq!(report.discovered_sources, 2);
        assert_eq!(report.imported_accounts, 1);
        assert_eq!(report.skipped_sources, 1);
        assert_eq!(store.load().len(), 1);
        assert_eq!(store.load()[0].id, "evolution-source:mail-1");
        assert_eq!(report.warnings.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn includes_non_secret_settings_from_an_explicit_profile_export() {
        let root = temporary_directory("migration-settings");
        let profile_root = root.join("profile");
        fs::create_dir_all(&profile_root).expect("profile directory should be created");
        fs::write(
            profile_root.join("evolution-settings.ini"),
            "[org/gnome/evolution/mail]\nmail-layout='classic'\nshow-preview=true\npassword='ignored'\n",
        )
        .expect("settings export should be written");

        let store = AccountConfigStore::new(root.join("accounts.json"));
        let report = import_evolution_sources(&profile_root, &store)
            .expect("settings export should be read");
        assert_eq!(report.discovered_settings_files, 1);
        assert_eq!(report.imported_settings, 2);
        assert_eq!(report.ignored_secret_fields, 1);
        assert!(
            report
                .settings
                .iter()
                .any(|setting| setting.key == "mail-layout" && setting.value == "classic")
        );
        assert!(
            report
                .settings
                .iter()
                .any(|setting| setting.key == "show-message-preview" && setting.value == "true")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_valid_local_calendar_and_contact_files_without_touching_profile() {
        let root = temporary_directory("migration-local-data");
        let profile_root = root.join("profile");
        let calendar_source = profile_root.join("calendar/system.ics");
        let contact_source = profile_root.join("addressbook/contacts.vcf");
        let mail_source = profile_root.join("mail/local/cur/message-1");
        let nested_mail_source = profile_root.join("mail/local/.Projects/.2026/cur/message-2");
        let empty_mail_folder = profile_root.join("mail/local/.Empty");
        fs::create_dir_all(calendar_source.parent().unwrap())
            .expect("calendar profile directory should be created");
        fs::create_dir_all(contact_source.parent().unwrap())
            .expect("address book profile directory should be created");
        fs::create_dir_all(mail_source.parent().unwrap())
            .expect("mail profile directory should be created");
        fs::create_dir_all(nested_mail_source.parent().unwrap())
            .expect("nested mail profile directory should be created");
        fs::create_dir_all(&empty_mail_folder).expect("empty mail folder should be created");
        fs::write(
            &calendar_source,
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n",
        )
        .expect("calendar profile file should be written");
        fs::write(
            &contact_source,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported\r\nEND:VCARD\r\n",
        )
        .expect("contact profile file should be written");
        fs::write(
            &mail_source,
            "From: sender@example.org\r\nTo: person@example.org\r\nSubject: Imported mail\r\n\r\nHello\r\n",
        )
        .expect("mail profile file should be written");
        fs::write(
            &nested_mail_source,
            "From: nested@example.org\r\nTo: person@example.org\r\nSubject: Nested mail\r\n\r\nNested\r\n",
        )
        .expect("nested mail profile file should be written");
        fs::write(profile_root.join("not-a-calendar.ics"), "plain text")
            .expect("invalid calendar fixture should be written");

        let calendar_destination = root.join("app/ical");
        let contact_destination = root.join("app/vcards");
        let calendar_store =
            CalendarStoreService::with_writable_calendar_dir(&calendar_destination);
        let contact_store = ContactStoreService::with_writable_contact_dir(&contact_destination);
        let mail_destination = root.join("app/mail");
        let mail_store = MailStoreService::with_mail_dir_and_state_path(
            &mail_destination,
            root.join("app/mail-state.json"),
        );
        let report = import_evolution_local_data(
            &profile_root,
            &calendar_store,
            &contact_store,
            &mail_store,
        )
        .expect("local profile data should import");

        assert_eq!(report.discovered_calendar_files, 1);
        assert_eq!(report.imported_calendar_files, 1);
        assert_eq!(report.discovered_contact_files, 1);
        assert_eq!(report.imported_contact_files, 1);
        assert_eq!(report.discovered_mail_files, 2);
        assert_eq!(report.imported_mail_files, 2);
        assert_eq!(report.imported_mail_folders, 3);
        assert!(calendar_destination.join("system.ics").is_file());
        assert!(contact_destination.join("contacts.vcf").is_file());
        assert!(mail_destination.join("cur/message-1").is_file());
        assert!(
            mail_destination
                .join(".Projects/.2026/cur/message-2")
                .is_file()
        );
        assert!(
            mail_store
                .local_folder_definitions()
                .contains("custom:Empty")
        );
        assert!(
            mail_store
                .load_local_records()
                .iter()
                .any(|message| message.folder_id == "custom:Projects/2026")
        );
        assert_eq!(
            fs::read_to_string(&calendar_source).unwrap(),
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n"
        );
        assert_eq!(
            fs::read_to_string(&contact_source).unwrap(),
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported\r\nEND:VCARD\r\n"
        );
        assert_eq!(
            fs::read_to_string(&mail_source).unwrap(),
            "From: sender@example.org\r\nTo: person@example.org\r\nSubject: Imported mail\r\n\r\nHello\r\n"
        );
        assert_eq!(
            fs::read_to_string(&nested_mail_source).unwrap(),
            "From: nested@example.org\r\nTo: person@example.org\r\nSubject: Nested mail\r\n\r\nNested\r\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_non_secret_calendar_and_address_book_sources_into_cache() {
        let root = temporary_directory("migration-source-cache");
        let profile_root = root.join("profile").join("sources");
        fs::create_dir_all(&profile_root).expect("source directory should be created");
        fs::write(
            profile_root.join("mail.source"),
            "[Data Source]\nUID=mail-1\nDisplayName=Mail\nEnabled=true\n[Mail Account]\nBackendName=imapx\n[Mail Identity]\nAddress=person@example.org\n",
        )
        .expect("mail source should be written");
        fs::write(
            profile_root.join("calendar.source"),
            "[Data Source]\nUID=calendar-1\nDisplayName=Kalender\nEnabled=false\n[Calendar]\nBackendName=caldav\nPassword=must-not-be-imported\n",
        )
        .expect("calendar source should be written");
        fs::write(
            profile_root.join("address-book.source"),
            "[Data Source]\nUID=book-1\nDisplayName=Kontakte\n[Address Book]\nBackendName=google\nOAuthToken=must-not-be-imported\n",
        )
        .expect("address book source should be written");

        let account_store = AccountConfigStore::new(root.join("accounts.json"));
        let filter_store = MailFilterStore::new(root.join("mail-filters.json"));
        let signature_store = SignatureStore::new(root.join("signatures.json"));
        let cache = EdsSourceCache::with_path(root.join("eds-sources.json"));
        let report = import_evolution_sources_with_stores_and_cache(
            &root.join("profile"),
            &account_store,
            &filter_store,
            &signature_store,
            &cache,
        )
        .expect("source cache import should succeed");

        assert_eq!(report.cached_mail_sources, 1);
        assert_eq!(report.cached_calendar_sources, 1);
        assert_eq!(report.cached_address_book_sources, 1);
        assert_eq!(cache.load(EdsSourceKind::Mail)[0].uid, "mail-1");
        assert_eq!(
            cache.load(EdsSourceKind::Calendar)[0].display_name,
            "Kalender"
        );
        assert!(!cache.load(EdsSourceKind::Calendar)[0].enabled);
        assert_eq!(
            cache.load(EdsSourceKind::AddressBook)[0].backend_name,
            "google"
        );
        let raw_cache = fs::read_to_string(root.join("eds-sources.json"))
            .expect("source cache should be readable");
        assert!(!raw_cache.contains("must-not-be-imported"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_supported_evolution_filters_without_losing_xml_text() {
        let root = temporary_directory("migration-filters");
        let profile_root = root.join("profile");
        fs::create_dir_all(profile_root.join("mail")).expect("profile should be created");
        fs::create_dir_all(profile_root.join("signatures")).expect("signatures should be created");
        fs::write(
            profile_root.join("signatures/work.txt"),
            "Viele Grüße\nPerson",
        )
        .expect("signature should be written");
        fs::write(
            profile_root.join("signatures/private.txt"),
            "Beste Grüße\nPerson",
        )
        .expect("second signature should be written");
        fs::write(
            profile_root.join("signature-list.xml"),
            r#"<signatures>
  <signature name="Arbeit"><filename>signatures/work.txt</filename><format>text/plain</format></signature>
  <signature><name>Privat</name><filename>signatures/private.txt</filename></signature>
</signatures>"#,
        )
        .expect("signature list should be written");
        fs::write(
            profile_root.join("mail/filters.xml"),
            r#"<?xml version="1.0"?>
<filteroptions>
  <ruleset>
    <rule name="Rechnungen" enabled="true" source="incoming" match="any">
      <part name="subject"><value name="is" type="string">Rechnung &amp; Vertrag</value></part>
      <part name="from"><value name="does-not-contain" type="string">spam.example.org</value></part>
      <part name="important"><value name="is" type="boolean">yes</value></part>
      <part name="date-received"><value name="after" type="date">2026-08-01</value></part>
      <action name="move"><value name="folder">archive</value></action>
      <action name="mark-read" />
      <action name="stop-processing" />
      <action name="important" />
      <action name="junk" />
    </rule>
    <rule name="Beim Senden markieren" enabled="false" source="outgoing">
      <part name="from"><value name="contains">example.org</value></part>
      <action name="set-flagged" />
    </rule>
  </ruleset>
</filteroptions>
"#,
        )
        .expect("filter file should be written");

        let account_store = AccountConfigStore::new(root.join("accounts.json"));
        let filter_store = MailFilterStore::new(root.join("mail-filters.json"));
        let signature_store = SignatureStore::new(root.join("signatures.json"));
        let report = import_evolution_sources_with_stores(
            &profile_root,
            &account_store,
            &filter_store,
            &signature_store,
        )
        .expect("profile filters should import");

        assert_eq!(report.discovered_filter_files, 1);
        assert_eq!(report.imported_filters, 2);
        assert_eq!(report.imported_signatures, 2);
        assert_eq!(
            report.signature_text.as_deref(),
            Some("Viele Grüße\nPerson")
        );
        assert_eq!(signature_store.load().len(), 2);
        assert_eq!(
            signature_store.active().map(|signature| signature.name),
            Some("Arbeit".to_string())
        );
        let rules = filter_store.load();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].condition.value, "Rechnung & Vertrag");
        assert_eq!(rules[0].condition.operator, FilterOperator::Equals);
        assert_eq!(rules[0].conditions[1].operator, FilterOperator::NotContains);
        assert_eq!(
            rules[0].action,
            FilterAction::MoveToFolder("archive".to_string())
        );
        assert_eq!(rules[0].conditions.len(), 4);
        assert_eq!(rules[0].conditions[2].field, FilterField::Important);
        assert_eq!(rules[0].conditions[2].value, "yes");
        assert_eq!(rules[0].conditions[3].field, FilterField::Date);
        assert_eq!(rules[0].conditions[3].operator, FilterOperator::After);
        assert_eq!(rules[0].conditions[3].value, "2026-08-01");
        assert_eq!(rules[0].actions.len(), 5);
        assert_eq!(rules[0].actions[2], FilterAction::StopProcessing);
        assert_eq!(rules[0].actions[3], FilterAction::SetImportant);
        assert_eq!(rules[0].actions[4], FilterAction::SetJunk);
        assert!(!rules[0].match_all);
        assert_eq!(rules[1].scope, FilterScope::Outgoing);
        assert!(!rules[1].enabled);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reports_unsupported_filter_parts_and_actions_without_dropping_supported_rule() {
        let root = temporary_directory("migration-filter-warnings");
        let profile_root = root.join("profile");
        fs::create_dir_all(&profile_root).expect("profile should be created");
        fs::write(
            profile_root.join("filters.xml"),
            r#"<filteroptions><rule name="Teilweise unterstützt">
              <part name="subject"><value name="regex">Rechnung</value></part>
              <part name="score"><value>10</value></part>
              <action name="mark-read" />
              <action name="quarantine"><value>vault</value></action>
            </rule></filteroptions>"#,
        )
        .expect("filter file should be written");

        let account_store = AccountConfigStore::new(root.join("accounts.json"));
        let filter_store = MailFilterStore::new(root.join("mail-filters.json"));
        let signature_store = SignatureStore::new(root.join("signatures.json"));
        let report = import_evolution_sources_with_stores(
            &profile_root,
            &account_store,
            &filter_store,
            &signature_store,
        )
        .expect("filter import should succeed");

        assert_eq!(report.imported_filters, 1);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("score"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("quarantine"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("regex"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_html_signatures_as_sanitized_html_profiles() {
        let root = temporary_directory("migration-html-signature");
        let profile_root = root.join("profile");
        fs::create_dir_all(profile_root.join("signatures"))
            .expect("signature directory should be created");
        fs::write(
            profile_root.join("signatures/html.html"),
            r#"<p><strong>Viele Grüße</strong></p><script>alert('x')</script><p onclick="evil()">Person</p>"#,
        )
        .expect("HTML signature should be written");
        fs::write(
            profile_root.join("signature-list.xml"),
            r#"<signatures><signature name="HTML" format="text/html"><filename>signatures/html.html</filename></signature></signatures>"#,
        )
        .expect("signature list should be written");

        let account_store = AccountConfigStore::new(root.join("accounts.json"));
        let filter_store = MailFilterStore::new(root.join("mail-filters.json"));
        let signature_store = SignatureStore::new(root.join("signatures.json"));
        let report = import_evolution_sources_with_stores(
            &profile_root,
            &account_store,
            &filter_store,
            &signature_store,
        )
        .expect("HTML signature should import");

        assert_eq!(report.imported_signatures, 1);
        let signature = signature_store
            .active()
            .expect("signature should be active");
        assert_eq!(signature.format, "text/html");
        assert!(signature.text.contains("<strong>Viele Grüße</strong>"));
        assert!(!signature.text.contains("script"));
        assert!(!signature.text.contains("onclick"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_import_filters_with_extended_actions() {
        let root = temporary_directory("extended-filters");
        let profile_root = root.join(".config").join("evolution");
        fs::create_dir_all(&profile_root).expect("profile directory should be created");

        let xml = r#"<?xml version="1.0"?>
<filteroptions>
  <ruleset>
    <rule grouping="all" source="incoming" name="Extended Actions Rule">
      <partset>
        <part name="subject">
          <value name="contains" type="string">Alert</value>
        </part>
      </partset>
      <actionset>
        <action name="set-color">
          <value name="color" type="string">#ff0000</value>
        </action>
        <action name="set-score">
          <value name="score" type="integer">42</value>
        </action>
        <action name="beep"/>
        <action name="play-sound">
          <value name="sound" type="string">/path/to/alert.wav</value>
        </action>
        <action name="run-program">
          <value name="cmd" type="string">notify-send Alert</value>
        </action>
        <action name="pipe-to-program">
          <value name="cmd" type="string">proc-mail</value>
        </action>
        <action name="forward-to">
          <value name="addr" type="string">team@example.com</value>
        </action>
      </actionset>
    </rule>
  </ruleset>
</filteroptions>"#;

        fs::write(profile_root.join("filters.xml"), xml).expect("filters.xml written");

        let account_store = AccountConfigStore::new(root.join("accounts.json"));
        let filter_store = MailFilterStore::new(root.join("mail-filters.json"));
        let signature_store = SignatureStore::new(root.join("signatures.json"));

        let report = import_evolution_sources_with_stores(
            &profile_root,
            &account_store,
            &filter_store,
            &signature_store,
        )
        .expect("extended actions filters should import");

        assert_eq!(report.imported_filters, 1);
        let rules = filter_store.load();
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!(rule.name, "Extended Actions Rule");
        assert_eq!(rule.action, FilterAction::AssignColor("#ff0000".to_string()));
        assert_eq!(rule.actions.len(), 7);
        assert_eq!(rule.actions[1], FilterAction::AssignScore(42));
        assert_eq!(rule.actions[2], FilterAction::Beep);
        assert_eq!(rule.actions[3], FilterAction::PlaySound("/path/to/alert.wav".to_string()));
        assert_eq!(rule.actions[4], FilterAction::RunProgram("notify-send Alert".to_string()));
        assert_eq!(rule.actions[5], FilterAction::PipeToProgram("proc-mail".to_string()));
        assert_eq!(rule.actions[6], FilterAction::ForwardTo("team@example.com".to_string()));

        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "meridian-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
