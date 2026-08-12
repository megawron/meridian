// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent, backend-neutral mail filter definitions.
//
// The rule format is deliberately independent of Camel.  It can be migrated
// to CamelFilterDriver later, while local imports and tests can already use
// the same matching semantics without inventing successful server actions.

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterScope {
    Incoming,
    Outgoing,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterField {
    Subject,
    Sender,
    Recipient,
    Body,
    Header,
    Label,
    Unread,
    Flagged,
    Important,
    Attachment,
    Date,
    Folder,
    Junk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum FilterOperator {
    #[default]
    Contains,
    NotContains,
    Equals,
    NotEquals,
    StartsWith,
    EndsWith,
    Before,
    After,
    Exists,
}

impl FilterOperator {
    pub fn matches(self, field: FilterField, haystack: &str, value: &str) -> bool {
        let value = value.trim();
        match self {
            Self::Exists => {
                if field == FilterField::Header {
                    !value.is_empty() && header_exists(haystack, value)
                } else {
                    !haystack.trim().is_empty()
                }
            }
            Self::Contains => !value.is_empty() && lowercase(haystack).contains(&lowercase(value)),
            Self::NotContains => {
                !value.is_empty() && !lowercase(haystack).contains(&lowercase(value))
            }
            Self::Equals => !value.is_empty() && haystack.trim().eq_ignore_ascii_case(value),
            Self::NotEquals => !value.is_empty() && !haystack.trim().eq_ignore_ascii_case(value),
            Self::StartsWith => {
                !value.is_empty() && lowercase(haystack.trim()).starts_with(&lowercase(value))
            }
            Self::EndsWith => {
                !value.is_empty() && lowercase(haystack.trim()).ends_with(&lowercase(value))
            }
            Self::Before => !value.is_empty() && haystack.trim() < value,
            Self::After => !value.is_empty() && haystack.trim() > value,
        }
    }

    pub fn allows_empty_value(self, field: FilterField) -> bool {
        self == Self::Exists && field != FilterField::Header
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilterCondition {
    pub field: FilterField,
    pub value: String,
    #[serde(default)]
    pub operator: FilterOperator,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterAction {
    MoveToFolder(String),
    CopyToFolder(String),
    MarkRead,
    MarkUnread,
    SetFlagged,
    ClearFlagged,
    SetImportant,
    ClearImportant,
    Delete,
    StopProcessing,
    SetJunk,
    ClearJunk,
    SetLabel(String),
    ClearLabel(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailFilterRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub scope: FilterScope,
    pub condition: FilterCondition,
    pub action: FilterAction,
    /// Additional conditions imported from Evolution.  Older app-owned rule
    /// files only contain `condition`; in that case the legacy field remains
    /// the single effective condition.
    #[serde(default)]
    pub conditions: Vec<FilterCondition>,
    /// Additional actions imported from Evolution.  Older app-owned rule
    /// files only contain `action`; in that case the legacy field remains the
    /// single effective action.
    #[serde(default)]
    pub actions: Vec<FilterAction>,
    /// Whether all effective conditions must match.  Evolution's default is
    /// an AND rule; the field is defaulted for backward-compatible JSON.
    #[serde(default = "default_match_all")]
    pub match_all: bool,
}

/// Message metadata needed by conditions that are not plain text fields.
///
/// The context is intentionally borrowed from the backend-neutral message
/// representation.  Local mail can fill every field today, while the native
/// Camel path can use the same semantics once its complete message metadata
/// is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterMessageContext<'a> {
    pub date: &'a str,
    pub folder: &'a str,
    pub is_unread: bool,
    pub is_flagged: bool,
    pub is_important: bool,
    pub is_junk: bool,
    pub has_attachment: bool,
}

impl MailFilterRule {
    /// Validate a rule before it is persisted or handed to a backend.
    ///
    /// The legacy `condition`/`action` fields remain the effective values for
    /// older JSON files, so validation deliberately goes through the same
    /// compatibility accessors used by matching and execution.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("Die Filterregel benötigt eine ID".to_string());
        }
        if self.name.trim().is_empty() {
            return Err("Die Filterregel benötigt einen Namen".to_string());
        }

        let conditions = self.effective_conditions();
        if conditions.is_empty() {
            return Err("Die Filterregel benötigt mindestens eine Bedingung".to_string());
        }
        for (index, condition) in conditions.into_iter().enumerate() {
            if condition.value.trim().is_empty()
                && !condition.operator.allows_empty_value(condition.field)
            {
                return Err(format!("Bedingung {} benötigt einen Suchwert", index + 1));
            }
        }

        let actions = self.effective_actions();
        if actions.is_empty() {
            return Err("Die Filterregel benötigt mindestens eine Aktion".to_string());
        }
        for (index, action) in actions.into_iter().enumerate() {
            let target = match action {
                FilterAction::MoveToFolder(target)
                | FilterAction::CopyToFolder(target)
                | FilterAction::SetLabel(target)
                | FilterAction::ClearLabel(target) => Some(target),
                _ => None,
            };
            if target.is_some_and(|target| target.trim().is_empty()) {
                return Err(format!(
                    "Aktion {} benötigt einen Zielordner oder ein Label",
                    index + 1
                ));
            }
        }

        Ok(())
    }

    pub fn effective_conditions(&self) -> Vec<&FilterCondition> {
        if self.conditions.is_empty() {
            vec![&self.condition]
        } else {
            self.conditions.iter().collect()
        }
    }

    pub fn effective_actions(&self) -> Vec<&FilterAction> {
        if self.actions.is_empty() {
            vec![&self.action]
        } else {
            self.actions.iter().collect()
        }
    }

    pub fn matches(
        &self,
        subject: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        headers: &str,
        labels: &[String],
    ) -> bool {
        self.matches_with_context(
            subject,
            sender,
            recipient,
            body,
            headers,
            labels,
            FilterMessageContext {
                date: "",
                folder: "",
                is_unread: false,
                is_flagged: false,
                is_important: false,
                is_junk: false,
                has_attachment: false,
            },
        )
    }

    pub fn matches_with_status(
        &self,
        subject: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        headers: &str,
        labels: &[String],
        is_unread: bool,
        is_flagged: bool,
        is_important: bool,
        has_attachment: bool,
    ) -> bool {
        self.matches_with_context(
            subject,
            sender,
            recipient,
            body,
            headers,
            labels,
            FilterMessageContext {
                date: "",
                folder: "",
                is_unread,
                is_flagged,
                is_important,
                is_junk: false,
                has_attachment,
            },
        )
    }

    pub fn matches_with_context(
        &self,
        subject: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        headers: &str,
        labels: &[String],
        context: FilterMessageContext<'_>,
    ) -> bool {
        if !self.enabled {
            return false;
        }

        let label_text = labels.join(" ");
        let mut condition_matches = self.effective_conditions().into_iter().map(|condition| {
            let value = condition.value.trim().to_lowercase();
            match condition.field {
                FilterField::Date => {
                    date_condition_matches(condition.operator, context.date, condition.value.trim())
                }
                FilterField::Junk => {
                    boolean_condition_matches(condition.operator, &value, context.is_junk)
                }
                FilterField::Unread => {
                    boolean_condition_matches(condition.operator, &value, context.is_unread)
                }
                FilterField::Flagged => {
                    boolean_condition_matches(condition.operator, &value, context.is_flagged)
                }
                FilterField::Important => {
                    boolean_condition_matches(condition.operator, &value, context.is_important)
                }
                FilterField::Attachment => {
                    boolean_condition_matches(condition.operator, &value, context.has_attachment)
                }
                field => {
                    if value.is_empty() && !condition.operator.allows_empty_value(field) {
                        return false;
                    }
                    let haystack = match field {
                        FilterField::Subject => subject,
                        FilterField::Sender => sender,
                        FilterField::Recipient => recipient,
                        FilterField::Body => body,
                        FilterField::Header => headers,
                        FilterField::Label => &label_text,
                        FilterField::Folder => context.folder,
                        FilterField::Date | FilterField::Junk => unreachable!(),
                        FilterField::Unread
                        | FilterField::Flagged
                        | FilterField::Important
                        | FilterField::Attachment => unreachable!(),
                    };
                    condition.operator.matches(field, haystack, &value)
                }
            }
        });

        if self.match_all {
            condition_matches.all(|matches| matches)
        } else {
            condition_matches.any(|matches| matches)
        }
    }
}

fn boolean_condition_matches(operator: FilterOperator, value: &str, actual: bool) -> bool {
    if operator == FilterOperator::Exists {
        return actual;
    }

    let Some(expected) = parse_boolean_value(value) else {
        return false;
    };
    match operator {
        FilterOperator::NotContains | FilterOperator::NotEquals => actual != expected,
        _ => actual == expected,
    }
}

fn parse_boolean_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "ja" | "j" | "1" | "on" | "unread" | "starred" | "flagged"
        | "important" | "attachment" | "attachments" | "has-attachment" | "has_attachment" => {
            Some(true)
        }
        "false" | "no" | "n" | "nein" | "0" | "off" | "read" | "not-important"
        | "not_important" | "unflagged" | "unstarred" | "no-attachment" | "no_attachment"
        | "none" => Some(false),
        _ => None,
    }
}

fn date_condition_matches(operator: FilterOperator, actual: &str, wanted: &str) -> bool {
    if operator == FilterOperator::Exists {
        return !actual.trim().is_empty();
    }

    if wanted.trim().is_empty() {
        return false;
    }

    let actual = normalize_date_value(actual);
    let wanted = normalize_date_value(wanted);
    match operator {
        FilterOperator::Contains => actual.contains(&wanted),
        FilterOperator::NotContains => !actual.contains(&wanted),
        FilterOperator::Equals => actual == wanted,
        FilterOperator::NotEquals => actual != wanted,
        FilterOperator::StartsWith => actual.starts_with(&wanted),
        FilterOperator::EndsWith => actual.ends_with(&wanted),
        FilterOperator::Before => actual < wanted,
        FilterOperator::After => actual > wanted,
        FilterOperator::Exists => unreachable!(),
    }
}

fn normalize_date_value(value: &str) -> String {
    let value = value.trim();
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return date.format("%Y-%m-%d").to_string();
    }
    if value.len() >= 8 {
        if let Ok(date) = NaiveDate::parse_from_str(&value[..8], "%Y%m%d") {
            return date.format("%Y-%m-%d").to_string();
        }
    }
    if let Ok(date_time) = DateTime::parse_from_rfc3339(value) {
        return date_time.date_naive().format("%Y-%m-%d").to_string();
    }
    if let Ok(date_time) = DateTime::parse_from_rfc2822(value) {
        return date_time.date_naive().format("%Y-%m-%d").to_string();
    }
    for format in [
        "%Y%m%dT%H%M%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(date_time) = NaiveDateTime::parse_from_str(value, format) {
            return date_time.date().format("%Y-%m-%d").to_string();
        }
    }
    value.to_ascii_lowercase()
}

fn default_match_all() -> bool {
    true
}

fn lowercase(value: &str) -> String {
    value.to_lowercase()
}

fn header_exists(headers: &str, wanted: &str) -> bool {
    headers.lines().any(|line| {
        line.split_once(':')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(wanted))
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FilterFile {
    version: u32,
    #[serde(default)]
    rules: Vec<MailFilterRule>,
}

#[derive(Debug, Clone)]
pub struct MailFilterStore {
    path: PathBuf,
}

impl MailFilterStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_FILTER_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("mail-filters.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<MailFilterRule> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<FilterFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid mail filter file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported mail filter version"
            );
            return Vec::new();
        }
        file.rules
    }

    pub fn save(&self, rules: &[MailFilterRule]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = FilterFile {
            version: CURRENT_VERSION,
            rules: rules.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }

    pub fn upsert(&self, rule: MailFilterRule) -> io::Result<()> {
        rule.validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut rules = self.load();
        if let Some(existing) = rules.iter_mut().find(|existing| existing.id == rule.id) {
            *existing = rule;
        } else {
            rules.push(rule);
        }
        self.save(&rules)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> io::Result<bool> {
        let mut rules = self.load();
        let Some(rule) = rules.iter_mut().find(|rule| rule.id == id) else {
            return Ok(false);
        };
        if rule.enabled == enabled {
            return Ok(true);
        }
        rule.enabled = enabled;
        self.save(&rules)?;
        Ok(true)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut rules = self.load();
        let original_len = rules.len();
        rules.retain(|rule| rule.id != id);
        if rules.len() == original_len {
            return Ok(false);
        }
        self.save(&rules)?;
        Ok(true)
    }

    /// Move a rule by one position while preserving the order of every other
    /// rule.  The order is part of the portable rule semantics because later
    /// actions may observe a folder/flag change made by an earlier rule.
    pub fn move_rule(&self, id: &str, direction: i32) -> io::Result<bool> {
        let mut rules = self.load();
        let Some(index) = rules.iter().position(|rule| rule.id == id) else {
            return Ok(false);
        };
        let target = if direction < 0 {
            index.checked_sub(1)
        } else if direction > 0 {
            (index + 1 < rules.len()).then_some(index + 1)
        } else {
            None
        };
        let Some(target) = target else {
            return Ok(false);
        };
        rules.swap(index, target);
        self.save(&rules)?;
        Ok(true)
    }
}

impl Default for MailFilterStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_is_case_insensitive_and_respects_disabled_rules() {
        let mut rule = test_rule(FilterField::Subject, "rechnung");
        assert!(rule.matches(
            "Neue RECHNUNG",
            "sender@example.org",
            "recipient@example.org",
            "body",
            "",
            &[]
        ));

        rule.enabled = false;
        assert!(!rule.matches(
            "Neue RECHNUNG",
            "sender@example.org",
            "recipient@example.org",
            "body",
            "",
            &[]
        ));
    }

    #[test]
    fn recipient_matching_uses_to_cc_and_bcc_input() {
        let rule = test_rule(FilterField::Recipient, "billing@example.org");
        assert!(rule.matches(
            "Subject",
            "sender@example.org",
            "To recipient@example.org Cc billing@example.org",
            "body",
            "",
            &[]
        ));
    }

    #[test]
    fn operators_cover_common_evolution_conditions() {
        let mut rule = test_rule(FilterField::Subject, "invoice");
        rule.condition.operator = FilterOperator::NotContains;
        assert!(rule.matches("Quarterly report", "", "", "", "", &[]));
        assert!(!rule.matches("Invoice reminder", "", "", "", "", &[]));

        rule.condition.operator = FilterOperator::Equals;
        assert!(rule.matches("  INVOICE  ", "", "", "", "", &[]));
        assert!(!rule.matches("Invoice reminder", "", "", "", "", &[]));

        rule.condition.operator = FilterOperator::NotEquals;
        assert!(rule.matches("Invoice reminder", "", "", "", "", &[]));
        assert!(!rule.matches("invoice", "", "", "", "", &[]));

        rule.condition.operator = FilterOperator::StartsWith;
        assert!(rule.matches("Invoice reminder", "", "", "", "", &[]));
        assert!(!rule.matches("Re: invoice", "", "", "", "", &[]));

        rule.condition.operator = FilterOperator::EndsWith;
        assert!(rule.matches("Reminder invoice", "", "", "", "", &[]));
        assert!(!rule.matches("Invoice reminder", "", "", "", "", &[]));
    }

    #[test]
    fn exists_matches_fields_and_named_headers() {
        let mut subject_rule = test_rule(FilterField::Subject, "");
        subject_rule.condition.operator = FilterOperator::Exists;
        assert!(subject_rule.matches("Subject", "", "", "", "", &[]));
        assert!(!subject_rule.matches("", "", "", "", "", &[]));

        let mut header_rule = test_rule(FilterField::Header, "x-list-id");
        header_rule.condition.operator = FilterOperator::Exists;
        assert!(header_rule.matches("", "", "", "", "X-List-ID: team@example.org", &[]));
        assert!(!header_rule.matches("", "", "", "", "X-Other: team@example.org", &[]));
    }

    #[test]
    fn label_conditions_match_case_insensitively() {
        let rule = test_rule(FilterField::Label, "work");
        let labels = vec!["Work".to_string(), "Follow-up".to_string()];

        assert!(rule.matches("", "", "", "", "", &labels));
        assert!(!rule.matches("", "", "", "", "", &["Personal".to_string()]));
    }

    #[test]
    fn status_and_attachment_conditions_use_message_metadata() {
        let important = test_rule(FilterField::Important, "ja");
        assert!(important.matches_with_status("", "", "", "", "", &[], false, false, true, false));
        assert!(!important.matches_with_status(
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            false,
            false,
            false
        ));

        let mut not_flagged = test_rule(FilterField::Flagged, "true");
        not_flagged.condition.operator = FilterOperator::NotEquals;
        assert!(not_flagged.matches_with_status(
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            false,
            false,
            false
        ));
        assert!(!not_flagged.matches_with_status(
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            true,
            false,
            false
        ));

        let mut attachment = test_rule(FilterField::Attachment, "");
        attachment.condition.operator = FilterOperator::Exists;
        assert!(attachment.matches_with_status("", "", "", "", "", &[], false, false, false, true));
        assert!(!attachment.matches_with_status(
            "",
            "",
            "",
            "",
            "",
            &[],
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn date_folder_and_junk_conditions_use_message_context() {
        let context = FilterMessageContext {
            date: "Mon, 10 Aug 2026 12:00:00 +0000",
            folder: "archive/2026",
            is_unread: false,
            is_flagged: false,
            is_important: false,
            is_junk: true,
            has_attachment: false,
        };

        let date = test_rule(FilterField::Date, "2026-08-10");
        assert!(date.matches_with_context("", "", "", "", "", &[], context));

        let mut after = test_rule(FilterField::Date, "2026-08-09");
        after.condition.operator = FilterOperator::After;
        assert!(after.matches_with_context("", "", "", "", "", &[], context));

        let mut before = test_rule(FilterField::Date, "2026-08-11");
        before.condition.operator = FilterOperator::Before;
        assert!(before.matches_with_context("", "", "", "", "", &[], context));

        let folder = test_rule(FilterField::Folder, "archive");
        assert!(folder.matches_with_context("", "", "", "", "", &[], context));

        let junk = test_rule(FilterField::Junk, "ja");
        assert!(junk.matches_with_context("", "", "", "", "", &[], context));
    }

    #[test]
    fn invalid_rules_are_rejected_before_persistence() {
        let root = temporary_directory("filter-validation");
        let store = MailFilterStore::new(root.join("filters.json"));

        let mut missing_name = test_rule(FilterField::Subject, "invoice");
        missing_name.name.clear();
        let error = store
            .upsert(missing_name)
            .expect_err("a rule without a name must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(store.load().is_empty());

        let mut missing_condition_value = test_rule(FilterField::Subject, "");
        let error = store
            .upsert(missing_condition_value.clone())
            .expect_err("a non-existence condition without a value must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        missing_condition_value.condition.operator = FilterOperator::Exists;
        missing_condition_value.condition.value.clear();
        store
            .upsert(missing_condition_value)
            .expect("an existence condition may omit its value");
        assert_eq!(store.load().len(), 1);

        let mut missing_action_target = test_rule(FilterField::Subject, "invoice");
        missing_action_target.action = FilterAction::MoveToFolder(String::new());
        let error = store
            .upsert(missing_action_target)
            .expect_err("a move action without a target must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(store.load().len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rules_roundtrip_without_losing_actions() {
        let root = temporary_directory("filter-roundtrip");
        let store = MailFilterStore::new(root.join("filters.json"));
        let rule = MailFilterRule {
            id: "rule-1".to_string(),
            name: "Invoices".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Sender,
                value: "billing@example.org".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MoveToFolder("archive".to_string()),
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };

        store.upsert(rule.clone()).expect("rule should persist");
        assert_eq!(MailFilterStore::new(store.path()).load(), vec![rule]);

        assert!(
            store
                .set_enabled("rule-1", false)
                .expect("rule should disable")
        );
        assert!(!store.load()[0].enabled);
        assert!(store.delete("rule-1").expect("rule should delete"));
        assert!(store.load().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_single_condition_rules_get_compatible_defaults() {
        let raw = serde_json::json!({
            "id": "legacy",
            "name": "Legacy",
            "enabled": true,
            "scope": "Incoming",
            "condition": {"field": "Subject", "value": "invoice"},
            "action": "MarkRead"
        });
        let rule: MailFilterRule = serde_json::from_value(raw).expect("legacy rule should parse");
        assert!(rule.match_all);
        assert_eq!(rule.effective_conditions().len(), 1);
        assert_eq!(rule.effective_actions().len(), 1);
    }

    #[test]
    fn multiple_conditions_and_actions_roundtrip_and_match() {
        let rule = MailFilterRule {
            id: "multi".to_string(),
            name: "Billing and invoice".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MarkRead,
            conditions: vec![
                FilterCondition {
                    field: FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: FilterOperator::Contains,
                },
                FilterCondition {
                    field: FilterField::Sender,
                    value: "billing@example.org".to_string(),
                    operator: FilterOperator::Contains,
                },
            ],
            actions: vec![
                FilterAction::MarkRead,
                FilterAction::SetFlagged,
                FilterAction::SetImportant,
            ],
            match_all: true,
        };
        assert!(rule.matches(
            "Invoice",
            "Billing <billing@example.org>",
            "recipient@example.org",
            "body",
            "",
            &[]
        ));
        assert!(!rule.matches(
            "Invoice",
            "Other <other@example.org>",
            "recipient@example.org",
            "body",
            "",
            &[]
        ));
        assert_eq!(rule.effective_actions().len(), 3);

        let any_rule = MailFilterRule {
            match_all: false,
            ..rule.clone()
        };
        assert!(any_rule.matches(
            "Invoice",
            "Other <other@example.org>",
            "recipient@example.org",
            "body",
            "",
            &[]
        ));
        let encoded = serde_json::to_vec(&rule).expect("multi rule should serialize");
        assert_eq!(
            serde_json::from_slice::<MailFilterRule>(&encoded).unwrap(),
            rule
        );
    }

    #[test]
    fn rule_order_can_be_moved_and_persisted() {
        let root = temporary_directory("filter-order");
        let store = MailFilterStore::new(root.join("filters.json"));
        let make_rule = |id: &str| MailFilterRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: id.to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MarkRead,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        store
            .save(&[make_rule("one"), make_rule("two"), make_rule("three")])
            .expect("rules should persist");
        assert!(store.move_rule("three", -1).unwrap());
        assert_eq!(
            store
                .load()
                .into_iter()
                .map(|rule| rule.id)
                .collect::<Vec<_>>(),
            vec!["one", "three", "two"]
        );
        assert!(!store.move_rule("one", -1).unwrap());
        assert!(!store.move_rule("two", 1).unwrap());
        let _ = fs::remove_dir_all(root);
    }

    fn test_rule(field: FilterField, value: &str) -> MailFilterRule {
        MailFilterRule {
            id: "test".to_string(),
            name: "Test".to_string(),
            enabled: true,
            scope: FilterScope::Manual,
            condition: FilterCondition {
                field,
                value: value.to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::SetFlagged,
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        }
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
