// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK-free filtering contract for the memo surface.
//
// The filter deliberately works on the typed VJOURNAL fields instead of a
// serialized component blob.  That keeps Summary and Description separate,
// makes source matching exact, and gives migration/UI code one validation
// boundary for the persisted values.

use super::calendar_store_service::MemoRecord;

pub const ANY_CATEGORY_SETTING: &str = "any";
pub const WITHOUT_CATEGORY_SETTING: &str = "without";
pub const MAX_MEMO_CATEGORY_FILTER_BYTES: usize = 128;
pub const MAX_MEMO_ADVANCED_QUERY_BYTES: usize = 1_024;
pub const MAX_MEMO_SOURCE_FILTER_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoSearchMode {
    Summary,
    Description,
    #[default]
    Any,
    Advanced,
}

impl MemoSearchMode {
    pub fn setting_value(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Description => "description",
            Self::Any => "any-field",
            Self::Advanced => "advanced",
        }
    }

    pub fn parse_setting(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "summary" => Ok(Self::Summary),
            "description" => Ok(Self::Description),
            "any" | "any-field" => Ok(Self::Any),
            "advanced" => Ok(Self::Advanced),
            _ => Err(
                "Ungültiger Memo-Suchmodus; erwartet summary, description, any-field oder advanced"
                    .to_string(),
            ),
        }
    }

    pub fn from_setting(value: &str) -> Self {
        Self::parse_setting(value).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MemoCategoryFilter {
    #[default]
    Any,
    Without,
    Named(String),
}

impl MemoCategoryFilter {
    pub fn setting_value(&self) -> String {
        match self {
            Self::Any => ANY_CATEGORY_SETTING.to_string(),
            Self::Without => WITHOUT_CATEGORY_SETTING.to_string(),
            Self::Named(value) => value.clone(),
        }
    }

    pub fn parse_setting(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case(ANY_CATEGORY_SETTING) {
            return Ok(Self::Any);
        }
        if value.eq_ignore_ascii_case(WITHOUT_CATEGORY_SETTING)
            || value.eq_ignore_ascii_case("without-category")
        {
            return Ok(Self::Without);
        }
        validate_category_name(value).map(Self::Named)
    }

    pub fn from_setting(value: &str) -> Self {
        Self::parse_setting(value).unwrap_or_default()
    }

    pub fn matches(&self, categories: &[String]) -> bool {
        match self {
            Self::Any => true,
            Self::Without => categories.is_empty(),
            Self::Named(expected) => categories
                .iter()
                .any(|category| category.eq_ignore_ascii_case(expected)),
        }
    }
}

pub fn validate_category_name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Eine benannte Kategorie darf nicht leer sein".to_string());
    }
    if value.len() > MAX_MEMO_CATEGORY_FILTER_BYTES {
        return Err(format!(
            "Eine Memo-Kategorie darf höchstens {MAX_MEMO_CATEGORY_FILTER_BYTES} Bytes lang sein"
        ));
    }
    if value.eq_ignore_ascii_case(ANY_CATEGORY_SETTING)
        || value.eq_ignore_ascii_case(WITHOUT_CATEGORY_SETTING)
        || value.eq_ignore_ascii_case("without-category")
    {
        return Err("Reservierter Memo-Kategoriefilter".to_string());
    }
    if value
        .chars()
        .any(|character| character.is_control() || character == '\'' || character == '"')
    {
        return Err(
            "Memo-Kategorien dürfen keine Steuerzeichen oder Anführungszeichen enthalten"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

/// Validate a persisted source UID without trying to decide whether the
/// source is currently available.  Availability is a live EDS concern; the
/// setting must still be safe and unambiguous when it is restored.
pub fn validate_source_filter(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.len() > MAX_MEMO_SOURCE_FILTER_BYTES {
        return Err(format!(
            "Eine Memo-Quelle darf höchstens {MAX_MEMO_SOURCE_FILTER_BYTES} Bytes lang sein"
        ));
    }
    if value.chars().any(|character| {
        character.is_control() || character.is_whitespace() || matches!(character, '\'' | '"')
    }) {
        return Err(
            "Memo-Quellen dürfen keine Leer-, Steuerzeichen oder Anführungszeichen enthalten"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoAdvancedField {
    Summary,
    Description,
    Category,
    Source,
    Url,
}

impl MemoAdvancedField {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "summary" => Some(Self::Summary),
            "description" => Some(Self::Description),
            "category" => Some(Self::Category),
            "source" => Some(Self::Source),
            "url" => Some(Self::Url),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoAdvancedTerm {
    pub field: MemoAdvancedField,
    pub value: String,
}

/// Parse the deliberately small portable advanced-search grammar:
/// `field:value AND field:value`.
///
/// Values are single tokens.  This is intentional: accepting quoting or an
/// implicit operator would make a malformed imported setting look valid and
/// would make the result differ between the GTK and migration paths.
pub fn parse_advanced_query(value: &str) -> Result<Vec<MemoAdvancedTerm>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Die erweiterte Memo-Suche darf nicht leer sein".to_string());
    }
    if value.len() > MAX_MEMO_ADVANCED_QUERY_BYTES {
        return Err(format!(
            "Die erweiterte Memo-Suche darf höchstens {MAX_MEMO_ADVANCED_QUERY_BYTES} Bytes lang sein"
        ));
    }
    if value
        .chars()
        .any(|character| character == '\'' || character == '"' || character.is_control())
    {
        return Err(
            "Die erweiterte Memo-Suche erlaubt keine Anführungszeichen oder Steuerzeichen"
                .to_string(),
        );
    }

    let tokens = value.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() || tokens.len() % 2 == 0 {
        return Err(
            "Erwartet werden ein oder mehrere Feld:Wert-Paare mit AND dazwischen".to_string(),
        );
    }

    let mut terms = Vec::with_capacity((tokens.len() + 1) / 2);
    for (index, token) in tokens.iter().enumerate() {
        if index % 2 == 1 {
            if *token != "AND" {
                return Err("Die erweiterte Memo-Suche verwendet ausschließlich AND".to_string());
            }
            continue;
        }

        let Some((field, raw_term)) = token.split_once(':') else {
            return Err(format!("Ungültiges erweitertes Memo-Feld „{token}“"));
        };
        let Some(field) = MemoAdvancedField::parse(field) else {
            return Err(format!("Unbekanntes erweitertes Memo-Feld „{field}“"));
        };
        let raw_term = raw_term.trim();
        if raw_term.is_empty() {
            return Err("Ein erweitertes Memo-Feld darf keinen leeren Wert haben".to_string());
        }
        if raw_term.chars().any(char::is_control) {
            return Err("Ein erweiterter Memo-Wert darf keine Steuerzeichen enthalten".to_string());
        }
        terms.push(MemoAdvancedTerm {
            field,
            value: raw_term.to_string(),
        });
    }

    Ok(terms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoFields {
    pub summary: String,
    pub description: String,
    pub categories: Vec<String>,
    pub url: Option<String>,
}

impl MemoFields {
    fn from_component(component: &str) -> Self {
        let mut summary = String::new();
        let mut description = String::new();
        let mut categories = Vec::new();
        let mut url = None;
        let lines = unfold_lines(component);
        let mut depth = 0usize;
        let mut vjournal_depth = None;

        for line in lines {
            let upper = line.to_ascii_uppercase();
            if upper.starts_with("BEGIN:") {
                depth += 1;
                if upper == "BEGIN:VJOURNAL" && vjournal_depth.is_none() {
                    vjournal_depth = Some(depth);
                }
                continue;
            }
            if upper.starts_with("END:") {
                depth = depth.saturating_sub(1);
                continue;
            }
            // Search only the top-level VJOURNAL properties.  A VALARM or
            // another nested component may legally contain similarly named
            // properties, but those values are not memo fields.
            if vjournal_depth.is_some_and(|vjournal_depth| depth != vjournal_depth)
                || (vjournal_depth.is_none() && depth != 0)
            {
                continue;
            }
            let Some((property, value)) = line.split_once(':') else {
                continue;
            };
            let name = property.split(';').next().unwrap_or(property);
            if name.eq_ignore_ascii_case("SUMMARY") && summary.is_empty() {
                summary = unescape_ical_text(value);
            } else if name.eq_ignore_ascii_case("DESCRIPTION") && description.is_empty() {
                description = unescape_ical_text(value);
            } else if name.eq_ignore_ascii_case("CATEGORIES") {
                categories.extend(split_ical_list(value));
            } else if name.eq_ignore_ascii_case("URL") && url.is_none() {
                url = Some(unescape_ical_text(value));
            }
        }

        Self {
            summary,
            description,
            categories,
            url,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoFilter {
    pub search_mode: MemoSearchMode,
    pub query: String,
    pub category: MemoCategoryFilter,
    pub source_uid: Option<String>,
    advanced_terms: Vec<MemoAdvancedTerm>,
}

impl Default for MemoFilter {
    fn default() -> Self {
        Self {
            search_mode: MemoSearchMode::Any,
            query: String::new(),
            category: MemoCategoryFilter::Any,
            source_uid: None,
            advanced_terms: Vec::new(),
        }
    }
}

impl MemoFilter {
    pub fn new(
        search_mode: MemoSearchMode,
        query: impl Into<String>,
        category: MemoCategoryFilter,
        source_uid: Option<String>,
    ) -> Result<Self, String> {
        let query = query.into();
        let category = match category {
            MemoCategoryFilter::Any => MemoCategoryFilter::Any,
            MemoCategoryFilter::Without => MemoCategoryFilter::Without,
            MemoCategoryFilter::Named(value) => {
                MemoCategoryFilter::Named(validate_category_name(&value)?)
            }
        };
        let source_uid = source_uid
            .map(|value| validate_source_filter(&value))
            .transpose()?
            .and_then(|value| (!value.is_empty()).then_some(value));
        let advanced_terms = if search_mode == MemoSearchMode::Advanced {
            parse_advanced_query(&query)?
        } else {
            Vec::new()
        };
        Ok(Self {
            search_mode,
            query,
            category,
            source_uid,
            advanced_terms,
        })
    }

    pub fn matches(&self, memo: &MemoRecord) -> bool {
        if self
            .source_uid
            .as_deref()
            .is_some_and(|source_uid| memo.source_uid != source_uid)
        {
            return false;
        }

        let fields = MemoFields::from_component(&memo.component);
        if !self.category.matches(&fields.categories) {
            return false;
        }

        let query = self.query.trim();
        if query.is_empty() {
            return true;
        }

        match self.search_mode {
            MemoSearchMode::Summary => contains_ci(&fields.summary, query),
            MemoSearchMode::Description => contains_ci(&fields.description, query),
            MemoSearchMode::Any => {
                contains_ci(&fields.summary, query)
                    || contains_ci(&fields.description, query)
                    || fields
                        .categories
                        .iter()
                        .any(|category| contains_ci(category, query))
                    || contains_ci(&fields.url.clone().unwrap_or_default(), query)
                    || contains_ci(&memo.source_uid, query)
                    || contains_ci(&memo.source_display_name, query)
            }
            MemoSearchMode::Advanced => self.advanced_terms.iter().all(|term| match term.field {
                MemoAdvancedField::Summary => contains_ci(&fields.summary, &term.value),
                MemoAdvancedField::Description => contains_ci(&fields.description, &term.value),
                MemoAdvancedField::Category => fields
                    .categories
                    .iter()
                    .any(|category| contains_ci(category, &term.value)),
                MemoAdvancedField::Source => {
                    contains_ci(&memo.source_uid, &term.value)
                        || contains_ci(&memo.source_display_name, &term.value)
                }
                MemoAdvancedField::Url => fields
                    .url
                    .as_deref()
                    .is_some_and(|url| contains_ci(url, &term.value)),
            }),
        }
    }

    pub fn filter<'a>(&self, memos: &'a [MemoRecord]) -> Vec<&'a MemoRecord> {
        memos.iter().filter(|memo| self.matches(memo)).collect()
    }

    pub fn advanced_terms(&self) -> &[MemoAdvancedTerm] {
        &self.advanced_terms
    }
}

pub fn memo_fields(memo: &MemoRecord) -> MemoFields {
    MemoFields::from_component(&memo.component)
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn unfold_lines(value: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in value.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&raw_line[1..]);
            }
        } else if !raw_line.is_empty() {
            lines.push(raw_line.to_string());
        }
    }
    lines
}

fn split_ical_list(value: &str) -> Vec<String> {
    let mut values = Vec::new();
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
            if !current.trim().is_empty() {
                values.push(unescape_ical_text(current.trim()));
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.trim().is_empty() {
        values.push(unescape_ical_text(current.trim()));
    }
    values
}

fn unescape_ical_text(value: &str) -> String {
    let mut output = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            match character {
                'n' | 'N' => output.push('\n'),
                ',' => output.push(','),
                ';' => output.push(';'),
                '\\' => output.push('\\'),
                other => {
                    output.push('\\');
                    output.push(other);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped {
        output.push('\\');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memo(
        source_uid: &str,
        uid: &str,
        summary: &str,
        description: &str,
        extra: &str,
    ) -> MemoRecord {
        MemoRecord {
            uid: uid.to_string(),
            source_uid: source_uid.to_string(),
            source_display_name: format!("Source {source_uid}"),
            component: format!(
                "BEGIN:VJOURNAL\r\nUID:{uid}\r\nSUMMARY:{summary}\r\nDESCRIPTION:{description}\r\n{extra}END:VJOURNAL\r\n"
            ),
            summary: summary.to_string(),
            description: description.to_string(),
            updated: "2026-08-23 10:00".to_string(),
        }
    }

    #[test]
    fn search_fields_stay_separate_and_any_is_explicit() {
        let record = memo(
            "source-a",
            "same",
            "title hit",
            "body only",
            "CATEGORIES:Work\r\n",
        );
        assert!(
            MemoFilter::new(
                MemoSearchMode::Summary,
                "title",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&record)
        );
        assert!(
            !MemoFilter::new(
                MemoSearchMode::Summary,
                "body",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&record)
        );
        assert!(
            MemoFilter::new(
                MemoSearchMode::Description,
                "body",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&record)
        );
        assert!(
            MemoFilter::new(MemoSearchMode::Any, "work", MemoCategoryFilter::Any, None,)
                .unwrap()
                .matches(&record)
        );
    }

    #[test]
    fn nested_component_properties_do_not_leak_into_memo_fields() {
        let record = memo(
            "source-a",
            "nested",
            "top-level",
            "top-level body",
            concat!(
                "CATEGORIES:Work\r\n",
                "BEGIN:VALARM\r\n",
                "SUMMARY:nested title\r\n",
                "DESCRIPTION:nested body\r\n",
                "CATEGORIES:Nested\r\n",
                "URL:https://nested.example\r\n",
                "END:VALARM\r\n",
            ),
        );
        assert!(
            !MemoFilter::new(
                MemoSearchMode::Summary,
                "nested title",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&record)
        );
        assert!(
            !MemoFilter::new(
                MemoSearchMode::Any,
                "nested body",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&record)
        );
        assert!(
            !MemoFilter::new(
                MemoSearchMode::Any,
                "nested",
                MemoCategoryFilter::Named("Nested".to_string()),
                None,
            )
            .unwrap()
            .matches(&record)
        );
        assert!(
            !MemoFilter::new(
                MemoSearchMode::Advanced,
                "url:https://nested.example",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&record)
        );
    }

    #[test]
    fn advanced_grammar_is_strict_and_uses_and() {
        let valid = parse_advanced_query("summary:alpha AND description:beta AND category:work")
            .expect("valid advanced query");
        assert_eq!(valid.len(), 3);
        for invalid in [
            "",
            "summary:",
            "unknown:value",
            "summary:value OR description:other",
            "summary:value and description:other",
            "summary:\"quoted\"",
            "summary:value AND",
            "summary:value AND AND description:other",
        ] {
            assert!(parse_advanced_query(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn category_without_and_named_filters_are_distinct() {
        let with = memo("source-a", "with", "With", "", "CATEGORIES:Work\r\n");
        let without = memo("source-a", "without", "Without", "", "");
        assert!(
            MemoFilter::new(
                MemoSearchMode::Any,
                "",
                MemoCategoryFilter::Named("work".to_string()),
                None,
            )
            .unwrap()
            .matches(&with)
        );
        assert!(
            !MemoFilter::new(
                MemoSearchMode::Any,
                "",
                MemoCategoryFilter::Named("work".to_string()),
                None,
            )
            .unwrap()
            .matches(&without)
        );
        assert!(
            MemoFilter::new(MemoSearchMode::Any, "", MemoCategoryFilter::Without, None,)
                .unwrap()
                .matches(&without)
        );
        assert!(
            !MemoFilter::new(MemoSearchMode::Any, "", MemoCategoryFilter::Without, None,)
                .unwrap()
                .matches(&with)
        );
    }

    #[test]
    fn escaped_category_values_remain_one_category() {
        let record = memo(
            "source-a",
            "escaped",
            "Title",
            "",
            "CATEGORIES:Work\\, urgent,Personal\r\n",
        );
        assert!(
            MemoFilter::new(
                MemoSearchMode::Any,
                "work, urgent",
                MemoCategoryFilter::Named("Work, urgent".to_string()),
                None,
            )
            .unwrap()
            .matches(&record)
        );
    }

    #[test]
    fn source_filter_is_exact_and_keeps_duplicate_uids_separate() {
        let first = memo("source-a", "duplicate", "A", "", "");
        let second = memo("source-b", "duplicate", "B", "", "");
        let filter = MemoFilter::new(
            MemoSearchMode::Any,
            "",
            MemoCategoryFilter::Any,
            Some("source-b".to_string()),
        )
        .unwrap();
        let records = [first, second];
        let matches = filter.filter(&records);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].source_uid, "source-b");
    }

    #[test]
    fn setting_values_are_validated_without_accepting_quotes_or_reserved_names() {
        assert_eq!(
            MemoSearchMode::from_setting("any-field"),
            MemoSearchMode::Any
        );
        assert_eq!(
            MemoCategoryFilter::from_setting(""),
            MemoCategoryFilter::Any
        );
        assert_eq!(
            MemoCategoryFilter::parse_setting("without").unwrap(),
            MemoCategoryFilter::Without
        );
        for value in ["\"quoted\"", "any", "without", "bad\nvalue"] {
            assert!(validate_category_name(value).is_err(), "{value:?}");
        }
        assert_eq!(validate_source_filter("  source-a  ").unwrap(), "source-a");
        for value in ["source a", "source\"a", "source\n-a"] {
            assert!(validate_source_filter(value).is_err(), "{value:?}");
        }
        assert!(
            MemoFilter::new(
                MemoSearchMode::Any,
                "",
                MemoCategoryFilter::Named("bad\"category".to_string()),
                None,
            )
            .is_err()
        );
        assert!(
            MemoFilter::new(
                MemoSearchMode::Any,
                "",
                MemoCategoryFilter::Any,
                Some("source a".to_string()),
            )
            .is_err()
        );
    }

    #[test]
    fn envelope_wrapping_does_not_hide_top_level_vjournal_fields() {
        let record = memo(
            "source-a",
            "wrapped",
            "Wrapped title",
            "Wrapped body",
            "CATEGORIES:Work\r\n",
        );
        let mut wrapped_record = record.clone();
        wrapped_record.component = record
            .component
            .replace("BEGIN:VJOURNAL", "BEGIN:VCALENDAR\r\nBEGIN:VJOURNAL")
            .replace("END:VJOURNAL\r\n", "END:VJOURNAL\r\nEND:VCALENDAR\r\n");
        assert!(
            MemoFilter::new(
                MemoSearchMode::Summary,
                "wrapped title",
                MemoCategoryFilter::Any,
                None,
            )
            .unwrap()
            .matches(&wrapped_record)
        );
    }
}
