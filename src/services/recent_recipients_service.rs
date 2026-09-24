// SPDX-License-Identifier: GPL-3.0-or-later
//
// RecentRecipientsService — Persistent storage and frecency ranking
// of recently contacted email recipients for composer autocompletion.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::settings_service::AppSettings;

const MAX_SAVED_RECIPIENTS: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentRecipient {
    pub display_name: String,
    pub email: String,
    pub last_used: i64,
    pub send_count: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentRecipientsFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    recipients: Vec<RecentRecipient>,
}

#[derive(Debug, Clone)]
pub struct RecentRecipientsService {
    storage_path: PathBuf,
    cache: Arc<Mutex<Option<Vec<RecentRecipient>>>>,
}

impl Default for RecentRecipientsService {
    fn default() -> Self {
        Self::new()
    }
}

impl RecentRecipientsService {
    /// Create a service targeting the standard app data location.
    pub fn new() -> Self {
        let storage_path = crate::services::app_data_dir().join("recent_recipients.json");
        Self::with_storage_path(storage_path)
    }

    /// Create a service with a custom storage path (useful for testing).
    pub fn with_storage_path(storage_path: PathBuf) -> Self {
        Self {
            storage_path,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    fn is_enabled(&self) -> bool {
        AppSettings::try_new()
            .map(|s| s.composer_track_recent_recipients())
            .unwrap_or(true)
    }

    /// Load recipients from disk or cache.
    fn load_recipients(&self) -> Vec<RecentRecipient> {
        if let Ok(guard) = self.cache.lock() {
            if let Some(cached) = guard.as_ref() {
                return cached.clone();
            }
        }

        let recipients = if self.storage_path.exists() {
            match fs::read(&self.storage_path) {
                Ok(bytes) => {
                    serde_json::from_slice::<RecentRecipientsFile>(&bytes)
                        .map(|file| file.recipients)
                        .unwrap_or_default()
                }
                Err(err) => {
                    tracing::warn!(path = ?self.storage_path, %err, "Failed to read recent recipients file");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(recipients.clone());
        }

        recipients
    }

    /// Persist recipients to disk and update memory cache.
    fn save_recipients(&self, recipients: Vec<RecentRecipient>) {
        if let Some(parent) = self.storage_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let file_data = RecentRecipientsFile {
            version: 1,
            recipients: recipients.clone(),
        };

        if let Ok(bytes) = serde_json::to_vec_pretty(&file_data) {
            let temp_path = self.storage_path.with_extension("tmp");
            if fs::write(&temp_path, &bytes).is_ok() {
                let _ = fs::rename(&temp_path, &self.storage_path);
            }
        }

        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(recipients);
        }
    }

    /// Parse header value like `"John Doe" <john@example.com>, alice@example.com`
    /// into clean `(display_name, email)` pairs.
    pub fn parse_recipients(header_value: &str) -> Vec<(String, String)> {
        let mut results = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;

        for ch in header_value.chars() {
            match ch {
                '"' => {
                    in_quotes = !in_quotes;
                    current.push(ch);
                }
                ',' | ';' if !in_quotes => {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        if let Some(parsed) = Self::parse_single_recipient(trimmed) {
                            results.push(parsed);
                        }
                    }
                    current.clear();
                }
                _ => current.push(ch),
            }
        }

        let trimmed = current.trim();
        if !trimmed.is_empty() {
            if let Some(parsed) = Self::parse_single_recipient(trimmed) {
                results.push(parsed);
            }
        }

        results
    }

    fn parse_single_recipient(raw: &str) -> Option<(String, String)> {
        let trimmed = raw.trim();
        if let Some(start) = trimmed.find('<') {
            if let Some(end) = trimmed.rfind('>') {
                if start < end {
                    let display_part = trimmed[..start].trim();
                    let email_part = trimmed[start + 1..end].trim();
                    let clean_email = email_part.to_ascii_lowercase();
                    if clean_email.contains('@') {
                        let display_name = display_part
                            .trim_matches('"')
                            .trim_matches('\'')
                            .trim()
                            .to_string();
                        return Some((display_name, clean_email));
                    }
                }
            }
        }

        let clean_email = trimmed
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_ascii_lowercase();

        if clean_email.contains('@') && !clean_email.contains(' ') {
            Some((String::new(), clean_email))
        } else {
            None
        }
    }

    /// Record a single recipient.
    pub fn record_recipient(&self, display_name: &str, email: &str) {
        if !self.is_enabled() {
            return;
        }

        let email_norm = email.trim().to_ascii_lowercase();
        if !email_norm.contains('@') {
            return;
        }

        let display_clean = display_name
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();

        let mut recipients = self.load_recipients();
        let now = Utc::now().timestamp();

        if let Some(existing) = recipients.iter_mut().find(|r| r.email == email_norm) {
            existing.send_count = existing.send_count.saturating_add(1);
            existing.last_used = now;
            if !display_clean.is_empty() && (existing.display_name.is_empty() || existing.display_name == existing.email) {
                existing.display_name = display_clean;
            }
        } else {
            recipients.push(RecentRecipient {
                display_name: display_clean,
                email: email_norm,
                last_used: now,
                send_count: 1,
            });
        }

        if recipients.len() > MAX_SAVED_RECIPIENTS {
            recipients.sort_by(|a, b| {
                let score_a = (a.send_count as i64 * 100) + (a.last_used / 86400);
                let score_b = (b.send_count as i64 * 100) + (b.last_used / 86400);
                score_b.cmp(&score_a)
            });
            recipients.truncate(MAX_SAVED_RECIPIENTS);
        }

        self.save_recipients(recipients);
    }

    /// Record all recipients found in an RFC 822/5322 header string (e.g. To / Cc / Bcc).
    pub fn record_recipients_from_header(&self, header_value: &str) {
        if !self.is_enabled() {
            return;
        }

        let parsed = Self::parse_recipients(header_value);
        for (name, email) in parsed {
            self.record_recipient(&name, &email);
        }
    }

    /// Search recent recipients matching a query, ranked by frecency.
    pub fn search_recent(&self, query: &str) -> Vec<RecentRecipient> {
        if !self.is_enabled() {
            return Vec::new();
        }

        let query_norm = query.trim().to_ascii_lowercase();
        let recipients = self.load_recipients();
        let now = Utc::now().timestamp();

        let mut scored: Vec<(i64, RecentRecipient)> = recipients
            .into_iter()
            .filter_map(|r| {
                let name_lower = r.display_name.to_ascii_lowercase();
                let email_lower = r.email.to_ascii_lowercase();

                let match_score = if query_norm.is_empty() {
                    0
                } else if email_lower.starts_with(&query_norm) {
                    60
                } else if name_lower.starts_with(&query_norm) {
                    50
                } else if name_lower.split_whitespace().any(|w| w.starts_with(&query_norm)) {
                    40
                } else if email_lower.contains(&query_norm) || name_lower.contains(&query_norm) {
                    20
                } else {
                    return None;
                };

                let age_secs = (now - r.last_used).max(0);
                let recency_score = if age_secs < 86_400 {
                    100
                } else if age_secs < 7 * 86_400 {
                    60
                } else if age_secs < 30 * 86_400 {
                    30
                } else {
                    10
                };

                let frequency_score = (r.send_count as i64).min(50) * 10;
                let total_score = match_score + recency_score + frequency_score;

                Some((total_score, r))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, r)| r).collect()
    }

    /// Return all stored recipients.
    pub fn all_recipients(&self) -> Vec<RecentRecipient> {
        self.load_recipients()
    }

    /// Clear all stored recipients.
    pub fn clear(&self) {
        let _ = fs::remove_file(&self.storage_path);
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(Vec::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_recipients() {
        let header = "John Doe <john@example.com>, \"Doe, Jane\" <jane@example.com>; plain@example.com";
        let parsed = RecentRecipientsService::parse_recipients(header);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], ("John Doe".to_string(), "john@example.com".to_string()));
        assert_eq!(parsed[1], ("Doe, Jane".to_string(), "jane@example.com".to_string()));
        assert_eq!(parsed[2], ("".to_string(), "plain@example.com".to_string()));
    }

    #[test]
    fn test_record_and_search_recent() {
        let temp_dir = std::env::temp_dir().join(format!("meridian_test_recent_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        let storage_path = temp_dir.join("recent.json");
        let service = RecentRecipientsService::with_storage_path(storage_path);

        service.record_recipient("Alice Smith", "alice@example.com");
        service.record_recipient("Alice Smith", "alice@example.com"); // count = 2
        service.record_recipient("Bob Jones", "bob@domain.org");

        let results = service.search_recent("alice");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].email, "alice@example.com");
        assert_eq!(results[0].send_count, 2);

        let results_all = service.search_recent("example");
        assert_eq!(results_all.len(), 1);

        let results_blank = service.search_recent("");
        assert_eq!(results_blank.len(), 2);

        service.clear();
        assert!(service.all_recipients().is_empty());
        let _ = fs::remove_dir_all(temp_dir);
    }
}
