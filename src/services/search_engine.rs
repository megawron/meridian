// SPDX-License-Identifier: GPL-3.0-or-later
//
// search_engine — Unified cross-component search engine (Mail, Contacts, Calendar).

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::html_sanitizer::html_to_plain_text;

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

    /// Query local stores while allowing a UI worker to abandon an obsolete
    /// scan.  The cancellation flag is checked between stores and during the
    /// potentially large local-mail load; no GTK objects are created here.
    pub fn query_cancelable(
        domain: SearchDomain,
        query_str: &str,
        cancellation: &AtomicBool,
    ) -> Vec<SearchResultItem> {
        let Ok(Some(query)) = parse_search_query(query_str) else {
            return Vec::new();
        };

        let mut documents = Vec::new();

        if matches!(domain, SearchDomain::Mail | SearchDomain::All) {
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
            documents.extend(
                crate::services::mail_store_service::MailStoreService::new()
                    .load_local_records_cancelable(cancellation)
                    .into_iter()
                    .map(|message| {
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
                                reference_id: message.id,
                            },
                            searchable_text,
                        )
                        .with_fields(fields)
                    }),
            );
        }

        if matches!(domain, SearchDomain::Calendar | SearchDomain::All) {
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
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
            if cancellation.load(Ordering::Relaxed) {
                return Vec::new();
            }
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

        if cancellation.load(Ordering::Relaxed) {
            return Vec::new();
        }

        Self::query_search_documents_with_query(domain, &query, &documents)
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
                        reference_id: message.id.clone(),
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
enum SearchQueryParseError {
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
    if input.trim().is_empty() {
        return Ok(None);
    }

    let tokens = search_tokens(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }

    let mut cursor = 0;
    let expressions = parse_query_sequence(&tokens, &mut cursor, false)?;
    if cursor != tokens.len() {
        return Err(SearchQueryParseError::InvalidSyntax(
            "unexpected tokens after search expression".to_string(),
        ));
    }
    Ok(combine_all(expressions))
}

fn parse_query_sequence(
    tokens: &[SearchToken],
    cursor: &mut usize,
    expect_closing_parenthesis: bool,
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

        if let Some(expression) = parse_query_atom(tokens, cursor)? {
            expressions.push(expression);
        }
    }

    Ok(expressions)
}

fn parse_query_atom(
    tokens: &[SearchToken],
    cursor: &mut usize,
) -> Result<Option<SearchQuery>, SearchQueryParseError> {
    match tokens.get(*cursor) {
        Some(SearchToken::OpenParen) => {
            *cursor += 1;
            let expressions = parse_query_sequence(tokens, cursor, true)?;
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
                let expressions = parse_query_sequence(tokens, cursor, true)?;
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
            normalize_search_query_term(word).map(|term| term.map(SearchQuery::Term))
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
                field: SearchField::Any,
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
        "from" | "f" | "to" | "t" | "cc" | "c" | "bcc" | "recips" | "r" | "recipients"
        | "subject" | "s" | "body" | "b" | "content" | "label" | "l" | "tag" | "keyword"
        | "filename" | "name" | "folder" | "in" | "reply-to" | "replyto" | "mid" | "message-id"
        | "location" | "m" => parse_field_term(base_key, option, value, negated),
        "list" | "header" | "h" | "exists" | "e" | "size" | "sz" | "score" | "sc" => {
            Err(SearchQueryParseError::UnsupportedOperator(key))
        }
        _ => Err(SearchQueryParseError::UnknownOperator(key)),
    }
}

fn split_search_key_option(key: &str) -> (&str, Option<&str>) {
    key.split_once('-')
        .map(|(base, option)| (base, Some(option)))
        .unwrap_or((key, None))
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
        "from" | "f" => SearchField::From,
        "to" | "t" => SearchField::To,
        "cc" | "c" => SearchField::Cc,
        "bcc" => SearchField::Bcc,
        "recips" | "r" | "recipients" => SearchField::Recipients,
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

fn search_query_matches(
    query: &SearchQuery,
    document: &SearchDocument,
    document_date: Option<NaiveDate>,
) -> bool {
    match query {
        SearchQuery::All(expressions) => expressions
            .iter()
            .all(|expression| search_query_matches(expression, document, document_date)),
        SearchQuery::Any(expressions) => expressions
            .iter()
            .any(|expression| search_query_matches(expression, document, document_date)),
        SearchQuery::Not(expression) => !search_query_matches(expression, document, document_date),
        SearchQuery::Term(term) => search_term_matches(term, document, document_date),
    }
}

fn search_term_matches(
    term: &SearchQueryTerm,
    document: &SearchDocument,
    document_date: Option<NaiveDate>,
) -> bool {
    let matches = match term.kind {
        SearchTermKind::Text { field, matcher } => {
            let field_value = search_field_value(document, field);
            search_text_matches(field_value.as_ref(), &term.value, matcher)
        }
        SearchTermKind::DateAtOrAfter(date) => {
            document_date.is_some_and(|document_date| document_date >= date)
        }
        SearchTermKind::DateAtOrBefore(date) => {
            document_date.is_some_and(|document_date| document_date <= date)
        }
        SearchTermKind::DateAfter(date) => {
            document_date.is_some_and(|document_date| document_date > date)
        }
        SearchTermKind::DateBefore(date) => {
            document_date.is_some_and(|document_date| document_date < date)
        }
        SearchTermKind::DateOn(date) => {
            document_date.is_some_and(|document_date| document_date == date)
        }
        SearchTermKind::State(state) => search_state_matches(document, state).unwrap_or(false),
    };
    if term.negated { !matches } else { matches }
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
            preview_snippet: body.to_string(),
            html_body: format!("<p>{body}</p>"),
            raw_headers: None,
            raw_mime: None,
            is_unread,
            is_flagged,
            is_important,
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            attachments,
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
            raw_headers: None,
            raw_mime: None,
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
                results
                    .iter()
                    .map(|result| result.reference_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["m-inbox"],
                "query {query:?}"
            );
        }

        let results = GlobalSearchEngine::query_local_mail("a:0", &messages);
        assert_eq!(results[0].reference_id, "m-sent");
        let results = GlobalSearchEngine::query_local_mail("flag:Seen", &messages);
        assert_eq!(results[0].reference_id, "m-sent");
        let results = GlobalSearchEngine::query_local_mail("is:sent", &messages);
        assert_eq!(results[0].reference_id, "m-sent");
        let results = GlobalSearchEngine::query_local_mail("is:inbox", &messages);
        assert_eq!(results[0].reference_id, "m-inbox");
        let results = GlobalSearchEngine::query_local_mail("is:not-important", &messages);
        assert_eq!(results[0].reference_id, "m-sent");
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
        assert_eq!(results[0].reference_id, "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("not:(flag:Seen)", &messages);
        assert_eq!(results[0].reference_id, "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("-from:alice@example.org", &messages);
        assert_eq!(results[0].reference_id, "m-sent");

        let results = GlobalSearchEngine::query_local_mail("not:attachment", &messages);
        assert_eq!(results[0].reference_id, "m-sent");
    }

    #[test]
    fn evolution_date_forms_support_relative_and_comparison_syntax() {
        let messages = fixture_messages();

        let results = GlobalSearchEngine::query_local_mail("sent-=:2026-08-10", &messages);
        assert_eq!(results[0].reference_id, "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("sent-<:2026-08-10", &messages);
        assert_eq!(results[0].reference_id, "m-sent");

        let results = GlobalSearchEngine::query_local_mail("sent->:2026-08-05", &messages);
        assert_eq!(results[0].reference_id, "m-inbox");

        let results = GlobalSearchEngine::query_local_mail("date:2026-08-10", &messages);
        assert_eq!(results[0].reference_id, "m-inbox");

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
            reference_id: "quoted".to_string(),
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
            reference_id: "escaped-space".to_string(),
        }];
        let results =
            GlobalSearchEngine::query_documents(SearchDomain::Mail, r#"say\ hello"#, &documents);
        assert_eq!(results.len(), 1);

        let documents = vec![SearchResultItem {
            domain: SearchDomain::Mail,
            title: "alpha beta".to_string(),
            snippet: String::new(),
            date: String::new(),
            reference_id: "phrase".to_string(),
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
                reference_id: "must-not-match".to_string(),
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
}
