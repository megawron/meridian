// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native Linux mail adapter.  EDS owns account/source configuration and
// Camel owns the actual mail store.  The adapter deliberately returns plain
// Rust records so all GTK work remains on the main thread.

use super::exchange_service::graph_service_for_configured_account_worker;
#[cfg(any(eds_camel_bridge, test))]
use super::mail_metadata_service::MailFollowUpMetadata;
use super::mail_metadata_service::MailFollowUpState;
use super::mail_store_service::{MailBackendLocation, ParsedMail, assign_thread_metadata};
use super::mail_transport_service::{MailTransportMode, MailTransportService};

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

static CAMEL_NATIVE_LOCK: Mutex<()> = Mutex::new(());

const RAW_MESSAGE_MEMORY_CACHE_LIMIT: usize = 64;

type RawCacheKey = (String, String, String);

static RAW_MESSAGE_CACHE: Mutex<VecDeque<(RawCacheKey, Arc<Vec<u8>>)>> =
    Mutex::new(VecDeque::new());

fn get_cached_raw_message(key: &RawCacheKey) -> Option<Vec<u8>> {
    let mut cache = RAW_MESSAGE_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(pos) = cache.iter().position(|(k, _)| k == key) {
        let (k, data) = cache.remove(pos).unwrap();
        let result = (*data).clone();
        cache.push_front((k, data));
        return Some(result);
    }
    None
}

fn put_cached_raw_message(key: RawCacheKey, data: Vec<u8>) {
    let mut cache = RAW_MESSAGE_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(pos) = cache.iter().position(|(k, _)| k == &key) {
        cache.remove(pos);
    }
    cache.push_front((key, Arc::new(data)));
    while cache.len() > RAW_MESSAGE_MEMORY_CACHE_LIMIT {
        cache.pop_back();
    }
}

const DEFAULT_MAIL_PAGE_SIZE: usize = 100_000;
const MAX_MAIL_PAGE_SIZE: usize = 100_000;
const MAX_MAIL_PAGE_OFFSET: usize = i32::MAX as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageWindow {
    offset: usize,
    limit: usize,
}

impl PageWindow {
    fn normalized(offset: usize, requested_limit: i32) -> Result<Self, String> {
        if offset > MAX_MAIL_PAGE_OFFSET {
            return Err("Der angeforderte Nachrichtenbereich ist zu groß".to_string());
        }
        let requested_limit = usize::try_from(requested_limit)
            .ok()
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_MAIL_PAGE_SIZE);
        Ok(Self {
            offset,
            limit: requested_limit.min(MAX_MAIL_PAGE_SIZE),
        })
    }
}

fn cancellation_error(cancelled: &AtomicBool, operation: &str) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err(format!("{operation} wurde abgebrochen"))
    } else {
        Ok(())
    }
}

#[cfg(any(eds_camel_bridge, test))]
fn normalized_uids(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim().to_string();
            (!value.is_empty() && seen.insert(value.clone())).then_some(value)
        })
        .collect()
}

fn deduplicate_messages(messages: &mut Vec<ParsedMail>) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut malformed_backend_identity = false;
    messages.retain(|message| {
        let key = match message.backend_location.as_ref() {
            Some(location)
                if !location.source_uid.trim().is_empty()
                    && !location.folder_name.trim().is_empty()
                    && !location.message_uid.trim().is_empty() =>
            {
                Some(format!(
                    "backend\u{1f}{}\u{1f}{}\u{1f}{}",
                    location.source_uid, location.folder_name, location.message_uid
                ))
            }
            Some(_) => {
                malformed_backend_identity = true;
                None
            }
            None if !message.id.trim().is_empty() => Some(format!("id\u{1f}{}", message.id)),
            None => None,
        };
        key.is_some_and(|key| seen.insert(key))
    });
    if malformed_backend_identity {
        Err(
            "Mailbackend lieferte eine Nachricht ohne vollständige Konto-/Ordner-/UID-Identität"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

#[cfg(any(eds_camel_bridge, test))]
fn folder_path_is_within(folder_name: &str, parent_name: &str) -> bool {
    folder_name == parent_name
        || folder_name
            .strip_prefix(parent_name)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(any(eds_camel_bridge, test))]
fn renamed_folder_path(folder_name: &str, old_name: &str, new_name: &str) -> Option<String> {
    folder_name.strip_prefix(old_name).and_then(|suffix| {
        (suffix.is_empty() || suffix.starts_with('/')).then(|| format!("{new_name}{suffix}"))
    })
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CamelFolderSummary {
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) folder_name: String,
    pub(crate) folder_id: String,
    pub(crate) display_name: String,
    pub(crate) unread_count: u32,
    pub(crate) total_count: u32,
    pub(crate) is_selectable: bool,
}

/// One immutable row from Camel's authoritative subscription-list response.
/// This record exists only for a source that implements `CamelSubscribable`;
/// Graph and the app-owned local fallback never manufacture this capability.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct CamelSubscriptionFolder {
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) folder_name: String,
    pub(crate) parent_folder_name: Option<String>,
    pub(crate) display_name: String,
    pub(crate) unread_count: u32,
    pub(crate) total_count: u32,
    pub(crate) is_selectable: bool,
    pub(crate) is_subscribed: bool,
}

/// Exact-source subscription capability plus its latest recursive snapshot.
/// `is_subscribable == false` is a successful Camel capability probe, not an
/// empty subscription result and never authorizes a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct CamelSubscriptionSnapshot {
    pub(crate) source_uid: String,
    pub(crate) is_subscribable: bool,
    pub(crate) folders: Vec<CamelSubscriptionFolder>,
}

/// Provider-derived availability for one exact Camel folder.  Copy and move
/// stay explicit false because Camel has no atomic folder-transfer primitive;
/// callers must not turn that absence into a local or Graph fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct CamelFolderMutationCapabilities {
    pub(crate) source_uid: String,
    pub(crate) folder_name: String,
    pub(crate) can_create_child: bool,
    pub(crate) can_rename: bool,
    pub(crate) can_delete: bool,
    pub(crate) can_refresh: bool,
    pub(crate) can_copy: bool,
    pub(crate) can_move: bool,
}

/// Direct Camel operations supported by the native bridge.  Copy and move
/// deliberately remain representable so callers receive an explicit native
/// unsupported error instead of silently emulating a folder transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CamelFolderMutationKind {
    CreateChild,
    Rename,
    Delete,
    Refresh,
    Copy,
    Move,
}

impl CamelFolderMutationKind {
    fn ffi_code(self) -> i32 {
        match self {
            Self::CreateChild => 1,
            Self::Rename => 2,
            Self::Delete => 3,
            Self::Refresh => 4,
            Self::Copy => 5,
            Self::Move => 6,
        }
    }

    fn requires_target_name(self) -> bool {
        matches!(self, Self::CreateChild | Self::Rename)
    }
}

fn normalize_folder_mutation_capabilities(
    source_uid: &str,
    folder_name: &str,
    can_create_child: bool,
    can_rename: bool,
    can_delete: bool,
    can_refresh: bool,
    can_copy: bool,
    can_move: bool,
) -> Result<CamelFolderMutationCapabilities, String> {
    let source_uid = source_uid.trim();
    let folder_name = folder_name.trim();
    if source_uid.is_empty() || folder_name.is_empty() {
        return Err(
            "Camel-Ordneraktionen benötigen eine genaue Quelle und einen Ordnernamen".to_string(),
        );
    }
    if can_copy || can_move {
        return Err(
            "Die Camel-Bridge darf keine nicht-atomare Ordner-Kopier- oder Verschiebefähigkeit behaupten"
                .to_string(),
        );
    }
    Ok(CamelFolderMutationCapabilities {
        source_uid: source_uid.to_string(),
        folder_name: folder_name.to_string(),
        can_create_child,
        can_rename,
        can_delete,
        can_refresh,
        can_copy: false,
        can_move: false,
    })
}

#[allow(dead_code)]
fn normalize_subscription_snapshot(
    source_uid: &str,
    is_subscribable: bool,
    mut folders: Vec<CamelSubscriptionFolder>,
) -> Result<CamelSubscriptionSnapshot, String> {
    let source_uid = source_uid.trim();
    if source_uid.is_empty() {
        return Err("Camel-Abonnements benötigen eine genaue EDS-Quellen-ID".to_string());
    }
    if !is_subscribable {
        if !folders.is_empty() {
            return Err(
                "Eine nicht abonnierbare Camel-Quelle lieferte Abonnementordner".to_string(),
            );
        }
        return Ok(CamelSubscriptionSnapshot {
            source_uid: source_uid.to_string(),
            is_subscribable: false,
            folders,
        });
    }

    let mut seen = HashSet::new();
    for folder in &mut folders {
        if folder.source_uid != source_uid
            || folder.folder_name.trim().is_empty()
            || folder.display_name.trim().is_empty()
            || !seen.insert(folder.folder_name.clone())
        {
            return Err(
                "Camel-Abonnementsliste enthält keine eindeutigen genauen Ordneridentitäten"
                    .to_string(),
            );
        }
        if let Some(parent) = folder.parent_folder_name.as_deref() {
            if parent.trim().is_empty() || parent == folder.folder_name {
                return Err(
                    "Camel-Abonnementsliste enthält eine ungültige Ordnerhierarchie".to_string(),
                );
            }
        }
    }

    Ok(CamelSubscriptionSnapshot {
        source_uid: source_uid.to_string(),
        is_subscribable: true,
        folders,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CamelMailLoad {
    pub(crate) messages: Vec<ParsedMail>,
    pub(crate) folders: Vec<CamelFolderSummary>,
    pub(crate) configured_sources: usize,
    pub(crate) successful_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) warning: Option<String>,
    pub(crate) is_graph: bool,
    pub(crate) search_query: Option<String>,
    pub(crate) graph_next_links: HashMap<String, String>,
    /// Opaque continuation for a Graph search.  This is deliberately kept
    /// separate from `graph_next_links`, which is keyed by folder ID for the
    /// ordinary folder-list paging path.
    #[serde(default)]
    pub(crate) graph_search_next_cursor: Option<String>,
}

pub(crate) fn mail_cache_path() -> std::path::PathBuf {
    crate::services::app_cache_dir().join("mail_load_cache.json")
}

/// Build the local-store facade without resolving GSettings.  Graph and Camel
/// provider work runs on workers; the facade is only needed for portable
/// filter-rule loading and therefore uses the same explicit environment/data
/// root policy as the other framework-neutral service paths.
fn worker_mail_store_service() -> crate::services::mail_store_service::MailStoreService {
    let mail_dir = std::env::var_os("EVOLUTION_GTK4_MAIL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::services::app_data_dir().join("mail"));
    crate::services::mail_store_service::MailStoreService::with_mail_dir(mail_dir)
}

pub(crate) fn save_mail_load_cache(load: &CamelMailLoad) {
    if load.messages.is_empty() && load.folders.is_empty() {
        return;
    }
    let path = mail_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(load) {
        let _ = std::fs::write(path, json);
    }
}

#[allow(dead_code)]
pub(crate) fn load_mail_load_cache() -> Option<CamelMailLoad> {
    let started = std::time::Instant::now();
    let path = mail_cache_path();
    let content = std::fs::read_to_string(path).ok()?;
    let cached: CamelMailLoad = serde_json::from_str(&content).ok()?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        messages = cached.messages.len(),
        folders = cached.folders.len(),
        bytes = content.len(),
        origin = "cache",
        "Prepared startup mail cache"
    );
    Some(cached)
}

/// Portable Exchange fallback.  It deliberately returns the same plain
/// records as the native Camel adapter, so the mail page, threading, search,
/// flags and folder UI do not need a second remote-only model.  The initial
/// aggregate request loads a bounded page from each folder; a later folder
/// selection narrows the request to one folder.
pub(crate) fn load_messages_graph(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    offset: usize,
    max_messages_per_folder: i32,
    cursors: &HashMap<String, String>,
    cancelled: &Arc<AtomicBool>,
) -> Result<CamelMailLoad, String> {
    let window = PageWindow::normalized(offset, max_messages_per_folder)?;
    let Some((configured_source_uid, service)) = graph_service_for_configured_account_worker()
    else {
        return Err("Kein aktiviertes Exchange-Profil mit Graph-Zugriff verfügbar".to_string());
    };
    let service = service.with_cancellation_flag(cancelled.clone());
    if let Some(source_uid) = source_uid {
        if source_uid != configured_source_uid {
            return Err("Das angeforderte Konto ist kein Graph-Exchange-Konto".to_string());
        }
    }
    cancellation_error(cancelled, "Graph-Mail-Laden")?;
    let graph_folders = service
        .list_mail_folders_sync()
        .map_err(|error| error.to_string())?;
    cancellation_error(cancelled, "Graph-Mail-Laden")?;
    let folders = graph_folders
        .iter()
        .map(|folder| CamelFolderSummary {
            source_uid: configured_source_uid.clone(),
            source_display_name: service.username.clone(),
            folder_name: folder.display_name.clone(),
            folder_id: folder.id.clone(),
            display_name: folder.display_name.clone(),
            unread_count: folder.unread_item_count as u32,
            total_count: folder.total_item_count as u32,
            is_selectable: true,
        })
        .collect::<Vec<_>>();

    let selected = graph_folders
        .iter()
        .filter(|folder| {
            folder_name
                .map(|name| folder_matches_name(&folder.display_name, name))
                .unwrap_or_else(|| is_initial_graph_folder(&folder.display_name))
        })
        .cloned()
        .collect::<Vec<_>>();
    if folder_name.is_some() && selected.is_empty() {
        return Err("Der angeforderte Graph-Mailordner wurde nicht gefunden".to_string());
    }

    let mut messages = Vec::new();
    let mut failed_folders = Vec::new();
    let mut graph_next_links = HashMap::new();
    for folder in selected {
        let cursor = (window.offset > 0)
            .then(|| cursors.get(&folder.id).map(String::as_str))
            .flatten();
        if window.offset > 0 && cursor.is_none() {
            continue;
        }
        cancellation_error(cancelled, "Graph-Mail-Laden")?;
        let page = service
            .list_messages_cursor_sync(&folder.id, cursor, window.limit)
            .map(|(page, next_link)| {
                if let Some(next_link) = next_link {
                    graph_next_links.insert(folder.id.clone(), next_link);
                }
                page.into_iter()
                    .map(|message| {
                        message.into_parsed_mail(&configured_source_uid, &folder.display_name)
                    })
                    .collect::<Vec<_>>()
            });
        match page {
            Ok(mut page) => messages.append(&mut page),
            Err(error) => {
                // A transient failure while following a continuation must not
                // silently mark that folder as exhausted. Keep the last known
                // cursor so the next paging request can retry the same page.
                if let Some(cursor) = cursor {
                    graph_next_links.insert(folder.id.clone(), cursor.to_string());
                }
                failed_folders.push(format!("{}: {error}", folder.display_name));
            }
        }
        cancellation_error(cancelled, "Graph-Mail-Laden")?;
    }
    if messages.is_empty() && !failed_folders.is_empty() {
        return Err(format!(
            "Graph-Mailordner konnten nicht geladen werden: {}",
            failed_folders.join("; ")
        ));
    }
    deduplicate_messages(&mut messages)?;
    cancellation_error(cancelled, "Graph-Mail-Laden")?;
    worker_mail_store_service().apply_backend_filters(&mut messages);
    cancellation_error(cancelled, "Graph-Mail-Laden")?;
    assign_thread_metadata(&mut messages);
    cancellation_error(cancelled, "Graph-Mail-Laden")?;
    Ok(CamelMailLoad {
        messages,
        folders,
        configured_sources: 1,
        successful_sources: usize::from(failed_folders.is_empty()),
        failed_sources: usize::from(!failed_folders.is_empty()),
        warning: (!failed_folders.is_empty()).then(|| failed_folders.join("; ")),
        is_graph: true,
        search_query: None,
        graph_next_links,
        graph_search_next_cursor: None,
    })
}

/// Run a portable Graph mailbox search and convert the results into the same
/// records used by Camel.  Folder discovery stays in this worker so the GTK
/// thread never has to resolve a provider folder ID or perform HTTP I/O.
pub(crate) fn search_messages_graph(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    query: &str,
    max_items: i32,
    cancelled: &Arc<AtomicBool>,
) -> Result<CamelMailLoad, String> {
    search_messages_graph_page(source_uid, folder_name, query, None, max_items, cancelled)
}

/// Run one bounded page of a portable Graph mailbox search.
///
/// `cursor == None` starts a new KQL search.  A present cursor is an opaque,
/// origin-validated Graph continuation URL and must be passed back unchanged
/// by the Exchange service.  The legacy `search_messages_graph` wrapper above
/// keeps existing callers source-compatible while MailPage adopts paging.
pub(crate) fn search_messages_graph_page(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    query: &str,
    cursor: Option<&str>,
    max_items: i32,
    cancelled: &Arc<AtomicBool>,
) -> Result<CamelMailLoad, String> {
    let window = PageWindow::normalized(0, max_items)?;
    let Some((configured_source_uid, service)) = graph_service_for_configured_account_worker()
    else {
        return Err("Kein aktiviertes Exchange-Profil mit Graph-Zugriff verfügbar".to_string());
    };
    let service = service.with_cancellation_flag(cancelled.clone());
    if let Some(source_uid) = source_uid {
        if source_uid != configured_source_uid {
            return Err("Das angeforderte Konto ist kein Graph-Exchange-Konto".to_string());
        }
    }
    cancellation_error(cancelled, "Graph-Mail-Suche")?;

    let graph_folders = service
        .list_mail_folders_sync()
        .map_err(|error| error.to_string())?;
    cancellation_error(cancelled, "Graph-Mail-Suche")?;
    let selected_folder = folder_name.and_then(|wanted| {
        graph_folders
            .iter()
            .find(|folder| folder_matches_name(&folder.display_name, wanted))
    });
    if cursor.is_none() && folder_name.is_some() && selected_folder.is_none() {
        return Err("Der angeforderte Graph-Mailordner wurde nicht gefunden".to_string());
    }
    let search_result = service
        .search_messages_cursor_sync(
            selected_folder.map(|folder| folder.id.as_str()),
            folder_name,
            query,
            cursor,
            window.limit,
        )
        .map_err(|error| error.to_string())?;
    let is_complete = search_result.is_complete;
    cancellation_error(cancelled, "Graph-Mail-Suche")?;
    let mut messages = search_result
        .messages
        .into_iter()
        .map(|message| {
            let display_folder = folder_name
                .map(str::to_string)
                .or_else(|| {
                    graph_folders
                        .iter()
                        .find(|folder| folder.id == message.folder_id)
                        .map(|folder| folder.display_name.clone())
                })
                .unwrap_or_else(|| "Alle Nachrichten".to_string());
            message.into_parsed_mail(&configured_source_uid, &display_folder)
        })
        .collect::<Vec<_>>();
    deduplicate_messages(&mut messages)?;
    cancellation_error(cancelled, "Graph-Mail-Suche")?;
    worker_mail_store_service().apply_backend_filters(&mut messages);
    cancellation_error(cancelled, "Graph-Mail-Suche")?;
    assign_thread_metadata(&mut messages);
    cancellation_error(cancelled, "Graph-Mail-Suche")?;
    let folders = graph_folders
        .iter()
        .map(|folder| CamelFolderSummary {
            source_uid: configured_source_uid.clone(),
            source_display_name: service.username.clone(),
            folder_name: folder.display_name.clone(),
            folder_id: folder.id.clone(),
            display_name: folder.display_name.clone(),
            unread_count: folder.unread_item_count as u32,
            total_count: folder.total_item_count as u32,
            is_selectable: true,
        })
        .collect::<Vec<_>>();
    Ok(CamelMailLoad {
        messages,
        folders,
        configured_sources: 1,
        successful_sources: 1,
        failed_sources: 0,
        warning: graph_search_warning(is_complete),
        is_graph: true,
        search_query: Some(query.to_string()),
        graph_next_links: HashMap::new(),
        graph_search_next_cursor: search_result.next_cursor,
    })
}

fn graph_search_warning(is_complete: bool) -> Option<String> {
    (!is_complete).then(|| {
        "Graph-Suche ist unvollständig: Das Ergebnislimit wurde erreicht; weitere Treffer sind möglich."
            .to_string()
    })
}

fn is_initial_graph_folder(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "inbox" | "sent items" | "drafts" | "deleted items" | "junk email" | "archive"
    )
}

fn folder_matches_name(actual: &str, wanted: &str) -> bool {
    actual.eq_ignore_ascii_case(wanted)
        || matches!(
            (
                actual.to_ascii_lowercase().as_str(),
                wanted.trim().to_ascii_lowercase().as_str()
            ),
            ("sent items", "sent" | "sent-mail")
                | ("drafts", "draft")
                | ("deleted items", "trash" | "deleted")
                | ("junk email", "junk" | "spam")
        )
}

/// Camel folder flags are not reliable for every provider.  In particular,
/// some IMAP/Exchange stores expose every folder as `all` even though the
/// folder's full name is still available.  Keep the Evolution-compatible
/// built-in folder selection usable by deriving the stable UI id from that
/// name as a fallback.
pub(crate) fn normalized_camel_folder_id(folder_id: &str, folder_name: &str) -> String {
    let explicit = folder_id.trim().to_ascii_lowercase();
    let explicit_id = match explicit.as_str() {
        "inbox" => Some("inbox"),
        "sent" | "sent-mail" | "sent items" => Some("sent"),
        "draft" | "drafts" => Some("drafts"),
        "trash" | "deleted" | "deleted items" => Some("trash"),
        "junk" | "spam" | "junk email" => Some("junk"),
        "archive" => Some("archive"),
        "all" | "" => None,
        _ => return folder_id.trim().to_string(),
    };
    if let Some(explicit_id) = explicit_id {
        return explicit_id.to_string();
    }

    let name = folder_name.trim().to_ascii_lowercase();
    let leaf = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name.as_str())
        .trim();
    match leaf {
        "inbox" | "posteingang" => "inbox",
        "sent" | "sent items" | "sent mail" | "sent-mail" | "gesendet" | "gesendete elemente" => {
            "sent"
        }
        "draft" | "drafts" | "entwurf" | "entwürfe" => "drafts",
        "trash"
        | "deleted"
        | "deleted items"
        | "papierkorb"
        | "gelöscht"
        | "gelöschte elemente" => "trash",
        "junk" | "junk email" | "spam" | "junk-e-mail" => "junk",
        "archive" | "archiv" => "archive",
        _ => "all",
    }
    .to_string()
}

/// Parse Evolution's native cache `folder-tree` (INI KeyFile) into CamelFolderSummary records.
pub(crate) fn parse_folder_tree_content(
    content: &str,
    source_uid: &str,
    source_display_name: &str,
) -> Vec<CamelFolderSummary> {
    struct RawEntry {
        name: String,
        parent_id: String,
        total: u32,
        unread: u32,
    }

    let mut entries = HashMap::<String, RawEntry>::new();
    let mut current_sec = String::new();
    let mut cur_name = String::new();
    let mut cur_parent = String::new();
    let mut cur_total = 0u32;
    let mut cur_unread = 0u32;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if !current_sec.is_empty() && !cur_name.is_empty() {
                entries.insert(
                    current_sec.clone(),
                    RawEntry {
                        name: cur_name.clone(),
                        parent_id: cur_parent.clone(),
                        total: cur_total,
                        unread: cur_unread,
                    },
                );
            }
            current_sec = trimmed[1..trimmed.len() - 1].to_string();
            cur_name.clear();
            cur_parent.clear();
            cur_total = 0;
            cur_unread = 0;
            continue;
        }
        if current_sec.starts_with("##") {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            match key {
                "DisplayName" => cur_name = val.to_string(),
                "ParentFolderId" => cur_parent = val.to_string(),
                "Total" => cur_total = val.parse().unwrap_or(0),
                "UnRead" => cur_unread = val.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if !current_sec.is_empty() && !cur_name.is_empty() && !current_sec.starts_with("##") {
        entries.insert(
            current_sec,
            RawEntry {
                name: cur_name,
                parent_id: cur_parent,
                total: cur_total,
                unread: cur_unread,
            },
        );
    }

    fn build_path(
        sec_id: &str,
        entries: &HashMap<String, RawEntry>,
        visited: &mut HashSet<String>,
    ) -> String {
        if !visited.insert(sec_id.to_string()) {
            return String::new();
        }
        let Some(entry) = entries.get(sec_id) else {
            return String::new();
        };
        if !entry.parent_id.is_empty() && entries.contains_key(&entry.parent_id) {
            let parent_path = build_path(&entry.parent_id, entries, visited);
            if !parent_path.is_empty() {
                return format!("{parent_path}/{}", entry.name);
            }
        }
        entry.name.clone()
    }

    let mut result = Vec::new();
    for (sec_id, entry) in &entries {
        let mut visited = HashSet::new();
        let full_path = build_path(sec_id, &entries, &mut visited);
        let folder_name = if full_path.is_empty() {
            entry.name.clone()
        } else {
            full_path
        };
        let folder_id = normalized_camel_folder_id("", &entry.name);
        result.push(CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id,
            folder_name,
            display_name: entry.name.clone(),
            unread_count: entry.unread,
            total_count: entry.total,
            is_selectable: true,
        });
    }

    result.sort_by(|a, b| a.folder_name.cmp(&b.folder_name));
    result
}

/// Load Evolution's cached mail folders from disk for a given source_uid.
/// If folder-tree exists in `~/.cache/evolution/mail/<source_uid>`, it parses that.
/// If not, it inspects `folders/` subdirectories.
/// If neither exists, it provides standard mailbox folders (Posteingang, Gesendet, etc.)
/// so the account always presents a proper folder tree in the UI.
pub(crate) fn load_evolution_cached_folders(
    source_uid: &str,
    source_display_name: &str,
) -> Vec<CamelFolderSummary> {
    let cache_dir = crate::services::non_empty_env_path("XDG_CACHE_HOME")
        .unwrap_or_else(|| crate::services::home_dir().join(".cache"))
        .join("evolution/mail")
        .join(source_uid);

    let tree_path = cache_dir.join("folder-tree");
    if tree_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&tree_path) {
            let parsed = parse_folder_tree_content(&content, source_uid, source_display_name);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }

    let folders_dir = cache_dir.join("folders");
    if folders_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&folders_dir) {
            let mut result = Vec::new();
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let folder_id = normalized_camel_folder_id("", &name);
                        result.push(CamelFolderSummary {
                            source_uid: source_uid.to_string(),
                            source_display_name: source_display_name.to_string(),
                            folder_id,
                            folder_name: name.clone(),
                            display_name: name,
                            unread_count: 0,
                            total_count: 0,
                            is_selectable: true,
                        });
                    }
                }
            }
            if !result.is_empty() {
                result.sort_by(|a, b| a.folder_name.cmp(&b.folder_name));
                return result;
            }
        }
    }

    vec![
        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id: "inbox".to_string(),
            folder_name: "Posteingang".to_string(),
            display_name: "Posteingang".to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: true,
        },
        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id: "drafts".to_string(),
            folder_name: "Entwürfe".to_string(),
            display_name: "Entwürfe".to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: true,
        },
        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id: "sent".to_string(),
            folder_name: "Gesendete Elemente".to_string(),
            display_name: "Gesendete Elemente".to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: true,
        },
        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id: "trash".to_string(),
            folder_name: "Gelöschte Elemente".to_string(),
            display_name: "Gelöschte Elemente".to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: true,
        },
        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id: "junk".to_string(),
            folder_name: "Junk-E-Mail".to_string(),
            display_name: "Junk-E-Mail".to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: true,
        },
        CamelFolderSummary {
            source_uid: source_uid.to_string(),
            source_display_name: source_display_name.to_string(),
            folder_id: "archive".to_string(),
            folder_name: "Archiv".to_string(),
            display_name: "Archiv".to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: true,
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CamelSyncPhase {
    Discovering,
    Connecting,
    LoadingFolders,
    LoadingMessages,
    SourceComplete,
    Complete,
    Failed,
    Retrying,
}

impl CamelSyncPhase {
    fn from_native(value: i32) -> Option<Self> {
        Some(match value {
            1 => Self::Discovering,
            2 => Self::Connecting,
            3 => Self::LoadingFolders,
            4 => Self::LoadingMessages,
            5 => Self::SourceComplete,
            6 => Self::Complete,
            7 => Self::Failed,
            8 => Self::Retrying,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CamelSyncProgress {
    pub(crate) phase: CamelSyncPhase,
    pub(crate) source_uid: Option<String>,
    pub(crate) source_display_name: Option<String>,
    pub(crate) folder_name: Option<String>,
    pub(crate) configured_sources: usize,
    pub(crate) processed_sources: usize,
    pub(crate) successful_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) folders_seen: usize,
    pub(crate) messages_loaded: usize,
    pub(crate) attempt: usize,
    pub(crate) max_attempts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CamelChangeKind {
    Changed,
    FolderDeleted,
    FolderRenamed,
    SourceAdded,
    SourceRemoved,
    SourceEnabled,
    SourceDisabled,
    SourceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CamelChangeEvent {
    pub(crate) kind: CamelChangeKind,
    pub(crate) source_uid: String,
    pub(crate) source_display_name: Option<String>,
    pub(crate) folder_name: String,
    pub(crate) old_folder_name: Option<String>,
    pub(crate) added_count: usize,
    pub(crate) removed_count: usize,
    pub(crate) changed_count: usize,
    pub(crate) recent_count: usize,
    pub(crate) added_uids: Vec<String>,
    pub(crate) removed_uids: Vec<String>,
    pub(crate) changed_uids: Vec<String>,
    pub(crate) recent_uids: Vec<String>,
}

#[cfg(any(eds_camel_bridge, test))]
#[derive(Debug, Default)]
struct CamelChangeNormalizer {
    folder_aliases: HashMap<(String, String), String>,
}

#[cfg(any(eds_camel_bridge, test))]
impl CamelChangeNormalizer {
    fn resolve_folder(&self, source_uid: &str, folder_name: &str) -> String {
        let mut current = folder_name.to_string();
        for _ in 0..16 {
            let Some((alias, target)) = self
                .folder_aliases
                .iter()
                .filter(|((source, alias), _)| {
                    source == source_uid && folder_path_is_within(&current, alias)
                })
                .max_by_key(|((_, alias), _)| alias.len())
                .map(|((_, alias), target)| (alias, target))
            else {
                break;
            };
            let Some(next) = renamed_folder_path(&current, alias, target) else {
                break;
            };
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    fn normalize(&mut self, mut event: CamelChangeEvent) -> Result<CamelChangeEvent, String> {
        event.source_uid = event.source_uid.trim().to_string();

        if matches!(
            event.kind,
            CamelChangeKind::SourceAdded
                | CamelChangeKind::SourceRemoved
                | CamelChangeKind::SourceEnabled
                | CamelChangeKind::SourceDisabled
                | CamelChangeKind::SourceChanged
        ) {
            if event.source_uid.is_empty() {
                return Err("Camel-Kontoänderung ohne Quell-UID".to_string());
            }
            return Ok(event);
        }

        event.folder_name = event.folder_name.trim().to_string();
        event.old_folder_name = event
            .old_folder_name
            .take()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        if event.source_uid.is_empty() || event.folder_name.is_empty() {
            return Err("Camel-Änderung ohne Konto- oder Ordnerkennung".to_string());
        }

        match event.kind {
            CamelChangeKind::FolderRenamed => {
                let old_folder = event.old_folder_name.clone().ok_or_else(|| {
                    "Camel-Ordnerumbenennung ohne vorherigen Ordnernamen".to_string()
                })?;
                let old_folder = self.resolve_folder(&event.source_uid, &old_folder);
                // The new name comes from Camel's current folder object and
                // is authoritative. Resolving it through an older alias would
                // break valid name reuse such as A -> B -> A.
                let new_folder = event.folder_name.clone();
                self.folder_aliases
                    .remove(&(event.source_uid.clone(), new_folder.clone()));
                for ((source, _), target) in &mut self.folder_aliases {
                    if source == &event.source_uid
                        && let Some(renamed) = renamed_folder_path(target, &old_folder, &new_folder)
                    {
                        *target = renamed;
                    }
                }
                self.folder_aliases.insert(
                    (event.source_uid.clone(), old_folder.clone()),
                    new_folder.clone(),
                );
                event.old_folder_name = Some(old_folder);
                event.folder_name = new_folder;
            }
            CamelChangeKind::Changed | CamelChangeKind::FolderDeleted => {
                event.folder_name = self.resolve_folder(&event.source_uid, &event.folder_name);
                if event.kind == CamelChangeKind::FolderDeleted {
                    let source_uid = event.source_uid.clone();
                    let deleted = event.folder_name.clone();
                    self.folder_aliases.retain(|(source, alias), target| {
                        source != &source_uid
                            || (!folder_path_is_within(alias, &deleted)
                                && !folder_path_is_within(target, &deleted))
                    });
                }
            }
            _ => {}
        }

        event.removed_uids = normalized_uids(event.removed_uids);
        let removed = event.removed_uids.iter().cloned().collect::<HashSet<_>>();
        event.added_uids = normalized_uids(event.added_uids)
            .into_iter()
            .filter(|uid| !removed.contains(uid))
            .collect();
        let added = event.added_uids.iter().cloned().collect::<HashSet<_>>();
        event.changed_uids = normalized_uids(event.changed_uids)
            .into_iter()
            .filter(|uid| !removed.contains(uid) && !added.contains(uid))
            .collect();
        event.recent_uids = normalized_uids(event.recent_uids)
            .into_iter()
            .filter(|uid| !removed.contains(uid))
            .collect();
        event.added_count = event.added_uids.len();
        event.removed_count = event.removed_uids.len();
        event.changed_count = event.changed_uids.len();
        event.recent_count = event.recent_uids.len();
        Ok(event)
    }
}

pub(crate) fn shared_mailbox_container_prefixes(email: &str, display_name: &str) -> Vec<String> {
    let mut prefixes = Vec::new();
    let email = email.trim();
    let dname = display_name.trim();

    if !email.is_empty() {
        prefixes.push(format!("Foreign Folders/Postfach — {email}"));
        prefixes.push(format!("Foreign Folders/Postfach - {email}"));
        prefixes.push(format!("Foreign Folders/Mailbox — {email}"));
        prefixes.push(format!("Foreign Folders/Mailbox - {email}"));
        prefixes.push(format!("Postfach — {email}"));
        prefixes.push(format!("Postfach - {email}"));
    }

    if !dname.is_empty() && dname != email {
        prefixes.push(format!("Foreign Folders/Postfach — {dname}"));
        prefixes.push(format!("Foreign Folders/Postfach - {dname}"));
        prefixes.push(format!("Foreign Folders/Mailbox — {dname}"));
        prefixes.push(format!("Foreign Folders/Mailbox - {dname}"));
        prefixes.push(format!("Postfach — {dname}"));
        prefixes.push(format!("Postfach - {dname}"));
    }

    if let Some((local_part, _)) = email.split_once('@') {
        if !local_part.is_empty() && local_part != dname && local_part != email {
            prefixes.push(format!("Foreign Folders/Postfach — {local_part}"));
            prefixes.push(format!("Foreign Folders/Postfach - {local_part}"));
            prefixes.push(format!("Foreign Folders/Mailbox — {local_part}"));
            prefixes.push(format!("Foreign Folders/Mailbox - {local_part}"));
            prefixes.push(format!("Postfach — {local_part}"));
            prefixes.push(format!("Postfach - {local_part}"));
        }
    }

    prefixes
}

pub(crate) fn strip_shared_mailbox_prefix<'a>(
    folder_name: &'a str,
    email: &str,
    display_name: &str,
) -> Option<(&'a str, bool)> {
    let prefixes = shared_mailbox_container_prefixes(email, display_name);
    for prefix in &prefixes {
        if folder_name == prefix.as_str() {
            return Some(("", true));
        }
        if let Some(rest) = folder_name.strip_prefix(prefix.as_str()) {
            if let Some(subpath) = rest.strip_prefix('/') {
                return Some((subpath, false));
            }
        }
    }
    None
}

/// Resolves a shared mailbox / delegate mailbox source UID and folder name to its
/// primary authenticated host account UID and the delegate folder path in Camel
/// (`Foreign Folders/Postfach — <email_or_dname>/<folder_name>`).
pub(crate) fn resolve_shared_mailbox_target(
    source_uid: &str,
    folder_name: &str,
) -> (String, String) {
    let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
    for profile in &profiles {
        if !profile.is_shared_mailbox {
            continue;
        }
        let eds_uid = crate::services::account_config_service::eds_source_uid_for_email(&profile.email);
        let mail_uid = format!("{eds_uid}-mail");
        if profile.id == source_uid
            || profile.id.strip_prefix("eds:") == Some(source_uid)
            || profile.email == source_uid
            || eds_uid == source_uid
            || mail_uid == source_uid
        {
            let parent_uid = profile.parent_account_id.as_deref().unwrap_or("").trim();
            let parent_source_uid = if !parent_uid.is_empty() {
                if let Some(parent_profile) = profiles.iter().find(|p| {
                    p.id == parent_uid
                        || p.id.strip_prefix("eds:") == Some(parent_uid)
                        || p.email == parent_uid
                }) {
                    if let Some(real_eds) = parent_profile.id.strip_prefix("eds:") {
                        real_eds.to_string()
                    } else {
                        crate::services::account_config_service::eds_source_uid_for_email(&parent_profile.email)
                    }
                } else {
                    parent_uid.to_string()
                }
            } else {
                profiles
                    .iter()
                    .find(|p| !p.is_shared_mailbox && p.protocol == crate::services::account_config_service::AccountProtocol::Exchange)
                    .map(|p| {
                        if let Some(real_eds) = p.id.strip_prefix("eds:") {
                            real_eds.to_string()
                        } else {
                            crate::services::account_config_service::eds_source_uid_for_email(&p.email)
                        }
                    })
                    .unwrap_or_else(|| source_uid.to_string())
            };

            let trimmed_folder = folder_name.trim();

            let detected_prefix = if let Some(cached) = load_mail_load_cache() {
                cached.folders.iter().find_map(|f| {
                    for prefix in shared_mailbox_container_prefixes(&profile.email, &profile.display_name) {
                        if f.folder_name.starts_with(&prefix) {
                            return Some(format!("{prefix}/"));
                        }
                    }
                    None
                })
            } else {
                None
            };
            let mailbox_prefix = detected_prefix.unwrap_or_else(|| {
                format!("Foreign Folders/Postfach — {}/", profile.email.trim())
            });

            let target_subfolder = if let Some((rel, is_container)) =
                strip_shared_mailbox_prefix(trimmed_folder, &profile.email, &profile.display_name)
            {
                if is_container || rel.is_empty() {
                    "Posteingang".to_string()
                } else {
                    rel.to_string()
                }
            } else if trimmed_folder.is_empty()
                || trimmed_folder.eq_ignore_ascii_case("inbox")
            {
                "Posteingang".to_string()
            } else if trimmed_folder.eq_ignore_ascii_case("sent")
                || trimmed_folder.eq_ignore_ascii_case("sent items")
                || trimmed_folder.eq_ignore_ascii_case("gesendet")
            {
                "Gesendete Elemente".to_string()
            } else if trimmed_folder.eq_ignore_ascii_case("drafts")
                || trimmed_folder.eq_ignore_ascii_case("draft")
            {
                "Entwürfe".to_string()
            } else if trimmed_folder.eq_ignore_ascii_case("trash")
                || trimmed_folder.eq_ignore_ascii_case("deleted items")
                || trimmed_folder.eq_ignore_ascii_case("papierkorb")
            {
                "Gelöschte Elemente".to_string()
            } else if trimmed_folder.eq_ignore_ascii_case("junk")
                || trimmed_folder.eq_ignore_ascii_case("spam")
            {
                "Junk-E-Mail".to_string()
            } else {
                trimmed_folder.to_string()
            };

            let target_folder = format!("{mailbox_prefix}{target_subfolder}");
            return (parent_source_uid, target_folder);
        }
    }

    (source_uid.to_string(), folder_name.to_string())
}

#[derive(Debug, Clone)]
pub(crate) struct CamelWatcherStatus {
    pub(crate) configured_sources: usize,
    pub(crate) successful_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum CamelSyncEvent {
    Ready(CamelWatcherStatus),
    Change(CamelChangeEvent),
    ResyncRequired(String),
    Error(String),
}

impl CamelMailLoad {
    pub(crate) fn has_more_for(
        &self,
        source_uid: Option<&str>,
        folder_name: Option<&str>,
        message_offset: usize,
        max_messages_per_folder: i32,
    ) -> bool {
        let Ok(window) = PageWindow::normalized(message_offset, max_messages_per_folder) else {
            return false;
        };
        let Some(next_offset) = window.offset.checked_add(window.limit) else {
            return false;
        };

        if self.folders.is_empty() {
            return self.messages.len() >= window.limit;
        }

        let matching_folders: Vec<_> = self
            .folders
            .iter()
            .filter(|folder| {
                let source_matches = source_uid
                    .map(|uid| folder.source_uid == uid)
                    .unwrap_or(true);
                let folder_matches = folder_name
                    .map(|name| folder_matches_name(&folder.folder_name, name))
                    .unwrap_or_else(|| !self.is_graph || is_initial_graph_folder(&folder.folder_name));

                source_matches && folder_matches && folder.is_selectable
            })
            .collect();

        if !matching_folders.is_empty() {
            let has_uncounted = matching_folders.iter().any(|folder| folder.total_count <= 0);
            let total_known_count: usize = matching_folders
                .iter()
                .map(|folder| folder.total_count.max(0) as usize)
                .sum();
            if has_uncounted {
                (total_known_count > next_offset) || (self.messages.len() >= window.limit)
            } else {
                total_known_count > next_offset
            }
        } else {
            self.messages.len() >= window.limit
        }
    }
}

#[cfg(eds_camel_bridge)]
mod ffi {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelMessage {
        pub(super) source_uid: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) folder_id: *mut c_char,
        pub(super) message_uid: *mut c_char,
        pub(super) raw_message: *mut u8,
        pub(super) raw_message_len: usize,
        pub(super) is_unread: i32,
        pub(super) is_flagged: i32,
        pub(super) follow_up_status: i32,
        pub(super) follow_up: *mut c_char,
        pub(super) completed_on: *mut c_char,
        pub(super) due_by: *mut c_char,
        pub(super) is_replied: i32,
        pub(super) is_forwarded: i32,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelFolder {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) folder_id: *mut c_char,
        pub(super) display_name: *mut c_char,
        pub(super) unread_count: u32,
        pub(super) total_count: u32,
        pub(super) is_selectable: i32,
    }

    #[repr(C)]
    #[allow(dead_code)]
    pub(super) struct EvolutionEdsCamelSubscriptionFolder {
        pub(super) source_uid: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) parent_folder_name: *mut c_char,
        pub(super) display_name: *mut c_char,
        pub(super) unread_count: u32,
        pub(super) total_count: u32,
        pub(super) is_selectable: i32,
        pub(super) is_subscribed: i32,
    }

    #[repr(C)]
    #[allow(dead_code)]
    pub(super) struct EvolutionEdsCamelFolderMutationCapabilities {
        pub(super) can_create_child: i32,
        pub(super) can_rename: i32,
        pub(super) can_delete: i32,
        pub(super) can_refresh: i32,
        pub(super) can_copy: i32,
        pub(super) can_move: i32,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelOperation {
        pub(super) kind: i32,
        pub(super) source_uid: *const c_char,
        pub(super) folder_name: *const c_char,
        pub(super) message_uid: *const c_char,
        pub(super) message_id: *const c_char,
        pub(super) destination_folder: *const c_char,
        pub(super) is_unread: i32,
        pub(super) is_flagged: i32,
        pub(super) is_junk: i32,
        pub(super) is_important: i32,
        pub(super) follow_up_status: i32,
        pub(super) due_by: *const c_char,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelSyncProgress {
        pub(super) phase: i32,
        pub(super) source_uid: *const c_char,
        pub(super) source_display_name: *const c_char,
        pub(super) folder_name: *const c_char,
        pub(super) configured_sources: usize,
        pub(super) processed_sources: usize,
        pub(super) successful_sources: usize,
        pub(super) failed_sources: usize,
        pub(super) folders_seen: usize,
        pub(super) messages_loaded: usize,
    }

    #[repr(C)]
    pub(super) struct EvolutionEdsCamelChangeEvent {
        pub(super) kind: i32,
        pub(super) source_uid: *mut c_char,
        pub(super) folder_name: *mut c_char,
        pub(super) old_folder_name: *mut c_char,
        pub(super) source_display_name: *mut c_char,
        pub(super) added_count: usize,
        pub(super) removed_count: usize,
        pub(super) changed_count: usize,
        pub(super) recent_count: usize,
        pub(super) added_uids: *mut *mut c_char,
        pub(super) added_uid_count: usize,
        pub(super) removed_uids: *mut *mut c_char,
        pub(super) removed_uid_count: usize,
        pub(super) changed_uids: *mut *mut c_char,
        pub(super) changed_uid_count: usize,
        pub(super) recent_uids: *mut *mut c_char,
        pub(super) recent_uid_count: usize,
    }

    pub(super) type EvolutionEdsCamelProgressCallback = unsafe extern "C" fn(
        progress: *const EvolutionEdsCamelSyncProgress,
        user_data: *mut c_void,
    );

    pub(super) type EvolutionEdsCamelCancellationCallback =
        unsafe extern "C" fn(user_data: *mut c_void) -> i32;

    unsafe extern "C" {
        pub(super) fn evolution_eds_camel_load_messages(
            source_uid: *const c_char,
            folder_name: *const c_char,
            message_offset: i32,
            max_messages_per_folder: i32,
            out_messages: *mut *mut EvolutionEdsCamelMessage,
            out_count: *mut usize,
            out_folders: *mut *mut EvolutionEdsCamelFolder,
            out_folder_count: *mut usize,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
            progress_callback: Option<EvolutionEdsCamelProgressCallback>,
            progress_user_data: *mut c_void,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_load_messages_by_uid(
            source_uid: *const c_char,
            folder_name: *const c_char,
            message_uids: *const *const c_char,
            message_uid_count: usize,
            out_messages: *mut *mut EvolutionEdsCamelMessage,
            out_count: *mut usize,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_free_messages(
            messages: *mut EvolutionEdsCamelMessage,
            count: usize,
        );

        pub(super) fn evolution_eds_camel_free_folders(
            folders: *mut EvolutionEdsCamelFolder,
            count: usize,
        );

        #[allow(dead_code)]
        pub(super) fn evolution_eds_camel_load_subscription_folders(
            source_uid: *const c_char,
            out_folders: *mut *mut EvolutionEdsCamelSubscriptionFolder,
            out_folder_count: *mut usize,
            out_is_subscribable: *mut i32,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        #[allow(dead_code)]
        pub(super) fn evolution_eds_camel_free_subscription_folders(
            folders: *mut EvolutionEdsCamelSubscriptionFolder,
            count: usize,
        );

        #[allow(dead_code)]
        pub(super) fn evolution_eds_camel_set_folder_subscription(
            source_uid: *const c_char,
            folder_name: *const c_char,
            subscribed: i32,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        #[allow(dead_code)]
        pub(super) fn evolution_eds_camel_get_folder_mutation_capabilities(
            source_uid: *const c_char,
            folder_name: *const c_char,
            out_capabilities: *mut EvolutionEdsCamelFolderMutationCapabilities,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        #[allow(dead_code)]
        pub(super) fn evolution_eds_camel_apply_folder_mutation(
            kind: i32,
            source_uid: *const c_char,
            folder_name: *const c_char,
            target_name: *const c_char,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_apply_operation(
            operation: *const EvolutionEdsCamelOperation,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_send_message(
            source_uid: *const c_char,
            raw_message: *const u8,
            raw_message_len: usize,
            out_sent_message_saved: *mut i32,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_save_message(
            source_uid: *const c_char,
            raw_message: *const u8,
            raw_message_len: usize,
            folder: i32,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_watcher_new(
            source_uid: *const c_char,
            out_watcher: *mut *mut c_void,
            out_configured_sources: *mut usize,
            out_successful_sources: *mut usize,
            out_failed_sources: *mut usize,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_watcher_next_event(
            watcher: *mut c_void,
            timeout_milliseconds: i32,
            out_event: *mut EvolutionEdsCamelChangeEvent,
        ) -> i32;

        pub(super) fn evolution_eds_camel_free_change_event(
            event: *mut EvolutionEdsCamelChangeEvent,
        );

        pub(super) fn evolution_eds_camel_watcher_free(watcher: *mut c_void);

        pub(super) fn evolution_eds_camel_free_error(error_message: *mut c_char);

        pub(super) fn evolution_eds_camel_store_password(
            source_uid: *const c_char,
            password: *const c_char,
            out_error: *mut *mut c_char,
        ) -> i32;

        pub(super) fn evolution_eds_camel_search_folder(
            source_uid: *const c_char,
            folder_name: *const c_char,
            query_sexpr: *const c_char,
            out_uids: *mut *mut *mut c_char,
            out_count: *mut usize,
            out_error: *mut *mut c_char,
            cancellation_callback: Option<EvolutionEdsCamelCancellationCallback>,
            cancellation_user_data: *mut c_void,
        ) -> i32;

        pub(super) fn evolution_eds_camel_free_uids(
            uids: *mut *mut c_char,
            count: usize,
        );
    }
}

pub fn store_source_password(source_uid: &str, password: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let source_uid_c = CString::new(source_uid)
            .map_err(|_| "Quell-UID enthält ungültige Nullbytes".to_string())?;
        let password_c = CString::new(password)
            .map_err(|_| "Passwort enthält ungültige Nullbytes".to_string())?;
        let mut out_error: *mut std::os::raw::c_char = std::ptr::null_mut();

        let ok = unsafe {
            ffi::evolution_eds_camel_store_password(
                source_uid_c.as_ptr(),
                password_c.as_ptr(),
                &mut out_error,
            ) != 0
        };

        if !ok {
            let msg = if !out_error.is_null() {
                let err_str = unsafe {
                    std::ffi::CStr::from_ptr(out_error)
                        .to_string_lossy()
                        .into_owned()
                };
                unsafe { ffi::evolution_eds_camel_free_error(out_error) };
                err_str
            } else {
                "Passwort konnte nicht im Schlüsselbund hinterlegt werden".to_string()
            };
            return Err(msg);
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        crate::services::credential_service::CredentialService::set_password(source_uid, password)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (source_uid, password);
        Ok(())
    }
}

/// Load enabled remote EDS mail accounts through the original Camel
/// providers.  macOS builds intentionally omit this Linux-only adapter; the
/// Linux build script requires the EDS development packages before enabling
/// the same code path.
pub(crate) fn default_page_limit() -> i32 {
    std::env::var("EVOLUTION_GTK4_CAMEL_MESSAGES_PER_FOLDER")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(MAX_MAIL_PAGE_SIZE as i32))
        .unwrap_or(DEFAULT_MAIL_PAGE_SIZE as i32)
}

#[allow(dead_code)]
pub(crate) fn load_messages_page(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
) -> Result<CamelMailLoad, String> {
    load_messages_page_with_progress(
        source_uid,
        folder_name,
        message_offset,
        max_messages_per_folder,
        None,
    )
}

pub(crate) fn load_messages_page_with_progress(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    load_messages_page_with_progress_and_cancellation(
        source_uid,
        folder_name,
        message_offset,
        max_messages_per_folder,
        None,
        progress_sender,
    )
}

pub(crate) fn load_messages_page_with_progress_and_cancellation(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    cancelled: Option<&AtomicBool>,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    #[cfg(eds_camel_bridge)]
    {
        const MAX_ATTEMPTS: usize = 3;

        for attempt in 1..=MAX_ATTEMPTS {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err("Mail-Ladevorgang wurde abgebrochen.".to_string());
            }

            let result = load_messages_native(
                source_uid,
                folder_name,
                message_offset,
                max_messages_per_folder,
                cancelled,
                progress_sender,
            );
            let retryable = match &result {
                Ok(load) => {
                    load.configured_sources > 0
                        && load.successful_sources == 0
                        && load.failed_sources > 0
                        && load.warning.is_some()
                }
                Err(_) => true,
            };

            if attempt == MAX_ATTEMPTS || !retryable {
                return result;
            }

            if let Some(sender) = progress_sender {
                let _ = sender.try_send(CamelSyncProgress {
                    phase: CamelSyncPhase::Retrying,
                    source_uid: source_uid.map(str::to_string),
                    source_display_name: None,
                    folder_name: folder_name.map(str::to_string),
                    configured_sources: 0,
                    processed_sources: 0,
                    successful_sources: 0,
                    failed_sources: 0,
                    folders_seen: 0,
                    messages_loaded: 0,
                    attempt: attempt + 1,
                    max_attempts: MAX_ATTEMPTS,
                });
            }

            let delay_milliseconds = 250_u64.saturating_mul(1_u64 << (attempt - 1));
            let mut waited = 0;
            while waited < delay_milliseconds {
                if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    return Err("Mail-Ladevorgang wurde abgebrochen.".to_string());
                }
                let slice = (delay_milliseconds - waited).min(25);
                std::thread::sleep(std::time::Duration::from_millis(slice));
                waited += slice;
            }
        }

        unreachable!("Camel retry loop always returns on its final attempt")
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::load_messages_page(
                source_uid,
                folder_name,
                message_offset,
                max_messages_per_folder,
                cancelled,
                progress_sender,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (
                source_uid,
                folder_name,
                message_offset,
                max_messages_per_folder,
                cancelled,
                progress_sender,
            );
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


#[cfg(eds_camel_bridge)]
fn load_messages_native(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    cancelled: Option<&AtomicBool>,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::slice;

    struct ProgressContext<'a> {
        sender: &'a async_channel::Sender<CamelSyncProgress>,
    }

    unsafe extern "C" fn progress_callback(
        progress: *const ffi::EvolutionEdsCamelSyncProgress,
        user_data: *mut c_void,
    ) {
        if progress.is_null() || user_data.is_null() {
            return;
        }

        let progress = unsafe { &*progress };
        let context = unsafe { &*(user_data as *const ProgressContext<'_>) };
        let Some(phase) = CamelSyncPhase::from_native(progress.phase) else {
            return;
        };

        let _ = context.sender.try_send(CamelSyncProgress {
            phase,
            source_uid: c_string(progress.source_uid).into_option(),
            source_display_name: c_string(progress.source_display_name).into_option(),
            folder_name: c_string(progress.folder_name).into_option(),
            configured_sources: progress.configured_sources,
            processed_sources: progress.processed_sources,
            successful_sources: progress.successful_sources,
            failed_sources: progress.failed_sources,
            folders_seen: progress.folders_seen,
            messages_loaded: progress.messages_loaded,
            attempt: 1,
            max_attempts: 1,
        });
    }

    trait EmptyStringOption {
        fn into_option(self) -> Option<String>;
    }

    impl EmptyStringOption for String {
        fn into_option(self) -> Option<String> {
            (!self.is_empty()).then_some(self)
        }
    }

    unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
        if user_data.is_null() {
            return 0;
        }

        let cancelled = unsafe { &*(user_data as *const AtomicBool) };
        cancelled.load(Ordering::Relaxed) as i32
    }

    let window = PageWindow::normalized(message_offset, max_messages_per_folder)?;
    let message_offset = window.offset as i32;
    let max_messages = window.limit as i32;

    let mut records = std::ptr::null_mut();
    let mut record_count = 0usize;
    let mut folders = std::ptr::null_mut();
    let mut folder_count = 0usize;
    let mut configured_sources = 0usize;
    let mut successful_sources = 0usize;
    let mut failed_sources = 0usize;
    let mut error_message: *mut c_char = std::ptr::null_mut();
    let (resolved_source_uid, resolved_folder_name) = match (source_uid, folder_name) {
        (Some(src), Some(fld)) => {
            let (s, f) = resolve_shared_mailbox_target(src, fld);
            (Some(s), Some(f))
        }
        (Some(src), None) => {
            let (s, _) = resolve_shared_mailbox_target(src, "");
            (Some(s), None)
        }
        _ => (source_uid.map(str::to_string), folder_name.map(str::to_string)),
    };
    let source_uid_c = resolved_source_uid
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
    let folder_name_c = resolved_folder_name
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
    let source_uid_ptr = source_uid_c
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    let folder_name_ptr = folder_name_c
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    let progress_context = progress_sender.map(|sender| ProgressContext { sender });
    let progress_user_data = progress_context
        .as_ref()
        .map_or(std::ptr::null_mut(), |context| {
            context as *const ProgressContext<'_> as *mut c_void
        });
    let progress_callback = progress_context
        .as_ref()
        .map(|_| progress_callback as ffi::EvolutionEdsCamelProgressCallback);
    let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
        cancelled as *const AtomicBool as *mut c_void
    });
    let cancellation_callback =
        cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);

    // Passing null filters asks the adapter to load every enabled EDS mail
    // account and selectable folder.  The GTK view supplies source/folder
    // filters after the user selects an account-specific row.
    let _native_lock = CAMEL_NATIVE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages(
            source_uid_ptr,
            folder_name_ptr,
            message_offset,
            max_messages,
            &mut records,
            &mut record_count,
            &mut folders,
            &mut folder_count,
            &mut configured_sources,
            &mut successful_sources,
            &mut failed_sources,
            &mut error_message,
            progress_callback,
            progress_user_data,
            cancellation_callback,
            cancellation_user_data,
        ) != 0
    };

    let warning = (!error_message.is_null()).then(|| unsafe {
        let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
        ffi::evolution_eds_camel_free_error(error_message);
        message
    });

    if !success {
        if !records.is_null() {
            unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
        }
        if !folders.is_null() {
            unsafe { ffi::evolution_eds_camel_free_folders(folders, folder_count) };
        }
        return Err(warning.unwrap_or_else(|| "EDS/Camel mail loading failed".to_string()));
    }

    let shared_profiles = crate::services::account_config_service::AccountConfigStore::default()
        .load()
        .into_iter()
        .filter(|p| p.is_shared_mailbox)
        .collect::<Vec<_>>();

    let mut folder_summaries = Vec::with_capacity(folder_count);
    if !folders.is_null() {
        let folders_slice = unsafe { slice::from_raw_parts(folders, folder_count) };
        for folder in folders_slice {
            let raw_folder_name = c_string(folder.folder_name);
            let mut mapped_source_uid = c_string(folder.source_uid);
            let mut mapped_source_display_name = c_string(folder.source_display_name);
            let mut mapped_folder_name = raw_folder_name.clone();
            let mut mapped_display_name = c_string(folder.display_name);
            let mut skip_folder = false;

            if raw_folder_name == "Foreign Folders" {
                continue;
            }

            let mut matched_shared = false;
            for profile in &shared_profiles {
                if let Some((rel_path, is_container)) =
                    strip_shared_mailbox_prefix(&raw_folder_name, &profile.email, &profile.display_name)
                {
                    matched_shared = true;
                    if is_container || rel_path.is_empty() {
                        skip_folder = true;
                        break;
                    }
                    let dname = if !profile.display_name.trim().is_empty() {
                        profile.display_name.trim()
                    } else {
                        profile.email.trim()
                    };
                    mapped_source_uid = profile.id.clone();
                    mapped_source_display_name = dname.to_string();
                    mapped_folder_name = rel_path.to_string();
                    if mapped_display_name.is_empty() || mapped_display_name == raw_folder_name {
                        mapped_display_name = rel_path.rsplit('/').next().unwrap_or(rel_path).to_string();
                    }
                    break;
                }
            }

            if skip_folder {
                continue;
            }

            if raw_folder_name.starts_with('.')
                || raw_folder_name.contains("/.")
                || mapped_folder_name.starts_with('.')
                || mapped_folder_name.contains("/.")
            {
                continue;
            }

            if !matched_shared && raw_folder_name.starts_with("Foreign Folders") {
                // Do not display foreign root/folders under the primary account
                continue;
            }

            folder_summaries.push(CamelFolderSummary {
                source_uid: mapped_source_uid,
                source_display_name: mapped_source_display_name,
                folder_id: normalized_camel_folder_id(&c_string(folder.folder_id), &mapped_folder_name),
                folder_name: mapped_folder_name,
                display_name: mapped_display_name,
                unread_count: folder.unread_count,
                total_count: folder.total_count,
                is_selectable: folder.is_selectable != 0,
            });
        }
        unsafe { ffi::evolution_eds_camel_free_folders(folders, folder_count) };
    }

    let messages = parse_native_messages(records, record_count, cancelled);
    if !records.is_null() {
        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }
    let mut messages = messages?;

    for msg in &mut messages {
        for profile in &shared_profiles {
            if let Some((rel, is_container)) =
                strip_shared_mailbox_prefix(&msg.folder_name, &profile.email, &profile.display_name)
            {
                let target_rel = if is_container || rel.is_empty() {
                    "Posteingang".to_string()
                } else {
                    rel.to_string()
                };
                msg.folder_name = target_rel.clone();
                msg.folder_id = normalized_camel_folder_id("", &target_rel);
                if let Some(ref mut loc) = msg.backend_location {
                    loc.source_uid = profile.id.clone();
                    loc.folder_name = target_rel;
                }
                break;
            } else if let (Some(orig_src), Some(orig_fld)) = (source_uid, folder_name) {
                if orig_src == profile.id || orig_src == profile.email {
                    msg.folder_name = orig_fld.to_string();
                    msg.folder_id = normalized_camel_folder_id("", orig_fld);
                    if let Some(ref mut loc) = msg.backend_location {
                        loc.source_uid = orig_src.to_string();
                        loc.folder_name = orig_fld.to_string();
                    }
                    break;
                }
            }
        }
    }

    Ok(CamelMailLoad {
        messages,
        folders: folder_summaries,
        configured_sources,
        successful_sources,
        failed_sources,
        warning,
        is_graph: false,
        search_query: None,
        graph_next_links: HashMap::new(),
        graph_search_next_cursor: None,
    })
}

/// Read the recursive subscription hierarchy for one exact EDS/Camel source.
/// This intentionally never falls through to Graph or a local mail store:
/// they do not expose Camel's subscription contract.
#[allow(dead_code)]
pub(crate) fn load_subscription_folders_with_cancellation(
    source_uid: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<CamelSubscriptionSnapshot, String> {
    let source_uid = source_uid.trim();
    if source_uid.is_empty() {
        return Err("Camel-Abonnements benötigen eine genaue EDS-Quellen-ID".to_string());
    }
    if let Some(cancelled) = cancelled {
        cancellation_error(cancelled, "Camel-Abonnementsliste")?;
    }

    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char, c_void};

        unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
            if user_data.is_null() {
                return 0;
            }
            let cancelled = unsafe { &*(user_data as *const AtomicBool) };
            cancelled.load(Ordering::Relaxed) as i32
        }

        let source_uid_c = CString::new(source_uid)
            .map_err(|_| "EDS-Quellen-ID enthält ein ungültiges NUL-Byte".to_string())?;
        let mut records = std::ptr::null_mut();
        let mut record_count = 0usize;
        let mut is_subscribable = 0;
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
            cancelled as *const AtomicBool as *mut c_void
        });
        let cancellation_callback =
            cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);
        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let success = unsafe {
            ffi::evolution_eds_camel_load_subscription_folders(
                source_uid_c.as_ptr(),
                &mut records,
                &mut record_count,
                &mut is_subscribable,
                &mut error_message,
                cancellation_callback,
                cancellation_user_data,
            ) != 0
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });

        if !success {
            if !records.is_null() {
                unsafe {
                    ffi::evolution_eds_camel_free_subscription_folders(records, record_count)
                };
            }
            return Err(error.unwrap_or_else(|| {
                "Camel-Abonnementsliste konnte nicht geladen werden".to_string()
            }));
        }

        let mut folders = Vec::with_capacity(record_count);
        if !records.is_null() {
            let records_slice = unsafe { std::slice::from_raw_parts(records, record_count) };
            for record in records_slice {
                folders.push(CamelSubscriptionFolder {
                    source_uid: c_string(record.source_uid),
                    source_display_name: c_string(record.source_display_name),
                    folder_name: c_string(record.folder_name),
                    parent_folder_name: c_string_option(record.parent_folder_name),
                    display_name: c_string(record.display_name),
                    unread_count: record.unread_count,
                    total_count: record.total_count,
                    is_selectable: record.is_selectable != 0,
                    is_subscribed: record.is_subscribed != 0,
                });
            }
            unsafe { ffi::evolution_eds_camel_free_subscription_folders(records, record_count) };
        }
        if let Some(cancelled) = cancelled {
            cancellation_error(cancelled, "Camel-Abonnementsliste")?;
        }
        normalize_subscription_snapshot(source_uid, is_subscribable != 0, folders)
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::load_subscription_folders(source_uid, cancelled)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cancelled;
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


/// Confirm one exact Camel folder subscription mutation.  The bridge only
/// returns success after it re-reads the subscription list and observes the
/// requested bit, so callers may refresh their sidebar without optimistic
/// state changes.
#[allow(dead_code)]
pub(crate) fn set_folder_subscription_with_cancellation(
    source_uid: &str,
    folder_name: &str,
    subscribed: bool,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    let (resolved_source_uid, resolved_folder_name) =
        resolve_shared_mailbox_target(source_uid, folder_name);
    let source_uid = resolved_source_uid.trim();
    let folder_name = resolved_folder_name.trim();
    if source_uid.is_empty() || folder_name.is_empty() {
        return Err(
            "Camel-Abonnements benötigen eine genaue Quelle und einen Ordnernamen".to_string(),
        );
    }
    if let Some(cancelled) = cancelled {
        cancellation_error(cancelled, "Camel-Abonnementänderung")?;
    }

    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char, c_void};

        unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
            if user_data.is_null() {
                return 0;
            }
            let cancelled = unsafe { &*(user_data as *const AtomicBool) };
            cancelled.load(Ordering::Relaxed) as i32
        }

        let source_uid = CString::new(source_uid)
            .map_err(|_| "EDS-Quellen-ID enthält ein ungültiges NUL-Byte".to_string())?;
        let folder_name = CString::new(folder_name)
            .map_err(|_| "Camel-Ordnername enthält ein ungültiges NUL-Byte".to_string())?;
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
            cancelled as *const AtomicBool as *mut c_void
        });
        let cancellation_callback =
            cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);
        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let success = unsafe {
            ffi::evolution_eds_camel_set_folder_subscription(
                source_uid.as_ptr(),
                folder_name.as_ptr(),
                subscribed as i32,
                &mut error_message,
                cancellation_callback,
                cancellation_user_data,
            ) != 0
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });
        if success {
            Ok(())
        } else {
            Err(error.unwrap_or_else(|| {
                "Camel-Abonnementänderung konnte nicht bestätigt werden".to_string()
            }))
        }
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::set_folder_subscription(
                source_uid,
                folder_name,
                subscribed,
                cancelled,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (subscribed, cancelled);
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


/// Get exact provider-derived Folder-menu availability.  This deliberately
/// has no Graph or local fallback because those backends cannot establish the
/// Camel store/folder capability contract.
#[allow(dead_code)]
pub(crate) fn folder_mutation_capabilities_with_cancellation(
    source_uid: &str,
    folder_name: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<CamelFolderMutationCapabilities, String> {
    let (resolved_source_uid, resolved_folder_name) =
        resolve_shared_mailbox_target(source_uid, folder_name);
    let source_uid = resolved_source_uid.trim();
    let folder_name = resolved_folder_name.trim();
    if source_uid.is_empty() || folder_name.is_empty() {
        return Err(
            "Camel-Ordneraktionen benötigen eine genaue Quelle und einen Ordnernamen".to_string(),
        );
    }
    if let Some(cancelled) = cancelled {
        cancellation_error(cancelled, "Camel-Ordneraktionen")?;
    }

    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char, c_void};

        unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
            if user_data.is_null() {
                return 0;
            }
            let cancelled = unsafe { &*(user_data as *const AtomicBool) };
            cancelled.load(Ordering::Relaxed) as i32
        }

        let source_uid = CString::new(source_uid)
            .map_err(|_| "EDS-Quellen-ID enthält ein ungültiges NUL-Byte".to_string())?;
        let folder_name = CString::new(folder_name)
            .map_err(|_| "Camel-Ordnername enthält ein ungültiges NUL-Byte".to_string())?;
        let mut capabilities = ffi::EvolutionEdsCamelFolderMutationCapabilities {
            can_create_child: 0,
            can_rename: 0,
            can_delete: 0,
            can_refresh: 0,
            can_copy: 0,
            can_move: 0,
        };
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
            cancelled as *const AtomicBool as *mut c_void
        });
        let cancellation_callback =
            cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);
        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let success = unsafe {
            ffi::evolution_eds_camel_get_folder_mutation_capabilities(
                source_uid.as_ptr(),
                folder_name.as_ptr(),
                &mut capabilities,
                &mut error_message,
                cancellation_callback,
                cancellation_user_data,
            ) != 0
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });
        if !success {
            return Err(error.unwrap_or_else(|| {
                "Camel-Ordneraktionen konnten nicht geprüft werden".to_string()
            }));
        }
        if let Some(cancelled) = cancelled {
            cancellation_error(cancelled, "Camel-Ordneraktionen")?;
        }
        normalize_folder_mutation_capabilities(
            source_uid.to_str().unwrap_or_default(),
            folder_name.to_str().unwrap_or_default(),
            capabilities.can_create_child != 0,
            capabilities.can_rename != 0,
            capabilities.can_delete != 0,
            capabilities.can_refresh != 0,
            capabilities.can_copy != 0,
            capabilities.can_move != 0,
        )
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            let _ = cancelled;
            crate::services::portable_mail_engine::folder_mutation_capabilities(
                source_uid,
                folder_name,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cancelled;
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


/// Execute a confirmed direct Camel folder operation.  Success means the C
/// bridge refreshed the hierarchy and verified the provider's resulting
/// state; this facade never performs optimistic Graph/local emulation.
#[allow(dead_code)]
pub(crate) fn apply_folder_mutation_with_cancellation(
    kind: CamelFolderMutationKind,
    source_uid: &str,
    folder_name: &str,
    target_name: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    let (resolved_source_uid, resolved_folder_name) =
        resolve_shared_mailbox_target(source_uid, folder_name);
    let source_uid = resolved_source_uid.trim();
    let folder_name = resolved_folder_name.trim();
    let target_name = target_name.map(str::trim).filter(|name| !name.is_empty());
    if source_uid.is_empty() || folder_name.is_empty() {
        return Err(
            "Camel-Ordneraktionen benötigen eine genaue Quelle und einen Ordnernamen".to_string(),
        );
    }
    if kind.requires_target_name() != target_name.is_some() {
        return Err(
            "Diese Camel-Ordneraktion benötigt genau dann einen neuen Namen, wenn sie erstellt oder umbenennt"
                .to_string(),
        );
    }
    if target_name.is_some_and(|name| name.contains('/')) {
        return Err("Camel-Ordnernamen dürfen kein '/' enthalten".to_string());
    }
    if let Some(cancelled) = cancelled {
        cancellation_error(cancelled, "Camel-Ordneraktion")?;
    }

    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char, c_void};

        unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
            if user_data.is_null() {
                return 0;
            }
            let cancelled = unsafe { &*(user_data as *const AtomicBool) };
            cancelled.load(Ordering::Relaxed) as i32
        }

        let source_uid = CString::new(source_uid)
            .map_err(|_| "EDS-Quellen-ID enthält ein ungültiges NUL-Byte".to_string())?;
        let folder_name = CString::new(folder_name)
            .map_err(|_| "Camel-Ordnername enthält ein ungültiges NUL-Byte".to_string())?;
        let target_name = target_name
            .map(CString::new)
            .transpose()
            .map_err(|_| "Neuer Camel-Ordnername enthält ein ungültiges NUL-Byte".to_string())?;
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
            cancelled as *const AtomicBool as *mut c_void
        });
        let cancellation_callback =
            cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);
        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let success = unsafe {
            ffi::evolution_eds_camel_apply_folder_mutation(
                kind.ffi_code(),
                source_uid.as_ptr(),
                folder_name.as_ptr(),
                target_name
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
                &mut error_message,
                cancellation_callback,
                cancellation_user_data,
            ) != 0
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });
        if success {
            if let Some(cancelled) = cancelled {
                cancellation_error(cancelled, "Camel-Ordneraktion")?;
            }
            Ok(())
        } else {
            Err(error
                .unwrap_or_else(|| "Camel-Ordneraktion konnte nicht bestätigt werden".to_string()))
        }
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::apply_folder_mutation(
                kind,
                source_uid,
                folder_name,
                target_name,
                cancelled,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (kind, target_name, cancelled);
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


#[cfg(any(eds_camel_bridge, test))]
fn follow_up_state_from_native(
    native_status: i32,
    has_follow_up_tag: bool,
    has_completed_on_tag: bool,
) -> MailFollowUpState {
    let derived = if !has_follow_up_tag {
        MailFollowUpState::None
    } else if has_completed_on_tag {
        MailFollowUpState::Completed
    } else {
        MailFollowUpState::FollowUp
    };
    match (native_status, derived) {
        (0, MailFollowUpState::None) => MailFollowUpState::None,
        (1, MailFollowUpState::FollowUp) => MailFollowUpState::FollowUp,
        (2, MailFollowUpState::Completed) => MailFollowUpState::Completed,
        _ => derived,
    }
}

#[cfg(any(eds_camel_bridge, test))]
fn follow_up_metadata_from_native(
    native_status: i32,
    text: Option<String>,
    due_by: Option<String>,
    completed_on: Option<String>,
) -> MailFollowUpMetadata {
    MailFollowUpMetadata {
        state: follow_up_state_from_native(native_status, text.is_some(), completed_on.is_some()),
        text,
        due_by,
        completed_on,
    }
    .normalized()
}

#[cfg(eds_camel_bridge)]
fn parse_native_messages(
    records: *const ffi::EvolutionEdsCamelMessage,
    record_count: usize,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<ParsedMail>, String> {
    use std::slice;

    let mut messages = Vec::with_capacity(record_count);
    if records.is_null() {
        return Ok(messages);
    }

    let records_slice = unsafe { slice::from_raw_parts(records, record_count) };
    for record in records_slice {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
            return Err("Camel-Mail-Verarbeitung wurde abgebrochen".to_string());
        }
        if record.raw_message.is_null() || record.raw_message_len == 0 {
            continue;
        }

        let source_uid = c_string(record.source_uid);
        let folder_name = c_string(record.folder_name);
        let folder_id = normalized_camel_folder_id(&c_string(record.folder_id), &folder_name);
        let message_uid = c_string(record.message_uid);
        let raw = unsafe { slice::from_raw_parts(record.raw_message, record.raw_message_len) };
        let id = format!("eds:{source_uid}:{folder_name}:{message_uid}");

        if let Some(mut parsed) = crate::services::mail_store_service::parse_rfc822_bytes(
            &id,
            &folder_id,
            raw,
            record.is_unread != 0,
        ) {
            parsed.is_flagged = record.is_flagged != 0;
            parsed.is_replied = record.is_replied != 0;
            parsed.is_forwarded = record.is_forwarded != 0;
            parsed.folder_name = folder_name;
            parsed.follow_up = follow_up_metadata_from_native(
                record.follow_up_status,
                c_string_option(record.follow_up),
                c_string_option(record.due_by),
                c_string_option(record.completed_on),
            );
            parsed.follow_up_state = parsed.follow_up.state;
            let location = MailBackendLocation {
                source_uid,
                folder_name: parsed.folder_name.clone(),
                message_uid,
                message_id: parsed.message_id.clone(),
            };
            parsed.backend_location = Some(location);
            messages.push(parsed);
        }
    }

    deduplicate_messages(&mut messages)?;
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
        return Err("Camel-Mail-Verarbeitung wurde abgebrochen".to_string());
    }
    worker_mail_store_service().apply_backend_filters(&mut messages);
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
        return Err("Camel-Mail-Verarbeitung wurde abgebrochen".to_string());
    }
    assign_thread_metadata(&mut messages);
    Ok(messages)
}

pub(crate) fn load_messages_for_uids(
    source_uid: &str,
    folder_name: &str,
    message_uids: &[String],
    cancelled: &AtomicBool,
) -> Result<Vec<ParsedMail>, String> {
    #[cfg(eds_camel_bridge)]
    {
        let (resolved_source_uid, resolved_folder_name) =
            resolve_shared_mailbox_target(source_uid, folder_name);
        let mut messages = load_messages_for_uids_native(
            &resolved_source_uid,
            &resolved_folder_name,
            message_uids,
            cancelled,
        )?;
        if source_uid != resolved_source_uid {
            for msg in &mut messages {
                msg.folder_name = folder_name.to_string();
                msg.folder_id = normalized_camel_folder_id("", folder_name);
                if let Some(ref mut loc) = msg.backend_location {
                    loc.source_uid = source_uid.to_string();
                    loc.folder_name = folder_name.to_string();
                }
            }
        }
        return Ok(messages);
    }

    #[cfg(not(eds_camel_bridge))]
    {
        let _ = (source_uid, folder_name, message_uids, cancelled);
        Err("native EDS/Camel adapter is not available in this build".to_string())
    }
}

/// Load the exact RFC822 payload for one backend message without retaining a
/// duplicate raw body in every GTK row. This is used by Evolution's
/// "Message Source" workflow and intentionally runs in a worker.
pub(crate) fn load_raw_message(
    location: &MailBackendLocation,
    cancelled: &Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    let cache_key = (
        location.source_uid.clone(),
        location.folder_name.clone(),
        location.message_uid.clone(),
    );
    if let Some(cached) = get_cached_raw_message(&cache_key) {
        cancellation_error(cancelled, "Nachrichtenquelle laden")?;
        return Ok(cached);
    }

    let source = load_raw_message_uncached(location, cancelled)?;
    put_cached_raw_message(cache_key, source.clone());
    Ok(source)
}

fn load_raw_message_uncached(
    location: &MailBackendLocation,
    cancelled: &Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    if let Some(service) =
        super::exchange_service::graph_service_for_source_worker(&location.source_uid)
    {
        cancellation_error(cancelled, "Graph-Nachrichtenquelle laden")?;
        let service = service.with_cancellation_flag(cancelled.clone());
        let source = service
            .message_mime_sync(&location.message_uid)
            .map_err(|error| error.to_string())?;
        cancellation_error(cancelled, "Graph-Nachrichtenquelle laden")?;
        return Ok(source);
    }

    // Check if the message is already cached locally on disk in Evolution's cache (<1ms, no Camel lock):
    if let Some(cache) = crate::services::evolution_summary_cache::find_evolution_account_cache(Some(&location.source_uid)) {
        if let Some(raw) = crate::services::evolution_summary_cache::read_cached_raw_message_direct(
            &cache.cache_dir,
            &location.folder_name,
            &location.message_uid,
        ) {
            cancellation_error(cancelled, "Nachrichtenquelle laden")?;
            return Ok(raw);
        }
    }

    #[cfg(eds_camel_bridge)]
    {
        return load_raw_message_native(location, cancelled);
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::load_raw_message(location, cancelled)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (location, cancelled);
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


#[cfg(eds_camel_bridge)]
fn load_raw_message_native(
    location: &MailBackendLocation,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    use std::ffi::{CStr, CString, c_char, c_void};

    unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
        if user_data.is_null() {
            return 0;
        }
        let cancelled = unsafe { &*(user_data as *const AtomicBool) };
        cancelled.load(Ordering::Relaxed) as i32
    }

    cancellation_error(cancelled, "Nachrichtenquelle laden")?;
    let (resolved_source_uid, resolved_folder_name) =
        resolve_shared_mailbox_target(&location.source_uid, &location.folder_name);
    let source_uid = CString::new(resolved_source_uid.as_str())
        .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
    let folder_name = CString::new(resolved_folder_name.as_str())
        .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
    let message_uid = CString::new(location.message_uid.as_str())
        .map_err(|_| "EDS message UID contains an invalid NUL byte".to_string())?;
    let message_uids = [message_uid.as_ptr()];
    let mut records = std::ptr::null_mut();
    let mut record_count = 0usize;
    let mut error_message: *mut c_char = std::ptr::null_mut();
    let _native_lock = CAMEL_NATIVE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Selection may have changed while waiting for another Camel operation.
    // Do not make the obsolete request connect or touch the provider.
    cancellation_error(cancelled, "Nachrichtenquelle laden")?;
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages_by_uid(
            source_uid.as_ptr(),
            folder_name.as_ptr(),
            message_uids.as_ptr(),
            message_uids.len(),
            &mut records,
            &mut record_count,
            &mut error_message,
            Some(cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback),
            cancelled as *const AtomicBool as *mut c_void,
        ) != 0
    };
    let warning = (!error_message.is_null()).then(|| unsafe {
        let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
        ffi::evolution_eds_camel_free_error(error_message);
        message
    });
    if !success {
        if !records.is_null() {
            unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
        }
        return Err(warning.unwrap_or_else(|| {
            "Die originale EDS/Camel-Nachrichtenquelle konnte nicht geladen werden".to_string()
        }));
    }

    let raw = if records.is_null() || record_count == 0 {
        None
    } else {
        let records = unsafe { std::slice::from_raw_parts(records, record_count) };
        records.iter().find_map(|record| {
            (!record.raw_message.is_null() && record.raw_message_len > 0).then(|| unsafe {
                std::slice::from_raw_parts(record.raw_message, record.raw_message_len).to_vec()
            })
        })
    };
    if !records.is_null() {
        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }
    cancellation_error(cancelled, "Nachrichtenquelle laden")?;
    raw.ok_or_else(|| "EDS/Camel lieferte keine originale Nachrichtenquelle".to_string())
}

#[cfg(eds_camel_bridge)]
fn load_messages_for_uids_native(
    source_uid: &str,
    folder_name: &str,
    message_uids: &[String],
    cancelled: &AtomicBool,
) -> Result<Vec<ParsedMail>, String> {
    use std::ffi::{CStr, CString, c_char, c_void};

    unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
        if user_data.is_null() {
            return 0;
        }

        let cancelled = unsafe { &*(user_data as *const AtomicBool) };
        cancelled.load(Ordering::Relaxed) as i32
    }

    let source_uid = CString::new(source_uid)
        .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
    let folder_name = CString::new(folder_name)
        .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
    cancellation_error(cancelled, "Inkrementelles Camel-Laden")?;
    let message_uids = normalized_uids(message_uids.iter().cloned())
        .into_iter()
        .map(|uid| {
            CString::new(uid)
                .map_err(|_| "EDS message UID contains an invalid NUL byte".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if message_uids.is_empty() {
        return Ok(Vec::new());
    }
    let message_uid_ptrs = message_uids
        .iter()
        .map(|uid| uid.as_ptr())
        .collect::<Vec<*const c_char>>();
    let cancellation_user_data = cancelled as *const AtomicBool as *mut c_void;
    let mut records = std::ptr::null_mut();
    let mut record_count = 0usize;
    let mut error_message: *mut c_char = std::ptr::null_mut();
    let _native_lock = CAMEL_NATIVE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let success = unsafe {
        ffi::evolution_eds_camel_load_messages_by_uid(
            source_uid.as_ptr(),
            folder_name.as_ptr(),
            message_uid_ptrs.as_ptr(),
            message_uid_ptrs.len(),
            &mut records,
            &mut record_count,
            &mut error_message,
            Some(cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback),
            cancellation_user_data,
        ) != 0
    };
    let warning = (!error_message.is_null()).then(|| unsafe {
        let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
        ffi::evolution_eds_camel_free_error(error_message);
        message
    });

    if !success {
        if !records.is_null() {
            unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
        }
        return Err(warning
            .unwrap_or_else(|| "Inkrementelles EDS/Camel-Laden ist fehlgeschlagen".to_string()));
    }

    let messages = parse_native_messages(records, record_count, Some(cancelled));
    if !records.is_null() {
        unsafe { ffi::evolution_eds_camel_free_messages(records, record_count) };
    }
    let messages = messages?;
    cancellation_error(cancelled, "Inkrementelles Camel-Laden")?;
    Ok(messages)
}

pub(crate) fn search_folder_sync(
    source_uid: &str,
    folder_name: &str,
    query_sexpr: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<String>, String> {
    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char, c_void};
        unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
            if user_data.is_null() {
                return 0;
            }
            let cancelled = unsafe { &*(user_data as *const AtomicBool) };
            cancelled.load(Ordering::Relaxed) as i32
        }

        let (resolved_source_uid, resolved_folder_name) =
            resolve_shared_mailbox_target(source_uid, folder_name);
        let source_uid_c = CString::new(resolved_source_uid)
            .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
        let folder_name_c = CString::new(resolved_folder_name)
            .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
        let query_sexpr_c = CString::new(query_sexpr)
            .map_err(|_| "Camel search query contains an invalid NUL byte".to_string())?;

        let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
            cancelled as *const AtomicBool as *mut c_void
        });
        let cancellation_callback =
            cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);

        let mut out_uids: *mut *mut c_char = std::ptr::null_mut();
        let mut out_count: usize = 0;
        let mut error_message: *mut c_char = std::ptr::null_mut();

        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        let success = unsafe {
            ffi::evolution_eds_camel_search_folder(
                source_uid_c.as_ptr(),
                folder_name_c.as_ptr(),
                query_sexpr_c.as_ptr(),
                &mut out_uids,
                &mut out_count,
                &mut error_message,
                cancellation_callback,
                cancellation_user_data,
            ) != 0
        };

        let warning = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });

        if !success {
            return Err(warning.unwrap_or_else(|| "Camel-Ordnersuche fehlgeschlagen".to_string()));
        }

        let mut uids = Vec::with_capacity(out_count);
        if !out_uids.is_null() {
            for i in 0..out_count {
                let uid_ptr = unsafe { *out_uids.add(i) };
                if !uid_ptr.is_null() {
                    let uid = unsafe { CStr::from_ptr(uid_ptr).to_string_lossy().into_owned() };
                    uids.push(uid);
                }
            }
            unsafe { ffi::evolution_eds_camel_free_uids(out_uids, out_count) };
        }

        Ok(uids)
    }
    #[cfg(not(eds_camel_bridge))]
    {
        let _ = (source_uid, folder_name, query_sexpr, cancelled);
        Err("EDS/Camel bridge is not available on this platform".to_string())
    }
}

#[cfg(any(eds_camel_bridge, test))]
fn send_watcher_event(
    sender: &async_channel::Sender<CamelSyncEvent>,
    cancelled: &AtomicBool,
    event: CamelSyncEvent,
) -> bool {
    let mut pending = event;
    let mut overflowed = false;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return false;
        }
        match sender.try_send(pending) {
            Ok(()) => return true,
            Err(async_channel::TrySendError::Closed(_)) => return false,
            Err(async_channel::TrySendError::Full(event)) => {
                if !overflowed {
                    pending = CamelSyncEvent::ResyncRequired(
                        "Camel-Änderungswarteschlange war ausgelastet".to_string(),
                    );
                    overflowed = true;
                } else {
                    pending = event;
                }
                std::thread::park_timeout(std::time::Duration::from_millis(10));
            }
        }
    }
}

pub(crate) fn start_change_monitor()
-> Option<(Arc<AtomicBool>, async_channel::Receiver<CamelSyncEvent>)> {
    #[cfg(eds_camel_bridge)]
    {
        let (sender, receiver) = async_channel::bounded(32);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();

        std::thread::spawn(move || {
            use std::ffi::{CStr, c_char, c_void};

            let mut watcher: *mut c_void = std::ptr::null_mut();
            let mut configured_sources = 0usize;
            let mut successful_sources = 0usize;
            let mut failed_sources = 0usize;
            let mut error_message: *mut c_char = std::ptr::null_mut();
            let result = unsafe {
                ffi::evolution_eds_camel_watcher_new(
                    std::ptr::null(),
                    &mut watcher,
                    &mut configured_sources,
                    &mut successful_sources,
                    &mut failed_sources,
                    &mut error_message,
                )
            };

            let warning = (!error_message.is_null()).then(|| unsafe {
                let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
                ffi::evolution_eds_camel_free_error(error_message);
                message
            });

            if result == 0 || watcher.is_null() {
                let _ = send_watcher_event(
                    &sender,
                    &cancellation,
                    CamelSyncEvent::Error(warning.unwrap_or_else(|| {
                        "EDS/Camel-Signalmonitor konnte nicht gestartet werden".to_string()
                    })),
                );
                return;
            }

            if !send_watcher_event(
                &sender,
                &cancellation,
                CamelSyncEvent::Ready(CamelWatcherStatus {
                    configured_sources,
                    successful_sources,
                    failed_sources,
                    warning,
                }),
            ) {
                unsafe { ffi::evolution_eds_camel_watcher_free(watcher) };
                return;
            }

            let mut normalizer = CamelChangeNormalizer::default();

            while !cancellation.load(Ordering::Relaxed) {
                let mut raw_event = ffi::EvolutionEdsCamelChangeEvent {
                    kind: 0,
                    source_uid: std::ptr::null_mut(),
                    folder_name: std::ptr::null_mut(),
                    old_folder_name: std::ptr::null_mut(),
                    source_display_name: std::ptr::null_mut(),
                    added_count: 0,
                    removed_count: 0,
                    changed_count: 0,
                    recent_count: 0,
                    added_uids: std::ptr::null_mut(),
                    added_uid_count: 0,
                    removed_uids: std::ptr::null_mut(),
                    removed_uid_count: 0,
                    changed_uids: std::ptr::null_mut(),
                    changed_uid_count: 0,
                    recent_uids: std::ptr::null_mut(),
                    recent_uid_count: 0,
                };
                let next = unsafe {
                    ffi::evolution_eds_camel_watcher_next_event(watcher, 1000, &mut raw_event)
                };
                if next < 0 {
                    let _ = send_watcher_event(
                        &sender,
                        &cancellation,
                        CamelSyncEvent::Error(
                            "EDS/Camel-Signalmonitor wurde unerwartet beendet".to_string(),
                        ),
                    );
                    break;
                }
                if next == 0 {
                    continue;
                }

                let Some(kind) = (match raw_event.kind {
                    1 => Some(CamelChangeKind::Changed),
                    2 => Some(CamelChangeKind::FolderDeleted),
                    3 => Some(CamelChangeKind::FolderRenamed),
                    4 => Some(CamelChangeKind::SourceAdded),
                    5 => Some(CamelChangeKind::SourceRemoved),
                    6 => Some(CamelChangeKind::SourceEnabled),
                    7 => Some(CamelChangeKind::SourceDisabled),
                    8 => Some(CamelChangeKind::SourceChanged),
                    _ => None,
                }) else {
                    let unknown_kind = raw_event.kind;
                    unsafe { ffi::evolution_eds_camel_free_change_event(&mut raw_event) };
                    if !send_watcher_event(
                        &sender,
                        &cancellation,
                        CamelSyncEvent::ResyncRequired(format!(
                            "Unbekanntes Camel-Änderungsereignis {}",
                            unknown_kind
                        )),
                    ) {
                        break;
                    }
                    continue;
                };
                let source_display_name = if !raw_event.source_display_name.is_null() {
                    let s = c_string(raw_event.source_display_name);
                    (!s.is_empty()).then_some(s)
                } else {
                    None
                };
                let event = CamelChangeEvent {
                    kind,
                    source_uid: c_string(raw_event.source_uid),
                    source_display_name,
                    folder_name: c_string(raw_event.folder_name),
                    old_folder_name: (!raw_event.old_folder_name.is_null())
                        .then(|| c_string(raw_event.old_folder_name)),
                    added_count: raw_event.added_count,
                    removed_count: raw_event.removed_count,
                    changed_count: raw_event.changed_count,
                    recent_count: raw_event.recent_count,
                    added_uids: c_strings(raw_event.added_uids, raw_event.added_uid_count),
                    removed_uids: c_strings(raw_event.removed_uids, raw_event.removed_uid_count),
                    changed_uids: c_strings(raw_event.changed_uids, raw_event.changed_uid_count),
                    recent_uids: c_strings(raw_event.recent_uids, raw_event.recent_uid_count),
                };
                unsafe { ffi::evolution_eds_camel_free_change_event(&mut raw_event) };
                let event = match normalizer.normalize(event) {
                    Ok(event) => CamelSyncEvent::Change(event),
                    Err(reason) => CamelSyncEvent::ResyncRequired(reason),
                };
                if !send_watcher_event(&sender, &cancellation, event) {
                    break;
                }
            }

            unsafe { ffi::evolution_eds_camel_watcher_free(watcher) };
        });

        Some((cancelled, receiver))
    }

    #[cfg(not(eds_camel_bridge))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change_event(kind: CamelChangeKind, folder_name: &str) -> CamelChangeEvent {
        CamelChangeEvent {
            kind,
            source_uid: " account-a ".to_string(),
            source_display_name: None,
            folder_name: folder_name.to_string(),
            old_folder_name: None,
            added_count: 99,
            removed_count: 99,
            changed_count: 99,
            recent_count: 99,
            added_uids: Vec::new(),
            removed_uids: Vec::new(),
            changed_uids: Vec::new(),
            recent_uids: Vec::new(),
        }
    }

    fn parsed_mail(id: &str, message_uid: &str) -> ParsedMail {
        ParsedMail {
            id: id.to_string(),
            folder_id: "inbox".to_string(),
            folder_name: "INBOX".to_string(),
            backend_location: Some(MailBackendLocation {
                source_uid: "account-a".to_string(),
                folder_name: "INBOX".to_string(),
                message_uid: message_uid.to_string(),
                message_id: None,
            }),
            sender_name: String::new(),
            sender_email: String::new(),
            recipient_to: String::new(),
            recipient_cc: String::new(),
            recipient_bcc: String::new(),
            reply_to: String::new(),
            subject: String::new(),
            message_id: None,
            in_reply_to: Vec::new(),
            references: Vec::new(),
            thread_id: id.to_string(),
            thread_size: 1,
            thread_position: 0,
            date_str: String::new(),
            message_size: 0,
            preview_snippet: String::new(),
            html_body: String::new(),
            raw_headers: None,
            raw_mime: None,
            is_unread: false,
            is_flagged: false,
            is_important: false,
            is_replied: false,
            is_forwarded: false,
            follow_up_state: MailFollowUpState::None,
            follow_up: MailFollowUpMetadata::default(),
            labels: Vec::new(),
            attachments: Vec::new(),
            is_preview_only: false,
        }
    }

    fn subscription_folder(
        folder_name: &str,
        parent_folder_name: Option<&str>,
        is_subscribed: bool,
    ) -> CamelSubscriptionFolder {
        CamelSubscriptionFolder {
            source_uid: "account-a".to_string(),
            source_display_name: "Account A".to_string(),
            folder_name: folder_name.to_string(),
            parent_folder_name: parent_folder_name.map(str::to_string),
            display_name: folder_name
                .rsplit('/')
                .next()
                .unwrap_or(folder_name)
                .to_string(),
            unread_count: 2,
            total_count: 5,
            is_selectable: true,
            is_subscribed,
        }
    }

    #[test]
    fn subscription_snapshot_keeps_exact_source_hierarchy_and_state() {
        let snapshot = normalize_subscription_snapshot(
            " account-a ",
            true,
            vec![
                subscription_folder("Projects", None, false),
                subscription_folder("Projects/2026", Some("Projects"), true),
            ],
        )
        .expect("authoritative subscription rows should normalize");

        assert_eq!(snapshot.source_uid, "account-a");
        assert!(snapshot.is_subscribable);
        assert_eq!(snapshot.folders[1].folder_name, "Projects/2026");
        assert_eq!(
            snapshot.folders[1].parent_folder_name.as_deref(),
            Some("Projects")
        );
        assert!(snapshot.folders[1].is_subscribed);
    }

    #[test]
    fn subscription_snapshot_fails_closed_for_invalid_capability_or_identity() {
        assert!(
            normalize_subscription_snapshot(
                "account-a",
                false,
                vec![subscription_folder("Projects", None, false)],
            )
            .is_err()
        );
        assert!(
            normalize_subscription_snapshot(
                "account-a",
                true,
                vec![
                    subscription_folder("Projects", None, false),
                    subscription_folder("Projects", None, true),
                ],
            )
            .is_err()
        );
        assert!(
            normalize_subscription_snapshot(
                "account-a",
                true,
                vec![subscription_folder("Projects", Some("Projects"), false)],
            )
            .is_err()
        );
    }

    #[test]
    fn subscription_ffi_contract_names_exact_native_snapshot_and_confirmation_calls() {
        let header = include_str!("../../native/eds_camel_bridge.h");
        let native = include_str!("../../native/eds_camel_bridge.c");
        let rust = include_str!("camel_mail_service.rs");

        for field in ["parent_folder_name", "is_selectable", "is_subscribed"] {
            assert!(header.contains(field), "header misses {field}");
        }
        for symbol in [
            "evolution_eds_camel_load_subscription_folders",
            "evolution_eds_camel_set_folder_subscription",
            "evolution_eds_camel_free_subscription_folders",
        ] {
            assert!(header.contains(symbol), "header misses {symbol}");
            assert!(rust.contains(symbol), "Rust FFI misses {symbol}");
        }
        assert!(native.contains("CAMEL_STORE_FOLDER_INFO_SUBSCRIPTION_LIST"));
        assert!(native.contains("CAMEL_IS_SUBSCRIBABLE (store)"));
        assert!(native.contains("camel_subscribable_subscribe_folder_sync"));
        assert!(native.contains("camel_subscribable_unsubscribe_folder_sync"));
        assert!(native.contains("hat die angeforderte Abonnementänderung nicht bestätigt"));
    }

    #[test]
    fn folder_mutation_capabilities_preserve_verified_subset_and_reject_transfer_claims() {
        let capabilities = normalize_folder_mutation_capabilities(
            " account-a ",
            " Projects ",
            true,
            true,
            false,
            true,
            false,
            false,
        )
        .expect("direct Camel capabilities should normalize");
        assert_eq!(capabilities.source_uid, "account-a");
        assert_eq!(capabilities.folder_name, "Projects");
        assert!(capabilities.can_create_child);
        assert!(capabilities.can_rename);
        assert!(!capabilities.can_delete);
        assert!(capabilities.can_refresh);
        assert!(!capabilities.can_copy);
        assert!(!capabilities.can_move);

        assert!(
            normalize_folder_mutation_capabilities(
                "account-a",
                "Projects",
                false,
                false,
                false,
                false,
                true,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn folder_mutation_kinds_have_stable_native_codes_and_target_rules() {
        assert_eq!(CamelFolderMutationKind::CreateChild.ffi_code(), 1);
        assert_eq!(CamelFolderMutationKind::Rename.ffi_code(), 2);
        assert_eq!(CamelFolderMutationKind::Delete.ffi_code(), 3);
        assert_eq!(CamelFolderMutationKind::Refresh.ffi_code(), 4);
        assert_eq!(CamelFolderMutationKind::Copy.ffi_code(), 5);
        assert_eq!(CamelFolderMutationKind::Move.ffi_code(), 6);
        assert!(CamelFolderMutationKind::CreateChild.requires_target_name());
        assert!(CamelFolderMutationKind::Rename.requires_target_name());
        assert!(!CamelFolderMutationKind::Delete.requires_target_name());
        assert!(!CamelFolderMutationKind::Refresh.requires_target_name());
    }

    #[test]
    fn folder_mutation_ffi_contract_requires_provider_verification_and_no_transfer_fallback() {
        let header = include_str!("../../native/eds_camel_bridge.h");
        let native = include_str!("../../native/eds_camel_bridge.c");
        let rust = include_str!("camel_mail_service.rs");

        for symbol in [
            "evolution_eds_camel_get_folder_mutation_capabilities",
            "evolution_eds_camel_apply_folder_mutation",
            "EVOLUTION_EDS_CAMEL_FOLDER_CREATE_CHILD",
            "EVOLUTION_EDS_CAMEL_FOLDER_REFRESH",
        ] {
            assert!(header.contains(symbol), "header misses {symbol}");
            assert!(rust.contains(symbol), "Rust FFI misses {symbol}");
        }
        assert!(native.contains("CAMEL_STORE_CAN_EDIT_FOLDERS"));
        assert!(native.contains("CAMEL_FOLDER_SYSTEM"));
        assert!(native.contains("camel_store_create_folder_sync"));
        assert!(native.contains("camel_store_rename_folder_sync"));
        assert!(native.contains("camel_store_delete_folder_sync"));
        assert!(native.contains("camel_folder_refresh_info_sync"));
        assert!(native.contains("load_folder_mutation_tree"));
        assert!(native.contains("keine sichere atomare Ordner-Kopier- oder Verschiebeoperation"));
    }

    #[test]
    fn has_more_respects_source_folder_and_page_offset() {
        let load = CamelMailLoad {
            messages: Vec::new(),
            folders: vec![
                CamelFolderSummary {
                    source_uid: "account-a".to_string(),
                    source_display_name: "Account A".to_string(),
                    folder_name: "INBOX".to_string(),
                    folder_id: "inbox".to_string(),
                    display_name: "Inbox".to_string(),
                    unread_count: 4,
                    total_count: 12,
                    is_selectable: true,
                },
                CamelFolderSummary {
                    source_uid: "account-b".to_string(),
                    source_display_name: "Account B".to_string(),
                    folder_name: "INBOX".to_string(),
                    folder_id: "inbox".to_string(),
                    display_name: "Inbox".to_string(),
                    unread_count: 0,
                    total_count: 8,
                    is_selectable: true,
                },
            ],
            configured_sources: 2,
            successful_sources: 2,
            failed_sources: 0,
            warning: None,
            is_graph: false,
            search_query: None,
            graph_next_links: HashMap::new(),
            graph_search_next_cursor: None,
        };

        assert!(load.has_more_for(Some("account-a"), Some("INBOX"), 0, 10));
        assert!(!load.has_more_for(Some("account-b"), Some("INBOX"), 0, 10));
        assert!(!load.has_more_for(Some("account-a"), Some("Sent"), 0, 10));
        assert!(!load.has_more_for(Some("account-a"), Some("INBOX"), 10, 10));
        assert!(load.has_more_for(Some("account-a"), Some("inbox"), 0, 10));

        // When folder total_count is 0 (e.g. uninitialized or IMAP without count)
        let mut load_uncounted = load.clone();
        load_uncounted.folders[0].total_count = 0;
        // With empty messages, no more
        assert!(!load_uncounted.has_more_for(Some("account-a"), Some("INBOX"), 0, 10));
        // With a full batch (10 items >= limit 10), there are more
        load_uncounted.messages = (0..10).map(|i| parsed_mail(&format!("msg-{i}"), &format!("uid-{i}"))).collect();
        assert!(load_uncounted.has_more_for(Some("account-a"), Some("INBOX"), 0, 10));
        // With a partial batch (5 items < limit 10), we reached the end
        load_uncounted.messages = (0..5).map(|i| parsed_mail(&format!("msg-{i}"), &format!("uid-{i}"))).collect();
        // Multi-folder / unified inbox scenario:
        assert!(load.has_more_for(None, Some("INBOX"), 0, 10));
        assert!(load.has_more_for(None, None, 0, 10));
        assert!(!load.has_more_for(None, Some("INBOX"), 20, 10));
    }

    #[test]
    fn page_windows_apply_defaults_caps_and_reject_unrepresentable_offsets() {
        assert_eq!(
            PageWindow::normalized(20, 0).unwrap(),
            PageWindow {
                offset: 20,
                limit: DEFAULT_MAIL_PAGE_SIZE,
            }
        );
        assert_eq!(
            PageWindow::normalized(0, 200_000).unwrap().limit,
            MAX_MAIL_PAGE_SIZE
        );
        assert!(PageWindow::normalized(MAX_MAIL_PAGE_OFFSET + 1, 10).is_err());
    }

    #[test]
    fn incomplete_graph_search_is_exposed_as_a_clear_warning() {
        assert!(graph_search_warning(true).is_none());
        let warning = graph_search_warning(false).expect("incomplete search needs a warning");
        assert!(warning.contains("unvollständig"));
        assert!(warning.contains("Ergebnislimit"));
    }

    #[test]
    fn cached_mail_loads_default_search_cursor_to_none() {
        let cached = r#"{
            "messages": [],
            "folders": [],
            "configured_sources": 1,
            "successful_sources": 1,
            "failed_sources": 0,
            "warning": null,
            "is_graph": true,
            "search_query": null,
            "graph_next_links": {}
        }"#;
        let load: CamelMailLoad = serde_json::from_str(cached).unwrap();
        assert_eq!(load.graph_search_next_cursor, None);
    }

    #[test]
    fn parse_folder_tree_content_constructs_hierarchy_and_counts() {
        let sample = r#"
[##storepriv]
Version=3

[folder-root-id]
DisplayName=Archiv
Total=147
UnRead=0
ParentFolderId=account-root

[folder-inbox-id]
DisplayName=Posteingang
Total=95
UnRead=5
ParentFolderId=account-root

[folder-sync-id]
DisplayName=Synchronisierungsprobleme
Total=0
UnRead=0
ParentFolderId=account-root

[folder-conflict-id]
DisplayName=Konflikte
Total=1
UnRead=0
ParentFolderId=folder-sync-id
"#;
        let folders = parse_folder_tree_content(sample, "ews-account-uid", "EDV EWS");
        assert_eq!(folders.len(), 4);
        let inbox = folders
            .iter()
            .find(|f| f.display_name == "Posteingang")
            .unwrap();
        assert_eq!(inbox.folder_name, "Posteingang");
        assert_eq!(inbox.folder_id, "inbox");
        assert_eq!(inbox.unread_count, 5);
        assert_eq!(inbox.total_count, 95);

        let conflict = folders
            .iter()
            .find(|f| f.display_name == "Konflikte")
            .unwrap();
        assert_eq!(conflict.folder_name, "Synchronisierungsprobleme/Konflikte");
        assert_eq!(conflict.total_count, 1);
    }

    #[test]
    fn graph_search_adapter_keeps_search_cursor_outside_folder_cursors() {
        let source = include_str!("camel_mail_service.rs");
        let search_adapter = source
            .split_once("pub(crate) fn search_messages_graph_page")
            .and_then(|(_, source)| source.split_once("fn graph_search_warning"))
            .map(|(source, _)| source)
            .expect("cursor-aware Graph search adapter should be present");

        assert!(search_adapter.contains("search_messages_cursor_sync"));
        assert!(search_adapter.contains("graph_search_next_cursor: search_result.next_cursor"));
        assert!(search_adapter.contains("graph_next_links: HashMap::new()"));
    }

    #[test]
    fn cancellation_wins_before_a_late_backend_result_is_applied() {
        let cancelled = AtomicBool::new(false);
        assert!(cancellation_error(&cancelled, "Testladen").is_ok());
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            cancellation_error(&cancelled, "Testladen").unwrap_err(),
            "Testladen wurde abgebrochen"
        );
    }

    #[test]
    fn watcher_backpressure_coalesces_to_resync_without_ignoring_cancellation() {
        let (sender, receiver) = async_channel::bounded(1);
        sender
            .try_send(CamelSyncEvent::Error("first".to_string()))
            .unwrap();
        let receiver_thread = std::thread::spawn(move || {
            std::thread::park_timeout(std::time::Duration::from_millis(25));
            let first = receiver.recv_blocking().unwrap();
            let second = receiver.recv_blocking().unwrap();
            (first, second)
        });
        let cancelled = AtomicBool::new(false);
        assert!(send_watcher_event(
            &sender,
            &cancelled,
            CamelSyncEvent::Error("second".to_string()),
        ));
        let (first, second) = receiver_thread.join().unwrap();
        assert!(matches!(first, CamelSyncEvent::Error(message) if message == "first"));
        assert!(matches!(second, CamelSyncEvent::ResyncRequired(_)));

        cancelled.store(true, Ordering::Relaxed);
        assert!(!send_watcher_event(
            &sender,
            &cancelled,
            CamelSyncEvent::Error("ignored".to_string()),
        ));
    }

    #[test]
    fn uid_and_message_deduplication_is_stable_and_folder_scoped() {
        assert_eq!(
            normalized_uids(vec![
                " one ".to_string(),
                String::new(),
                "two".to_string(),
                "one".to_string(),
            ]),
            vec!["one", "two"]
        );

        let mut messages = vec![
            parsed_mail("first", "uid-1"),
            parsed_mail("duplicate", "uid-1"),
            parsed_mail("second", "uid-2"),
        ];
        deduplicate_messages(&mut messages).unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );

        let mut invalid = vec![parsed_mail("invalid", "")];
        assert!(deduplicate_messages(&mut invalid).is_err());
    }

    #[test]
    fn change_normalizer_deduplicates_uids_and_tracks_folder_renames() {
        let mut normalizer = CamelChangeNormalizer::default();
        let mut rename = change_event(CamelChangeKind::FolderRenamed, "Archive/2026");
        rename.old_folder_name = Some("Archive/Old".to_string());
        let rename = normalizer.normalize(rename).unwrap();
        assert_eq!(rename.old_folder_name.as_deref(), Some("Archive/Old"));

        let mut changed = change_event(CamelChangeKind::Changed, "Archive/Old");
        changed.added_uids = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        changed.changed_uids = vec!["a".to_string(), "c".to_string()];
        changed.removed_uids = vec![" b ".to_string(), "b".to_string()];
        changed.recent_uids = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        let changed = normalizer.normalize(changed).unwrap();

        assert_eq!(changed.folder_name, "Archive/2026");
        assert_eq!(changed.added_uids, vec!["a"]);
        assert_eq!(changed.changed_uids, vec!["c"]);
        assert_eq!(changed.removed_uids, vec!["b"]);
        assert_eq!(changed.recent_uids, vec!["a"]);
        assert_eq!(changed.added_count, 1);
        assert_eq!(changed.changed_count, 1);
        assert_eq!(changed.removed_count, 1);
        assert_eq!(changed.recent_count, 1);

        let descendant = normalizer
            .normalize(change_event(
                CamelChangeKind::Changed,
                "Archive/Old/Receipts",
            ))
            .unwrap();
        assert_eq!(descendant.folder_name, "Archive/2026/Receipts");

        let mut second_rename = change_event(CamelChangeKind::FolderRenamed, "Archive/Final");
        second_rename.old_folder_name = Some("Archive/2026".to_string());
        normalizer.normalize(second_rename).unwrap();
        let original_alias = normalizer
            .normalize(change_event(
                CamelChangeKind::Changed,
                "Archive/Old/Receipts",
            ))
            .unwrap();
        assert_eq!(original_alias.folder_name, "Archive/Final/Receipts");

        let mut reused_name = change_event(CamelChangeKind::FolderRenamed, "Archive/Old");
        reused_name.old_folder_name = Some("Archive/Final".to_string());
        normalizer.normalize(reused_name).unwrap();
        let reused_name_event = normalizer
            .normalize(change_event(
                CamelChangeKind::Changed,
                "Archive/Old/Receipts",
            ))
            .unwrap();
        assert_eq!(reused_name_event.folder_name, "Archive/Old/Receipts");
    }

    #[test]
    fn native_sync_phases_are_represented_without_accepting_unknown_values() {
        assert_eq!(
            CamelSyncPhase::from_native(1),
            Some(CamelSyncPhase::Discovering)
        );
        assert_eq!(
            CamelSyncPhase::from_native(8),
            Some(CamelSyncPhase::Retrying)
        );
        assert_eq!(CamelSyncPhase::from_native(99), None);
    }

    #[test]
    fn native_follow_up_tags_map_to_the_shared_three_state_model() {
        assert_eq!(
            follow_up_state_from_native(0, false, false),
            MailFollowUpState::None
        );
        assert_eq!(
            follow_up_state_from_native(1, true, false),
            MailFollowUpState::FollowUp
        );
        assert_eq!(
            follow_up_state_from_native(2, true, true),
            MailFollowUpState::Completed
        );

        // A malformed native status must not manufacture a completed state
        // without the exact Evolution tags that define it.
        assert_eq!(
            follow_up_state_from_native(99, false, true),
            MailFollowUpState::None
        );
        assert_eq!(
            follow_up_state_from_native(2, false, false),
            MailFollowUpState::None
        );
        assert_eq!(
            follow_up_state_from_native(99, true, false),
            MailFollowUpState::FollowUp
        );
        assert_eq!(MailFollowUpState::None.next(), MailFollowUpState::FollowUp);
        assert_eq!(
            MailFollowUpState::FollowUp.next(),
            MailFollowUpState::Completed
        );
        assert_eq!(MailFollowUpState::Completed.next(), MailFollowUpState::None);
    }

    #[test]
    fn native_follow_up_metadata_keeps_the_authoritative_tag_values() {
        let metadata = follow_up_metadata_from_native(
            2,
            Some("Reply to Alice".to_string()),
            Some("Mon, 24 Aug 2026 09:30:00 +0200".to_string()),
            Some("Mon, 24 Aug 2026 10:00:00 +0200".to_string()),
        );
        assert_eq!(metadata.state, MailFollowUpState::Completed);
        assert_eq!(metadata.text.as_deref(), Some("Reply to Alice"));
        assert_eq!(
            metadata.due_by.as_deref(),
            Some("Mon, 24 Aug 2026 09:30:00 +0200")
        );
        assert_eq!(
            metadata.completed_on.as_deref(),
            Some("Mon, 24 Aug 2026 10:00:00 +0200")
        );
    }

    #[test]
    fn graph_follow_up_rejects_due_by_instead_of_dropping_it() {
        assert!(graph_follow_up_due_is_supported(None));
        assert!(graph_follow_up_due_is_supported(Some("")));
        assert!(!graph_follow_up_due_is_supported(Some(
            "Mon, 24 Aug 2026 09:30:00 +0200"
        )));
    }

    #[test]
    fn camel_folder_ids_fall_back_to_provider_folder_names() {
        assert_eq!(normalized_camel_folder_id("all", "INBOX"), "inbox");
        assert_eq!(
            normalized_camel_folder_id("all", "[Gmail]/Sent Mail"),
            "sent"
        );
        assert_eq!(normalized_camel_folder_id("all", "Posteingang"), "inbox");
        assert_eq!(normalized_camel_folder_id("junk", "Spam"), "junk");
        assert_eq!(normalized_camel_folder_id("all", "Projects/2026"), "all");
    }

    #[test]
    fn test_shared_mailbox_prefix_stripping() {
        let email = "shared@example.com";
        let dname = "Shared Team";

        // Container matches
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — shared@example.com", email, dname),
            Some(("", true))
        );
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — Shared Team", email, dname),
            Some(("", true))
        );

        // Subfolders under email
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — shared@example.com/Posteingang", email, dname),
            Some(("Posteingang", false))
        );
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — shared@example.com/Posteingang/subfolder", email, dname),
            Some(("Posteingang/subfolder", false))
        );
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — shared@example.com/Gesendete Elemente", email, dname),
            Some(("Gesendete Elemente", false))
        );

        // Subfolders under display name
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — Shared Team/Posteingang", email, dname),
            Some(("Posteingang", false))
        );

        // English Mailbox variants
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Mailbox - shared@example.com/Inbox", email, dname),
            Some(("Inbox", false))
        );

        // Hyphen instead of em-dash
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach - shared@example.com/Posteingang", email, dname),
            Some(("Posteingang", false))
        );

        // Unrelated folders
        assert_eq!(
            strip_shared_mailbox_prefix("Posteingang", email, dname),
            None
        );
        assert_eq!(
            strip_shared_mailbox_prefix("Foreign Folders/Postfach — other@example.com/Posteingang", email, dname),
            None
        );
    }
}

#[allow(dead_code)]
pub(crate) enum CamelMailOperation {
    SetMessageState {
        location: MailBackendLocation,
        is_unread: bool,
        is_flagged: bool,
    },
    SetFollowUp {
        location: MailBackendLocation,
        status: MailFollowUpState,
        /// `Some("")` clears `due-by`; `None` is reserved for legacy callers
        /// which intentionally preserve it.
        due_by: Option<String>,
    },
    Delete {
        location: MailBackendLocation,
    },
    Move {
        location: MailBackendLocation,
        destination: String,
    },
    Copy {
        location: MailBackendLocation,
        destination: String,
    },
    RemoveCopy {
        location: MailBackendLocation,
    },
    Archive {
        location: MailBackendLocation,
    },
    SetJunk {
        location: MailBackendLocation,
        is_junk: bool,
    },
    SetImportant {
        location: MailBackendLocation,
        is_important: bool,
    },
    SetLabel {
        location: MailBackendLocation,
        label: String,
        labeled: bool,
    },
    RestoreDeleted {
        location: MailBackendLocation,
    },
    RestoreArchived {
        location: MailBackendLocation,
    },
}

pub(crate) fn apply_operation(operation: CamelMailOperation) -> Result<(), String> {
    apply_operation_with_cancellation(operation, None)
}

fn apply_operation_with_cancellation(
    operation: CamelMailOperation,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    if let Some(cancelled) = cancelled {
        cancellation_error(cancelled, "EDS/Camel-Änderung")?;
    }
    if let Some(result) = apply_graph_operation(&operation) {
        return result;
    }
    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char, c_void};

        unsafe extern "C" fn cancellation_callback(user_data: *mut c_void) -> i32 {
            if user_data.is_null() {
                return 0;
            }
            let cancelled = unsafe { &*(user_data as *const AtomicBool) };
            cancelled.load(Ordering::Relaxed) as i32
        }

        let (
            kind,
            location,
            destination,
            is_unread,
            is_flagged,
            is_junk,
            is_important,
            follow_up_status,
            due_by,
        ) = match operation {
            CamelMailOperation::SetMessageState {
                location,
                is_unread,
                is_flagged,
            } => (
                1, location, None, is_unread, is_flagged, false, false, 0, None,
            ),
            CamelMailOperation::SetFollowUp {
                location,
                status,
                due_by,
            } => (
                9,
                location,
                None,
                false,
                false,
                false,
                false,
                status as i32,
                due_by,
            ),
            CamelMailOperation::Delete { location } => {
                (2, location, None, false, false, false, false, 0, None)
            }
            CamelMailOperation::Move {
                location,
                destination,
            } => (
                3,
                location,
                Some(destination),
                false,
                false,
                false,
                false,
                0,
                None,
            ),
            CamelMailOperation::Copy { .. } | CamelMailOperation::RemoveCopy { .. } => {
                return Err(
                    "EDS/Camel-Kopieren ist in diesem Backend noch nicht implementiert".to_string(),
                );
            }
            CamelMailOperation::Archive { location } => {
                (4, location, None, false, false, false, false, 0, None)
            }
            CamelMailOperation::SetJunk { location, is_junk } => {
                (5, location, None, false, false, is_junk, false, 0, None)
            }
            CamelMailOperation::SetImportant {
                location,
                is_important,
            } => (
                6,
                location,
                None,
                false,
                false,
                false,
                is_important,
                0,
                None,
            ),
            CamelMailOperation::SetLabel { .. } => {
                return Err(
                    "EDS/Camel-Kategorien werden in diesem Backend noch nicht unterstützt"
                        .to_string(),
                );
            }
            CamelMailOperation::RestoreDeleted { location } => {
                (7, location, None, false, false, false, false, 0, None)
            }
            CamelMailOperation::RestoreArchived { location } => {
                (8, location, None, false, false, false, false, 0, None)
            }
        };

        let (resolved_source_uid, resolved_folder_name) =
            resolve_shared_mailbox_target(&location.source_uid, &location.folder_name);
        let resolved_destination = destination.map(|dest| {
            let (_, res_dest) = resolve_shared_mailbox_target(&location.source_uid, &dest);
            res_dest
        });

        let source_uid = CString::new(resolved_source_uid)
            .map_err(|_| "EDS source UID contains an invalid NUL byte".to_string())?;
        let folder_name = CString::new(resolved_folder_name)
            .map_err(|_| "EDS folder name contains an invalid NUL byte".to_string())?;
        let message_uid = CString::new(location.message_uid)
            .map_err(|_| "EDS message UID contains an invalid NUL byte".to_string())?;
        let destination = resolved_destination
            .map(CString::new)
            .transpose()
            .map_err(|_| "EDS destination contains an invalid NUL byte".to_string())?;
        let destination_ptr = destination
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr());
        let message_id = location
            .message_id
            .map(CString::new)
            .transpose()
            .map_err(|_| "RFC-822 Message-ID contains an invalid NUL byte".to_string())?;
        let message_id_ptr = message_id
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr());
        let due_by = due_by
            .map(CString::new)
            .transpose()
            .map_err(|_| "EDS due-by tag contains an invalid NUL byte".to_string())?;
        let due_by_ptr = due_by
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr());
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let ffi_operation = ffi::EvolutionEdsCamelOperation {
            kind,
            source_uid: source_uid.as_ptr(),
            folder_name: folder_name.as_ptr(),
            message_uid: message_uid.as_ptr(),
            message_id: message_id_ptr,
            destination_folder: destination_ptr,
            is_unread: is_unread as i32,
            is_flagged: is_flagged as i32,
            is_junk: is_junk as i32,
            is_important: is_important as i32,
            follow_up_status,
            due_by: due_by_ptr,
        };

        let cancellation_user_data = cancelled.map_or(std::ptr::null_mut(), |cancelled| {
            cancelled as *const AtomicBool as *mut c_void
        });
        let cancellation_callback =
            cancelled.map(|_| cancellation_callback as ffi::EvolutionEdsCamelCancellationCallback);

        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let result = unsafe {
            ffi::evolution_eds_camel_apply_operation(
                &ffi_operation,
                &mut error_message,
                cancellation_callback,
                cancellation_user_data,
            )
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });

        if result == 1 {
            Ok(())
        } else if result == 2 {
            let message =
                error.unwrap_or_else(|| "Der Serverzustand hat sich geändert".to_string());
            Err(
                if message
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("conflict:")
                {
                    message
                } else {
                    format!("conflict: {message}")
                },
            )
        } else {
            Err(error.unwrap_or_else(|| "EDS/Camel operation failed".to_string()))
        }
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::apply_operation(&operation, cancelled)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (operation, cancelled);
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


/// Set Evolution's three-state follow-up status for one folder-scoped Camel
/// message.  The native bridge writes `follow-up`, `completed-on`, and the
/// optional `due-by` user tags and observes `cancelled` while synchronizing.
pub(crate) fn set_follow_up_state(
    location: MailBackendLocation,
    state: MailFollowUpState,
    due_by: Option<String>,
) -> Result<(), String> {
    apply_follow_up_state_with_cancellation_inner(location, state, due_by, None)
}

/// Cancellable variant used by worker-owned mutation flows.  The native
/// operation observes this flag while Camel synchronizes the folder+UID.
pub(crate) fn apply_follow_up_state_with_cancellation(
    location: MailBackendLocation,
    state: MailFollowUpState,
    due_by: Option<String>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    if let Some(service) =
        super::exchange_service::graph_service_for_source_worker(&location.source_uid)
    {
        if !graph_follow_up_due_is_supported(due_by.as_deref()) {
            return Err("Microsoft Graph unterstützt keine Follow-up-Fälligkeit".to_string());
        }
        return service
            .with_cancellation_flag(cancelled)
            .set_message_follow_up_state_sync(&location.message_uid, state)
            .map_err(|error| error.to_string());
    }
    apply_follow_up_state_with_cancellation_inner(location, state, due_by, Some(cancelled.as_ref()))
}

fn graph_follow_up_due_is_supported(due_by: Option<&str>) -> bool {
    due_by.is_none_or(str::is_empty)
}

fn apply_follow_up_state_with_cancellation_inner(
    location: MailBackendLocation,
    state: MailFollowUpState,
    due_by: Option<String>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    apply_operation_with_cancellation(
        CamelMailOperation::SetFollowUp {
            location,
            status: state,
            due_by,
        },
        cancelled,
    )
}

/// Apply an operation that may need to return backend-created identity data.
/// Graph copies are special because the copied message receives a new ID;
/// the command stack uses that ID to make undo delete only the copy.
pub(crate) fn apply_operation_with_result(
    operation: CamelMailOperation,
) -> Result<Option<MailBackendLocation>, String> {
    match operation {
        CamelMailOperation::Copy {
            location,
            destination,
        } => {
            let Some(service) =
                super::exchange_service::graph_service_for_source_worker(&location.source_uid)
            else {
                return Err(
                    "EDS/Camel-Kopieren ist in diesem Backend noch nicht implementiert".to_string(),
                );
            };
            let destination_id = service
                .resolve_mail_folder_id_sync(&destination)
                .map_err(|error| error.to_string())?;
            let copied = service
                .copy_message_to_folder_sync(&location.message_uid, &destination_id)
                .map_err(|error| error.to_string())?;
            Ok(Some(MailBackendLocation {
                source_uid: location.source_uid,
                folder_name: destination,
                message_uid: copied.id,
                message_id: copied.internet_message_id,
            }))
        }
        CamelMailOperation::RemoveCopy { location } => {
            let Some(service) =
                super::exchange_service::graph_service_for_source_worker(&location.source_uid)
            else {
                return Err(
                    "EDS/Camel-Kopieren ist in diesem Backend noch nicht implementiert".to_string(),
                );
            };
            // This location is created by the successful Copy operation above,
            // so permanent deletion cannot target the original message.
            service
                .delete_message_sync(&location.message_uid)
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
        operation => apply_operation(operation).map(|()| None),
    }
}

fn apply_graph_operation(operation: &CamelMailOperation) -> Option<Result<(), String>> {
    use super::exchange_service::graph_service_for_source_worker;
    let location = match operation {
        CamelMailOperation::SetMessageState { location, .. }
        | CamelMailOperation::SetFollowUp { location, .. }
        | CamelMailOperation::Delete { location }
        | CamelMailOperation::Move { location, .. }
        | CamelMailOperation::Copy { location, .. }
        | CamelMailOperation::RemoveCopy { location }
        | CamelMailOperation::Archive { location }
        | CamelMailOperation::SetJunk { location, .. }
        | CamelMailOperation::SetImportant { location, .. }
        | CamelMailOperation::SetLabel { location, .. }
        | CamelMailOperation::RestoreDeleted { location }
        | CamelMailOperation::RestoreArchived { location } => location,
    };
    let service = graph_service_for_source_worker(&location.source_uid)?;
    if matches!(
        operation,
        CamelMailOperation::Copy { .. } | CamelMailOperation::RemoveCopy { .. }
    ) {
        return Some(Err(
            "Graph-Kopieren benötigt den rückgabefähigen Operationspfad".to_string(),
        ));
    }
    let result = match operation {
        CamelMailOperation::SetFollowUp {
            location,
            status,
            due_by,
        } => {
            if !graph_follow_up_due_is_supported(due_by.as_deref()) {
                return Some(Err(
                    "Microsoft Graph unterstützt keine Follow-up-Fälligkeit".to_string(),
                ));
            }
            service.set_message_follow_up_state_sync(&location.message_uid, *status)
        }
        CamelMailOperation::SetMessageState {
            location,
            is_unread,
            is_flagged: _,
        } => service.set_message_read_state_sync(&location.message_uid, !is_unread),
        CamelMailOperation::SetImportant {
            location,
            is_important,
        } => service.set_message_importance_sync(&location.message_uid, *is_important),
        CamelMailOperation::SetLabel {
            location,
            label,
            labeled,
        } => service.set_message_label_sync(&location.message_uid, label, *labeled),
        CamelMailOperation::Move {
            location,
            destination,
        } => service.move_message_to_folder_name_sync(&location.message_uid, destination),
        CamelMailOperation::Copy { .. } | CamelMailOperation::RemoveCopy { .. } => {
            unreachable!("copy operations return before the Graph mutation match")
        }
        CamelMailOperation::Archive { location } => {
            service.move_message_to_folder_name_sync(&location.message_uid, "archive")
        }
        CamelMailOperation::SetJunk { location, is_junk } => {
            service.set_message_junk_sync(&location.message_uid, &location.folder_name, *is_junk)
        }
        CamelMailOperation::Delete { location } => {
            // Evolution's normal delete action moves a message to the
            // Deleted Items folder so undo/restore remains possible.  A
            // permanent Graph DELETE is intentionally not used here.
            service.move_message_to_folder_name_sync(&location.message_uid, "trash")
        }
        CamelMailOperation::RestoreDeleted { location } => {
            service.move_message_to_folder_name_sync(&location.message_uid, &location.folder_name)
        }
        CamelMailOperation::RestoreArchived { location } => {
            service.move_message_to_folder_name_sync(&location.message_uid, &location.folder_name)
        }
    };
    Some(result.map_err(|error| error.to_string()))
}

/// Send one complete RFC822 message through the transport configured for an
/// EDS mail-account source.  The operation is intentionally synchronous here;
/// callers run it on a worker thread so Camel can authenticate and perform
/// network I/O without blocking GTK's main context.
pub(crate) fn send_message(source_uid: &str, raw_message: &[u8]) -> Result<bool, String> {
    send_message_with_cancellation(source_uid, raw_message, None)
}

/// Send one message while binding Graph's cancellation token to the caller's
/// one-shot flag.  The native Camel bridge predates a cancellable send entry
/// point, so it receives a pre-flight cancellation check and remains isolated
/// behind `eds_camel_bridge`; a confirmed native send is never re-queued merely
/// because cancellation was requested while Camel was finishing it.
pub(crate) fn send_message_with_cancellation(
    source_uid: &str,
    raw_message: &[u8],
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<bool, String> {
    let (resolved_source_uid, _) = resolve_shared_mailbox_target(source_uid, "");
    let source_uid = resolved_source_uid.as_str();

    if cancelled
        .as_ref()
        .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
    {
        return Err("Mail-Versand wurde abgebrochen".to_string());
    }

    // Offline sending is an explicit local queue operation.  Read the
    // framework-neutral transport state instead of GSettings: this function
    // is called by the composer worker, where GTK/GObject access is forbidden.
    // It returns an error to the existing composer contract so no caller can
    // mistake a queued message for an EDS/Camel/Graph success or delete its
    // draft.
    let transport = MailTransportService::shared();
    if transport.snapshot().mode == MailTransportMode::Offline {
        let queue_id = format!("offline-{}", Uuid::new_v4());
        transport
            .enqueue_message(&queue_id, source_uid, raw_message)
            .map_err(|error| {
                format!("Offline-Nachricht konnte nicht vorgemerkt werden: {error}")
            })?;
        return Err(
            "Offline-Modus: Nachricht wurde in die lokale Transportwarteschlange gelegt"
                .to_string(),
        );
    }

    if let Some(service) = super::exchange_service::graph_service_for_source_worker(source_uid) {
        let service = cancelled
            .as_ref()
            .map(|cancelled| service.with_cancellation_flag(cancelled.clone()))
            .unwrap_or(service);
        return service
            .send_raw_message_sync(raw_message, false)
            .map_err(|error| error.to_string());
    }
    #[cfg(eds_camel_bridge)]
    {
        use std::ffi::{CStr, CString, c_char};

        let source_uid = CString::new(source_uid)
            .map_err(|_| "EDS source UID enthält ein ungültiges NUL-Byte".to_string())?;
        if raw_message.is_empty() {
            return Err("Die RFC822-Nachricht ist leer".to_string());
        }

        let mut sent_message_saved = 0;
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let _native_lock = CAMEL_NATIVE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let result = unsafe {
            ffi::evolution_eds_camel_send_message(
                source_uid.as_ptr(),
                raw_message.as_ptr(),
                raw_message.len(),
                &mut sent_message_saved,
                &mut error_message,
            )
        };
        let error = (!error_message.is_null()).then(|| unsafe {
            let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
            ffi::evolution_eds_camel_free_error(error_message);
            message
        });

        if result == 1 {
            Ok(sent_message_saved != 0)
        } else {
            Err(error.unwrap_or_else(|| "EDS/Camel-Versand fehlgeschlagen".to_string()))
        }
    }

    #[cfg(not(eds_camel_bridge))]
    {
        #[cfg(target_os = "macos")]
        {
            crate::services::portable_mail_engine::send_message(
                source_uid,
                raw_message,
                cancelled.as_ref(),
            )
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (source_uid, raw_message, cancelled);
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CamelSaveFolder {
    Drafts,
    Sent,
}

pub(crate) fn save_message(
    source_uid: &str,
    raw_message: &[u8],
    folder: CamelSaveFolder,
) -> Result<(), String> {
    if let Some(service) = super::exchange_service::graph_service_for_source_worker(source_uid) {
        let save_as_draft = matches!(folder, CamelSaveFolder::Drafts);
        service
            .send_raw_message_sync(raw_message, save_as_draft)
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        #[cfg(eds_camel_bridge)]
        {
            use std::ffi::{CStr, CString, c_char};

            let source_uid = CString::new(source_uid)
                .map_err(|_| "EDS source UID enthält ein ungültiges NUL-Byte".to_string())?;
            if raw_message.is_empty() {
                return Err("Die RFC822-Nachricht ist leer".to_string());
            }
            let folder = match folder {
                CamelSaveFolder::Drafts => 1,
                CamelSaveFolder::Sent => 2,
            };
            let mut error_message: *mut c_char = std::ptr::null_mut();
            let _native_lock = CAMEL_NATIVE_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let result = unsafe {
                ffi::evolution_eds_camel_save_message(
                    source_uid.as_ptr(),
                    raw_message.as_ptr(),
                    raw_message.len(),
                    folder,
                    &mut error_message,
                )
            };
            let error = (!error_message.is_null()).then(|| unsafe {
                let message = CStr::from_ptr(error_message).to_string_lossy().into_owned();
                ffi::evolution_eds_camel_free_error(error_message);
                message
            });

            if result == 1 {
                Ok(())
            } else {
                Err(error.unwrap_or_else(|| "EDS-Draft-Speicherung fehlgeschlagen".to_string()))
            }
        }

        #[cfg(not(eds_camel_bridge))]
        {
            let _ = (source_uid, raw_message, folder);
            Err("native EDS/Camel adapter is not available in this build".to_string())
        }
    }
}

pub(crate) const fn native_adapter_available() -> bool {
    cfg!(any(eds_camel_bridge, target_os = "macos"))
}

#[cfg(eds_camel_bridge)]
fn c_string(pointer: *const std::ffi::c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }

    unsafe { std::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(eds_camel_bridge)]
fn c_string_option(pointer: *const std::ffi::c_char) -> Option<String> {
    let value = c_string(pointer);
    (!value.is_empty()).then_some(value)
}

#[cfg(eds_camel_bridge)]
fn c_strings(pointer: *mut *mut std::ffi::c_char, count: usize) -> Vec<String> {
    if pointer.is_null() || count == 0 {
        return Vec::new();
    }

    let pointers = unsafe { std::slice::from_raw_parts(pointer, count) };
    pointers.iter().map(|pointer| c_string(*pointer)).collect()
}
