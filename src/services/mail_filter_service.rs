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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterScope {
    Incoming,
    Outgoing,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterField {
    Subject,
    Sender,
    Body,
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterCondition {
    pub field: FilterField,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterAction {
    MoveToFolder(String),
    CopyToFolder(String),
    MarkRead,
    SetFlagged,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailFilterRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub scope: FilterScope,
    pub condition: FilterCondition,
    pub action: FilterAction,
}

impl MailFilterRule {
    pub fn matches(&self, subject: &str, sender: &str, body: &str, headers: &str) -> bool {
        if !self.enabled {
            return false;
        }

        let value = self.condition.value.trim().to_lowercase();
        if value.is_empty() {
            return false;
        }

        let haystack = match self.condition.field {
            FilterField::Subject => subject,
            FilterField::Sender => sender,
            FilterField::Body => body,
            FilterField::Header => headers,
        };
        haystack.to_lowercase().contains(&value)
    }
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
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4/mail-filters.json"));
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
        assert!(rule.matches("Neue RECHNUNG", "sender@example.org", "body", ""));

        rule.enabled = false;
        assert!(!rule.matches("Neue RECHNUNG", "sender@example.org", "body", ""));
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
            },
            action: FilterAction::MoveToFolder("archive".to_string()),
        };

        store.upsert(rule.clone()).expect("rule should persist");
        assert_eq!(MailFilterStore::new(store.path()).load(), vec![rule]);

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
            },
            action: FilterAction::SetFlagged,
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
