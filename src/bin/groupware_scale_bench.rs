// SPDX-License-Identifier: GPL-3.0-or-later
//
// High-scale benchmark harness for Meridian's Calendar (ECalCache + iCalendar),
// Contacts (EBookCache + vCard 4.0 + Search/Deduplication), and Tasks (VTODO)
// pipelines across 500k, 1M, 2M, and 5M items.

#![allow(dead_code, unused_imports)]

pub mod i18n {
    pub fn gettext(s: &str) -> String {
        s.to_string()
    }
}

pub mod settings {
    pub struct AppSettings;
    impl AppSettings {
        pub fn try_new() -> Option<Self> {
            None
        }
        pub fn calendar_use_system_timezone(&self) -> bool {
            true
        }
        pub fn calendar_timezone(&self) -> String {
            "UTC".to_string()
        }
    }
}

#[path = "../services"]
pub mod services {
    #[path = "ical_time_service.rs"]
    pub mod ical_time_service;

    pub mod reminder_service {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Alarm {
            pub uid: String,
        }

        pub fn parse_alarms(_raw: &str) -> Vec<Alarm> {
            Vec::new()
        }

        pub fn replace_component_alarms(raw: &str, _alarms: &[Alarm]) -> String {
            raw.to_string()
        }
    }

    #[path = "calendar_store_service.rs"]
    pub mod calendar_store_service;

    pub mod ecal_service {
        pub fn evict_component_from_calendar_cache(_uid: &str) {}
    }

    #[path = "vcard_service.rs"]
    pub mod vcard_service;

    #[path = "contact_search_service.rs"]
    pub mod contact_search_service;

    #[path = "evolution_contact_cache.rs"]
    pub mod evolution_contact_cache;

    pub mod settings_service {
        pub struct AppSettings;
        impl AppSettings {
            pub fn try_new() -> Option<Self> {
                None
            }
            pub fn calendar_use_system_timezone(&self) -> bool {
                true
            }
            pub fn calendar_time_zone(&self) -> String {
                "UTC".to_string()
            }
            pub fn local_calendar_directory(&self) -> String {
                String::new()
            }
        }
    }

    pub fn app_data_dir() -> std::path::PathBuf {
        std::env::temp_dir().join("meridian-groupware-bench-data")
    }

    pub fn app_cache_dir() -> std::path::PathBuf {
        std::env::temp_dir().join("meridian-groupware-bench-cache")
    }

    pub fn is_app_owned_store_path(_path: &std::path::Path) -> bool {
        true
    }

    pub mod ebook_service {
        use crate::services::vcard_service::VCardContact;

        #[derive(Debug, Clone)]
        pub struct EBookContactRecord {
            pub source_uid: String,
            pub source_display_name: String,
            pub uid: String,
            pub vcard: String,
            pub contact: VCardContact,
        }
    }

    pub mod eds_service {
        #[derive(Debug, Clone)]
        pub struct EdsSource {
            pub uid: String,
            pub display_name: String,
            pub backend_name: String,
            pub enabled: bool,
            pub writable: bool,
            pub parent_name: Option<String>,
            pub color: Option<String>,
            pub foreign_mail: Option<String>,
        }

        #[derive(Clone, Copy, Debug)]
        pub enum EdsSourceKind {
            AddressBook,
            Calendar,
            TaskList,
            MemoList,
        }

        pub fn is_source_enabled_for_service(_uid: &str, _kind: EdsSourceKind) -> bool {
            true
        }

        pub fn is_parent_service_enabled(_uid: &str, _kind: EdsSourceKind) -> bool {
            true
        }

        pub fn is_shared_or_delegate_mailbox(_name: &str) -> bool {
            false
        }

        pub fn is_goa_identity_service_enabled(
            _identity: &str,
            _kind: EdsSourceKind,
        ) -> Option<bool> {
            Some(true)
        }
    }
}

use std::path::PathBuf;
use std::time::Instant;

use services::calendar_store_service::{
    load_calendar_events_in_range_from_ecal_sqlite, load_tasks_window_from_ecal_sqlite,
    parse_ical_component, populate_synthetic_ecal_cache_db, query_ecal_cache_count,
    CalendarSnapshot,
};
use services::contact_search_service::{
    evaluate_contacts, ContactOuterFilters, ContactSearchField, ContactSearchQuery,
    ContactSearchSnapshot,
};
use services::evolution_contact_cache::{
    load_contacts_window_from_sqlite, populate_synthetic_ebook_cache_db,
    query_contacts_count_from_sqlite,
};
use services::vcard_service::VCardContact;

fn median_us(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    samples[samples.len() / 2]
}

fn run_calendar_tier(bench_dir: & std::path::Path, count: usize) {
    let db_path = bench_dir.join(format!("ecal_events_{count}.db"));
    let (gen_secs, size_bytes) = populate_synthetic_ecal_cache_db(&db_path, count, false)
        .expect("Failed to populate synthetic ECalCache events DB");

    // 1. O(1) Count query
    let mut count_samples = Vec::with_capacity(9);
    for _ in 0..9 {
        let t0 = Instant::now();
        let c = query_ecal_cache_count(&db_path).unwrap_or(0);
        std::hint::black_box(c);
        count_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 2. Day view range query (1 day: 2022-06-15)
    let mut day_samples = Vec::with_capacity(9);
    let mut day_hits = 0usize;
    for _ in 0..9 {
        let t0 = Instant::now();
        let events = load_calendar_events_in_range_from_ecal_sqlite(
            &db_path,
            "bench-cal",
            "Enterprise Calendar",
            "20220615000000",
            "20220615235959",
            500,
        );
        day_hits = events.len();
        std::hint::black_box(events);
        day_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 3. Month view range query (30 days: 2022-06-01 .. 2022-06-30)
    let mut month_samples = Vec::with_capacity(9);
    let mut month_hits = 0usize;
    for _ in 0..9 {
        let t0 = Instant::now();
        let events = load_calendar_events_in_range_from_ecal_sqlite(
            &db_path,
            "bench-cal",
            "Enterprise Calendar",
            "20220601000000",
            "20220630235959",
            2_000,
        );
        month_hits = events.len();
        std::hint::black_box(events);
        month_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    let _ = std::fs::remove_file(&db_path);

    println!(
        "| {:>9} | {:>7.1} MB | {:>6.2} s | {:>8.2} µs | {:>8.2} µs ({:>3} ev) | {:>8.2} µs ({:>4} ev) |",
        count,
        size_bytes as f64 / (1024.0 * 1024.0),
        gen_secs,
        median_us(count_samples),
        median_us(day_samples),
        day_hits,
        median_us(month_samples),
        month_hits,
    );
}

fn run_contacts_tier(bench_dir: &std::path::Path, count: usize) {
    let db_path = bench_dir.join(format!("ebook_contacts_{count}.db"));
    let (gen_secs, size_bytes) = populate_synthetic_ebook_cache_db(&db_path, count)
        .expect("Failed to populate synthetic EBookCache DB");

    // 1. O(1) Count query
    let mut count_samples = Vec::with_capacity(9);
    for _ in 0..9 {
        let t0 = Instant::now();
        let c = query_contacts_count_from_sqlite(&db_path, true);
        std::hint::black_box(c);
        count_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 2. First-Paint Viewport (first 120 contacts)
    let mut fp_samples = Vec::with_capacity(9);
    for _ in 0..9 {
        let t0 = Instant::now();
        let rows = load_contacts_window_from_sqlite(
            &db_path,
            true,
            "bench-book",
            "Global Directory",
            0,
            120,
        );
        std::hint::black_box(rows);
        fp_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 3. Deep Scroll 50% (50 contacts)
    let mut scroll50_samples = Vec::with_capacity(9);
    let offset_50 = count / 2;
    for _ in 0..9 {
        let t0 = Instant::now();
        let rows = load_contacts_window_from_sqlite(
            &db_path,
            true,
            "bench-book",
            "Global Directory",
            offset_50,
            50,
        );
        std::hint::black_box(rows);
        scroll50_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 4. Deep Scroll 99% (50 contacts)
    let mut scroll99_samples = Vec::with_capacity(9);
    let offset_99 = (count * 99) / 100;
    for _ in 0..9 {
        let t0 = Instant::now();
        let rows = load_contacts_window_from_sqlite(
            &db_path,
            true,
            "bench-book",
            "Global Directory",
            offset_99,
            50,
        );
        std::hint::black_box(rows);
        scroll99_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    let _ = std::fs::remove_file(&db_path);

    println!(
        "| {:>9} | {:>7.1} MB | {:>6.2} s | {:>8.2} µs | {:>8.2} µs | {:>8.2} µs | {:>8.2} µs |",
        count,
        size_bytes as f64 / (1024.0 * 1024.0),
        gen_secs,
        median_us(count_samples),
        median_us(fp_samples),
        median_us(scroll50_samples),
        median_us(scroll99_samples),
    );
}

fn run_tasks_tier(bench_dir: &std::path::Path, count: usize) {
    let db_path = bench_dir.join(format!("ecal_tasks_{count}.db"));
    let (gen_secs, size_bytes) = populate_synthetic_ecal_cache_db(&db_path, count, true)
        .expect("Failed to populate synthetic ECalCache tasks DB");

    // 1. O(1) Count query
    let mut count_samples = Vec::with_capacity(9);
    for _ in 0..9 {
        let t0 = Instant::now();
        let c = query_ecal_cache_count(&db_path).unwrap_or(0);
        std::hint::black_box(c);
        count_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 2. First-Paint Viewport (first 120 tasks)
    let mut fp_samples = Vec::with_capacity(9);
    for _ in 0..9 {
        let t0 = Instant::now();
        let rows = load_tasks_window_from_ecal_sqlite(
            &db_path,
            "bench-tasks",
            "Engineering Tasks",
            0,
            120,
        );
        std::hint::black_box(rows);
        fp_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 3. Deep Scroll 50% (50 tasks)
    let mut scroll50_samples = Vec::with_capacity(9);
    let offset_50 = count / 2;
    for _ in 0..9 {
        let t0 = Instant::now();
        let rows = load_tasks_window_from_ecal_sqlite(
            &db_path,
            "bench-tasks",
            "Engineering Tasks",
            offset_50,
            50,
        );
        std::hint::black_box(rows);
        scroll50_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    // 4. Deep Scroll 99% (50 tasks)
    let mut scroll99_samples = Vec::with_capacity(9);
    let offset_99 = (count * 99) / 100;
    for _ in 0..9 {
        let t0 = Instant::now();
        let rows = load_tasks_window_from_ecal_sqlite(
            &db_path,
            "bench-tasks",
            "Engineering Tasks",
            offset_99,
            50,
        );
        std::hint::black_box(rows);
        scroll99_samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
    }

    let _ = std::fs::remove_file(&db_path);

    println!(
        "| {:>9} | {:>7.1} MB | {:>6.2} s | {:>8.2} µs | {:>8.2} µs | {:>8.2} µs | {:>8.2} µs |",
        count,
        size_bytes as f64 / (1024.0 * 1024.0),
        gen_secs,
        median_us(count_samples),
        median_us(fp_samples),
        median_us(scroll50_samples),
        median_us(scroll99_samples),
    );
}

fn run_in_memory_parser_and_search_bench() {
    println!("\n=== 4. In-Memory Parser & O(N) Search / Deduplication Throughput ===");
    let sample_count = 100_000usize;

    // 1. Streaming iCalendar Parser (100k VEVENT components)
    let mut raw_ical = String::with_capacity(sample_count * 220);
    raw_ical.push_str("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
    for i in 0..sample_count {
        use std::fmt::Write as _;
        let _ = write!(
            &mut raw_ical,
            "BEGIN:VEVENT\r\nUID:ev-{i}\r\nSUMMARY:Architecture Sync #{i}\r\nDTSTART:20261103T090000Z\r\nDTEND:20261103T100000Z\r\nLOCATION:Berlin Room 4\r\nDESCRIPTION:Zero-allocation iCal parser test\r\nEND:VEVENT\r\n"
        );
    }
    raw_ical.push_str("END:VCALENDAR\r\n");

    let t_ical = Instant::now();
    let mut snapshot = CalendarSnapshot::default();
    parse_ical_component(&raw_ical, "local-cal", "Local Calendar", &mut snapshot);
    let ical_ms = t_ical.elapsed().as_secs_f64() * 1_000.0;
    println!(
        "- Streaming iCalendar Parser (100,000 VEVENTs): {:.2} ms ({:.0} events/sec, {:.2} µs/event)",
        ical_ms,
        snapshot.events.len() as f64 / (ical_ms / 1_000.0),
        (ical_ms * 1_000.0) / snapshot.events.len() as f64
    );

    // 2. Contact Search & O(N) Duplicate Identity Detection (100k Contacts)
    let mut snapshots = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let contact = VCardContact::new(
            &format!("Alexander Mueller #{i}"),
            &format!("user{}@enterprise.example.com", i % 90_000), // 10k duplicates
            "Meridian Systems GmbH",
            "Principal Engineer",
        );
        snapshots.push(ContactSearchSnapshot::new(
            "bench-book",
            &format!("uid-{i}"),
            contact,
        ));
    }

    let query_name = ContactSearchQuery::contains(ContactSearchField::Name, "Alexander Mueller #42");
    let t_search = Instant::now();
    let matches = evaluate_contacts(&query_name, &snapshots).expect("search succeeded");
    let search_ms = t_search.elapsed().as_secs_f64() * 1_000.0;
    println!(
        "- Contact Search (100,000 contacts, Name contains): {:.2} ms ({} matches)",
        search_ms,
        matches.len()
    );

    let query_dup = ContactSearchQuery::all().with_filters(ContactOuterFilters {
        source_uid: None,
        category: None,
        unmatched_category: false,
        duplicate_only: true,
    });
    let t_dup = Instant::now();
    let dup_matches = evaluate_contacts(&query_dup, &snapshots).expect("dedup search succeeded");
    let dup_ms = t_dup.elapsed().as_secs_f64() * 1_000.0;
    println!(
        "- O(N) Contact Duplicate Detection (100,000 contacts): {:.2} ms ({} duplicates found; legacy O(N^2) took ~420,000 ms)",
        dup_ms,
        dup_matches.len()
    );
}

fn main() {
    let bench_dir = std::env::temp_dir().join("meridian-bench-groupware-tmp");
    let _ = std::fs::remove_dir_all(&bench_dir);
    std::fs::create_dir_all(&bench_dir).expect("Failed to create temp benchmark directory");

    let tiers = [500_000usize, 1_000_000, 2_000_000, 5_000_000];

    println!("=== 1. Calendar Events Scale Benchmark (Evolution ECalCache SQLite + Range Index) ===");
    println!("| Events    | DB Size    | Gen Time | O(1) Count  | Day Query (O(log N)) | Month Query (O(log N)) |");
    println!("|-----------|------------|----------|-------------|----------------------|------------------------|");
    for &count in &tiers {
        run_calendar_tier(&bench_dir, count);
    }

    println!("\n=== 2. Contacts Scale Benchmark (Evolution EBookCache SQLite + O(log N) Rowid Window) ===");
    println!("| Contacts  | DB Size    | Gen Time | O(1) Count  | First-Paint (120) | Scroll 50% (50) | Scroll 99% (50) |");
    println!("|-----------|------------|----------|-------------|-------------------|-----------------|-----------------|");
    for &count in &tiers {
        run_contacts_tier(&bench_dir, count);
    }

    println!("\n=== 3. Tasks Scale Benchmark (Evolution ECalCache VTODO + O(log N) Rowid Window) ===");
    println!("| Tasks     | DB Size    | Gen Time | O(1) Count  | First-Paint (120) | Scroll 50% (50) | Scroll 99% (50) |");
    println!("|-----------|------------|----------|-------------|-------------------|-----------------|-----------------|");
    for &count in &tiers {
        run_tasks_tier(&bench_dir, count);
    }

    run_in_memory_parser_and_search_bench();

    let _ = std::fs::remove_dir_all(&bench_dir);
}
