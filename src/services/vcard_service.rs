// SPDX-License-Identifier: GPL-3.0-or-later
//
// vcard_service — Service utility for parsing and serializing vCard 4.0 (RFC 6350) contacts.
//
// Used for importing and exporting contacts to/from Evolution Data Server (libebook).

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardContact {
    pub fn_name: String,
    pub email: String,
    pub org: String,
    pub title: String,
    pub tel: String,
    pub note: String,
}

#[allow(dead_code)]
impl VCardContact {
    pub fn new(name: &str, email: &str, org: &str, title: &str) -> Self {
        Self {
            fn_name: name.to_string(),
            email: email.to_string(),
            org: org.to_string(),
            title: title.to_string(),
            tel: String::new(),
            note: String::new(),
        }
    }

    /// Serialize this contact into vCard 4.0 format.
    pub fn to_vcard_string(&self) -> String {
        format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:{}\r\nEMAIL;TYPE=INTERNET:{}\r\nORG:{}\r\nTITLE:{}\r\nTEL;TYPE=CELL:{}\r\nNOTE:{}\r\nEND:VCARD\r\n",
            self.fn_name, self.email, self.org, self.title, self.tel, self.note
        )
    }

    /// Parse a single vCard string into a VCardContact struct.
    pub fn parse_vcard_string(vcard: &str) -> Option<Self> {
        if !vcard
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("BEGIN:VCARD"))
            || !vcard
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case("END:VCARD"))
        {
            return None;
        }

        let mut fn_name = String::new();
        let mut email = String::new();
        let mut org = String::new();
        let mut title = String::new();
        let mut tel = String::new();
        let mut note = String::new();

        let mut lines: Vec<String> = Vec::new();
        for raw_line in vcard.replace("\r\n", "\n").replace('\r', "\n").lines() {
            if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
                if let Some(previous) = lines.last_mut() {
                    previous.push_str(raw_line.trim_start());
                }
            } else {
                lines.push(raw_line.trim().to_string());
            }
        }

        for line in lines {
            let Some((property, value)) = line.split_once(':') else {
                continue;
            };
            let property_name = property
                .split(';')
                .next()
                .unwrap_or(property)
                .to_ascii_uppercase();
            let value = unescape_value(value);
            match property_name.as_str() {
                "FN" => fn_name = value,
                "EMAIL" => {
                    if email.is_empty() {
                        email = value;
                    }
                }
                "ORG" => org = value,
                "TITLE" => title = value,
                "TEL" => {
                    if tel.is_empty() {
                        tel = value;
                    }
                }
                "NOTE" => note = value,
                _ => {}
            }
        }

        if fn_name.is_empty() {
            return None;
        }

        Some(Self {
            fn_name,
            email,
            org,
            title,
            tel,
            note,
        })
    }
}

fn unescape_value(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vcard_serialization_roundtrip() {
        let contact = VCardContact::new(
            "Max Mustermann",
            "max@example.com",
            "ACME Corp",
            "Senior Dev",
        );
        let vcard_str = contact.to_vcard_string();
        assert!(vcard_str.contains("BEGIN:VCARD"));
        assert!(vcard_str.contains("FN:Max Mustermann"));

        let parsed = VCardContact::parse_vcard_string(&vcard_str).expect("Parsing failed");
        assert_eq!(parsed.fn_name, "Max Mustermann");
        assert_eq!(parsed.email, "max@example.com");
        assert_eq!(parsed.org, "ACME Corp");
        assert_eq!(parsed.title, "Senior Dev");
    }
}
