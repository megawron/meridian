// SPDX-License-Identifier: GPL-3.0-or-later
//
// JMAP Service — Implementation of JSON Meta Application Protocol
// (RFC 8620 Core, RFC 8621 Mail, RFC 8887 Calendars, and JMAP Contacts).
//
// Provides modern, low-latency JSON-over-HTTPS groupware communication
// supporting Fastmail, Stalwart, Apache James, Cyrus, and RFC 8620 endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;

pub const JMAP_CORE_URN: &str = "urn:ietf:params:jmap:core";
pub const JMAP_MAIL_URN: &str = "urn:ietf:params:jmap:mail";
pub const JMAP_SUBMISSION_URN: &str = "urn:ietf:params:jmap:submission";
pub const JMAP_CALENDARS_URN: &str = "urn:ietf:params:jmap:calendars";
pub const JMAP_CONTACTS_URN: &str = "urn:ietf:params:jmap:contacts";

/// JMAP Client Error types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JmapError {
    Network(String),
    Authentication(String),
    Protocol(String),
    AccountNotFound(String),
    MethodError {
        call_id: String,
        error_type: String,
        description: Option<String>,
    },
    Serialization(String),
}

impl fmt::Display for JmapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "JMAP-Netzwerkfehler: {msg}"),
            Self::Authentication(msg) => write!(f, "JMAP-Authentifizierungsfehler: {msg}"),
            Self::Protocol(msg) => write!(f, "JMAP-Protokollfehler: {msg}"),
            Self::AccountNotFound(id) => write!(f, "JMAP-Konto '{id}' nicht gefunden"),
            Self::MethodError {
                call_id,
                error_type,
                description,
            } => {
                write!(
                    f,
                    "JMAP-Methodenfehler [{call_id}]: {error_type} ({})",
                    description.as_deref().unwrap_or("keine Beschreibung")
                )
            }
            Self::Serialization(msg) => write!(f, "JMAP-Serialisierungsfehler: {msg}"),
        }
    }
}

impl std::error::Error for JmapError {}

// ── 1. RFC 8620 Core Session and Call Models ─────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapAccount {
    pub name: String,
    #[serde(default)]
    pub is_personal: bool,
    #[serde(default)]
    pub is_read_only: bool,
    #[serde(default)]
    pub account_capabilities: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapSession {
    pub capabilities: HashMap<String, Value>,
    pub accounts: HashMap<String, JmapAccount>,
    #[serde(default)]
    pub primary_accounts: HashMap<String, String>,
    pub username: String,
    pub api_url: String,
    pub download_url: String,
    pub upload_url: String,
    #[serde(default)]
    pub event_source_url: String,
    pub state: String,
}

impl JmapSession {
    /// Return the primary mail account ID for this session.
    pub fn primary_mail_account(&self) -> Option<&str> {
        self.primary_accounts
            .get(JMAP_MAIL_URN)
            .map(|s| s.as_str())
            .or_else(|| {
                self.accounts
                    .iter()
                    .find(|(_, a)| a.account_capabilities.contains_key(JMAP_MAIL_URN))
                    .map(|(id, _)| id.as_str())
            })
            .or_else(|| self.accounts.keys().next().map(|s| s.as_str()))
    }

    /// Return whether this session supports JMAP Mail.
    pub fn has_mail_support(&self) -> bool {
        self.capabilities.contains_key(JMAP_MAIL_URN)
    }

    /// Return whether this session supports JMAP Calendars.
    pub fn has_calendar_support(&self) -> bool {
        self.capabilities.contains_key(JMAP_CALENDARS_URN)
    }

    /// Return whether this session supports JMAP Contacts.
    pub fn has_contacts_support(&self) -> bool {
        self.capabilities.contains_key(JMAP_CONTACTS_URN)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapRequest {
    pub using: Vec<String>,
    pub method_calls: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapResponse {
    pub method_responses: Vec<Value>,
    #[serde(default)]
    pub session_state: String,
}

// ── 2. RFC 8621 Mailbox & Email Models ───────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JmapMailboxRights {
    #[serde(default)]
    pub may_read_items: bool,
    #[serde(default)]
    pub may_add_items: bool,
    #[serde(default)]
    pub may_remove_items: bool,
    #[serde(default)]
    pub may_set_seen: bool,
    #[serde(default)]
    pub may_set_keywords: bool,
    #[serde(default)]
    pub may_create_child: bool,
    #[serde(default)]
    pub may_rename: bool,
    #[serde(default)]
    pub may_delete: bool,
    #[serde(default)]
    pub may_submit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapMailbox {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub sort_order: u32,
    #[serde(default)]
    pub total_emails: u32,
    #[serde(default)]
    pub unread_emails: u32,
    #[serde(default)]
    pub total_threads: u32,
    #[serde(default)]
    pub unread_threads: u32,
    #[serde(default)]
    pub my_rights: JmapMailboxRights,
    #[serde(default)]
    pub is_subscribed: bool,
}

impl JmapMailbox {
    /// Return standard German localized display label for standard roles.
    pub fn localized_name(&self) -> String {
        if let Some(role) = self.role.as_deref() {
            match role.to_ascii_lowercase().as_str() {
                "inbox" => "Posteingang".to_string(),
                "drafts" => "Entwürfe".to_string(),
                "sent" => "Gesendet".to_string(),
                "trash" => "Papierkorb".to_string(),
                "archive" => "Archiv".to_string(),
                "junk" | "spam" => "Spam".to_string(),
                _ => self.name.clone(),
            }
        } else {
            self.name.clone()
        }
    }

    /// Return symbolic icon name for this mailbox.
    pub fn icon_name(&self) -> &'static str {
        if let Some(role) = self.role.as_deref() {
            match role.to_ascii_lowercase().as_str() {
                "inbox" => "mail-inbox-symbolic",
                "drafts" => "document-open-recent-symbolic",
                "sent" => "mail-send-symbolic",
                "trash" => "user-trash-symbolic",
                "archive" => "folder-download-symbolic",
                "junk" | "spam" => "mail-mark-junk-symbolic",
                _ => "folder-symbolic",
            }
        } else {
            "folder-symbolic"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    pub email: String,
}

impl JmapEmailAddress {
    pub fn formatted(&self) -> String {
        if let Some(name) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
            format!("{name} <{}>", self.email)
        } else {
            self.email.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmailBodyValue {
    pub value: String,
    #[serde(default)]
    pub is_encoding_problem: bool,
    #[serde(default)]
    pub is_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmailBodyPart {
    #[serde(default)]
    pub part_id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub name: Option<String>,
    pub type_: String,
    #[serde(default)]
    pub charset: Option<String>,
    #[serde(default)]
    pub disposition: Option<String>,
    #[serde(default)]
    pub cid: Option<String>,
    #[serde(default)]
    pub sub_parts: Option<Vec<JmapEmailBodyPart>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmail {
    pub id: String,
    pub blob_id: String,
    pub thread_id: String,
    #[serde(default)]
    pub mailbox_ids: HashMap<String, bool>,
    #[serde(default)]
    pub keywords: HashMap<String, bool>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub received_at: String,
    #[serde(default)]
    pub message_id: Option<Vec<String>>,
    #[serde(default)]
    pub in_reply_to: Option<Vec<String>>,
    #[serde(default)]
    pub references: Option<Vec<String>>,
    #[serde(default)]
    pub sender: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub from: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub reply_to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub body_values: HashMap<String, JmapEmailBodyValue>,
    #[serde(default)]
    pub text_body: Option<Vec<JmapEmailBodyPart>>,
    #[serde(default)]
    pub html_body: Option<Vec<JmapEmailBodyPart>>,
    #[serde(default)]
    pub attachments: Option<Vec<JmapEmailBodyPart>>,
    #[serde(default)]
    pub has_attachment: bool,
    #[serde(default)]
    pub preview: Option<String>,
}

impl JmapEmail {
    pub fn is_seen(&self) -> bool {
        self.keywords.get("$seen").copied().unwrap_or(false)
    }

    pub fn is_flagged(&self) -> bool {
        self.keywords.get("$flagged").copied().unwrap_or(false)
    }

    pub fn is_draft(&self) -> bool {
        self.keywords.get("$draft").copied().unwrap_or(false)
    }

    pub fn is_answered(&self) -> bool {
        self.keywords.get("$answered").copied().unwrap_or(false)
    }

    pub fn is_forwarded(&self) -> bool {
        self.keywords.get("$forwarded").copied().unwrap_or(false)
    }

    /// Extract text content from body values.
    pub fn plain_text_content(&self) -> Option<String> {
        if let Some(parts) = self.text_body.as_ref() {
            let mut result = String::new();
            for part in parts {
                if let Some(part_id) = part.part_id.as_deref() {
                    if let Some(body_val) = self.body_values.get(part_id) {
                        result.push_str(&body_val.value);
                    }
                }
            }
            if !result.is_empty() {
                return Some(result);
            }
        }
        self.preview.clone()
    }

    /// Extract HTML content from body values.
    pub fn html_content(&self) -> Option<String> {
        if let Some(parts) = self.html_body.as_ref() {
            let mut result = String::new();
            for part in parts {
                if let Some(part_id) = part.part_id.as_deref() {
                    if let Some(body_val) = self.body_values.get(part_id) {
                        result.push_str(&body_val.value);
                    }
                }
            }
            if !result.is_empty() {
                return Some(result);
            }
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEnvelope {
    pub mail_from: JmapEmailAddress,
    pub rcpt_to: Vec<JmapEmailAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmailSubmission {
    #[serde(default)]
    pub id: Option<String>,
    pub identity_id: String,
    pub email_id: String,
    pub thread_id: String,
    pub envelope: JmapEnvelope,
    #[serde(default)]
    pub send_at: Option<String>,
    #[serde(default)]
    pub undo_status: Option<String>,
    #[serde(default)]
    pub delivery_status: Option<HashMap<String, String>>,
}

// ── 3. RFC 8887 Calendars & Contacts Models ─────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapCalendar {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default = "default_true")]
    pub is_visible: bool,
    #[serde(default = "default_true")]
    pub is_subscribed: bool,
    #[serde(default)]
    pub total_events: u32,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapCalendarEvent {
    pub id: String,
    pub calendar_id: String,
    pub uid: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub start: String,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub is_all_day: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapContactCard {
    pub id: String,
    #[serde(default)]
    pub address_book_id: Option<String>,
    pub formatted_name: String,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub organization: Option<String>,
}

// ── 4. JMAP Client Implementation ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct JmapClient {
    pub session_url: String,
    pub bearer_token: String,
    session_cache: Option<JmapSession>,
}

impl JmapClient {
    pub fn new(session_url: &str, bearer_token: &str) -> Self {
        Self {
            session_url: session_url.trim().to_string(),
            bearer_token: bearer_token.trim().to_string(),
            session_cache: None,
        }
    }

    /// Discover and parse the JMAP session from the server.
    pub fn discover_session(&mut self) -> Result<JmapSession, JmapError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| JmapError::Network(e.to_string()))?;

        let response = client
            .get(&self.session_url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .header("Accept", "application/json")
            .send()
            .map_err(|e| JmapError::Network(e.to_string()))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(JmapError::Authentication(
                "Ungültiges JMAP-API-Token oder fehlende Berechtigung".to_string(),
            ));
        }

        if !response.status().is_success() {
            return Err(JmapError::Protocol(format!(
                "JMAP-Session-Abruf fehlgeschlagen mit Status: {}",
                response.status()
            )));
        }

        let session: JmapSession = response
            .json()
            .map_err(|e| JmapError::Serialization(e.to_string()))?;

        self.session_cache = Some(session.clone());
        Ok(session)
    }

    /// Execute batch JMAP method calls against the server API endpoint.
    pub fn execute_calls(
        &mut self,
        using: &[&str],
        calls: Vec<(&str, Value, &str)>,
    ) -> Result<Vec<(String, Value, String)>, JmapError> {
        let session = match &self.session_cache {
            Some(s) => s.clone(),
            None => self.discover_session()?,
        };

        let method_calls: Vec<Value> = calls
            .into_iter()
            .map(|(name, args, call_id)| json!([name, args, call_id]))
            .collect();

        let request = JmapRequest {
            using: using.iter().map(|s| s.to_string()).collect(),
            method_calls,
        };

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| JmapError::Network(e.to_string()))?;

        let response = client
            .post(&session.api_url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&request)
            .send()
            .map_err(|e| JmapError::Network(e.to_string()))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(JmapError::Authentication("Sitzung abgelaufen".to_string()));
        }

        let jmap_response: JmapResponse = response
            .json()
            .map_err(|e| JmapError::Serialization(e.to_string()))?;

        let mut results = Vec::new();
        for item in jmap_response.method_responses {
            if let Some(arr) = item.as_array() {
                if arr.len() >= 3 {
                    let name = arr[0].as_str().unwrap_or("").to_string();
                    let args = arr[1].clone();
                    let call_id = arr[2].as_str().unwrap_or("").to_string();

                    if name == "error" {
                        let error_type = args
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let description = args
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        return Err(JmapError::MethodError {
                            call_id,
                            error_type,
                            description,
                        });
                    }

                    results.push((name, args, call_id));
                }
            }
        }

        Ok(results)
    }

    /// Fetch all mailboxes for an account.
    pub fn get_mailboxes(&mut self, account_id: &str) -> Result<Vec<JmapMailbox>, JmapError> {
        let args = json!({
            "accountId": account_id,
        });

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN],
            vec![("Mailbox/get", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "Mailbox/get" {
                if let Some(list) = args.get("list") {
                    let mailboxes: Vec<JmapMailbox> = serde_json::from_value(list.clone())
                        .map_err(|e| JmapError::Serialization(e.to_string()))?;
                    return Ok(mailboxes);
                }
            }
        }

        Ok(Vec::new())
    }

    /// Query email IDs matching given filter/sort criteria.
    pub fn query_emails(
        &mut self,
        account_id: &str,
        filter: Option<Value>,
        sort: Option<Value>,
        limit: Option<usize>,
    ) -> Result<Vec<String>, JmapError> {
        let mut args = json!({
            "accountId": account_id,
        });

        if let Some(f) = filter {
            args["filter"] = f;
        }
        if let Some(s) = sort {
            args["sort"] = s;
        }
        if let Some(l) = limit {
            args["limit"] = json!(l);
        }

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN],
            vec![("Email/query", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "Email/query" {
                if let Some(ids) = args.get("ids").and_then(|v| v.as_array()) {
                    let email_ids: Vec<String> = ids
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                    return Ok(email_ids);
                }
            }
        }

        Ok(Vec::new())
    }

    /// Get detailed email objects by IDs.
    pub fn get_emails(
        &mut self,
        account_id: &str,
        ids: &[String],
    ) -> Result<Vec<JmapEmail>, JmapError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let args = json!({
            "accountId": account_id,
            "ids": ids,
            "properties": [
                "id", "blobId", "threadId", "mailboxIds", "keywords", "size",
                "receivedAt", "messageId", "inReplyTo", "references", "sender",
                "from", "to", "cc", "bcc", "replyTo", "subject", "sentAt",
                "hasAttachment", "preview", "bodyValues", "textBody", "htmlBody", "attachments"
            ],
            "fetchTextBodyValues": true,
            "fetchHTMLBodyValues": true,
        });

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN],
            vec![("Email/get", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "Email/get" {
                if let Some(list) = args.get("list") {
                    let emails: Vec<JmapEmail> = serde_json::from_value(list.clone())
                        .map_err(|e| JmapError::Serialization(e.to_string()))?;
                    return Ok(emails);
                }
            }
        }

        Ok(Vec::new())
    }

    /// Update email keywords (e.g. `$seen`, `$flagged`, `$draft`, custom tags).
    pub fn update_email_keywords(
        &mut self,
        account_id: &str,
        email_id: &str,
        keywords: &HashMap<String, bool>,
    ) -> Result<bool, JmapError> {
        let mut patch = HashMap::new();
        for (kw, state) in keywords {
            patch.insert(format!("keywords/{kw}"), json!(state));
        }

        let args = json!({
            "accountId": account_id,
            "update": {
                email_id: patch
            }
        });

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN],
            vec![("Email/set", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "Email/set" {
                if let Some(updated) = args.get("updated").and_then(|v| v.as_object()) {
                    return Ok(updated.contains_key(email_id));
                }
            }
        }

        Ok(false)
    }

    /// Move an email from one mailbox to another.
    pub fn move_email(
        &mut self,
        account_id: &str,
        email_id: &str,
        from_mailbox: &str,
        to_mailbox: &str,
    ) -> Result<bool, JmapError> {
        let patch = json!({
            format!("mailboxIds/{from_mailbox}"): serde_json::Value::Null,
            format!("mailboxIds/{to_mailbox}"): true,
        });

        let args = json!({
            "accountId": account_id,
            "update": {
                email_id: patch
            }
        });

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN],
            vec![("Email/set", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "Email/set" {
                if let Some(updated) = args.get("updated").and_then(|v| v.as_object()) {
                    return Ok(updated.contains_key(email_id));
                }
            }
        }

        Ok(false)
    }

    /// Delete an email (destroy).
    pub fn delete_email(&mut self, account_id: &str, email_id: &str) -> Result<bool, JmapError> {
        let args = json!({
            "accountId": account_id,
            "destroy": [email_id]
        });

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN],
            vec![("Email/set", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "Email/set" {
                if let Some(destroyed) = args.get("destroyed").and_then(|v| v.as_array()) {
                    return Ok(destroyed
                        .iter()
                        .any(|v| v.as_str().map(|s| s == email_id).unwrap_or(false)));
                }
            }
        }

        Ok(false)
    }

    /// Submit (send) an outgoing message via JMAP Submission.
    pub fn submit_email(
        &mut self,
        account_id: &str,
        identity_id: &str,
        email_id: &str,
        envelope: JmapEnvelope,
    ) -> Result<String, JmapError> {
        let submission_obj = json!({
            "identityId": identity_id,
            "emailId": email_id,
            "envelope": envelope,
        });

        let args = json!({
            "accountId": account_id,
            "create": {
                "sub1": submission_obj
            }
        });

        let responses = self.execute_calls(
            &[JMAP_CORE_URN, JMAP_MAIL_URN, JMAP_SUBMISSION_URN],
            vec![("EmailSubmission/set", args, "c1")],
        )?;

        for (name, args, _) in responses {
            if name == "EmailSubmission/set" {
                if let Some(created) = args.get("created").and_then(|v| v.as_object()) {
                    if let Some(sub_resp) = created.get("sub1") {
                        let sub_id = sub_resp
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("sub1")
                            .to_string();
                        return Ok(sub_id);
                    }
                }
            }
        }

        Err(JmapError::Protocol(
            "E-Mail-Versand über JMAP Submission fehlgeschlagen".to_string(),
        ))
    }
}

// ── 5. Unit Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jmap_session_deserialization() {
        let json_data = json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50000000,
                    "maxConcurrentUpload": 4
                },
                "urn:ietf:params:jmap:mail": {
                    "maxMailboxesPerEmail": 1000
                },
                "urn:ietf:params:jmap:submission": {},
                "urn:ietf:params:jmap:calendars": {}
            },
            "accounts": {
                "acc123": {
                    "name": "user@fastmail.com",
                    "isPersonal": true,
                    "isReadOnly": false,
                    "accountCapabilities": {
                        "urn:ietf:params:jmap:mail": {}
                    }
                }
            },
            "primaryAccounts": {
                "urn:ietf:params:jmap:mail": "acc123"
            },
            "username": "user@fastmail.com",
            "apiUrl": "https://api.fastmail.com/jmap/api/",
            "downloadUrl": "https://api.fastmail.com/jmap/download/{blobId}",
            "uploadUrl": "https://api.fastmail.com/jmap/upload/",
            "eventSourceUrl": "https://api.fastmail.com/jmap/events/",
            "state": "s12345"
        });

        let session: JmapSession = serde_json::from_value(json_data).unwrap();
        assert_eq!(session.username, "user@fastmail.com");
        assert_eq!(session.primary_mail_account(), Some("acc123"));
        assert!(session.has_mail_support());
        assert!(session.has_calendar_support());
        assert!(!session.has_contacts_support());
    }

    #[test]
    fn test_jmap_mailbox_role_and_icons() {
        let inbox = JmapMailbox {
            id: "mb-inbox".to_string(),
            name: "Inbox".to_string(),
            parent_id: None,
            role: Some("inbox".to_string()),
            sort_order: 1,
            total_emails: 150,
            unread_emails: 3,
            total_threads: 120,
            unread_threads: 3,
            my_rights: JmapMailboxRights::default(),
            is_subscribed: true,
        };

        assert_eq!(inbox.localized_name(), "Posteingang");
        assert_eq!(inbox.icon_name(), "mail-inbox-symbolic");

        let archive = JmapMailbox {
            id: "mb-archive".to_string(),
            name: "Archive".to_string(),
            parent_id: None,
            role: Some("archive".to_string()),
            sort_order: 5,
            total_emails: 500,
            unread_emails: 0,
            total_threads: 450,
            unread_threads: 0,
            my_rights: JmapMailboxRights::default(),
            is_subscribed: true,
        };

        assert_eq!(archive.localized_name(), "Archiv");
        assert_eq!(archive.icon_name(), "folder-download-symbolic");
    }

    #[test]
    fn test_jmap_email_deserialization_and_keywords() {
        let json_data = json!({
            "id": "em-001",
            "blobId": "b-001",
            "threadId": "th-001",
            "mailboxIds": {
                "mb-inbox": true
            },
            "keywords": {
                "$seen": true,
                "$flagged": true
            },
            "size": 2048,
            "receivedAt": "2026-09-06T12:00:00Z",
            "from": [{
                "name": "Alice Developer",
                "email": "alice@example.org"
            }],
            "to": [{
                "name": "Bob User",
                "email": "bob@example.org"
            }],
            "subject": "JMAP Protocol Test",
            "preview": "This is a preview of the JMAP message payload.",
            "hasAttachment": false,
            "bodyValues": {
                "p1": {
                    "value": "Hello from JMAP!"
                }
            },
            "textBody": [{
                "partId": "p1",
                "type": "text/plain"
            }]
        });

        let email: JmapEmail = serde_json::from_value(json_data).unwrap();
        assert_eq!(email.id, "em-001");
        assert!(email.is_seen());
        assert!(email.is_flagged());
        assert!(!email.is_draft());
        assert_eq!(email.subject.as_deref(), Some("JMAP Protocol Test"));
        assert_eq!(
            email.plain_text_content().as_deref(),
            Some("Hello from JMAP!")
        );
    }
}
