// SPDX-License-Identifier: GPL-3.0-or-later
//
// Persistent, backend-neutral mail filter definitions.
//
// The rule format is deliberately independent of Camel.  It can be migrated
// to CamelFilterDriver later, while local imports and tests can already use
// the same matching semantics without inventing successful server actions.

use crate::i18n::gettext;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const CURRENT_VERSION: u32 = 1;

pub const PRESET_LOCAL_BACKUP_RULE_ID: &str = "preset-local-backup";
pub const PRESET_LOCAL_FIRST_RULE_ID: &str = "preset-local-first-download-delete";

pub fn is_explicit_local_folder_target(target: &str) -> bool {
    let trimmed = target.trim().to_ascii_lowercase();
    trimmed.starts_with("local:")
        || trimmed.starts_with("local/")
        || trimmed.starts_with("custom:")
        || trimmed.starts_with("on this computer:")
        || trimmed.starts_with("on this computer/")
        || trimmed.starts_with("auf diesem computer:")
        || trimmed.starts_with("auf diesem computer/")
}

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
    AllMessages,
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
    MatchesRegex,
}

impl FilterOperator {
    pub fn matches(self, field: FilterField, haystack: &str, value: &str) -> bool {
        if field == FilterField::AllMessages {
            return true;
        }
        let value = value.trim();
        match self {
            Self::Exists => {
                if field == FilterField::Header {
                    !value.is_empty() && header_exists(haystack, value)
                } else {
                    !haystack.trim().is_empty()
                }
            }
            Self::Contains => {
                if value.is_empty() {
                    false
                } else if value == "*" {
                    true
                } else if value.contains('*') || value.contains('?') {
                    wildcard_matches(&lowercase(haystack), &lowercase(value), false)
                } else {
                    lowercase(haystack).contains(&lowercase(value))
                }
            }
            Self::NotContains => {
                if value.is_empty() || value == "*" {
                    false
                } else if value.contains('*') || value.contains('?') {
                    !wildcard_matches(&lowercase(haystack), &lowercase(value), false)
                } else {
                    !lowercase(haystack).contains(&lowercase(value))
                }
            }
            Self::Equals => {
                if value.is_empty() {
                    false
                } else if value == "*" {
                    true
                } else if value.contains('*') || value.contains('?') {
                    wildcard_matches(&lowercase(haystack.trim()), &lowercase(value), true)
                } else {
                    haystack.trim().eq_ignore_ascii_case(value)
                }
            }
            Self::NotEquals => {
                if value.is_empty() || value == "*" {
                    false
                } else if value.contains('*') || value.contains('?') {
                    !wildcard_matches(&lowercase(haystack.trim()), &lowercase(value), true)
                } else {
                    !haystack.trim().eq_ignore_ascii_case(value)
                }
            }
            Self::StartsWith => {
                !value.is_empty() && lowercase(haystack.trim()).starts_with(&lowercase(value))
            }
            Self::EndsWith => {
                !value.is_empty() && lowercase(haystack.trim()).ends_with(&lowercase(value))
            }
            Self::Before => !value.is_empty() && haystack.trim() < value,
            Self::After => !value.is_empty() && haystack.trim() > value,
            Self::MatchesRegex => {
                if value.is_empty() {
                    false
                } else {
                    regex::RegexBuilder::new(value)
                        .case_insensitive(true)
                        .build()
                        .map(|re| re.is_match(haystack))
                        .unwrap_or(false)
                }
            }
        }
    }

    pub fn allows_empty_value(self, field: FilterField) -> bool {
        field == FilterField::AllMessages || (self == Self::Exists && field != FilterField::Header)
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
    AssignColor(String),
    UnsetColor,
    AssignScore(i32),
    AdjustScore(i32),
    Beep,
    PlaySound(String),
    RunProgram(String),
    PipeToProgram(String),
    ForwardTo(String),
}

impl FilterAction {
    pub fn description(&self) -> String {
        match self {
            Self::MoveToFolder(folder) => format!("Move to folder \"{folder}\""),
            Self::CopyToFolder(folder) => format!("Copy to folder \"{folder}\""),
            Self::MarkRead => "Mark as read".to_string(),
            Self::MarkUnread => "Mark as unread".to_string(),
            Self::SetFlagged => "Set star/flag status".to_string(),
            Self::ClearFlagged => "Clear star/flag status".to_string(),
            Self::SetImportant => "Mark as important".to_string(),
            Self::ClearImportant => "Clear importance".to_string(),
            Self::Delete => "Delete".to_string(),
            Self::StopProcessing => "Stop processing more filter rules".to_string(),
            Self::SetJunk => "Mark as junk".to_string(),
            Self::ClearJunk => "Clear junk status".to_string(),
            Self::SetLabel(label) => format!("Assign label \"{label}\""),
            Self::ClearLabel(label) => {
                if label.is_empty() {
                    "Clear all labels".to_string()
                } else {
                    format!("Clear label \"{label}\"")
                }
            }
            Self::AssignColor(color) => format!("Assign color \"{color}\""),
            Self::UnsetColor => "Unset color".to_string(),
            Self::AssignScore(score) => format!("Set score to {score}"),
            Self::AdjustScore(score) => format!("Adjust score by {score}"),
            Self::Beep => "Beep".to_string(),
            Self::PlaySound(file) => format!("Play sound file \"{file}\""),
            Self::RunProgram(cmd) => format!("Run program: \"{cmd}\""),
            Self::PipeToProgram(cmd) => format!("Pipe to program: \"{cmd}\""),
            Self::ForwardTo(addr) => format!("Forward to \"{addr}\""),
        }
    }

    pub fn target_value(&self) -> Option<&str> {
        match self {
            Self::MoveToFolder(target)
            | Self::CopyToFolder(target)
            | Self::SetLabel(target)
            | Self::ClearLabel(target)
            | Self::AssignColor(target)
            | Self::PlaySound(target)
            | Self::RunProgram(target)
            | Self::PipeToProgram(target)
            | Self::ForwardTo(target) => Some(target.as_str()),
            _ => None,
        }
    }
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
            return Err("Filter rule requires an ID".to_string());
        }
        if self.name.trim().is_empty() {
            return Err("Filter rule requires a name".to_string());
        }

        let conditions = self.effective_conditions();
        if conditions.is_empty() {
            return Err("Filter rule requires at least one condition".to_string());
        }
        for (index, condition) in conditions.into_iter().enumerate() {
            if condition.value.trim().is_empty()
                && !condition.operator.allows_empty_value(condition.field)
            {
                return Err(format!("Condition {} requires a search value", index + 1));
            }
        }

        let actions = self.effective_actions();
        if actions.is_empty() {
            return Err("Filter rule requires at least one action".to_string());
        }
        for (index, action) in actions.into_iter().enumerate() {
            let target = match action {
                FilterAction::MoveToFolder(target)
                | FilterAction::CopyToFolder(target)
                | FilterAction::SetLabel(target)
                | FilterAction::ClearLabel(target)
                | FilterAction::AssignColor(target)
                | FilterAction::PlaySound(target)
                | FilterAction::RunProgram(target)
                | FilterAction::PipeToProgram(target)
                | FilterAction::ForwardTo(target) => Some(target),
                _ => None,
            };
            if target.is_some_and(|target| target.trim().is_empty()) {
                return Err(format!(
                    "Action {} requires a target folder or label",
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
                FilterField::AllMessages => true,
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
                        FilterField::Date | FilterField::Junk | FilterField::AllMessages => {
                            unreachable!()
                        }
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

    /// Compiles the rule into upstream Camel filter driver S-expressions:
    /// returns `(search_query_sexp, actions_sexp)`.
    pub fn to_camel_sexp(&self) -> (String, String) {
        let conditions = self.effective_conditions();
        let cond_sexps: Vec<String> = conditions.into_iter().map(|c| c.to_camel_sexp()).collect();
        let search_sexp = if cond_sexps.is_empty() {
            String::new()
        } else if cond_sexps.len() == 1 {
            cond_sexps[0].clone()
        } else if self.match_all {
            format!("(and {})", cond_sexps.join(" "))
        } else {
            format!("(or {})", cond_sexps.join(" "))
        };

        let actions = self.effective_actions();
        let action_sexps: Vec<String> = actions.into_iter().map(|a| a.to_camel_sexp()).collect();
        let actions_sexp = action_sexps.join(" ");

        (search_sexp, actions_sexp)
    }
}

pub fn camel_escape_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

impl FilterCondition {
    pub fn to_camel_sexp(&self) -> String {
        let val = &self.value;
        match self.field {
            FilterField::Subject => match self.operator {
                FilterOperator::Contains => {
                    format!("(header-contains \"Subject\" {})", camel_escape_string(val))
                }
                FilterOperator::NotContains => format!(
                    "(not (header-contains \"Subject\" {}))",
                    camel_escape_string(val)
                ),
                FilterOperator::Equals => {
                    format!("(header-matches \"Subject\" {})", camel_escape_string(val))
                }
                FilterOperator::NotEquals => format!(
                    "(not (header-matches \"Subject\" {}))",
                    camel_escape_string(val)
                ),
                FilterOperator::StartsWith => format!(
                    "(header-starts-with \"Subject\" {})",
                    camel_escape_string(val)
                ),
                FilterOperator::EndsWith => {
                    format!("(header-ends-with \"Subject\" {})", camel_escape_string(val))
                }
                FilterOperator::Exists => "(header-exists \"Subject\")".to_string(),
                _ => format!("(header-contains \"Subject\" {})", camel_escape_string(val)),
            },
            FilterField::Sender => match self.operator {
                FilterOperator::Contains => {
                    format!("(header-contains \"From\" {})", camel_escape_string(val))
                }
                FilterOperator::NotContains => format!(
                    "(not (header-contains \"From\" {}))",
                    camel_escape_string(val)
                ),
                FilterOperator::Equals => {
                    format!("(header-matches \"From\" {})", camel_escape_string(val))
                }
                FilterOperator::NotEquals => format!(
                    "(not (header-matches \"From\" {}))",
                    camel_escape_string(val)
                ),
                FilterOperator::StartsWith => format!(
                    "(header-starts-with \"From\" {})",
                    camel_escape_string(val)
                ),
                FilterOperator::EndsWith => {
                    format!("(header-ends-with \"From\" {})", camel_escape_string(val))
                }
                FilterOperator::Exists => "(header-exists \"From\")".to_string(),
                _ => format!("(header-contains \"From\" {})", camel_escape_string(val)),
            },
            FilterField::Recipient => {
                let esc = camel_escape_string(val);
                match self.operator {
                    FilterOperator::Contains => format!(
                        "(or (header-contains \"To\" {esc}) (header-contains \"Cc\" {esc}))"
                    ),
                    FilterOperator::NotContains => format!(
                        "(not (or (header-contains \"To\" {esc}) (header-contains \"Cc\" {esc})))"
                    ),
                    FilterOperator::Equals => format!(
                        "(or (header-matches \"To\" {esc}) (header-matches \"Cc\" {esc}))"
                    ),
                    FilterOperator::NotEquals => format!(
                        "(not (or (header-matches \"To\" {esc}) (header-matches \"Cc\" {esc})))"
                    ),
                    FilterOperator::StartsWith => format!(
                        "(or (header-starts-with \"To\" {esc}) (header-starts-with \"Cc\" {esc}))"
                    ),
                    FilterOperator::EndsWith => format!(
                        "(or (header-ends-with \"To\" {esc}) (header-ends-with \"Cc\" {esc}))"
                    ),
                    FilterOperator::Exists => {
                        "(or (header-exists \"To\") (header-exists \"Cc\"))".to_string()
                    }
                    _ => format!(
                        "(or (header-contains \"To\" {esc}) (header-contains \"Cc\" {esc}))"
                    ),
                }
            }
            FilterField::Body => match self.operator {
                FilterOperator::NotContains => {
                    format!("(not (body-contains {}))", camel_escape_string(val))
                }
                _ => format!("(body-contains {})", camel_escape_string(val)),
            },
            FilterField::Header => match self.operator {
                FilterOperator::Exists => format!("(header-exists {})", camel_escape_string(val)),
                _ => format!("(header-contains \"{}\" {})", val, camel_escape_string(val)),
            },
            FilterField::Label => match self.operator {
                FilterOperator::NotEquals | FilterOperator::NotContains => {
                    format!("(not (user-flag {}))", camel_escape_string(val))
                }
                _ => format!("(user-flag {})", camel_escape_string(val)),
            },
            FilterField::Unread => {
                let is_true = parse_boolean_value(val).unwrap_or(true);
                if is_true {
                    "(not (system-flag \"Seen\"))".to_string()
                } else {
                    "(system-flag \"Seen\")".to_string()
                }
            }
            FilterField::Flagged => {
                let is_true = parse_boolean_value(val).unwrap_or(true);
                if is_true {
                    "(system-flag \"Flagged\")".to_string()
                } else {
                    "(not (system-flag \"Flagged\"))".to_string()
                }
            }
            FilterField::Important => {
                let is_true = parse_boolean_value(val).unwrap_or(true);
                if is_true {
                    "(system-flag \"Flagged\")".to_string()
                } else {
                    "(not (system-flag \"Flagged\"))".to_string()
                }
            }
            FilterField::Attachment => {
                let is_true = parse_boolean_value(val).unwrap_or(true);
                if is_true {
                    "(system-flag \"Attachments\")".to_string()
                } else {
                    "(not (system-flag \"Attachments\"))".to_string()
                }
            }
            FilterField::Junk => {
                let is_true = parse_boolean_value(val).unwrap_or(true);
                if is_true {
                    "(= (junk-test) 1)".to_string()
                } else {
                    "(not (= (junk-test) 1))".to_string()
                }
            }
            FilterField::Date => match self.operator {
                FilterOperator::Before => {
                    format!("(sent-date-before {})", camel_escape_string(val))
                }
                FilterOperator::After => {
                    format!("(sent-date-after {})", camel_escape_string(val))
                }
                _ => format!("(sent-date-is {})", camel_escape_string(val)),
            },
            FilterField::Folder => format!("(folder-name {})", camel_escape_string(val)),
            FilterField::AllMessages => "(match-all #t)".to_string(),
        }
    }
}

impl FilterAction {
    pub fn to_camel_sexp(&self) -> String {
        match self {
            Self::MoveToFolder(folder) => format!("(move-to {})", camel_escape_string(folder)),
            Self::CopyToFolder(folder) => format!("(copy-to {})", camel_escape_string(folder)),
            Self::MarkRead => "(set-system-flag \"Seen\")".to_string(),
            Self::MarkUnread => "(unset-system-flag \"Seen\")".to_string(),
            Self::SetFlagged => "(set-system-flag \"Flagged\")".to_string(),
            Self::ClearFlagged => "(unset-system-flag \"Flagged\")".to_string(),
            Self::SetImportant => "(set-system-flag \"Flagged\")".to_string(),
            Self::ClearImportant => "(unset-system-flag \"Flagged\")".to_string(),
            Self::Delete => "(delete)".to_string(),
            Self::StopProcessing => "(stop)".to_string(),
            Self::SetJunk => "(set-system-flag \"Junk\")".to_string(),
            Self::ClearJunk => "(unset-system-flag \"Junk\")".to_string(),
            Self::SetLabel(label) => format!("(set-label {})", camel_escape_string(label)),
            Self::ClearLabel(label) => {
                if label.is_empty() {
                    "(unset-label)".to_string()
                } else {
                    format!("(unset-label {})", camel_escape_string(label))
                }
            }
            Self::AssignColor(color) => format!("(set-color {})", camel_escape_string(color)),
            Self::UnsetColor => "(set-color \"\")".to_string(),
            Self::AssignScore(score) => format!("(set-score {score})"),
            Self::AdjustScore(score) => format!("(adjust-score {score})"),
            Self::Beep => "(beep)".to_string(),
            Self::PlaySound(sound) => format!("(play-sound {})", camel_escape_string(sound)),
            Self::RunProgram(cmd) => {
                format!("(shell \"/bin/sh\" \"-c\" {})", camel_escape_string(cmd))
            }
            Self::PipeToProgram(cmd) => format!(
                "(pipe-message \"/bin/sh\" \"-c\" {})",
                camel_escape_string(cmd)
            ),
            Self::ForwardTo(addr) => format!("(forward-to {} \"\")", camel_escape_string(addr)),
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
        FilterOperator::MatchesRegex => regex::RegexBuilder::new(&wanted)
            .case_insensitive(true)
            .build()
            .map(|re| re.is_match(&actual))
            .unwrap_or(false),
        FilterOperator::Exists => unreachable!(),
    }
}

fn wildcard_matches(haystack: &str, pattern: &str, full_match: bool) -> bool {
    let mut regex_src = String::new();
    if full_match {
        regex_src.push('^');
    }
    for ch in pattern.chars() {
        match ch {
            '*' => regex_src.push_str(".*"),
            '?' => regex_src.push('.'),
            other => {
                regex_src.push_str(&regex::escape(&other.to_string()));
            }
        }
    }
    if full_match {
        regex_src.push('$');
    }
    regex::RegexBuilder::new(&regex_src)
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .build()
        .map(|re| re.is_match(haystack))
        .unwrap_or(false)
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

fn rewrite_renamed_folder_target(target: &str, old_folder: &str, new_folder: &str) -> Option<String> {
    let strip_prefix_ci = |s: &str| -> String {
        let t = s.trim();
        let lower = t.to_ascii_lowercase();
        for p in ["custom:", "local:", "local/"] {
            if lower.starts_with(p) {
                return t[p.len()..].trim().trim_matches('/').to_string();
            }
        }
        t.trim_matches('/').to_string()
    };
    let old_clean = strip_prefix_ci(old_folder);
    let new_clean = strip_prefix_ci(new_folder);
    if old_clean.is_empty() || new_clean.is_empty() || old_clean == new_clean {
        return None;
    }

    let trimmed = target.trim();
    let lower = trimmed.to_ascii_lowercase();
    let prefixes = [
        "local:",
        "local/",
        "custom:",
        "on this computer:",
        "on this computer/",
        "auf diesem computer:",
        "auf diesem computer/",
        "",
    ];
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            let actual_prefix = &trimmed[..prefix.len()];
            let rest = trimmed[prefix.len()..].trim().trim_matches('/');
            if rest.eq_ignore_ascii_case(&old_clean) {
                let out_prefix = if actual_prefix.is_empty() {
                    ""
                } else {
                    actual_prefix
                };
                return Some(format!("{out_prefix}{new_clean}"));
            }
            if rest.len() > old_clean.len()
                && rest[..old_clean.len()].eq_ignore_ascii_case(&old_clean)
                && rest.as_bytes().get(old_clean.len()) == Some(&b'/')
            {
                let suffix = &rest[old_clean.len()..];
                let out_prefix = if actual_prefix.is_empty() {
                    ""
                } else {
                    actual_prefix
                };
                return Some(format!("{out_prefix}{new_clean}{suffix}"));
            }
        }
    }
    None
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FilterFile {
    version: u32,
    #[serde(default)]
    rules: Vec<MailFilterRule>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    presets_seeded: bool,
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
        let path = std::env::var_os("MERIDIAN_FILTER_FILE").or_else(|| std::env::var_os("EVOLUTION_GTK4_FILTER_FILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("mail-filters.json"));
        Self::new(path)
    }

    /// Ready-to-use, disabled-by-default filter presets for local-first workflows.
    ///
    /// Normal users never have their disk filled or server messages deleted
    /// because both rules ship with `enabled: false`. Users who want a local
    /// backup or a POP3-like "download to disk & delete from server" workflow
    /// can simply flip the switch in Preferences -> Mail Filters.
    pub fn preset_rules() -> Vec<MailFilterRule> {
        vec![
            MailFilterRule {
                id: PRESET_LOCAL_BACKUP_RULE_ID.to_string(),
                name: gettext("Local Backup"),
                enabled: false,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field: FilterField::AllMessages,
                    value: String::new(),
                    operator: FilterOperator::Exists,
                },
                action: FilterAction::CopyToFolder("local:archive".to_string()),
                conditions: vec![FilterCondition {
                    field: FilterField::AllMessages,
                    value: String::new(),
                    operator: FilterOperator::Exists,
                }],
                actions: vec![FilterAction::CopyToFolder("local:archive".to_string())],
                match_all: true,
            },
            MailFilterRule {
                id: PRESET_LOCAL_FIRST_RULE_ID.to_string(),
                name: gettext("Local-First Mode"),
                enabled: false,
                scope: FilterScope::Incoming,
                condition: FilterCondition {
                    field: FilterField::AllMessages,
                    value: String::new(),
                    operator: FilterOperator::Exists,
                },
                action: FilterAction::MoveToFolder("local:inbox".to_string()),
                conditions: vec![FilterCondition {
                    field: FilterField::AllMessages,
                    value: String::new(),
                    operator: FilterOperator::Exists,
                }],
                actions: vec![FilterAction::MoveToFolder("local:inbox".to_string())],
                match_all: true,
            },
        ]
    }

    /// Load filter rules and seed the disabled local-first presets once when
    /// the filter store has not yet been initialized with presets.
    pub fn load_or_init_presets(&self) -> Vec<MailFilterRule> {
        if let Ok(raw) = fs::read(&self.path) {
            if let Ok(mut file) = serde_json::from_slice::<FilterFile>(&raw) {
                if file.version == CURRENT_VERSION {
                    let mut dirty = false;
                    if !file.presets_seeded {
                        for preset in Self::preset_rules() {
                            if !file.rules.iter().any(|r| r.id == preset.id) {
                                file.rules.push(preset);
                            }
                        }
                        dirty = true;
                    }
                    for rule in &mut file.rules {
                        if rule.id == PRESET_LOCAL_BACKUP_RULE_ID
                            && (rule.name.trim().is_empty()
                                || matches!(
                                    rule.name.as_str(),
                                    "Local Backup: Keep a copy on this computer"
                                        | "Local Backup (Keep Copy)"
                                ))
                        {
                            rule.name = gettext("Local Backup");
                            dirty = true;
                        } else if rule.id == PRESET_LOCAL_FIRST_RULE_ID
                            && (rule.name.trim().is_empty()
                                || matches!(
                                    rule.name.as_str(),
                                    "Local-First: Download to this computer & delete from server"
                                        | "Local-First (Download & Remove from Server)"
                                ))
                        {
                            rule.name = gettext("Local-First Mode");
                            dirty = true;
                        }
                        if matches!(
                            rule.id.as_str(),
                            PRESET_LOCAL_BACKUP_RULE_ID | PRESET_LOCAL_FIRST_RULE_ID
                        ) {
                            if rule.condition.field == FilterField::Subject
                                && rule.condition.operator == FilterOperator::Exists
                                && rule.condition.value.trim().is_empty()
                            {
                                rule.condition.field = FilterField::AllMessages;
                                dirty = true;
                            }
                            for cond in &mut rule.conditions {
                                if cond.field == FilterField::Subject
                                    && cond.operator == FilterOperator::Exists
                                    && cond.value.trim().is_empty()
                                {
                                    cond.field = FilterField::AllMessages;
                                    dirty = true;
                                }
                            }
                        }
                    }
                    if dirty {
                        let _ = self.save_with_preset_flag(&file.rules, true);
                    }
                    return file.rules;
                }
            }
        }
        let presets = Self::preset_rules();
        let _ = self.save_with_preset_flag(&presets, true);
        presets
    }

    pub fn rename_folder_references(&self, old_folder: &str, new_folder: &str) -> io::Result<usize> {
        let mut rules = self.load();
        if rules.is_empty() {
            return Ok(0);
        }
        let mut updated_rules = 0usize;
        for rule in &mut rules {
            let mut changed = false;
            match &mut rule.action {
                FilterAction::MoveToFolder(target) | FilterAction::CopyToFolder(target) => {
                    if let Some(new_target) =
                        rewrite_renamed_folder_target(target, old_folder, new_folder)
                    {
                        *target = new_target;
                        changed = true;
                    }
                }
                _ => {}
            }
            for action in &mut rule.actions {
                match action {
                    FilterAction::MoveToFolder(target) | FilterAction::CopyToFolder(target) => {
                        if let Some(new_target) =
                            rewrite_renamed_folder_target(target, old_folder, new_folder)
                        {
                            *target = new_target;
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
            if rule.condition.field == FilterField::Folder {
                if let Some(new_target) =
                    rewrite_renamed_folder_target(&rule.condition.value, old_folder, new_folder)
                {
                    rule.condition.value = new_target;
                    changed = true;
                }
            }
            for cond in &mut rule.conditions {
                if cond.field == FilterField::Folder {
                    if let Some(new_target) =
                        rewrite_renamed_folder_target(&cond.value, old_folder, new_folder)
                    {
                        cond.value = new_target;
                        changed = true;
                    }
                }
            }
            if changed {
                updated_rules += 1;
            }
        }
        if updated_rules > 0 {
            self.save(&rules)?;
        }
        Ok(updated_rules)
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
        let presets_seeded = fs::read(&self.path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<FilterFile>(&raw).ok())
            .is_some_and(|f| f.presets_seeded);
        self.save_with_preset_flag(rules, presets_seeded)
    }

    fn save_with_preset_flag(
        &self,
        rules: &[MailFilterRule],
        presets_seeded: bool,
    ) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = FilterFile {
            version: CURRENT_VERSION,
            rules: rules.to_vec(),
            presets_seeded,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterActionStatus {
    Success,
    Failed,
    Skipped,
    Simulated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterLogEntry {
    pub timestamp: DateTime<Utc>,
    pub rule_id: String,
    pub rule_name: String,
    pub action_description: String,
    pub message_id: String,
    pub message_subject: String,
    pub status: FilterActionStatus,
    pub error_message: Option<String>,
}

const MAX_LOG_ENTRIES: usize = 500;

#[derive(Debug, Default)]
pub struct FilterExecutionLogger {
    entries: Mutex<Vec<FilterLogEntry>>,
}

impl FilterExecutionLogger {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn log(&self, entry: FilterLogEntry, log_file: Option<&Path>) {
        if let Some(path) = log_file {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
                use std::io::Write;
                let error_suffix = match &entry.error_message {
                    Some(err) => format!(" - error: {err}"),
                    None => String::new(),
                };
                let line = format!(
                    "[{}] [{:?}] rule '{}' ({}) -> {} on message '{}' (ID: {}){}\n",
                    entry.timestamp.format("%Y-%m-%d %H:%M:%S"),
                    entry.status,
                    entry.rule_name,
                    entry.rule_id,
                    entry.action_description,
                    entry.message_subject,
                    entry.message_id,
                    error_suffix
                );
                let _ = file.write_all(line.as_bytes());
            }
        }

        if let Ok(mut entries) = self.entries.lock() {
            entries.push(entry);
            if entries.len() > MAX_LOG_ENTRIES {
                let excess = entries.len() - MAX_LOG_ENTRIES;
                entries.drain(0..excess);
            }
        }
    }

    pub fn entries(&self) -> Vec<FilterLogEntry> {
        self.entries
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }
}

static GLOBAL_FILTER_LOGGER: OnceLock<FilterExecutionLogger> = OnceLock::new();

pub fn global_filter_logger() -> &'static FilterExecutionLogger {
    GLOBAL_FILTER_LOGGER.get_or_init(FilterExecutionLogger::new)
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

    #[test]
    fn camel_sexp_compiler_generates_valid_expressions() {
        let rule = MailFilterRule {
            id: "full-test".to_string(),
            name: "Parity Rule".to_string(),
            enabled: true,
            scope: FilterScope::Incoming,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "meeting".to_string(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MarkRead,
            conditions: vec![
                FilterCondition {
                    field: FilterField::Subject,
                    value: "meeting".to_string(),
                    operator: FilterOperator::Contains,
                },
                FilterCondition {
                    field: FilterField::Sender,
                    value: "boss@example.org".to_string(),
                    operator: FilterOperator::Equals,
                },
                FilterCondition {
                    field: FilterField::Unread,
                    value: "true".to_string(),
                    operator: FilterOperator::Equals,
                },
                FilterCondition {
                    field: FilterField::Attachment,
                    value: "yes".to_string(),
                    operator: FilterOperator::Equals,
                },
            ],
            actions: vec![
                FilterAction::MarkRead,
                FilterAction::AssignColor("#ff0000".to_string()),
                FilterAction::UnsetColor,
                FilterAction::AssignScore(10),
                FilterAction::AdjustScore(-5),
                FilterAction::Beep,
                FilterAction::PlaySound("alarm.ogg".to_string()),
                FilterAction::RunProgram("notify-send alert".to_string()),
                FilterAction::PipeToProgram("procmail".to_string()),
                FilterAction::ForwardTo("team@example.org".to_string()),
                FilterAction::MoveToFolder("archive".to_string()),
                FilterAction::StopProcessing,
            ],
            match_all: true,
        };

        let (query_sexp, actions_sexp) = rule.to_camel_sexp();
        assert!(query_sexp.starts_with("(and "));
        assert!(query_sexp.contains("(header-contains \"Subject\" \"meeting\")"));
        assert!(query_sexp.contains("(header-matches \"From\" \"boss@example.org\")"));
        assert!(query_sexp.contains("(not (system-flag \"Seen\"))"));
        assert!(query_sexp.contains("(system-flag \"Attachments\")"));

        assert!(actions_sexp.contains("(set-system-flag \"Seen\")"));
        assert!(actions_sexp.contains("(set-color \"#ff0000\")"));
        assert!(actions_sexp.contains("(set-color \"\")"));
        assert!(actions_sexp.contains("(set-score 10)"));
        assert!(actions_sexp.contains("(adjust-score -5)"));
        assert!(actions_sexp.contains("(beep)"));
        assert!(actions_sexp.contains("(play-sound \"alarm.ogg\")"));
        assert!(actions_sexp.contains("(shell \"/bin/sh\" \"-c\" \"notify-send alert\")"));
        assert!(actions_sexp.contains("(pipe-message \"/bin/sh\" \"-c\" \"procmail\")"));
        assert!(actions_sexp.contains("(forward-to \"team@example.org\" \"\")"));
        assert!(actions_sexp.contains("(move-to \"archive\")"));
        assert!(actions_sexp.contains("(stop)"));
    }

    #[test]
    fn new_actions_require_valid_targets() {
        let mut rule = test_rule(FilterField::Subject, "test");

        rule.action = FilterAction::AssignColor("".to_string());
        assert!(rule.validate().is_err());
        rule.action = FilterAction::AssignColor("#00ff00".to_string());
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::UnsetColor;
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::AssignScore(0);
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::Beep;
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::PlaySound("".to_string());
        assert!(rule.validate().is_err());
        rule.action = FilterAction::PlaySound("sound.wav".to_string());
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::RunProgram("".to_string());
        assert!(rule.validate().is_err());
        rule.action = FilterAction::RunProgram("echo hi".to_string());
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::PipeToProgram("".to_string());
        assert!(rule.validate().is_err());
        rule.action = FilterAction::PipeToProgram("cat".to_string());
        assert!(rule.validate().is_ok());

        rule.action = FilterAction::ForwardTo("".to_string());
        assert!(rule.validate().is_err());
        rule.action = FilterAction::ForwardTo("user@domain.org".to_string());
        assert!(rule.validate().is_ok());
    }

    #[test]
    fn filter_execution_logger_records_and_caps() {
        let logger = FilterExecutionLogger::new();
        assert!(logger.entries().is_empty());

        let temp = temporary_directory("logger-test");
        let log_file = temp.join("filter.log");

        for i in 0..520 {
            logger.log(
                FilterLogEntry {
                    timestamp: Utc::now(),
                    rule_id: format!("rule-{i}"),
                    rule_name: "Test Rule".to_string(),
                    action_description: "mark read".to_string(),
                    message_id: format!("msg-{i}"),
                    message_subject: "Subject".to_string(),
                    status: FilterActionStatus::Success,
                    error_message: None,
                },
                Some(&log_file),
            );
        }

        let entries = logger.entries();
        assert_eq!(entries.len(), 500);
        assert_eq!(entries.first().unwrap().rule_id, "rule-20");
        assert_eq!(entries.last().unwrap().rule_id, "rule-519");

        assert!(log_file.exists());
        let content = fs::read_to_string(&log_file).unwrap();
        assert!(content.contains("[Success] rule 'Test Rule' (rule-0) -> mark read"));

        logger.clear();
        assert!(logger.entries().is_empty());

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn load_or_init_presets_seeds_disabled_local_first_presets_once() {
        let temp = temporary_directory("preset-seed-test");
        let store = MailFilterStore::new(temp.join("mail-filters.json"));

        // Plain load() remains empty for uninitialized stores.
        assert!(store.load().is_empty());

        // load_or_init_presets() seeds both disabled-by-default rules.
        let seeded = store.load_or_init_presets();
        assert_eq!(seeded.len(), 2);
        assert!(seeded.iter().all(|rule| !rule.enabled));
        assert!(seeded.iter().any(|rule| rule.id == PRESET_LOCAL_BACKUP_RULE_ID));
        assert!(seeded.iter().any(|rule| rule.id == PRESET_LOCAL_FIRST_RULE_ID));

        // If the user deletes the presets and saves, load_or_init_presets() does not re-add them.
        store.save(&[]).unwrap();
        assert!(store.load_or_init_presets().is_empty());

        assert!(is_explicit_local_folder_target("local:inbox"));
        assert!(is_explicit_local_folder_target("local:archive"));
        assert!(is_explicit_local_folder_target("custom:Taxes"));
        assert!(!is_explicit_local_folder_target("Archive"));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn all_messages_wildcards_regex_and_folder_rename_sync_work() {
        let mut all_rule = test_rule(FilterField::AllMessages, "");
        all_rule.condition.operator = FilterOperator::Exists;
        assert!(all_rule.validate().is_ok());
        assert!(all_rule.matches("", "", "", "", "", &[]));
        assert_eq!(all_rule.to_camel_sexp().0, "(match-all #t)");

        let wildcard_rule = test_rule(FilterField::Subject, "inv*2026");
        assert!(wildcard_rule.matches("Invoice #42 for 2026", "", "", "", "", &[]));
        assert!(!wildcard_rule.matches("Invoice #42 for 2025", "", "", "", "", &[]));

        let star_all_rule = test_rule(FilterField::Subject, "*");
        assert!(star_all_rule.matches("", "", "", "", "", &[]));

        let mut regex_rule = test_rule(FilterField::Subject, r"^\[URGENT\]\s+#\d+");
        regex_rule.condition.operator = FilterOperator::MatchesRegex;
        assert!(regex_rule.matches("[urgent] #123 Server down", "", "", "", "", &[]));
        assert!(!regex_rule.matches("Normal #123 Server ok", "", "", "", "", &[]));

        let temp = temporary_directory("folder-rename-sync");
        let store = MailFilterStore::new(temp.join("mail-filters.json"));
        let mut folder_rule = test_rule(FilterField::AllMessages, "");
        folder_rule.action = FilterAction::MoveToFolder("local:Projects".to_string());
        folder_rule.actions = vec![
            FilterAction::MoveToFolder("local:Projects".to_string()),
            FilterAction::CopyToFolder("local:Projects/2026".to_string()),
        ];
        store.upsert(folder_rule).unwrap();

        let changed = store
            .rename_folder_references("Projects", "Clients")
            .unwrap();
        assert_eq!(changed, 1);
        let reloaded = store.load();
        assert_eq!(
            reloaded[0].action,
            FilterAction::MoveToFolder("local:Clients".to_string())
        );
        assert_eq!(
            reloaded[0].actions[1],
            FilterAction::CopyToFolder("local:Clients/2026".to_string())
        );

        let _ = fs::remove_dir_all(temp);
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
            std::env::temp_dir().join(format!("meridian-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
