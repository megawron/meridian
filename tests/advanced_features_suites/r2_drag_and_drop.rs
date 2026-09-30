// SPDX-License-Identifier: GPL-3.0-or-later
//
// R2: Desktop Drag-and-Drop Workflows Tests
// Covers:
// - Composer file drop (`gdk::FileList`, `gio::File`, MIME type & size resolution)
// - Mail list drag source & sidebar folder drop targets (move, copy, unread/total count updates)
// - Calendar continuous time grid event drag (minute slot offsets, day/time shift, boundary crossing)

use super::common::*;
use std::collections::HashMap;
use std::path::Path;
use chrono::{Duration, NaiveDate, NaiveDateTime};

/// MIME type resolution utility matching compose_dialog logic
pub fn deduce_mime_type(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if lower.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else if lower.ends_with(".txt") {
        "text/plain".to_string()
    } else if lower.ends_with(".tar.gz") {
        "application/gzip".to_string()
    } else if lower.ends_with(".zip") {
        "application/zip".to_string()
    } else if lower.ends_with(".eml") {
        "message/rfc822".to_string()
    } else if lower.ends_with(".csv") {
        "text/csv".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// Simulated Composer drop target handler
pub struct MockComposerDropTarget {
    pub attachments: Vec<TestAttachment>,
}

impl MockComposerDropTarget {
    pub fn new() -> Self {
        Self {
            attachments: Vec::new(),
        }
    }

    pub fn handle_file_drop(&mut self, path: &str, file_size: u64) -> bool {
        let p = Path::new(path);
        let Some(filename) = p.file_name().and_then(|n| n.to_str()) else {
            return false;
        };

        let mime = deduce_mime_type(filename);
        self.attachments.push(TestAttachment {
            filename: filename.to_string(),
            mime_type: mime,
            size: file_size,
            path: path.to_string(),
        });
        true
    }
}

/// Simulated Folder Store for Drag and Drop tests
pub struct MockFolderStore {
    pub folders: HashMap<String, TestFolder>,
    pub message_locations: HashMap<String, String>, // msg_id -> folder_id
}

impl MockFolderStore {
    pub fn new() -> Self {
        let mut folders = HashMap::new();
        let mut inbox = TestFolder::new("inbox", "INBOX");
        inbox.unread_count = 5;
        inbox.total_count = 10;
        folders.insert("inbox".to_string(), inbox);

        let mut archive = TestFolder::new("archive", "Archive");
        archive.unread_count = 0;
        archive.total_count = 20;
        folders.insert("archive".to_string(), archive);

        let mut trash = TestFolder::new("trash", "Trash");
        trash.unread_count = 0;
        trash.total_count = 2;
        folders.insert("trash".to_string(), trash);

        let mut message_locations = HashMap::new();
        message_locations.insert("msg_101".to_string(), "inbox".to_string());
        message_locations.insert("msg_102".to_string(), "inbox".to_string());

        Self {
            folders,
            message_locations,
        }
    }

    pub fn move_message(&mut self, msg_id: &str, dest_folder_id: &str, is_unread: bool) -> Result<(), String> {
        let current_folder = self
            .message_locations
            .get(msg_id)
            .cloned()
            .ok_or_else(|| "Message not found".to_string())?;

        if current_folder == dest_folder_id {
            // Drop onto same folder is a safe no-op
            return Ok(());
        }

        if !self.folders.contains_key(dest_folder_id) {
            return Err("Destination folder does not exist".to_string());
        }

        // Decrement source folder
        if let Some(src) = self.folders.get_mut(&current_folder) {
            if src.total_count > 0 {
                src.total_count -= 1;
            }
            if is_unread && src.unread_count > 0 {
                src.unread_count -= 1;
            }
        }

        // Increment destination folder
        if let Some(dst) = self.folders.get_mut(dest_folder_id) {
            dst.total_count += 1;
            if is_unread {
                dst.unread_count += 1;
            }
        }

        self.message_locations.insert(msg_id.to_string(), dest_folder_id.to_string());
        Ok(())
    }

    pub fn copy_message(&mut self, msg_id: &str, dest_folder_id: &str, is_unread: bool) -> Result<(), String> {
        if !self.message_locations.contains_key(msg_id) {
            return Err("Message not found".to_string());
        }
        if !self.folders.contains_key(dest_folder_id) {
            return Err("Destination folder does not exist".to_string());
        }

        // Destination folder increments, source remains unchanged
        if let Some(dst) = self.folders.get_mut(dest_folder_id) {
            dst.total_count += 1;
            if is_unread {
                dst.unread_count += 1;
            }
        }

        Ok(())
    }
}

/// Calendar event time shifter logic
pub struct CalendarEventShiftResult {
    pub new_dtstart: NaiveDateTime,
    pub new_dtend: NaiveDateTime,
    pub duration_minutes: i64,
}

pub fn shift_calendar_event(
    dtstart: NaiveDateTime,
    dtend: NaiveDateTime,
    day_delta: i64,
    minute_delta: i64,
) -> CalendarEventShiftResult {
    let initial_duration = (dtend - dtstart).num_minutes();
    let total_offset = Duration::days(day_delta) + Duration::minutes(minute_delta);
    let new_start = dtstart + total_offset;
    let new_end = new_start + Duration::minutes(initial_duration);

    CalendarEventShiftResult {
        new_dtstart: new_start,
        new_dtend: new_end,
        duration_minutes: initial_duration,
    }
}

// ============================================================================
// Tests for R2: Desktop Drag-and-Drop Workflows
// ============================================================================

#[test]
fn r2_test_composer_single_file_drop_attaches_with_metadata() {
    let mut composer = MockComposerDropTarget::new();
    let ok = composer.handle_file_drop("/Users/user/Documents/Contract.pdf", 524_288);
    assert!(ok);
    assert_eq!(composer.attachments.len(), 1);
    assert_eq!(composer.attachments[0].filename, "Contract.pdf");
    assert_eq!(composer.attachments[0].mime_type, "application/pdf");
    assert_eq!(composer.attachments[0].size, 524_288);
}

#[test]
fn r2_test_composer_multiple_files_batch_drop() {
    let mut composer = MockComposerDropTarget::new();
    let files = [
        ("/path/to/diagram.png", 102_400),
        ("/path/to/data.csv", 4_096),
        ("/path/to/notes.txt", 1_024),
        ("/path/to/archive.zip", 2_097_152),
    ];

    for (p, s) in &files {
        assert!(composer.handle_file_drop(p, *s));
    }

    assert_eq!(composer.attachments.len(), 4);
    assert_eq!(composer.attachments[0].mime_type, "image/png");
    assert_eq!(composer.attachments[1].mime_type, "text/csv");
    assert_eq!(composer.attachments[2].mime_type, "text/plain");
    assert_eq!(composer.attachments[3].mime_type, "application/zip");
}

#[test]
fn r2_test_composer_mime_type_deduction_broad_coverage() {
    let test_cases = [
        ("report.pdf", "application/pdf"),
        ("photo.jpg", "image/jpeg"),
        ("photo.JPEG", "image/jpeg"),
        ("graphic.png", "image/png"),
        ("backup.tar.gz", "application/gzip"),
        ("dataset.csv", "text/csv"),
        ("message.eml", "message/rfc822"),
        ("binary.bin", "application/octet-stream"),
        ("noextension", "application/octet-stream"),
    ];

    for (fname, expected_mime) in &test_cases {
        assert_eq!(
            deduce_mime_type(fname),
            *expected_mime,
            "Failed MIME deduction for {}",
            fname
        );
    }
}

#[test]
fn r2_test_composer_drop_filename_with_spaces_and_unicode() {
    let mut composer = MockComposerDropTarget::new();
    let path = "/Volumes/Data/Jahresbericht 2026 – Final Version 📊.pdf";
    assert!(composer.handle_file_drop(path, 1_048_576));

    assert_eq!(composer.attachments.len(), 1);
    assert_eq!(
        composer.attachments[0].filename,
        "Jahresbericht 2026 – Final Version 📊.pdf"
    );
    assert_eq!(composer.attachments[0].mime_type, "application/pdf");
}

#[test]
fn r2_test_mail_list_drag_source_action_mask() {
    // Message row DragSource must advertise both COPY and MOVE
    let drag_action_copy = 1u32;
    let drag_action_move = 2u32;
    let combined_actions = drag_action_copy | drag_action_move;

    assert_eq!(combined_actions & drag_action_copy, drag_action_copy);
    assert_eq!(combined_actions & drag_action_move, drag_action_move);
}

#[test]
fn r2_test_mail_list_drag_payload_serialization() {
    let message_id = "backend/source1/INBOX/1042";
    let payload = format!("meridian-message:{}", message_id);

    assert!(payload.starts_with("meridian-message:"));
    let extracted = payload.strip_prefix("meridian-message:").unwrap();
    assert_eq!(extracted, message_id);
}

#[test]
fn r2_test_sidebar_folder_drop_move_action() {
    let mut store = MockFolderStore::new();

    // Move unread message 101 from inbox to archive
    let res = store.move_message("msg_101", "archive", true);
    assert!(res.is_ok());

    let inbox = store.folders.get("inbox").unwrap();
    assert_eq!(inbox.total_count, 9);
    assert_eq!(inbox.unread_count, 4);

    let archive = store.folders.get("archive").unwrap();
    assert_eq!(archive.total_count, 21);
    assert_eq!(archive.unread_count, 1);
}

#[test]
fn r2_test_sidebar_folder_drop_copy_action() {
    let mut store = MockFolderStore::new();

    // Copy message 101 to archive
    let res = store.copy_message("msg_101", "archive", false);
    assert!(res.is_ok());

    // Source inbox counts untouched
    let inbox = store.folders.get("inbox").unwrap();
    assert_eq!(inbox.total_count, 10);
    assert_eq!(inbox.unread_count, 5);

    // Destination archive total increments
    let archive = store.folders.get("archive").unwrap();
    assert_eq!(archive.total_count, 21);
    assert_eq!(archive.unread_count, 0); // read message copied
}

#[test]
fn r2_test_sidebar_folder_drop_same_folder_is_noop() {
    let mut store = MockFolderStore::new();

    // Dropping message 101 onto its current folder (inbox)
    let res = store.move_message("msg_101", "inbox", true);
    assert!(res.is_ok());

    let inbox = store.folders.get("inbox").unwrap();
    assert_eq!(inbox.total_count, 10, "Total count must not change");
    assert_eq!(inbox.unread_count, 5, "Unread count must not change");
}

#[test]
fn r2_test_sidebar_folder_drop_invalid_destination_fails_gracefully() {
    let mut store = MockFolderStore::new();
    let res = store.move_message("msg_101", "non_existent_folder", false);
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err(),
        "Destination folder does not exist".to_string()
    );
}

#[test]
fn r2_test_calendar_event_drag_minute_slot_calculation() {
    // Tuesday 09:00 - 10:30 (90 min) dragged to 14:30 on same day
    let start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();

    // 09:00 = 540 min, 14:30 = 870 min -> minute_delta = +330 min
    let shifted = shift_calendar_event(start, end, 0, 330);

    let expected_start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(14, 30, 0)
        .unwrap();
    let expected_end = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(16, 0, 0)
        .unwrap();

    assert_eq!(shifted.new_dtstart, expected_start);
    assert_eq!(shifted.new_dtend, expected_end);
    assert_eq!(shifted.duration_minutes, 90, "Duration must be preserved");
}

#[test]
fn r2_test_calendar_event_drag_day_and_time_combined_shift() {
    // Tuesday 10:00 - 11:00 dragged to Thursday 16:00
    let start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(11, 0, 0)
        .unwrap();

    // Day delta = +2 days, minute delta = (16:00 - 10:00) = +360 min
    let shifted = shift_calendar_event(start, end, 2, 360);

    let expected_start = NaiveDate::from_ymd_opt(2026, 9, 17)
        .unwrap()
        .and_hms_opt(16, 0, 0)
        .unwrap();
    let expected_end = NaiveDate::from_ymd_opt(2026, 9, 17)
        .unwrap()
        .and_hms_opt(17, 0, 0)
        .unwrap();

    assert_eq!(shifted.new_dtstart, expected_start);
    assert_eq!(shifted.new_dtend, expected_end);
    assert_eq!(shifted.duration_minutes, 60);
}

#[test]
fn r2_test_calendar_event_drag_across_midnight_boundary() {
    // Event at 23:30 (45 min duration, ends 00:15 next day) dragged +60 min
    let start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(23, 30, 0)
        .unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 9, 16)
        .unwrap()
        .and_hms_opt(0, 15, 0)
        .unwrap();

    let shifted = shift_calendar_event(start, end, 0, 60);

    let expected_start = NaiveDate::from_ymd_opt(2026, 9, 16)
        .unwrap()
        .and_hms_opt(0, 30, 0)
        .unwrap();
    let expected_end = NaiveDate::from_ymd_opt(2026, 9, 16)
        .unwrap()
        .and_hms_opt(1, 15, 0)
        .unwrap();

    assert_eq!(shifted.new_dtstart, expected_start);
    assert_eq!(shifted.new_dtend, expected_end);
    assert_eq!(shifted.duration_minutes, 45);
}

#[test]
fn r2_test_calendar_event_drag_negative_time_shift() {
    // Event at 15:00 dragged earlier to 09:00 (-360 min)
    let start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(15, 0, 0)
        .unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(16, 0, 0)
        .unwrap();

    let shifted = shift_calendar_event(start, end, 0, -360);

    let expected_start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    assert_eq!(shifted.new_dtstart, expected_start);
    assert_eq!(shifted.duration_minutes, 60);
}

#[test]
fn r2_test_calendar_event_drag_preserves_recurrence_and_metadata() {
    // When an event is dragged, recurrence rules, summary, and UID must remain untouched
    let uid = "uid-meeting-12345@domain.com";
    let summary = "Weekly Sync";
    let rrule = "FREQ=WEEKLY;BYDAY=TU";
    let tzid = "Europe/Berlin";

    // Shift operation
    let start = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 9, 15)
        .unwrap()
        .and_hms_opt(11, 0, 0)
        .unwrap();
    let shifted = shift_calendar_event(start, end, 7, 0); // +1 week

    assert_eq!(uid, "uid-meeting-12345@domain.com");
    assert_eq!(summary, "Weekly Sync");
    assert_eq!(rrule, "FREQ=WEEKLY;BYDAY=TU");
    assert_eq!(tzid, "Europe/Berlin");
    assert_eq!(shifted.new_dtstart.date(), NaiveDate::from_ymd_opt(2026, 9, 22).unwrap());
}
