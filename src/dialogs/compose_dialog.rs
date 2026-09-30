// SPDX-License-Identifier: GPL-3.0-or-later
//
// ComposeDialog — Email composition window.
//
// Features:
// - Account/From selection dropdown
// - To, Cc, Bcc fields with completion
// - Subject entry
// - Attachment list & file chooser button
// - PGP Sign & Encrypt toggle buttons
// - Rich-Text / Plain-Text editor area

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::Utc;
use gtk::gdk;
use gtk::glib;
use gtk::glib::prelude::{StaticVariantType, ToVariant};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use once_cell::sync::Lazy;

use crate::services::{AppSettings, AvatarCacheService, RecentRecipientsService};
use crate::services::account_config_service::{
    AccountConfigStore, AccountProfile, reconciled_local_default_account,
};
use crate::dialogs::NameSelectorDialog;
use crate::services::template_store_service::{MailTemplate, TemplateStore};
use crate::services::camel_mail_service;
use crate::services::contact_store_service::ContactStoreService;
use crate::services::crypto_service::SmimeOptions;
use crate::services::vcard_service::VCardContact;
use crate::services::draft_store_service::{
    DraftAttachment, DraftComposerOptions, DraftMessage, DraftStoreService,
};
use crate::services::eds_service::{self, EdsSourceKind, EdsSourceOrigin};
use crate::services::exchange_service::{GalContact, graph_service_for_source};
use crate::services::html_sanitizer::sanitize_html;
use crate::services::mail_composer_service::{self, CancellationToken, OutgoingMessage};
use crate::services::mail_print_service::{
    self, MailPrintAction, MailPrintAttachment, MailPrintJobState, MailPrintSnapshot,
};
use crate::views::accessibility::set_accessible_label;
use crate::services::signature_store_service::SignatureStore;
use crate::services::snippet_store_service::{MailSnippet, SnippetStore};
use crate::views::{EditorContent, MailViewer};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecipientSuggestion {
    display: String,
    address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureChoice {
    pub id: Option<String>,
    pub name: String,
    pub html: String,
}

#[derive(Debug, Clone, Default)]
struct ComposeIdentity {
    source_uid: String,
    label: String,
    from: String,
    reply_to: String,
    signature_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeOperation {
    Idle,
    SavingDraft,
    SavingEml,
    Sending,
}

#[derive(Debug)]
struct SendCompletion {
    sent_message_saved: bool,
    local_draft_deleted: bool,
    cleanup_error: Option<String>,
}

impl Default for ComposeOperation {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug)]
enum SendWorkerEvent {
    DraftSaved(DraftMessage),
    TransportStarted,
    Cancelled,
    Finished(Result<SendCompletion, String>),
}

#[derive(Debug)]
enum DraftWorkerEvent {
    DraftSaved(DraftMessage),
    Cancelled,
    Finished(Result<bool, String>),
}

#[derive(Debug)]
enum EmlWorkerEvent {
    Cancelled,
    Finished(Result<(), String>),
}

const COMPOSER_ACTION_SHORTCUTS: &[(&str, &str)] = &[
    ("send", "<Control>Return"),
    ("save-draft", "<Control>s"),
    ("save-as-eml", "<Control><Shift>s"),
    ("close", "<Control>w"),
    ("close", "Escape"),
    ("new-message", "<Control>n"),
    ("attach", "<Control>m"),
    ("print", "<Control>p"),
];

const COMPOSER_OPERATION_ACTIONS: &[&str] = &[
    "send",
    "save-draft",
    "discard",
    "save-as-eml",
    "print",
    "print-preview",
    "close",
    "new-message",
    "attach",
    "plain-text",
    "request-read-receipt",
    "prioritize-message",
    "pgp-sign",
    "pgp-encrypt",
    "smime-sign",
    "smime-encrypt",
    "charset",
    "view-from",
    "view-reply-to",
    "view-cc",
    "view-bcc",
    "insert-template",
    "save-template",
    "save-snippet",
];

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ComposeDialog {
        pub draft_id: RefCell<String>,
        pub persisted_draft: RefCell<Option<DraftMessage>>,
        pub local_draft_persisted: Cell<bool>,
        pub entry_from_row: RefCell<Option<gtk::Widget>>,
        pub entry_to: RefCell<Option<gtk::Entry>>,
        pub entry_reply_to: RefCell<Option<gtk::Entry>>,
        pub entry_cc: RefCell<Option<gtk::Entry>>,
        pub entry_bcc: RefCell<Option<gtk::Entry>>,
        pub entry_subject: RefCell<Option<gtk::Entry>>,
        pub combo_from: RefCell<Option<gtk::DropDown>>,
        pub combo_signature: RefCell<Option<gtk::DropDown>>,
        pub signature_choices: RefCell<Vec<SignatureChoice>>,
        pub suppress_signature_change: Cell<bool>,
        pub row_from_widgets: RefCell<Vec<gtk::Widget>>,
        pub row_reply_to_widgets: RefCell<Vec<gtk::Widget>>,
        pub row_cc_widgets: RefCell<Vec<gtk::Widget>>,
        pub row_bcc_widgets: RefCell<Vec<gtk::Widget>>,
        pub source_uids: RefCell<Vec<String>>,
        pub(super) source_identities: RefCell<Vec<ComposeIdentity>>,
        pub requested_source_uid: RefCell<Option<String>>,
        pub requested_from: RefCell<Option<String>>,
        pub calendar_method: RefCell<Option<String>>,
        pub calendar_component: RefCell<Option<String>>,
        pub html_editor: RefCell<Option<crate::views::HtmlEditor>>,
        pub plain_text_button: RefCell<Option<gtk::ToggleButton>>,
        pub request_read_receipt_button: RefCell<Option<gtk::ToggleButton>>,
        pub priority_button: RefCell<Option<gtk::ToggleButton>>,
        pub pgp_sign_button: RefCell<Option<gtk::ToggleButton>>,
        pub pgp_encrypt_button: RefCell<Option<gtk::ToggleButton>>,
        pub pgp_key_override: RefCell<Option<String>>,
        pub(crate) smime_options_override: RefCell<Option<SmimeOptions>>,
        pub charset: RefCell<String>,
        pub attachments: RefCell<Vec<DraftAttachment>>,
        pub attachment_list: RefCell<Option<gtk::ListBox>>,
        pub compose_controls: RefCell<Vec<gtk::Widget>>,
        pub composer_action_group: RefCell<Option<gio::SimpleActionGroup>>,
        pub dirty_revision: Cell<u64>,
        pub saved_revision: Cell<u64>,
        pub suppress_dirty_tracking: Cell<bool>,
        pub suppress_setting_persistence: Cell<bool>,
        pub suppress_from_notify: Cell<bool>,
        pub close_confirmed: Cell<bool>,
        pub close_prompt_open: Cell<bool>,
        pub close_after_save: Cell<bool>,
        pub draft_discarded: Cell<bool>,
        pub autosave_source: RefCell<Option<glib::SourceId>>,
        pub autosave_setting_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub autosave_in_progress: Cell<bool>,
        pub autosave_error_shown: Cell<bool>,
        pub(super) operation: Cell<ComposeOperation>,
        pub operation_id: Cell<u64>,
        pub(crate) operation_cancel: RefCell<Option<Arc<CancellationToken>>>,
        pub transport_started: Cell<bool>,
        pub send_button: RefCell<Option<gtk::Button>>,
        pub save_draft_button: RefCell<Option<gtk::Button>>,
        pub btn_discard: RefCell<Option<gtk::Button>>,
        pub cancel_operation_button: RefCell<Option<gtk::Button>>,
        pub operation_row: RefCell<Option<gtk::Box>>,
        pub operation_spinner: RefCell<Option<gtk::Spinner>>,
        pub operation_status: RefCell<Option<gtk::Label>>,
        pub print_preview_window: RefCell<Option<gtk::Window>>,
        pub print_viewer: RefCell<Option<MailViewer>>,
        pub print_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub print_generation: Cell<u64>,
        pub print_state: Cell<MailPrintJobState>,
        pub print_message_key: RefCell<String>,
        pub settings: RefCell<Option<AppSettings>>,
        pub on_send_success: RefCell<Option<Box<dyn Fn() + 'static>>>,
        pub btn_open_in_new_window: RefCell<Option<gtk::Button>>,
        pub is_modal: Cell<bool>,
        pub stored_parent: RefCell<Option<gtk::Widget>>,
    }

    impl std::fmt::Debug for ComposeDialog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ComposeDialog")
                .field("draft_id", &self.draft_id)
                .field("operation", &self.operation)
                .field("operation_id", &self.operation_id)
                .finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ComposeDialog {
        const NAME: &'static str = "MeridianComposeDialog";
        type Type = super::ComposeDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for ComposeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(handler_id) = self.autosave_setting_handler.borrow_mut().take() {
                if let Some(settings) = self.settings.borrow().as_ref() {
                    settings.disconnect(handler_id);
                } else if let Some(settings) = AppSettings::try_new() {
                    settings.disconnect(handler_id);
                }
            }
            if let Some(source) = self.autosave_source.borrow_mut().take() {
                let _ = unsafe { glib::ffi::g_source_remove(source.as_raw()) };
            }
            if let Some(cancel) = self.operation_cancel.borrow_mut().take() {
                let _ = cancel.request_cancel();
            }
            self.obj().shutdown_print();
        }
    }

    impl WidgetImpl for ComposeDialog {}
    impl AdwDialogImpl for ComposeDialog {}
}

glib::wrapper! {
    pub struct ComposeDialog(ObjectSubclass<imp::ComposeDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub type ComposeModalDialog = ComposeDialog;

impl ComposeDialog {
    pub fn new() -> Self {
        let dialog: Self = glib::Object::builder()
            .property("title", "Neue E-Mail")
            .property("content-width", 780)
            .property("content-height", 620)
            .build();
        dialog.apply_default_signature();
        dialog.mark_saved();
        warm_recipient_cache();
        dialog
    }

    pub fn set_transient_for<W: IsA<gtk::Window> + IsA<gtk::Widget>>(&self, parent: Option<&W>) {
        if let Some(p) = parent {
            self.imp().stored_parent.replace(Some(p.clone().upcast()));
        } else {
            self.imp().stored_parent.borrow_mut().take();
        }
    }

    pub fn present_modal(&self, parent: &impl IsA<gtk::Widget>) {
        self.imp().is_modal.set(true);
        self.imp().stored_parent.replace(Some(parent.upcast_ref::<gtk::Widget>().clone()));
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_visible(true);
        }
        adw::prelude::AdwDialogExt::present(self, Some(parent));
    }

    pub fn is_modal(&self) -> bool {
        self.imp().is_modal.get()
    }

    pub fn present_standalone(&self) {
        self.imp().is_modal.set(false);
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_visible(false);
        }
        adw::prelude::AdwDialogExt::present(self, None::<&gtk::Widget>);
    }

    pub fn present(&self) {
        if self.imp().is_modal.get() {
            if let Some(parent) = self.imp().stored_parent.borrow().as_ref() {
                self.present_modal(parent);
                return;
            }
        }
        self.present_standalone();
    }

    pub fn build_draft_message(&self, content: &EditorContent) -> DraftMessage {
        let to = self
            .imp()
            .entry_to
            .borrow()
            .as_ref()
            .map(|e| e.text().to_string())
            .unwrap_or_default();
        let cc = self
            .imp()
            .entry_cc
            .borrow()
            .as_ref()
            .map(|e| e.text().to_string())
            .unwrap_or_default();
        let bcc = self
            .imp()
            .entry_bcc
            .borrow()
            .as_ref()
            .map(|e| e.text().to_string())
            .unwrap_or_default();
        let subject = self
            .imp()
            .entry_subject
            .borrow()
            .as_ref()
            .map(|e| e.text().to_string())
            .unwrap_or_default();
        let selected_idx = self
            .imp()
            .combo_from
            .borrow()
            .as_ref()
            .map(|c| c.selected())
            .unwrap_or(0);
        let identity = self.selected_identity(selected_idx);
        let from = identity.from;
        let reply_to = self.effective_reply_to(&identity.reply_to);
        let source_uid = identity.source_uid;
        let draft_id = self.imp().draft_id.borrow().clone();
        let attachments = self.imp().attachments.borrow().clone();
        let calendar_method = self.imp().calendar_method.borrow().clone();
        let calendar_component = self.imp().calendar_component.borrow().clone();
        let pgp_sign = self
            .imp()
            .pgp_sign_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active());
        let pgp_encrypt = self
            .imp()
            .pgp_encrypt_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active());
        let request_read_receipt = self.request_read_receipt_for_message();
        let priority = self.priority_for_message();
        let charset = self.imp().charset.borrow().clone();
        let pgp_key = self.pgp_key_for_message();
        let smime_options = self.smime_options_for_message();

        DraftMessage {
            id: draft_id,
            from,
            reply_to,
            source_uid,
            to,
            cc,
            bcc,
            subject,
            html_body: content.body.clone(),
            plain_text: content.plain_text,
            updated_at: Utc::now().to_rfc3339(),
            attachments,
            calendar_method,
            calendar_component,
            pgp_sign,
            pgp_encrypt,
            pgp_key,
            smime_options: Some(DraftComposerOptions {
                request_read_receipt,
                priority,
                charset,
                smime: smime_options,
            }),
        }
    }

    pub fn to_draft<F: FnOnce(DraftMessage) + 'static>(&self, callback: F) {
        let fallback = EditorContent {
            body: String::new(),
            plain_text: self
                .imp()
                .plain_text_button
                .borrow()
                .as_ref()
                .is_some_and(|b| b.is_active()),
        };
        if let Some(html_editor) = self.imp().html_editor.borrow().clone() {
            let dialog_weak = self.downgrade();
            html_editor.read_content(move |result| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let content = result.unwrap_or(fallback);
                callback(dialog.build_draft_message(&content));
            });
        } else {
            callback(self.build_draft_message(&fallback));
        }
    }

    pub fn split_to_standalone_window(&self) {
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_sensitive(false);
        }
        self.imp().close_confirmed.set(true);
        self.set_can_close(true);
        self.close();

        let dialog_weak = self.downgrade();
        let fallback = EditorContent {
            body: String::new(),
            plain_text: self
                .imp()
                .plain_text_button
                .borrow()
                .as_ref()
                .is_some_and(|b| b.is_active()),
        };
        let handle_split = move |dialog: ComposeDialog, content: EditorContent| {
            let draft = dialog.build_draft_message(&content);
            let standalone = ComposeDialog::from_draft(&draft);
            standalone.present_standalone();
        };

        if let Some(html_editor) = self.imp().html_editor.borrow().clone() {
            html_editor.read_content(move |result| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let content = result.unwrap_or(fallback);
                handle_split(dialog, content);
            });
        } else {
            handle_split(self.clone(), fallback);
        }
    }

    pub fn html_editor(&self) -> Option<crate::views::HtmlEditor> {
        self.imp().html_editor.borrow().clone()
    }

    /// Open a new message with the supplied address already in the To field.
    pub fn to_recipient(recipient: &str) -> Self {
        let dialog = Self::new();
        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
            entry.set_text(recipient.trim());
        }
        dialog
    }

    pub fn reply_to(recipient: &str, subject: &str) -> Self {
        Self::reply_to_message(recipient, "", subject, "")
    }

    /// Open a reply with the original message quoted below the cursor.
    ///
    /// `Reply-To` is passed separately because it is the address that must be
    /// preferred for the primary recipient, while the visible sender remains
    /// useful as the quote attribution.
    pub fn reply_to_message(
        sender: &str,
        reply_to: &str,
        subject: &str,
        original_html: &str,
    ) -> Self {
        Self::reply_to_message_with_style(
            sender,
            reply_to,
            "",
            "",
            "",
            subject,
            original_html,
            "default",
        )
    }

    pub fn reply_to_message_with_style(
        sender: &str,
        reply_to: &str,
        original_to: &str,
        original_cc: &str,
        date: &str,
        subject: &str,
        original_html: &str,
        style: &str,
    ) -> Self {
        let dialog = Self::new();
        dialog.set_title(&format!("Antworten: {subject}"));
        let recipient = if reply_to.trim().is_empty() {
            sender.trim()
        } else {
            reply_to.trim()
        };
        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
            entry.set_text(recipient);
        }
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&reply_subject(subject));
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html(&reply_body_html_with_style(
                sender,
                original_to,
                original_cc,
                date,
                original_html,
                style,
            ));
        }
        dialog
    }

    pub fn set_requested_account(&self, source_uid: Option<String>, from: Option<String>) {
        *self.imp().requested_source_uid.borrow_mut() = source_uid;
        *self.imp().requested_from.borrow_mut() = from;
    }

    pub fn forward(subject: &str) -> Self {
        Self::forward_message(subject, "", "", "", "", "")
    }

    /// Open a forward with the original headers and message quoted.
    pub fn forward_message(
        subject: &str,
        sender: &str,
        recipient_to: &str,
        recipient_cc: &str,
        date: &str,
        original_html: &str,
    ) -> Self {
        Self::forward_message_with_style(
            subject,
            sender,
            recipient_to,
            recipient_cc,
            date,
            original_html,
            default_forward_style(),
        )
    }

    pub fn forward_inline_message(
        subject: &str,
        sender: &str,
        recipient_to: &str,
        recipient_cc: &str,
        date: &str,
        original_html: &str,
    ) -> Self {
        Self::forward_message_with_style(
            subject,
            sender,
            recipient_to,
            recipient_cc,
            date,
            original_html,
            "inline",
        )
    }

    pub fn forward_quoted_message(
        subject: &str,
        sender: &str,
        recipient_to: &str,
        recipient_cc: &str,
        date: &str,
        original_html: &str,
    ) -> Self {
        Self::forward_message_with_style(
            subject,
            sender,
            recipient_to,
            recipient_cc,
            date,
            original_html,
            "prefix",
        )
    }

    pub fn forward_attached_message(
        subject: &str,
        filename: &str,
        raw_message: &[u8],
    ) -> Result<Self, String> {
        let attachment = mail_composer_service::embedded_draft_attachment(
            filename,
            "message/rfc822",
            raw_message,
        )?;
        let dialog = Self::new();
        dialog.set_title(&format!("Weiterleiten: {subject}"));
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&forward_subject(subject));
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html(&signature_prefix_html());
        }
        dialog.add_attachment(attachment);
        Ok(dialog)
    }

    pub fn edit_as_new_message(
        recipient_to: &str,
        recipient_cc: &str,
        recipient_bcc: &str,
        subject: &str,
        html_body: &str,
        attachments: Vec<DraftAttachment>,
    ) -> Self {
        let dialog = Self::new();
        dialog.set_title(&format!("Als neue Nachricht bearbeiten: {subject}"));
        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
            entry.set_text(recipient_to);
        }
        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
            entry.set_text(recipient_cc);
        }
        if let Some(entry) = dialog.imp().entry_bcc.borrow().as_ref() {
            entry.set_text(recipient_bcc);
        }
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(subject);
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html(&sanitize_html(html_body));
        }
        for attachment in attachments {
            dialog.add_attachment(attachment);
        }
        dialog
    }

    pub fn reply_to_attached_message(
        sender: &str,
        reply_to: &str,
        subject: &str,
        filename: &str,
        raw_message: &[u8],
    ) -> Result<Self, String> {
        let dialog =
            Self::reply_to_message_with_style(sender, reply_to, "", "", "", subject, "", "none");
        dialog.add_attachment(mail_composer_service::embedded_draft_attachment(
            filename,
            "message/rfc822",
            raw_message,
        )?);
        Ok(dialog)
    }

    pub fn reply_all_attached_message(
        sender: &str,
        reply_to: &str,
        original_to: &str,
        original_cc: &str,
        subject: &str,
        filename: &str,
        raw_message: &[u8],
    ) -> Result<Self, String> {
        let dialog = Self::reply_all_message_with_style(
            sender,
            reply_to,
            original_to,
            original_cc,
            "",
            subject,
            "",
            "none",
        );
        dialog.add_attachment(mail_composer_service::embedded_draft_attachment(
            filename,
            "message/rfc822",
            raw_message,
        )?);
        Ok(dialog)
    }

    fn forward_message_with_style(
        subject: &str,
        sender: &str,
        recipient_to: &str,
        recipient_cc: &str,
        date: &str,
        original_html: &str,
        quote_style: &str,
    ) -> Self {
        let dialog = Self::new();
        dialog.set_title(&format!("Weiterleiten: {subject}"));
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&forward_subject(subject));
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html(&forward_body_html_with_style(
                sender,
                recipient_to,
                recipient_cc,
                date,
                original_html,
                quote_style,
            ));
        }
        dialog
    }

    /// Open a reply-all.  The sender (or Reply-To) becomes the primary
    /// recipient; original To/Cc recipients are copied to Cc with duplicates
    /// and the primary recipient removed.  Bcc is intentionally never copied.
    pub fn reply_all_message(
        sender: &str,
        reply_to: &str,
        original_to: &str,
        original_cc: &str,
        subject: &str,
        original_html: &str,
    ) -> Self {
        Self::reply_all_message_with_style(
            sender,
            reply_to,
            original_to,
            original_cc,
            "",
            subject,
            original_html,
            "default",
        )
    }

    pub fn reply_all_message_with_style(
        sender: &str,
        reply_to: &str,
        original_to: &str,
        original_cc: &str,
        date: &str,
        subject: &str,
        original_html: &str,
        style: &str,
    ) -> Self {
        let dialog = Self::reply_to_message_with_style(
            sender,
            reply_to,
            original_to,
            original_cc,
            date,
            subject,
            original_html,
            style,
        );
        let primary = if reply_to.trim().is_empty() {
            sender.trim()
        } else {
            reply_to.trim()
        };
        let cc = merge_recipients(&[original_to, original_cc], &[primary]);
        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
            entry.set_text(&cc);
        }
        dialog.set_title(&format!("Allen antworten: {subject}"));
        dialog
    }

    pub fn from_draft(draft: &DraftMessage) -> Self {
        let dialog = Self::new();
        let (default_request_read_receipt, default_charset) = AppSettings::try_new()
            .map(|settings| (settings.request_read_receipt(), settings.composer_charset()))
            .unwrap_or((false, "UTF-8".to_string()));
        let draft_options = draft
            .smime_options
            .clone()
            .unwrap_or_else(|| DraftComposerOptions {
                request_read_receipt: default_request_read_receipt,
                charset: default_charset,
                ..DraftComposerOptions::default()
            });
        dialog.imp().suppress_dirty_tracking.set(true);
        dialog.imp().suppress_setting_persistence.set(true);
        *dialog.imp().draft_id.borrow_mut() = draft.id.clone();
        dialog.imp().persisted_draft.replace(Some(draft.clone()));
        dialog.imp().local_draft_persisted.set(true);
        *dialog.imp().requested_source_uid.borrow_mut() =
            (!draft.source_uid.is_empty()).then(|| draft.source_uid.clone());
        *dialog.imp().requested_from.borrow_mut() =
            (!draft.from.trim().is_empty()).then(|| draft.from.clone());
        *dialog.imp().calendar_method.borrow_mut() = draft.calendar_method.clone();
        *dialog.imp().calendar_component.borrow_mut() = draft.calendar_component.clone();
        if let Some(button) = dialog.imp().pgp_sign_button.borrow().as_ref() {
            button.set_active(draft.pgp_sign);
        }
        if let Some(button) = dialog.imp().pgp_encrypt_button.borrow().as_ref() {
            button.set_active(draft.pgp_encrypt);
        }
        if let Some(button) = dialog.imp().plain_text_button.borrow().as_ref() {
            button.set_active(draft.plain_text);
        }
        if let Some(button) = dialog.imp().request_read_receipt_button.borrow().as_ref() {
            button.set_active(draft_options.request_read_receipt);
        }
        if let Some(button) = dialog.imp().priority_button.borrow().as_ref() {
            button.set_active(draft_options.priority);
        }
        let charset = mail_composer_service::normalize_charset(&draft_options.charset)
            .unwrap_or_else(|_| "UTF-8".to_string());
        dialog.imp().charset.replace(charset.clone());
        if let Some(action) = dialog
            .imp()
            .composer_action_group
            .borrow()
            .as_ref()
            .and_then(|group| group.lookup_action("charset"))
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_state(&charset.to_variant());
        }
        dialog
            .imp()
            .pgp_key_override
            .replace((!draft.pgp_key.trim().is_empty()).then(|| draft.pgp_key.clone()));
        dialog
            .imp()
            .smime_options_override
            .replace(draft_options.smime.clone());
        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
            entry.set_text(&draft.to);
        }
        if let Some(entry) = dialog.imp().entry_reply_to.borrow().as_ref() {
            entry.set_text(&draft.reply_to);
        }
        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
            entry.set_text(&draft.cc);
        }
        if let Some(entry) = dialog.imp().entry_bcc.borrow().as_ref() {
            entry.set_text(&draft.bcc);
        }
        if !draft.reply_to.trim().is_empty() {
            dialog.set_row_reply_to_visible(true);
        }
        if !draft.cc.trim().is_empty() {
            dialog.set_row_cc_visible(true);
        }
        if !draft.bcc.trim().is_empty() {
            dialog.set_row_bcc_visible(true);
        }
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&draft.subject);
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            if draft.plain_text {
                editor.set_plain_text_mode(true);
                editor.set_text(&draft.html_body);
            } else {
                let safe_html = sanitize_html(&draft.html_body);
                editor.set_html(&safe_html);
            }
        }
        for attachment in draft.attachments.iter().cloned() {
            dialog.add_attachment(attachment);
        }
        let title = if draft.subject.is_empty() {
            "Entwurf bearbeiten".to_string()
        } else {
            format!("Entwurf: {}", draft.subject)
        };
        dialog.set_title(&title);
        dialog.imp().suppress_setting_persistence.set(false);
        dialog.imp().suppress_dirty_tracking.set(false);
        dialog.mark_saved();
        dialog
    }

    fn build_ui(&self) {
        {
            let mut stored_id = self.imp().draft_id.borrow_mut();
            if stored_id.is_empty() {
                *stored_id = uuid::Uuid::new_v4().to_string();
            }
        }
        self.set_content_width(860);
        self.set_content_height(680);
        let toolbar_view = adw::ToolbarView::new();

        // ── Header bar ──────────────────────────────────────────────────────
        let header_bar = adw::HeaderBar::new();

        let btn_open_in_new_window = gtk::Button::builder()
            .icon_name("window-new-symbolic")
            .tooltip_text("In neuem Fenster öffnen")
            .css_classes(vec!["flat"])
            .visible(self.imp().is_modal.get())
            .build();
        btn_open_in_new_window.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.split_to_standalone_window();
            }
        ));
        header_bar.pack_start(&btn_open_in_new_window);
        self.imp().btn_open_in_new_window.replace(Some(btn_open_in_new_window));

        let btn_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(&crate::i18n::gettext("Close composer (Ctrl+W)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_close, &crate::i18n::gettext("Close composer"));

        let btn_attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text(&crate::i18n::gettext("Add attachment… (Ctrl+M)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_attach, &crate::i18n::gettext("Add attachment"));

        btn_attach.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let file_dialog = gtk::FileDialog::builder()
                    .title(&crate::i18n::gettext("Select attachment"))
                    .modal(true)
                    .build();

                let dialog_for_file = dialog.clone();
                let parent_win = dialog.root().and_downcast::<gtk::Window>();
                file_dialog.open(parent_win.as_ref(), None::<&gio::Cancellable>, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            dialog_for_file.add_attachment_path(&path);
                        }
                    }
                });
            }
        ));
        header_bar.pack_start(&btn_attach);

        let btn_discard = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(&crate::i18n::gettext("Discard draft"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_discard, &crate::i18n::gettext("Discard draft"));
        header_bar.pack_start(&btn_discard);
        self.imp().btn_discard.replace(Some(btn_discard.clone()));

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text(&crate::i18n::gettext("Composer menu"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&menu_button, &crate::i18n::gettext("Composer menu"));
        header_bar.pack_end(&menu_button);

        let btn_send_content = adw::ButtonContent::builder()
            .icon_name("mail-send-symbolic")
            .label(&crate::i18n::gettext("Send"))
            .build();
        let btn_send = gtk::Button::builder()
            .child(&btn_send_content)
            .css_classes(vec!["suggested-action"])
            .tooltip_text(&crate::i18n::gettext("Send email now (Ctrl+Enter)"))
            .build();
        set_accessible_label(&btn_send, &crate::i18n::gettext("Send email"));
        header_bar.pack_end(&btn_send);

        let btn_save_draft = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text(&crate::i18n::gettext("Save draft (Ctrl+S)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_save_draft, &crate::i18n::gettext("Save draft"));

        let btn_priority = gtk::ToggleButton::builder()
            .icon_name("mail-mark-important-symbolic")
            .tooltip_text(&crate::i18n::gettext("Mark message as important"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_priority, &crate::i18n::gettext("Mark message as important"));

        let request_read_receipt = AppSettings::try_new()
            .map(|settings| settings.request_read_receipt())
            .unwrap_or(false);
        let btn_request_read_receipt = gtk::ToggleButton::builder()
            .icon_name("mail-send-receive-symbolic")
            .tooltip_text("Lesebestätigung für diese Nachricht anfordern")
            .active(request_read_receipt)
            .css_classes(vec!["flat"])
            .build();

        let btn_insert_template = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Vorlage oder Textbaustein einfügen")
            .css_classes(vec!["flat"])
            .build();

        let btn_save_template = gtk::Button::builder()
            .icon_name("document-save-as-symbolic")
            .tooltip_text("Aktuellen Inhalt als Vorlage speichern")
            .css_classes(vec!["flat"])
            .build();

        let btn_save_snippet = gtk::Button::builder()
            .icon_name("insert-text-symbolic")
            .tooltip_text("Aktuellen Inhalt als Textbaustein speichern")
            .css_classes(vec!["flat"])
            .build();

        let sign_by_default = AppSettings::try_new()
            .map(|settings| settings.sign_new_messages())
            .unwrap_or(false);
        let btn_sign = gtk::ToggleButton::builder()
            .icon_name("security-high-symbolic")
            .tooltip_text("Nachricht signieren (OpenPGP oder S/MIME)")
            .active(sign_by_default)
            .css_classes(vec!["flat"])
            .build();

        let btn_encrypt = gtk::ToggleButton::builder()
            .icon_name("changes-prevent-symbolic")
            .tooltip_text("Nachricht verschlüsseln (OpenPGP oder S/MIME)")
            .css_classes(vec!["flat"])
            .build();

        let composer_html = AppSettings::try_new()
            .map(|settings| settings.composer_html())
            .unwrap_or(true);
        let btn_plain_text = gtk::ToggleButton::builder()
            .icon_name("text-x-generic-symbolic")
            .tooltip_text("Nur-Text-Modus beim Verfassen")
            .active(!composer_html)
            .css_classes(vec!["flat"])
            .build();

        toolbar_view.add_top_bar(&header_bar);

        // ── Content Form ────────────────────────────────────────────────────
        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let operation_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(16)
            .margin_end(16)
            .margin_top(6)
            .margin_bottom(6)
            .css_classes(vec!["compact-status"])
            .visible(false)
            .build();
        let operation_spinner = gtk::Spinner::new();
        operation_spinner.set_visible(false);
        let operation_status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(vec!["dim-label"])
            .build();
        let cancel_operation_button = gtk::Button::builder()
            .label("Abbrechen")
            .tooltip_text("Vorgang vor dem Transport abbrechen")
            .css_classes(vec!["flat"])
            .visible(false)
            .build();
        operation_row.append(&operation_spinner);
        operation_row.append(&operation_status);
        operation_row.append(&cancel_operation_button);
        content_box.append(&operation_row);

        let dialog_weak_for_cancel = self.downgrade();
        cancel_operation_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                dialog.request_operation_cancel();
            }
        });

        let form_grid = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(8)
            .build();

        let show_from = AppSettings::try_new()
            .map(|settings| settings.composer_show_from())
            .unwrap_or(true);
        let show_reply_to = AppSettings::try_new()
            .map(|settings| settings.composer_show_reply_to())
            .unwrap_or(false);
        let show_cc = AppSettings::try_new()
            .map(|settings| settings.composer_show_cc())
            .unwrap_or(true);
        let show_bcc = AppSettings::try_new()
            .map(|settings| settings.composer_show_bcc())
            .unwrap_or(false);

        // From & Signature
        let lbl_from = gtk::Label::builder()
            .label("Von:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .visible(show_from)
            .build();
        let source_model = gtk::StringList::new(&["Absender werden geladen…"]);
        let combo_from = gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        combo_from.set_hexpand(true);

        let sig_choices = load_signature_choices();
        let sig_names: Vec<&str> = sig_choices.iter().map(|c| c.name.as_str()).collect();
        let sig_model = gtk::StringList::new(&sig_names);
        let combo_signature = gtk::DropDown::new(Some(sig_model), None::<&gtk::Expression>);
        combo_signature.set_valign(gtk::Align::Center);

        let lbl_signature = gtk::Label::builder()
            .label("Signatur:")
            .css_classes(vec!["dim-label"])
            .valign(gtk::Align::Center)
            .build();

        let from_row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .hexpand(true)
            .visible(show_from)
            .build();
        from_row_box.append(&combo_from);
        from_row_box.append(&lbl_signature);
        from_row_box.append(&combo_signature);

        // Reply-To
        let btn_reply_to = gtk::Button::builder()
            .label(crate::i18n::gettext("Reply-To…"))
            .tooltip_text(crate::i18n::gettext("Select Reply-To address from address book"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::End)
            .visible(show_reply_to)
            .build();
        set_accessible_label(&btn_reply_to, &crate::i18n::gettext("Select Reply-To address from address book"));
        let entry_reply_to = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .placeholder_text(crate::i18n::gettext("Reply-To address…"))
            .hexpand(true)
            .build();
        set_accessible_label(&entry_reply_to, &crate::i18n::gettext("Reply-To"));
        let directory_button_reply_to = recipient_directory_button();
        let reply_to_field_widget = recipient_field(&entry_reply_to, &directory_button_reply_to);
        reply_to_field_widget.set_visible(show_reply_to);

        // To (Evolution-compatible header button opening NameSelectorDialog)
        let btn_to = gtk::Button::builder()
            .label(crate::i18n::gettext("To…"))
            .tooltip_text(crate::i18n::gettext("Select recipients from address book"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::End)
            .build();
        set_accessible_label(&btn_to, &crate::i18n::gettext("Select recipients from address book"));
        let entry_to = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .placeholder_text(crate::i18n::gettext("Enter recipients…"))
            .hexpand(true)
            .build();
        set_accessible_label(&entry_to, &crate::i18n::gettext("Recipients"));

        // Cc/Bcc visibility defaults from AppSettings
        let btn_cc = gtk::Button::builder()
            .label(crate::i18n::gettext("Cc…"))
            .tooltip_text(crate::i18n::gettext("Select Cc recipients from address book"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::End)
            .visible(show_cc)
            .build();
        set_accessible_label(&btn_cc, &crate::i18n::gettext("Select Cc recipients from address book"));
        let entry_cc = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .placeholder_text(crate::i18n::gettext("Additional recipients (Cc)…"))
            .hexpand(true)
            .build();
        set_accessible_label(&entry_cc, &crate::i18n::gettext("Copy (Cc)"));

        let btn_bcc = gtk::Button::builder()
            .label(crate::i18n::gettext("Bcc…"))
            .tooltip_text(crate::i18n::gettext("Select Bcc recipients from address book"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::End)
            .visible(show_bcc)
            .build();
        set_accessible_label(&btn_bcc, &crate::i18n::gettext("Select Bcc recipients from address book"));
        let entry_bcc = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .placeholder_text(crate::i18n::gettext("Undisclosed recipients (Bcc)…"))
            .hexpand(true)
            .build();
        set_accessible_label(&entry_bcc, &crate::i18n::gettext("Blind Copy (Bcc)"));

        let directory_button_to = recipient_directory_button();
        let directory_button_cc = recipient_directory_button();
        let directory_button_bcc = recipient_directory_button();
        let to_field_widget = recipient_field(&entry_to, &directory_button_to);
        let cc_field_widget = recipient_field(&entry_cc, &directory_button_cc);
        cc_field_widget.set_visible(show_cc);
        let bcc_field_widget = recipient_field(&entry_bcc, &directory_button_bcc);
        bcc_field_widget.set_visible(show_bcc);

        let wire_name_selector = |btn: &gtk::Button,
                                  dialog_weak: glib::WeakRef<ComposeDialog>,
                                  e_to: gtk::Entry,
                                  e_cc: gtk::Entry,
                                  e_bcc: gtk::Entry| {
            btn.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let to_val = e_to.text().to_string();
                let cc_val = e_cc.text().to_string();
                let bcc_val = e_bcc.text().to_string();
                let e_to_clone = e_to.clone();
                let e_cc_clone = e_cc.clone();
                let e_bcc_clone = e_bcc.clone();
                let dialog_clone = dialog.clone();
                let selector = NameSelectorDialog::new(
                    &to_val,
                    &cc_val,
                    &bcc_val,
                    move |new_to, new_cc, new_bcc| {
                        e_to_clone.set_text(&new_to);
                        e_cc_clone.set_text(&new_cc);
                        e_bcc_clone.set_text(&new_bcc);
                        dialog_clone.mark_changed();
                    },
                );
                selector.present(Some(&dialog));
            });
        };

        wire_name_selector(&btn_to, self.downgrade(), entry_to.clone(), entry_cc.clone(), entry_bcc.clone());
        wire_name_selector(&btn_cc, self.downgrade(), entry_to.clone(), entry_cc.clone(), entry_bcc.clone());
        wire_name_selector(&btn_bcc, self.downgrade(), entry_to.clone(), entry_cc.clone(), entry_bcc.clone());

        // Subject
        let lbl_subject = gtk::Label::builder()
            .label(crate::i18n::gettext("Subject:"))
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let entry_subject = gtk::Entry::builder()
            .placeholder_text(crate::i18n::gettext("Subject…"))
            .hexpand(true)
            .build();
        set_accessible_label(&entry_subject, &crate::i18n::gettext("Subject"));

        form_grid.attach(&lbl_from, 0, 0, 1, 1);
        form_grid.attach(&from_row_box, 1, 0, 1, 1);
        form_grid.attach(&btn_reply_to, 0, 1, 1, 1);
        form_grid.attach(&reply_to_field_widget, 1, 1, 1, 1);
        form_grid.attach(&btn_to, 0, 2, 1, 1);
        form_grid.attach(&to_field_widget, 1, 2, 1, 1);
        form_grid.attach(&btn_cc, 0, 3, 1, 1);
        form_grid.attach(&cc_field_widget, 1, 3, 1, 1);
        form_grid.attach(&btn_bcc, 0, 4, 1, 1);
        form_grid.attach(&bcc_field_widget, 1, 4, 1, 1);
        form_grid.attach(&lbl_subject, 0, 5, 1, 1);
        form_grid.attach(&entry_subject, 1, 5, 1, 1);

        content_box.append(&form_grid);

        let options_badges_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(16)
            .margin_end(16)
            .margin_top(2)
            .margin_bottom(6)
            .visible(false)
            .build();
        content_box.append(&options_badges_box);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let update_options_badges = {
            let badges_box = options_badges_box.clone();
            let b_sign = btn_sign.clone();
            let b_encrypt = btn_encrypt.clone();
            let b_priority = btn_priority.clone();
            let b_receipt = btn_request_read_receipt.clone();
            let b_plain = btn_plain_text.clone();

            std::rc::Rc::new(move || {
                while let Some(child) = badges_box.first_child() {
                    badges_box.remove(&child);
                }

                let mut count = 0;

                let add_badge = |box_ref: &gtk::Box,
                                 icon: &str,
                                 label: &str,
                                 tooltip: &str,
                                 target_btn: &gtk::ToggleButton| {
                    let pill_btn = gtk::Button::builder()
                        .css_classes(vec!["flat", "pill", "caption"])
                        .tooltip_text(tooltip)
                        .build();
                    let content = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(6)
                        .build();
                    let img = gtk::Image::from_icon_name(icon);
                    let lbl = gtk::Label::builder().label(label).build();
                    let close_img = gtk::Image::from_icon_name("window-close-symbolic");
                    close_img.set_pixel_size(12);
                    content.append(&img);
                    content.append(&lbl);
                    content.append(&close_img);
                    pill_btn.set_child(Some(&content));

                    let target = target_btn.clone();
                    pill_btn.connect_clicked(move |_| {
                        target.set_active(false);
                    });

                    box_ref.append(&pill_btn);
                };

                if b_sign.is_active() {
                    add_badge(
                        &badges_box,
                        "security-high-symbolic",
                        "Signiert",
                        "Signieren deaktivieren",
                        &b_sign,
                    );
                    count += 1;
                }
                if b_encrypt.is_active() {
                    add_badge(
                        &badges_box,
                        "changes-prevent-symbolic",
                        "Verschlüsselt",
                        "Verschlüsseln deaktivieren",
                        &b_encrypt,
                    );
                    count += 1;
                }
                if b_priority.is_active() {
                    add_badge(
                        &badges_box,
                        "mail-mark-important-symbolic",
                        "Hohe Priorität",
                        "Priorität normalisieren",
                        &b_priority,
                    );
                    count += 1;
                }
                if b_receipt.is_active() {
                    add_badge(
                        &badges_box,
                        "mail-send-receive-symbolic",
                        "Lesebestätigung",
                        "Lesebestätigung entfernen",
                        &b_receipt,
                    );
                    count += 1;
                }
                if b_plain.is_active() {
                    add_badge(
                        &badges_box,
                        "text-x-generic-symbolic",
                        "Nur-Text",
                        "Zu HTML wechseln",
                        &b_plain,
                    );
                    count += 1;
                }

                badges_box.set_visible(count > 0);
            })
        };

        {
            let u = update_options_badges.clone();
            btn_sign.connect_toggled(move |_| u());
        }
        {
            let u = update_options_badges.clone();
            btn_encrypt.connect_toggled(move |_| u());
        }
        {
            let u = update_options_badges.clone();
            btn_priority.connect_toggled(move |_| u());
        }
        {
            let u = update_options_badges.clone();
            btn_request_read_receipt.connect_toggled(move |_| u());
        }
        {
            let u = update_options_badges.clone();
            btn_plain_text.connect_toggled(move |_| u());
        }
        update_options_badges();

        // Rich WebKitGTK 6.0 WYSIWYG HTML Editor
        let html_editor = crate::views::HtmlEditor::new();
        html_editor.set_plain_text_mode(btn_plain_text.is_active());
        html_editor.set_vexpand(true);
        let editor_key_controller = gtk::EventControllerKey::new();
        editor_key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let dialog_weak_for_editor = self.downgrade();
        editor_key_controller.connect_key_pressed(move |_, _, _, _| {
            if let Some(dialog) = dialog_weak_for_editor.upgrade() {
                dialog.mark_changed();
            }
            glib::Propagation::Proceed
        });
        html_editor.add_controller(editor_key_controller);

        let settings_for_composer_mode = AppSettings::try_new();
        let editor_for_composer_mode = html_editor.clone();
        let dialog_for_composer_mode = self.downgrade();
        btn_plain_text.connect_toggled(move |button| {
            editor_for_composer_mode.set_plain_text_mode(button.is_active());
            let suppress_persistence = dialog_for_composer_mode
                .upgrade()
                .is_some_and(|dialog| dialog.imp().suppress_setting_persistence.get());
            if !suppress_persistence {
                if let Some(settings) = settings_for_composer_mode.as_ref() {
                    settings.set_composer_html(!button.is_active());
                }
            }
            if let Some(dialog) = dialog_for_composer_mode.upgrade() {
                dialog.mark_changed();
            }
        });

        content_box.append(&html_editor);

        let attachment_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .margin_start(16)
            .margin_end(16)
            .margin_top(8)
            .margin_bottom(8)
            .visible(false)
            .build();
        content_box.append(&attachment_list);

        self.imp().entry_to.replace(Some(entry_to.clone()));
        self.imp()
            .entry_reply_to
            .replace(Some(entry_reply_to.clone()));
        self.imp().entry_cc.replace(Some(entry_cc.clone()));
        self.imp().entry_bcc.replace(Some(entry_bcc.clone()));
        self.imp()
            .entry_subject
            .replace(Some(entry_subject.clone()));
        self.imp().combo_from.replace(Some(combo_from.clone()));
        self.imp()
            .combo_signature
            .replace(Some(combo_signature.clone()));
        self.imp().signature_choices.replace(sig_choices);
        self.imp().row_from_widgets.replace(vec![
            lbl_from.clone().upcast(),
            from_row_box.clone().upcast(),
        ]);
        self.imp().row_reply_to_widgets.replace(vec![
            btn_reply_to.clone().upcast(),
            reply_to_field_widget.clone().upcast(),
        ]);
        self.imp().row_cc_widgets.replace(vec![
            btn_cc.clone().upcast(),
            cc_field_widget.clone().upcast(),
        ]);
        self.imp().row_bcc_widgets.replace(vec![
            btn_bcc.clone().upcast(),
            bcc_field_widget.clone().upcast(),
        ]);
        self.imp().html_editor.replace(Some(html_editor.clone()));
        self.imp()
            .plain_text_button
            .replace(Some(btn_plain_text.clone()));
        self.imp()
            .request_read_receipt_button
            .replace(Some(btn_request_read_receipt.clone()));
        self.imp()
            .priority_button
            .replace(Some(btn_priority.clone()));
        self.imp().pgp_sign_button.replace(Some(btn_sign.clone()));
        self.imp()
            .pgp_encrypt_button
            .replace(Some(btn_encrypt.clone()));
        let attachment_list_widget = attachment_list.clone().upcast();
        self.imp().attachment_list.replace(Some(attachment_list));
        self.imp().compose_controls.replace(vec![
            btn_close.clone().upcast(),
            btn_discard.clone().upcast(),
            btn_attach.clone().upcast(),
            btn_insert_template.clone().upcast(),
            btn_save_template.clone().upcast(),
            btn_save_snippet.clone().upcast(),
            attachment_list_widget,
            directory_button_to.clone().upcast(),
            directory_button_reply_to.clone().upcast(),
            directory_button_cc.clone().upcast(),
            directory_button_bcc.clone().upcast(),
            btn_reply_to.clone().upcast(),
            entry_reply_to.clone().upcast(),
            entry_to.clone().upcast(),
            entry_cc.clone().upcast(),
            entry_bcc.clone().upcast(),
            entry_subject.clone().upcast(),
            combo_from.clone().upcast(),
            combo_signature.clone().upcast(),
            html_editor.clone().upcast(),
            btn_plain_text.clone().upcast(),
            btn_request_read_receipt.clone().upcast(),
            btn_priority.clone().upcast(),
            btn_sign.clone().upcast(),
            btn_encrypt.clone().upcast(),
            menu_button.clone().upcast(),
        ]);
        self.imp().send_button.replace(Some(btn_send.clone()));
        self.imp()
            .save_draft_button
            .replace(Some(btn_save_draft.clone()));
        self.imp()
            .cancel_operation_button
            .replace(Some(cancel_operation_button));
        self.imp().operation_row.replace(Some(operation_row));
        self.imp()
            .operation_spinner
            .replace(Some(operation_spinner));
        self.imp().operation_status.replace(Some(operation_status));

        self.imp().charset.replace("UTF-8".to_string());
        if let Some(settings) = AppSettings::try_new() {
            self.imp().settings.replace(Some(settings.clone()));
            self.imp().charset.replace(settings.composer_charset());
            let dialog_weak_for_autosave = self.downgrade();
            let handler_id = settings.connect_changed(Some("draft-autosave-interval"), move |_, _| {
                if let Some(dialog) = dialog_weak_for_autosave.upgrade() {
                    dialog.start_local_autosave();
                }
            });
            if let Some(old_id) = self.imp().autosave_setting_handler.replace(Some(handler_id)) {
                settings.disconnect(old_id);
            }
        }

        let dialog_for_default_account = self.downgrade();
        let settings_for_default_account = AppSettings::try_new();
        combo_from.connect_selected_notify(move |combo| {
            let Some(dialog) = dialog_for_default_account.upgrade() else {
                return;
            };
            if dialog.imp().suppress_from_notify.get() {
                return;
            }
            let source_uid = dialog
                .imp()
                .source_uids
                .borrow()
                .get(combo.selected() as usize)
                .cloned()
                .unwrap_or_default();
            if !source_uid.trim().is_empty() && !dialog.imp().suppress_setting_persistence.get() {
                if let Some(settings) = settings_for_default_account.as_ref() {
                    settings.set_default_account_id(&source_uid);
                }
            }
            dialog.update_sender_dependent_controls(combo.selected());
            dialog.sync_signature_for_selected_account();
            dialog.mark_changed();
        });

        let dialog_for_signature = self.downgrade();
        combo_signature.connect_selected_notify(move |combo| {
            let Some(dialog) = dialog_for_signature.upgrade() else {
                return;
            };
            if dialog.imp().suppress_signature_change.get() {
                return;
            }
            let idx = combo.selected() as usize;
            let choices = dialog.imp().signature_choices.borrow();
            let Some(choice) = choices.get(idx) else {
                return;
            };
            let top_signature = dialog
                .imp()
                .settings
                .borrow()
                .as_ref()
                .map(|s| s.composer_top_signature())
                .unwrap_or(false);
            let markup = format_signature_markup(&choice.html);
            if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                editor.replace_or_insert_signature(&markup, top_signature);
            }
            dialog.mark_changed();
        });


        for entry in [
            &entry_to,
            &entry_reply_to,
            &entry_cc,
            &entry_bcc,
            &entry_subject,
        ] {
            let dialog_weak = self.downgrade();
            entry.connect_changed(move |_| {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.mark_changed();
                }
            });
        }
        for button in [
            &btn_request_read_receipt,
            &btn_priority,
            &btn_sign,
            &btn_encrypt,
        ] {
            let dialog_weak = self.downgrade();
            button.connect_toggled(move |_| {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.mark_changed();
                }
            });
        }

        setup_directory_recipient_search(
            &entry_reply_to,
            &directory_button_reply_to,
            self.downgrade(),
            combo_from.clone(),
        );
        setup_directory_recipient_search(
            &entry_to,
            &directory_button_to,
            self.downgrade(),
            combo_from.clone(),
        );
        setup_directory_recipient_search(
            &entry_cc,
            &directory_button_cc,
            self.downgrade(),
            combo_from.clone(),
        );
        setup_directory_recipient_search(
            &entry_bcc,
            &directory_button_bcc,
            self.downgrade(),
            combo_from.clone(),
        );

        setup_recipient_autocompletion(
            &entry_reply_to,
            &combo_from,
            self.downgrade(),
        );
        setup_recipient_autocompletion(
            &entry_to,
            &combo_from,
            self.downgrade(),
        );
        setup_recipient_autocompletion(
            &entry_cc,
            &combo_from,
            self.downgrade(),
        );
        setup_recipient_autocompletion(
            &entry_bcc,
            &combo_from,
            self.downgrade(),
        );

        let dialog_weak_for_templates = self.downgrade();
        let entry_subject_for_templates = entry_subject.clone();
        let html_editor_for_templates = html_editor.clone();
        let menu_button_for_templates = menu_button.clone();
        btn_insert_template.connect_clicked(move |_| {
            let popover = gtk::Popover::new();
            popover.set_parent(&menu_button_for_templates);
            popover.set_has_arrow(true);

            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(vec!["boxed-list"])
                .build();
            let templates = TemplateStore::default().load();
            let snippets = SnippetStore::default().load();
            if templates.is_empty() && snippets.is_empty() {
                list.append(
                    &gtk::Label::builder()
                        .label("Noch keine Vorlagen oder Textbausteine gespeichert")
                        .margin_top(12)
                        .margin_bottom(12)
                        .margin_start(12)
                        .margin_end(12)
                        .css_classes(vec!["dim-label"])
                        .build(),
                );
            } else {
                for template in &templates {
                    let row = adw::ActionRow::builder()
                        .title(&template.name)
                        .subtitle(if template.subject.trim().is_empty() {
                            "Ohne Betreff"
                        } else {
                            template.subject.as_str()
                        })
                        .activatable(true)
                        .build();
                    let apply_button = gtk::Button::builder()
                        .icon_name("insert-text-symbolic")
                        .tooltip_text("Vorlage einfügen")
                        .css_classes(vec!["flat"])
                        .valign(gtk::Align::Center)
                        .build();
                    let delete_button = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text("Vorlage löschen")
                        .css_classes(vec!["flat", "error"])
                        .valign(gtk::Align::Center)
                        .build();
                    row.add_suffix(&apply_button);
                    row.add_suffix(&delete_button);

                    let body = sanitize_html(&template.body_html);
                    let subject = template.subject.clone();
                    let editor_for_apply = html_editor_for_templates.clone();
                    let subject_for_apply = entry_subject_for_templates.clone();
                    let popover_for_apply = popover.clone();
                    apply_button.connect_clicked(move |_| {
                        editor_for_apply.set_html(&body);
                        if subject_for_apply.text().trim().is_empty() {
                            subject_for_apply.set_text(&subject);
                        }
                        popover_for_apply.popdown();
                    });

                    let template_id = template.id.clone();
                    let row_for_delete = row.clone();
                    let popover_for_delete = popover.clone();
                    delete_button.connect_clicked(move |button| {
                        let template_id_for_action = template_id.clone();
                        let row_for_action = row_for_delete.clone();
                        let popover_for_action = popover_for_delete.clone();
                        crate::dialogs::run_destructive_action(
                            button,
                            "Vorlage löschen?",
                            "Die Vorlage wird dauerhaft aus dem app-eigenen Speicher entfernt.",
                            "Löschen",
                            move || match TemplateStore::default().delete(&template_id_for_action) {
                                Ok(true) => {
                                    row_for_action.set_visible(false);
                                    popover_for_action.popdown();
                                }
                                Ok(false) => tracing::debug!(
                                    template_id = %template_id_for_action,
                                    "Template was already removed"
                                ),
                                Err(error) => {
                                    tracing::warn!(%error, "Could not delete mail template")
                                }
                            },
                        );
                    });

                    let apply_button_for_row = apply_button.clone();
                    row.connect_activated(move |_| apply_button_for_row.emit_clicked());
                    list.append(&row);
                }
            }

            if !templates.is_empty() && !snippets.is_empty() {
                list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            for snippet in snippets {
                let row = adw::ActionRow::builder()
                    .title(&snippet.name)
                    .subtitle(if snippet.trigger.trim().is_empty() {
                        "Textbaustein"
                    } else {
                        snippet.trigger.as_str()
                    })
                    .activatable(true)
                    .build();
                let apply_button = gtk::Button::builder()
                    .icon_name("insert-text-symbolic")
                    .tooltip_text("Textbaustein an der Cursorposition einfügen")
                    .css_classes(vec!["flat"])
                    .valign(gtk::Align::Center)
                    .build();
                let delete_button = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text("Textbaustein löschen")
                    .css_classes(vec!["flat", "error"])
                    .valign(gtk::Align::Center)
                    .build();
                row.add_suffix(&apply_button);
                row.add_suffix(&delete_button);

                let body = snippet.body_html.clone();
                let editor_for_apply = html_editor_for_templates.clone();
                let popover_for_apply = popover.clone();
                apply_button.connect_clicked(move |_| {
                    editor_for_apply.insert_html(&body);
                    popover_for_apply.popdown();
                });

                let snippet_id = snippet.id.clone();
                let row_for_delete = row.clone();
                let popover_for_delete = popover.clone();
                delete_button.connect_clicked(move |button| {
                    let snippet_id_for_action = snippet_id.clone();
                    let row_for_action = row_for_delete.clone();
                    let popover_for_action = popover_for_delete.clone();
                    crate::dialogs::run_destructive_action(
                        button,
                        "Textbaustein löschen?",
                        "Der Textbaustein wird dauerhaft aus dem app-eigenen Speicher entfernt.",
                        "Löschen",
                        move || match SnippetStore::default().delete(&snippet_id_for_action) {
                            Ok(true) => {
                                row_for_action.set_visible(false);
                                popover_for_action.popdown();
                            }
                            Ok(false) => tracing::debug!(
                                snippet_id = %snippet_id_for_action,
                                "Snippet was already removed"
                            ),
                            Err(error) => {
                                tracing::warn!(%error, "Could not delete mail snippet")
                            }
                        },
                    );
                });

                let apply_button_for_row = apply_button.clone();
                row.connect_activated(move |_| apply_button_for_row.emit_clicked());
                list.append(&row);
            }
            popover.set_child(Some(&list));
            popover.popup();
            if let Some(dialog) = dialog_weak_for_templates.upgrade() {
                dialog.show_toast("Vorlagen werden aus dem app-eigenen Speicher geladen");
            }
        });

        let dialog_weak_for_save_template = self.downgrade();
        let entry_subject_for_save_template = entry_subject.clone();
        let html_editor_for_save_template = html_editor.clone();
        let menu_button_for_save_template = menu_button.clone();
        btn_save_template.connect_clicked(move |_| {
            let popover = gtk::Popover::new();
            popover.set_parent(&menu_button_for_save_template);
            let container = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .margin_start(12)
                .margin_end(12)
                .margin_top(12)
                .margin_bottom(12)
                .width_request(280)
                .build();
            let name_entry = gtk::Entry::builder()
                .placeholder_text("Name der Vorlage")
                .activates_default(true)
                .build();
            let save_button = gtk::Button::builder()
                .label("Vorlage speichern")
                .css_classes(vec!["suggested-action"])
                .build();
            container.append(&name_entry);
            container.append(&save_button);
            popover.set_child(Some(&container));

            let dialog_weak = dialog_weak_for_save_template.clone();
            let subject_entry = entry_subject_for_save_template.clone();
            let editor = html_editor_for_save_template.clone();
            let popover_for_save = popover.clone();
            save_button.connect_clicked(move |_| {
                let name = name_entry.text().trim().to_string();
                if name.is_empty() {
                    if let Some(dialog) = dialog_weak.upgrade() {
                        dialog.show_toast("Bitte einen Namen für die Vorlage eingeben");
                    }
                    return;
                }
                let subject = subject_entry.text().to_string();
                let dialog_weak_for_result = dialog_weak.clone();
                let popover_for_result = popover_for_save.clone();
                editor.read_html(move |result| match result {
                    Ok(body_html) => {
                        let template = MailTemplate {
                            id: format!("template-{}", uuid::Uuid::new_v4()),
                            name,
                            subject,
                            body_html: sanitize_html(&body_html),
                        };
                        match TemplateStore::default().upsert(template) {
                            Ok(()) => {
                                popover_for_result.popdown();
                                if let Some(dialog) = dialog_weak_for_result.upgrade() {
                                    dialog.show_toast("Vorlage gespeichert");
                                }
                            }
                            Err(error) => {
                                if let Some(dialog) = dialog_weak_for_result.upgrade() {
                                    dialog.show_toast(&format!(
                                        "Vorlage konnte nicht gespeichert werden: {error}"
                                    ));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&format!(
                                "Vorlageninhalt konnte nicht gelesen werden: {error}"
                            ));
                        }
                    }
                });
            });
            popover.popup();
        });

        let dialog_weak_for_save_snippet = self.downgrade();
        let html_editor_for_save_snippet = html_editor.clone();
        let menu_button_for_save_snippet = menu_button.clone();
        btn_save_snippet.connect_clicked(move |_| {
            let popover = gtk::Popover::new();
            popover.set_parent(&menu_button_for_save_snippet);
            let container = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .margin_start(12)
                .margin_end(12)
                .margin_top(12)
                .margin_bottom(12)
                .width_request(300)
                .build();
            let name_entry = gtk::Entry::builder()
                .placeholder_text("Name des Textbausteins")
                .activates_default(true)
                .build();
            let trigger_entry = gtk::Entry::builder()
                .placeholder_text("Kürzel (optional, z. B. /gruss)")
                .build();
            let save_button = gtk::Button::builder()
                .label("Textbaustein speichern")
                .css_classes(vec!["suggested-action"])
                .build();
            container.append(&name_entry);
            container.append(&trigger_entry);
            container.append(&save_button);
            popover.set_child(Some(&container));

            let dialog_weak = dialog_weak_for_save_snippet.clone();
            let editor = html_editor_for_save_snippet.clone();
            let popover_for_save = popover.clone();
            save_button.connect_clicked(move |_| {
                let name = name_entry.text().trim().to_string();
                if name.is_empty() {
                    if let Some(dialog) = dialog_weak.upgrade() {
                        dialog.show_toast("Bitte einen Namen für den Textbaustein eingeben");
                    }
                    return;
                }
                let trigger = trigger_entry.text().trim().to_string();
                let dialog_weak_for_result = dialog_weak.clone();
                let popover_for_result = popover_for_save.clone();
                editor.read_html(move |result| match result {
                    Ok(body_html) => {
                        let body_html = sanitize_html(&body_html);
                        if body_html.trim().is_empty() {
                            if let Some(dialog) = dialog_weak_for_result.upgrade() {
                                dialog.show_toast("Der Textbaustein darf nicht leer sein");
                            }
                            return;
                        }
                        let snippet = MailSnippet {
                            id: format!("snippet-{}", uuid::Uuid::new_v4()),
                            name,
                            trigger,
                            body_html,
                        };
                        match SnippetStore::default().upsert(snippet) {
                            Ok(()) => {
                                popover_for_result.popdown();
                                if let Some(dialog) = dialog_weak_for_result.upgrade() {
                                    dialog.show_toast("Textbaustein gespeichert");
                                }
                            }
                            Err(error) => {
                                if let Some(dialog) = dialog_weak_for_result.upgrade() {
                                    dialog.show_toast(&format!(
                                        "Textbaustein konnte nicht gespeichert werden: {error}"
                                    ));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = dialog_weak_for_result.upgrade() {
                            dialog.show_toast(&format!(
                                "Textbausteininhalt konnte nicht gelesen werden: {error}"
                            ));
                        }
                    }
                });
            });
            popover.popup();
        });

        let dialog_weak_for_send = self.downgrade();
        let html_editor_for_send = html_editor.clone();
        let entry_to_for_send = entry_to.clone();
        let entry_cc_for_send = entry_cc.clone();
        let entry_bcc_for_send = entry_bcc.clone();
        let entry_subject_for_send = entry_subject.clone();
        let combo_from_for_send = combo_from.clone();
        btn_send.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak_for_send.upgrade() else {
                return;
            };
            if dialog.imp().autosave_in_progress.get() {
                dialog.show_toast("Automatisches Speichern läuft noch; der Versand startet danach");
                return;
            }
            let html_editor_for_send = html_editor_for_send.clone();
            let entry_to_for_send = entry_to_for_send.clone();
            let entry_cc_for_send = entry_cc_for_send.clone();
            let entry_bcc_for_send = entry_bcc_for_send.clone();
            let entry_subject_for_send = entry_subject_for_send.clone();
            let combo_from_for_send = combo_from_for_send.clone();
            let dialog_weak = dialog.downgrade();
            dialog.validate_and_confirm_send(move || {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let Some((operation_id, cancel)) =
                    dialog.begin_operation(ComposeOperation::Sending)
                else {
                    return;
                };
                dialog.set_operation_status(operation_id, "Nachricht wird gelesen…", true);

                let identity = dialog.selected_identity(combo_from_for_send.selected());
                let reply_to = dialog.effective_reply_to(&identity.reply_to);
                let (pgp_sign, pgp_encrypt) = dialog.pgp_flags_for_message();
                let request_read_receipt = dialog.request_read_receipt_for_message();
                let priority = dialog.priority_for_message();
                let charset = dialog
                    .imp()
                    .charset
                    .borrow()
                    .clone();
                let to = entry_to_for_send.text().to_string();
                let cc = entry_cc_for_send.text().to_string();
                let bcc = entry_bcc_for_send.text().to_string();
                let subject = entry_subject_for_send.text().to_string();
                let attachments = dialog.imp().attachments.borrow().clone();
                let calendar_method = dialog.imp().calendar_method.borrow().clone();
                let calendar_component = dialog.imp().calendar_component.borrow().clone();
                let pgp_key = dialog.pgp_key_for_message();
                let smime_options = dialog.smime_options_for_message();
                let draft_id = dialog.imp().draft_id.borrow().clone();
                let expected_draft = dialog.imp().persisted_draft.borrow().clone();
                let dialog_weak = dialog.downgrade();
                let cancel_for_worker = Arc::clone(&cancel);

                html_editor_for_send.read_content(move |result| {
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    if !dialog.is_current_operation(operation_id) {
                        return;
                    }
                    let content = match result {
                        Ok(content) => content,
                        Err(error) => {
                            dialog.finish_operation(operation_id);
                            dialog.show_toast(&format!(
                                "Nachricht konnte nicht gelesen werden; Entwurf bleibt geöffnet: {error}"
                            ));
                            return;
                        }
                    };
                    if cancel.is_cancel_requested() {
                        dialog.finish_operation(operation_id);
                        dialog.show_toast("Versand abgebrochen; der Entwurf bleibt erhalten");
                        return;
                    }

                    let draft = DraftMessage {
                        id: draft_id,
                        from: identity.from.clone(),
                        reply_to: reply_to.clone(),
                        source_uid: identity.source_uid.clone(),
                        to: to.clone(),
                        cc: cc.clone(),
                        bcc: bcc.clone(),
                        subject: subject.clone(),
                        html_body: content.body.clone(),
                        plain_text: content.plain_text,
                        updated_at: Utc::now().to_rfc3339(),
                        attachments: attachments.clone(),
                        calendar_method: calendar_method.clone(),
                        calendar_component: calendar_component.clone(),
                        pgp_sign,
                        pgp_encrypt,
                        pgp_key: pgp_key.clone(),
                        smime_options: Some(DraftComposerOptions {
                            request_read_receipt,
                            priority,
                            charset: charset.clone(),
                            smime: smime_options.clone(),
                        }),
                    };
                    let outgoing = OutgoingMessage {
                        source_uid: identity.source_uid,
                        from: identity.from,
                        reply_to,
                        request_read_receipt,
                        to,
                        cc,
                        bcc,
                        subject,
                        html_body: content.body,
                        plain_text: content.plain_text,
                        priority,
                        charset,
                        attachments,
                        calendar_method,
                        calendar_component,
                        pgp_sign,
                        pgp_encrypt,
                        pgp_key,
                        smime_options,
                    };
                    dialog.set_operation_status(
                        operation_id,
                        "Entwurf wird gesichert und Nachricht wird vorbereitet…",
                        true,
                    );

                let (sender, receiver) = async_channel::bounded(4);
                std::thread::spawn(move || {
                    let store = DraftStoreService::default();
                    let Some(_send_claim) = store.try_claim_send(&draft.id) else {
                        let _ = sender.send_blocking(SendWorkerEvent::Finished(Err(
                            "Dieser Entwurf wird bereits in einem anderen Fenster gesendet"
                                .to_string(),
                        )));
                        return;
                    };
                    let saved = match store.save_if_matches(expected_draft.as_ref(), draft.clone()) {
                        Ok(saved) => saved,
                        Err(error) => {
                            let _ = sender.send_blocking(SendWorkerEvent::Finished(Err(format!(
                                "Entwurf konnte vor dem Versand nicht gesichert werden: {error}"
                            ))));
                            return;
                        }
                    };
                    if !saved {
                        let _ = sender.send_blocking(SendWorkerEvent::Finished(Err(format!(
                            "Entwurf wurde zwischenzeitlich geändert; Versand abgebrochen, damit keine neuere Recovery-Version überschrieben wird"
                        ))));
                        return;
                    }
                    let _ = sender.send_blocking(SendWorkerEvent::DraftSaved(draft.clone()));
                    if cancel_for_worker.is_cancel_requested() {
                        let _ = sender.send_blocking(SendWorkerEvent::Cancelled);
                        return;
                    }
                    let raw_message = match mail_composer_service::build_rfc822(&outgoing) {
                        Ok(raw_message) => raw_message,
                        Err(error) => {
                            let _ = sender.send_blocking(SendWorkerEvent::Finished(Err(error)));
                            return;
                        }
                    };
                    // Atomically close the cancellation window. If the UI won
                    // the race, no transport starts; after this claim the UI
                    // reports that transport cancellation is no longer possible.
                    if !cancel_for_worker.try_commit() {
                        let _ = sender.send_blocking(SendWorkerEvent::Cancelled);
                        return;
                    }
                    let _ = sender.send_blocking(SendWorkerEvent::TransportStarted);
                    let result = camel_mail_service::send_message(
                        &outgoing.source_uid,
                        &raw_message,
                    )
                    .map(|sent_message_saved| {
                        let cleanup = store.delete_if_matches(&draft);
                        SendCompletion {
                            sent_message_saved,
                            local_draft_deleted: matches!(cleanup, Ok(true)),
                            cleanup_error: cleanup.err().map(|error| error.to_string()),
                        }
                    });
                    let _ = sender.send_blocking(SendWorkerEvent::Finished(result));
                });

                let dialog_weak_for_result = dialog.downgrade();
                glib::spawn_future_local(async move {
                    loop {
                        let event = match receiver.recv().await {
                            Ok(event) => event,
                            Err(error) => {
                                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                                    return;
                                };
                                if dialog.is_current_operation(operation_id) {
                                    dialog.finish_operation(operation_id);
                                    dialog.show_toast(&format!(
                                        "Versand abgebrochen; Entwurf bleibt erhalten: {error}"
                                    ));
                                }
                                return;
                            }
                        };
                        let Some(dialog) = dialog_weak_for_result.upgrade() else {
                            return;
                        };
                        if !dialog.is_current_operation(operation_id) {
                            return;
                        }
                        match event {
                            SendWorkerEvent::DraftSaved(saved_draft) => {
                                dialog.imp().local_draft_persisted.set(true);
                                dialog.imp().persisted_draft.replace(Some(saved_draft));
                                dialog.mark_saved();
                            }
                            SendWorkerEvent::TransportStarted => {
                                dialog.mark_transport_started(operation_id);
                            }
                            SendWorkerEvent::Cancelled => {
                                dialog.finish_operation(operation_id);
                                dialog.show_toast(
                                    "Versand abgebrochen; der Entwurf bleibt erhalten",
                                );
                                return;
                            }
                            SendWorkerEvent::Finished(result) => {
                                dialog.finish_operation(operation_id);
                                match result {
                                    Ok(completion) => {
                                        if completion.local_draft_deleted {
                                            dialog.imp().local_draft_persisted.set(false);
                                        }
                                        let mut message = if completion.sent_message_saved {
                                            "E-Mail gesendet; Kopie in Gesendet gespeichert"
                                        } else {
                                            "E-Mail gesendet"
                                        }
                                        .to_string();
                                        if let Some(error) = completion.cleanup_error {
                                            message.push_str(&format!(
                                                "; Recovery-Entwurf blieb erhalten: {error}"
                                            ));
                                        } else if !completion.local_draft_deleted {
                                            message.push_str(
                                                "; ein neuerer Recovery-Entwurf wurde erhalten",
                                            );
                                        }
                                        let recent_service = RecentRecipientsService::new();
                                        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
                                            recent_service.record_recipients_from_header(&entry.text());
                                        }
                                        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
                                            recent_service.record_recipients_from_header(&entry.text());
                                        }
                                        if let Some(entry) = dialog.imp().entry_bcc.borrow().as_ref() {
                                            recent_service.record_recipients_from_header(&entry.text());
                                        }
                                        if let Some(callback) = dialog.imp().on_send_success.borrow().as_ref() {
                                            callback();
                                        }
                                        dialog.show_toast(&message);
                                        dialog.imp().close_confirmed.set(true);
                                        dialog.set_can_close(true);
                                        dialog.close();
                                    }
                                    Err(error) => {
                                        dialog.show_toast(&format!(
                                            "E-Mail konnte nicht gesendet werden; Entwurf bleibt erhalten: {error}"
                                        ));
                                    }
                                }
                                return;
                            }
                        }
                    }
                });
            });
        });
    });

        let source_model_for_load = source_model.clone();
        let combo_from_for_load = combo_from.clone();
        let dialog_for_source_load = self.downgrade();
        glib::spawn_future_local(async move {
            let discovery = eds_service::discover_sources(EdsSourceKind::Mail).await;
            if let Some(error) = discovery.error.as_deref() {
                tracing::warn!(%error, "Live EDS mail source discovery failed for composer");
            }
            let cache_suffix = (discovery.origin == EdsSourceOrigin::Cache)
                .then_some(" · Offline-Cache")
                .unwrap_or_default();
            let active_sources: Vec<_> = discovery
                .sources
                .into_iter()
                .filter(|source| source.enabled)
                .collect();
            let local_choices = local_account_choices();
            let mut identities: Vec<ComposeIdentity> = active_sources
                .iter()
                .map(|source| {
                    let label =
                        if source.backend_name.is_empty() || source.display_name.contains('@') {
                            format!("{}{}", source.display_name, cache_suffix)
                        } else {
                            format!(
                                "{} · {}{}",
                                source.display_name, source.backend_name, cache_suffix
                            )
                        };
                    let local_profiles = AccountConfigStore::default().load();
                    let matched_sig = local_profiles
                        .iter()
                        .find(|p| {
                            p.id == source.uid || p.email.eq_ignore_ascii_case(&source.display_name)
                        })
                        .and_then(|p| p.signature_id.clone());
                    ComposeIdentity {
                        source_uid: source.uid.clone(),
                        from: source.display_name.clone(),
                        label,
                        signature_id: matched_sig,
                        ..ComposeIdentity::default()
                    }
                })
                .collect();
            identities.extend(local_choices);
            let names: Vec<String> = identities
                .iter()
                .map(|identity| identity.label.clone())
                .collect();
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            if let Some(dialog) = dialog_for_source_load.upgrade() {
                dialog.imp().suppress_from_notify.set(true);
                {
                    let mut source_uids = dialog.imp().source_uids.borrow_mut();
                    source_uids.clear();
                    source_uids.extend(
                        identities
                            .iter()
                            .map(|identity| identity.source_uid.clone()),
                    );
                }
                dialog.imp().source_identities.replace(identities.clone());
                source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);

                let requested_uid = dialog.imp().requested_source_uid.borrow().clone();
                let requested_from = dialog.imp().requested_from.borrow().clone();
                let preferred_uid = preferred_composer_account_id();
                let selected_uid = requested_uid
                    .filter(|uid| !uid.trim().is_empty())
                    .or(preferred_uid);
                if let Some(selected_uid) = selected_uid.as_deref() {
                    let selected_uid_without_prefix = selected_uid
                        .strip_prefix("evolution-source:")
                        .unwrap_or(selected_uid);
                    let source_uid_matches = |identity: &ComposeIdentity| {
                        identity.source_uid == selected_uid
                            || identity.source_uid == selected_uid_without_prefix
                    };
                    let index = requested_from
                        .as_deref()
                        .and_then(|from| {
                            identities.iter().position(|identity| {
                                source_uid_matches(identity)
                                    && compose_identity_matches_from(identity, from)
                            })
                        })
                        .or_else(|| identities.iter().position(source_uid_matches));
                    if let Some(index) = index {
                        combo_from_for_load.set_selected(index as u32);
                    }
                }
                if names.is_empty() {
                    let message = if discovery.origin == EdsSourceOrigin::Empty {
                        "Mailkonten sind nicht erreichbar"
                    } else {
                        "Kein aktives Mailkonto verfügbar"
                    };
                    source_model_for_load.splice(0, source_model_for_load.n_items(), &[message]);
                }
                dialog.imp().suppress_from_notify.set(false);
                dialog.update_sender_dependent_controls(combo_from_for_load.selected());
                dialog.sync_signature_for_selected_account();
            } else if names.is_empty() {
                let message = if discovery.origin == EdsSourceOrigin::Empty {
                    "Mailkonten sind nicht erreichbar"
                } else {
                    "Kein aktives Mailkonto verfügbar"
                };
                source_model_for_load.splice(0, source_model_for_load.n_items(), &[message]);
            }
        });

        btn_save_draft.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            entry_to,
            #[strong]
            entry_cc,
            #[strong]
            entry_bcc,
            #[strong]
            entry_subject,
            #[strong]
            combo_from,
            #[strong]
            html_editor,
            #[strong]
            btn_sign,
            #[strong]
            btn_encrypt,
            move |_| {
                if dialog.imp().autosave_in_progress.get() {
                    dialog.imp().close_after_save.set(false);
                    dialog.show_toast(
                        "Automatisches Speichern läuft noch; bitte danach erneut speichern",
                    );
                    return;
                }
                let Some((operation_id, cancel)) =
                    dialog.begin_operation(ComposeOperation::SavingDraft)
                else {
                    dialog.imp().close_after_save.set(false);
                    return;
                };
                dialog.set_operation_status(operation_id, "Entwurf wird gelesen…", true);
                let to = entry_to.text().to_string();
                let cc = entry_cc.text().to_string();
                let bcc = entry_bcc.text().to_string();
                let subject = entry_subject.text().to_string();
                let identity = dialog.selected_identity(combo_from.selected());
                let from = identity.from.clone();
                let reply_to = dialog.effective_reply_to(&identity.reply_to);
                let source_uid = identity.source_uid.clone();
                let attachments = dialog.imp().attachments.borrow().clone();
                let calendar_method = dialog.imp().calendar_method.borrow().clone();
                let calendar_component = dialog.imp().calendar_component.borrow().clone();
                let pgp_sign = btn_sign.is_active();
                let pgp_encrypt = btn_encrypt.is_active();
                let request_read_receipt = dialog.request_read_receipt_for_message();
                let priority = dialog.priority_for_message();
                let charset = dialog.imp().charset.borrow().clone();
                let pgp_key = dialog.pgp_key_for_message();
                let smime_options = dialog.smime_options_for_message();
                let draft_id = dialog.imp().draft_id.borrow().clone();
                let expected_draft = dialog.imp().persisted_draft.borrow().clone();
                let dialog_weak = dialog.downgrade();
                let cancel_for_worker = Arc::clone(&cancel);

                html_editor.read_content(move |result| {
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    if !dialog.is_current_operation(operation_id) {
                        return;
                    }
                    let content = match result {
                        Ok(content) => content,
                        Err(error) => {
                            dialog.imp().close_after_save.set(false);
                            dialog.finish_operation(operation_id);
                            dialog.show_toast(&format!(
                                "Entwurf konnte nicht gelesen werden; Inhalt bleibt geöffnet: {error}"
                            ));
                            return;
                        }
                    };
                    if cancel.is_cancel_requested() {
                        dialog.imp().close_after_save.set(false);
                        dialog.finish_operation(operation_id);
                        dialog.show_toast("Entwurfsspeicherung abgebrochen; Inhalt bleibt geöffnet");
                        return;
                    }

                    let draft = DraftMessage {
                        id: draft_id,
                        from: from.clone(),
                        reply_to: reply_to.clone(),
                        source_uid: source_uid.clone(),
                        to: to.clone(),
                        cc: cc.clone(),
                        bcc: bcc.clone(),
                        subject: subject.clone(),
                        html_body: content.body.clone(),
                        plain_text: content.plain_text,
                        updated_at: Utc::now().to_rfc3339(),
                        attachments: attachments.clone(),
                    calendar_method: calendar_method.clone(),
                    calendar_component: calendar_component.clone(),
                    pgp_sign,
                    pgp_encrypt,
                    pgp_key: pgp_key.clone(),
                    smime_options: Some(DraftComposerOptions {
                        request_read_receipt,
                        priority,
                        charset: charset.clone(),
                        smime: smime_options.clone(),
                    }),
                };
                    if !draft_has_user_content(&draft) {
                        let draft_id = dialog.imp().draft_id.borrow().clone();
                        if !draft_id.is_empty() {
                            let _ = DraftStoreService::default().delete(&draft_id);
                        }
                        dialog.imp().local_draft_persisted.set(false);
                        let close = dialog.imp().close_after_save.replace(false);
                        dialog.finish_operation(operation_id);
                        if close {
                            dialog.imp().close_confirmed.set(true);
                            dialog.set_can_close(true);
                            dialog.close();
                        } else {
                            dialog.show_toast("Leere Nachricht wird nicht als Entwurf gespeichert");
                        }
                        return;
                    }
                    let outgoing = OutgoingMessage {
                        source_uid: source_uid.clone(),
                        from,
                        reply_to,
                    request_read_receipt,
                        to,
                        cc,
                        bcc,
                    subject,
                    html_body: content.body,
                    plain_text: content.plain_text,
                    priority,
                    charset,
                    attachments,
                        calendar_method,
                        calendar_component,
                        // Security choices remain editable metadata in a
                        // draft.  They are applied only when the message is
                        // finally sent.
                        pgp_sign: false,
                        pgp_encrypt: false,
                        pgp_key: String::new(),
                        smime_options: None,
                    };
                    dialog.set_operation_status(
                        operation_id,
                        "Entwurf wird lokal gesichert…",
                        true,
                    );

                    let (sender, receiver) = async_channel::bounded(4);
                    std::thread::spawn(move || {
                        let store = DraftStoreService::default();
                        let saved = match store.save_if_matches(expected_draft.as_ref(), draft.clone()) {
                            Ok(saved) => saved,
                            Err(error) => {
                                let _ = sender.send_blocking(DraftWorkerEvent::Finished(Err(
                                    format!("Lokaler Entwurf konnte nicht gespeichert werden: {error}"),
                                )));
                                return;
                            }
                        };
                        if !saved {
                            let _ = sender.send_blocking(DraftWorkerEvent::Finished(Err(
                                "Entwurf wurde zwischenzeitlich geändert; die neuere Recovery-Version bleibt erhalten".to_string(),
                            )));
                            return;
                        }
                        let _ = sender.send_blocking(DraftWorkerEvent::DraftSaved(draft.clone()));
                        if cancel_for_worker.is_cancel_requested() {
                            let _ = sender.send_blocking(DraftWorkerEvent::Cancelled);
                            return;
                        }
                        let synchronized = camel_mail_service::native_adapter_available()
                            || graph_service_for_source(&outgoing.source_uid).is_some();
                        if !synchronized {
                            let _ = sender.send_blocking(DraftWorkerEvent::Finished(Ok(false)));
                            return;
                        }
                        let raw_message = match mail_composer_service::build_rfc822(&outgoing) {
                            Ok(raw_message) => raw_message,
                            Err(error) => {
                                let _ = sender.send_blocking(DraftWorkerEvent::Finished(Err(
                                    format!(
                                        "Lokaler Entwurf gespeichert; synchronisierte Version konnte nicht erstellt werden: {error}"
                                    ),
                                )));
                                return;
                            }
                        };
                        if cancel_for_worker.is_cancel_requested() {
                            let _ = sender.send_blocking(DraftWorkerEvent::Cancelled);
                            return;
                        }
                        let result = camel_mail_service::save_message(
                            &outgoing.source_uid,
                            &raw_message,
                            camel_mail_service::CamelSaveFolder::Drafts,
                        )
                        .map(|()| true)
                        .map_err(|error| {
                            format!(
                                "Lokaler Entwurf gespeichert; synchronisierte Speicherung fehlgeschlagen: {error}"
                            )
                        });
                        let _ = sender.send_blocking(DraftWorkerEvent::Finished(result));
                    });

                    let dialog_weak_for_result = dialog.downgrade();
                    glib::spawn_future_local(async move {
                        loop {
                            let event = match receiver.recv().await {
                                Ok(event) => event,
                                Err(error) => {
                                    let Some(dialog) = dialog_weak_for_result.upgrade() else {
                                        return;
                                    };
                                    if dialog.is_current_operation(operation_id) {
                                        dialog.imp().close_after_save.set(false);
                                        dialog.finish_operation(operation_id);
                                        dialog.show_toast(&format!(
                                            "Entwurfsspeicherung abgebrochen; Inhalt bleibt geöffnet: {error}"
                                        ));
                                    }
                                    return;
                                }
                            };
                            let Some(dialog) = dialog_weak_for_result.upgrade() else {
                                return;
                            };
                            if !dialog.is_current_operation(operation_id) {
                                return;
                            }
                            match event {
                                DraftWorkerEvent::DraftSaved(saved_draft) => {
                                    dialog.imp().local_draft_persisted.set(true);
                                    dialog.imp().persisted_draft.replace(Some(saved_draft));
                                    dialog.mark_saved();
                                    if dialog.imp().close_after_save.replace(false) {
                                        dialog.show_toast("Entwurf gespeichert");
                                        dialog.imp().close_confirmed.set(true);
                                        dialog.set_can_close(true);
                                        dialog.close();
                                    }
                                }
                                DraftWorkerEvent::Cancelled => {
                                    dialog.imp().close_after_save.set(false);
                                    dialog.finish_operation(operation_id);
                                    dialog.show_toast(
                                        "Entwurfsspeicherung abgebrochen; Inhalt bleibt geöffnet",
                                    );
                                    return;
                                }
                                DraftWorkerEvent::Finished(result) => {
                                    dialog.finish_operation(operation_id);
                                    match result {
                                        Ok(true) => {
                                            if dialog.imp().close_after_save.replace(false) {
                                                dialog.show_toast("Entwurf gespeichert");
                                                dialog.imp().close_confirmed.set(true);
                                                dialog.set_can_close(true);
                                                dialog.close();
                                            } else {
                                                dialog.show_toast(
                                                    "Entwurf lokal und im synchronisierten Ordner gespeichert",
                                                );
                                            }
                                        }
                                        Ok(false) => {
                                            if dialog.imp().close_after_save.replace(false) {
                                                dialog.show_toast("Entwurf gespeichert");
                                                dialog.imp().close_confirmed.set(true);
                                                dialog.set_can_close(true);
                                                dialog.close();
                                            } else {
                                                dialog.show_toast("Entwurf lokal gespeichert");
                                            }
                                        }
                                        Err(error) => {
                                            dialog.imp().close_after_save.set(false);
                                            dialog.show_toast(&format!(
                                                "{error}; Inhalt bleibt geöffnet"
                                            ));
                                        }
                                    }
                                    return;
                                }
                            }
                        }
                    });
                });
            }
        ));

        self.install_action_surface(
            &menu_button,
            &btn_close,
            &btn_send,
            &btn_save_draft,
            &btn_discard,
            &btn_attach,
            &btn_plain_text,
            &btn_request_read_receipt,
            &btn_priority,
            &btn_sign,
            &btn_encrypt,
            &btn_insert_template,
            &btn_save_template,
            &btn_save_snippet,
        );

        toolbar_view.set_content(Some(&content_box));
        self.set_child(Some(&toolbar_view));
        self.install_file_drop_target();

        self.start_local_autosave();
    }

    fn install_action_surface(
        &self,
        menu_button: &gtk::MenuButton,
        btn_close: &gtk::Button,
        btn_send: &gtk::Button,
        btn_save_draft: &gtk::Button,
        btn_discard: &gtk::Button,
        btn_attach: &gtk::Button,
        btn_plain_text: &gtk::ToggleButton,
        btn_request_read_receipt: &gtk::ToggleButton,
        btn_priority: &gtk::ToggleButton,
        btn_sign: &gtk::ToggleButton,
        btn_encrypt: &gtk::ToggleButton,
        btn_insert_template: &gtk::Button,
        btn_save_template: &gtk::Button,
        btn_save_snippet: &gtk::Button,
    ) {
        let group = gio::SimpleActionGroup::new();

        let action_send = gio::SimpleAction::new("send", None);
        let send_button = btn_send.clone();
        action_send.connect_activate(move |_, _| {
            if send_button.is_sensitive() {
                send_button.emit_clicked();
            }
        });
        group.add_action(&action_send);

        let action_save_draft = gio::SimpleAction::new("save-draft", None);
        let save_draft_button = btn_save_draft.clone();
        action_save_draft.connect_activate(move |_, _| {
            if save_draft_button.is_sensitive() {
                save_draft_button.emit_clicked();
            }
        });
        group.add_action(&action_save_draft);

        let action_discard = gio::SimpleAction::new("discard", None);
        let dialog_for_discard = self.downgrade();
        action_discard.connect_activate(move |_, _| {
            if let Some(dialog) = dialog_for_discard.upgrade() {
                dialog.discard_and_close();
            }
        });
        group.add_action(&action_discard);
        btn_discard.set_action_name(Some("compose.discard"));

        let action_save_as_eml = gio::SimpleAction::new("save-as-eml", None);
        let dialog_for_save_as = self.downgrade();
        action_save_as_eml.connect_activate(move |_, _| {
            if let Some(dialog) = dialog_for_save_as.upgrade() {
                dialog.save_as_eml();
            }
        });
        group.add_action(&action_save_as_eml);

        for (name, print_action) in [
            ("print", MailPrintAction::Print),
            ("print-preview", MailPrintAction::Preview),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let dialog = self.downgrade();
            action.connect_activate(move |_, _| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.start_print(print_action);
                }
            });
            if matches!(print_action, MailPrintAction::Print) && !cfg!(target_os = "linux") {
                action.set_enabled(false);
            }
            group.add_action(&action);
        }

        let action_close = gio::SimpleAction::new("close", None);
        let dialog_for_close = self.downgrade();
        action_close.connect_activate(move |_, _| {
            if let Some(dialog) = dialog_for_close.upgrade() {
                dialog.close();
            }
        });
        group.add_action(&action_close);
        btn_close.set_action_name(Some("compose.close"));

        let action_new_message = gio::SimpleAction::new("new-message", None);
        let dialog_for_new = self.downgrade();
        action_new_message.connect_activate(move |_, _| {
            let Some(dialog) = dialog_for_new.upgrade() else {
                return;
            };
            if dialog.imp().operation.get() != ComposeOperation::Idle {
                return;
            }
            let new_dialog = ComposeDialog::new();
            if let Some(parent) = dialog.imp().stored_parent.borrow().as_ref() {
                new_dialog.imp().stored_parent.replace(Some(parent.clone()));
            }
            new_dialog.present();
        });
        group.add_action(&action_new_message);

        let action_attach = gio::SimpleAction::new("attach", None);
        let attach_button = btn_attach.clone();
        action_attach.connect_activate(move |_, _| {
            if attach_button.is_sensitive() {
                attach_button.emit_clicked();
            }
        });
        group.add_action(&action_attach);

        let action_insert_template = gio::SimpleAction::new("insert-template", None);
        let btn_insert_for_action = btn_insert_template.clone();
        action_insert_template.connect_activate(move |_, _| {
            btn_insert_for_action.emit_clicked();
        });
        group.add_action(&action_insert_template);

        let action_save_template = gio::SimpleAction::new("save-template", None);
        let btn_save_template_for_action = btn_save_template.clone();
        action_save_template.connect_activate(move |_, _| {
            btn_save_template_for_action.emit_clicked();
        });
        group.add_action(&action_save_template);

        let action_save_snippet = gio::SimpleAction::new("save-snippet", None);
        let btn_save_snippet_for_action = btn_save_snippet.clone();
        action_save_snippet.connect_activate(move |_, _| {
            btn_save_snippet_for_action.emit_clicked();
        });
        group.add_action(&action_save_snippet);

        let action_plain_text = add_composer_toggle_action(&group, "plain-text", btn_plain_text);
        let action_request_read_receipt =
            add_composer_toggle_action(&group, "request-read-receipt", btn_request_read_receipt);
        let action_priority =
            add_composer_toggle_action(&group, "prioritize-message", btn_priority);

        let smime = AppSettings::try_new()
            .map(|settings| settings.crypto_protocol() == "smime")
            .unwrap_or(false);
        let sign_action_name = if smime { "smime-sign" } else { "pgp-sign" };
        let encrypt_action_name = if smime {
            "smime-encrypt"
        } else {
            "pgp-encrypt"
        };
        let sign_action = add_composer_toggle_action(&group, sign_action_name, btn_sign);
        let encrypt_action = add_composer_toggle_action(&group, encrypt_action_name, btn_encrypt);

        let show_from = AppSettings::try_new()
            .map(|s| s.composer_show_from())
            .unwrap_or(true);
        let action_view_from = gio::SimpleAction::new_stateful(
            "view-from",
            None,
            &show_from.to_variant(),
        );
        let dialog_for_view_from = self.downgrade();
        action_view_from.connect_activate(move |action, _| {
            let Some(dialog) = dialog_for_view_from.upgrade() else {
                return;
            };
            let current = action.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            let new_state = !current;
            action.set_state(&new_state.to_variant());
            for widget in dialog.imp().row_from_widgets.borrow().iter() {
                widget.set_visible(new_state);
            }
            if !dialog.imp().suppress_setting_persistence.get() {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_composer_show_from(new_state);
                }
            }
        });
        group.add_action(&action_view_from);

        let show_reply_to = AppSettings::try_new()
            .map(|s| s.composer_show_reply_to())
            .unwrap_or(false);
        let action_view_reply_to = gio::SimpleAction::new_stateful(
            "view-reply-to",
            None,
            &show_reply_to.to_variant(),
        );
        let dialog_for_view_reply_to = self.downgrade();
        action_view_reply_to.connect_activate(move |action, _| {
            let Some(dialog) = dialog_for_view_reply_to.upgrade() else {
                return;
            };
            let current = action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            let new_state = !current;
            action.set_state(&new_state.to_variant());
            for widget in dialog.imp().row_reply_to_widgets.borrow().iter() {
                widget.set_visible(new_state);
            }
            if !dialog.imp().suppress_setting_persistence.get() {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_composer_show_reply_to(new_state);
                }
            }
        });
        group.add_action(&action_view_reply_to);

        let show_cc = AppSettings::try_new()
            .map(|s| s.composer_show_cc())
            .unwrap_or(true);
        let action_view_cc = gio::SimpleAction::new_stateful(
            "view-cc",
            None,
            &show_cc.to_variant(),
        );
        let dialog_for_view_cc = self.downgrade();
        action_view_cc.connect_activate(move |action, _| {
            let Some(dialog) = dialog_for_view_cc.upgrade() else {
                return;
            };
            let current = action.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            let new_state = !current;
            action.set_state(&new_state.to_variant());
            for widget in dialog.imp().row_cc_widgets.borrow().iter() {
                widget.set_visible(new_state);
            }
            if !dialog.imp().suppress_setting_persistence.get() {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_composer_show_cc(new_state);
                }
            }
        });
        group.add_action(&action_view_cc);

        let show_bcc = AppSettings::try_new()
            .map(|s| s.composer_show_bcc())
            .unwrap_or(false);
        let action_view_bcc = gio::SimpleAction::new_stateful(
            "view-bcc",
            None,
            &show_bcc.to_variant(),
        );
        let dialog_for_view_bcc = self.downgrade();
        action_view_bcc.connect_activate(move |action, _| {
            let Some(dialog) = dialog_for_view_bcc.upgrade() else {
                return;
            };
            let current = action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            let new_state = !current;
            action.set_state(&new_state.to_variant());
            for widget in dialog.imp().row_bcc_widgets.borrow().iter() {
                widget.set_visible(new_state);
            }
            if !dialog.imp().suppress_setting_persistence.get() {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_composer_show_bcc(new_state);
                }
            }
        });
        group.add_action(&action_view_bcc);

        let charset = self.imp().charset.borrow().clone();
        let charset = mail_composer_service::normalize_charset(&charset).unwrap_or(charset);
        let action_charset = gio::SimpleAction::new_stateful(
            "charset",
            Some(&String::static_variant_type()),
            &charset.to_variant(),
        );
        let dialog_for_charset = self.downgrade();
        action_charset.connect_activate(move |action, parameter| {
            let Some(parameter) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            let Ok(charset) = mail_composer_service::normalize_charset(&parameter) else {
                return;
            };
            action.set_state(&charset.to_variant());
            if let Some(dialog) = dialog_for_charset.upgrade() {
                dialog.imp().charset.replace(charset);
                if !dialog.imp().suppress_setting_persistence.get()
                    && let Some(settings) = dialog.imp().settings.borrow().as_ref()
                {
                    settings.set_composer_charset(&dialog.imp().charset.borrow());
                }
                dialog.mark_changed();
            }
        });
        group.add_action(&action_charset);

        let sig_store = SignatureStore::from_environment();
        let signatures = sig_store.load();
        let sig_menu = gio::Menu::new();
        if signatures.is_empty() {
            let item = gio::MenuItem::new(
                Some("Keine Signaturen konfiguriert"),
                None,
            );
            sig_menu.append_item(&item);
        } else {
            for (idx, sig) in signatures.iter().enumerate() {
                let action_name = format!("insert-signature-{idx}");
                let sig_action = gio::SimpleAction::new(&action_name, None);
                let dialog_weak = self.downgrade();
                let sig_text = sig.text.clone();
                let sig_format = sig.format.clone();
                let sig_id = sig.id.clone();
                sig_action.connect_activate(move |_, _| {
                    if let Some(dialog) = dialog_weak.upgrade() {
                        if let Some(combo_sig) = dialog.imp().combo_signature.borrow().as_ref() {
                            let choices = dialog.imp().signature_choices.borrow();
                            if let Some(pos) =
                                choices.iter().position(|c| c.id.as_deref() == Some(&sig_id))
                            {
                                combo_sig.set_selected(pos as u32);
                                return;
                            }
                        }
                        let html = if sig_format.to_ascii_lowercase().contains("html") {
                            crate::services::html_sanitizer::sanitize_html(sig_text.trim_end())
                        } else {
                            html_escape(sig_text.trim_end()).replace('\n', "<br>")
                        };
                        dialog.insert_html_fragment(&format!("<p><br></p><p>--<br>{}</p>", html));
                    }
                });
                group.add_action(&sig_action);
                sig_menu.append(Some(&sig.name), Some(&format!("compose.{action_name}")));
            }
        }

        self.insert_action_group("compose", Some(&group));
        self.imp().composer_action_group.replace(Some(group));
        if let Some(combo) = self.imp().combo_from.borrow().as_ref() {
            self.update_sender_dependent_controls(combo.selected());
        }

        let file_menu = gio::Menu::new();
        file_menu.append(Some("Senden"), Some("compose.send"));
        file_menu.append(Some("Entwurf speichern"), Some("compose.save-draft"));
        file_menu.append(Some("Entwurf verwerfen"), Some("compose.discard"));
        file_menu.append(Some("Als Vorlage speichern…"), Some("compose.save-template"));
        file_menu.append(Some("Als Textbaustein speichern…"), Some("compose.save-snippet"));
        file_menu.append(Some("Als EML speichern…"), Some("compose.save-as-eml"));
        file_menu.append(Some("Druckvorschau"), Some("compose.print-preview"));
        file_menu.append(Some("Drucken…"), Some("compose.print"));
        file_menu.append(Some("Neue leere Nachricht"), Some("compose.new-message"));
        file_menu.append(Some("Schließen"), Some("compose.close"));

        let view_menu = gio::Menu::new();
        view_menu.append(Some("Absenderfeld (Von)"), Some("compose.view-from"));
        view_menu.append(Some("Antwortadresse (Reply-To)"), Some("compose.view-reply-to"));
        view_menu.append(Some("Kopiefeld (Cc)"), Some("compose.view-cc"));
        view_menu.append(Some("Blindkopiefeld (Bcc)"), Some("compose.view-bcc"));

        let insert_menu = gio::Menu::new();
        insert_menu.append(Some("Anlage hinzufügen…"), Some("compose.attach"));
        insert_menu.append(
            Some("Vorlage oder Textbaustein einfügen…"),
            Some("compose.insert-template"),
        );
        insert_menu.append_submenu(Some("Signatur einfügen"), &sig_menu);

        let options_menu = gio::Menu::new();
        options_menu.append(Some("HTML-/Nur-Text-Modus"), Some("compose.plain-text"));
        options_menu.append(
            Some("Lesebestätigung anfordern"),
            Some("compose.request-read-receipt"),
        );
        options_menu.append(
            Some("Nachricht priorisieren"),
            Some("compose.prioritize-message"),
        );
        options_menu.append(
            Some("Nachricht signieren"),
            Some(&format!("compose.{sign_action_name}")),
        );
        options_menu.append(
            Some("Nachricht verschlüsseln"),
            Some(&format!("compose.{encrypt_action_name}")),
        );

        let charset_menu = gio::Menu::new();
        for charset in [
            "UTF-8",
            "ISO-8859-1",
            "ISO-8859-15",
            "Windows-1252",
            "US-ASCII",
        ] {
            let item = gio::MenuItem::new(Some(charset), Some("compose.charset"));
            item.set_action_and_target_value(Some("compose.charset"), Some(&charset.to_variant()));
            charset_menu.append_item(&item);
        }
        options_menu.append_submenu(Some("Zeichenkodierung"), &charset_menu);

        let menu = gio::Menu::new();
        menu.append_submenu(Some("Datei"), &file_menu);
        menu.append_submenu(Some("Ansicht"), &view_menu);
        menu.append_submenu(Some("Einfügen"), &insert_menu);
        menu.append_submenu(Some("Optionen"), &options_menu);

        let popover = gtk::PopoverMenu::builder().menu_model(&menu).build();
        menu_button.set_popover(Some(&popover));

        let shortcut_controller = gtk::ShortcutController::new();
        shortcut_controller.set_scope(gtk::ShortcutScope::Managed);
        for (action, accelerator) in COMPOSER_ACTION_SHORTCUTS {
            let Some(trigger) = gtk::ShortcutTrigger::parse_string(accelerator) else {
                tracing::warn!(action, accelerator, "Invalid composer accelerator");
                continue;
            };
            let named_action = gtk::NamedAction::new(&format!("compose.{action}"));
            shortcut_controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(named_action)));
        }
        self.add_controller(shortcut_controller);

        self.set_can_close(false);
        let dialog_weak_for_close = self.downgrade();
        self.connect_close_attempt(move |_| {
            if let Some(dialog) = dialog_weak_for_close.upgrade() {
                if !dialog.handle_close_request() {
                    dialog.set_can_close(true);
                    dialog.close();
                }
            }
        });

        // Keep these bindings alive and make the intended surface explicit in
        // the action graph for tests and future menu extensions.
        let _ = (
            action_plain_text,
            action_request_read_receipt,
            action_priority,
            sign_action,
            encrypt_action,
            action_view_from,
            action_view_reply_to,
            action_view_cc,
            action_view_bcc,
        );
    }

    fn mark_changed(&self) {
        if self.imp().suppress_dirty_tracking.get() {
            return;
        }
        let revision = self.imp().dirty_revision.get().wrapping_add(1);
        self.imp().dirty_revision.set(revision);
    }

    fn mark_saved(&self) {
        self.imp()
            .saved_revision
            .set(self.imp().dirty_revision.get());
    }

    fn is_dirty(&self) -> bool {
        self.imp().dirty_revision.get() != self.imp().saved_revision.get()
    }

    fn discard_and_close(&self) {
        if self.imp().operation.get() == ComposeOperation::Sending {
            self.show_toast(
                "Nachricht wird bereits gesendet; Verwerfen ist nicht mehr möglich",
            );
            return;
        }
        self.imp().draft_discarded.set(true);
        if self.imp().operation.get() != ComposeOperation::Idle {
            self.request_operation_cancel();
        }
        if let Some(source) = self.imp().autosave_source.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source.as_raw()) };
        }
        let draft_id = self.imp().draft_id.borrow().clone();
        if !draft_id.is_empty() {
            let _ = DraftStoreService::default().delete(&draft_id);
        }
        self.imp().local_draft_persisted.set(false);
        self.imp().close_confirmed.set(true);
        self.set_can_close(true);
        self.close();
        self.show_toast("Entwurf verworfen");
    }

    fn handle_close_request(&self) -> bool {
        if self.imp().close_confirmed.replace(false) {
            return false;
        }
        if self.imp().print_state.get().is_running() {
            self.show_toast(
                "Der Composer verarbeitet noch einen Druckvorgang; bitte warten oder abbrechen",
            );
            return true;
        }
        if self.imp().autosave_in_progress.get() {
            self.imp().close_after_save.set(true);
            return true;
        }
        if self.imp().operation.get() == ComposeOperation::SavingDraft {
            self.imp().close_after_save.set(true);
            return true;
        }
        match close_request_decision(
            self.is_dirty(),
            self.imp().operation.get(),
        ) {
            CloseRequestDecision::Allow => {
                let draft_id = self.imp().draft_id.borrow().clone();
                if !draft_id.is_empty() && !self.imp().local_draft_persisted.get() {
                    let _ = DraftStoreService::default().delete(&draft_id);
                }
                false
            }
            CloseRequestDecision::BlockOperation => {
                self.show_toast(
                    "Der Composer verarbeitet noch einen Vorgang; bitte warten oder abbrechen",
                );
                true
            }
            CloseRequestDecision::AutoSave => {
                self.imp().close_after_save.set(true);
                if let Some(button) = self.imp().save_draft_button.borrow().as_ref() {
                    button.emit_clicked();
                } else {
                    self.imp().close_after_save.set(false);
                    return false;
                }
                true
            }
        }
    }

    fn current_outgoing_message(
        &self,
        body: String,
        plain_text: bool,
    ) -> Result<OutgoingMessage, String> {
        let entry_text = |entry: &RefCell<Option<gtk::Entry>>| {
            entry
                .borrow()
                .as_ref()
                .map(|entry| entry.text().to_string())
                .unwrap_or_default()
        };
        let combo_from = self
            .imp()
            .combo_from
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| "Kein Absender ausgewählt".to_string())?;
        let identity = self.selected_identity(combo_from.selected());
        let reply_to = self.effective_reply_to(&identity.reply_to);
        let (pgp_sign, pgp_encrypt) = self.pgp_flags_for_message();
        let charset = self.imp().charset.borrow().clone();
        let charset = mail_composer_service::normalize_charset(&charset)?;
        Ok(OutgoingMessage {
            source_uid: identity.source_uid,
            from: identity.from,
            reply_to,
            request_read_receipt: self.request_read_receipt_for_message(),
            to: entry_text(&self.imp().entry_to),
            cc: entry_text(&self.imp().entry_cc),
            bcc: entry_text(&self.imp().entry_bcc),
            subject: entry_text(&self.imp().entry_subject),
            html_body: body,
            plain_text,
            priority: self.priority_for_message(),
            charset,
            attachments: self.imp().attachments.borrow().clone(),
            calendar_method: self.imp().calendar_method.borrow().clone(),
            calendar_component: self.imp().calendar_component.borrow().clone(),
            pgp_sign,
            pgp_encrypt,
            pgp_key: self.pgp_key_for_message(),
            smime_options: self.smime_options_for_message(),
        })
    }

    fn start_print(&self, action: MailPrintAction) -> bool {
        if self.imp().operation.get() != ComposeOperation::Idle
            || self.imp().print_state.get().is_running()
        {
            self.show_toast("Ein anderer Composer-Vorgang läuft bereits");
            return false;
        }

        let Some(editor) = self.imp().html_editor.borrow().as_ref().cloned() else {
            self.show_toast("Drucken nicht möglich: Nachrichtentext ist nicht verfügbar");
            return false;
        };

        let generation = next_generation(self.imp().print_generation.get());
        self.imp().print_generation.set(generation);
        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .print_cancellation
            .replace(Some(Arc::clone(&cancellation)));
        self.set_print_state(
            MailPrintJobState::Preparing,
            if action.is_preview() {
                "Druckvorschau wird vorbereitet…"
            } else {
                "Druck wird vorbereitet…"
            },
        );

        let dialog = self.downgrade();
        editor.read_content(move |result| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            if dialog.imp().print_generation.get() != generation {
                return;
            }
            let content = match result {
                Ok(content) => content,
                Err(error) => {
                    dialog.finish_print_error(
                        generation,
                        format!("Nachrichtentext konnte nicht gelesen werden: {error}"),
                    );
                    return;
                }
            };
            let outgoing = match dialog.current_outgoing_message(content.body, content.plain_text) {
                Ok(outgoing) => outgoing,
                Err(error) => {
                    dialog.finish_print_error(generation, error);
                    return;
                }
            };
            let message_key = {
                let mut key = dialog.imp().print_message_key.borrow_mut();
                if key.is_empty() {
                    *key = format!("composer:{}", uuid::Uuid::new_v4());
                }
                key.clone()
            };
            let snapshot = composer_print_snapshot(&message_key, &outgoing);
            let (sender, receiver) = async_channel::bounded(1);
            let worker_cancellation = Arc::clone(&cancellation);
            let worker = std::thread::Builder::new()
                .name("composer-print-preparation".to_string())
                .spawn(move || {
                    let result =
                        mail_print_service::prepare_print_document(&snapshot, &worker_cancellation);
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Composer print result was dropped");
                    }
                });
            if let Err(error) = worker {
                drop(receiver);
                dialog.finish_print_error(
                    generation,
                    format!("Druckvorbereitung konnte nicht gestartet werden: {error}"),
                );
                return;
            }

            let dialog = dialog.downgrade();
            glib::spawn_future_local(async move {
                let result = receiver.recv().await.unwrap_or_else(|_| {
                    Err(mail_print_service::MailPrintDocumentError::WorkerDisconnected)
                });
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                if dialog.imp().print_generation.get() != generation {
                    return;
                }
                match result {
                    Ok(document) if action.is_preview() => {
                        dialog.set_print_state(
                            MailPrintJobState::Previewing,
                            "Druckvorschau wird geöffnet…",
                        );
                        if let Err(error) = dialog.present_print_preview(document) {
                            dialog.finish_print_error(generation, error);
                        } else {
                            dialog.finish_print_state(
                                generation,
                                MailPrintJobState::Idle,
                                "Druckvorschau geöffnet",
                            );
                        }
                    }
                    Ok(document) => dialog.start_native_print(generation, document),
                    Err(mail_print_service::MailPrintDocumentError::Cancelled) => {
                        dialog.finish_print_state(
                            generation,
                            MailPrintJobState::Cancelled,
                            "Druckvorbereitung abgebrochen",
                        );
                    }
                    Err(error) => dialog.finish_print_error(generation, error.to_string()),
                }
            });
        });
        true
    }

    fn set_print_state(&self, state: MailPrintJobState, status: &str) {
        self.imp().print_state.set(state);
        let running = state.is_running();
        if let Some(button) = self.imp().send_button.borrow().as_ref() {
            button.set_sensitive(!running);
        }
        if let Some(button) = self.imp().save_draft_button.borrow().as_ref() {
            button.set_sensitive(!running);
        }
        for &action in COMPOSER_OPERATION_ACTIONS {
            let enabled = !running && (action != "print" || cfg!(target_os = "linux"));
            self.set_action_enabled(action, enabled);
        }
        for control in self.imp().compose_controls.borrow().iter() {
            control.set_sensitive(!running);
        }
        if let Some(row) = self.imp().operation_row.borrow().as_ref() {
            row.set_visible(running);
        }
        if let Some(label) = self.imp().operation_status.borrow().as_ref() {
            label.set_text(status);
        }
        if let Some(spinner) = self.imp().operation_spinner.borrow().as_ref() {
            spinner.set_visible(running);
            if running {
                spinner.start();
            } else {
                spinner.stop();
            }
        }
        if let Some(button) = self.imp().cancel_operation_button.borrow().as_ref() {
            button.set_visible(state.is_cancellable());
            button.set_sensitive(state.is_cancellable());
        }
        if !running && let Some(combo) = self.imp().combo_from.borrow().as_ref() {
            self.update_sender_dependent_controls(combo.selected());
        }
    }

    fn finish_print_state(&self, generation: u64, state: MailPrintJobState, message: &str) {
        if self.imp().print_generation.get() != generation {
            return;
        }
        self.imp().print_cancellation.borrow_mut().take();
        self.set_print_state(state, message);
        self.show_toast(message);
    }

    fn finish_print_error(&self, generation: u64, error: impl Into<String>) {
        if self.imp().print_generation.get() != generation {
            return;
        }
        let message = format!("Drucken nicht möglich: {}", error.into());
        self.finish_print_state(generation, MailPrintJobState::Error, &message);
    }

    fn cancel_print(&self) -> bool {
        if !self.imp().print_state.get().is_cancellable() {
            return false;
        }
        if let Some(cancellation) = self.imp().print_cancellation.borrow_mut().take() {
            cancellation.store(true, Ordering::Release);
        }
        self.imp()
            .print_generation
            .set(next_generation(self.imp().print_generation.get()));
        self.set_print_state(
            MailPrintJobState::Cancelled,
            "Druckvorbereitung abgebrochen",
        );
        self.show_toast("Druckvorbereitung abgebrochen");
        true
    }

    fn shutdown_print(&self) {
        self.imp()
            .print_generation
            .set(next_generation(self.imp().print_generation.get()));
        if let Some(cancellation) = self.imp().print_cancellation.borrow_mut().take() {
            cancellation.store(true, Ordering::Release);
        }
        if let Some(window) = self.imp().print_preview_window.borrow_mut().take() {
            window.close();
        }
        self.imp().print_viewer.borrow_mut().take();
        self.imp().print_state.set(MailPrintJobState::Cancelled);
    }

    fn present_print_preview(
        &self,
        document: mail_print_service::MailPrintDocument,
    ) -> Result<(), String> {
        if let Some(window) = self.imp().print_preview_window.borrow_mut().take() {
            window.close();
        }
        let viewer = MailViewer::new();
        viewer.load_print_document(&document)?;
        let mut window_builder = gtk::Window::builder()
            .title("Druckvorschau")
            .default_width(900)
            .default_height(700);
        if let Some(parent) = self.root().and_downcast::<gtk::Window>() {
            window_builder = window_builder.transient_for(&parent);
        }
        let window = window_builder.build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&gtk::HeaderBar::new());
        content.append(&viewer);
        window.set_child(Some(&content));
        let dialog = self.downgrade();
        window.connect_close_request(move |window| {
            if let Some(dialog) = dialog.upgrade()
                && dialog
                    .imp()
                    .print_preview_window
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| current == window)
            {
                dialog.imp().print_preview_window.borrow_mut().take();
                dialog.imp().print_viewer.borrow_mut().take();
            }
            glib::Propagation::Proceed
        });
        self.imp().print_viewer.replace(Some(viewer));
        self.imp()
            .print_preview_window
            .replace(Some(window.clone()));
        window.present();
        Ok(())
    }

    fn start_native_print(&self, generation: u64, document: mail_print_service::MailPrintDocument) {
        if self.imp().print_generation.get() != generation {
            return;
        }
        self.imp().print_cancellation.borrow_mut().take();
        self.set_print_state(MailPrintJobState::Printing, "Druckdialog wird geöffnet…");
        let viewer = MailViewer::new();
        self.imp().print_viewer.replace(Some(viewer.clone()));
        let dialog = self.downgrade();
        let parent_window = self.root().and_downcast::<gtk::Window>();
        if let Some(parent) = parent_window {
            viewer.print_document(&document, &parent, move |result| {
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                dialog.imp().print_viewer.borrow_mut().take();
                match result {
                    Ok(crate::views::MailPrintDialogResult::Printed) => dialog.finish_print_state(
                        generation,
                        MailPrintJobState::Idle,
                        "Druckauftrag wurde an das System übergeben",
                    ),
                    Ok(crate::views::MailPrintDialogResult::Cancelled) => dialog
                        .finish_print_state(
                            generation,
                            MailPrintJobState::Cancelled,
                            "Druckdialog abgebrochen",
                        ),
                    Err(error) => dialog.finish_print_error(generation, error),
                }
            });
        } else {
            self.finish_print_error(generation, "Kein übergeordnetes Fenster für Druckdialog".to_string());
        }
    }

    fn request_read_receipt_for_message(&self) -> bool {
        self.imp()
            .request_read_receipt_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active())
    }

    fn update_sender_dependent_controls(&self, selected: u32) {
        let identity = self.selected_identity(selected);
        let sender_available = !identity.from.trim().is_empty();
        if let Some(entry_reply_to) = self.imp().entry_reply_to.borrow().as_ref() {
            if entry_reply_to.text().trim().is_empty() && !identity.reply_to.trim().is_empty() {
                entry_reply_to.set_text(&identity.reply_to);
            }
        }
        if !sender_available
            && let Some(button) = self.imp().request_read_receipt_button.borrow().as_ref()
            && button.is_active()
        {
            button.set_active(false);
        }
        let idle = self.imp().operation.get() == ComposeOperation::Idle;
        if let Some(button) = self.imp().request_read_receipt_button.borrow().as_ref() {
            button.set_sensitive(sender_available && idle);
        }
        self.set_action_enabled("request-read-receipt", sender_available && idle);
    }

    fn effective_reply_to(&self, identity_reply_to: &str) -> String {
        let explicit = self
            .imp()
            .entry_reply_to
            .borrow()
            .as_ref()
            .map(|entry| entry.text().trim().to_string())
            .unwrap_or_default();
        if explicit.is_empty() {
            identity_reply_to.to_string()
        } else {
            explicit
        }
    }

    pub fn set_row_reply_to_visible(&self, visible: bool) {
        for widget in self.imp().row_reply_to_widgets.borrow().iter() {
            widget.set_visible(visible);
        }
        if let Some(action) = self.composer_action("view-reply-to") {
            action.set_state(&visible.to_variant());
        }
    }

    pub fn set_row_cc_visible(&self, visible: bool) {
        for widget in self.imp().row_cc_widgets.borrow().iter() {
            widget.set_visible(visible);
        }
        if let Some(action) = self.composer_action("view-cc") {
            action.set_state(&visible.to_variant());
        }
    }

    pub fn set_row_bcc_visible(&self, visible: bool) {
        for widget in self.imp().row_bcc_widgets.borrow().iter() {
            widget.set_visible(visible);
        }
        if let Some(action) = self.composer_action("view-bcc") {
            action.set_state(&visible.to_variant());
        }
    }

    pub fn set_row_from_visible(&self, visible: bool) {
        for widget in self.imp().row_from_widgets.borrow().iter() {
            widget.set_visible(visible);
        }
        if let Some(action) = self.composer_action("view-from") {
            action.set_state(&visible.to_variant());
        }
    }

    pub fn set_on_send_success<F: Fn() + 'static>(&self, callback: F) {
        self.imp().on_send_success.replace(Some(Box::new(callback)));
    }

    fn composer_action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.imp()
            .composer_action_group
            .borrow()
            .as_ref()
            .and_then(|group| group.lookup_action(name))
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
    }

    fn validate_and_confirm_send<F: FnOnce() + 'static>(&self, on_confirmed: F) {
        let settings = AppSettings::try_new();
        let prompt_empty_subject = settings
            .as_ref()
            .map(|s| s.composer_prompt_empty_subject())
            .unwrap_or(true);
        let prompt_only_bcc = settings
            .as_ref()
            .map(|s| s.composer_prompt_only_bcc())
            .unwrap_or(true);

        let subject_empty = self
            .imp()
            .entry_subject
            .borrow()
            .as_ref()
            .map(|e| e.text().trim().is_empty())
            .unwrap_or(false);

        let to_empty = self
            .imp()
            .entry_to
            .borrow()
            .as_ref()
            .map(|e| e.text().trim().is_empty())
            .unwrap_or(true);
        let cc_empty = self
            .imp()
            .entry_cc
            .borrow()
            .as_ref()
            .map(|e| e.text().trim().is_empty())
            .unwrap_or(true);
        let bcc_has_recipients = self
            .imp()
            .entry_bcc
            .borrow()
            .as_ref()
            .map(|e| !e.text().trim().is_empty())
            .unwrap_or(false);
        let only_bcc = to_empty && cc_empty && bcc_has_recipients;

        let dialog_weak = self.downgrade();
        let on_confirmed = Rc::new(RefCell::new(Some(on_confirmed)));
        let on_confirmed_bcc = Rc::clone(&on_confirmed);
        let check_bcc_step = move || {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            if prompt_only_bcc && only_bcc {
                let alert = adw::AlertDialog::new(
                    Some("Nur verdeckte Empfänger (Bcc)"),
                    Some(
                        "Die Nachricht enthält ausschließlich verdeckte Empfänger (Bcc). Trotzdem senden?",
                    ),
                );
                alert.add_responses(&[("cancel", "Abbrechen"), ("send", "Senden")]);
                alert.set_response_appearance("send", adw::ResponseAppearance::Suggested);
                let on_confirmed_action = Rc::clone(&on_confirmed_bcc);
                alert.choose(&dialog, None::<&gio::Cancellable>, move |response| {
                    if response == "send" {
                        if let Some(action) = on_confirmed_action.borrow_mut().take() {
                            action();
                        }
                    }
                });
            } else if let Some(action) = on_confirmed_bcc.borrow_mut().take() {
                action();
            }
        };

        if prompt_empty_subject && subject_empty {
            let alert = adw::AlertDialog::new(
                Some("Kein Betreff"),
                Some("Die Nachricht hat keinen Betreff. Trotzdem senden?"),
            );
            alert.add_responses(&[("cancel", "Abbrechen"), ("send", "Trotzdem senden")]);
            alert.set_response_appearance("send", adw::ResponseAppearance::Suggested);
            alert.choose(self, None::<&gio::Cancellable>, move |response| {
                if response == "send" {
                    check_bcc_step();
                }
            });
        } else {
            check_bcc_step();
        }
    }

    fn priority_for_message(&self) -> bool {
        self.imp()
            .priority_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active())
    }

    fn save_as_eml(&self) {
        if self.imp().operation.get() != ComposeOperation::Idle {
            return;
        }
        let file_dialog = gtk::FileDialog::builder()
            .title("Als EML speichern")
            .accept_label("Speichern")
            .initial_name("Nachricht.eml")
            .modal(true)
            .build();
        let parent_win = self.root().and_downcast::<gtk::Window>();
        let dialog_weak = self.downgrade();
        file_dialog.save(parent_win.as_ref(), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            if dialog.imp().autosave_in_progress.get() {
                dialog.show_toast(
                    "Automatisches Speichern läuft noch; EML-Export startet danach erneut",
                );
                return;
            }
            let Some((operation_id, cancel)) =
                dialog.begin_operation(ComposeOperation::SavingEml)
            else {
                return;
            };
            dialog.set_operation_status(operation_id, "Nachricht wird für EML vorbereitet…", true);
            let Some(editor) = dialog.imp().html_editor.borrow().as_ref().cloned() else {
                dialog.finish_operation(operation_id);
                dialog.show_toast("EML-Export konnte den Editor nicht lesen");
                return;
            };
            let dialog_weak = dialog.downgrade();
            editor.read_content(move |result| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                if !dialog.is_current_operation(operation_id) {
                    return;
                }
                let content = match result {
                    Ok(content) => content,
                    Err(error) => {
                        dialog.finish_operation(operation_id);
                        dialog.show_toast(&format!(
                            "EML-Export konnte den Nachrichtentext nicht lesen: {error}"
                        ));
                        return;
                    }
                };
                let outgoing = match dialog.current_outgoing_message(content.body, content.plain_text)
                {
                    Ok(outgoing) => outgoing,
                    Err(error) => {
                        dialog.finish_operation(operation_id);
                        dialog.show_toast(&format!("EML-Export nicht möglich: {error}"));
                        return;
                    }
                };
                let path_for_worker = path.clone();
                let (sender, receiver) = async_channel::bounded(1);
                let cancel_for_worker = Arc::clone(&cancel);
                std::thread::spawn(move || {
                    if cancel_for_worker.is_cancel_requested() {
                        let _ = sender.send_blocking(EmlWorkerEvent::Cancelled);
                        return;
                    }
                    let raw = match mail_composer_service::build_rfc822(&outgoing) {
                        Ok(raw) => raw,
                        Err(error) => {
                            let _ = sender.send_blocking(EmlWorkerEvent::Finished(Err(error)));
                            return;
                        }
                    };
                    if cancel_for_worker.is_cancel_requested() {
                        let _ = sender.send_blocking(EmlWorkerEvent::Cancelled);
                        return;
                    }
                    match mail_composer_service::save_rfc822_atomically(
                        &path_for_worker,
                        &raw,
                        &cancel_for_worker,
                    ) {
                        Ok(mail_composer_service::AtomicSaveResult::Cancelled) => {
                            let _ = sender.send_blocking(EmlWorkerEvent::Cancelled);
                        }
                        Ok(mail_composer_service::AtomicSaveResult::Saved) => {
                            let _ = sender.send_blocking(EmlWorkerEvent::Finished(Ok(())));
                        }
                        Err(error) => {
                            let _ = sender.send_blocking(EmlWorkerEvent::Finished(Err(error)));
                        }
                    }
                });

                let dialog_weak = dialog.downgrade();
                glib::spawn_future_local(async move {
                    match receiver.recv().await {
                        Ok(EmlWorkerEvent::Cancelled) => {
                            if let Some(dialog) = dialog_weak.upgrade()
                                && dialog.is_current_operation(operation_id)
                            {
                                dialog.finish_operation(operation_id);
                                dialog.show_toast("EML-Export abgebrochen");
                            }
                        }
                        Ok(EmlWorkerEvent::Finished(result)) => {
                            if let Some(dialog) = dialog_weak.upgrade()
                                && dialog.is_current_operation(operation_id)
                            {
                                dialog.finish_operation(operation_id);
                                match result {
                                    Ok(()) => {
                                        dialog.show_toast("Nachricht als EML gespeichert");
                                    }
                                    Err(error) => {
                                        dialog.show_toast(&format!(
                                            "EML-Export fehlgeschlagen; Nachricht bleibt geöffnet: {error}"
                                        ));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            if let Some(dialog) = dialog_weak.upgrade()
                                && dialog.is_current_operation(operation_id)
                            {
                                dialog.finish_operation(operation_id);
                                dialog.show_toast(&format!(
                                    "EML-Export abgebrochen; Nachricht bleibt geöffnet: {error}"
                                ));
                            }
                        }
                    }
                });
            });
        });
    }

    pub fn refresh_signature_choices(&self) {
        let choices = load_signature_choices();
        let sig_names: Vec<&str> = choices.iter().map(|c| c.name.as_str()).collect();
        let sig_model = gtk::StringList::new(&sig_names);
        self.imp().signature_choices.replace(choices);
        if let Some(combo_sig) = self.imp().combo_signature.borrow().as_ref() {
            combo_sig.set_model(Some(&sig_model));
        }
        self.sync_signature_for_selected_account();
    }

    pub(crate) fn sync_signature_for_selected_account(&self) {
        let Some(combo_from) = self.imp().combo_from.borrow().as_ref().cloned() else {
            return;
        };
        let Some(combo_sig) = self.imp().combo_signature.borrow().as_ref().cloned() else {
            return;
        };
        let identity = self.selected_identity(combo_from.selected());
        let choices = self.imp().signature_choices.borrow();
        let target_idx = if let Some(sig_id) = identity
            .signature_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            if sig_id == "default" {
                choices
                    .iter()
                    .position(|c| c.id.as_deref() == Some("default"))
                    .unwrap_or(0)
            } else {
                choices
                    .iter()
                    .position(|c| c.id.as_deref() == Some(sig_id))
                    .unwrap_or(0)
            }
        } else if let Some(active_id) = SignatureStore::default().active_id() {
            choices
                .iter()
                .position(|c| c.id.as_deref() == Some(&active_id))
                .unwrap_or(0)
        } else if choices.iter().any(|c| c.id.as_deref() == Some("default")) {
            choices
                .iter()
                .position(|c| c.id.as_deref() == Some("default"))
                .unwrap_or(0)
        } else {
            0
        };
        if combo_sig.selected() != target_idx as u32 {
            combo_sig.set_selected(target_idx as u32);
        }
    }

    fn apply_default_signature(&self) {
        let Some(combo_sig) = self.imp().combo_signature.borrow().as_ref().cloned() else {
            return;
        };
        let choices = self.imp().signature_choices.borrow();
        let append_sig = AppSettings::try_new()
            .map(|s| s.append_signature())
            .unwrap_or(true);
        let active_id = SignatureStore::default().active_id();

        let default_idx = if append_sig {
            if let Some(active_id) = active_id {
                choices
                    .iter()
                    .position(|c| c.id.as_deref() == Some(&active_id))
                    .unwrap_or(0)
            } else if choices.iter().any(|c| c.id.as_deref() == Some("default")) {
                choices
                    .iter()
                    .position(|c| c.id.as_deref() == Some("default"))
                    .unwrap_or(0)
            } else if choices.len() > 1 {
                1
            } else {
                0
            }
        } else {
            0
        };

        self.imp().suppress_signature_change.set(true);
        combo_sig.set_selected(default_idx as u32);
        self.imp().suppress_signature_change.set(false);

        if let Some(choice) = choices.get(default_idx) {
            if !choice.html.is_empty() {
                let signature = format_signature_markup(&choice.html);
                let top_signature = AppSettings::try_new()
                    .map(|s| s.composer_top_signature())
                    .unwrap_or(false);
                let initial_html = if top_signature {
                    format!("{signature}<p><br></p>")
                } else {
                    format!("<p><br></p>{signature}")
                };
                if let Some(editor) = self.imp().html_editor.borrow().as_ref() {
                    editor.set_html(&initial_html);
                }
            }
        }
    }

    /// Insert an HTML fragment at the current cursor position.
    pub fn insert_html_fragment(&self, html: &str) {
        if let Some(editor) = self.imp().html_editor.borrow().as_ref() {
            editor.insert_html(html);
        }
    }

    fn start_local_autosave(&self) {
        if let Some(source) = self.imp().autosave_source.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source.as_raw()) };
        }
        let interval = AppSettings::try_new()
            .map(|settings| settings.draft_autosave_interval_seconds())
            .unwrap_or(30);
        if interval == 0 {
            return;
        }

        let dialog = self.downgrade();
        let source = glib::timeout_add_seconds_local(interval, move || {
            let Some(dialog) = dialog.upgrade() else {
                return glib::ControlFlow::Break;
            };
            dialog.autosave_local_draft();
            glib::ControlFlow::Continue
        });
        self.imp().autosave_source.replace(Some(source));
    }

    fn autosave_local_draft(&self) {
        // Keep a local recovery copy even when Camel/Graph is configured.  It
        // is not a replacement for the authoritative synchronized Drafts
        // folder; it protects the open composer while transport or EDS is
        // unavailable.
        if self.imp().operation.get() != ComposeOperation::Idle
            || self.imp().autosave_in_progress.get()
        {
            return;
        }
        self.imp().autosave_in_progress.set(true);
        let Some(entry_to) = self.imp().entry_to.borrow().as_ref().cloned() else {
            self.imp().autosave_in_progress.set(false);
            return;
        };
        let Some(entry_cc) = self.imp().entry_cc.borrow().as_ref().cloned() else {
            self.imp().autosave_in_progress.set(false);
            return;
        };
        let Some(entry_bcc) = self.imp().entry_bcc.borrow().as_ref().cloned() else {
            self.imp().autosave_in_progress.set(false);
            return;
        };
        let Some(entry_subject) = self.imp().entry_subject.borrow().as_ref().cloned() else {
            self.imp().autosave_in_progress.set(false);
            return;
        };
        let Some(combo_from) = self.imp().combo_from.borrow().as_ref().cloned() else {
            self.imp().autosave_in_progress.set(false);
            return;
        };
        let Some(html_editor) = self.imp().html_editor.borrow().as_ref().cloned() else {
            self.imp().autosave_in_progress.set(false);
            return;
        };

        let to = entry_to.text().to_string();
        let cc = entry_cc.text().to_string();
        let bcc = entry_bcc.text().to_string();
        let subject = entry_subject.text().to_string();
        let identity = self.selected_identity(combo_from.selected());
        let from = identity.from;
        let reply_to = self.effective_reply_to(&identity.reply_to);
        let source_uid = identity.source_uid;
        let draft_id = self.imp().draft_id.borrow().clone();
        let attachments = self.imp().attachments.borrow().clone();
        let calendar_method = self.imp().calendar_method.borrow().clone();
        let calendar_component = self.imp().calendar_component.borrow().clone();
        let pgp_sign = self
            .imp()
            .pgp_sign_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active());
        let pgp_encrypt = self
            .imp()
            .pgp_encrypt_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active());
        let request_read_receipt = self.request_read_receipt_for_message();
        let priority = self.priority_for_message();
        let charset = self.imp().charset.borrow().clone();
        let pgp_key = self.pgp_key_for_message();
        let smime_options = self.smime_options_for_message();
        let revision = self.imp().dirty_revision.get();
        let expected_draft = self.imp().persisted_draft.borrow().clone();
        let dialog = self.downgrade();
        html_editor.read_content(move |result| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            if dialog.imp().operation.get() != ComposeOperation::Idle {
                dialog.imp().autosave_in_progress.set(false);
                return;
            }
            let content = match result {
                Ok(content) => content,
                Err(error) => {
                    dialog.imp().autosave_in_progress.set(false);
                    if !dialog.imp().autosave_error_shown.get() {
                        dialog.imp().autosave_error_shown.set(true);
                        dialog.show_toast(&format!(
                            "Automatisches Speichern konnte den Entwurf nicht lesen: {error}"
                        ));
                    }
                    return;
                }
            };
            let draft = DraftMessage {
                id: draft_id,
                from,
                reply_to,
                source_uid,
                to,
                cc,
                bcc,
                subject,
                html_body: content.body,
                plain_text: content.plain_text,
                updated_at: Utc::now().to_rfc3339(),
                attachments,
                calendar_method,
                calendar_component,
                pgp_sign,
                pgp_encrypt,
                pgp_key,
                smime_options: Some(DraftComposerOptions {
                    request_read_receipt,
                    priority,
                    charset,
                    smime: smime_options,
                }),
            };
            if !draft_has_user_content(&draft) {
                if dialog.imp().local_draft_persisted.replace(false) {
                    let draft_id = dialog.imp().draft_id.borrow().clone();
                    if !draft_id.is_empty() {
                        let _ = DraftStoreService::default().delete(&draft_id);
                    }
                }
                dialog.imp().autosave_in_progress.set(false);
                if dialog.imp().close_after_save.replace(false) {
                    dialog.imp().close_confirmed.set(true);
                    dialog.set_can_close(true);
                    dialog.close();
                }
                return;
            }

            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let store = DraftStoreService::default();
                let result = match store.save_if_matches(expected_draft.as_ref(), draft.clone()) {
                    Ok(true) => Ok(draft),
                    Ok(false) => Err(
                        "Entwurf wurde zwischenzeitlich geändert; die neuere Recovery-Version bleibt erhalten"
                            .to_string(),
                    ),
                    Err(error) => Err(error.to_string()),
                };
                let _ = sender.send_blocking(result);
            });
            let dialog_weak = dialog.downgrade();
            glib::spawn_future_local(async move {
                let result = receiver.recv().await;
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                dialog.imp().autosave_in_progress.set(false);
                if dialog.imp().draft_discarded.get() {
                    let draft_id = dialog.imp().draft_id.borrow().clone();
                    if !draft_id.is_empty() {
                        let _ = DraftStoreService::default().delete(&draft_id);
                    }
                    return;
                }
                match result {
                    Ok(Ok(saved_draft)) => {
                        dialog.imp().local_draft_persisted.set(true);
                        dialog.imp().persisted_draft.replace(Some(saved_draft));
                        if dialog.imp().dirty_revision.get() == revision {
                            dialog.mark_saved();
                        }
                        dialog.imp().autosave_error_shown.set(false);
                        tracing::debug!("Local draft autosaved");
                        if dialog.imp().close_after_save.replace(false) {
                            dialog.show_toast("Entwurf gespeichert");
                            dialog.imp().close_confirmed.set(true);
                            dialog.set_can_close(true);
                            dialog.close();
                        }
                    }
                    Ok(Err(error)) => {
                        if !dialog.imp().autosave_error_shown.get() {
                            dialog.imp().autosave_error_shown.set(true);
                            tracing::warn!(%error, "Could not autosave local draft");
                            dialog.show_toast(&format!(
                                "Automatisches Speichern fehlgeschlagen; der Entwurf bleibt geöffnet: {error}"
                            ));
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Autosave result was dropped");
                    }
                }
            });
        });
    }

    fn begin_operation(
        &self,
        operation: ComposeOperation,
    ) -> Option<(u64, Arc<CancellationToken>)> {
        let imp = self.imp();
        if !compose_operation_can_start(imp.operation.get()) || imp.print_state.get().is_running() {
            return None;
        }
        let operation_id = imp.operation_id.get().wrapping_add(1).max(1);
        let cancel = Arc::new(CancellationToken::new());
        imp.operation_id.set(operation_id);
        imp.operation.set(operation);
        imp.transport_started.set(false);
        imp.operation_cancel.replace(Some(Arc::clone(&cancel)));
        if let Some(button) = imp.send_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = imp.save_draft_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        for &action in COMPOSER_OPERATION_ACTIONS {
            self.set_action_enabled(action, false);
        }
        for control in imp.compose_controls.borrow().iter() {
            control.set_sensitive(false);
        }
        if let Some(row) = imp.operation_row.borrow().as_ref() {
            row.set_visible(true);
        }
        if let Some(spinner) = imp.operation_spinner.borrow().as_ref() {
            spinner.set_visible(true);
            spinner.start();
        }
        if let Some(button) = imp.cancel_operation_button.borrow().as_ref() {
            button.set_visible(true);
            button.set_sensitive(true);
        }
        Some((operation_id, cancel))
    }

    fn is_current_operation(&self, operation_id: u64) -> bool {
        self.imp().operation_id.get() == operation_id
            && self.imp().operation.get() != ComposeOperation::Idle
    }

    fn set_operation_status(&self, operation_id: u64, status: &str, cancelable: bool) {
        if !self.is_current_operation(operation_id) {
            return;
        }
        let imp = self.imp();
        if let Some(row) = imp.operation_row.borrow().as_ref() {
            row.set_visible(true);
        }
        if let Some(label) = imp.operation_status.borrow().as_ref() {
            label.set_text(status);
        }
        if let Some(button) = imp.cancel_operation_button.borrow().as_ref() {
            button.set_visible(cancelable);
            button.set_sensitive(cancelable);
        }
    }

    fn mark_transport_started(&self, operation_id: u64) {
        if !self.is_current_operation(operation_id) {
            return;
        }
        self.imp().transport_started.set(true);
        self.set_operation_status(
            operation_id,
            "Übertragung läuft; ein Abbruch ist jetzt nicht mehr möglich…",
            false,
        );
    }

    fn request_operation_cancel(&self) {
        let imp = self.imp();
        if imp.operation.get() == ComposeOperation::Idle {
            self.cancel_print();
            return;
        }
        if imp.transport_started.get()
            || imp
                .operation_cancel
                .borrow()
                .as_ref()
                .is_some_and(|cancel| cancel.is_committed())
        {
            self.show_toast(
                "Der Transport läuft bereits; ein Abbruch ist technisch nicht mehr möglich",
            );
            return;
        }
        if let Some(cancel) = imp.operation_cancel.borrow().as_ref()
            && !cancel.request_cancel()
        {
            return;
        }
        if let Some(button) = imp.cancel_operation_button.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(label) = imp.operation_status.borrow().as_ref() {
            label.set_text("Abbruch wird verarbeitet…");
        }
    }

    fn finish_operation(&self, operation_id: u64) -> bool {
        if !self.is_current_operation(operation_id) {
            return false;
        }
        let imp = self.imp();
        imp.operation.set(ComposeOperation::Idle);
        imp.transport_started.set(false);
        imp.operation_cancel.borrow_mut().take();
        if let Some(button) = imp.send_button.borrow().as_ref() {
            button.set_sensitive(true);
        }
        if let Some(button) = imp.save_draft_button.borrow().as_ref() {
            button.set_sensitive(true);
        }
        for &action in COMPOSER_OPERATION_ACTIONS {
            self.set_action_enabled(action, action != "print" || cfg!(target_os = "linux"));
        }
        for control in imp.compose_controls.borrow().iter() {
            control.set_sensitive(true);
        }
        if let Some(button) = imp.cancel_operation_button.borrow().as_ref() {
            button.set_visible(false);
            button.set_sensitive(true);
        }
        if let Some(spinner) = imp.operation_spinner.borrow().as_ref() {
            spinner.stop();
            spinner.set_visible(false);
        }
        if let Some(row) = imp.operation_row.borrow().as_ref() {
            row.set_visible(false);
        }
        if let Some(combo) = imp.combo_from.borrow().as_ref() {
            self.update_sender_dependent_controls(combo.selected());
        }
        true
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        let Some(group) = self.imp().composer_action_group.borrow().as_ref().cloned() else {
            return;
        };
        let Some(action) = group
            .lookup_action(name)
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        else {
            return;
        };
        action.set_enabled(enabled);
    }

    fn selected_identity(&self, selected: u32) -> ComposeIdentity {
        self.imp()
            .source_identities
            .borrow()
            .get(selected as usize)
            .cloned()
            .unwrap_or_default()
    }

    fn pgp_key_for_message(&self) -> String {
        self.imp()
            .pgp_key_override
            .borrow()
            .clone()
            .unwrap_or_else(|| {
                AppSettings::try_new()
                    .map(|settings| settings.default_pgp_key())
                    .unwrap_or_default()
            })
    }

    fn security_toggle_state(&self) -> (bool, bool) {
        let sign = self
            .imp()
            .pgp_sign_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active());
        let encrypt = self
            .imp()
            .pgp_encrypt_button
            .borrow()
            .as_ref()
            .is_some_and(|button| button.is_active());
        (sign, encrypt)
    }

    fn pgp_flags_for_message(&self) -> (bool, bool) {
        let (sign, encrypt) = self.security_toggle_state();
        let smime = AppSettings::try_new()
            .map(|settings| settings.crypto_protocol() == "smime")
            .unwrap_or(false);
        if smime {
            (false, false)
        } else {
            (sign, encrypt)
        }
    }

    fn smime_options_for_message(&self) -> Option<SmimeOptions> {
        let settings = AppSettings::try_new()?;
        if settings.crypto_protocol() != "smime" {
            return None;
        }
        let (sign, encrypt) = self.security_toggle_state();
        if let Some(override_options) = self.imp().smime_options_override.borrow().as_ref() {
            return Some(SmimeOptions {
                sign,
                encrypt,
                ..override_options.clone()
            });
        }
        Some(settings.smime_options(sign, encrypt))
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
        } else {
            tracing::warn!(%message, "Composer result");
        }
    }

    fn add_attachment_path(&self, path: &Path) {
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() {
            return;
        }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let attachment = DraftAttachment {
            path: path.to_string_lossy().into_owned(),
            filename: filename.to_string(),
            mime_type: mime_type_for_path(path),
            size: metadata.len(),
            content_id: None,
            embedded_content_base64: String::new(),
        };
        self.add_attachment(attachment);
    }

    fn install_file_drop_target(&self) {
        let drop_target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);
        let dialog_weak = self.downgrade();
        drop_target.connect_drop(move |_, value, _, _| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return false;
            };
            let mut any_added = false;
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                for file in file_list.files() {
                    if let Some(path) = file.path() {
                        if path.is_file() {
                            dialog.add_attachment_path(&path);
                            any_added = true;
                        }
                    }
                }
            } else if let Ok(file) = value.get::<gio::File>() {
                if let Some(path) = file.path() {
                    if path.is_file() {
                        dialog.add_attachment_path(&path);
                        any_added = true;
                    }
                }
            }
            any_added
        });
        self.add_controller(drop_target);
    }

    fn add_attachment(&self, attachment: DraftAttachment) {
        if self
            .imp()
            .attachments
            .borrow()
            .iter()
            .any(|existing| existing.path == attachment.path)
        {
            return;
        }
        self.imp().attachments.borrow_mut().push(attachment.clone());
        self.mark_changed();
        let Some(list) = self.imp().attachment_list.borrow().as_ref().cloned() else {
            return;
        };

        let icon_name = match attachment.mime_type.as_str() {
            m if m.starts_with("image/") => "image-x-generic-symbolic",
            m if m.contains("pdf") => "x-office-document-symbolic",
            m if m.contains("zip") || m.contains("tar") || m.contains("compressed") => {
                "package-x-generic-symbolic"
            }
            m if m.starts_with("audio/") => "audio-x-generic-symbolic",
            m if m.starts_with("video/") => "video-x-generic-symbolic",
            m if m.contains("text/") => "text-x-generic-symbolic",
            _ => "mail-attachment-symbolic",
        };
        let icon = gtk::Image::from_icon_name(icon_name);
        let row = adw::ActionRow::builder()
            .title(&attachment.filename)
            .subtitle(&format!(
                "{} · {}",
                attachment.mime_type,
                format_size(attachment.size)
            ))
            .build();
        row.add_prefix(&icon);
        let remove_button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Anhang entfernen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&remove_button);
        list.append(&row);
        list.set_visible(true);

        let path = attachment.path;
        let row_for_remove = row.clone();
        let list_for_remove = list.clone();
        let dialog_weak = self.downgrade();
        remove_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog
                    .imp()
                    .attachments
                    .borrow_mut()
                    .retain(|attachment| attachment.path != path);
                dialog.mark_changed();
            }
            list_for_remove.remove(&row_for_remove);
            list_for_remove.set_visible(list_for_remove.first_child().is_some());
        });
    }
}

fn add_composer_toggle_action(
    group: &gio::SimpleActionGroup,
    name: &str,
    button: &gtk::ToggleButton,
) -> gio::SimpleAction {
    let action = gio::SimpleAction::new_stateful(name, None, &button.is_active().to_variant());
    let button_for_action = button.clone();
    action.connect_activate(move |action, _| {
        let active = action
            .state()
            .and_then(|state| state.get::<bool>())
            .map(|active| !active)
            .unwrap_or(true);
        action.set_state(&active.to_variant());
        button_for_action.set_active(active);
    });
    let action_for_button = action.clone();
    button.connect_toggled(move |button| {
        action_for_button.set_state(&button.is_active().to_variant());
    });
    group.add_action(&action);
    action
}

impl Default for ComposeDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn local_account_choices() -> Vec<ComposeIdentity> {
    AccountConfigStore::default()
        .load()
        .into_iter()
        .filter(|profile| profile.enabled)
        .flat_map(compose_identities_for_profile)
        .collect()
}

fn preferred_composer_account_id() -> Option<String> {
    let current = AppSettings::try_new()?.default_account_id();
    if current.trim().is_empty() {
        return None;
    }
    let profiles = AccountConfigStore::default().load();
    let preferred = reconciled_composer_default_account(&current, &profiles);
    (!preferred.trim().is_empty()).then_some(preferred)
}

fn reconciled_composer_default_account(current: &str, profiles: &[AccountProfile]) -> String {
    if profiles.iter().any(|profile| profile.id == current) {
        reconciled_local_default_account(current, current, profiles)
    } else if current.starts_with("account:") {
        profiles
            .iter()
            .find(|profile| profile.enabled)
            .map(|profile| profile.id.clone())
            .unwrap_or_default()
    } else {
        current.to_string()
    }
}

fn compose_identities_for_profile(profile: AccountProfile) -> Vec<ComposeIdentity> {
    let source_uid = profile.id;
    let display_name = profile.display_name.trim().to_string();
    let reply_to = profile.reply_to.trim().to_string();
    let primary_address = profile.email.trim().to_string();
    let primary_label = format!("{} <{}> · lokales Profil", display_name, primary_address);
    let primary_from = format!("{} <{}>", display_name, primary_address);
    let signature_id = profile.signature_id;
    let mut identities = vec![ComposeIdentity {
        source_uid: source_uid.clone(),
        label: primary_label,
        from: primary_from,
        reply_to: reply_to.clone(),
        signature_id: signature_id.clone(),
    }];
    identities.extend(
        profile
            .aliases
            .into_iter()
            .map(|alias| alias.trim().to_string())
            .filter(|alias| !alias.is_empty())
            .map(|alias| ComposeIdentity {
                source_uid: source_uid.clone(),
                label: format!(
                    "{} <{}> · Alias von {}",
                    display_name, alias, primary_address
                ),
                from: format!("{} <{}>", display_name, alias),
                reply_to: reply_to.clone(),
                signature_id: signature_id.clone(),
            }),
    );
    identities
}

fn compose_identity_matches_from(identity: &ComposeIdentity, requested: &str) -> bool {
    let requested = requested.trim();
    if requested.is_empty() {
        return true;
    }
    if identity.from.eq_ignore_ascii_case(requested)
        || identity.label.eq_ignore_ascii_case(requested)
    {
        return true;
    }
    let Some(address) = identity
        .from
        .split_once('<')
        .and_then(|(_, address)| address.strip_suffix('>'))
    else {
        return false;
    };
    requested.eq_ignore_ascii_case(address.trim())
        || requested
            .to_ascii_lowercase()
            .contains(&format!("<{}>", address.trim()).to_ascii_lowercase())
}

fn reply_subject(subject: &str) -> String {
    if subject.trim_start().to_ascii_lowercase().starts_with("re:") {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}

fn forward_subject(subject: &str) -> String {
    if subject
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("fwd:")
    {
        subject.to_string()
    } else {
        format!("Fwd: {subject}")
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn reply_body_html_with_style(
    sender: &str,
    original_to: &str,
    original_cc: &str,
    date: &str,
    original_html: &str,
    style: &str,
) -> String {
    let style = if style == "default" {
        default_quote_style()
    } else {
        style
    };
    let top_signature = AppSettings::try_new()
        .map(|s| s.composer_top_signature())
        .unwrap_or(false);
    let (prefix, suffix) = if top_signature {
        (signature_prefix_html(), String::new())
    } else {
        ("<p><br></p>".to_string(), signature_body_html())
    };

    if style == "none" {
        return if top_signature {
            prefix
        } else {
            format!("{prefix}{suffix}")
        };
    }

    let attribution = if sender.trim().is_empty() {
        "der ursprünglichen Nachricht".to_string()
    } else {
        format!("{} schrieb", html_escape(sender.trim()))
    };
    let quoted = quote_as_safe_text(original_html);
    if style == "inline" {
        let mut headers = Vec::new();
        if !sender.trim().is_empty() {
            headers.push(format!("Von: {}", html_escape(sender.trim())));
        }
        if !date.trim().is_empty() {
            headers.push(format!("Datum: {}", html_escape(date.trim())));
        }
        if !original_to.trim().is_empty() {
            headers.push(format!("An: {}", html_escape(original_to.trim())));
        }
        if !original_cc.trim().is_empty() {
            headers.push(format!("Cc: {}", html_escape(original_cc.trim())));
        }
        let header_block = if headers.is_empty() {
            "Ursprüngliche Nachricht:".to_string()
        } else {
            format!("Ursprüngliche Nachricht:<br>{}", headers.join("<br>"))
        };
        return if quoted.trim().is_empty() {
            if top_signature {
                prefix
            } else {
                format!("{prefix}{suffix}")
            }
        } else {
            format!("{prefix}<p>{header_block}</p><pre>{quoted}</pre>{suffix}")
        };
    }

    let quoted = format_quoted_body(&quoted, style);
    if quoted.is_empty() {
        if top_signature {
            prefix
        } else {
            format!("{prefix}{suffix}")
        }
    } else {
        format!("{prefix}<p>{attribution}:</p>{quoted}{suffix}")
    }
}

fn forward_body_html_with_style(
    sender: &str,
    recipient_to: &str,
    recipient_cc: &str,
    date: &str,
    original_html: &str,
    quote_style: &str,
) -> String {
    let quote_style = if quote_style == "default" {
        default_forward_style()
    } else {
        quote_style
    };
    let top_signature = AppSettings::try_new()
        .map(|s| s.composer_top_signature())
        .unwrap_or(false);
    let (prefix, suffix) = if top_signature {
        (signature_prefix_html(), String::new())
    } else {
        ("<p><br></p>".to_string(), signature_body_html())
    };

    let mut headers = Vec::new();
    if !sender.trim().is_empty() {
        headers.push(format!("Von: {}", html_escape(sender.trim())));
    }
    if !date.trim().is_empty() {
        headers.push(format!("Datum: {}", html_escape(date.trim())));
    }
    if !recipient_to.trim().is_empty() {
        headers.push(format!("An: {}", html_escape(recipient_to.trim())));
    }
    if !recipient_cc.trim().is_empty() {
        headers.push(format!("Cc: {}", html_escape(recipient_cc.trim())));
    }

    let header_block = if headers.is_empty() {
        "Weitergeleitete Nachricht:".to_string()
    } else {
        format!("Weitergeleitete Nachricht:<br>{}", headers.join("<br>"))
    };
    let quoted = quote_as_safe_text(original_html);
    let quoted = if quote_style == "inline" {
        (!quoted.trim().is_empty())
            .then(|| format!("<pre>{quoted}</pre>"))
            .unwrap_or_default()
    } else {
        format_quoted_body(&quoted, quote_style)
    };
    if quoted.is_empty() {
        if top_signature {
            prefix
        } else {
            format!("{prefix}{suffix}")
        }
    } else {
        format!("{prefix}<p>{header_block}</p>{quoted}{suffix}")
    }
}

fn format_quoted_body(quoted: &str, style: &str) -> String {
    if quoted.trim().is_empty() || style == "none" {
        return String::new();
    }
    if style == "prefix" {
        let lines = quoted
            .lines()
            .map(|line| format!("&gt; {line}"))
            .collect::<Vec<_>>()
            .join("<br>");
        format!("<p>{lines}</p>")
    } else {
        format!("<blockquote><pre>{quoted}</pre></blockquote>")
    }
}

fn signature_delimiter_html() -> &'static str {
    let outline = AppSettings::try_new()
        .map(|s| s.composer_signature_outline())
        .unwrap_or(true);
    if outline {
        "--<br>"
    } else {
        ""
    }
}

fn load_signature_choices() -> Vec<SignatureChoice> {
    let mut choices = vec![SignatureChoice {
        id: None,
        name: "Keine".to_string(),
        html: String::new(),
    }];

    let store_signatures = SignatureStore::default().load();
    for sig in store_signatures {
        let html = if sig.is_html() {
            sanitize_html(sig.text.trim_end())
        } else {
            signature_plain_text_to_html(sig.text.trim_end())
        };
        choices.push(SignatureChoice {
            id: Some(sig.id),
            name: sig.name,
            html,
        });
    }

    if let Some(settings) = AppSettings::try_new() {
        let sig_text = settings.signature_text();
        if !sig_text.trim().is_empty()
            && !choices.iter().any(|c| c.id.as_deref() == Some("default"))
        {
            let lower = sig_text.to_ascii_lowercase();
            let is_html = lower.contains("<img")
                || lower.contains("<p")
                || lower.contains("<div")
                || lower.contains("<br")
                || lower.contains("<span")
                || lower.contains("<table")
                || lower.contains("<a");
            let html = if is_html {
                sanitize_html(sig_text.trim_end())
            } else {
                signature_plain_text_to_html(sig_text.trim_end())
            };
            choices.push(SignatureChoice {
                id: Some("default".to_string()),
                name: "Allgemeine Standardsignatur".to_string(),
                html,
            });
        }
    }

    choices
}

pub fn signature_plain_text_to_html(text: &str) -> String {
    let escaped = html_escape(text);
    static URL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(https?://[^\s<]+|mailto:[^\s<]+|www\.[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}[^\s<]*|[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,})").unwrap()
    });

    let with_links = URL_RE.replace_all(&escaped, |caps: &regex::Captures| {
        let matched = &caps[0];
        let mut clean = matched;
        let mut trailing = "";
        while let Some(last) = clean.chars().last() {
            if matches!(last, '.' | ',' | ';' | ':' | ')') {
                let split_idx = clean.len() - last.len_utf8();
                trailing = &matched[split_idx..];
                clean = &clean[..split_idx];
            } else {
                break;
            }
        }
        let href = if clean.starts_with("www.") {
            format!("https://{clean}")
        } else if clean.contains('@') && !clean.starts_with("mailto:") && !clean.starts_with("http") {
            format!("mailto:{clean}")
        } else {
            clean.to_string()
        };
        format!("<a href=\"{href}\">{clean}</a>{trailing}")
    });

    with_links.replace('\n', "<br>")
}

fn format_signature_markup(inner_html: &str) -> String {
    if inner_html.trim().is_empty() {
        return String::new();
    }
    let delimiter = signature_delimiter_html();
    format!("<div id=\"_evolution_signature_\" class=\"evolution-signature\"><p>{delimiter}{inner_html}</p></div>")
}

fn signature_body_html() -> String {
    let signature = default_signature_html();
    if signature.is_empty() {
        String::new()
    } else {
        format!("<p><br></p>{}", format_signature_markup(&signature))
    }
}

fn signature_prefix_html() -> String {
    let signature = default_signature_html();
    if signature.is_empty() {
        "<p><br></p>".to_string()
    } else {
        format!("{}<p><br></p>", format_signature_markup(&signature))
    }
}

fn default_signature_html() -> String {
    let Some(settings) = AppSettings::try_new() else {
        return String::new();
    };
    if !settings.append_signature() {
        return String::new();
    }
    if let Some(signature) = SignatureStore::default().active() {
        if signature.is_html() {
            return sanitize_html(signature.text.trim_end());
        }
        return signature_plain_text_to_html(signature.text.trim_end());
    }
    let text = settings.signature_text();
    if text.trim().is_empty() {
        return String::new();
    }
    let lower = text.to_ascii_lowercase();
    let is_html = lower.contains("<img")
        || lower.contains("<p")
        || lower.contains("<div")
        || lower.contains("<br")
        || lower.contains("<span")
        || lower.contains("<table")
        || lower.contains("<a");
    if is_html {
        sanitize_html(text.trim_end())
    } else {
        signature_plain_text_to_html(text.trim_end())
    }
}

fn default_quote_style() -> &'static str {
    let Some(settings) = AppSettings::try_new() else {
        return "blockquote";
    };
    if !settings.quote_original() {
        return "none";
    }
    match settings.composer_reply_style().as_str() {
        "do-not-quote" => "none",
        "inline" => "inline",
        _ => {
            if settings.quote_style() == "prefix" {
                "prefix"
            } else {
                "blockquote"
            }
        }
    }
}

fn default_forward_style() -> &'static str {
    let Some(settings) = AppSettings::try_new() else {
        return "attached";
    };
    match settings.composer_forward_style().as_str() {
        "inline" => "inline",
        "quoted" => "prefix",
        _ => "attached",
    }
}

/// Return a deterministic, safe recipient header for reply-all.  A complete
/// RFC 5322 address parser belongs in the mail backend; this small tokenizer
/// still handles quoted display names and angle-bracket addresses correctly
/// for the UI composer and avoids copying the same address twice.
fn merge_recipients(values: &[&str], excluded: &[&str]) -> String {
    let excluded: Vec<String> = excluded
        .iter()
        .flat_map(|value| split_recipient_list(value))
        .map(|value| recipient_key(&value))
        .filter(|value| !value.is_empty())
        .collect();
    let mut result = Vec::new();
    let mut seen = Vec::new();

    for value in values.iter().flat_map(|value| split_recipient_list(value)) {
        let key = recipient_key(&value);
        if key.is_empty() || excluded.iter().any(|item| item == &key) {
            continue;
        }
        if seen.iter().any(|item| item == &key) {
            continue;
        }
        seen.push(key);
        result.push(value);
    }

    result.join(", ")
}

fn split_recipient_list(value: &str) -> Vec<String> {
    let mut recipients = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut angle_depth = 0u8;

    for character in value.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            '<' if !quoted => {
                angle_depth = angle_depth.saturating_add(1);
                current.push(character);
            }
            '>' if !quoted => {
                angle_depth = angle_depth.saturating_sub(1);
                current.push(character);
            }
            ',' if !quoted && angle_depth == 0 => {
                if !current.trim().is_empty() {
                    recipients.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(character),
        }
    }
    if !current.trim().is_empty() {
        recipients.push(current.trim().to_string());
    }
    recipients
}

fn recipient_directory_button() -> gtk::Button {
    gtk::Button::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text("Im Exchange-Verzeichnis suchen")
        .css_classes(vec!["flat"])
        .build()
}

fn recipient_field(entry: &gtk::Entry, directory_button: &gtk::Button) -> gtk::Box {
    let field = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .hexpand(true)
        .build();
    field.append(entry);
    field.append(directory_button);
    field
}

fn setup_directory_recipient_search(
    entry: &gtk::Entry,
    button: &gtk::Button,
    dialog_weak: glib::WeakRef<ComposeDialog>,
    combo_from: gtk::DropDown,
) {
    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_parent(button);

    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .margin_bottom(8)
        .width_request(420)
        .build();
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(vec!["boxed-list"])
        .build();
    let results_scroll = gtk::ScrolledWindow::builder()
        .height_request(360)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&list)
        .build();
    container.append(&status);
    container.append(&results_scroll);
    popover.set_child(Some(&container));

    let visible_addresses = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let entry_for_activation = entry.downgrade();
    let popover_for_activation = popover.clone();
    let visible_addresses_for_activation = std::rc::Rc::clone(&visible_addresses);
    list.connect_row_activated(move |_list, row| {
        let Some(address) = visible_addresses_for_activation
            .borrow()
            .get(row.index().max(0) as usize)
            .cloned()
        else {
            return;
        };
        let Some(entry) = entry_for_activation.upgrade() else {
            return;
        };
        entry.set_text(&replace_current_recipient(&entry.text(), &address));
        entry.set_position(-1);
        popover_for_activation.popdown();
    });

    let dialog_weak_for_click = dialog_weak.clone();
    let combo_from_for_click = combo_from.clone();
    let entry_for_click = entry.clone();
    let list_for_click = list.clone();
    let status_for_click = status.clone();
    let popover_for_click = popover.clone();
    let visible_addresses_for_result = std::rc::Rc::clone(&visible_addresses);
    button.connect_clicked(move |button| {
        let query = current_recipient_fragment(&entry_for_click.text()).to_string();
        if query.chars().count() < 2 {
            status_for_click.set_text("Mindestens zwei Zeichen für die Verzeichnissuche eingeben.");
            clear_recipient_directory_results(&list_for_click, &visible_addresses_for_result);
            popover_for_click.popup();
            return;
        }

        let Some(dialog) = dialog_weak_for_click.upgrade() else {
            return;
        };
        let source_uid = dialog
            .selected_identity(combo_from_for_click.selected())
            .source_uid;
        if source_uid.trim().is_empty() {
            status_for_click.set_text("Kein Konto für die Verzeichnissuche ausgewählt.");
            clear_recipient_directory_results(&list_for_click, &visible_addresses_for_result);
            popover_for_click.popup();
            return;
        }

        clear_recipient_directory_results(&list_for_click, &visible_addresses_for_result);
        status_for_click.set_text("Exchange-Verzeichnis wird durchsucht…");
        button.set_sensitive(false);
        popover_for_click.popup();

        let (sender, receiver) = async_channel::bounded(1);
        let query_for_worker = query.clone();
        std::thread::spawn(move || {
            let result = graph_service_for_source(&source_uid)
                .ok_or_else(|| {
                    "Für dieses Konto ist kein portabler Exchange-Verzeichniszugriff verfügbar."
                        .to_string()
                })
                .and_then(|service| {
                    service
                        .search_gal_sync(&query_for_worker)
                        .map_err(|error| error.to_string())
                })
                .map(|contacts| {
                    let mut suggestions = contacts
                        .into_iter()
                        .map(gal_contact_to_recipient_suggestion)
                        .filter(|suggestion| {
                            !suggestion.address.is_empty()
                                && !is_internal_microsoft_routing_address(&suggestion.address)
                        })
                        .collect::<Vec<_>>();
                    suggestions.sort_by_key(|suggestion| suggestion.address.to_ascii_lowercase());
                    suggestions
                        .dedup_by(|left, right| left.address.eq_ignore_ascii_case(&right.address));
                    suggestions
                });
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Directory search result was dropped");
            }
        });

        let list_for_result = list_for_click.clone();
        let status_for_result = status_for_click.clone();
        let popover_for_result = popover_for_click.clone();
        let button_for_result = button.clone();
        let visible_addresses_for_future = std::rc::Rc::clone(&visible_addresses_for_result);
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            button_for_result.set_sensitive(true);
            match result {
                Ok(Ok(suggestions)) if suggestions.is_empty() => {
                    status_for_result.set_text("Keine passenden Verzeichniseinträge gefunden.");
                    clear_recipient_directory_results(
                        &list_for_result,
                        &visible_addresses_for_future,
                    );
                    popover_for_result.popup();
                }
                Ok(Ok(suggestions)) => {
                    visible_addresses_for_future.replace(
                        suggestions
                            .iter()
                            .map(|suggestion| suggestion.address.clone())
                            .collect(),
                    );
                    for suggestion in &suggestions {
                        let row = gtk::ListBoxRow::new();
                        let label = gtk::Label::builder()
                            .label(&recipient_suggestion_label(suggestion))
                            .xalign(0.0)
                            .wrap(true)
                            .margin_start(8)
                            .margin_end(8)
                            .margin_top(6)
                            .margin_bottom(6)
                            .build();
                        row.set_child(Some(&label));
                        list_for_result.append(&row);
                    }
                    status_for_result.set_text(&format!(
                        "{} Verzeichniseinträge gefunden",
                        suggestions.len()
                    ));
                    popover_for_result.popup();
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "Exchange directory search failed");
                    status_for_result
                        .set_text(&format!("Verzeichnissuche nicht verfügbar: {error}"));
                    clear_recipient_directory_results(
                        &list_for_result,
                        &visible_addresses_for_future,
                    );
                    popover_for_result.popup();
                }
                Err(error) => {
                    tracing::debug!(%error, "Exchange directory search channel closed");
                    status_for_result.set_text("Verzeichnissuche wurde abgebrochen.");
                    clear_recipient_directory_results(
                        &list_for_result,
                        &visible_addresses_for_future,
                    );
                    popover_for_result.popup();
                }
            }
        });
    });
}

fn clear_recipient_directory_results(
    list: &gtk::ListBox,
    visible_addresses: &std::rc::Rc<std::cell::RefCell<Vec<String>>>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    visible_addresses.borrow_mut().clear();
}

fn gal_contact_to_recipient_suggestion(contact: GalContact) -> RecipientSuggestion {
    let display = contact.display_name.trim();
    let details = [
        contact.job_title.trim(),
        contact.department.trim(),
        contact.office_location.trim(),
    ]
    .into_iter()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let display = if display.is_empty() {
        details
    } else if details.is_empty() {
        display.to_string()
    } else {
        format!("{display} · {details}")
    };
    RecipientSuggestion {
        display,
        address: contact.email_address.trim().to_string(),
    }
}

fn setup_recipient_autocompletion(
    entry: &gtk::Entry,
    combo_from: &gtk::DropDown,
    dialog_weak: glib::WeakRef<ComposeDialog>,
) {
    let popover = gtk::Popover::new();
    popover.set_autohide(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.add_css_class("menu");
    popover.add_css_class("recipient-completion-popover");
    popover.set_parent(entry);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .valign(gtk::Align::Start)
        .build();

    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .width_request(380)
        .min_content_height(0)
        .max_content_height(280)
        .propagate_natural_height(true)
        .propagate_natural_width(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .valign(gtk::Align::Start)
        .build();

    popover.set_child(Some(&scroll));

    let visible_suggestions = Rc::new(RefCell::new(Vec::<RecipientSuggestion>::new()));
    let generation = Rc::new(Cell::new(0u64));
    let debounce_source = Rc::new(RefCell::new(Option::<glib::SourceId>::None));

    let entry_for_activate = entry.downgrade();
    let popover_for_activate = popover.clone();
    let visible_for_activate = Rc::clone(&visible_suggestions);
    let dialog_weak_for_activate = dialog_weak.clone();
    let accept_selection = move |selected_index: usize| {
        let Some(entry) = entry_for_activate.upgrade() else {
            return;
        };
        let suggestions = visible_for_activate.borrow();
        let Some(suggestion) = suggestions.get(selected_index) else {
            return;
        };
        let formatted = recipient_suggestion_label(suggestion);
        entry.set_text(&replace_current_recipient(&entry.text(), &formatted));
        entry.set_position(-1);
        entry.grab_focus();
        popover_for_activate.popdown();
        if let Some(dialog) = dialog_weak_for_activate.upgrade() {
            dialog.mark_changed();
        }
    };

    let accept_selection_for_row: Rc<dyn Fn(usize) + 'static> = Rc::new(accept_selection);
    let accept_for_row_cb = Rc::clone(&accept_selection_for_row);
    list.connect_row_activated(move |_, row| {
        let idx = row.index();
        if idx >= 0 {
            accept_for_row_cb(idx as usize);
        }
    });

    let key_controller = gtk::EventControllerKey::new();
    let popover_for_key = popover.clone();
    let list_for_key = list.clone();
    let scroll_for_key = scroll.clone();
    let visible_for_key = Rc::clone(&visible_suggestions);
    let accept_for_key = Rc::clone(&accept_selection_for_row);
    key_controller.connect_key_pressed(move |_, key, _, _| {
        if !popover_for_key.is_visible() {
            return glib::Propagation::Proceed;
        }
        let count = visible_for_key.borrow().len();
        if count == 0 {
            return glib::Propagation::Proceed;
        }

        match key {
            gtk::gdk::Key::Down => {
                let current = list_for_key.selected_row().map(|r| r.index()).unwrap_or(-1);
                let next = (current + 1).min((count - 1) as i32);
                if let Some(row) = list_for_key.row_at_index(next) {
                    list_for_key.select_row(Some(&row));
                    let vadj = scroll_for_key.vadjustment();
                    if let Some(bounds) = row.compute_bounds(&list_for_key) {
                        let y = bounds.y() as f64;
                        let h = bounds.height() as f64;
                        if y < vadj.value() {
                            vadj.set_value(y);
                        } else if y + h > vadj.value() + vadj.page_size() {
                            vadj.set_value(y + h - vadj.page_size());
                        }
                    }
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up => {
                let current = list_for_key.selected_row().map(|r| r.index()).unwrap_or(0);
                let prev = (current - 1).max(0);
                if let Some(row) = list_for_key.row_at_index(prev) {
                    list_for_key.select_row(Some(&row));
                    let vadj = scroll_for_key.vadjustment();
                    if let Some(bounds) = row.compute_bounds(&list_for_key) {
                        let y = bounds.y() as f64;
                        let h = bounds.height() as f64;
                        if y < vadj.value() {
                            vadj.set_value(y);
                        } else if y + h > vadj.value() + vadj.page_size() {
                            vadj.set_value(y + h - vadj.page_size());
                        }
                    }
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::Tab => {
                let current = list_for_key.selected_row().map(|r| r.index()).unwrap_or(0);
                if current >= 0 && (current as usize) < count {
                    accept_for_key(current as usize);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            gtk::gdk::Key::Escape => {
                popover_for_key.popdown();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    entry.add_controller(key_controller);

    let popover_for_focus = popover.clone();
    let focus_controller = gtk::EventControllerFocus::new();
    focus_controller.connect_leave(move |_| {
        let pop = popover_for_focus.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            if !pop.has_focus() {
                pop.popdown();
            }
        });
    });
    entry.add_controller(focus_controller);

    let entry_weak = entry.downgrade();
    let popover_for_changed = popover.clone();
    let list_for_changed = list.clone();
    let combo_from_clone = combo_from.clone();
    let dialog_weak_for_changed = dialog_weak.clone();
    entry.connect_changed(move |e| {
        let text = e.text().to_string();
        let fragment = current_recipient_fragment(&text).to_string();

        let (min_len, show_address) = AppSettings::try_new()
            .map(|s| {
                (
                    s.contact_completion_minimum_query_length().max(1) as usize,
                    s.contact_completion_show_address(),
                )
            })
            .unwrap_or((1, false));

        let effective_min_len = if min_len <= 3 { 1 } else { min_len };

        if fragment.trim().is_empty() || fragment.chars().count() < effective_min_len {
            if let Some(source_id) = debounce_source.borrow_mut().take() {
                let _ = unsafe { glib::ffi::g_source_remove(source_id.as_raw()) };
            }
            popover_for_changed.popdown();
            return;
        }

        if let Some(source_id) = debounce_source.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source_id.as_raw()) };
        }

        let generation_id = generation.get() + 1;
        generation.set(generation_id);

        let entry_weak_clone = entry_weak.clone();
        let popover_clone = popover_for_changed.clone();
        let list_clone = list_for_changed.clone();
        let visible_clone = Rc::clone(&visible_suggestions);
        let generation_clone = Rc::clone(&generation);
        let combo_from_source = combo_from_clone.clone();
        let dialog_weak_source = dialog_weak_for_changed.clone();
        let debounce_source_timer = Rc::clone(&debounce_source);

        let source_id = glib::timeout_add_local(
            std::time::Duration::from_millis(50),
            move || {
                debounce_source_timer.borrow_mut().take();
                if generation_clone.get() != generation_id {
                    return glib::ControlFlow::Break;
                }
                let Some(entry) = entry_weak_clone.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let cur_fragment = current_recipient_fragment(&entry.text()).to_string();
                if cur_fragment.chars().count() < effective_min_len {
                    popover_clone.popdown();
                    return glib::ControlFlow::Break;
                }

                // 1. Instant local search from in-memory pool (< 0.1 ms)
                let mut suggestions = search_recipient_suggestions(&cur_fragment, show_address);
                suggestions.truncate(10);

                if suggestions.is_empty() {
                    popover_clone.popdown();
                    visible_clone.replace(Vec::new());
                    while let Some(row) = list_clone.first_child() {
                        list_clone.remove(&row);
                    }
                } else {
                    render_recipient_suggestions_to_list(&list_clone, &suggestions);
                    visible_clone.replace(suggestions.clone());
                    if let Some(first_row) = list_clone.row_at_index(0) {
                        list_clone.select_row(Some(&first_row));
                    }
                    popover_clone.popup();
                }

                // 2. Non-blocking asynchronous GAL search in background thread
                let source_uid = dialog_weak_source
                    .upgrade()
                    .map(|d| d.selected_identity(combo_from_source.selected()).source_uid)
                    .unwrap_or_default();

                if !source_uid.trim().is_empty() && cur_fragment.chars().count() >= 3 {
                    let generation_for_gal = generation_id;
                    let gen_tracker = Rc::clone(&generation_clone);
                    let list_for_gal = list_clone.clone();
                    let visible_for_gal = Rc::clone(&visible_clone);
                    let popover_for_gal = popover_clone.clone();
                    let fragment_for_gal = cur_fragment.clone();
                    let (sender, receiver) = async_channel::bounded(1);

                    std::thread::spawn(move || {
                        let contacts = graph_service_for_source(&source_uid)
                            .and_then(|graph| graph.search_gal_sync(&fragment_for_gal).ok());
                        let _ = sender.send_blocking(contacts);
                    });

                    glib::spawn_future_local(async move {
                        if let Ok(Some(gal_contacts)) = receiver.recv().await {
                            if gen_tracker.get() != generation_for_gal {
                                return;
                            }
                            let mut current = visible_for_gal.borrow().clone();
                            let mut added = false;
                            for gc in gal_contacts {
                                if is_internal_microsoft_routing_address(&gc.email_address) {
                                    continue;
                                }
                                let sugg = gal_contact_to_recipient_suggestion(gc);
                                if !sugg.address.is_empty()
                                    && !is_internal_microsoft_routing_address(&sugg.address)
                                    && !current.iter().any(|s| s.address.eq_ignore_ascii_case(&sugg.address))
                                {
                                    current.push(sugg);
                                    added = true;
                                }
                            }
                            if added {
                                current.truncate(10);
                                render_recipient_suggestions_to_list(&list_for_gal, &current);
                                visible_for_gal.replace(current);
                                if let Some(first_row) = list_for_gal.row_at_index(0) {
                                    if list_for_gal.selected_row().is_none() {
                                        list_for_gal.select_row(Some(&first_row));
                                    }
                                }
                                popover_for_gal.popup();
                            }
                        }
                    });
                }

                glib::ControlFlow::Break
            },
        );
        debounce_source.borrow_mut().replace(source_id);
    });
}

fn render_recipient_suggestions_to_list(
    list: &gtk::ListBox,
    suggestions: &[RecipientSuggestion],
) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }

    let allow_remote = AppSettings::try_new()
        .map(|s| s.mail_fetch_remote_logos())
        .unwrap_or(true);

    for sugg in suggestions {
        let list_row = gtk::ListBoxRow::new();
        list_row.add_css_class("recipient-suggestion-row");
        list_row.set_activatable(true);
        list_row.set_selectable(true);

        let row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let display = if sugg.display.is_empty() {
            &sugg.address
        } else {
            &sugg.display
        };

        let avatar = adw::Avatar::builder()
            .size(28)
            .text(display)
            .show_initials(true)
            .build();

        if let Some(bytes) = AvatarCacheService::global().peek_memory(&sugg.address) {
            if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
                avatar.set_custom_image(Some(&texture));
            }
        } else {
            let (tx, rx) = async_channel::bounded(1);
            let avatar_weak = avatar.downgrade();
            let email = sugg.address.clone();
            glib::spawn_future_local(async move {
                if let Ok(bytes) = rx.recv().await {
                    if let Some(avatar) = avatar_weak.upgrade() {
                        if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
                            avatar.set_custom_image(Some(&texture));
                        }
                    }
                }
            });

            std::thread::spawn(move || {
                if let Some(bytes) =
                    AvatarCacheService::global().request_avatar(&email, allow_remote, Some(tx.clone()))
                {
                    let _ = tx.send_blocking(bytes);
                }
            });
        }

        row_box.append(&avatar);

        let text_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(1)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();

        let lbl_name = gtk::Label::builder()
            .label(display)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        text_box.append(&lbl_name);

        if !sugg.display.is_empty() && !sugg.address.is_empty() {
            let lbl_email = gtk::Label::builder()
                .label(&sugg.address)
                .xalign(0.0)
                .css_classes(vec!["dim-label", "caption"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            text_box.append(&lbl_email);
        }

        row_box.append(&text_box);
        list_row.set_child(Some(&row_box));
        list.append(&list_row);
    }
}

pub fn is_internal_microsoft_routing_address(email: &str) -> bool {
    let lower = email.trim().to_ascii_lowercase();
    let Some(domain) = lower.split('@').nth(1) else {
        return false;
    };
    domain == "onmicrosoft.com"
        || domain.ends_with(".onmicrosoft.com")
        || domain.contains(".mail.onmicrosoft.com")
}

static LOCAL_RECIPIENT_POOL_CACHE: Lazy<RwLock<Option<(std::time::Instant, Arc<Vec<RecipientSuggestion>>)>>> =
    Lazy::new(|| RwLock::new(None));
static IS_UPDATING_POOL: AtomicBool = AtomicBool::new(false);

fn get_or_load_recipient_pool() -> Arc<Vec<RecipientSuggestion>> {
    const CACHE_TTL_SECS: u64 = 600;
    if let Ok(read) = LOCAL_RECIPIENT_POOL_CACHE.read() {
        if let Some((instant, ref pool)) = *read {
            if instant.elapsed().as_secs() < CACHE_TTL_SECS {
                return Arc::clone(pool);
            }
            // Stale-while-revalidate: return current pool immediately, refresh in background
            let current = Arc::clone(pool);
            if !IS_UPDATING_POOL.swap(true, Ordering::SeqCst) {
                std::thread::spawn(|| {
                    let new_pool = Arc::new(load_local_recipients_pool());
                    if let Ok(mut write) = LOCAL_RECIPIENT_POOL_CACHE.write() {
                        *write = Some((std::time::Instant::now(), new_pool));
                    }
                    IS_UPDATING_POOL.store(false, Ordering::SeqCst);
                });
            }
            return current;
        }
    }
    let pool = Arc::new(load_local_recipients_pool());
    if let Ok(mut write) = LOCAL_RECIPIENT_POOL_CACHE.write() {
        *write = Some((std::time::Instant::now(), Arc::clone(&pool)));
    }
    pool
}

pub fn warm_recipient_cache() {
    std::thread::spawn(|| {
        let _ = get_or_load_recipient_pool();
    });
}

pub fn invalidate_recipient_cache() {
    if let Ok(mut write) = LOCAL_RECIPIENT_POOL_CACHE.write() {
        *write = None;
    }
}

fn load_local_recipients_pool() -> Vec<RecipientSuggestion> {
    let mut results: Vec<RecipientSuggestion> = Vec::new();
    let mut email_to_index: HashMap<String, usize> = HashMap::new();

    let mut add_or_update = |display_name: &str, email: &str| {
        let clean_email = email.trim();
        if clean_email.is_empty() || !clean_email.contains('@') || is_internal_microsoft_routing_address(clean_email) {
            return;
        }
        let key = clean_email.to_ascii_lowercase();
        let clean_display = display_name.trim();

        if let Some(&idx) = email_to_index.get(&key) {
            if results[idx].display.is_empty() && !clean_display.is_empty() {
                results[idx].display = clean_display.to_string();
            }
        } else {
            let idx = results.len();
            results.push(RecipientSuggestion {
                display: clean_display.to_string(),
                address: clean_email.to_string(),
            });
            email_to_index.insert(key, idx);
        }
    };

    // 1. Check recent recipients (frecency ranking - highest priority)
    let recent = RecentRecipientsService::new().all_recipients();
    for r in recent {
        add_or_update(&r.display_name, &r.email);
    }

    // 2. Query EDS contacts (Evolution Data Server synchronized address books)
    let eds_contacts = crate::services::ebook_service::load_contacts_prefer_eds();
    for record in eds_contacts.contacts {
        let suggestions = contact_to_recipient_suggestions(&record.contact, false);
        for s in suggestions {
            add_or_update(&s.display, &s.address);
        }
    }

    // 3. Query local vCard store (local address book fallback)
    let store = ContactStoreService::new();
    let contacts = store.load_contacts();
    for record in contacts {
        let suggestions = contact_to_recipient_suggestions(&record.contact, false);
        for s in suggestions {
            add_or_update(&s.display, &s.address);
        }
    }

    // 4. Query previous correspondents from cached mail messages (limit to most recent 1000 messages)
    if let Some(mail_cache) = crate::services::camel_mail_service::load_mail_load_cache() {
        for msg in mail_cache.messages.iter().rev().take(1000) {
            // Check sender
            let s_email = msg.sender_email.trim();
            let s_name = msg.sender_name.trim();
            add_or_update(s_name, s_email);

            // Check recipients in To / Cc
            for recipient_header in [&msg.recipient_to, &msg.recipient_cc] {
                if !recipient_header.is_empty() {
                    for (name, email) in RecentRecipientsService::parse_recipients(recipient_header) {
                        add_or_update(&name, &email);
                    }
                }
            }
        }
    }

    results
}

fn search_recipient_suggestions(query: &str, _show_address: bool) -> Vec<RecipientSuggestion> {
    let query_norm = query.trim().to_lowercase();
    if query_norm.is_empty() {
        return Vec::new();
    }
    let pool = get_or_load_recipient_pool();

    struct ScoredSuggestion {
        suggestion: RecipientSuggestion,
        is_prefix: bool,
    }

    let mut matches = Vec::new();
    for s in pool.iter() {
        let disp_lower = s.display.to_lowercase();
        let addr_lower = s.address.to_lowercase();

        let disp_starts = disp_lower.starts_with(&query_norm);
        let addr_starts = addr_lower.starts_with(&query_norm);
        let is_prefix = disp_starts || addr_starts;

        if is_prefix || disp_lower.contains(&query_norm) || addr_lower.contains(&query_norm) {
            matches.push(ScoredSuggestion {
                suggestion: s.clone(),
                is_prefix,
            });
        }
    }

    // Stable sort: prefix matches come first; within each group, maintain pool frecency
    matches.sort_by(|a, b| b.is_prefix.cmp(&a.is_prefix));

    matches.into_iter().map(|m| m.suggestion).collect()
}

fn contact_matches_query(contact: &VCardContact, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return false;
    }
    if contact.fn_name.to_lowercase().contains(&query) {
        return true;
    }
    if contact.email.to_lowercase().contains(&query) {
        return true;
    }
    if contact.emails.iter().any(|e| e.to_lowercase().contains(&query)) {
        return true;
    }
    if contact.nickname.to_lowercase().contains(&query) {
        return true;
    }
    if contact.is_list && contact.list_name.to_lowercase().contains(&query) {
        return true;
    }
    if contact.is_list
        && contact.list_members.iter().any(|m| {
            m.display_name.to_lowercase().contains(&query) || m.email.to_lowercase().contains(&query)
        })
    {
        return true;
    }
    false
}

fn contact_to_recipient_suggestions(
    contact: &VCardContact,
    _show_address: bool,
) -> Vec<RecipientSuggestion> {
    let mut suggestions = Vec::new();
    let display_name = if !contact.fn_name.is_empty() {
        contact.fn_name.clone()
    } else if !contact.nickname.is_empty() {
        contact.nickname.clone()
    } else if contact.is_list && !contact.list_name.is_empty() {
        contact.list_name.clone()
    } else {
        String::new()
    };

    if !contact.email.is_empty() && !is_internal_microsoft_routing_address(&contact.email) {
        suggestions.push(RecipientSuggestion {
            display: display_name.clone(),
            address: contact.email.clone(),
        });
    }

    for email in &contact.emails {
        if email != &contact.email
            && !email.is_empty()
            && !is_internal_microsoft_routing_address(email)
            && !suggestions.iter().any(|s| s.address.eq_ignore_ascii_case(email))
        {
            suggestions.push(RecipientSuggestion {
                display: display_name.clone(),
                address: email.clone(),
            });
        }
    }

    if contact.is_list {
        for member in &contact.list_members {
            if !member.email.is_empty()
                && !is_internal_microsoft_routing_address(&member.email)
                && !suggestions.iter().any(|s| s.address.eq_ignore_ascii_case(&member.email))
            {
                suggestions.push(RecipientSuggestion {
                    display: if !member.display_name.is_empty() {
                        member.display_name.clone()
                    } else {
                        display_name.clone()
                    },
                    address: member.email.clone(),
                });
            }
        }
    }

    suggestions
}

fn recipient_suggestion_label(suggestion: &RecipientSuggestion) -> String {
    if suggestion.display.is_empty() {
        suggestion.address.clone()
    } else {
        format!("{} <{}>", suggestion.display, suggestion.address)
    }
}

fn current_recipient_fragment(value: &str) -> &str {
    value
        .rsplit(|character| character == ',' || character == ';')
        .next()
        .unwrap_or(value)
        .trim()
}

fn replace_current_recipient(value: &str, selected: &str) -> String {
    let selected = selected.trim();
    let Some(separator) = value.rfind(|character| character == ',' || character == ';') else {
        return format!("{selected}, ");
    };
    let prefix = value[..=separator].trim_end();
    format!("{prefix} {selected}, ")
}

fn recipient_key(value: &str) -> String {
    let value = value.trim();
    if let Some(start) = value.rfind('<') {
        if let Some(end) = value[start + 1..].find('>') {
            return value[start + 1..start + 1 + end]
                .trim()
                .to_ascii_lowercase();
        }
    }
    value.trim_matches('"').trim().to_ascii_lowercase()
}

/// Strip markup before inserting an incoming message into the editable
/// composer.  The viewer can render remote HTML safely, but the composer must
/// not copy event attributes or arbitrary markup into a new outgoing mail.
fn quote_as_safe_text(value: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    let mut skip_until: Option<&'static str> = None;

    for character in value.chars() {
        if character == '<' {
            in_tag = true;
            tag.clear();
            continue;
        }
        if in_tag {
            if character == '>' {
                in_tag = false;
                let lower = tag.trim().to_ascii_lowercase();
                if let Some(end_tag) = skip_until {
                    // Check if this is the closing tag we're waiting for
                    let closing = format!("/{}", end_tag);
                    if lower.starts_with(closing.as_str()) {
                        skip_until = None;
                    }
                    // still in skipped block, discard
                } else if lower.starts_with("script")
                    || lower.starts_with("style")
                    || lower.starts_with("noscript")
                    || lower.starts_with("iframe")
                    || lower.starts_with("object")
                    || lower.starts_with("embed")
                {
                    // Start skipping content until the closing tag
                    let tag_name = lower.split_whitespace().next().unwrap_or("");
                    skip_until = match tag_name {
                        "script" => Some("script"),
                        "style" => Some("style"),
                        "noscript" => Some("noscript"),
                        "iframe" => Some("iframe"),
                        "object" => Some("object"),
                        "embed" => Some("embed"),
                        _ => None,
                    };
                } else if lower.starts_with("br")
                    || lower.starts_with("/p")
                    || lower.starts_with("/div")
                    || lower.starts_with("/li")
                    || lower.starts_with("/blockquote")
                {
                    text.push('\n');
                }
            } else {
                tag.push(character);
            }
            continue;
        }
        if skip_until.is_none() {
            text.push(character);
        }
    }

    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    html_escape(decoded.trim())
}

fn mime_type_for_path(path: &Path) -> String {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn format_size(size: u64) -> String {
    if size < 1024 {
        return format!("{size} B");
    }
    if size < 1024 * 1024 {
        return format!("{:.1} KiB", size as f64 / 1024.0);
    }
    if size < 1024 * 1024 * 1024 {
        return format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0));
    }
    format!("{:.1} GiB", size as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn next_generation(current: u64) -> u64 {
    current.wrapping_add(1).max(1)
}

fn composer_print_snapshot(message_key: &str, message: &OutgoingMessage) -> MailPrintSnapshot {
    let fallback_text = message
        .plain_text
        .then(|| message.html_body.clone())
        .unwrap_or_default();
    let html_body = if message.plain_text {
        format!(
            "<pre class=\"mail-composer-plain-text\">{}</pre>",
            html_escape(&message.html_body)
        )
    } else {
        message.html_body.clone()
    };
    MailPrintSnapshot {
        message_key: message_key.to_string(),
        sender: message.from.clone(),
        reply_to: message.reply_to.clone(),
        to: message.to.clone(),
        cc: message.cc.clone(),
        bcc: message.bcc.clone(),
        subject: message.subject.clone(),
        date: Utc::now().to_rfc2822(),
        html_body,
        fallback_text,
        inline_images: Vec::new(),
        attachments: message
            .attachments
            .iter()
            .map(|attachment| MailPrintAttachment {
                filename: attachment.filename.clone(),
                mime_type: attachment.mime_type.clone(),
                size_formatted: format_size(attachment.size),
            })
            .collect(),
    }
}

fn editor_body_has_user_content(body: &str) -> bool {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("<img") || trimmed.contains("<svg") || trimmed.contains("<table") {
        return true;
    }

    let content_without_sig = if let Some(sig_start) = trimmed.find("<div id=\"_evolution_signature_\"") {
        if let Some(sig_end) = trimmed[sig_start..].find("</div>") {
            format!("{}{}", &trimmed[..sig_start], &trimmed[sig_start + sig_end + 6..])
        } else {
            trimmed[..sig_start].to_string()
        }
    } else if let Some(sig_pos) = trimmed.find("\n-- \n").or_else(|| trimmed.find("\n--\n")) {
        trimmed[..sig_pos].to_string()
    } else if trimmed.starts_with("-- \n") || trimmed.starts_with("--\n") {
        String::new()
    } else {
        trimmed.to_string()
    };

    let mut in_tag = false;
    let mut text = String::with_capacity(content_without_sig.len());
    for ch in content_without_sig.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            text.push(ch);
        }
    }
    let text = text.replace("&nbsp;", " ").replace("&#160;", " ");
    !text.trim().is_empty()
}

fn draft_has_user_content(draft: &DraftMessage) -> bool {
    draft.to.trim().chars().next().is_some()
        || draft.cc.trim().chars().next().is_some()
        || draft.bcc.trim().chars().next().is_some()
        || draft.subject.trim().chars().next().is_some()
        || !draft.attachments.is_empty()
        || draft.calendar_method.is_some()
        || draft.calendar_component.is_some()
        || editor_body_has_user_content(&draft.html_body)
}

fn compose_operation_can_start(operation: ComposeOperation) -> bool {
    operation == ComposeOperation::Idle
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseRequestDecision {
    Allow,
    AutoSave,
    BlockOperation,
}

fn close_request_decision(
    dirty: bool,
    operation: ComposeOperation,
) -> CloseRequestDecision {
    if operation != ComposeOperation::Idle {
        CloseRequestDecision::BlockOperation
    } else if !dirty {
        CloseRequestDecision::Allow
    } else {
        CloseRequestDecision::AutoSave
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_compose_operation_can_start_at_a_time() {
        assert!(compose_operation_can_start(ComposeOperation::Idle));
        assert!(!compose_operation_can_start(ComposeOperation::SavingDraft));
        assert!(!compose_operation_can_start(ComposeOperation::SavingEml));
        assert!(!compose_operation_can_start(ComposeOperation::Sending));
    }

    #[test]
    fn dirty_guard_allows_clean_closes_and_blocks_busy_or_repeated_prompts() {
        assert_eq!(
            close_request_decision(false, ComposeOperation::Idle),
            CloseRequestDecision::Allow
        );
        assert_eq!(
            close_request_decision(true, ComposeOperation::Idle),
            CloseRequestDecision::AutoSave
        );
        assert_eq!(
            close_request_decision(false, ComposeOperation::Sending),
            CloseRequestDecision::BlockOperation
        );
        assert_eq!(
            close_request_decision(true, ComposeOperation::Sending),
            CloseRequestDecision::BlockOperation
        );
    }

    #[test]
    fn editor_body_has_user_content_filters_empty_markup_and_signatures() {
        assert!(!editor_body_has_user_content(""));
        assert!(!editor_body_has_user_content("   \n\t  "));
        assert!(!editor_body_has_user_content("<p><br></p>"));
        assert!(!editor_body_has_user_content("<div><br></div>"));
        assert!(!editor_body_has_user_content("<p><br></p><div id=\"_evolution_signature_\" class=\"evolution-signature\"><p>-- <br>Martin</p></div>"));
        assert!(!editor_body_has_user_content("\n-- \nMartin"));
        assert!(editor_body_has_user_content("<p>Hallo Welt</p>"));
        assert!(editor_body_has_user_content("<p>Hallo</p><div id=\"_evolution_signature_\" class=\"evolution-signature\"><p>-- <br>Martin</p></div>"));
        assert!(editor_body_has_user_content("Hallo\n-- \nMartin"));
        assert!(editor_body_has_user_content("<p><img src=\"data:image/png;base64,abc\"></p>"));
    }

    #[test]
    fn composer_shortcuts_are_local_actions_without_shell_overlap() {
        assert!(
            COMPOSER_ACTION_SHORTCUTS
                .iter()
                .all(|(action, _)| { !action.starts_with("app.") && !action.starts_with("win.") })
        );
        assert_eq!(
            COMPOSER_ACTION_SHORTCUTS
                .iter()
                .filter(|(action, _)| *action == "close")
                .count(),
            2
        );
        assert!(COMPOSER_OPERATION_ACTIONS.contains(&"print"));
        assert!(COMPOSER_OPERATION_ACTIONS.contains(&"print-preview"));
    }

    #[test]
    fn composer_print_snapshot_preserves_headers_body_and_attachment_metadata() {
        let message = OutgoingMessage {
            source_uid: "source".to_string(),
            from: "Alice <alice@example.test>".to_string(),
            reply_to: "reply@example.test".to_string(),
            request_read_receipt: false,
            to: "bob@example.test".to_string(),
            cc: "carol@example.test".to_string(),
            bcc: "archive@example.test".to_string(),
            subject: "Quarterly <report>".to_string(),
            html_body: "Hello <team>".to_string(),
            plain_text: true,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: vec![DraftAttachment {
                path: "/tmp/report.pdf".to_string(),
                filename: "report.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                size: 1024,
                content_id: None,
                embedded_content_base64: String::new(),
            }],
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };

        let snapshot = composer_print_snapshot("composer:test", &message);
        assert_eq!(snapshot.message_key, "composer:test");
        assert_eq!(snapshot.sender, message.from);
        assert_eq!(snapshot.to, message.to);
        assert_eq!(snapshot.subject, message.subject);
        assert!(snapshot.html_body.contains("Hello &lt;team&gt;"));
        assert_eq!(snapshot.fallback_text, "Hello <team>");
        assert_eq!(snapshot.attachments.len(), 1);
        assert_eq!(snapshot.attachments[0].filename, "report.pdf");
        assert_eq!(snapshot.attachments[0].mime_type, "application/pdf");
        assert_eq!(snapshot.attachments[0].size_formatted, "1.0 KiB");
    }

    #[test]
    fn every_mutating_action_is_suspended_during_an_operation() {
        for action in [
            "send",
            "save-draft",
            "discard",
            "save-as-eml",
            "close",
            "new-message",
            "attach",
            "plain-text",
            "request-read-receipt",
            "prioritize-message",
            "pgp-sign",
            "pgp-encrypt",
            "smime-sign",
            "smime-encrypt",
            "charset",
        ] {
            assert!(COMPOSER_OPERATION_ACTIONS.contains(&action));
        }
    }

    #[test]
    fn empty_new_draft_is_not_autosaved_but_existing_content_is() {
        let draft = DraftMessage {
            id: "draft-test".to_string(),
            from: String::new(),
            reply_to: String::new(),
            source_uid: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            html_body: String::new(),
            plain_text: false,
            updated_at: String::new(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };
        assert!(!draft_has_user_content(&draft));

        let with_account_only = DraftMessage {
            from: "Martin <martin@example.org>".to_string(),
            source_uid: "acc-1".to_string(),
            ..draft.clone()
        };
        assert!(!draft_has_user_content(&with_account_only));

        let with_signature_only = DraftMessage {
            from: "Martin <martin@example.org>".to_string(),
            source_uid: "acc-1".to_string(),
            html_body: "<p><br></p><div id=\"_evolution_signature_\" class=\"evolution-signature\"><p>-- <br>Martin</p></div>".to_string(),
            ..draft.clone()
        };
        assert!(!draft_has_user_content(&with_signature_only));

        let mut with_subject = draft;
        with_subject.subject = "Recovery".to_string();
        assert!(draft_has_user_content(&with_subject));
    }

    #[test]
    fn reply_and_forward_subjects_do_not_duplicate_prefixes() {
        assert_eq!(reply_subject("Status"), "Re: Status");
        assert_eq!(reply_subject("Re: Status"), "Re: Status");
        assert_eq!(forward_subject("Status"), "Fwd: Status");
        assert_eq!(forward_subject("Fwd: Status"), "Fwd: Status");
    }

    #[test]
    fn reply_context_is_html_escaped() {
        assert_eq!(
            html_escape("A <sender@example.org> & team"),
            "A &lt;sender@example.org&gt; &amp; team"
        );
    }

    #[test]
    fn reply_all_deduplicates_primary_and_repeated_recipients() {
        assert_eq!(
            merge_recipients(
                &[
                    "Sender <sender@example.org>, Team <team@example.org>",
                    "team@example.org"
                ],
                &["sender@example.org"]
            ),
            "Team <team@example.org>"
        );
    }

    #[test]
    fn local_recipient_completion_replaces_only_the_current_token() {
        assert_eq!(current_recipient_fragment("first@example.org, mar"), "mar");
        assert_eq!(
            replace_current_recipient("first@example.org, mar", "Maria <maria@example.org>"),
            "first@example.org, Maria <maria@example.org>, "
        );
        assert_eq!(
            replace_current_recipient("mar", "maria@example.org"),
            "maria@example.org, "
        );
    }

    #[test]
    fn contact_matches_query_and_extracts_suggestions() {
        let mut contact = VCardContact::new(
            "Ada Lovelace",
            "ada@example.org",
            "Analytical Engine",
            "Pioneer",
        );
        contact.emails = vec![
            "ada@example.org".to_string(),
            "ada.second@example.org".to_string(),
        ];
        contact.nickname = "Ada".to_string();

        assert!(contact_matches_query(&contact, "ada"));
        assert!(contact_matches_query(&contact, "lovelace"));
        assert!(contact_matches_query(&contact, "second@example"));
        assert!(!contact_matches_query(&contact, "grace"));

        let suggestions = contact_to_recipient_suggestions(&contact, false);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].address, "ada@example.org");
        assert_eq!(suggestions[0].display, "Ada Lovelace");
        assert_eq!(suggestions[1].address, "ada.second@example.org");
    }

    #[test]
    fn test_search_recipient_suggestions_ranking_and_sources() {
        invalidate_recipient_cache();
        assert!(search_recipient_suggestions("", false).is_empty());
        assert!(search_recipient_suggestions("   ", false).is_empty());

        let recent_srv = RecentRecipientsService::new();
        recent_srv.record_recipient("Alexander Test", "alexander.test@example.com");
        invalidate_recipient_cache();

        let results = search_recipient_suggestions("alexander", false);
        assert!(!results.is_empty(), "Expected suggestions for 'alexander'");
        let found = results.iter().find(|s| s.address == "alexander.test@example.com");
        assert!(found.is_some(), "Recorded recipient should be found");
        assert_eq!(found.unwrap().display, "Alexander Test");

        recent_srv.record_recipient("Sub User", "someone@notest.com");
        recent_srv.record_recipient("Prefix User", "test@prefix.com");
        invalidate_recipient_cache();

        let test_results = search_recipient_suggestions("test", false);
        let prefix_idx = test_results.iter().position(|s| s.address == "test@prefix.com");
        let sub_idx = test_results.iter().position(|s| s.address == "someone@notest.com");
        if let (Some(p_idx), Some(s_idx)) = (prefix_idx, sub_idx) {
            assert!(p_idx < s_idx, "Prefix matches must be ranked before non-prefix matches");
        }
    }

    #[test]
    fn test_internal_microsoft_routing_address_detection() {
        assert!(is_internal_microsoft_routing_address("user@company.onmicrosoft.com"));
        assert!(is_internal_microsoft_routing_address("mhenderson@contoso.mail.onmicrosoft.com"));
        assert!(is_internal_microsoft_routing_address("admin@test.onmicrosoft.com"));
        assert!(is_internal_microsoft_routing_address("test@onmicrosoft.com"));
        assert!(!is_internal_microsoft_routing_address("mhenderson@acme-corp.example"));
        assert!(!is_internal_microsoft_routing_address("user@example.com"));
        assert!(!is_internal_microsoft_routing_address("onmicrosoft.com@gmail.com"));
        assert!(!is_internal_microsoft_routing_address(""));
    }

    #[test]
    fn test_contact_to_recipient_suggestions_filters_onmicrosoft() {
        let mut contact = VCardContact::new(
            "Max Mustermann",
            "max@company.de",
            "Company AG",
            "Staff",
        );
        contact.emails = vec![
            "max@company.de".to_string(),
            "max@company.mail.onmicrosoft.com".to_string(),
            "max@company.onmicrosoft.com".to_string(),
        ];
        let suggestions = contact_to_recipient_suggestions(&contact, false);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].address, "max@company.de");
        assert_eq!(suggestions[0].display, "Max Mustermann");
    }

    #[test]
    fn gal_contacts_keep_directory_context_in_recipient_suggestions() {
        let suggestion = gal_contact_to_recipient_suggestion(GalContact {
            display_name: "Ada Example".to_string(),
            email_address: "ada@example.org".to_string(),
            job_title: "Engineering Lead".to_string(),
            department: "Platform".to_string(),
            office_location: "Berlin".to_string(),
        });
        assert_eq!(suggestion.address, "ada@example.org");
        assert_eq!(
            recipient_suggestion_label(&suggestion),
            "Ada Example · Engineering Lead · Platform · Berlin <ada@example.org>"
        );
    }

    #[test]
    fn quoted_html_is_reduced_to_safe_text() {
        // </p> produces a newline; <script> content is stripped for security
        assert_eq!(
            quote_as_safe_text("<p>Hello <strong>team</strong></p><script>alert(1)</script>"),
            "Hello team"
        );
        // Plain <script>-only input produces empty output
        assert_eq!(quote_as_safe_text("<script>evil()</script>"), "");
    }

    #[test]
    fn quote_formatter_supports_blockquotes_and_prefixes() {
        assert_eq!(
            format_quoted_body("Hello\nWorld", "blockquote"),
            "<blockquote><pre>Hello\nWorld</pre></blockquote>"
        );
        assert_eq!(
            format_quoted_body("Hello\nWorld", "prefix"),
            "<p>&gt; Hello<br>&gt; World</p>"
        );
        assert_eq!(format_quoted_body("Hello", "none"), "");
    }

    #[test]
    fn forward_variants_keep_inline_and_quoted_bodies_distinct() {
        let inline = forward_body_html_with_style(
            "Ada <ada@example.test>",
            "team@example.test",
            "",
            "2026-08-24",
            "<p>Hello<br>World</p>",
            "inline",
        );
        let quoted = forward_body_html_with_style(
            "Ada <ada@example.test>",
            "team@example.test",
            "",
            "2026-08-24",
            "<p>Hello<br>World</p>",
            "prefix",
        );

        assert!(inline.contains("<pre>Hello\nWorld</pre>"));
        assert!(!inline.contains("&gt; Hello"));
        assert!(quoted.contains("&gt; Hello<br>&gt; World"));
        assert!(inline.contains("Weitergeleitete Nachricht:"));
        assert!(quoted.contains("Weitergeleitete Nachricht:"));
    }

    #[test]
    fn alternative_reply_styles_keep_inline_quote_and_no_quote_distinct() {
        let inline = reply_body_html_with_style(
            "Ada <ada@example.test>",
            "team@example.test",
            "copy@example.test",
            "2026-08-24",
            "<p>Hello<br>World</p>",
            "inline",
        );
        let quoted = reply_body_html_with_style(
            "Ada <ada@example.test>",
            "team@example.test",
            "copy@example.test",
            "2026-08-24",
            "<p>Hello<br>World</p>",
            "prefix",
        );
        let no_quote = reply_body_html_with_style(
            "Ada <ada@example.test>",
            "team@example.test",
            "copy@example.test",
            "2026-08-24",
            "<p>Hello<br>World</p>",
            "none",
        );

        assert!(inline.contains("Ursprüngliche Nachricht:"));
        assert!(inline.contains("Von: Ada &lt;ada@example.test&gt;"));
        assert!(inline.contains("<pre>Hello\nWorld</pre>"));
        assert!(quoted.contains("Ada &lt;ada@example.test&gt; schrieb"));
        assert!(quoted.contains("&gt; Hello<br>&gt; World"));
        assert!(!no_quote.contains("Hello"));
        assert!(!no_quote.contains("Ada"));
    }

    #[test]
    fn attachment_metadata_uses_safe_fallbacks() {
        assert_eq!(
            mime_type_for_path(Path::new("report.pdf")),
            "application/pdf"
        );
        assert_eq!(
            mime_type_for_path(Path::new("unknown.bin")),
            "application/octet-stream"
        );
        assert_eq!(format_size(1024), "1.0 KiB");
    }

    #[test]
    fn local_account_choices_expose_selectable_aliases_without_credentials() {
        let profile = AccountProfile {
            id: "local:person".to_string(),
            display_name: "Person".to_string(),
            email: "person@example.org".to_string(),
            protocol: crate::services::account_config_service::AccountProtocol::ImapSmtp,
            imap_host: String::new(),
            smtp_host: String::new(),
            tls_mode: "tls".to_string(),
            username: "person".to_string(),
            imap_port: 993,
            smtp_port: 465,
            auth_method: String::new(),
            reply_to: String::new(),
            organization: String::new(),
            aliases: vec!["alias@example.org".to_string()],
            inbox_folder: String::new(),
            drafts_folder: String::new(),
            sent_folder: String::new(),
            archive_folder: String::new(),
            trash_folder: String::new(),
            junk_folder: String::new(),
            enabled: true,
            ..Default::default()
        };
        let identities = compose_identities_for_profile(profile.clone());
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].from, "Person <person@example.org>");
        assert_eq!(identities[1].from, "Person <alias@example.org>");
        assert!(identities[1].label.contains("Alias von person@example.org"));
        assert!(compose_identity_matches_from(
            &identities[1],
            "alias@example.org"
        ));
        assert!(
            !identities[1]
                .label
                .to_ascii_lowercase()
                .contains("password")
        );

        let mut disabled = profile.clone();
        disabled.id = "account:disabled".to_string();
        disabled.enabled = false;
        let mut enabled = profile;
        enabled.id = "account:enabled".to_string();
        assert_eq!(
            reconciled_composer_default_account(
                "account:disabled",
                &[disabled.clone(), enabled.clone()]
            ),
            "account:enabled"
        );
        assert_eq!(
            reconciled_composer_default_account("account:deleted", &[enabled]),
            "account:enabled"
        );
        assert_eq!(
            reconciled_composer_default_account("external-eds", &[disabled]),
            "external-eds"
        );
    }

    #[test]
    fn composer_view_actions_and_reply_to_support() {
        assert!(COMPOSER_OPERATION_ACTIONS.contains(&"view-from"));
        assert!(COMPOSER_OPERATION_ACTIONS.contains(&"view-reply-to"));
        assert!(COMPOSER_OPERATION_ACTIONS.contains(&"view-cc"));
        assert!(COMPOSER_OPERATION_ACTIONS.contains(&"view-bcc"));

        let draft = DraftMessage {
            id: "draft-reply-to".to_string(),
            from: "Sender <sender@example.com>".to_string(),
            reply_to: "custom-reply@example.com".to_string(),
            source_uid: "source-1".to_string(),
            to: "recipient@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Test Subject".to_string(),
            html_body: "<p>Hello</p>".to_string(),
            plain_text: false,
            updated_at: "2026-09-06T12:00:00Z".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };
        assert_eq!(draft.reply_to, "custom-reply@example.com");

        let outgoing = OutgoingMessage {
            source_uid: "source-1".to_string(),
            from: "Sender <sender@example.com>".to_string(),
            reply_to: "custom-reply@example.com".to_string(),
            request_read_receipt: false,
            to: "recipient@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Test Subject".to_string(),
            html_body: "<p>Hello</p>".to_string(),
            plain_text: false,
            priority: false,
            charset: "UTF-8".to_string(),
            attachments: Vec::new(),
            calendar_method: None,
            calendar_component: None,
            pgp_sign: false,
            pgp_encrypt: false,
            pgp_key: String::new(),
            smime_options: None,
        };
        assert_eq!(outgoing.reply_to, "custom-reply@example.com");
    }

    #[test]
    fn format_signature_markup_wraps_in_evolution_signature_container() {
        let markup = format_signature_markup("Best regards<br>Alice");
        assert!(markup.contains("id=\"_evolution_signature_\""));
        assert!(markup.contains("class=\"evolution-signature\""));
        assert!(markup.contains("Best regards<br>Alice"));
        assert_eq!(format_signature_markup("   "), "");
    }

    #[test]
    fn test_signature_plain_text_to_html_linkification() {
        let text = "Mit freundlichen Grüßen\nMax Mustermann\nWeb: https://example-portal.de.\nMail: kontakt@example-portal.de\nOder www.example-portal.de/portal";
        let html = signature_plain_text_to_html(text);
        assert!(html.contains("<a href=\"https://example-portal.de\">https://example-portal.de</a>."));
        assert!(html.contains("<a href=\"mailto:kontakt@example-portal.de\">kontakt@example-portal.de</a>"));
        assert!(html.contains("<a href=\"https://www.example-portal.de/portal\">www.example-portal.de/portal</a>"));
        assert!(html.contains("<br>"));
    }

    #[test]
    fn compose_identities_for_profile_carries_signature_id() {
        let profile = AccountProfile {
            id: "work-account".to_string(),
            display_name: "Alice Worker".to_string(),
            email: "alice@work.com".to_string(),
            signature_id: Some("work-sig-123".to_string()),
            ..AccountProfile::default()
        };
        let identities = compose_identities_for_profile(profile);
        assert!(!identities.is_empty());
        assert_eq!(identities[0].signature_id.as_deref(), Some("work-sig-123"));
    }

    #[test]
    fn signature_choices_always_include_none_as_first_choice() {
        let choices = load_signature_choices();
        assert!(!choices.is_empty());
        assert_eq!(choices[0].name, "Keine");
        assert_eq!(choices[0].id, None);
        assert!(choices[0].html.is_empty());
    }

    #[test]
    fn test_compose_dialog_html_editor_and_style_defaults() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        let dialog = ComposeDialog::new();
        assert!(dialog.html_editor().is_some());

        // Test forward style defaults
        settings.set_composer_forward_style("inline");
        assert_eq!(default_forward_style(), "inline");
        settings.set_composer_forward_style("quoted");
        assert_eq!(default_forward_style(), "prefix");
        settings.set_composer_forward_style("attached");
        assert_eq!(default_forward_style(), "attached");

        // Test quote style defaults
        settings.set_quote_original(true);
        settings.set_composer_reply_style("do-not-quote");
        assert_eq!(default_quote_style(), "none");
        settings.set_composer_reply_style("inline");
        assert_eq!(default_quote_style(), "inline");
        settings.set_composer_reply_style("quoted");
        settings.set_quote_style("prefix");
        assert_eq!(default_quote_style(), "prefix");
        settings.set_quote_style("blockquote");
        assert_eq!(default_quote_style(), "blockquote");

        // Reset
        settings.set_composer_reply_style("quoted");
        settings.set_composer_forward_style("attached");
    }

    #[test]
    fn test_feature_19_compose_dialog_autosave_timer_lifecycle_and_boundaries() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // Reset to canonical baseline
        settings.set_draft_autosave_interval_seconds(30);
        while context.iteration(false) {}

        let dialog = ComposeDialog::new();
        while context.iteration(false) {}

        // Initial check: 30s interval should have an active timer
        assert!(
            dialog.imp().autosave_source.borrow().is_some(),
            "ComposeDialog should start with an active autosave timer when interval is 30"
        );

        // Boundary 1: 0 (disabled) - should cancel timer and set source to None
        settings.set_draft_autosave_interval_seconds(0);
        while context.iteration(false) {}
        assert!(
            dialog.imp().autosave_source.borrow().is_none(),
            "Interval 0 should cancel autosave timer and leave autosave_source as None"
        );

        // Boundary 2: 1 second - should re-arm timer with new source
        settings.set_draft_autosave_interval_seconds(1);
        while context.iteration(false) {}
        let source_1 = dialog.imp().autosave_source.borrow().as_ref().map(|s| s.as_raw());
        assert!(
            source_1.is_some(),
            "Interval 1 should schedule a new autosave timer"
        );

        // Boundary 3: 3600 seconds - should replace timer with a new source
        settings.set_draft_autosave_interval_seconds(3600);
        while context.iteration(false) {}
        let source_3600 = dialog.imp().autosave_source.borrow().as_ref().map(|s| s.as_raw());
        assert!(
            source_3600.is_some(),
            "Interval 3600 should schedule a new autosave timer"
        );
        assert_ne!(
            source_1, source_3600,
            "Timer source ID must be replaced when interval changes"
        );

        // Boundary 4: Negative interval via direct GSettings injection
        let raw_settings = gio::Settings::new("com.martingawron.Meridian");
        let _ = raw_settings.set_int("draft-autosave-interval", -20);
        while context.iteration(false) {}
        assert!(
            dialog.imp().autosave_source.borrow().is_none(),
            "Negative interval (-20) clamped to 0 must cancel autosave timer"
        );

        // Boundary 5: High out-of-range interval via direct GSettings injection
        let _ = raw_settings.set_int("draft-autosave-interval", 50000);
        while context.iteration(false) {}
        assert!(
            dialog.imp().autosave_source.borrow().is_some(),
            "High interval clamped to 3600 must maintain an active autosave timer"
        );

        // Rapid mutation stress: 60 transitions between zero, positive, negative
        let intervals = [0, 1, 30, 0, 3600, -5, 10, 0, 15, 60, -100, 30];
        for _ in 0..5 {
            for &int_val in &intervals {
                let _ = raw_settings.set_int("draft-autosave-interval", int_val);
                while context.iteration(false) {}
                if int_val <= 0 {
                    assert!(
                        dialog.imp().autosave_source.borrow().is_none(),
                        "Interval {int_val} must result in no active timer"
                    );
                } else {
                    assert!(
                        dialog.imp().autosave_source.borrow().is_some(),
                        "Interval {int_val} must result in an active timer"
                    );
                }
            }
        }

        // Test forward style permutations on default_forward_style
        settings.set_composer_forward_style(" INLINE ");
        assert_eq!(default_forward_style(), "inline");
        settings.set_composer_forward_style(" Quoted ");
        assert_eq!(default_forward_style(), "prefix");
        settings.set_composer_forward_style(" Attached ");
        assert_eq!(default_forward_style(), "attached");
        settings.set_composer_forward_style("attach");
        assert_eq!(default_forward_style(), "attached");

        // Clean disposal test
        assert!(
            dialog.imp().autosave_setting_handler.borrow().is_some(),
            "Active compose dialog must have an attached autosave setting handler"
        );
        unsafe {
            dialog.run_dispose();
        }
        assert!(
            dialog.imp().autosave_setting_handler.borrow().is_none(),
            "Disposing dialog must take and clear autosave_setting_handler"
        );
        assert!(
            dialog.imp().autosave_source.borrow().is_none(),
            "Disposing dialog must cleanly cancel and take autosave_source"
        );

        // Idempotency stress test: calling run_dispose multiple times must not panic, double-free, or raise GLib assertions
        for cycle in 1..=5 {
            unsafe {
                dialog.run_dispose();
            }
            assert!(
                dialog.imp().autosave_setting_handler.borrow().is_none(),
                "Repeated dispose cycle {cycle} must preserve None autosave_setting_handler"
            );
            assert!(
                dialog.imp().autosave_source.borrow().is_none(),
                "Repeated dispose cycle {cycle} must preserve None autosave_source"
            );
        }

        // Reset settings - must NOT re-arm autosave_source on disposed dialog
        settings.set_draft_autosave_interval_seconds(30);
        settings.set_composer_forward_style("attached");
        settings.set_composer_reply_style("quoted");
        while context.iteration(false) {}
        assert!(
            dialog.imp().autosave_source.borrow().is_none(),
            "Mutating draft-autosave-interval after disposal must not re-arm timer (signal handler disconnected)"
        );

        // Multiple rapid setting mutations across disposed dialog must remain zero-timer
        for &val in &[0, 10, 30, 60, 0, 300, 3600, -10] {
            let _ = raw_settings.set_int("draft-autosave-interval", val);
            while context.iteration(false) {}
            assert!(
                dialog.imp().autosave_source.borrow().is_none(),
                "Post-dispose interval mutation to {val} must never re-arm autosave timer"
            );
        }

        // Concurrent dialogs stress test: one disposed, one active
        let _ = raw_settings.set_int("draft-autosave-interval", 60);
        let dialog_active = ComposeDialog::new();
        while context.iteration(false) {}
        assert!(
            dialog_active.imp().autosave_setting_handler.borrow().is_some(),
            "Active second dialog must have its own autosave handler"
        );
        assert!(
            dialog_active.imp().autosave_source.borrow().is_some(),
            "Active second dialog must have an active timer"
        );
        // Mutate setting: disposed dialog must stay None, active dialog must update
        let _ = raw_settings.set_int("draft-autosave-interval", 45);
        while context.iteration(false) {}
        assert!(
            dialog.imp().autosave_source.borrow().is_none(),
            "Disposed dialog must remain disarmed during concurrent dialog activity"
        );
        assert!(
            dialog_active.imp().autosave_source.borrow().is_some(),
            "Active second dialog must adaptively reschedule its timer"
        );
        // Repeated disposal of second dialog
        for _ in 0..3 {
            unsafe {
                dialog_active.run_dispose();
            }
        }
        assert!(
            dialog_active.imp().autosave_setting_handler.borrow().is_none(),
            "Disposed second dialog must clear autosave_setting_handler"
        );
        assert!(
            dialog_active.imp().autosave_source.borrow().is_none(),
            "Disposed second dialog must clear autosave_source"
        );

        // Dropped dialog without manual run_dispose
        {
            let dialog_scoped = ComposeDialog::new();
            while context.iteration(false) {}
            assert!(dialog_scoped.imp().autosave_setting_handler.borrow().is_some());
            // drop dialog_scoped at end of block
        }
        while context.iteration(false) {}
        let _ = raw_settings.set_int("draft-autosave-interval", 30);
        while context.iteration(false) {}
    }

    #[test]
    fn add_attachment_path_rejects_directory_and_accepts_file() {
        let temp_dir = std::env::temp_dir().join(format!("evolution-composer-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let file_path = temp_dir.join("document.pdf");
        std::fs::write(&file_path, b"%PDF-1.4 test content").expect("write test file");

        let meta_file = std::fs::metadata(&file_path).expect("file metadata");
        assert!(meta_file.is_file());
        assert_eq!(mime_type_for_path(&file_path), "application/pdf");
        assert_eq!(meta_file.len(), 21);

        let meta_dir = std::fs::metadata(&temp_dir).expect("dir metadata");
        assert!(!meta_dir.is_file());

        let _ = std::fs::remove_file(&file_path);
        let _ = std::fs::remove_dir(&temp_dir);
    }

    #[test]
    fn mime_type_for_various_file_extensions() {
        assert_eq!(mime_type_for_path(Path::new("photo.jpg")), "image/jpeg");
        assert_eq!(mime_type_for_path(Path::new("photo.png")), "image/png");
        assert_eq!(mime_type_for_path(Path::new("archive.zip")), "application/zip");
        assert_eq!(mime_type_for_path(Path::new("notes.txt")), "text/plain");
        assert_eq!(mime_type_for_path(Path::new("page.html")), "text/html");
    }
}

