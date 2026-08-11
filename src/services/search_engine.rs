// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_engine — Unified cross-component search engine (Mail, Contacts, Calendar).

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDomain {
    Mail,
    Contacts,
    Calendar,
    All,
}

impl Default for SearchDomain {
    fn default() -> Self {
        Self::All
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SearchResultItem {
    pub domain: SearchDomain,
    pub title: String,
    pub snippet: String,
    pub date: String,
    pub reference_id: String,
}

#[derive(Debug, Clone)]
struct SearchDocument {
    result: SearchResultItem,
    searchable_text: String,
}

impl SearchDocument {
    fn new(result: SearchResultItem, searchable_text: impl Into<String>) -> Self {
        Self {
            result,
            searchable_text: searchable_text.into(),
        }
    }
}

#[allow(dead_code)]
pub struct GlobalSearchEngine;

#[allow(dead_code)]
impl GlobalSearchEngine {
    /// Query the currently available portable data sources.
    ///
    /// This intentionally searches only app-owned/read-only local stores.
    /// Native EDS/Camel live search is a separate Linux integration step, but
    /// the portable application already has a useful cross-component search.
    pub fn query(domain: SearchDomain, query_str: &str) -> Vec<SearchResultItem> {
        if query_str.trim().is_empty() {
            return Vec::new();
        }

        let mut documents = Vec::new();

        if matches!(domain, SearchDomain::Mail | SearchDomain::All) {
            documents.extend(
                crate::services::mail_store_service::MailStoreService::new()
                    .load_local_records()
                    .into_iter()
                    .map(|message| {
                        let snippet = local_mail_search_text(&message);
                        let searchable_text = format!(
                            "{} {} {}",
                            local_mail_search_index(&message),
                            message.subject,
                            message.date_str
                        );
                        SearchDocument::new(
                            SearchResultItem {
                                domain: SearchDomain::Mail,
                                title: message.subject,
                                snippet,
                                date: message.date_str,
                                reference_id: message.id,
                            },
                            searchable_text,
                        )
                    }),
            );
        }

        if matches!(domain, SearchDomain::Calendar | SearchDomain::All) {
            let snapshot = crate::services::calendar_store_service::CalendarStoreService::new()
                .load_snapshot();
            documents.extend(snapshot.events.into_iter().map(|event| {
                let title = event.summary;
                let snippet = format!(
                    "{} {} {} {}",
                    event.location, event.description, event.source_display_name, event.component
                );
                let date = event.start;
                SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Calendar,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: date.clone(),
                        reference_id: format!("{}:{}", event.source_uid, event.uid),
                    },
                    format!("{title} {snippet} {date}"),
                )
            }));
            documents.extend(snapshot.tasks.into_iter().map(|task| {
                let title = task.summary;
                let snippet = format!(
                    "Aufgabe {} Priorität {} {} {}",
                    task.source_display_name, task.priority, task.due, task.component
                );
                let date = task.due;
                SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Calendar,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: date.clone(),
                        reference_id: format!("{}:{}", task.source_uid, task.uid),
                    },
                    format!("{title} {snippet} {date}"),
                )
            }));
            documents.extend(snapshot.memos.into_iter().map(|memo| {
                let title = memo.summary;
                let snippet = format!(
                    "{} {} {}",
                    memo.description, memo.source_display_name, memo.component
                );
                let date = memo.updated;
                SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Calendar,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: date.clone(),
                        reference_id: format!("{}:{}", memo.source_uid, memo.uid),
                    },
                    format!("{title} {snippet} {date}"),
                )
            }));
        }

        if matches!(domain, SearchDomain::Contacts | SearchDomain::All) {
            documents.extend(
                crate::services::contact_store_service::ContactStoreService::new()
                    .load_contacts()
                    .into_iter()
                    .map(|record| {
                        let title = record.contact.fn_name.clone();
                        let snippet = record.contact.searchable_text();
                        SearchDocument::new(
                            SearchResultItem {
                                domain: SearchDomain::Contacts,
                                title: title.clone(),
                                snippet: snippet.clone(),
                                date: String::new(),
                                reference_id: record.id,
                            },
                            format!("{title} {snippet}"),
                        )
                    }),
            );
        }

        Self::query_search_documents(domain, query_str, &documents)
    }

    /// Search an explicit repository snapshot.  This is the backend-neutral
    /// part of the engine and can be used by EDS/Camel adapters without
    /// coupling them to GTK widgets.
    pub fn query_documents<'a, I>(
        domain: SearchDomain,
        query_str: &str,
        documents: I,
    ) -> Vec<SearchResultItem>
    where
        I: IntoIterator<Item = &'a SearchResultItem>,
    {
        let terms = search_query_terms(query_str);
        if terms.is_empty() {
            return Vec::new();
        }

        documents
            .into_iter()
            .filter(|document| {
                if domain != SearchDomain::All && document.domain != domain {
                    return false;
                }
                let searchable =
                    format!("{} {} {}", document.title, document.snippet, document.date)
                        .to_lowercase();
                let document_date = result_date(&document.date);
                terms
                    .iter()
                    .all(|term| search_term_matches(term, &searchable, document_date))
            })
            .cloned()
            .collect()
    }

    /// Search a snapshot from the real local mail reader.  This adapter keeps
    /// the result format independent from GTK and is ready to be replaced by
    /// CamelFolderSearch for synchronized accounts.
    pub fn query_local_mail<'a, I>(query_str: &str, messages: I) -> Vec<SearchResultItem>
    where
        I: IntoIterator<Item = &'a crate::services::mail_store_service::ParsedMail>,
    {
        let documents: Vec<SearchDocument> = messages
            .into_iter()
            .map(|message| {
                let snippet = local_mail_search_text(message);
                SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Mail,
                        title: message.subject.clone(),
                        snippet: snippet.clone(),
                        date: message.date_str.clone(),
                        reference_id: message.id.clone(),
                    },
                    format!(
                        "{} {} {}",
                        local_mail_search_index(message),
                        message.subject,
                        message.date_str
                    ),
                )
            })
            .collect();
        Self::query_search_documents(SearchDomain::Mail, query_str, &documents)
    }

    fn query_search_documents(
        domain: SearchDomain,
        query_str: &str,
        documents: &[SearchDocument],
    ) -> Vec<SearchResultItem> {
        let terms = search_query_terms(query_str);
        if terms.is_empty() {
            return Vec::new();
        }

        documents
            .iter()
            .filter(|document| {
                if domain != SearchDomain::All && document.result.domain != domain {
                    return false;
                }
                let searchable = document.searchable_text.to_lowercase();
                let document_date = result_date(&document.result.date);
                terms
                    .iter()
                    .all(|term| search_term_matches(term, &searchable, document_date))
            })
            .map(|document| document.result.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchQueryTerm {
    value: String,
    negated: bool,
    kind: SearchTermKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchTermKind {
    Text,
    DateAtOrAfter(NaiveDate),
    DateAtOrBefore(NaiveDate),
    DateOn(NaiveDate),
}

fn search_query_terms(input: &str) -> Vec<SearchQueryTerm> {
    search_tokens(input)
        .into_iter()
        .filter_map(|token| normalize_search_query_term(&token))
        .collect()
}

fn normalize_search_query_term(token: &str) -> Option<SearchQueryTerm> {
    let (initial_negated, token) = token
        .strip_prefix('-')
        .filter(|value| !value.is_empty())
        .map(|value| (true, value))
        .unwrap_or((false, token));
    let token = token.to_ascii_lowercase();
    if token.is_empty() {
        return None;
    }

    let mut term = token.clone();
    let mut negated = initial_negated;
    let date_token = token.strip_prefix("not-").unwrap_or(token.as_str());
    let date_negated = date_token != token;
    if let Some((key, value)) = date_token.split_once(':') {
        let date_kind = match key {
            "on" | "date" => Some(0),
            "after" | "newer" => Some(1),
            "before" | "older" => Some(2),
            _ => None,
        };
        if let Some(date_kind) = date_kind {
            if let Some(date) = parse_search_date(value) {
                let kind = match date_kind {
                    0 => SearchTermKind::DateOn(date),
                    1 => SearchTermKind::DateAtOrAfter(date),
                    _ => SearchTermKind::DateAtOrBefore(date),
                };
                return Some(SearchQueryTerm {
                    value: date_token.to_string(),
                    negated: negated ^ date_negated,
                    kind,
                });
            }
        }
    }
    if let Some(status) = token.strip_prefix("is:") {
        let (canonical, status_negated) = match status {
            "not-unread" => ("is:unread", true),
            "not-read" => ("is:read", true),
            "not-starred" | "not-flagged" => ("is:starred", true),
            "not-important" => ("is:important", true),
            "not-junk" | "not-spam" => ("is:junk", true),
            "attachment" | "attachments" => ("has:attachment", false),
            "not-attachment" | "not-attachments" => ("has:attachment", true),
            "inbox" | "sent" | "sent-mail" | "draft" | "drafts" | "trash" | "deleted"
            | "archive" | "junk" | "spam" => {
                return Some(SearchQueryTerm {
                    value: if matches!(status, "junk" | "spam") {
                        "is:junk".to_string()
                    } else {
                        format!("folder:{status}")
                    },
                    negated,
                    kind: SearchTermKind::Text,
                });
            }
            value if value.starts_with("not-") => {
                let folder = value.trim_start_matches("not-");
                if matches!(folder, "inbox" | "sent" | "drafts" | "trash" | "archive") {
                    return Some(SearchQueryTerm {
                        value: format!("folder:{folder}"),
                        negated: !negated,
                        kind: SearchTermKind::Text,
                    });
                }
                (token.as_str(), false)
            }
            "all" | "all-mail" => return None,
            _ => (token.as_str(), false),
        };
        term = canonical.to_string();
        negated ^= status_negated;
    } else if token == "no:attachment" || token == "no:attachments" {
        term = "has:attachment".to_string();
        negated = !negated;
    } else if token == "not:attachment" || token == "not:attachments" {
        term = "has:attachment".to_string();
        negated = !negated;
    } else if let Some(value) = token.strip_prefix("not:") {
        if value.is_empty() {
            return None;
        }
        term = value.to_string();
        negated = !negated;
    } else if let Some(value) = token.strip_prefix("not-") {
        if value.is_empty() {
            return None;
        }
        term = value.to_string();
        negated = !negated;
    }

    Some(SearchQueryTerm {
        value: term,
        negated,
        kind: SearchTermKind::Text,
    })
}

fn parse_search_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if let Some(days) = value
        .strip_suffix('d')
        .and_then(|value| value.parse::<i64>().ok())
    {
        return Some(Local::now().date_naive() - chrono::Duration::days(days.max(0)));
    }

    let relative = match value.to_ascii_lowercase().as_str() {
        "today" | "heute" => Some(Local::now().date_naive()),
        "yesterday" | "gestern" => Some(Local::now().date_naive() - chrono::Duration::days(1)),
        "tomorrow" | "morgen" => Some(Local::now().date_naive() + chrono::Duration::days(1)),
        _ => None,
    };

    relative
        .or_else(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .or_else(|| NaiveDate::parse_from_str(value, "%Y%m%d").ok())
        .or_else(|| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|date| date.date_naive())
        })
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|date| date.date_naive())
        })
        .or_else(|| {
            [
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%d %H:%M",
                "%Y%m%dT%H%M%S",
            ]
            .into_iter()
            .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
            .map(|date| date.date())
        })
}

fn result_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    parse_search_date(value).or_else(|| {
        value
            .split_once(':')
            .and_then(|(_, value)| parse_search_date(value))
    })
}

fn search_term_matches(
    term: &SearchQueryTerm,
    searchable: &str,
    document_date: Option<NaiveDate>,
) -> bool {
    let matches = match term.kind {
        SearchTermKind::Text => searchable.contains(&term.value),
        SearchTermKind::DateAtOrAfter(date) => {
            document_date.is_some_and(|document_date| document_date >= date)
        }
        SearchTermKind::DateAtOrBefore(date) => {
            document_date.is_some_and(|document_date| document_date <= date)
        }
        SearchTermKind::DateOn(date) => {
            document_date.is_some_and(|document_date| document_date == date)
        }
    };
    if term.negated { !matches } else { matches }
}

fn search_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn local_mail_search_text(message: &crate::services::mail_store_service::ParsedMail) -> String {
    let attachment_names = message
        .attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let state_terms = local_mail_state_terms(message);
    format!(
        "{} {} {} {} {} {} {} {} {} {} {}",
        message.sender_name,
        message.sender_email,
        message.recipient_to,
        message.recipient_cc,
        message.recipient_bcc,
        message.reply_to,
        message.folder_name,
        message.preview_snippet,
        message.html_body,
        message.labels.join(" "),
        format!("{} {}", attachment_names, state_terms),
    )
}

fn local_mail_search_index(message: &crate::services::mail_store_service::ParsedMail) -> String {
    let attachment_names = message
        .attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let state_terms = local_mail_state_terms(message);
    format!(
        "from:{} {} to:{} cc:{} bcc:{} reply-to:{} folder:{} in:{} subject:{} body:{} label:{} filename:{} {}",
        message.sender_email,
        message.sender_name,
        message.recipient_to,
        message.recipient_cc,
        message.recipient_bcc,
        message.reply_to,
        message.folder_name,
        message.folder_id,
        message.subject,
        format!("{} {}", message.preview_snippet, message.html_body),
        message.labels.join(" "),
        attachment_names,
        state_terms,
    )
}

fn local_mail_state_terms(message: &crate::services::mail_store_service::ParsedMail) -> String {
    let folder = message.folder_id.to_ascii_lowercase();
    [
        if message.is_unread {
            "is:unread"
        } else {
            "is:read"
        },
        if message.is_flagged {
            "is:starred"
        } else {
            "is:not-starred"
        },
        if message.is_important {
            "is:important"
        } else {
            "is:not-important"
        },
        if folder == "junk" || folder == "spam" || folder.starts_with("junk/") {
            "is:junk"
        } else {
            "is:not-junk"
        },
        if message.attachments.is_empty() {
            "no:attachment"
        } else {
            "has:attachment"
        },
    ]
    .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_global_queries_are_empty() {
        let results = GlobalSearchEngine::query(SearchDomain::All, "");
        assert!(results.is_empty());
    }

    #[test]
    fn search_filters_explicit_documents() {
        let documents = vec![
            SearchResultItem {
                domain: SearchDomain::Calendar,
                title: "Team Meeting".to_string(),
                snippet: "10:00".to_string(),
                date: "2026-08-10".to_string(),
                reference_id: "event-1".to_string(),
            },
            SearchResultItem {
                domain: SearchDomain::Mail,
                title: "Invoice".to_string(),
                snippet: "Finance".to_string(),
                date: "2026-08-09".to_string(),
                reference_id: "message-1".to_string(),
            },
        ];

        let results = GlobalSearchEngine::query_documents(SearchDomain::All, "meeting", &documents);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Team Meeting");
        assert_eq!(results[0].domain, SearchDomain::Calendar);
    }

    #[test]
    fn search_requires_all_terms_and_respects_domain() {
        let documents = vec![
            SearchResultItem {
                domain: SearchDomain::Contacts,
                title: "Ada Lovelace".to_string(),
                snippet: "Analytical Engine".to_string(),
                date: String::new(),
                reference_id: "contact-1".to_string(),
            },
            SearchResultItem {
                domain: SearchDomain::Mail,
                title: "Ada".to_string(),
                snippet: "Lovelace".to_string(),
                date: String::new(),
                reference_id: "message-1".to_string(),
            },
        ];

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::Contacts, "ada lovelace", &documents);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].reference_id, "contact-1");
    }

    #[test]
    fn search_supports_date_predicates_across_domains() {
        let documents = vec![
            SearchResultItem {
                domain: SearchDomain::Calendar,
                title: "Team Meeting".to_string(),
                snippet: "Room 1".to_string(),
                date: "2026-08-10 12:00".to_string(),
                reference_id: "event-1".to_string(),
            },
            SearchResultItem {
                domain: SearchDomain::Mail,
                title: "Invoice".to_string(),
                snippet: "Finance".to_string(),
                date: "2026-08-11".to_string(),
                reference_id: "message-1".to_string(),
            },
            SearchResultItem {
                domain: SearchDomain::Contacts,
                title: "Ada Lovelace".to_string(),
                snippet: "Analytical Engine".to_string(),
                date: String::new(),
                reference_id: "contact-1".to_string(),
            },
        ];

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::All, "on:2026-08-10", &documents);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].reference_id, "event-1");

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::All, "after:2026-08-10", &documents);
        assert_eq!(
            results
                .iter()
                .map(|result| result.reference_id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-1", "message-1"]
        );

        let results = GlobalSearchEngine::query_documents(
            SearchDomain::All,
            "older:2026-08-10 -on:2026-08-11",
            &documents,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].reference_id, "event-1");

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::All, "-after:2026-08-10", &documents);
        assert_eq!(
            results
                .iter()
                .map(|result| result.reference_id.as_str())
                .collect::<Vec<_>>(),
            vec!["contact-1"]
        );
    }

    #[test]
    fn local_mail_search_returns_stable_message_references() {
        let messages = [crate::services::mail_store_service::ParsedMail {
            id: "local:Inbox#0".to_string(),
            folder_id: "inbox".to_string(),
            folder_name: "inbox".to_string(),
            backend_location: None,
            sender_name: "Sender".to_string(),
            sender_email: "sender@example.org".to_string(),
            recipient_to: "me@example.org".to_string(),
            recipient_cc: String::new(),
            recipient_bcc: String::new(),
            reply_to: String::new(),
            subject: "Project update".to_string(),
            message_id: Some("message-1@example.org".to_string()),
            in_reply_to: Vec::new(),
            references: Vec::new(),
            thread_id: "message-1@example.org".to_string(),
            thread_size: 1,
            thread_position: 0,
            date_str: "2026-08-10".to_string(),
            preview_snippet: "The project is ready".to_string(),
            html_body: "<p>The project is ready</p>".to_string(),
            is_unread: true,
            is_flagged: false,
            is_important: false,
            labels: Vec::new(),
            attachments: Vec::new(),
        }];

        let results = GlobalSearchEngine::query_local_mail("project", &messages);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].reference_id, "local:Inbox#0");

        let results = GlobalSearchEngine::query_local_mail("me@example.org is:unread", &messages);
        assert_eq!(results.len(), 1);

        let results = GlobalSearchEngine::query_local_mail(
            "from:sender@example.org subject:update folder:inbox",
            &messages,
        );
        assert_eq!(results.len(), 1);

        let results = GlobalSearchEngine::query_local_mail("from:other@example.org", &messages);
        assert!(results.is_empty());

        let results =
            GlobalSearchEngine::query_local_mail("is:not-important is:not-junk", &messages);
        assert_eq!(results.len(), 1);

        let results = GlobalSearchEngine::query_local_mail("-from:other@example.org", &messages);
        assert_eq!(results.len(), 1);

        let results = GlobalSearchEngine::query_local_mail("is:important", &messages);
        assert!(results.is_empty());

        let results = GlobalSearchEngine::query_local_mail("newer:2026-08-10", &messages);
        assert_eq!(results.len(), 1);

        let results = GlobalSearchEngine::query_local_mail("-on:2026-08-10", &messages);
        assert!(results.is_empty());
    }
}
