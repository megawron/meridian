// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable, GTK-free contact search.  This module deliberately evaluates the
// logical VCardContact model instead of searching a serialized vCard.  The
// latter contains UIDs, provider parameters, and Evolution extensions which
// are identities/metadata rather than user-visible contact fields.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::vcard_service::{VCardContact, VCardListMemberKind};

pub const CONTACT_SEARCH_FIXTURE_VERSION: u32 = 1;
const CONTACT_SEARCH_FIXTURE_FORMAT: &str = "meridian-contact-search";

/// The stable identity used by the contacts page and every search result.
/// Source UID is intentionally part of the key: providers are allowed to
/// reuse a contact UID in different address books.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ContactIdentity {
    pub source_uid: String,
    pub uid: String,
}

impl ContactIdentity {
    pub fn new(source_uid: impl Into<String>, uid: impl Into<String>) -> Self {
        Self {
            source_uid: source_uid.into(),
            uid: uid.into(),
        }
    }

    pub fn as_tuple(&self) -> (String, String) {
        (self.source_uid.clone(), self.uid.clone())
    }
}

/// An immutable worker input.  `source_display_name` is retained for the
/// view's source label, but is never included in a searchable field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactSearchSnapshot {
    pub identity: ContactIdentity,
    pub source_display_name: String,
    pub contact: VCardContact,
}

pub type ContactSnapshot = ContactSearchSnapshot;
pub type LocalContactSnapshot = ContactSearchSnapshot;
pub type ContactSearchAst = ContactSearchQuery;

impl ContactSearchSnapshot {
    pub fn new(
        source_uid: impl Into<String>,
        uid: impl Into<String>,
        contact: VCardContact,
    ) -> Self {
        Self {
            identity: ContactIdentity::new(source_uid, uid),
            source_display_name: String::new(),
            contact,
        }
    }

    pub fn with_source_display_name(
        source_uid: impl Into<String>,
        uid: impl Into<String>,
        source_display_name: impl Into<String>,
        contact: VCardContact,
    ) -> Self {
        Self {
            identity: ContactIdentity::new(source_uid, uid),
            source_display_name: source_display_name.into(),
            contact,
        }
    }

    pub fn from_vcard(
        source_uid: impl Into<String>,
        uid: impl Into<String>,
        source_display_name: impl Into<String>,
        vcard: &str,
    ) -> Result<Self, ContactSnapshotError> {
        let contact =
            VCardContact::parse_vcard_string(vcard).ok_or(ContactSnapshotError::InvalidVCard)?;
        Ok(Self::with_source_display_name(
            source_uid,
            uid,
            source_display_name,
            contact,
        ))
    }

    pub fn source_uid(&self) -> &str {
        &self.identity.source_uid
    }

    pub fn uid(&self) -> &str {
        &self.identity.uid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactSnapshotError {
    InvalidVCard,
}

impl fmt::Display for ContactSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVCard => formatter.write_str("vCard is invalid or has no FN field"),
        }
    }
}

impl std::error::Error for ContactSnapshotError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactSearchField {
    Name,
    Email,
    Phone,
    Category,
    #[serde(rename = "any_field")]
    AnyField,
}

pub type ContactField = ContactSearchField;

impl ContactSearchField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Email => "E-Mail",
            Self::Phone => "Telefon",
            Self::Category => "Kategorie",
            Self::AnyField => "Alle Felder",
        }
    }
}

impl Default for ContactSearchField {
    fn default() -> Self {
        Self::AnyField
    }
}

impl fmt::Display for ContactSearchField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactSearchOperator {
    Contains,
    #[serde(rename = "not_contains")]
    NotContains,
    Is,
    #[serde(rename = "is_not")]
    IsNot,
    #[serde(rename = "starts_with")]
    StartsWith,
    #[serde(rename = "ends_with")]
    EndsWith,
}

pub type ContactOperator = ContactSearchOperator;

impl ContactSearchOperator {
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "enthält",
            Self::NotContains => "enthält nicht",
            Self::Is => "ist",
            Self::IsNot => "ist nicht",
            Self::StartsWith => "beginnt mit",
            Self::EndsWith => "endet mit",
        }
    }
}

impl fmt::Display for ContactSearchOperator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactClauseGroup {
    All,
    Any,
}

pub type ClauseGroup = ContactClauseGroup;

impl Default for ContactClauseGroup {
    fn default() -> Self {
        Self::All
    }
}

impl ContactClauseGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Alle Bedingungen",
            Self::Any => "Mindestens eine Bedingung",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactSearchClause {
    pub field: ContactSearchField,
    pub operator: ContactSearchOperator,
    pub value: String,
}

pub type ContactClause = ContactSearchClause;

impl ContactSearchClause {
    pub fn new(
        field: ContactSearchField,
        operator: ContactSearchOperator,
        value: impl Into<String>,
    ) -> Self {
        Self {
            field,
            operator,
            value: value.into(),
        }
    }
}

/// Filters which remain outside the typed clause AST.  They correspond to
/// page-level source/category controls and are always ANDed with the AST.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContactOuterFilters {
    pub source_uid: Option<String>,
    pub category: Option<String>,
    pub unmatched_category: bool,
    pub duplicate_only: bool,
}

pub type ContactFilters = ContactOuterFilters;

impl ContactOuterFilters {
    pub fn source(mut self, source_uid: impl Into<String>) -> Self {
        self.source_uid = Some(source_uid.into());
        self
    }

    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn unmatched_category(mut self, enabled: bool) -> Self {
        self.unmatched_category = enabled;
        self
    }

    pub fn duplicate_only(mut self, enabled: bool) -> Self {
        self.duplicate_only = enabled;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactSearchQuery {
    pub group: ContactClauseGroup,
    pub clauses: Vec<ContactSearchClause>,
    #[serde(default)]
    pub filters: ContactOuterFilters,
}

pub type ContactQuery = ContactSearchQuery;

impl Default for ContactSearchQuery {
    fn default() -> Self {
        Self::all()
    }
}

impl ContactSearchQuery {
    pub fn all() -> Self {
        Self {
            group: ContactClauseGroup::All,
            clauses: Vec::new(),
            filters: ContactOuterFilters::default(),
        }
    }

    pub fn any() -> Self {
        Self {
            group: ContactClauseGroup::Any,
            clauses: Vec::new(),
            filters: ContactOuterFilters::default(),
        }
    }

    pub fn new(group: ContactClauseGroup, clauses: Vec<ContactSearchClause>) -> Self {
        Self {
            group,
            clauses,
            filters: ContactOuterFilters::default(),
        }
    }

    pub fn with_filters(mut self, filters: ContactOuterFilters) -> Self {
        self.filters = filters;
        self
    }

    pub fn contains(field: ContactSearchField, value: impl Into<String>) -> Self {
        Self::new(
            ContactClauseGroup::All,
            vec![ContactSearchClause::new(
                field,
                ContactSearchOperator::Contains,
                value,
            )],
        )
    }

    /// Explicit global-search mapping.  This is intentionally not a parser:
    /// the cross-component global search contract is always Any Field
    /// contains, so unsupported native search expressions cannot broaden it.
    pub fn global_search(value: &str) -> Self {
        let value = value.trim();
        if value.is_empty() {
            Self::all()
        } else {
            Self::contains(ContactSearchField::AnyField, value)
        }
    }

    pub fn validate(&self) -> Result<(), ContactSearchValidationError> {
        if self.group == ContactClauseGroup::Any && self.clauses.is_empty() {
            return Err(ContactSearchValidationError::EmptyAnyGroup);
        }
        for (index, clause) in self.clauses.iter().enumerate() {
            if normalize_search_text(&clause.value).is_empty() {
                return Err(ContactSearchValidationError::EmptyClauseValue { index });
            }
            if !operators_for_field(clause.field).contains(&clause.operator) {
                return Err(ContactSearchValidationError::UnsupportedOperator {
                    field: clause.field,
                    operator: clause.operator,
                });
            }
        }

        if self
            .filters
            .source_uid
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ContactSearchValidationError::EmptySourceFilter);
        }
        if self
            .filters
            .category
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ContactSearchValidationError::EmptyCategoryFilter);
        }
        Ok(())
    }

    pub fn encode_fixture(&self) -> Result<String, ContactSearchValidationError> {
        self.validate()?;
        let fixture = ContactSearchFixtureV1 {
            format: CONTACT_SEARCH_FIXTURE_FORMAT.to_string(),
            version: CONTACT_SEARCH_FIXTURE_VERSION,
            group: self.group,
            clauses: self.clauses.clone(),
            filters: self.filters.clone(),
        };
        serde_json::to_string_pretty(&fixture)
            .map_err(|error| ContactSearchValidationError::InvalidFixture(error.to_string()))
    }

    pub fn decode_fixture(fixture: &str) -> Result<Self, ContactSearchValidationError> {
        // Decode the small envelope first.  A future fixture is allowed to
        // change its payload shape, so strict v1 decoding must not turn an
        // otherwise well-formed future version into a misleading payload
        // error.
        let envelope: ContactSearchFixtureEnvelope = serde_json::from_str(fixture)
            .map_err(|error| ContactSearchValidationError::InvalidFixture(error.to_string()))?;
        if envelope.format != CONTACT_SEARCH_FIXTURE_FORMAT {
            return Err(ContactSearchValidationError::InvalidFixture(
                "unknown contact-search fixture format".to_string(),
            ));
        }
        if envelope.version != CONTACT_SEARCH_FIXTURE_VERSION {
            return Err(ContactSearchValidationError::UnsupportedFixtureVersion(
                envelope.version,
            ));
        }
        let parsed: ContactSearchFixtureV1 = serde_json::from_str(fixture)
            .map_err(|error| ContactSearchValidationError::InvalidFixture(error.to_string()))?;
        let query = Self {
            group: parsed.group,
            clauses: parsed.clauses,
            filters: parsed.filters,
        };
        query.validate()?;
        Ok(query)
    }
}

#[derive(Debug, Deserialize)]
struct ContactSearchFixtureEnvelope {
    format: String,
    version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactSearchFixtureV1 {
    format: String,
    version: u32,
    group: ContactClauseGroup,
    clauses: Vec<ContactSearchClause>,
    #[serde(default)]
    filters: ContactOuterFilters,
}

pub fn encode_query_fixture(
    query: &ContactSearchQuery,
) -> Result<String, ContactSearchValidationError> {
    query.encode_fixture()
}

pub fn decode_query_fixture(
    fixture: &str,
) -> Result<ContactSearchQuery, ContactSearchValidationError> {
    ContactSearchQuery::decode_fixture(fixture)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ContactQueryCodec;

impl ContactQueryCodec {
    pub const VERSION: u32 = CONTACT_SEARCH_FIXTURE_VERSION;

    pub fn encode(query: &ContactSearchQuery) -> Result<String, ContactSearchValidationError> {
        encode_query_fixture(query)
    }

    pub fn decode(fixture: &str) -> Result<ContactSearchQuery, ContactSearchValidationError> {
        decode_query_fixture(fixture)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldOperatorSpec {
    pub field: ContactSearchField,
    pub operators: &'static [ContactSearchOperator],
}

const TEXT_OPERATORS: &[ContactSearchOperator] = &[
    ContactSearchOperator::Contains,
    ContactSearchOperator::NotContains,
    ContactSearchOperator::Is,
    ContactSearchOperator::IsNot,
    ContactSearchOperator::StartsWith,
    ContactSearchOperator::EndsWith,
];
const CATEGORY_OPERATORS: &[ContactSearchOperator] = &[
    ContactSearchOperator::Contains,
    ContactSearchOperator::NotContains,
    ContactSearchOperator::Is,
    ContactSearchOperator::IsNot,
];
const ANY_FIELD_OPERATORS: &[ContactSearchOperator] = &[ContactSearchOperator::Contains];

pub fn operators_for_field(field: ContactSearchField) -> &'static [ContactSearchOperator] {
    match field {
        ContactSearchField::Name | ContactSearchField::Email | ContactSearchField::Phone => {
            TEXT_OPERATORS
        }
        ContactSearchField::Category => CATEGORY_OPERATORS,
        ContactSearchField::AnyField => ANY_FIELD_OPERATORS,
    }
}

pub fn field_operator_registry() -> Vec<FieldOperatorSpec> {
    [
        ContactSearchField::Name,
        ContactSearchField::Email,
        ContactSearchField::Phone,
        ContactSearchField::Category,
        ContactSearchField::AnyField,
    ]
    .into_iter()
    .map(|field| FieldOperatorSpec {
        field,
        operators: operators_for_field(field),
    })
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactSearchValidationError {
    EmptyAnyGroup,
    EmptyClauseValue {
        index: usize,
    },
    EmptySourceFilter,
    EmptyCategoryFilter,
    UnsupportedOperator {
        field: ContactSearchField,
        operator: ContactSearchOperator,
    },
    UnsupportedField(String),
    UnsupportedOperatorText(String),
    UnsupportedRawSyntax(String),
    InvalidSyntax(String),
    InvalidFixture(String),
    UnsupportedFixtureVersion(u32),
}

impl ContactSearchValidationError {
    pub fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedOperator { .. }
                | Self::UnsupportedField(_)
                | Self::UnsupportedOperatorText(_)
                | Self::UnsupportedRawSyntax(_)
                | Self::UnsupportedFixtureVersion(_)
        )
    }
}

impl fmt::Display for ContactSearchValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAnyGroup => {
                formatter.write_str("Eine Any-Gruppe benötigt mindestens eine Bedingung")
            }
            Self::EmptyClauseValue { index } => {
                write!(formatter, "Bedingung {} benötigt einen Suchwert", index + 1)
            }
            Self::EmptySourceFilter => formatter.write_str("Quellfilter darf nicht leer sein"),
            Self::EmptyCategoryFilter => {
                formatter.write_str("Kategoriefilter darf nicht leer sein")
            }
            Self::UnsupportedOperator { field, operator } => write!(
                formatter,
                "Operator „{operator}“ wird für das Feld „{field}“ nicht unterstützt"
            ),
            Self::UnsupportedField(field) => {
                write!(formatter, "Unbekanntes Suchfeld „{field}“")
            }
            Self::UnsupportedOperatorText(operator) => {
                write!(formatter, "Unbekannter Suchoperator „{operator}“")
            }
            Self::UnsupportedRawSyntax(message) => {
                write!(formatter, "Nicht unterstützte Suche: {message}")
            }
            Self::InvalidSyntax(message) => write!(formatter, "Ungültige Suche: {message}"),
            Self::InvalidFixture(message) => write!(formatter, "Ungültiges Suchfixture: {message}"),
            Self::UnsupportedFixtureVersion(version) => write!(
                formatter,
                "Suchfixture-Version {version} wird nicht unterstützt"
            ),
        }
    }
}

impl std::error::Error for ContactSearchValidationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactSearchRunError {
    InvalidQuery(ContactSearchValidationError),
    Cancelled,
}

impl fmt::Display for ContactSearchRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuery(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("Kontakt-Suche abgebrochen"),
        }
    }
}

impl std::error::Error for ContactSearchRunError {}

/// Return only the visible logical values for a field.  In particular this
/// never returns source IDs, contact UIDs, property names, extension values,
/// raw vCard text, or provider parameters.
pub fn field_values(contact: &VCardContact, field: ContactSearchField) -> Vec<String> {
    let mut values = Vec::new();
    match field {
        ContactSearchField::Name => append_name_values(contact, &mut values),
        ContactSearchField::Email => append_email_values(contact, &mut values),
        ContactSearchField::Phone => append_phone_values(contact, &mut values),
        ContactSearchField::Category => append_category_values(contact, &mut values),
        ContactSearchField::AnyField => append_any_field_values(contact, &mut values),
    }
    values
}

fn append_name_values(contact: &VCardContact, values: &mut Vec<String>) {
    // Evolution's GTK3 name query operates on the single full-name value
    // (FN), not independently on the structured N components.  Keep that
    // contract here so `is`, `starts-with`, and `ends-with` cannot match a
    // lone family/given-name component by accident.
    let canonical_name = if contact.fn_name.trim().is_empty() {
        &contact.list_name
    } else {
        &contact.fn_name
    };
    push_visible(values, canonical_name);
}

fn append_email_values(contact: &VCardContact, values: &mut Vec<String>) {
    if contact.emails.is_empty() {
        push_visible(values, &contact.email);
    } else {
        for email in &contact.emails {
            push_visible(values, email);
        }
    }
    for member in &contact.list_members {
        if matches!(
            member.kind,
            VCardListMemberKind::ExternalAddress
                | VCardListMemberKind::LocalContact
                | VCardListMemberKind::StandardMember
        ) {
            push_visible(values, &member.email);
        }
    }
}

fn append_phone_values(contact: &VCardContact, values: &mut Vec<String>) {
    if contact.phones.is_empty() {
        push_visible(values, &contact.tel);
    } else {
        for phone in &contact.phones {
            push_visible(values, phone);
        }
    }
}

fn append_category_values(contact: &VCardContact, values: &mut Vec<String>) {
    for category in &contact.categories {
        push_visible(values, category);
    }
}

fn append_any_field_values(contact: &VCardContact, values: &mut Vec<String>) {
    append_name_values(contact, values);
    push_visible(values, &contact.list_name);
    push_visible(values, &contact.org);
    push_visible(values, &contact.title);
    push_visible(values, &contact.note);
    push_visible(values, &contact.nickname);
    push_visible(values, &contact.birthday);
    push_visible(values, &contact.anniversary);
    push_visible(values, &contact.role);
    push_visible(values, &contact.gender);
    push_visible(values, &contact.timezone);
    push_visible(values, &contact.geo);
    append_email_values(contact, values);
    append_phone_values(contact, values);
    append_category_values(contact, values);
    for address in &contact.addresses {
        push_visible(values, address);
    }
    for url in &contact.urls {
        push_visible(values, url);
    }
    for impp in &contact.impps {
        push_visible(values, impp);
    }
    for related in &contact.related {
        push_visible(values, related);
    }
    // Keep every member kind searchable.  `value` is important for invalid
    // and URI members where the parsed email/display fields are empty; source
    // and contact UIDs are intentionally excluded because they are metadata,
    // not visible contact content.
    for member in &contact.list_members {
        push_visible(values, &member.display_name);
        push_visible(values, &member.email);
        push_visible(values, &member.value);
    }
}

fn push_visible(values: &mut Vec<String>, value: &str) {
    if !value.trim().is_empty() {
        values.push(value.to_string());
    }
}

pub fn normalize_search_text(value: &str) -> String {
    // GLib is already a production dependency and provides Unicode-aware
    // case folding and canonical composition.  Applying NFC after folding
    // makes composed and decomposed input compare identically while keeping
    // whitespace tokenization deterministic.
    let composed = glib::normalize(value, glib::NormalizeMode::DefaultCompose);
    let folded = glib::casefold(composed.as_str());
    let composed = glib::normalize(folded.as_str(), glib::NormalizeMode::DefaultCompose);
    composed.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn value_matches(value: &str, needle: &str, operator: ContactSearchOperator) -> bool {
    let value = normalize_search_text(value);
    let needle = normalize_search_text(needle);
    match operator {
        ContactSearchOperator::Contains => value.contains(&needle),
        ContactSearchOperator::NotContains => !value.contains(&needle),
        ContactSearchOperator::Is => value == needle,
        ContactSearchOperator::IsNot => value != needle,
        ContactSearchOperator::StartsWith => value.starts_with(&needle),
        ContactSearchOperator::EndsWith => value.ends_with(&needle),
    }
}

fn field_matches(
    contact: &VCardContact,
    field: ContactSearchField,
    operator: ContactSearchOperator,
    needle: &str,
) -> bool {
    let values = field_values(contact, field);
    if field == ContactSearchField::AnyField && operator == ContactSearchOperator::Contains {
        let terms = normalize_search_text(needle)
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        return !terms.is_empty()
            && terms.iter().all(|term| {
                values
                    .iter()
                    .any(|value| normalize_search_text(value).contains(term))
            });
    }
    match operator {
        ContactSearchOperator::NotContains | ContactSearchOperator::IsNot => values
            .iter()
            .all(|value| value_matches(value, needle, operator)),
        _ => values
            .iter()
            .any(|value| value_matches(value, needle, operator)),
    }
}

fn outer_filters_match(
    snapshot: &ContactSearchSnapshot,
    filters: &ContactOuterFilters,
    duplicate_ids: &HashSet<ContactIdentity>,
) -> bool {
    let source_matches = filters
        .source_uid
        .as_deref()
        .is_none_or(|source_uid| source_uid == snapshot.source_uid());
    let category_matches = filters.category.as_deref().is_none_or(|category| {
        let needle = normalize_search_text(category);
        snapshot
            .contact
            .categories
            .iter()
            .any(|candidate| normalize_search_text(candidate) == needle)
    });
    let unmatched_matches = !filters.unmatched_category
        || snapshot
            .contact
            .categories
            .iter()
            .all(|category| category.trim().is_empty());
    let duplicate_matches = !filters.duplicate_only || duplicate_ids.contains(&snapshot.identity);
    source_matches && category_matches && unmatched_matches && duplicate_matches
}

fn query_matches(
    query: &ContactSearchQuery,
    snapshot: &ContactSearchSnapshot,
    duplicate_ids: &HashSet<ContactIdentity>,
) -> bool {
    if !outer_filters_match(snapshot, &query.filters, duplicate_ids) {
        return false;
    }
    match query.group {
        ContactClauseGroup::All => query.clauses.iter().all(|clause| {
            field_matches(
                &snapshot.contact,
                clause.field,
                clause.operator,
                &clause.value,
            )
        }),
        ContactClauseGroup::Any => query.clauses.iter().any(|clause| {
            field_matches(
                &snapshot.contact,
                clause.field,
                clause.operator,
                &clause.value,
            )
        }),
    }
}

fn duplicate_identities(snapshots: &[ContactSearchSnapshot]) -> HashSet<ContactIdentity> {
    let keys = snapshots
        .iter()
        .map(|snapshot| {
            (
                snapshot.identity.clone(),
                snapshot
                    .contact
                    .duplicate_keys()
                    .into_iter()
                    .collect::<HashSet<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let mut duplicate_ids = HashSet::new();
    for (index, (identity, own_keys)) in keys.iter().enumerate() {
        if keys
            .iter()
            .enumerate()
            .any(|(other_index, (_, other_keys))| {
                index != other_index && own_keys.iter().any(|key| other_keys.contains(key))
            })
        {
            duplicate_ids.insert(identity.clone());
        }
    }
    duplicate_ids
}

fn duplicate_identities_cancelable<F>(
    snapshots: &[ContactSearchSnapshot],
    cancellation: &AtomicBool,
    checkpoint: &mut F,
) -> Result<HashSet<ContactIdentity>, ContactSearchRunError>
where
    F: FnMut(),
{
    let mut keys = Vec::with_capacity(snapshots.len());
    for (index, snapshot) in snapshots.iter().enumerate() {
        if index % 16 == 0 && cancellation.load(Ordering::Relaxed) {
            return Err(ContactSearchRunError::Cancelled);
        }
        keys.push((
            snapshot.identity.clone(),
            snapshot
                .contact
                .duplicate_keys()
                .into_iter()
                .collect::<HashSet<_>>(),
        ));
        if index % 16 == 15 {
            checkpoint();
        }
    }
    let mut duplicate_ids = HashSet::new();
    for (index, (identity, own_keys)) in keys.iter().enumerate() {
        if index % 32 == 0 && cancellation.load(Ordering::Relaxed) {
            return Err(ContactSearchRunError::Cancelled);
        }
        for (other_index, (_, other_keys)) in keys.iter().enumerate() {
            if index != other_index && own_keys.iter().any(|key| other_keys.contains(key)) {
                duplicate_ids.insert(identity.clone());
                break;
            }
            if other_index % 128 == 0 && cancellation.load(Ordering::Relaxed) {
                return Err(ContactSearchRunError::Cancelled);
            }
        }
        if index % 32 == 31 {
            checkpoint();
        }
    }
    Ok(duplicate_ids)
}

pub fn evaluate_contacts(
    query: &ContactSearchQuery,
    snapshots: &[ContactSearchSnapshot],
) -> Result<Vec<ContactIdentity>, ContactSearchValidationError> {
    query.validate()?;
    let duplicate_ids = duplicate_identities(snapshots);
    Ok(snapshots
        .iter()
        .filter(|snapshot| query_matches(query, snapshot, &duplicate_ids))
        .map(|snapshot| snapshot.identity.clone())
        .collect())
}

pub fn evaluate_contacts_cancelable(
    query: &ContactSearchQuery,
    snapshots: &[ContactSearchSnapshot],
    cancellation: &AtomicBool,
) -> Result<Vec<ContactIdentity>, ContactSearchRunError> {
    evaluate_contacts_cancelable_with_checkpoint(query, snapshots, cancellation, || {})
}

fn evaluate_contacts_cancelable_with_checkpoint<F>(
    query: &ContactSearchQuery,
    snapshots: &[ContactSearchSnapshot],
    cancellation: &AtomicBool,
    mut checkpoint: F,
) -> Result<Vec<ContactIdentity>, ContactSearchRunError>
where
    F: FnMut(),
{
    query
        .validate()
        .map_err(ContactSearchRunError::InvalidQuery)?;
    if cancellation.load(Ordering::Relaxed) {
        return Err(ContactSearchRunError::Cancelled);
    }
    let duplicate_ids = duplicate_identities_cancelable(snapshots, cancellation, &mut checkpoint)?;
    let mut matches = Vec::new();
    for (index, snapshot) in snapshots.iter().enumerate() {
        if index % 32 == 0 && cancellation.load(Ordering::Relaxed) {
            return Err(ContactSearchRunError::Cancelled);
        }
        if query_matches(query, snapshot, &duplicate_ids) {
            matches.push(snapshot.identity.clone());
        }
        if index % 32 == 31 {
            checkpoint();
        }
    }
    if cancellation.load(Ordering::Relaxed) {
        Err(ContactSearchRunError::Cancelled)
    } else {
        Ok(matches)
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct ContactSearchTestGates {
    mid_evaluation: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
    pause_once: Arc<AtomicBool>,
}

#[cfg(test)]
impl Default for ContactSearchTestGates {
    fn default() -> Self {
        Self {
            mid_evaluation: Arc::new(std::sync::Barrier::new(2)),
            release: Arc::new(std::sync::Barrier::new(2)),
            pause_once: Arc::new(AtomicBool::new(true)),
        }
    }
}

#[cfg(test)]
impl ContactSearchTestGates {
    fn checkpoint(&self) {
        if self.pause_once.swap(false, Ordering::AcqRel) {
            self.mid_evaluation.wait();
            self.release.wait();
        }
    }

    fn wait_until_mid_evaluation(&self) {
        self.mid_evaluation.wait();
    }

    fn release_evaluation(&self) {
        self.release.wait();
    }
}

/// A latest-wins, framework-neutral generation coordinator.  Each `begin`
/// cancels the previous request and makes its result stale even if a worker
/// happens to finish after cancellation was observed.
#[derive(Debug, Clone, Default)]
pub struct LatestGenerationEvaluator {
    current_generation: Arc<AtomicU64>,
    current_cancellation: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    #[cfg(test)]
    test_gates: Option<Arc<ContactSearchTestGates>>,
}

pub type ContactSearchCoordinator = LatestGenerationEvaluator;

#[derive(Debug, Clone)]
pub struct ContactSearchRequest {
    generation: u64,
    cancellation: Arc<AtomicBool>,
    current_generation: Arc<AtomicU64>,
}

impl ContactSearchRequest {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancellation.clone()
    }

    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::Relaxed);
    }

    pub fn is_current(&self) -> bool {
        self.current_generation.load(Ordering::Acquire) == self.generation
            && !self.cancellation.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactSearchEvaluation {
    Results {
        generation: u64,
        identities: Vec<ContactIdentity>,
    },
    Cancelled {
        generation: u64,
    },
}

impl ContactSearchEvaluation {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Results { generation, .. } | Self::Cancelled { generation } => *generation,
        }
    }

    pub fn identities(&self) -> Option<&[ContactIdentity]> {
        match self {
            Self::Results { identities, .. } => Some(identities),
            Self::Cancelled { .. } => None,
        }
    }
}

impl LatestGenerationEvaluator {
    #[cfg(test)]
    fn with_test_gates(gates: Arc<ContactSearchTestGates>) -> Self {
        Self {
            test_gates: Some(gates),
            ..Self::default()
        }
    }

    pub fn begin(&self) -> ContactSearchRequest {
        let generation = self.current_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let cancellation = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self
            .current_cancellation
            .lock()
            .expect("contact search generation mutex is not poisoned")
            .replace(cancellation.clone())
        {
            previous.store(true, Ordering::Relaxed);
        }
        ContactSearchRequest {
            generation,
            cancellation,
            current_generation: self.current_generation.clone(),
        }
    }

    pub fn current_generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    fn checkpoint(&self) {
        #[cfg(test)]
        if let Some(gates) = self.test_gates.as_ref() {
            gates.checkpoint();
        }
    }

    pub fn evaluate(
        &self,
        request: &ContactSearchRequest,
        query: &ContactSearchQuery,
        snapshots: &[ContactSearchSnapshot],
    ) -> Result<ContactSearchEvaluation, ContactSearchValidationError> {
        if !request.is_current() {
            return Ok(ContactSearchEvaluation::Cancelled {
                generation: request.generation,
            });
        }
        let identities = match evaluate_contacts_cancelable_with_checkpoint(
            query,
            snapshots,
            &request.cancellation,
            || self.checkpoint(),
        ) {
            Ok(identities) => identities,
            Err(ContactSearchRunError::Cancelled) => {
                return Ok(ContactSearchEvaluation::Cancelled {
                    generation: request.generation,
                });
            }
            Err(ContactSearchRunError::InvalidQuery(error)) => return Err(error),
        };
        if !request.is_current() {
            Ok(ContactSearchEvaluation::Cancelled {
                generation: request.generation,
            })
        } else {
            Ok(ContactSearchEvaluation::Results {
                generation: request.generation,
                identities,
            })
        }
    }
}

#[derive(Debug)]
struct ContactSearchJob {
    coordinator: LatestGenerationEvaluator,
    request: ContactSearchRequest,
    query: ContactSearchQuery,
    snapshots: Vec<ContactSearchSnapshot>,
    results: async_channel::Sender<Result<ContactSearchEvaluation, ContactSearchValidationError>>,
}

#[derive(Debug)]
struct ContactSearchQueue {
    pending: Mutex<Option<ContactSearchJob>>,
    wake: Condvar,
}

impl ContactSearchQueue {
    fn replace_pending(&self, job: ContactSearchJob) {
        let mut pending = self
            .pending
            .lock()
            .expect("contact search queue mutex is not poisoned");
        *pending = Some(job);
        self.wake.notify_one();
    }

    fn wait_for_pending(&self) -> ContactSearchJob {
        let mut pending = self
            .pending
            .lock()
            .expect("contact search queue mutex is not poisoned");
        loop {
            if let Some(job) = pending.take() {
                return job;
            }
            pending = self
                .wake
                .wait(pending)
                .expect("contact search queue mutex is not poisoned");
        }
    }
}

/// One bounded latest-wins worker for a page/search session.  A new request
/// replaces a queued request, while an in-flight evaluation is cooperatively
/// cancelled through its generation flag.  This keeps rapid typing from
/// creating one OS thread per key press.
#[derive(Debug, Clone)]
pub struct LatestContactSearchWorker {
    queue: Arc<ContactSearchQueue>,
}

impl Default for LatestContactSearchWorker {
    fn default() -> Self {
        let queue = Arc::new(ContactSearchQueue {
            pending: Mutex::new(None),
            wake: Condvar::new(),
        });
        let worker_queue = Arc::clone(&queue);
        std::thread::Builder::new()
            .name("contact-search".to_string())
            .spawn(move || {
                loop {
                    let job = worker_queue.wait_for_pending();
                    let result = job
                        .coordinator
                        .evaluate(&job.request, &job.query, &job.snapshots);
                    let _ = job.results.send_blocking(result);
                }
            })
            .expect("contact search worker thread should start");
        Self { queue }
    }
}

impl LatestContactSearchWorker {
    pub fn submit(
        &self,
        coordinator: LatestGenerationEvaluator,
        request: ContactSearchRequest,
        query: ContactSearchQuery,
        snapshots: Vec<ContactSearchSnapshot>,
    ) -> async_channel::Receiver<Result<ContactSearchEvaluation, ContactSearchValidationError>>
    {
        let (results, receiver) = async_channel::bounded(1);
        let job = ContactSearchJob {
            coordinator,
            request,
            query,
            snapshots,
            results,
        };
        self.queue.replace_pending(job);
        receiver
    }
}

/// Stores the last successful result.  A rejected query never changes this
/// value, which is the portable half of the page's last-valid-result rule.
#[derive(Debug, Clone)]
pub struct LastValidContactSearch {
    last_valid_query: ContactSearchQuery,
    last_valid_results: Vec<ContactIdentity>,
}

impl Default for LastValidContactSearch {
    fn default() -> Self {
        Self {
            last_valid_query: ContactSearchQuery::all(),
            last_valid_results: Vec::new(),
        }
    }
}

impl LastValidContactSearch {
    pub fn last_valid_query(&self) -> &ContactSearchQuery {
        &self.last_valid_query
    }

    pub fn last_valid_results(&self) -> &[ContactIdentity] {
        &self.last_valid_results
    }

    pub fn accept_results(&mut self, query: ContactSearchQuery, results: Vec<ContactIdentity>) {
        self.last_valid_query = query;
        self.last_valid_results = results;
    }

    pub fn try_apply(
        &mut self,
        query: &ContactSearchQuery,
        snapshots: &[ContactSearchSnapshot],
    ) -> Result<Vec<ContactIdentity>, ContactSearchValidationError> {
        let identities = evaluate_contacts(query, snapshots)?;
        self.last_valid_query = query.clone();
        self.last_valid_results = identities.clone();
        Ok(identities)
    }
}

pub fn parse_query_text(input: &str) -> Result<ContactSearchQuery, ContactSearchValidationError> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(ContactSearchQuery::all());
    }
    reject_unsupported_raw_syntax(input)?;

    let (group, expression) = parse_group_prefix(input)?;
    let (parts, separator_group) = split_query_expression(expression)?;
    let group = separator_group.unwrap_or(group);
    if parts.is_empty() {
        return Err(ContactSearchValidationError::InvalidSyntax(
            "keine Bedingung gefunden".to_string(),
        ));
    }
    let clauses = parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            parse_clause_text(&part).map_err(|error| add_clause_context(error, index))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ContactSearchQuery::new(group, clauses).tap_validate()
}

pub fn validate_query_text(
    input: &str,
) -> Result<ContactSearchQuery, ContactSearchValidationError> {
    parse_query_text(input)
}

trait TapValidate: Sized {
    fn tap_validate(self) -> Result<Self, ContactSearchValidationError>;
}

impl TapValidate for ContactSearchQuery {
    fn tap_validate(self) -> Result<Self, ContactSearchValidationError> {
        self.validate()?;
        Ok(self)
    }
}

fn add_clause_context(
    error: ContactSearchValidationError,
    index: usize,
) -> ContactSearchValidationError {
    match error {
        ContactSearchValidationError::InvalidSyntax(message) => {
            ContactSearchValidationError::InvalidSyntax(format!(
                "Bedingung {}: {message}",
                index + 1
            ))
        }
        other => other,
    }
}

fn reject_unsupported_raw_syntax(input: &str) -> Result<(), ContactSearchValidationError> {
    let lower = input.to_ascii_lowercase();
    if input.trim_start().starts_with('(')
        || lower.starts_with("gtkexpression")
        || lower.starts_with("gtk3-expression")
        || lower.starts_with("expression:")
        || lower.starts_with("s-expression:")
        || lower.starts_with("sexp:")
        || lower.contains("gtk3 expression")
    {
        return Err(ContactSearchValidationError::UnsupportedRawSyntax(
            "GTK3-Expressionen und S-Ausdrücke sind nicht Teil der portablen Suche".to_string(),
        ));
    }
    Ok(())
}

fn parse_group_prefix(
    input: &str,
) -> Result<(ContactClauseGroup, &str), ContactSearchValidationError> {
    for (prefix, group) in [
        ("all:", ContactClauseGroup::All),
        ("any:", ContactClauseGroup::Any),
        ("all ", ContactClauseGroup::All),
        ("any ", ContactClauseGroup::Any),
    ] {
        if input
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        {
            let rest = input[prefix.len()..].trim();
            if rest.is_empty() {
                return Err(ContactSearchValidationError::InvalidSyntax(
                    "Gruppenpräfix benötigt Bedingungen".to_string(),
                ));
            }
            return Ok((group, rest));
        }
    }
    Ok((ContactClauseGroup::All, input))
}

fn split_query_expression(
    input: &str,
) -> Result<(Vec<String>, Option<ContactClauseGroup>), ContactSearchValidationError> {
    let mut parts = Vec::new();
    let mut separators = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut chars = input.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        match quote {
            Some(quote_character) if character == quote_character => quote = None,
            Some(_) => continue,
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == ';' => {
                push_query_part(&mut parts, &input[start..index])?;
                separators.push(ContactClauseGroup::All);
                start = index + character.len_utf8();
            }
            None if character == '&' && chars.peek().is_some_and(|(_, next)| *next == '&') => {
                let (next_index, next) = chars.next().expect("peeked &&");
                push_query_part(&mut parts, &input[start..index])?;
                separators.push(ContactClauseGroup::All);
                start = next_index + next.len_utf8();
            }
            None if character == '|' && chars.peek().is_some_and(|(_, next)| *next == '|') => {
                let (next_index, next) = chars.next().expect("peeked ||");
                push_query_part(&mut parts, &input[start..index])?;
                separators.push(ContactClauseGroup::Any);
                start = next_index + next.len_utf8();
            }
            None => {
                let tail = &input[index..];
                let candidates = [
                    (" AND ", ContactClauseGroup::All),
                    (" OR ", ContactClauseGroup::Any),
                ];
                if let Some((separator, group)) = candidates.iter().find(|(separator, _)| {
                    tail.len() >= separator.len()
                        && tail[..separator.len()].eq_ignore_ascii_case(separator)
                }) {
                    push_query_part(&mut parts, &input[start..index])?;
                    separators.push(*group);
                    start = index + separator.len();
                    for _ in 0..separator.chars().count().saturating_sub(1) {
                        chars.next();
                    }
                }
            }
        }
    }
    if quote.is_some() {
        return Err(ContactSearchValidationError::InvalidSyntax(
            "Anführungszeichen nicht geschlossen".to_string(),
        ));
    }
    push_query_part(&mut parts, &input[start..])?;
    let group = separators.first().copied();
    if separators.iter().any(|candidate| Some(*candidate) != group) {
        return Err(ContactSearchValidationError::InvalidSyntax(
            "AND und OR müssen über eine All- oder Any-Gruppe getrennt werden".to_string(),
        ));
    }
    Ok((parts, group))
}

fn push_query_part(
    parts: &mut Vec<String>,
    part: &str,
) -> Result<(), ContactSearchValidationError> {
    let part = part.trim();
    if part.is_empty() {
        return Err(ContactSearchValidationError::InvalidSyntax(
            "leere Bedingung".to_string(),
        ));
    }
    parts.push(part.to_string());
    Ok(())
}

fn parse_clause_text(input: &str) -> Result<ContactSearchClause, ContactSearchValidationError> {
    let input = input.trim();
    let tokens = if let Some((field, rest)) = input.split_once(':') {
        if let Some((operator, value)) = rest.split_once(':') {
            vec![
                field.trim().to_string(),
                operator.trim().to_string(),
                value.trim().to_string(),
            ]
        } else {
            tokenize_clause(input)?
        }
    } else {
        tokenize_clause(input)?
    };
    if tokens.len() < 3 {
        return Err(ContactSearchValidationError::InvalidSyntax(
            "erwartet wird: feld operator wert".to_string(),
        ));
    }
    let field = parse_field(&tokens[0])?;
    let operator = parse_operator(&tokens[1])?;
    let value = tokens[2..].join(" ");
    Ok(ContactSearchClause::new(field, operator, value))
}

fn tokenize_clause(input: &str) -> Result<Vec<String>, ContactSearchValidationError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        match quote {
            Some(quote_character) if character == quote_character => quote = None,
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if escaped || quote.is_some() {
        return Err(ContactSearchValidationError::InvalidSyntax(
            "Anführungszeichen oder Escape nicht abgeschlossen".to_string(),
        ));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn parse_field(input: &str) -> Result<ContactSearchField, ContactSearchValidationError> {
    match input.trim().to_ascii_lowercase().as_str() {
        "name" | "fn" | "full-name" => Ok(ContactSearchField::Name),
        "email" | "mail" => Ok(ContactSearchField::Email),
        "phone" | "tel" => Ok(ContactSearchField::Phone),
        "category" | "categories" => Ok(ContactSearchField::Category),
        "any" | "any-field" | "any_field" => Ok(ContactSearchField::AnyField),
        _ => Err(ContactSearchValidationError::UnsupportedField(
            input.to_string(),
        )),
    }
}

fn parse_operator(input: &str) -> Result<ContactSearchOperator, ContactSearchValidationError> {
    match input.trim().to_ascii_lowercase().as_str() {
        "contains" => Ok(ContactSearchOperator::Contains),
        "not-contains" | "not_contains" | "does-not-contain" => {
            Ok(ContactSearchOperator::NotContains)
        }
        "is" | "equals" => Ok(ContactSearchOperator::Is),
        "is-not" | "is_not" | "not" => Ok(ContactSearchOperator::IsNot),
        "starts" | "starts-with" | "starts_with" => Ok(ContactSearchOperator::StartsWith),
        "ends" | "ends-with" | "ends_with" => Ok(ContactSearchOperator::EndsWith),
        _ => Err(ContactSearchValidationError::UnsupportedOperatorText(
            input.to_string(),
        )),
    }
}

/// Public service facade for callers that prefer a named service over the
/// module-level pure functions.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContactSearchService;

impl ContactSearchService {
    pub fn evaluate(
        query: &ContactSearchQuery,
        snapshots: &[ContactSearchSnapshot],
    ) -> Result<Vec<ContactIdentity>, ContactSearchValidationError> {
        evaluate_contacts(query, snapshots)
    }

    pub fn evaluate_cancelable(
        query: &ContactSearchQuery,
        snapshots: &[ContactSearchSnapshot],
        cancellation: &AtomicBool,
    ) -> Result<Vec<ContactIdentity>, ContactSearchRunError> {
        evaluate_contacts_cancelable(query, snapshots, cancellation)
    }

    pub fn parse(input: &str) -> Result<ContactSearchQuery, ContactSearchValidationError> {
        parse_query_text(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn vcard(version: &str, body: &str) -> String {
        format!("BEGIN:VCARD\r\nVERSION:{version}\r\nFN:Test Contact\r\n{body}\r\nEND:VCARD\r\n")
    }

    fn snapshot(source: &str, uid: &str, body: &str) -> ContactSearchSnapshot {
        ContactSearchSnapshot::from_vcard(source, uid, "Private book", &vcard("4.0", body))
            .expect("fixture vCard should parse")
    }

    #[test]
    fn registry_exactly_matches_the_supported_field_operator_matrix() {
        assert_eq!(
            operators_for_field(ContactSearchField::Name),
            TEXT_OPERATORS
        );
        assert_eq!(
            operators_for_field(ContactSearchField::Email),
            TEXT_OPERATORS
        );
        assert_eq!(
            operators_for_field(ContactSearchField::Phone),
            TEXT_OPERATORS
        );
        assert_eq!(
            operators_for_field(ContactSearchField::Category),
            CATEGORY_OPERATORS
        );
        assert_eq!(
            operators_for_field(ContactSearchField::AnyField),
            ANY_FIELD_OPERATORS
        );
        assert_eq!(field_operator_registry().len(), 5);
    }

    #[test]
    fn positive_negative_and_multiword_matching_are_unicode_case_insensitive() {
        let contact = snapshot(
            "book-a",
            "ada-1",
            "FN:Ada Lovelace\r\nEMAIL:ada@example.test\r\nEMAIL:countess@example.test\r\nTEL:+49 30 123\r\nCATEGORIES:Research,Élite",
        );
        let contacts = [contact];
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::Name, "ADA LOVE"),
                &contacts
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::new(
                    ContactClauseGroup::All,
                    vec![ContactSearchClause::new(
                        ContactSearchField::Email,
                        ContactSearchOperator::Is,
                        "COUNTESS@EXAMPLE.TEST",
                    )],
                ),
                &contacts
            )
            .unwrap()
            .len(),
            1
        );
        assert!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::Name, "Grace Hopper"),
                &contacts
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            evaluate_contacts(
                &ContactSearchQuery::new(
                    ContactClauseGroup::All,
                    vec![ContactSearchClause::new(
                        ContactSearchField::Email,
                        ContactSearchOperator::NotContains,
                        "example.test",
                    )],
                ),
                &contacts
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::Category, "élite"),
                &contacts
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            field_values(&contacts[0].contact, ContactSearchField::Email).len(),
            2
        );
    }

    #[test]
    fn every_operator_has_explicit_positive_and_negative_semantics() {
        let contact = snapshot(
            "book-a",
            "one",
            "FN:Ada Lovelace\r\nEMAIL:ada@example.test\r\nTEL:+49 30 123",
        );
        let contacts = [contact];
        let cases = [
            (
                ContactSearchField::Name,
                ContactSearchOperator::Contains,
                "Ada",
                true,
            ),
            (
                ContactSearchField::Name,
                ContactSearchOperator::NotContains,
                "Grace",
                true,
            ),
            (
                ContactSearchField::Name,
                ContactSearchOperator::Is,
                "ada lovelace",
                true,
            ),
            (
                ContactSearchField::Name,
                ContactSearchOperator::IsNot,
                "Grace Hopper",
                true,
            ),
            (
                ContactSearchField::Name,
                ContactSearchOperator::StartsWith,
                "Ada",
                true,
            ),
            (
                ContactSearchField::Name,
                ContactSearchOperator::EndsWith,
                "Lovelace",
                true,
            ),
            (
                ContactSearchField::Email,
                ContactSearchOperator::Contains,
                "example",
                true,
            ),
            (
                ContactSearchField::Email,
                ContactSearchOperator::NotContains,
                "other",
                true,
            ),
            (
                ContactSearchField::Email,
                ContactSearchOperator::Is,
                "ADA@EXAMPLE.TEST",
                true,
            ),
            (
                ContactSearchField::Email,
                ContactSearchOperator::IsNot,
                "other@test",
                true,
            ),
            (
                ContactSearchField::Email,
                ContactSearchOperator::StartsWith,
                "ada@",
                true,
            ),
            (
                ContactSearchField::Email,
                ContactSearchOperator::EndsWith,
                ".test",
                true,
            ),
            (
                ContactSearchField::Phone,
                ContactSearchOperator::Contains,
                "30 12",
                true,
            ),
            (
                ContactSearchField::Phone,
                ContactSearchOperator::StartsWith,
                "+49",
                true,
            ),
            (
                ContactSearchField::Phone,
                ContactSearchOperator::EndsWith,
                "123",
                true,
            ),
        ];
        for (field, operator, value, expected) in cases {
            let query = ContactSearchQuery::new(
                ContactClauseGroup::All,
                vec![ContactSearchClause::new(field, operator, value)],
            );
            assert_eq!(
                evaluate_contacts(&query, &contacts).unwrap().len() == 1,
                expected,
                "{field:?} {operator:?} {value:?}"
            );
        }
    }

    #[test]
    fn name_operators_use_only_the_canonical_full_name() {
        let mut contact = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        contact.name.family = "Lovelace".to_string();
        contact.name.given = "Ada".to_string();
        let snapshot = ContactSearchSnapshot::new("book", "ada", contact);

        let cases = [
            (ContactSearchOperator::Contains, "Ada", true),
            (ContactSearchOperator::NotContains, "Grace", true),
            (ContactSearchOperator::Is, "Ada Lovelace", true),
            (ContactSearchOperator::IsNot, "Grace Hopper", true),
            (ContactSearchOperator::StartsWith, "Ada", true),
            (ContactSearchOperator::EndsWith, "Lovelace", true),
        ];
        for (operator, value, expected) in cases {
            let query = ContactSearchQuery::new(
                ContactClauseGroup::All,
                vec![ContactSearchClause::new(
                    ContactSearchField::Name,
                    operator,
                    value,
                )],
            );
            assert_eq!(
                evaluate_contacts(&query, std::slice::from_ref(&snapshot))
                    .unwrap()
                    .len()
                    == 1,
                expected,
                "{operator:?} {value:?}"
            );
        }

        for (operator, value) in [
            (ContactSearchOperator::Is, "Ada"),
            (ContactSearchOperator::StartsWith, "Lovelace"),
            (ContactSearchOperator::EndsWith, "Ada"),
        ] {
            let query = ContactSearchQuery::new(
                ContactClauseGroup::All,
                vec![ContactSearchClause::new(
                    ContactSearchField::Name,
                    operator,
                    value,
                )],
            );
            assert!(
                evaluate_contacts(&query, std::slice::from_ref(&snapshot))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn any_field_matches_tokens_across_visible_fields_and_unicode_forms() {
        let mut contact = VCardContact::new("Visible Name", "visible@example.test", "", "");
        contact.org = "Acme Forschung".to_string();
        contact.title = "Leitung".to_string();
        contact.note = "Projekt Aurora".to_string();
        contact.addresses = vec!["München, Deutschland".to_string()];
        contact.urls = vec!["https://example.test/aurora".to_string()];
        contact.nickname = "Ada".to_string();
        contact.birthday = "1815-12-10".to_string();
        let snapshot = ContactSearchSnapshot::new("book", "visible", contact);

        for value in [
            "Acme Leitung",
            "Forschung Aurora",
            "Deutschland example.test",
            "ada 1815-12-10",
            "MU\u{0308}NCHEN DEUTSCHLAND",
        ] {
            let query = ContactSearchQuery::global_search(value);
            assert_eq!(
                evaluate_contacts(&query, std::slice::from_ref(&snapshot))
                    .unwrap()
                    .len(),
                1,
                "AnyField should match {value:?}"
            );
        }
        assert!(
            evaluate_contacts(
                &ContactSearchQuery::global_search("Leitung missing"),
                std::slice::from_ref(&snapshot),
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn all_any_and_outer_filters_compose_without_becoming_clause_logic() {
        let first = snapshot(
            "book-a",
            "first",
            "FN:Ada Lovelace\r\nEMAIL:ada@example.test\r\nCATEGORIES:Work",
        );
        let second = snapshot(
            "book-b",
            "second",
            "FN:Grace Hopper\r\nEMAIL:grace@example.test\r\nCATEGORIES:Work",
        );
        let third = snapshot(
            "book-a",
            "third",
            "FN:No Category\r\nEMAIL:shared@example.test",
        );
        let contacts = [first, second, third];
        let query = ContactSearchQuery::new(
            ContactClauseGroup::All,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Contains,
                    "a",
                ),
                ContactSearchClause::new(
                    ContactSearchField::Email,
                    ContactSearchOperator::Contains,
                    "example.test",
                ),
            ],
        )
        .with_filters(
            ContactOuterFilters::default()
                .source("book-a")
                .category("work"),
        );
        assert_eq!(
            evaluate_contacts(&query, &contacts).unwrap(),
            vec![ContactIdentity::new("book-a", "first")]
        );

        let any_query = ContactSearchQuery::new(
            ContactClauseGroup::Any,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Is,
                    "Grace Hopper",
                ),
                ContactSearchClause::new(
                    ContactSearchField::Email,
                    ContactSearchOperator::Is,
                    "not-present@example.test",
                ),
            ],
        );
        assert_eq!(
            evaluate_contacts(&any_query, &contacts).unwrap(),
            vec![ContactIdentity::new("book-b", "second")]
        );
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::all()
                    .with_filters(ContactOuterFilters::default().unmatched_category(true)),
                &contacts,
            )
            .unwrap(),
            vec![ContactIdentity::new("book-a", "third")]
        );
    }

    #[test]
    fn duplicate_outer_filter_uses_logical_contact_keys_and_stable_identity() {
        let first = snapshot(
            "book-a",
            "same",
            "FN:Same Person\r\nEMAIL:duplicate@example.test",
        );
        let second = snapshot(
            "book-b",
            "same",
            "FN:Other Card\r\nEMAIL:DUPLICATE@example.test",
        );
        let unique = snapshot("book-a", "unique", "FN:Unique\r\nEMAIL:unique@example.test");
        let query = ContactSearchQuery::all()
            .with_filters(ContactOuterFilters::default().duplicate_only(true));
        assert_eq!(
            evaluate_contacts(&query, &[first, second, unique]).unwrap(),
            vec![
                ContactIdentity::new("book-a", "same"),
                ContactIdentity::new("book-b", "same")
            ]
        );
    }

    #[test]
    fn re_evaluation_after_merge_or_delete_recomputes_duplicate_and_structured_filters() {
        let first = snapshot("book", "first", "FN:Ada Lovelace\r\nEMAIL:ada@example.test");
        let second = snapshot("book", "second", "FN:Ada Byron\r\nEMAIL:ADA@example.test");
        let merged = snapshot("book", "first", "FN:Ada Lovelace\r\nEMAIL:ada@example.test");
        let query = ContactSearchQuery::new(
            ContactClauseGroup::All,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Contains,
                    "Ada",
                ),
                ContactSearchClause::new(
                    ContactSearchField::Email,
                    ContactSearchOperator::EndsWith,
                    "example.test",
                ),
            ],
        )
        .with_filters(ContactOuterFilters::default().duplicate_only(true));

        assert_eq!(
            evaluate_contacts(&query, &[first.clone(), second.clone()]).unwrap(),
            vec![first.identity.clone(), second.identity.clone()]
        );
        assert!(evaluate_contacts(&query, &[merged]).unwrap().is_empty());
        assert!(evaluate_contacts(&query, &[second]).unwrap().is_empty());
    }

    #[test]
    fn every_outer_filter_composes_with_an_advanced_clause_set() {
        let first = snapshot(
            "book-a",
            "first",
            "FN:Ada Lovelace\r\nEMAIL:shared@example.test\r\nCATEGORIES:Work",
        );
        let second = snapshot(
            "book-b",
            "second",
            "FN:Ada Byron\r\nEMAIL:shared@example.test\r\nCATEGORIES:Work",
        );
        let third = snapshot(
            "book-a",
            "third",
            "FN:Ada NoCategory\r\nEMAIL:shared@example.test",
        );
        let contacts = [first, second, third];
        let advanced = ContactSearchQuery::new(
            ContactClauseGroup::All,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Contains,
                    "Ada",
                ),
                ContactSearchClause::new(
                    ContactSearchField::Email,
                    ContactSearchOperator::Contains,
                    "example.test",
                ),
            ],
        );

        assert_eq!(
            evaluate_contacts(
                &advanced
                    .clone()
                    .with_filters(ContactOuterFilters::default().source("book-a")),
                &contacts,
            )
            .unwrap(),
            vec![
                ContactIdentity::new("book-a", "first"),
                ContactIdentity::new("book-a", "third"),
            ]
        );
        assert_eq!(
            evaluate_contacts(
                &advanced
                    .clone()
                    .with_filters(ContactOuterFilters::default().category("work")),
                &contacts,
            )
            .unwrap(),
            vec![
                ContactIdentity::new("book-a", "first"),
                ContactIdentity::new("book-b", "second"),
            ]
        );
        assert_eq!(
            evaluate_contacts(
                &advanced
                    .clone()
                    .with_filters(ContactOuterFilters::default().unmatched_category(true)),
                &contacts,
            )
            .unwrap(),
            vec![ContactIdentity::new("book-a", "third")]
        );
        assert_eq!(
            evaluate_contacts(
                &advanced
                    .clone()
                    .with_filters(ContactOuterFilters::default().duplicate_only(true)),
                &contacts,
            )
            .unwrap(),
            vec![
                ContactIdentity::new("book-a", "first"),
                ContactIdentity::new("book-b", "second"),
                ContactIdentity::new("book-a", "third"),
            ]
        );
        assert_eq!(
            evaluate_contacts(
                &advanced.with_filters(
                    ContactOuterFilters::default()
                        .source("book-a")
                        .category("work")
                        .duplicate_only(true),
                ),
                &contacts,
            )
            .unwrap(),
            vec![ContactIdentity::new("book-a", "first")]
        );
    }

    #[test]
    fn vcard3_vcard4_and_lists_search_visible_repeated_fields() {
        let v3 = ContactSearchSnapshot::from_vcard(
            "v3",
            "legacy",
            "Legacy",
            &vcard(
                "3.0",
                "FN:Legacy Team\r\nEMAIL;X-EVOLUTION-DEST-CONTACT-UID=member-1:member@example.test\r\nX-EVOLUTION-LIST:TRUE\r\nX-EVOLUTION-LIST-NAME:Legacy Team",
            ),
        )
        .unwrap();
        let v4 = ContactSearchSnapshot::from_vcard(
            "v4",
            "modern",
            "Modern",
            &vcard(
                "4.0",
                "KIND:group\r\nFN:Modern Group\r\nMEMBER:mailto:member4@example.test\r\nCATEGORIES:Teams",
            ),
        )
        .unwrap();
        let contacts = [v3, v4];
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::Name, "legacy team"),
                &contacts
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::Email, "member4"),
                &contacts
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::Category, "teams"),
                &contacts
            )
            .unwrap()
            .len(),
            1
        );

        let mut mixed_list = VCardContact::new_list("Mixed members", true, Some("mixed-list"));
        mixed_list.list_members = vec![
            super::super::vcard_service::VCardListMember::local_contact(
                "local-member",
                "book",
                "Local Ada",
                "local@example.test",
                Some(0),
            ),
            super::super::vcard_service::VCardListMember::external_address(
                "external@example.test",
                "External Grace",
            ),
            super::super::vcard_service::VCardListMember::nested_list(
                "nested-list",
                "Nested Team",
                None,
            ),
            super::super::vcard_service::VCardListMember {
                kind: VCardListMemberKind::Invalid,
                display_name: "Broken member".to_string(),
                email: String::new(),
                contact_uid: String::new(),
                source_uid: String::new(),
                email_num: None,
                parent_id: None,
                property: "MEMBER".to_string(),
                value: "mailto:not-an-address".to_string(),
                raw_value: "mailto:not-an-address".to_string(),
            },
        ];
        let mixed = ContactSearchSnapshot::new("v4", "mixed-list", mixed_list);
        for value in [
            "Local Ada",
            "local@example.test",
            "External Grace",
            "external@example.test",
            "Nested Team",
            "not-an-address",
        ] {
            assert_eq!(
                evaluate_contacts(
                    &ContactSearchQuery::global_search(value),
                    std::slice::from_ref(&mixed),
                )
                .unwrap()
                .len(),
                1,
                "list member value should be searchable: {value}"
            );
        }
    }

    #[test]
    fn any_field_never_leaks_source_uid_uid_or_vcard_metadata() {
        let contact = snapshot(
            "secret-source",
            "secret-uid",
            "FN:Visible Name\r\nEMAIL:visible@example.test\r\nX-EVOLUTION-PRIVATE:secret-extension",
        );
        assert!(
            evaluate_contacts(
                &ContactSearchQuery::contains(ContactSearchField::AnyField, "Visible Name"),
                std::slice::from_ref(&contact)
            )
            .is_ok_and(|results| results.len() == 1)
        );
        for needle in [
            "secret-source",
            "secret-uid",
            "secret-extension",
            "X-EVOLUTION",
        ] {
            assert!(
                evaluate_contacts(
                    &ContactSearchQuery::contains(ContactSearchField::AnyField, needle),
                    std::slice::from_ref(&contact)
                )
                .unwrap()
                .is_empty(),
                "metadata leaked for {needle}"
            );
        }
    }

    #[test]
    fn invalid_field_operator_pairs_fail_closed() {
        let query = ContactSearchQuery::new(
            ContactClauseGroup::All,
            vec![ContactSearchClause::new(
                ContactSearchField::Category,
                ContactSearchOperator::StartsWith,
                "work",
            )],
        );
        let error = query.validate().unwrap_err();
        assert!(matches!(
            error,
            ContactSearchValidationError::UnsupportedOperator { .. }
        ));
        assert!(error.is_unsupported());
    }

    #[test]
    fn fixture_codec_is_versioned_and_rejects_invalid_or_unknown_data() {
        let query = ContactSearchQuery::new(
            ContactClauseGroup::Any,
            vec![ContactSearchClause::new(
                ContactSearchField::Email,
                ContactSearchOperator::EndsWith,
                "example.test",
            )],
        )
        .with_filters(
            ContactOuterFilters::default()
                .source("book-a")
                .category("Work")
                .duplicate_only(true),
        );
        let fixture = query.encode_fixture().unwrap();
        assert!(fixture.contains("\"version\": 1"));
        assert_eq!(ContactSearchQuery::decode_fixture(&fixture).unwrap(), query);
        assert!(matches!(
            ContactSearchQuery::decode_fixture(
                &fixture.replace("\"version\": 1", "\"version\": 2")
            ),
            Err(ContactSearchValidationError::UnsupportedFixtureVersion(2))
        ));
        assert!(matches!(
            ContactSearchQuery::decode_fixture(
                r#"{
                    "format": "meridian-contact-search",
                    "version": 7,
                    "future_payload": {"new": ["shape"]}
                }"#
            ),
            Err(ContactSearchValidationError::UnsupportedFixtureVersion(7))
        ));
        assert!(matches!(
            ContactSearchQuery::decode_fixture(&fixture.replace(
                "\"format\": \"meridian-contact-search\"",
                "\"format\": \"other\""
            )),
            Err(ContactSearchValidationError::InvalidFixture(_))
        ));
        let invalid = fixture.replace("\"email\"", "\"category\"");
        assert!(matches!(
            ContactSearchQuery::decode_fixture(&invalid),
            Err(ContactSearchValidationError::UnsupportedOperator { .. })
        ));
        assert!(
            ContactSearchQuery::decode_fixture(
                &fixture.replace("\"filters\": {", "\"unknown\": true, \"filters\": {")
            )
            .is_err()
        );

        assert!(matches!(
            ContactSearchQuery::any().validate(),
            Err(ContactSearchValidationError::EmptyAnyGroup)
        ));
    }

    #[test]
    fn typed_text_parser_accepts_only_the_portable_contract() {
        let query =
            parse_query_text("any: name contains \"Ada Lovelace\" OR email is ada@example.test")
                .unwrap();
        assert_eq!(query.group, ContactClauseGroup::Any);
        assert_eq!(query.clauses.len(), 2);
        assert!(
            parse_query_text("(and (contains name Ada))").is_err_and(|error| {
                matches!(error, ContactSearchValidationError::UnsupportedRawSyntax(_))
            })
        );
        assert!(
            parse_query_text("GtkExpression(name contains Ada)").is_err_and(|error| {
                matches!(error, ContactSearchValidationError::UnsupportedRawSyntax(_))
            })
        );
        assert!(
            parse_query_text("category starts-with work").is_err_and(|error| {
                matches!(
                    error,
                    ContactSearchValidationError::UnsupportedOperator { .. }
                )
            })
        );
        assert!(
            parse_query_text("field-that-does-not-exist contains value").is_err_and(|error| {
                matches!(error, ContactSearchValidationError::UnsupportedField(_))
            })
        );
    }

    #[test]
    fn global_mapping_is_explicit_any_field_contains() {
        let query = ContactSearchQuery::global_search("Ada Lovelace");
        assert_eq!(query.group, ContactClauseGroup::All);
        assert_eq!(query.clauses.len(), 1);
        assert_eq!(query.clauses[0].field, ContactSearchField::AnyField);
        assert_eq!(query.clauses[0].operator, ContactSearchOperator::Contains);
        assert!(ContactSearchQuery::global_search("  ").clauses.is_empty());
    }

    #[test]
    fn rejected_query_preserves_the_last_valid_result() {
        let contact = snapshot("book", "one", "FN:Ada\r\nEMAIL:ada@example.test");
        let mut session = LastValidContactSearch::default();
        let valid = ContactSearchQuery::contains(ContactSearchField::Name, "Ada");
        assert_eq!(
            session
                .try_apply(&valid, std::slice::from_ref(&contact))
                .unwrap()
                .len(),
            1
        );
        let invalid = ContactSearchQuery::new(
            ContactClauseGroup::All,
            vec![ContactSearchClause::new(
                ContactSearchField::Category,
                ContactSearchOperator::StartsWith,
                "x",
            )],
        );
        assert!(
            session
                .try_apply(&invalid, std::slice::from_ref(&contact))
                .is_err()
        );
        assert_eq!(session.last_valid_results().len(), 1);
        assert_eq!(session.last_valid_query(), &valid);
    }

    #[test]
    fn latest_generation_cancels_stale_requests_and_removed_ids_are_not_reintroduced() {
        let coordinator = LatestGenerationEvaluator::default();
        let first_request = coordinator.begin();
        let second_request = coordinator.begin();
        let contact = snapshot("book", "one", "FN:One\r\nEMAIL:one@example.test");
        let stale = coordinator
            .evaluate(
                &first_request,
                &ContactSearchQuery::all(),
                std::slice::from_ref(&contact),
            )
            .unwrap();
        assert_eq!(stale, ContactSearchEvaluation::Cancelled { generation: 1 });
        let current = coordinator
            .evaluate(
                &second_request,
                &ContactSearchQuery::all(),
                std::slice::from_ref(&contact),
            )
            .unwrap();
        assert_eq!(current.generation(), second_request.generation());
        assert_eq!(
            current.identities().unwrap(),
            &[ContactIdentity::new("book", "one")]
        );

        let removed = coordinator.begin();
        let no_contacts = coordinator
            .evaluate(&removed, &ContactSearchQuery::all(), &[])
            .unwrap();
        assert_eq!(no_contacts.identities().unwrap(), &[]);
    }

    #[test]
    fn latest_worker_cancels_in_flight_request_and_delivers_only_latest_result() {
        let gates = Arc::new(ContactSearchTestGates::default());
        let coordinator = LatestGenerationEvaluator::with_test_gates(Arc::clone(&gates));
        let worker = LatestContactSearchWorker::default();
        let snapshots = (0..64)
            .map(|index| {
                let email = match index {
                    0 => "first@example.test".to_string(),
                    1 => "second@example.test".to_string(),
                    _ => format!("other{index}@example.test"),
                };
                snapshot("book", &index.to_string(), &format!("EMAIL:{email}"))
            })
            .collect::<Vec<_>>();

        let first_request = coordinator.begin();
        let first_receiver = worker.submit(
            coordinator.clone(),
            first_request.clone(),
            ContactSearchQuery::contains(ContactSearchField::Email, "first@example.test"),
            snapshots.clone(),
        );
        gates.wait_until_mid_evaluation();

        let second_request = coordinator.begin();
        assert!(first_request.cancellation().load(Ordering::Acquire));
        assert!(!first_request.is_current());
        let second_receiver = worker.submit(
            coordinator.clone(),
            second_request.clone(),
            ContactSearchQuery::contains(ContactSearchField::Email, "second@example.test"),
            snapshots,
        );
        gates.release_evaluation();

        assert_eq!(
            first_receiver
                .recv_blocking()
                .expect("stale worker result should be delivered")
                .expect("stale request should be evaluated, then canceled"),
            ContactSearchEvaluation::Cancelled {
                generation: first_request.generation(),
            }
        );
        assert_eq!(
            second_receiver
                .recv_blocking()
                .expect("latest worker result should be delivered")
                .expect("latest request should succeed"),
            ContactSearchEvaluation::Results {
                generation: second_request.generation(),
                identities: vec![ContactIdentity::new("book", "1")],
            }
        );
    }

    #[test]
    fn cancellation_is_checked_before_and_during_evaluation() {
        let cancellation = AtomicBool::new(true);
        let contact = snapshot("book", "one", "FN:One");
        assert_eq!(
            evaluate_contacts_cancelable(
                &ContactSearchQuery::all(),
                std::slice::from_ref(&contact),
                &cancellation,
            ),
            Err(ContactSearchRunError::Cancelled)
        );
        let cancellation = AtomicBool::new(false);
        let contacts = (0..128)
            .map(|index| snapshot("book", &format!("{index}"), &format!("FN:Name {index}")))
            .collect::<Vec<_>>();
        cancellation.store(true, Ordering::Relaxed);
        assert_eq!(
            evaluate_contacts_cancelable(&ContactSearchQuery::all(), &contacts, &cancellation),
            Err(ContactSearchRunError::Cancelled)
        );
    }

    #[test]
    fn identity_keeps_same_uid_from_two_sources_distinct() {
        let left = snapshot("left", "same-uid", "FN:Left");
        let right = snapshot("right", "same-uid", "FN:Right");
        let results = evaluate_contacts(&ContactSearchQuery::all(), &[left, right]).unwrap();
        assert_eq!(results.len(), 2);
        assert_ne!(results[0], results[1]);
        assert_eq!(results[0].uid, results[1].uid);
        assert_ne!(results[0].source_uid, results[1].source_uid);
    }
}
