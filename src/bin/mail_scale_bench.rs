// SPDX-License-Identifier: GPL-3.0-or-later
//
// High-scale benchmark harness for Meridian's SQLite summary reader,
// RFC-822 / subject threading engine, GObject lazy message model,
// and virtualized Cairo table viewport pipeline across 500k to 20M emails.

#![allow(dead_code, unused_imports)]

pub mod i18n {
    pub fn gettext(s: &str) -> String {
        s.to_string()
    }
}

#[path = "../models"]
pub mod models {
    #[path = "mail_message.rs"]
    pub mod mail_message;
}

pub mod views {
    pub fn parse_flexible_datetime(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        chrono::DateTime::parse_from_rfc2822(value.trim()).ok()
    }
}

#[path = "../services"]
pub mod services {
    #[path = "html_sanitizer.rs"]
    pub mod html_sanitizer;

    #[path = "inline_content_service.rs"]
    pub mod inline_content_service;

    #[path = "mail_metadata_service.rs"]
    pub mod mail_metadata_service;

    #[path = "mail_thread_service.rs"]
    pub mod mail_thread_service;

    #[path = "evolution_summary_cache.rs"]
    pub mod evolution_summary_cache;

    pub mod settings_service {
        pub fn is_speculative_prefetch_enabled() -> bool {
            true
        }
    }

    pub fn app_data_dir() -> std::path::PathBuf {
        std::env::temp_dir().join("meridian-bench-data")
    }

    pub fn app_cache_dir() -> std::path::PathBuf {
        std::env::temp_dir().join("meridian-bench-cache")
    }

    pub mod eds_service {
        #[derive(Clone, Copy, Debug)]
        pub enum EdsSourceKind {
            Mail,
        }

        pub fn is_source_enabled_for_service(_content: &str, _kind: EdsSourceKind) -> bool {
            true
        }
    }

    pub mod account_config_service {
        #[derive(Clone, Debug)]
        pub struct AccountProfile {
            pub id: String,
            pub email: String,
            pub display_name: String,
            pub is_shared_mailbox: bool,
            pub parent_account_id: Option<String>,
        }

        #[derive(Default)]
        pub struct AccountConfigStore;

        impl AccountConfigStore {
            pub fn load(&self) -> Vec<AccountProfile> {
                Vec::new()
            }

            pub fn load_cached(&self) -> Vec<AccountProfile> {
                Vec::new()
            }
        }

        pub fn eds_source_uid_for_email(email: &str) -> String {
            email.to_string()
        }
    }

    pub mod camel_mail_service {
        #[derive(Clone, Debug)]
        pub struct CamelFolderSummary {
            pub folder_id: String,
            pub source_uid: String,
            pub source_display_name: String,
            pub folder_name: String,
            pub display_name: String,
            pub unread_count: u32,
            pub total_count: u32,
            pub is_selectable: bool,
        }

        pub fn remember_folder_names_in_memory<'a, I>(_names: I)
        where
            I: IntoIterator<Item = &'a str>,
        {
        }

        pub fn resolve_shared_mailbox_target(source_uid: &str, folder_name: &str) -> (String, String) {
            (source_uid.to_string(), folder_name.to_string())
        }

        pub fn strip_shared_mailbox_prefix<'a>(
            _real_folder_name: &'a str,
            _email: &str,
            _display_name: &str,
        ) -> Option<(&'a str, bool)> {
            None
        }

        pub fn normalized_camel_folder_id(_source_uid: &str, folder_name: &str) -> String {
            let trimmed = folder_name.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("inbox") || trimmed.eq_ignore_ascii_case("posteingang") {
                "inbox".to_string()
            } else {
                trimmed.to_ascii_lowercase()
            }
        }
    }

    pub mod mail_store_service {
        use serde::{Deserialize, Serialize};
        use crate::models::mail_message::{AttachmentInfo, MailMessageObject};
        use crate::services::mail_metadata_service::{MailFollowUpMetadata, MailFollowUpState};
        use crate::services::mail_thread_service::{ThreadIdentity, group_messages_with_options};

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct MailBackendLocation {
            pub source_uid: String,
            pub folder_name: String,
            pub message_uid: String,
            #[serde(default)]
            pub message_id: Option<String>,
        }

        #[derive(Debug, Clone)]
        pub struct ParsedMail {
            pub id: String,
            pub folder_id: String,
            pub folder_name: String,
            pub backend_location: Option<MailBackendLocation>,
            pub sender_name: String,
            pub sender_email: String,
            pub recipient_to: String,
            pub recipient_cc: String,
            pub recipient_bcc: String,
            pub reply_to: String,
            pub subject: String,
            pub message_id: Option<String>,
            pub in_reply_to: Vec<String>,
            pub references: Vec<String>,
            pub thread_id: String,
            pub thread_size: usize,
            pub thread_position: usize,
            pub date_str: String,
            pub message_size: usize,
            pub preview_snippet: String,
            pub html_body: String,
            pub raw_headers: Option<std::sync::Arc<str>>,
            pub raw_mime: Option<std::sync::Arc<[u8]>>,
            pub is_unread: bool,
            pub is_flagged: bool,
            pub is_important: bool,
            pub is_replied: bool,
            pub is_forwarded: bool,
            pub follow_up_state: MailFollowUpState,
            pub follow_up: MailFollowUpMetadata,
            pub labels: Vec<String>,
            pub attachments: Vec<AttachmentInfo>,
            pub is_preview_only: bool,
        }

        impl ParsedMail {
            pub fn into_object(self) -> MailMessageObject {
                MailMessageObject::from_parsed(self)
            }
        }

        #[inline]
        pub fn decode_mime_header(value: &str) -> String {
            value.trim().to_string()
        }

        pub fn assign_thread_metadata(messages: &mut [ParsedMail]) {
            let inputs = messages
                .iter()
                .map(|message| ThreadIdentity {
                    message_id: message.message_id.as_deref(),
                    in_reply_to: &message.in_reply_to,
                    references: &message.references,
                    subject: &message.subject,
                    sender: if !message.sender_name.trim().is_empty() {
                        &message.sender_name
                    } else {
                        &message.sender_email
                    },
                    date_ts: crate::services::evolution_summary_cache::parse_rfc2822_utc_fast(&message.date_str)
                        .unwrap_or(0),
                })
                .collect::<Vec<_>>();
            let memberships = group_messages_with_options(&inputs, true);
            for (message, membership) in messages.iter_mut().zip(memberships) {
                message.thread_id = membership.thread_id;
                message.thread_size = membership.size;
                message.thread_position = membership.position;
            }
        }

        pub fn parse_rfc822_bytes(
            _id: &str,
            _folder: &str,
            _raw: &[u8],
            _unread: bool,
        ) -> Option<ParsedMail> {
            None
        }
    }
}

use std::path::PathBuf;
use std::time::Instant;

use crate::models::mail_message::MailMessageObject;
use crate::services::evolution_summary_cache::{
    load_folder_messages_page_from_sqlite, load_folder_messages_window_from_sqlite,
    parse_rfc2822_utc_fast, populate_synthetic_camel_folders_db,
    query_folder_message_count_from_sqlite,
};
use crate::services::mail_thread_service::{ThreadIdentity, group_messages_with_options};

fn main() {
    let keep_db = !std::env::args().any(|a| a == "--clean-db");
    let bench_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("target")
        .join("bench_camel_scale");
    let _ = std::fs::create_dir_all(&bench_dir);
    let db_path = bench_dir.join("folders.db");

    let tiers: &[usize] = &[
        500_000,
        1_000_000,
        2_000_000,
        5_000_000,
        10_000_000,
        20_000_000,
    ];

    println!("==========================================================================================================");
    println!(" MERIDIAN MASSIVE MAILBOX BENCHMARK SUITE (500k -> 1M -> 2M -> 5M -> 10M -> 20M EMAILS)");
    println!(" Isolated DB Path: {}", db_path.display());
    println!("==========================================================================================================\n");

    let mut prev_total = if db_path.exists() {
        query_folder_message_count_from_sqlite(&db_path, "bench-account", "INBOX").unwrap_or(0)
    } else {
        0
    };

    for &target_total in tiers {
        println!("----------------------------------------------------------------------------------------------------------");
        println!(
            " TIER: {:>10} EMAILS (existing={}, inserting +{} rows into Camel folders.db)",
            target_total,
            prev_total,
            target_total.saturating_sub(prev_total)
        );
        println!("----------------------------------------------------------------------------------------------------------");

        if prev_total < target_total {
            let stats = populate_synthetic_camel_folders_db(&db_path, "INBOX", prev_total, target_total)
                .expect("Failed to populate synthetic Camel folders.db");
            prev_total = target_total;

            let insert_rate = (stats.inserted_rows as f64) / (stats.insert_elapsed_ms / 1000.0);
            let size_mb = (stats.file_size_bytes as f64) / (1024.0 * 1024.0);
            println!(
                "  [1] DB Populate & Index : insert={:.1} ms ({:.0} rows/s), index={:.1} ms, disk={:.1} MB ({:.2} GB)",
                stats.insert_elapsed_ms,
                insert_rate,
                stats.index_elapsed_ms,
                size_mb,
                size_mb / 1024.0
            );
        } else {
            let size_mb = std::fs::metadata(&db_path).map(|m| m.len() as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);
            println!(
                "  [1] DB Reused From Disk : already contains {} rows, disk={:.1} MB ({:.2} GB)",
                prev_total,
                size_mb,
                size_mb / 1024.0
            );
        }

        // 2. O(1) Folder total count query for virtual scrollbar sizing
        let t_count = Instant::now();
        let total_count = query_folder_message_count_from_sqlite(&db_path, "bench-account", "INBOX")
            .expect("Failed to query folder count");
        let count_us = t_count.elapsed().as_secs_f64() * 1_000_000.0;
        assert!(total_count >= target_total);
        println!(
            "  [2] O(1) Folder Count   : {:.1} µs (total={}, virtual_canvas_height={:.0} px)",
            count_us,
            total_count,
            total_count as f64 * 28.0
        );

        // 3. First-Paint Folder Switch (LIMIT 80 + GObject creation + 35-row viewport materialization)
        let t_first_paint = Instant::now();
        let (first_page, cursor_80) = load_folder_messages_page_from_sqlite(
            &db_path,
            "bench-account",
            "INBOX",
            None,
            80,
        )
        .expect("Failed to load first 80 messages");
        let sql_80_ms = t_first_paint.elapsed().as_secs_f64() * 1000.0;

        let t_gobj = Instant::now();
        let gobjects: Vec<MailMessageObject> = first_page.into_iter().map(|p| p.into_object()).collect();
        let gobj_ms = t_gobj.elapsed().as_secs_f64() * 1000.0;

        // Materialize 35 visible viewport rows (lazy snippet clean + cached timestamp + Pango markup)
        let t_viewport = Instant::now();
        let mut materialized_markups = 0usize;
        for msg in gobjects.iter().take(35) {
            let subj = msg.subject();
            let snip = msg.preview_snippet();
            let ts = msg.timestamp_cached(|raw| parse_rfc2822_utc_fast(raw).unwrap_or(0));
            let markup_dark = format!(
                "<span foreground=\"#ffffff\" weight=\"bold\">{}</span> <span foreground=\"#c2c2c2\">- {}</span>",
                glib::markup_escape_text(&subj),
                glib::markup_escape_text(&snip)
            );
            if ts > 0 && !markup_dark.is_empty() {
                materialized_markups += 1;
            }
        }
        let viewport_us = t_viewport.elapsed().as_secs_f64() * 1_000_000.0;
        let total_first_paint_ms = sql_80_ms + gobj_ms + (viewport_us / 1000.0);
        println!(
            "  [3] First-Paint (80)    : total={:.2} ms (sqlite+thread={:.2} ms, GObject={:.2} ms, 35-row viewport={:.1} µs, rows={})",
            total_first_paint_ms, sql_80_ms, gobj_ms, viewport_us, materialized_markups
        );

        // 4. Keyset Pagination Next Page (LIMIT 80 using cursor)
        let t_next_page = Instant::now();
        let (second_page, _) = load_folder_messages_page_from_sqlite(
            &db_path,
            "bench-account",
            "INBOX",
            cursor_80,
            80,
        )
        .expect("Failed to load keyset next page");
        let next_page_ms = t_next_page.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(second_page.len(), 80);
        println!(
            "  [4] Keyset Next Page    : {:.2} ms (80 rows via O(log N) index cursor seek)",
            next_page_ms
        );

        // 5. Larger Page Load (LIMIT 500)
        let t_500 = Instant::now();
        let (page_500, _) = load_folder_messages_page_from_sqlite(
            &db_path,
            "bench-account",
            "INBOX",
            None,
            500,
        )
        .expect("Failed to load 500 messages");
        let page_500_ms = t_500.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(page_500.len(), 500);
        println!(
            "  [5] Warm Batch (500)    : {:.2} ms (500 rows + RFC-822 thread resolution)",
            page_500_ms
        );

        // 6. Scrollbar Jump to 50% Depth & 99% Depth (Deferred Covering-Index Join)
        let offset_50 = target_total / 2;
        let t_jump_50 = Instant::now();
        let win_50 = load_folder_messages_window_from_sqlite(
            &db_path,
            "bench-account",
            "INBOX",
            offset_50,
            80,
        )
        .expect("Failed to load 50% window");
        let jump_50_ms = t_jump_50.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(win_50.len(), 80);

        let offset_99 = (target_total * 99) / 100;
        let t_jump_99 = Instant::now();
        let win_99 = load_folder_messages_window_from_sqlite(
            &db_path,
            "bench-account",
            "INBOX",
            offset_99,
            80,
        )
        .expect("Failed to load 99% window");
        let jump_99_ms = t_jump_99.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(win_99.len(), 80);
        println!(
            "  [6] Scrollbar Jump      : 50% depth (row {:>10}) = {:>6.2} ms | 99% depth (row {:>10}) = {:>6.2} ms",
            offset_50, jump_50_ms, offset_99, jump_99_ms
        );

        // 7. At 500k and 1M, also measure raw in-memory JWZ/Subject Threading engine throughput
        if target_total <= 1_000_000 {
            let mut synth_identities = Vec::with_capacity(target_total);
            let msg_ids: Vec<String> = (0..target_total)
                .map(|i| format!("186a0-{:x}@camel", i + 1))
                .collect();
            let parent_refs: Vec<Vec<String>> = (0..target_total)
                .map(|i| {
                    if (i % 5) != 0 && i >= (i % 5) {
                        let root_idx = (i - (i % 5)) + 1;
                        vec![format!("186a0-{:x}@camel", root_idx)]
                    } else {
                        Vec::new()
                    }
                })
                .collect();
            for i in 0..target_total {
                synth_identities.push(ThreadIdentity {
                    message_id: Some(msg_ids[i].as_str()),
                    in_reply_to: &parent_refs[i],
                    references: &parent_refs[i],
                    subject: if (i % 5) != 0 {
                        "Re: Architecture review: Zero-copy SQLite cursor pagination"
                    } else {
                        "Architecture review: Zero-copy SQLite cursor pagination"
                    },
                    sender: "Linus Torvalds",
                    date_ts: 1_700_000_000 + (i as i64) * 7,
                });
            }
            let t_thread = Instant::now();
            let grouped = group_messages_with_options(&synth_identities, true);
            let thread_ms = t_thread.elapsed().as_secs_f64() * 1000.0;
            let thread_rate = (target_total as f64) / (thread_ms / 1000.0);
            assert_eq!(grouped.len(), target_total);
            println!(
                "  [7] Full Threading ({:>4}k) : {:.1} ms for {} messages ({:.0} msgs/sec)",
                target_total / 1_000,
                thread_ms,
                target_total,
                thread_rate
            );
        }
        println!();
    }

    if !keep_db {
        let _ = std::fs::remove_file(&db_path);
        println!("Cleaned up temporary benchmark database: {}", db_path.display());
    } else {
        println!("Kept benchmark database on disk: {}", db_path.display());
    }
}
