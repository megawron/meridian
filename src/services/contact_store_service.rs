// SPDX-License-Identifier: GPL-3.0-or-later
//
// Local vCard store. EBook/EDS remains the authoritative synchronized
// address-book backend; explicitly selected external folders stay read-only,
// while the app-owned fallback persists local contacts until the Linux EBook
// adapter is available.

use super::vcard_service::VCardContact;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalContactRecord {
    pub id: String,
    pub contact: VCardContact,
    pub vcard: String,
}

#[derive(Debug, Clone)]
pub struct ContactStoreService {
    contact_dir: PathBuf,
    writable: bool,
}

impl ContactStoreService {
    pub fn new() -> Self {
        let (contact_dir, writable) =
            if let Some(path) = std::env::var_os("EVOLUTION_GTK4_VCARD_DIR").map(PathBuf::from) {
                (path, false)
            } else if let Some(path) = crate::services::settings_service::AppSettings::try_new()
                .map(|settings| settings.local_contact_directory())
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
                .map(|path| {
                    let writable = crate::services::is_app_owned_store_path(&path);
                    (path, writable)
                })
            {
                path
            } else {
                (crate::services::app_data_dir().join("vcards"), true)
            };
        Self {
            contact_dir,
            writable,
        }
    }

    #[allow(dead_code)]
    pub fn with_contact_dir(contact_dir: impl Into<PathBuf>) -> Self {
        Self {
            contact_dir: contact_dir.into(),
            writable: false,
        }
    }

    pub fn with_writable_contact_dir(contact_dir: impl Into<PathBuf>) -> Self {
        Self {
            contact_dir: contact_dir.into(),
            writable: true,
        }
    }

    #[allow(dead_code)]
    pub fn contact_dir(&self) -> &Path {
        &self.contact_dir
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Copy user-selected vCard files into the configured local source.
    /// Existing files are never replaced, so an import can be repeated safely.
    pub fn import_files(&self, files: &[PathBuf]) -> io::Result<usize> {
        fs::create_dir_all(&self.contact_dir)?;
        let mut imported = 0;
        for source in files {
            if !source.is_file()
                || !source
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(extension.to_ascii_lowercase().as_str(), "vcf" | "vcard")
                    })
            {
                continue;
            }
            let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let destination = unique_destination(&self.contact_dir, file_name);
            fs::copy(source, destination)?;
            imported += 1;
        }
        Ok(imported)
    }

    pub fn load_contacts(&self) -> Vec<LocalContactRecord> {
        let mut paths = Vec::new();
        collect_vcard_files(&self.contact_dir, &mut paths);
        paths.sort();

        let mut contacts = Vec::new();
        for path in paths {
            let Ok(raw) = fs::read_to_string(&path) else {
                tracing::warn!(path = %path.display(), "Could not read local vCard file");
                continue;
            };

            for (index, card) in split_vcards(&raw).into_iter().enumerate() {
                let Some(contact) = VCardContact::parse_vcard_string(&card) else {
                    tracing::warn!(
                        path = %path.display(),
                        index,
                        "Ignoring invalid local vCard"
                    );
                    continue;
                };
                contacts.push(LocalContactRecord {
                    id: local_contact_id(&card, &path, index),
                    contact,
                    vcard: card,
                });
            }
        }

        contacts.sort_by(|left, right| {
            left.contact
                .fn_name
                .to_lowercase()
                .cmp(&right.contact.fn_name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        contacts
    }

    pub fn create_contact(&self, vcard: &str) -> io::Result<(String, String)> {
        self.ensure_writable()?;
        let vcard = ensure_vcard_uid(vcard, None);
        validate_vcard(&vcard)?;
        let uid = vcard_uid(&vcard).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "vCard enthält keine UID")
        })?;
        let path = self.contact_file_path();
        let mut contents = read_contact_file(&path)?;
        for existing_path in self.mutable_vcard_files() {
            let existing_contents = read_contact_file(&existing_path)?;
            if split_vcards(&existing_contents)
                .iter()
                .filter_map(|card| vcard_uid(card))
                .any(|existing_uid| existing_uid == uid)
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Eine vCard mit der UID existiert bereits: {uid}"),
                ));
            }
        }
        insert_vcard(&mut contents, &vcard)?;
        write_atomic(&path, contents.as_bytes())?;
        Ok((uid, vcard))
    }

    pub fn modify_contact(&self, uid: &str, vcard: &str) -> io::Result<(String, String)> {
        self.ensure_writable()?;
        if uid.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ändern benötigt eine Kontakt-UID",
            ));
        }
        let vcard = ensure_vcard_uid(vcard, Some(uid));
        validate_vcard(&vcard)?;
        let replacement_uid = vcard_uid(&vcard).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "vCard enthält keine UID")
        })?;
        if replacement_uid != uid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "vCard-UID stimmt nicht mit dem Ziel überein",
            ));
        }
        for path in self.mutable_vcard_files() {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            let cards = split_vcards(&contents);
            let Some(index) = cards
                .iter()
                .enumerate()
                .position(|(index, card)| local_contact_id(card, &path, index) == uid)
            else {
                continue;
            };
            let updated = replace_vcard(&contents, index, &vcard)?;
            write_atomic(&path, updated.as_bytes())?;
            return Ok((replacement_uid, vcard));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("vCard nicht gefunden: {uid}"),
        ))
    }

    pub fn remove_contact(&self, uid: &str) -> io::Result<String> {
        self.ensure_writable()?;
        for path in self.mutable_vcard_files() {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            let cards = split_vcards(&contents);
            let Some(index) = cards
                .iter()
                .enumerate()
                .position(|(index, card)| local_contact_id(card, &path, index) == uid)
            else {
                continue;
            };
            let updated = remove_vcard(&contents, index)?;
            write_atomic(&path, updated.as_bytes())?;
            return Ok(uid.to_string());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("vCard nicht gefunden: {uid}"),
        ))
    }

    /// Merge one primary contact and one or more duplicates in a single local
    /// store transaction.  The caller supplies the already merged vCard so
    /// this service only has to replace/remove cards and preserve their raw
    /// provider fields.  All affected files are staged first; if a later
    /// atomic rename fails, files written earlier in the transaction are
    /// restored from their original contents.
    pub fn merge_contacts(
        &self,
        primary_uid: &str,
        duplicate_uids: &[String],
        merged_vcard: &str,
    ) -> io::Result<(String, String)> {
        self.ensure_writable()?;
        if primary_uid.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Zusammenführung benötigt eine Primär-UID",
            ));
        }
        if duplicate_uids.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Zusammenführung benötigt mindestens ein Duplikat",
            ));
        }
        if duplicate_uids
            .iter()
            .any(|uid| uid.trim().is_empty() || uid == primary_uid)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Primär- und Duplikat-UIDs müssen verschieden und nicht leer sein",
            ));
        }
        let merged_vcard = ensure_vcard_uid(merged_vcard, Some(primary_uid));
        validate_vcard(&merged_vcard)?;
        let merged_uid = vcard_uid(&merged_vcard).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Die zusammengeführte vCard enthält keine UID",
            )
        })?;
        if merged_uid != primary_uid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Die zusammengeführte vCard hat nicht die Primär-UID",
            ));
        }

        let mut files = Vec::new();
        for path in self.mutable_vcard_files() {
            let contents = fs::read_to_string(&path)?;
            files.push(MutableVCardFile {
                path,
                cards: split_vcards(&contents),
                original: contents,
            });
        }

        let primary_locations = contact_locations(&files, primary_uid);
        if primary_locations.len() != 1 {
            return Err(io::Error::new(
                if primary_locations.is_empty() {
                    io::ErrorKind::NotFound
                } else {
                    io::ErrorKind::InvalidData
                },
                format!("Primärkontakt {primary_uid} wurde nicht genau einmal gefunden"),
            ));
        }

        let mut duplicate_locations = Vec::with_capacity(duplicate_uids.len());
        for uid in duplicate_uids {
            if duplicate_locations
                .iter()
                .any(|(existing_uid, _)| existing_uid == uid)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Duplikat-UID mehrfach angegeben: {uid}"),
                ));
            }
            let locations = contact_locations(&files, uid);
            if locations.len() != 1 {
                return Err(io::Error::new(
                    if locations.is_empty() {
                        io::ErrorKind::NotFound
                    } else {
                        io::ErrorKind::InvalidData
                    },
                    format!("Duplikat {uid} wurde nicht genau einmal gefunden"),
                ));
            }
            duplicate_locations.push((uid.clone(), locations[0]));
        }

        let (primary_file_index, primary_card_index) = primary_locations[0];
        let mut updates = Vec::new();
        for (file_index, file) in files.iter().enumerate() {
            let mut cards = file.cards.clone();
            if file_index == primary_file_index {
                cards[primary_card_index] = merged_vcard.trim().to_string();
            }

            let mut remove_indices = duplicate_locations
                .iter()
                .filter_map(|(_, (duplicate_file_index, card_index))| {
                    (*duplicate_file_index == file_index).then_some(*card_index)
                })
                .collect::<Vec<_>>();
            remove_indices.sort_unstable();
            remove_indices.dedup();
            for card_index in remove_indices.into_iter().rev() {
                cards.remove(card_index);
            }

            let updated = serialize_vcards(&cards);
            if updated != file.original {
                updates.push((file.path.clone(), file.original.clone(), updated));
            }
        }

        let mut written = Vec::<(PathBuf, String)>::new();
        for (path, original, updated) in &updates {
            if let Err(error) = write_atomic(path, updated.as_bytes()) {
                let rollback_failed = written.iter().any(|(written_path, contents)| {
                    write_atomic(written_path, contents.as_bytes()).is_err()
                });
                let rollback_note = if rollback_failed {
                    "; Wiederherstellung mindestens einer Datei fehlgeschlagen"
                } else {
                    "; vorherige Änderungen wurden wiederhergestellt"
                };
                return Err(io::Error::new(
                    error.kind(),
                    format!("Kontaktzusammenführung fehlgeschlagen{rollback_note}: {error}"),
                ));
            }
            written.push((path.clone(), original.clone()));
        }

        Ok((primary_uid.to_string(), merged_vcard))
    }

    fn ensure_writable(&self) -> io::Result<()> {
        if self.writable {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Das ausgewählte vCard-Verzeichnis ist schreibgeschützt",
            ))
        }
    }

    fn contact_file_path(&self) -> PathBuf {
        self.contact_dir.join("evolution-gtk4-local.vcf")
    }

    fn mutable_vcard_files(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        collect_vcard_files(&self.contact_dir, &mut paths);
        paths.sort();
        paths
    }
}

struct MutableVCardFile {
    path: PathBuf,
    original: String,
    cards: Vec<String>,
}

fn contact_locations(files: &[MutableVCardFile], uid: &str) -> Vec<(usize, usize)> {
    files
        .iter()
        .enumerate()
        .flat_map(|(file_index, file)| {
            file.cards
                .iter()
                .enumerate()
                .filter_map(move |(card_index, card)| {
                    (local_contact_id(card, &file.path, card_index) == uid)
                        .then_some((file_index, card_index))
                })
        })
        .collect()
}

impl Default for ContactStoreService {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_vcard_files(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !file_type.is_symlink() {
                collect_vcard_files(&path, paths);
            }
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(extension.to_ascii_lowercase().as_str(), "vcf" | "vcard")
                })
        {
            paths.push(path);
        }
    }
}

fn unique_destination(root: &Path, file_name: &str) -> PathBuf {
    let original = root.join(file_name);
    if !original.exists() {
        return original;
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("contacts");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let mut suffix = 1;
    loop {
        let candidate = root.join(format!("{stem}-{suffix}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

fn vcard_uid(raw: &str) -> Option<String> {
    let mut depth = 0usize;
    for line in unfold_lines(raw) {
        if line
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BEGIN:"))
        {
            depth += 1;
            continue;
        }
        if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("END:"))
        {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth != 1 {
            continue;
        }
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        if property
            .split(';')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("UID"))
        {
            let uid = unescape_vcard_value(value);
            if !uid.trim().is_empty() {
                return Some(uid);
            }
        }
    }
    None
}

fn local_contact_id(raw: &str, path: &Path, index: usize) -> String {
    vcard_uid(raw).unwrap_or_else(|| format!("local:{}#{index}", path.to_string_lossy()))
}

fn ensure_vcard_uid(raw: &str, fallback_uid: Option<&str>) -> String {
    if vcard_uid(raw).is_some() {
        return normalize_line_endings(raw);
    }
    let uid = fallback_uid
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("evolution-gtk4-{}", uuid::Uuid::new_v4()));
    let normalized = normalize_line_endings(raw);
    let mut lines = normalized
        .split("\r\n")
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let insertion = lines
        .iter()
        .position(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
        .map(|index| index + 1)
        .unwrap_or(0);
    lines.insert(insertion, format!("UID:{uid}"));
    lines.join("\r\n")
}

fn normalize_line_endings(raw: &str) -> String {
    let mut result = raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    if !result.ends_with("\r\n") {
        result.push_str("\r\n");
    }
    result
}

fn read_contact_file(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

fn insert_vcard(contents: &mut String, vcard: &str) -> io::Result<()> {
    if !vcard
        .lines()
        .any(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
        || !vcard
            .lines()
            .any(|line| line.eq_ignore_ascii_case("END:VCARD"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige vCard-Komponente",
        ));
    }
    let mut cards = split_vcards(contents);
    cards.push(vcard.trim().to_string());
    *contents = serialize_vcards(&cards);
    Ok(())
}

fn replace_vcard(contents: &str, index: usize, replacement: &str) -> io::Result<String> {
    let mut cards = split_vcards(contents);
    if index >= cards.len() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "vCard-Index nicht gefunden",
        ));
    }
    cards[index] = replacement.trim().to_string();
    Ok(serialize_vcards(&cards))
}

fn remove_vcard(contents: &str, index: usize) -> io::Result<String> {
    let mut cards = split_vcards(contents);
    if index >= cards.len() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "vCard-Index nicht gefunden",
        ));
    }
    cards.remove(index);
    Ok(serialize_vcards(&cards))
}

fn serialize_vcards(cards: &[String]) -> String {
    if cards.is_empty() {
        String::new()
    } else {
        format!("{}\r\n", cards.join("\r\n"))
    }
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("contacts.vcf");
    let temporary = path.with_file_name(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&temporary, path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                if let Ok(directory) = fs::File::open(parent) {
                    let _ = directory.sync_all();
                }
            }
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

fn validate_vcard(vcard: &str) -> io::Result<VCardContact> {
    if split_vcards(vcard).len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Eine Mutation benötigt genau eine vCard",
        ));
    }
    let Some(contact) = VCardContact::parse_vcard_string(vcard) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige vCard",
        ));
    };
    if let Some(error) = contact.list_validation_error() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, error));
    }
    Ok(contact)
}

fn unescape_vcard_value(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn split_vcards(raw: &str) -> Vec<String> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut cards = Vec::new();
    let mut current = Vec::new();
    let mut inside = false;

    for line in normalized.split('\n') {
        if line.is_empty() && !inside {
            continue;
        }
        if line.eq_ignore_ascii_case("BEGIN:VCARD") {
            current.clear();
            current.push(line.to_string());
            inside = true;
        } else if inside && line.eq_ignore_ascii_case("END:VCARD") {
            current.push(line.to_string());
            cards.push(current.join("\r\n"));
            current.clear();
            inside = false;
        } else if inside {
            current.push(line.to_string());
        }
    }

    cards
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&raw_line[1..]);
            }
        } else {
            lines.push(raw_line.to_string());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_multiple_cards_and_sorts_by_name() {
        let root = temporary_directory("vcard-roundtrip");
        let path = root.join("people.vcf");
        fs::write(
            &path,
            concat!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Zoe Example\r\n",
                "EMAIL:zoe@example.com\r\nEND:VCARD\r\n",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Anna Example\r\n",
                "EMAIL:anna@example.com\r\nNOTE:Folded\r\n note\r\nEND:VCARD\r\n"
            ),
        )
        .expect("vCard file should be written");

        let contacts = ContactStoreService::with_contact_dir(&root).load_contacts();
        assert_eq!(contacts.len(), 2);
        assert_eq!(contacts[0].contact.fn_name, "Anna Example");
        assert_eq!(contacts[0].contact.note, "Foldednote");
        assert!(contacts[0].id.contains("people.vcf#1"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_directory_is_empty_and_does_not_create_data() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-missing-vcards-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        assert!(
            ContactStoreService::with_contact_dir(&root)
                .load_contacts()
                .is_empty()
        );
        assert!(!root.exists());
    }

    #[test]
    fn imports_vcard_files_without_overwriting_existing_files() {
        let source_root = temporary_directory("vcard-import-source");
        let destination_root = temporary_directory("vcard-import-destination");
        let source = source_root.join("people.vcf");
        fs::write(
            &source,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported\r\nEND:VCARD\r\n",
        )
        .expect("source vCard should be written");
        fs::write(destination_root.join("people.vcf"), "existing contacts\n")
            .expect("existing vCard should be written");

        let service = ContactStoreService::with_contact_dir(&destination_root);
        assert_eq!(
            service.import_files(std::slice::from_ref(&source)).unwrap(),
            1
        );
        assert_eq!(
            fs::read_to_string(destination_root.join("people.vcf")).unwrap(),
            "existing contacts\n"
        );
        assert!(destination_root.join("people-1.vcf").is_file());

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(destination_root);
    }

    #[test]
    fn writable_local_store_persists_create_modify_and_remove() {
        let root = temporary_directory("vcard-mutations");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact-local-1\r\n",
            "FN:Initial\r\nEMAIL:initial@example.org\r\nEND:VCARD\r\n"
        );
        let (uid, _) = service
            .create_contact(vcard)
            .expect("contact should be created");
        assert_eq!(uid, "contact-local-1");
        assert_eq!(service.load_contacts()[0].contact.fn_name, "Initial");
        let updated = vcard.replace("FN:Initial", "FN:Updated");
        service
            .modify_contact(&uid, &updated)
            .expect("contact should be modified");
        assert_eq!(service.load_contacts()[0].contact.fn_name, "Updated");
        assert_eq!(
            service
                .remove_contact(&uid)
                .expect("contact should be removed"),
            uid
        );
        assert!(service.load_contacts().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writable_local_store_persists_and_reloads_contact_lists_without_losing_extensions() {
        let root = temporary_directory("vcard-list-persistence");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let list = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:list-1\r\nFN:Projektteam\r\n",
            "N:;Projektteam;;;\r\nX-EVOLUTION-LIST:TRUE\r\n",
            "X-EVOLUTION-LIST-SHOW-ADDRESSES:FALSE\r\n",
            "X-EVOLUTION-LIST-NAME:Projektteam\r\n",
            "EMAIL:Extern <external@example.test>\r\n",
            "EMAIL;X-EVOLUTION-DEST-CONTACT-UID=contact-1;X-EVOLUTION-DEST-SOURCE-UID=local-addressbook:Alice <alice@example.test>\r\n",
            "X-PROVIDER-UNKNOWN;X-PARAM=kept:opaque\r\nEND:VCARD\r\n"
        );
        service
            .create_contact(list)
            .expect("contact list should be persisted");

        let loaded = service.load_contacts();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].contact.is_list);
        assert_eq!(loaded[0].contact.list_members.len(), 2);
        assert_eq!(loaded[0].contact.list_members[1].contact_uid, "contact-1");
        assert!(
            loaded[0]
                .vcard
                .contains("X-PROVIDER-UNKNOWN;X-PARAM=kept:opaque")
        );
        assert!(!loaded[0].contact.list_show_addresses);

        let updated = loaded[0].contact.to_vcard_string().replace(
            "X-EVOLUTION-LIST-SHOW-ADDRESSES:FALSE",
            "X-EVOLUTION-LIST-SHOW-ADDRESSES:TRUE",
        );
        service
            .modify_contact("list-1", &updated)
            .expect("contact list should be modified");
        let reloaded = service.load_contacts();
        assert!(reloaded[0].contact.list_show_addresses);
        assert_eq!(reloaded[0].contact.list_members.len(), 2);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_list_member_is_rejected_without_changing_existing_file() {
        let root = temporary_directory("vcard-list-invalid");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let valid = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:list-2\r\nFN:Team\r\n",
            "X-EVOLUTION-LIST:TRUE\r\nEMAIL:valid@example.test\r\nEND:VCARD\r\n"
        );
        service
            .create_contact(valid)
            .expect("valid list should be created");
        let path = root.join("evolution-gtk4-local.vcf");
        let original = fs::read_to_string(&path).expect("list file should exist");
        let invalid = valid.replace("valid@example.test", "not-an-email");
        let error = service
            .modify_contact("list-2", &invalid)
            .expect_err("invalid list member should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(path).unwrap(), original);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_uid_is_rejected_before_the_atomic_write() {
        let root = temporary_directory("vcard-duplicate-uid");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:duplicate-uid\r\n",
            "FN:Erster Kontakt\r\nEND:VCARD\r\n"
        );
        service
            .create_contact(vcard)
            .expect("first contact should save");
        let error = service
            .create_contact(vcard)
            .expect_err("duplicate UID should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(service.load_contacts().len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn existing_folding_survives_create_and_modify() {
        let root = temporary_directory("vcard-folding-persistence");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:folded-1\r\nFN:Gefaltete Karte\r\n",
            "X-PROVIDER-LONG:erste Zeile\r\n zweite Zeile mit Daten\r\n",
            "END:VCARD\r\n"
        );
        service
            .create_contact(vcard)
            .expect("folded contact should save");
        let path = root.join("evolution-gtk4-local.vcf");
        let stored = fs::read_to_string(&path).expect("store should be readable");
        assert!(stored.contains("X-PROVIDER-LONG:erste Zeile\r\n zweite Zeile"));

        service
            .modify_contact("folded-1", vcard)
            .expect("folded contact should modify");
        let modified = fs::read_to_string(path).expect("modified store should be readable");
        assert!(modified.contains("X-PROVIDER-LONG:erste Zeile\r\n zweite Zeile"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_or_multi_card_mutations_are_rejected_without_creating_a_file() {
        let root = temporary_directory("vcard-invalid-mutation");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let malformed = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Unvollständig\r\n";
        let error = service
            .create_contact(malformed)
            .expect_err("malformed vCard should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let multiple = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Eins\r\nEND:VCARD\r\n",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Zwei\r\nEND:VCARD\r\n"
        );
        let error = service
            .create_contact(multiple)
            .expect_err("multiple cards should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!root.join("evolution-gtk4-local.vcf").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn vcard4_group_requires_kind_group_in_local_persistence() {
        let root = temporary_directory("vcard4-group-validation");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let invalid = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:group-invalid\r\nFN:Team\r\n",
            "MEMBER:mailto:person@example.test\r\nEND:VCARD\r\n"
        );
        let error = service
            .create_contact(invalid)
            .expect_err("vCard 4 member without KIND should be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merge_contacts_replaces_primary_and_removes_duplicates_atomically() {
        let root = temporary_directory("vcard-merge");
        let path = root.join("people.vcf");
        fs::write(
            &path,
            concat!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:primary\r\nFN:Ada Lovelace\r\n",
                "EMAIL:ada@example.test\r\nEND:VCARD\r\n",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:duplicate\r\nFN:Ada Lovelace\r\n",
                "TEL;TYPE=CELL:+49 123\r\nX-PROVIDER:kept\r\nEND:VCARD\r\n"
            ),
        )
        .expect("contact file should be written");

        let service = ContactStoreService::with_writable_contact_dir(&root);
        let merged = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:primary\r\nFN:Ada Lovelace\r\n",
            "EMAIL:ada@example.test\r\nTEL;TYPE=CELL:+49 123\r\n",
            "X-PROVIDER:kept\r\nEND:VCARD\r\n"
        );
        let result = service
            .merge_contacts("primary", &["duplicate".to_string()], merged)
            .expect("contacts should merge");

        assert_eq!(result.0, "primary");
        assert_eq!(service.load_contacts().len(), 1);
        assert_eq!(service.load_contacts()[0].id, "primary");
        let contents = fs::read_to_string(path).expect("merged file should be readable");
        assert!(contents.contains("TEL;TYPE=CELL:+49 123"));
        assert!(contents.contains("X-PROVIDER:kept"));
        assert!(!contents.contains("UID:duplicate"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merge_contacts_rejects_missing_duplicate_without_changing_store() {
        let root = temporary_directory("vcard-merge-missing");
        let path = root.join("people.vcf");
        let original =
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:primary\r\nFN:Ada Lovelace\r\nEND:VCARD\r\n";
        fs::write(&path, original).expect("contact file should be written");

        let service = ContactStoreService::with_writable_contact_dir(&root);
        assert!(
            service
                .merge_contacts("primary", &["missing".to_string()], original,)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(path).expect("original file should remain"),
            original
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imported_contact_store_rejects_mutations() {
        let root = temporary_directory("vcard-read-only");
        let service = ContactStoreService::with_contact_dir(&root);
        assert!(
            service
                .create_contact("BEGIN:VCARD\r\nFN:Read only\r\nEND:VCARD\r\n")
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writable_imported_contact_without_uid_can_be_modified_and_removed() {
        let root = temporary_directory("vcard-uidless-mutations");
        let path = root.join("imported.vcf");
        fs::write(
            &path,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported\r\nEND:VCARD\r\n",
        )
        .expect("uid-less contact should be written");
        let service = ContactStoreService::with_writable_contact_dir(&root);
        let uid = service.load_contacts()[0].id.clone();
        assert!(uid.starts_with("local:"));

        let updated = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Updated\r\nEND:VCARD\r\n";
        service
            .modify_contact(&uid, updated)
            .expect("uid-less contact should be modified");
        assert_eq!(service.load_contacts()[0].contact.fn_name, "Updated");
        service
            .remove_contact(&uid)
            .expect("uid-less contact should be removed");
        assert!(service.load_contacts().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
