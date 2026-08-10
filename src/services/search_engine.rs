// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_engine — Unified cross-component search engine (Mail, Contacts, Calendar).

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchDomain {
    Mail,
    Contacts,
    Calendar,
    All,
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
    /// Query the currently available data source.
    ///
    /// There is no live repository wired yet, so returning an empty result is
    /// safer than presenting invented mail, contact or calendar entries.  UI
    /// integration will pass repository snapshots to `query_documents`.
    pub fn query(domain: SearchDomain, query_str: &str) -> Vec<SearchResultItem> {
        tracing::debug!(
            ?domain,
            query = query_str,
            "Global search has no live repository yet"
        );
        Vec::new()
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
        let query = query_str.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        documents
            .into_iter()
            .filter(|document| {
                (domain == SearchDomain::All || document.domain == domain)
                    && [
                        document.title.as_str(),
                        document.snippet.as_str(),
                        document.date.as_str(),
                    ]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&query))
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
    fn search_does_not_invent_results_without_a_repository() {
        let results = GlobalSearchEngine::query(SearchDomain::All, "meeting");
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
    fn local_mail_search_returns_stable_message_references() {
        let messages = [crate::services::mail_store_service::ParsedMail {
            id: "local:Inbox#0".to_string(),
            folder_id: "inbox".to_string(),
            folder_name: "inbox".to_string(),
            backend_location: None,
            sender_name: "Sender".to_string(),
            sender_email: "sender@example.org".to_string(),
            recipient_to: "me@example.org".to_string(),
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
            attachments: Vec::new(),
        }];

        let results = GlobalSearchEngine::query_local_mail("project", &messages);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].reference_id, "local:Inbox#0");
    }
}
