// SPDX-License-Identifier: GPL-3.0-or-later
//
// MailMessageObject — GObject model representing a single email message for GTK4 ListModels.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;
use std::sync::Arc;

use crate::services::mail_metadata_service::{MailFollowUpMetadata, MailFollowUpState};
use crate::services::mail_store_service::MailBackendLocation;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MailAttachmentKind {
    #[default]
    None,
    File,
    Calendar,
    Contact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub filename: String,
    pub size_formatted: String,
    pub mime_type: String,
    /// MIME Content-ID used by `cid:` references in an HTML body.
    pub content_id: Option<String>,
    /// Whether this part is an inline body resource (such as an embedded image).
    #[serde(default)]
    pub is_inline: bool,
    /// Decoded attachment bytes.  Keeping the payload with the parsed local
    /// message lets the UI offer a real “Save as…” operation without making
    /// assumptions about the lifetime of the source EML file.
    #[serde(skip, default = "empty_arc_bytes")]
    pub content: Arc<[u8]>,
}

impl AttachmentInfo {
    /// Infers a specific MIME type from a filename extension when the declared
    /// MIME type is generic (such as `application/octet-stream`) or missing.
    pub fn infer_mime_type_from_filename(filename: &str) -> Option<&'static str> {
        let ext = std::path::Path::new(filename.trim())
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Some("image/jpeg"),
            "png" => Some("image/png"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            "svg" | "svgz" => Some("image/svg+xml"),
            "bmp" | "dib" => Some("image/bmp"),
            "tif" | "tiff" => Some("image/tiff"),
            "ico" => Some("image/vnd.microsoft.icon"),
            "heic" | "heif" => Some("image/heic"),
            "avif" => Some("image/avif"),
            "pdf" => Some("application/pdf"),
            "doc" | "dot" => Some("application/msword"),
            "docx" | "dotx" | "docm" => {
                Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
            }
            "odt" | "ott" => Some("application/vnd.oasis.opendocument.text"),
            "rtf" => Some("application/rtf"),
            "xls" | "xlt" | "xla" => Some("application/vnd.ms-excel"),
            "xlsx" | "xltx" | "xlsm" | "xlsb" => {
                Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
            }
            "ods" | "ots" => Some("application/vnd.oasis.opendocument.spreadsheet"),
            "csv" | "tsv" => Some("text/csv"),
            "ppt" | "pot" | "pps" => Some("application/vnd.ms-powerpoint"),
            "pptx" | "potx" | "ppsx" | "pptm" => {
                Some("application/vnd.openxmlformats-officedocument.presentationml.presentation")
            }
            "odp" | "otp" => Some("application/vnd.oasis.opendocument.presentation"),
            "zip" => Some("application/zip"),
            "tar" => Some("application/x-tar"),
            "gz" | "tgz" => Some("application/gzip"),
            "7z" => Some("application/x-7z-compressed"),
            "rar" => Some("application/vnd.rar"),
            "bz2" | "tbz2" => Some("application/x-bzip2"),
            "xz" | "txz" => Some("application/x-xz"),
            "mp3" => Some("audio/mpeg"),
            "wav" => Some("audio/wav"),
            "ogg" | "oga" => Some("audio/ogg"),
            "flac" => Some("audio/flac"),
            "m4a" | "aac" => Some("audio/mp4"),
            "mp4" | "m4v" => Some("video/mp4"),
            "mkv" => Some("video/x-matroska"),
            "mov" => Some("video/quicktime"),
            "avi" => Some("video/x-msvideo"),
            "webm" => Some("video/webm"),
            "txt" | "log" | "md" | "markdown" => Some("text/plain"),
            "html" | "htm" => Some("text/html"),
            "json" => Some("application/json"),
            "xml" => Some("application/xml"),
            "ics" | "vcs" => Some("text/calendar"),
            "vcf" => Some("text/vcard"),
            "eml" => Some("message/rfc822"),
            _ => None,
        }
    }

    /// Returns the effective MIME type, falling back to filename extension
    /// inference when the declared MIME type is empty or generic (`application/octet-stream`).
    pub fn effective_mime_type(&self) -> String {
        let raw = self.mime_type.trim();
        let lower = raw.to_ascii_lowercase();
        let is_generic = lower.is_empty()
            || lower == "application/octet-stream"
            || lower == "binary/octet-stream"
            || lower == "application/unknown"
            || lower == "application/x-unknown"
            || lower == "unknown";
        if is_generic {
            if let Some(inferred) = Self::infer_mime_type_from_filename(&self.filename) {
                return inferred.to_string();
            }
        }
        raw.to_string()
    }

    /// Resolves the best symbolic icon name for a given MIME type and filename.
    pub fn icon_name_for(mime_type: &str, filename: &str) -> &'static str {
        let raw_lower = mime_type.trim().to_ascii_lowercase();
        let effective = if raw_lower.is_empty()
            || raw_lower == "application/octet-stream"
            || raw_lower == "binary/octet-stream"
            || raw_lower == "application/unknown"
            || raw_lower == "application/x-unknown"
            || raw_lower == "unknown"
        {
            Self::infer_mime_type_from_filename(filename)
                .unwrap_or(raw_lower.as_str())
                .to_ascii_lowercase()
        } else {
            raw_lower
        };
        let ext = std::path::Path::new(filename.trim())
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if effective.starts_with("image/")
            || matches!(
                ext.as_str(),
                "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "webp" | "svg" | "svgz" | "bmp"
                    | "tif" | "tiff" | "ico" | "heic" | "heif" | "avif"
            )
        {
            "image-x-generic-symbolic"
        } else if effective.contains("spreadsheet")
            || effective.contains("excel")
            || effective == "text/csv"
            || matches!(
                ext.as_str(),
                "xls" | "xlsx" | "xlsm" | "xlsb" | "xlt" | "xltx" | "ods" | "ots" | "csv" | "tsv"
            )
        {
            "x-office-spreadsheet-symbolic"
        } else if effective.contains("presentation")
            || effective.contains("powerpoint")
            || matches!(
                ext.as_str(),
                "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "pot" | "potx" | "odp" | "otp"
            )
        {
            "x-office-presentation-symbolic"
        } else if effective.contains("calendar") || matches!(ext.as_str(), "ics" | "vcs") {
            "x-office-calendar-symbolic"
        } else if effective.contains("vcard") || ext == "vcf" {
            "x-office-address-book-symbolic"
        } else if effective.contains("pdf")
            || effective.contains("msword")
            || effective.contains("wordprocessing")
            || effective.contains("opendocument.text")
            || effective == "application/rtf"
            || matches!(
                ext.as_str(),
                "pdf" | "doc" | "docx" | "dot" | "dotx" | "docm" | "odt" | "ott" | "rtf"
            )
        {
            "x-office-document-symbolic"
        } else if effective.contains("zip")
            || effective.contains("tar")
            || effective.contains("compressed")
            || effective.contains("archive")
            || effective.contains("gzip")
            || effective.contains("bzip")
            || effective.contains("xz")
            || effective.contains("rar")
            || matches!(
                ext.as_str(),
                "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "bz2" | "tbz2" | "xz" | "txz"
            )
        {
            "package-x-generic-symbolic"
        } else if effective.starts_with("audio/")
            || matches!(ext.as_str(), "mp3" | "wav" | "ogg" | "oga" | "flac" | "m4a" | "aac")
        {
            "audio-x-generic-symbolic"
        } else if effective.starts_with("video/")
            || matches!(ext.as_str(), "mp4" | "m4v" | "mkv" | "mov" | "avi" | "webm")
        {
            "video-x-generic-symbolic"
        } else if effective.starts_with("text/")
            || effective.contains("json")
            || effective.contains("xml")
            || matches!(
                ext.as_str(),
                "txt" | "log" | "md" | "markdown" | "json" | "xml" | "html" | "htm" | "yaml" | "yml"
            )
        {
            "text-x-generic-symbolic"
        } else {
            "mail-attachment-symbolic"
        }
    }

    /// Returns the symbolic icon name for this attachment.
    pub fn icon_name(&self) -> &'static str {
        Self::icon_name_for(&self.mime_type, &self.filename)
    }

    /// Returns true if this is a user-facing file attachment rather than an
    /// inline body resource (such as a signature logo or embedded CID image).
    pub fn is_user_attachment(&self) -> bool {
        let mime = self.mime_type.to_ascii_lowercase();
        // Any non-image (PDF, zip, docx, calendar, text, etc.) is always a user-facing attachment.
        if !mime.starts_with("image/") {
            return true;
        }
        // For images, if it is marked as inline and has a content-id, it is an embedded
        // HTML body resource (e.g. signature image or inline screenshot).
        // Otherwise, it is a user-facing attachment.
        !(self.is_inline && self.content_id.is_some())
    }

    /// User-friendly description of the attachment's file type (e.g. "PDF document", "JPEG image").
    pub fn type_description(&self) -> String {
        let effective_mime = self.effective_mime_type();
        let eff_lower = effective_mime.to_ascii_lowercase();
        let is_generic = eff_lower.is_empty()
            || eff_lower == "application/octet-stream"
            || eff_lower == "binary/octet-stream"
            || eff_lower == "application/unknown"
            || eff_lower == "application/x-unknown"
            || eff_lower == "unknown";
        if !is_generic {
            let desc = gio::content_type_get_description(&effective_mime);
            let s = desc.to_string();
            let s_lower = s.trim().to_ascii_lowercase();
            if !s_lower.is_empty()
                && s_lower != eff_lower
                && s_lower != "unknown"
                && s_lower != "unbekannt"
                && !s_lower.contains("unknown ")
                && !s_lower.contains("octet-stream")
            {
                return s;
            }
        }
        let ext = std::path::Path::new(&self.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_uppercase();
        if !ext.is_empty() {
            format!("{ext} file")
        } else if !is_generic {
            effective_mime
        } else {
            "File".to_string()
        }
    }
}

fn empty_arc_bytes() -> Arc<[u8]> {
    Arc::from([])
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MailBackendIdentity {
    pub(crate) source_uid: String,
    pub(crate) folder_name: String,
    pub(crate) message_uid: String,
    pub(crate) message_id: Option<String>,
}

/// Stable identity for one displayed message record.
///
/// The row ID alone is not sufficient: a local record and a Camel record can
/// legitimately expose the same ID while carrying different bodies.  The
/// backend location therefore remains part of the key, and local records
/// retain their folder and RFC822 Message-ID context.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MailMessageIdentity {
    pub(crate) id: String,
    pub(crate) folder_id: String,
    pub(crate) message_id: Option<String>,
    pub(crate) backend: Option<MailBackendIdentity>,
}

impl MailMessageIdentity {
    pub(crate) fn from_parts(
        id: &str,
        folder_id: &str,
        message_id: Option<&str>,
        backend_location: Option<&MailBackendLocation>,
    ) -> Self {
        let message_id = message_id
            .map(str::to_string)
            .or_else(|| backend_location.and_then(|location| location.message_id.clone()));
        let backend = backend_location.map(|location| MailBackendIdentity {
            source_uid: location.source_uid.clone(),
            folder_name: location.folder_name.clone(),
            message_uid: location.message_uid.clone(),
            message_id: message_id.clone(),
        });
        Self {
            id: id.to_string(),
            folder_id: folder_id.to_string(),
            message_id,
            backend,
        }
    }
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
        pub message_size: RefCell<usize>,
        pub preview_snippet: RefCell<String>,
        pub html_body: RefCell<String>,
        pub searchable_body_text: RefCell<Option<Arc<str>>>,
        pub is_unread: RefCell<bool>,
        pub is_flagged: RefCell<bool>,
        pub is_important: RefCell<bool>,
        pub is_replied: RefCell<bool>,
        pub is_forwarded: RefCell<bool>,
        /// Local sidecar deletion is intentionally independent from a folder:
        /// a visible deleted row remains in its original logical folder.
        pub is_deleted: RefCell<bool>,
        pub follow_up_state: RefCell<MailFollowUpState>,
        pub follow_up: RefCell<MailFollowUpMetadata>,
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
        pub is_preview_only: RefCell<bool>,
        pub cached_formatted_date: RefCell<Option<(String, String)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailMessageObject {
        const NAME: &'static str = "MeridianMailMessageObject";
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
        message_size: usize,
        preview_snippet: &str,
        html_body: &str,
        is_unread: bool,
        is_flagged: bool,
        is_important: bool,
        follow_up_state: MailFollowUpState,
        follow_up: MailFollowUpMetadata,
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
        *imp.subject.borrow_mut() =
            crate::services::mail_store_service::decode_mime_header(subject);
        *imp.message_id.borrow_mut() = message_id;
        *imp.in_reply_to.borrow_mut() = in_reply_to;
        *imp.references.borrow_mut() = references;
        *imp.thread_id.borrow_mut() = thread_id.to_string();
        *imp.thread_size.borrow_mut() = thread_size.max(1);
        *imp.thread_position.borrow_mut() = thread_position;
        *imp.date_str.borrow_mut() = date_str.to_string();
        *imp.message_size.borrow_mut() = message_size;
        let clean_snippet = crate::services::html_sanitizer::clean_mail_snippet(preview_snippet);
        *imp.preview_snippet.borrow_mut() = clean_snippet;
        *imp.html_body.borrow_mut() = html_body.to_string();
        *imp.is_unread.borrow_mut() = is_unread;
        *imp.is_flagged.borrow_mut() = is_flagged;
        *imp.is_important.borrow_mut() = is_important;
        *imp.follow_up_state.borrow_mut() = follow_up_state;
        *imp.follow_up.borrow_mut() = follow_up;
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

    pub fn message_size(&self) -> usize {
        *self.imp().message_size.borrow()
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

    pub fn formatted_date_cached<F>(&self, cache_key: &str, compute: F) -> String
    where
        F: FnOnce(&str) -> String,
    {
        let imp = self.imp();
        if let Some((ref key, ref formatted)) = *imp.cached_formatted_date.borrow() {
            if key == cache_key {
                return formatted.clone();
            }
        }
        let date_str = imp.date_str.borrow();
        let formatted = compute(&date_str);
        *imp.cached_formatted_date.borrow_mut() = Some((cache_key.to_string(), formatted.clone()));
        formatted
    }

    pub fn preview_snippet(&self) -> String {
        self.imp().preview_snippet.borrow().clone()
    }

    pub fn html_body(&self) -> String {
        self.imp().html_body.borrow().clone()
    }

    pub fn inline_images(&self) -> Vec<crate::services::inline_content_service::InlineImage> {
        let attachments = self.attachments();
        let mut inline_images = Vec::new();
        for attachment in &attachments {
            if attachment.content.is_empty() {
                continue;
            }
            if !attachment.filename.is_empty() {
                inline_images.push(crate::services::inline_content_service::InlineImage::new(
                    attachment.filename.clone(),
                    attachment.mime_type.clone(),
                    attachment.content.clone(),
                ));
            }
            if let Some(content_id) = attachment.content_id.as_ref() {
                if let Some((local_part, _)) = content_id.trim_matches(['<', '>']).split_once('@') {
                    if !local_part.is_empty() {
                        inline_images.push(
                            crate::services::inline_content_service::InlineImage::new(
                                local_part.to_string(),
                                attachment.mime_type.clone(),
                                attachment.content.clone(),
                            ),
                        );
                    }
                }
                inline_images.push(crate::services::inline_content_service::InlineImage::new(
                    content_id.clone(),
                    attachment.mime_type.clone(),
                    attachment.content.clone(),
                ));
            }
        }
        inline_images
    }

    pub fn resolved_html_body(&self) -> String {
        let html = self.html_body();
        if html.is_empty() {
            return html;
        }
        let inline_images = self.inline_images();
        if inline_images.is_empty() {
            return html;
        }
        crate::services::inline_content_service::resolve_cid_images(&html, &inline_images)
    }

    /// Return the body text prepared by a non-GTK worker.
    ///
    /// Filtering and in-message search must never turn a selected GObject's
    /// HTML into plaintext lazily.  A missing value therefore means that the
    /// record has not crossed the worker boundary with a searchable body yet;
    /// callers must treat it as an empty body rather than preparing it here.
    pub(crate) fn searchable_body_text(&self) -> Option<Arc<str>> {
        self.imp().searchable_body_text.borrow().clone()
    }

    pub(crate) fn set_searchable_body_text(&self, text: Arc<str>) {
        self.imp().searchable_body_text.replace(Some(text));
    }

    pub(crate) fn message_identity(&self) -> MailMessageIdentity {
        let id = self.id();
        let folder_id = self.folder_id();
        let message_id = self.message_id();
        let backend_location = self.backend_location();
        MailMessageIdentity::from_parts(
            &id,
            &folder_id,
            message_id.as_deref(),
            backend_location.as_ref(),
        )
    }

    pub(crate) fn replace_rendered_content(
        &self,
        html_body: String,
        preview_snippet: String,
        attachments: Vec<AttachmentInfo>,
        searchable_body_text: Option<Arc<str>>,
    ) {
        *self.imp().html_body.borrow_mut() = html_body.clone();
        self.imp().searchable_body_text.take();
        let clean_snippet = crate::services::html_sanitizer::clean_mail_snippet(&preview_snippet);
        *self.imp().preview_snippet.borrow_mut() = clean_snippet;
        *self.imp().attachments.borrow_mut() = attachments;
        *self.imp().is_preview_only.borrow_mut() = false;
        if let Some(searchable_body_text) = searchable_body_text {
            self.set_searchable_body_text(searchable_body_text);
        }
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

    pub fn is_replied(&self) -> bool {
        *self.imp().is_replied.borrow()
    }

    pub fn set_replied(&self, replied: bool) {
        *self.imp().is_replied.borrow_mut() = replied;
    }

    pub fn is_forwarded(&self) -> bool {
        *self.imp().is_forwarded.borrow()
    }

    pub fn set_forwarded(&self, forwarded: bool) {
        *self.imp().is_forwarded.borrow_mut() = forwarded;
    }

    pub(crate) fn is_deleted(&self) -> bool {
        *self.imp().is_deleted.borrow()
    }

    pub(crate) fn set_deleted(&self, deleted: bool) {
        *self.imp().is_deleted.borrow_mut() = deleted;
    }

    pub fn follow_up_state(&self) -> MailFollowUpState {
        *self.imp().follow_up_state.borrow()
    }

    pub fn set_follow_up_state(&self, state: MailFollowUpState) {
        *self.imp().follow_up_state.borrow_mut() = state;
        self.imp().follow_up.borrow_mut().state = state;
    }

    pub fn follow_up(&self) -> MailFollowUpMetadata {
        self.imp().follow_up.borrow().clone()
    }

    pub fn set_follow_up(&self, metadata: MailFollowUpMetadata) {
        let metadata = metadata.normalized();
        *self.imp().follow_up_state.borrow_mut() = metadata.state;
        *self.imp().follow_up.borrow_mut() = metadata;
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

    pub fn set_labels(&self, labels: Vec<String>) {
        *self.imp().labels.borrow_mut() = labels;
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

    pub fn user_attachments(&self) -> Vec<AttachmentInfo> {
        self.imp()
            .attachments
            .borrow()
            .iter()
            .filter(|att| att.is_user_attachment())
            .cloned()
            .collect()
    }

    pub fn attachment_count(&self) -> usize {
        self.imp()
            .attachments
            .borrow()
            .iter()
            .filter(|att| att.is_user_attachment())
            .count()
    }

    pub fn attachment_kind(&self) -> MailAttachmentKind {
        let atts = self.imp().attachments.borrow();
        let user_atts: Vec<_> = atts.iter().filter(|att| att.is_user_attachment()).collect();
        if user_atts.is_empty() {
            return MailAttachmentKind::None;
        }
        for att in user_atts {
            let mime = att.mime_type.to_ascii_lowercase();
            let filename = att.filename.to_ascii_lowercase();
            if mime.contains("calendar") || filename.ends_with(".ics") || filename.ends_with(".ical") {
                return MailAttachmentKind::Calendar;
            }
            if mime.contains("vcard") || mime.contains("x-vcard") || filename.ends_with(".vcf") {
                return MailAttachmentKind::Contact;
            }
        }
        MailAttachmentKind::File
    }

    pub fn is_preview_only(&self) -> bool {
        *self.imp().is_preview_only.borrow()
    }

    pub fn set_preview_only(&self, is_preview_only: bool) {
        *self.imp().is_preview_only.borrow_mut() = is_preview_only;
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
    use std::sync::Arc;

    use super::{
        MailBackendLocation, MailFollowUpMetadata, MailFollowUpState, MailMessageIdentity,
        MailMessageObject, update_label_values,
    };

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
            0,
            "",
            "",
            false,
            false,
            false,
            MailFollowUpState::None,
            MailFollowUpMetadata::default(),
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

        assert!(message.searchable_body_text().is_none());
        message.set_searchable_body_text(Arc::from("first body"));
        assert_eq!(
            message.searchable_body_text().as_deref(),
            Some("first body")
        );
        message.replace_rendered_content(
            "<p>Updated body</p>".to_string(),
            String::new(),
            Vec::new(),
            Some(Arc::from("updated body")),
        );
        assert_eq!(
            message.searchable_body_text().as_deref(),
            Some("updated body")
        );

        message.replace_rendered_content(
            "<p>Unprepared body</p>".to_string(),
            String::new(),
            Vec::new(),
            None,
        );
        assert!(message.searchable_body_text().is_none());
    }

    #[test]
    fn message_identity_distinguishes_local_and_camel_records_with_same_id() {
        let local =
            MailMessageIdentity::from_parts("same-id", "inbox", Some("<same@example.test>"), None);
        let camel = MailMessageIdentity::from_parts(
            "same-id",
            "inbox",
            Some("<same@example.test>"),
            Some(&MailBackendLocation {
                source_uid: "account-a".to_string(),
                folder_name: "INBOX".to_string(),
                message_uid: "uid-1".to_string(),
                message_id: Some("<same@example.test>".to_string()),
            }),
        );

        assert_ne!(local, camel);
        assert_eq!(local, local.clone());
        assert_eq!(camel, camel.clone());
    }

    #[test]
    fn message_replied_and_forwarded_flags_roundtrip() {
        let message = MailMessageObject::new(
            "msg-1",
            "inbox",
            "Sender",
            "sender@example.com",
            "to@example.com",
            "",
            "",
            "",
            "Subject",
            None,
            Vec::new(),
            Vec::new(),
            "thread-1",
            1,
            0,
            "2026-09-10",
            100,
            "snippet",
            "<p>snippet</p>",
            false,
            false,
            false,
            crate::services::mail_metadata_service::MailFollowUpState::None,
            crate::services::mail_metadata_service::MailFollowUpMetadata::default(),
            Vec::new(),
            None,
            Vec::new(),
        );

        assert!(!message.is_replied());
        assert!(!message.is_forwarded());

        message.set_replied(true);
        assert!(message.is_replied());

        message.set_forwarded(true);
        assert!(message.is_forwarded());
    }

    #[test]
    fn test_attachment_info_is_user_attachment() {
        use super::AttachmentInfo;

        // Non-images are always user attachments, even with content_id or is_inline
        let pdf_att = AttachmentInfo {
            filename: "invoice.pdf".to_string(),
            size_formatted: "10 KB".to_string(),
            mime_type: "application/pdf".to_string(),
            content_id: Some("part1@example.com".to_string()),
            is_inline: true,
            content: Arc::from([]),
        };
        assert!(pdf_att.is_user_attachment());

        let doc_att = AttachmentInfo {
            filename: "notes.docx".to_string(),
            size_formatted: "25 KB".to_string(),
            mime_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string(),
            content_id: Some("part2@example.com".to_string()),
            is_inline: false,
            content: Arc::from([]),
        };
        assert!(doc_att.is_user_attachment());

        // Images with both is_inline and content_id are treated as body resources (e.g. logos)
        let inline_logo = AttachmentInfo {
            filename: "logo.png".to_string(),
            size_formatted: "5 KB".to_string(),
            mime_type: "image/png".to_string(),
            content_id: Some("logo@company.org".to_string()),
            is_inline: true,
            content: Arc::from([]),
        };
        assert!(!inline_logo.is_user_attachment());

        // Images not marked inline are user attachments
        let attached_photo = AttachmentInfo {
            filename: "photo.jpg".to_string(),
            size_formatted: "2 MB".to_string(),
            mime_type: "image/jpeg".to_string(),
            content_id: Some("photo@example.com".to_string()),
            is_inline: false,
            content: Arc::from([]),
        };
        assert!(attached_photo.is_user_attachment());

        // Images without content_id are user attachments even if disposition said inline
        let no_cid_image = AttachmentInfo {
            filename: "screenshot.png".to_string(),
            size_formatted: "100 KB".to_string(),
            mime_type: "image/png".to_string(),
            content_id: None,
            is_inline: true,
            content: Arc::from([]),
        };
        assert!(no_cid_image.is_user_attachment());
    }

    #[test]
    fn test_attachment_info_icon_and_type_description_fallback() {
        use super::AttachmentInfo;

        // 1. JPEG image sent with generic application/octet-stream MIME type
        let octet_jpg = AttachmentInfo {
            filename: "img0001.jpg".to_string(),
            size_formatted: "12.7 KiB".to_string(),
            mime_type: "application/octet-stream".to_string(),
            content_id: None,
            is_inline: false,
            content: Arc::from([]),
        };
        assert_eq!(octet_jpg.effective_mime_type(), "image/jpeg");
        assert_eq!(octet_jpg.icon_name(), "image-x-generic-symbolic");
        assert_ne!(octet_jpg.type_description().to_ascii_lowercase(), "unknown");

        // 2. Excel spreadsheet (.xlsx)
        let xlsx = AttachmentInfo {
            filename: "Fuhrparkliste vom 13. Juli 2026.xlsx".to_string(),
            size_formatted: "14.4 KiB".to_string(),
            mime_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                .to_string(),
            content_id: None,
            is_inline: false,
            content: Arc::from([]),
        };
        assert_eq!(xlsx.icon_name(), "x-office-spreadsheet-symbolic");

        // 3. Word document (.docx) and PDF (.pdf)
        assert_eq!(
            AttachmentInfo::icon_name_for("application/octet-stream", "Bericht.docx"),
            "x-office-document-symbolic"
        );
        assert_eq!(
            AttachmentInfo::icon_name_for("application/pdf", "Rechnung.pdf"),
            "x-office-document-symbolic"
        );

        // 4. Presentation (.pptx)
        assert_eq!(
            AttachmentInfo::icon_name_for("application/octet-stream", "Folien.pptx"),
            "x-office-presentation-symbolic"
        );
    }
}
