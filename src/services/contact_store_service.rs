// SPDX-License-Identifier: GPL-3.0-or-later
//
// Read-only importer for local vCard files.  EBook/EDS remains the
// authoritative synchronized address-book backend; this service gives the UI
// a deterministic source for exported or locally maintained .vcf files until
// the Linux EBook adapter is available.

use super::vcard_service::VCardContact;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalContactRecord {
    pub id: String,
    pub contact: VCardContact,
}

#[derive(Debug, Clone)]
pub struct ContactStoreService {
    contact_dir: PathBuf,
}

impl ContactStoreService {
    pub fn new() -> Self {
        let contact_dir = std::env::var_os("EVOLUTION_GTK4_VCARD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| glib::user_data_dir().join("evolution-gtk4/vcards"));
        Self { contact_dir }
    }

    #[allow(dead_code)]
    pub fn with_contact_dir(contact_dir: impl Into<PathBuf>) -> Self {
        Self {
            contact_dir: contact_dir.into(),
        }
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
                    id: format!("local:{}#{index}", path.to_string_lossy()),
                    contact,
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

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
