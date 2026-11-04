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
    /// The original property name and parameters, when the photo was parsed.
    /// Keeping this avoids rewriting a vCard 3.0 `TYPE` parameter as a vCard
    /// 4.0 `MEDIATYPE` parameter during an otherwise lossless export.
    pub property: String,
    /// The original unfolded value.  It is used only while the logical photo
    /// value is unchanged, so edits still serialize from `value`.
    pub raw_value: String,
}

/// A parsed content line whose value is not represented by the compact
/// contact model, or which carries parameters that must survive a roundtrip.
///
/// `value` is the logical, unescaped value used by the application.
/// `raw_value` is the unfolded RFC 6350/RFC 2425 value as it appeared on
/// input.  Keeping both lets an untouched contact retain structured values,
/// parameter-specific escaping, and provider fields while still allowing
/// callers to edit the logical value safely.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardProperty {
    pub property: String,
    pub value: String,
    pub raw_value: String,
}

impl VCardProperty {
    pub fn property_name(&self) -> &str {
        property_base_name(&self.property)
    }

    fn serialized_value(&self, current_value: &str) -> String {
        if self.value == current_value && !self.raw_value.is_empty() {
            self.raw_value.clone()
        } else {
            escape_value(current_value)
        }
    }
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
        property_base_name(&self.property)
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
        let value = self
            .value
            .get(..5)
            .filter(|prefix| prefix.eq_ignore_ascii_case("data:"))
            .and_then(|_| self.value.get(5..))?;
        let (metadata, payload) = value.split_once(',')?;
        metadata
            .split(';')
            .any(|part| part.eq_ignore_ascii_case("base64"))
            .then(|| decode_base64(payload))?
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VCardListMemberKind {
    ExternalAddress,
    LocalContact,
    NestedList,
    StandardMember,
    Invalid,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardListMember {
    pub kind: VCardListMemberKind,
    pub display_name: String,
    pub email: String,
    pub contact_uid: String,
    pub source_uid: String,
    pub email_num: Option<usize>,
    pub parent_id: Option<String>,
    /// The original property and logical value. Keeping these on the member
    /// lets an untouched EDS destination survive a parse/serialize cycle.
    pub property: String,
    pub value: String,
    pub raw_value: String,
}

impl VCardListMember {
    pub fn external_address(email: &str, display_name: &str) -> Self {
        Self {
            kind: VCardListMemberKind::ExternalAddress,
            display_name: display_name.trim().to_string(),
            email: normalize_mail_address(email),
            contact_uid: String::new(),
            source_uid: String::new(),
            email_num: Some(0),
            parent_id: Some("0".to_string()),
            property: String::new(),
            value: String::new(),
            raw_value: String::new(),
        }
    }

    pub fn local_contact(
        uid: &str,
        source_uid: &str,
        display_name: &str,
        email: &str,
        email_num: Option<usize>,
    ) -> Self {
        Self {
            kind: VCardListMemberKind::LocalContact,
            display_name: display_name.trim().to_string(),
            email: normalize_mail_address(email),
            contact_uid: uid.trim().to_string(),
            source_uid: source_uid.trim().to_string(),
            email_num: email_num.or(Some(0)),
            parent_id: Some("0".to_string()),
            property: String::new(),
            value: String::new(),
            raw_value: String::new(),
        }
    }

    pub fn nested_list(uid: &str, display_name: &str, parent_id: Option<&str>) -> Self {
        Self {
            kind: VCardListMemberKind::NestedList,
            display_name: display_name.trim().to_string(),
            email: String::new(),
            contact_uid: uid.trim().to_string(),
            source_uid: String::new(),
            email_num: None,
            parent_id: parent_id.map(ToOwned::to_owned).or(Some("0".to_string())),
            property: String::new(),
            value: String::new(),
            raw_value: String::new(),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.validation_error().is_none()
    }

    pub fn validation_error(&self) -> Option<String> {
        match self.kind {
            VCardListMemberKind::ExternalAddress => {
                if is_valid_email(&self.email) {
                    None
                } else {
                    Some("ungültige externe E-Mail-Adresse".to_string())
                }
            }
            VCardListMemberKind::LocalContact => {
                if self.contact_uid.trim().is_empty() {
                    Some("lokale Kontakt-UID fehlt".to_string())
                } else if !is_valid_email(&self.email) {
                    Some("lokales Mitglied hat keine gültige E-Mail-Adresse".to_string())
                } else {
                    None
                }
            }
            VCardListMemberKind::NestedList => {
                if self.contact_uid.trim().is_empty() {
                    Some("verschachtelte Liste hat keine CL-UID".to_string())
                } else if self.display_name.trim().is_empty() {
                    Some("verschachtelte Liste hat keinen Anzeigenamen".to_string())
                } else {
                    None
                }
            }
            VCardListMemberKind::StandardMember => {
                if self.email.trim().is_empty() && self.value.trim().is_empty() {
                    Some("Mitgliedswert fehlt".to_string())
                } else if !self.email.trim().is_empty() && !is_valid_email(&self.email) {
                    Some("MEMBER enthält keine gültige mailto-Adresse".to_string())
                } else {
                    None
                }
            }
            VCardListMemberKind::Invalid => Some("unbekanntes Listenmitglied".to_string()),
        }
    }

    pub fn display_text(&self, show_address: bool) -> String {
        let display_name = self.display_name.trim();
        let email = self.email.trim();
        let text = if show_address && !email.is_empty() && !display_name.is_empty() {
            format!("{display_name} <{email}>")
        } else if !display_name.is_empty() {
            display_name.to_string()
        } else if !show_address && !email.is_empty() {
            match self.kind {
                VCardListMemberKind::ExternalAddress => "Externe Adresse".to_string(),
                VCardListMemberKind::LocalContact => "Lokaler Kontakt".to_string(),
                VCardListMemberKind::StandardMember => "Listenmitglied".to_string(),
                _ => email.to_string(),
            }
        } else if !email.is_empty() {
            email.to_string()
        } else if !self.value.trim().is_empty() {
            self.value.trim().to_string()
        } else {
            "Unbekanntes Mitglied".to_string()
        };
        if self.is_valid() {
            text
        } else {
            format!("⚠ {text}")
        }
    }

    pub fn to_vcard_line(&self) -> Option<String> {
        self.to_vcard_line_for_version(false)
    }

    fn to_vcard_line_for_version(&self, vcard4_group: bool) -> Option<String> {
        if !self.property.trim().is_empty()
            && !self.raw_value.is_empty()
            && is_safe_property(self.property.trim())
            && unescape_value(&self.raw_value) == self.value
        {
            return Some(format!("{}:{}", self.property.trim(), self.raw_value));
        }

        let parent_id = self
            .parent_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("0");
        if vcard4_group {
            return match self.kind {
                VCardListMemberKind::ExternalAddress => self
                    .is_valid()
                    .then(|| format!("MEMBER:mailto:{}", self.email.trim())),
                VCardListMemberKind::LocalContact | VCardListMemberKind::NestedList => {
                    if !self.is_valid() {
                        return None;
                    }
                    let uid = self.contact_uid.trim();
                    (!uid.is_empty()).then(|| {
                        let value = if uid
                            .get(..9)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:uuid:"))
                        {
                            uid.to_string()
                        } else {
                            format!("urn:uuid:{uid}")
                        };
                        format!("MEMBER:{value}")
                    })
                }
                VCardListMemberKind::StandardMember | VCardListMemberKind::Invalid => {
                    let value = if self.value.trim().is_empty() {
                        self.email.trim().to_string()
                    } else {
                        self.value.trim().to_string()
                    };
                    (!value.is_empty()).then(|| format!("MEMBER:{value}"))
                }
            };
        }
        match self.kind {
            VCardListMemberKind::NestedList => {
                let uid = self.contact_uid.trim();
                if uid.is_empty() || self.display_name.trim().is_empty() {
                    return None;
                }
                Some(format!(
                    "X-EVOLUTION-CONTACT-LIST;X-EVOLUTION-CL-UID={};X-EVOLUTION-PARENT-CL={parent_id}:{}",
                    format_parameter_value(uid),
                    escape_value(self.display_name.trim())
                ))
            }
            VCardListMemberKind::ExternalAddress | VCardListMemberKind::LocalContact => {
                if !self.is_valid() {
                    return None;
                }
                let mut property = String::from("EMAIL;X-EVOLUTION-DEST-HTML-MAIL=FALSE");
                if !self.contact_uid.trim().is_empty() {
                    property.push_str(";X-EVOLUTION-DEST-EMAIL-NUM=");
                    property.push_str(&self.email_num.unwrap_or(0).to_string());
                    property.push_str(";X-EVOLUTION-DEST-CONTACT-UID=\"");
                    property.push_str(&escape_parameter(self.contact_uid.trim()));
                    property.push('"');
                }
                if !self.source_uid.trim().is_empty() {
                    property.push_str(";X-EVOLUTION-DEST-SOURCE-UID=\"");
                    property.push_str(&escape_parameter(self.source_uid.trim()));
                    property.push('"');
                }
                property.push_str(";X-EVOLUTION-PARENT-CL=");
                property.push_str(parent_id);
                let value = if self.display_name.trim().is_empty() {
                    self.email.trim().to_string()
                } else {
                    format!("{} <{}>", self.display_name.trim(), self.email.trim())
                };
                Some(format!("{property}:{}", escape_value(&value)))
            }
            VCardListMemberKind::StandardMember => {
                let value = if self.value.trim().is_empty() {
                    format!("mailto:{}", self.email.trim())
                } else {
                    self.value.trim().to_string()
                };
                (!value.is_empty()).then(|| format!("MEMBER:{}", escape_value(&value)))
            }
            VCardListMemberKind::Invalid => {
                if self.property.trim().is_empty() || self.value.trim().is_empty() {
                    None
                } else {
                    Some(format!(
                        "{}:{}",
                        self.property.trim(),
                        escape_value(self.value.trim())
                    ))
                }
            }
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCardContact {
    /// The input version for parsed cards. New contacts use vCard 4.0.
    pub version: String,
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
    /// All PHOTO properties in input order. `photo` remains the first-photo
    /// convenience field used by the current UI.
    pub photos: Vec<VCardPhoto>,
    /// Provider- and Evolution-specific properties which are not represented
    /// by the compact editor yet. They remain exportable and searchable.
    pub extensions: Vec<VCardExtension>,
    /// Parameters and original values for repeated contact methods. These
    /// are public so an importer/editor can intentionally retain or replace
    /// them without coupling to the parser implementation.
    pub email_properties: Vec<VCardProperty>,
    pub phone_properties: Vec<VCardProperty>,
    pub address_properties: Vec<VCardProperty>,
    pub url_properties: Vec<VCardProperty>,
    pub impp_properties: Vec<VCardProperty>,
    pub related_properties: Vec<VCardProperty>,
    /// Raw X-* properties in input order. `extensions` remains the compact
    /// logical API used by providers, while this parallel representation
    /// preserves provider escaping when a contact is edited and serialized.
    pub extension_properties: Vec<VCardProperty>,
    /// Evolution contact lists use X-EVOLUTION-LIST and EMAIL destinations;
    /// vCard 4 lists may use KIND:group and MEMBER instead.
    pub is_list: bool,
    pub list_name: String,
    pub list_show_addresses: bool,
    pub list_members: Vec<VCardListMember>,
    /// Unknown standard properties such as UID, PRODID, REV, KEY, or LOGO.
    /// They are retained separately from `extensions` so Evolution/X fields
    /// remain available through the existing API.
    pub unknown_properties: Vec<VCardProperty>,
}

#[allow(dead_code)]
impl VCardContact {
    pub fn new(name: &str, email: &str, org: &str, title: &str) -> Self {
        Self {
            version: "4.0".to_string(),
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
            photos: Vec::new(),
            extensions: Vec::new(),
            email_properties: Vec::new(),
            phone_properties: Vec::new(),
            address_properties: Vec::new(),
            url_properties: Vec::new(),
            impp_properties: Vec::new(),
            related_properties: Vec::new(),
            extension_properties: Vec::new(),
            is_list: false,
            list_name: String::new(),
            list_show_addresses: false,
            list_members: Vec::new(),
            unknown_properties: Vec::new(),
        }
    }

    pub fn new_list(name: &str, show_addresses: bool, uid: Option<&str>) -> Self {
        let name = name.trim();
        let mut contact = Self::new(name, "", "", "");
        contact.version = "3.0".to_string();
        contact.is_list = true;
        contact.list_name = name.to_string();
        contact.list_show_addresses = show_addresses;
        contact.set_extension_value("X-EVOLUTION-FILE-AS", name);
        if let Some(uid) = uid.filter(|uid| !uid.trim().is_empty()) {
            contact.set_uid(uid);
        }
        contact
    }

    pub fn set_uid(&mut self, uid: &str) {
        let uid = uid.trim();
        if uid.is_empty() {
            return;
        }
        if let Some(property) = self
            .unknown_properties
            .iter_mut()
            .find(|property| property.property_name().eq_ignore_ascii_case("UID"))
        {
            property.value = uid.to_string();
            property.raw_value = escape_value(uid);
        } else {
            self.unknown_properties.push(VCardProperty {
                property: "UID".to_string(),
                value: uid.to_string(),
                raw_value: escape_value(uid),
            });
        }
    }

    pub fn uid(&self) -> Option<String> {
        self.unknown_properties
            .iter()
            .find(|property| property.property_name().eq_ignore_ascii_case("UID"))
            .map(|property| property.value.clone())
    }

    pub fn set_extension_value(&mut self, property_name: &str, value: &str) {
        let extension_index = self.extensions.iter().position(|extension| {
            extension
                .property_name()
                .eq_ignore_ascii_case(property_name)
        });
        if let Some(extension_index) = extension_index {
            let property = self.extensions[extension_index].property.clone();
            self.extensions[extension_index].value = value.to_string();
            if let Some(raw) = self
                .extension_properties
                .iter_mut()
                .find(|property| property.property_name().eq_ignore_ascii_case(property_name))
            {
                raw.value = value.to_string();
                raw.raw_value = escape_value(value);
            } else {
                self.extension_properties.push(VCardProperty {
                    property,
                    value: value.to_string(),
                    raw_value: escape_value(value),
                });
            }
        } else {
            self.extensions.push(VCardExtension {
                property: property_name.to_string(),
                value: value.to_string(),
            });
            self.extension_properties.push(VCardProperty {
                property: property_name.to_string(),
                value: value.to_string(),
                raw_value: escape_value(value),
            });
        }
    }

    pub fn invalid_list_members(&self) -> Vec<(usize, String)> {
        self.list_members
            .iter()
            .enumerate()
            .filter_map(|(index, member)| member.validation_error().map(|error| (index, error)))
            .collect()
    }

    pub fn list_member_emails(&self) -> Vec<String> {
        self.list_members
            .iter()
            .filter(|member| member.is_valid())
            .map(|member| member.email.trim().to_string())
            .filter(|email| !email.is_empty())
            .fold(Vec::new(), |mut emails, email| {
                if !emails
                    .iter()
                    .any(|existing: &String| existing.eq_ignore_ascii_case(&email))
                {
                    emails.push(email);
                }
                emails
            })
    }

    fn uses_vcard4_group_format(&self) -> bool {
        self.is_list
            && self.version.trim().eq_ignore_ascii_case("4.0")
            && (self.has_vcard4_group_kind()
                || self
                    .list_members
                    .iter()
                    .any(|member| matches!(member.kind, VCardListMemberKind::StandardMember)))
    }

    fn has_vcard4_group_kind(&self) -> bool {
        self.unknown_properties.iter().any(|property| {
            property.property_name().eq_ignore_ascii_case("KIND")
                && property.value.trim().eq_ignore_ascii_case("group")
        })
    }

    /// Repair a vCard 4 list which already carries MEMBER values but omitted
    /// the mandatory group kind.  This is used only on an explicit list save;
    /// parsing itself keeps the missing property observable for validation.
    pub fn ensure_vcard4_group_kind(&mut self) {
        if self.version.trim().eq_ignore_ascii_case("4.0")
            && self.is_list
            && !self.has_vcard4_group_kind()
        {
            self.unknown_properties.push(VCardProperty {
                property: "KIND".to_string(),
                value: "group".to_string(),
                raw_value: "group".to_string(),
            });
        }
    }

    /// Return a backend-facing validation error for a contact list.
    ///
    /// vCard 4 requires `KIND:group` whenever `MEMBER` is present.  The
    /// Evolution vCard 3 representation is deliberately kept separate: its
    /// destinations are EMAIL properties with X-EVOLUTION parameters, while
    /// a vCard 3 MEMBER line is not a valid replacement for those metadata.
    pub fn list_validation_error(&self) -> Option<String> {
        if !self.is_list {
            return None;
        }
        if self.version.trim().eq_ignore_ascii_case("4.0") && !self.has_vcard4_group_kind() {
            return Some("Eine vCard-4-Kontaktliste benötigt KIND:group".to_string());
        }
        if self.version.trim().eq_ignore_ascii_case("4.0") {
            return self
                .invalid_list_members()
                .into_iter()
                .next()
                .map(|(index, error)| format!("Listenmitglied {} ist {error}", index + 1));
        }
        if self
            .list_members
            .iter()
            .any(|member| matches!(member.kind, VCardListMemberKind::StandardMember))
        {
            return Some("vCard-3-Kontaktlisten benötigen EMAIL-Ziele".to_string());
        }
        self.invalid_list_members()
            .into_iter()
            .next()
            .map(|(index, error)| format!("Listenmitglied {} ist {error}", index + 1))
    }

    /// Serialize this contact into vCard format.
    ///
    /// Parsed cards retain their input version and repeated-property
    /// parameters. Newly created contacts default to vCard 4.0.
    pub fn to_vcard_string(&self) -> String {
        let mut lines = vec![
            "BEGIN:VCARD".to_string(),
            format!(
                "VERSION:{}",
                if self.version.trim().is_empty() {
                    "4.0"
                } else {
                    self.version.trim()
                }
            ),
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
        if !self.is_list || self.uses_vcard4_group_format() {
            let emails = if self.emails.is_empty() {
                std::slice::from_ref(&self.email)
            } else {
                self.emails.as_slice()
            };
            append_repeated_properties(
                &mut lines,
                "EMAIL;TYPE=INTERNET",
                "EMAIL",
                emails,
                &self.email_properties,
            );
        }
        if !self.org.trim().is_empty() {
            lines.push(format!("ORG:{}", escape_value(&self.org)));
        }
        if !self.title.trim().is_empty() {
            lines.push(format!("TITLE:{}", escape_value(&self.title)));
        }
        let phones = if self.phones.is_empty() {
            std::slice::from_ref(&self.tel)
        } else {
            self.phones.as_slice()
        };
        append_repeated_properties(
            &mut lines,
            "TEL;TYPE=CELL",
            "TEL",
            phones,
            &self.phone_properties,
        );
        append_address_properties(&mut lines, &self.addresses, &self.address_properties);
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
        append_repeated_properties(&mut lines, "URL", "URL", &self.urls, &self.url_properties);
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
        append_repeated_properties(
            &mut lines,
            "IMPP",
            "IMPP",
            &self.impps,
            &self.impp_properties,
        );
        append_repeated_properties(
            &mut lines,
            "RELATED",
            "RELATED",
            &self.related,
            &self.related_properties,
        );
        if !self.note.trim().is_empty() {
            lines.push(format!("NOTE:{}", escape_value(&self.note)));
        }
        if self.is_list {
            let vcard4_group = self.uses_vcard4_group_format();
            if !vcard4_group {
                lines.push("X-EVOLUTION-LIST:TRUE".to_string());
                lines.push(format!(
                    "X-EVOLUTION-LIST-SHOW-ADDRESSES:{}",
                    if self.list_show_addresses {
                        "TRUE"
                    } else {
                        "FALSE"
                    }
                ));
                if !self.list_name.trim().is_empty() {
                    lines.push(format!(
                        "X-EVOLUTION-LIST-NAME:{}",
                        escape_value(self.list_name.trim())
                    ));
                }
            }
            if vcard4_group {
                if let Some(kind) = self
                    .unknown_properties
                    .iter()
                    .find(|property| property.property_name().eq_ignore_ascii_case("KIND"))
                {
                    lines.push(format!(
                        "{}:{}",
                        kind.property.trim(),
                        kind.serialized_value(&kind.value)
                    ));
                } else {
                    lines.push("KIND:group".to_string());
                }
            }
            for member in &self.list_members {
                if let Some(line) = member.to_vcard_line_for_version(vcard4_group) {
                    lines.push(line);
                }
            }
        }
        let photos = if self.photos.is_empty() {
            self.photo.iter().collect::<Vec<_>>()
        } else {
            self.photos.iter().collect::<Vec<_>>()
        };
        for photo in photos {
            if !photo.value.trim().is_empty() {
                let media_type = if photo.media_type.trim().is_empty() {
                    "image/jpeg"
                } else {
                    photo.media_type.trim()
                };
                let property = if is_safe_property(photo.property.trim()) {
                    photo.property.trim().to_string()
                } else {
                    match photo.encoding.as_deref() {
                        Some(encoding) if !encoding.trim().is_empty() => {
                            format!("PHOTO;ENCODING={encoding};TYPE={media_type}")
                        }
                        _ => format!("PHOTO;MEDIATYPE={media_type}"),
                    }
                };
                let value = if !photo.raw_value.is_empty()
                    && ((photo
                        .encoding
                        .as_deref()
                        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("b"))
                        && compact_base64(&photo.raw_value) == photo.value)
                        || unescape_value(&photo.raw_value) == photo.value)
                {
                    photo.raw_value.clone()
                } else {
                    escape_value(photo.value.trim())
                };
                lines.push(format!("{property}:{value}"));
            }
        }
        if self.extension_properties.is_empty() {
            for extension in &self.extensions {
                let property = extension.property.trim();
                if !is_extension_property(property)
                    || extension.value.contains('\r')
                    || extension.value.contains('\n')
                    || (self.is_list
                        && !self.uses_vcard4_group_format()
                        && is_list_representation_property(property))
                {
                    continue;
                }
                lines.push(format!("{property}:{}", escape_value(&extension.value)));
            }
        } else {
            for extension in &self.extension_properties {
                let property = extension.property.trim();
                if !is_extension_property(property)
                    || extension.value.contains('\r')
                    || extension.value.contains('\n')
                    || (self.is_list
                        && !self.uses_vcard4_group_format()
                        && is_list_representation_property(property))
                {
                    continue;
                }
                let value = if !extension.raw_value.is_empty()
                    && unescape_value(&extension.raw_value) == extension.value
                {
                    extension.raw_value.clone()
                } else {
                    escape_value(&extension.value)
                };
                lines.push(format!("{property}:{value}"));
            }
        }
        for unknown in &self.unknown_properties {
            if !is_safe_property(unknown.property.trim())
                || is_core_property(unknown.property_name())
                || (self.uses_vcard4_group_format()
                    && unknown.property_name().eq_ignore_ascii_case("KIND"))
                || unknown.value.contains('\r')
                || unknown.value.contains('\n')
            {
                continue;
            }
            let property = unknown.property.trim();
            lines.push(format!(
                "{property}:{}",
                unknown.serialized_value(&unknown.value)
            ));
        }
        lines.push("END:VCARD".to_string());
        let folded = lines
            .into_iter()
            .flat_map(fold_content_line)
            .collect::<Vec<_>>();
        format!("{}\r\n", folded.join("\r\n"))
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
        if self.is_list {
            values.push(self.list_name.clone());
            values.extend(self.list_members.iter().flat_map(|member| {
                [
                    member.display_name.clone(),
                    member.email.clone(),
                    member.value.clone(),
                ]
            }));
        }
        values.extend(self.emails.iter().cloned());
        values.extend(self.phones.iter().cloned());
        values.extend(self.addresses.iter().cloned());
        values.extend(self.categories.iter().cloned());
        values.extend(self.urls.iter().cloned());
        values.extend(self.impps.iter().cloned());
        values.extend(self.related.iter().cloned());
        for photo in &self.photos {
            values.push(photo.media_type.clone());
            values.push(photo.value.clone());
        }
        if self.photos.is_empty() {
            if let Some(photo) = &self.photo {
                values.push(photo.media_type.clone());
                values.push(photo.value.clone());
            }
        }
        for extension in &self.extensions {
            values.push(extension.property.clone());
            values.push(extension.value.clone());
        }
        for property in &self.unknown_properties {
            values.push(property.property.clone());
            values.push(property.value.clone());
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
        let list_emails = self.list_member_emails();
        let emails: Vec<&str> = if self.is_list {
            list_emails.iter().map(String::as_str).collect()
        } else if self.emails.is_empty() {
            vec![self.email.as_str()]
        } else {
            self.emails.iter().map(String::as_str).collect()
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
                let property_name = split_property_value(&line)
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
        let lines = unfold_vcard_lines(vcard);
        if !is_complete_vcard(&lines) {
            return None;
        }

        let mut version = "4.0".to_string();
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
        let mut photos = Vec::new();
        let mut extensions = Vec::new();
        let mut email_properties = Vec::new();
        let mut phone_properties = Vec::new();
        let mut address_properties = Vec::new();
        let mut url_properties = Vec::new();
        let mut impp_properties = Vec::new();
        let mut related_properties = Vec::new();
        let mut extension_properties = Vec::new();
        let mut is_list = false;
        let mut list_name = String::new();
        let mut list_show_addresses = false;
        let mut list_members = Vec::new();
        let mut unknown_properties = Vec::new();

        for line in lines {
            let Some((raw_property, raw_value)) = split_property_value(&line) else {
                continue;
            };
            let property = raw_property.trim();
            let property_name = property_base_name(property).to_ascii_uppercase();
            let value = unescape_value(raw_value);
            match property_name.as_str() {
                "VERSION" => {
                    if !value.trim().is_empty() {
                        version = value.trim().to_string();
                    }
                }
                "FN" => fn_name = value,
                "N" => name = parse_name_value(raw_value),
                "EMAIL" => {
                    if email.is_empty() {
                        email = value.clone();
                    }
                    emails.push(value.clone());
                    email_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                    if !version.trim().eq_ignore_ascii_case("4.0") {
                        list_members.push(parse_email_list_member(property, raw_value));
                    }
                }
                "ORG" => org = format_org_value(raw_value),
                "TITLE" => title = value,
                "TEL" => {
                    if tel.is_empty() {
                        tel = value.clone();
                    }
                    phones.push(value.clone());
                    phone_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                }
                "ADR" => {
                    let address = format_address_value(raw_value);
                    addresses.push(address.clone());
                    address_properties.push(VCardProperty {
                        property: property.to_string(),
                        value: address,
                        raw_value: raw_value.to_string(),
                    });
                }
                "CATEGORIES" => categories.extend(
                    split_escaped_list(raw_value)
                        .into_iter()
                        .map(|category| unescape_value(&category))
                        .map(|category| category.trim().to_string())
                        .filter(|category| !category.is_empty()),
                ),
                "URL" => {
                    urls.push(value.clone());
                    url_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                }
                "NICKNAME" => nickname = value,
                "BDAY" => birthday = value,
                "ANNIVERSARY" => anniversary = value,
                "ROLE" => role = value,
                "GENDER" => gender = value,
                "TZ" => timezone = value,
                "GEO" => geo = value,
                "IMPP" => {
                    impps.push(value.clone());
                    impp_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                }
                "RELATED" => {
                    related.push(value.clone());
                    related_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                }
                "NOTE" => note = value,
                "PHOTO" => {
                    if let Some(parsed_photo) = parse_photo(property, raw_value) {
                        if photo.is_none() {
                            photo = Some(parsed_photo.clone());
                        }
                        photos.push(parsed_photo);
                    }
                }
                "MEMBER" => {
                    is_list = true;
                    list_members.push(parse_standard_list_member(property, raw_value, &value));
                }
                "KIND" if value.trim().eq_ignore_ascii_case("group") => {
                    is_list = true;
                    unknown_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                }
                _ if property_name.starts_with("X-") => {
                    if property_name.eq_ignore_ascii_case("X-EVOLUTION-LIST") {
                        is_list = parse_vcard_bool(&value);
                    } else if property_name.eq_ignore_ascii_case("X-EVOLUTION-LIST-NAME") {
                        list_name = value.clone();
                    } else if is_list_show_addresses_property(&property_name) {
                        list_show_addresses = parse_vcard_bool(&value);
                    } else if property_name.eq_ignore_ascii_case("X-EVOLUTION-CONTACT-LIST") {
                        is_list = true;
                        list_members.push(parse_nested_list_member(property, raw_value, &value));
                    }
                    if property_name.eq_ignore_ascii_case("X-EVOLUTION-FILE-AS")
                        && list_name.trim().is_empty()
                    {
                        list_name = value.clone();
                    }
                    if !property_name.eq_ignore_ascii_case("X-EVOLUTION-CONTACT-LIST") {
                        extensions.push(VCardExtension {
                            property: property.to_string(),
                            value: value.clone(),
                        });
                        extension_properties.push(VCardProperty {
                            property: property.to_string(),
                            value,
                            raw_value: raw_value.to_string(),
                        });
                    }
                }
                _ if !matches!(property_name.as_str(), "BEGIN" | "END") => {
                    unknown_properties.push(VCardProperty {
                        property: property.to_string(),
                        value,
                        raw_value: raw_value.to_string(),
                    });
                }
                _ => {}
            }
        }

        if fn_name.is_empty() {
            return None;
        }

        Some(Self {
            version,
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
            photos,
            extensions,
            email_properties,
            phone_properties,
            address_properties,
            url_properties,
            impp_properties,
            related_properties,
            extension_properties,
            is_list,
            list_name,
            list_show_addresses,
            list_members: if is_list { list_members } else { Vec::new() },
            unknown_properties,
        })
    }
}

fn property_base_name(property: &str) -> &str {
    let name = property.split(';').next().unwrap_or(property);
    name.rsplit('.').next().unwrap_or(name)
}

fn split_property_value(line: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ':' {
            return Some((&line[..index], &line[index + character.len_utf8()..]));
        }
    }
    None
}

fn parse_vcard_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "1" | "on"
    )
}

fn is_list_show_addresses_property(property: &str) -> bool {
    property.eq_ignore_ascii_case("X-EVOLUTION-LIST-SHOW-ADDRESSES")
        || property.eq_ignore_ascii_case("X-EVOLUTION-LIST-SHOW_ADDRESSES")
}

fn is_list_representation_property(property: &str) -> bool {
    let name = property_base_name(property);
    name.eq_ignore_ascii_case("X-EVOLUTION-LIST")
        || name.eq_ignore_ascii_case("X-EVOLUTION-LIST-NAME")
        || is_list_show_addresses_property(name)
        || name.eq_ignore_ascii_case("X-EVOLUTION-CONTACT-LIST")
}

fn split_property_parts(property: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, character) in property.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&property[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&property[start..]);
    parts
}

fn property_parameters(property: &str) -> Vec<(String, String)> {
    split_property_parts(property)
        .into_iter()
        .skip(1)
        .filter_map(|part| {
            let (name, value) = part.split_once('=')?;
            Some((
                name.trim().to_ascii_uppercase(),
                unescape_parameter(value.trim().trim_matches('"')),
            ))
        })
        .collect()
}

fn parameter_value(property: &str, name: &str) -> Option<String> {
    property_parameters(property)
        .into_iter()
        .find(|(parameter, _)| parameter.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

fn parse_email_list_member(property: &str, raw_value: &str) -> VCardListMember {
    let value = unescape_value(raw_value);
    let (display_name, parsed_email) = parse_recipient_value(&value);
    let contact_uid = parameter_value(property, "X-EVOLUTION-DEST-CONTACT-UID").unwrap_or_default();
    let source_uid = parameter_value(property, "X-EVOLUTION-DEST-SOURCE-UID").unwrap_or_default();
    let display_name = parameter_value(property, "X-EVOLUTION-DEST-NAME")
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(display_name);
    let email = parameter_value(property, "X-EVOLUTION-DEST-EMAIL")
        .map(|value| normalize_mail_address(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or(parsed_email);
    let email_num = parameter_value(property, "X-EVOLUTION-DEST-EMAIL-NUM")
        .and_then(|value| value.parse::<usize>().ok());
    let kind = if !contact_uid.trim().is_empty() {
        VCardListMemberKind::LocalContact
    } else if is_valid_email(&email) {
        VCardListMemberKind::ExternalAddress
    } else {
        VCardListMemberKind::Invalid
    };
    VCardListMember {
        kind,
        display_name,
        email,
        contact_uid,
        source_uid,
        email_num,
        parent_id: parameter_value(property, "X-EVOLUTION-PARENT-CL"),
        property: property.to_string(),
        value,
        raw_value: raw_value.to_string(),
    }
}

fn parse_nested_list_member(property: &str, raw_value: &str, value: &str) -> VCardListMember {
    VCardListMember {
        kind: VCardListMemberKind::NestedList,
        display_name: value.trim().to_string(),
        email: String::new(),
        contact_uid: parameter_value(property, "X-EVOLUTION-CL-UID").unwrap_or_default(),
        source_uid: String::new(),
        email_num: None,
        parent_id: parameter_value(property, "X-EVOLUTION-PARENT-CL"),
        property: property.to_string(),
        value: value.to_string(),
        raw_value: raw_value.to_string(),
    }
}

fn parse_standard_list_member(property: &str, raw_value: &str, value: &str) -> VCardListMember {
    let (display_name, parsed_email) = parse_recipient_value(value);
    let is_uri_member = !value.to_ascii_lowercase().starts_with("mailto:")
        && value.contains(':')
        && !value.chars().any(char::is_whitespace);
    let email = if is_uri_member {
        String::new()
    } else {
        parsed_email
    };
    let contact_uid = value
        .get(..9)
        .filter(|prefix| prefix.eq_ignore_ascii_case("urn:uuid:"))
        .map(|_| value[9..].to_string())
        .unwrap_or_default();
    VCardListMember {
        kind: if is_valid_email(&email) || is_uri_member {
            VCardListMemberKind::StandardMember
        } else {
            VCardListMemberKind::Invalid
        },
        display_name,
        email,
        contact_uid,
        source_uid: String::new(),
        email_num: None,
        parent_id: None,
        property: property.to_string(),
        value: value.to_string(),
        raw_value: raw_value.to_string(),
    }
}

fn parse_recipient_value(value: &str) -> (String, String) {
    let value = value.trim();
    if let Some(start) = value.rfind('<') {
        if value.ends_with('>') && start + 1 < value.len() {
            let display_name = value[..start].trim().trim_matches('"').trim().to_string();
            let email = normalize_mail_address(&value[start + 1..value.len() - 1]);
            return (display_name, email);
        }
    }
    let email = normalize_mail_address(value);
    (String::new(), email)
}

fn normalize_mail_address(value: &str) -> String {
    let value = value.trim().trim_matches('<').trim_matches('>');
    let value = if value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
    {
        &value[7..]
    } else {
        value
    };
    value.trim().to_string()
}

fn is_valid_email(value: &str) -> bool {
    let value = normalize_mail_address(value);
    if value.matches('@').count() != 1 || value.chars().any(char::is_control) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.trim().is_empty()
        && !domain.trim().is_empty()
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !value.chars().any(|character| {
            character.is_whitespace() || matches!(character, '<' | '>' | ',' | ';')
        })
}

fn escape_parameter(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(';', "\\;")
        .replace(':', "\\:")
        .replace(',', "\\,")
}

fn unescape_parameter(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let mut unescaped = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next() {
            Some(next @ ('\\' | '"' | ';' | ':' | ',')) => unescaped.push(next),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

fn format_parameter_value(value: &str) -> String {
    if value.chars().any(|character| {
        matches!(character, ',' | ';' | ':' | '"' | '\\') || character.is_whitespace()
    }) {
        format!("\"{}\"", escape_parameter(value))
    } else {
        value.to_string()
    }
}

fn is_core_property(property: &str) -> bool {
    matches!(
        property.to_ascii_uppercase().as_str(),
        "BEGIN"
            | "END"
            | "VERSION"
            | "FN"
            | "N"
            | "EMAIL"
            | "ORG"
            | "TITLE"
            | "TEL"
            | "ADR"
            | "CATEGORIES"
            | "URL"
            | "NICKNAME"
            | "BDAY"
            | "ANNIVERSARY"
            | "ROLE"
            | "GENDER"
            | "TZ"
            | "GEO"
            | "IMPP"
            | "RELATED"
            | "NOTE"
            | "PHOTO"
    )
}

fn is_safe_property(property: &str) -> bool {
    if property.is_empty()
        || property.contains(['\r', '\n'])
        || property_base_name(property).trim().is_empty()
    {
        return false;
    }
    let mut quoted = false;
    let mut escaped = false;
    for character in property.chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ':' && !quoted {
            return false;
        }
    }
    !quoted
}

fn is_extension_property(property: &str) -> bool {
    is_safe_property(property)
        && property_base_name(property)
            .to_ascii_uppercase()
            .starts_with("X-")
}

fn append_repeated_properties(
    lines: &mut Vec<String>,
    first_property: &str,
    repeated_property: &str,
    values: &[String],
    retained_properties: &[VCardProperty],
) {
    let mut retained_index = 0;
    for (value_index, value) in values.iter().enumerate() {
        if value.trim().is_empty() {
            continue;
        }

        let retained = retained_properties.get(retained_index).filter(|property| {
            is_safe_property(property.property.trim())
                && property_base_name(&property.property).eq_ignore_ascii_case(repeated_property)
        });
        let (property, serialized_value) = if let Some(property) = retained {
            retained_index += 1;
            (
                property.property.trim().to_string(),
                property.serialized_value(value),
            )
        } else {
            (
                if value_index == 0 {
                    first_property
                } else {
                    repeated_property
                }
                .to_string(),
                escape_value(value),
            )
        };
        lines.push(format!("{property}:{serialized_value}"));
    }
}

fn append_address_properties(
    lines: &mut Vec<String>,
    addresses: &[String],
    retained_properties: &[VCardProperty],
) {
    let mut retained_index = 0;
    for address in addresses {
        if address.trim().is_empty() {
            continue;
        }
        let retained = retained_properties.get(retained_index).filter(|property| {
            is_safe_property(property.property.trim())
                && property_base_name(&property.property).eq_ignore_ascii_case("ADR")
        });
        let (property, value) = if let Some(property) = retained {
            retained_index += 1;
            (
                property.property.trim().to_string(),
                if property.value == *address {
                    property.raw_value.clone()
                } else {
                    format_address_for_vcard(address)
                },
            )
        } else {
            ("ADR".to_string(), format_address_for_vcard(address))
        };
        lines.push(format!("{property}:{value}"));
    }
}

fn format_org_value(raw_value: &str) -> String {
    split_structured_value(raw_value)
        .into_iter()
        .map(|component| component.trim().to_string())
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_address_value(raw_value: &str) -> String {
    let mut components = split_structured_value(raw_value);
    components.resize(7, String::new());
    components
        .into_iter()
        .map(|component| component.trim().to_string())
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_address_for_vcard(address: &str) -> String {
    // The compact model exposes a display string. Put a newly edited address
    // into ADR's street component while retaining a valid seven-part value.
    format!(";;{};;;;", escape_value(address))
}

fn fold_content_line(line: String) -> Vec<String> {
    const MAX_OCTETS: usize = 75;
    if line.len() <= MAX_OCTETS {
        return vec![line];
    }

    let mut folded = Vec::new();
    let mut current = String::new();
    for character in line.chars() {
        if current.len() + character.len_utf8() > MAX_OCTETS && !current.is_empty() {
            folded.push(current);
            current = " ".to_string();
        }
        current.push(character);
    }
    if !current.is_empty() {
        folded.push(current);
    }
    folded
}

fn compact_base64(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect()
}

fn unfold_vcard_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::with_capacity(16);
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut pos = 0;

    while pos < len {
        let start = pos;
        while pos < len && bytes[pos] != b'\n' && bytes[pos] != b'\r' {
            pos += 1;
        }
        let raw_line = &raw[start..pos];
        if pos < len {
            if bytes[pos] == b'\r' && pos + 1 < len && bytes[pos + 1] == b'\n' {
                pos += 2;
            } else {
                pos += 1;
            }
        }

        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                // RFC folding removes exactly the first WSP octet. Further
                // spaces are data and must survive the roundtrip.
                previous.push_str(&raw_line[1..]);
            }
        } else if !raw_line.is_empty() || !lines.is_empty() {
            lines.push(raw_line.to_string());
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
    let (property, value) = split_property_value(line)?;
    let property_name = property_base_name(property).to_ascii_lowercase();
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
        let value = value.trim().trim_matches('"');
        match name.to_ascii_uppercase().as_str() {
            "TYPE" | "MEDIATYPE" => media_type = normalize_photo_media_type(value),
            "ENCODING" => encoding = Some(value.to_ascii_lowercase()),
            _ => {}
        }
    }
    if media_type.is_empty()
        && value
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        media_type = value
            .get(5..)
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
            compact_base64(value)
        } else {
            unescape_value(value)
        },
        encoding,
        property: property.to_string(),
        raw_value: value.to_string(),
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

pub(crate) fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let mut compact = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    if compact.len() % 4 == 1 {
        return None;
    }
    if let Some(first_padding) = compact.find('=') {
        if compact[first_padding..]
            .chars()
            .any(|character| character != '=')
            || compact[..first_padding].contains('=')
        {
            return None;
        }
    }
    while compact.len() % 4 != 0 {
        compact.push('=');
    }

    let symbols = compact
        .chars()
        .map(base64_symbol)
        .collect::<Option<Vec<_>>>()?;

    let mut decoded = Vec::with_capacity(symbols.len() / 4 * 3);
    for (chunk_index, chunk) in symbols.chunks_exact(4).enumerate() {
        let is_last = chunk_index + 1 == symbols.len() / 4;
        if chunk[0] >= 64 || chunk[1] >= 64 || (!is_last && (chunk[2] >= 64 || chunk[3] >= 64)) {
            return None;
        }
        decoded.push((chunk[0] << 2) | (chunk[1] >> 4));
        if chunk[2] < 64 {
            decoded.push((chunk[1] << 4) | (chunk[2] >> 2));
            if chunk[3] < 64 {
                decoded.push((chunk[2] << 6) | chunk[3]);
            } else if chunk[3] != 64 {
                return None;
            }
        } else if chunk[3] != 64 {
            return None;
        }
    }
    Some(decoded)
}

fn base64_symbol(character: char) -> Option<u8> {
    match character {
        'A'..='Z' => Some(character as u8 - b'A'),
        'a'..='z' => Some(character as u8 - b'a' + 26),
        '0'..='9' => Some(character as u8 - b'0' + 52),
        '+' => Some(62),
        '/' => Some(63),
        '=' => Some(64),
        _ => None,
    }
}

fn escape_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            ';' => escaped.push_str("\\;"),
            ',' => escaped.push_str("\\,"),
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                escaped.push_str("\\n");
            }
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn unescape_value(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let mut unescaped = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        let Some(escaped) = characters.next() else {
            unescaped.push('\\');
            break;
        };
        match escaped {
            'n' | 'N' => unescaped.push('\n'),
            '\\' => unescaped.push('\\'),
            ',' => unescaped.push(','),
            ';' => unescaped.push(';'),
            // Preserve non-standard escape sequences instead of silently
            // changing provider data that this compact model does not own.
            other => {
                unescaped.push('\\');
                unescaped.push(other);
            }
        }
    }
    unescaped
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
        assert_eq!(contact.addresses, vec!["London, London, UK"]);
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
                property: "PHOTO;ENCODING=b;TYPE=JPEG".to_string(),
                raw_value: "YWJjZA==".to_string(),
            })
        );
        assert_eq!(
            contact.photo.as_ref().and_then(VCardPhoto::decoded_bytes),
            Some(b"abcd".to_vec())
        );
        let roundtrip = contact.to_vcard_string();
        assert!(roundtrip.contains("N:Lovelace;Ada;;Dr;III"));
        assert!(roundtrip.contains("PHOTO;ENCODING=b;TYPE=JPEG:YWJjZA=="));
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

    #[test]
    fn vcard_3_parameters_structured_addresses_and_unknown_fields_survive() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\n",
            "FN:Ada\\, Lovelace\r\n",
            "N:Lovelace;Ada;Byron;Dr;III\r\n",
            "UID:ada-123\r\n",
            "PRODID:-//Example Corp//Contacts//EN\r\n",
            "REV:20260812T120000Z\r\n",
            "EMAIL;TYPE=\"HOME,INTERNET\";PREF=1:ada@example.test\r\n",
            "EMAIL;TYPE=WORK:ada@analytical.example\r\n",
            "TEL;TYPE=\"CELL,VOICE\":+44 20 123\r\n",
            "TEL;TYPE=WORK:+44 20 456\r\n",
            "ADR;TYPE=HOME;LABEL=\"London, UK\":;;Baker\\, Street 221B;London;;;GB\r\n",
            "ADR;TYPE=WORK:;;Cambridge;Cambridge;;;GB\r\n",
            "URL;TYPE=WORK:https://example.test/a,b\r\n",
            "CATEGORIES:Research\\, Applied,Friends\r\n",
            "X-EVOLUTION-FILE-AS:Lovelace\\, Ada\r\n",
            "END:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(vcard).expect("vCard should parse");

        assert_eq!(contact.version, "3.0");
        assert_eq!(contact.fn_name, "Ada, Lovelace");
        assert_eq!(
            contact.emails,
            vec!["ada@example.test", "ada@analytical.example"]
        );
        assert_eq!(contact.phones, vec!["+44 20 123", "+44 20 456"]);
        assert_eq!(
            contact.addresses,
            vec!["Baker, Street 221B, London, GB", "Cambridge, Cambridge, GB"]
        );
        assert_eq!(
            contact.email_properties[0].property,
            "EMAIL;TYPE=\"HOME,INTERNET\";PREF=1"
        );
        assert_eq!(
            contact.address_properties[0].property,
            "ADR;TYPE=HOME;LABEL=\"London, UK\""
        );
        assert_eq!(contact.unknown_properties.len(), 3);
        assert_eq!(contact.unknown_properties[0].property, "UID");
        assert_eq!(contact.unknown_properties[0].value, "ada-123");

        let roundtrip = contact.to_vcard_string();
        assert!(roundtrip.contains("VERSION:3.0"));
        assert!(roundtrip.contains("EMAIL;TYPE=\"HOME,INTERNET\";PREF=1:ada@example.test"));
        assert!(
            roundtrip
                .contains("ADR;TYPE=HOME;LABEL=\"London, UK\":;;Baker\\, Street 221B;London;;;GB")
        );
        assert!(roundtrip.contains("UID:ada-123"));
        assert!(roundtrip.contains("PRODID:-//Example Corp//Contacts//EN"));
        assert!(roundtrip.contains("X-EVOLUTION-FILE-AS:Lovelace\\, Ada"));

        let reparsed = VCardContact::parse_vcard_string(&roundtrip).expect("roundtrip parses");
        assert_eq!(reparsed.version, "3.0");
        assert_eq!(reparsed.addresses, contact.addresses);
        assert_eq!(reparsed.categories, contact.categories);
        assert_eq!(
            reparsed.extension_values("X-EVOLUTION-FILE-AS"),
            vec!["Lovelace, Ada"]
        );
    }

    #[test]
    fn folded_lines_and_escaping_keep_logical_text() {
        let folded = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Folded Person\r\n",
            "NOTE:First line \r\n",
            " continuation with comma\\, and slash\\\\\r\n",
            "END:VCARD\r\n"
        );
        let parsed = VCardContact::parse_vcard_string(folded).expect("folded vCard should parse");
        assert_eq!(
            parsed.note,
            "First line continuation with comma, and slash\\"
        );

        let mut contact = VCardContact::new("Long Person", "long@example.test", "", "");
        contact.note = "a".repeat(180);
        let serialized = contact.to_vcard_string();
        assert!(
            serialized.contains("\r\n "),
            "long content should be folded"
        );
        assert!(
            serialized
                .replace("\r\n", "\n")
                .lines()
                .all(|line| line.len() <= 75)
        );
        let reparsed = VCardContact::parse_vcard_string(&serialized).expect("folded output parses");
        assert_eq!(reparsed.note, contact.note);
    }

    #[test]
    fn data_uri_and_multiple_binary_photos_roundtrip_without_loading_remote_content() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Photo Person\r\n",
            "PHOTO:DaTa:image/png;base64,YWJj\r\n",
            "PHOTO;ENCODING=\"b\";TYPE=PNG:YWJjZA==\r\n",
            "END:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(vcard).expect("photo vCard should parse");
        assert_eq!(contact.photos.len(), 2);
        assert_eq!(contact.photo.as_ref().unwrap().media_type, "image/png");
        assert_eq!(
            contact.photo.as_ref().and_then(VCardPhoto::decoded_bytes),
            Some(b"abc".to_vec())
        );
        assert_eq!(contact.photos[1].decoded_bytes(), Some(b"abcd".to_vec()));

        let roundtrip = contact.to_vcard_string();
        assert!(roundtrip.contains("PHOTO:DaTa:image/png;base64,YWJj"));
        assert!(roundtrip.contains("PHOTO;ENCODING=\"b\";TYPE=PNG:YWJjZA=="));
        let reparsed = VCardContact::parse_vcard_string(&roundtrip).expect("photos roundtrip");
        assert_eq!(reparsed.photos.len(), 2);
        assert_eq!(reparsed.photos[1].decoded_bytes(), Some(b"abcd".to_vec()));
    }

    #[test]
    fn evolution_list_vcard_3_roundtrip_keeps_external_local_and_nested_members() {
        let mut list = VCardContact::new_list("Team Europa", false, Some("list-1"));
        list.list_members = vec![
            VCardListMember::external_address("external@example.test", "Extern"),
            VCardListMember::local_contact(
                "contact-1",
                "local-addressbook",
                "Ada Lovelace",
                "ada@example.test",
                Some(0),
            ),
            VCardListMember::nested_list("2", "Unterteam", Some("0")),
        ];

        let serialized = list.to_vcard_string();
        assert!(serialized.contains("VERSION:3.0"));
        assert!(serialized.contains("X-EVOLUTION-LIST:TRUE"));
        assert!(serialized.contains("X-EVOLUTION-LIST-SHOW-ADDRESSES:FALSE"));
        let unfolded = serialized.replace("\r\n ", "");
        assert!(unfolded.contains("X-EVOLUTION-DEST-CONTACT-UID="));
        assert!(unfolded.contains("contact-1"));
        assert!(serialized.contains("X-EVOLUTION-CONTACT-LIST;X-EVOLUTION-CL-UID=2"));

        let parsed = VCardContact::parse_vcard_string(&serialized).expect("list should parse");
        assert!(parsed.is_list);
        assert_eq!(parsed.list_name, "Team Europa");
        assert!(!parsed.list_show_addresses);
        assert_eq!(parsed.list_members.len(), 3);
        assert_eq!(
            parsed.list_members[0].kind,
            VCardListMemberKind::ExternalAddress
        );
        assert_eq!(parsed.list_members[1].contact_uid, "contact-1");
        assert_eq!(parsed.list_members[2].kind, VCardListMemberKind::NestedList);

        let reparsed = VCardContact::parse_vcard_string(&parsed.to_vcard_string())
            .expect("list roundtrip should parse");
        assert_eq!(reparsed.list_members, parsed.list_members);
    }

    #[test]
    fn vcard_4_member_lists_roundtrip_and_mark_invalid_members() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:group\r\nFN:Projekt\r\n",
            "MEMBER:mailto:ada@example.test\r\n",
            "MEMBER:mailto:not-an-address\r\n",
            "MEMBER:urn:uuid:contact-1\r\n",
            "X-PROVIDER-UNKNOWN;X-PARAM=one:kept\r\nEND:VCARD\r\n"
        );
        let list = VCardContact::parse_vcard_string(vcard).expect("vCard 4 list should parse");
        assert!(list.is_list);
        assert_eq!(list.list_members.len(), 3);
        assert!(list.list_members[0].is_valid());
        assert!(!list.list_members[1].is_valid());
        assert_eq!(
            list.list_members[2].kind,
            VCardListMemberKind::StandardMember
        );
        assert!(list.list_members[2].is_valid());
        assert_eq!(list.invalid_list_members().len(), 1);
        let roundtrip = list.to_vcard_string();
        assert!(roundtrip.contains("VERSION:4.0"));
        assert!(roundtrip.contains("MEMBER:mailto:ada@example.test"));
        assert!(roundtrip.contains("MEMBER:mailto:not-an-address"));
        assert!(roundtrip.contains("MEMBER:urn:uuid:contact-1"));
        assert!(roundtrip.contains("X-PROVIDER-UNKNOWN;X-PARAM=one:kept"));
    }

    #[test]
    fn eds_list_parser_distinguishes_invalid_external_and_local_destinations() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ungültige Liste\r\n",
            "X-EVOLUTION-LIST:TRUE\r\n",
            "EMAIL:broken-address\r\n",
            "EMAIL;X-EVOLUTION-DEST-CONTACT-UID=local-1:broken-local\r\n",
            "END:VCARD\r\n"
        );
        let list = VCardContact::parse_vcard_string(vcard).expect("EDS list should parse");
        assert_eq!(list.list_members.len(), 2);
        assert_eq!(list.list_members[0].kind, VCardListMemberKind::Invalid);
        assert_eq!(list.list_members[1].kind, VCardListMemberKind::LocalContact);
        assert_eq!(list.invalid_list_members().len(), 2);
    }

    #[test]
    fn vcard4_group_uses_kind_and_member_without_eds_destination_lines() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:group-1\r\n",
            "FN:Projekt\r\nKIND:group\r\nEMAIL:group@example.test\r\n",
            "MEMBER:mailto:ada@example.test\r\nMEMBER:urn:uuid:contact-1\r\n",
            "END:VCARD\r\n"
        );
        let list = VCardContact::parse_vcard_string(vcard).expect("group should parse");
        assert!(list.is_list);
        assert!(list.list_validation_error().is_none());

        let roundtrip = list.to_vcard_string();
        assert!(roundtrip.contains("KIND:group"));
        assert!(roundtrip.contains("MEMBER:mailto:ada@example.test"));
        assert!(roundtrip.contains("MEMBER:urn:uuid:contact-1"));
        assert!(roundtrip.contains("EMAIL:group@example.test"));
        assert!(!roundtrip.contains("X-EVOLUTION-LIST:TRUE"));
        assert!(!roundtrip.contains("X-EVOLUTION-DEST-CONTACT-UID"));

        let reparsed = VCardContact::parse_vcard_string(&roundtrip).expect("roundtrip parses");
        assert_eq!(reparsed.list_members, list.list_members);
        assert_eq!(reparsed.emails, vec!["group@example.test"]);
    }

    #[test]
    fn vcard4_member_without_group_kind_is_rejected_until_repaired() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:group-2\r\nFN:Projekt\r\n",
            "MEMBER:mailto:ada@example.test\r\nEND:VCARD\r\n"
        );
        let mut list = VCardContact::parse_vcard_string(vcard).expect("group should parse");
        assert!(
            list.list_validation_error()
                .is_some_and(|error| error.contains("KIND:group"))
        );
        list.ensure_vcard4_group_kind();
        assert!(list.list_validation_error().is_none());
        assert!(list.to_vcard_string().contains("KIND:group"));
    }

    #[test]
    fn evolution_destination_parameters_unescape_and_roundtrip_exactly() {
        let vcard = concat!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:list-parameters\r\nFN:Team\r\n",
            "X-EVOLUTION-LIST:TRUE\r\n",
            "EMAIL;X-EVOLUTION-DEST-NAME=\"Ada\\, Lovelace\";",
            "X-EVOLUTION-DEST-CONTACT-UID=\"contact\\:1\";",
            "X-EVOLUTION-DEST-SOURCE-UID=\"source\\,1\";",
            "X-EVOLUTION-DEST-EMAIL-NUM=2;X-EVOLUTION-PARENT-CL=0:",
            "Ada Lovelace <ada@example.test>\r\nEND:VCARD\r\n"
        );
        let list = VCardContact::parse_vcard_string(vcard).expect("list should parse");
        let member = &list.list_members[0];
        assert_eq!(member.display_name, "Ada, Lovelace");
        assert_eq!(member.contact_uid, "contact:1");
        assert_eq!(member.source_uid, "source,1");
        assert_eq!(member.email_num, Some(2));
        let serialized = list.to_vcard_string();
        let unfolded = serialized.replace("\r\n ", "");
        assert!(unfolded.contains(
            "EMAIL;X-EVOLUTION-DEST-NAME=\"Ada\\, Lovelace\";X-EVOLUTION-DEST-CONTACT-UID=\"contact\\:1\";X-EVOLUTION-DEST-SOURCE-UID=\"source\\,1\";X-EVOLUTION-DEST-EMAIL-NUM=2;X-EVOLUTION-PARENT-CL=0:Ada Lovelace <ada@example.test>"
        ), "serialized vCard: {serialized}");
    }

    #[test]
    fn long_unknown_extension_keeps_logical_value_after_folding() {
        let long_value = "provider-value-".to_string() + &"x".repeat(160);
        let vcard = format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Provider\r\nX-PROVIDER-LONG:{long_value}\r\nEND:VCARD\r\n"
        );
        let contact = VCardContact::parse_vcard_string(&vcard).expect("vCard should parse");
        let serialized = contact.to_vcard_string();
        assert!(serialized.contains("X-PROVIDER-LONG:"));
        assert!(serialized.contains("\r\n "));
        let reparsed = VCardContact::parse_vcard_string(&serialized).expect("folded output parses");
        assert_eq!(
            reparsed.extension_values("X-PROVIDER-LONG"),
            vec![long_value]
        );
    }
}
