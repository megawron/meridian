// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_engine — Unified cross-component search engine (Mail, Contacts, Calendar).

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
        let terms = query_str
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
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
                terms.iter().all(|term| searchable.contains(term))
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
        let terms = query_str
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return Vec::new();
        }

        documents
            .iter()
            .filter(|document| {
                (domain == SearchDomain::All || document.result.domain == domain)
                    && terms
                        .iter()
                        .all(|term| document.searchable_text.to_lowercase().contains(term))
            })
            .map(|document| document.result.clone())
            .collect()
    }
}

fn local_mail_search_text(message: &crate::services::mail_store_service::ParsedMail) -> String {
    let attachment_names = message
        .attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let state_terms = [
        if message.is_unread {
            "is:unread"
        } else {
            "is:read"
        },
        if message.is_flagged { "is:starred" } else { "" },
        if message.attachments.is_empty() {
            "no:attachment"
        } else {
            "has:attachment"
        },
    ]
    .join(" ");
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
    let state_terms = [
        if message.is_unread {
            "is:unread"
        } else {
            "is:read"
        },
        if message.is_flagged { "is:starred" } else { "" },
        if message.attachments.is_empty() {
            "no:attachment"
        } else {
            "has:attachment"
        },
    ]
    .join(" ");
    format!(
        "from:{} {} to:{} cc:{} bcc:{} reply-to:{} folder:{} subject:{} body:{} label:{} filename:{} {}",
        message.sender_email,
        message.sender_name,
        message.recipient_to,
        message.recipient_cc,
        message.recipient_bcc,
        message.reply_to,
        message.folder_name,
        message.subject,
        format!("{} {}", message.preview_snippet, message.html_body),
        message.labels.join(" "),
        attachment_names,
        state_terms,
    )
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
    }
}
