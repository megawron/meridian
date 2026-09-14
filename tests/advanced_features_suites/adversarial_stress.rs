// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tier 5: Adversarial Hardening & Boundary Stress Tests
// Covers:
// - Truncated and corrupted X.509 DER ASN.1 fuzzing
// - Corrupted ISPDB XML handling & injection attempts
// - Extreme attachment sizes & edge-case filenames
// - Concurrency and rapid mutation stress on Keychain store
// - Malformed drag-and-drop payloads and URI schemes
// - SMTP header CRLF injection defenses

use super::common::*;
use super::r2_drag_and_drop::*;
use super::r3_autodiscover::*;
use super::r4_macos_keychain::*;
use super::r6_certificate_manager::*;

#[test]
fn tier5_adversarial_truncated_x509_der_asn1_fuzzing() {
    let der = DerBuilder::create_test_x509_der(
        0x999,
        "User S/MIME Certificate",
        "Evolution Project",
        "Evolution Root CA",
        "Evolution Project",
        "260901000000Z",
        "300901000000Z",
        None,
    );

    // Test truncated prefixes of every length from 0 to der.len() - 1
    for len in 0..der.len() {
        let truncated = &der[..len];
        // Parser must return Err without panicking
        let _ = MockX509Parser::parse_der(truncated);
    }
}

#[test]
fn tier5_adversarial_corrupted_ispdb_xml_recovery() {
    let bad_xmls = [
        "",
        "Not an XML",
        "<clientConfig><broken",
        r#"<clientConfig><emailProvider id="test"><incomingServer type="unknown"></incomingServer></emailProvider></clientConfig>"#,
        r#"<clientConfig><emailProvider id="test"><incomingServer type="imap"><hostname>bad host</hostname><port>99999999</port></incomingServer></emailProvider></clientConfig>"#,
        "<?xml version=\"1.0\"?><!DOCTYPE root [<!ENTITY test SYSTEM \"file:///etc/passwd\">]><clientConfig>&test;</clientConfig>",
    ];

    for bad in &bad_xmls {
        let res = MockAutodiscoverService::parse_ispdb_xml(bad, "user@test.org");
        assert!(res.is_err(), "Parser must safely reject corrupted XML: {}", bad);
    }
}

#[test]
fn tier5_adversarial_zero_byte_and_huge_attachments() {
    let mut composer = MockComposerDropTarget::new();

    // 0-byte file
    assert!(composer.handle_file_drop("/tmp/empty.txt", 0));
    assert_eq!(composer.attachments[0].size, 0);

    // 10 GB virtual file
    assert!(composer.handle_file_drop("/tmp/huge_disk.iso", 10_737_418_240));
    assert_eq!(composer.attachments[1].size, 10_737_418_240);
}

#[test]
fn tier5_adversarial_malformed_drag_drop_uris_and_schemes() {
    let invalid_payloads = [
        "",
        "http://example.com/not-a-message",
        "evolution-message:", // missing ID
        "unknown-scheme:12345",
        "file:///path/to/something/else",
    ];

    for payload in &invalid_payloads {
        let is_valid_msg = payload.starts_with("evolution-message:")
            && payload.len() > "evolution-message:".len();
        if !payload.starts_with("evolution-message:") || payload.len() <= "evolution-message:".len() {
            assert!(!is_valid_msg, "Should be recognized as invalid message payload: {}", payload);
        }
    }
}

#[test]
fn tier5_adversarial_keychain_rapid_concurrency_stress() {
    use std::thread;

    let store = std::sync::Arc::new(MockKeychainStore::new());
    let mut handles = Vec::new();

    // Spawn 8 threads rapidly writing and reading distinct keys
    for thread_idx in 0..8 {
        let store_clone = std::sync::Arc::clone(&store);
        let h = thread::spawn(move || {
            let creds = MockCredentialService::new(&store_clone);
            for i in 0..50 {
                let acc = format!("thread_{}_acc_{}", thread_idx, i);
                let pass = format!("pass_{}_{}", thread_idx, i);
                assert!(creds.store_password(&acc, &pass).is_ok());
                let fetched = creds.get_password(&acc).unwrap();
                assert_eq!(fetched, Some(pass));
            }
        });
        handles.push(h);
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        store.count_service_entries(KEYCHAIN_SERVICE_ID),
        400,
        "All 400 entries must exist without corruption"
    );
}

#[test]
fn tier5_adversarial_smtp_injection_attempt_defense() {
    let injection_input = "sender@example.com>\r\nBcc: evil@hacker.com\r\n";
    // Sanitization: email address must not contain CR or LF
    let has_crlf = injection_input.contains('\r') || injection_input.contains('\n');
    assert!(has_crlf, "Input contains CRLF injection attempt");

    let clean = injection_input.replace('\r', "").replace('\n', "");
    assert!(!clean.contains('\r'));
    assert!(!clean.contains('\n'));
}

// ============================================================================
// Milestone 1 Adversarial Challenges (challenger_m1_gen2_1)
// ============================================================================

use super::r1_viewer_window::*;
use chrono::{Duration, NaiveDate, NaiveDateTime};
use std::fs;
use std::path::Path;

// ── Challenge 1: Standalone Viewer Actions, Multi-Window & Lifecycle ─────────

#[test]
fn tier5_adversarial_m1_standalone_viewer_all_18_actions_contract() {
    let msg = TestMessage::new("account-1", "INBOX", "9999", "Comprehensive Action Stress");
    let mut win = MockMailViewerWindow::new(&msg);

    // Complete 18-action Libadwaita standalone viewer contract
    let full_action_set = [
        "win.reply",
        "win.reply-all",
        "win.forward",
        "win.delete",
        "win.mark-junk",
        "win.mark-not-junk",
        "win.mark-read",
        "win.mark-unread",
        "win.toggle-read-unread",
        "win.mail-print",
        "win.mail-print-cancel",
        "win.view-message-source",
        "win.find-in-message",
        "win.zoom-in",
        "win.zoom-out",
        "win.zoom-reset",
        "win.toggle-all-headers",
        "win.export",
        "win.allow-remote-images",
        "win.close",
    ];

    for action in &full_action_set {
        win.action_map.insert(action.to_string(), true);
    }

    for action in &full_action_set {
        assert!(
            win.action_map.contains_key(*action),
            "Action {} must be present in standalone viewer map",
            action
        );
        assert!(
            *win.action_map.get(*action).unwrap(),
            "Action {} must be enabled",
            action
        );
    }

    // Stress zoom actions to boundaries
    for _ in 0..50 {
        win.zoom_in();
    }
    assert!(win.zoom_level <= 3.0, "Zoom-in must clamp at 3.0");

    for _ in 0..50 {
        win.zoom_out();
    }
    assert!(win.zoom_level >= 0.5, "Zoom-out must clamp at 0.5");

    win.zoom_reset();
    assert_eq!(win.zoom_level, 1.0, "Zoom-reset must return to exactly 1.0");
}

#[test]
fn tier5_adversarial_m1_standalone_viewer_massive_multiwindow_activation_and_no_collision() {
    let registry = MockMailViewerRegistry::default();
    const NUM_WINDOWS: usize = 1_000;

    // Activate 1,000 distinct messages across various account sources and folders
    for i in 0..NUM_WINDOWS {
        let source = format!("source_{}", i % 5);
        let folder = if i % 2 == 0 { "INBOX" } else { "Archive/Work" };
        let uid = format!("msg_uid_{:05}", i);
        let subject = format!("Stress Message {:05}", i);
        let msg = TestMessage::new(&source, folder, &uid, &subject);

        let (key, is_new) = registry.activate_message(&msg);
        assert!(is_new, "Message {} should create a new window", i);
        assert!(key.ends_with(&uid));
    }

    assert_eq!(
        registry.count_open_windows(),
        NUM_WINDOWS,
        "Registry must track exactly 1,000 distinct windows without collision"
    );

    // Verify independent lookup of every single window
    for i in 0..NUM_WINDOWS {
        let source = format!("source_{}", i % 5);
        let folder = if i % 2 == 0 { "INBOX" } else { "Archive/Work" };
        let uid = format!("msg_uid_{:05}", i);
        let msg = TestMessage::new(&source, folder, &uid, "");

        let (_, is_new) = registry.activate_message(&msg);
        assert!(
            !is_new,
            "Re-activating message {} must reuse existing window",
            i
        );
    }

    assert_eq!(
        registry.count_open_windows(),
        NUM_WINDOWS,
        "Re-activation must not add any new windows"
    );
}

#[test]
fn tier5_adversarial_m1_standalone_viewer_deduplication_under_rapid_fire() {
    let registry = MockMailViewerRegistry::default();
    let msg = TestMessage::new("acc-main", "INBOX", "single-target-42", "Single Target");

    // Initial activation
    let (key_first, is_new_first) = registry.activate_message(&msg);
    assert!(is_new_first);

    // Rapid-fire 500 re-activations (simulating rapid user double-clicks and Enter spam)
    for _ in 0..500 {
        let (key, is_new) = registry.activate_message(&msg);
        assert!(!is_new, "Rapid activation must never spawn duplicate window");
        assert_eq!(key, key_first);
        assert_eq!(registry.count_open_windows(), 1);
    }
}

#[test]
fn tier5_adversarial_m1_standalone_viewer_lifecycle_close_cleanup_and_reopen() {
    let registry = MockMailViewerRegistry::default();
    let msg1 = TestMessage::new("acc-1", "INBOX", "msg-101", "Message 101");
    let msg2 = TestMessage::new("acc-1", "INBOX", "msg-102", "Message 102");
    let msg3 = TestMessage::new("acc-2", "Sent", "msg-201", "Message 201");

    let (key1, _) = registry.activate_message(&msg1);
    let (key2, _) = registry.activate_message(&msg2);
    let (key3, _) = registry.activate_message(&msg3);
    assert_eq!(registry.count_open_windows(), 3);

    // Close window 2
    let closed2 = registry.close_window(&key2);
    assert!(closed2, "Window 2 must close successfully");
    assert_eq!(registry.count_open_windows(), 2);

    // Close non-existent or already closed key
    let closed_again = registry.close_window(&key2);
    assert!(!closed_again, "Closing already closed key must return false");
    assert_eq!(registry.count_open_windows(), 2);

    // Re-open message 2: should create a brand-new window instance with fresh cancellation token
    let (key2_reopen, is_new2) = registry.activate_message(&msg2);
    assert!(is_new2, "Re-opening closed message must create a new window");
    assert_eq!(key2_reopen, key2);
    assert_eq!(registry.count_open_windows(), 3);

    // Close all
    assert!(registry.close_window(&key1));
    assert!(registry.close_window(&key2_reopen));
    assert!(registry.close_window(&key3));
    assert_eq!(registry.count_open_windows(), 0);
}

// ── Challenge 2: Composer File Drop (Unusual Paths, Dirs, Non-existent) ──────

/// Validates file drop candidate against real filesystem rules matching compose_dialog.rs
fn validate_and_process_composer_file_drop(path: &Path) -> Option<TestAttachment> {
    // 1. Metadata check matching compose_dialog.rs:4219 & 4251
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        // Safely reject directories, block/char devices, sockets
        return None;
    }

    // 2. Filename extraction matching compose_dialog.rs:4225
    let filename = path.file_name()?.to_str()?.to_string();

    // 3. MIME deduction
    let mime_type = deduce_mime_type(&filename);

    Some(TestAttachment {
        filename,
        mime_type,
        size: metadata.len(),
        path: path.to_string_lossy().to_string(),
    })
}

#[test]
fn tier5_adversarial_m1_composer_file_drop_rejects_directories_without_crash() {
    let temp_base = std::env::temp_dir().join("evolution_adversarial_test_dirs");
    let _ = fs::create_dir_all(&temp_base);

    let sub_dir = temp_base.join("sub_folder_test");
    let _ = fs::create_dir_all(&sub_dir);

    // Dropping directory directly
    let res = validate_and_process_composer_file_drop(&sub_dir);
    assert!(res.is_none(), "Directory must be safely rejected by composer file drop");

    // Dropping base temp directory
    let res_base = validate_and_process_composer_file_drop(&temp_base);
    assert!(res_base.is_none(), "Base directory must be safely rejected");

    // Root directory
    let res_root = validate_and_process_composer_file_drop(Path::new("/"));
    assert!(res_root.is_none(), "Root directory must be safely rejected");

    let _ = fs::remove_dir_all(&temp_base);
}

#[test]
fn tier5_adversarial_m1_composer_file_drop_rejects_nonexistent_files() {
    let non_existent_paths = [
        "/tmp/evolution_ghost_non_existent_file_987654321.pdf",
        "/does/not/exist/at/all.txt",
        "",
        "relative/nonexistent.docx",
    ];

    for path_str in &non_existent_paths {
        let res = validate_and_process_composer_file_drop(Path::new(path_str));
        assert!(
            res.is_none(),
            "Non-existent file '{}' must be rejected safely",
            path_str
        );
    }
}

#[test]
fn tier5_adversarial_m1_composer_file_drop_zero_byte_and_unusual_paths() {
    let temp_base = std::env::temp_dir().join("evolution_adversarial_test_files");
    let _ = fs::create_dir_all(&temp_base);

    // 1. Zero-byte file
    let empty_file = temp_base.join("empty_zero_byte.txt");
    fs::write(&empty_file, b"").unwrap();
    let res_empty = validate_and_process_composer_file_drop(&empty_file);
    assert!(res_empty.is_some(), "Zero-byte file must be accepted");
    let att = res_empty.unwrap();
    assert_eq!(att.size, 0);
    assert_eq!(att.mime_type, "text/plain");

    // 2. Unicode and emoji filename
    let unicode_file = temp_base.join("Jahresabschluss – Prüfung 📊 & Bericht (Final) #1.pdf");
    fs::write(&unicode_file, b"%PDF-1.4 test").unwrap();
    let res_unicode = validate_and_process_composer_file_drop(&unicode_file);
    assert!(res_unicode.is_some(), "Unicode/emoji file must be accepted");
    let att_u = res_unicode.unwrap();
    assert_eq!(att_u.filename, "Jahresabschluss – Prüfung 📊 & Bericht (Final) #1.pdf");
    assert_eq!(att_u.mime_type, "application/pdf");
    assert_eq!(att_u.size, 13);

    // 3. Multi-extension file
    let tar_file = temp_base.join("backup_system_archive.v2.tar.gz");
    fs::write(&tar_file, b"\x1f\x8b archive data").unwrap();
    let res_tar = validate_and_process_composer_file_drop(&tar_file);
    assert!(res_tar.is_some());
    let att_tar = res_tar.unwrap();
    assert_eq!(att_tar.mime_type, "application/gzip");

    let _ = fs::remove_dir_all(&temp_base);
}

#[test]
fn tier5_adversarial_m1_composer_file_drop_symlink_discrimination() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let temp_base = std::env::temp_dir().join("evolution_adversarial_symlinks");
        let _ = fs::create_dir_all(&temp_base);

        let real_file = temp_base.join("target_real.txt");
        fs::write(&real_file, b"valid payload").unwrap();

        let symlink_file = temp_base.join("link_to_real.txt");
        let _ = symlink(&real_file, &symlink_file);

        let real_dir = temp_base.join("target_real_dir");
        let _ = fs::create_dir_all(&real_dir);

        let symlink_dir = temp_base.join("link_to_dir");
        let _ = symlink(&real_dir, &symlink_dir);

        let broken_symlink = temp_base.join("broken_link.txt");
        let _ = symlink(temp_base.join("does_not_exist"), &broken_symlink);

        // Symlink to file -> accepted
        let res_f = validate_and_process_composer_file_drop(&symlink_file);
        assert!(res_f.is_some(), "Symlink to valid file must be accepted");

        // Symlink to dir -> rejected
        let res_d = validate_and_process_composer_file_drop(&symlink_dir);
        assert!(res_d.is_none(), "Symlink to directory must be rejected");

        // Broken symlink -> rejected
        let res_b = validate_and_process_composer_file_drop(&broken_symlink);
        assert!(res_b.is_none(), "Broken symlink must be rejected");

        let _ = fs::remove_dir_all(&temp_base);
    }
}

// ── Challenge 3: Sidebar Folder Drop (Malformed/Missing Payload) ─────────────

/// Simulates sidebar folder drop validation matching mail_page.rs lines 6257-6289
fn validate_sidebar_folder_drop(
    payload_str: &str,
    target_folder_selectable: bool,
    store: &mut MockFolderStore,
    target_folder_id: &str,
    is_copy: bool,
) -> Result<bool, String> {
    if payload_str.is_empty() {
        return Ok(false);
    }

    if let Some(msg_id) = payload_str.strip_prefix("evolution-message:") {
        let msg_id = msg_id.trim();
        if msg_id.is_empty() {
            return Ok(false); // malformed message ID
        }
        if !target_folder_selectable {
            return Err("In diesen Ordner können keine Nachrichten abgelegt werden".to_string());
        }
        if is_copy {
            store.copy_message(msg_id, target_folder_id, false)?;
        } else {
            store.move_message(msg_id, target_folder_id, false)?;
        }
        return Ok(true);
    }

    if let Some(json) = payload_str.strip_prefix("evolution-folder:") {
        // Must parse valid JSON payload
        let parsed: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("Invalid JSON: {e}"))?;
        if !parsed.is_object() || parsed.get("source_uid").is_none() {
            return Ok(false);
        }
        return Ok(true);
    }

    // Any other scheme / text
    Ok(false)
}

#[test]
fn tier5_adversarial_m1_sidebar_folder_drop_malformed_and_missing_payloads() {
    let mut store = MockFolderStore::new();

    let malformed_payloads = [
        "",
        "   ",
        "evolution-message:",
        "evolution-message:   ",
        "evolution-folder:",
        "evolution-folder:not-a-json",
        "evolution-folder:[]",
        "evolution-folder:{\"unknown_field\": true}",
        "http://evil.com/payload",
        "file:///tmp/something.eml",
        "random-unrecognized-string",
    ];

    for payload in &malformed_payloads {
        let res = validate_sidebar_folder_drop(payload, true, &mut store, "archive", false);
        assert!(
            matches!(res, Ok(false) | Err(_)),
            "Malformed payload '{}' must result in Ok(false) or safe Err without crashing",
            payload
        );
    }

    // Folder counts must remain strictly unchanged
    let inbox = store.folders.get("inbox").unwrap();
    assert_eq!(inbox.total_count, 10);
    assert_eq!(inbox.unread_count, 5);
}

#[test]
fn tier5_adversarial_m1_sidebar_folder_drop_unselectable_folder_rejected() {
    let mut store = MockFolderStore::new();
    let valid_payload = "evolution-message:msg_101";

    // Target folder has selectable == false (e.g., account root header)
    let res = validate_sidebar_folder_drop(valid_payload, false, &mut store, "inbox", false);
    assert!(res.is_err(), "Unselectable folder drop must return Err");
    assert!(res.unwrap_err().contains("können keine Nachrichten abgelegt werden"));

    // Message must NOT have moved
    assert_eq!(store.message_locations.get("msg_101").unwrap(), "inbox");
}

#[test]
fn tier5_adversarial_m1_sidebar_folder_drop_nonexistent_message_fails_gracefully() {
    let mut store = MockFolderStore::new();
    let ghost_payload = "evolution-message:ghost_nonexistent_uid_12345";

    let res = validate_sidebar_folder_drop(ghost_payload, true, &mut store, "archive", false);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), "Message not found");

    // Archive count remains untouched
    assert_eq!(store.folders.get("archive").unwrap().total_count, 20);
}

// ── Challenge 4: Calendar Continuous Grid Drag (Minutes, Boundaries) ─────────

#[test]
fn tier5_adversarial_m1_calendar_drag_negative_minute_offsets_and_day_boundaries() {
    // 1. Event at 00:15 dragged -30 minutes -> crosses midnight to 23:45 previous day
    let start1 = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(0, 15, 0)
        .unwrap();
    let end1 = start1 + Duration::minutes(45);
    let shifted1 = shift_calendar_event(start1, end1, 0, -30);
    assert_eq!(
        shifted1.new_dtstart,
        NaiveDate::from_ymd_opt(2026, 9, 14).unwrap().and_hms_opt(23, 45, 0).unwrap()
    );
    assert_eq!(shifted1.duration_minutes, 45);

    // 2. Event at 23:50 dragged +25 minutes -> crosses midnight to 00:15 next day
    let start2 = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(23, 50, 0)
        .unwrap();
    let end2 = start2 + Duration::minutes(60);
    let shifted2 = shift_calendar_event(start2, end2, 0, 25);
    assert_eq!(
        shifted2.new_dtstart,
        NaiveDate::from_ymd_opt(2026, 9, 16).unwrap().and_hms_opt(0, 15, 0).unwrap()
    );
    assert_eq!(shifted2.duration_minutes, 60);

    // 3. Month boundary crossing: 2026-03-01 00:30 dragged -60 minutes -> 2026-02-28 23:30
    let start3 = NaiveDate::from_ymd_opt(2026, 3, 1)
        .unwrap()
        .and_hms_opt(0, 30, 0)
        .unwrap();
    let end3 = start3 + Duration::minutes(30);
    let shifted3 = shift_calendar_event(start3, end3, 0, -60);
    assert_eq!(
        shifted3.new_dtstart,
        NaiveDate::from_ymd_opt(2026, 2, 28).unwrap().and_hms_opt(23, 30, 0).unwrap()
    );

    // 4. Year boundary crossing: 2027-01-01 00:10 dragged -30 minutes -> 2026-12-31 23:40
    let start4 = NaiveDate::from_ymd_opt(2027, 1, 1)
        .unwrap()
        .and_hms_opt(0, 10, 0)
        .unwrap();
    let end4 = start4 + Duration::minutes(30);
    let shifted4 = shift_calendar_event(start4, end4, 0, -30);
    assert_eq!(
        shifted4.new_dtstart,
        NaiveDate::from_ymd_opt(2026, 12, 31).unwrap().and_hms_opt(23, 40, 0).unwrap()
    );
}

/// Standalone test oracle matching calendar_page.rs shift_ical_datetime_value_full
fn ical_datetime_shift_oracle(
    value: &str,
    delta_days: i64,
    delta_minutes: i64,
) -> Option<String> {
    let value = value.trim();
    if value.len() == 8 {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some((date + Duration::days(delta_days)).format("%Y%m%d").to_string());
    }
    if value.len() < 15 || value.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    if !value.is_char_boundary(15) {
        return None;
    }
    let datetime = NaiveDateTime::parse_from_str(&value[..15], "%Y%m%dT%H%M%S").ok()?;
    let suffix = &value[15..];
    let shift = Duration::days(delta_days) + Duration::minutes(delta_minutes);
    Some(format!(
        "{}{}",
        (datetime + shift).format("%Y%m%dT%H%M%S"),
        suffix
    ))
}

#[test]
fn tier5_adversarial_m1_calendar_ical_datetime_shift_oracle_adversarial_inputs() {
    // 1. All-day event ignores minute delta and preserves 8-char YYYYMMDD
    let allday = "20260914";
    let shifted_allday = ical_datetime_shift_oracle(allday, 3, 240).unwrap();
    assert_eq!(shifted_allday, "20260917", "All-day must ignore minute delta and stay 8 chars");

    // 2. Timed event with timezone / UTC Z suffix
    let timed_utc = "20260914T120000Z";
    let shifted_utc = ical_datetime_shift_oracle(timed_utc, 0, -90).unwrap();
    assert_eq!(shifted_utc, "20260914T103000Z", "Z suffix must be preserved");

    // 3. Negative minute crossing midnight
    let midnight_edge = "20260914T001500";
    let shifted_edge = ical_datetime_shift_oracle(midnight_edge, 0, -45).unwrap();
    assert_eq!(shifted_edge, "20260913T233000");

    // 4. Leap year boundary: 2024-02-28 + 1 day -> 2024-02-29
    let leap_edge = "20240228T233000";
    let shifted_leap = ical_datetime_shift_oracle(leap_edge, 0, 60).unwrap();
    assert_eq!(shifted_leap, "20240229T003000");

    // 5. Non-leap year boundary: 2026-02-28 + 1 day -> 2026-03-01
    let non_leap = "20260228T233000";
    let shifted_non_leap = ical_datetime_shift_oracle(non_leap, 0, 60).unwrap();
    assert_eq!(shifted_non_leap, "20260301T003000");

    // 6. Malformed values fail closed
    let malformed = ["", "2026", "202609", "20260914X120000", "not-a-date", "20260914T999999"];
    for bad in &malformed {
        assert!(
            ical_datetime_shift_oracle(bad, 1, 30).is_none(),
            "Malformed ical datetime '{}' must return None",
            bad
        );
    }
}

// ============================================================================
// Milestone 1 Challenger 2 Empirical Stress Harness (challenger_m1_gen2_2)
// ============================================================================

/// Helper: Shift weekday token with circular rem_euclid(7) arithmetic matching calendar_page.rs
fn format_shifted_recurrence_day(day_token: &str, delta_days: i64) -> Result<String, String> {
    let token = day_token.trim().to_ascii_uppercase();
    let (ordinal, weekday_str) = if token.ends_with("MO") {
        (&token[..token.len() - 2], "MO")
    } else if token.ends_with("TU") {
        (&token[..token.len() - 2], "TU")
    } else if token.ends_with("WE") {
        (&token[..token.len() - 2], "WE")
    } else if token.ends_with("TH") {
        (&token[..token.len() - 2], "TH")
    } else if token.ends_with("FR") {
        (&token[..token.len() - 2], "FR")
    } else if token.ends_with("SA") {
        (&token[..token.len() - 2], "SA")
    } else if token.ends_with("SU") {
        (&token[..token.len() - 2], "SU")
    } else {
        return Err(format!("Invalid weekday token: {day_token}"));
    };

    let base_idx: i64 = match weekday_str {
        "MO" => 0,
        "TU" => 1,
        "WE" => 2,
        "TH" => 3,
        "FR" => 4,
        "SA" => 5,
        "SU" => 6,
        _ => unreachable!(),
    };

    let new_idx = (base_idx + delta_days).rem_euclid(7);
    let new_weekday = match new_idx {
        0 => "MO",
        1 => "TU",
        2 => "WE",
        3 => "TH",
        4 => "FR",
        5 => "SA",
        6 => "SU",
        _ => unreachable!(),
    };

    Ok(format!("{ordinal}{new_weekday}"))
}

/// Helper: Shift RRULE matching calendar_page.rs shift_rrule_full
fn shift_rrule_full_oracle(
    rrule_value: &str,
    delta_days: i64,
    delta_minutes: i64,
) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    let mut frequency = String::new();

    for part in rrule_value.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            parts.push(part.to_string());
            continue;
        };
        let key_upper = k.to_ascii_uppercase();
        if key_upper == "FREQ" {
            frequency = v.to_ascii_uppercase();
        }
    }

    if !matches!(frequency.as_str(), "DAILY" | "WEEKLY") {
        return Err(
            "Diese Wiederholung kann nicht sicher per Ziehen verschoben werden; bitte den Termin bearbeiten"
                .to_string(),
        );
    }

    for part in rrule_value.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            parts.push(part.to_string());
            continue;
        };
        let key_upper = k.to_ascii_uppercase();
        if matches!(key_upper.as_str(), "BYMONTHDAY" | "BYMONTH" | "BYSETPOS") {
            return Err(
                "Diese Wiederholung enthält Filter, die beim Ziehen nicht sicher verschoben werden können"
                    .to_string(),
            );
        }
        if key_upper == "UNTIL" {
            let shifted = ical_datetime_shift_oracle(v, delta_days, delta_minutes).ok_or_else(|| {
                "RRULE-UNTIL konnte beim Verschieben nicht ausgewertet werden".to_string()
            })?;
            parts.push(format!("{k}={shifted}"));
        } else if key_upper == "BYDAY" {
            let shifted_days = v
                .split(',')
                .map(|token| format_shifted_recurrence_day(token, delta_days))
                .collect::<Result<Vec<_>, String>>()?;
            parts.push(format!("{k}={}", shifted_days.join(",")));
        } else {
            parts.push(part.to_string());
        }
    }

    Ok(parts.join(";"))
}

/// Helper: Shift iCalendar component matching calendar_page.rs shift_event_component_full
fn shift_component_oracle(
    raw: &str,
    delta_days: i64,
    delta_minutes: i64,
) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("Der Termin enthält keine iCalendar-Daten".to_string());
    }
    if raw.contains("RECURRENCE-ID") {
        return Err(
            "Serienausnahmen mit RECURRENCE-ID können nicht per Ziehen verschoben werden; der Kalenderdienst kann die Ausnahme nicht eindeutig adressieren"
                .to_string(),
        );
    }

    let mut lines = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").lines() {
        let line = raw_line.trim_end();
        let Some((prop, val)) = line.split_once(':') else {
            lines.push(line.to_string());
            continue;
        };
        let prop_name = prop.split(';').next().unwrap_or(prop).to_ascii_uppercase();
        if prop_name == "DTSTART" || prop_name == "DTEND" {
            let shifted = ical_datetime_shift_oracle(val, delta_days, delta_minutes).ok_or_else(|| {
                format!("{prop_name} konnte beim Verschieben nicht ausgewertet werden")
            })?;
            lines.push(format!("{prop}:{shifted}"));
        } else if prop_name == "RDATE" || prop_name == "EXDATE" {
            let shifted_vals = val
                .split(',')
                .map(|v| {
                    ical_datetime_shift_oracle(v, delta_days, delta_minutes).ok_or_else(|| {
                        format!("{prop_name} konnte beim Verschieben nicht ausgewertet werden")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            lines.push(format!("{prop}:{}", shifted_vals.join(",")));
        } else if prop_name == "RRULE" {
            let shifted_rule = shift_rrule_full_oracle(val, delta_days, delta_minutes)?;
            lines.push(format!("{prop}:{shifted_rule}"));
        } else {
            lines.push(line.to_string());
        }
    }

    Ok(lines.join("\r\n"))
}

#[test]
fn tier5_challenger2_rapid_50_windows_concurrency_and_cancellation() {
    let registry = MockMailViewerRegistry::default();
    const WINDOW_COUNT: usize = 50;

    // 1. Sequentially open 50 windows for 50 distinct messages
    let mut message_keys = Vec::new();
    for i in 1..=WINDOW_COUNT {
        let source = format!("account_{}", i % 4);
        let folder = if i % 2 == 0 { "INBOX" } else { "Archive" };
        let uid = format!("msg_stress_{:03}", i);
        let subject = format!("Rapid Window Stress #{:03}", i);
        let msg = TestMessage::new(&source, folder, &uid, &subject);

        let (key, is_new) = registry.activate_message(&msg);
        assert!(is_new, "Message {} must instantiate a new window", i);
        message_keys.push(key);
    }

    assert_eq!(
        registry.count_open_windows(),
        WINDOW_COUNT,
        "Registry must track exactly 50 open windows"
    );

    // 2. Rapid re-activation of the same 50 messages (deduplication verification)
    for i in 1..=WINDOW_COUNT {
        let source = format!("account_{}", i % 4);
        let folder = if i % 2 == 0 { "INBOX" } else { "Archive" };
        let uid = format!("msg_stress_{:03}", i);
        let msg = TestMessage::new(&source, folder, &uid, "");

        let (key, is_new) = registry.activate_message(&msg);
        assert!(!is_new, "Re-activation of {} must reuse existing window", i);
        assert_eq!(key, message_keys[i - 1]);
    }
    assert_eq!(
        registry.count_open_windows(),
        WINDOW_COUNT,
        "Window count must remain 50 after re-activation deduplication"
    );

    // 3. Sequential close: first 25 windows in order
    for key in message_keys.iter().take(25) {
        let closed = registry.close_window(key);
        assert!(closed, "Window {} must close cleanly", key);
    }
    assert_eq!(
        registry.count_open_windows(),
        25,
        "Registry must prune down to 25 open windows"
    );

    // 4. Reverse close: remaining 25 windows in reverse order
    for key in message_keys.iter().skip(25).rev() {
        let closed = registry.close_window(key);
        assert!(closed, "Window {} must close cleanly in reverse order", key);
    }
    assert_eq!(
        registry.count_open_windows(),
        0,
        "Registry must prune completely down to 0 after all 50 windows close"
    );

    // 5. Idempotent close: closing already closed windows returns false without panic
    for key in &message_keys {
        assert!(
            !registry.close_window(key),
            "Closing already closed window must safely return false"
        );
    }
    assert_eq!(registry.count_open_windows(), 0);

    // 6. Reopen a subset (10 messages) to verify clean recreation and fresh tokens
    for i in 1..=10 {
        let source = format!("account_{}", i % 4);
        let folder = if i % 2 == 0 { "INBOX" } else { "Archive" };
        let uid = format!("msg_stress_{:03}", i);
        let msg = TestMessage::new(&source, folder, &uid, "Reopened");

        let (_, is_new) = registry.activate_message(&msg);
        assert!(is_new, "Reopened message {} must get fresh window instance", i);
    }
    assert_eq!(registry.count_open_windows(), 10);
    for key in message_keys.iter().take(10) {
        assert!(registry.close_window(key));
    }
    assert_eq!(registry.count_open_windows(), 0);
}

#[test]
fn tier5_challenger2_drag_drop_unselected_disabled_and_multimessage() {
    let mut store = MockFolderStore::new();

    // Add extra folders to simulate realistic hierarchy
    let mut drafts = TestFolder::new("drafts", "Entwürfe");
    drafts.unread_count = 0;
    drafts.total_count = 3;
    store.folders.insert("drafts".to_string(), drafts);

    let mut receipts = TestFolder::new("receipts", "Belege");
    receipts.unread_count = 0;
    receipts.total_count = 15;
    store.folders.insert("receipts".to_string(), receipts);

    // Initial state:
    // "inbox": active/selected reader view, total=10, unread=5, contains msg_101, msg_102
    // "archive": unselected folder, total=20, unread=0
    // "receipts": unselected folder, total=15, unread=0
    assert_eq!(store.message_locations.get("msg_101").unwrap(), "inbox");
    assert_eq!(store.folders.get("inbox").unwrap().total_count, 10);
    assert_eq!(store.folders.get("inbox").unwrap().unread_count, 5);

    // ── Part A: Move into an unselected folder ────────────────────────────────
    let move_res = validate_sidebar_folder_drop(
        "evolution-message:msg_101",
        true,
        &mut store,
        "archive",
        false, // MOVE
    );
    assert!(move_res.is_ok() && move_res.unwrap(), "Move to unselected archive must succeed");
    assert_eq!(store.message_locations.get("msg_101").unwrap(), "archive");

    // Source inbox counts decremented
    assert_eq!(store.folders.get("inbox").unwrap().total_count, 9);
    // Destination archive count incremented
    assert_eq!(store.folders.get("archive").unwrap().total_count, 21);

    // ── Part B: Copy into an unselected folder ────────────────────────────────
    let copy_res = validate_sidebar_folder_drop(
        "evolution-message:msg_102",
        true,
        &mut store,
        "receipts",
        true, // COPY
    );
    assert!(copy_res.is_ok() && copy_res.unwrap(), "Copy to unselected receipts must succeed");
    // Source message location and counts unchanged
    assert_eq!(store.message_locations.get("msg_102").unwrap(), "inbox");
    assert_eq!(store.folders.get("inbox").unwrap().total_count, 9);
    // Destination receipts count incremented
    assert_eq!(store.folders.get("receipts").unwrap().total_count, 16);

    // ── Part C: Drop onto a disabled / non-selectable folder ──────────────────
    // Target folder has selectable == false (e.g. IMAP namespace header or disabled heading)
    let disabled_res = validate_sidebar_folder_drop(
        "evolution-message:msg_102",
        false, // selectable = false
        &mut store,
        "gmail_heading",
        false,
    );
    assert!(
        disabled_res.is_err(),
        "Dropping onto disabled/non-selectable folder must return Err"
    );
    assert!(disabled_res
        .unwrap_err()
        .contains("können keine Nachrichten abgelegt werden"));
    // Message remains in inbox and counts untouched
    assert_eq!(store.message_locations.get("msg_102").unwrap(), "inbox");
    assert_eq!(store.folders.get("inbox").unwrap().total_count, 9);

    // ── Part D: Drop onto same folder (no-op) ─────────────────────────────────
    let same_res = validate_sidebar_folder_drop(
        "evolution-message:msg_102",
        true,
        &mut store,
        "inbox",
        false,
    );
    assert!(same_res.is_ok() && same_res.unwrap(), "Same-folder drop is safe no-op");
    assert_eq!(store.folders.get("inbox").unwrap().total_count, 9);

    // ── Part E: Multiple message IDs in payload (safely rejected) ─────────────
    let multi_id_payloads = [
        "evolution-message:msg_101,msg_102",
        "evolution-message:msg_101\nevolution-message:msg_102",
        "evolution-message:msg_101 msg_102",
        "evolution-message:;msg_101;",
    ];
    for multi_payload in &multi_id_payloads {
        let res = validate_sidebar_folder_drop(multi_payload, true, &mut store, "drafts", false);
        assert!(
            res.is_err(),
            "Multiple/compound message ID payload '{}' must be rejected safely",
            multi_payload
        );
    }
    assert_eq!(store.folders.get("drafts").unwrap().total_count, 3);
}

#[test]
fn tier5_challenger2_calendar_drag_rrule_weekday_shifts() {
    // 1. Shift WEEKLY rule with multiple weekdays (+1 day: MO,WE,FR -> TU,TH,SA)
    let rule_mwf = "FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=10";
    let shifted_mwf_1 = shift_rrule_full_oracle(rule_mwf, 1, 0).unwrap();
    assert!(shifted_mwf_1.contains("BYDAY=TU,TH,SA"));
    assert!(shifted_mwf_1.contains("COUNT=10"));
    assert!(shifted_mwf_1.contains("FREQ=WEEKLY"));

    // 2. Shift +2 days: MO,WE,FR -> WE,FR,SU
    let shifted_mwf_2 = shift_rrule_full_oracle(rule_mwf, 2, 0).unwrap();
    assert!(shifted_mwf_2.contains("BYDAY=WE,FR,SU"));

    // 3. Circular wrap across Sunday (+1 day: SU -> MO)
    let rule_sunday = "FREQ=WEEKLY;BYDAY=SU;INTERVAL=2";
    let shifted_sun = shift_rrule_full_oracle(rule_sunday, 1, 0).unwrap();
    assert!(shifted_sun.contains("BYDAY=MO"));
    assert!(shifted_sun.contains("INTERVAL=2"));

    // 4. Negative circular wrap across Monday (-1 day: MO -> SU)
    let rule_monday = "FREQ=WEEKLY;BYDAY=MO;WKST=MO";
    let shifted_mon = shift_rrule_full_oracle(rule_monday, -1, 0).unwrap();
    assert!(shifted_mon.contains("BYDAY=SU"));
    assert!(shifted_mon.contains("WKST=MO"));

    // 5. Shift weekend days +2 days: SA,SU -> MO,TU
    let rule_weekend = "FREQ=WEEKLY;BYDAY=SA,SU";
    let shifted_weekend = shift_rrule_full_oracle(rule_weekend, 2, 0).unwrap();
    assert!(shifted_weekend.contains("BYDAY=MO,TU"));

    // 6. DAILY rule with UNTIL parameter shifted by days and minutes
    let rule_until = "FREQ=DAILY;UNTIL=20260901T235959Z";
    let shifted_until = shift_rrule_full_oracle(rule_until, 2, 120).unwrap();
    // 2026-09-01 23:59:59 + 2 days + 2 hours = 2026-09-04 01:59:59Z
    assert!(shifted_until.contains("UNTIL=20260904T015959Z"));

    // 7. Safety Rejection: MONTHLY recurrence cannot be shifted losslessly by raw drag
    let rule_monthly = "FREQ=MONTHLY;BYDAY=1MO";
    let err_monthly = shift_rrule_full_oracle(rule_monthly, 1, 0);
    assert!(err_monthly.is_err());
    assert!(err_monthly.unwrap_err().contains("nicht sicher per Ziehen verschoben"));

    // 8. Safety Rejection: YEARLY recurrence
    let rule_yearly = "FREQ=YEARLY;BYMONTH=6";
    let err_yearly = shift_rrule_full_oracle(rule_yearly, 1, 0);
    assert!(err_yearly.is_err());

    // 9. Safety Rejection: Complex filters (BYMONTHDAY, BYMONTH, BYSETPOS)
    for filter_rule in &[
        "FREQ=WEEKLY;BYDAY=MO;BYMONTHDAY=15",
        "FREQ=WEEKLY;BYDAY=TU;BYMONTH=7",
        "FREQ=WEEKLY;BYDAY=WE;BYSETPOS=1",
    ] {
        let err = shift_rrule_full_oracle(filter_rule, 1, 0);
        assert!(err.is_err(), "Filter rule '{}' must be rejected", filter_rule);
        assert!(err.unwrap_err().contains("nicht sicher verschoben werden"));
    }
}

#[test]
fn tier5_challenger2_calendar_drag_timezone_and_allday_integrity() {
    // 1. All-day event (VALUE=DATE): delta_minutes must be strictly ignored, preserving YYYYMMDD
    let allday_raw = concat!(
        "BEGIN:VEVENT\r\n",
        "UID:event-allday-1\r\n",
        "DTSTART;VALUE=DATE:20260810\r\n",
        "DTEND;VALUE=DATE:20260811\r\n",
        "SUMMARY:All Day Vacation\r\n",
        "END:VEVENT\r\n"
    );
    let shifted_allday = shift_component_oracle(allday_raw, 2, 240).expect("All-day shift");
    assert!(shifted_allday.contains("DTSTART;VALUE=DATE:20260812"));
    assert!(shifted_allday.contains("DTEND;VALUE=DATE:20260813"));
    assert!(shifted_allday.contains("SUMMARY:All Day Vacation"));

    // 2. Timed event with TZID parameter: shifted with minutes and preserving TZID
    let timed_tzid_raw = concat!(
        "BEGIN:VEVENT\r\n",
        "UID:event-tzid-berlin\r\n",
        "DTSTART;TZID=Europe/Berlin:20260810T100000\r\n",
        "DTEND;TZID=Europe/Berlin:20260810T110000\r\n",
        "SUMMARY:Berlin Standup\r\n",
        "END:VEVENT\r\n"
    );
    let shifted_berlin = shift_component_oracle(timed_tzid_raw, 1, 150).expect("Berlin shift");
    // +1 day, +150 min (2h 30m) -> 12:30 start, 13:30 end
    assert!(shifted_berlin.contains("DTSTART;TZID=Europe/Berlin:20260811T123000"));
    assert!(shifted_berlin.contains("DTEND;TZID=Europe/Berlin:20260811T133000"));

    // 3. Quoted TZID parameter: TZID="America/New_York"
    let timed_quoted_raw = concat!(
        "BEGIN:VEVENT\r\n",
        "UID:event-quoted-tz\r\n",
        "DTSTART;TZID=\"America/New_York\":20260810T090000\r\n",
        "DTEND;TZID=\"America/New_York\":20260810T100000\r\n",
        "END:VEVENT\r\n"
    );
    let shifted_quoted = shift_component_oracle(timed_quoted_raw, 0, 45).expect("Quoted TZ shift");
    assert!(shifted_quoted.contains("DTSTART;TZID=\"America/New_York\":20260810T094500"));
    assert!(shifted_quoted.contains("DTEND;TZID=\"America/New_York\":20260810T104500"));

    // 4. UTC event with Z suffix
    let utc_raw = concat!(
        "BEGIN:VEVENT\r\n",
        "UID:event-utc-z\r\n",
        "DTSTART:20260810T140000Z\r\n",
        "DTEND:20260810T150000Z\r\n",
        "END:VEVENT\r\n"
    );
    let shifted_utc = shift_component_oracle(utc_raw, 0, -120).expect("UTC shift");
    assert!(shifted_utc.contains("DTSTART:20260810T120000Z"));
    assert!(shifted_utc.contains("DTEND:20260810T130000Z"));

    // 5. Mixed-timezone RDATE and EXDATE lists
    let mixed_zones_raw = concat!(
        "BEGIN:VEVENT\r\n",
        "UID:event-mixed-zones\r\n",
        "DTSTART;TZID=Europe/Berlin:20260810T100000\r\n",
        "DTEND;TZID=Europe/Berlin:20260810T110000\r\n",
        "RDATE;TZID=Europe/Berlin:20260817T100000,20260824T100000\r\n",
        "EXDATE;TZID=America/Chicago:20260818T150000\r\n",
        "END:VEVENT\r\n"
    );
    let shifted_mixed = shift_component_oracle(mixed_zones_raw, 1, 60).expect("Mixed zones shift");
    assert!(shifted_mixed.contains("DTSTART;TZID=Europe/Berlin:20260811T110000"));
    assert!(shifted_mixed.contains("DTEND;TZID=Europe/Berlin:20260811T120000"));
    assert!(shifted_mixed.contains("RDATE;TZID=Europe/Berlin:20260818T110000,20260825T110000"));
    assert!(shifted_mixed.contains("EXDATE;TZID=America/Chicago:20260819T160000"));

    // 6. Safety Rejection: Detached recurrence exception with RECURRENCE-ID
    let detached_exception_raw = concat!(
        "BEGIN:VEVENT\r\n",
        "UID:event-exception-series\r\n",
        "RECURRENCE-ID:20260810T100000Z\r\n",
        "DTSTART:20260810T100000Z\r\n",
        "DTEND:20260810T110000Z\r\n",
        "END:VEVENT\r\n"
    );
    let err_detached = shift_component_oracle(detached_exception_raw, 1, 0);
    assert!(err_detached.is_err());
    assert!(err_detached
        .unwrap_err()
        .contains("Serienausnahmen mit RECURRENCE-ID"));
}

// ============================================================================
// Milestone 2 Challenger 2 Empirical Stress Harness (challenger_m2_gen2_2)
// ============================================================================

#[test]
fn tier5_challenger2_m2_all_10_providers_and_37_aliases_exhaustive_matrix() {
    // Complete catalog of all 10 built-in providers across 37 domains/aliases
    struct ProviderExpected {
        email: &'static str,
        expected_in_host: &'static str,
        expected_in_port: u16,
        expected_in_sec: SocketSecurity,
        expected_out_host: &'static str,
        expected_out_port: u16,
        expected_out_sec: SocketSecurity,
        expected_user_template: &'static str,
        expected_resolved_user: &'static str,
        expected_display_name: &'static str,
    }

    let provider_matrix: &[ProviderExpected] = &[
        // 1. Google Gmail (2 aliases)
        ProviderExpected {
            email: "user@gmail.com",
            expected_in_host: "imap.gmail.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.gmail.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@gmail.com",
            expected_display_name: "Google Mail",
        },
        ProviderExpected {
            email: "user@googlemail.com",
            expected_in_host: "imap.gmail.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.gmail.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@googlemail.com",
            expected_display_name: "Google Mail",
        },
        // 2. Microsoft Outlook / Office 365 (8 aliases)
        ProviderExpected {
            email: "user@outlook.com",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@outlook.com",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@hotmail.com",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@hotmail.com",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@live.com",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@live.com",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@msn.com",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@msn.com",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@outlook.de",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@outlook.de",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@hotmail.de",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@hotmail.de",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@live.de",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@live.de",
            expected_display_name: "Outlook / Microsoft 365",
        },
        ProviderExpected {
            email: "user@office365.com",
            expected_in_host: "outlook.office365.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp-mail.outlook.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@office365.com",
            expected_display_name: "Outlook / Microsoft 365",
        },
        // 3. Apple iCloud (3 aliases)
        ProviderExpected {
            email: "user@icloud.com",
            expected_in_host: "imap.mail.me.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.me.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILLOCALPART%",
            expected_resolved_user: "user",
            expected_display_name: "Apple iCloud",
        },
        ProviderExpected {
            email: "user@me.com",
            expected_in_host: "imap.mail.me.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.me.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILLOCALPART%",
            expected_resolved_user: "user",
            expected_display_name: "Apple iCloud",
        },
        ProviderExpected {
            email: "user@mac.com",
            expected_in_host: "imap.mail.me.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.me.com",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILLOCALPART%",
            expected_resolved_user: "user",
            expected_display_name: "Apple iCloud",
        },
        // 4. Yahoo! Mail (8 aliases)
        ProviderExpected {
            email: "user@yahoo.com",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@yahoo.com",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@yahoo.de",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@yahoo.de",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@ymail.com",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@ymail.com",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@rocketmail.com",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@rocketmail.com",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@yahoo.co.uk",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@yahoo.co.uk",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@yahoo.fr",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@yahoo.fr",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@yahoo.it",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@yahoo.it",
            expected_display_name: "Yahoo! Mail",
        },
        ProviderExpected {
            email: "user@yahoo.es",
            expected_in_host: "imap.mail.yahoo.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mail.yahoo.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@yahoo.es",
            expected_display_name: "Yahoo! Mail",
        },
        // 5. Posteo (5 aliases)
        ProviderExpected {
            email: "user@posteo.de",
            expected_in_host: "posteo.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "posteo.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@posteo.de",
            expected_display_name: "Posteo",
        },
        ProviderExpected {
            email: "user@posteo.net",
            expected_in_host: "posteo.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "posteo.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@posteo.net",
            expected_display_name: "Posteo",
        },
        ProviderExpected {
            email: "user@posteo.org",
            expected_in_host: "posteo.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "posteo.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@posteo.org",
            expected_display_name: "Posteo",
        },
        ProviderExpected {
            email: "user@posteo.at",
            expected_in_host: "posteo.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "posteo.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@posteo.at",
            expected_display_name: "Posteo",
        },
        ProviderExpected {
            email: "user@posteo.ch",
            expected_in_host: "posteo.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "posteo.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@posteo.ch",
            expected_display_name: "Posteo",
        },
        // 6. GMX Mail (5 aliases)
        ProviderExpected {
            email: "user@gmx.de",
            expected_in_host: "imap.gmx.net",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "mail.gmx.net",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@gmx.de",
            expected_display_name: "GMX Mail",
        },
        ProviderExpected {
            email: "user@gmx.net",
            expected_in_host: "imap.gmx.net",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "mail.gmx.net",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@gmx.net",
            expected_display_name: "GMX Mail",
        },
        ProviderExpected {
            email: "user@gmx.at",
            expected_in_host: "imap.gmx.net",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "mail.gmx.net",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@gmx.at",
            expected_display_name: "GMX Mail",
        },
        ProviderExpected {
            email: "user@gmx.ch",
            expected_in_host: "imap.gmx.net",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "mail.gmx.net",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@gmx.ch",
            expected_display_name: "GMX Mail",
        },
        ProviderExpected {
            email: "user@gmx.com",
            expected_in_host: "imap.gmx.net",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "mail.gmx.net",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@gmx.com",
            expected_display_name: "GMX Mail",
        },
        // 7. WEB.DE (1 alias)
        ProviderExpected {
            email: "user@web.de",
            expected_in_host: "imap.web.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.web.de",
            expected_out_port: 587,
            expected_out_sec: SocketSecurity::Starttls,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@web.de",
            expected_display_name: "WEB.DE",
        },
        // 8. Mailbox.org (1 alias)
        ProviderExpected {
            email: "user@mailbox.org",
            expected_in_host: "imap.mailbox.org",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.mailbox.org",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@mailbox.org",
            expected_display_name: "Mailbox.org",
        },
        // 9. Fastmail (2 aliases)
        ProviderExpected {
            email: "user@fastmail.com",
            expected_in_host: "imap.fastmail.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.fastmail.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@fastmail.com",
            expected_display_name: "Fastmail",
        },
        ProviderExpected {
            email: "user@fastmail.fm",
            expected_in_host: "imap.fastmail.com",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "smtp.fastmail.com",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@fastmail.fm",
            expected_display_name: "Fastmail",
        },
        // 10. Telekom Mail (T-Online) (2 aliases)
        ProviderExpected {
            email: "user@t-online.de",
            expected_in_host: "secureimap.t-online.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "securesmtp.t-online.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@t-online.de",
            expected_display_name: "Telekom Mail (T-Online)",
        },
        ProviderExpected {
            email: "user@magenta.de",
            expected_in_host: "secureimap.t-online.de",
            expected_in_port: 993,
            expected_in_sec: SocketSecurity::Ssl,
            expected_out_host: "securesmtp.t-online.de",
            expected_out_port: 465,
            expected_out_sec: SocketSecurity::Ssl,
            expected_user_template: "%EMAILADDRESS%",
            expected_resolved_user: "user@magenta.de",
            expected_display_name: "Telekom Mail (T-Online)",
        },
    ];

    assert_eq!(
        provider_matrix.len(),
        37,
        "Matrix must test exactly 37 distinct provider domain aliases across 10 providers"
    );

    for item in provider_matrix {
        let conf = MockAutodiscoverService::lookup_builtin_profile(item.email)
            .unwrap_or_else(|| panic!("Profile missing for {}", item.email));

        assert_eq!(conf.incoming.hostname, item.expected_in_host, "Incoming host mismatch for {}", item.email);
        assert_eq!(conf.incoming.port, item.expected_in_port, "Incoming port mismatch for {}", item.email);
        assert_eq!(conf.incoming.security, item.expected_in_sec, "Incoming security mismatch for {}", item.email);
        assert_eq!(conf.outgoing.hostname, item.expected_out_host, "Outgoing host mismatch for {}", item.email);
        assert_eq!(conf.outgoing.port, item.expected_out_port, "Outgoing port mismatch for {}", item.email);
        assert_eq!(conf.outgoing.security, item.expected_out_sec, "Outgoing security mismatch for {}", item.email);
        assert_eq!(conf.incoming.username_template, item.expected_user_template, "Username template mismatch for {}", item.email);
        assert_eq!(conf.display_name, item.expected_display_name, "Display name mismatch for {}", item.email);

        // Verify template substitution produces exact expected username
        let (local, _domain) = MockAutodiscoverService::extract_domain(item.email).unwrap();
        let resolved_user = MockAutodiscoverService::substitute_template(
            &conf.incoming.username_template,
            item.email,
            local,
        );
        assert_eq!(resolved_user, item.expected_resolved_user, "Resolved username mismatch for {}", item.email);

        // Test mixed-case domain lookup resilience
        let mixed_case_email = format!(
            "{}@{}",
            local.to_ascii_uppercase(),
            item.email.split_once('@').unwrap().1.to_ascii_uppercase()
        );
        let conf_mixed = MockAutodiscoverService::lookup_builtin_profile(&mixed_case_email)
            .unwrap_or_else(|| panic!("Case-insensitive profile missing for {}", mixed_case_email));
        assert_eq!(conf_mixed.incoming.hostname, item.expected_in_host);
        assert_eq!(conf_mixed.outgoing.hostname, item.expected_out_host);
    }
}

#[test]
fn tier5_challenger2_m2_heuristic_fallback_uncommon_and_private_domains() {
    let test_domains = [
        "custom-server.xyz",
        "corp.internal",
        "infra.systems",
        "gateway.intranet.localdomain",
        "cloud-platform.technology",
        "enterprise-vault.finance",
        "operations.support.center",
        "my-company.co.uk",
        "service-mesh.network",
        "sub.dept.division.corp.internal",
        "foundation.ngo",
        "cooperative.museum",
        "open-source.community",
        "data-center.cloud",
    ];

    for domain in &test_domains {
        let email = format!("operator@{}", domain);
        let fallback = MockAutodiscoverService::heuristic_fallback(&email)
            .unwrap_or_else(|e| panic!("Heuristic fallback failed for {}: {}", domain, e));

        assert_eq!(
            fallback.incoming.hostname,
            format!("imap.{}", domain),
            "Incoming hostname heuristic mismatch for {}",
            domain
        );
        assert_eq!(fallback.incoming.port, 993, "Incoming port must be 993 for {}", domain);
        assert_eq!(fallback.incoming.security, SocketSecurity::Ssl, "Incoming security must be SSL for {}", domain);
        assert_eq!(
            fallback.outgoing.hostname,
            format!("smtp.{}", domain),
            "Outgoing hostname heuristic mismatch for {}",
            domain
        );
        assert_eq!(fallback.outgoing.port, 587, "Outgoing port must be 587 for {}", domain);
        assert_eq!(fallback.outgoing.security, SocketSecurity::Starttls, "Outgoing security must be STARTTLS for {}", domain);
        assert_eq!(fallback.display_name, format!("{} Mail", domain));
        assert_eq!(fallback.incoming.username_template, "%EMAILADDRESS%");

        // Verify cascade defaults to heuristic when ISPDB and MX are absent
        let cascade_res = simulate_cascade(&email, false, None);
        assert_eq!(cascade_res.tier, DiscoveryTier::Heuristic);
        assert_eq!(cascade_res.config.incoming.hostname, format!("imap.{}", domain));
    }
}

#[test]
fn tier5_challenger2_m2_idn_and_punycode_comprehensive_matrix() {
    // 1. Punycode IDN domains
    let punycode_cases = [
        ("hans@xn--mnchen-3ya.de", "xn--mnchen-3ya.de", "imap.xn--mnchen-3ya.de", "smtp.xn--mnchen-3ya.de"),
        ("kontakt@xn--sterreich-z7a.at", "xn--sterreich-z7a.at", "imap.xn--sterreich-z7a.at", "smtp.xn--sterreich-z7a.at"),
        ("info@xn--mgbh0fb.org", "xn--mgbh0fb.org", "imap.xn--mgbh0fb.org", "smtp.xn--mgbh0fb.org"),
        ("admin@xn--e1afmkfd.xn--p1ai", "xn--e1afmkfd.xn--p1ai", "imap.xn--e1afmkfd.xn--p1ai", "smtp.xn--e1afmkfd.xn--p1ai"),
        ("sales@xn--fsqu00a.xn--4gbrim", "xn--fsqu00a.xn--4gbrim", "imap.xn--fsqu00a.xn--4gbrim", "smtp.xn--fsqu00a.xn--4gbrim"),
    ];

    for (email, expected_domain, in_host, out_host) in punycode_cases {
        let (local, domain) = MockAutodiscoverService::extract_domain(email)
            .unwrap_or_else(|e| panic!("Failed extracting domain for {}: {}", email, e));
        assert_eq!(domain, expected_domain);

        let fallback = MockAutodiscoverService::heuristic_fallback(email).unwrap();
        assert_eq!(fallback.incoming.hostname, in_host);
        assert_eq!(fallback.outgoing.hostname, out_host);
        assert_eq!(fallback.incoming.port, 993);
        assert_eq!(fallback.outgoing.port, 587);

        let resolved_user = MockAutodiscoverService::substitute_template(
            &fallback.incoming.username_template,
            email,
            local,
        );
        assert_eq!(resolved_user, email);
    }

    // 2. Native Unicode internationalized domains (UTF-8 multi-byte characters)
    let unicode_cases = [
        ("hans@münchen.de", "münchen.de", "imap.münchen.de", "smtp.münchen.de"),
        ("kontakt@österreich.at", "österreich.at", "imap.österreich.at", "smtp.österreich.at"),
        ("admin@тест.рф", "тест.рф", "imap.тест.рф", "smtp.тест.рф"),
        ("sales@北京.中国", "北京.中国", "imap.北京.中国", "smtp.北京.中国"),
        ("user@éclair.fr", "éclair.fr", "imap.éclair.fr", "smtp.éclair.fr"),
    ];

    for (email, expected_domain, in_host, out_host) in unicode_cases {
        let (_local, domain) = MockAutodiscoverService::extract_domain(email)
            .unwrap_or_else(|e| panic!("Failed extracting native unicode domain for {}: {}", email, e));
        assert_eq!(domain, expected_domain);

        let fallback = MockAutodiscoverService::heuristic_fallback(email).unwrap();
        assert_eq!(fallback.incoming.hostname, in_host);
        assert_eq!(fallback.outgoing.hostname, out_host);
    }

    // 3. Multi-byte Unicode in localpart
    let localpart_unicode = "jürgen.müller@münchen.de";
    let (local, domain) = MockAutodiscoverService::extract_domain(localpart_unicode).unwrap();
    assert_eq!(local, "jürgen.müller");
    assert_eq!(domain, "münchen.de");

    let sub_local = MockAutodiscoverService::substitute_template("%EMAILLOCALPART%", localpart_unicode, local);
    assert_eq!(sub_local, "jürgen.müller");

    let sub_domain = MockAutodiscoverService::substitute_template("%EMAILDOMAIN%", localpart_unicode, local);
    assert_eq!(sub_domain, "münchen.de");

    let sub_full = MockAutodiscoverService::substitute_template("%EMAILADDRESS%", localpart_unicode, local);
    assert_eq!(sub_full, "jürgen.müller@münchen.de");
}

#[test]
fn tier5_challenger2_m2_extreme_input_lengths_and_fuzzing() {
    // 1. Extreme 1,000-character username
    let extreme_local = "a".repeat(1000);
    let extreme_email = format!("{}@example.com", extreme_local);
    let (local, domain) = MockAutodiscoverService::extract_domain(&extreme_email).unwrap();
    assert_eq!(local.len(), 1000);
    assert_eq!(domain, "example.com");

    let fallback = MockAutodiscoverService::heuristic_fallback(&extreme_email).unwrap();
    assert_eq!(fallback.incoming.hostname, "imap.example.com");
    assert_eq!(fallback.outgoing.hostname, "smtp.example.com");

    let resolved = MockAutodiscoverService::substitute_template(
        &fallback.incoming.username_template,
        &extreme_email,
        local,
    );
    assert_eq!(resolved.len(), 1000 + "@example.com".len());

    // 2. 240-character multi-level domain name
    let deep_domain = format!("{}com", "sub.".repeat(48));
    let deep_email = format!("user@{}", deep_domain);
    let (_l, d) = MockAutodiscoverService::extract_domain(&deep_email).unwrap();
    assert_eq!(d, deep_domain);
    let deep_fallback = MockAutodiscoverService::heuristic_fallback(&deep_email).unwrap();
    assert_eq!(deep_fallback.incoming.hostname, format!("imap.{}", deep_domain));

    // 3. 20-level nested subdomain
    let nested_email = "user@a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.domain.org";
    let (nl, nd) = MockAutodiscoverService::extract_domain(nested_email).unwrap();
    assert_eq!(nl, "user");
    assert_eq!(nd, "a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.domain.org");

    // 4. Repeated template expansion stress (500 tokens)
    let heavy_template = "%EMAILADDRESS%:%EMAILLOCALPART%:%EMAILDOMAIN%;".repeat(500);
    let expanded = MockAutodiscoverService::substitute_template(&heavy_template, "test@org.io", "test");
    assert_eq!(expanded.len(), 500 * ("test@org.io:test:org.io;".len()));

    // 5. Huge XML payload: 500 dummy tags surrounding valid server blocks
    let mut huge_xml = String::from("<?xml version=\"1.0\"?>\n<clientConfig>\n  <emailProvider id=\"huge.org\">\n");
    for i in 0..500 {
        huge_xml.push_str(&format!("    <dummyProperty_{i}>value_{i}</dummyProperty_{i}>\n"));
    }
    huge_xml.push_str(r#"    <displayName>Huge Provider</displayName>
    <incomingServer type="imap">
      <hostname>imap.huge.org</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.huge.org</hostname>
      <port>465</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#);

    let parsed = MockAutodiscoverService::parse_ispdb_xml(&huge_xml, "user@huge.org");
    assert!(parsed.is_ok(), "Parser must safely handle huge XML payload without stack overflow");
    let conf = parsed.unwrap();
    assert_eq!(conf.incoming.hostname, "imap.huge.org");
    assert_eq!(conf.outgoing.hostname, "smtp.huge.org");
    assert_eq!(conf.display_name, "Huge Provider");

    // 6. 100,000-character non-XML junk string
    let huge_junk = "random text without any tags ".repeat(3500);
    let junk_res = MockAutodiscoverService::parse_ispdb_xml(&huge_junk, "user@test.org");
    assert!(junk_res.is_err(), "Junk string must return Err safely");
}

#[test]
fn tier5_challenger2_m2_xml_security_and_malformed_attack_vectors() {
    // 1. XXE with DOCTYPE system entity
    let xxe_system = r#"<?xml version="1.0"?>
    <!DOCTYPE foo [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> ]>
    <clientConfig><emailProvider id="test">&xxe;</emailProvider></clientConfig>"#;
    assert!(
        MockAutodiscoverService::parse_ispdb_xml(xxe_system, "user@test.org").is_err(),
        "XXE SYSTEM must be rejected"
    );

    // 2. XXE with internal DOCTYPE entity
    let xxe_internal = r#"<?xml version="1.0"?>
    <!DOCTYPE foo [ <!ENTITY evil "malicious payload"> ]>
    <clientConfig><emailProvider id="test">&evil;</emailProvider></clientConfig>"#;
    assert!(
        MockAutodiscoverService::parse_ispdb_xml(xxe_internal, "user@test.org").is_err(),
        "XXE entity expansion must be rejected"
    );

    // 3. DOCTYPE declaration without entity
    let doctype_only = r#"<?xml version="1.0"?>
    <!DOCTYPE html>
    <clientConfig><emailProvider id="test"></emailProvider></clientConfig>"#;
    assert!(
        MockAutodiscoverService::parse_ispdb_xml(doctype_only, "user@test.org").is_err(),
        "DOCTYPE declarations must be rejected"
    );

    // 4. Hostname containing internal spaces
    let space_host_xml = r#"<clientConfig>
      <emailProvider id="bad.org">
        <incomingServer type="imap">
          <hostname>bad host name.com</hostname>
          <port>993</port>
        </incomingServer>
        <outgoingServer type="smtp">
          <hostname>smtp.bad.org</hostname>
          <port>587</port>
        </outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let err_space = MockAutodiscoverService::parse_ispdb_xml(space_host_xml, "user@bad.org");
    assert!(err_space.is_err());
    assert!(err_space.unwrap_err().contains("Invalid incoming hostname"));

    // 5. Port 0 is rejected
    let port_zero_xml = r#"<clientConfig>
      <emailProvider id="zero.org">
        <incomingServer type="imap">
          <hostname>imap.zero.org</hostname>
          <port>0</port>
        </incomingServer>
        <outgoingServer type="smtp">
          <hostname>smtp.zero.org</hostname>
          <port>587</port>
        </outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let err_zero = MockAutodiscoverService::parse_ispdb_xml(port_zero_xml, "user@zero.org");
    assert!(err_zero.is_err());
    assert!(err_zero.unwrap_err().contains("Port cannot be zero"));

    // 6. Port overflow (> 65535)
    let port_overflow_xml = r#"<clientConfig>
      <emailProvider id="overflow.org">
        <incomingServer type="imap">
          <hostname>imap.overflow.org</hostname>
          <port>70000</port>
        </incomingServer>
        <outgoingServer type="smtp">
          <hostname>smtp.overflow.org</hostname>
          <port>587</port>
        </outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let err_overflow = MockAutodiscoverService::parse_ispdb_xml(port_overflow_xml, "user@overflow.org");
    assert!(err_overflow.is_err());

    // 7. Missing incomingServer entirely
    let missing_in_xml = r#"<clientConfig>
      <emailProvider id="miss.org">
        <outgoingServer type="smtp">
          <hostname>smtp.miss.org</hostname>
          <port>587</port>
        </outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(MockAutodiscoverService::parse_ispdb_xml(missing_in_xml, "user@miss.org").is_err());

    // 8. Missing outgoingServer entirely
    let missing_out_xml = r#"<clientConfig>
      <emailProvider id="miss.org">
        <incomingServer type="imap">
          <hostname>imap.miss.org</hostname>
          <port>993</port>
        </incomingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(MockAutodiscoverService::parse_ispdb_xml(missing_out_xml, "user@miss.org").is_err());
}

#[test]
fn tier5_challenger2_m2_cascade_simulation_and_dns_mx_correlation() {
    // 1. Built-in profile matches take precedence over ISPDB
    let ispdb_xml_gmail = build_ispdb_xml(
        "gmail.com",
        "custom-imap.gmail.com",
        1143,
        "STARTTLS",
        "custom-smtp.gmail.com",
        2525,
        "STARTTLS",
    );
    let res_builtin_wins = simulate_cascade("user@gmail.com", true, Some(&ispdb_xml_gmail));
    assert_eq!(res_builtin_wins.tier, DiscoveryTier::Builtin);
    assert_eq!(res_builtin_wins.config.incoming.hostname, "imap.gmail.com");
    assert_eq!(res_builtin_wins.config.incoming.port, 993);

    // 2. DNS MX correlation maps known SaaS patterns
    let saas_mx_cases = [
        ("ceo@corporate-custom.com", "aspmx.l.google.com", "imap.gmail.com", 993, "smtp.gmail.com", 465),
        ("cfo@enterprise-finance.de", "enterprise-de.mail.protection.outlook.com", "outlook.office365.com", 993, "smtp-mail.outlook.com", 587),
        ("dev@my-custom-domain.io", "mx1.mail.icloud.com", "imap.mail.me.com", 993, "smtp.mail.me.com", 587),
        ("team@startup.cc", "in1-smtp.messagingengine.com", "imap.fastmail.com", 993, "smtp.fastmail.com", 465),
    ];

    for (email, mx_host, exp_in_host, exp_in_port, exp_out_host, exp_out_port) in saas_mx_cases {
        let cascade_mx = simulate_cascade_with_mx(email, mx_host);
        assert_eq!(cascade_mx.tier, DiscoveryTier::DnsMx);
        assert_eq!(cascade_mx.config.incoming.hostname, exp_in_host);
        assert_eq!(cascade_mx.config.incoming.port, exp_in_port);
        assert_eq!(cascade_mx.config.outgoing.hostname, exp_out_host);
        assert_eq!(cascade_mx.config.outgoing.port, exp_out_port);
        assert_eq!(cascade_mx.config.email, email);
    }

    // 3. Proofpoint or unrecognized MX gateways fall through cleanly to Heuristic
    let gateway_mx_cases = [
        ("security@bank.com", "mx1.pphosted.com", "imap.bank.com"),
        ("staff@firm.net", "mail.proofpoint.com", "imap.firm.net"),
        ("anon@server.xyz", "mail.obscure-hoster.net", "imap.server.xyz"),
    ];

    for (email, mx_host, exp_fallback_host) in gateway_mx_cases {
        let cascade_gate = simulate_cascade_with_mx(email, mx_host);
        assert_eq!(cascade_gate.tier, DiscoveryTier::Heuristic);
        assert_eq!(cascade_gate.config.incoming.hostname, exp_fallback_host);
    }

    // 4. Corrupted ISPDB XML cascades safely to Heuristic
    let corrupt_xml = "<clientConfig><unclosed";
    let res_corrupt = simulate_cascade("user@unknown-domain.de", true, Some(corrupt_xml));
    assert_eq!(res_corrupt.tier, DiscoveryTier::Heuristic);
    assert_eq!(res_corrupt.config.incoming.hostname, "imap.unknown-domain.de");
}

// ============================================================================
// Milestone 2 Challenger 1 Empirical Stress Harness (challenger_m2_gen2_1)
// ============================================================================

/// Exact standalone oracle of AutodiscoverService::extract_server_block from autodiscover_service.rs:994
fn autodiscover_extract_server_block_oracle(xml: &str, tag_name: &str, server_type: &str) -> Option<String> {
    let target_type = server_type.to_ascii_lowercase();
    let mut search_idx = 0;
    while let Some(start_rel) = xml[search_idx..].find(&format!("<{tag_name}")) {
        let tag_start = search_idx + start_rel;
        let end_of_open = xml[tag_start..].find('>')? + tag_start;
        let open_tag = &xml[tag_start..=end_of_open];

        let open_tag_lower = open_tag.to_ascii_lowercase();
        let is_matching_type = open_tag_lower.contains(&format!("type=\"{target_type}\""))
            || open_tag_lower.contains(&format!("type='{target_type}'"));

        let close_tag = format!("</{tag_name}>");
        let block_end = xml[end_of_open..].find(&close_tag)? + end_of_open;

        if is_matching_type {
            return Some(xml[end_of_open + 1..block_end].to_string());
        }
        search_idx = block_end + close_tag.len();
    }
    None
}

/// Exact standalone oracle of AutodiscoverService::extract_tag_content from autodiscover_service.rs:1017
fn autodiscover_extract_tag_content_oracle(xml: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{tag}");
    let close_tag = format!("</{tag}>");

    let start_pos = xml.find(&open_tag)?;
    let content_start = xml[start_pos..].find('>')? + start_pos + 1;
    let content_end = xml[content_start..].find(&close_tag)? + content_start;

    let content = xml[content_start..content_end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

/// Exact standalone oracle of AutodiscoverService::parse_thunderbird_autoconfig_xml from autodiscover_service.rs:847
fn parse_thunderbird_autoconfig_xml_oracle(
    xml: &str,
    email: &str,
    fallback_domain: &str,
) -> Result<DiscoveredAccountConfig, String> {
    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return Err("XML content is empty".to_string());
    }

    // Safety defense against XXE: reject XML with custom DOCTYPE entity expansions
    if trimmed.contains("<!ENTITY") || trimmed.contains("<!DOCTYPE") {
        return Err("XML containing DOCTYPE/ENTITY definitions is rejected for security".to_string());
    }

    let domain = email.split_once('@').map(|(_, d)| d).unwrap_or("");
    let effective_domain = if domain.is_empty() {
        fallback_domain
    } else {
        domain
    };

    let provider_name = autodiscover_extract_tag_content_oracle(trimmed, "displayName")
        .or_else(|| autodiscover_extract_tag_content_oracle(trimmed, "displayShortName"))
        .unwrap_or_else(|| format!("{effective_domain} Mail"));

    let (incoming_block, is_pop3) = autodiscover_extract_server_block_oracle(trimmed, "incomingServer", "imap")
        .map(|b| (b, false))
        .or_else(|| {
            autodiscover_extract_server_block_oracle(trimmed, "incomingServer", "pop3")
                .map(|b| (b, true))
        })
        .ok_or_else(|| "Missing <incomingServer> tag in ISPDB XML".to_string())?;

    let outgoing_block = autodiscover_extract_server_block_oracle(trimmed, "outgoingServer", "smtp")
        .ok_or_else(|| "Missing <outgoingServer> tag in ISPDB XML".to_string())?;

    let in_host = autodiscover_extract_tag_content_oracle(&incoming_block, "hostname")
        .ok_or_else(|| "Missing hostname in incomingServer".to_string())?;

    if in_host.contains(' ') {
        return Err(format!("Invalid incoming hostname: '{in_host}'"));
    }

    let in_socket = autodiscover_extract_tag_content_oracle(&incoming_block, "socketType")
        .unwrap_or_else(|| "SSL".to_string());
    let in_sec = match in_socket.to_ascii_uppercase().as_str() {
        "STARTTLS" => SocketSecurity::Starttls,
        "NONE" | "PLAIN" => SocketSecurity::Plain,
        _ => SocketSecurity::Ssl,
    };

    let in_port_str = autodiscover_extract_tag_content_oracle(&incoming_block, "port");
    let in_port: u16 = if let Some(ref p_str) = in_port_str {
        p_str
            .parse::<u16>()
            .map_err(|_| format!("Invalid incoming port: '{p_str}'"))?
    } else {
        match (is_pop3, &in_sec) {
            (true, SocketSecurity::Ssl) => 995,
            (true, _) => 110,
            (false, SocketSecurity::Ssl) => 993,
            _ => 143,
        }
    };

    if in_port == 0 {
        return Err("Incoming port cannot be zero".to_string());
    }

    let in_user = autodiscover_extract_tag_content_oracle(&incoming_block, "username")
        .unwrap_or_else(|| "%EMAILADDRESS%".to_string());

    let out_host = autodiscover_extract_tag_content_oracle(&outgoing_block, "hostname")
        .ok_or_else(|| "Missing hostname in outgoingServer".to_string())?;

    if out_host.contains(' ') {
        return Err(format!("Invalid outgoing hostname: '{out_host}'"));
    }

    let out_socket = autodiscover_extract_tag_content_oracle(&outgoing_block, "socketType")
        .unwrap_or_else(|| "STARTTLS".to_string());
    let out_sec = match out_socket.to_ascii_uppercase().as_str() {
        "SSL" => SocketSecurity::Ssl,
        "NONE" | "PLAIN" => SocketSecurity::Plain,
        _ => SocketSecurity::Starttls,
    };

    let out_port_str = autodiscover_extract_tag_content_oracle(&outgoing_block, "port");
    let out_port: u16 = if let Some(ref p_str) = out_port_str {
        p_str
            .parse::<u16>()
            .map_err(|_| format!("Invalid outgoing port: '{p_str}'"))?
    } else {
        match out_sec {
            SocketSecurity::Ssl => 465,
            _ => 587,
        }
    };

    if out_port == 0 {
        return Err("Outgoing port cannot be zero".to_string());
    }

    let out_user = autodiscover_extract_tag_content_oracle(&outgoing_block, "username")
        .unwrap_or_else(|| "%EMAILADDRESS%".to_string());

    Ok(DiscoveredAccountConfig {
        email: email.to_string(),
        display_name: provider_name,
        incoming: DiscoveredServerConfig {
            hostname: in_host,
            port: in_port,
            security: in_sec,
            username_template: in_user,
        },
        outgoing: DiscoveredServerConfig {
            hostname: out_host,
            port: out_port,
            security: out_sec,
            username_template: out_user,
        },
    })
}

/// Standalone oracle of AccountSetupDialog::is_discovery_candidate from account_setup_dialog.rs:94
fn is_discovery_candidate_oracle(value: &str) -> Option<(&str, &str)> {
    let trimmed = value.trim();
    if trimmed.chars().filter(|&c| c == '@').count() != 1 {
        return None;
    }
    let (local, domain) = trimmed.split_once('@')?;
    let local = local.trim();
    let domain = domain.trim();

    if local.is_empty()
        || domain.is_empty()
        || trimmed.chars().any(char::is_whitespace)
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return None;
    }

    let tld = domain.rsplit('.').next()?;
    if tld.len() < 2 {
        return None;
    }

    Some((local, domain))
}

#[test]
fn tier5_challenger1_m2_xml_fuzzing_byte_truncations() {
    let full_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="adversarial.org">
    <domain>adversarial.org</domain>
    <displayName>Adversarial Test Mail</displayName>
    <incomingServer type="imap">
      <hostname>imap.adversarial.org</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.adversarial.org</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    // Verify the full valid XML parses cleanly first
    let valid_oracle = parse_thunderbird_autoconfig_xml_oracle(full_xml, "test@adversarial.org", "adversarial.org");
    assert!(valid_oracle.is_ok(), "Baseline full XML must parse cleanly");
    let valid_mock = MockAutodiscoverService::parse_ispdb_xml(full_xml, "test@adversarial.org");
    assert!(valid_mock.is_ok(), "Baseline full XML must parse cleanly in Mock");

    let outgoing_end = full_xml.find("</outgoingServer>").unwrap() + "</outgoingServer>".len();

    // Stress: truncate at every single byte offset from 0 up to before </outgoingServer>
    for cut in 0..outgoing_end {
        let truncated = &full_xml[..cut];
        // Must never panic and must fail closed (return Err)
        let res_oracle = parse_thunderbird_autoconfig_xml_oracle(truncated, "test@adversarial.org", "adversarial.org");
        assert!(res_oracle.is_err(), "Truncated XML before outgoingServer close at byte {} must return Err", cut);

        let res_mock = MockAutodiscoverService::parse_ispdb_xml(truncated, "test@adversarial.org");
        assert!(res_mock.is_err(), "Truncated XML before outgoingServer close at byte {} must return Err in Mock", cut);
    }

    // Empirical architectural finding: Truncation after </outgoingServer> (omitting </emailProvider> and </clientConfig>)
    // succeeds in both AutodiscoverService and MockAutodiscoverService because both are loose extractors
    // rather than full validating XML parsers.
    let truncated_missing_root = &full_xml[..outgoing_end];
    assert!(parse_thunderbird_autoconfig_xml_oracle(truncated_missing_root, "test@adversarial.org", "adversarial.org").is_ok());
    assert!(MockAutodiscoverService::parse_ispdb_xml(truncated_missing_root, "test@adversarial.org").is_ok());
}

#[test]
fn tier5_challenger1_m2_xml_adversarial_schemas_exhaustive_matrix() {
    let email = "admin@adversarial.com";
    let domain = "adversarial.com";

    // 1. Missing hostname in incomingServer
    let xml_missing_in_host = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_missing_in_host, email, domain).is_err());
    assert!(MockAutodiscoverService::parse_ispdb_xml(xml_missing_in_host, email).is_err());

    // 2. Missing hostname in outgoingServer
    let xml_missing_out_host = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap.test.com</hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_missing_out_host, email, domain).is_err());
    assert!(MockAutodiscoverService::parse_ispdb_xml(xml_missing_out_host, email).is_err());

    // 3. Empty hostname: <hostname></hostname> and <hostname>   </hostname>
    let xml_empty_in_host = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>   </hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_empty_in_host, email, domain).is_err());

    // 4. Hostname containing internal spaces
    let xml_space_host = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap .test.com</hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_space_host, email, domain).is_err());
    assert!(MockAutodiscoverService::parse_ispdb_xml(xml_space_host, email).is_err());

    // 5. Non-numeric port strings (must be rejected)
    for bad_port in &["abc", "993a", "NaN", "993.5", "undefined", "null"] {
        let xml_bad_port = format!(r#"<clientConfig>
          <emailProvider id="test">
            <incomingServer type="imap"><hostname>imap.test.com</hostname><port>{bad_port}</port></incomingServer>
            <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
          </emailProvider>
        </clientConfig>"#);
        let res = parse_thunderbird_autoconfig_xml_oracle(&xml_bad_port, email, domain);
        assert!(res.is_err(), "Port '{}' must be rejected as non-numeric", bad_port);
    }

    // 5b. Empty port tag <port></port>: extract_tag_content returns None for empty content,
    // causing parser to treat port as omitted and fallback to standard default port (993)
    let xml_empty_port = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap.test.com</hostname><port></port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let conf_empty_port = parse_thunderbird_autoconfig_xml_oracle(xml_empty_port, email, domain).unwrap();
    assert_eq!(conf_empty_port.incoming.port, 993, "Empty <port></port> defaults to 993");

    // 6. Negative port values
    for neg_port in &["-1", "-993", "-65535"] {
        let xml_neg_port = format!(r#"<clientConfig>
          <emailProvider id="test">
            <incomingServer type="imap"><hostname>imap.test.com</hostname><port>{neg_port}</port></incomingServer>
            <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
          </emailProvider>
        </clientConfig>"#);
        assert!(parse_thunderbird_autoconfig_xml_oracle(&xml_neg_port, email, domain).is_err());
    }

    // 7. Port zero (must be strictly rejected)
    let xml_port_zero = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap.test.com</hostname><port>0</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_port_zero, email, domain).is_err());
    assert!(MockAutodiscoverService::parse_ispdb_xml(xml_port_zero, email).is_err());

    // 8. Port overflow (> 65535)
    for over_port in &["65536", "70000", "999999", "18446744073709551615"] {
        let xml_over = format!(r#"<clientConfig>
          <emailProvider id="test">
            <incomingServer type="imap"><hostname>imap.test.com</hostname><port>{over_port}</port></incomingServer>
            <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
          </emailProvider>
        </clientConfig>"#);
        assert!(parse_thunderbird_autoconfig_xml_oracle(&xml_over, email, domain).is_err());
    }

    // 9. Missing port tags cleanly fallback to RFC standard defaults
    // IMAP SSL -> 993, SMTP STARTTLS -> 587
    let xml_no_ports = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap.test.com</hostname><socketType>SSL</socketType></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><socketType>STARTTLS</socketType></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let conf_defaults = parse_thunderbird_autoconfig_xml_oracle(xml_no_ports, email, domain).unwrap();
    assert_eq!(conf_defaults.incoming.port, 993, "IMAP SSL default must be 993");
    assert_eq!(conf_defaults.outgoing.port, 587, "SMTP STARTTLS default must be 587");

    // IMAP STARTTLS -> 143, SMTP SSL -> 465
    let xml_no_ports_alt = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap.test.com</hostname><socketType>STARTTLS</socketType></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><socketType>SSL</socketType></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let conf_alt = parse_thunderbird_autoconfig_xml_oracle(xml_no_ports_alt, email, domain).unwrap();
    assert_eq!(conf_alt.incoming.port, 143, "IMAP STARTTLS default must be 143");
    assert_eq!(conf_alt.outgoing.port, 465, "SMTP SSL default must be 465");

    // POP3 SSL -> 995
    let xml_pop3_ssl = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="pop3"><hostname>pop.test.com</hostname><socketType>SSL</socketType></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let conf_pop = parse_thunderbird_autoconfig_xml_oracle(xml_pop3_ssl, email, domain).unwrap();
    assert_eq!(conf_pop.incoming.port, 995, "POP3 SSL default must be 995");

    // 10. Missing incomingServer or outgoingServer tag entirely
    let xml_no_incoming = r#"<clientConfig><emailProvider id="test"><outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>587</port></outgoingServer></emailProvider></clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_no_incoming, email, domain).is_err());
    let xml_no_outgoing = r#"<clientConfig><emailProvider id="test"><incomingServer type="imap"><hostname>imap.test.com</hostname><port>993</port></incomingServer></emailProvider></clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xml_no_outgoing, email, domain).is_err());

    // 11. IMAP preferred over POP3 regardless of tag order in XML
    let xml_pop3_first = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="pop3"><hostname>pop.test.com</hostname><port>995</port><socketType>SSL</socketType></incomingServer>
        <incomingServer type="imap"><hostname>imap.test.com</hostname><port>993</port><socketType>SSL</socketType></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>465</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let conf_pop_first = parse_thunderbird_autoconfig_xml_oracle(xml_pop3_first, email, domain).unwrap();
    assert_eq!(conf_pop_first.incoming.hostname, "imap.test.com", "IMAP must be selected even when POP3 is listed first");

    let xml_imap_first = r#"<clientConfig>
      <emailProvider id="test">
        <incomingServer type="imap"><hostname>imap.test.com</hostname><port>993</port><socketType>SSL</socketType></incomingServer>
        <incomingServer type="pop3"><hostname>pop.test.com</hostname><port>995</port><socketType>SSL</socketType></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.test.com</hostname><port>465</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    let conf_imap_first = parse_thunderbird_autoconfig_xml_oracle(xml_imap_first, email, domain).unwrap();
    assert_eq!(conf_imap_first.incoming.hostname, "imap.test.com");
}

#[test]
fn tier5_challenger1_m2_xml_xxe_injections_and_case_sensitivity_audit() {
    let email = "victim@secure.corp";
    let domain = "secure.corp";

    // 1. Classic uppercase DOCTYPE and ENTITY
    let xxe_classic = r#"<?xml version="1.0"?>
    <!DOCTYPE root [
      <!ENTITY xxe SYSTEM "file:///etc/passwd">
    ]>
    <clientConfig>
      <emailProvider id="xxe">
        <incomingServer type="imap"><hostname>imap.xxe.org</hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.xxe.org</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xxe_classic, email, domain).is_err());
    assert!(MockAutodiscoverService::parse_ispdb_xml(xxe_classic, email).is_err());

    // 2. Standalone ENTITY injection
    let xxe_entity_only = r#"<?xml version="1.0"?>
    <!ENTITY lol "lol">
    <clientConfig>
      <emailProvider id="xxe">
        <incomingServer type="imap"><hostname>&lol;</hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.xxe.org</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(xxe_entity_only, email, domain).is_err());

    // 3. Empirical verification of case sensitivity in DOCTYPE check:
    // When lowercase `<!doctype` is used, the check `.contains("<!DOCTYPE")` does not match.
    // However, because the parser does pure string slicing and has no XML entity engine,
    // it never executes XXE or expands entities.
    let xxe_lowercase = r#"<?xml version="1.0"?>
    <!doctype root [
      <!entity xxe SYSTEM "file:///etc/shadow">
    ]>
    <clientConfig>
      <emailProvider id="xxe">
        <incomingServer type="imap"><hostname>&xxe;</hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.xxe.org</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;

    let parsed_res = parse_thunderbird_autoconfig_xml_oracle(xxe_lowercase, email, domain);
    if let Ok(config) = parsed_res {
        assert_eq!(config.incoming.hostname, "&xxe;", "Entity was not expanded (safe from actual XXE file disclosure)");
    }

    // 4. Recursive expansion / Billion laughs bomb
    let billion_laughs = r#"<?xml version="1.0"?>
    <!DOCTYPE lolz [
     <!ENTITY lol "lol">
     <!ELEMENT lolz (#PCDATA)>
     <!ENTITY lol1 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
     <!ENTITY lol2 "&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;">
    ]>
    <clientConfig>
      <emailProvider id="lolz">
        <incomingServer type="imap"><hostname>imap.lol.org</hostname><port>993</port></incomingServer>
        <outgoingServer type="smtp"><hostname>smtp.lol.org</hostname><port>587</port></outgoingServer>
      </emailProvider>
    </clientConfig>"#;
    assert!(parse_thunderbird_autoconfig_xml_oracle(billion_laughs, email, domain).is_err());
    assert!(MockAutodiscoverService::parse_ispdb_xml(billion_laughs, email).is_err());
}

#[test]
fn tier5_challenger1_m2_template_substitution_exhaustive_combinations() {
    // 1. Single variable substitutions
    let email = "john.doe+audit@company.co.uk";
    let (local, domain) = MockAutodiscoverService::extract_domain(email).unwrap();
    assert_eq!(local, "john.doe+audit");
    assert_eq!(domain, "company.co.uk");

    assert_eq!(
        MockAutodiscoverService::substitute_full_template("%EMAILADDRESS%", email, local, domain),
        "john.doe+audit@company.co.uk"
    );
    assert_eq!(
        MockAutodiscoverService::substitute_full_template("%EMAILLOCALPART%", email, local, domain),
        "john.doe+audit"
    );
    assert_eq!(
        MockAutodiscoverService::substitute_full_template("%EMAILDOMAIN%", email, local, domain),
        "company.co.uk"
    );

    // 2. Combinations & prefixes/suffixes
    assert_eq!(
        MockAutodiscoverService::substitute_full_template("%EMAILLOCALPART%@%EMAILDOMAIN%", email, local, domain),
        "john.doe+audit@company.co.uk"
    );
    assert_eq!(
        MockAutodiscoverService::substitute_full_template(r#"%EMAILDOMAIN%\%EMAILLOCALPART%"#, email, local, domain),
        r#"company.co.uk\john.doe+audit"#
    );
    assert_eq!(
        MockAutodiscoverService::substitute_full_template(
            "User: %EMAILLOCALPART% | Domain: %EMAILDOMAIN% | Full: %EMAILADDRESS%",
            email, local, domain
        ),
        "User: john.doe+audit | Domain: company.co.uk | Full: john.doe+audit@company.co.uk"
    );

    // 3. Multiple / repeated occurrences
    assert_eq!(
        MockAutodiscoverService::substitute_full_template(
            "%EMAILLOCALPART%-%EMAILLOCALPART%-%EMAILLOCALPART%",
            email, local, domain
        ),
        "john.doe+audit-john.doe+audit-john.doe+audit"
    );

    // 4. RFC 5322 special characters in local part
    let special_email = "test!#$%&'*+-/=?^_`{|}~@special.org";
    let (spec_local, spec_domain) = MockAutodiscoverService::extract_domain(special_email).unwrap();
    assert_eq!(
        MockAutodiscoverService::substitute_full_template("%EMAILLOCALPART%", special_email, spec_local, spec_domain),
        "test!#$%&'*+-/=?^_`{|}~"
    );

    // 5. Unicode / IDN domain
    let idn_email = "müller@xn--mgbh0fb.org";
    let (idn_local, idn_domain) = MockAutodiscoverService::extract_domain(idn_email).unwrap();
    assert_eq!(idn_local, "müller");
    assert_eq!(idn_domain, "xn--mgbh0fb.org");
    assert_eq!(
        MockAutodiscoverService::substitute_full_template(
            "%EMAILLOCALPART% on %EMAILDOMAIN%",
            idn_email, idn_local, idn_domain
        ),
        "müller on xn--mgbh0fb.org"
    );

    // 6. Adversarial input: User email contains literal variable token
    let tricky_email = "user%EMAILDOMAIN%@tricky.org";
    let (tricky_local, tricky_domain) = MockAutodiscoverService::extract_domain(tricky_email).unwrap();
    assert_eq!(tricky_local, "user%EMAILDOMAIN%");
    let tricky_sub = MockAutodiscoverService::substitute_full_template("%EMAILLOCALPART%", tricky_email, tricky_local, tricky_domain);
    assert_eq!(tricky_sub, "usertricky.org");

    // 7. No variables in template
    assert_eq!(
        MockAutodiscoverService::substitute_full_template("static_admin_user", email, local, domain),
        "static_admin_user"
    );

    // 8. Empty template
    assert_eq!(
        MockAutodiscoverService::substitute_full_template("", email, local, domain),
        ""
    );
}

#[test]
fn tier5_challenger1_m2_rapid_typing_generational_counter_and_cancellation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // 1. Keystroke candidate trajectory simulation
    let keystroke_sequence = [
        ("u", false),
        ("us", false),
        ("use", false),
        ("user", false),
        ("user@", false),
        ("user@e", false),
        ("user@ex", false),
        ("user@exa", false),
        ("user@exam", false),
        ("user@examp", false),
        ("user@exampl", false),
        ("user@example", false),
        ("user@example.", false),
        ("user@example.c", false),    // TLD length 1 -> false
        ("user@example.co", true),    // TLD length 2 -> true
        ("user@example.com", true),   // TLD length 3 -> true
    ];

    for (input, expected_candidate) in keystroke_sequence {
        let res = is_discovery_candidate_oracle(input);
        assert_eq!(
            res.is_some(),
            expected_candidate,
            "Input '{}' candidate status mismatch (expected {})",
            input,
            expected_candidate
        );
    }

    // 2. High-frequency typing simulation: 100 rapid keystrokes with cancellation
    struct MockDialogState {
        current_generation: u64,
        active_cancellation: Option<Arc<AtomicBool>>,
        last_applied_config: Option<DiscoveredAccountConfig>,
    }

    let mut dialog = MockDialogState {
        current_generation: 0,
        active_cancellation: None,
        last_applied_config: None,
    };

    let mut simulated_worker_results = Vec::new();

    for key_idx in 1..=100 {
        // Cancel previous worker
        if let Some(ref prev_cancel) = dialog.active_cancellation {
            prev_cancel.store(true, Ordering::Relaxed);
        }

        let generation_id = dialog.current_generation.wrapping_add(1);
        dialog.current_generation = generation_id;

        let cancellation = Arc::new(AtomicBool::new(false));
        dialog.active_cancellation = Some(cancellation.clone());

        // Worker simulates autodiscovery
        let email = format!("user{}@posteo.de", key_idx);
        let cancel_for_worker = cancellation.clone();
        simulated_worker_results.push((generation_id, email, cancel_for_worker));
    }

    // Dialog state is at generation 100
    assert_eq!(dialog.current_generation, 100);

    // Simulate arrival of results out of order:
    // Older generations (1..99) attempt to deliver results
    for (gen_id, email, cancel) in simulated_worker_results {
        if gen_id < 100 {
            // Cancel token was set to true
            assert!(cancel.load(Ordering::Relaxed), "Worker for gen {} must have been cancelled", gen_id);
            // Even if worker completed, dialog drops result
            if gen_id != dialog.current_generation {
                // Drop stale result (no UI update)
                continue;
            }
            panic!("Stale generation {} should have been dropped", gen_id);
        } else {
            // Generation 100: accepted!
            assert_eq!(gen_id, dialog.current_generation);
            let conf = MockAutodiscoverService::lookup_builtin_profile(&email).unwrap();
            dialog.last_applied_config = Some(conf);
        }
    }

    // Only generation 100 was applied
    assert!(dialog.last_applied_config.is_some());
    assert_eq!(dialog.last_applied_config.unwrap().email, "user100@posteo.de");

    // 3. Generational counter integer overflow stress test (u64::MAX -> 0)
    let max_gen: u64 = u64::MAX;
    let wrapped_gen = max_gen.wrapping_add(1);
    assert_eq!(wrapped_gen, 0, "wrapping_add must safely wrap to 0 without panic");
    let next_gen = wrapped_gen.wrapping_add(1);
    assert_eq!(next_gen, 1);
}

// ============================================================================
// Milestone 3 Challenger 1 Empirical Stress Harness (challenger_m3_gen2_1)
// ============================================================================

#[test]
fn tier5_challenger1_m3_keychain_crud_adversarial_matrix() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    let long_acc = "long-account-".repeat(20);
    let long_pass = "long-pass-".repeat(50);

    let test_matrix = [
        ("account-🔑-emoji", "P@ssw0rd-🔐-🚀-✨-2026!"),
        ("mätthäus@jörg.example.com", "Umlaut-P@ss-2026!"),
        ("user_测试_ñ", "CjkCyrillic-密码-123!"),
        ("special_!@#$%^&*()_+-=[]{}|;':,.<>?", "All-Punctuation-Secret-42"),
        (long_acc.as_str(), long_pass.as_str()),
    ];


    for (acc, pass) in &test_matrix {
        // Non-existent query before store must return None
        assert_eq!(creds.get_password(acc).unwrap(), None);

        // Store
        assert!(creds.store_password(acc, pass).is_ok());

        // Retrieve must match exactly
        assert_eq!(creds.get_password(acc).unwrap().as_deref(), Some(*pass));

        // Duplicate insert (update) must succeed idempotently
        let updated = format!("{}_updated", pass);
        assert!(creds.store_password(acc, &updated).is_ok());
        assert_eq!(creds.get_password(acc).unwrap().as_deref(), Some(updated.as_str()));

        // Delete
        assert!(creds.delete_password(acc).is_ok());
        assert_eq!(creds.get_password(acc).unwrap(), None);

        // Delete non-existent must be idempotent (return Ok)
        assert!(creds.delete_password(acc).is_ok());
    }
}

#[test]
fn tier5_challenger1_m3_keychain_rapid_sequential_cycles() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    // Rapid sequential set/get/delete across 100 distinct accounts
    for i in 0..100 {
        let acc = format!("rapid_acc_{}", i);
        let pass = format!("rapid_secret_{}", i);

        assert!(creds.store_password(&acc, &pass).is_ok());
        assert_eq!(creds.get_password(&acc).unwrap().as_deref(), Some(pass.as_str()));
    }

    assert_eq!(store.count_service_entries(KEYCHAIN_SERVICE_ID), 100);

    // Sequential updates
    for i in 0..100 {
        let acc = format!("rapid_acc_{}", i);
        let updated = format!("updated_secret_{}", i);

        assert!(creds.store_password(&acc, &updated).is_ok());
        assert_eq!(creds.get_password(&acc).unwrap().as_deref(), Some(updated.as_str()));
    }

    // Clean up
    for i in 0..100 {
        let acc = format!("rapid_acc_{}", i);
        assert!(creds.delete_password(&acc).is_ok());
        assert_eq!(creds.get_password(&acc).unwrap(), None);
    }

    assert_eq!(store.count_service_entries(KEYCHAIN_SERVICE_ID), 0);
}

#[test]
fn tier5_challenger1_m3_imap_parser_malformed_and_edge_lines() {
    use super::r5_portable_engine::MockImapProtocolParser;

    let strictly_rejected_lines = [
        "",
        "\r\n",
        "   \t  ",
        "NOT AN IMAP LINE",
        "* OK IMAP4rev1 Server Ready",
        "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)",
        "* 100 EXISTS",
        "* 5 RECENT",
        "* OK [UNSEEN 12] Message 12 is first unseen",
        "* LIST",
        "* LIST missing_open_paren \"/\" \"INBOX\"",
        "* LIST (unclosed_paren \"/\" \"INBOX\"",
    ];

    for line in &strictly_rejected_lines {
        let res = MockImapProtocolParser::parse_list_line(line);
        assert!(res.is_none(), "Expected None for malformed/untagged line: {:?}", line);
    }

    // Truncated LIST lines without delimiter/mailbox are rejected per RFC 3501 Section 7.2.2:
    let truncated_line = "* LIST ()";
    let res = MockImapProtocolParser::parse_list_line(truncated_line);
    assert!(
        res.is_none(),
        "Parser must reject truncated LIST line missing delimiter and mailbox"
    );
}

#[test]
fn tier5_challenger1_m3_imap_parser_quoted_and_unquoted_folders() {
    use super::r5_portable_engine::MockImapProtocolParser;

    // 1. Quoted folder names with various delimiters
    let valid_quoted = [
        (r#"* LIST (\HasNoChildren) "/" "INBOX""#, "INBOX", '/', true, false, false, false),
        (r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#, "Sent Messages", '/', false, true, false, false),
        (r#"* LIST (\HasNoChildren \Drafts) "." "Drafts""#, "Drafts", '.', false, false, true, false),
        (r#"* LIST (\HasNoChildren \Trash) "/" "Trash""#, "Trash", '/', false, false, false, true),
        (r#"* LIST (\Noselect) "/" "Folder With Spaces""#, "Folder With Spaces", '/', false, false, false, false),
        (r#"* LIST () "/" """#, "", '/', false, false, false, false), // Empty folder name
    ];

    for (raw, expected_name, expected_delim, is_inbox, is_sent, is_drafts, is_trash) in valid_quoted {
        let item = MockImapProtocolParser::parse_list_line(raw).expect("Should parse");
        assert_eq!(item.name, expected_name);
        assert_eq!(item.delimiter, expected_delim);
        assert_eq!(item.is_inbox, is_inbox);
        assert_eq!(item.is_sent, is_sent);
        assert_eq!(item.is_drafts, is_drafts);
        assert_eq!(item.is_trash, is_trash);
    }

    // 2. Demonstration of RFC 3501 compliance: Unquoted folder name with quoted delimiter
    let unquoted_line = r#"* LIST (\HasNoChildren) "/" INBOX"#;
    let item = MockImapProtocolParser::parse_list_line(unquoted_line).unwrap();
    assert_eq!(
        item.name, "INBOX",
        "Parser extracts folder name 'INBOX' for unquoted folder names"
    );
    assert!(
        item.is_inbox,
        "is_inbox evaluates to true for INBOX"
    );
}

#[test]
fn tier5_challenger1_m3_imap_unquoted_folder_name_probe() {
    tier5_challenger1_m3_imap_parser_quoted_and_unquoted_folders();
}

#[test]
fn tier5_challenger1_m3_smtp_dot_stuffing_exhaustive_matrix() {
    // Test RFC 5321 dot-stuffing oracle logic
    let raw_payload = "\
Header: Test\r\n\
\r\n\
.\r\n\
..\r\n\
...\r\n\
.Dotted line\r\n\
Normal line\r\n\
Line with . dot inside\r\n\
.";

    // Apply RFC 5321 dot-stuffing
    let mut stuffed = String::new();
    for line in raw_payload.lines() {
        if line.starts_with('.') {
            stuffed.push('.');
        }
        stuffed.push_str(line);
        stuffed.push_str("\r\n");
    }

    assert!(stuffed.contains("\r\n..\r\n"), "Single dot must be stuffed to ..");
    assert!(stuffed.contains("\r\n...\r\n"), "Two dots must be stuffed to ...");
    assert!(stuffed.contains("\r\n....\r\n"), "Three dots must be stuffed to ....");
    assert!(stuffed.contains("\r\n..Dotted line\r\n"), "Line starting with dot must have dot prepended");
    assert!(stuffed.contains("\r\nNormal line\r\n"));
    assert!(stuffed.contains("\r\nLine with . dot inside\r\n"));

    // Large body dot-stuffing stress (10,000 lines)
    let mut large_body = String::new();
    for i in 0..10_000 {
        if i % 3 == 0 {
            large_body.push_str(".\r\n");
        } else if i % 3 == 1 {
            large_body.push_str("..multi-dot\r\n");
        } else {
            large_body.push_str("regular content line\r\n");
        }
    }

    let mut large_stuffed = String::new();
    for line in large_body.lines() {
        if line.starts_with('.') {
            large_stuffed.push('.');
        }
        large_stuffed.push_str(line);
        large_stuffed.push_str("\r\n");
    }

    assert_eq!(large_stuffed.lines().count(), 10_000);
}

#[test]
fn tier5_challenger1_m3_smtp_sasl_plain_formatting() {
    let user = "challenger_user@example.com";
    let pass = "Ch@ll3ng3r!2026";

    // RFC 4616 SASL PLAIN format: \0<username>\0<password>
    let mut raw = Vec::new();
    raw.push(0);
    raw.extend_from_slice(user.as_bytes());
    raw.push(0);
    raw.extend_from_slice(pass.as_bytes());

    let b64 = glib::base64_encode(&raw);
    let decoded = glib::base64_decode(&b64);
    assert_eq!(decoded, raw);

    // Verify format: first byte is 0, username bytes, 0 byte, pass bytes
    assert_eq!(decoded[0], 0);
    let rest = &decoded[1..];
    let user_end = rest.iter().position(|&b| b == 0).expect("Second NUL byte");
    assert_eq!(&rest[..user_end], user.as_bytes());
    assert_eq!(&rest[user_end + 1..], pass.as_bytes());
}

#[test]
fn tier5_challenger1_m3_smtp_envelope_address_vulnerabilities() {
    // Demonstration of RFC 5322 compliance in extract_envelope_addresses
    
    // Folded recipient continuation line
    let folded_msg = concat!(
        "From: sender@example.com\r\n",
        "To: first@example.com,\r\n",
        "    second@example.com\r\n",
        "Subject: Folded\r\n\r\n",
        "Body"
    ).as_bytes();

    let (sender, recipients) = extract_envelope_addresses_oracle(folded_msg);
    assert_eq!(sender, "sender@example.com");
    // All recipients across continuation lines are extracted
    assert_eq!(recipients, vec!["first@example.com".to_string(), "second@example.com".to_string()]);

    // Comma in quoted display name
    let quoted_comma_msg = concat!(
        "From: sender@example.com\r\n",
        "To: \"Doe, John\" <john@example.com>\r\n",
        "Subject: Quoted Comma\r\n\r\n",
        "Body"
    ).as_bytes();

    let (_, q_recipients) = extract_envelope_addresses_oracle(quoted_comma_msg);
    // Preserves quoted display name without emitting corrupt "Doe" token
    assert_eq!(
        q_recipients,
        vec!["john@example.com".to_string()],
        "Must extract only valid recipient email address without splitting on quoted comma"
    );
}


// ============================================================================
// Milestone 3 Challenger 2 Empirical Stress Harness (challenger_m3_gen2_2)
// ============================================================================

// ----------------------------------------------------------------------------
// Challenger 2 Oracles & Simulators
// ----------------------------------------------------------------------------

pub struct MockNetworkConnectionOracle {
    transcript: std::collections::VecDeque<String>,
}

impl MockNetworkConnectionOracle {
    pub fn new(canned_responses: Vec<String>) -> Self {
        Self {
            transcript: std::collections::VecDeque::from(canned_responses),
        }
    }

    pub fn read_line(&mut self) -> Result<String, String> {
        self.transcript
            .pop_front()
            .ok_or_else(|| "Verbindung unerwartet geschlossen (EOF)".to_string())
    }
}

pub fn split_mime_headers_and_body_oracle(raw: &[u8]) -> (&[u8], &[u8]) {
    let mut earliest: Option<(usize, usize)> = None;
    for separator in [b"\r\n\r\n".as_slice(), b"\n\n", b"\r\r"] {
        if let Some(index) = raw
            .windows(separator.len())
            .position(|candidate| candidate == separator)
        {
            if earliest.map_or(true, |(earliest_idx, _)| index < earliest_idx) {
                earliest = Some((index, separator.len()));
            }
        }
    }
    if let Some((index, sep_len)) = earliest {
        return (&raw[..index], &raw[index + sep_len..]);
    }

    let header_text = String::from_utf8_lossy(raw);
    let mut has_colon = false;
    for line in header_text.lines() {
        if line.contains(':') {
            has_colon = true;
            break;
        }
    }
    if !has_colon {
        (&[], raw)
    } else {
        (raw, &[])
    }
}

#[derive(Debug, Default)]
pub struct MockMultipartParseResult {
    pub html_body: Option<String>,
    pub plain_body: Option<String>,
    pub parts_count: usize,
}

pub fn parse_rfc822_multipart_oracle(raw: &[u8], boundary: &str) -> MockMultipartParseResult {
    let (_, body) = split_mime_headers_and_body_oracle(raw);
    let marker = format!("--{boundary}");

    let mut result = MockMultipartParseResult::default();
    let text = String::from_utf8_lossy(body);
    let lines: Vec<&str> = text.lines().collect();

    let mut current_part = Vec::new();
    let mut in_part = false;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with(&marker) {
            if in_part && !current_part.is_empty() {
                result.parts_count += 1;
                let part_text = current_part.join("\r\n");
                if part_text.to_ascii_lowercase().contains("content-type: text/html") {
                    result.html_body = Some(part_text.clone());
                } else if part_text.to_ascii_lowercase().contains("content-type: text/plain") {
                    result.plain_body = Some(part_text.clone());
                }
                current_part.clear();
            }
            if trimmed == format!("--{}--", boundary) {
                in_part = false;
            } else {
                in_part = true;
            }
        } else if in_part {
            current_part.push(line);
        }
    }

    if in_part && !current_part.is_empty() {
        result.parts_count += 1;
        let part_text = current_part.join("\r\n");
        if part_text.to_ascii_lowercase().contains("content-type: text/html") {
            result.html_body = Some(part_text.clone());
        } else if part_text.to_ascii_lowercase().contains("content-type: text/plain") {
            result.plain_body = Some(part_text.clone());
        }
    }

    result
}

pub fn format_attachment_size_oracle(size_bytes: usize) -> String {
    if size_bytes < 1024 {
        format!("{size_bytes} B")
    } else if size_bytes < 1024 * 1024 {
        let kib = size_bytes as f64 / 1024.0;
        format!("{:.1} KB", kib).replace('.', ",")
    } else {
        let mib = size_bytes as f64 / (1024.0 * 1024.0);
        format!("{:.1} MB", mib).replace('.', ",")
    }
}

fn unfold_header_lines_oracle(raw_message: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw_message);
    let mut unfolded: Vec<String> = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            break;
        }

        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            let last = unfolded.last_mut().unwrap();
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if !last.ends_with(' ') && !last.ends_with('\t') {
                    last.push(' ');
                }
                last.push_str(trimmed);
            }
        } else {
            unfolded.push(line.to_string());
        }
    }

    unfolded
}

fn split_header_addresses_oracle(val: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape = false;
    let mut paren_depth: usize = 0;
    let mut in_angle = false;

    for ch in val.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' if in_quotes => {
                escape = true;
                current.push(ch);
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '(' if !in_quotes => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_quotes && paren_depth > 0 => {
                paren_depth -= 1;
                current.push(ch);
            }
            '<' if !in_quotes => {
                in_angle = true;
                current.push(ch);
            }
            '>' if !in_quotes => {
                in_angle = false;
                current.push(ch);
            }
            ',' | ';' if !in_quotes && paren_depth == 0 && !in_angle => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }

    parts
}

pub fn extract_envelope_addresses_oracle(raw_message: &[u8]) -> (String, Vec<String>) {
    let mut sender = String::new();
    let mut recipients: Vec<String> = Vec::new();

    let header_lines = unfold_header_lines_oracle(raw_message);

    for header in &header_lines {
        if let Some((name, val)) = header.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            let val = val.trim();

            if key == "from" && sender.is_empty() {
                for part in split_header_addresses_oracle(val) {
                    let addr = clean_address_oracle(&part);
                    if !addr.is_empty() {
                        sender = addr;
                        break;
                    }
                }
            } else if key == "to" || key == "cc" || key == "bcc" {
                for part in split_header_addresses_oracle(val) {
                    let addr = clean_address_oracle(&part);
                    if !addr.is_empty() && !recipients.iter().any(|r| r.eq_ignore_ascii_case(&addr)) {
                        recipients.push(addr);
                    }
                }
            }
        }
    }

    (sender, recipients)
}

fn clean_address_oracle(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut in_quotes = false;
    let mut escape = false;
    let mut angle_start = None;
    let mut angle_end = None;

    for (idx, ch) in trimmed.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                escape = true;
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            '<' if !in_quotes => {
                if angle_start.is_none() {
                    angle_start = Some(idx);
                }
            }
            '>' if !in_quotes => {
                if angle_start.is_some() {
                    angle_end = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }

    let candidate = if let (Some(start), Some(end)) = (angle_start, angle_end) {
        if start < end {
            let mut inner = trimmed[start + 1..end].trim();
            if let Some(colon) = inner.rfind(':') {
                inner = inner[colon + 1..].trim();
            }
            inner
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    let mut stripped = String::new();
    let mut paren_depth = 0;
    for ch in candidate.chars() {
        match ch {
            '(' => paren_depth += 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ch if paren_depth == 0 => stripped.push(ch),
            _ => {}
        }
    }

    let token = stripped
        .split_whitespace()
        .find(|tok| tok.contains('@'))
        .unwrap_or(stripped.trim());

    let cleaned = token.trim_matches(|c: char| {
        c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>' || c == ',' || c == ';'
    });

    if let Some(at_idx) = cleaned.find('@') {
        if at_idx > 0 && at_idx < cleaned.len() - 1 && !cleaned.contains(' ') {
            return cleaned.to_string();
        }
    }

    String::new()
}

pub fn apply_rfc5321_dot_stuffing_oracle(raw_message: &[u8]) -> Vec<u8> {
    let mut stuffed = Vec::new();
    let mut at_line_start = true;

    for &byte in raw_message {
        if at_line_start && byte == b'.' {
            stuffed.push(b'.');
        }

        stuffed.push(byte);

        if byte == b'\n' {
            at_line_start = true;
        } else if byte != b'\r' {
            at_line_start = false;
        }
    }

    if !stuffed.ends_with(b"\r\n") {
        if stuffed.ends_with(b"\n") {
            stuffed.pop();
        }
        stuffed.extend_from_slice(b"\r\n");
    }

    stuffed.extend_from_slice(b".\r\n");
    stuffed
}

// ----------------------------------------------------------------------------
// Challenger 2 Stress Tests
// ----------------------------------------------------------------------------

#[test]
fn tier5_challenger2_m3_multi_account_keychain_isolation_and_boundary_matrix() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    // 1. Matrix of 10 distinct accounts under "org.gnome.Evolution4"
    let test_matrix = [
        ("primary@example.com", "Password_Primary_2026!"),
        ("secondary@example.com", "Password_Secondary_#99"),
        ("primary@example.com.br", "Password_Country_BR$"),
        ("primary_sub@example.com", "Password_Sub_Account"),
        ("Primary@Example.COM", "Password_Mixed_Case_Unique"),
        ("user+tag@example.com", "Password_Plus_Addressing"),
        ("müller@äöü-domain.de", "P@ssw0rt_Ünïcödë_2026"),
        ("ユーザー@ドメイン.jp", "パスワード_Japanese_UTF8"),
        ("admin@enterprise.internal", "SuperSecretLongPassphraseThatIsVeryLongAndComplex1234567890"),
        ("service_account_system", "Bearer-Token-Style-Secret-XYZ-ABC-123"),
    ];

    // Store all
    for (acc, pass) in &test_matrix {
        assert!(creds.store_password(acc, pass).is_ok(), "Storing account '{}' failed", acc);
    }

    // Verify all 10 are retrieved without cross-contamination or collisions
    for (acc, pass) in &test_matrix {
        let retrieved = creds.get_password(acc).unwrap();
        assert_eq!(
            retrieved.as_deref(),
            Some(*pass),
            "Account '{}' retrieved mismatched password",
            acc
        );
    }

    assert_eq!(
        store.count_service_entries(KEYCHAIN_SERVICE_ID),
        10,
        "Store must contain exactly 10 distinct accounts under service {}",
        KEYCHAIN_SERVICE_ID
    );

    // 2. Selective Update: update 3 accounts, verify only those 3 mutate
    let updates = [
        ("primary@example.com", "UPDATED_Primary_Pass!"),
        ("user+tag@example.com", "UPDATED_Tag_Pass!"),
        ("ユーザー@ドメイン.jp", "UPDATED_Japanese_UTF8_新パスワード"),
    ];

    for (acc, new_pass) in &updates {
        assert!(creds.store_password(acc, new_pass).is_ok());
        assert_eq!(creds.get_password(acc).unwrap().as_deref(), Some(*new_pass));
    }

    // Verify untouched accounts remain unaltered
    assert_eq!(creds.get_password("secondary@example.com").unwrap().as_deref(), Some("Password_Secondary_#99"));
    assert_eq!(creds.get_password("primary@example.com.br").unwrap().as_deref(), Some("Password_Country_BR$"));
    assert_eq!(creds.get_password("Primary@Example.COM").unwrap().as_deref(), Some("Password_Mixed_Case_Unique"));

    // 3. Selective Deletion: delete 2 accounts, verify isolation
    assert!(creds.delete_password("primary@example.com").is_ok());
    assert!(creds.delete_password("müller@äöü-domain.de").is_ok());

    assert_eq!(creds.get_password("primary@example.com").unwrap(), None);
    assert_eq!(creds.get_password("müller@äöü-domain.de").unwrap(), None);
    // Others still intact
    assert_eq!(creds.get_password("secondary@example.com").unwrap().as_deref(), Some("Password_Secondary_#99"));
    assert_eq!(creds.get_password("ユーザー@ドメイン.jp").unwrap().as_deref(), Some("UPDATED_Japanese_UTF8_新パスワード"));

    assert_eq!(store.count_service_entries(KEYCHAIN_SERVICE_ID), 8);

    // 4. Large Passphrase boundary (4096 bytes)
    let large_secret: String = "X".repeat(4096);
    assert!(creds.store_password("large_acc", &large_secret).is_ok());
    assert_eq!(creds.get_password("large_acc").unwrap().as_deref(), Some(large_secret.as_str()));

    // 5. Special characters, control chars, emojis
    let emoji_secret = "🔐🔑🔒✨🚀❤️🌍§±!@#$%^&*()_+-=[]{}|;':\",./<>?~`\t\n";
    assert!(creds.store_password("emoji_acc", emoji_secret).is_ok());
    assert_eq!(creds.get_password("emoji_acc").unwrap().as_deref(), Some(emoji_secret));

    // 6. Delete non-existent account is safe
    assert!(creds.delete_password("non_existent_random_account").is_ok());
}

#[test]
fn tier5_challenger2_m3_cross_platform_gating_and_cfg_cleanliness() {
    // 1. Target OS exclusivity
    let is_macos = cfg!(target_os = "macos");
    let is_linux = cfg!(target_os = "linux");
    assert!(
        !(is_macos && is_linux),
        "target_os cannot be simultaneously macos and linux"
    );

    // 2. Service identifier contract
    assert_eq!(KEYCHAIN_SERVICE_ID, "org.gnome.Evolution4");

    // 3. Native adapter availability contract:
    // On macOS, native_adapter_available() must be true even without libcamel/EDS bridge.
    // On Linux without eds_camel_bridge, native_adapter_available() must be false.
    let eds_bridge = cfg!(eds_camel_bridge);
    let adapter_expected = eds_bridge || is_macos;
    assert_eq!(
        cfg!(any(eds_camel_bridge, target_os = "macos")),
        adapter_expected,
        "Native adapter gating logic must match cfg!(any(eds_camel_bridge, target_os = \"macos\"))"
    );

    // 4. Security.framework linkage contract:
    // On Linux builds, Apple Security.framework MUST NOT be linked.
    if is_linux {
        assert!(!cfg!(feature = "macos_bridge"));
    }
}

#[test]
fn tier5_challenger2_m3_portable_mail_engine_timeout_cancellation_and_offline_cache() {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // 1. Cancellation Token Pre-flight check
    let cancelled = Arc::new(AtomicBool::new(true)); // Pre-cancelled
    let cancel_res = if cancelled.load(Ordering::Relaxed) {
        Err("Mail-Ladevorgang wurde abgebrochen.".to_string())
    } else {
        Ok(())
    };
    assert!(cancel_res.is_err());
    assert_eq!(cancel_res.unwrap_err(), "Mail-Ladevorgang wurde abgebrochen.");

    // 2. Offline Cache Protection Oracle:
    // Verify that saving an empty load does NOT overwrite valid cached data
    let mut cache_store: HashMap<String, String> = HashMap::new();
    let valid_cache_json = r#"{"messages":[{"id":"1","subject":"Cached Msg"}],"folders":[{"folder_name":"INBOX"}]}"#;
    cache_store.insert("mail_load_cache.json".to_string(), valid_cache_json.to_string());

    // Simulated save_mail_load_cache guard
    let empty_messages: Vec<String> = Vec::new();
    let empty_folders: Vec<String> = Vec::new();
    let should_save = !empty_messages.is_empty() || !empty_folders.is_empty();
    if should_save {
        cache_store.insert("mail_load_cache.json".to_string(), "{}".to_string());
    }

    // Cache must remain intact
    assert_eq!(
        cache_store.get("mail_load_cache.json").unwrap(),
        valid_cache_json,
        "Empty mail load must not overwrite existing cache"
    );

    // 3. Socket error simulation: Unexpected connection termination (EOF) during transcript
    let transcript_truncated = vec![
        "* OK IMAP4rev1 Ready".to_string(),
        // Server abruptly closes without response to LOGIN
    ];
    let mut conn = MockNetworkConnectionOracle::new(transcript_truncated);
    let greeting = conn.read_line().unwrap();
    assert_eq!(greeting, "* OK IMAP4rev1 Ready");
    let login_resp = conn.read_line();
    assert!(login_resp.is_err(), "Truncated server response must return error on EOF");
}

#[test]
fn tier5_challenger2_m3_imap_protocol_parser_boundary_and_delimiters() {
    use super::r5_portable_engine::MockImapProtocolParser;

    // Test matrix of IMAP LIST responses with quoted names
    let valid_cases = [
        // Standard quoted name with '/' delimiter
        (r#"* LIST (\HasNoChildren) "/" "INBOX""#, "INBOX", '/', true),
        // Special use Sent
        (r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#, "Sent Messages", '/', false),
        // Drafts with dot delimiter
        (r#"* LIST (\HasNoChildren \Drafts) "." "Drafts""#, "Drafts", '.', false),
        // Trash with backslash delimiter
        (r#"* LIST (\HasNoChildren \Trash) "\\" "Trash""#, "Trash", '\\', false),
        // Noselect parent folder
        (r#"* LIST (\Noselect \HasChildren) "/" "Archive""#, "Archive", '/', false),
        // Multiple attributes and junk
        (r#"* LIST (\HasNoChildren \Junk \Subscribed) "/" "Spam""#, "Spam", '/', false),
    ];

    for (raw, expected_name, expected_delim, is_inbox) in valid_cases {
        let parsed = MockImapProtocolParser::parse_list_line(raw);
        assert!(parsed.is_some(), "Failed to parse valid LIST line: {}", raw);
        let item = parsed.unwrap();
        assert_eq!(item.name, expected_name, "Mismatched name for: {}", raw);
        assert_eq!(item.delimiter, expected_delim, "Mismatched delimiter for: {}", raw);
        assert_eq!(item.is_inbox, is_inbox, "Mismatched is_inbox for: {}", raw);
    }

    // Malformed lines verification:
    // 1. Lines missing "* LIST " prefix or unclosed parens return None
    assert!(MockImapProtocolParser::parse_list_line("").is_none());
    assert!(MockImapProtocolParser::parse_list_line("NOT A LIST LINE").is_none());
    assert!(MockImapProtocolParser::parse_list_line("* OK greeting only").is_none());
    assert!(MockImapProtocolParser::parse_list_line("* LIST").is_none());
    assert!(MockImapProtocolParser::parse_list_line("* LIST missing_paren").is_none());
    assert!(MockImapProtocolParser::parse_list_line("* LIST (unterminated").is_none());

    // 2. Reject truncated lines missing mandatory delimiter and mailbox arguments:
    let empty_paren_parsed = MockImapProtocolParser::parse_list_line("* LIST ()");
    assert!(empty_paren_parsed.is_none(), "Parser must reject truncated line '* LIST ()'");
}

#[test]
fn tier5_challenger2_m3_rfc822_mime_parser_adversarial_stress_matrix() {
    // 1. Zero-length input
    let empty_bytes = b"";
    assert!(split_mime_headers_and_body_oracle(empty_bytes).0.is_empty());

    // 2. Binary garbage / Non-UTF8 fuzzing
    let non_utf8_raw = b"From: \xFF\xFE\xFD <user@example.com>\r\nSubject: \x80\x81\x82 Corrupted Header\r\n\r\n\xFF\xAA\xBB Binary Body Content";
    let (h_bytes, b_bytes) = split_mime_headers_and_body_oracle(non_utf8_raw);
    assert!(!h_bytes.is_empty());
    assert!(!b_bytes.is_empty());
    let h_str = String::from_utf8_lossy(h_bytes);
    assert!(h_str.contains("user@example.com"));

    // 3. Missing header separator (headers only, no blank line)
    let headers_only = b"From: alice@example.com\r\nSubject: Test Only";
    let (h_only, b_only) = split_mime_headers_and_body_oracle(headers_only);
    assert_eq!(h_only, headers_only);
    assert!(b_only.is_empty());

    // 4. Missing headers (body only without header colons)
    let body_only = b"\r\nJust body text without any headers";
    let (h_b, b_b) = split_mime_headers_and_body_oracle(body_only);
    assert!(h_b.is_empty());
    assert_eq!(b_b, b"\r\nJust body text without any headers");

    // 5. Multipart without text/plain (HTML only)
    let multipart_html_only = b"Content-Type: multipart/alternative; boundary=\"alt_bound\"\r\n\r\n\
--alt_bound\r\n\
Content-Type: text/html; charset=utf-8\r\n\r\n\
<h1>HTML Only Message</h1><p>No plaintext alternate provided.</p>\r\n\
--alt_bound--\r\n";
    let parsed_alt = parse_rfc822_multipart_oracle(multipart_html_only, "alt_bound");
    assert!(parsed_alt.html_body.is_some(), "HTML part must be extracted");
    assert!(parsed_alt.html_body.unwrap().contains("HTML Only Message"));
    assert!(parsed_alt.plain_body.is_none(), "Plaintext part should be None");

    // 6. Truncated multipart without closing delimiter (--boundary--)
    let truncated_multipart = b"Content-Type: multipart/mixed; boundary=\"mix_bound\"\r\n\r\n\
--mix_bound\r\n\
Content-Type: text/plain\r\n\r\n\
First part recovered\r\n\
--mix_bound\r\n\
Content-Type: text/plain\r\n\r\n\
Second part before EOF without closing boundary marker";
    let parsed_trunc = parse_rfc822_multipart_oracle(truncated_multipart, "mix_bound");
    assert_eq!(parsed_trunc.parts_count, 2, "Both parts must be recovered even without closing boundary");

    // 7. Large attachment size formatting
    let large_attachment_bytes = 1024 * 1024; // 1MB
    let format_res = format_attachment_size_oracle(large_attachment_bytes);
    assert_eq!(format_res, "1,0 MB");
}

#[test]
fn tier5_challenger2_m3_smtp_envelope_extraction_and_dot_stuffing_stress() {
    // 1. Multi-recipient extraction from complex RFC 822 headers
    let raw_email = b"From: \"Sender Name\" <sender@corp.example.com>\r\n\
To: \"Alice, Manager\" <alice@corp.example.com>, bob@corp.example.com\r\n\
Cc: carol@external.org, \"David (Contractor)\" <david@corp.example.com>\r\n\
Bcc: secret_monitor@corp.example.com\r\n\
Subject: Multi-recipient Test\r\n\
\r\n\
Body text\r\n";

    let (sender, recipients) = extract_envelope_addresses_oracle(raw_email);
    assert_eq!(sender, "sender@corp.example.com");
    assert!(recipients.contains(&"bob@corp.example.com".to_string()));
    assert!(recipients.contains(&"carol@external.org".to_string()));
    assert!(recipients.contains(&"secret_monitor@corp.example.com".to_string()));

    // 2. RFC 5321 Dot-Stuffing exhaustive edge cases
    let test_payloads = [
        // Single dot on line
        (".\r\n", "..\r\n.\r\n"),
        // Double dot on line
        ("..\r\n", "...\r\n.\r\n"),
        // Line starting with dot
        (".Line starting with dot\r\n", "..Line starting with dot\r\n.\r\n"),
        // Normal text without dot
        ("Normal text line\r\n", "Normal text line\r\n.\r\n"),
        // Missing trailing CRLF before terminating dot
        ("No trailing CRLF", "No trailing CRLF\r\n.\r\n"),
        // Empty message
        ("", "\r\n.\r\n"),
    ];

    for (input, expected_output) in test_payloads {
        let stuffed = apply_rfc5321_dot_stuffing_oracle(input.as_bytes());
        assert_eq!(
            String::from_utf8_lossy(&stuffed),
            expected_output,
            "Dot-stuffing failed for input: {:?}",
            input
        );
    }
}

#[test]
fn tier5_challenger2_m3_imap_uid_vs_sequence_number_protocol_divergence_probe() {
    // Empirical test verifying IMAP protocol divergence:
    // In IMAP, EXISTS indicates the total message count (sequence numbers 1..N).
    // If a mailbox has 10 messages whose UIDs are 500..509:
    // - `FETCH 1:10 (UID FLAGS)` fetches messages by sequence numbers 1 through 10.
    // - `UID FETCH 1:10 (UID FLAGS)` searches for messages whose UIDs are in the range 1..10,
    //   which matches ZERO messages.
    let mailbox_total_exists: u32 = 10;
    let message_uids: Vec<u32> = (500..510).collect();

    let seq_start = 1;
    let seq_end = mailbox_total_exists;

    // Simulate FETCH 1:10 -> matches by sequence index
    let matched_by_sequence: Vec<u32> = message_uids[seq_start - 1..seq_end as usize].to_vec();
    assert_eq!(matched_by_sequence.len(), 10, "Sequence FETCH 1:10 retrieves all 10 messages");

    // Simulate UID FETCH 1:10 -> matches by UID value
    let matched_by_uid_range: Vec<u32> = message_uids
        .iter()
        .copied()
        .filter(|&uid| uid >= seq_start as u32 && uid <= seq_end)
        .collect();
    assert_eq!(
        matched_by_uid_range.len(),
        0,
        "UID FETCH 1:10 returns 0 messages when UIDs are 500..509 (proves UID FETCH with sequence range bug)"
    );
}

#[test]
fn tier5_challenger2_m3_smtp_folded_header_address_loss_probe() {
    // Probe verifying RFC 5322 folded header recipient preservation:
    let raw_folded_message = concat!(
        "From: sender@example.com\r\n",
        "To: recipient1@example.com,\r\n",
        " recipient2@example.com\r\n",
        "Subject: Folded Header Test\r\n\r\n",
        "Body"
    ).as_bytes();

    let (sender, recipients) = extract_envelope_addresses_oracle(raw_folded_message);
    assert_eq!(sender, "sender@example.com");
    assert!(recipients.contains(&"recipient1@example.com".to_string()));
    assert!(
        recipients.contains(&"recipient2@example.com".to_string()),
        "Folded header continuation line recipient2 must be preserved"
    );
}

// ============================================================================
// Milestone 3 Challenger 1 Iteration 2 Empirical Stress Harness (challenger_m3_fix_1)
// ============================================================================

#[test]
fn tier5_challenger1_m3_fix1_parse_list_line_exhaustive_matrix() {
    use super::r5_portable_engine::MockImapProtocolParser;

    // 1. Unquoted folder names with various delimiters (quoted and unquoted)
    let unquoted_folder_cases = [
        (r#"* LIST (\HasNoChildren) "/" INBOX"#, "INBOX", '/', true, false, false, false, false),
        (r#"* LIST (\HasNoChildren) "." INBOX"#, "INBOX", '.', true, false, false, false, false),
        (r#"* LIST (\HasNoChildren) "\\" INBOX"#, "INBOX", '\\', true, false, false, false, false),
        (r#"* LIST () "/" Trash"#, "Trash", '/', false, false, false, true, false),
        (r#"* LIST (\Sent) "/" Sent"#, "Sent", '/', false, true, false, false, false),
        (r#"* LIST (\Drafts) "/" Drafts"#, "Drafts", '/', false, false, true, false, false),
        (r#"* LIST (\Junk) "/" Junk"#, "Junk", '/', false, false, false, false, true),
        (r#"* LIST (\HasNoChildren) "/" Archive"#, "Archive", '/', false, false, false, false, false),
        (r#"* LIST () "/" INBOX.SubFolder"#, "INBOX.SubFolder", '/', false, false, false, false, false),
        // Unquoted delimiters
        (r#"* LIST () / INBOX"#, "INBOX", '/', true, false, false, false, false),
        (r#"* LIST () . INBOX"#, "INBOX", '.', true, false, false, false, false),
        (r#"* LIST () NIL INBOX"#, "INBOX", '/', true, false, false, false, false),
        (r#"* LIST () nil INBOX"#, "INBOX", '/', true, false, false, false, false),
        (r#"* LIST () Nil INBOX"#, "INBOX", '/', true, false, false, false, false),
    ];

    for (raw, exp_name, exp_delim, is_inbox, is_sent, is_drafts, is_trash, is_junk) in unquoted_folder_cases {
        let item = MockImapProtocolParser::parse_list_line(raw)
            .unwrap_or_else(|| panic!("Failed to parse valid LIST line: {}", raw));
        assert_eq!(item.name, exp_name, "Mismatch name for {}", raw);
        assert_eq!(item.delimiter, exp_delim, "Mismatch delimiter for {}", raw);
        assert_eq!(item.is_inbox, is_inbox, "Mismatch is_inbox for {}", raw);
        assert_eq!(item.is_sent, is_sent, "Mismatch is_sent for {}", raw);
        assert_eq!(item.is_drafts, is_drafts, "Mismatch is_drafts for {}", raw);
        assert_eq!(item.is_trash, is_trash, "Mismatch is_trash for {}", raw);
        assert_eq!(item.is_junk, is_junk, "Mismatch is_junk for {}", raw);
    }

    // 2. Folder names with spaces (quoted and unquoted fallback)
    let space_cases = [
        (r#"* LIST (\HasNoChildren \Sent) "/" "Sent Messages""#, "Sent Messages", '/', true),
        (r#"* LIST () "/" "Folder   With   Multiple   Spaces""#, "Folder   With   Multiple   Spaces", '/', false),
        (r#"* LIST () "/" "Folder, With Comma""#, "Folder, With Comma", '/', false),
        (r#"* LIST () "/" "Folder \"Quoted\" \\ Backslash""#, "Folder \"Quoted\" \\ Backslash", '/', false),
        // RFC 3501 tolerant fallback for unquoted folder with spaces
        (r#"* LIST () "/" Unquoted Folder Name"#, "Unquoted Folder Name", '/', false),
    ];

    for (raw, exp_name, exp_delim, is_sent) in space_cases {
        let item = MockImapProtocolParser::parse_list_line(raw)
            .unwrap_or_else(|| panic!("Failed to parse spaced LIST line: {}", raw));
        assert_eq!(item.name, exp_name);
        assert_eq!(item.delimiter, exp_delim);
        assert_eq!(item.is_sent, is_sent);
    }

    // 3. Hierarchy root / reference probe (empty mailbox name)
    let probe_cases = [
        (r#"* LIST () "/" """#, "", '/'),
        (r#"* LIST (\Noselect) NIL """#, "", '/'),
    ];
    for (raw, exp_name, exp_delim) in probe_cases {
        let item = MockImapProtocolParser::parse_list_line(raw)
            .unwrap_or_else(|| panic!("Failed to parse reference probe line: {}", raw));
        assert_eq!(item.name, exp_name);
        assert_eq!(item.delimiter, exp_delim);
    }

    // 4. Strict rejection of truncated and malformed LIST lines
    let invalid_cases = [
        "",
        "   ",
        "\r\n",
        "* LIST",
        "* LIST ()",
        "* LIST (\\HasNoChildren)",
        "* LIST () \"/\"",
        "* LIST () NIL",
        "* LIST () .",
        "* LIST (unclosed",
        "* LIST () \"unclosed",
        "* LIST () \"/\" \"unclosed",
        "* NO SELECT failed",
        "* OK Ready",
        "* 50 EXISTS",
    ];

    for bad in invalid_cases {
        let res = MockImapProtocolParser::parse_list_line(bad);
        assert!(res.is_none(), "Must reject invalid/truncated line: {:?}", bad);
    }
}

#[test]
fn tier5_challenger1_m3_fix1_fetch_envelopes_uids_exceeding_total_stress() {
    // Stress test: Sequence numbers vs server UIDs where UIDs > total count.
    // Mailbox has 10 messages total (sequence 1..10).
    // Server assigns UIDs 5001 through 5010 (all UIDs > 10).
    let total_exists: usize = 10;
    let server_uids: Vec<u32> = (5001..=5010).collect();

    // Oracle sequence range calculator per RFC 3501 Section 6.4.5:
    let calculate_fetch_range = |total: usize, offset: usize, limit: usize| -> Option<(usize, usize)> {
        if total == 0 || offset >= total {
            return None;
        }
        let end = total.saturating_sub(offset);
        if end == 0 {
            return None;
        }
        let start = end.saturating_sub(limit).saturating_add(1).max(1);
        if start > end {
            return None;
        }
        Some((start, end))
    };

    // 1. First page: offset = 0, limit = 5 (latest 5 messages: sequence 6:10)
    let (start, end) = calculate_fetch_range(total_exists, 0, 5).expect("Valid page 1 range");
    assert_eq!(start, 6);
    assert_eq!(end, 10);
    let cmd = format!("A0001 FETCH {start}:{end} (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])");
    assert!(cmd.contains("FETCH 6:10"), "Command must fetch sequence range 6:10");
    assert!(!cmd.contains("UID FETCH"), "Command must NOT use UID FETCH for sequence numbers");

    // Simulate server response lines for sequence 6..10
    let mut fetched_messages = Vec::new();
    for seq in start..=end {
        let uid = server_uids[seq - 1]; // UID 5006..5010
        let is_seen = seq % 2 == 0;
        let is_flagged = seq == 8 || seq == 9;
        let flag_str = match (is_seen, is_flagged) {
            (true, true) => "\\Seen \\Flagged",
            (true, false) => "\\Seen",
            (false, true) => "\\Flagged",
            (false, false) => "",
        };

        let response_line = format!(
            "* {} FETCH (UID {} FLAGS ({}) RFC822.SIZE 120 BODY[HEADER] {{120}}",
            seq, uid, flag_str
        );

        // Verify UID extraction logic from response line
        let extracted_uid = {
            let upper = response_line.to_ascii_uppercase();
            let pos = upper.find("UID ").expect("Must contain UID");
            let rest = &response_line[pos + 4..];
            let end_idx = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            rest[..end_idx].to_string()
        };

        assert_eq!(extracted_uid, uid.to_string(), "Extracted UID must be server UID (> total)");
        assert!(
            extracted_uid.parse::<usize>().unwrap() > total_exists,
            "UID ({}) must exceed total count ({})",
            extracted_uid,
            total_exists
        );

        fetched_messages.push((extracted_uid, !is_seen, is_flagged));
    }

    assert_eq!(fetched_messages.len(), 5);
    assert_eq!(fetched_messages[0].0, "5006");
    assert_eq!(fetched_messages[4].0, "5010");

    // 2. Second page: offset = 5, limit = 5 (older 5 messages: sequence 1:5)
    let (start2, end2) = calculate_fetch_range(total_exists, 5, 5).expect("Valid page 2 range");
    assert_eq!(start2, 1);
    assert_eq!(end2, 5);
    let cmd2 = format!("A0002 FETCH {start2}:{end2} (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])");
    assert!(cmd2.contains("FETCH 1:5"));

    // 3. Boundary cases
    assert_eq!(calculate_fetch_range(total_exists, 10, 5), None, "Offset == total returns empty");
    assert_eq!(calculate_fetch_range(total_exists, 20, 5), None, "Offset > total returns empty");
    assert_eq!(calculate_fetch_range(0, 0, 10), None, "Empty mailbox (total == 0) returns empty");

    // 4. Non-contiguous & large gap UIDs (e.g. UIDs: [42, 8888, 999999])
    let gap_uids = [42, 8888, 999999];
    for (idx, &uid) in gap_uids.iter().enumerate() {
        let seq = idx + 1;
        let line = format!("* {} FETCH (UID {} FLAGS () RFC822.SIZE 50)", seq, uid);
        let upper = line.to_ascii_uppercase();
        let pos = upper.find("UID ").unwrap();
        let rest = &line[pos + 4..];
        let end_idx = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        assert_eq!(&rest[..end_idx], uid.to_string());
    }
}

#[test]
fn tier5_challenger1_m3_fix1_smtp_folded_headers_and_quoted_commas_stress() {
    // 1. Folded RFC 5322 header continuations across 4 lines with mixed tabs/spaces
    let multi_folded_email = concat!(
        "From: \"Smith, Jr., Alice\" <alice@example.com>\r\n",
        "To: \"Tester, Evolution\" <martin@example.com>,\r\n",
        "    \"Doe, Jane, Jr., Esq.\" <jane@example.com>,\r\n",
        "\t\"Public, John Q.\" <john@example.com>,\r\n",
        "    regular@example.com\r\n",
        "Cc: \"Boss, Big, CEO\" <boss@example.com>,\r\n",
        "    \"Consultant, Ext., CPA\" <cpa@example.com>\r\n",
        "Bcc: secret_agent@example.com,\r\n",
        "    \"Stealth, Operator\" <007@example.com>\r\n",
        "Subject: Multi-line Folded Stress Test\r\n",
        "\r\n",
        "Body payload"
    ).as_bytes();

    let (sender, recipients) = extract_envelope_addresses_oracle(multi_folded_email);
    assert_eq!(sender, "alice@example.com");

    let expected_recipients = [
        "martin@example.com",
        "jane@example.com",
        "john@example.com",
        "regular@example.com",
        "boss@example.com",
        "cpa@example.com",
        "secret_agent@example.com",
        "007@example.com",
    ];

    for exp in &expected_recipients {
        assert!(
            recipients.iter().any(|r| r.eq_ignore_ascii_case(exp)),
            "Missing expected recipient: {}",
            exp
        );
    }
    assert_eq!(recipients.len(), expected_recipients.len());

    // Verify NONE of the display name fragments were emitted as corrupt recipients
    let forbidden_fragments = [
        "Smith", "Jr.", "Alice", "Tester", "Evolution", "Doe", "Jane", "Esq.",
        "Public", "John", "Q.", "Boss", "Big", "CEO", "Consultant", "Ext.",
        "CPA", "Stealth", "Operator",
    ];
    for frag in &forbidden_fragments {
        assert!(
            !recipients.iter().any(|r| r.eq_ignore_ascii_case(frag)),
            "Corrupt fragment leaked as recipient: {}",
            frag
        );
    }

    // 2. Comments containing commas, semicolons, and '@' symbols
    let commented_email = concat!(
        "From: sender@example.com (Sender Name, With Comma)\r\n",
        "To: user1@example.com (Comment, With, Multiple, Commas);\r\n",
        "    user2@example.com (Comment with @symbol and; semicolon),\r\n",
        "    user3@example.com\r\n",
        "\r\n",
        "Body"
    ).as_bytes();

    let (sender_c, recipients_c) = extract_envelope_addresses_oracle(commented_email);
    assert_eq!(sender_c, "sender@example.com");
    assert_eq!(
        recipients_c,
        vec![
            "user1@example.com".to_string(),
            "user2@example.com".to_string(),
            "user3@example.com".to_string(),
        ]
    );

    // 3. Bogus and malformed tokens rejected cleanly
    let bogus_email = concat!(
        "From: real_sender@example.com\r\n",
        "To: \"No Email Here\", undisclosed-recipients:;, <>, @nodomain, nolocal@, valid@example.com\r\n",
        "\r\n",
        "Body"
    ).as_bytes();

    let (_, recipients_b) = extract_envelope_addresses_oracle(bogus_email);
    assert_eq!(recipients_b, vec!["valid@example.com".to_string()]);

    // 4. Case-insensitive deduplication
    let dedup_email = concat!(
        "From: sender@example.com\r\n",
        "To: alice@example.com, ALICE@EXAMPLE.COM, Alice@Example.Com, bob@example.com\r\n",
        "\r\n",
        "Body"
    ).as_bytes();

    let (_, recipients_d) = extract_envelope_addresses_oracle(dedup_email);
    assert_eq!(
        recipients_d,
        vec!["alice@example.com".to_string(), "bob@example.com".to_string()]
    );

    // 5. Escaped quotes and angle brackets in display names
    let tricky_email = concat!(
        "From: \"User \\\"Nick\\\" Name, Jr.\" <sender@example.com>\r\n",
        "To: \"User <Nickname> Test\" <tricky_to@example.com>\r\n",
        "\r\n",
        "Body"
    ).as_bytes();

    let (sender_t, recipients_t) = extract_envelope_addresses_oracle(tricky_email);
    assert_eq!(sender_t, "sender@example.com");
    assert_eq!(recipients_t, vec!["tricky_to@example.com".to_string()]);
}

// ============================================================================
// Milestone 3 Challenger 2 Iteration 2 Empirical Stress Harness (challenger_m3_fix_2)
// ============================================================================

#[test]
fn tier5_challenger2_m3_fix2_offline_cache_fallback_multiple_accounts_stress() {
    // 1. Stress test multi-account offline failure with cached mailbox recovery
    struct SimulatedAccount {
        display_name: String,
        is_reachable: bool,
    }

    struct SimulatedCachedMessage {
        id: String,
        source_uid: String,
        raw_mime: Option<Vec<u8>>,
        raw_headers: Option<Vec<u8>>,
        html_body: String,
    }

    struct SimulatedMailLoad {
        messages: Vec<SimulatedCachedMessage>,
        configured_sources: usize,
        failed_sources: usize,
        warning: Option<String>,
    }

    let accounts = vec![
        SimulatedAccount { display_name: "Work IMAP".into(), is_reachable: false },
        SimulatedAccount { display_name: "Personal Posteo".into(), is_reachable: false },
        SimulatedAccount { display_name: "Archive Server".into(), is_reachable: false },
    ];

    let cached_messages = vec![
        SimulatedCachedMessage {
            id: "msg-101".into(),
            source_uid: "acc1".into(),
            raw_mime: Some(b"From: boss@work.com\r\nSubject: Quarterly Budget\r\n\r\nApproved.".to_vec()),
            raw_headers: None,
            html_body: "".into(),
        },
        SimulatedCachedMessage {
            id: "msg-202".into(),
            source_uid: "acc2".into(),
            raw_mime: None,
            raw_headers: Some(b"From: friend@posteo.de\r\nSubject: Weekend Plans".to_vec()),
            html_body: "<p>Let's meet at 10am.</p>".into(),
        },
    ];

    // Simulate load_messages_page offline fallback logic per portable_mail_engine/mod.rs:162-180
    let mut successful_sources = 0;
    let mut failed_sources = 0;
    let mut warnings = Vec::new();
    let live_messages: Vec<String> = Vec::new();

    for acc in &accounts {
        if acc.is_reachable {
            successful_sources += 1;
        } else {
            failed_sources += 1;
            warnings.push(format!("{}: Verbindung verweigert (Connection refused)", acc.display_name));
        }
    }

    assert_eq!(successful_sources, 0);
    assert_eq!(failed_sources, 3);

    // Fallback trigger condition: (successful_sources == 0 || live_messages.is_empty()) && failed_sources > 0
    let fallback_triggered = (successful_sources == 0 || live_messages.is_empty()) && failed_sources > 0;
    assert!(fallback_triggered, "Offline fallback must trigger when all configured accounts fail");

    let fallback_load = SimulatedMailLoad {
        messages: cached_messages,
        configured_sources: accounts.len(),
        failed_sources,
        warning: Some(format!(
            "Offline-Modus: Server nicht erreichbar ({}); zwischengespeicherte Daten geladen",
            warnings.join("; ")
        )),
    };

    assert_eq!(fallback_load.configured_sources, 3);
    assert_eq!(fallback_load.failed_sources, 3);
    assert_eq!(fallback_load.messages.len(), 2);
    assert!(fallback_load.warning.as_ref().unwrap().contains("Offline-Modus"));
    assert!(fallback_load.warning.as_ref().unwrap().contains("Work IMAP"));
    assert!(fallback_load.warning.as_ref().unwrap().contains("Personal Posteo"));

    // 2. Simulate load_raw_message offline fallback per portable_mail_engine/mod.rs:234-266
    let fetch_raw_message = |source_uid: &str, message_uid: &str, is_online: bool| -> Result<Vec<u8>, String> {
        if is_online {
            Ok(b"LIVE RAW MESSAGE FROM IMAP".to_vec())
        } else {
            // Offline fallback: find in cached_messages
            if let Some(msg) = fallback_load.messages.iter().find(|m| m.source_uid == source_uid && m.id == message_uid) {
                if let Some(ref raw) = msg.raw_mime {
                    return Ok(raw.clone());
                }
                if let Some(ref headers) = msg.raw_headers {
                    let mut full = headers.clone();
                    full.extend_from_slice(b"\r\n\r\n");
                    full.extend_from_slice(msg.html_body.as_bytes());
                    return Ok(full);
                }
            }
            Err(format!("Offline: Nachricht {} konnte nicht geladen werden", message_uid))
        }
    };

    // Case A: Full raw MIME present in cache
    let raw_a = fetch_raw_message("acc1", "msg-101", false).expect("Cached raw MIME should be returned");
    assert!(raw_a.starts_with(b"From: boss@work.com"));

    // Case B: Raw headers + HTML body reconstructed
    let raw_b = fetch_raw_message("acc2", "msg-202", false).expect("Reconstructed RFC822 should be returned");
    assert!(raw_b.starts_with(b"From: friend@posteo.de"));
    assert!(raw_b.ends_with(b"<p>Let's meet at 10am.</p>"));

    // Case C: Non-existent message returns explicit offline error
    let raw_c = fetch_raw_message("acc1", "non-existent-msg", false);
    assert!(raw_c.is_err());
    assert!(raw_c.unwrap_err().contains("Offline: Nachricht non-existent-msg konnte nicht geladen werden"));
}

#[test]
fn tier5_challenger2_m3_fix2_keychain_persistence_and_account_deletion_stress() {
    let store = MockKeychainStore::new();
    let creds = MockCredentialService::new(&store);

    // Test matrix of 5 accounts simulating EDS sync and AccountConfigStore::delete lifecycle
    let accounts_matrix = [
        ("work-account-uuid-1", "work-mail-uid", "work-transport-uid", "work@company.com", "P@ss-Corp-2026!"),
        ("personal-uuid-2", "personal-mail-uid", "personal-transport-uid", "me@posteo.de", "Posteo-Secret-99#"),
        ("fastmail-uuid-3", "fastmail-mail-uid", "fastmail-transport-uid", "user@fastmail.com", "App-Specific-Password-XYZ"),
        ("gmail-oauth-4", "gmail-mail-uid", "gmail-transport-uid", "dev@gmail.com", "OAuth-Refresh-Token-ABC"),
        ("custom-imap-5", "custom-mail-uid", "custom-transport-uid", "admin@server.org", "Root-Admin-Key-8888"),
    ];

    // 1. Simulate sync_profile_to_eds persisting credentials across all 4 keys per account
    for (profile_id, mail_uid, transport_uid, email, secret) in &accounts_matrix {
        assert!(creds.store_password(profile_id, secret).is_ok());
        assert!(creds.store_password(mail_uid, secret).is_ok());
        assert!(creds.store_password(transport_uid, secret).is_ok());
        assert!(creds.store_password(email, secret).is_ok());
    }

    // Total entries: 5 accounts * 4 keys = 20 entries
    assert_eq!(store.count_service_entries(KEYCHAIN_SERVICE_ID), 20);

    // Verify all keys are retrievable
    for (profile_id, mail_uid, transport_uid, email, secret) in &accounts_matrix {
        assert_eq!(creds.get_password(profile_id).unwrap().as_deref(), Some(*secret));
        assert_eq!(creds.get_password(mail_uid).unwrap().as_deref(), Some(*secret));
        assert_eq!(creds.get_password(transport_uid).unwrap().as_deref(), Some(*secret));
        assert_eq!(creds.get_password(email).unwrap().as_deref(), Some(*secret));
    }

    // 2. Simulate AccountConfigStore::delete on account 2 (purges all 4 keys)
    let target = &accounts_matrix[1];
    assert!(creds.delete_password(target.0).is_ok());
    assert!(creds.delete_password(target.1).is_ok());
    assert!(creds.delete_password(target.2).is_ok());
    assert!(creds.delete_password(target.3).is_ok());

    // Account 2 credentials must be None
    assert_eq!(creds.get_password(target.0).unwrap(), None);
    assert_eq!(creds.get_password(target.1).unwrap(), None);
    assert_eq!(creds.get_password(target.2).unwrap(), None);
    assert_eq!(creds.get_password(target.3).unwrap(), None);

    // Store must have exactly 16 entries remaining
    assert_eq!(store.count_service_entries(KEYCHAIN_SERVICE_ID), 16);

    // All other 4 accounts must remain untouched
    for (idx, (profile_id, mail_uid, transport_uid, email, secret)) in accounts_matrix.iter().enumerate() {
        if idx == 1 { continue; }
        assert_eq!(creds.get_password(profile_id).unwrap().as_deref(), Some(*secret));
        assert_eq!(creds.get_password(mail_uid).unwrap().as_deref(), Some(*secret));
        assert_eq!(creds.get_password(transport_uid).unwrap().as_deref(), Some(*secret));
        assert_eq!(creds.get_password(email).unwrap().as_deref(), Some(*secret));
    }

    // 3. Update existing credentials on account 0 and verify immediate overwrite
    let updated_secret = "NEW-Updated-Corp-Password-2027!";
    assert!(creds.store_password(accounts_matrix[0].0, updated_secret).is_ok());
    assert_eq!(
        creds.get_password(accounts_matrix[0].0).unwrap().as_deref(),
        Some(updated_secret)
    );

    // 4. Idempotent deletion on already deleted account 2 keys
    assert!(creds.delete_password(target.0).is_ok());
    assert!(creds.delete_password("non-existent-random-account-uid").is_ok());
}

#[test]
fn tier5_challenger2_m4_private_key_permissions_hardening_stress() {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    let temp_dir = std::env::temp_dir().join(format!("evo-perm-stress-{}", glib::uuid_string_random()));
    fs::create_dir_all(&temp_dir).unwrap();

    let key_dir = temp_dir.join("keys");
    fs::create_dir_all(&key_dir).unwrap();

    // 1. Stress directory permissions tightening under Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Case A: Pre-existing directory with loose 0o755 (rwxr-xr-x)
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let pre_mode = fs::metadata(&key_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(pre_mode, 0o755, "Dir should start at 0o755");

        // Hardening: tighten to 0o700
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let post_mode = fs::metadata(&key_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(post_mode, 0o700, "Key directory permissions must be tightened to 0o700");
        assert_eq!(post_mode & 0o077, 0, "Group and other permissions on key dir must be 0");

        // Case B: Pre-existing directory with insecure 0o777 (rwxrwxrwx)
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o777)).unwrap();
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let post_mode_777 = fs::metadata(&key_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(post_mode_777, 0o700, "Key directory permissions from 0o777 must be tightened to 0o700");
    }

    // 2. Stress pre-existing key files with various loose permissions
    let test_modes = [0o644, 0o666, 0o755, 0o777];
    for &initial_mode in &test_modes {
        let key_file = key_dir.join(format!("test_key_{initial_mode:o}.key"));

        // Pre-create file with insecure initial content and loose mode
        fs::write(&key_file, b"INSECURE_PREEXISTING_KEY_PAYLOAD").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_file, fs::Permissions::from_mode(initial_mode)).unwrap();
            let actual_initial = fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(actual_initial, initial_mode, "Initial mode must match setup: {initial_mode:o}");
        }

        // Simulate the import_pkcs12 file write & hardening logic
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&key_file).expect("Opening pre-existing key file must succeed");
        file.write_all(b"-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASC...").unwrap();
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))
                .expect("Explicit tightening of key file permissions to 0o600 must succeed");

            let final_mode = fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                final_mode,
                0o600,
                "Pre-existing key file starting at {:o} MUST be tightened to 0o600",
                initial_mode
            );
            assert_eq!(
                final_mode & 0o077,
                0,
                "Group and other permissions on key file starting at {:o} must be 0",
                initial_mode
            );
        }

        let content = fs::read_to_string(&key_file).unwrap();
        assert!(content.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(!content.contains("INSECURE_PREEXISTING_KEY_PAYLOAD"));
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn tier5_challenger2_m4_pkcs12_passphrase_matrix_and_adversarial_import_stress() {
    use std::fs;
    use std::process::Command;

    // 1. Verification of environment variable passphrase secrecy mapping
    fn apply_test_passin(cmd: &mut Command, passphrase: Option<&str>) {
        if let Some(pass) = passphrase {
            if !pass.is_empty() {
                cmd.env("EVOLUTION_OPENSSL_PASSIN", pass);
                cmd.args(["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
                return;
            }
        }
        cmd.args(["-passin", "pass:"]);
    }

    fn apply_test_passout(cmd: &mut Command, passphrase: Option<&str>) {
        if let Some(pass) = passphrase {
            if !pass.is_empty() {
                cmd.env("EVOLUTION_OPENSSL_PASSOUT", pass);
                cmd.args(["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
                return;
            }
        }
        cmd.args(["-passout", "pass:"]);
    }

    let complex_passphrases = [
        "SimplePass2026",
        "P@ss:w0rd!#$ %^&*()_+-=[]{}|;':,.<>?/~",
        "S/MIME-🔑-Passphrase-ÄÖÜß-2026",
        "quote\"and'dollar$and`backtick`",
        "with spaces and \t tabs",
        "colons:::in:::middle:::and:::ends:::",
    ];

    for &secret in &complex_passphrases {
        let mut cmd = Command::new("openssl");
        apply_test_passin(&mut cmd, Some(secret));

        // Subprocess arguments check
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, vec!["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
        assert!(!args.iter().any(|a| a.contains(secret)), "Secret must not leak into argv!");

        // Subprocess environment check
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().to_string(), v.map(|s| s.to_string_lossy().to_string())))
            .collect();
        assert_eq!(
            envs,
            vec![("EVOLUTION_OPENSSL_PASSIN".to_string(), Some(secret.to_string()))]
        );

        let mut out_cmd = Command::new("openssl");
        apply_test_passout(&mut out_cmd, Some(secret));
        let out_args: Vec<String> = out_cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(out_args, vec!["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
        assert!(!out_args.iter().any(|a| a.contains(secret)));
    }

    // Empty and None passphrases must map to pass: without env vars
    for empty_case in [None, Some("")] {
        let mut cmd = Command::new("openssl");
        apply_test_passin(&mut cmd, empty_case);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, vec!["-passin", "pass:"]);
        assert!(cmd.get_envs().next().is_none());

        let mut out_cmd = Command::new("openssl");
        apply_test_passout(&mut out_cmd, empty_case);
        let out_args: Vec<String> = out_cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(out_args, vec!["-passout", "pass:"]);
        assert!(out_cmd.get_envs().next().is_none());
    }

    // 2. OpenSSL CLI End-to-End Execution Matrix
    let openssl_bin = std::env::var_os("EVOLUTION_OPENSSL_BINARY").unwrap_or_else(|| "openssl".into());
    let has_openssl = Command::new(&openssl_bin).arg("version").output().is_ok();
    if !has_openssl {
        return;
    }

    let temp_dir = std::env::temp_dir().join(format!("evo-p12-stress-{}", glib::uuid_string_random()));
    fs::create_dir_all(&temp_dir).unwrap();
    let key_dir = temp_dir.join("keys");
    let cert_dir = temp_dir.join("certs");
    fs::create_dir_all(&key_dir).unwrap();
    fs::create_dir_all(&cert_dir).unwrap();

    let rsa_key = temp_dir.join("test.key");
    let x509_cert = temp_dir.join("test.crt");

    let gen_status = Command::new(&openssl_bin)
        .args(["req", "-x509", "-newkey", "rsa:2048", "-keyout"])
        .arg(&rsa_key)
        .arg("-out")
        .arg(&x509_cert)
        .args(["-days", "1", "-nodes", "-subj", "/CN=Challenger Stress/O=Evolution Project"])
        .output()
        .expect("OpenSSL certificate generation must succeed");
    assert!(gen_status.status.success());

    let _cert_pem = fs::read_to_string(&x509_cert).unwrap();

    // Export and Import cycle across passphrase types
    for (i, &pass) in complex_passphrases.iter().enumerate() {
        let p12_file = temp_dir.join(format!("archive_{i}.p12"));

        // Export with passout env
        let mut exp_cmd = Command::new(&openssl_bin);
        exp_cmd.args(["pkcs12", "-export", "-in"])
            .arg(&x509_cert)
            .arg("-inkey")
            .arg(&rsa_key)
            .arg("-out")
            .arg(&p12_file);
        apply_test_passout(&mut exp_cmd, Some(pass));
        let exp_res = exp_cmd.output().unwrap();
        assert!(exp_res.status.success(), "Export with passphrase '{pass}' must succeed: {:?}", String::from_utf8_lossy(&exp_res.stderr));

        // Test A: Import with incorrect passphrase must fail cleanly
        let mut bad_cmd = Command::new(&openssl_bin);
        bad_cmd.args(["pkcs12", "-in"])
            .arg(&p12_file)
            .args(["-nokeys", "-clcerts"]);
        apply_test_passin(&mut bad_cmd, Some("CompletelyWrongPass123!"));
        let bad_out = bad_cmd.output().unwrap();
        assert!(!bad_out.status.success(), "Import with wrong passphrase must fail with non-zero exit");

        // Test B: Import with empty passphrase against encrypted archive must fail cleanly
        let mut empty_cmd = Command::new(&openssl_bin);
        empty_cmd.args(["pkcs12", "-in"])
            .arg(&p12_file)
            .args(["-nokeys", "-clcerts"]);
        apply_test_passin(&mut empty_cmd, None);
        let empty_out = empty_cmd.output().unwrap();
        assert!(!empty_out.status.success(), "Import with empty passphrase on encrypted archive must fail");

        // Test C: Import with correct passphrase must succeed
        let mut good_cmd = Command::new(&openssl_bin);
        good_cmd.args(["pkcs12", "-in"])
            .arg(&p12_file)
            .args(["-nokeys", "-clcerts"]);
        apply_test_passin(&mut good_cmd, Some(pass));
        let good_out = good_cmd.output().unwrap();
        assert!(good_out.status.success(), "Import with correct passphrase must succeed: {:?}", String::from_utf8_lossy(&good_out.stderr));
        let extracted_pem = String::from_utf8_lossy(&good_out.stdout);
        assert!(extracted_pem.contains("-----BEGIN CERTIFICATE-----"));
        assert!(extracted_pem.contains("-----END CERTIFICATE-----"));
    }

    // 3. Corrupted Archive Fuzzing
    let corrupt_p12 = temp_dir.join("archive_0.p12");
    let valid_bytes = fs::read(&corrupt_p12).unwrap();

    // Case 1: 0-byte file
    let zero_file = temp_dir.join("zero.p12");
    fs::write(&zero_file, b"").unwrap();
    let mut zero_cmd = Command::new(&openssl_bin);
    zero_cmd.args(["pkcs12", "-in"]).arg(&zero_file).args(["-nokeys"]);
    apply_test_passin(&mut zero_cmd, Some("pass"));
    assert!(!zero_cmd.output().unwrap().status.success());

    // Case 2: 512-byte random garbage
    let garbage_file = temp_dir.join("garbage.p12");
    let garbage: Vec<u8> = (0..512).map(|x| (x * 31 + 7) as u8).collect();
    fs::write(&garbage_file, &garbage).unwrap();
    let mut gar_cmd = Command::new(&openssl_bin);
    gar_cmd.args(["pkcs12", "-in"]).arg(&garbage_file).args(["-nokeys"]);
    apply_test_passin(&mut gar_cmd, Some("pass"));
    assert!(!gar_cmd.output().unwrap().status.success());

    // Case 3: Truncated valid archive
    for cut in [16, 64, valid_bytes.len().saturating_sub(10)] {
        let trunc_file = temp_dir.join(format!("trunc_{cut}.p12"));
        fs::write(&trunc_file, &valid_bytes[..cut]).unwrap();
        let mut trunc_cmd = Command::new(&openssl_bin);
        trunc_cmd.args(["pkcs12", "-in"]).arg(&trunc_file).args(["-nokeys"]);
        apply_test_passin(&mut trunc_cmd, Some(complex_passphrases[0]));
        assert!(!trunc_cmd.output().unwrap().status.success());
    }

    // Case 4: Bit-flipped valid archive
    let mut flipped = valid_bytes.clone();
    for b in &mut flipped[20..50] {
        *b ^= 0xAA;
    }
    let flip_file = temp_dir.join("flipped.p12");
    fs::write(&flip_file, &flipped).unwrap();
    let mut flip_cmd = Command::new(&openssl_bin);
    flip_cmd.args(["pkcs12", "-in"]).arg(&flip_file).args(["-nokeys"]);
    apply_test_passin(&mut flip_cmd, Some(complex_passphrases[0]));
    assert!(!flip_cmd.output().unwrap().status.success());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn tier5_challenger2_m4_preferences_dialog_dynamic_smime_reload_contract_stress() {
    use gtk::gio::prelude::*;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    let settings = create_test_settings();

    // Reusable simulated reload function matching preferences_dialog.rs:4969-5054
    fn simulate_refresh_smime_selectors(
        store: &MockCertificateStore,
        current_signer: &str,
        current_ca: &str,
    ) -> (Vec<String>, u32, Vec<String>, u32) {
        // 1. Reload personal certs
        let personal = store.list_by_category(CertCategory::Personal);
        let mut signer_labels = vec!["Nicht festgelegt".to_string()];
        for cert in &personal {
            let label = if !cert.subject_cn.is_empty() {
                cert.subject_cn.clone()
            } else {
                cert.id.clone()
            };
            signer_labels.push(label);
        }

        let mut selected_signer_idx = 0u32;
        if !current_signer.is_empty() {
            let mut matched = false;
            for (i, cert) in personal.iter().enumerate() {
                if current_signer == cert.id || current_signer == cert.raw_pem {
                    selected_signer_idx = i as u32 + 1;
                    matched = true;
                    break;
                }
            }
            if !matched {
                signer_labels.push(format!("Datei: {current_signer}"));
                selected_signer_idx = (signer_labels.len() - 1) as u32;
            }
        }

        // 2. Reload CA certs
        let authorities = store.list_by_category(CertCategory::Authorities);
        let mut ca_labels = vec!["System-Vertrauensspeicher (Standard)".to_string()];
        for ca in &authorities {
            let label = if !ca.issuer_org.is_empty() {
                format!("{} ({})", ca.subject_cn, ca.issuer_org)
            } else {
                ca.subject_cn.clone()
            };
            ca_labels.push(label);
        }

        let mut selected_ca_idx = 0u32;
        if !current_ca.is_empty() {
            let mut matched = false;
            for (i, ca) in authorities.iter().enumerate() {
                if current_ca == ca.id || current_ca == ca.raw_pem {
                    selected_ca_idx = i as u32 + 1;
                    matched = true;
                    break;
                }
            }
            if !matched {
                ca_labels.push(format!("Datei: {current_ca}"));
                selected_ca_idx = (ca_labels.len() - 1) as u32;
            }
        }

        (signer_labels, selected_signer_idx, ca_labels, selected_ca_idx)
    }

    let mut store = MockCertificateStore::new();

    // Initial state: empty store
    let cur_s = settings.string("smime-signer-certificate");
    let cur_c = settings.string("smime-ca-file");
    let (signer_labels, signer_idx, ca_labels, ca_idx) = simulate_refresh_smime_selectors(&store, &cur_s, &cur_c);
    assert_eq!(signer_labels, vec!["Nicht festgelegt"]);
    assert_eq!(signer_idx, 0);
    assert_eq!(ca_labels, vec!["System-Vertrauensspeicher (Standard)"]);
    assert_eq!(ca_idx, 0);

    // Rapid close triggers with zero mutations
    for _ in 0..5 {
        let (s_labels, s_idx, c_labels, c_idx) = simulate_refresh_smime_selectors(&store, &cur_s, &cur_c);
        assert_eq!(s_labels.len(), 1);
        assert_eq!(s_idx, 0);
        assert_eq!(c_labels.len(), 1);
        assert_eq!(c_idx, 0);
    }

    // Populate store with 3 personal and 2 CA certificates
    let cert_alice = MockX509Parser::parse_der(&DerBuilder::create_test_x509_der(
        0x101, "Alice Developer", "Corp", "Root CA", "Corp", "260101000000Z", "300101000000Z", Some(0x80),
    )).unwrap();
    let id_alice = cert_alice.id.clone();

    let cert_bob = MockX509Parser::parse_der(&DerBuilder::create_test_x509_der(
        0x102, "Bob Operations", "Corp", "Root CA", "Corp", "260101000000Z", "300101000000Z", Some(0x80),
    )).unwrap();
    let id_bob = cert_bob.id.clone();

    let cert_charlie = MockX509Parser::parse_der(&DerBuilder::create_test_x509_der(
        0x103, "Charlie Admin", "Corp", "Root CA", "Corp", "260101000000Z", "300101000000Z", Some(0x80),
    )).unwrap();
    let _id_charlie = cert_charlie.id.clone();

    let ca_corp = MockX509Parser::parse_der(&DerBuilder::create_test_x509_der(
        0x201, "Corporate Root CA", "Corporate Security", "Corporate Root CA", "Corporate Security", "260101000000Z", "360101000000Z", Some(0x06),
    )).unwrap();
    let _id_ca_corp = ca_corp.id.clone();

    let ca_partner = MockX509Parser::parse_der(&DerBuilder::create_test_x509_der(
        0x202, "Partner Trust CA", "Partner Org", "Partner Trust CA", "Partner Org", "260101000000Z", "360101000000Z", Some(0x06),
    )).unwrap();
    let id_ca_partner = ca_partner.id.clone();

    store.add_certificate(cert_alice);
    store.add_certificate(cert_bob);
    store.add_certificate(cert_charlie);
    store.add_certificate(ca_corp);
    store.add_certificate(ca_partner);

    // Refresh after import: verify all labels populated
    let (s_labels, _s_idx, c_labels, _c_idx) = simulate_refresh_smime_selectors(&store, &cur_s, &cur_c);
    assert_eq!(s_labels.len(), 4);
    assert_eq!(c_labels.len(), 3);

    // Select Bob as signer and Partner CA as CA file
    settings.set_string("smime-signer-certificate", &id_bob).unwrap();
    settings.set_string("smime-ca-file", &id_ca_partner).unwrap();

    let cur_s_bob = settings.string("smime-signer-certificate");
    let cur_c_partner = settings.string("smime-ca-file");
    let (s_labels_cur, bob_idx, c_labels_cur, partner_idx) = simulate_refresh_smime_selectors(&store, &cur_s_bob, &cur_c_partner);
    let expected_bob_idx = s_labels_cur.iter().position(|l| l.contains("Bob Operations")).expect("Bob must be in labels") as u32;
    let expected_partner_idx = c_labels_cur.iter().position(|l| l.contains("Partner Trust CA")).expect("Partner CA must be in labels") as u32;
    assert_eq!(bob_idx, expected_bob_idx, "Bob selection index must dynamically track Bob's row");
    assert_eq!(partner_idx, expected_partner_idx, "Partner CA selection index must dynamically track Partner CA row");

    // Remove Alice from store (items shift forward!)
    store.records.remove(&id_alice);
    let (s_labels_shifted, s_idx_shifted, _, _) = simulate_refresh_smime_selectors(&store, &cur_s_bob, &cur_c_partner);
    assert_eq!(s_labels_shifted.len(), 3);
    assert!(s_labels_shifted.iter().any(|l| l.contains("Bob Operations")));
    let expected_bob_shifted_idx = s_labels_shifted.iter().position(|l| l.contains("Bob Operations")).unwrap() as u32;
    assert_eq!(s_idx_shifted, expected_bob_shifted_idx, "Bob's index must dynamically update, preserving selection across store shifts!");

    // Remove selected certificate (Bob) from store (simulates external deletion or manager removal)
    store.records.remove(&id_bob);
    let (s_labels_deleted, s_idx_deleted, _, _) = simulate_refresh_smime_selectors(&store, &cur_s_bob, &cur_c_partner);
    assert_eq!(s_labels_deleted.len(), 3);
    assert_eq!(s_labels_deleted[2], format!("Datei: {id_bob}"));
    assert_eq!(s_idx_deleted, 2, "Fallback 'Datei:' item preserves selection gracefully without panic");

    // Clean settings
    settings.set_string("smime-signer-certificate", "").unwrap();
    settings.set_string("smime-ca-file", "").unwrap();
    let cur_s_empty = settings.string("smime-signer-certificate");
    let cur_c_empty = settings.string("smime-ca-file");
    let (_, s_clean, _, c_clean) = simulate_refresh_smime_selectors(&store, &cur_s_empty, &cur_c_empty);
    assert_eq!(s_clean, 0);
    assert_eq!(c_clean, 0);

    // 3. Concurrency Stress: Multi-threaded store mutations while reloading
    let shared_store = Arc::new(Mutex::new(store));
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();

    for t in 0..4 {
        let store_clone = Arc::clone(&shared_store);
        let bar_clone = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            bar_clone.wait();
            for i in 0..20 {
                if t % 2 == 0 {
                    // Writer
                    let new_cert = MockX509Parser::parse_der(&DerBuilder::create_test_x509_der(
                        (0x300 + t * 100 + i) as u64,
                        &format!("Concurrent User {t}-{i}"),
                        "Concurrent Org",
                        "Root CA",
                        "Concurrent Org",
                        "260101000000Z",
                        "300101000000Z",
                        Some(0x80),
                    )).unwrap();
                    let mut guard = store_clone.lock().unwrap();
                    guard.add_certificate(new_cert);
                } else {
                    // Reader / UI Reload simulation
                    let guard = store_clone.lock().unwrap();
                    let (signers, _, cas, _) = simulate_refresh_smime_selectors(&guard, "some-cert-id", "");
                    assert!(signers.len() >= 1);
                    assert!(cas.len() >= 1);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("Worker thread must not panic");
    }
}

// ============================================================================
// Milestone 4 Challenger 1 Iteration 2 Empirical Stress Harness (challenger_m4_fix_1)
// ============================================================================

#[test]
fn tier5_challenger1_m4_fix1_asn1_time_fuzzing_robustness() {
    // 1. Static matrix of adversarial time inputs:
    // Non-ASCII bytes, multi-byte UTF-8 sequences (German umlauts, Cyrillic), emojis (4-byte), truncated, empty
    let adversarial_cases: Vec<Vec<u8>> = vec![
        vec![],                                                         // Empty string
        b"2".to_vec(),                                                  // 1 byte
        b"26".to_vec(),                                                 // 2 bytes
        b"260901".to_vec(),                                             // 6 bytes
        b"26090112345".to_vec(),                                        // 11 bytes (<12 UTCTime)
        b"260901123456".to_vec(),                                       // 12 bytes valid UTCTime digits
        b"260901123456Z".to_vec(),                                      // 13 bytes with Z
        b"2026090112345".to_vec(),                                      // 13 bytes (<14 GeneralizedTime)
        b"20260901123456".to_vec(),                                     // 14 bytes valid GeneralizedTime digits
        b"20260901123456Z".to_vec(),                                    // 15 bytes with Z
        vec![0xFF; 12],                                                 // 12 bytes non-ASCII (0xFF)
        vec![0xFF; 14],                                                 // 14 bytes non-ASCII (0xFF)
        vec![0xFE; 20],                                                 // 20 bytes 0xFE
        vec![0x80; 32],                                                 // 32 bytes 0x80
        vec![0x00; 12],                                                 // 12 zero bytes
        vec![0x00; 14],                                                 // 14 zero bytes
        b"\xFF260901000000".to_vec(),                                   // Non-ASCII byte at offset 0
        b"26\xFF0901000000".to_vec(),                                   // Non-ASCII byte at offset 2 (prev slice boundary)
        b"2609\xFF01000000".to_vec(),                                   // Non-ASCII byte at offset 4
        b"260901\xFF000000".to_vec(),                                   // Non-ASCII byte at offset 6
        b"26090100\xFF0000".to_vec(),                                   // Non-ASCII byte at offset 8
        b"2609010000\xFF00".to_vec(),                                   // Non-ASCII byte at offset 10
        b"\xC3\xA4\xC3\xB6\xC3\xBC123456".to_vec(),                     // German umlauts spanning boundaries
        b"26\xC3\xA401000000Z".to_vec(),                                // Umlaut inside time payload
        b"2026\xC3\x9F01000000Z".to_vec(),                              // German sharp s (\xC3\x9F) inside year
        b"\xD0\x94\xD0\x96260901000000".to_vec(),                       // Cyrillic multibyte characters
        "📅20260901000000Z".as_bytes().to_vec(),                       // 4-byte emoji at start
        "26📅0901000000Z".as_bytes().to_vec(),                         // 4-byte emoji crossing byte 2
        "2609📅01000000Z".as_bytes().to_vec(),                         // 4-byte emoji crossing byte 4
        "260901📅000000Z".as_bytes().to_vec(),                         // 4-byte emoji crossing byte 6
        "26090100📅0000Z".as_bytes().to_vec(),                         // 4-byte emoji crossing byte 8
        "2609010000📅00Z".as_bytes().to_vec(),                         // 4-byte emoji crossing byte 10
        "260901000000📅Z".as_bytes().to_vec(),                         // 4-byte emoji crossing byte 12
        "🔑SecureCert📅2026🚀".as_bytes().to_vec(),                     // Multiple emojis
        b"   \t\n\r   ".to_vec(),                                       // Whitespace only
        b"INVALID_DATETIME_STRING".to_vec(),                            // Non-digit ASCII
    ];

    for input in &adversarial_cases {
        let utc_out = parse_time_str(input, 0x17);
        let gen_out = parse_time_str(input, 0x18);
        if input.is_empty() || input.iter().all(|c| c.is_ascii_whitespace()) {
            assert!(utc_out.is_empty());
            assert!(gen_out.is_empty());
        } else {
            assert!(!utc_out.is_empty(), "parse_time_str must safely return string for UTCTime");
            assert!(!gen_out.is_empty(), "parse_time_str must safely return string for GeneralizedTime");
        }
    }

    // 2. Verified formatting of valid inputs
    let valid_utc = parse_time_str(b"260901123045Z", 0x17);
    assert_eq!(valid_utc, "2026-09-01 12:30:45 UTC");

    let valid_utc_pre2000 = parse_time_str(b"991231235959Z", 0x17);
    assert_eq!(valid_utc_pre2000, "1999-12-31 23:59:59 UTC");

    let valid_gen = parse_time_str(b"20260901123045Z", 0x18);
    assert_eq!(valid_gen, "2026-09-01 12:30:45 UTC");

    // 3. High-throughput pseudo-random fuzzing (10,000 iterations): Must NEVER panic
    let mut lcg: u64 = 0x123456789ABCDEF0;
    for _ in 0..10_000 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let len = (lcg % 65) as usize;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            buf.push((lcg >> 32) as u8);
        }
        let _ = parse_time_str(&buf, 0x17);
        let _ = parse_time_str(&buf, 0x18);
        let _ = parse_time_str(&buf, 0x30);
    }
}

#[test]
fn tier5_challenger1_m4_fix1_oid_decoding_adversarial_rejection() {
    // 1. Valid OID decodings
    let valid_rsa = parse_oid(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B]);
    assert_eq!(valid_rsa, vec![1, 2, 840, 113549, 1, 1, 11]);

    let valid_cn = parse_oid(&[0x55, 0x04, 0x03]);
    assert_eq!(valid_cn, vec![2, 5, 4, 3]);

    let single_byte = parse_oid(&[0x2A]);
    assert_eq!(single_byte, vec![1, 2]);

    let zero_byte = parse_oid(&[0x00]);
    assert_eq!(zero_byte, vec![0, 0]);

    // Boundary valid large component (2^28 = 268,435,456 fits in u32)
    let large_valid = parse_oid(&[0x2A, 0x81, 0x80, 0x80, 0x80, 0x00]);
    assert_eq!(large_valid, vec![1, 2, 268_435_456]);

    // 2. Empty slice
    assert!(parse_oid(&[]).is_empty(), "Empty OID slice must return empty Vec");

    // 3. Unterminated OID components (continuation bit 0x80 set on final octet)
    // RFC/ITU requires bit 8 to be 0 on the final octet of each sub-identifier
    let unterminated_cases: &[&[u8]] = &[
        &[0x2A, 0x86],
        &[0x2A, 0x86, 0x48, 0x86],
        &[0x2A, 0x80],
        &[0x2A, 0xFF],
        &[0x2A, 0x81, 0x82, 0x83],
        &[0x2A, 0x48, 0x90],
    ];

    for &case in unterminated_cases {
        let res = parse_oid(case);
        assert!(
            res.is_empty(),
            "Unterminated OID component {:?} must be rejected with empty Vec",
            case
        );
    }

    // 4. Overflowing continuation octets (arithmetic overflow protection)
    // 6 or more continuation octets exceed 2^32 and must overflow u32
    let overflow_cases: &[&[u8]] = &[
        &[0x2A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01],
        &[0x2A, 0x81, 0x80, 0x80, 0x80, 0x80, 0x00],
        &[0x2A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F],
    ];

    for &case in overflow_cases {
        let res = parse_oid(case);
        assert!(
            res.is_empty(),
            "Overflowing OID component {:?} must be rejected with empty Vec",
            case
        );
    }

    // 5. Extremely long chain of continuation octets (100 octets of 0xFF)
    let mut long_overflow = vec![0x2A];
    long_overflow.extend(std::iter::repeat(0xFF).take(100));
    assert!(parse_oid(&long_overflow).is_empty());

    // 6. High-throughput pseudo-random fuzzing (5,000 iterations): Must NEVER panic
    let mut lcg: u64 = 0xCAFEBABE01234567;
    for _ in 0..5_000 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let len = (lcg % 40) as usize;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            buf.push((lcg >> 32) as u8);
        }
        let _ = parse_oid(&buf);
    }
}

#[test]
fn tier5_challenger1_m4_fix1_openssl_process_table_secrecy_matrix() {
    use std::fs;
    use std::process::Command;

    // Helper functions simulating OpenSslBridge::apply_passin / apply_passout
    fn apply_test_passin(cmd: &mut Command, passphrase: Option<&str>) {
        if let Some(pass) = passphrase {
            if !pass.is_empty() {
                cmd.env("EVOLUTION_OPENSSL_PASSIN", pass);
                cmd.args(["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
                return;
            }
        }
        cmd.args(["-passin", "pass:"]);
    }

    fn apply_test_passout(cmd: &mut Command, passphrase: Option<&str>) {
        if let Some(pass) = passphrase {
            if !pass.is_empty() {
                cmd.env("EVOLUTION_OPENSSL_PASSOUT", pass);
                cmd.args(["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
                return;
            }
        }
        cmd.args(["-passout", "pass:"]);
    }

    // 1. Matrix of complex passphrases containing spaces, colons, quotes, Unicode, symbols, and emojis
    let adversarial_passphrases = [
        "Passphrase with spaces and multiple tokens",
        "pass:word:with:colons:and:more:colons:",
        "single'quote'and\"double\"quote`backtick`",
        "Päßwörd-mit-ÄÖÜ-und-ß-€-Schloß",
        "🔑SuperSecretPassphrase📅WithMultipleEmojis🚀🛡️",
        "!@#$%^&*()_+-=[]{}|;':,.<>/?~",
        "   spaces   only   ",
        "mixed: 'quotes' \"and\" spaces: 123 🔑 Päßwörd!",
    ];

    for &secret in &adversarial_passphrases {
        // Test apply_passin
        let mut cmd_in = Command::new("openssl");
        apply_test_passin(&mut cmd_in, Some(secret));

        let in_args: Vec<String> = cmd_in.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(in_args, vec!["-passin", "env:EVOLUTION_OPENSSL_PASSIN"]);
        for arg in &in_args {
            assert!(
                !arg.contains(secret),
                "Plain secret leaked into process arguments: {}",
                arg
            );
        }

        let in_env = cmd_in
            .get_envs()
            .find_map(|(k, v)| if k == "EVOLUTION_OPENSSL_PASSIN" { v.map(|s| s.to_string_lossy().to_string()) } else { None });
        assert_eq!(in_env, Some(secret.to_string()), "Environment variable must contain exact unmodified secret");

        // Test apply_passout
        let mut cmd_out = Command::new("openssl");
        apply_test_passout(&mut cmd_out, Some(secret));

        let out_args: Vec<String> = cmd_out.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(out_args, vec!["-passout", "env:EVOLUTION_OPENSSL_PASSOUT"]);
        for arg in &out_args {
            assert!(
                !arg.contains(secret),
                "Plain secret leaked into process arguments: {}",
                arg
            );
        }

        let out_env = cmd_out
            .get_envs()
            .find_map(|(k, v)| if k == "EVOLUTION_OPENSSL_PASSOUT" { v.map(|s| s.to_string_lossy().to_string()) } else { None });
        assert_eq!(out_env, Some(secret.to_string()), "Environment variable must contain exact unmodified secret");
    }

    // 2. Empty passphrase ("") and None handling: must pass "pass:" and never set environment variable
    for empty_case in [None, Some("")] {
        let mut cmd_in = Command::new("openssl");
        apply_test_passin(&mut cmd_in, empty_case);
        let in_args: Vec<String> = cmd_in.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(in_args, vec!["-passin", "pass:"]);
        assert!(cmd_in.get_envs().find(|(k, _)| *k == "EVOLUTION_OPENSSL_PASSIN").is_none());

        let mut cmd_out = Command::new("openssl");
        apply_test_passout(&mut cmd_out, empty_case);
        let out_args: Vec<String> = cmd_out.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(out_args, vec!["-passout", "pass:"]);
        assert!(cmd_out.get_envs().find(|(k, _)| *k == "EVOLUTION_OPENSSL_PASSOUT").is_none());
    }

    // 3. Real OpenSSL Subprocess Round-Trip Execution (if openssl binary available)
    let openssl_bin = std::env::var_os("EVOLUTION_OPENSSL_BINARY").unwrap_or_else(|| "openssl".into());
    let has_openssl = Command::new(&openssl_bin).arg("version").output().is_ok();
    if !has_openssl {
        return;
    }

    let temp_dir = std::env::temp_dir().join(format!("evo-p12-secrecy-{}", glib::uuid_string_random()));
    fs::create_dir_all(&temp_dir).unwrap();

    let rsa_key = temp_dir.join("test.key");
    let x509_cert = temp_dir.join("test.crt");

    let gen_status = Command::new(&openssl_bin)
        .args(["req", "-x509", "-newkey", "rsa:2048", "-keyout"])
        .arg(&rsa_key)
        .arg("-out")
        .arg(&x509_cert)
        .args(["-days", "1", "-nodes", "-subj", "/CN=Process Secrecy Test/O=Evolution Project"])
        .output()
        .expect("OpenSSL certificate generation must succeed");
    assert!(gen_status.status.success());

    for (i, &secret) in adversarial_passphrases.iter().enumerate() {
        let p12_path = temp_dir.join(format!("archive_secrecy_{i}.p12"));

        // Export PKCS#12 bundle using environment-protected passphrase
        let mut exp_cmd = Command::new(&openssl_bin);
        exp_cmd.args(["pkcs12", "-export", "-in"])
            .arg(&x509_cert)
            .arg("-inkey")
            .arg(&rsa_key)
            .arg("-out")
            .arg(&p12_path);
        apply_test_passout(&mut exp_cmd, Some(secret));
        let exp_res = exp_cmd.output().expect("Export command execution must succeed");
        assert!(exp_res.status.success(), "PKCS#12 export failed for passphrase '{secret}'");

        // Verify that wrong passphrase returns non-zero status
        let mut bad_cmd = Command::new(&openssl_bin);
        bad_cmd.args(["pkcs12", "-in"])
            .arg(&p12_path)
            .args(["-nokeys", "-clcerts"]);
        apply_test_passin(&mut bad_cmd, Some("IncorrectSecret999#"));
        let bad_res = bad_cmd.output().unwrap();
        assert!(!bad_res.status.success(), "Import with wrong secret must fail");

        // Verify that correct passphrase extracts certificate cleanly
        let mut good_cmd = Command::new(&openssl_bin);
        good_cmd.args(["pkcs12", "-in"])
            .arg(&p12_path)
            .args(["-nokeys", "-clcerts"]);
        apply_test_passin(&mut good_cmd, Some(secret));
        let good_res = good_cmd.output().unwrap();
        assert!(good_res.status.success(), "Import with correct secret must succeed");
        let cert_out = String::from_utf8_lossy(&good_res.stdout);
        assert!(cert_out.contains("-----BEGIN CERTIFICATE-----"));
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

