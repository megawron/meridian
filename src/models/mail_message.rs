// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailMessageObject — GObject model representing a single email message for GTK4 ListModels.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;
use std::sync::Arc;

use crate::services::mail_metadata_service::MailFollowUpState;
use crate::services::mail_store_service::MailBackendLocation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub filename: String,
    pub size_formatted: String,
    pub mime_type: String,
    /// MIME Content-ID used by `cid:` references in an HTML body.
    pub content_id: Option<String>,
    /// Decoded attachment bytes.  Keeping the payload with the parsed local
    /// message lets the UI offer a real “Save as…” operation without making
    /// assumptions about the lifetime of the source EML file.
    pub content: Arc<[u8]>,
}

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct MailMessageObject {
        pub id: RefCell<String>,
        pub folder_id: RefCell<String>,
        pub sender_name: RefCell<String>,
        pub sender_email: RefCell<String>,
        pub recipient_to: RefCell<String>,
        pub recipient_cc: RefCell<String>,
        pub recipient_bcc: RefCell<String>,
        pub reply_to: RefCell<String>,
        pub subject: RefCell<String>,
        pub message_id: RefCell<Option<String>>,
        pub in_reply_to: RefCell<Vec<String>>,
        pub references: RefCell<Vec<String>>,
        pub thread_id: RefCell<String>,
        pub thread_size: RefCell<usize>,
        pub thread_position: RefCell<usize>,
        pub date_str: RefCell<String>,
        pub preview_snippet: RefCell<String>,
        pub html_body: RefCell<String>,
        pub searchable_body_text: RefCell<Option<Arc<str>>>,
        pub is_unread: RefCell<bool>,
        pub is_flagged: RefCell<bool>,
        pub is_important: RefCell<bool>,
        pub follow_up_state: RefCell<MailFollowUpState>,
        pub labels: RefCell<Vec<String>>,
        pub(crate) backend_location: RefCell<Option<MailBackendLocation>>,
        /// Original RFC822 header block. Keeping headers independently avoids
        /// retaining every potentially large MIME body only for the optional
        /// "all headers" view.
        pub(crate) raw_headers: RefCell<Option<Arc<[u8]>>>,
        /// Original RFC822 bytes for cryptographic inspection.  The parser
        /// retains this only for messages with a detected crypto envelope.
        pub(crate) raw_mime: RefCell<Option<Arc<[u8]>>>,
        pub attachments: RefCell<Vec<AttachmentInfo>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailMessageObject {
        const NAME: &'static str = "EvolutionMailMessageObject";
        type Type = super::MailMessageObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for MailMessageObject {}
}

glib::wrapper! {
    pub struct MailMessageObject(ObjectSubclass<imp::MailMessageObject>);
}

impl MailMessageObject {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: &str,
        folder_id: &str,
        sender_name: &str,
        sender_email: &str,
        recipient_to: &str,
        recipient_cc: &str,
        recipient_bcc: &str,
        reply_to: &str,
        subject: &str,
        message_id: Option<String>,
        in_reply_to: Vec<String>,
        references: Vec<String>,
        thread_id: &str,
        thread_size: usize,
        thread_position: usize,
        date_str: &str,
        preview_snippet: &str,
        html_body: &str,
        is_unread: bool,
        is_flagged: bool,
        is_important: bool,
        follow_up_state: MailFollowUpState,
        labels: Vec<String>,
        backend_location: Option<MailBackendLocation>,
        attachments: Vec<AttachmentInfo>,
    ) -> Self {
        let obj: Self = Object::builder().build();
        let imp = obj.imp();
        *imp.id.borrow_mut() = id.to_string();
        *imp.folder_id.borrow_mut() = folder_id.to_string();
        *imp.sender_name.borrow_mut() = sender_name.to_string();
        *imp.sender_email.borrow_mut() = sender_email.to_string();
        *imp.recipient_to.borrow_mut() = recipient_to.to_string();
        *imp.recipient_cc.borrow_mut() = recipient_cc.to_string();
        *imp.recipient_bcc.borrow_mut() = recipient_bcc.to_string();
        *imp.reply_to.borrow_mut() = reply_to.to_string();
        *imp.subject.borrow_mut() = subject.to_string();
        *imp.message_id.borrow_mut() = message_id;
        *imp.in_reply_to.borrow_mut() = in_reply_to;
        *imp.references.borrow_mut() = references;
        *imp.thread_id.borrow_mut() = thread_id.to_string();
        *imp.thread_size.borrow_mut() = thread_size.max(1);
        *imp.thread_position.borrow_mut() = thread_position;
        *imp.date_str.borrow_mut() = date_str.to_string();
        *imp.preview_snippet.borrow_mut() = preview_snippet.to_string();
        *imp.html_body.borrow_mut() = html_body.to_string();
        *imp.is_unread.borrow_mut() = is_unread;
        *imp.is_flagged.borrow_mut() = is_flagged;
        *imp.is_important.borrow_mut() = is_important;
        *imp.follow_up_state.borrow_mut() = follow_up_state;
        *imp.labels.borrow_mut() = labels;
        *imp.backend_location.borrow_mut() = backend_location;
        *imp.attachments.borrow_mut() = attachments;
        obj
    }

    pub fn id(&self) -> String {
        self.imp().id.borrow().clone()
    }

    pub fn folder_id(&self) -> String {
        self.imp().folder_id.borrow().clone()
    }

    pub fn sender_name(&self) -> String {
        self.imp().sender_name.borrow().clone()
    }

    pub fn sender_email(&self) -> String {
        self.imp().sender_email.borrow().clone()
    }

    pub fn recipient_to(&self) -> String {
        self.imp().recipient_to.borrow().clone()
    }

    pub fn recipient_cc(&self) -> String {
        self.imp().recipient_cc.borrow().clone()
    }

    pub fn recipient_bcc(&self) -> String {
        self.imp().recipient_bcc.borrow().clone()
    }

    pub fn reply_to(&self) -> String {
        self.imp().reply_to.borrow().clone()
    }

    pub fn subject(&self) -> String {
        self.imp().subject.borrow().clone()
    }

    pub fn message_id(&self) -> Option<String> {
        self.imp().message_id.borrow().clone()
    }

    pub fn in_reply_to(&self) -> Vec<String> {
        self.imp().in_reply_to.borrow().clone()
    }

    pub fn references(&self) -> Vec<String> {
        self.imp().references.borrow().clone()
    }

    pub fn thread_id(&self) -> String {
        self.imp().thread_id.borrow().clone()
    }

    pub fn thread_size(&self) -> usize {
        (*self.imp().thread_size.borrow()).max(1)
    }

    pub fn thread_position(&self) -> usize {
        *self.imp().thread_position.borrow()
    }

    pub(crate) fn set_thread_metadata(&self, thread_id: &str, size: usize, position: usize) {
        *self.imp().thread_id.borrow_mut() = thread_id.to_string();
        *self.imp().thread_size.borrow_mut() = size.max(1);
        *self.imp().thread_position.borrow_mut() = position;
    }

    pub fn date_str(&self) -> String {
        self.imp().date_str.borrow().clone()
    }

    pub fn preview_snippet(&self) -> String {
        self.imp().preview_snippet.borrow().clone()
    }

    pub fn html_body(&self) -> String {
        self.imp().html_body.borrow().clone()
    }

    pub(crate) fn searchable_body_text(&self) -> Arc<str> {
        if let Some(cached) = self.imp().searchable_body_text.borrow().as_ref() {
            return cached.clone();
        }
        let text: Arc<str> =
            crate::services::html_sanitizer::html_to_plain_text(&self.imp().html_body.borrow())
                .to_ascii_lowercase()
                .into();
        self.imp().searchable_body_text.replace(Some(text.clone()));
        text
    }

    pub(crate) fn replace_rendered_content(
        &self,
        html_body: String,
        preview_snippet: String,
        attachments: Vec<AttachmentInfo>,
    ) {
        *self.imp().html_body.borrow_mut() = html_body;
        self.imp().searchable_body_text.take();
        *self.imp().preview_snippet.borrow_mut() = preview_snippet;
        *self.imp().attachments.borrow_mut() = attachments;
    }

    pub fn is_unread(&self) -> bool {
        *self.imp().is_unread.borrow()
    }

    pub fn set_unread(&self, unread: bool) {
        *self.imp().is_unread.borrow_mut() = unread;
    }

    pub fn is_flagged(&self) -> bool {
        *self.imp().is_flagged.borrow()
    }

    pub fn set_flagged(&self, flagged: bool) {
        *self.imp().is_flagged.borrow_mut() = flagged;
    }

    pub fn is_important(&self) -> bool {
        *self.imp().is_important.borrow()
    }

    pub fn set_important(&self, important: bool) {
        *self.imp().is_important.borrow_mut() = important;
    }

    pub fn follow_up_state(&self) -> MailFollowUpState {
        *self.imp().follow_up_state.borrow()
    }

    pub fn set_follow_up_state(&self, state: MailFollowUpState) {
        *self.imp().follow_up_state.borrow_mut() = state;
    }

    pub fn labels(&self) -> Vec<String> {
        self.imp().labels.borrow().clone()
    }

    pub fn has_label(&self, wanted: &str) -> bool {
        self.imp()
            .labels
            .borrow()
            .iter()
            .any(|label| label.eq_ignore_ascii_case(wanted.trim()))
    }

    /// Update one app-owned label in the in-memory row.  The sidecar write is
    /// performed by `MailOperationsService`; keeping this small UI update
    /// separate lets a successful command refresh without changing any EDS
    /// message data.
    pub fn set_label(&self, label: &str, labeled: bool) -> bool {
        let mut labels = self.imp().labels.borrow_mut();
        let changed = update_label_values(&mut labels, label, labeled);
        if changed {
            labels.sort_by_key(|value| value.to_ascii_lowercase());
        }
        changed
    }

    pub(crate) fn backend_location(&self) -> Option<MailBackendLocation> {
        self.imp().backend_location.borrow().clone()
    }

    /// Keep a loaded Camel row addressable after an incremental folder
    /// rename. Camel UIDs are folder-scoped, so both the backend location and
    /// the synthetic row ID must change together.
    pub(crate) fn rename_backend_folder(
        &self,
        source_uid: &str,
        old_folder_name: &str,
        new_folder_name: &str,
    ) -> Option<(String, String)> {
        let mut location = self.imp().backend_location.borrow_mut();
        let Some(location) = location.as_mut() else {
            return None;
        };
        if location.source_uid != source_uid || location.folder_name != old_folder_name {
            return None;
        }

        let old_id = self.id();
        location.folder_name = new_folder_name.to_string();
        let new_id = format!(
            "eds:{}:{}:{}",
            location.source_uid, location.folder_name, location.message_uid
        );
        *self.imp().id.borrow_mut() = new_id.clone();
        if self.thread_id() == old_id {
            *self.imp().thread_id.borrow_mut() = new_id.clone();
        }
        Some((old_id, new_id))
    }

    pub(crate) fn set_raw_mime(&self, raw: Option<Vec<u8>>) {
        *self.imp().raw_mime.borrow_mut() = raw.map(Into::into);
    }

    pub(crate) fn set_raw_headers(&self, raw: Option<Vec<u8>>) {
        *self.imp().raw_headers.borrow_mut() = raw.map(Into::into);
    }

    pub(crate) fn raw_headers(&self) -> Option<Arc<[u8]>> {
        self.imp().raw_headers.borrow().clone()
    }

    pub(crate) fn raw_mime(&self) -> Option<Arc<[u8]>> {
        self.imp().raw_mime.borrow().clone()
    }

    pub fn attachments(&self) -> Vec<AttachmentInfo> {
        self.imp().attachments.borrow().clone()
    }

    pub fn attachment_count(&self) -> usize {
        self.imp().attachments.borrow().len()
    }
}

fn update_label_values(labels: &mut Vec<String>, label: &str, labeled: bool) -> bool {
    let label = label.trim();
    if label.is_empty() {
        return false;
    }

    if labeled {
        if labels
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(label))
        {
            return false;
        }
        labels.push(label.to_string());
        true
    } else {
        let original_len = labels.len();
        labels.retain(|candidate| !candidate.eq_ignore_ascii_case(label));
        original_len != labels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{MailBackendLocation, MailFollowUpState, MailMessageObject, update_label_values};

    #[test]
    fn label_updates_are_case_insensitive_and_idempotent() {
        let mut labels = vec!["Work".to_string()];
        assert!(!update_label_values(&mut labels, " work ", true));
        assert_eq!(labels, ["Work"]);
        assert!(update_label_values(&mut labels, "Personal", true));
        assert_eq!(labels, ["Work", "Personal"]);
        assert!(update_label_values(&mut labels, "WORK", false));
        assert_eq!(labels, ["Personal"]);
        assert!(!update_label_values(&mut labels, "", true));
    }

    #[test]
    fn folder_rename_updates_backend_row_and_fallback_thread_identity_together() {
        let old_id = "eds:account-a:Projects:uid-1";
        let message = MailMessageObject::new(
            old_id,
            "all",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            None,
            Vec::new(),
            Vec::new(),
            old_id,
            1,
            0,
            "",
            "",
            "",
            false,
            false,
            false,
            MailFollowUpState::None,
            Vec::new(),
            Some(MailBackendLocation {
                source_uid: "account-a".to_string(),
                folder_name: "Projects".to_string(),
                message_uid: "uid-1".to_string(),
                message_id: None,
            }),
            Vec::new(),
        );

        assert_eq!(
            message.rename_backend_folder("account-a", "Projects", "Archive"),
            Some((
                old_id.to_string(),
                "eds:account-a:Archive:uid-1".to_string()
            ))
        );
        assert_eq!(message.id(), "eds:account-a:Archive:uid-1");
        assert_eq!(message.thread_id(), "eds:account-a:Archive:uid-1");
        assert_eq!(message.backend_location().unwrap().folder_name, "Archive");

        message.replace_rendered_content(
            "<p>First body</p>".to_string(),
            String::new(),
            Vec::new(),
        );
        assert_eq!(message.searchable_body_text().as_ref(), "first body");
        message.replace_rendered_content(
            "<p>Updated body</p>".to_string(),
            String::new(),
            Vec::new(),
        );
        assert_eq!(message.searchable_body_text().as_ref(), "updated body");
    }
}
