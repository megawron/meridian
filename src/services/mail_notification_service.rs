// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailNotificationService — desktop and audio notifications for incoming email,
// reproducing the exact behavior of GNOME Evolution's mail-notification plugin.

use adw::prelude::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::i18n::{gettext, ngettext};
use crate::services::settings_service::AppSettings;

/// Throttle interval between sound alerts in seconds (identical to Evolution's NOTIFY_THROTTLE = 30)
pub const NOTIFY_THROTTLE_SECONDS: u64 = 30;

/// Standard notification ID used for replacement/update in notification daemon
pub const MAIL_NOTIFICATION_ID: &str = "meridian-new-mail";

/// Structured target for notification actions (open mail, mark read)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationActionTarget {
    pub source_uid: String,
    pub folder_name: String,
    pub message_uid: Option<String>,
    pub message_id: Option<String>,
}

impl NotificationActionTarget {
    pub fn new(
        source_uid: impl Into<String>,
        folder_name: impl Into<String>,
        message_uid: Option<String>,
        message_id: Option<String>,
    ) -> Self {
        Self {
            source_uid: source_uid.into(),
            folder_name: folder_name.into(),
            message_uid,
            message_id,
        }
    }

    /// Format as stable target string: `eds:<source_uid>:<folder_name>:<message_uid>:<message_id>`
    pub fn to_target_string(&self) -> String {
        format!(
            "eds:{}:{}:{}:{}",
            self.source_uid,
            self.folder_name,
            self.message_uid.as_deref().unwrap_or(""),
            self.message_id.as_deref().unwrap_or("")
        )
    }

    /// Parse from target string
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.splitn(5, ':').collect();
        if parts.len() < 3 || parts[0] != "eds" {
            return None;
        }
        let source_uid = parts[1].to_string();
        let folder_name = parts[2].to_string();
        let message_uid = parts.get(3).filter(|u| !u.is_empty()).map(|u| u.to_string());
        let message_id = parts.get(4).filter(|i| !i.is_empty()).map(|i| i.to_string());

        Some(Self {
            source_uid,
            folder_name,
            message_uid,
            message_id,
        })
    }
}

/// Summary of new mail arriving in a folder
#[derive(Debug, Clone)]
pub struct NewMailSummary {
    pub source_uid: String,
    pub source_display_name: Option<String>,
    pub folder_name: String,
    pub folder_display_name: Option<String>,
    pub is_inbox: bool,
    pub new_messages_count: usize,
    pub latest_message_uid: Option<String>,
    pub latest_message_id: Option<String>,
    pub latest_sender: Option<String>,
    pub latest_subject: Option<String>,
}

/// Internal state tracking accumulated new message count and sound throttling
struct NotificationState {
    last_sound_time: Option<Instant>,
}

static NOTIFICATION_STATE: Mutex<Option<NotificationState>> = Mutex::new(None);
static ACCUMULATED_STATUS_COUNT: AtomicUsize = AtomicUsize::new(0);

pub struct MailNotificationService;

impl MailNotificationService {
    /// Return the current accumulated count of new messages notified
    pub fn status_count() -> usize {
        ACCUMULATED_STATUS_COUNT.load(Ordering::Relaxed)
    }

    /// Reset the accumulated status count (e.g. when notifications are withdrawn)
    pub fn reset_status_count() {
        ACCUMULATED_STATUS_COUNT.store(0, Ordering::Relaxed);
    }

    /// Check if a folder name represents an Inbox
    pub fn is_inbox_folder(folder_name: &str) -> bool {
        let lower = folder_name.trim().to_lowercase();
        lower == "inbox"
            || lower == "posteingang"
            || lower == "courrier entrant"
            || lower == "bandeja de entrada"
            || lower == "posta in arrivo"
            || lower.ends_with("/inbox")
            || lower.ends_with("/posteingang")
    }

    /// Determine if an account should be notified according to excluded accounts
    pub fn should_notify_account(source_uid: &str, not_accounts: &[String]) -> bool {
        let trimmed = source_uid.trim();
        !not_accounts.iter().any(|uid| uid.trim() == trimmed)
    }

    /// Determine if a folder should trigger notifications based on the only-inbox setting
    pub fn should_notify_folder(folder_name: &str, is_inbox: bool, only_inbox: bool) -> bool {
        if !only_inbox {
            return true;
        }
        is_inbox || Self::is_inbox_folder(folder_name)
    }

    /// Format notification summary/title (Evolution: "New email in Evolution")
    pub fn notification_summary() -> String {
        gettext("New email in Meridian")
    }

    /// Format notification body matching Evolution's `new_notify_status`:
    /// ```text
    /// You have received %d new message.
    /// From: %s
    /// Subject: %s
    /// Folder: %s
    /// (and %d more)
    /// ```
    pub fn format_notification_body(
        status_count: usize,
        sender: Option<&str>,
        subject: Option<&str>,
        folder_display_name: Option<&str>,
    ) -> String {
        let mut text = String::with_capacity(256);

        // Line 1: message count
        let count_str = status_count.to_string();
        let count_line = ngettext(
            "You have received %d new message.",
            "You have received %d new messages.",
            status_count as u32,
        );
        let count_line = if count_line.contains("%d") {
            count_line.replace("%d", &count_str)
        } else {
            count_line
        };
        text.push_str(&count_line);

        // Line 2: Sender (if available)
        if let Some(s) = sender.filter(|s| !s.trim().is_empty()) {
            text.push('\n');
            let tmpl = gettext("From: %s");
            let line = if tmpl.contains("%s") {
                tmpl.replace("%s", s.trim())
            } else {
                format!("{}: {}", gettext("From"), s.trim())
            };
            text.push_str(&line);
        }

        // Line 3: Subject (if available)
        if let Some(sub) = subject.filter(|s| !s.trim().is_empty()) {
            text.push('\n');
            let tmpl = gettext("Subject: %s");
            let line = if tmpl.contains("%s") {
                tmpl.replace("%s", sub.trim())
            } else {
                format!("{}: {}", gettext("Subject"), sub.trim())
            };
            text.push_str(&line);
        }

        // Line 4: Folder name (if available)
        if let Some(folder) = folder_display_name.filter(|f| !f.trim().is_empty()) {
            text.push('\n');
            let tmpl = gettext("Folder: %s");
            let line = if tmpl.contains("%s") {
                tmpl.replace("%s", folder.trim())
            } else {
                format!("{}: {}", gettext("Folder"), folder.trim())
            };
            text.push_str(&line);
        }

        // Line 5: Additional messages hint if status_count > 1 and details were shown
        if status_count > 1 && (sender.is_some() || subject.is_some()) {
            let additional = status_count - 1;
            let add_str = additional.to_string();
            text.push('\n');
            let tmpl = ngettext("(and %d more)", "(and %d more)", additional as u32);
            let line = if tmpl.contains("%d") {
                tmpl.replace("%d", &add_str)
            } else {
                tmpl
            };
            text.push_str(&line);
        }

        text
    }

    /// Check if audio notification should be played (with 30-second throttling)
    pub fn should_play_sound(now: Instant) -> bool {
        let mut guard = NOTIFICATION_STATE.lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.get_or_insert_with(|| NotificationState {
            last_sound_time: None,
        });

        if let Some(last_time) = state.last_sound_time {
            if now.duration_since(last_time) < Duration::from_secs(NOTIFY_THROTTLE_SECONDS) {
                return false;
            }
        }

        state.last_sound_time = Some(now);
        true
    }

    /// Play sound according to settings (beep, custom file, or system theme event)
    pub fn play_notification_sound(settings: &AppSettings) {
        if !settings.mail_notification_sound_enabled() {
            return;
        }

        if !Self::should_play_sound(Instant::now()) {
            tracing::debug!("Notification sound throttled (< {}s)", NOTIFY_THROTTLE_SECONDS);
            return;
        }

        if settings.mail_notification_sound_beep() {
            glib::idle_add_local_once(|| {
                if let Some(display) = gdk::Display::default() {
                    display.beep();
                }
            });
            return;
        }

        if settings.mail_notification_sound_play_file() {
            let file_path = settings.mail_notification_sound_file();
            if !file_path.trim().is_empty() {
                let path = file_path.trim().to_string();
                std::thread::spawn(move || {
                    Self::spawn_sound_file(&path);
                });
                return;
            }
        }

        // Default: use sound theme ("message-new-email")
        if settings.mail_notification_sound_use_theme() {
            std::thread::spawn(|| {
                Self::spawn_theme_sound("message-new-email");
            });
        }
    }

    /// Play notification sound immediately without throttle (for settings preview / test)
    pub fn preview_notification_sound(settings: &AppSettings) {
        if settings.mail_notification_sound_beep() {
            glib::idle_add_local_once(|| {
                if let Some(display) = gdk::Display::default() {
                    display.beep();
                }
            });
            return;
        }

        if settings.mail_notification_sound_play_file() {
            let file_path = settings.mail_notification_sound_file();
            if !file_path.trim().is_empty() {
                let path = file_path.trim().to_string();
                std::thread::spawn(move || {
                    Self::spawn_sound_file(&path);
                });
                return;
            }
        }

        if settings.mail_notification_sound_use_theme() {
            std::thread::spawn(|| {
                Self::spawn_theme_sound("message-new-email");
            });
        }
    }

    /// Play audible alarm sound for calendar/task reminders matching Evolution's alarm sound
    pub fn play_alarm_sound() {
        std::thread::spawn(|| {
            Self::spawn_theme_sound("alarm-clock-elapsed");
        });
    }

    /// Play an event sound by sound-theme event ID (e.g. `message-new-email`)
    fn spawn_theme_sound(event_id: &str) {
        let status = std::process::Command::new("canberra-gtk-play")
            .arg("-i")
            .arg(event_id)
            .status();

        if let Ok(s) = status {
            if s.success() {
                return;
            }
        }

        let candidates = [
            format!("/usr/share/sounds/freedesktop/stereo/{event_id}.oga"),
            format!("/usr/share/sounds/freedesktop/stereo/{event_id}.ogg"),
            format!("/usr/share/sounds/gnome/default/alerts/{event_id}.ogg"),
        ];
        for path in &candidates {
            if std::path::Path::new(path).is_file() {
                Self::spawn_sound_file(path);
                return;
            }
        }
    }

    /// Play a specific audio file with available system sound utilities
    fn spawn_sound_file(path: &str) {
        if let Ok(s) = std::process::Command::new("canberra-gtk-play")
            .arg("-f")
            .arg(path)
            .status()
        {
            if s.success() {
                return;
            }
        }

        if let Ok(s) = std::process::Command::new("pw-play")
            .arg(path)
            .status()
        {
            if s.success() {
                return;
            }
        }

        if let Ok(s) = std::process::Command::new("paplay")
            .arg(path)
            .status()
        {
            if s.success() {
                return;
            }
        }

        let _ = std::process::Command::new("aplay")
            .arg("-q")
            .arg(path)
            .status();
    }

    /// Build a `gio::Notification` matching Evolution 1:1 with actions
    pub fn build_notification(
        summary: &NewMailSummary,
        status_count: usize,
    ) -> gio::Notification {
        let notification = gio::Notification::new(&Self::notification_summary());

        let folder_display = summary
            .folder_display_name
            .as_deref()
            .unwrap_or(&summary.folder_name);

        let body = Self::format_notification_body(
            status_count,
            summary.latest_sender.as_deref(),
            summary.latest_subject.as_deref(),
            Some(folder_display),
        );
        notification.set_body(Some(&body));
        notification.set_priority(gio::NotificationPriority::Normal);

        let icon = gio::ThemedIcon::new("mail-unread-symbolic");
        notification.set_icon(&icon);

        let target = NotificationActionTarget::new(
            &summary.source_uid,
            &summary.folder_name,
            summary.latest_message_uid.clone(),
            summary.latest_message_id.clone(),
        );
        let target_variant = target.to_target_string().to_variant();

        // Default action on notification click: open mail
        notification.set_default_action_and_target_value("app.open-mail", Some(&target_variant));

        // Action button 1: "Show <Folder>" (Evolution: "Show %s", e.g. "Show Inbox" / "Posteingang anzeigen")
        let show_tmpl = gettext("Show %s");
        let show_label = if show_tmpl.contains("%s") {
            show_tmpl.replace("%s", folder_display)
        } else {
            format!("{}: {}", gettext("Show"), folder_display)
        };
        notification.add_button_with_target_value(
            &show_label,
            "app.open-mail",
            Some(&target_variant),
        );

        // Action button 2: "Mark as read" (only for single new message, identical to Evolution)
        if status_count == 1 && summary.latest_message_uid.is_some() {
            let mark_read_label = gettext("Mark as read");
            notification.add_button_with_target_value(
                &mark_read_label,
                "app.mark-mail-read",
                Some(&target_variant),
            );
        }

        notification
    }

    /// Handle new mail delivery: update state, send desktop notification, play sound
    pub fn notify_new_mail<A: IsA<gio::Application>>(
        app: &A,
        summary: NewMailSummary,
    ) {
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        if !settings.mail_notification_enabled() {
            return;
        }

        if !Self::should_notify_account(&summary.source_uid, &settings.mail_notification_not_accounts()) {
            tracing::debug!(source_uid = %summary.source_uid, "Notifications disabled for this account");
            return;
        }

        if !Self::should_notify_folder(&summary.folder_name, summary.is_inbox, settings.mail_notification_only_inbox()) {
            tracing::debug!(folder_name = %summary.folder_name, "Notifications limited to Inbox only");
            return;
        }

        let count = summary.new_messages_count.max(1);
        let status_count = ACCUMULATED_STATUS_COUNT.fetch_add(count, Ordering::SeqCst) + count;

        let notification = Self::build_notification(&summary, status_count);
        app.send_notification(Some(MAIL_NOTIFICATION_ID), &notification);

        Self::play_notification_sound(&settings);
        Self::send_dbus_signal(&summary, count);
    }

    /// Emit D-Bus new mail signal matching Evolution's `/org/gnome/evolution/mail/newmail`
    pub fn send_dbus_signal(summary: &NewMailSummary, new_count: usize) {
        let folder_display = summary
            .folder_display_name
            .clone()
            .unwrap_or_else(|| summary.folder_name.clone());
        let sender = summary.latest_sender.clone();
        let subject = summary.latest_subject.clone();
        let source_display = summary.source_display_name.clone();

        std::thread::spawn(move || {
            if let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
                let mut items = vec![folder_display.to_variant()];
                if new_count > 0 {
                    items.push(folder_display.to_variant());
                    items.push((new_count as u32).to_variant());
                }
                if let Some(s) = sender {
                    items.push(format!("sender:{s}").to_variant());
                }
                if let Some(sub) = subject {
                    items.push(format!("subject:{sub}").to_variant());
                }
                if let Some(src) = source_display {
                    items.push(format!("account:{src}").to_variant());
                }
                let body = glib::Variant::tuple_from_iter(items);
                let _ = connection.emit_signal(
                    None::<&str>,
                    "/org/gnome/evolution/mail/newmail",
                    "org.gnome.evolution.mail.dbus.Signal",
                    "NewMail",
                    Some(&body),
                );
            }
        });
    }

    /// Withdraw active notification from the system and reset accumulated counter
    pub fn withdraw_notification<A: IsA<gio::Application>>(app: &A) {
        Self::reset_status_count();
        app.withdraw_notification(MAIL_NOTIFICATION_ID);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_action_serialization_and_parsing() {
        let target = NotificationActionTarget::new(
            "account-123",
            "INBOX",
            Some("1042".to_string()),
            Some("eds:account-123:INBOX:1042".to_string()),
        );
        let s = target.to_target_string();
        assert_eq!(s, "eds:account-123:INBOX:1042:eds:account-123:INBOX:1042");

        let parsed = NotificationActionTarget::parse(&s).expect("should parse target");
        assert_eq!(parsed.source_uid, "account-123");
        assert_eq!(parsed.folder_name, "INBOX");
        assert_eq!(parsed.message_uid.as_deref(), Some("1042"));
        assert_eq!(parsed.message_id.as_deref(), Some("eds:account-123:INBOX:1042"));

        // Test without message_id
        let target2 = NotificationActionTarget::new("acc-1", "Sent", Some("99".to_string()), None);
        let s2 = target2.to_target_string();
        let parsed2 = NotificationActionTarget::parse(&s2).expect("should parse target2");
        assert_eq!(parsed2.source_uid, "acc-1");
        assert_eq!(parsed2.folder_name, "Sent");
        assert_eq!(parsed2.message_uid.as_deref(), Some("99"));
        assert_eq!(parsed2.message_id, None);
    }

    #[test]
    fn test_inbox_folder_detection() {
        assert!(MailNotificationService::is_inbox_folder("INBOX"));
        assert!(MailNotificationService::is_inbox_folder("inbox"));
        assert!(MailNotificationService::is_inbox_folder("Posteingang"));
        assert!(MailNotificationService::is_inbox_folder("posteingang"));
        assert!(MailNotificationService::is_inbox_folder("personal/inbox"));
        assert!(MailNotificationService::is_inbox_folder("archive/posteingang"));
        assert!(!MailNotificationService::is_inbox_folder("Sent"));
        assert!(!MailNotificationService::is_inbox_folder("Trash"));
        assert!(!MailNotificationService::is_inbox_folder("Entwürfe"));
    }

    #[test]
    fn test_account_filtering() {
        let excluded = vec!["acc-spam".to_string(), "acc-muted".to_string()];
        assert!(MailNotificationService::should_notify_account("acc-work", &excluded));
        assert!(MailNotificationService::should_notify_account("acc-personal", &excluded));
        assert!(!MailNotificationService::should_notify_account("acc-spam", &excluded));
        assert!(!MailNotificationService::should_notify_account("acc-muted", &excluded));
    }

    #[test]
    fn test_folder_filtering() {
        // only_inbox = true
        assert!(MailNotificationService::should_notify_folder("INBOX", false, true));
        assert!(MailNotificationService::should_notify_folder("custom_folder", true, true));
        assert!(!MailNotificationService::should_notify_folder("Sent", false, true));

        // only_inbox = false
        assert!(MailNotificationService::should_notify_folder("Sent", false, false));
        assert!(MailNotificationService::should_notify_folder("Trash", false, false));
    }

    #[test]
    fn test_format_notification_body_single_message() {
        let body = MailNotificationService::format_notification_body(
            1,
            Some("Alice <alice@example.com>"),
            Some("Meeting Tomorrow"),
            Some("Inbox"),
        );
        assert!(body.contains("1"));
        assert!(body.contains("alice@example.com"));
        assert!(body.contains("Meeting Tomorrow"));
        assert!(body.contains("Inbox"));
        assert!(!body.contains("more"));
    }

    #[test]
    fn test_format_notification_body_multiple_messages() {
        let body = MailNotificationService::format_notification_body(
            5,
            Some("Bob <bob@example.com>"),
            Some("Project Update"),
            Some("Inbox"),
        );
        assert!(body.contains("5"));
        assert!(body.contains("bob@example.com"));
        assert!(body.contains("Project Update"));
        assert!(body.contains("Inbox"));
        assert!(body.contains("4")); // (and 4 more)
    }

    #[test]
    fn test_sound_throttling() {
        let t0 = Instant::now();
        assert!(MailNotificationService::should_play_sound(t0));

        // Within 30 seconds -> false
        let t1 = t0 + Duration::from_secs(10);
        assert!(!MailNotificationService::should_play_sound(t1));

        let t2 = t0 + Duration::from_secs(29);
        assert!(!MailNotificationService::should_play_sound(t2));

        // After 30 seconds -> true
        let t3 = t0 + Duration::from_secs(31);
        assert!(MailNotificationService::should_play_sound(t3));
    }

    #[test]
    fn test_build_notification_single_and_multiple() {
        let single_summary = NewMailSummary {
            source_uid: "test-acc".to_string(),
            source_display_name: Some("Work Account".to_string()),
            folder_name: "INBOX".to_string(),
            folder_display_name: Some("Posteingang".to_string()),
            is_inbox: true,
            new_messages_count: 1,
            latest_message_uid: Some("msg-42".to_string()),
            latest_message_id: Some("eds:test-acc:INBOX:msg-42".to_string()),
            latest_sender: Some("Carol <carol@example.com>".to_string()),
            latest_subject: Some("Report Q3".to_string()),
        };

        // Single message: should construct successfully
        let notif_single = MailNotificationService::build_notification(&single_summary, 1);
        let _ = notif_single;

        // Multi message: should construct successfully
        let multi_summary = NewMailSummary {
            new_messages_count: 3,
            ..single_summary
        };
        let notif_multi = MailNotificationService::build_notification(&multi_summary, 3);
        let _ = notif_multi;
    }

    #[test]
    fn test_status_counter() {
        MailNotificationService::reset_status_count();
        assert_eq!(MailNotificationService::status_count(), 0);
        ACCUMULATED_STATUS_COUNT.fetch_add(3, Ordering::SeqCst);
        assert_eq!(MailNotificationService::status_count(), 3);
        MailNotificationService::reset_status_count();
        assert_eq!(MailNotificationService::status_count(), 0);
    }

    #[test]
    fn test_dbus_signal_tuple_payload() {
        let summary = NewMailSummary {
            source_uid: "test-src".to_string(),
            source_display_name: Some("Work Account".to_string()),
            folder_name: "INBOX".to_string(),
            folder_display_name: Some("Posteingang".to_string()),
            is_inbox: true,
            new_messages_count: 2,
            latest_message_uid: Some("msg-99".to_string()),
            latest_message_id: Some("eds:test-src:INBOX:msg-99".to_string()),
            latest_sender: Some("Dave <dave@example.com>".to_string()),
            latest_subject: Some("Status Update".to_string()),
        };

        let folder_display = summary.folder_display_name.as_deref().unwrap_or(&summary.folder_name);
        let mut items = vec![folder_display.to_variant()];
        items.push(folder_display.to_variant());
        items.push((2u32).to_variant());
        items.push(format!("sender:{}", summary.latest_sender.as_deref().unwrap()).to_variant());
        items.push(format!("subject:{}", summary.latest_subject.as_deref().unwrap()).to_variant());
        let body = glib::Variant::tuple_from_iter(items);
        assert!(body.is_container());
    }
}
