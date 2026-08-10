// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailMessageObject — GObject model representing a single email message for GTK4 ListModels.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;

use crate::services::mail_store_service::MailBackendLocation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub filename: String,
    pub size_formatted: String,
    pub mime_type: String,
    /// Decoded attachment bytes.  Keeping the payload with the parsed local
    /// message lets the UI offer a real “Save as…” operation without making
    /// assumptions about the lifetime of the source EML file.
    pub content: Vec<u8>,
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
        pub subject: RefCell<String>,
        pub date_str: RefCell<String>,
        pub preview_snippet: RefCell<String>,
        pub html_body: RefCell<String>,
        pub is_unread: RefCell<bool>,
        pub is_flagged: RefCell<bool>,
        pub(crate) backend_location: RefCell<Option<MailBackendLocation>>,
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
        subject: &str,
        date_str: &str,
        preview_snippet: &str,
        html_body: &str,
        is_unread: bool,
        is_flagged: bool,
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
        *imp.subject.borrow_mut() = subject.to_string();
        *imp.date_str.borrow_mut() = date_str.to_string();
        *imp.preview_snippet.borrow_mut() = preview_snippet.to_string();
        *imp.html_body.borrow_mut() = html_body.to_string();
        *imp.is_unread.borrow_mut() = is_unread;
        *imp.is_flagged.borrow_mut() = is_flagged;
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

    pub fn subject(&self) -> String {
        self.imp().subject.borrow().clone()
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

    pub(crate) fn backend_location(&self) -> Option<MailBackendLocation> {
        self.imp().backend_location.borrow().clone()
    }

    pub fn attachments(&self) -> Vec<AttachmentInfo> {
        self.imp().attachments.borrow().clone()
    }
}
