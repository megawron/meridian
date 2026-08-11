// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent, backend-neutral mail filter definitions.
//
// The rule format is deliberately independent of Camel.  It can be migrated
// to CamelFilterDriver later, while local imports and tests can already use
// the same matching semantics without inventing successful server actions.

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
    Delete,
    StopProcessing,
    SetJunk,
    ClearJunk,
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

impl MailFilterRule {
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
    ) -> bool {
        if !self.enabled {
            return false;
        }

        let mut condition_matches = self.effective_conditions().into_iter().map(|condition| {
            let value = condition.value.trim().to_lowercase();
            if value.is_empty() && !condition.operator.allows_empty_value(condition.field) {
                return false;
            }
            let haystack = match condition.field {
                FilterField::Subject => subject,
                FilterField::Sender => sender,
                FilterField::Recipient => recipient,
                FilterField::Body => body,
                FilterField::Header => headers,
            };
            condition
                .operator
                .matches(condition.field, haystack, &value)
        });

        if self.match_all {
            condition_matches.all(|matches| matches)
        } else {
            condition_matches.any(|matches| matches)
        }
    }
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
            ""
        ));

        rule.enabled = false;
        assert!(!rule.matches(
            "Neue RECHNUNG",
            "sender@example.org",
            "recipient@example.org",
            "body",
            ""
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
            ""
        ));
    }

    #[test]
    fn operators_cover_common_evolution_conditions() {
        let mut rule = test_rule(FilterField::Subject, "invoice");
        rule.condition.operator = FilterOperator::NotContains;
        assert!(rule.matches("Quarterly report", "", "", "", ""));
        assert!(!rule.matches("Invoice reminder", "", "", "", ""));

        rule.condition.operator = FilterOperator::Equals;
        assert!(rule.matches("  INVOICE  ", "", "", "", ""));
        assert!(!rule.matches("Invoice reminder", "", "", "", ""));

        rule.condition.operator = FilterOperator::NotEquals;
        assert!(rule.matches("Invoice reminder", "", "", "", ""));
        assert!(!rule.matches("invoice", "", "", "", ""));

        rule.condition.operator = FilterOperator::StartsWith;
        assert!(rule.matches("Invoice reminder", "", "", "", ""));
        assert!(!rule.matches("Re: invoice", "", "", "", ""));

        rule.condition.operator = FilterOperator::EndsWith;
        assert!(rule.matches("Reminder invoice", "", "", "", ""));
        assert!(!rule.matches("Invoice reminder", "", "", "", ""));
    }

    #[test]
    fn exists_matches_fields_and_named_headers() {
        let mut subject_rule = test_rule(FilterField::Subject, "");
        subject_rule.condition.operator = FilterOperator::Exists;
        assert!(subject_rule.matches("Subject", "", "", "", ""));
        assert!(!subject_rule.matches("", "", "", "", ""));

        let mut header_rule = test_rule(FilterField::Header, "x-list-id");
        header_rule.condition.operator = FilterOperator::Exists;
        assert!(header_rule.matches("", "", "", "", "X-List-ID: team@example.org"));
        assert!(!header_rule.matches("", "", "", "", "X-Other: team@example.org"));
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
            actions: vec![FilterAction::MarkRead, FilterAction::SetFlagged],
            match_all: true,
        };
        assert!(rule.matches(
            "Invoice",
            "Billing <billing@example.org>",
            "recipient@example.org",
            "body",
            ""
        ));
        assert!(!rule.matches(
            "Invoice",
            "Other <other@example.org>",
            "recipient@example.org",
            "body",
            ""
        ));
        assert_eq!(rule.effective_actions().len(), 2);

        let any_rule = MailFilterRule {
            match_all: false,
            ..rule.clone()
        };
        assert!(any_rule.matches(
            "Invoice",
            "Other <other@example.org>",
            "recipient@example.org",
            "body",
            ""
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
