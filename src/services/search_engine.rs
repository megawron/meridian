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
                    .map(|message| SearchResultItem {
                        domain: SearchDomain::Mail,
                        title: message.subject,
                        snippet: format!(
                            "{} {} {} {}",
                            message.sender_name,
                            message.sender_email,
                            message.preview_snippet,
                            message.folder_name
                        ),
                        date: message.date_str,
                        reference_id: message.id,
                    }),
            );
        }

        if matches!(domain, SearchDomain::Calendar | SearchDomain::All) {
            let snapshot = crate::services::calendar_store_service::CalendarStoreService::new()
                .load_snapshot();
            documents.extend(snapshot.events.into_iter().map(|event| SearchResultItem {
                domain: SearchDomain::Calendar,
                title: event.summary,
                snippet: format!("{} {}", event.location, event.description),
                date: event.start,
                reference_id: format!("{}:{}", event.source_uid, event.uid),
            }));
            documents.extend(snapshot.tasks.into_iter().map(|task| SearchResultItem {
                domain: SearchDomain::Calendar,
                title: task.summary,
                snippet: format!("Aufgabe {}", task.source_display_name),
                date: task.due,
                reference_id: format!("{}:{}", task.source_uid, task.uid),
            }));
            documents.extend(snapshot.memos.into_iter().map(|memo| SearchResultItem {
                domain: SearchDomain::Calendar,
                title: memo.summary,
                snippet: memo.description,
                date: memo.updated,
                reference_id: format!("{}:{}", memo.source_uid, memo.uid),
            }));
        }

        if matches!(domain, SearchDomain::Contacts | SearchDomain::All) {
            documents.extend(
                crate::services::contact_store_service::ContactStoreService::new()
                    .load_contacts()
                    .into_iter()
                    .map(|record| SearchResultItem {
                        domain: SearchDomain::Contacts,
                        title: record.contact.fn_name.clone(),
                        snippet: record.contact.searchable_text(),
                        date: String::new(),
                        reference_id: record.id,
                    }),
            );
        }

        Self::query_documents(domain, query_str, documents.iter())
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
        let documents: Vec<SearchResultItem> = messages
            .into_iter()
            .map(|message| SearchResultItem {
                domain: SearchDomain::Mail,
                title: message.subject.clone(),
                snippet: message.preview_snippet.clone(),
                date: message.date_str.clone(),
                reference_id: message.id.clone(),
            })
            .collect();
        Self::query_documents(SearchDomain::Mail, query_str, &documents)
    }
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
            labels: Vec::new(),
            attachments: Vec::new(),
        }];

        let results = GlobalSearchEngine::query_local_mail("project", &messages);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].reference_id, "local:Inbox#0");
    }
}
