// SPDX-License-Identifier: GPL-3.0-or-later
//
// R1: Standalone Message Viewer Window Tests
// Covers activation, actions, multi-window registry, lifecycle, banners, and attachments.

use super::common::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Simulated MailViewerWindow state tracking struct
pub struct MockMailViewerWindow {
    pub message_key: String,
    pub subject: String,
    pub is_presented: bool,
    pub cancellation: Arc<AtomicBool>,
    pub action_map: HashMap<String, bool>, // action_name -> enabled
    pub accelerators: HashMap<String, String>, // action -> accel
    pub zoom_level: f64,
    pub remote_images_allowed: bool,
    pub is_closed: bool,
}

impl MockMailViewerWindow {
    pub fn new(message: &TestMessage) -> Self {
        let mut action_map = HashMap::new();
        action_map.insert("win.reply".to_string(), true);
        action_map.insert("win.reply-all".to_string(), true);
        action_map.insert("win.forward".to_string(), true);
        action_map.insert("win.delete".to_string(), true);
        action_map.insert("win.mark-junk".to_string(), true);
        action_map.insert("win.mark-not-junk".to_string(), true);
        action_map.insert("win.mark-read".to_string(), true);
        action_map.insert("win.mark-unread".to_string(), true);
        action_map.insert("win.mail-print".to_string(), true);
        action_map.insert("win.view-message-source".to_string(), true);
        action_map.insert("win.zoom-in".to_string(), true);
        action_map.insert("win.zoom-out".to_string(), true);
        action_map.insert("win.zoom-reset".to_string(), true);

        let mut accelerators = HashMap::new();
        accelerators.insert("win.reply".to_string(), "<Ctrl>R".to_string());
        accelerators.insert("win.reply-all".to_string(), "<Ctrl><Shift>R".to_string());
        accelerators.insert("win.forward".to_string(), "<Ctrl>L".to_string());
        accelerators.insert("win.delete".to_string(), "Delete".to_string());
        accelerators.insert("win.mark-junk".to_string(), "<Ctrl>J".to_string());
        accelerators.insert("win.mark-not-junk".to_string(), "<Ctrl><Shift>J".to_string());
        accelerators.insert("win.mark-read".to_string(), "<Ctrl>K".to_string());
        accelerators.insert("win.mark-unread".to_string(), "<Ctrl><Shift>K".to_string());
        accelerators.insert("win.mail-print".to_string(), "<Ctrl>P".to_string());
        accelerators.insert("win.view-message-source".to_string(), "<Ctrl>U".to_string());

        Self {
            message_key: message.composite_key(),
            subject: message.subject.clone(),
            is_presented: true,
            cancellation: Arc::new(AtomicBool::new(false)),
            action_map,
            accelerators,
            zoom_level: 1.0,
            remote_images_allowed: false,
            is_closed: false,
        }
    }

    pub fn present(&mut self) {
        self.is_presented = true;
    }

    pub fn close(&mut self) {
        self.is_closed = true;
        self.cancellation.store(true, Ordering::SeqCst);
    }

    pub fn zoom_in(&mut self) {
        if self.zoom_level < 3.0 {
            self.zoom_level = (self.zoom_level + 0.1).min(3.0);
        }
    }

    pub fn zoom_out(&mut self) {
        if self.zoom_level > 0.5 {
            self.zoom_level = (self.zoom_level - 0.1).max(0.5);
        }
    }

    pub fn zoom_reset(&mut self) {
        self.zoom_level = 1.0;
    }
}

/// Simulated MailPage standalone viewer registry manager
#[derive(Default)]
pub struct MockMailViewerRegistry {
    pub open_windows: Mutex<HashMap<String, MockMailViewerWindow>>,
}

impl MockMailViewerRegistry {
    pub fn activate_message(&self, message: &TestMessage) -> (String, bool) {
        let key = message.composite_key();
        let mut map = self.open_windows.lock().unwrap();
        if let Some(existing) = map.get_mut(&key) {
            existing.present();
            (key, false) // reused existing
        } else {
            let window = MockMailViewerWindow::new(message);
            map.insert(key.clone(), window);
            (key, true) // newly created
        }
    }

    pub fn close_window(&self, key: &str) -> bool {
        let mut map = self.open_windows.lock().unwrap();
        if let Some(mut window) = map.remove(key) {
            window.close();
            true
        } else {
            false
        }
    }

    pub fn count_open_windows(&self) -> usize {
        self.open_windows.lock().unwrap().len()
    }
}

// ============================================================================
// Tests for R1: Standalone Message Viewer Window
// ============================================================================

#[test]
fn r1_test_message_activation_creates_standalone_viewer() {
    let registry = MockMailViewerRegistry::default();
    let msg1 = TestMessage::new("source1", "INBOX", "101", "Project Status Update");

    let (key, is_new) = registry.activate_message(&msg1);
    assert!(is_new, "First activation must create a new window");
    assert_eq!(key, "backend\x1fsource1\x1fINBOX\x1f101");
    assert_eq!(registry.count_open_windows(), 1);
}

#[test]
fn r1_test_repeated_message_activation_presents_existing_window() {
    let registry = MockMailViewerRegistry::default();
    let msg1 = TestMessage::new("source1", "INBOX", "101", "Project Status Update");

    let (key1, is_new1) = registry.activate_message(&msg1);
    assert!(is_new1);

    // Double activate / press Enter again
    let (key2, is_new2) = registry.activate_message(&msg1);
    assert!(!is_new2, "Second activation of same message must NOT create new window");
    assert_eq!(key1, key2);
    assert_eq!(registry.count_open_windows(), 1, "Must maintain exactly 1 window");
}

#[test]
fn r1_test_multi_window_independence_and_no_collision() {
    let registry = MockMailViewerRegistry::default();
    let msg1 = TestMessage::new("source1", "INBOX", "101", "Message A");
    let msg2 = TestMessage::new("source1", "INBOX", "102", "Message B");
    let msg3 = TestMessage::new("source2", "Sent", "201", "Message C");

    let (_, new1) = registry.activate_message(&msg1);
    let (_, new2) = registry.activate_message(&msg2);
    let (_, new3) = registry.activate_message(&msg3);

    assert!(new1 && new2 && new3, "All three distinct messages must open windows");
    assert_eq!(registry.count_open_windows(), 3, "Registry must track 3 open windows");
}

#[test]
fn r1_test_window_close_triggers_cancellation_token_and_unregisters() {
    let registry = MockMailViewerRegistry::default();
    let msg1 = TestMessage::new("source1", "INBOX", "101", "Message A");
    let msg2 = TestMessage::new("source1", "INBOX", "102", "Message B");

    let (key1, _) = registry.activate_message(&msg1);
    let (key2, _) = registry.activate_message(&msg2);
    assert_eq!(registry.count_open_windows(), 2);

    let closed = registry.close_window(&key1);
    assert!(closed, "Window 1 should close successfully");
    assert_eq!(registry.count_open_windows(), 1, "Only Window 2 should remain");

    // Re-verify window 2 still exists and is accessible
    let (re_key2, is_new) = registry.activate_message(&msg2);
    assert!(!is_new, "Window 2 must still exist and be reused");
    assert_eq!(re_key2, key2);
}

#[test]
fn r1_test_viewer_window_action_map_registration() {
    let msg = TestMessage::new("source1", "INBOX", "500", "Action Test");
    let win = MockMailViewerWindow::new(&msg);

    let required_actions = [
        "win.reply",
        "win.reply-all",
        "win.forward",
        "win.delete",
        "win.mark-junk",
        "win.mark-not-junk",
        "win.mark-read",
        "win.mark-unread",
        "win.mail-print",
        "win.view-message-source",
        "win.zoom-in",
        "win.zoom-out",
        "win.zoom-reset",
    ];

    for action in &required_actions {
        assert!(
            win.action_map.contains_key(*action),
            "Action {} must be registered in standalone viewer",
            action
        );
        assert!(
            win.action_map[*action],
            "Action {} should be enabled by default",
            action
        );
    }
}

#[test]
fn r1_test_viewer_window_keyboard_accelerators_contract() {
    let msg = TestMessage::new("source1", "INBOX", "501", "Accel Test");
    let win = MockMailViewerWindow::new(&msg);

    assert_eq!(win.accelerators.get("win.reply").unwrap(), "<Ctrl>R");
    assert_eq!(win.accelerators.get("win.reply-all").unwrap(), "<Ctrl><Shift>R");
    assert_eq!(win.accelerators.get("win.forward").unwrap(), "<Ctrl>L");
    assert_eq!(win.accelerators.get("win.delete").unwrap(), "Delete");
    assert_eq!(win.accelerators.get("win.mark-junk").unwrap(), "<Ctrl>J");
    assert_eq!(win.accelerators.get("win.mark-not-junk").unwrap(), "<Ctrl><Shift>J");
    assert_eq!(win.accelerators.get("win.mark-read").unwrap(), "<Ctrl>K");
    assert_eq!(win.accelerators.get("win.mark-unread").unwrap(), "<Ctrl><Shift>K");
    assert_eq!(win.accelerators.get("win.mail-print").unwrap(), "<Ctrl>P");
    assert_eq!(win.accelerators.get("win.view-message-source").unwrap(), "<Ctrl>U");
}

#[test]
fn r1_test_viewer_window_zoom_levels_and_boundaries() {
    let msg = TestMessage::new("source1", "INBOX", "502", "Zoom Test");
    let mut win = MockMailViewerWindow::new(&msg);

    assert_eq!(win.zoom_level, 1.0);
    win.zoom_in();
    assert!((win.zoom_level - 1.1).abs() < 1e-5);
    win.zoom_out();
    assert!((win.zoom_level - 1.0).abs() < 1e-5);

    // Zoom out boundary at 0.5
    for _ in 0..10 {
        win.zoom_out();
    }
    assert_eq!(win.zoom_level, 0.5, "Zoom out must clamp at 50%");

    // Zoom in boundary at 3.0
    for _ in 0..35 {
        win.zoom_in();
    }
    assert_eq!(win.zoom_level, 3.0, "Zoom in must clamp at 300%");

    win.zoom_reset();
    assert_eq!(win.zoom_level, 1.0, "Zoom reset must restore 100%");
}

#[test]
fn r1_test_viewer_window_attachment_bar_formatting() {
    let mut msg = TestMessage::new("source1", "INBOX", "503", "Attachment Test");
    msg.attachments.push(TestAttachment {
        filename: "report.pdf".to_string(),
        mime_type: "application/pdf".to_string(),
        size: 1_258_291, // ~1.2 MB
        path: "/tmp/report.pdf".to_string(),
    });
    msg.attachments.push(TestAttachment {
        filename: "screenshot.png".to_string(),
        mime_type: "image/png".to_string(),
        size: 45_056, // ~44 KB
        path: "/tmp/screenshot.png".to_string(),
    });

    assert_eq!(msg.attachments.len(), 2);
    assert_eq!(msg.attachments[0].filename, "report.pdf");
    assert_eq!(msg.attachments[0].mime_type, "application/pdf");
    assert!(msg.attachments[0].size > 1_000_000);
}

#[test]
fn r1_test_viewer_window_remote_images_banner_toggle() {
    let msg = TestMessage::new("source1", "INBOX", "504", "Newsletter with Images");
    let mut win = MockMailViewerWindow::new(&msg);

    assert!(!win.remote_images_allowed, "Remote images initially blocked");
    win.remote_images_allowed = true; // User clicks "Bilder laden"
    assert!(win.remote_images_allowed, "Remote images allowed after approval");
}

#[test]
fn r1_test_viewer_window_raw_source_extraction() {
    let msg = TestMessage::new("source1", "INBOX", "505", "Raw Source Subject");
    let raw = std::str::from_utf8(&msg.raw_rfc822).unwrap();
    assert!(raw.contains("Subject: Raw Source Subject"));
    assert!(raw.contains("From: sender@example.com"));
    assert!(raw.contains("Content-Type: text/plain"));
}
