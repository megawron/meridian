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
use super::eds_service::{EdsSource, EdsSourceKind};
use super::eds_source_cache_service::EdsSourceCache;
use super::html_sanitizer::sanitize_html;
use super::mail_filter_service::{
    FilterAction, FilterCondition, FilterField, FilterOperator, FilterScope, MailFilterRule,
    MailFilterStore,
};
use super::signature_store_service::{SignatureProfile, SignatureStore};
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
    pub signature_text: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct SourceKeyFile {
    groups: BTreeMap<String, BTreeMap<String, String>>,
    ignored_secret_keys: Vec<String>,
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

/// Import source metadata, portable filters, and all safe text signatures into
/// explicitly supplied app-owned stores.
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
        let result = fs::read_to_string(&path)
            .map_err(|error| format!("Datei konnte nicht gelesen werden: {error}"))
            .and_then(|raw| parse_source(&raw).map_err(|error| error.to_string()))
            .and_then(|source| {
                report.ignored_secret_fields += source.ignored_secret_keys.len();
                for (kind, eds_source) in eds_sources_from_keyfile(&path, &source) {
                    match kind {
                        EdsSourceKind::Mail => cached_mail_sources.push(eds_source),
                        EdsSourceKind::Calendar => cached_calendar_sources.push(eds_source),
                        EdsSourceKind::AddressBook => cached_address_book_sources.push(eds_source),
                    }
                }
                profile_from_source(&path, &source)
            });

        match result {
            Ok(profile) => {
                store.upsert(profile).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "Profil aus {} konnte nicht gespeichert werden: {error}",
                            path.display()
                        ),
                    )
                })?;
                report.imported_accounts += 1;
            }
            Err(reason) => {
                report.skipped_sources += 1;
                report
                    .warnings
                    .push(format!("{}: {reason}", path.display()));
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
                    path.display()
                ));
                continue;
            }
        };
        let rules = match parse_evolution_filters(&raw, &path) {
            Ok(rules) => rules,
            Err(reason) => {
                report.warnings.push(format!(
                    "{}: Filter konnten nicht importiert werden: {reason}",
                    path.display()
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
                    path.display()
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
                    list_path.display()
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
                    list_path.display()
                ));
                continue;
            };
            let contents = match fs::read_to_string(&signature_path) {
                Ok(contents) => contents,
                Err(error) => {
                    report.warnings.push(format!(
                        "{}: Signatur konnte nicht gelesen werden: {error}",
                        signature_path.display()
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
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            list_path.to_string_lossy().hash(&mut hasher);
            signature_index.hash(&mut hasher);
            relative_path.hash(&mut hasher);
            name.hash(&mut hasher);
            let signature = SignatureProfile {
                id: format!("evolution-signature-{:016x}", hasher.finish()),
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
                    signature_path.display()
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
        let rule_name = attribute_value(&opening_tag, "name")
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("Regel {}", index + 1));
        for (part_tag, _) in extract_xml_blocks(&body, "part") {
            let name = attribute_value(&part_tag, "name").unwrap_or_default();
            if !name.trim().is_empty() && filter_field_from_evolution_name(&name).is_none() {
                warnings.push(format!(
                    "{}: Bedingung „{}“ in „{}“ wird portabel nicht ausgewertet",
                    path.display(),
                    name,
                    rule_name
                ));
            }
        }
        for (part_tag, part_body) in extract_xml_blocks(&body, "part") {
            if filter_field_from_evolution_name(
                &attribute_value(&part_tag, "name").unwrap_or_default(),
            )
            .is_none()
            {
                continue;
            }
            let Some((value_tag, _)) = extract_xml_blocks(&part_body, "value").into_iter().next()
            else {
                continue;
            };
            let operator_name = attribute_value(&value_tag, "name").unwrap_or_default();
            if !operator_name.trim().is_empty()
                && filter_operator_from_evolution_name(&operator_name).is_none()
            {
                warnings.push(format!(
                    "{}: Vergleich „{}“ in „{}“ wird portabel als „enthält“ importiert",
                    path.display(),
                    operator_name,
                    rule_name
                ));
            }
        }
        for (action_tag, _) in extract_xml_blocks(&body, "action") {
            let name = attribute_value(&action_tag, "name").unwrap_or_default();
            let recognition_target = if matches!(
                name.to_ascii_lowercase().as_str(),
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
            ) {
                "placeholder".to_string()
            } else {
                String::new()
            };
            if !name.trim().is_empty()
                && filter_action_from_evolution_name(&name, recognition_target).is_none()
            {
                warnings.push(format!(
                    "{}: Aktion „{}“ in „{}“ wird portabel nicht ausgeführt",
                    path.display(),
                    name,
                    rule_name
                ));
            }
        }
    }
    warnings
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

    for line in raw.lines() {
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
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
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
        "passphrase",
        "secret",
        "token",
        "credential",
        "privatekey",
        "private-key",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn unescape_keyfile_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            result.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                's' => ' ',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

fn profile_from_source(path: &Path, source: &SourceKeyFile) -> Result<AccountProfile, String> {
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
    let tls_mode = first_value(
        source,
        &[
            "Security",
            "Mail Account",
            "Mail Submission",
            "SMTP",
            "Authentication",
        ],
        &["Method", "TLS", "Encryption", "SecurityMethod", "UseSSL"],
    )
    .map(|value| tls_mode_from_value(&value))
    .unwrap_or_else(|| "tls".to_string());
    let username = first_value(
        source,
        &["Authentication", "Mail Account", "IMAP", "Camel"],
        &["User", "Username", "Login", "UserName"],
    )
    .unwrap_or_default();
    let imap_port = first_value(
        source,
        &["Mail Account", "Authentication", "IMAP", "Camel"],
        &["Port", "PortNumber", "IMAPPort"],
    )
    .and_then(|value| value.parse::<u16>().ok())
    .unwrap_or_else(|| if tls_mode == "starttls" { 143 } else { 993 });
    let smtp_port = first_value(
        source,
        &[
            "Mail Submission",
            "Submission",
            "SMTP",
            "Transport",
            "Camel SMTP",
        ],
        &["Port", "PortNumber", "SMTPPort"],
    )
    .and_then(|value| value.parse::<u16>().ok())
    .unwrap_or_else(|| if tls_mode == "starttls" { 587 } else { 465 });
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
    let enabled = first_value(source, &["Data Source", "Source"], &["Enabled", "Active"])
        .map(|value| parse_bool(&value))
        .unwrap_or(true);

    Ok(AccountProfile {
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
    })
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
    fn parses_mail_source_without_retaining_secret_keys() {
        let source = parse_source(
            r#"
[Data Source]
UID=mail-source-1
DisplayName=Person\sMailbox
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
        assert_eq!(source.ignored_secret_keys.len(), 2);

        let profile = profile_from_source(Path::new("mail-source-1.source"), &source)
            .expect("mail profile should be detected");
        assert_eq!(profile.id, "evolution-source:mail-source-1");
        assert_eq!(profile.display_name, "Person Mailbox");
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
              <action name="play-sound"><value>bell</value></action>
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
                .any(|warning| warning.contains("play-sound"))
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
