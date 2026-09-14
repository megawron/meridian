// SPDX-License-Identifier: GPL-3.0-or-later
//
// Standalone Portable Mail Engine for macOS & libcamel-free platforms.
// Provides async/worker IMAP & SMTP communication, live folder listing,
// envelope & body fetching using `mail_store_service::parse_rfc822_bytes`,
// and direct RFC 5321 message delivery.

pub mod connection;
pub mod imap_client;
pub mod smtp_client;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::account_config_service::AccountConfigStore;
use crate::services::camel_mail_service::{
    CamelFolderMutationCapabilities, CamelFolderMutationKind, CamelFolderSummary,
    CamelMailLoad, CamelMailOperation, CamelSubscriptionFolder, CamelSubscriptionSnapshot,
    CamelSyncProgress, load_mail_load_cache, save_mail_load_cache,
};
use crate::services::credential_service::CredentialService;
use crate::services::mail_store_service::{MailBackendLocation, ParsedMail};

pub use imap_client::{ImapClient, ImapFolderItem};
pub use smtp_client::{SmtpClient, extract_envelope_addresses};

/// Fetch a page of messages and folder hierarchy from configured IMAP accounts.
pub fn load_messages_page(
    source_uid: Option<&str>,
    folder_name: Option<&str>,
    message_offset: usize,
    max_messages_per_folder: i32,
    cancelled: Option<&AtomicBool>,
    progress_sender: Option<&async_channel::Sender<CamelSyncProgress>>,
) -> Result<CamelMailLoad, String> {
    if let Some(c) = cancelled {
        if c.load(Ordering::Relaxed) {
            return Err("Mail-Ladevorgang wurde abgebrochen.".into());
        }
    }

    let accounts = AccountConfigStore::from_environment().load();
    let target_accounts: Vec<_> = accounts
        .into_iter()
        .filter(|acc| acc.enabled)
        .filter(|acc| {
            if let Some(target_uid) = source_uid {
                acc.id == target_uid || acc.email == target_uid
            } else {
                true
            }
        })
        .collect();

    if target_accounts.is_empty() {
        // Check if cached mail exists
        if let Some(cached) = load_mail_load_cache() {
            return Ok(cached);
        }
        return Ok(CamelMailLoad {
            messages: Vec::new(),
            folders: Vec::new(),
            configured_sources: 0,
            successful_sources: 0,
            failed_sources: 0,
            warning: None,
            is_graph: false,
            search_query: None,
            graph_next_links: HashMap::new(),
            graph_search_next_cursor: None,
        });
    }

    let mut all_messages = Vec::new();
    let mut all_folders = Vec::new();
    let mut successful_sources = 0;
    let mut failed_sources = 0;
    let mut warnings = Vec::new();

    let limit = if max_messages_per_folder <= 0 {
        50
    } else {
        max_messages_per_folder as usize
    };

    for account in &target_accounts {
        if let Some(c) = cancelled {
            if c.load(Ordering::Relaxed) {
                return Err("Mail-Ladevorgang wurde abgebrochen.".into());
            }
        }

        if let Some(sender) = progress_sender {
            let _ = sender.try_send(CamelSyncProgress {
                phase: crate::services::camel_mail_service::CamelSyncPhase::Connecting,
                source_uid: Some(account.id.clone()),
                source_display_name: Some(account.display_name.clone()),
                folder_name: None,
                configured_sources: target_accounts.len(),
                processed_sources: successful_sources + failed_sources,
                successful_sources,
                failed_sources,
                folders_seen: all_folders.len(),
                messages_loaded: all_messages.len(),
                attempt: 1,
                max_attempts: 1,
            });
        }


        let pass = CredentialService::fetch_password(&account.id)
            .ok()
            .flatten()
            .unwrap_or_default();

        let client_res = ImapClient::connect_and_login(
            &account.imap_host,
            account.imap_port,
            &account.tls_mode,
            &account.username,
            &pass,
            cancelled,
        );

        match client_res {
            Ok(mut client) => {
                successful_sources += 1;
                // List folders
                if let Ok(folders) = client.list_folders() {
                    for f in &folders {
                        let summary = f.to_folder_summary(
                            &account.id,
                            &account.display_name,
                            0,
                            0,
                        );
                        all_folders.push(summary);
                    }
                }

                // Fetch messages for target folder
                let target_folder = folder_name.unwrap_or("INBOX");
                if let Ok(messages) = client.fetch_envelopes(
                    target_folder,
                    message_offset,
                    limit,
                    &account.id,
                ) {
                    all_messages.extend(messages);
                }

                let _ = client.logout();
            }
            Err(err) => {
                failed_sources += 1;
                warnings.push(format!("{}: {}", account.display_name, err));
            }
        }
    }

    // Offline Cache Fallback:
    // If all configured accounts failed to connect (e.g. offline, network down) or produced zero
    // data due to connection errors, fall back to previously persisted mailbox cache.
    if (successful_sources == 0 || (all_messages.is_empty() && all_folders.is_empty())) && failed_sources > 0 {
        if let Some(mut cached) = load_mail_load_cache() {
            let offline_notice = format!(
                "Offline-Modus: Server nicht erreichbar ({}); zwischengespeicherte Daten geladen",
                warnings.join("; ")
            );
            cached.warning = Some(match cached.warning {
                Some(prev) => format!("{prev}; {offline_notice}"),
                None => offline_notice,
            });
            cached.configured_sources = target_accounts.len();
            cached.successful_sources = successful_sources;
            cached.failed_sources = failed_sources;
            return Ok(cached);
        }
    }

    let load = CamelMailLoad {
        messages: all_messages,
        folders: all_folders,
        configured_sources: target_accounts.len(),
        successful_sources,
        failed_sources,
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        is_graph: false,
        search_query: None,
        graph_next_links: HashMap::new(),
        graph_search_next_cursor: None,
    };

    save_mail_load_cache(&load);
    Ok(load)
}

/// Load full RFC 822 body for message viewer / source viewing.
pub fn load_raw_message(
    location: &MailBackendLocation,
    cancelled: &Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Ladevorgang wurde abgebrochen".into());
    }

    let accounts = AccountConfigStore::from_environment().load();
    let account = accounts
        .into_iter()
        .find(|acc| acc.id == location.source_uid || acc.email == location.source_uid)
        .ok_or_else(|| format!("Konto {} nicht gefunden", location.source_uid))?;

    let pass = CredentialService::fetch_password(&account.id)
        .ok()
        .flatten()
        .unwrap_or_default();

    let client_res = ImapClient::connect_and_login(
        &account.imap_host,
        account.imap_port,
        &account.tls_mode,
        &account.username,
        &pass,
        Some(cancelled.as_ref()),
    );

    match client_res {
        Ok(mut client) => {
            let raw = client.fetch_raw_message(&location.folder_name, &location.message_uid)?;
            let _ = client.logout();
            Ok(raw)
        }
        Err(net_err) => {
            // Offline fallback: check cached messages in mail_load_cache
            if let Some(cached) = load_mail_load_cache() {
                if let Some(msg) = cached.messages.iter().find(|m| {
                    let src_match = m.backend_location.as_ref().map(|b| {
                        b.source_uid == location.source_uid
                            || b.source_uid == account.email
                    }).unwrap_or(true);
                    let uid_match = m.id == location.message_uid
                        || m.backend_location.as_ref().map(|b| b.message_uid == location.message_uid).unwrap_or(false);
                    src_match && uid_match
                }) {
                    if let Some(ref raw_mime) = msg.raw_mime {
                        return Ok(raw_mime.clone());
                    }
                    if let Some(ref raw_headers) = msg.raw_headers {
                        let mut full = raw_headers.clone();
                        full.extend_from_slice(b"\r\n\r\n");
                        full.extend_from_slice(msg.html_body.as_bytes());
                        return Ok(full);
                    }
                    if !msg.html_body.is_empty() {
                        let pseudo_rfc822 = format!(
                            "From: {}\r\nSubject: {}\r\nDate: {}\r\n\r\n{}",
                            msg.sender_email, msg.subject, msg.date_str, msg.html_body
                        );
                        return Ok(pseudo_rfc822.into_bytes());
                    }
                }
            }
            Err(format!("Offline: Nachricht konnte nicht geladen werden ({net_err})"))
        }
    }
}

/// Send RFC 822 message via SMTP.
pub fn send_message(
    source_uid: &str,
    raw_message: &[u8],
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<bool, String> {
    if raw_message.is_empty() {
        return Err("Die RFC822-Nachricht ist leer".to_string());
    }

    let accounts = AccountConfigStore::from_environment().load();
    let account = accounts
        .into_iter()
        .find(|acc| acc.id == source_uid || acc.email == source_uid)
        .ok_or_else(|| format!("Konto {} nicht gefunden", source_uid))?;

    let (sender, recipients) = extract_envelope_addresses(raw_message);
    let sender = if sender.is_empty() {
        account.email.clone()
    } else {
        sender
    };

    if recipients.is_empty() {
        return Err("Keine Empfänger in der Nachricht angegeben".to_string());
    }

    let cancel_ref = cancelled.map(|c| c.as_ref());
    let mut smtp = SmtpClient::connect(
        &account.smtp_host,
        account.smtp_port,
        &account.tls_mode,
        cancel_ref,
    )?;

    smtp.ehlo("localhost")?;

    if account.tls_mode.eq_ignore_ascii_case("starttls") || account.smtp_port == 587 {
        smtp.starttls(&account.smtp_host, account.smtp_port)?;
        smtp.ehlo("localhost")?;
    }

    let pass = CredentialService::fetch_password(&account.id)
        .ok()
        .flatten()
        .unwrap_or_default();

    if !account.username.is_empty() && !pass.is_empty() {
        if let Err(e) = smtp.auth_plain(&account.username, &pass) {
            smtp.auth_login(&account.username, &pass)
                .map_err(|e2| format!("SMTP-Authentifizierung fehlgeschlagen: {e}; {e2}"))?;
        }
    }

    smtp.mail_from(&sender)?;

    for recipient in &recipients {
        smtp.rcpt_to(recipient)?;
    }

    smtp.data(raw_message)?;
    smtp.quit()?;

    Ok(false)
}

/// Load subscription folders from IMAP.
pub fn load_subscription_folders(
    source_uid: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<CamelSubscriptionSnapshot, String> {
    let accounts = AccountConfigStore::from_environment().load();
    let account = accounts
        .into_iter()
        .find(|acc| acc.id == source_uid || acc.email == source_uid)
        .ok_or_else(|| format!("Konto {source_uid} nicht gefunden"))?;

    let pass = CredentialService::fetch_password(&account.id)
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut client = ImapClient::connect_and_login(
        &account.imap_host,
        account.imap_port,
        &account.tls_mode,
        &account.username,
        &pass,
        cancelled,
    )?;

    let folder_items = client.list_folders()?;
    let _ = client.logout();

    let mut folders = Vec::new();
    for item in folder_items {
        let parent = if let Some(idx) = item.name.rfind(item.delimiter) {
            Some(item.name[..idx].to_string())
        } else {
            None
        };

        folders.push(CamelSubscriptionFolder {
            source_uid: source_uid.to_string(),
            source_display_name: account.display_name.clone(),
            folder_name: item.name.clone(),
            parent_folder_name: parent,
            display_name: item.name.clone(),
            unread_count: 0,
            total_count: 0,
            is_selectable: !item.is_noselect,
            is_subscribed: true,
        });
    }

    Ok(CamelSubscriptionSnapshot {
        source_uid: source_uid.to_string(),
        is_subscribable: true,
        folders,
    })
}

/// Set folder subscription on IMAP.
pub fn set_folder_subscription(
    source_uid: &str,
    folder_name: &str,
    subscribed: bool,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    let accounts = AccountConfigStore::from_environment().load();
    let account = accounts
        .into_iter()
        .find(|acc| acc.id == source_uid || acc.email == source_uid)
        .ok_or_else(|| format!("Konto {source_uid} nicht gefunden"))?;

    let pass = CredentialService::fetch_password(&account.id)
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut client = ImapClient::connect_and_login(
        &account.imap_host,
        account.imap_port,
        &account.tls_mode,
        &account.username,
        &pass,
        cancelled,
    )?;

    client.set_subscription(folder_name, subscribed)?;
    let _ = client.logout();
    Ok(())
}

/// Report folder mutation capabilities for portable engine.
pub fn folder_mutation_capabilities(
    source_uid: &str,
    folder_name: &str,
) -> Result<CamelFolderMutationCapabilities, String> {
    let is_inbox = folder_name.eq_ignore_ascii_case("INBOX");
    Ok(CamelFolderMutationCapabilities {
        source_uid: source_uid.to_string(),
        folder_name: folder_name.to_string(),
        can_create_child: true,
        can_rename: !is_inbox,
        can_delete: !is_inbox,
        can_refresh: true,
        can_copy: true,
        can_move: true,
    })
}

/// Execute folder mutation (create, rename, delete) on IMAP.
pub fn apply_folder_mutation(
    kind: CamelFolderMutationKind,
    source_uid: &str,
    folder_name: &str,
    target_name: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    let accounts = AccountConfigStore::from_environment().load();
    let account = accounts
        .into_iter()
        .find(|acc| acc.id == source_uid || acc.email == source_uid)
        .ok_or_else(|| format!("Konto {source_uid} nicht gefunden"))?;

    let pass = CredentialService::fetch_password(&account.id)
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut client = ImapClient::connect_and_login(
        &account.imap_host,
        account.imap_port,
        &account.tls_mode,
        &account.username,
        &pass,
        cancelled,
    )?;

    match kind {
        CamelFolderMutationKind::CreateChild => {
            let target = target_name.ok_or_else(|| "Neuer Ordnername erforderlich".to_string())?;
            let full_name = format!("{folder_name}/{target}");
            client.create_folder(&full_name)?;
        }
        CamelFolderMutationKind::Rename => {
            let target = target_name.ok_or_else(|| "Neuer Ordnername erforderlich".to_string())?;
            client.rename_folder(folder_name, target)?;
        }
        CamelFolderMutationKind::Delete => {
            client.delete_folder(folder_name)?;
        }
        CamelFolderMutationKind::Refresh => {
            let _ = client.select_folder(folder_name);
        }
        CamelFolderMutationKind::Copy | CamelFolderMutationKind::Move => {
            return Err("Ordnerkopie/-verschiebung wird für IMAP-Hierarchien nicht unterstützt".into());
        }
    }

    let _ = client.logout();
    Ok(())
}

/// Execute message operation (read/unread flags, deletion, moving).
pub fn apply_operation(
    operation: &CamelMailOperation,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    if let Some(c) = cancelled {
        if c.load(Ordering::Relaxed) {
            return Err("Operation wurde abgebrochen".into());
        }
    }

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

    let accounts = AccountConfigStore::from_environment().load();
    let account = accounts
        .into_iter()
        .find(|acc| acc.id == location.source_uid || acc.email == location.source_uid)
        .ok_or_else(|| format!("Konto {} nicht gefunden", location.source_uid))?;

    let pass = CredentialService::fetch_password(&account.id)
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut client = ImapClient::connect_and_login(
        &account.imap_host,
        account.imap_port,
        &account.tls_mode,
        &account.username,
        &pass,
        cancelled,
    )?;

    match operation {
        CamelMailOperation::SetMessageState {
            is_unread,
            is_flagged,
            ..
        } => {
            let mut plus = Vec::new();
            let mut minus = Vec::new();
            if *is_unread {
                minus.push("\\Seen");
            } else {
                plus.push("\\Seen");
            }
            if *is_flagged {
                plus.push("\\Flagged");
            } else {
                minus.push("\\Flagged");
            }
            client.set_flags(&location.folder_name, &location.message_uid, &plus, &minus)?;
        }
        CamelMailOperation::Delete { .. } => {
            client.set_flags(
                &location.folder_name,
                &location.message_uid,
                &["\\Deleted"],
                &[],
            )?;
        }
        CamelMailOperation::Move { destination, .. } => {
            client.copy_message(&location.folder_name, &location.message_uid, destination)?;
            client.set_flags(
                &location.folder_name,
                &location.message_uid,
                &["\\Deleted"],
                &[],
            )?;
        }
        CamelMailOperation::Copy { destination, .. } => {
            client.copy_message(&location.folder_name, &location.message_uid, destination)?;
        }
        _ => {}
    }

    let _ = client.logout();

    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_envelope_addresses() {
        let raw = b"From: Alice <alice@example.com>\r\nTo: Bob <bob@example.com>, carol@example.com\r\nSubject: Test\r\n\r\nBody";
        let (from, to) = extract_envelope_addresses(raw);
        assert_eq!(from, "alice@example.com");
        assert_eq!(to, vec!["bob@example.com", "carol@example.com"]);
    }

    #[test]
    fn test_folder_mutation_capabilities_inbox_protection() {
        let caps_inbox = folder_mutation_capabilities("acc1", "INBOX").unwrap();
        assert!(!caps_inbox.can_delete);
        assert!(!caps_inbox.can_rename);
        assert!(caps_inbox.can_create_child);

        let caps_custom = folder_mutation_capabilities("acc1", "Work").unwrap();
        assert!(caps_custom.can_delete);
        assert!(caps_custom.can_rename);
    }

    #[test]
    fn test_imap_parse_list_line_unquoted_folder() {
        let line = r#"* LIST (\HasNoChildren) "/" INBOX"#;
        let item = ImapFolderItem::parse_list_line(line).expect("Should parse");
        assert_eq!(item.name, "INBOX");
        assert!(item.is_inbox);
    }

    #[test]
    fn test_load_messages_page_offline_cache_fallback() {
        use crate::services::camel_mail_service::{CamelMailLoad, save_mail_load_cache};
        use crate::services::mail_store_service::{parse_rfc822_bytes, MailBackendLocation};
        use crate::services::account_config_service::{AccountConfigStore, AccountProfile};

        let store = AccountConfigStore::from_environment();
        let test_acc = AccountProfile {
            id: "offline-acc-id".to_string(),
            display_name: "Offline Account".to_string(),
            email: "offline-acc-id@example.org".to_string(),
            imap_host: "127.0.0.1".to_string(),
            imap_port: 65432,
            tls_mode: "none".to_string(),
            username: "offline-user".to_string(),
            enabled: true,
            ..Default::default()
        };
        let _ = store.upsert(test_acc);

        // Seed mock cache
        let raw = b"From: sender@example.com\r\nSubject: Offline Subject\r\n\r\nCached offline content";
        let mut cached_msg = parse_rfc822_bytes(
            "offline-cached-msg-1",
            "INBOX",
            raw,
            false,
        ).expect("should parse");
        cached_msg.backend_location = Some(MailBackendLocation {
            source_uid: "offline-acc-id".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "offline-cached-msg-1".to_string(),
            message_id: None,
        });
        cached_msg.raw_mime = Some(raw.to_vec());

        let initial_load = CamelMailLoad {
            messages: vec![cached_msg],
            folders: Vec::new(),
            configured_sources: 1,
            successful_sources: 1,
            failed_sources: 0,
            warning: None,
            is_graph: false,
            search_query: None,
            graph_next_links: HashMap::new(),
            graph_search_next_cursor: None,
        };
        save_mail_load_cache(&initial_load);

        // Now call load_messages_page with the configured unreachable source_uid to simulate offline failure
        let result = load_messages_page(
            Some("offline-acc-id"),
            Some("INBOX"),
            0,
            50,
            None,
            None,
        );
        let _ = store.delete("offline-acc-id");

        assert!(result.is_ok());
        let load = result.unwrap();
        // Cache must have been returned instead of empty list
        assert!(!load.messages.is_empty(), "Must fall back to cached messages when offline");
        assert_eq!(load.messages[0].id, "offline-cached-msg-1");
        assert!(load.warning.is_some(), "Warning must indicate offline fallback");
        assert!(load.warning.unwrap().contains("Offline-Modus"));
    }
}


