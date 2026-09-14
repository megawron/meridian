// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_engine — Unified cross-component search engine (Mail, Contacts, Calendar).

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::contact_store_service::ContactStoreService;
use crate::services::html_sanitizer::html_to_plain_text;
use crate::services::mail_filter_service::{
    FilterAction, FilterMessageContext, FilterScope, MailFilterRule, MailFilterStore,
};
use crate::services::mail_metadata_service::{
    MailFollowUpState, MailMessageState, MailMetadataStore,
};
use crate::services::mail_store_service::{MailStoreService, ParsedMail};

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

/// Typed identity carried by a global-search result into the window router.
/// Source UIDs are intentionally not normalized or encoded; an empty source
/// UID is valid for read-only imported sources.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTarget {
    Mail {
        local_id: String,
    },
    Event {
        source_uid: String,
        component_uid: String,
    },
    Task {
        source_uid: String,
        component_uid: String,
    },
    Memo {
        source_uid: String,
        component_uid: String,
    },
    Contact {
        source_uid: String,
        contact_uid: String,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SearchResultItem {
    pub domain: SearchDomain,
    pub title: String,
    pub snippet: String,
    pub date: String,
    pub target: Option<SearchTarget>,
}

const LOCAL_ADDRESS_BOOK_SOURCE_UID: &str = "local-addressbook";

/// Owned local-store inputs for one global-search worker.
///
/// The value is captured on the GTK main thread before a worker is spawned.
/// The service objects are plain, owned Rust values; no GTK/GObject/AppSettings
/// handle or process-global lookup is part of this boundary.
#[derive(Debug, Clone)]
pub struct SearchStoreSnapshot {
    mail: MailStoreService,
    mail_metadata: MailMetadataStore,
    filter_store: MailFilterStore,
    filter_rules: Vec<MailFilterRule>,
    raw_mail_filter_store: MailFilterStore,
    calendar: CalendarStoreService,
    contacts: ContactStoreService,
}

impl SearchStoreSnapshot {
    /// Capture all local search inputs before entering a worker thread.
    ///
    /// This function must run on the GTK main thread, where settings-backed
    /// constructors and environment/app-data resolution are allowed.  The
    /// returned value is plain Rust data and can be moved to a worker.
    pub fn capture_on_main_thread() -> Self {
        let mail = MailStoreService::new();
        let mail_metadata = MailMetadataStore::new(mail.metadata_path());
        let filter_store = MailFilterStore::default();
        let filter_rules = filter_store.load();

        // MailStoreService's local loader accepts a filter store, but applying
        // incoming actions through that API also persists those actions to the
        // app-owned mail sidecar.  Global search is read-only, so this captured
        // empty store deliberately gives the loader only its raw/state-loading
        // path.  The real rules above are applied to owned ParsedMail values
        // below without any filesystem mutation.
        let raw_mail_filter_store = MailFilterStore::new(PathBuf::new());

        let calendar = CalendarStoreService::new();
        let contacts = ContactStoreService::new();

        Self {
            mail,
            mail_metadata,
            filter_store,
            filter_rules,
            raw_mail_filter_store,
            calendar,
            contacts,
        }
    }
}

/// Apply incoming local rules as a read-only projection for global search.
///
/// `MailStoreService::load_local_records_cancelable_with_store` is also used
/// by mutation/import workflows and therefore persists filter actions through
/// its metadata sidecar.  Search must not turn a read into a write.  Loading
/// with the captured empty filter store above gives us the raw records plus
/// already-persisted client state; this function then applies the captured
/// rules only to owned values.  Copy actions produce virtual records, while
/// the source mailbox and sidecar remain untouched.
fn apply_read_only_mail_filters(
    messages: Vec<ParsedMail>,
    rules: &[MailFilterRule],
    metadata_store: &MailMetadataStore,
    cancellation: &AtomicBool,
) -> Option<Vec<ParsedMail>> {
    let metadata = ReadOnlyMailMetadata::load(metadata_store);
    let mut source_messages = Vec::with_capacity(messages.len());
    let mut persisted_copies = BTreeMap::<String, Vec<ParsedMail>>::new();
    for message in messages {
        if let Some((source_id, _)) = local_copy_parts(&message.id) {
            persisted_copies
                .entry(source_id.to_string())
                .or_default()
                .push(message);
        } else {
            source_messages.push(message);
        }
    }

    let mut visible_messages = Vec::with_capacity(source_messages.len());
    let mut copied_messages = Vec::new();

    for mut message in source_messages {
        if cancellation.load(Ordering::Relaxed) {
            return None;
        }

        let mut deleted = false;
        let mut copy_folders = BTreeSet::new();
        // Persisted logical copies are created after incoming rules in the
        // service loader.  Do not run the same incoming rules a second time on
        // those records.
        if !message.id.contains("::copy::") {
            for rule in rules {
                if rule.scope != FilterScope::Incoming || !mail_filter_matches(rule, &message) {
                    continue;
                }

                let mut stop_processing = false;
                for action in rule.effective_actions() {
                    match action {
                        FilterAction::MarkRead => message.is_unread = false,
                        FilterAction::MarkUnread => message.is_unread = true,
                        FilterAction::SetFlagged => message.is_flagged = true,
                        FilterAction::ClearFlagged => message.is_flagged = false,
                        FilterAction::SetImportant => message.is_important = true,
                        FilterAction::ClearImportant => message.is_important = false,
                        FilterAction::MoveToFolder(folder) => {
                            message.folder_id = local_filter_folder_id(folder);
                        }
                        FilterAction::SetJunk => {
                            message.folder_id = local_filter_folder_id("junk");
                        }
                        FilterAction::ClearJunk => {
                            message.folder_id = local_filter_folder_id("inbox");
                        }
                        FilterAction::CopyToFolder(folder) => {
                            copy_folders.insert(local_filter_folder_id(folder));
                        }
                        FilterAction::Delete => deleted = true,
                        FilterAction::StopProcessing => {
                            stop_processing = true;
                            break;
                        }
                        FilterAction::SetLabel(label) => {
                            let label = label.trim();
                            if !label.is_empty()
                                && !message
                                    .labels
                                    .iter()
                                    .any(|candidate| candidate.eq_ignore_ascii_case(label))
                            {
                                message.labels.push(label.to_string());
                                message
                                    .labels
                                    .sort_by_key(|value| value.to_ascii_lowercase());
                            }
                        }
                        FilterAction::ClearLabel(label) => {
                            let label = label.trim();
                            message
                                .labels
                                .retain(|candidate| !candidate.eq_ignore_ascii_case(label));
                        }
                        FilterAction::AssignColor(_)
                        | FilterAction::UnsetColor
                        | FilterAction::AssignScore(_)
                        | FilterAction::AdjustScore(_)
                        | FilterAction::Beep
                        | FilterAction::PlaySound(_)
                        | FilterAction::RunProgram(_)
                        | FilterAction::PipeToProgram(_)
                        | FilterAction::ForwardTo(_) => {}
                    }
                }
                if stop_processing {
                    break;
                }
            }
        }

        if deleted {
            continue;
        }

        let source_id = message.id.clone();
        let mut copy_records = BTreeMap::<String, ParsedMail>::new();
        for raw_copy in persisted_copies.remove(&source_id).unwrap_or_default() {
            if cancellation.load(Ordering::Relaxed) {
                return None;
            }
            let copy_id = raw_copy.id.clone();
            let Some((_, target_folder)) = local_copy_parts(&copy_id) else {
                continue;
            };
            if metadata.deleted.contains(&copy_id) || target_folder == message.folder_id {
                continue;
            }

            let mut copy = message.clone();
            copy.id = copy_id.clone();
            copy.folder_id = metadata
                .folder_overrides
                .get(&copy_id)
                .cloned()
                .unwrap_or_else(|| target_folder.to_string());
            copy.folder_name = copy.folder_id.clone();
            copy.backend_location = None;
            if let Some(state) = metadata.states.get(&copy_id) {
                copy.is_unread = state.is_unread;
                copy.is_flagged = state.is_flagged;
                copy.is_important = state.is_important;
            }
            if let Some(labels) = metadata.labels.get(&copy_id) {
                copy.labels = sorted_read_only_labels(labels);
            }
            if let Some(follow_up) = metadata.follow_up.get(&copy_id) {
                copy.follow_up_state = *follow_up;
            }
            copy_records.insert(copy_id, copy);
        }

        if !copy_folders.is_empty() {
            for folder_id in copy_folders {
                if cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                if folder_id == message.folder_id {
                    continue;
                }
                let mut copy = message.clone();
                copy.id = format!("{source_id}::copy::{folder_id}");
                if metadata.deleted.contains(&copy.id) || copy_records.contains_key(&copy.id) {
                    continue;
                }
                copy.folder_id = folder_id.clone();
                copy.folder_name = folder_id;
                copy.backend_location = None;
                copy_records.insert(copy.id.clone(), copy);
            }
        }
        visible_messages.push(message);
        copied_messages.extend(copy_records.into_values());
    }

    visible_messages.extend(copied_messages);
    Some(visible_messages)
}

#[derive(Debug, Default)]
struct ReadOnlyMailMetadata {
    states: HashMap<String, MailMessageState>,
    folder_overrides: HashMap<String, String>,
    deleted: HashSet<String>,
    labels: HashMap<String, HashSet<String>>,
    follow_up: HashMap<String, MailFollowUpState>,
}

impl ReadOnlyMailMetadata {
    fn load(store: &MailMetadataStore) -> Self {
        Self {
            states: store.load(),
            folder_overrides: store.load_folder_overrides(),
            deleted: store.load_deleted(),
            labels: store.load_labels(),
            // Search filters only inspect the three-state value.  Keep that
            // view stable while the sidecar retains text and due metadata.
            follow_up: store
                .load_follow_up()
                .into_iter()
                .map(|(id, metadata)| (id, metadata.state))
                .collect(),
        }
    }
}

fn local_copy_parts(id: &str) -> Option<(&str, &str)> {
    let (source_id, folder_id) = id.split_once("::copy::")?;
    (!source_id.is_empty() && !folder_id.is_empty()).then_some((source_id, folder_id))
}

fn sorted_read_only_labels(labels: &HashSet<String>) -> Vec<String> {
    let mut labels = labels.iter().cloned().collect::<Vec<_>>();
    labels.sort_by_key(|value| value.to_ascii_lowercase());
    labels
}

fn mail_filter_matches(rule: &MailFilterRule, message: &ParsedMail) -> bool {
    let body = format!(
        "{} {}",
        message.preview_snippet,
        html_to_plain_text(&message.html_body)
    );
    let sender = format!("{} <{}>", message.sender_name, message.sender_email);
    let recipient = format!(
        "{} {} {}",
        message.recipient_to, message.recipient_cc, message.recipient_bcc
    );
    let headers = format!(
        "From: {}\nTo: {}\nCc: {}\nBcc: {}\nReply-To: {}\nSubject: {}\nMessage-ID: {}\nIn-Reply-To: {}\nReferences: {}",
        sender,
        message.recipient_to,
        message.recipient_cc,
        message.recipient_bcc,
        message.reply_to,
        message.subject,
        message.message_id.as_deref().unwrap_or_default(),
        message.in_reply_to.join(" "),
        message.references.join(" "),
    );

    rule.matches_with_context(
        &message.subject,
        &sender,
        &recipient,
        &body,
        &headers,
        &message.labels,
        FilterMessageContext {
            date: &message.date_str,
            folder: &message.folder_id,
            is_unread: message.is_unread,
            is_flagged: message.is_flagged,
            is_important: message.is_important,
            is_junk: message.folder_id == "junk" || message.folder_id.starts_with("junk/"),
            has_attachment: !message.attachments.is_empty(),
        },
    )
}

fn local_filter_folder_id(folder: &str) -> String {
    let normalized = folder.trim().trim_start_matches('.');
    match normalized.to_ascii_lowercase().as_str() {
        "inbox" => "inbox".to_string(),
        "sent" | "sent-mail" => "sent".to_string(),
        "draft" | "drafts" => "drafts".to_string(),
        "trash" | "deleted" => "trash".to_string(),
        "archive" | "all-mail" => "archive".to_string(),
        "junk" | "spam" => "junk".to_string(),
        _ if normalized.starts_with("custom:") => normalized.to_string(),
        _ => format!("custom:{normalized}"),
    }
}

#[derive(Debug, Clone)]
struct SearchDocument {
    result: SearchResultItem,
    searchable_text: String,
    fields: SearchDocumentFields,
}

impl SearchDocument {
    fn new(result: SearchResultItem, searchable_text: impl Into<String>) -> Self {
        let fields = SearchDocumentFields::from_result(&result);
        Self {
            result,
            searchable_text: searchable_text.into(),
            fields,
        }
    }

    fn with_fields(mut self, fields: SearchDocumentFields) -> Self {
        self.fields = fields;
        self
    }

    fn from_mail_record(record: &MailSearchRecord) -> Self {
        Self {
            result: SearchResultItem {
                domain: SearchDomain::Mail,
                title: String::new(),
                snippet: String::new(),
                date: String::new(),
                target: None,
            },
            searchable_text: record.searchable_text.clone(),
            fields: SearchDocumentFields::from_mail_record(record),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SearchDocumentFields {
    from: String,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    labels: String,
    filenames: String,
    folder: String,
    reply_to: String,
    message_id: String,
    location: String,
    is_unread: Option<bool>,
    is_flagged: Option<bool>,
    is_important: Option<bool>,
    is_junk: Option<bool>,
    has_attachment: Option<bool>,
}

impl SearchDocumentFields {
    fn from_result(result: &SearchResultItem) -> Self {
        Self {
            subject: result.title.clone(),
            body: result.snippet.clone(),
            ..Self::default()
        }
    }

    fn from_mail_record(record: &MailSearchRecord) -> Self {
        Self {
            from: record.from.clone(),
            to: record.to.clone(),
            cc: record.cc.clone(),
            bcc: record.bcc.clone(),
            subject: record.subject.clone(),
            body: record.body.clone(),
            labels: record.labels.clone(),
            filenames: record.filenames.clone(),
            folder: record.folder.clone(),
            reply_to: record.reply_to.clone(),
            message_id: record.message_id.clone(),
            location: record.location.clone(),
            is_unread: record.is_unread,
            is_flagged: record.is_flagged,
            is_important: record.is_important,
            is_junk: record.is_junk,
            has_attachment: record.has_attachment,
        }
    }
}

/// Provider-neutral mail snapshot consumed by the canonical query matcher.
///
/// Providers should populate only data available for the message snapshot.
/// Optional state/date values are deliberately tri-state: an unavailable
/// value fails closed for positive and negative predicates alike.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MailSearchRecord {
    pub(crate) searchable_text: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) labels: String,
    pub(crate) filenames: String,
    pub(crate) folder: String,
    pub(crate) reply_to: String,
    pub(crate) message_id: String,
    pub(crate) location: String,
    pub(crate) date: Option<NaiveDate>,
    pub(crate) is_unread: Option<bool>,
    pub(crate) is_flagged: Option<bool>,
    pub(crate) is_important: Option<bool>,
    pub(crate) is_junk: Option<bool>,
    pub(crate) has_attachment: Option<bool>,
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
        let cancellation = AtomicBool::new(false);
        Self::query_cancelable(domain, query_str, &cancellation)
    }

    /// Synchronous convenience query which captures local-store inputs on the
    /// calling thread.  UI workers must capture a [`SearchStoreSnapshot`] on
    /// the main thread and call [`Self::query_cancelable_with_stores`] instead.
    /// The cancellation flag is checked between stores and during the
    /// potentially large local-mail load.
    pub fn query_cancelable(
        domain: SearchDomain,
        query_str: &str,
        cancellation: &AtomicBool,
    ) -> Vec<SearchResultItem> {
        if !matches!(parse_search_query(query_str), Ok(Some(_))) {
            return Vec::new();
        }
        let stores = SearchStoreSnapshot::capture_on_main_thread();
        Self::query_cancelable_with_stores(domain, query_str, cancellation, stores)
    }

    /// Query explicit local-store inputs while allowing a UI worker to
    /// abandon an obsolete scan.
    ///
    /// The caller owns the snapshot/configuration and must capture it before
    /// spawning the worker.  This function intentionally does not construct
    /// any settings-backed store and does not resolve environment or
    /// application-data paths.  All local data loads remain here so large
    /// mailboxes and filesystem-backed calendar/contact stores stay off the
    /// GTK main loop; the captured stores retain their original writeability
    /// and path/source semantics.
    pub fn query_cancelable_with_stores(
        domain: SearchDomain,
        query_str: &str,
        cancellation: &AtomicBool,
        stores: SearchStoreSnapshot,
    ) -> Vec<SearchResultItem> {
        let Ok(Some(query)) = parse_search_query(query_str) else {
            return Vec::new();
        };

        let SearchStoreSnapshot {
            mail,
            mail_metadata,
            filter_store: _filter_store,
            filter_rules,
            raw_mail_filter_store,
            calendar: calendar_store,
            contacts: contact_store,
        } = stores;
        let mut documents = Vec::new();
        let contact_source_uid = if contact_store.is_writable() {
            LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string()
        } else {
            String::new()
        };

        if matches!(domain, SearchDomain::Mail | SearchDomain::All) {
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let messages =
                mail.load_local_records_cancelable_with_store(cancellation, &raw_mail_filter_store);
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let Some(messages) =
                apply_read_only_mail_filters(messages, &filter_rules, &mail_metadata, cancellation)
            else {
                return Vec::new();
            };
            documents.extend(messages.into_iter().map(|message| {
                let snippet = local_mail_search_text(&message);
                let searchable_text = format!(
                    "{} {} {}",
                    local_mail_search_index(&message),
                    message.subject,
                    message.date_str
                );
                let fields = local_mail_search_fields(&message);
                SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Mail,
                        title: message.subject,
                        snippet,
                        date: message.date_str,
                        target: Some(SearchTarget::Mail {
                            local_id: message.id,
                        }),
                    },
                    searchable_text,
                )
                .with_fields(fields)
            }));
        }

        if matches!(domain, SearchDomain::Calendar | SearchDomain::All) {
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let calendar_snapshot = calendar_store.load_snapshot();
            for event in calendar_snapshot.events {
                if cancellation.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                let title = event.summary;
                let snippet = format!(
                    "{} {} {} {}",
                    event.location, event.description, event.source_display_name, event.component
                );
                let date = event.start;
                documents.push(SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Calendar,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: date.clone(),
                        target: Some(SearchTarget::Event {
                            source_uid: event.source_uid,
                            component_uid: event.uid,
                        }),
                    },
                    format!("{title} {snippet} {date}"),
                ));
            }
            for task in calendar_snapshot.tasks {
                if cancellation.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                let title = task.summary;
                let snippet = format!(
                    "Aufgabe {} Priorität {} {} {}",
                    task.source_display_name, task.priority, task.due, task.component
                );
                let date = task.due;
                documents.push(SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Calendar,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: date.clone(),
                        target: Some(SearchTarget::Task {
                            source_uid: task.source_uid,
                            component_uid: task.uid,
                        }),
                    },
                    format!("{title} {snippet} {date}"),
                ));
            }
            for memo in calendar_snapshot.memos {
                if cancellation.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                let title = memo.summary;
                let snippet = format!(
                    "{} {} {}",
                    memo.description, memo.source_display_name, memo.component
                );
                let date = memo.updated;
                documents.push(SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Calendar,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: date.clone(),
                        target: Some(SearchTarget::Memo {
                            source_uid: memo.source_uid,
                            component_uid: memo.uid,
                        }),
                    },
                    format!("{title} {snippet} {date}"),
                ));
            }
        }

        if matches!(domain, SearchDomain::Contacts | SearchDomain::All) {
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
            for record in contact_store.load_contacts() {
                if cancellation.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                let title = record.contact.fn_name.clone();
                let snippet = record.contact.searchable_text();
                documents.push(SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Contacts,
                        title: title.clone(),
                        snippet: snippet.clone(),
                        date: String::new(),
                        target: Some(SearchTarget::Contact {
                            source_uid: contact_source_uid.clone(),
                            contact_uid: record.id,
                        }),
                    },
                    format!("{title} {snippet}"),
                ));
            }
        }

        if cancellation.load(Ordering::Relaxed) {
            return Vec::new();
        }

        let results = Self::query_search_documents_with_query(domain, &query, &documents);
        if cancellation.load(Ordering::Relaxed) {
            Vec::new()
        } else {
            results
        }
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
        let Ok(Some(query)) = parse_search_query(query_str) else {
            return Vec::new();
        };
        let documents = documents
            .into_iter()
            .map(|document| {
                SearchDocument::new(
                    document.clone(),
                    format!("{} {} {}", document.title, document.snippet, document.date),
                )
            })
            .collect::<Vec<_>>();

        Self::query_search_documents_with_query(domain, &query, &documents)
    }

    /// Search a snapshot from the real local mail reader.  This adapter keeps
    /// the result format independent from GTK and is ready to be replaced by
    /// CamelFolderSearch for synchronized accounts.
    pub fn query_local_mail<'a, I>(query_str: &str, messages: I) -> Vec<SearchResultItem>
    where
        I: IntoIterator<Item = &'a crate::services::mail_store_service::ParsedMail>,
    {
        let Ok(Some(query)) = parse_search_query(query_str) else {
            return Vec::new();
        };
        let documents: Vec<SearchDocument> = messages
            .into_iter()
            .map(|message| {
                let snippet = local_mail_search_text(message);
                let fields = local_mail_search_fields(message);
                SearchDocument::new(
                    SearchResultItem {
                        domain: SearchDomain::Mail,
                        title: message.subject.clone(),
                        snippet: snippet.clone(),
                        date: message.date_str.clone(),
                        target: Some(SearchTarget::Mail {
                            local_id: message.id.clone(),
                        }),
                    },
                    format!(
                        "{} {} {}",
                        local_mail_search_index(message),
                        message.subject,
                        message.date_str
                    ),
                )
                .with_fields(fields)
            })
            .collect();
        Self::query_search_documents_with_query(SearchDomain::Mail, &query, &documents)
    }

    fn query_search_documents_with_query(
        domain: SearchDomain,
        query: &SearchQuery,
        documents: &[SearchDocument],
    ) -> Vec<SearchResultItem> {
        documents
            .iter()
            .filter(|document| {
                if domain != SearchDomain::All && document.result.domain != domain {
                    return false;
                }
                let document_date = result_date(&document.result.date);
                search_query_matches(&query, document, document_date)
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchQuery {
    All(Vec<SearchQuery>),
    Any(Vec<SearchQuery>),
    Not(Box<SearchQuery>),
    Term(SearchQueryTerm),
}

/// Opaque parsed free-form mail query exposed to provider adapters.
///
/// The AST stays private so callers cannot accidentally depend on parser
/// implementation details or construct queries which have not passed the
/// strict parser.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedMailSearchQuery(SearchQuery);

/// Version of the provider-neutral mail-query fixture format.
///
/// This is intentionally separate from the parser's textual syntax version:
/// saved-search consumers persist the fixture as a typed AST and must reject
/// a future wire shape instead of attempting a lossy best-effort migration.
pub(crate) const MAIL_QUERY_FIXTURE_VERSION: u32 = 1;
const MAIL_QUERY_FIXTURE_FORMAT: &str = "evolution-mail-search-query";
const MAIL_QUERY_FIXTURE_MAX_BYTES: usize = 256 * 1024;
const MAIL_QUERY_FIXTURE_MAX_DEPTH: usize = 64;
const MAIL_QUERY_FIXTURE_MAX_NODES: usize = 1024;
const MAIL_QUERY_FIXTURE_MAX_TEXT_CHARS: usize = 16 * 1024;
const MAIL_QUERY_FIXTURE_MAX_TOTAL_TEXT_CHARS: usize = 64 * 1024;

/// Errors returned by the versioned mail-query fixture codec.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailSearchFixtureError {
    InvalidJson(String),
    InvalidShape(String),
    UnknownFormat(String),
    UnsupportedVersion(u32),
    SizeLimitExceeded(&'static str),
}

impl fmt::Display for MailSearchFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(message) => {
                write!(formatter, "invalid mail-search fixture JSON: {message}")
            }
            Self::InvalidShape(message) => {
                write!(formatter, "invalid mail-search fixture shape: {message}")
            }
            Self::UnknownFormat(format) => {
                write!(formatter, "unknown mail-search fixture format: {format}")
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported mail-search fixture version: {version}"
                )
            }
            Self::SizeLimitExceeded(limit) => {
                write!(formatter, "mail-search fixture exceeds {limit} limit")
            }
        }
    }
}

impl std::error::Error for MailSearchFixtureError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MailQueryFixtureV1 {
    format: String,
    version: u32,
    query: MailQueryFixtureNode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum MailQueryFixtureNode {
    And {
        children: Vec<MailQueryFixtureNode>,
    },
    Or {
        children: Vec<MailQueryFixtureNode>,
    },
    Not {
        child: Box<MailQueryFixtureNode>,
    },
    Term {
        value: String,
        negated: bool,
        predicate: MailQueryFixturePredicate,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum MailQueryFixturePredicate {
    Text {
        field: MailQueryFixtureField,
        matcher: MailQueryFixtureMatcher,
    },
    State {
        state: MailQueryFixtureState,
    },
    DateAtOrAfter {
        date: String,
        relative: bool,
    },
    DateAtOrBefore {
        date: String,
        relative: bool,
    },
    DateAfter {
        date: String,
        relative: bool,
    },
    DateBefore {
        date: String,
        relative: bool,
    },
    DateOn {
        date: String,
        relative: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MailQueryFixtureField {
    Any,
    From,
    To,
    Cc,
    Bcc,
    Recipients,
    Subject,
    Body,
    Label,
    Filename,
    Folder,
    ReplyTo,
    MessageId,
    Location,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MailQueryFixtureMatcher {
    Contains,
    HasWords,
    Exact,
    StartsWith,
    EndsWith,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MailQueryFixtureState {
    Unread,
    Flagged,
    Important,
    Junk,
    Attachment,
    Draft,
    Deleted,
    Sent,
    Inbox,
    Archive,
}

#[derive(Debug, Default)]
struct MailQueryFixtureLimits {
    nodes: usize,
    total_text_chars: usize,
}

impl MailQueryFixtureLimits {
    fn visit_node(&mut self, depth: usize) -> Result<(), MailSearchFixtureError> {
        if depth > MAIL_QUERY_FIXTURE_MAX_DEPTH {
            return Err(MailSearchFixtureError::SizeLimitExceeded(
                "maximum AST depth",
            ));
        }
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(MailSearchFixtureError::SizeLimitExceeded(
                "maximum AST nodes",
            ))?;
        if self.nodes > MAIL_QUERY_FIXTURE_MAX_NODES {
            return Err(MailSearchFixtureError::SizeLimitExceeded(
                "maximum AST nodes",
            ));
        }
        Ok(())
    }

    fn visit_text(&mut self, value: &str) -> Result<(), MailSearchFixtureError> {
        let chars = value.chars().count();
        if chars > MAIL_QUERY_FIXTURE_MAX_TEXT_CHARS {
            return Err(MailSearchFixtureError::SizeLimitExceeded(
                "maximum term text",
            ));
        }
        self.total_text_chars = self.total_text_chars.checked_add(chars).ok_or(
            MailSearchFixtureError::SizeLimitExceeded("maximum total term text"),
        )?;
        if self.total_text_chars > MAIL_QUERY_FIXTURE_MAX_TOTAL_TEXT_CHARS {
            return Err(MailSearchFixtureError::SizeLimitExceeded(
                "maximum total term text",
            ));
        }
        Ok(())
    }
}

/// Encode an already validated opaque mail query as a stable, versioned JSON
/// fixture.  The private wire representation is deliberately separate from
/// [`SearchQuery`], so callers cannot construct an unchecked AST by relying on
/// serialization details.
#[allow(dead_code)]
pub(crate) fn encode_mail_query_fixture(
    query: &ParsedMailSearchQuery,
) -> Result<String, MailSearchFixtureError> {
    let mut limits = MailQueryFixtureLimits::default();
    let query = encode_mail_query_node(&query.0, 1, &mut limits)?;
    let fixture = MailQueryFixtureV1 {
        format: MAIL_QUERY_FIXTURE_FORMAT.to_string(),
        version: MAIL_QUERY_FIXTURE_VERSION,
        query,
    };
    let encoded = serde_json::to_string_pretty(&fixture)
        .map_err(|error| MailSearchFixtureError::InvalidJson(error.to_string()))?;
    if encoded.len() > MAIL_QUERY_FIXTURE_MAX_BYTES {
        return Err(MailSearchFixtureError::SizeLimitExceeded(
            "maximum fixture size",
        ));
    }
    Ok(encoded)
}

/// Decode a stable mail-query fixture.  Unknown versions, enum values, wire
/// fields, malformed shapes, and resource-limit violations all fail closed.
#[allow(dead_code)]
pub(crate) fn decode_mail_query_fixture(
    fixture: &str,
) -> Result<ParsedMailSearchQuery, MailSearchFixtureError> {
    if fixture.len() > MAIL_QUERY_FIXTURE_MAX_BYTES {
        return Err(MailSearchFixtureError::SizeLimitExceeded(
            "maximum fixture size",
        ));
    }

    // Inspect the small envelope first so a future version is reported as
    // unsupported even when its payload shape is intentionally different.
    let value: serde_json::Value = serde_json::from_str(fixture)
        .map_err(|error| MailSearchFixtureError::InvalidJson(error.to_string()))?;
    let object = value.as_object().ok_or_else(|| {
        MailSearchFixtureError::InvalidShape("fixture root must be an object".to_string())
    })?;
    let format = object
        .get("format")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MailSearchFixtureError::InvalidShape("fixture format must be a string".to_string())
        })?;
    if format != MAIL_QUERY_FIXTURE_FORMAT {
        return Err(MailSearchFixtureError::UnknownFormat(format.to_string()));
    }
    let version = object
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            MailSearchFixtureError::InvalidShape("fixture version must be a u32".to_string())
        })?;
    if version != MAIL_QUERY_FIXTURE_VERSION {
        return Err(MailSearchFixtureError::UnsupportedVersion(version));
    }

    let fixture: MailQueryFixtureV1 = serde_json::from_value(value)
        .map_err(|error| MailSearchFixtureError::InvalidShape(error.to_string()))?;
    let mut limits = MailQueryFixtureLimits::default();
    let query = decode_mail_query_node(fixture.query, 1, &mut limits)?;
    Ok(ParsedMailSearchQuery(query))
}

fn encode_mail_query_node(
    query: &SearchQuery,
    depth: usize,
    limits: &mut MailQueryFixtureLimits,
) -> Result<MailQueryFixtureNode, MailSearchFixtureError> {
    limits.visit_node(depth)?;
    match query {
        SearchQuery::All(expressions) => {
            if expressions.len() < 2 {
                return Err(MailSearchFixtureError::InvalidShape(
                    "and node must contain at least two children".to_string(),
                ));
            }
            Ok(MailQueryFixtureNode::And {
                children: expressions
                    .iter()
                    .map(|expression| encode_mail_query_node(expression, depth + 1, limits))
                    .collect::<Result<_, _>>()?,
            })
        }
        SearchQuery::Any(expressions) => {
            if expressions.len() < 2 {
                return Err(MailSearchFixtureError::InvalidShape(
                    "or node must contain at least two children".to_string(),
                ));
            }
            Ok(MailQueryFixtureNode::Or {
                children: expressions
                    .iter()
                    .map(|expression| encode_mail_query_node(expression, depth + 1, limits))
                    .collect::<Result<_, _>>()?,
            })
        }
        SearchQuery::Not(expression) => Ok(MailQueryFixtureNode::Not {
            child: Box::new(encode_mail_query_node(expression, depth + 1, limits)?),
        }),
        SearchQuery::Term(term) => {
            limits.visit_text(&term.value)?;
            let predicate = encode_mail_query_predicate(&term.kind, &term.value, limits)?;
            Ok(MailQueryFixtureNode::Term {
                value: term.value.clone(),
                negated: term.negated,
                predicate,
            })
        }
    }
}

fn encode_mail_query_predicate(
    kind: &SearchTermKind,
    term_value: &str,
    limits: &mut MailQueryFixtureLimits,
) -> Result<MailQueryFixturePredicate, MailSearchFixtureError> {
    match kind {
        SearchTermKind::Text { field, matcher } => Ok(MailQueryFixturePredicate::Text {
            field: encode_mail_query_field(*field),
            matcher: encode_mail_query_matcher(*matcher),
        }),
        SearchTermKind::State(state) => Ok(MailQueryFixturePredicate::State {
            state: encode_mail_query_state(*state),
        }),
        SearchTermKind::DateAtOrAfter(date) => encode_mail_query_date(date, term_value, limits)
            .map(|(date, relative)| MailQueryFixturePredicate::DateAtOrAfter { date, relative }),
        SearchTermKind::DateAtOrBefore(date) => encode_mail_query_date(date, term_value, limits)
            .map(|(date, relative)| MailQueryFixturePredicate::DateAtOrBefore { date, relative }),
        SearchTermKind::DateAfter(date) => encode_mail_query_date(date, term_value, limits)
            .map(|(date, relative)| MailQueryFixturePredicate::DateAfter { date, relative }),
        SearchTermKind::DateBefore(date) => encode_mail_query_date(date, term_value, limits)
            .map(|(date, relative)| MailQueryFixturePredicate::DateBefore { date, relative }),
        SearchTermKind::DateOn(date) => encode_mail_query_date(date, term_value, limits)
            .map(|(date, relative)| MailQueryFixturePredicate::DateOn { date, relative }),
    }
}

fn encode_mail_query_date(
    date: &NaiveDate,
    term_value: &str,
    limits: &mut MailQueryFixtureLimits,
) -> Result<(String, bool), MailSearchFixtureError> {
    // Relative expressions must remain relative on disk.  Persisting the
    // resolved local date would freeze queries such as `after:7d` at the day
    // on which the search was saved.  Compact YYYYMMDD values are the one
    // absolute numeric form accepted by the general date parser; preserve
    // those as absolute dates for fixture-v1 compatibility.
    let normalized_relative = normalize_relative_mail_query_date(term_value);
    let (value, relative) = match normalized_relative {
        Some(value) if !is_compact_absolute_date(&value, date) => (value, true),
        _ => (date.format("%Y-%m-%d").to_string(), false),
    };
    limits.visit_text(&value)?;
    Ok((value, relative))
}

fn decode_mail_query_node(
    node: MailQueryFixtureNode,
    depth: usize,
    limits: &mut MailQueryFixtureLimits,
) -> Result<SearchQuery, MailSearchFixtureError> {
    limits.visit_node(depth)?;
    match node {
        MailQueryFixtureNode::And { children } => {
            if children.len() < 2 {
                return Err(MailSearchFixtureError::InvalidShape(
                    "and node must contain at least two children".to_string(),
                ));
            }
            Ok(SearchQuery::All(
                children
                    .into_iter()
                    .map(|child| decode_mail_query_node(child, depth + 1, limits))
                    .collect::<Result<_, _>>()?,
            ))
        }
        MailQueryFixtureNode::Or { children } => {
            if children.len() < 2 {
                return Err(MailSearchFixtureError::InvalidShape(
                    "or node must contain at least two children".to_string(),
                ));
            }
            Ok(SearchQuery::Any(
                children
                    .into_iter()
                    .map(|child| decode_mail_query_node(child, depth + 1, limits))
                    .collect::<Result<_, _>>()?,
            ))
        }
        MailQueryFixtureNode::Not { child } => Ok(SearchQuery::Not(Box::new(
            decode_mail_query_node(*child, depth + 1, limits)?,
        ))),
        MailQueryFixtureNode::Term {
            value,
            negated,
            predicate,
        } => {
            limits.visit_text(&value)?;
            let kind = decode_mail_query_predicate(predicate, &value, limits)?;
            if matches!(kind, SearchTermKind::State(_)) {
                if !value.is_empty() {
                    return Err(MailSearchFixtureError::InvalidShape(
                        "state term value must be empty".to_string(),
                    ));
                }
            } else if value.trim().is_empty() {
                return Err(MailSearchFixtureError::InvalidShape(
                    "text and date term values must not be empty".to_string(),
                ));
            }
            Ok(SearchQuery::Term(SearchQueryTerm {
                value,
                negated,
                kind,
            }))
        }
    }
}

fn decode_mail_query_predicate(
    predicate: MailQueryFixturePredicate,
    term_value: &str,
    limits: &mut MailQueryFixtureLimits,
) -> Result<SearchTermKind, MailSearchFixtureError> {
    match predicate {
        MailQueryFixturePredicate::Text { field, matcher } => Ok(SearchTermKind::Text {
            field: decode_mail_query_field(field),
            matcher: decode_mail_query_matcher(matcher),
        }),
        MailQueryFixturePredicate::State { state } => {
            Ok(SearchTermKind::State(decode_mail_query_state(state)))
        }
        MailQueryFixturePredicate::DateAtOrAfter { date, relative } => {
            decode_mail_query_date(date, relative, term_value, limits)
                .map(SearchTermKind::DateAtOrAfter)
        }
        MailQueryFixturePredicate::DateAtOrBefore { date, relative } => {
            decode_mail_query_date(date, relative, term_value, limits)
                .map(SearchTermKind::DateAtOrBefore)
        }
        MailQueryFixturePredicate::DateAfter { date, relative } => {
            decode_mail_query_date(date, relative, term_value, limits)
                .map(SearchTermKind::DateAfter)
        }
        MailQueryFixturePredicate::DateBefore { date, relative } => {
            decode_mail_query_date(date, relative, term_value, limits)
                .map(SearchTermKind::DateBefore)
        }
        MailQueryFixturePredicate::DateOn { date, relative } => {
            decode_mail_query_date(date, relative, term_value, limits).map(SearchTermKind::DateOn)
        }
    }
}

fn decode_mail_query_date(
    date: String,
    relative: bool,
    term_value: &str,
    limits: &mut MailQueryFixtureLimits,
) -> Result<NaiveDate, MailSearchFixtureError> {
    limits.visit_text(&date)?;
    let normalized_term = term_value.trim().to_ascii_lowercase();
    if relative {
        let Some(normalized_relative) = normalize_relative_mail_query_date(&normalized_term) else {
            return Err(MailSearchFixtureError::InvalidShape(
                "relative date flag does not match the term value".to_string(),
            ));
        };
        if date != normalized_relative {
            return Err(MailSearchFixtureError::InvalidShape(
                "relative date fixture expression does not match the term value".to_string(),
            ));
        }
        // Resolve the raw relative expression exactly once at decode time so
        // a saved search follows the current local day after a restart.
        parse_search_date_value(&normalized_relative, true).ok_or_else(|| {
            MailSearchFixtureError::InvalidShape(
                "relative date term value is not a valid search date".to_string(),
            )
        })
    } else {
        let parsed_date = NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|error| {
            MailSearchFixtureError::InvalidShape(format!("invalid date `{date}`: {error}"))
        })?;
        if normalize_relative_mail_query_date(&normalized_term)
            .is_some_and(|value| !is_compact_absolute_date(&value, &parsed_date))
        {
            return Err(MailSearchFixtureError::InvalidShape(
                "relative date term value is missing its relative flag".to_string(),
            ));
        }
        parse_search_date_value(term_value, false)
            .filter(|date| *date == parsed_date)
            .ok_or_else(|| {
                MailSearchFixtureError::InvalidShape(
                    "date term value does not match the encoded date".to_string(),
                )
            })
    }
}

fn normalize_relative_mail_query_date(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    if value
        .strip_suffix('d')
        .and_then(|days| days.parse::<i64>().ok())
        .is_some_and(|days| (-100_000_000..=100_000_000).contains(&days))
    {
        return Some(value);
    }
    if matches!(
        value.as_str(),
        "today"
            | "heute"
            | "yesterday"
            | "gestern"
            | "tomorrow"
            | "morgen"
            | "past-week"
            | "last-week"
            | "letzte-woche"
            | "past-month"
            | "last-month"
            | "letzter-monat"
            | "past-year"
            | "last-year"
            | "letztes-jahr"
    ) {
        return Some(value);
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|days| (-100_000_000..=100_000_000).contains(days))
        .map(|_| value)
}

fn is_compact_absolute_date(value: &str, encoded_date: &NaiveDate) -> bool {
    value.len() == 8
        && NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()
            .is_some_and(|date| date == *encoded_date)
}

fn encode_mail_query_field(field: SearchField) -> MailQueryFixtureField {
    match field {
        SearchField::Any => MailQueryFixtureField::Any,
        SearchField::From => MailQueryFixtureField::From,
        SearchField::To => MailQueryFixtureField::To,
        SearchField::Cc => MailQueryFixtureField::Cc,
        SearchField::Bcc => MailQueryFixtureField::Bcc,
        SearchField::Recipients => MailQueryFixtureField::Recipients,
        SearchField::Subject => MailQueryFixtureField::Subject,
        SearchField::Body => MailQueryFixtureField::Body,
        SearchField::Label => MailQueryFixtureField::Label,
        SearchField::Filename => MailQueryFixtureField::Filename,
        SearchField::Folder => MailQueryFixtureField::Folder,
        SearchField::ReplyTo => MailQueryFixtureField::ReplyTo,
        SearchField::MessageId => MailQueryFixtureField::MessageId,
        SearchField::Location => MailQueryFixtureField::Location,
    }
}

fn decode_mail_query_field(field: MailQueryFixtureField) -> SearchField {
    match field {
        MailQueryFixtureField::Any => SearchField::Any,
        MailQueryFixtureField::From => SearchField::From,
        MailQueryFixtureField::To => SearchField::To,
        MailQueryFixtureField::Cc => SearchField::Cc,
        MailQueryFixtureField::Bcc => SearchField::Bcc,
        MailQueryFixtureField::Recipients => SearchField::Recipients,
        MailQueryFixtureField::Subject => SearchField::Subject,
        MailQueryFixtureField::Body => SearchField::Body,
        MailQueryFixtureField::Label => SearchField::Label,
        MailQueryFixtureField::Filename => SearchField::Filename,
        MailQueryFixtureField::Folder => SearchField::Folder,
        MailQueryFixtureField::ReplyTo => SearchField::ReplyTo,
        MailQueryFixtureField::MessageId => SearchField::MessageId,
        MailQueryFixtureField::Location => SearchField::Location,
    }
}

fn encode_mail_query_matcher(matcher: SearchMatchKind) -> MailQueryFixtureMatcher {
    match matcher {
        SearchMatchKind::Contains => MailQueryFixtureMatcher::Contains,
        SearchMatchKind::HasWords => MailQueryFixtureMatcher::HasWords,
        SearchMatchKind::Exact => MailQueryFixtureMatcher::Exact,
        SearchMatchKind::StartsWith => MailQueryFixtureMatcher::StartsWith,
        SearchMatchKind::EndsWith => MailQueryFixtureMatcher::EndsWith,
    }
}

fn decode_mail_query_matcher(matcher: MailQueryFixtureMatcher) -> SearchMatchKind {
    match matcher {
        MailQueryFixtureMatcher::Contains => SearchMatchKind::Contains,
        MailQueryFixtureMatcher::HasWords => SearchMatchKind::HasWords,
        MailQueryFixtureMatcher::Exact => SearchMatchKind::Exact,
        MailQueryFixtureMatcher::StartsWith => SearchMatchKind::StartsWith,
        MailQueryFixtureMatcher::EndsWith => SearchMatchKind::EndsWith,
    }
}

fn encode_mail_query_state(state: SearchState) -> MailQueryFixtureState {
    match state {
        SearchState::Unread => MailQueryFixtureState::Unread,
        SearchState::Flagged => MailQueryFixtureState::Flagged,
        SearchState::Important => MailQueryFixtureState::Important,
        SearchState::Junk => MailQueryFixtureState::Junk,
        SearchState::Attachment => MailQueryFixtureState::Attachment,
        SearchState::Draft => MailQueryFixtureState::Draft,
        SearchState::Deleted => MailQueryFixtureState::Deleted,
        SearchState::Sent => MailQueryFixtureState::Sent,
        SearchState::Inbox => MailQueryFixtureState::Inbox,
        SearchState::Archive => MailQueryFixtureState::Archive,
    }
}

fn decode_mail_query_state(state: MailQueryFixtureState) -> SearchState {
    match state {
        MailQueryFixtureState::Unread => SearchState::Unread,
        MailQueryFixtureState::Flagged => SearchState::Flagged,
        MailQueryFixtureState::Important => SearchState::Important,
        MailQueryFixtureState::Junk => SearchState::Junk,
        MailQueryFixtureState::Attachment => SearchState::Attachment,
        MailQueryFixtureState::Draft => SearchState::Draft,
        MailQueryFixtureState::Deleted => SearchState::Deleted,
        MailQueryFixtureState::Sent => SearchState::Sent,
        MailQueryFixtureState::Inbox => SearchState::Inbox,
        MailQueryFixtureState::Archive => SearchState::Archive,
    }
}

/// Default field or upstream quick-search mode used for unqualified
/// mail-search terms.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MailSearchFieldScope {
    #[default]
    Any,
    Subject,
    Sender,
    Recipients,
    Body,
    /// Evolution's "Subject or Addresses contain" quick-search mode.
    /// Recipients deliberately means To/Cc, never Bcc.
    SubjectOrAddresses,
    /// Evolution's "Message contains" quick-search mode.
    /// Recipients deliberately means To/Cc, never Bcc.
    Message,
}

/// The only Graph search plans the portable parser is allowed to emit.
///
/// `SafePositiveConjunction` is deliberately narrower than the local
/// matcher: Graph receives it only when every canonical predicate has an
/// exact positive Contains representation.  Everything else stays typed as
/// `Unsupported`, so an adapter cannot silently turn a partial query into a
/// broader search.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailGraphSearchPlan {
    SafePositiveConjunction(String),
    Unsupported(String),
}

const MAIL_GRAPH_SEARCH_VALUE_MAX_CHARS: usize = 128;

impl ParsedMailSearchQuery {
    /// Compile this validated query to the strict portable Graph subset.
    ///
    /// The AST remains private: callers receive either the complete KQL
    /// conjunction or an explicit unsupported result and can never observe a
    /// partial clause list.
    #[allow(dead_code)]
    pub(crate) fn graph_search_plan(&self) -> MailGraphSearchPlan {
        let mut clauses = Vec::new();
        match append_mail_graph_clauses(&self.0, &mut clauses) {
            Ok(()) if !clauses.is_empty() => {
                MailGraphSearchPlan::SafePositiveConjunction(clauses.join(" AND "))
            }
            Ok(()) => MailGraphSearchPlan::Unsupported(
                "empty query cannot be represented by Graph search".to_string(),
            ),
            Err(reason) => MailGraphSearchPlan::Unsupported(reason),
        }
    }
}

fn append_mail_graph_clauses(query: &SearchQuery, clauses: &mut Vec<String>) -> Result<(), String> {
    match query {
        SearchQuery::All(expressions) => {
            if expressions.is_empty() {
                return Err("empty conjunction cannot be represented by Graph search".to_string());
            }
            for expression in expressions {
                append_mail_graph_clauses(expression, clauses)?;
            }
            Ok(())
        }
        SearchQuery::Any(_) => Err("OR expressions are unsupported by Graph search".to_string()),
        SearchQuery::Not(_) => {
            Err("negated expressions are unsupported by Graph search".to_string())
        }
        SearchQuery::Term(term) => {
            clauses.push(mail_graph_clause(term)?);
            Ok(())
        }
    }
}

fn mail_graph_clause(term: &SearchQueryTerm) -> Result<String, String> {
    if term.negated {
        return Err("negated terms are unsupported by Graph search".to_string());
    }

    match term.kind {
        SearchTermKind::Text { field, matcher } => {
            if matcher != SearchMatchKind::Contains {
                return Err("only Contains text matching is supported by Graph search".to_string());
            }
            let value = escape_mail_graph_value(&term.value)?;
            let field_name = match field {
                SearchField::Any => None,
                SearchField::From => Some("from"),
                SearchField::Recipients => Some("recipients"),
                SearchField::Cc => Some("cc"),
                SearchField::Bcc => Some("bcc"),
                SearchField::Subject => Some("subject"),
                SearchField::Body => Some("body"),
                SearchField::Filename => Some("attachment"),
                SearchField::To
                | SearchField::Label
                | SearchField::Folder
                | SearchField::ReplyTo
                | SearchField::MessageId
                | SearchField::Location => {
                    return Err("local-only or unsupported field in Graph search".to_string());
                }
            };
            Ok(match field_name {
                Some(field_name) => format!("\"{field_name}:{value}\""),
                None => format!("\"{value}\""),
            })
        }
        SearchTermKind::State(SearchState::Attachment) => Ok("\"hasAttachments:true\"".to_string()),
        SearchTermKind::State(_) => {
            Err("local state predicates are unsupported by Graph search".to_string())
        }
        SearchTermKind::DateAtOrAfter(_)
        | SearchTermKind::DateAtOrBefore(_)
        | SearchTermKind::DateAfter(_)
        | SearchTermKind::DateBefore(_)
        | SearchTermKind::DateOn(_) => {
            Err("date predicates are unsupported by Graph search".to_string())
        }
    }
}

fn escape_mail_graph_value(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("empty value cannot be represented by Graph search".to_string());
    }
    if value.chars().count() > MAIL_GRAPH_SEARCH_VALUE_MAX_CHARS {
        return Err("Graph search value exceeds the supported length".to_string());
    }
    if value.contains(['\r', '\n']) {
        return Err("Graph search value contains a line break".to_string());
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchField {
    Any,
    From,
    To,
    Cc,
    Bcc,
    Recipients,
    Subject,
    Body,
    Label,
    Filename,
    Folder,
    ReplyTo,
    MessageId,
    Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMatchKind {
    Contains,
    HasWords,
    Exact,
    StartsWith,
    EndsWith,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchState {
    Unread,
    Flagged,
    Important,
    Junk,
    Attachment,
    Draft,
    Deleted,
    Sent,
    Inbox,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchTermKind {
    Text {
        field: SearchField,
        matcher: SearchMatchKind,
    },
    DateAtOrAfter(NaiveDate),
    DateAtOrBefore(NaiveDate),
    DateAfter(NaiveDate),
    DateBefore(NaiveDate),
    DateOn(NaiveDate),
    State(SearchState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchQueryParseError {
    EmptyTerm,
    UnterminatedQuote,
    DanglingEscape,
    UnexpectedClosingParenthesis,
    MissingClosingParenthesis,
    EmptyGroup,
    InvalidSyntax(String),
    UnknownOperator(String),
    UnsupportedOperator(String),
    InvalidValue(String),
}

impl fmt::Display for SearchQueryParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTerm => formatter.write_str("Suchbegriff darf nicht leer sein"),
            Self::UnterminatedQuote => {
                formatter.write_str("Suchanfrage enthält ein ungeschlossenes Anführungszeichen")
            }
            Self::DanglingEscape => {
                formatter.write_str("Suchanfrage enthält einen unvollständigen Escape")
            }
            Self::UnexpectedClosingParenthesis => {
                formatter.write_str("Suchanfrage enthält eine unerwartete schließende Klammer")
            }
            Self::MissingClosingParenthesis => {
                formatter.write_str("Suchanfrage enthält eine nicht geschlossene Klammer")
            }
            Self::EmptyGroup => formatter.write_str("Suchanfrage enthält eine leere Gruppe"),
            Self::InvalidSyntax(message) => write!(formatter, "Ungültige Suchsyntax: {message}"),
            Self::UnknownOperator(operator) => {
                write!(formatter, "Unbekannter Suchoperator „{operator}“")
            }
            Self::UnsupportedOperator(operator) => {
                write!(formatter, "Nicht unterstützter Suchoperator „{operator}“")
            }
            Self::InvalidValue(value) => write!(formatter, "Ungültiger Suchwert „{value}“"),
        }
    }
}

impl std::error::Error for SearchQueryParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchWord {
    text: String,
    unquoted_colon: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchToken {
    Word(SearchWord),
    OpenParen,
    CloseParen,
}

// This is the portable subset of Evolution's free-form expression language.
// Known operators are parsed into typed predicates; unsupported or malformed
// expressions fail closed so a typo cannot become an accidental broad text
// search. Native CamelFolderSearch/CamelSExp execution remains a separate
// backend adapter and is intentionally not simulated here.
fn parse_search_query(input: &str) -> Result<Option<SearchQuery>, SearchQueryParseError> {
    parse_search_query_with_scope(input, None)
}

/// Parse a mail query while applying `default_field` only to bare terms.
/// Explicit field/state/date operators always retain their canonical meaning.
fn parse_search_query_with_scope(
    input: &str,
    default_field: Option<SearchField>,
) -> Result<Option<SearchQuery>, SearchQueryParseError> {
    if input.trim().is_empty() {
        return Ok(None);
    }

    let tokens = search_tokens(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }

    let mut cursor = 0;
    let expressions = parse_query_sequence(&tokens, &mut cursor, false, default_field)?;
    if cursor != tokens.len() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "unexpected tokens after search expression".to_string(),
        ));
    }
    Ok(combine_all(expressions))
}

/// Parse the portable Evolution-compatible mail query language.
///
/// Empty input produces `Ok(None)`, while malformed or unsupported syntax
/// returns an error so callers can fail closed instead of broadening a search.
#[allow(dead_code)]
pub(crate) fn parse_mail_query(
    input: &str,
    scope: MailSearchFieldScope,
) -> Result<Option<ParsedMailSearchQuery>, SearchQueryParseError> {
    let default_field = match scope {
        MailSearchFieldScope::Any => SearchField::Any,
        MailSearchFieldScope::Subject => SearchField::Subject,
        MailSearchFieldScope::Sender => SearchField::From,
        MailSearchFieldScope::Recipients => SearchField::Recipients,
        MailSearchFieldScope::Body => SearchField::Body,
        // Parse combined quick-search modes from the neutral Any field first,
        // then expand only unqualified terms below. Explicit field operators
        // retain their canonical meaning.
        MailSearchFieldScope::SubjectOrAddresses | MailSearchFieldScope::Message => {
            SearchField::Any
        }
    };
    parse_search_query_with_scope(input, Some(default_field)).map(|query| {
        query.map(|query| {
            let query = match scope {
                MailSearchFieldScope::SubjectOrAddresses => expand_unqualified_mail_fields(
                    query,
                    &[
                        SearchField::Subject,
                        SearchField::From,
                        SearchField::Recipients,
                    ],
                ),
                MailSearchFieldScope::Message => expand_unqualified_mail_fields(
                    query,
                    &[
                        SearchField::Subject,
                        SearchField::From,
                        SearchField::Recipients,
                        SearchField::Body,
                    ],
                ),
                MailSearchFieldScope::Any
                | MailSearchFieldScope::Subject
                | MailSearchFieldScope::Sender
                | MailSearchFieldScope::Recipients
                | MailSearchFieldScope::Body => query,
            };
            ParsedMailSearchQuery(query)
        })
    })
}

/// Expand a bare term into one of Evolution's multi-field quick-search
/// presets. Explicit fields and state/date predicates are left untouched.
/// Negated bare terms become `NOT (field1 OR field2 ...)`, preserving the
/// intuitive meaning of excluding a value from the whole preset.
fn expand_unqualified_mail_fields(query: SearchQuery, fields: &[SearchField]) -> SearchQuery {
    match query {
        SearchQuery::All(expressions) => SearchQuery::All(
            expressions
                .into_iter()
                .map(|expression| expand_unqualified_mail_fields(expression, fields))
                .collect(),
        ),
        SearchQuery::Any(expressions) => SearchQuery::Any(
            expressions
                .into_iter()
                .map(|expression| expand_unqualified_mail_fields(expression, fields))
                .collect(),
        ),
        SearchQuery::Not(expression) => SearchQuery::Not(Box::new(expand_unqualified_mail_fields(
            *expression,
            fields,
        ))),
        SearchQuery::Term(term) => {
            let SearchTermKind::Text { field, matcher } = term.kind else {
                return SearchQuery::Term(term);
            };
            if field != SearchField::Any || matcher != SearchMatchKind::Contains {
                return SearchQuery::Term(term);
            }

            let negated = term.negated;
            let alternatives = SearchQuery::Any(
                fields
                    .iter()
                    .copied()
                    .map(|field| {
                        SearchQuery::Term(SearchQueryTerm {
                            value: term.value.clone(),
                            negated: false,
                            kind: SearchTermKind::Text {
                                field,
                                matcher: SearchMatchKind::Contains,
                            },
                        })
                    })
                    .collect(),
            );
            if negated {
                SearchQuery::Not(Box::new(alternatives))
            } else {
                alternatives
            }
        }
    }
}

/// Validate a saved/free-form search without exposing or duplicating the AST
/// parser at the caller.
pub(crate) fn validate_search_syntax(input: &str) -> Result<(), SearchQueryParseError> {
    if input.trim().is_empty() {
        return Err(SearchQueryParseError::EmptyTerm);
    }
    parse_search_query(input).map(|_| ())
}

fn parse_query_sequence(
    tokens: &[SearchToken],
    cursor: &mut usize,
    expect_closing_parenthesis: bool,
    default_field: Option<SearchField>,
) -> Result<Vec<SearchQuery>, SearchQueryParseError> {
    let mut expressions = Vec::new();

    loop {
        let Some(token) = tokens.get(*cursor) else {
            if expect_closing_parenthesis {
                return Err(SearchQueryParseError::MissingClosingParenthesis);
            }
            break;
        };

        if matches!(token, SearchToken::CloseParen) {
            if expect_closing_parenthesis {
                *cursor += 1;
                break;
            }
            return Err(SearchQueryParseError::UnexpectedClosingParenthesis);
        }

        if let Some(expression) = parse_query_atom(tokens, cursor, default_field)? {
            expressions.push(expression);
        }
    }

    Ok(expressions)
}

fn parse_query_atom(
    tokens: &[SearchToken],
    cursor: &mut usize,
    default_field: Option<SearchField>,
) -> Result<Option<SearchQuery>, SearchQueryParseError> {
    match tokens.get(*cursor) {
        Some(SearchToken::OpenParen) => {
            *cursor += 1;
            let expressions = parse_query_sequence(tokens, cursor, true, default_field)?;
            combine_all(expressions)
                .ok_or(SearchQueryParseError::EmptyGroup)
                .map(Some)
        }
        Some(SearchToken::Word(word)) => {
            let group = word.text.to_ascii_lowercase();
            if matches!(group.as_str(), "and:" | "or:" | "not:") {
                if !matches!(tokens.get(*cursor + 1), Some(SearchToken::OpenParen)) {
                    return Err(SearchQueryParseError::InvalidSyntax(format!(
                        "{group} must be followed by a parenthesized expression"
                    )));
                }
                *cursor += 2;
                let expressions = parse_query_sequence(tokens, cursor, true, default_field)?;
                let expression = match group.as_str() {
                    "and:" => combine_all(expressions).ok_or(SearchQueryParseError::EmptyGroup)?,
                    "or:" => {
                        if expressions.is_empty() {
                            return Err(SearchQueryParseError::EmptyGroup);
                        }
                        if expressions.len() == 1 {
                            expressions
                                .into_iter()
                                .next()
                                .expect("length checked above")
                        } else {
                            SearchQuery::Any(expressions)
                        }
                    }
                    "not:" => SearchQuery::Not(Box::new(
                        combine_all(expressions).ok_or(SearchQueryParseError::EmptyGroup)?,
                    )),
                    _ => unreachable!(),
                };
                return Ok(Some(expression));
            }

            if word.text.ends_with(':')
                && matches!(tokens.get(*cursor + 1), Some(SearchToken::OpenParen))
            {
                return Err(SearchQueryParseError::UnknownOperator(
                    word.text.trim_end_matches(':').to_string(),
                ));
            }

            *cursor += 1;
            normalize_search_query_term(word, default_field).map(|term| term.map(SearchQuery::Term))
        }
        Some(SearchToken::CloseParen) => Err(SearchQueryParseError::UnexpectedClosingParenthesis),
        None => Err(SearchQueryParseError::InvalidSyntax(
            "missing search expression".to_string(),
        )),
    }
}

fn combine_all(mut expressions: Vec<SearchQuery>) -> Option<SearchQuery> {
    match expressions.len() {
        0 => None,
        1 => Some(expressions.pop().expect("length checked above")),
        _ => Some(SearchQuery::All(expressions)),
    }
}

fn normalize_search_query_term(
    token: &SearchWord,
    default_field: Option<SearchField>,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    if token.text.is_empty() {
        return Err(SearchQueryParseError::EmptyTerm);
    }

    let mut offset = 0;
    let mut negated = false;
    if token.text.starts_with('-') && token.text.len() > 1 {
        offset = 1;
        negated = true;
    }

    let token_text = &token.text[offset..];
    let separator = token
        .unquoted_colon
        .filter(|separator| *separator >= offset)
        .map(|separator| separator - offset);

    let Some(separator) = separator else {
        if token_text.is_empty() {
            return Err(SearchQueryParseError::EmptyTerm);
        }
        return Ok(Some(SearchQueryTerm {
            value: token_text.to_lowercase(),
            negated,
            kind: SearchTermKind::Text {
                field: default_field.unwrap_or(SearchField::Any),
                matcher: SearchMatchKind::Contains,
            },
        }));
    };

    let raw_key = &token_text[..separator];
    let raw_value = token_text[separator + 1..].trim();
    if raw_key.is_empty() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "search operator has no name".to_string(),
        ));
    }
    if raw_value.is_empty() {
        return Err(SearchQueryParseError::InvalidValue(raw_key.to_string()));
    }

    let mut key = raw_key.to_ascii_lowercase();
    if let Some(stripped) = key.strip_prefix("not-") {
        if stripped.is_empty() {
            return Err(SearchQueryParseError::InvalidSyntax(
                "empty negated search operator".to_string(),
            ));
        }
        key = stripped.to_string();
        negated = !negated;
    }

    let (base_key, option) = split_search_key_option(&key);
    let value = raw_value.to_lowercase();

    match base_key {
        "not" => parse_not_term(&value, negated, option),
        "is" => parse_is_term(&value, negated, option),
        "has" | "no" => parse_attachment_operator(base_key, &value, negated, option),
        "flag" => parse_flag_term(&value, negated, option),
        "attachment" | "a" => parse_attachment_value(&value, negated, option),
        "on" | "date" | "after" | "newer" | "before" | "older" | "sent" | "received" | "rcv" => {
            parse_date_term(base_key, option, raw_value, negated)
        }
        "any" | "from" | "f" | "sender" | "to" | "t" | "cc" | "c" | "bcc" | "recipient"
        | "recips" | "r" | "recipients" | "subject" | "s" | "body" | "b" | "content" | "label"
        | "l" | "tag" | "keyword" | "filename" | "name" | "folder" | "in" | "reply-to"
        | "replyto" | "mid" | "message-id" | "location" | "m" => {
            parse_field_term(base_key, option, value, negated)
        }
        "list" | "header" | "h" | "exists" | "e" | "size" | "sz" | "score" | "sc" => {
            Err(SearchQueryParseError::UnsupportedOperator(key))
        }
        _ => Err(SearchQueryParseError::UnknownOperator(key)),
    }
}

fn split_search_key_option(key: &str) -> (&str, Option<&str>) {
    // Matching options form a small closed set (`-no`, `-has-words`, `-regex`,
    // …) while the base field itself may contain hyphens (`reply-to`,
    // `message-id`).  Split at the first hyphen whose remainder is a known
    // option; with no such suffix the whole key is the field name.
    const MATCHER_OPTIONS: &[&str] = &[
        "no",
        "contains",
        "c",
        "has-words",
        "w",
        "matches",
        "m",
        "starts-with",
        "sw",
        "ends-with",
        "ew",
        "regex",
        "r",
        "full-regex",
        "fr",
        "soundex",
        "se",
        "=",
        "<",
        ">", // Evolution date comparisons: sent-=:d, sent-<:d, sent->:d
    ];
    for (idx, ch) in key.char_indices() {
        if ch != '-' {
            continue;
        }
        let rest = &key[idx + 1..];
        if MATCHER_OPTIONS.contains(&rest) {
            return (&key[..idx], Some(rest));
        }
    }
    (key, None)
}

fn parse_not_term(
    value: &str,
    negated: bool,
    option: Option<&str>,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    if option.is_some() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "not does not accept a matching option".to_string(),
        ));
    }

    match value {
        "attachment" | "attachments" => Ok(Some(state_term(SearchState::Attachment, !negated))),
        "unread" => Ok(Some(state_term(SearchState::Unread, !negated))),
        "read" | "seen" => Ok(Some(state_term(SearchState::Unread, negated))),
        "flagged" | "starred" => Ok(Some(state_term(SearchState::Flagged, !negated))),
        "important" => Ok(Some(state_term(SearchState::Important, !negated))),
        "junk" | "spam" => Ok(Some(state_term(SearchState::Junk, !negated))),
        _ => Ok(Some(SearchQueryTerm {
            value: value.to_string(),
            negated: !negated,
            kind: SearchTermKind::Text {
                field: SearchField::Any,
                matcher: SearchMatchKind::Contains,
            },
        })),
    }
}

fn parse_is_term(
    value: &str,
    negated: bool,
    option: Option<&str>,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    if option.is_some() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "is does not accept a matching option".to_string(),
        ));
    }

    let (value, value_negated) = value
        .strip_prefix("not-")
        .map(|value| (value, true))
        .unwrap_or((value, false));
    let negated = negated ^ value_negated;

    let state = match value {
        "unread" => Some(state_term(SearchState::Unread, negated)),
        "read" | "seen" => Some(state_term(SearchState::Unread, !negated)),
        "starred" | "flagged" => Some(state_term(SearchState::Flagged, negated)),
        "important" => Some(state_term(SearchState::Important, negated)),
        "junk" | "spam" => Some(state_term(SearchState::Junk, negated)),
        "attachment" | "attachments" | "has-attachment" => {
            Some(state_term(SearchState::Attachment, negated))
        }
        "no-attachment" | "no-attachments" => Some(state_term(SearchState::Attachment, !negated)),
        "all" | "all-mail" if !negated => None,
        "inbox" => Some(state_term(SearchState::Inbox, negated)),
        "sent" | "sent-mail" => Some(state_term(SearchState::Sent, negated)),
        "draft" | "drafts" => Some(state_term(SearchState::Draft, negated)),
        "trash" | "deleted" => Some(state_term(SearchState::Deleted, negated)),
        "archive" => Some(state_term(SearchState::Archive, negated)),
        _ => {
            return Err(SearchQueryParseError::UnknownOperator(format!(
                "is:{value}"
            )));
        }
    };

    Ok(state)
}

fn parse_attachment_operator(
    key: &str,
    value: &str,
    negated: bool,
    option: Option<&str>,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    if option.is_some() {
        return Err(SearchQueryParseError::InvalidSyntax(format!(
            "{key} does not accept a matching option"
        )));
    }
    if !matches!(value, "attachment" | "attachments") {
        return Err(SearchQueryParseError::InvalidValue(format!(
            "{key}:{value}"
        )));
    }
    Ok(Some(state_term(
        SearchState::Attachment,
        negated ^ (key == "no"),
    )))
}

fn parse_attachment_value(
    value: &str,
    negated: bool,
    option: Option<&str>,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    if option.is_some() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "attachment does not accept a matching option".to_string(),
        ));
    }
    let has_attachment = match value {
        "1" | "true" | "yes" | "y" | "on" | "attachment" | "attachments" | "any" => true,
        "0" | "false" | "no" | "n" | "off" | "none" | "no-attachment" => false,
        _ => {
            return Err(SearchQueryParseError::InvalidValue(format!(
                "attachment:{value}"
            )));
        }
    };
    Ok(Some(state_term(
        SearchState::Attachment,
        negated ^ !has_attachment,
    )))
}

fn parse_flag_term(
    value: &str,
    negated: bool,
    option: Option<&str>,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    if option.is_some() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "flag does not accept a matching option".to_string(),
        ));
    }
    let term = match value {
        "seen" | "read" => state_term(SearchState::Unread, !negated),
        "flagged" | "important" => state_term(
            if value == "flagged" {
                SearchState::Flagged
            } else {
                SearchState::Important
            },
            negated,
        ),
        "attachment" | "attachments" => state_term(SearchState::Attachment, negated),
        "deleted" => state_term(SearchState::Deleted, negated),
        "draft" => state_term(SearchState::Draft, negated),
        "sent" => state_term(SearchState::Sent, negated),
        _ => {
            return Err(SearchQueryParseError::UnsupportedOperator(format!(
                "flag:{value}"
            )));
        }
    };
    Ok(Some(term))
}

fn parse_date_term(
    key: &str,
    option: Option<&str>,
    raw_value: &str,
    negated: bool,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    let relative_days_allowed = matches!(key, "sent" | "received" | "rcv");
    let Some(date) = parse_search_date_value(raw_value, relative_days_allowed) else {
        return Err(SearchQueryParseError::InvalidValue(format!(
            "{key}:{raw_value}"
        )));
    };

    let kind = match (key, option) {
        ("on" | "date", None | Some("=")) => SearchTermKind::DateOn(date),
        ("on" | "date", Some(">")) => SearchTermKind::DateAfter(date),
        ("on" | "date", Some("<")) => SearchTermKind::DateBefore(date),
        ("after" | "newer", None) => SearchTermKind::DateAtOrAfter(date),
        ("after" | "newer", Some(">")) => SearchTermKind::DateAfter(date),
        ("after" | "newer", Some("=")) => SearchTermKind::DateOn(date),
        ("after" | "newer", Some("<")) => SearchTermKind::DateBefore(date),
        ("before" | "older", None) => SearchTermKind::DateAtOrBefore(date),
        ("before" | "older", Some("<")) => SearchTermKind::DateBefore(date),
        ("before" | "older", Some("=")) => SearchTermKind::DateOn(date),
        ("before" | "older", Some(">")) => SearchTermKind::DateAfter(date),
        ("sent" | "received" | "rcv", None) => SearchTermKind::DateAtOrAfter(date),
        ("sent" | "received" | "rcv", Some(">")) => SearchTermKind::DateAfter(date),
        ("sent" | "received" | "rcv", Some("=")) => SearchTermKind::DateOn(date),
        ("sent" | "received" | "rcv", Some("<")) => SearchTermKind::DateBefore(date),
        (_, None) => return Err(SearchQueryParseError::UnsupportedOperator(key.to_string())),
        (_, Some(option)) => {
            return Err(SearchQueryParseError::UnsupportedOperator(format!(
                "{key}-{option}"
            )));
        }
    };

    Ok(Some(SearchQueryTerm {
        value: raw_value.trim().to_lowercase(),
        negated,
        kind,
    }))
}

fn parse_field_term(
    key: &str,
    option: Option<&str>,
    value: String,
    negated: bool,
) -> Result<Option<SearchQueryTerm>, SearchQueryParseError> {
    let field = match key {
        "any" => SearchField::Any,
        "from" | "f" | "sender" => SearchField::From,
        "to" | "t" => SearchField::To,
        "cc" | "c" => SearchField::Cc,
        "bcc" => SearchField::Bcc,
        "recipient" | "recips" | "r" | "recipients" => SearchField::Recipients,
        "subject" | "s" => SearchField::Subject,
        "body" | "b" | "content" => SearchField::Body,
        "label" | "l" | "tag" | "keyword" => SearchField::Label,
        "filename" | "name" => SearchField::Filename,
        "folder" | "in" => SearchField::Folder,
        "reply-to" | "replyto" => SearchField::ReplyTo,
        "mid" | "message-id" => SearchField::MessageId,
        "location" | "m" => SearchField::Location,
        _ => return Err(SearchQueryParseError::UnknownOperator(key.to_string())),
    };

    let default_matcher = if matches!(field, SearchField::MessageId) {
        SearchMatchKind::Exact
    } else {
        SearchMatchKind::Contains
    };
    let (matcher, option_negated) = parse_matcher_option(option, default_matcher, key)?;
    Ok(Some(SearchQueryTerm {
        value,
        negated: negated ^ option_negated,
        kind: SearchTermKind::Text { field, matcher },
    }))
}

fn parse_matcher_option(
    option: Option<&str>,
    default: SearchMatchKind,
    key: &str,
) -> Result<(SearchMatchKind, bool), SearchQueryParseError> {
    match option {
        None => Ok((default, false)),
        Some("no") => Ok((default, true)),
        Some("contains" | "c") => Ok((SearchMatchKind::Contains, false)),
        Some("has-words" | "w") => Ok((SearchMatchKind::HasWords, false)),
        Some("matches" | "m") => Ok((SearchMatchKind::Exact, false)),
        Some("starts-with" | "sw") => Ok((SearchMatchKind::StartsWith, false)),
        Some("ends-with" | "ew") => Ok((SearchMatchKind::EndsWith, false)),
        Some("regex" | "r" | "full-regex" | "fr" | "soundex" | "se") => {
            Err(SearchQueryParseError::UnsupportedOperator(format!(
                "{key}-{}",
                option.expect("matching option checked above")
            )))
        }
        Some(option) => Err(SearchQueryParseError::UnsupportedOperator(format!(
            "{key}-{option}"
        ))),
    }
}

fn state_term(state: SearchState, negated: bool) -> SearchQueryTerm {
    SearchQueryTerm {
        value: String::new(),
        negated,
        kind: SearchTermKind::State(state),
    }
}

fn parse_search_date_value(value: &str, bare_days_allowed: bool) -> Option<NaiveDate> {
    let value = value.trim();
    if bare_days_allowed {
        if let Ok(days) = value.parse::<i64>() {
            return relative_search_date(days);
        }
    }
    parse_search_date(value)
}

fn parse_search_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if let Some(days) = value
        .strip_suffix('d')
        .and_then(|value| value.parse::<i64>().ok())
    {
        return relative_search_date(days);
    }

    let value_lowercase = value.to_ascii_lowercase();
    let relative_days = match value_lowercase.as_str() {
        "today" | "heute" => Some(0),
        "yesterday" | "gestern" => Some(1),
        "tomorrow" | "morgen" => Some(-1),
        "past-week" | "last-week" | "letzte-woche" => Some(7),
        "past-month" | "last-month" | "letzter-monat" => Some(30),
        "past-year" | "last-year" | "letztes-jahr" => Some(365),
        _ => None,
    };
    if let Some(days) = relative_days {
        return relative_search_date(days);
    }

    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
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

fn relative_search_date(days_ago: i64) -> Option<NaiveDate> {
    if !(-100_000_000..=100_000_000).contains(&days_ago) {
        return None;
    }
    Local::now()
        .date_naive()
        .checked_sub_signed(chrono::Duration::days(days_ago))
}

fn result_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    parse_search_date(value).or_else(|| {
        value
            .split_once(':')
            .and_then(|(_, value)| parse_search_date(value))
    })
}

/// Match an opaque parsed query against a provider-owned mail snapshot.
#[allow(dead_code)]
pub(crate) fn matches_mail_query(query: &ParsedMailSearchQuery, record: &MailSearchRecord) -> bool {
    let document = SearchDocument::from_mail_record(record);
    search_query_matches(&query.0, &document, record.date)
}

/// Return whether evaluating `query` requires message-body text.
///
/// A bare term searches the complete portable message index and therefore
/// potentially includes the body. Negation and groups preserve that need.
#[allow(dead_code)]
pub(crate) fn mail_query_needs_body(query: &ParsedMailSearchQuery) -> bool {
    fn expression_needs_body(query: &SearchQuery) -> bool {
        match query {
            SearchQuery::All(expressions) | SearchQuery::Any(expressions) => {
                expressions.iter().any(expression_needs_body)
            }
            SearchQuery::Not(expression) => expression_needs_body(expression),
            SearchQuery::Term(term) => matches!(
                term.kind,
                SearchTermKind::Text {
                    field: SearchField::Any | SearchField::Body,
                    ..
                }
            ),
        }
    }

    expression_needs_body(&query.0)
}

fn search_query_matches(
    query: &SearchQuery,
    document: &SearchDocument,
    document_date: Option<NaiveDate>,
) -> bool {
    search_query_truth(query, document, document_date).unwrap_or(false)
}

/// Evaluate the query with Kleene's three-valued boolean algebra.
///
/// `None` means that a required provider value was unavailable. Keeping that
/// state through groups is important: `NOT unknown` must remain unknown rather
/// than becoming an accidental match. The public bool matcher unwraps only at
/// the outer boundary and therefore fails closed.
fn search_query_truth(
    query: &SearchQuery,
    document: &SearchDocument,
    document_date: Option<NaiveDate>,
) -> Option<bool> {
    match query {
        SearchQuery::All(expressions) => {
            let mut unknown = false;
            for expression in expressions {
                match search_query_truth(expression, document, document_date) {
                    Some(false) => return Some(false),
                    Some(true) => {}
                    None => unknown = true,
                }
            }
            if unknown { None } else { Some(true) }
        }
        SearchQuery::Any(expressions) => {
            let mut unknown = false;
            for expression in expressions {
                match search_query_truth(expression, document, document_date) {
                    Some(true) => return Some(true),
                    Some(false) => {}
                    None => unknown = true,
                }
            }
            if unknown { None } else { Some(false) }
        }
        SearchQuery::Not(expression) => {
            search_query_truth(expression, document, document_date).map(|matches| !matches)
        }
        SearchQuery::Term(term) => search_term_matches(term, document, document_date),
    }
}

fn search_term_matches(
    term: &SearchQueryTerm,
    document: &SearchDocument,
    document_date: Option<NaiveDate>,
) -> Option<bool> {
    if let SearchTermKind::State(state) = term.kind {
        return search_state_matches(document, state)
            .map(|matches| if term.negated { !matches } else { matches });
    }

    let matches = match term.kind {
        SearchTermKind::Text { field, matcher } => {
            let field_value = search_field_value(document, field);
            Some(search_text_matches(
                field_value.as_ref(),
                &term.value,
                matcher,
            ))
        }
        SearchTermKind::DateAtOrAfter(date) => {
            document_date.map(|document_date| document_date >= date)
        }
        SearchTermKind::DateAtOrBefore(date) => {
            document_date.map(|document_date| document_date <= date)
        }
        SearchTermKind::DateAfter(date) => document_date.map(|document_date| document_date > date),
        SearchTermKind::DateBefore(date) => document_date.map(|document_date| document_date < date),
        SearchTermKind::DateOn(date) => document_date.map(|document_date| document_date == date),
        SearchTermKind::State(_) => unreachable!("state terms are handled above"),
    };
    matches.map(|matches| if term.negated { !matches } else { matches })
}

fn search_field_value<'a>(document: &'a SearchDocument, field: SearchField) -> Cow<'a, str> {
    match field {
        SearchField::Any => Cow::Borrowed(&document.searchable_text),
        SearchField::From => Cow::Borrowed(&document.fields.from),
        SearchField::To => Cow::Borrowed(&document.fields.to),
        SearchField::Cc => Cow::Borrowed(&document.fields.cc),
        SearchField::Bcc => Cow::Borrowed(&document.fields.bcc),
        SearchField::Recipients => {
            Cow::Owned(format!("{} {}", document.fields.to, document.fields.cc))
        }
        SearchField::Subject => Cow::Borrowed(&document.fields.subject),
        SearchField::Body => Cow::Borrowed(&document.fields.body),
        SearchField::Label => Cow::Borrowed(&document.fields.labels),
        SearchField::Filename => Cow::Borrowed(&document.fields.filenames),
        SearchField::Folder => Cow::Borrowed(&document.fields.folder),
        SearchField::ReplyTo => Cow::Borrowed(&document.fields.reply_to),
        SearchField::MessageId => Cow::Borrowed(&document.fields.message_id),
        SearchField::Location => Cow::Borrowed(&document.fields.location),
    }
}

fn search_text_matches(value: &str, needle: &str, matcher: SearchMatchKind) -> bool {
    let value = value.to_lowercase();
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return false;
    }
    match matcher {
        SearchMatchKind::Contains => value.contains(&needle),
        SearchMatchKind::HasWords => needle
            .split_whitespace()
            .all(|word| contains_whole_word(&value, word)),
        SearchMatchKind::Exact => value.trim() == needle.trim(),
        SearchMatchKind::StartsWith => value.trim_start().starts_with(needle.trim()),
        SearchMatchKind::EndsWith => value.trim_end().ends_with(needle.trim()),
    }
}

fn contains_whole_word(value: &str, needle: &str) -> bool {
    value.match_indices(needle).any(|(index, _)| {
        let before_is_boundary = value[..index]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphanumeric());
        let end = index + needle.len();
        let after_is_boundary = value[end..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_alphanumeric());
        before_is_boundary && after_is_boundary
    })
}

fn search_state_matches(document: &SearchDocument, state: SearchState) -> Option<bool> {
    match state {
        SearchState::Unread => document.fields.is_unread,
        SearchState::Flagged => document.fields.is_flagged,
        SearchState::Important => document.fields.is_important,
        SearchState::Junk => document.fields.is_junk,
        SearchState::Attachment => document.fields.has_attachment,
        SearchState::Draft => folder_state_matches(&document.fields.folder, SearchState::Draft),
        SearchState::Deleted => folder_state_matches(&document.fields.folder, SearchState::Deleted),
        SearchState::Sent => folder_state_matches(&document.fields.folder, SearchState::Sent),
        SearchState::Inbox => folder_state_matches(&document.fields.folder, SearchState::Inbox),
        SearchState::Archive => folder_state_matches(&document.fields.folder, SearchState::Archive),
    }
}

fn folder_state_matches(folder: &str, state: SearchState) -> Option<bool> {
    let folder = folder.trim().to_ascii_lowercase();
    if folder.is_empty() {
        return None;
    }
    let matches = match state {
        SearchState::Draft => folder_alias_matches(&folder, &["draft", "drafts"]),
        SearchState::Deleted => folder_alias_matches(&folder, &["trash", "deleted"]),
        SearchState::Sent => folder_alias_matches(&folder, &["sent", "sent-mail"]),
        SearchState::Inbox => folder_alias_matches(&folder, &["inbox"]),
        SearchState::Archive => folder_alias_matches(&folder, &["archive"]),
        _ => false,
    };
    Some(matches)
}

fn folder_alias_matches(folder: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| {
        folder == *alias
            || folder.ends_with(&format!("/{alias}"))
            || folder.ends_with(&format!(":{alias}"))
            || folder.contains(&format!(" {alias}"))
    })
}

fn search_tokens(input: &str) -> Result<Vec<SearchToken>, SearchQueryParseError> {
    let characters = input.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut unquoted_colon = None;
    let mut token_started = false;
    let mut quoted = false;
    let mut index = 0;

    let flush_word = |tokens: &mut Vec<SearchToken>,
                      current: &mut String,
                      unquoted_colon: &mut Option<usize>,
                      token_started: &mut bool| {
        if *token_started {
            tokens.push(SearchToken::Word(SearchWord {
                text: std::mem::take(current),
                unquoted_colon: unquoted_colon.take(),
            }));
            *token_started = false;
        }
    };

    while index < characters.len() {
        let character = characters[index];
        if character == '\\' {
            let Some(escaped) = characters.get(index + 1).copied() else {
                return Err(SearchQueryParseError::DanglingEscape);
            };
            current.push(escaped);
            token_started = true;
            index += 2;
            continue;
        }

        if character == '"' {
            if quoted && characters.get(index + 1) == Some(&'"') {
                current.push('"');
                token_started = true;
                index += 2;
                continue;
            }
            quoted = !quoted;
            token_started = true;
            index += 1;
            continue;
        }

        if !quoted && character.is_whitespace() {
            flush_word(
                &mut tokens,
                &mut current,
                &mut unquoted_colon,
                &mut token_started,
            );
            index += 1;
            continue;
        }

        if !quoted && character == '(' {
            flush_word(
                &mut tokens,
                &mut current,
                &mut unquoted_colon,
                &mut token_started,
            );
            tokens.push(SearchToken::OpenParen);
            index += 1;
            continue;
        }
        if !quoted && character == ')' {
            flush_word(
                &mut tokens,
                &mut current,
                &mut unquoted_colon,
                &mut token_started,
            );
            tokens.push(SearchToken::CloseParen);
            index += 1;
            continue;
        }

        if !quoted && character == ':' && unquoted_colon.is_none() {
            unquoted_colon = Some(current.len());
        }
        current.push(character);
        token_started = true;
        index += 1;
    }

    if quoted {
        return Err(SearchQueryParseError::UnterminatedQuote);
    }
    flush_word(
        &mut tokens,
        &mut current,
        &mut unquoted_colon,
        &mut token_started,
    );
    Ok(tokens)
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
        html_to_plain_text(&message.html_body),
        message.labels.join(" "),
        format!("{} {}", attachment_names, state_terms),
    )
}

fn local_mail_search_fields(
    message: &crate::services::mail_store_service::ParsedMail,
) -> SearchDocumentFields {
    let body = format!(
        "{} {}",
        message.preview_snippet,
        html_to_plain_text(&message.html_body)
    );
    let filenames = message
        .attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let folder = format!("{} {}", message.folder_name, message.folder_id);
    let location = message
        .backend_location
        .as_ref()
        .map(|location| {
            format!(
                "{} {} {} {}",
                location.source_uid,
                location.folder_name,
                location.message_uid,
                location.message_id.as_deref().unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let message_id = message
        .message_id
        .clone()
        .unwrap_or_else(|| message.id.clone());
    let folder_lower = folder.to_ascii_lowercase();

    SearchDocumentFields {
        from: format!("{} {}", message.sender_name, message.sender_email),
        to: message.recipient_to.clone(),
        cc: message.recipient_cc.clone(),
        bcc: message.recipient_bcc.clone(),
        subject: message.subject.clone(),
        body,
        labels: message.labels.join(" "),
        filenames,
        folder,
        reply_to: message.reply_to.clone(),
        message_id,
        location,
        is_unread: Some(message.is_unread),
        is_flagged: Some(message.is_flagged),
        is_important: Some(message.is_important),
        is_junk: Some(folder_alias_matches(&folder_lower, &["junk", "spam"])),
        has_attachment: Some(!message.attachments.is_empty()),
    }
}

fn local_mail_search_index(message: &crate::services::mail_store_service::ParsedMail) -> String {
    let attachment_names = message
        .attachments
        .iter()
        .map(|attachment| attachment.filename.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let state_terms = local_mail_state_terms(message);
    let field_terms = [
        prefixed_search_terms("from", &message.sender_email),
        prefixed_search_terms("to", &message.recipient_to),
        prefixed_search_terms("cc", &message.recipient_cc),
        prefixed_search_terms("bcc", &message.recipient_bcc),
        prefixed_search_terms("reply-to", &message.reply_to),
        prefixed_search_terms("folder", &message.folder_name),
        prefixed_search_terms("in", &message.folder_id),
        prefixed_search_terms("subject", &message.subject),
        prefixed_search_terms(
            "body",
            &format!(
                "{} {}",
                message.preview_snippet,
                html_to_plain_text(&message.html_body)
            ),
        ),
        prefixed_search_terms("label", &message.labels.join(" ")),
        prefixed_search_terms("filename", &attachment_names),
    ]
    .into_iter()
    .filter(|terms| !terms.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    format!(
        "from:{} {} to:{} cc:{} bcc:{} reply-to:{} folder:{} in:{} subject:{} body:{} label:{} filename:{} {} {}",
        message.sender_email,
        message.sender_name,
        message.recipient_to,
        message.recipient_cc,
        message.recipient_bcc,
        message.reply_to,
        message.folder_name,
        message.folder_id,
        message.subject,
        format!(
            "{} {}",
            message.preview_snippet,
            html_to_plain_text(&message.html_body)
        ),
        message.labels.join(" "),
        attachment_names,
        state_terms,
        field_terms,
    )
}

fn prefixed_search_terms(prefix: &str, value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| format!("{prefix}:{part}"))
        .collect::<Vec<_>>()
        .join(" ")
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
    use std::fs;
    use std::path::PathBuf;

    fn snapshot_with_stores(
        mail: MailStoreService,
        filter_store: MailFilterStore,
        calendar: CalendarStoreService,
        contacts: ContactStoreService,
    ) -> SearchStoreSnapshot {
        let filter_rules = filter_store.load();
        let mail_metadata = MailMetadataStore::new(mail.metadata_path());
        SearchStoreSnapshot {
            mail,
            mail_metadata,
            filter_store,
            filter_rules,
            raw_mail_filter_store: MailFilterStore::new(PathBuf::new()),
            calendar,
            contacts,
        }
    }

    fn test_store_snapshot(
        calendar: CalendarStoreService,
        contacts: ContactStoreService,
    ) -> SearchStoreSnapshot {
        snapshot_with_stores(
            MailStoreService::with_mail_dir_and_state_path(PathBuf::new(), PathBuf::new()),
            MailFilterStore::new(PathBuf::new()),
            calendar,
            contacts,
        )
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "evolution-gtk4-search-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).expect("temporary search directory should be created");
        path
    }

    fn fixture_mail(
        id: &str,
        folder_id: &str,
        date: &str,
        sender_name: &str,
        sender_email: &str,
        recipient_to: &str,
        recipient_cc: &str,
        subject: &str,
        body: &str,
        is_unread: bool,
        is_flagged: bool,
        is_important: bool,
        labels: &[&str],
        attachment_filename: Option<&str>,
    ) -> crate::services::mail_store_service::ParsedMail {
        let attachments = attachment_filename
            .map(|filename| {
                vec![crate::models::mail_message::AttachmentInfo {
                    filename: filename.to_string(),
                    size_formatted: "1 KiB".to_string(),
                    mime_type: "application/octet-stream".to_string(),
                    content_id: None,
                    content: Vec::new().into(),
                }]
            })
            .unwrap_or_default();
        crate::services::mail_store_service::ParsedMail {
            id: id.to_string(),
            folder_id: folder_id.to_string(),
            folder_name: folder_id.to_string(),
            backend_location: None,
            sender_name: sender_name.to_string(),
            sender_email: sender_email.to_string(),
            recipient_to: recipient_to.to_string(),
            recipient_cc: recipient_cc.to_string(),
            recipient_bcc: String::new(),
            reply_to: "reply@example.org".to_string(),
            subject: subject.to_string(),
            message_id: Some(format!("{id}@example.org")),
            in_reply_to: Vec::new(),
            references: Vec::new(),
            thread_id: id.to_string(),
            thread_size: 1,
            thread_position: 0,
            date_str: date.to_string(),
            message_size: 0,
            preview_snippet: body.to_string(),
            html_body: format!("<p>{body}</p>"),
            raw_headers: None,
            raw_mime: None,
            is_unread,
            is_replied: false,
            is_forwarded: false,
            is_flagged,
            is_important,
            follow_up_state: crate::services::mail_metadata_service::MailFollowUpState::None,
            follow_up: crate::services::mail_metadata_service::MailFollowUpMetadata::default(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            attachments,
            is_preview_only: false,
        }
    }

    fn fixture_messages() -> [crate::services::mail_store_service::ParsedMail; 2] {
        [
            fixture_mail(
                "m-inbox",
                "inbox",
                "2026-08-10",
                "Alice Example",
                "alice@example.org",
                "Bob Example <bob@example.org>",
                "team@example.org",
                "Project update",
                "Green blue release notes are ready",
                true,
                false,
                true,
                &["work"],
                Some("report.pdf"),
            ),
            fixture_mail(
                "m-sent",
                "sent",
                "2026-08-05",
                "Bob Example",
                "bob@example.org",
                "Alice Example <alice@example.org>",
                "",
                "Status report",
                "The deployment is complete",
                false,
                true,
                false,
                &["personal"],
                None,
            ),
        ]
    }

    fn target_id(result: &SearchResultItem) -> &str {
        match result
            .target
            .as_ref()
            .expect("fixture result should have a target")
        {
            SearchTarget::Mail { local_id } => local_id,
            SearchTarget::Event { component_uid, .. }
            | SearchTarget::Task { component_uid, .. }
            | SearchTarget::Memo { component_uid, .. } => component_uid,
            SearchTarget::Contact { contact_uid, .. } => contact_uid,
        }
    }

    #[test]
    fn empty_global_queries_are_empty() {
        let results = GlobalSearchEngine::query(SearchDomain::All, "");
        assert!(results.is_empty());
    }

    #[test]
    fn typed_search_targets_keep_component_kind_and_delimiters() {
        let source_uid = "source:with&separator".to_string();
        let component_uid = "uid:with&separator".to_string();
        let targets = [
            SearchTarget::Event {
                source_uid: source_uid.clone(),
                component_uid: component_uid.clone(),
            },
            SearchTarget::Task {
                source_uid: source_uid.clone(),
                component_uid: component_uid.clone(),
            },
            SearchTarget::Memo {
                source_uid,
                component_uid,
            },
        ];

        assert_eq!(
            targets[0].clone(),
            SearchTarget::Event {
                source_uid: "source:with&separator".to_string(),
                component_uid: "uid:with&separator".to_string(),
            }
        );
        assert_ne!(targets[0], targets[1]);
        assert_ne!(targets[1], targets[2]);
    }

    #[test]
    fn search_filters_explicit_documents() {
        let documents = vec![
            SearchResultItem {
                domain: SearchDomain::Calendar,
                title: "Team Meeting".to_string(),
                snippet: "10:00".to_string(),
                date: "2026-08-10".to_string(),
                target: Some(SearchTarget::Event {
                    source_uid: "source".to_string(),
                    component_uid: "event-1".to_string(),
                }),
            },
            SearchResultItem {
                domain: SearchDomain::Mail,
                title: "Invoice".to_string(),
                snippet: "Finance".to_string(),
                date: "2026-08-09".to_string(),
                target: Some(SearchTarget::Mail {
                    local_id: "message-1".to_string(),
                }),
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
                target: Some(SearchTarget::Contact {
                    source_uid: "source".to_string(),
                    contact_uid: "contact-1".to_string(),
                }),
            },
            SearchResultItem {
                domain: SearchDomain::Mail,
                title: "Ada".to_string(),
                snippet: "Lovelace".to_string(),
                date: String::new(),
                target: Some(SearchTarget::Mail {
                    local_id: "message-1".to_string(),
                }),
            },
        ];

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::Contacts, "ada lovelace", &documents);
        assert_eq!(results.len(), 1);
        assert_eq!(target_id(&results[0]), "contact-1");
    }

    #[test]
    fn search_supports_date_predicates_across_domains() {
        let documents = vec![
            SearchResultItem {
                domain: SearchDomain::Calendar,
                title: "Team Meeting".to_string(),
                snippet: "Room 1".to_string(),
                date: "2026-08-10 12:00".to_string(),
                target: Some(SearchTarget::Event {
                    source_uid: "source".to_string(),
                    component_uid: "event-1".to_string(),
                }),
            },
            SearchResultItem {
                domain: SearchDomain::Mail,
                title: "Invoice".to_string(),
                snippet: "Finance".to_string(),
                date: "2026-08-11".to_string(),
                target: Some(SearchTarget::Mail {
                    local_id: "message-1".to_string(),
                }),
            },
            SearchResultItem {
                domain: SearchDomain::Contacts,
                title: "Ada Lovelace".to_string(),
                snippet: "Analytical Engine".to_string(),
                date: String::new(),
                target: Some(SearchTarget::Contact {
                    source_uid: "source".to_string(),
                    contact_uid: "contact-1".to_string(),
                }),
            },
        ];

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::All, "on:2026-08-10", &documents);
        assert_eq!(results.len(), 1);
        assert_eq!(target_id(&results[0]), "event-1");

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::All, "after:2026-08-10", &documents);
        assert_eq!(
            results.iter().map(target_id).collect::<Vec<_>>(),
            vec!["event-1", "message-1"]
        );

        let results = GlobalSearchEngine::query_documents(
            SearchDomain::All,
            "older:2026-08-10 -on:2026-08-11",
            &documents,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(target_id(&results[0]), "event-1");

        let results =
            GlobalSearchEngine::query_documents(SearchDomain::All, "-after:2026-08-10", &documents);
        assert!(
            results.is_empty(),
            "missing document dates must remain unknown for negated predicates"
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
            message_size: 0,
            preview_snippet: "The project is ready".to_string(),
            html_body: "<p>The project is ready</p>".to_string(),
            raw_headers: None,
            raw_mime: None,
            is_unread: true,
            is_replied: false,
            is_forwarded: false,
            is_flagged: false,
            is_important: false,
            follow_up_state: crate::services::mail_metadata_service::MailFollowUpState::None,
            follow_up: crate::services::mail_metadata_service::MailFollowUpMetadata::default(),
            labels: Vec::new(),
            attachments: Vec::new(),
            is_preview_only: false,
        }];

        let results = GlobalSearchEngine::query_local_mail("project", &messages);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].target,
            Some(SearchTarget::Mail {
                local_id: "local:Inbox#0".to_string(),
            })
        );

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

    #[test]
    fn evolution_field_aliases_and_match_options_are_evaluated_locally() {
        let messages = fixture_messages();

        for query in [
            "f:alice@example.org",
            "from:alice@example.org",
            "t:bob@example.org",
            "r:team@example.org",
            "s-has-words:\"project update\"",
            "b:\"green blue\"",
            "l:work",
            "a:1",
            "filename:report.pdf",
            "subject-starts-with:project",
            "from-ends-with:alice@example.org",
            "s-matches:\"project update\"",
        ] {
            let results = GlobalSearchEngine::query_local_mail(query, &messages);
            assert_eq!(
                results.iter().map(target_id).collect::<Vec<_>>(),
                vec!["m-inbox"],
                "query {query:?}"
            );
        }

        let results = GlobalSearchEngine::query_local_mail("a:0", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");
        let results = GlobalSearchEngine::query_local_mail("flag:Seen", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");
        let results = GlobalSearchEngine::query_local_mail("is:sent", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");
        let results = GlobalSearchEngine::query_local_mail("is:inbox", &messages);
        assert_eq!(target_id(&results[0]), "m-inbox");
        let results = GlobalSearchEngine::query_local_mail("is:not-important", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");
    }

    #[test]
    fn evolution_groups_and_negation_have_and_or_not_semantics() {
        let messages = fixture_messages();

        let results = GlobalSearchEngine::query_local_mail(
            "or:(from:alice@example.org from:bob@example.org)",
            &messages,
        );
        assert_eq!(results.len(), 2);

        let results = GlobalSearchEngine::query_local_mail(
            "and:(from:alice@example.org not:(flag:Seen))",
            &messages,
        );
        assert_eq!(target_id(&results[0]), "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("not:(flag:Seen)", &messages);
        assert_eq!(target_id(&results[0]), "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("-from:alice@example.org", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");

        let results = GlobalSearchEngine::query_local_mail("not:attachment", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");
    }

    #[test]
    fn evolution_date_forms_support_relative_and_comparison_syntax() {
        let messages = fixture_messages();

        let results = GlobalSearchEngine::query_local_mail("sent-=:2026-08-10", &messages);
        assert_eq!(target_id(&results[0]), "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("sent-<:2026-08-10", &messages);
        assert_eq!(target_id(&results[0]), "m-sent");

        let results = GlobalSearchEngine::query_local_mail("sent->:2026-08-05", &messages);
        assert_eq!(target_id(&results[0]), "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("date:2026-08-10", &messages);
        assert_eq!(target_id(&results[0]), "m-inbox");

        let today = Local::now().date_naive();
        let today_query = format!("sent-=:{}", today.format("%Y-%m-%d"));
        let today_message = fixture_mail(
            "m-today",
            "inbox",
            &today.format("%Y-%m-%d").to_string(),
            "Today",
            "today@example.org",
            "me@example.org",
            "",
            "Today",
            "Today body",
            true,
            false,
            false,
            &[],
            None,
        );
        let results = GlobalSearchEngine::query_local_mail(&today_query, [&today_message]);
        assert_eq!(results.len(), 1);

        let results = GlobalSearchEngine::query_local_mail("sent:0", [&today_message]);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn quotes_escaped_quotes_and_escaped_spaces_are_single_terms() {
        let documents = vec![SearchResultItem {
            domain: SearchDomain::Mail,
            title: "Say \"hello\" to the team".to_string(),
            snippet: "alpha beta".to_string(),
            date: String::new(),
            target: Some(SearchTarget::Mail {
                local_id: "quoted".to_string(),
            }),
        }];

        for query in ["\"say \\\"hello\\\"\"", "\"say \"\"hello\"\"\""] {
            let results =
                GlobalSearchEngine::query_documents(SearchDomain::Mail, query, &documents);
            assert_eq!(results.len(), 1, "query {query:?}");
        }

        let documents = vec![SearchResultItem {
            domain: SearchDomain::Mail,
            title: "say hello".to_string(),
            snippet: String::new(),
            date: String::new(),
            target: Some(SearchTarget::Mail {
                local_id: "escaped-space".to_string(),
            }),
        }];
        let results =
            GlobalSearchEngine::query_documents(SearchDomain::Mail, r#"say\ hello"#, &documents);
        assert_eq!(results.len(), 1);

        let documents = vec![SearchResultItem {
            domain: SearchDomain::Mail,
            title: "alpha beta".to_string(),
            snippet: String::new(),
            date: String::new(),
            target: Some(SearchTarget::Mail {
                local_id: "phrase".to_string(),
            }),
        }];
        let results =
            GlobalSearchEngine::query_documents(SearchDomain::Mail, "\"alpha beta\"", &documents);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn invalid_or_unsupported_syntax_is_rejected_instead_of_becoming_text() {
        for query in [
            "unknown:value",
            "after:not-a-date",
            "subject-regex:project",
            "size->:10",
            "or:(subject:one",
            "subject:one)",
            "subject:\"unterminated",
            "subject:\"dangling\\",
            "or:subject:one",
        ] {
            assert!(parse_search_query(query).is_err(), "query {query:?}");
            let documents = [SearchResultItem {
                domain: SearchDomain::Mail,
                title: query.to_string(),
                snippet: "one project".to_string(),
                date: String::new(),
                target: Some(SearchTarget::Mail {
                    local_id: "must-not-match".to_string(),
                }),
            }];
            assert!(
                GlobalSearchEngine::query_documents(SearchDomain::Mail, query, &documents)
                    .is_empty()
            );
        }

        assert!(parse_search_query("subject:").is_err());
        assert!(parse_search_query("not:()").is_err());
        assert!(parse_search_query("(subject:one)").is_ok());
    }

    #[test]
    fn search_syntax_validation_reports_invalid_queries_and_rejects_empty_input() {
        for (query, expected_error) in [
            (
                "unknown:value",
                SearchQueryParseError::UnknownOperator("unknown".to_string()),
            ),
            (
                "or:(subject:one",
                SearchQueryParseError::MissingClosingParenthesis,
            ),
            (
                "subject:one)",
                SearchQueryParseError::UnexpectedClosingParenthesis,
            ),
            (
                "subject:\"unterminated",
                SearchQueryParseError::UnterminatedQuote,
            ),
        ] {
            let error = validate_search_syntax(query).expect_err("invalid query must fail");
            assert_eq!(error, expected_error, "query {query:?}");
            assert!(!error.to_string().is_empty());
        }

        assert_eq!(
            validate_search_syntax("   ")
                .expect_err("empty search input must fail")
                .to_string(),
            "Suchbegriff darf nicht leer sein"
        );
    }

    #[test]
    fn repeated_search_fields_are_accepted_by_the_canonical_parser() {
        let query = parse_search_query("from:alice@example.org from:bob@example.org")
            .expect("repeated fields should be valid")
            .expect("non-empty query should produce an AST");

        assert!(matches!(query, SearchQuery::All(expressions) if expressions.len() == 2));
        assert!(validate_search_syntax("from:alice@example.org from:bob@example.org").is_ok());
        assert!(validate_search_syntax("sender:alice recipient:bob").is_ok());
    }

    fn parsed_mail_query(input: &str, scope: MailSearchFieldScope) -> ParsedMailSearchQuery {
        parse_mail_query(input, scope)
            .expect("mail query should parse")
            .expect("non-empty mail query should produce a parsed query")
    }

    #[test]
    fn mail_query_fixture_round_trips_the_complete_typed_ast() {
        let query = parsed_mail_query(
            concat!(
                "and:(",
                "or:(subject-contains:\"alpha beta\" from-has-words:alice@example.org) ",
                "not:(-subject-starts-with:secret) ",
                "subject-matches:exact ",
                "subject-ends-with:suffix ",
                "subject-no:blocked ",
                "from:alice@example.org from:bob@example.org ",
                "to:to@example.org cc:cc@example.org bcc:bcc@example.org ",
                "recipients:recipient@example.org body:body label:team ",
                "filename:report.pdf folder:inbox reply-to:reply@example.org ",
                "mid:message@example.org location:local ",
                "is:unread is:flagged is:important is:junk is:attachment ",
                "is:draft is:deleted is:sent is:inbox is:archive ",
                "after:2026-08-01 before:2026-08-31 after->:2026-08-02 ",
                "before-<:2026-08-30 on:2026-08-15)"
            ),
            MailSearchFieldScope::Any,
        );

        let encoded = encode_mail_query_fixture(&query).expect("query should encode");
        assert!(encoded.contains("\"version\": 1"));
        assert!(encoded.contains("\"op\": \"and\""));
        assert!(encoded.contains("\"kind\": \"text\""));
        assert!(encoded.contains("\"kind\": \"state\""));
        assert!(encoded.contains("\"kind\": \"date_at_or_after\""));
        let decoded = decode_mail_query_fixture(&encoded).expect("query should decode");
        assert_eq!(decoded, query);
    }

    #[test]
    fn mail_query_fixture_preserves_dynamic_relative_date_semantics() {
        for input in ["after:7d", "before:7d", "on:today", "sent:7"] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            let encoded = encode_mail_query_fixture(&query).expect("query should encode");
            assert!(
                encoded.contains("\"relative\": true"),
                "relative date marker should be persisted for {input:?}"
            );
            let decoded = decode_mail_query_fixture(&encoded).expect("query should decode");
            assert_eq!(
                decoded, query,
                "relative query should round-trip: {input:?}"
            );
        }

        let absolute = parsed_mail_query("after:20260801", MailSearchFieldScope::Any);
        let encoded = encode_mail_query_fixture(&absolute).expect("query should encode");
        assert!(encoded.contains("\"relative\": false"));
        assert_eq!(
            decode_mail_query_fixture(&encoded).expect("absolute query should decode"),
            absolute
        );
    }

    #[test]
    fn mail_query_fixture_uses_stable_raw_relative_date_wire_values() {
        for (input, expected_date) in [("after:7d", "7d"), ("on:today", "today"), ("sent:7", "7")] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            let encoded = encode_mail_query_fixture(&query).expect("query should encode");
            assert!(encoded.contains(&format!("\"date\": \"{expected_date}\"")));
            assert!(encoded.contains("\"relative\": true"));

            // Re-encoding after a dynamic decode must not change the wire
            // representation when the local day changes.
            let decoded = decode_mail_query_fixture(&encoded).expect("query should decode");
            assert_eq!(encode_mail_query_fixture(&decoded).unwrap(), encoded);
        }
    }

    #[test]
    fn mail_query_fixture_rejects_tampered_relative_date_expression() {
        let query = parsed_mail_query("after:7d", MailSearchFieldScope::Any);
        let encoded = encode_mail_query_fixture(&query).expect("query should encode");
        let tampered = encoded.replace("\"date\": \"7d\"", "\"date\": \"6d\"");
        assert!(matches!(
            decode_mail_query_fixture(&tampered),
            Err(MailSearchFixtureError::InvalidShape(message))
                if message.contains("does not match the term value")
        ));

        let query = parsed_mail_query("sent:7", MailSearchFieldScope::Any);
        let encoded = encode_mail_query_fixture(&query).expect("query should encode");
        let tampered = encoded.replace("\"date\": \"7\"", "\"date\": \"1970-01-01\"");
        assert!(decode_mail_query_fixture(&tampered).is_err());
    }

    #[test]
    fn mail_query_fixture_rejects_unknown_corrupt_and_oversized_data() {
        let query = parsed_mail_query("subject:project", MailSearchFieldScope::Any);
        let encoded = encode_mail_query_fixture(&query).expect("query should encode");

        let unknown_version = encoded.replace("\"version\": 1", "\"version\": 2");
        assert!(matches!(
            decode_mail_query_fixture(&unknown_version),
            Err(MailSearchFixtureError::UnsupportedVersion(2))
        ));

        let unknown_field = encoded.replace("\"subject\"", "\"future_field\"");
        assert!(decode_mail_query_fixture(&unknown_field).is_err());
        let unknown_matcher = encoded.replace("\"contains\"", "\"future_matcher\"");
        assert!(decode_mail_query_fixture(&unknown_matcher).is_err());
        let unknown_format = encoded.replace(MAIL_QUERY_FIXTURE_FORMAT, "future-mail-search-query");
        assert!(matches!(
            decode_mail_query_fixture(&unknown_format),
            Err(MailSearchFixtureError::UnknownFormat(_))
        ));
        assert!(decode_mail_query_fixture("not-json").is_err());
        assert!(decode_mail_query_fixture("[]").is_err());
        assert!(
            decode_mail_query_fixture(
                &encoded.replace("\"predicate\": {", "\"predicate\": {\"unexpected\": true,")
            )
            .is_err()
        );

        let oversized = format!(
            r#"{{"format":"{MAIL_QUERY_FIXTURE_FORMAT}","version":1,"query":{{"op":"term","value":"{}","negated":false,"predicate":{{"kind":"text","field":"subject","matcher":"contains"}}}}}}"#,
            "x".repeat(MAIL_QUERY_FIXTURE_MAX_TEXT_CHARS + 1),
        );
        assert!(matches!(
            decode_mail_query_fixture(&oversized),
            Err(MailSearchFixtureError::SizeLimitExceeded(_))
        ));
    }

    #[test]
    fn mail_query_fixture_enforces_ast_depth_and_node_limits() {
        let mut nested = r#"{"op":"term","value":"x","negated":false,"predicate":{"kind":"text","field":"subject","matcher":"contains"}}"#.to_string();
        for _ in 0..MAIL_QUERY_FIXTURE_MAX_DEPTH {
            nested = format!(r#"{{"op":"not","child":{nested}}}"#);
        }
        let too_deep =
            format!(r#"{{"format":"{MAIL_QUERY_FIXTURE_FORMAT}","version":1,"query":{nested}}}"#);
        assert!(matches!(
            decode_mail_query_fixture(&too_deep),
            Err(MailSearchFixtureError::SizeLimitExceeded(
                "maximum AST depth"
            ))
        ));

        let children = (0..=MAIL_QUERY_FIXTURE_MAX_NODES)
            .map(|_| {
                r#"{"op":"term","value":"x","negated":false,"predicate":{"kind":"text","field":"subject","matcher":"contains"}}"#
            })
            .collect::<Vec<_>>()
            .join(",");
        let too_many_nodes = format!(
            r#"{{"format":"{MAIL_QUERY_FIXTURE_FORMAT}","version":1,"query":{{"op":"and","children":[{children}]}}}}"#
        );
        assert!(matches!(
            decode_mail_query_fixture(&too_many_nodes),
            Err(MailSearchFixtureError::SizeLimitExceeded(
                "maximum AST nodes"
            ))
        ));
    }

    fn assert_graph_plan_unsupported(input: &str) {
        let query = parsed_mail_query(input, MailSearchFieldScope::Any);
        assert!(
            matches!(
                query.graph_search_plan(),
                MailGraphSearchPlan::Unsupported(_)
            ),
            "query should not be lowered to a partial Graph search: {input:?}"
        );
    }

    #[test]
    fn mail_graph_plan_emits_exact_safe_flat_and_clauses() {
        let query = parsed_mail_query(
            "subject:Roadmap from:Alice filename:Report.pdf has:attachment",
            MailSearchFieldScope::Any,
        );
        assert_eq!(
            query.graph_search_plan(),
            MailGraphSearchPlan::SafePositiveConjunction(
                "\"subject:roadmap\" AND \"from:alice\" AND \"attachment:report.pdf\" AND \"hasAttachments:true\""
                    .to_string()
            )
        );

        let escaped =
            parsed_mail_query(r#"subject:"A \"Quote\" \\path""#, MailSearchFieldScope::Any);
        assert_eq!(
            escaped.graph_search_plan(),
            MailGraphSearchPlan::SafePositiveConjunction(format!(
                "\"subject:{}\"",
                r#"a \"quote\" \\path"#
            ))
        );
    }

    #[test]
    fn mail_graph_plan_rejects_unsupported_or_unsafe_predicates() {
        for input in [
            "or:(subject:one subject:two)",
            "not:(subject:one)",
            "and:(subject:one or:(subject:two subject:three))",
            "label:team",
            "after:2026-08-01",
            "folder:inbox",
            "reply-to:alice@example.org",
            "mid:message@example.org",
            "location:local",
            "is:unread",
            "-is:attachment",
            "no:attachment",
            "subject-has-words:roadmap",
            "subject-matches:roadmap",
            "subject-starts-with:road",
            "subject-ends-with:map",
            "-subject:roadmap",
            "to:alice@example.org",
        ] {
            assert_graph_plan_unsupported(input);
        }

        let line_break = parsed_mail_query(
            &format!("subject:\"bad\rvalue\""),
            MailSearchFieldScope::Any,
        );
        assert_graph_plan_unsupported_from_parsed(line_break);

        let long_value = format!(
            "subject:{}",
            "x".repeat(MAIL_GRAPH_SEARCH_VALUE_MAX_CHARS + 1)
        );
        let long_query = parsed_mail_query(&long_value, MailSearchFieldScope::Any);
        assert_graph_plan_unsupported_from_parsed(long_query);

        assert!(
            parse_mail_query("", MailSearchFieldScope::Any)
                .expect("empty input should parse as no query")
                .is_none()
        );
    }

    fn assert_graph_plan_unsupported_from_parsed(query: ParsedMailSearchQuery) {
        assert!(matches!(
            query.graph_search_plan(),
            MailGraphSearchPlan::Unsupported(_)
        ));
    }

    #[test]
    fn mail_graph_plan_maps_scoped_bare_terms_to_graph_fields() {
        for (scope, expected) in [
            (MailSearchFieldScope::Any, "\"needle\""),
            (MailSearchFieldScope::Subject, "\"subject:needle\""),
            (MailSearchFieldScope::Sender, "\"from:needle\""),
            (MailSearchFieldScope::Recipients, "\"recipients:needle\""),
            (MailSearchFieldScope::Body, "\"body:needle\""),
        ] {
            let query = parsed_mail_query("needle", scope);
            assert_eq!(
                query.graph_search_plan(),
                MailGraphSearchPlan::SafePositiveConjunction(expected.to_string()),
                "scope {scope:?}"
            );
        }
    }

    #[test]
    fn mail_quick_modes_expand_bare_terms_to_the_upstream_field_sets() {
        let subject_or_addresses =
            parsed_mail_query("needle", MailSearchFieldScope::SubjectOrAddresses);
        let message = parsed_mail_query("needle", MailSearchFieldScope::Message);

        let records = [
            (
                MailSearchRecord {
                    subject: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
                true,
                true,
            ),
            (
                MailSearchRecord {
                    from: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
                true,
                true,
            ),
            (
                MailSearchRecord {
                    to: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
                true,
                true,
            ),
            (
                MailSearchRecord {
                    cc: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
                true,
                true,
            ),
            (
                MailSearchRecord {
                    body: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
                false,
                true,
            ),
            (
                MailSearchRecord {
                    bcc: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
                false,
                false,
            ),
        ];

        for (record, subject_or_addresses_expected, message_expected) in records {
            assert_eq!(
                matches_mail_query(&subject_or_addresses, &record),
                subject_or_addresses_expected,
                "Subject-or-addresses mode must use only Subject/From/To/Cc"
            );
            assert_eq!(
                matches_mail_query(&message, &record),
                message_expected,
                "Message mode must use Subject/From/To/Cc/Body"
            );
        }
    }

    #[test]
    fn mail_quick_mode_recipients_excludes_bcc() {
        let query = parsed_mail_query("needle", MailSearchFieldScope::Recipients);
        let bcc_only = MailSearchRecord {
            bcc: "needle".to_string(),
            ..MailSearchRecord::default()
        };
        let to_and_bcc = MailSearchRecord {
            to: "needle".to_string(),
            bcc: "needle".to_string(),
            ..MailSearchRecord::default()
        };

        assert!(!matches_mail_query(&query, &bcc_only));
        assert!(matches_mail_query(&query, &to_and_bcc));
    }

    #[test]
    fn mail_quick_mode_or_shapes_are_unsupported_by_graph_as_a_whole() {
        for scope in [
            MailSearchFieldScope::SubjectOrAddresses,
            MailSearchFieldScope::Message,
        ] {
            let query = parsed_mail_query("needle", scope);
            assert!(matches!(
                &query.0,
                SearchQuery::Any(expressions) if expressions.len() == match scope {
                    MailSearchFieldScope::SubjectOrAddresses => 3,
                    MailSearchFieldScope::Message => 4,
                    _ => unreachable!(),
                }
            ));
            assert!(matches!(
                query.graph_search_plan(),
                MailGraphSearchPlan::Unsupported(reason) if reason.contains("OR")
            ));
        }
    }

    #[test]
    fn mail_graph_plan_keeps_repeated_fields_as_distinct_and_clauses() {
        let query = parsed_mail_query("subject:one subject:two", MailSearchFieldScope::Any);
        assert_eq!(
            query.graph_search_plan(),
            MailGraphSearchPlan::SafePositiveConjunction(
                "\"subject:one\" AND \"subject:two\"".to_string()
            )
        );
    }

    #[test]
    fn mail_query_scopes_apply_to_unqualified_terms() {
        let cases = [
            (
                MailSearchFieldScope::Any,
                MailSearchRecord {
                    searchable_text: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
            ),
            (
                MailSearchFieldScope::Subject,
                MailSearchRecord {
                    subject: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
            ),
            (
                MailSearchFieldScope::Sender,
                MailSearchRecord {
                    from: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
            ),
            (
                MailSearchFieldScope::Recipients,
                MailSearchRecord {
                    cc: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
            ),
            (
                MailSearchFieldScope::Body,
                MailSearchRecord {
                    body: "needle".to_string(),
                    ..MailSearchRecord::default()
                },
            ),
        ];

        for (scope, record) in cases {
            let query = parsed_mail_query("needle", scope);
            assert!(matches_mail_query(&query, &record), "scope {scope:?}");
        }
    }

    #[test]
    fn mail_query_explicit_fields_override_default_scope() {
        let subject_record = MailSearchRecord {
            subject: "project".to_string(),
            ..MailSearchRecord::default()
        };
        let sender_record = MailSearchRecord {
            from: "alice@example.org".to_string(),
            ..MailSearchRecord::default()
        };

        assert!(matches_mail_query(
            &parsed_mail_query("subject:project", MailSearchFieldScope::Sender),
            &subject_record
        ));
        assert!(matches_mail_query(
            &parsed_mail_query("from:alice@example.org", MailSearchFieldScope::Subject),
            &sender_record
        ));
        assert!(!matches_mail_query(
            &parsed_mail_query("project", MailSearchFieldScope::Sender),
            &subject_record
        ));
    }

    #[test]
    fn mail_query_repeated_fields_are_conjoined() {
        let complete = MailSearchRecord {
            from: "alice bob".to_string(),
            ..MailSearchRecord::default()
        };
        let incomplete = MailSearchRecord {
            from: "alice".to_string(),
            ..MailSearchRecord::default()
        };
        let query = parsed_mail_query("from:alice from:bob", MailSearchFieldScope::Any);

        assert!(matches_mail_query(&query, &complete));
        assert!(!matches_mail_query(&query, &incomplete));
    }

    #[test]
    fn mail_query_nested_or_and_not_truth_tables() {
        let query = parsed_mail_query(
            "or:(and:(subject:alpha from:alice) and:(subject:beta from:bob))",
            MailSearchFieldScope::Any,
        );
        let alpha_alice = MailSearchRecord {
            subject: "alpha".to_string(),
            from: "alice".to_string(),
            ..MailSearchRecord::default()
        };
        let beta_bob = MailSearchRecord {
            subject: "beta".to_string(),
            from: "bob".to_string(),
            ..MailSearchRecord::default()
        };
        let alpha_bob = MailSearchRecord {
            subject: "alpha".to_string(),
            from: "bob".to_string(),
            ..MailSearchRecord::default()
        };

        assert!(matches_mail_query(&query, &alpha_alice));
        assert!(matches_mail_query(&query, &beta_bob));
        assert!(!matches_mail_query(&query, &alpha_bob));

        let not_query = parsed_mail_query(
            "not:(or:(subject:alpha subject:beta))",
            MailSearchFieldScope::Any,
        );
        let gamma = MailSearchRecord {
            subject: "gamma".to_string(),
            ..MailSearchRecord::default()
        };
        assert!(!matches_mail_query(&not_query, &alpha_alice));
        assert!(matches_mail_query(&not_query, &gamma));
    }

    #[test]
    fn mail_query_body_requirement_tracks_nested_and_negated_terms() {
        for (input, expected) in [
            ("needle", true),
            ("body:needle", true),
            ("-body:needle", true),
            ("not:(body:needle)", true),
            ("or:(subject:needle body:needle)", true),
            ("subject:needle", false),
            ("from:alice", false),
            ("is:unread", false),
            ("after:2026-08-01", false),
            ("folder:inbox", false),
        ] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            assert_eq!(mail_query_needs_body(&query), expected, "query {input:?}");
        }
    }

    #[test]
    fn mail_query_invalid_syntax_fails_closed() {
        for input in [
            "unknown:value",
            "or:(subject:one",
            "subject:one)",
            "subject:\"unterminated",
            "or:subject:one",
            "not:()",
        ] {
            assert!(
                parse_mail_query(input, MailSearchFieldScope::Any).is_err(),
                "invalid query must not produce a matchable AST: {input:?}"
            );
        }
    }

    #[test]
    fn mail_query_matches_states_dates_and_folders() {
        let record = MailSearchRecord {
            folder: "inbox".to_string(),
            date: NaiveDate::from_ymd_opt(2026, 8, 10),
            is_unread: Some(true),
            is_flagged: Some(true),
            is_important: Some(true),
            is_junk: Some(true),
            has_attachment: Some(true),
            ..MailSearchRecord::default()
        };

        for input in [
            "is:unread",
            "is:flagged",
            "is:important",
            "is:junk",
            "has:attachment",
            "is:inbox",
            "folder:inbox",
            "after:2026-08-01",
            "before:2026-08-11",
            "on:2026-08-10",
        ] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            assert!(matches_mail_query(&query, &record), "query {input:?}");
        }

        let unknown_state = MailSearchRecord {
            is_unread: None,
            ..MailSearchRecord::default()
        };
        for input in ["is:unread", "-is:unread"] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            assert!(
                !matches_mail_query(&query, &unknown_state),
                "query {input:?}"
            );
        }
    }

    #[test]
    fn mail_query_unknown_values_follow_kleene_boolean_rules() {
        let unknown = MailSearchRecord {
            subject: "known".to_string(),
            ..MailSearchRecord::default()
        };

        for input in [
            "not:(is:unread)",
            "not:(or:(is:unread is:flagged))",
            "not:(and:(is:unread subject:known))",
            "not:(or:(is:unread subject:missing))",
        ] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            assert!(
                !matches_mail_query(&query, &unknown),
                "unknown must not become a match through {input:?}"
            );
        }

        let known_false_and = parsed_mail_query(
            "not:(and:(is:unread subject:missing))",
            MailSearchFieldScope::Any,
        );
        assert!(matches_mail_query(&known_false_and, &unknown));

        let known_true_or = MailSearchRecord {
            is_flagged: Some(true),
            ..MailSearchRecord::default()
        };
        let dominant_true_or =
            parsed_mail_query("not:(or:(is:unread is:flagged))", MailSearchFieldScope::Any);
        assert!(!matches_mail_query(&dominant_true_or, &known_true_or));

        let missing_date = MailSearchRecord::default();
        for input in ["-after:2026-08-01", "not:(after:2026-08-01)"] {
            let query = parsed_mail_query(input, MailSearchFieldScope::Any);
            assert!(
                !matches_mail_query(&query, &missing_date),
                "missing date must remain unknown through {input:?}"
            );
        }
    }

    #[test]
    fn unknown_state_does_not_match_even_when_negated() {
        let documents = [SearchResultItem {
            domain: SearchDomain::Calendar,
            title: "Statusless event".to_string(),
            snippet: "No mail state".to_string(),
            date: "2026-08-10".to_string(),
            target: Some(SearchTarget::Event {
                source_uid: "calendar".to_string(),
                component_uid: "event-1".to_string(),
            }),
        }];

        for query in ["is:unread", "-is:unread", "is:sent", "-is:sent"] {
            assert!(
                GlobalSearchEngine::query_documents(SearchDomain::Calendar, query, &documents)
                    .is_empty(),
                "unknown state must not match query {query:?}"
            );
        }
    }

    #[test]
    fn unsupported_evolution_regex_and_soundex_are_explicitly_not_local_features() {
        for query in [
            "s-regex:project",
            "s-full-regex:project",
            "from-soundex:alice",
            "header:Subject=project",
            "exists:X-Secret-Header",
        ] {
            assert!(matches!(
                parse_search_query(query),
                Err(SearchQueryParseError::UnsupportedOperator(_))
            ));
        }
    }

    #[test]
    fn search_store_snapshot_is_send_sync_and_keeps_calendar_and_contact_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_static<T: 'static>() {}

        assert_send_sync::<SearchStoreSnapshot>();
        assert_send_sync::<MailStoreService>();
        assert_send_sync::<MailMetadataStore>();
        assert_send_sync::<MailFilterStore>();
        assert_send_sync::<MailFilterRule>();
        assert_send_sync::<CalendarStoreService>();
        assert_send_sync::<ContactStoreService>();
        assert_static::<SearchStoreSnapshot>();

        let root = temporary_directory("store-config");
        let calendar_dir = root.join("calendar");
        let contact_dir = root.join("contacts");
        fs::create_dir_all(&calendar_dir).expect("calendar directory should be created");
        fs::create_dir_all(&contact_dir).expect("contact directory should be created");
        fs::write(
            calendar_dir.join("calendar.ics"),
            concat!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
                "BEGIN:VEVENT\r\nUID:duplicate-uid\r\nSUMMARY:Shared planning local one\r\n",
                "DTSTART:20260823T100000Z\r\nEND:VEVENT\r\n",
                "BEGIN:VEVENT\r\nUID:duplicate-uid\r\nSUMMARY:Shared planning local two\r\n",
                "DTSTART:20260824T100000Z\r\nEND:VEVENT\r\n",
                "BEGIN:VTODO\r\nUID:duplicate-uid\r\nSUMMARY:Shared planning task\r\n",
                "DUE:20260825T100000Z\r\nEND:VTODO\r\n",
                "BEGIN:VJOURNAL\r\nUID:duplicate-uid\r\nSUMMARY:Shared planning memo\r\n",
                "DESCRIPTION:shared planning note\r\nEND:VJOURNAL\r\n",
                "END:VCALENDAR\r\n"
            ),
        )
        .expect("calendar should be written");
        fs::write(
            contact_dir.join("contacts.vcf"),
            concat!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:shared-uid\r\nFN:Ada shared one\r\n",
                "END:VCARD\r\n",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:shared-uid\r\nFN:Ada shared two\r\n",
                "END:VCARD\r\n",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada shared path\r\nEND:VCARD\r\n"
            ),
        )
        .expect("contacts should be written");

        let stores = test_store_snapshot(
            CalendarStoreService::with_writable_calendar_dir(&calendar_dir),
            ContactStoreService::with_contact_dir(&contact_dir),
        );
        let isolated_copy = stores.clone();
        fs::write(
            calendar_dir.join("after-capture.ics"),
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:new\r\nSUMMARY:changed after capture\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .expect("post-capture calendar should be writable");

        let cancellation = AtomicBool::new(false);
        let results = std::thread::spawn(move || {
            GlobalSearchEngine::query_cancelable_with_stores(
                SearchDomain::All,
                "shared",
                &cancellation,
                stores,
            )
        })
        .join()
        .expect("search snapshot should cross the worker boundary");

        assert_eq!(
            results
                .iter()
                .filter(|result| result.domain == SearchDomain::Calendar)
                .map(|result| result.target.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(SearchTarget::Event {
                    source_uid: "local-calendar".to_string(),
                    component_uid: "duplicate-uid".to_string(),
                }),
                Some(SearchTarget::Event {
                    source_uid: "local-calendar".to_string(),
                    component_uid: "duplicate-uid".to_string(),
                }),
                Some(SearchTarget::Task {
                    source_uid: "local-calendar".to_string(),
                    component_uid: "duplicate-uid".to_string(),
                }),
                Some(SearchTarget::Memo {
                    source_uid: "local-calendar".to_string(),
                    component_uid: "duplicate-uid".to_string(),
                }),
            ]
        );
        assert!(
            results
                .iter()
                .any(|result| result.title == "Shared planning task")
        );
        assert!(
            results
                .iter()
                .any(|result| result.title == "Shared planning memo")
        );
        assert!(
            results
                .iter()
                .any(|result| result.snippet.contains("Lokaler Kalender"))
        );
        let contact_references = results
            .iter()
            .filter(|result| result.domain == SearchDomain::Contacts)
            .map(target_id)
            .collect::<Vec<_>>();
        assert!(
            results
                .iter()
                .filter(|result| result.domain == SearchDomain::Contacts)
                .all(|result| match result.target.as_ref() {
                    Some(SearchTarget::Contact {
                        source_uid,
                        contact_uid,
                    }) => source_uid.is_empty() && contact_uid == target_id(result),
                    _ => false,
                })
        );
        assert_eq!(
            contact_references
                .iter()
                .filter(|reference| **reference == "shared-uid")
                .count(),
            2
        );
        let uidless_reference = format!(
            "local:{}#2",
            contact_dir.join("contacts.vcf").to_string_lossy()
        );
        assert!(
            contact_references
                .iter()
                .any(|reference| *reference == uidless_reference)
        );
        assert!(isolated_copy.calendar.is_writable());
        assert_eq!(isolated_copy.calendar.calendar_dir(), &calendar_dir);
        assert_eq!(isolated_copy.contacts.contact_dir(), &contact_dir);
        assert!(!isolated_copy.contacts.is_writable());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_calendar_snapshot_preserves_source_semantics() {
        let root = temporary_directory("read-only-calendar-source");
        fs::write(
            root.join("external.ics"),
            concat!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
                "BEGIN:VEVENT\r\nUID:external-event\r\nSUMMARY:External shared source\r\n",
                "DTSTART:20260823T100000Z\r\nEND:VEVENT\r\n",
                "END:VCALENDAR\r\n"
            ),
        )
        .expect("external calendar should be written");
        let calendar = CalendarStoreService::with_calendar_dir(&root);
        let stores = test_store_snapshot(
            calendar.clone(),
            ContactStoreService::with_contact_dir(root.join("contacts")),
        );
        let results = GlobalSearchEngine::query_cancelable_with_stores(
            SearchDomain::Calendar,
            "external",
            &AtomicBool::new(false),
            stores,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].target,
            Some(SearchTarget::Event {
                source_uid: String::new(),
                component_uid: "external-event".to_string(),
            })
        );
        assert!(!results[0].snippet.contains("Lokaler Kalender"));
        assert!(!calendar.is_writable());
        assert_eq!(calendar.calendar_dir(), root.as_path());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_filter_store_is_injected_into_worker_search() {
        let root = temporary_directory("explicit-filter");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("invoice.eml"),
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nPayable\n",
        )
        .expect("mail should be written");
        let mail_path = mail_dir.join("invoice.eml");
        let mail_before = fs::read(&mail_path).expect("mail should be readable");
        let state_path = root.join("mail-state.json");
        fs::write(&state_path, b"read-only-search-sentinel")
            .expect("sidecar sentinel should be written");
        let state_before = fs::read(&state_path).expect("sidecar should be readable");

        let filter_path = root.join("filters.json");
        let filters = MailFilterStore::new(&filter_path);
        filters
            .upsert(crate::services::mail_filter_service::MailFilterRule {
                id: "delete-invoice".to_string(),
                name: "Delete invoice".to_string(),
                enabled: true,
                scope: crate::services::mail_filter_service::FilterScope::Incoming,
                condition: crate::services::mail_filter_service::FilterCondition {
                    field: crate::services::mail_filter_service::FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: crate::services::mail_filter_service::FilterOperator::Contains,
                },
                action: crate::services::mail_filter_service::FilterAction::Delete,
                conditions: Vec::new(),
                actions: Vec::new(),
                match_all: true,
            })
            .expect("explicit filter should be persisted");

        let stores = snapshot_with_stores(
            MailStoreService::with_mail_dir_and_state_path(mail_dir, &state_path),
            filters,
            CalendarStoreService::with_calendar_dir(root.join("calendar")),
            ContactStoreService::with_contact_dir(root.join("contacts")),
        );
        fs::write(&filter_path, br#"{"version":1,"rules":[]}"#)
            .expect("post-capture filter update should be writable");
        let results = GlobalSearchEngine::query_cancelable_with_stores(
            SearchDomain::Mail,
            "invoice",
            &AtomicBool::new(false),
            stores,
        );
        assert!(results.is_empty());
        assert_eq!(
            fs::read(&mail_path).expect("mail should remain readable"),
            mail_before
        );
        assert_eq!(
            fs::read(&state_path).expect("sidecar should remain readable"),
            state_before
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_mail_filter_projection_does_not_persist_state_actions() {
        let root = temporary_directory("read-only-filter-projection");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        let mail_path = mail_dir.join("invoice.eml");
        fs::write(
            &mail_path,
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nPayable\n",
        )
        .expect("mail should be written");
        let state_path = root.join("mail-state.json");

        let filter_store = MailFilterStore::new(root.join("filters.json"));
        filter_store
            .upsert(crate::services::mail_filter_service::MailFilterRule {
                id: "mark-read-invoice".to_string(),
                name: "Mark invoice read".to_string(),
                enabled: true,
                scope: crate::services::mail_filter_service::FilterScope::Incoming,
                condition: crate::services::mail_filter_service::FilterCondition {
                    field: crate::services::mail_filter_service::FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: crate::services::mail_filter_service::FilterOperator::Contains,
                },
                action: crate::services::mail_filter_service::FilterAction::MarkRead,
                conditions: Vec::new(),
                actions: vec![
                    crate::services::mail_filter_service::FilterAction::MarkRead,
                    crate::services::mail_filter_service::FilterAction::CopyToFolder(
                        "archive".to_string(),
                    ),
                ],
                match_all: true,
            })
            .expect("read-only filter should be persisted");
        let stores = snapshot_with_stores(
            MailStoreService::with_mail_dir_and_state_path(mail_dir, &state_path),
            filter_store,
            CalendarStoreService::with_calendar_dir(root.join("calendar")),
            ContactStoreService::with_contact_dir(root.join("contacts")),
        );

        let results = GlobalSearchEngine::query_cancelable_with_stores(
            SearchDomain::Mail,
            "invoice is:read",
            &AtomicBool::new(false),
            stores,
        );
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .any(|result| target_id(result).ends_with("::copy::archive"))
        );
        assert!(!state_path.exists(), "search must not create a sidecar");
        assert!(mail_path.exists(), "search must not rewrite source mail");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_delete_filter_drops_persisted_copies_of_deleted_source() {
        let root = temporary_directory("read-only-filter-copy-delete");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("invoice.eml"),
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nPayable\n",
        )
        .expect("mail should be written");
        let state_path = root.join("mail-state.json");
        let mail = MailStoreService::with_mail_dir_and_state_path(&mail_dir, &state_path);
        let source_id = mail
            .load_local_records()
            .into_iter()
            .next()
            .expect("source mail should parse")
            .id;
        MailMetadataStore::new(&state_path)
            .set_copy(&source_id, "archive", true)
            .expect("copy should be persisted");
        let state_before = fs::read(&state_path).expect("sidecar should be readable");

        let filter_store = MailFilterStore::new(root.join("filters.json"));
        filter_store
            .upsert(crate::services::mail_filter_service::MailFilterRule {
                id: "delete-invoice-with-copy".to_string(),
                name: "Delete invoice with copy".to_string(),
                enabled: true,
                scope: crate::services::mail_filter_service::FilterScope::Incoming,
                condition: crate::services::mail_filter_service::FilterCondition {
                    field: crate::services::mail_filter_service::FilterField::Subject,
                    value: "invoice".to_string(),
                    operator: crate::services::mail_filter_service::FilterOperator::Contains,
                },
                action: crate::services::mail_filter_service::FilterAction::Delete,
                conditions: Vec::new(),
                actions: Vec::new(),
                match_all: true,
            })
            .expect("delete filter should be persisted");

        let stores = snapshot_with_stores(
            mail,
            filter_store,
            CalendarStoreService::with_calendar_dir(root.join("calendar")),
            ContactStoreService::with_contact_dir(root.join("contacts")),
        );
        let results = GlobalSearchEngine::query_cancelable_with_stores(
            SearchDomain::Mail,
            "invoice",
            &AtomicBool::new(false),
            stores,
        );
        assert!(results.is_empty());
        assert_eq!(
            fs::read(&state_path).expect("sidecar should remain readable"),
            state_before
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_snapshot_query_returns_no_partial_results() {
        let stores = test_store_snapshot(
            CalendarStoreService::with_calendar_dir(PathBuf::new()),
            ContactStoreService::with_contact_dir(PathBuf::new()),
        );
        let cancellation = AtomicBool::new(true);
        let results = GlobalSearchEngine::query_cancelable_with_stores(
            SearchDomain::Calendar,
            "cancel",
            &cancellation,
            stores,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn search_worker_boundary_does_not_resolve_framework_or_global_state() {
        let source = include_str!("search_engine.rs");
        let start = source
            .find("pub fn query_cancelable_with_stores(")
            .expect("explicit search worker API should exist");
        let end = source[start..]
            .find("    /// Search an explicit repository snapshot.")
            .map(|offset| start + offset)
            .expect("search worker API should have a bounded source section");
        let worker = &source[start..end];

        assert!(worker.contains("load_local_records_cancelable_with_store"));
        assert!(worker.contains("raw_mail_filter_store"));
        assert!(worker.contains("filter_rules"));
        for forbidden in [
            "MailStoreService::new(",
            "MailStoreService::new_worker_safe(",
            "CalendarStoreService::new(",
            "ContactStoreService::new(",
            "AppSettings",
            "GSettings",
            "gtk::",
            "glib::",
            "GObject",
            "std::env::",
            "app_data_dir(",
        ] {
            assert!(
                !worker.contains(forbidden),
                "worker boundary found {forbidden}"
            );
        }
    }
}
