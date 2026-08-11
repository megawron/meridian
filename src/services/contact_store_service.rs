// SPDX-License-Identifier: GPL-3.0-or-later
//
// Local vCard store. EBook/EDS remains the authoritative synchronized
// address-book backend; explicitly selected external folders stay read-only,
// while the app-owned fallback persists local contacts until the Linux EBook
// adapter is available.

use super::vcard_service::VCardContact;
use std::fs;
use std::io;
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
        let uid = vcard_uid(&vcard).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "vCard enthält keine UID")
        })?;
        let path = self.contact_file_path();
        let mut contents = read_contact_file(&path)?;
        insert_vcard(&mut contents, &vcard)?;
        write_atomic(&path, contents.as_bytes())?;
        Ok((uid, vcard))
    }

    pub fn modify_contact(&self, uid: &str, vcard: &str) -> io::Result<(String, String)> {
        self.ensure_writable()?;
        let vcard = ensure_vcard_uid(vcard, Some(uid));
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
            return Some(unescape_vcard_value(value));
        }
    }
    None
}

fn local_contact_id(raw: &str, path: &Path, index: usize) -> String {
    vcard_uid(raw).unwrap_or_else(|| format!("local:{}#{index}", path.to_string_lossy()))
}

fn ensure_vcard_uid(raw: &str, fallback_uid: Option<&str>) -> String {
    let mut lines = unfold_lines(raw);
    let uid = vcard_uid(raw)
        .or_else(|| fallback_uid.map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("evolution-gtk4-{}", uuid::Uuid::new_v4()));
    if vcard_uid(raw).is_none() {
        let insertion = lines
            .iter()
            .position(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
            .map(|index| index + 1)
            .unwrap_or(0);
        lines.insert(insertion, format!("UID:{uid}"));
    }
    let mut result = lines.join("\r\n");
    result.push_str("\r\n");
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
    let temporary = path.with_extension("vcf.tmp");
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
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
    let lines = unfold_lines(raw);
    let mut cards = Vec::new();
    let mut current = Vec::new();
    let mut inside = false;

    for line in lines {
        if line.eq_ignore_ascii_case("BEGIN:VCARD") {
            current.clear();
            current.push(line);
            inside = true;
        } else if inside && line.eq_ignore_ascii_case("END:VCARD") {
            current.push(line);
            cards.push(current.join("\r\n"));
            current.clear();
            inside = false;
        } else if inside {
            current.push(line);
        }
    }

    cards
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.trim_start());
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
