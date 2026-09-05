// SPDX-License-Identifier: GPL-3.0-or-later
//
// Contact export service supporting vCard (.vcf), CSV (.csv), and LDIF (.ldif)
// with upstream Evolution addressbook exporter parity.

use super::vcard_service::VCardContact;

/// Supported contact export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactExportFormat {
    /// RFC 6350 / RFC 2426 vCard (`.vcf`, `.vcard`)
    VCard,
    /// Comma-separated values (`.csv`)
    Csv,
    /// RFC 2849 LDAP Data Interchange Format (`.ldif`, `.ldi`)
    Ldif,
}

impl ContactExportFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::VCard => "vcf",
            Self::Csv => "csv",
            Self::Ldif => "ldif",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::VCard => "text/vcard",
            Self::Csv => "text/csv",
            Self::Ldif => "text/x-ldif",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::VCard => "vCard (.vcf)",
            Self::Csv => "Kommagetrennte Werte (.csv)",
            Self::Ldif => "LDAP Data Interchange Format (.ldif)",
        }
    }
}

/// Options for CSV contact export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactCsvOptions {
    pub delimiter: String,
    pub quote: char,
    pub newline: String,
    pub include_header: bool,
}

impl Default for ContactCsvOptions {
    fn default() -> Self {
        Self {
            delimiter: ",".to_string(),
            quote: '"',
            newline: "\r\n".to_string(),
            include_header: true,
        }
    }
}

/// Escape a CSV cell value according to RFC 4180.
fn escape_csv_cell(cell: &str, delimiter: &str, quote: char) -> String {
    let quote_str = quote.to_string();
    let needs_quote = cell.contains(delimiter)
        || cell.contains(quote)
        || cell.contains('\n')
        || cell.contains('\r')
        || cell.starts_with(' ')
        || cell.ends_with(' ');

    if needs_quote {
        let escaped = cell.replace(&quote_str, &format!("{quote}{quote}"));
        format!("{quote}{escaped}{quote}")
    } else {
        cell.to_string()
    }
}

/// Helper to classify phone numbers into (business, home, mobile).
fn classify_phones(contact: &VCardContact) -> (String, String, String) {
    let mut business = String::new();
    let mut home = String::new();
    let mut mobile = String::new();

    // Check phone_properties for TYPE parameters first
    for prop in &contact.phone_properties {
        let prop_upper = prop.property.to_ascii_uppercase();
        if prop_upper.contains("CELL") || prop_upper.contains("MOBILE") {
            if mobile.is_empty() {
                mobile = prop.value.clone();
            }
        } else if prop_upper.contains("HOME") {
            if home.is_empty() {
                home = prop.value.clone();
            }
        } else if prop_upper.contains("WORK") || prop_upper.contains("BUSINESS") {
            if business.is_empty() {
                business = prop.value.clone();
            }
        }
    }

    // Fallback using phones list and primary tel
    if business.is_empty() && !contact.tel.is_empty() {
        business = contact.tel.clone();
    }
    for phone in &contact.phones {
        if phone == &business || phone == &home || phone == &mobile {
            continue;
        }
        if home.is_empty() {
            home = phone.clone();
        } else if mobile.is_empty() {
            mobile = phone.clone();
        }
    }

    (business, home, mobile)
}

/// Export a slice of contacts into the chosen format.
pub fn export_contacts(
    contacts: &[VCardContact],
    format: ContactExportFormat,
    options: Option<ContactCsvOptions>,
) -> Result<String, String> {
    if contacts.is_empty() {
        return Err("Keine Kontakte zum Exportieren ausgewählt".to_string());
    }

    match format {
        ContactExportFormat::VCard => export_contacts_to_vcard(contacts),
        ContactExportFormat::Csv => {
            let opts = options.unwrap_or_default();
            export_contacts_to_csv(contacts, &opts)
        }
        ContactExportFormat::Ldif => export_contacts_to_ldif(contacts),
    }
}

/// Export contacts to a continuous multi-record vCard payload.
fn export_contacts_to_vcard(contacts: &[VCardContact]) -> Result<String, String> {
    let mut output = String::new();
    for contact in contacts {
        let vcard = contact.to_vcard_string();
        let trimmed = vcard.trim_end_matches(['\r', '\n']);
        output.push_str(trimmed);
        output.push_str("\r\n");
    }
    Ok(output)
}

/// Export contacts to CSV format.
/// Headers: First Name, Last Name, Full Name, Email, Business Phone, Home Phone, Mobile Phone, Organization, Job Title, Address, Notes
fn export_contacts_to_csv(
    contacts: &[VCardContact],
    options: &ContactCsvOptions,
) -> Result<String, String> {
    let mut output = String::new();

    if options.include_header {
        let headers = [
            "First Name",
            "Last Name",
            "Full Name",
            "Email",
            "Business Phone",
            "Home Phone",
            "Mobile Phone",
            "Organization",
            "Job Title",
            "Address",
            "Notes",
        ];
        let header_row = headers
            .iter()
            .map(|h| escape_csv_cell(h, &options.delimiter, options.quote))
            .collect::<Vec<_>>()
            .join(&options.delimiter);
        output.push_str(&header_row);
        output.push_str(&options.newline);
    }

    for contact in contacts {
        let full_name = if !contact.fn_name.is_empty() {
            contact.fn_name.clone()
        } else if !contact.name.given.is_empty() || !contact.name.family.is_empty() {
            format!("{} {}", contact.name.given, contact.name.family)
                .trim()
                .to_string()
        } else if contact.is_list {
            contact.list_name.clone()
        } else {
            String::new()
        };

        let email = if !contact.email.is_empty() {
            contact.email.clone()
        } else {
            contact.emails.first().cloned().unwrap_or_default()
        };

        let (work_phone, home_phone, mobile_phone) = classify_phones(contact);
        let address = contact.addresses.join("; ");

        let cells = [
            escape_csv_cell(&contact.name.given, &options.delimiter, options.quote),
            escape_csv_cell(&contact.name.family, &options.delimiter, options.quote),
            escape_csv_cell(&full_name, &options.delimiter, options.quote),
            escape_csv_cell(&email, &options.delimiter, options.quote),
            escape_csv_cell(&work_phone, &options.delimiter, options.quote),
            escape_csv_cell(&home_phone, &options.delimiter, options.quote),
            escape_csv_cell(&mobile_phone, &options.delimiter, options.quote),
            escape_csv_cell(&contact.org, &options.delimiter, options.quote),
            escape_csv_cell(&contact.title, &options.delimiter, options.quote),
            escape_csv_cell(&address, &options.delimiter, options.quote),
            escape_csv_cell(&contact.note, &options.delimiter, options.quote),
        ];

        output.push_str(&cells.join(&options.delimiter));
        output.push_str(&options.newline);
    }

    Ok(output)
}

/// Export contacts to RFC 2849 LDIF format.
fn export_contacts_to_ldif(contacts: &[VCardContact]) -> Result<String, String> {
    let mut output = String::new();

    for contact in contacts {
        let full_name = if !contact.fn_name.is_empty() {
            contact.fn_name.clone()
        } else if !contact.name.given.is_empty() || !contact.name.family.is_empty() {
            format!("{} {}", contact.name.given, contact.name.family)
                .trim()
                .to_string()
        } else if contact.is_list {
            contact.list_name.clone()
        } else {
            "Unknown".to_string()
        };

        let email = if !contact.email.is_empty() {
            contact.email.clone()
        } else {
            contact.emails.first().cloned().unwrap_or_default()
        };

        // Construct DN
        let dn = if !email.is_empty() {
            format!("cn={},mail={}", full_name.trim(), email.trim())
        } else {
            format!("cn={}", full_name.trim())
        };

        output.push_str(&format!("dn: {dn}\n"));
        output.push_str("objectClass: top\n");
        output.push_str("objectClass: person\n");
        output.push_str("objectClass: organizationalPerson\n");
        output.push_str("objectClass: inetOrgPerson\n");
        output.push_str(&format!("cn: {}\n", full_name.trim()));

        let family = if !contact.name.family.is_empty() {
            contact.name.family.trim()
        } else {
            full_name.trim()
        };
        output.push_str(&format!("sn: {family}\n"));

        if !contact.name.given.is_empty() {
            output.push_str(&format!("givenName: {}\n", contact.name.given.trim()));
        }

        if !email.is_empty() {
            output.push_str(&format!("mail: {}\n", email.trim()));
        }

        // Additional emails
        for mail in &contact.emails {
            if mail.trim() != email.trim() && !mail.trim().is_empty() {
                output.push_str(&format!("mail: {}\n", mail.trim()));
            }
        }

        let (work_phone, home_phone, mobile_phone) = classify_phones(contact);
        if !work_phone.is_empty() {
            output.push_str(&format!("telephoneNumber: {}\n", work_phone.trim()));
        }
        if !home_phone.is_empty() {
            output.push_str(&format!("homePhone: {}\n", home_phone.trim()));
        }
        if !mobile_phone.is_empty() {
            output.push_str(&format!("mobile: {}\n", mobile_phone.trim()));
        }

        if !contact.org.is_empty() {
            output.push_str(&format!("o: {}\n", contact.org.trim()));
        }
        if !contact.title.is_empty() {
            output.push_str(&format!("title: {}\n", contact.title.trim()));
        }
        if let Some(first_addr) = contact.addresses.first() {
            if !first_addr.is_empty() {
                output.push_str(&format!("postalAddress: {}\n", first_addr.trim()));
            }
        }
        if !contact.note.is_empty() {
            // Unfold notes to single line or wrap
            let clean_note = contact.note.replace(['\r', '\n'], " ");
            output.push_str(&format!("description: {}\n", clean_note.trim()));
        }

        output.push('\n');
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::vcard_service::{VCardName, VCardProperty};

    fn sample_contacts() -> Vec<VCardContact> {
        let mut ada = VCardContact::new(
            "Ada Lovelace",
            "ada@analytical.org",
            "Babbage Engines Inc.",
            "Chief Mathematician",
        );
        ada.name = VCardName {
            given: "Ada".to_string(),
            family: "Lovelace".to_string(),
            ..VCardName::default()
        };
        ada.tel = "+44 20 7946 0991".to_string();
        ada.phones = vec!["+44 20 7946 0991".to_string()];
        ada.note = "First computer programmer".to_string();
        ada.addresses = vec!["12 St James Square, London".to_string()];
        ada.categories = vec!["Pioneers".to_string()];
        ada.phone_properties = vec![VCardProperty {
            property: "TEL;TYPE=WORK".to_string(),
            value: "+44 20 7946 0991".to_string(),
            raw_value: "+44 20 7946 0991".to_string(),
        }];

        let mut grace =
            VCardContact::new("Grace Hopper", "grace@navy.mil", "US Navy", "Rear Admiral");
        grace.name = VCardName {
            given: "Grace".to_string(),
            family: "Hopper".to_string(),
            ..VCardName::default()
        };
        grace.tel = "+1 703 555 0123".to_string();
        grace.phones = vec!["+1 703 555 0123".to_string()];
        grace.note = "Compiler inventor, COBOL pioneer".to_string();
        grace.addresses = vec!["Arlington, VA".to_string()];
        grace.categories = vec!["Computing".to_string()];
        grace.phone_properties = vec![VCardProperty {
            property: "TEL;TYPE=CELL".to_string(),
            value: "+1 703 555 0123".to_string(),
            raw_value: "+1 703 555 0123".to_string(),
        }];

        vec![ada, grace]
    }

    #[test]
    fn export_contacts_vcard_aggregates_multiple_contacts() {
        let contacts = sample_contacts();
        let vcard = export_contacts(&contacts, ContactExportFormat::VCard, None).unwrap();

        assert!(vcard.contains("BEGIN:VCARD"));
        assert!(vcard.contains("FN:Ada Lovelace"));
        assert!(vcard.contains("ada@analytical.org"));
        assert!(vcard.contains("FN:Grace Hopper"));
        assert!(vcard.contains("grace@navy.mil"));
        assert!(vcard.ends_with("\r\n"));
    }

    #[test]
    fn export_contacts_csv_produces_rfc4180_headers_and_quotes() {
        let contacts = sample_contacts();
        let csv = export_contacts(&contacts, ContactExportFormat::Csv, None).unwrap();

        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines.len() >= 3);
        assert_eq!(
            lines[0],
            "First Name,Last Name,Full Name,Email,Business Phone,Home Phone,Mobile Phone,Organization,Job Title,Address,Notes"
        );

        // Ada Lovelace
        assert!(lines[1].contains("Ada,Lovelace,Ada Lovelace,ada@analytical.org"));
        assert!(lines[1].contains("\"12 St James Square, London\""));
        assert!(lines[1].contains("Babbage Engines Inc."));

        // Grace Hopper (comma in note should cause quotes)
        assert!(lines[2].contains("Grace,Hopper,Grace Hopper,grace@navy.mil"));
        assert!(lines[2].contains("\"Compiler inventor, COBOL pioneer\""));
    }

    #[test]
    fn export_contacts_ldif_produces_rfc2849_records_and_attributes() {
        let contacts = sample_contacts();
        let ldif = export_contacts(&contacts, ContactExportFormat::Ldif, None).unwrap();

        assert!(ldif.contains("dn: cn=Ada Lovelace,mail=ada@analytical.org"));
        assert!(ldif.contains("objectClass: inetOrgPerson"));
        assert!(ldif.contains("cn: Ada Lovelace"));
        assert!(ldif.contains("sn: Lovelace"));
        assert!(ldif.contains("givenName: Ada"));
        assert!(ldif.contains("mail: ada@analytical.org"));
        assert!(ldif.contains("o: Babbage Engines Inc."));
        assert!(ldif.contains("title: Chief Mathematician"));
        assert!(ldif.contains("telephoneNumber: +44 20 7946 0991"));

        assert!(ldif.contains("dn: cn=Grace Hopper,mail=grace@navy.mil"));
        assert!(ldif.contains("mobile: +1 703 555 0123"));
    }

    #[test]
    fn export_contacts_handles_empty_selection_fail_closed() {
        let empty: Vec<VCardContact> = Vec::new();
        assert!(export_contacts(&empty, ContactExportFormat::VCard, None).is_err());
        assert!(export_contacts(&empty, ContactExportFormat::Csv, None).is_err());
        assert!(export_contacts(&empty, ContactExportFormat::Ldif, None).is_err());
    }
}
