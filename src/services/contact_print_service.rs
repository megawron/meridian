// SPDX-License-Identifier: GPL-3.0-or-later
//
// Backend-neutral contact and address book print snapshot and document contract.

use std::collections::HashSet;

use crate::services::contact_search_service::ContactIdentity;
use crate::services::ebook_service::EBookContactRecord;
use crate::services::vcard_service::VCardContact;

/// Deterministic, self-contained contact print document (cards).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactPrintDocument {
    pub html: String,
    pub plain_text: String,
    pub contact_count: usize,
}

/// Deterministic, self-contained address book contact list print document (table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactListPrintDocument {
    pub html: String,
    pub plain_text: String,
    pub contact_count: usize,
}

fn contact_display_name(contact: &VCardContact) -> String {
    if contact.is_list {
        let name = contact.list_name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    let fn_name = contact.fn_name.trim();
    if !fn_name.is_empty() {
        return fn_name.to_string();
    }
    let family = contact.name.family.trim();
    let given = contact.name.given.trim();
    if !family.is_empty() && !given.is_empty() {
        format!("{given} {family}")
    } else if !family.is_empty() {
        family.to_string()
    } else if !given.is_empty() {
        given.to_string()
    } else {
        "Unnamed Contact".to_string()
    }
}

pub fn contact_sort_key_for_print(contact: &VCardContact) -> String {
    let family = contact.name.family.trim();
    let given = contact.name.given.trim();
    let name = if family.is_empty() {
        contact.fn_name.trim().to_string()
    } else if given.is_empty() {
        family.to_string()
    } else {
        format!("{family} {given}")
    };
    if name.is_empty() && contact.is_list {
        contact.list_name.trim().to_lowercase()
    } else {
        name.to_lowercase()
    }
}

fn contact_initial_letter(sort_key: &str, display_name: &str) -> String {
    let candidate = sort_key
        .chars()
        .find(|c| c.is_alphanumeric())
        .or_else(|| display_name.chars().find(|c| c.is_alphanumeric()))
        .unwrap_or('#');
    candidate.to_uppercase().to_string()
}

fn contact_organization_meta(contact: &VCardContact) -> Option<String> {
    let org = contact.org.trim();
    let title = contact.title.trim();
    let role = contact.role.trim();

    let mut parts = Vec::new();
    if !title.is_empty() {
        parts.push(title);
    } else if !role.is_empty() {
        parts.push(role);
    }
    if !org.is_empty() {
        parts.push(org);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn contact_card_html_and_text(record: &EBookContactRecord) -> (String, String) {
    let contact = &record.contact;
    let display_name = contact_display_name(contact);
    let escaped_name = glib::markup_escape_text(&display_name);
    let org_meta = contact_organization_meta(contact);

    let mut html_fields = Vec::new();
    let mut text_fields = Vec::new();

    // Emails
    let mut emails: Vec<String> = Vec::new();
    if !contact.email.trim().is_empty() {
        emails.push(contact.email.trim().to_string());
    }
    for e in &contact.emails {
        let trimmed = e.trim();
        if !trimmed.is_empty() && !emails.contains(&trimmed.to_string()) {
            emails.push(trimmed.to_string());
        }
    }
    if !emails.is_empty() {
        let label = if emails.len() == 1 {
            "Email"
        } else {
            "Emails"
        };
        let escaped_emails = emails
            .iter()
            .map(|e| glib::markup_escape_text(e).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        html_fields.push(format!("<dt>{label}</dt><dd>{escaped_emails}</dd>"));
        text_fields.push(format!("{label}: {}", emails.join(", ")));
    }

    // Phones
    let mut phones: Vec<String> = Vec::new();
    if !contact.tel.trim().is_empty() {
        phones.push(contact.tel.trim().to_string());
    }
    for p in &contact.phones {
        let trimmed = p.trim();
        if !trimmed.is_empty() && !phones.contains(&trimmed.to_string()) {
            phones.push(trimmed.to_string());
        }
    }
    if !phones.is_empty() {
        let label = if phones.len() == 1 {
            "Phone"
        } else {
            "Phones"
        };
        let escaped_phones = phones
            .iter()
            .map(|p| glib::markup_escape_text(p).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        html_fields.push(format!("<dt>{label}</dt><dd>{escaped_phones}</dd>"));
        text_fields.push(format!("{label}: {}", phones.join(", ")));
    }

    // Addresses
    if !contact.addresses.is_empty() {
        let label = if contact.addresses.len() == 1 {
            "Address"
        } else {
            "Addresses"
        };
        let escaped_addrs = contact
            .addresses
            .iter()
            .map(|a| glib::markup_escape_text(a.trim()).to_string())
            .collect::<Vec<_>>()
            .join("; ");
        html_fields.push(format!("<dt>{label}</dt><dd>{escaped_addrs}</dd>"));
        text_fields.push(format!("{label}: {}", contact.addresses.join("; ")));
    }

    // Web / URLs
    if !contact.urls.is_empty() {
        let label = if contact.urls.len() == 1 {
            "Website"
        } else {
            "Websites"
        };
        let escaped_urls = contact
            .urls
            .iter()
            .map(|u| glib::markup_escape_text(u.trim()).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        html_fields.push(format!("<dt>{label}</dt><dd>{escaped_urls}</dd>"));
        text_fields.push(format!("{label}: {}", contact.urls.join(", ")));
    }

    // Birthday / Anniversary
    if !contact.birthday.trim().is_empty() {
        html_fields.push(format!(
            "<dt>Birthday</dt><dd>{}</dd>",
            glib::markup_escape_text(contact.birthday.trim())
        ));
        text_fields.push(format!("Birthday: {}", contact.birthday.trim()));
    }
    if !contact.anniversary.trim().is_empty() {
        html_fields.push(format!(
            "<dt>Anniversary</dt><dd>{}</dd>",
            glib::markup_escape_text(contact.anniversary.trim())
        ));
        text_fields.push(format!("Anniversary: {}", contact.anniversary.trim()));
    }

    // Categories
    if !contact.categories.is_empty() {
        let cats = contact.categories.join(", ");
        html_fields.push(format!(
            "<dt>Categories</dt><dd>{}</dd>",
            glib::markup_escape_text(&cats)
        ));
        text_fields.push(format!("Categories: {cats}"));
    }

    // Address book source
    if !record.source_display_name.trim().is_empty() {
        html_fields.push(format!(
            "<dt>Address Book</dt><dd>{}</dd>",
            glib::markup_escape_text(record.source_display_name.trim())
        ));
        text_fields.push(format!("Address Book: {}", record.source_display_name.trim()));
    }

    // Contact list
    let mut list_html = String::new();
    let mut list_text = Vec::new();
    if contact.is_list {
        let member_count = contact.list_members.len();
        list_html.push_str(&format!(
            "<div class=\"contact-list-section\"><h3>Members ({member_count})</h3><ul>"
        ));
        list_text.push(format!("Members ({member_count}):"));
        for member in &contact.list_members {
            let m_text = member.display_text(contact.list_show_addresses);
            list_html.push_str(&format!("<li>{}</li>", glib::markup_escape_text(&m_text)));
            list_text.push(format!("  • {m_text}"));
        }
        list_html.push_str("</ul></div>");
    }

    // Notes
    let mut note_html = String::new();
    let mut note_text = String::new();
    if !contact.note.trim().is_empty() {
        note_html.push_str(&format!(
            "<div class=\"contact-notes\"><h4>Notes</h4><pre>{}</pre></div>",
            glib::markup_escape_text(contact.note.trim())
        ));
        note_text = format!("Notes:\n{}", contact.note.trim());
    }

    let meta_html = if let Some(meta) = &org_meta {
        format!(
            "<div class=\"contact-meta\">{}</div>",
            glib::markup_escape_text(meta)
        )
    } else {
        String::new()
    };

    let html = format!(
        "<article class=\"contact-card\"><h2>{escaped_name}</h2>{meta_html}<dl class=\"contact-fields\">{}</dl>{list_html}{note_html}</article>",
        html_fields.join("")
    );

    let mut text_lines = Vec::new();
    text_lines.push(display_name);
    if let Some(meta) = org_meta {
        text_lines.push(meta);
    }
    text_lines.extend(text_fields);
    if !list_text.is_empty() {
        text_lines.extend(list_text);
    }
    if !note_text.is_empty() {
        text_lines.push(note_text);
    }

    (html, text_lines.join("\n"))
}

const PRINT_STYLESHEET: &str = r#"@page{size:auto;margin:15mm 15mm}body{color:#202124;background:#fff;font-family:system-ui,-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;font-size:10pt;line-height:1.35;margin:0}h1.print-title{font-size:15pt;border-bottom:2px solid #202124;padding-bottom:3pt;margin:0 0 10pt}.letter-heading{background:#202124;color:#fff;display:inline-block;font-size:11pt;font-weight:bold;padding:2pt 7pt;margin:10pt 0 5pt;border-radius:2pt}.contact-card{border:1px solid #ccc;border-radius:3pt;padding:8pt 10pt;margin-bottom:8pt;page-break-inside:avoid;break-inside:avoid}.contact-card h2{font-size:12pt;margin:0 0 2pt;color:#1a73e8}.contact-card .contact-meta{font-size:9.5pt;color:#555;margin-bottom:6pt}dl.contact-fields{display:grid;grid-template-columns:max-content 1fr;column-gap:10pt;row-gap:2pt;margin:0}dt{font-weight:600;color:#555}dd{margin:0;overflow-wrap:anywhere}.contact-list-section{margin-top:6pt;border-top:1px solid #eee;padding-top:4pt}.contact-list-section h3{font-size:10pt;margin:0 0 2pt}.contact-list-section ul{margin:0;padding-left:14pt}.contact-notes{margin-top:6pt;border-top:1px solid #eee;padding-top:4pt}.contact-notes h4{font-size:9.5pt;margin:0 0 2pt;color:#555}.contact-notes pre{white-space:pre-wrap;font-family:inherit;margin:0;font-size:9pt}table.contact-table{width:100%;border-collapse:collapse;margin-top:8pt}table.contact-table th,table.contact-table td{border:1px solid #ccc;padding:4pt 6pt;text-align:left;font-size:9pt}table.contact-table th{background:#f1f3f4;font-weight:600}"#;

/// Prepare a printable document for selected contacts (1 or n cards).
pub fn build_contact_print_document(
    selection: &[EBookContactRecord],
) -> Result<ContactPrintDocument, String> {
    if selection.is_empty() {
        return Err("No contacts selected for printing".to_string());
    }

    let mut seen = HashSet::new();
    for record in selection {
        let identity = ContactIdentity::new(record.source_uid.clone(), record.uid.clone());
        if !seen.insert(identity) {
            return Err("Selection contains duplicate contacts".to_string());
        }
    }

    // Sort contacts alphabetically
    let mut sorted = selection.to_vec();
    sorted.sort_by_cached_key(|r| contact_sort_key_for_print(&r.contact));

    let contact_count = sorted.len();
    let is_multiple = contact_count > 1;

    let doc_title = if contact_count == 1 {
        contact_display_name(&sorted[0].contact)
    } else {
        format!("{contact_count} Contacts")
    };
    let escaped_doc_title = glib::markup_escape_text(&doc_title);

    let mut html_parts = Vec::new();
    let mut text_sections = Vec::new();

    html_parts.push(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{escaped_doc_title}</title><style>{PRINT_STYLESHEET}</style></head><body><h1 class=\"print-title\">{escaped_doc_title}</h1>"
    ));

    let mut current_letter = String::new();

    for record in &sorted {
        let sort_key = contact_sort_key_for_print(&record.contact);
        let disp_name = contact_display_name(&record.contact);

        if is_multiple {
            let letter = contact_initial_letter(&sort_key, &disp_name);
            if letter != current_letter {
                current_letter = letter.clone();
                let escaped_letter = glib::markup_escape_text(&letter);
                html_parts.push(format!(
                    "<div class=\"letter-heading\">{escaped_letter}</div>"
                ));
                text_sections.push(format!("\n[{letter}]\n{}", "=".repeat(40)));
            }
        }

        let (card_html, card_text) = contact_card_html_and_text(record);
        html_parts.push(card_html);
        text_sections.push(card_text);
    }

    html_parts.push("</body></html>".to_string());

    let plain_text = format!(
        "{doc_title}\n{}\n\n{}",
        "=".repeat(doc_title.len().max(20)),
        text_sections.join("\n\n")
    );

    Ok(ContactPrintDocument {
        html: html_parts.join(""),
        plain_text,
        contact_count,
    })
}

/// Prepare a printable document for the visible address book contact list (table view).
pub fn build_contact_list_print_document(
    visible: &[EBookContactRecord],
) -> Result<ContactListPrintDocument, String> {
    if visible.is_empty() {
        return Err("No visible contacts available to print".to_string());
    }

    let mut seen = HashSet::new();
    for record in visible {
        let identity = ContactIdentity::new(record.source_uid.clone(), record.uid.clone());
        if !seen.insert(identity) {
            return Err("Visible contact list contains a duplicate identity".to_string());
        }
    }

    let contact_count = visible.len();
    let doc_title = format!("Contacts ({contact_count} entries)");
    let escaped_doc_title = glib::markup_escape_text(&doc_title);

    let mut html_rows = Vec::with_capacity(visible.len());
    let mut text_rows = Vec::with_capacity(visible.len());

    for record in visible {
        let contact = &record.contact;
        let name = contact_display_name(contact);
        let org_meta = contact_organization_meta(contact).unwrap_or_default();
        let email = if !contact.email.trim().is_empty() {
            contact.email.trim().to_string()
        } else {
            contact
                .emails
                .first()
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let phone = if !contact.tel.trim().is_empty() {
            contact.tel.trim().to_string()
        } else {
            contact
                .phones
                .first()
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let source = record.source_display_name.trim().to_string();
        let categories = contact.categories.join(", ");

        let cells = [
            glib::markup_escape_text(&name),
            glib::markup_escape_text(&org_meta),
            glib::markup_escape_text(&email),
            glib::markup_escape_text(&phone),
            glib::markup_escape_text(&source),
            glib::markup_escape_text(&categories),
        ];

        html_rows.push(format!(
            "<tr>{}</tr>",
            cells
                .iter()
                .map(|val| format!("<td>{val}</td>"))
                .collect::<Vec<_>>()
                .join("")
        ));

        text_rows.push(format!(
            "{name} | {org_meta} | {email} | {phone} | {source} | {categories}"
        ));
    }

    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{escaped_doc_title}</title><style>{PRINT_STYLESHEET}</style></head><body><h1 class=\"print-title\">{escaped_doc_title}</h1><table class=\"contact-table\"><thead><tr><th>Name</th><th>Organization / Role</th><th>Email</th><th>Phone</th><th>Address Book</th><th>Categories</th></tr></thead><tbody>{}</tbody></table></body></html>",
        html_rows.join("")
    );

    let header_text = "Name | Organization / Role | Email | Phone | Address Book | Categories";
    let plain_text = format!(
        "{doc_title}\n{}\n{header_text}\n{}\n{}",
        "=".repeat(doc_title.len().max(20)),
        "-".repeat(header_text.len()),
        text_rows.join("\n")
    );

    Ok(ContactListPrintDocument {
        html,
        plain_text,
        contact_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::vcard_service::{VCardListMember, VCardName};

    fn make_test_record(
        source_uid: &str,
        source_name: &str,
        uid: &str,
        family: &str,
        given: &str,
        email: &str,
        org: &str,
    ) -> EBookContactRecord {
        let mut contact = VCardContact::new(&format!("{given} {family}"), email, org, "Engineer");
        contact.name = VCardName {
            family: family.to_string(),
            given: given.to_string(),
            additional: String::new(),
            prefix: String::new(),
            suffix: String::new(),
        };
        contact.tel = "+49 123 456789".to_string();
        contact.categories = vec!["Colleagues".to_string()];
        contact.note = "Important note text\nwith line break".to_string();
        EBookContactRecord {
            source_uid: source_uid.to_string(),
            source_display_name: source_name.to_string(),
            uid: uid.to_string(),
            vcard: String::new(),
            contact,
        }
    }

    #[test]
    fn print_selection_fails_closed_on_empty() {
        let err = build_contact_print_document(&[]).unwrap_err();
        assert_eq!(err, "No contacts selected for printing");
    }

    #[test]
    fn print_selection_fails_closed_on_duplicates() {
        let r1 = make_test_record(
            "src1",
            "Local",
            "uid1",
            "Lovelace",
            "Ada",
            "ada@example.com",
            "Tech",
        );
        let r2 = make_test_record(
            "src1",
            "Local",
            "uid1",
            "Lovelace",
            "Ada",
            "ada@example.com",
            "Tech",
        );
        let err = build_contact_print_document(&[r1, r2]).unwrap_err();
        assert_eq!(err, "Selection contains duplicate contacts");
    }

    #[test]
    fn print_single_contact_card_renders_cleanly() {
        let r = make_test_record(
            "local",
            "My Address Book",
            "c1",
            "Lovelace",
            "Ada",
            "ada@example.com",
            "Analytics Corp",
        );
        let doc = build_contact_print_document(&[r]).expect("valid single document");
        assert_eq!(doc.contact_count, 1);
        assert!(doc.html.contains("<title>Ada Lovelace</title>"));
        assert!(
            doc.html
                .contains("<h1 class=\"print-title\">Ada Lovelace</h1>")
        );
        assert!(doc.html.contains("<h2>Ada Lovelace</h2>"));
        assert!(doc.html.contains("Engineer, Analytics Corp"));
        assert!(doc.html.contains("<dt>Email</dt><dd>ada@example.com</dd>"));
        assert!(doc.html.contains("<dt>Phone</dt><dd>+49 123 456789</dd>"));
        assert!(doc.html.contains("<dt>Categories</dt><dd>Colleagues</dd>"));
        assert!(
            doc.html
                .contains("<dt>Address Book</dt><dd>My Address Book</dd>")
        );
        assert!(doc.html.contains("<div class=\"contact-notes\"><h4>Notes</h4><pre>Important note text\nwith line break</pre></div>"));
        assert!(doc.plain_text.contains("Ada Lovelace"));
        assert!(doc.plain_text.contains("Email: ada@example.com"));
    }

    #[test]
    fn print_multiple_contacts_groups_alphabetically_with_letter_headings() {
        let r_b = make_test_record(
            "local",
            "Local",
            "u2",
            "Babbage",
            "Charles",
            "charles@example.com",
            "UK",
        );
        let r_a = make_test_record(
            "local",
            "Local",
            "u1",
            "Ada",
            "Countess",
            "ada@example.com",
            "UK",
        );
        let r_t = make_test_record(
            "local",
            "Local",
            "u3",
            "Turing",
            "Alan",
            "alan@example.com",
            "UK",
        );

        // Input unsorted
        let doc = build_contact_print_document(&[r_t, r_a, r_b]).expect("multiple print document");
        assert_eq!(doc.contact_count, 3);
        assert!(doc.html.contains("<title>3 Contacts</title>"));
        assert!(doc.html.contains("<div class=\"letter-heading\">A</div>"));
        assert!(doc.html.contains("<div class=\"letter-heading\">B</div>"));
        assert!(doc.html.contains("<div class=\"letter-heading\">T</div>"));

        // Verify order in HTML: A before B before T
        let pos_a = doc
            .html
            .find("<div class=\"letter-heading\">A</div>")
            .unwrap();
        let pos_b = doc
            .html
            .find("<div class=\"letter-heading\">B</div>")
            .unwrap();
        let pos_t = doc
            .html
            .find("<div class=\"letter-heading\">T</div>")
            .unwrap();
        assert!(pos_a < pos_b && pos_b < pos_t);

        // Verify plain text groups
        assert!(doc.plain_text.contains("[A]"));
        assert!(doc.plain_text.contains("[B]"));
        assert!(doc.plain_text.contains("[T]"));
    }

    #[test]
    fn print_contact_list_renders_members_section() {
        let mut list_contact = VCardContact::new("Project Team", "", "", "");
        list_contact.is_list = true;
        list_contact.list_name = "Project Team".to_string();
        list_contact.list_show_addresses = true;
        list_contact.list_members = vec![
            VCardListMember::external_address("ext@example.com", "External Partner"),
            VCardListMember::local_contact(
                "c1",
                "local",
                "Ada Lovelace",
                "ada@example.com",
                Some(0),
            ),
        ];

        let record = EBookContactRecord {
            source_uid: "local".to_string(),
            source_display_name: "Local Address Book".to_string(),
            uid: "list-1".to_string(),
            vcard: String::new(),
            contact: list_contact,
        };

        let doc = build_contact_print_document(&[record]).expect("list contact print document");
        assert_eq!(doc.contact_count, 1);
        assert!(doc.html.contains("Project Team"));
        assert!(doc.html.contains("Members (2)"));
        assert!(
            doc.html
                .contains("External Partner &lt;ext@example.com&gt;")
        );
        assert!(doc.html.contains("Ada Lovelace &lt;ada@example.com&gt;"));
        assert!(doc.plain_text.contains("Members (2):"));
        assert!(
            doc.plain_text
                .contains("External Partner <ext@example.com>")
        );
    }

    #[test]
    fn print_escapes_xss_content_strictly() {
        let mut evil_contact = VCardContact::new(
            "<script>alert('xss')</script>",
            "\"foo@example.com",
            "<b>Evil Org</b>",
            "<img src=x onerror=alert(1)>",
        );
        evil_contact.name.family = "<evil>".to_string();
        evil_contact.note = "</pre><script>malicious()</script>".to_string();

        let record = EBookContactRecord {
            source_uid: "local".to_string(),
            source_display_name: "<i>Injected</i>".to_string(),
            uid: "xss1".to_string(),
            vcard: String::new(),
            contact: evil_contact,
        };

        let doc = build_contact_print_document(&[record]).expect("escaped document");
        assert!(!doc.html.contains("<script>"));
        assert!(!doc.html.contains("alert('xss')"));
        assert!(!doc.html.contains("<img src=x"));
        assert!(
            doc.html
                .contains("&lt;script&gt;alert(&apos;xss&apos;)&lt;/script&gt;")
        );
        assert!(doc.html.contains("&lt;b&gt;Evil Org&lt;/b&gt;"));
        assert!(doc.html.contains("&lt;i&gt;Injected&lt;/i&gt;"));
    }

    #[test]
    fn print_contact_list_table_fails_on_empty_and_duplicates() {
        assert_eq!(
            build_contact_list_print_document(&[]).unwrap_err(),
            "No visible contacts available to print"
        );

        let r1 = make_test_record("s", "L", "u1", "A", "B", "a@b.c", "O");
        let r2 = make_test_record("s", "L", "u1", "A", "B", "a@b.c", "O");
        assert_eq!(
            build_contact_list_print_document(&[r1, r2]).unwrap_err(),
            "Visible contact list contains a duplicate identity"
        );
    }

    #[test]
    fn print_contact_list_table_renders_valid_html_and_text() {
        let r1 = make_test_record(
            "local",
            "Local",
            "u1",
            "Lovelace",
            "Ada",
            "ada@example.com",
            "Tech Corp",
        );
        let r2 = make_test_record(
            "local",
            "Local",
            "u2",
            "Babbage",
            "Charles",
            "charles@example.com",
            "Cam Corp",
        );

        let doc = build_contact_list_print_document(&[r1, r2]).expect("valid list doc");
        assert_eq!(doc.contact_count, 2);
        assert!(doc.html.contains("<title>Contacts (2 entries)</title>"));
        assert!(doc.html.contains("<table class=\"contact-table\">"));
        assert!(doc.html.contains("<td>Ada Lovelace</td>"));
        assert!(doc.html.contains("<td>Engineer, Tech Corp</td>"));
        assert!(doc.html.contains("<td>ada@example.com</td>"));
        assert!(doc.html.contains("<td>+49 123 456789</td>"));
        assert!(doc.html.contains("<td>Charles Babbage</td>"));
        assert!(doc.plain_text.contains("Contacts (2 entries)"));
        assert!(doc.plain_text.contains("Ada Lovelace | Engineer, Tech Corp | ada@example.com | +49 123 456789 | Local | Kollegen"));
    }
}
