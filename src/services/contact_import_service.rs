// SPDX-License-Identifier: GPL-3.0-or-later
//
// contact_import_service.rs — Upstream parity parser, preview planner, and batch execution for vCard, CSV, and LDIF contact sources.

use std::io;
use std::path::Path;

use crate::services::contact_store_service::{ContactStoreService, split_vcards};
use crate::services::vcard_service::VCardContact;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactImportFormat {
    VCard,
    Csv,
    Ldif,
    Unknown,
}

impl ContactImportFormat {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::VCard => "vCard (.vcf, .vcard)",
            Self::Csv => "Comma-Separated Values (.csv)",
            Self::Ldif => "LDAP Data Interchange Format (.ldif, .ldi)",
            Self::Unknown => "Unknown Format",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactImportItem {
    pub full_name: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub address: Option<String>,
    pub notes: Option<String>,
    pub vcard_contact: VCardContact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContactDuplicatePolicy {
    #[default]
    KeepBothNew,
    OverwriteExisting,
    SkipExisting,
}

impl ContactDuplicatePolicy {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::KeepBothNew => "Keep both (create new contact)",
            Self::OverwriteExisting => "Overwrite existing contacts",
            Self::SkipExisting => "Skip existing contacts",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactImportPlan {
    pub format: ContactImportFormat,
    pub items: Vec<ContactImportItem>,
    pub duplicate_count: usize,
    pub duplicate_names: Vec<String>,
    pub policy: ContactDuplicatePolicy,
}

impl ContactImportPlan {
    pub fn summary_label(&self) -> String {
        let count = self.items.len();
        if count == 0 {
            return "No importable contacts found".to_string();
        }

        let base = format!(
            "{} {}",
            count,
            if count == 1 {
                "contact found"
            } else {
                "contacts found"
            }
        );

        if self.duplicate_count > 0 {
            format!(
                "{base} ({} duplicate{})",
                self.duplicate_count,
                if self.duplicate_count == 1 { "" } else { "s" }
            )
        } else {
            base
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContactImportResult {
    pub imported_count: usize,
    pub overwritten_count: usize,
    pub skipped_count: usize,
    pub created_uids: Vec<String>,
}

pub fn detect_contact_format(path: &Path, content: &str) -> ContactImportFormat {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "vcf" | "vcard" => return ContactImportFormat::VCard,
            "csv" => return ContactImportFormat::Csv,
            "ldif" | "ldi" => return ContactImportFormat::Ldif,
            _ => {}
        }
    }

    let upper = content.to_ascii_uppercase();
    if upper.contains("BEGIN:VCARD") {
        ContactImportFormat::VCard
    } else if upper.contains("DN:") || upper.contains("DN::") {
        ContactImportFormat::Ldif
    } else if content
        .lines()
        .next()
        .is_some_and(|l| l.contains(',') || l.contains(';'))
    {
        ContactImportFormat::Csv
    } else {
        ContactImportFormat::Unknown
    }
}

pub fn parse_vcard_stream(content: &str) -> Vec<ContactImportItem> {
    let mut items = Vec::new();
    for card in split_vcards(content) {
        let Some(contact) = VCardContact::parse_vcard_string(&card) else {
            continue;
        };

        let full_name = if !contact.fn_name.trim().is_empty() {
            contact.fn_name.clone()
        } else if !contact.name.family.trim().is_empty() || !contact.name.given.trim().is_empty() {
            format!("{} {}", contact.name.given, contact.name.family)
                .trim()
                .to_string()
        } else if !contact.email.trim().is_empty() {
            contact.email.clone()
        } else {
            "Unnamed Contact".to_string()
        };

        let first_name = if !contact.name.given.trim().is_empty() {
            Some(contact.name.given.clone())
        } else {
            None
        };

        let last_name = if !contact.name.family.trim().is_empty() {
            Some(contact.name.family.clone())
        } else {
            None
        };

        let email = if !contact.email.trim().is_empty() {
            Some(contact.email.clone())
        } else {
            contact.emails.first().cloned()
        };

        let phone = if !contact.tel.trim().is_empty() {
            Some(contact.tel.clone())
        } else {
            contact.phones.first().cloned()
        };

        let organization = if !contact.org.trim().is_empty() {
            Some(contact.org.clone())
        } else {
            None
        };

        let title = if !contact.title.trim().is_empty() {
            Some(contact.title.clone())
        } else {
            None
        };

        let address = contact.addresses.first().cloned();
        let notes = if !contact.note.trim().is_empty() {
            Some(contact.note.clone())
        } else {
            None
        };

        items.push(ContactImportItem {
            full_name,
            first_name,
            last_name,
            email,
            phone,
            organization,
            title,
            address,
            notes,
            vcard_contact: contact,
        });
    }

    items
}

pub fn parse_csv_stream(content: &str) -> Vec<ContactImportItem> {
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let Some(header_line) = lines.next() else {
        return Vec::new();
    };

    let delimiter = if header_line.contains(';') && !header_line.contains(',') {
        ';'
    } else {
        ','
    };

    let headers = parse_csv_line(header_line, delimiter);
    let mut col_fn = None;
    let mut col_given = None;
    let mut col_family = None;
    let mut col_email = None;
    let mut col_phone = None;
    let mut col_org = None;
    let mut col_title = None;
    let mut col_address = None;
    let mut col_note = None;

    for (idx, header) in headers.iter().enumerate() {
        let clean = header.trim().to_ascii_lowercase();
        if clean == "name"
            || clean == "full name"
            || clean == "display name"
            || clean == "anzeigename"
        {
            col_fn = Some(idx);
        } else if clean == "first name" || clean == "vorname" || clean == "given name" {
            col_given = Some(idx);
        } else if clean == "last name"
            || clean == "nachname"
            || clean == "family name"
            || clean == "surname"
        {
            col_family = Some(idx);
        } else if clean == "e-mail"
            || clean == "email"
            || clean == "e-mail-adresse"
            || clean.starts_with("e-mail 1")
        {
            col_email = Some(idx);
        } else if clean == "phone"
            || clean == "telefon"
            || clean == "mobile"
            || clean == "mobil"
            || clean.contains("phone")
        {
            if col_phone.is_none() {
                col_phone = Some(idx);
            }
        } else if clean == "organization"
            || clean == "company"
            || clean == "firma"
            || clean == "unternehmen"
        {
            col_org = Some(idx);
        } else if clean == "title"
            || clean == "job title"
            || clean == "titel"
            || clean == "position"
        {
            col_title = Some(idx);
        } else if clean == "address" || clean == "adresse" || clean == "street" || clean == "straße"
        {
            col_address = Some(idx);
        } else if clean == "notes" || clean == "notizen" || clean == "description" {
            col_note = Some(idx);
        }
    }

    let mut items = Vec::new();
    for line in lines {
        let row = parse_csv_line(line, delimiter);
        let get = |idx: Option<usize>| {
            idx.and_then(|i| row.get(i))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };

        let fn_val = get(col_fn);
        let given_val = get(col_given);
        let family_val = get(col_family);
        let email_val = get(col_email);
        let phone_val = get(col_phone);
        let org_val = get(col_org);
        let title_val = get(col_title);
        let address_val = get(col_address);
        let note_val = get(col_note);

        let full_name = if let Some(ref name) = fn_val {
            name.clone()
        } else if given_val.is_some() || family_val.is_some() {
            format!(
                "{} {}",
                given_val.as_deref().unwrap_or(""),
                family_val.as_deref().unwrap_or("")
            )
            .trim()
            .to_string()
        } else if let Some(ref em) = email_val {
            em.clone()
        } else {
            continue;
        };

        if full_name.is_empty() && email_val.is_none() {
            continue;
        }

        let mut contact = VCardContact::new(
            &full_name,
            email_val.as_deref().unwrap_or(""),
            org_val.as_deref().unwrap_or(""),
            title_val.as_deref().unwrap_or(""),
        );

        if let Some(ref g) = given_val {
            contact.name.given = g.clone();
        }
        if let Some(ref f) = family_val {
            contact.name.family = f.clone();
        }
        if let Some(ref p) = phone_val {
            contact.tel = p.clone();
            contact.phones.push(p.clone());
        }
        if let Some(ref a) = address_val {
            contact.addresses.push(a.clone());
        }
        if let Some(ref n) = note_val {
            contact.note = n.clone();
        }

        items.push(ContactImportItem {
            full_name,
            first_name: given_val,
            last_name: family_val,
            email: email_val,
            phone: phone_val,
            organization: org_val,
            title: title_val,
            address: address_val,
            notes: note_val,
            vcard_contact: contact,
        });
    }

    items
}

fn parse_csv_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '"' {
            if in_quotes && chars.peek() == Some(&'"') {
                chars.next();
                current.push('"');
            } else {
                in_quotes = !in_quotes;
            }
        } else if c == delimiter && !in_quotes {
            fields.push(current);
            current = String::new();
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

pub fn parse_ldif_stream(content: &str) -> Vec<ContactImportItem> {
    let mut unfolded: Vec<String> = Vec::new();
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.lines() {
        if line.starts_with(' ') && !unfolded.is_empty() {
            let last = unfolded.last_mut().expect("checked not empty");
            last.push_str(&line[1..]);
        } else {
            unfolded.push(line.to_string());
        }
    }

    let mut records = Vec::new();
    let mut current_record: Vec<String> = Vec::new();

    for line in unfolded {
        if line.trim().is_empty() {
            if !current_record.is_empty() {
                records.push(std::mem::take(&mut current_record));
            }
        } else {
            current_record.push(line);
        }
    }
    if !current_record.is_empty() {
        records.push(current_record);
    }

    let mut items = Vec::new();
    for record in records {
        let mut cn = None;
        let mut given_name = None;
        let mut sn = None;
        let mut mail = None;
        let mut phone = None;
        let mut org = None;
        let mut title = None;
        let mut street = None;
        let mut locality = None;
        let mut postal_code = None;
        let mut country = None;
        let mut note = None;

        for line in record {
            let Some((attr, val)) = line.split_once(':') else {
                continue;
            };
            let attr_clean = attr.trim().to_ascii_lowercase();
            let val_clean = val.trim_start_matches(':').trim().to_string();

            match attr_clean.as_str() {
                "cn" | "commonname" => cn = Some(val_clean),
                "givenname" => given_name = Some(val_clean),
                "sn" | "surname" => sn = Some(val_clean),
                "mail" => mail = Some(val_clean),
                "telephonenumber" | "homephone" | "mobile" => {
                    if phone.is_none() {
                        phone = Some(val_clean);
                    }
                }
                "o" | "company" => org = Some(val_clean),
                "title" => title = Some(val_clean),
                "street" | "postaladdress" => street = Some(val_clean),
                "l" | "locality" => locality = Some(val_clean),
                "postalcode" => postal_code = Some(val_clean),
                "c" | "countryname" => country = Some(val_clean),
                "description" => note = Some(val_clean),
                _ => {}
            }
        }

        let full_name = if let Some(ref name) = cn {
            name.clone()
        } else if given_name.is_some() || sn.is_some() {
            format!(
                "{} {}",
                given_name.as_deref().unwrap_or(""),
                sn.as_deref().unwrap_or("")
            )
            .trim()
            .to_string()
        } else if let Some(ref m) = mail {
            m.clone()
        } else {
            continue;
        };

        if full_name.is_empty() && mail.is_none() {
            continue;
        }

        let mut address_parts = Vec::new();
        if let Some(ref s) = street {
            address_parts.push(s.clone());
        }
        if let Some(ref pc) = postal_code {
            if let Some(ref l) = locality {
                address_parts.push(format!("{pc} {l}"));
            } else {
                address_parts.push(pc.clone());
            }
        } else if let Some(ref l) = locality {
            address_parts.push(l.clone());
        }
        if let Some(ref c) = country {
            address_parts.push(c.clone());
        }
        let address = if !address_parts.is_empty() {
            Some(address_parts.join(", "))
        } else {
            None
        };

        let mut contact = VCardContact::new(
            &full_name,
            mail.as_deref().unwrap_or(""),
            org.as_deref().unwrap_or(""),
            title.as_deref().unwrap_or(""),
        );

        if let Some(ref g) = given_name {
            contact.name.given = g.clone();
        }
        if let Some(ref s) = sn {
            contact.name.family = s.clone();
        }
        if let Some(ref p) = phone {
            contact.tel = p.clone();
            contact.phones.push(p.clone());
        }
        if let Some(ref a) = address {
            contact.addresses.push(a.clone());
        }
        if let Some(ref n) = note {
            contact.note = n.clone();
        }

        items.push(ContactImportItem {
            full_name,
            first_name: given_name,
            last_name: sn,
            email: mail,
            phone,
            organization: org,
            title,
            address,
            notes: note,
            vcard_contact: contact,
        });
    }

    items
}

pub fn plan_contact_import(
    content: &str,
    format: ContactImportFormat,
    existing_contacts: &[VCardContact],
    policy: ContactDuplicatePolicy,
) -> ContactImportPlan {
    let items = match format {
        ContactImportFormat::VCard => parse_vcard_stream(content),
        ContactImportFormat::Csv => parse_csv_stream(content),
        ContactImportFormat::Ldif => parse_ldif_stream(content),
        ContactImportFormat::Unknown => Vec::new(),
    };

    let mut duplicate_count = 0usize;
    let mut duplicate_names = Vec::new();

    for item in &items {
        let is_dup = existing_contacts.iter().any(|existing| {
            if let Some(ref email) = item.email {
                if !email.is_empty() && existing.email.eq_ignore_ascii_case(email) {
                    return true;
                }
            }
            if !item.full_name.is_empty() && existing.fn_name.eq_ignore_ascii_case(&item.full_name)
            {
                return true;
            }
            false
        });

        if is_dup {
            duplicate_count += 1;
            duplicate_names.push(item.full_name.clone());
        }
    }

    ContactImportPlan {
        format,
        items,
        duplicate_count,
        duplicate_names,
        policy,
    }
}

pub fn execute_contact_import(
    plan: &ContactImportPlan,
    store: &ContactStoreService,
) -> io::Result<ContactImportResult> {
    let mut result = ContactImportResult::default();
    let existing_records = store.load_contacts();

    for item in &plan.items {
        let matching_record = existing_records.iter().find(|rec| {
            if let Some(ref email) = item.email {
                if !email.is_empty() && rec.contact.email.eq_ignore_ascii_case(email) {
                    return true;
                }
            }
            if !item.full_name.is_empty()
                && rec.contact.fn_name.eq_ignore_ascii_case(&item.full_name)
            {
                return true;
            }
            false
        });

        let is_duplicate = matching_record.is_some();

        match plan.policy {
            ContactDuplicatePolicy::SkipExisting if is_duplicate => {
                result.skipped_count += 1;
                continue;
            }
            ContactDuplicatePolicy::OverwriteExisting if is_duplicate => {
                let rec = matching_record.expect("checked duplicate");
                let vcard_str = item.vcard_contact.to_vcard_string();
                match store.modify_contact(&rec.id, &vcard_str) {
                    Ok(_) => {
                        result.overwritten_count += 1;
                    }
                    Err(err) => {
                        rollback_created_contacts(store, &result.created_uids);
                        return Err(err);
                    }
                }
            }
            _ => {
                // New contact (either non-duplicate or KeepBothNew)
                let vcard_str = item.vcard_contact.to_vcard_string();
                match store.create_contact(&vcard_str) {
                    Ok((uid, _)) => {
                        result.created_uids.push(uid);
                        result.imported_count += 1;
                    }
                    Err(err) => {
                        rollback_created_contacts(store, &result.created_uids);
                        return Err(err);
                    }
                }
            }
        }
    }

    Ok(result)
}

fn rollback_created_contacts(store: &ContactStoreService, created_uids: &[String]) {
    for uid in created_uids {
        let _ = store.remove_contact(uid);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const SAMPLE_VCARD: &str = r#"BEGIN:VCARD
VERSION:4.0
FN:Dr. Erika Mustermann
N:Mustermann;Erika;;Dr.;
EMAIL:erika@example.com
TEL;TYPE=work:+49 30 123456
ORG:Musterfirma GmbH
TITLE:Leitende Entwicklerin
END:VCARD
BEGIN:VCARD
VERSION:3.0
FN:Max Mustermann
EMAIL:max@example.com
TEL:+49 30 654321
END:VCARD
"#;

    const SAMPLE_CSV: &str = r#"First Name,Last Name,Email,Phone,Company,Job Title
Anna,Schmidt,anna@example.com,+49 40 112233,Hanseatic Tech,CTO
Bernd,Bäcker,bernd@example.com,+49 89 445566,Bavaria Cloud,VP Sales
"#;

    const SAMPLE_LDIF: &str = r#"dn: cn=Clara Oswald,ou=Staff,dc=example,dc=com
cn: Clara Oswald
givenName: Clara
sn: Oswald
mail: clara@example.com
telephoneNumber: +44 20 7946 0991
o: Coal Hill School
title: Teacher

dn: cn=Danny Pink,ou=Staff,dc=example,dc=com
cn: Danny Pink
mail: danny@example.com
"#;

    #[test]
    fn parse_vcard_stream_creates_contact_items() {
        let items = parse_vcard_stream(SAMPLE_VCARD);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].full_name, "Dr. Erika Mustermann");
        assert_eq!(items[0].email.as_deref(), Some("erika@example.com"));
        assert_eq!(items[0].organization.as_deref(), Some("Musterfirma GmbH"));
        assert_eq!(items[1].full_name, "Max Mustermann");
    }

    #[test]
    fn parse_csv_stream_maps_headers_to_contact_items() {
        let items = parse_csv_stream(SAMPLE_CSV);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].full_name, "Anna Schmidt");
        assert_eq!(items[0].email.as_deref(), Some("anna@example.com"));
        assert_eq!(items[0].organization.as_deref(), Some("Hanseatic Tech"));
        assert_eq!(items[0].title.as_deref(), Some("CTO"));
        assert_eq!(items[1].full_name, "Bernd Bäcker");
        assert_eq!(items[1].email.as_deref(), Some("bernd@example.com"));
    }

    #[test]
    fn parse_ldif_stream_maps_ldap_attributes_accurately() {
        let items = parse_ldif_stream(SAMPLE_LDIF);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].full_name, "Clara Oswald");
        assert_eq!(items[0].email.as_deref(), Some("clara@example.com"));
        assert_eq!(items[0].organization.as_deref(), Some("Coal Hill School"));
        assert_eq!(items[0].title.as_deref(), Some("Teacher"));
        assert_eq!(items[1].full_name, "Danny Pink");
        assert_eq!(items[1].email.as_deref(), Some("danny@example.com"));
    }

    #[test]
    fn detect_format_handles_extensions_and_content_heuristics() {
        assert_eq!(
            detect_contact_format(Path::new("contacts.vcf"), ""),
            ContactImportFormat::VCard
        );
        assert_eq!(
            detect_contact_format(Path::new("contacts.csv"), ""),
            ContactImportFormat::Csv
        );
        assert_eq!(
            detect_contact_format(Path::new("export.ldif"), ""),
            ContactImportFormat::Ldif
        );
        assert_eq!(
            detect_contact_format(
                Path::new("unknown.txt"),
                "BEGIN:VCARD\r\nFN:Test\r\nEND:VCARD"
            ),
            ContactImportFormat::VCard
        );
        assert_eq!(
            detect_contact_format(Path::new("unknown.txt"), "dn: cn=Test\r\ncn: Test\r\n"),
            ContactImportFormat::Ldif
        );
    }

    #[test]
    fn plan_contact_import_finds_duplicates_and_summarizes() {
        let existing = vec![VCardContact::new(
            "Anna Schmidt",
            "anna@example.com",
            "",
            "",
        )];
        let plan = plan_contact_import(
            SAMPLE_CSV,
            ContactImportFormat::Csv,
            &existing,
            ContactDuplicatePolicy::default(),
        );
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.duplicate_count, 1);
        assert_eq!(plan.duplicate_names, vec!["Anna Schmidt"]);
        assert_eq!(plan.summary_label(), "2 contacts found (1 duplicate)");
    }

    #[test]
    fn duplicate_policies_behave_deterministically_with_contact_store() {
        let unique_suffix = uuid::Uuid::new_v4();
        let temp_path =
            std::env::temp_dir().join(format!("meridian-contact-import-test-{unique_suffix}"));
        std::fs::create_dir_all(&temp_path).unwrap();
        let store = ContactStoreService::with_writable_contact_dir(&temp_path);

        // Pre-create a contact
        let existing_contact = VCardContact::new("Anna Schmidt", "anna@example.com", "Alt", "Dev");
        store
            .create_contact(&existing_contact.to_vcard_string())
            .unwrap();

        let existing = vec![existing_contact];

        // 1. SkipDuplicates
        let plan_skip = plan_contact_import(
            SAMPLE_CSV,
            ContactImportFormat::Csv,
            &existing,
            ContactDuplicatePolicy::SkipExisting,
        );
        let res_skip = execute_contact_import(&plan_skip, &store).unwrap();
        assert_eq!(res_skip.skipped_count, 1);
        assert_eq!(res_skip.imported_count, 1); // Bernd Bäcker imported

        // 2. OverwriteExisting
        let plan_overwrite = plan_contact_import(
            SAMPLE_CSV,
            ContactImportFormat::Csv,
            &existing,
            ContactDuplicatePolicy::OverwriteExisting,
        );
        let res_overwrite = execute_contact_import(&plan_overwrite, &store).unwrap();
        assert_eq!(res_overwrite.overwritten_count, 2); // both Anna Schmidt and Bernd Bäcker overwritten

        let _ = std::fs::remove_dir_all(&temp_path);
    }
}
