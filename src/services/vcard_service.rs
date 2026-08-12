// SPDX-License-Identifier: GPL-3.0-or-later
//
// vcard_service — Service utility for parsing and serializing vCard 4.0 (RFC 6350) contacts.
//
// Used for importing and exporting contacts to/from Evolution Data Server (libebook).

#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VCardName {
    pub family: String,
    pub given: String,
    pub additional: String,
    pub prefix: String,
    pub suffix: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardPhoto {
    pub media_type: String,
    pub value: String,
    pub encoding: Option<String>,
}

/// A vCard property which is not part of the portable core model.
///
/// Evolution stores a number of address-book-specific properties under the
/// `X-EVOLUTION-*` namespace. Keeping the complete property name (including
/// parameters) lets the portable parser and exporter round-trip those fields
/// without silently discarding data from an existing address book. The same
/// container is used for other `X-*` extensions from providers and imported
/// vCards.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardExtension {
    pub property: String,
    pub value: String,
}

impl VCardExtension {
    pub fn property_name(&self) -> &str {
        self.property
            .split(';')
            .next()
            .unwrap_or(self.property.as_str())
    }
}

impl VCardPhoto {
    /// Decode an inline binary photo when the vCard carries one.  URI-based
    /// photos intentionally return `None`; the caller must apply its own
    /// remote-content policy before loading those resources.
    pub fn decoded_bytes(&self) -> Option<Vec<u8>> {
        if self
            .encoding
            .as_deref()
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("b"))
        {
            return decode_base64(&self.value);
        }
        let value = self.value.strip_prefix("data:")?;
        let (metadata, payload) = value.split_once(',')?;
        metadata
            .split(';')
            .any(|part| part.eq_ignore_ascii_case("base64"))
            .then(|| decode_base64(payload))?
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardContact {
    pub fn_name: String,
    pub name: VCardName,
    pub email: String,
    pub org: String,
    pub title: String,
    pub tel: String,
    pub note: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub addresses: Vec<String>,
    pub categories: Vec<String>,
    pub urls: Vec<String>,
    pub nickname: String,
    pub birthday: String,
    pub anniversary: String,
    pub role: String,
    pub gender: String,
    pub timezone: String,
    pub geo: String,
    pub impps: Vec<String>,
    pub related: Vec<String>,
    pub photo: Option<VCardPhoto>,
    /// Provider- and Evolution-specific properties which are not represented
    /// by the compact editor yet. They remain exportable and searchable.
    pub extensions: Vec<VCardExtension>,
}

#[allow(dead_code)]
impl VCardContact {
    pub fn new(name: &str, email: &str, org: &str, title: &str) -> Self {
        Self {
            fn_name: name.to_string(),
            name: VCardName::default(),
            email: email.to_string(),
            org: org.to_string(),
            title: title.to_string(),
            tel: String::new(),
            note: String::new(),
            emails: if email.trim().is_empty() {
                Vec::new()
            } else {
                vec![email.to_string()]
            },
            phones: Vec::new(),
            addresses: Vec::new(),
            categories: Vec::new(),
            urls: Vec::new(),
            nickname: String::new(),
            birthday: String::new(),
            anniversary: String::new(),
            role: String::new(),
            gender: String::new(),
            timezone: String::new(),
            geo: String::new(),
            impps: Vec::new(),
            related: Vec::new(),
            photo: None,
            extensions: Vec::new(),
        }
    }

    /// Serialize this contact into vCard 4.0 format.
    pub fn to_vcard_string(&self) -> String {
        let mut lines = vec![
            "BEGIN:VCARD".to_string(),
            "VERSION:4.0".to_string(),
            format!("FN:{}", escape_value(&self.fn_name)),
        ];
        if self.name != VCardName::default() {
            lines.push(format!(
                "N:{};{};{};{};{}",
                escape_value(&self.name.family),
                escape_value(&self.name.given),
                escape_value(&self.name.additional),
                escape_value(&self.name.prefix),
                escape_value(&self.name.suffix)
            ));
        }
        if !self.email.trim().is_empty() {
            lines.push(format!("EMAIL;TYPE=INTERNET:{}", escape_value(&self.email)));
        }
        for email in self
            .emails
            .iter()
            .filter(|email| email.as_str() != self.email.as_str())
        {
            if !email.trim().is_empty() {
                lines.push(format!("EMAIL:{}", escape_value(email)));
            }
        }
        if !self.org.trim().is_empty() {
            lines.push(format!("ORG:{}", escape_value(&self.org)));
        }
        if !self.title.trim().is_empty() {
            lines.push(format!("TITLE:{}", escape_value(&self.title)));
        }
        if !self.tel.trim().is_empty() {
            lines.push(format!("TEL;TYPE=CELL:{}", escape_value(&self.tel)));
        }
        for phone in self
            .phones
            .iter()
            .filter(|phone| phone.as_str() != self.tel.as_str())
        {
            if !phone.trim().is_empty() {
                lines.push(format!("TEL:{}", escape_value(phone)));
            }
        }
        for address in &self.addresses {
            if !address.trim().is_empty() {
                lines.push(format!("ADR:{}", escape_value(address)));
            }
        }
        let categories = self
            .categories
            .iter()
            .map(|category| category.trim())
            .filter(|category| !category.is_empty())
            .map(escape_value)
            .collect::<Vec<_>>()
            .join(",");
        if !categories.is_empty() {
            lines.push(format!("CATEGORIES:{categories}"));
        }
        for url in &self.urls {
            if !url.trim().is_empty() {
                lines.push(format!("URL:{}", escape_value(url)));
            }
        }
        if !self.nickname.trim().is_empty() {
            lines.push(format!("NICKNAME:{}", escape_value(&self.nickname)));
        }
        if !self.birthday.trim().is_empty() {
            lines.push(format!("BDAY:{}", escape_value(&self.birthday)));
        }
        if !self.anniversary.trim().is_empty() {
            lines.push(format!("ANNIVERSARY:{}", escape_value(&self.anniversary)));
        }
        if !self.role.trim().is_empty() {
            lines.push(format!("ROLE:{}", escape_value(&self.role)));
        }
        if !self.gender.trim().is_empty() {
            lines.push(format!("GENDER:{}", escape_value(&self.gender)));
        }
        if !self.timezone.trim().is_empty() {
            lines.push(format!("TZ:{}", escape_value(&self.timezone)));
        }
        if !self.geo.trim().is_empty() {
            lines.push(format!("GEO:{}", escape_value(&self.geo)));
        }
        for impp in &self.impps {
            if !impp.trim().is_empty() {
                lines.push(format!("IMPP:{}", escape_value(impp)));
            }
        }
        for related in &self.related {
            if !related.trim().is_empty() {
                lines.push(format!("RELATED:{}", escape_value(related)));
            }
        }
        if !self.note.trim().is_empty() {
            lines.push(format!("NOTE:{}", escape_value(&self.note)));
        }
        if let Some(photo) = &self.photo {
            if !photo.value.trim().is_empty() {
                let media_type = if photo.media_type.trim().is_empty() {
                    "image/jpeg"
                } else {
                    photo.media_type.trim()
                };
                let property = match photo.encoding.as_deref() {
                    Some(encoding) if !encoding.trim().is_empty() => {
                        format!("PHOTO;ENCODING={encoding};TYPE={media_type}")
                    }
                    _ => format!("PHOTO;MEDIATYPE={media_type}"),
                };
                lines.push(format!("{property}:{}", photo.value.trim()));
            }
        }
        for extension in &self.extensions {
            let property = extension.property.trim();
            if property.is_empty()
                || property
                    .split(';')
                    .next()
                    .is_none_or(|name| !name.to_ascii_uppercase().starts_with("X-"))
                || extension.value.contains('\r')
                || extension.value.contains('\n')
            {
                continue;
            }
            lines.push(format!("{property}:{}", escape_value(&extension.value)));
        }
        lines.push("END:VCARD".to_string());
        format!("{}\r\n", lines.join("\r\n"))
    }

    pub fn searchable_text(&self) -> String {
        let mut values = vec![
            self.fn_name.clone(),
            self.email.clone(),
            self.org.clone(),
            self.title.clone(),
            self.tel.clone(),
            self.note.clone(),
            self.nickname.clone(),
            self.birthday.clone(),
            self.anniversary.clone(),
            self.role.clone(),
            self.gender.clone(),
            self.timezone.clone(),
            self.geo.clone(),
        ];
        values.extend(self.emails.iter().cloned());
        values.extend(self.phones.iter().cloned());
        values.extend(self.addresses.iter().cloned());
        values.extend(self.categories.iter().cloned());
        values.extend(self.urls.iter().cloned());
        values.extend(self.impps.iter().cloned());
        values.extend(self.related.iter().cloned());
        if let Some(photo) = &self.photo {
            values.push(photo.media_type.clone());
        }
        for extension in &self.extensions {
            values.push(extension.property.clone());
            values.push(extension.value.clone());
        }
        values.join(" ").to_lowercase()
    }

    /// Return all extension values for a property, ignoring parameters and
    /// property-name casing.
    pub fn extension_values(&self, property_name: &str) -> Vec<String> {
        self.extensions
            .iter()
            .filter(|extension| {
                extension
                    .property_name()
                    .eq_ignore_ascii_case(property_name)
            })
            .map(|extension| extension.value.clone())
            .collect()
    }

    pub fn duplicate_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        let emails = if self.emails.is_empty() {
            std::slice::from_ref(&self.email)
        } else {
            self.emails.as_slice()
        };
        for email in emails {
            let normalized = email.trim().to_ascii_lowercase();
            if !normalized.is_empty() {
                keys.push(format!("email:{normalized}"));
            }
        }
        let phones = if self.phones.is_empty() {
            std::slice::from_ref(&self.tel)
        } else {
            self.phones.as_slice()
        };
        for phone in phones {
            let normalized: String = phone
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .collect();
            if !normalized.is_empty() {
                keys.push(format!("phone:{normalized}"));
            }
        }
        if keys.is_empty() {
            let normalized = self
                .fn_name
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            if !normalized.is_empty() {
                keys.push(format!("name:{normalized}"));
            }
        }
        keys
    }

    /// Merge the non-primary properties of duplicate cards into the selected
    /// primary card while keeping the primary card's identity and raw
    /// properties intact.  This is deliberately line-oriented: reserializing
    /// through the compact model would lose provider-specific parameters and
    /// unknown RFC/X-* fields that Evolution must preserve.
    pub fn merge_vcards(primary: &str, duplicates: &[&str]) -> Result<String, String> {
        let mut primary_lines = unfold_vcard_lines(primary);
        if !is_complete_vcard(&primary_lines) {
            return Err("Der Primärkontakt enthält keine gültige vCard".to_string());
        }

        let mut seen = primary_lines
            .iter()
            .filter_map(|line| merge_property_signature(line))
            .collect::<std::collections::HashSet<_>>();
        let insertion_index = primary_lines
            .iter()
            .position(|line| line.eq_ignore_ascii_case("END:VCARD"))
            .ok_or_else(|| "Primärkontakt enthält kein END:VCARD".to_string())?;
        let mut additions = Vec::new();

        for (duplicate_index, duplicate) in duplicates.iter().enumerate() {
            let lines = unfold_vcard_lines(duplicate);
            if !is_complete_vcard(&lines) {
                return Err(format!(
                    "Das Duplikat an Position {} enthält keine gültige vCard",
                    duplicate_index + 1
                ));
            }
            for line in lines {
                let property_name = line
                    .split_once(':')
                    .map(|(property, _)| {
                        property
                            .split(';')
                            .next()
                            .unwrap_or(property)
                            .to_ascii_uppercase()
                    })
                    .unwrap_or_default();
                if matches!(
                    property_name.as_str(),
                    "BEGIN" | "END" | "VERSION" | "UID" | "FN" | "N"
                ) {
                    continue;
                }
                let Some(signature) = merge_property_signature(&line) else {
                    continue;
                };
                if seen.insert(signature) {
                    additions.push(line);
                }
            }
        }

        primary_lines.splice(insertion_index..insertion_index, additions);
        Ok(format!("{}\r\n", primary_lines.join("\r\n")))
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
        let mut name = VCardName::default();
        let mut email = String::new();
        let mut org = String::new();
        let mut title = String::new();
        let mut tel = String::new();
        let mut note = String::new();
        let mut emails = Vec::new();
        let mut phones = Vec::new();
        let mut addresses = Vec::new();
        let mut categories = Vec::new();
        let mut urls = Vec::new();
        let mut nickname = String::new();
        let mut birthday = String::new();
        let mut anniversary = String::new();
        let mut role = String::new();
        let mut gender = String::new();
        let mut timezone = String::new();
        let mut geo = String::new();
        let mut impps = Vec::new();
        let mut related = Vec::new();
        let mut photo = None;
        let mut extensions = Vec::new();

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
            let raw_value = value;
            let value = unescape_value(raw_value);
            match property_name.as_str() {
                "FN" => fn_name = value,
                "N" => name = parse_name_value(raw_value),
                "EMAIL" => {
                    if email.is_empty() {
                        email = value.clone();
                    }
                    emails.push(value);
                }
                "ORG" => org = value,
                "TITLE" => title = value,
                "TEL" => {
                    if tel.is_empty() {
                        tel = value.clone();
                    }
                    phones.push(value);
                }
                "ADR" => addresses.push(value.replace(';', ", ")),
                "CATEGORIES" => categories.extend(
                    split_escaped_list(raw_value)
                        .into_iter()
                        .map(|category| unescape_value(&category))
                        .map(|category| category.trim().to_string())
                        .filter(|category| !category.is_empty()),
                ),
                "URL" => urls.push(value),
                "NICKNAME" => nickname = value,
                "BDAY" => birthday = value,
                "ANNIVERSARY" => anniversary = value,
                "ROLE" => role = value,
                "GENDER" => gender = value,
                "TZ" => timezone = value,
                "GEO" => geo = value,
                "IMPP" => impps.push(value),
                "RELATED" => related.push(value),
                "NOTE" => note = value,
                "PHOTO" => photo = parse_photo(property, raw_value),
                _ if property_name.starts_with("X-") => {
                    extensions.push(VCardExtension {
                        property: property.to_string(),
                        value,
                    });
                }
                _ => {}
            }
        }

        if fn_name.is_empty() {
            return None;
        }

        Some(Self {
            fn_name,
            name,
            email,
            org,
            title,
            tel,
            note,
            emails,
            phones,
            addresses,
            categories,
            urls,
            nickname,
            birthday,
            anniversary,
            role,
            gender,
            timezone,
            geo,
            impps,
            related,
            photo,
            extensions,
        })
    }
}

fn unfold_vcard_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.trim_start());
            }
        } else {
            lines.push(raw_line.trim().to_string());
        }
    }
    lines
}

fn is_complete_vcard(lines: &[String]) -> bool {
    lines
        .iter()
        .any(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
        && lines
            .iter()
            .any(|line| line.eq_ignore_ascii_case("END:VCARD"))
}

fn merge_property_signature(line: &str) -> Option<String> {
    let (property, value) = line.split_once(':')?;
    let property_name = property
        .split(';')
        .next()
        .unwrap_or(property)
        .to_ascii_lowercase();
    if matches!(property_name.as_str(), "begin" | "end") {
        return None;
    }
    let normalized_value = match property_name.as_str() {
        "email" => value.trim().to_ascii_lowercase(),
        "tel" => value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase(),
        _ => value.trim().to_ascii_lowercase(),
    };
    Some(format!("{property_name}:{normalized_value}"))
}

fn parse_name_value(value: &str) -> VCardName {
    let mut parts = split_structured_value(value);
    parts.resize(5, String::new());
    VCardName {
        family: parts.remove(0),
        given: parts.remove(0),
        additional: parts.remove(0),
        prefix: parts.remove(0),
        suffix: parts.remove(0),
    }
}

fn split_structured_value(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push('\\');
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ';' {
            parts.push(unescape_value(&current));
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    parts.push(unescape_value(&current));
    parts
}

fn split_escaped_list(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push('\\');
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ',' {
            parts.push(current);
            current = String::new();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    parts.push(current);
    parts
}

fn parse_photo(property: &str, value: &str) -> Option<VCardPhoto> {
    let mut media_type = String::new();
    let mut encoding = None;
    for parameter in property.split(';').skip(1) {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        match name.to_ascii_uppercase().as_str() {
            "TYPE" | "MEDIATYPE" => media_type = normalize_photo_media_type(value),
            "ENCODING" => encoding = Some(value.to_ascii_lowercase()),
            _ => {}
        }
    }
    if media_type.is_empty() && value.starts_with("data:") {
        media_type = value
            .strip_prefix("data:")
            .and_then(|value| value.split([';', ',']).next())
            .map(normalize_photo_media_type)
            .unwrap_or_default();
    }
    if media_type.is_empty() {
        media_type = "image/jpeg".to_string();
    }
    Some(VCardPhoto {
        media_type,
        value: if encoding.as_deref() == Some("b") {
            value
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect()
        } else {
            unescape_value(value)
        },
        encoding,
    })
}

fn normalize_photo_media_type(value: &str) -> String {
    let value = value.trim().trim_matches('"').to_ascii_lowercase();
    if value.contains('/') {
        value
    } else {
        match value.as_str() {
            "jpg" | "jpeg" => "image/jpeg".to_string(),
            "png" => "image/png".to_string(),
            "gif" => "image/gif".to_string(),
            "webp" => "image/webp".to_string(),
            _ if value.is_empty() => String::new(),
            _ => format!("image/{value}"),
        }
    }
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let mut symbols = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .map(|character| match character {
            'A'..='Z' => Some(character as u8 - b'A'),
            'a'..='z' => Some(character as u8 - b'a' + 26),
            '0'..='9' => Some(character as u8 - b'0' + 52),
            '+' => Some(62),
            '/' => Some(63),
            '=' => Some(64),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if symbols.len() % 4 != 0 {
        return None;
    }

    let mut decoded = Vec::with_capacity(symbols.len() / 4 * 3);
    for chunk in symbols.chunks_exact_mut(4) {
        if chunk[0] >= 64 || chunk[1] >= 64 || (chunk[2] == 64 && chunk[3] != 64) {
            return None;
        }
        decoded.push((chunk[0] << 2) | (chunk[1] >> 4));
        if chunk[2] < 64 {
            decoded.push((chunk[1] << 4) | (chunk[2] >> 2));
            if chunk[3] < 64 {
                decoded.push((chunk[2] << 6) | chunk[3]);
            }
        }
        if chunk[3] == 64 && chunk[2] == 64 {
            continue;
        }
        if chunk[3] == 64 && chunk[2] < 64 {
            continue;
        }
    }
    Some(decoded)
}

fn escape_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
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
        assert_eq!(parsed.emails, vec!["max@example.com"]);
    }

    #[test]
    fn parser_keeps_multiple_contact_methods_and_extended_fields() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\n",
            "EMAIL;TYPE=HOME:ada@example.test\r\n",
            "EMAIL;TYPE=WORK:ada@analytical.example\r\n",
            "TEL;TYPE=CELL:+44 123\r\nTEL;TYPE=WORK:+44 456\r\n",
            "ADR;TYPE=HOME:;;London;London;;;UK\r\n",
            "CATEGORIES:Friends,Important\r\nURL:https://example.test/ada\r\n",
            "NICKNAME:Ada\r\nBDAY:1815-12-10\r\nEND:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(vcard).expect("vCard should parse");
        assert_eq!(contact.emails.len(), 2);
        assert_eq!(contact.phones.len(), 2);
        assert_eq!(contact.addresses, vec![", , London, London, , , UK"]);
        assert_eq!(contact.categories, vec!["Friends", "Important"]);
        assert_eq!(contact.urls, vec!["https://example.test/ada"]);
        assert_eq!(contact.nickname, "Ada");
        assert_eq!(contact.birthday, "1815-12-10");
    }

    #[test]
    fn duplicate_keys_normalize_email_phone_and_name_fallback() {
        let contact = VCardContact::parse_vcard_string(
            "BEGIN:VCARD\r\nFN: Ada   Example \r\nEMAIL:ADA@EXAMPLE.TEST\r\nTEL:+49 (0) 123-45\r\nEND:VCARD\r\n",
        )
        .expect("vCard should parse");
        assert_eq!(
            contact.duplicate_keys(),
            vec![
                "email:ada@example.test".to_string(),
                "phone:49012345".to_string()
            ]
        );

        let name_only =
            VCardContact::parse_vcard_string("BEGIN:VCARD\r\nFN: Ada   Example \r\nEND:VCARD\r\n")
                .expect("vCard should parse");
        assert_eq!(name_only.duplicate_keys(), vec!["name:ada example"]);
    }

    #[test]
    fn merge_vcards_keeps_primary_identity_and_adds_unique_raw_fields() {
        let primary = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:primary\r\nFN:Ada Lovelace\r\n",
            "EMAIL;TYPE=WORK:ada@example.test\r\n",
            "X-EVOLUTION-FILE-AS:Lovelace\\, Ada\r\nEND:VCARD\r\n"
        );
        let duplicate = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:duplicate\r\nFN:Ada Lovelace\r\n",
            "EMAIL;TYPE=HOME:ada@example.test\r\n",
            "EMAIL;TYPE=OTHER:ada.personal@example.test\r\n",
            "TEL;TYPE=CELL:+49 (0) 123-45\r\n",
            "X-PROVIDER-CUSTOM;TYPE=work:Value\\;with\\, punctuation\r\n",
            "END:VCARD\r\n"
        );

        let merged = VCardContact::merge_vcards(primary, &[duplicate])
            .expect("duplicate vCard should merge");
        assert!(merged.contains("UID:primary"));
        assert!(!merged.contains("UID:duplicate"));
        assert!(merged.contains("EMAIL;TYPE=OTHER:ada.personal@example.test"));
        assert!(merged.contains("TEL;TYPE=CELL:+49 (0) 123-45"));
        assert!(merged.contains("X-PROVIDER-CUSTOM;TYPE=work:Value\\;with\\, punctuation"));
        assert_eq!(merged.matches("ada@example.test").count(), 1);
        assert_eq!(merged.matches("END:VCARD").count(), 1);

        let parsed = VCardContact::parse_vcard_string(&merged).expect("merged vCard parses");
        assert_eq!(parsed.fn_name, "Ada Lovelace");
        assert_eq!(parsed.emails.len(), 2);
        assert_eq!(parsed.phones, vec!["+49 (0) 123-45"]);
    }

    #[test]
    fn merge_vcards_rejects_malformed_duplicates_instead_of_dropping_them() {
        let primary = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:primary\r\nFN:Ada\r\nEND:VCARD\r\n";
        let error = VCardContact::merge_vcards(primary, &["BEGIN:VCARD\r\nFN:Broken\r\n"])
            .expect_err("malformed duplicate should be rejected");
        assert!(error.contains("Duplikat"));
    }

    #[test]
    fn parser_and_serializer_roundtrip_name_photo_and_extended_fields() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\n",
            "FN:Dr Ada Lovelace\r\nN:Lovelace;Ada;;Dr;III\r\n",
            "PHOTO;ENCODING=b;TYPE=JPEG:YWJjZA==\r\n",
            "ROLE:Researcher\r\nGENDER:F\r\nTZ:Europe/London\r\n",
            "GEO:51.5;-0.1\r\nIMPP:matrix:u@example.test\r\n",
            "RELATED:mailto:grace@example.test\r\n",
            "ANNIVERSARY:1843-07-08\r\nEND:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(vcard).expect("vCard should parse");
        assert_eq!(contact.name.family, "Lovelace");
        assert_eq!(contact.name.given, "Ada");
        assert_eq!(contact.name.prefix, "Dr");
        assert_eq!(contact.name.suffix, "III");
        assert_eq!(contact.role, "Researcher");
        assert_eq!(contact.gender, "F");
        assert_eq!(contact.timezone, "Europe/London");
        assert_eq!(contact.geo, "51.5;-0.1");
        assert_eq!(contact.impps, vec!["matrix:u@example.test"]);
        assert_eq!(contact.related, vec!["mailto:grace@example.test"]);
        assert_eq!(contact.anniversary, "1843-07-08");
        assert_eq!(
            contact.photo,
            Some(VCardPhoto {
                media_type: "image/jpeg".to_string(),
                value: "YWJjZA==".to_string(),
                encoding: Some("b".to_string()),
            })
        );
        assert_eq!(
            contact.photo.as_ref().and_then(VCardPhoto::decoded_bytes),
            Some(b"abcd".to_vec())
        );
        let roundtrip = contact.to_vcard_string();
        assert!(roundtrip.contains("N:Lovelace;Ada;;Dr;III"));
        assert!(roundtrip.contains("PHOTO;ENCODING=b;TYPE=image/jpeg:YWJjZA=="));
        assert!(roundtrip.contains("IMPP:matrix:u@example.test"));
    }

    #[test]
    fn evolution_extensions_roundtrip_and_are_searchable() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\n",
            "X-EVOLUTION-FILE-AS:Lovelace\\, Ada\r\n",
            "X-EVOLUTION-MANAGER:Grace\\, Hopper\r\n",
            "X-EVOLUTION-WANTS-HTML:TRUE\r\n",
            "X-PROVIDER-CUSTOM;TYPE=work:Value\\;with\\, punctuation\r\n",
            "END:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(vcard).expect("vCard should parse");
        assert_eq!(
            contact.extension_values("x-evolution-file-as"),
            vec!["Lovelace, Ada"]
        );
        assert_eq!(
            contact.extension_values("X-EVOLUTION-WANTS-HTML"),
            vec!["TRUE"]
        );
        assert!(contact.searchable_text().contains("grace, hopper"));
        let roundtrip = contact.to_vcard_string();
        assert!(roundtrip.contains("X-EVOLUTION-FILE-AS:Lovelace\\, Ada"));
        assert!(roundtrip.contains("X-PROVIDER-CUSTOM;TYPE=work:Value\\;with\\, punctuation"));
    }

    #[test]
    fn escaped_category_commas_survive_parse_and_serialize() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\n",
            "CATEGORIES:Research\\, Applied,Friends\r\nEND:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(vcard).expect("vCard should parse");
        assert_eq!(contact.categories, vec!["Research, Applied", "Friends"]);
        let roundtrip = contact.to_vcard_string();
        assert!(roundtrip.contains("CATEGORIES:Research\\, Applied,Friends"));
        let reparsed = VCardContact::parse_vcard_string(&roundtrip).expect("roundtrip parses");
        assert_eq!(reparsed.categories, contact.categories);
    }
}
