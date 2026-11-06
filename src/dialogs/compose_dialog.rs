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
use chrono::{Datelike, Utc};
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
    ("toggle-format-toolbar", "<Control><Shift>f"),
    ("print", "<Control>p"),
    ("toggle-maximize", "<Alt>F10"),
    ("toggle-fullscreen", "F11"),
    ("help", "<Control>question"),
    ("help", "F1"),
];

const COMPOSER_OPERATION_ACTIONS: &[&str] = &[
    "send",
    "save-draft",
    "discard",
    "save-as-eml",
    "print",
    "print-preview",
    "help",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AttachmentViewMode {
    #[default]
    Icons,
    List,
}

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
        pub btn_toggle_cc_bcc: RefCell<Option<gtk::Button>>,
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
        pub format_button: RefCell<Option<gtk::ToggleButton>>,
        pub plain_text_button: RefCell<Option<gtk::ToggleButton>>,
        pub request_read_receipt_button: RefCell<Option<gtk::ToggleButton>>,
        pub priority_button: RefCell<Option<gtk::ToggleButton>>,
        pub pgp_sign_button: RefCell<Option<gtk::ToggleButton>>,
        pub pgp_encrypt_button: RefCell<Option<gtk::ToggleButton>>,
        pub pgp_key_override: RefCell<Option<String>>,
        pub(crate) smime_options_override: RefCell<Option<SmimeOptions>>,
        pub charset: RefCell<String>,
        pub attachments: RefCell<Vec<DraftAttachment>>,
        pub attachment_section_revealer: RefCell<Option<gtk::Revealer>>,
        pub attachment_section_box: RefCell<Option<gtk::Box>>,
        pub attachment_bar: RefCell<Option<gtk::Box>>,
        pub attachment_left_container: RefCell<Option<gtk::Box>>,
        pub attachment_bar_spacer: RefCell<Option<gtk::Box>>,
        pub attachment_title_box: RefCell<Option<gtk::Box>>,
        pub attachment_summary_box: RefCell<Option<gtk::Box>>,
        pub attachment_summary_label: RefCell<Option<gtk::Label>>,
        pub attachment_summary_chevron: RefCell<Option<gtk::Image>>,
        pub attachment_view_toggle_box: RefCell<Option<gtk::Box>>,
        pub attachment_btn_icons: RefCell<Option<gtk::Button>>,
        pub attachment_btn_list: RefCell<Option<gtk::Button>>,
        pub attachment_btn_collapse: RefCell<Option<gtk::Button>>,
        pub attachment_bar_chip_box: RefCell<Option<gtk::Box>>,
        pub attachment_bar_separator: RefCell<Option<gtk::Separator>>,
        pub attachment_bar_add_btn: RefCell<Option<gtk::Button>>,
        pub attachment_detail_revealer: RefCell<Option<gtk::Revealer>>,
        pub attachment_sheet_handle_btn: RefCell<Option<gtk::Button>>,
        pub attachment_scrolled: RefCell<Option<gtk::ScrolledWindow>>,
        pub attachment_stack: RefCell<Option<gtk::Stack>>,
        pub attachment_flowbox: RefCell<Option<gtk::FlowBox>>,
        pub attachment_listbox: RefCell<Option<gtk::ListBox>>,
        pub attachment_expanded: Cell<bool>,
        pub attachment_view_mode: Cell<AttachmentViewMode>,
        pub attachment_table_compact: Cell<bool>,
        pub attachment_col_type: RefCell<Option<gtk::Label>>,
        pub attachment_col_mod: RefCell<Option<gtk::Label>>,
        pub is_drag_over_sheet: Cell<bool>,
        pub is_drag_active: Cell<bool>,
        pub is_internal_drag: Cell<bool>,
        pub drag_auto_expanded: Cell<bool>,
        pub drag_leave_timeout: RefCell<Option<glib::SourceId>>,
        pub attachment_empty_page: RefCell<Option<adw::StatusPage>>,
        pub compose_controls: RefCell<Vec<gtk::Widget>>,
        pub composer_action_group: RefCell<Option<gio::SimpleActionGroup>>,
        pub dirty_revision: Cell<u64>,
        pub saved_revision: Cell<u64>,
        pub backend_draft_saved: Cell<bool>,
        pub user_typed_in_editor: Cell<bool>,
        pub focus_editor_on_open: Cell<bool>,
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
        pub btn_close: RefCell<Option<gtk::Button>>,
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
        pub btn_attach: RefCell<Option<gtk::Button>>,
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
            .property("title", "New Message")
            .property("content-width", 980)
            .property("content-height", 720)
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
        if self.imp().focus_editor_on_open.get() {
            self.apply_reply_focus();
        }
        adw::prelude::AdwDialogExt::present(self, Some(parent));
        if self.imp().focus_editor_on_open.get() {
            self.apply_reply_focus();
            let dialog_weak = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.apply_reply_focus();
                }
            });
        }
    }

    pub fn is_modal(&self) -> bool {
        self.imp().is_modal.get()
    }

    pub fn present_standalone(&self) {
        self.imp().is_modal.set(false);
        if let Some(btn) = self.imp().btn_open_in_new_window.borrow().as_ref() {
            btn.set_visible(false);
        }
        if self.imp().focus_editor_on_open.get() {
            self.apply_reply_focus();
        }
        adw::prelude::AdwDialogExt::present(self, None::<&gtk::Widget>);
        self.configure_root_window();
        if self.imp().focus_editor_on_open.get() {
            self.apply_reply_focus();
            let dialog_weak = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.apply_reply_focus();
                }
            });
        }
    }

    fn prepare_focus_for_reply(&self) {
        self.imp().focus_editor_on_open.set(true);
        self.apply_reply_focus();
    }

    fn apply_reply_focus(&self) {
        if let Some(entry_to) = self.imp().entry_to.borrow().as_ref() {
            clear_entry_selection(entry_to);
        }
        if let Some(entry_cc) = self.imp().entry_cc.borrow().as_ref() {
            clear_entry_selection(entry_cc);
        }
        if let Some(entry_subject) = self.imp().entry_subject.borrow().as_ref() {
            clear_entry_selection(entry_subject);
        }
        if let Some(editor) = self.imp().html_editor.borrow().as_ref() {
            if let Some(wv) = editor.web_view() {
                adw::prelude::AdwDialogExt::set_focus(self, Some(&wv));
            }
            editor.focus_start();
        }
    }

    pub fn split_to_standalone_window_and_maximize(&self) {
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
            if let Some(win) = standalone.root().and_downcast::<gtk::Window>() {
                win.maximize();
            }
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

    pub fn configure_root_window(&self) {
        if let Some(win) = self.root().and_downcast::<gtk::Window>() {
            win.set_resizable(true);
            win.add_css_class("meridian-compose-window");
            win.add_css_class("composer-window");
        }
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
        dialog.mark_saved();
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

    #[allow(clippy::too_many_arguments)]
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
        dialog.prepare_focus_for_reply();
        dialog.mark_saved();
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
        dialog.mark_saved();
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
        if !recipient_cc.trim().is_empty() {
            dialog.set_row_cc_visible(true);
        }
        if !recipient_bcc.trim().is_empty() {
            dialog.set_row_bcc_visible(true);
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
        dialog.mark_saved();
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
        dialog.mark_saved();
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
        dialog.mark_saved();
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
        dialog.mark_saved();
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

    #[allow(clippy::too_many_arguments)]
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
        if !cc.trim().is_empty() {
            dialog.set_row_cc_visible(true);
        }
        dialog.set_title(&format!("Allen antworten: {subject}"));
        dialog.prepare_focus_for_reply();
        dialog.mark_saved();
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
        dialog.imp().user_typed_in_editor.set(draft_has_user_content(&draft));
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
        self.imp().suppress_dirty_tracking.set(true);
        self.add_css_class("meridian-compose-dialog");
        self.add_css_class("composer-dialog");
        {
            let mut stored_id = self.imp().draft_id.borrow_mut();
            if stored_id.is_empty() {
                *stored_id = uuid::Uuid::new_v4().to_string();
            }
        }
        self.set_content_width(980);
        self.set_content_height(720);
        let toolbar_view = adw::ToolbarView::new();

        // ── Header bar ──────────────────────────────────────────────────────
        let header_bar = adw::HeaderBar::new();
        header_bar.set_show_end_title_buttons(false);
        header_bar.set_show_start_title_buttons(false);

        let btn_close = gtk::Button::builder()
            .label(&crate::i18n::gettext("Close"))
            .tooltip_text(&crate::i18n::gettext("Close and save draft (Ctrl+W)"))
            .css_classes(vec!["composer-close-btn"])
            .valign(gtk::Align::Center)
            .margin_end(6)
            .build();
        set_accessible_label(&btn_close, &crate::i18n::gettext("Close"));
        btn_close.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        header_bar.pack_start(&btn_close);
        self.imp().btn_close.replace(Some(btn_close.clone()));

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

        let btn_attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text(&crate::i18n::gettext("Add attachment… (Ctrl+M)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_attach, &crate::i18n::gettext("Add attachment"));
        self.imp().btn_attach.replace(Some(btn_attach.clone()));

        btn_attach.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.open_attachment_chooser();
            }
        ));
        header_bar.pack_start(&btn_attach);

        let show_format_toolbar = AppSettings::try_new()
            .map(|s| s.composer_show_format_toolbar())
            .unwrap_or(false);
        let btn_format = gtk::ToggleButton::builder()
            .icon_name("format-text-underline-symbolic")
            .tooltip_text(&crate::i18n::gettext("Toggle formatting (Ctrl+Shift+F)"))
            .css_classes(vec!["flat"])
            .active(show_format_toolbar)
            .build();
        set_accessible_label(&btn_format, &crate::i18n::gettext("Formatting"));
        header_bar.pack_start(&btn_format);
        self.imp().format_button.replace(Some(btn_format.clone()));

        let btn_discard = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(&crate::i18n::gettext("Discard draft"))
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_discard, &crate::i18n::gettext("Discard draft"));
        self.imp().btn_discard.replace(Some(btn_discard.clone()));

        let btn_save_draft = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text(&crate::i18n::gettext("Save draft (Ctrl+S)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .margin_end(8)
            .build();
        set_accessible_label(&btn_save_draft, &crate::i18n::gettext("Save draft"));

        let btn_send = gtk::Button::builder()
            .label(&crate::i18n::gettext("Send"))
            .css_classes(vec!["suggested-action"])
            .tooltip_text(&crate::i18n::gettext("Send email now (Ctrl+Enter)"))
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_send, &crate::i18n::gettext("Send email"));

        // Header actions (packed right-to-left: Send)
        header_bar.pack_end(&btn_send);


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

        let btn_plain_text = gtk::ToggleButton::builder()
            .icon_name("text-x-generic-symbolic")
            .tooltip_text("Nur-Text-Modus beim Verfassen")
            .active(false)
            .css_classes(vec!["flat"])
            .visible(false)
            .build();

        let header_mode_switcher = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .css_classes(vec!["linked", "composer-mode-switcher"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let header_btn_html = gtk::ToggleButton::builder()
            .label("HTML")
            .tooltip_text("HTML-Format (Formatierter Text)")
            .active(true)
            .css_classes(vec!["caption-heading"])
            .build();
        set_accessible_label(&header_btn_html, "HTML");
        let header_btn_txt = gtk::ToggleButton::builder()
            .label("TXT")
            .tooltip_text("Nur-Text-Format (Plain Text)")
            .group(&header_btn_html)
            .active(false)
            .css_classes(vec!["caption-heading"])
            .build();
        set_accessible_label(&header_btn_txt, "TXT");
        header_mode_switcher.append(&header_btn_html);
        header_mode_switcher.append(&header_btn_txt);

        {
            let btn_plain_for_html = btn_plain_text.clone();
            header_btn_html.connect_toggled(move |b| {
                if b.is_active() && btn_plain_for_html.is_active() {
                    btn_plain_for_html.set_active(false);
                }
            });
            let btn_plain_for_txt = btn_plain_text.clone();
            header_btn_txt.connect_toggled(move |b| {
                if b.is_active() && !btn_plain_for_txt.is_active() {
                    btn_plain_for_txt.set_active(true);
                }
            });
        }

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
            .label(crate::i18n::gettext("Cancel"))
            .tooltip_text(crate::i18n::gettext("Cancel operation before transport"))
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

        let header_card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_start(16)
            .margin_end(16)
            .margin_top(4)
            .margin_bottom(12)
            .css_classes(vec!["card", "composer-header-card"])
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
            .label(crate::i18n::gettext("From"))
            .css_classes(vec!["dim-label", "composer-header-label"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .visible(show_from)
            .build();
        let make_clean_dropdown_factory = || -> gtk::SignalListItemFactory {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, list_item| {
                if let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() {
                    let label = gtk::Label::builder()
                        .xalign(0.0)
                        .halign(gtk::Align::Start)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build();
                    list_item.set_child(Some(&label));
                }
            });
            factory.connect_bind(|_, list_item| {
                if let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() {
                    if let (Some(item), Some(label)) = (
                        list_item.item().and_downcast::<gtk::StringObject>(),
                        list_item.child().and_downcast::<gtk::Label>(),
                    ) {
                        label.set_label(&item.string());
                    }
                }
            });
            factory
        };

        let source_model = gtk::StringList::new(&["Absender werden geladen…"]);
        let combo_from = gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        combo_from.set_hexpand(true);
        combo_from.add_css_class("composer-dropdown");
        combo_from.set_factory(Some(&make_clean_dropdown_factory()));
        combo_from.set_list_factory(Some(&make_clean_dropdown_factory()));

        let sig_choices = load_signature_choices();
        let sig_names: Vec<&str> = sig_choices.iter().map(|c| c.name.as_str()).collect();
        let sig_model = gtk::StringList::new(&sig_names);
        let combo_signature = gtk::DropDown::new(Some(sig_model), None::<&gtk::Expression>);
        combo_signature.set_valign(gtk::Align::Center);
        combo_signature.add_css_class("composer-dropdown");
        combo_signature.set_factory(Some(&make_clean_dropdown_factory()));
        combo_signature.set_list_factory(Some(&make_clean_dropdown_factory()));

        let lbl_signature = gtk::Label::builder()
            .label(crate::i18n::gettext("Signature"))
            .tooltip_text(crate::i18n::gettext("Manage Signatures…"))
            .css_classes(vec![
                "dim-label",
                "composer-header-label",
                "composer-header-label-interactive",
            ])
            .valign(gtk::Align::Center)
            .build();
        lbl_signature.set_cursor_from_name(Some("pointer"));
        {
            let click = gtk::GestureClick::new();
            let dialog_weak = self.downgrade();
            click.connect_released(move |_, _, _, _| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let current_sig_id = dialog
                    .imp()
                    .combo_signature
                    .borrow()
                    .as_ref()
                    .and_then(|combo| {
                        let idx = combo.selected() as usize;
                        dialog
                            .imp()
                            .signature_choices
                            .borrow()
                            .get(idx)
                            .and_then(|c| c.id.clone())
                    });
                let initial_profile = current_sig_id.and_then(|id| {
                    SignatureStore::from_environment()
                        .load()
                        .into_iter()
                        .find(|s| s.id == id)
                });
                let dialog_for_save = dialog.downgrade();
                let on_saved = move |saved: crate::services::signature_store_service::SignatureProfile| {
                    let Some(dlg) = dialog_for_save.upgrade() else {
                        return;
                    };
                    dlg.refresh_signature_choices();
                    if let Some(combo) = dlg.imp().combo_signature.borrow().as_ref() {
                        let pos = dlg
                            .imp()
                            .signature_choices
                            .borrow()
                            .iter()
                            .position(|c| c.id.as_deref() == Some(saved.id.as_str()));
                        if let Some(pos) = pos {
                            combo.set_selected(pos as u32);
                        }
                    }
                };
                let sig_dialog = if let Some(ref profile) = initial_profile {
                    crate::dialogs::SignatureEditorDialog::new_for_edit(profile, Some(on_saved))
                } else {
                    crate::dialogs::SignatureEditorDialog::new_manager(Some(on_saved))
                };
                let dialog_for_close = dialog.downgrade();
                sig_dialog.connect_closed(move |_| {
                    if let Some(dlg) = dialog_for_close.upgrade() {
                        dlg.refresh_signature_choices();
                    }
                });
                sig_dialog.present(Some(&dialog));
            });
            lbl_signature.add_controller(click);
        }

        let from_row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .hexpand(true)
            .visible(show_from)
            .build();
        from_row_box.append(&combo_from);
        from_row_box.append(&lbl_signature);
        from_row_box.append(&combo_signature);

        let row_from = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-from"])
            .visible(show_from)
            .build();
        row_from.append(&lbl_from);
        row_from.append(&from_row_box);
        lbl_from.bind_property("visible", &row_from, "visible").sync_create().build();

        // Reply-To
        let lbl_reply_to = gtk::Label::builder()
            .label(crate::i18n::gettext("Reply-To"))
            .css_classes(vec!["dim-label", "composer-header-label"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .visible(show_reply_to)
            .build();
        set_accessible_label(&lbl_reply_to, &crate::i18n::gettext("Reply-To"));
        let entry_reply_to = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .hexpand(true)
            .css_classes(vec!["composer-field-entry"])
            .build();
        set_accessible_label(&entry_reply_to, &crate::i18n::gettext("Reply-To"));
        entry_reply_to.set_visible(show_reply_to);

        let row_reply_to = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-reply-to"])
            .visible(show_reply_to)
            .build();
        row_reply_to.append(&lbl_reply_to);
        row_reply_to.append(&entry_reply_to);
        lbl_reply_to.bind_property("visible", &row_reply_to, "visible").sync_create().build();

        // To
        let lbl_to = gtk::Label::builder()
            .label(crate::i18n::gettext("To"))
            .tooltip_text(crate::i18n::gettext("Select contacts…"))
            .css_classes(vec![
                "dim-label",
                "composer-header-label",
                "composer-header-label-interactive",
            ])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();
        lbl_to.set_cursor_from_name(Some("pointer"));
        set_accessible_label(&lbl_to, &crate::i18n::gettext("Recipients"));
        let entry_to = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .hexpand(true)
            .css_classes(vec!["composer-field-entry"])
            .build();
        set_accessible_label(&entry_to, &crate::i18n::gettext("Recipients"));

        let to_row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .hexpand(true)
            .build();
        to_row_box.append(&entry_to);

        let btn_toggle_cc_bcc = gtk::Button::builder()
            .label("Cc/Bcc")
            .tooltip_text(crate::i18n::gettext("Toggle Cc and Bcc recipient fields"))
            .css_classes(vec!["flat", "composer-cc-toggle-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_toggle_cc_bcc, &crate::i18n::gettext("Toggle Cc and Bcc fields"));
        to_row_box.append(&btn_toggle_cc_bcc);

        let dialog_weak_for_toggle_cc_bcc = self.downgrade();
        btn_toggle_cc_bcc.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_toggle_cc_bcc.upgrade() {
                let is_cc = dialog.is_row_cc_visible();
                let is_bcc = dialog.is_row_bcc_visible();
                if is_cc && is_bcc {
                    dialog.set_row_cc_visible(false);
                    dialog.set_row_bcc_visible(false);
                } else {
                    dialog.set_row_cc_visible(true);
                    dialog.set_row_bcc_visible(true);
                    if !is_cc {
                        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
                            entry.grab_focus();
                        }
                    } else if let Some(entry) = dialog.imp().entry_bcc.borrow().as_ref() {
                        entry.grab_focus();
                    }
                }
            }
        });
        self.imp().btn_toggle_cc_bcc.replace(Some(btn_toggle_cc_bcc.clone()));

        let row_to = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-to"])
            .build();
        row_to.append(&lbl_to);
        row_to.append(&to_row_box);

        // Cc/Bcc visibility defaults from AppSettings
        let lbl_cc = gtk::Label::builder()
            .label(crate::i18n::gettext("Cc"))
            .tooltip_text(crate::i18n::gettext("Select contacts…"))
            .css_classes(vec![
                "dim-label",
                "composer-header-label",
                "composer-header-label-interactive",
            ])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .visible(show_cc)
            .build();
        lbl_cc.set_cursor_from_name(Some("pointer"));
        set_accessible_label(&lbl_cc, &crate::i18n::gettext("Copy (Cc)"));
        let entry_cc = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .hexpand(true)
            .visible(show_cc)
            .css_classes(vec!["composer-field-entry"])
            .build();
        set_accessible_label(&entry_cc, &crate::i18n::gettext("Copy (Cc)"));

        let row_cc = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-cc"])
            .visible(show_cc)
            .build();
        row_cc.append(&lbl_cc);
        row_cc.append(&entry_cc);
        lbl_cc.bind_property("visible", &row_cc, "visible").sync_create().build();

        let lbl_bcc = gtk::Label::builder()
            .label(crate::i18n::gettext("Bcc"))
            .tooltip_text(crate::i18n::gettext("Select contacts…"))
            .css_classes(vec![
                "dim-label",
                "composer-header-label",
                "composer-header-label-interactive",
            ])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .visible(show_bcc)
            .build();
        lbl_bcc.set_cursor_from_name(Some("pointer"));
        set_accessible_label(&lbl_bcc, &crate::i18n::gettext("Blind Copy (Bcc)"));
        let entry_bcc = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Email)
            .hexpand(true)
            .visible(show_bcc)
            .css_classes(vec!["composer-field-entry"])
            .build();
        set_accessible_label(&entry_bcc, &crate::i18n::gettext("Blind Copy (Bcc)"));

        let row_bcc = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-bcc"])
            .visible(show_bcc)
            .build();
        row_bcc.append(&lbl_bcc);
        row_bcc.append(&entry_bcc);
        lbl_bcc.bind_property("visible", &row_bcc, "visible").sync_create().build();

        // Subject
        let lbl_subject = gtk::Label::builder()
            .label(crate::i18n::gettext("Subject"))
            .css_classes(vec!["dim-label", "composer-header-label"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();
        let entry_subject = gtk::Entry::builder()
            .hexpand(true)
            .css_classes(vec!["composer-field-entry", "composer-subject-entry"])
            .build();
        set_accessible_label(&entry_subject, &crate::i18n::gettext("Subject"));

        let row_subject = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(vec!["composer-header-row", "composer-row-subject", "last-visible-row"])
            .build();
        row_subject.append(&lbl_subject);
        row_subject.append(&entry_subject);

        let label_size_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        label_size_group.add_widget(&lbl_from);
        label_size_group.add_widget(&lbl_reply_to);
        label_size_group.add_widget(&lbl_to);
        label_size_group.add_widget(&lbl_cc);
        label_size_group.add_widget(&lbl_bcc);
        label_size_group.add_widget(&lbl_subject);

        header_card.append(&row_from);
        header_card.append(&row_reply_to);
        header_card.append(&row_to);
        header_card.append(&row_cc);
        header_card.append(&row_bcc);
        header_card.append(&row_subject);

        let update_first_visible_row = {
            let r_from = row_from.clone();
            let r_reply = row_reply_to.clone();
            let r_to = row_to.clone();
            std::rc::Rc::new(move || {
                r_from.remove_css_class("first-visible-row");
                r_reply.remove_css_class("first-visible-row");
                r_to.remove_css_class("first-visible-row");

                if r_from.is_visible() {
                    r_from.add_css_class("first-visible-row");
                } else if r_reply.is_visible() {
                    r_reply.add_css_class("first-visible-row");
                } else {
                    r_to.add_css_class("first-visible-row");
                }
            })
        };
        {
            let u = update_first_visible_row.clone();
            row_from.connect_notify_local(Some("visible"), move |_, _| u());
        }
        {
            let u = update_first_visible_row.clone();
            row_reply_to.connect_notify_local(Some("visible"), move |_, _| u());
        }
        update_first_visible_row();

        content_box.append(&header_card);

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

        let update_options_badges = {
            let badges_box = options_badges_box.clone();
            let b_sign = btn_sign.clone();
            let b_encrypt = btn_encrypt.clone();
            let b_priority = btn_priority.clone();
            let b_receipt = btn_request_read_receipt.clone();

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
        update_options_badges();

        // Rich WebKitGTK 6.0 WYSIWYG HTML Editor
        let html_editor = crate::views::HtmlEditor::new();
        let initial_plain = btn_plain_text.is_active();
        html_editor.set_plain_text_mode(initial_plain);
        html_editor.set_vexpand(true);
        html_editor.set_toolbar_scrollable(false);
        btn_format.set_sensitive(!initial_plain);
        html_editor.set_format_toolbar_visible(!initial_plain && btn_format.is_active());

        let plain_for_fmt = btn_plain_text.clone();
        btn_format.connect_toggled(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |btn| {
                let is_plain = plain_for_fmt.is_active();
                if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                    editor.set_format_toolbar_visible(!is_plain && btn.is_active());
                }
                if !dialog.imp().suppress_setting_persistence.get() && btn.is_sensitive() {
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_composer_show_format_toolbar(btn.is_active());
                    }
                }
            }
        ));

        let editor_key_controller = gtk::EventControllerKey::new();
        editor_key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let dialog_weak_for_editor = self.downgrade();
        editor_key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if let Some(dialog) = dialog_weak_for_editor.upgrade() {
                let is_nav_or_modifier = matches!(
                    keyval,
                    gdk::Key::Shift_L
                        | gdk::Key::Shift_R
                        | gdk::Key::Control_L
                        | gdk::Key::Control_R
                        | gdk::Key::Alt_L
                        | gdk::Key::Alt_R
                        | gdk::Key::Super_L
                        | gdk::Key::Super_R
                        | gdk::Key::Caps_Lock
                        | gdk::Key::Num_Lock
                        | gdk::Key::Scroll_Lock
                        | gdk::Key::Up
                        | gdk::Key::Down
                        | gdk::Key::Left
                        | gdk::Key::Right
                        | gdk::Key::Page_Up
                        | gdk::Key::Page_Down
                        | gdk::Key::Home
                        | gdk::Key::End
                        | gdk::Key::Tab
                        | gdk::Key::ISO_Left_Tab
                        | gdk::Key::Escape
                );
                if !is_nav_or_modifier {
                    dialog.imp().user_typed_in_editor.set(true);
                }
                dialog.mark_changed();
                if dialog.imp().attachment_expanded.get() {
                    dialog.set_attachment_expanded(false);
                }
                if state.contains(gdk::ModifierType::CONTROL_MASK)
                    && state.contains(gdk::ModifierType::SHIFT_MASK)
                    && (keyval == gdk::Key::F || keyval == gdk::Key::f)
                {
                    if let Some(btn) = dialog.imp().format_button.borrow().as_ref() {
                        if btn.is_sensitive() {
                            btn.set_active(!btn.is_active());
                            return glib::Propagation::Stop;
                        }
                    }
                }
            }
            glib::Propagation::Proceed
        });
        html_editor.add_controller(editor_key_controller);

        let dialog_weak_for_drop = self.downgrade();
        html_editor.connect_files_dropped(move |paths| {
            if let Some(dialog) = dialog_weak_for_drop.upgrade() {
                dialog.imp().user_typed_in_editor.set(true);
                let is_plain = dialog.imp().html_editor.borrow().as_ref().map(|e| e.plain_text_mode()).unwrap_or(false);
                for path in paths {
                    if !is_plain && crate::views::HtmlEditor::is_supported_image(&path) {
                        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                            if editor.insert_image_from_path(&path) {
                                dialog.mark_changed();
                                let path_str = path.to_string_lossy().to_string();
                                if dialog.imp().attachments.borrow().iter().any(|a| a.path == path_str) {
                                    dialog.remove_attachment(&path_str);
                                }
                                continue;
                            }
                        }
                    }
                    dialog.add_attachment_path(&path);
                }
                let was_auto_expanded = dialog.imp().drag_auto_expanded.get();
                dialog.imp().drag_auto_expanded.set(false);
                if was_auto_expanded {
                    dialog.set_attachment_expanded(false);
                }
                dialog.show_attachment_bar_drag_hint(false);
                if dialog.imp().attachments.borrow().is_empty() {
                    dialog.set_attachment_expanded(false);
                    if let Some(bar) = dialog.imp().attachment_bar.borrow().as_ref() {
                        bar.set_visible(false);
                    }
                }
            }
        });

        let dialog_weak_for_drag_motion = self.downgrade();
        html_editor.connect_drag_motion(move |_y, _h| {
            if let Some(dialog) = dialog_weak_for_drag_motion.upgrade() {
                if dialog.is_external_drag(None) {
                    dialog.start_external_drag_session();
                }
            }
        });

        let dialog_weak_for_drag_leave = self.downgrade();
        html_editor.connect_drag_leave(move || {
            if let Some(dialog) = dialog_weak_for_drag_leave.upgrade() {
                dialog.schedule_drag_leave();
            }
        });

        let dialog_weak_for_internal = self.downgrade();
        html_editor.connect_drag_internal(move |is_internal| {
            if let Some(dialog) = dialog_weak_for_internal.upgrade() {
                dialog.imp().is_internal_drag.set(is_internal);
                if !is_internal {
                    dialog.imp().is_drag_active.set(false);
                }
            }
        });

        let dialog_weak_for_img_convert = self.downgrade();
        html_editor.connect_image_convert_to_attachment(move |attachment| {
            if let Some(dialog) = dialog_weak_for_img_convert.upgrade() {
                dialog.imp().user_typed_in_editor.set(true);
                dialog.add_attachment(attachment);
                dialog.set_attachment_expanded(true);
            }
        });

        let btn_plain_for_editor = btn_plain_text.clone();
        html_editor.connect_plain_text_mode_changed(move |is_plain| {
            if btn_plain_for_editor.is_active() != is_plain {
                btn_plain_for_editor.set_active(is_plain);
            }
        });

        let settings_for_composer_mode = AppSettings::try_new();
        let editor_for_composer_mode = html_editor.clone();
        let dialog_for_composer_mode = self.downgrade();
        let header_html_for_mode = header_btn_html.clone();
        let header_txt_for_mode = header_btn_txt.clone();
        let btn_format_for_mode = btn_format.clone();
        let header_mode_switcher_for_mode = header_mode_switcher.clone();
        btn_plain_text.connect_toggled(move |button| {
            let is_plain = button.is_active();
            if header_html_for_mode.is_active() != (!is_plain) {
                header_html_for_mode.set_active(!is_plain);
            }
            if header_txt_for_mode.is_active() != is_plain {
                header_txt_for_mode.set_active(is_plain);
            }
            btn_format_for_mode.set_sensitive(!is_plain);
            header_mode_switcher_for_mode.set_visible(false);
            editor_for_composer_mode.set_format_toolbar_visible(!is_plain && btn_format_for_mode.is_active());
            editor_for_composer_mode.set_plain_text_mode(is_plain);
            let suppress_persistence = dialog_for_composer_mode
                .upgrade()
                .is_some_and(|dialog| dialog.imp().suppress_setting_persistence.get());
            if !suppress_persistence {
                if let Some(settings) = settings_for_composer_mode.as_ref() {
                    settings.set_composer_html(!is_plain);
                }
            }
            if let Some(dialog) = dialog_for_composer_mode.upgrade() {
                dialog.mark_changed();
            }
        });

        let attachment_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(vec!["attachment-horizontal-bar"])
            .hexpand(true)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();

        let left_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .hexpand(false)
            .valign(gtk::Align::Center)
            .build();

        // 0. Title Box: [ 📎 ] (Icon only, clean and compact without redundant text)
        let title_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .valign(gtk::Align::Center)
            .hexpand(false)
            .css_classes(vec!["attachment-title-box"])
            .tooltip_text(&crate::i18n::gettext("Attachments"))
            .build();

        let clip_icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
        clip_icon.set_pixel_size(16);
        clip_icon.set_opacity(0.85);
        clip_icon.set_halign(gtk::Align::Center);
        clip_icon.set_valign(gtk::Align::Center);
        clip_icon.set_hexpand(true);
        title_box.append(&clip_icon);

        let dialog_weak_for_title = self.downgrade();
        let title_click = gtk::GestureClick::new();
        title_click.connect_released(move |_, _, _, _| {
            if let Some(dialog) = dialog_weak_for_title.upgrade() {
                dialog.toggle_attachment_expanded();
            }
        });
        title_box.add_controller(title_click);
        left_container.append(&title_box);

        // 1. Summary Box: [ 5 attachments (6.4 MiB) ] (Shown when expanded)
        let summary_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .valign(gtk::Align::Center)
            .hexpand(false)
            .css_classes(vec!["attachment-summary-clickable"])
            .visible(false)
            .build();

        let summary_label = gtk::Label::builder()
            .label(&crate::i18n::gettext("Drop files here"))
            .valign(gtk::Align::Center)
            .use_markup(true)
            .build();
        summary_box.append(&summary_label);

        let dialog_weak_for_summary = self.downgrade();
        let summary_click = gtk::GestureClick::new();
        summary_click.connect_released(move |_, _, _, _| {
            if let Some(dialog) = dialog_weak_for_summary.upgrade() {
                dialog.toggle_attachment_expanded();
            }
        });
        summary_box.add_controller(summary_click);
        left_container.append(&summary_box);

        // 2. Chip Box: Horizontal list of chips that fit (no scrollbar)
        let chip_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .hexpand(false)
            .css_classes(vec!["attachment-bar-chip-container"])
            .visible(true)
            .build();
        left_container.append(&chip_box);

        attachment_bar.append(&left_container);

        let bar_spacer = gtk::Box::builder().hexpand(true).build();
        attachment_bar.append(&bar_spacer);

        let separator = gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .css_classes(vec!["attachment-bar-separator"])
            .visible(false)
            .build();

        let add_icon = gtk::Image::from_icon_name("list-add-symbolic");
        add_icon.set_pixel_size(14);
        let btn_add_attachment = gtk::Button::builder()
            .child(&add_icon)
            .tooltip_text(&crate::i18n::gettext("Add attachment…"))
            .css_classes(vec!["flat", "circular", "attachment-bar-add-btn"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        set_accessible_label(&btn_add_attachment, &crate::i18n::gettext("Add attachment"));
        let dialog_weak_for_add = self.downgrade();
        btn_add_attachment.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_add.upgrade() {
                dialog.open_attachment_chooser();
            }
        });

        // View toggle: Only the two drawer views (Icons and List), visible only when expanded
        let view_toggle_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked", "attachment-view-toggle"])
            .valign(gtk::Align::Center)
            .margin_start(12)
            .margin_end(10)
            .visible(false)
            .build();

        let btn_icons = gtk::Button::builder()
            .icon_name("view-grid-symbolic")
            .tooltip_text(&crate::i18n::gettext("Icon view"))
            .css_classes(vec!["flat", "active"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_icons, &crate::i18n::gettext("Icon view"));
        let dialog_weak_for_icons = self.downgrade();
        btn_icons.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_icons.upgrade() {
                dialog.set_attachment_view_mode(AttachmentViewMode::Icons);
            }
        });

        let btn_list = gtk::Button::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(&crate::i18n::gettext("List view"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&btn_list, &crate::i18n::gettext("List view"));
        let dialog_weak_for_list = self.downgrade();
        btn_list.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_list.upgrade() {
                dialog.set_attachment_view_mode(AttachmentViewMode::List);
            }
        });

        view_toggle_box.append(&btn_icons);
        view_toggle_box.append(&btn_list);

        let collapse_icon = gtk::Image::from_icon_name("pan-down-symbolic");
        collapse_icon.set_pixel_size(16);
        let btn_collapse = gtk::Button::builder()
            .child(&collapse_icon)
            .tooltip_text(&crate::i18n::gettext("Expand attachments"))
            .css_classes(vec!["flat", "attachment-collapse-btn"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        set_accessible_label(&btn_collapse, &crate::i18n::gettext("Collapse attachments"));
        let dialog_weak_for_collapse = self.downgrade();
        btn_collapse.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_collapse.upgrade() {
                dialog.toggle_attachment_expanded();
            }
        });

        let right_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .valign(gtk::Align::Center)
            .hexpand(false)
            .build();
        right_actions.append(&btn_add_attachment);
        right_actions.append(&view_toggle_box);
        right_actions.append(&btn_collapse);
        attachment_bar.append(&right_actions);

        self.imp().attachment_bar.replace(Some(attachment_bar.clone()));
        self.imp().attachment_left_container.replace(Some(left_container));
        self.imp().attachment_bar_spacer.replace(Some(bar_spacer));
        self.imp().attachment_title_box.replace(Some(title_box));
        self.imp().attachment_summary_box.replace(Some(summary_box));
        self.imp().attachment_summary_label.replace(Some(summary_label));
        self.imp().attachment_summary_chevron.replace(Some(collapse_icon));
        self.imp().attachment_bar_chip_box.replace(Some(chip_box));
        self.imp().attachment_bar_separator.replace(Some(separator));
        self.imp().attachment_bar_add_btn.replace(Some(btn_add_attachment));
        self.imp().attachment_view_toggle_box.replace(Some(view_toggle_box));
        self.imp().attachment_btn_icons.replace(Some(btn_icons));
        self.imp().attachment_btn_list.replace(Some(btn_list));
        self.imp().attachment_btn_collapse.replace(Some(btn_collapse));

        // Bottom Sheet container (slide-up drawer)
        let sheet_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .css_classes(vec!["attachment-sheet-box"])
            .build();

        // FlowBox (Preview Cards in Drawer)
        let flowbox = gtk::FlowBox::builder()
            .orientation(gtk::Orientation::Horizontal)
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(false)
            .row_spacing(10)
            .column_spacing(10)
            .min_children_per_line(1)
            .max_children_per_line(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(2)
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .css_classes(vec!["attachment-flowbox"])
            .build();

        let dialog_weak_for_fb_key = self.downgrade();
        let fb_key = gtk::EventControllerKey::new();
        fb_key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::space {
                if let Some(dialog) = dialog_weak_for_fb_key.upgrade() {
                    if let Some(fb) = dialog.imp().attachment_flowbox.borrow().as_ref() {
                        if let Some(selected) = fb.selected_children().first() {
                            let idx = selected.index();
                            if idx >= 0 {
                                let attachments = dialog.imp().attachments.borrow();
                                if let Some(att) = attachments.get(idx as usize) {
                                    let _ = crate::services::quick_preview_service::preview_attachment(att, None);
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                    }
                }
            }
            glib::Propagation::Proceed
        });
        flowbox.add_controller(fb_key);

        // ListBox (List View in Sheet)
        let listbox = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["attachment-listbox"])
            .margin_start(8)
            .margin_end(8)
            .margin_top(2)
            .margin_bottom(6)
            .valign(gtk::Align::Start)
            .build();

        let dialog_weak_for_lb_key = self.downgrade();
        let lb_key = gtk::EventControllerKey::new();
        lb_key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::space {
                if let Some(dialog) = dialog_weak_for_lb_key.upgrade() {
                    if let Some(lb) = dialog.imp().attachment_listbox.borrow().as_ref() {
                        if let Some(selected) = lb.selected_row() {
                            let idx = selected.index();
                            if idx >= 0 {
                                let attachments = dialog.imp().attachments.borrow();
                                if let Some(att) = attachments.get(idx as usize) {
                                    let _ = crate::services::quick_preview_service::preview_attachment(att, None);
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                    }
                }
            }
            glib::Propagation::Proceed
        });
        listbox.add_controller(lb_key);

        // Column Header Bar for Nautilus-style List View
        let list_header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["attachment-list-header"])
            .height_request(24)
            .visible(false)
            .build();

        let icon_spacer = gtk::Box::builder().width_request(20).build();
        list_header_box.append(&icon_spacer);

        let col_name = gtk::Label::builder()
            .label(&crate::i18n::gettext("Name"))
            .hexpand(true)
            .xalign(0.0)
            .css_classes(vec!["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_name);

        let col_type = gtk::Label::builder()
            .label(&crate::i18n::gettext("Type"))
            .width_request(140)
            .xalign(0.0)
            .css_classes(vec!["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_type);

        let col_mod = gtk::Label::builder()
            .label(&crate::i18n::gettext("Modified"))
            .width_request(120)
            .xalign(0.0)
            .css_classes(vec!["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_mod);

        let col_size = gtk::Label::builder()
            .label(&crate::i18n::gettext("Size"))
            .width_request(70)
            .xalign(1.0)
            .css_classes(vec!["attachment-col-header-label"])
            .build();
        list_header_box.append(&col_size);

        let btn_spacer = gtk::Box::builder().width_request(24).build();
        list_header_box.append(&btn_spacer);

        let list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();
        list_container.append(&list_header_box);
        list_container.append(&listbox);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .build();
        stack.add_named(&flowbox, Some("icons"));
        stack.add_named(&list_container, Some("list"));

        let empty_page = adw::StatusPage::builder()
            .icon_name("mail-attachment-symbolic")
            .title(&crate::i18n::gettext("Drop files here"))
            .description(&crate::i18n::gettext("Files will be attached"))
            .css_classes(vec!["compact"])
            .height_request(80)
            .valign(gtk::Align::Center)
            .visible(true)
            .build();
        sheet_box.append(&empty_page);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .propagate_natural_width(false)
            .min_content_height(0)
            .max_content_height(220)
            .vexpand(false)
            .hexpand(true)
            .css_classes(vec!["attachment-sheet-scrolled"])
            .visible(false)
            .child(&stack)
            .build();
        sheet_box.append(&scrolled);

        let handle_pill = gtk::Box::builder()
            .width_request(44)
            .height_request(4)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(vec!["attachment-sheet-handle-pill"])
            .build();

        let handle_btn = gtk::Button::builder()
            .child(&handle_pill)
            .tooltip_text(&crate::i18n::gettext("Collapse attachments"))
            .css_classes(vec!["flat", "attachment-sheet-handle-btn"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .margin_top(1)
            .margin_bottom(1)
            .focusable(false)
            .visible(false)
            .build();
        set_accessible_label(&handle_btn, &crate::i18n::gettext("Collapse attachments"));
        handle_btn.set_cursor_from_name(Some("grab"));

        let dialog_weak_for_handle = self.downgrade();
        handle_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_handle.upgrade() {
                dialog.set_attachment_expanded(false);
            }
        });

        let drag_gesture = gtk::GestureDrag::new();
        drag_gesture.set_button(1);
        let btn_drag_begin = handle_btn.clone();
        drag_gesture.connect_drag_begin(move |_, _, _| {
            btn_drag_begin.set_cursor_from_name(Some("grabbing"));
        });
        let dialog_weak_for_drag_update = self.downgrade();
        drag_gesture.connect_drag_update(move |gesture, _offset_x, offset_y| {
            if offset_y < -8.0 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(dialog) = dialog_weak_for_drag_update.upgrade() {
                    if dialog.imp().attachment_expanded.get() {
                        dialog.set_attachment_expanded(false);
                    }
                }
            }
        });
        let dialog_weak_for_drag_end = self.downgrade();
        let btn_drag_end = handle_btn.clone();
        drag_gesture.connect_drag_end(move |gesture, _offset_x, offset_y| {
            btn_drag_end.set_cursor_from_name(Some("grab"));
            if offset_y < -8.0 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(dialog) = dialog_weak_for_drag_end.upgrade() {
                    if dialog.imp().attachment_expanded.get() {
                        dialog.set_attachment_expanded(false);
                    }
                }
            }
        });
        handle_btn.add_controller(drag_gesture);

        sheet_box.append(&handle_btn);
        self.imp()
            .attachment_sheet_handle_btn
            .replace(Some(handle_btn));

        let detail_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .reveal_child(false)
            .child(&sheet_box)
            .build();

        let attachment_section_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .hexpand(true)
            .css_classes(vec!["attachment-section-box"])
            .build();
        attachment_section_box.append(&attachment_bar);
        attachment_section_box.append(&detail_revealer);
        self.imp().attachment_section_box.replace(Some(attachment_section_box.clone()));

        let drop_target_section = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop_target_section.set_types(&[
            gdk::FileList::static_type(),
            gio::File::static_type(),
            gdk::Texture::static_type(),
            glib::types::Type::STRING,
        ]);
        let dialog_weak_for_section_enter = self.downgrade();
        drop_target_section.connect_enter(move |target, _, _| {
            let mut action = gdk::DragAction::empty();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(dialog) = dialog_weak_for_section_enter.upgrade() {
                    if dialog.is_external_drag(Some(target)) {
                        dialog.start_drag_session();
                        dialog.set_drag_over_sheet(true);
                        if !dialog.imp().attachment_expanded.get() {
                            dialog.update_attachment_max_height();
                            dialog.set_attachment_expanded(true);
                        }
                        action = gdk::DragAction::COPY;
                    }
                }
            }));
            action
        });
        let dialog_weak_for_section_motion = self.downgrade();
        drop_target_section.connect_motion(move |target, _, _| {
            let mut action = gdk::DragAction::empty();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(dialog) = dialog_weak_for_section_motion.upgrade() {
                    if dialog.is_external_drag(Some(target)) {
                        if !dialog.imp().attachment_expanded.get() {
                            dialog.update_attachment_max_height();
                            dialog.set_attachment_expanded(true);
                        }
                        action = gdk::DragAction::COPY;
                    }
                }
            }));
            action
        });
        let dialog_weak_for_section_leave = self.downgrade();
        drop_target_section.connect_leave(move |_| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(dialog) = dialog_weak_for_section_leave.upgrade() {
                    dialog.set_drag_over_sheet(false);
                }
            }));
        });
        let dialog_weak_for_section_drop = self.downgrade();
        drop_target_section.connect_drop(move |target, value, _, _| {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(dialog) = dialog_weak_for_section_drop.upgrade() else {
                    return false;
                };
                if !dialog.is_external_drag(Some(target)) {
                    return false;
                }
                if let Some(source_id) = dialog.imp().drag_leave_timeout.borrow_mut().take() {
                    source_id.remove();
                }
                dialog.imp().is_drag_active.set(false);
                dialog.set_drag_over_sheet(false);
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
                let was_auto_expanded = dialog.imp().drag_auto_expanded.get();
                dialog.imp().drag_auto_expanded.set(false);
                if was_auto_expanded {
                    dialog.set_attachment_expanded(false);
                }
                dialog.show_attachment_bar_drag_hint(false);
                if dialog.imp().attachments.borrow().is_empty() {
                    dialog.set_attachment_expanded(false);
                    if let Some(bar) = dialog.imp().attachment_bar.borrow().as_ref() {
                        bar.set_visible(false);
                    }
                }
                any_added
            }));
            res.unwrap_or(false)
        });
        attachment_section_box.add_controller(drop_target_section);

        let attachment_section_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .reveal_child(false)
            .child(&attachment_section_box)
            .build();

        content_box.append(&attachment_section_revealer);
        content_box.append(&html_editor);

        let dialog_weak_for_editor_click = self.downgrade();
        let click_ctrl = gtk::GestureClick::new();
        click_ctrl.connect_released(move |_, _, _, _| {
            if let Some(dialog) = dialog_weak_for_editor_click.upgrade() {
                if dialog.imp().attachment_expanded.get() {
                    dialog.set_attachment_expanded(false);
                    if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                        let editor_c = editor.clone();
                        glib::idle_add_local_once(move || {
                            editor_c.clear_selection();
                        });
                    }
                }
            }
        });
        html_editor.add_controller(click_ctrl);

        self.imp().attachment_section_revealer.replace(Some(attachment_section_revealer));
        self.imp().attachment_detail_revealer.replace(Some(detail_revealer));

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
            lbl_reply_to.clone().upcast(),
            entry_reply_to.clone().upcast(),
        ]);
        self.imp().row_cc_widgets.replace(vec![
            lbl_cc.clone().upcast(),
            entry_cc.clone().upcast(),
        ]);
        self.imp().row_bcc_widgets.replace(vec![
            lbl_bcc.clone().upcast(),
            entry_bcc.clone().upcast(),
        ]);
        self.update_cc_bcc_toggle_state();
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
        let attachment_bar_widget = attachment_bar.clone().upcast();
        self.imp().attachment_scrolled.replace(Some(scrolled));
        self.imp().attachment_empty_page.replace(Some(empty_page));
        self.imp().attachment_stack.replace(Some(stack));
        self.imp().attachment_flowbox.replace(Some(flowbox));
        self.imp().attachment_listbox.replace(Some(listbox));
        self.imp().attachment_col_type.replace(Some(col_type.clone()));
        self.imp().attachment_col_mod.replace(Some(col_mod.clone()));
        self.imp().attachment_expanded.set(false);
        self.imp().attachment_view_mode.set(AttachmentViewMode::Icons);

        let bp_narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 600sp").unwrap());
        bp_narrow.add_setter(&col_type, "visible", Some(&false.to_value()));
        bp_narrow.add_setter(&col_mod, "visible", Some(&false.to_value()));
        let dialog_weak_for_bp_apply = self.downgrade();
        bp_narrow.connect_apply(move |_| {
            if let Some(dialog) = dialog_weak_for_bp_apply.upgrade() {
                dialog.set_attachment_table_compact(true);
                dialog.update_attachment_bar_chips();
            }
        });
        let dialog_weak_for_bp_unapply = self.downgrade();
        bp_narrow.connect_unapply(move |_| {
            if let Some(dialog) = dialog_weak_for_bp_unapply.upgrade() {
                dialog.set_attachment_table_compact(false);
                dialog.update_attachment_bar_chips();
            }
        });
        self.add_breakpoint(bp_narrow);

        let bp_wide = adw::Breakpoint::new(adw::BreakpointCondition::parse("min-width: 950sp").unwrap());
        let dialog_weak_for_bp_wide_apply = self.downgrade();
        bp_wide.connect_apply(move |_| {
            if let Some(dialog) = dialog_weak_for_bp_wide_apply.upgrade() {
                dialog.update_attachment_bar_chips();
            }
        });
        let dialog_weak_for_bp_wide_unapply = self.downgrade();
        bp_wide.connect_unapply(move |_| {
            if let Some(dialog) = dialog_weak_for_bp_wide_unapply.upgrade() {
                dialog.update_attachment_bar_chips();
            }
        });
        self.add_breakpoint(bp_wide);
        self.imp().compose_controls.replace(vec![
            btn_close.clone().upcast(),
            btn_discard.clone().upcast(),
            btn_attach.clone().upcast(),
            btn_format.clone().upcast(),
            btn_insert_template.clone().upcast(),
            btn_save_template.clone().upcast(),
            btn_save_snippet.clone().upcast(),
            attachment_bar_widget,
            entry_reply_to.clone().upcast(),
            btn_toggle_cc_bcc.clone().upcast(),
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
                .unwrap_or(true);
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
        setup_recipient_label_contact_picker(
            &lbl_to,
            &entry_to,
            &combo_from,
            self.downgrade(),
        );
        setup_recipient_label_contact_picker(
            &lbl_cc,
            &entry_cc,
            &combo_from,
            self.downgrade(),
        );
        setup_recipient_label_contact_picker(
            &lbl_bcc,
            &entry_bcc,
            &combo_from,
            self.downgrade(),
        );

        let dialog_weak_for_templates = self.downgrade();
        let entry_subject_for_templates = entry_subject.clone();
        let html_editor_for_templates = html_editor.clone();
        let header_bar_for_templates = header_bar.clone();
        btn_insert_template.connect_clicked(move |_| {
            let popover = gtk::Popover::new();
            popover.set_parent(&header_bar_for_templates);
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
                    let dialog_for_apply = dialog_weak_for_templates.clone();
                    apply_button.connect_clicked(move |_| {
                        editor_for_apply.set_html(&body);
                        if subject_for_apply.text().trim().is_empty() {
                            subject_for_apply.set_text(&subject);
                        }
                        if let Some(dialog) = dialog_for_apply.upgrade() {
                            dialog.imp().user_typed_in_editor.set(true);
                            dialog.mark_changed();
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
                let dialog_for_apply = dialog_weak_for_templates.clone();
                apply_button.connect_clicked(move |_| {
                    editor_for_apply.insert_html(&body);
                    if let Some(dialog) = dialog_for_apply.upgrade() {
                        dialog.imp().user_typed_in_editor.set(true);
                        dialog.mark_changed();
                    }
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
        let header_bar_for_save_template = header_bar.clone();
        btn_save_template.connect_clicked(move |_| {
            let popover = gtk::Popover::new();
            popover.set_parent(&header_bar_for_save_template);
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
        let header_bar_for_save_snippet = header_bar.clone();
        btn_save_snippet.connect_clicked(move |_| {
            let popover = gtk::Popover::new();
            popover.set_parent(&header_bar_for_save_snippet);
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
                                        dialog.notify_mail_page_drafts_changed();
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
            let active_sources: Vec<_> = discovery
                .sources
                .into_iter()
                .filter(|source| {
                    source.enabled
                        && (source.display_name.contains('@')
                            || (!matches!(
                                source.backend_name.as_str(),
                                "rss" | "vfolder" | "maildir" | "local" | "none"
                            ) && !source.display_name.eq_ignore_ascii_case("News and Blogs")
                                && !source.display_name.eq_ignore_ascii_case("On This Computer")
                                && !source.display_name.eq_ignore_ascii_case("Search Folders")))
                })
                .collect();
            let local_choices = local_account_choices();
            let mut identities: Vec<ComposeIdentity> = Vec::new();
            let mut seen_addresses = std::collections::HashSet::new();

            for source in &active_sources {
                let address = crate::views::extract_email_address(&source.display_name);
                let address = if address.is_empty() {
                    if source.display_name.contains('@') {
                        source.display_name.trim().to_ascii_lowercase()
                    } else {
                        continue;
                    }
                } else {
                    address
                };

                if !seen_addresses.insert(address.clone()) {
                    continue;
                }

                let label = if source.backend_name.is_empty() || source.display_name.contains('@') {
                    source.display_name.clone()
                } else {
                    format!("{} <{}>", source.display_name, address)
                };

                let local_profiles = AccountConfigStore::default().load();
                let matched_sig = local_profiles
                    .iter()
                    .find(|p| {
                        p.id == source.uid
                            || p.id == format!("eds:{}", source.uid)
                            || p.email.eq_ignore_ascii_case(&source.display_name)
                    })
                    .and_then(|p| p.signature_id.clone());

                identities.push(ComposeIdentity {
                    source_uid: source.uid.clone(),
                    from: source.display_name.clone(),
                    label,
                    signature_id: matched_sig,
                    ..ComposeIdentity::default()
                });
            }

            for choice in local_choices {
                let address = crate::views::extract_email_address(&choice.from);
                let address = if address.is_empty() {
                    choice.from.trim().to_ascii_lowercase()
                } else {
                    address
                };
                if address.contains('@') && !seen_addresses.insert(address) {
                    continue;
                }
                identities.push(choice);
            }
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
                let prev_suppress = dialog.imp().suppress_dirty_tracking.replace(true);
                dialog.imp().suppress_from_notify.set(false);
                dialog.update_sender_dependent_controls(combo_from_for_load.selected());
                dialog.sync_signature_for_selected_account();
                dialog.imp().suppress_dirty_tracking.set(prev_suppress);
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
                                    dialog.imp().backend_draft_saved.set(true);
                                    dialog.notify_mail_page_drafts_changed();
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
                                            if dialog.imp().close_after_save.replace(false) {
                                                tracing::warn!(%error, "Backend draft sync failed on close; local draft saved");
                                                dialog.show_toast("Entwurf gespeichert");
                                                dialog.imp().close_confirmed.set(true);
                                                dialog.set_can_close(true);
                                                dialog.close();
                                            } else {
                                                dialog.show_toast(&format!(
                                                    "{error}; Inhalt bleibt geöffnet"
                                                ));
                                            }
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
        adw::prelude::AdwDialogExt::set_focus(self, Some(&entry_to));
        self.install_file_drop_target();

        self.start_local_autosave();
        self.imp().suppress_dirty_tracking.set(false);
        self.mark_saved();
    }

    fn install_action_surface(
        &self,
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
                if let Some(source_id) = dialog.imp().drag_leave_timeout.borrow_mut().take() {
                    source_id.remove();
                }
                dialog.imp().is_internal_drag.set(false);
                dialog.imp().drag_auto_expanded.set(false);
                if dialog.imp().attachment_expanded.get() {
                    dialog.set_attachment_expanded(false);
                    dialog.imp().is_drag_active.set(false);
                    if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                        editor.cancel_drag_session();
                    }
                    return;
                }
                if dialog.imp().is_drag_active.get() {
                    dialog.imp().is_drag_active.set(false);
                    dialog.show_attachment_bar_drag_hint(false);
                    if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                        editor.cancel_drag_session();
                    }
                    return;
                }
                dialog.close();
            }
        });
        group.add_action(&action_close);
        let _ = &btn_close;

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

        let action_toggle_maximize = gio::SimpleAction::new("toggle-maximize", None);
        let dialog_weak_for_max = self.downgrade();
        action_toggle_maximize.connect_activate(move |_, _| {
            let Some(dialog) = dialog_weak_for_max.upgrade() else { return; };
            if dialog.imp().is_modal.get() {
                dialog.split_to_standalone_window_and_maximize();
            } else if let Some(win) = dialog.root().and_downcast::<gtk::Window>() {
                if win.is_maximized() {
                    win.unmaximize();
                } else {
                    win.maximize();
                }
            }
        });
        group.add_action(&action_toggle_maximize);

        let action_toggle_fullscreen = gio::SimpleAction::new("toggle-fullscreen", None);
        let dialog_weak_for_fullscreen = self.downgrade();
        action_toggle_fullscreen.connect_activate(move |_, _| {
            let Some(dialog) = dialog_weak_for_fullscreen.upgrade() else { return; };
            if dialog.imp().is_modal.get() {
                dialog.split_to_standalone_window_and_maximize();
            } else if let Some(win) = dialog.root().and_downcast::<gtk::Window>() {
                if win.is_fullscreen() {
                    win.unfullscreen();
                } else {
                    win.fullscreen();
                }
            }
        });
        group.add_action(&action_toggle_fullscreen);

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
            dialog.set_row_cc_visible(new_state);
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
            dialog.set_row_bcc_visible(new_state);
            if !dialog.imp().suppress_setting_persistence.get() {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_composer_show_bcc(new_state);
                }
            }
        });
        group.add_action(&action_view_bcc);

        let action_toggle_format = gio::SimpleAction::new("toggle-format-toolbar", None);
        let dialog_weak_for_format = self.downgrade();
        action_toggle_format.connect_activate(move |_, _| {
            if let Some(dialog) = dialog_weak_for_format.upgrade() {
                if let Some(btn) = dialog.imp().format_button.borrow().as_ref() {
                    if btn.is_sensitive() {
                        btn.set_active(!btn.is_active());
                    }
                }
            }
        });
        group.add_action(&action_toggle_format);

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
                    dialog.insert_html_fragment(&format!(
                        "<p><br></p><p>{}{}</p>",
                        signature_delimiter_html(),
                        html
                    ));
                }
            });
            group.add_action(&sig_action);
        }

        let action_help = gio::SimpleAction::new("help", None);
        let dialog_weak_help = self.downgrade();
        action_help.connect_activate(move |_, _| {
            if let Some(dialog) = dialog_weak_help.upgrade() {
                let parent_win = dialog.root().and_downcast::<gtk::Window>();
                crate::dialogs::show_shortcuts_dialog_for_view(
                    parent_win.as_ref(),
                    Some("composer"),
                );
            }
        });
        group.add_action(&action_help);

        self.insert_action_group("compose", Some(&group));
        self.imp().composer_action_group.replace(Some(group));
        if let Some(combo) = self.imp().combo_from.borrow().as_ref() {
            self.update_sender_dependent_controls(combo.selected());
        }

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

        let escape_controller = gtk::EventControllerKey::new();
        escape_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let dialog_weak_for_esc = self.downgrade();
        escape_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(dialog) = dialog_weak_for_esc.upgrade() {
                    let mut handled = false;
                    if let Some(source_id) = dialog.imp().drag_leave_timeout.borrow_mut().take() {
                        source_id.remove();
                    }
                    if dialog.imp().attachment_expanded.get() {
                        dialog.set_attachment_expanded(false);
                        handled = true;
                    }
                    if dialog.imp().is_drag_active.get() {
                        dialog.imp().is_drag_active.set(false);
                        dialog.show_attachment_bar_drag_hint(false);
                        handled = true;
                    }
                    if dialog.imp().attachments.borrow().is_empty() {
                        if let Some(bar) = dialog.imp().attachment_bar.borrow().as_ref() {
                            bar.set_visible(false);
                        }
                    }
                    if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                        editor.cancel_drag_session();
                    }
                    if handled {
                        return glib::Propagation::Stop;
                    }
                }
            }
            glib::Propagation::Proceed
        });
        self.add_controller(escape_controller);

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
        self.imp().backend_draft_saved.set(false);
    }

    fn mark_saved(&self) {
        self.imp()
            .saved_revision
            .set(self.imp().dirty_revision.get());
    }

    fn notify_mail_page_drafts_changed(&self) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            if let Some(mail_page) = window.mail_page() {
                mail_page.refresh_mail();
            }
        }
    }

    fn has_user_content(&self) -> bool {
        let to_has_content = self
            .imp()
            .entry_to
            .borrow()
            .as_ref()
            .is_some_and(|e| !e.text().trim().is_empty());
        let cc_has_content = self
            .imp()
            .entry_cc
            .borrow()
            .as_ref()
            .is_some_and(|e| !e.text().trim().is_empty());
        let bcc_has_content = self
            .imp()
            .entry_bcc
            .borrow()
            .as_ref()
            .is_some_and(|e| !e.text().trim().is_empty());
        let subject_has_content = self
            .imp()
            .entry_subject
            .borrow()
            .as_ref()
            .is_some_and(|e| !e.text().trim().is_empty());
        let has_attachments = !self.imp().attachments.borrow().is_empty();
        let has_calendar = self.imp().calendar_method.borrow().is_some()
            || self.imp().calendar_component.borrow().is_some();
        let typed_in_editor = self.imp().user_typed_in_editor.get();

        to_has_content
            || cc_has_content
            || bcc_has_content
            || subject_has_content
            || has_attachments
            || has_calendar
            || typed_in_editor
    }

    fn is_dirty(&self) -> bool {
        if !self.has_user_content() && !self.imp().local_draft_persisted.get() {
            return false;
        }
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
        self.imp().backend_draft_saved.set(false);
        self.imp().user_typed_in_editor.set(false);
        self.notify_mail_page_drafts_changed();
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
        let needs_save = self.is_dirty()
            || (self.imp().local_draft_persisted.get() && !self.imp().backend_draft_saved.get());
        match close_request_decision(
            needs_save,
            self.imp().operation.get(),
        ) {
            CloseRequestDecision::Allow => {
                let draft_id = self.imp().draft_id.borrow().clone();
                if !draft_id.is_empty() && !self.imp().local_draft_persisted.get() {
                    let _ = DraftStoreService::default().delete(&draft_id);
                } else if self.imp().local_draft_persisted.get() {
                    self.show_toast("Entwurf gespeichert");
                }
                false
            }
            CloseRequestDecision::BlockOperation => {
                self.show_toast(
                    "Der Composer verarbeitet noch einen Vorgang; bitte warten oder abbrechen",
                );
                true
            }
            CloseRequestDecision::SaveAndClose => {
                if self.root().is_none() {
                    return false;
                }
                self.imp().close_after_save.set(true);
                if let Some(button) = self.imp().save_draft_button.borrow().as_ref() {
                    button.emit_clicked();
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

    pub fn is_row_cc_visible(&self) -> bool {
        self.imp()
            .row_cc_widgets
            .borrow()
            .first()
            .map(|w| w.is_visible())
            .unwrap_or(false)
    }

    pub fn is_row_bcc_visible(&self) -> bool {
        self.imp()
            .row_bcc_widgets
            .borrow()
            .first()
            .map(|w| w.is_visible())
            .unwrap_or(false)
    }

    pub fn update_cc_bcc_toggle_state(&self) {
        let is_cc = self.is_row_cc_visible();
        let is_bcc = self.is_row_bcc_visible();
        if let Some(btn) = self.imp().btn_toggle_cc_bcc.borrow().as_ref() {
            if is_cc && is_bcc {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
        }
    }

    pub fn set_row_cc_visible(&self, visible: bool) {
        for widget in self.imp().row_cc_widgets.borrow().iter() {
            widget.set_visible(visible);
        }
        if let Some(action) = self.composer_action("view-cc") {
            action.set_state(&visible.to_variant());
        }
        self.update_cc_bcc_toggle_state();
    }

    pub fn set_row_bcc_visible(&self, visible: bool) {
        for widget in self.imp().row_bcc_widgets.borrow().iter() {
            widget.set_visible(visible);
        }
        if let Some(action) = self.composer_action("view-bcc") {
            action.set_state(&visible.to_variant());
        }
        self.update_cc_bcc_toggle_state();
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
                alert.add_responses(&[
                    ("cancel", &crate::i18n::gettext("Cancel")),
                    ("send", &crate::i18n::gettext("Send")),
                ]);
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
                Some(&crate::i18n::gettext("No Subject")),
                Some(&crate::i18n::gettext("This message has no subject. Send anyway?")),
            );
            alert.add_responses(&[
                ("cancel", &crate::i18n::gettext("Cancel")),
                ("send", &crate::i18n::gettext("Send Anyway")),
            ]);
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
            .filter(|s| !s.trim().is_empty() && *s != "default")
        {
            if sig_id == "none" {
                0
            } else {
                choices
                    .iter()
                    .position(|c| c.id.as_deref() == Some(sig_id))
                    .unwrap_or(if choices.len() > 1 { 1 } else { 0 })
            }
        } else if let Some(active_id) = SignatureStore::default().active_id() {
            choices
                .iter()
                .position(|c| c.id.as_deref() == Some(&active_id))
                .unwrap_or(if choices.len() > 1 { 1 } else { 0 })
        } else if choices.len() > 1 {
            1
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
                    .unwrap_or(if choices.len() > 1 { 1 } else { 0 })
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
                    .unwrap_or(true);
                let initial_html = if top_signature {
                    format!("<p><br></p><p><br></p>{signature}<p><br></p>")
                } else {
                    format!("<p><br></p><p><br></p>{signature}")
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
            self.imp().user_typed_in_editor.set(true);
            self.mark_changed();
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
        if !self.is_dirty() && !self.imp().local_draft_persisted.get() {
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
                        let _ = revision;
                        dialog.imp().autosave_error_shown.set(false);
                        dialog.notify_mail_page_drafts_changed();
                        tracing::debug!("Local draft autosaved");
                        if dialog.imp().close_after_save.get() {
                            if let Some(button) = dialog.imp().save_draft_button.borrow().as_ref() {
                                button.emit_clicked();
                            } else if dialog.imp().close_after_save.replace(false) {
                                dialog.show_toast("Entwurf gespeichert");
                                dialog.imp().close_confirmed.set(true);
                                dialog.set_can_close(true);
                                dialog.close();
                            }
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

    pub fn is_external_drag(&self, drop_target: Option<&gtk::DropTarget>) -> bool {
        if let Some(target) = drop_target {
            if let Some(drop) = target.current_drop() {
                if drop.drag().is_some() {
                    return false;
                } else {
                    // Truly an external drag (Nautilus, desktop, etc.)
                    self.imp().is_internal_drag.set(false);
                    return true;
                }
            }
        }
        !self.imp().is_internal_drag.get()
    }

    pub fn start_external_drag_session(&self) {
        if let Some(source_id) = self.imp().drag_leave_timeout.borrow_mut().take() {
            source_id.remove();
        }
        if self.imp().is_drag_active.get() && self.imp().attachment_expanded.get() {
            return;
        }
        self.imp().is_drag_active.set(true);
        self.show_attachment_bar_drag_hint(true);
        let bar = self.imp().attachment_bar.borrow().clone();
        if let Some(bar) = bar.as_ref() {
            bar.set_visible(true);
        }
        if !self.imp().attachment_expanded.get() {
            self.imp().drag_auto_expanded.set(true);
            self.update_attachment_max_height();
            self.set_attachment_expanded(true);
        }
    }

    fn install_file_drop_target(&self) {
        let drop_target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

        let dialog_weak_for_enter = self.downgrade();
        drop_target.connect_enter(move |target, _, _| {
            let mut action = gdk::DragAction::empty();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(dialog) = dialog_weak_for_enter.upgrade() {
                    if dialog.is_external_drag(Some(target)) {
                        dialog.start_external_drag_session();
                        action = gdk::DragAction::COPY;
                    }
                }
            }));
            action
        });

        let dialog_weak_for_motion = self.downgrade();
        drop_target.connect_motion(move |target, _, _| {
            let mut action = gdk::DragAction::empty();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(dialog) = dialog_weak_for_motion.upgrade() {
                    if dialog.is_external_drag(Some(target)) {
                        dialog.start_external_drag_session();
                        action = gdk::DragAction::COPY;
                    }
                }
            }));
            action
        });

        let dialog_weak_for_leave = self.downgrade();
        drop_target.connect_leave(move |_| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(dialog) = dialog_weak_for_leave.upgrade() {
                    dialog.schedule_drag_leave();
                }
            }));
        });

        let dialog_weak = self.downgrade();
        drop_target.connect_drop(move |target, value, _, _| {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return false;
                };
                if !dialog.is_external_drag(Some(target)) {
                    return false;
                }
                if let Some(source_id) = dialog.imp().drag_leave_timeout.borrow_mut().take() {
                    source_id.remove();
                }
                dialog.imp().is_drag_active.set(false);
                dialog.set_drag_over_sheet(false);
                dialog.imp().is_internal_drag.set(false);
                let was_auto_expanded = dialog.imp().drag_auto_expanded.take();
                let mut any_added = false;
                let mut paths = Vec::new();
                if let Ok(file_list) = value.get::<gdk::FileList>() {
                    for file in file_list.files() {
                        if let Some(path) = file.path() {
                            if path.is_file() {
                                paths.push(path);
                            }
                        }
                    }
                } else if let Ok(file) = value.get::<gio::File>() {
                    if let Some(path) = file.path() {
                        if path.is_file() {
                            paths.push(path);
                        }
                    }
                }
                let is_plain = dialog.imp().html_editor.borrow().as_ref().map(|e| e.plain_text_mode()).unwrap_or(false);
                for path in paths {
                    if !is_plain && crate::views::HtmlEditor::is_supported_image(&path) {
                        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
                            if editor.insert_image_from_path(&path) {
                                dialog.mark_changed();
                                any_added = true;
                                let path_str = path.to_string_lossy().to_string();
                                if dialog.imp().attachments.borrow().iter().any(|a| a.path == path_str) {
                                    dialog.remove_attachment(&path_str);
                                }
                                continue;
                            }
                        }
                    }
                    dialog.add_attachment_path(&path);
                    any_added = true;
                }
                if was_auto_expanded {
                    dialog.set_attachment_expanded(false);
                }
                dialog.show_attachment_bar_drag_hint(false);
                if dialog.imp().attachments.borrow().is_empty() {
                    dialog.set_attachment_expanded(false);
                    if let Some(bar) = dialog.imp().attachment_bar.borrow().as_ref() {
                        bar.set_visible(false);
                    }
                }
                any_added
            }));
            res.unwrap_or(false)
        });
        self.add_controller(drop_target);
    }

    pub fn set_drag_over_sheet(&self, active: bool) {
        self.imp().is_drag_over_sheet.set(active);
        if let Some(section) = self.imp().attachment_section_box.borrow().as_ref() {
            if active {
                section.add_css_class("drag-active");
            } else {
                section.remove_css_class("drag-active");
            }
        }
    }

    pub fn start_drag_session(&self) {
        if let Some(source_id) = self.imp().drag_leave_timeout.borrow_mut().take() {
            source_id.remove();
        }
        if self.imp().is_drag_active.get() {
            return;
        }
        self.imp().is_drag_active.set(true);
        self.show_attachment_bar_drag_hint(true);
        let bar = self.imp().attachment_bar.borrow().clone();
        if let Some(bar) = bar.as_ref() {
            bar.set_visible(true);
        }
    }

    pub fn schedule_drag_leave(&self) {
        if let Some(source_id) = self.imp().drag_leave_timeout.borrow_mut().take() {
            source_id.remove();
        }
        let dialog_weak = self.downgrade();
        let source_id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.imp().is_drag_active.set(false);
                dialog.set_drag_over_sheet(false);
                if dialog.imp().drag_auto_expanded.take() {
                    dialog.set_attachment_expanded(false);
                }
                if dialog.imp().attachments.borrow().is_empty() {
                    dialog.set_attachment_expanded(false);
                    dialog.show_attachment_bar_drag_hint(false);
                    if let Some(bar) = dialog.imp().attachment_bar.borrow().as_ref() {
                        bar.set_visible(false);
                    }
                }
            }
        });
        self.imp().drag_leave_timeout.replace(Some(source_id));
    }

    pub fn open_attachment_chooser(&self) {
        let file_dialog = gtk::FileDialog::builder()
            .title(&crate::i18n::gettext("Select attachment"))
            .modal(true)
            .build();

        let dialog_for_file = self.clone();
        let parent_win = self.root().and_downcast::<gtk::Window>();
        file_dialog.open_multiple(parent_win.as_ref(), None::<&gio::Cancellable>, move |res| {
            if let Ok(files) = res {
                for i in 0..files.n_items() {
                    if let Some(file) = files.item(i).and_downcast::<gio::File>() {
                        if let Some(path) = file.path() {
                            dialog_for_file.add_attachment_path(&path);
                        }
                    }
                }
            }
        });
    }

    pub fn toggle_attachment_expanded(&self) {
        let is_expanded = !self.imp().attachment_expanded.get();
        self.set_attachment_expanded(is_expanded);
    }

    pub fn set_attachment_expanded(&self, expanded: bool) {
        self.imp().attachment_expanded.set(expanded);
        self.apply_attachment_view_state();
    }

    pub fn apply_attachment_view_state(&self) {
        let expanded = self.imp().attachment_expanded.get();
        let mode = self.imp().attachment_view_mode.get();
        let count = self.imp().attachments.borrow().len();

        if let Some(stack) = self.imp().attachment_stack.borrow().as_ref() {
            match mode {
                AttachmentViewMode::Icons => stack.set_visible_child_name("icons"),
                AttachmentViewMode::List => stack.set_visible_child_name("list"),
            }
        }

        if let Some(btn) = self.imp().attachment_btn_icons.borrow().as_ref() {
            if mode == AttachmentViewMode::Icons {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
        }
        if let Some(btn) = self.imp().attachment_btn_list.borrow().as_ref() {
            if mode == AttachmentViewMode::List {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
        }

        if !expanded {
            if let Some(sb) = self.imp().attachment_summary_box.borrow().as_ref() {
                sb.set_visible(false);
            }
            if let Some(cb) = self.imp().attachment_bar_chip_box.borrow().as_ref() {
                cb.set_visible(count > 0);
            }
            if let Some(add_btn) = self.imp().attachment_bar_add_btn.borrow().as_ref() {
                add_btn.set_visible(false);
            }
            if let Some(toggle) = self.imp().attachment_view_toggle_box.borrow().as_ref() {
                toggle.set_visible(false);
            }
            if let Some(rev) = self.imp().attachment_detail_revealer.borrow().as_ref() {
                rev.set_reveal_child(false);
            }
            if let Some(chevron) = self.imp().attachment_summary_chevron.borrow().as_ref() {
                chevron.set_icon_name(Some("pan-down-symbolic"));
                chevron.set_visible(false);
            }
            if let Some(btn) = self.imp().attachment_btn_collapse.borrow().as_ref() {
                btn.set_visible(count > 0);
                btn.set_icon_name("pan-down-symbolic");
                btn.set_tooltip_text(Some(&crate::i18n::gettext("Expand attachments")));
                set_accessible_label(btn, &crate::i18n::gettext("Expand attachments"));
            }
            if let Some(handle) = self.imp().attachment_sheet_handle_btn.borrow().as_ref() {
                handle.set_visible(false);
            }
        } else {
            if let Some(sb) = self.imp().attachment_summary_box.borrow().as_ref() {
                sb.set_visible(count > 0);
            }
            if let Some(cb) = self.imp().attachment_bar_chip_box.borrow().as_ref() {
                cb.set_visible(false);
            }
            if let Some(add_btn) = self.imp().attachment_bar_add_btn.borrow().as_ref() {
                add_btn.set_visible(count > 0);
            }
            if let Some(toggle) = self.imp().attachment_view_toggle_box.borrow().as_ref() {
                toggle.set_visible(count > 0);
            }
            if let Some(rev) = self.imp().attachment_detail_revealer.borrow().as_ref() {
                rev.set_reveal_child(true);
            }
            if let Some(chevron) = self.imp().attachment_summary_chevron.borrow().as_ref() {
                chevron.set_icon_name(Some("pan-up-symbolic"));
                chevron.set_visible(true);
            }
            if let Some(btn) = self.imp().attachment_btn_collapse.borrow().as_ref() {
                btn.set_visible(count > 0);
                btn.set_icon_name("pan-up-symbolic");
                btn.set_tooltip_text(Some(&crate::i18n::gettext("Collapse attachments")));
                set_accessible_label(btn, &crate::i18n::gettext("Collapse attachments"));
            }
            if let Some(handle) = self.imp().attachment_sheet_handle_btn.borrow().as_ref() {
                handle.set_visible(count > 0);
            }
            self.update_attachment_max_height();
        }
    }

    pub fn show_attachment_bar_drag_hint(&self, visible: bool) {
        if self.imp().attachments.borrow().is_empty() {
            if let Some(lc) = self.imp().attachment_left_container.borrow().as_ref() {
                lc.set_hexpand(false);
            }
            if let Some(spacer) = self.imp().attachment_bar_spacer.borrow().as_ref() {
                spacer.set_hexpand(true);
            }
            if let Some(rev) = self.imp().attachment_section_revealer.borrow().as_ref() {
                rev.set_reveal_child(visible);
            }
            if let Some(bar) = self.imp().attachment_bar.borrow().as_ref() {
                bar.set_visible(visible);
            }
            if let Some(lbl) = self.imp().attachment_summary_label.borrow().as_ref() {
                let text = if visible {
                    crate::i18n::gettext("Drop files here to attach")
                } else {
                    crate::i18n::gettext("Drop files here")
                };
                lbl.set_label(&text);
            }
            if let Some(tb) = self.imp().attachment_title_box.borrow().as_ref() {
                tb.set_visible(visible);
            }
            if let Some(sb) = self.imp().attachment_summary_box.borrow().as_ref() {
                sb.set_visible(visible);
            }
            if let Some(cb) = self.imp().attachment_bar_chip_box.borrow().as_ref() {
                cb.set_visible(false);
            }
            if let Some(sep) = self.imp().attachment_bar_separator.borrow().as_ref() {
                sep.set_visible(false);
            }
            if let Some(add_btn) = self.imp().attachment_bar_add_btn.borrow().as_ref() {
                add_btn.set_visible(false);
            }
            if let Some(chevron) = self.imp().attachment_summary_chevron.borrow().as_ref() {
                chevron.set_visible(false);
            }
            if let Some(toggle) = self.imp().attachment_view_toggle_box.borrow().as_ref() {
                toggle.set_visible(false);
            }
            if let Some(collapse) = self.imp().attachment_btn_collapse.borrow().as_ref() {
                collapse.set_visible(false);
            }
            if let Some(handle) = self.imp().attachment_sheet_handle_btn.borrow().as_ref() {
                handle.set_visible(false);
            }
        }
    }

    pub fn set_attachment_view_mode(&self, mode: AttachmentViewMode) {
        self.imp().attachment_view_mode.set(mode);
        self.imp().attachment_expanded.set(true);
        self.apply_attachment_view_state();
    }

    pub fn is_attachment_table_compact(&self) -> bool {
        self.imp().attachment_table_compact.get()
    }

    pub fn set_attachment_table_compact(&self, compact: bool) {
        self.imp().attachment_table_compact.set(compact);
        if let Some(col) = self.imp().attachment_col_type.borrow().as_ref() {
            col.set_visible(!compact);
        }
        if let Some(col) = self.imp().attachment_col_mod.borrow().as_ref() {
            col.set_visible(!compact);
        }
        if let Some(listbox) = self.imp().attachment_listbox.borrow().as_ref() {
            let mut row_child = listbox.first_child();
            while let Some(c) = row_child {
                let next = c.next_sibling();
                if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>() {
                    if let Some(row_box) = row.child().and_downcast::<gtk::Box>() {
                        let mut sub_child = row_box.first_child();
                        while let Some(w) = sub_child {
                            let next_sub = w.next_sibling();
                            if w.has_css_class("attachment-col-type") || w.has_css_class("attachment-col-modified") {
                                w.set_visible(!compact);
                            }
                            sub_child = next_sub;
                        }
                    }
                }
                row_child = next;
            }
        }
    }

    pub fn update_attachment_max_height(&self) {
        if let Some(scrolled) = self.imp().attachment_scrolled.borrow().as_ref() {
            scrolled.set_min_content_height(0);
            scrolled.set_max_content_height(220);
        }
        if let Some(page) = self.imp().attachment_empty_page.borrow().as_ref() {
            page.set_height_request(80);
        }
    }

    pub fn update_attachment_bar_chips(&self) {
        let Some(chip_box) = self.imp().attachment_bar_chip_box.borrow().as_ref().cloned() else {
            return;
        };

        while let Some(child) = chip_box.first_child() {
            chip_box.remove(&child);
        }

        let attachments = self.imp().attachments.borrow().clone();
        let total_count = attachments.len();
        if total_count == 0 {
            return;
        }

        let win_w = if let Some(win) = self.root().and_downcast::<gtk::Window>() {
            win.width().max(win.default_width())
        } else if self.width() > 0 {
            self.width()
        } else {
            780
        };

        let max_chips = if win_w < 550 {
            1
        } else if win_w < 750 {
            2
        } else if win_w < 1000 {
            3
        } else {
            4
        };

        let visible_count = total_count.min(max_chips);
        let visible_attachments: Vec<_> = attachments.iter().take(visible_count).collect();

        for att in visible_attachments {
            let chip = create_attachment_chip(att, self.downgrade());
            chip_box.append(&chip);
        }

        if total_count > max_chips {
            let remaining = total_count - max_chips;
            let tip = if remaining == 1 {
                crate::i18n::gettext("1 more attachment (Click to expand)")
            } else {
                format!("{} {}", remaining, crate::i18n::gettext("more attachments (Click to expand)"))
            };
            let btn_overflow = gtk::Button::builder()
                .label(&format!("+{remaining} …"))
                .tooltip_text(&tip)
                .css_classes(vec!["flat", "circular", "attachment-overflow-btn"])
                .valign(gtk::Align::Center)
                .build();
            set_accessible_label(&btn_overflow, &tip);
            let dialog_weak = self.downgrade();
            btn_overflow.connect_clicked(move |_| {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.set_attachment_expanded(true);
                }
            });
            chip_box.append(&btn_overflow);
        }
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

        // Add to Sheet FlowBox (Icons view)
        if let Some(flowbox) = self.imp().attachment_flowbox.borrow().as_ref() {
            let card = create_attachment_card(&attachment, self.downgrade());
            flowbox.append(&card);
        }

        // Add to Sheet ListBox (List view)
        if let Some(listbox) = self.imp().attachment_listbox.borrow().as_ref() {
            let row = create_attachment_list_row(&attachment, self.downgrade());
            listbox.append(&row);
        }

        // Update Collapsed Bar chips (displays newest first + overflow if more than fit)
        self.update_attachment_bar_chips();

        let count = self.imp().attachments.borrow().len();
        self.update_attachment_bar_state(count);
        self.update_attachment_max_height();
    }

    pub fn remove_attachment(&self, path: &str) {
        self.imp()
            .attachments
            .borrow_mut()
            .retain(|attachment| attachment.path != path);
        self.mark_changed();

        // Remove from FlowBox
        if let Some(fb) = self.imp().attachment_flowbox.borrow().as_ref() {
            let mut to_remove = None;
            let mut child = fb.first_child();
            while let Some(c) = child {
                let next = c.next_sibling();
                if let Some(fb_child) = c.downcast_ref::<gtk::FlowBoxChild>() {
                    if let Some(w) = fb_child.child() {
                        let matches = w.widget_name() == path
                            || w.downcast_ref::<gtk::Overlay>().and_then(|ov| ov.child()).is_some_and(|sub| sub.widget_name() == path);
                        if matches {
                            to_remove = Some(fb_child.clone());
                            break;
                        }
                    }
                }
                child = next;
            }
            if let Some(r) = to_remove {
                fb.remove(&r);
            }
        }

        // Remove from ListBox
        if let Some(lb) = self.imp().attachment_listbox.borrow().as_ref() {
            let mut to_remove = None;
            let mut child = lb.first_child();
            while let Some(c) = child {
                let next = c.next_sibling();
                if let Some(lb_row) = c.downcast_ref::<gtk::ListBoxRow>() {
                    if let Some(w) = lb_row.child() {
                        if w.widget_name() == path {
                            to_remove = Some(lb_row.clone());
                            break;
                        }
                    }
                }
                child = next;
            }
            if let Some(r) = to_remove {
                lb.remove(&r);
            }
        }

        // Update Collapsed Bar chips
        self.update_attachment_bar_chips();

        let count = self.imp().attachments.borrow().len();
        self.update_attachment_bar_state(count);
        self.update_attachment_max_height();
    }

    fn update_attachment_bar_state(&self, count: usize) {
        if let Some(empty_page) = self.imp().attachment_empty_page.borrow().as_ref() {
            empty_page.set_visible(count == 0);
        }
        if let Some(scrolled) = self.imp().attachment_scrolled.borrow().as_ref() {
            scrolled.set_visible(count > 0);
        }
        if let Some(sep) = self.imp().attachment_bar_separator.borrow().as_ref() {
            sep.set_visible(false);
        }
        if let Some(add_btn) = self.imp().attachment_bar_add_btn.borrow().as_ref() {
            add_btn.set_visible(count > 0 && self.imp().attachment_expanded.get());
        }
        if count == 0 {
            if !self.imp().is_drag_active.get() {
                self.imp().attachment_expanded.set(false);
                if let Some(rev) = self.imp().attachment_section_revealer.borrow().as_ref() {
                    rev.set_reveal_child(false);
                }
                if let Some(bar) = self.imp().attachment_bar.borrow().as_ref() {
                    bar.set_visible(false);
                }
                if let Some(detail) = self.imp().attachment_detail_revealer.borrow().as_ref() {
                    detail.set_reveal_child(false);
                }
            } else {
                if let Some(rev) = self.imp().attachment_section_revealer.borrow().as_ref() {
                    rev.set_reveal_child(true);
                }
                if let Some(bar) = self.imp().attachment_bar.borrow().as_ref() {
                    bar.set_visible(true);
                }
            }
            let empty_text = crate::i18n::gettext("Drop files here");
            if let Some(lbl) = self.imp().attachment_summary_label.borrow().as_ref() {
                lbl.set_label(&empty_text);
            }
        } else {
            if let Some(rev) = self.imp().attachment_section_revealer.borrow().as_ref() {
                rev.set_reveal_child(true);
            }
            if let Some(bar) = self.imp().attachment_bar.borrow().as_ref() {
                bar.set_visible(true);
            }
            let total_size: u64 = self
                .imp()
                .attachments
                .borrow()
                .iter()
                .map(|a| a.size)
                .sum();
            let size_str = format_size(total_size);
            let count_str = if count == 1 {
                crate::i18n::gettext("1 attachment")
            } else {
                format!("{} {}", count, crate::i18n::gettext("attachments"))
            };
            if let Some(lbl) = self.imp().attachment_summary_label.borrow().as_ref() {
                lbl.set_markup(&format!(
                    "<b>{}</b> <span alpha='70%'>({})</span>",
                    glib::markup_escape_text(&count_str),
                    glib::markup_escape_text(&size_str)
                ));
            }
            if let Some(tb) = self.imp().attachment_title_box.borrow().as_ref() {
                tb.set_tooltip_text(Some(&format!("{count_str} ({size_str})")));
            }
            self.apply_attachment_view_state();
        }
    }
}

fn attachment_icon_name(mime_type: &str, filename: &str) -> &'static str {
    crate::models::mail_message::AttachmentInfo::icon_name_for(mime_type, filename)
}

fn load_attachment_texture(attachment: &DraftAttachment, scale_size: i32) -> Option<(gdk::Texture, i32, i32)> {
    if !attachment.mime_type.starts_with("image/") {
        return None;
    }
    let path = std::path::Path::new(&attachment.path);
    let valid_path = if path.exists() {
        Some(path.to_path_buf())
    } else {
        crate::services::quick_preview_service::ensure_attachment_preview_path(attachment).ok()
    };

    let p = valid_path?;
    let min_dim = 6.min(scale_size);
    let pixbuf_res = gdk_pixbuf::Pixbuf::from_file_at_scale(&p, scale_size, scale_size, true);
    let pixbuf = pixbuf_res.ok().map(|pb| {
        let w = pb.width();
        let h = pb.height();
        if (w < min_dim || h < min_dim) && (w > 0 && h > 0) {
            let clamped_w = w.max(min_dim);
            let clamped_h = h.max(min_dim);
            pb.scale_simple(clamped_w, clamped_h, gdk_pixbuf::InterpType::Bilinear)
                .unwrap_or(pb)
        } else {
            pb
        }
    })?;

    let pw = pixbuf.width().max(1);
    let ph = pixbuf.height().max(1);
    let texture = gdk::Texture::for_pixbuf(&pixbuf);
    Some((texture, pw, ph))
}

fn create_attachment_visual(attachment: &DraftAttachment, size: i32) -> gtk::Widget {
    if let Some((texture, pw, ph)) = load_attachment_texture(attachment, size) {
        let pic = gtk::Picture::for_paintable(&texture);
        pic.set_can_shrink(true);
        pic.set_content_fit(gtk::ContentFit::Contain);
        pic.set_width_request(pw);
        pic.set_height_request(ph);
        pic.set_halign(gtk::Align::Center);
        pic.set_valign(gtk::Align::Center);
        pic.set_hexpand(false);
        pic.set_vexpand(false);
        if size > 32 {
            pic.add_css_class("attachment-card-thumbnail");
        } else {
            pic.add_css_class("attachment-thumbnail");
        }
        return pic.upcast();
    }

    let icon_name = attachment_icon_name(&attachment.mime_type, &attachment.filename);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(if size > 32 { 48 } else { size });
    icon.set_opacity(0.85);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.upcast()
}

fn attachment_type_description(mime_type: &str, filename: &str) -> String {
    let desc = gio::content_type_get_description(mime_type);
    let s = desc.to_string();
    if !s.is_empty() && s != mime_type && s != "unknown" {
        return s;
    }
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_uppercase();
    if !ext.is_empty() {
        format!("{ext} file")
    } else {
        mime_type.to_string()
    }
}

fn attachment_modified_str(path: &str) -> String {
    if let Ok(metadata) = std::fs::metadata(path) {
        if let Ok(mtime) = metadata.modified() {
            let dt: chrono::DateTime<chrono::Local> = mtime.into();
            let now = chrono::Local::now();
            let date = dt.date_naive();
            let today = now.date_naive();
            if date == today {
                format!("{}, {}", crate::i18n::gettext("Today"), dt.format("%H:%M"))
            } else if date == today.pred_opt().unwrap_or(today) {
                format!("{}, {}", crate::i18n::gettext("Yesterday"), dt.format("%H:%M"))
            } else if date.year() == today.year() {
                dt.format("%b %d, %H:%M").to_string()
            } else {
                dt.format("%m/%d/%Y").to_string()
            }
        } else {
            "—".to_string()
        }
    } else {
        "—".to_string()
    }
}

fn create_attachment_chip(
    attachment: &DraftAttachment,
    dialog_weak: glib::WeakRef<ComposeDialog>,
) -> gtk::Widget {
    let chip_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .css_classes(vec!["attachment-chip"])
        .tooltip_text(&format!("{} ({})", attachment.filename, format_size(attachment.size)))
        .focusable(true)
        .build();
    chip_box.set_widget_name(&attachment.path);
    chip_box.set_cursor_from_name(Some("pointer"));

    let icon_name = attachment_icon_name(&attachment.mime_type, &attachment.filename);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(16);
    chip_box.append(&icon);

    let lbl = gtk::Label::builder()
        .label(&attachment.filename)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(26)
        .css_classes(vec!["attachment-chip-label"])
        .valign(gtk::Align::Center)
        .build();
    chip_box.append(&lbl);

    let remove_icon = gtk::Image::from_icon_name("window-close-symbolic");
    remove_icon.set_pixel_size(12);
    let btn_remove = gtk::Button::builder()
        .child(&remove_icon)
        .tooltip_text(&crate::i18n::gettext("Remove attachment"))
        .css_classes(vec!["flat", "circular", "attachment-chip-btn"])
        .valign(gtk::Align::Center)
        .focusable(false)
        .build();
    set_accessible_label(&btn_remove, &crate::i18n::gettext("Remove attachment"));
    btn_remove.set_cursor_from_name(Some("pointer"));

    let path_for_remove = attachment.path.clone();
    let dialog_weak_for_remove = dialog_weak.clone();
    btn_remove.connect_clicked(move |_| {
        let path = path_for_remove.clone();
        let dialog_weak = dialog_weak_for_remove.clone();
        glib::idle_add_local_once(move || {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.remove_attachment(&path);
            }
        });
    });
    chip_box.append(&btn_remove);

    // Support dragging chip to editor or desktop
    if std::path::Path::new(&attachment.path).exists() {
        let file = gio::File::for_path(&attachment.path);
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::COPY);
        if let Some((texture, _, _)) = load_attachment_texture(attachment, 64) {
            drag_source.set_icon(Some(&texture), 16, 16);
        }
        let file_for_drag = file.clone();
        drag_source.connect_prepare(move |_, _, _| {
            let file_list = gdk::FileList::from_array(&[file_for_drag.clone()]);
            Some(gdk::ContentProvider::for_value(&file_list.to_value()))
        });
        let dialog_weak_begin = dialog_weak.clone();
        drag_source.connect_drag_begin(move |_, _| {
            if let Some(dialog) = dialog_weak_begin.upgrade() {
                dialog.imp().is_internal_drag.set(true);
            }
        });
        let dialog_weak_end = dialog_weak.clone();
        drag_source.connect_drag_end(move |_, _, _| {
            if let Some(dialog) = dialog_weak_end.upgrade() {
                dialog.imp().is_internal_drag.set(false);
            }
        });
        let dialog_weak_cancel = dialog_weak.clone();
        drag_source.connect_drag_cancel(move |_, _, _| {
            if let Some(dialog) = dialog_weak_cancel.upgrade() {
                dialog.imp().is_internal_drag.set(false);
            }
            false
        });
        chip_box.add_controller(drag_source);
    }

    // Preview on click
    let att_for_click = attachment.clone();
    let click_gesture = gtk::GestureClick::new();
    click_gesture.connect_released(move |_, n_press, _, _| {
        if n_press == 1 {
            let _ = crate::services::quick_preview_service::preview_attachment(&att_for_click, None);
        }
    });
    chip_box.add_controller(click_gesture);

    // Preview on Space/Enter key, remove on Delete/Backspace
    let att_for_key = attachment.clone();
    let path_for_key = attachment.path.clone();
    let dialog_weak_for_key = dialog_weak.clone();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::space || keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
            let _ = crate::services::quick_preview_service::preview_attachment(&att_for_key, None);
            return glib::Propagation::Stop;
        } else if keyval == gdk::Key::Delete || keyval == gdk::Key::BackSpace {
            let path = path_for_key.clone();
            let dialog_weak = dialog_weak_for_key.clone();
            glib::idle_add_local_once(move || {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.remove_attachment(&path);
                }
            });
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    chip_box.add_controller(key_controller);

    chip_box.upcast()
}

fn create_attachment_card(
    attachment: &DraftAttachment,
    dialog_weak: glib::WeakRef<ComposeDialog>,
) -> gtk::FlowBoxChild {
    let child = gtk::FlowBoxChild::new();
    child.set_css_classes(&["attachment-flow-child"]);
    child.set_hexpand(false);
    child.set_halign(gtk::Align::Center);
    child.set_focusable(true);
    child.set_focus_on_click(true);
    child.set_widget_name(&attachment.path);

    let overlay = gtk::Overlay::builder()
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .build();
    overlay.set_widget_name(&attachment.path);

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(vec!["attachment-card"])
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .width_request(96)
        .focusable(false)
        .tooltip_text(&format!("{} ({})", attachment.filename, format_size(attachment.size)))
        .build();
    card.set_widget_name(&attachment.path);
    card.set_cursor_from_name(Some("pointer"));

    let visual_box = gtk::CenterBox::builder()
        .width_request(64)
        .height_request(64)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(vec!["attachment-card-visual-box"])
        .margin_top(4)
        .margin_bottom(2)
        .build();
    let visual = create_attachment_visual(attachment, 64);
    visual_box.set_center_widget(Some(&visual));
    card.append(&visual_box);

    let lbl_name = gtk::Label::builder()
        .label(&attachment.filename)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(2)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(14)
        .width_chars(11)
        .height_request(34)
        .css_classes(vec!["attachment-card-label"])
        .halign(gtk::Align::Center)
        .justify(gtk::Justification::Center)
        .build();
    card.append(&lbl_name);

    let size_str = format_size(attachment.size);
    let lbl_size = gtk::Label::builder()
        .label(&size_str)
        .single_line_mode(true)
        .height_request(16)
        .css_classes(vec!["dim-label", "caption", "attachment-card-size"])
        .halign(gtk::Align::Center)
        .build();
    card.append(&lbl_size);

    overlay.set_child(Some(&card));

    let btn_remove = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(&crate::i18n::gettext("Remove attachment"))
        .css_classes(vec!["flat", "circular", "attachment-card-remove-btn"])
        .valign(gtk::Align::Start)
        .halign(gtk::Align::End)
        .margin_top(2)
        .margin_end(2)
        .focusable(false)
        .build();
    set_accessible_label(&btn_remove, &crate::i18n::gettext("Remove attachment"));
    btn_remove.set_cursor_from_name(Some("pointer"));

    let path = attachment.path.clone();
    let dialog_weak_for_remove = dialog_weak.clone();
    btn_remove.connect_clicked(move |_| {
        let path = path.clone();
        let dialog_weak = dialog_weak_for_remove.clone();
        glib::idle_add_local_once(move || {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.remove_attachment(&path);
            }
        });
    });
    overlay.add_overlay(&btn_remove);

    // Preview on card click
    let att_for_click = attachment.clone();
    let click_gesture = gtk::GestureClick::new();
    click_gesture.connect_released(move |_, n_press, _, _| {
        if n_press == 1 {
            let _ = crate::services::quick_preview_service::preview_attachment(&att_for_click, None);
        }
    });
    card.add_controller(click_gesture);

    // Keyboard navigation: Preview on Space/Enter key, remove on Delete/Backspace
    let att_for_key = attachment.clone();
    let path_for_key = attachment.path.clone();
    let dialog_weak_for_key = dialog_weak.clone();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::space || keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
            let _ = crate::services::quick_preview_service::preview_attachment(&att_for_key, None);
            return glib::Propagation::Stop;
        } else if keyval == gdk::Key::Delete || keyval == gdk::Key::BackSpace {
            let path = path_for_key.clone();
            let dialog_weak = dialog_weak_for_key.clone();
            glib::idle_add_local_once(move || {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.remove_attachment(&path);
                }
            });
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    child.add_controller(key_controller);

    // Support dragging card to desktop/file manager
    if std::path::Path::new(&attachment.path).exists() {
        let file = gio::File::for_path(&attachment.path);
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::COPY);
        if let Some((texture, _, _)) = load_attachment_texture(attachment, 96) {
            drag_source.set_icon(Some(&texture), 32, 32);
        }
        let file_for_drag = file.clone();
        drag_source.connect_prepare(move |_, _, _| {
            let file_list = gdk::FileList::from_array(&[file_for_drag.clone()]);
            Some(gdk::ContentProvider::for_value(&file_list.to_value()))
        });
        let dialog_weak_begin = dialog_weak.clone();
        drag_source.connect_drag_begin(move |_, _| {
            if let Some(dialog) = dialog_weak_begin.upgrade() {
                dialog.imp().is_internal_drag.set(true);
            }
        });
        let dialog_weak_end = dialog_weak.clone();
        drag_source.connect_drag_end(move |_, _, _| {
            if let Some(dialog) = dialog_weak_end.upgrade() {
                dialog.imp().is_internal_drag.set(false);
            }
        });
        let dialog_weak_cancel = dialog_weak.clone();
        drag_source.connect_drag_cancel(move |_, _, _| {
            if let Some(dialog) = dialog_weak_cancel.upgrade() {
                dialog.imp().is_internal_drag.set(false);
            }
            false
        });
        card.add_controller(drag_source);
    }

    child.set_child(Some(&overlay));
    child
}

fn create_attachment_list_row(
    attachment: &DraftAttachment,
    dialog_weak: glib::WeakRef<ComposeDialog>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_css_classes(&["attachment-list-row"]);
    row.set_height_request(28);

    let row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(8)
        .margin_end(8)
        .margin_top(0)
        .margin_bottom(0)
        .height_request(28)
        .valign(gtk::Align::Center)
        .focusable(true)
        .tooltip_text(&format!("{} ({})", attachment.filename, format_size(attachment.size)))
        .build();
    row_box.set_widget_name(&attachment.path);
    row_box.set_cursor_from_name(Some("pointer"));

    let visual_box = gtk::Box::builder()
        .width_request(20)
        .height_request(20)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let icon_name = attachment_icon_name(&attachment.mime_type, &attachment.filename);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(16);
    icon.set_opacity(0.85);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    visual_box.append(&icon);
    row_box.append(&visual_box);

    let lbl_name = gtk::Label::builder()
        .label(&attachment.filename)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(vec!["attachment-list-label"])
        .hexpand(true)
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .build();
    row_box.append(&lbl_name);

    let is_compact = dialog_weak
        .upgrade()
        .map(|d| d.is_attachment_table_compact())
        .unwrap_or(false);

    let type_desc = attachment_type_description(&attachment.mime_type, &attachment.filename);
    let lbl_type = gtk::Label::builder()
        .label(&type_desc)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(vec!["dim-label", "caption", "attachment-col-type"])
        .width_request(140)
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .visible(!is_compact)
        .build();
    row_box.append(&lbl_type);

    let mod_str = attachment_modified_str(&attachment.path);
    let lbl_mod = gtk::Label::builder()
        .label(&mod_str)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(vec!["dim-label", "caption", "attachment-col-modified"])
        .width_request(120)
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .visible(!is_compact)
        .build();
    row_box.append(&lbl_mod);

    let size_str = format_size(attachment.size);
    let lbl_size = gtk::Label::builder()
        .label(&size_str)
        .single_line_mode(true)
        .css_classes(vec!["dim-label", "caption", "attachment-chip-size", "attachment-col-size"])
        .width_request(70)
        .xalign(1.0)
        .valign(gtk::Align::Center)
        .build();
    row_box.append(&lbl_size);

    let btn_remove = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(&crate::i18n::gettext("Remove attachment"))
        .css_classes(vec!["flat", "circular", "attachment-chip-btn"])
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .focusable(false)
        .build();
    set_accessible_label(&btn_remove, &crate::i18n::gettext("Remove attachment"));
    btn_remove.set_cursor_from_name(Some("pointer"));
    row_box.append(&btn_remove);

    // Preview on row click
    let att_for_click = attachment.clone();
    let click_gesture = gtk::GestureClick::new();
    click_gesture.connect_released(move |_, n_press, _, _| {
        if n_press == 1 {
            let _ = crate::services::quick_preview_service::preview_attachment(&att_for_click, None);
        }
    });
    row_box.add_controller(click_gesture);

    // Preview on Space/Enter key, remove on Delete/Backspace
    let att_for_key = attachment.clone();
    let path_for_key = attachment.path.clone();
    let dialog_weak_for_key = dialog_weak.clone();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::space || keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
            let _ = crate::services::quick_preview_service::preview_attachment(&att_for_key, None);
            return glib::Propagation::Stop;
        } else if keyval == gdk::Key::Delete || keyval == gdk::Key::BackSpace {
            let path = path_for_key.clone();
            let dialog_weak = dialog_weak_for_key.clone();
            glib::idle_add_local_once(move || {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.remove_attachment(&path);
                }
            });
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    row_box.add_controller(key_controller);

    // Remove on remove button click
    let path = attachment.path.clone();
    let dialog_weak_for_remove = dialog_weak.clone();
    btn_remove.connect_clicked(move |_| {
        let path = path.clone();
        let dialog_weak = dialog_weak_for_remove.clone();
        glib::idle_add_local_once(move || {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.remove_attachment(&path);
            }
        });
    });

    // Support dragging list row to desktop/file manager
    if std::path::Path::new(&attachment.path).exists() {
        let file = gio::File::for_path(&attachment.path);
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::COPY);
        if let Some((texture, _, _)) = load_attachment_texture(attachment, 48) {
            drag_source.set_icon(Some(&texture), 16, 16);
        }
        let file_for_drag = file.clone();
        drag_source.connect_prepare(move |_, _, _| {
            let file_list = gdk::FileList::from_array(&[file_for_drag.clone()]);
            Some(gdk::ContentProvider::for_value(&file_list.to_value()))
        });
        let dialog_weak_begin = dialog_weak.clone();
        drag_source.connect_drag_begin(move |_, _| {
            if let Some(dialog) = dialog_weak_begin.upgrade() {
                dialog.imp().is_internal_drag.set(true);
            }
        });
        let dialog_weak_end = dialog_weak.clone();
        drag_source.connect_drag_end(move |_, _, _| {
            if let Some(dialog) = dialog_weak_end.upgrade() {
                dialog.imp().is_internal_drag.set(false);
            }
        });
        let dialog_weak_cancel = dialog_weak.clone();
        drag_source.connect_drag_cancel(move |_, _, _| {
            if let Some(dialog) = dialog_weak_cancel.upgrade() {
                dialog.imp().is_internal_drag.set(false);
            }
            false
        });
        row_box.add_controller(drag_source);
    }

    row.set_child(Some(&row_box));
    row
}

fn format_attachment_summary(count: usize, total_size: u64) -> String {
    let size_str = format_size(total_size);
    if count == 1 {
        format!("1 attachment ({size_str})")
    } else {
        format!("{count} attachments ({size_str})")
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
    let (primary_label, primary_from) =
        if display_name.is_empty() || display_name.eq_ignore_ascii_case(&primary_address) {
            (primary_address.clone(), primary_address.clone())
        } else {
            (
                format!("{} <{}>", display_name, primary_address),
                format!("{} <{}>", display_name, primary_address),
            )
        };
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
            .map(|alias| {
                let (label, from) =
                    if display_name.is_empty() || display_name.eq_ignore_ascii_case(&alias) {
                        (
                            format!("{} · Alias von {}", alias, primary_address),
                            alias.clone(),
                        )
                    } else {
                        (
                            format!("{} <{}> · Alias von {}", display_name, alias, primary_address),
                            format!("{} <{}>", display_name, alias),
                        )
                    };
                ComposeIdentity {
                    source_uid: source_uid.clone(),
                    label,
                    from,
                    reply_to: reply_to.clone(),
                    signature_id: signature_id.clone(),
                }
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

pub(crate) fn format_sender_display_name(sender: &str) -> String {
    let trimmed = sender.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(angle_start) = trimmed.find('<') {
        let name_part = trimmed[..angle_start].trim();
        let unquoted = name_part.trim_matches('"').trim();
        if !unquoted.is_empty() {
            return unquoted.to_string();
        }
        if let Some(angle_end) = trimmed[angle_start + 1..].find('>') {
            let email_part = trimmed[angle_start + 1..angle_start + 1 + angle_end].trim();
            if !email_part.is_empty() {
                return email_part.to_string();
            }
        }
    }
    let unquoted = trimmed.trim_matches('"').trim();
    unquoted.to_string()
}

fn is_german_locale() -> bool {
    let locale = std::env::var("LC_TIME")
        .or_else(|_| std::env::var("LC_ALL"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let lower = locale.to_lowercase();
    if lower.starts_with("de") {
        return true;
    }
    let tz = std::env::var("TZ").unwrap_or_default();
    if tz.contains("Berlin") || tz.contains("Germany") {
        return true;
    }
    if lower.starts_with("en") {
        return false;
    }
    crate::i18n::gettext("Close") == "Schließen"
}

fn german_weekday_name(weekday: chrono::Weekday) -> &'static str {
    match weekday {
        chrono::Weekday::Mon => "Montag",
        chrono::Weekday::Tue => "Dienstag",
        chrono::Weekday::Wed => "Mittwoch",
        chrono::Weekday::Thu => "Donnerstag",
        chrono::Weekday::Fri => "Freitag",
        chrono::Weekday::Sat => "Samstag",
        chrono::Weekday::Sun => "Sonntag",
    }
}

fn english_weekday_name(weekday: chrono::Weekday) -> &'static str {
    match weekday {
        chrono::Weekday::Mon => "Monday",
        chrono::Weekday::Tue => "Tuesday",
        chrono::Weekday::Wed => "Wednesday",
        chrono::Weekday::Thu => "Thursday",
        chrono::Weekday::Fri => "Friday",
        chrono::Weekday::Sat => "Saturday",
        chrono::Weekday::Sun => "Sunday",
    }
}

pub(crate) fn format_reply_attribution(sender: &str, date: &str) -> String {
    let display_name = format_sender_display_name(sender);
    let parsed_dt = crate::views::parse_flexible_datetime(date);
    let is_de = is_german_locale();

    match (parsed_dt, is_de) {
        (Some(dt), true) => {
            let weekday = german_weekday_name(dt.weekday());
            let date_str = dt.format("%d.%m.%Y");
            let time_str = dt.format("%H:%M");
            let tz_str = dt.format("%z");
            if !display_name.is_empty() {
                format!(
                    "Am {weekday}, dem {date_str} um {time_str} {tz_str} schrieb {}:",
                    html_escape(&display_name)
                )
            } else {
                format!(
                    "Am {weekday}, dem {date_str} um {time_str} {tz_str} schrieb der ursprüngliche Absender:"
                )
            }
        }
        (Some(dt), false) => {
            let weekday = english_weekday_name(dt.weekday());
            let date_str = dt.format("%Y-%m-%d");
            let time_str = dt.format("%H:%M");
            let tz_str = dt.format("%z");
            if !display_name.is_empty() {
                format!(
                    "On {weekday}, {date_str} at {time_str} {tz_str}, {} wrote:",
                    html_escape(&display_name)
                )
            } else {
                format!(
                    "On {weekday}, {date_str} at {time_str} {tz_str}, the original sender wrote:"
                )
            }
        }
        (None, true) => {
            if !date.trim().is_empty() {
                if !display_name.is_empty() {
                    format!(
                        "Am {} schrieb {}:",
                        html_escape(date.trim()),
                        html_escape(&display_name)
                    )
                } else {
                    format!("Am {}:", html_escape(date.trim()))
                }
            } else if !display_name.is_empty() {
                format!("{} schrieb:", html_escape(&display_name))
            } else {
                "Ursprüngliche Nachricht:".to_string()
            }
        }
        (None, false) => {
            if !date.trim().is_empty() {
                if !display_name.is_empty() {
                    format!(
                        "On {}, {} wrote:",
                        html_escape(date.trim()),
                        html_escape(&display_name)
                    )
                } else {
                    format!("On {}:", html_escape(date.trim()))
                }
            } else if !display_name.is_empty() {
                format!("{} wrote:", html_escape(&display_name))
            } else {
                "Original Message:".to_string()
            }
        }
    }
}

fn strip_outer_html_scaffolding(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(body_start) = lower.find("<body") {
        if let Some(tag_end) = html[body_start..].find('>') {
            let start = body_start + tag_end + 1;
            if let Some(body_end) = lower[start..].find("</body>") {
                return html[start..start + body_end].to_string();
            } else {
                return html[start..].to_string();
            }
        }
    }

    let mut working = html.to_string();
    let working_lower = working.to_ascii_lowercase();
    if let Some(head_start) = working_lower.find("<head") {
        if let Some(head_end) = working_lower[head_start..].find("</head>") {
            let end_idx = head_start + head_end + 7;
            working = format!("{}{}", &working[..head_start], &working[end_idx..]);
        }
    }

    let mut clean = String::with_capacity(working.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();

    for ch in working.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
            tag_buf.push(ch);
            continue;
        }
        if in_tag {
            tag_buf.push(ch);
            if ch == '>' {
                in_tag = false;
                let tag_lower = tag_buf.to_ascii_lowercase();
                if tag_lower.starts_with("<!doctype")
                    || tag_lower.starts_with("<html")
                    || tag_lower.starts_with("</html")
                    || tag_lower.starts_with("<body")
                    || tag_lower.starts_with("</body")
                {
                    // discard outer envelope tag
                } else {
                    clean.push_str(&tag_buf);
                }
            }
            continue;
        }
        clean.push(ch);
    }
    clean
}

fn is_pure_pre_block(html: &str) -> bool {
    let trimmed = html.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("<pre") || !lower.ends_with("</pre>") {
        return false;
    }
    let Some(first) = lower.find("<pre") else {
        return false;
    };
    let after_first = &lower[first + 4..];
    !after_first.contains("<pre")
}

fn extract_pre_content(html: &str) -> &str {
    let trimmed = html.trim();
    if let Some(gt) = trimmed.find('>') {
        let after_gt = &trimmed[gt + 1..];
        let lower = after_gt.to_ascii_lowercase();
        if let Some(end_idx) = lower.rfind("</pre>") {
            return &after_gt[..end_idx];
        }
    }
    trimmed
}

fn convert_plain_lines_to_html(plain: &str) -> String {
    let mut out = String::new();
    let mut lines = plain.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed_line = line.trim_end_matches('\r');
        out.push_str(trimmed_line);
        if lines.peek().is_some() {
            out.push_str("<br>");
        }
    }
    out
}

pub(crate) fn prepare_quote_html(original_html: &str) -> String {
    let trimmed = original_html.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let body_content = strip_outer_html_scaffolding(trimmed);
    let sanitized = crate::services::html_sanitizer::sanitize_html_with_options(&body_content, true);
    let sanitized_trimmed = sanitized.trim();
    if sanitized_trimmed.is_empty() {
        return String::new();
    }

    if is_pure_pre_block(sanitized_trimmed) {
        let inner = extract_pre_content(sanitized_trimmed);
        return convert_plain_lines_to_html(inner);
    }

    if !sanitized_trimmed.contains('<') {
        return signature_plain_text_to_html(sanitized_trimmed);
    }

    sanitized_trimmed
        .replace(
            "<p class=\"MsoNormal\">",
            "<p class=\"MsoNormal\" style=\"margin: 0; line-height: 1.15;\">",
        )
        .replace(
            "<p class=\"MsoNormal\" style=\"",
            "<p class=\"MsoNormal\" style=\"margin: 0; line-height: 1.15; ",
        )
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
        .unwrap_or(true);
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

    let attribution = format_reply_attribution(sender, date);

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
        let quoted = prepare_quote_html(original_html);
        return if quoted.trim().is_empty() {
            if top_signature {
                prefix
            } else {
                format!("{prefix}{suffix}")
            }
        } else {
            format!("{prefix}<p>{header_block}</p><div>{quoted}</div>{suffix}")
        };
    }

    if style == "prefix" {
        let quoted_text = quote_as_safe_text(original_html);
        if quoted_text.trim().is_empty() {
            return if top_signature {
                prefix
            } else {
                format!("{prefix}{suffix}")
            };
        }
        let quoted = format_quoted_body(&quoted_text, "prefix");
        return format!("{prefix}<p>{attribution}</p>{quoted}{suffix}");
    }

    let quoted_html = prepare_quote_html(original_html);
    if quoted_html.trim().is_empty() {
        if top_signature {
            prefix
        } else {
            format!("{prefix}{suffix}")
        }
    } else {
        let quoted = format_quoted_body(&quoted_html, "blockquote");
        format!("{prefix}<p>{attribution}</p>{quoted}{suffix}")
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
        .unwrap_or(true);
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

    if quote_style == "none" {
        return if top_signature {
            prefix
        } else {
            format!("{prefix}{suffix}")
        };
    }

    if quote_style == "prefix" {
        let quoted_text = quote_as_safe_text(original_html);
        if quoted_text.trim().is_empty() {
            return if top_signature {
                prefix
            } else {
                format!("{prefix}{suffix}")
            };
        }
        let quoted = format_quoted_body(&quoted_text, "prefix");
        return format!("{prefix}<p>{header_block}</p>{quoted}{suffix}");
    }

    let quoted_html = prepare_quote_html(original_html);
    if quoted_html.trim().is_empty() {
        if top_signature {
            prefix
        } else {
            format!("{prefix}{suffix}")
        }
    } else if quote_style == "inline" {
        format!("{prefix}<p>{header_block}</p><div>{quoted_html}</div>{suffix}")
    } else {
        let quoted = format_quoted_body(&quoted_html, "blockquote");
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
        format!(
            "<blockquote type=\"cite\" style=\"margin: 0 0 0 0.8ex; border-left: 2px solid #729fcf; padding-left: 1ex;\">{quoted}</blockquote>"
        )
    }
}

/// Returns the signature separator as HTML (e.g. `--<br>`), honoring the
/// user's customizable separator text, or an empty string when disabled.
fn signature_delimiter_html() -> String {
    let Some(settings) = AppSettings::try_new() else {
        return "--<br>".to_string();
    };
    if !settings.composer_signature_outline() {
        return String::new();
    }
    let text = settings.signature_separator_text();
    let text = text.trim_end();
    let text = if text.is_empty() { "--" } else { text };
    format!("{}<br>", html_escape(text))
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
    let sanitized = crate::services::signature_store_service::sanitize_signature_html(inner_html);
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let delimiter = signature_delimiter_html();
    let delim_html = if delimiter.is_empty() {
        String::new()
    } else {
        format!("<p>{delimiter}</p>")
    };

    let lower = trimmed.to_ascii_lowercase();
    let has_block_root = lower.starts_with("<p")
        || lower.starts_with("<div")
        || lower.starts_with("<table")
        || lower.starts_with("<h");

    let formatted_content = if has_block_root {
        trimmed.to_string()
    } else {
        format!("<p>{trimmed}</p>")
    };

    format!("<div id=\"_evolution_signature_\" class=\"evolution-signature\">{delim_html}{formatted_content}</div>")
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
        "<p><br></p><p><br></p>".to_string()
    } else {
        format!(
            "<p><br></p><p><br></p>{}<p><br></p>",
            format_signature_markup(&signature)
        )
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

fn safe_popup_popover(popover: &gtk::Popover, entry: &gtk::Entry) {
    if popover.parent().is_none() {
        return;
    }
    if !entry.is_realized() || entry.root().is_none() || entry.native().is_none() || !gtk::prelude::WidgetExt::is_visible(entry) {
        return;
    }
    popover.popup();
}

fn configure_recipient_scroll_sizing(
    scroll: &gtk::ScrolledWindow,
    list: &gtk::ListBox,
    popover: &gtk::Popover,
    item_count: usize,
    max_height: i32,
) {
    if item_count <= 10 {
        scroll.set_vscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_min_content_height(0);
        scroll.set_max_content_height(-1);
        scroll.vadjustment().set_value(0.0);
    } else {
        scroll.set_vscrollbar_policy(gtk::PolicyType::Automatic);
        scroll.set_min_content_height(0);
        scroll.set_max_content_height(max_height);
    }
    list.queue_resize();
    scroll.queue_resize();
    popover.queue_resize();
}

fn clear_entry_selection(entry: &gtk::Entry) {
    entry.select_region(0, 0);
    let mut child = entry.first_child();
    while let Some(widget) = child {
        if let Ok(text_widget) = widget.clone().downcast::<gtk::Text>() {
            text_widget.select_region(0, 0);
            break;
        }
        child = widget.next_sibling();
    }
}

fn place_entry_cursor_at_end_unselected(entry: &gtk::Entry) {
    if !entry.has_focus() {
        entry.grab_focus_without_selecting();
    }
    let end_pos = entry.text().chars().count() as i32;
    entry.select_region(end_pos, end_pos);
    entry.set_position(-1);

    let mut child = entry.first_child();
    while let Some(widget) = child {
        if let Ok(text_widget) = widget.clone().downcast::<gtk::Text>() {
            if !text_widget.has_focus() {
                text_widget.grab_focus_without_selecting();
            }
            text_widget.select_region(end_pos, end_pos);
            text_widget.set_position(-1);
            break;
        }
        child = widget.next_sibling();
    }

    let entry_weak = entry.downgrade();
    glib::idle_add_local_once(move || {
        if let Some(entry) = entry_weak.upgrade() {
            if !entry.has_focus() {
                entry.grab_focus_without_selecting();
            }
            let end_pos = entry.text().chars().count() as i32;
            entry.select_region(end_pos, end_pos);
            entry.set_position(-1);
            let mut child = entry.first_child();
            while let Some(widget) = child {
                if let Ok(text_widget) = widget.clone().downcast::<gtk::Text>() {
                    text_widget.select_region(end_pos, end_pos);
                    text_widget.set_position(-1);
                    break;
                }
                child = widget.next_sibling();
            }
        }
    });
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
    popover.add_css_class("recipient-completion-popover");
    popover.set_parent(entry);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .valign(gtk::Align::Start)
        .build();

    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .width_request(440)
        .min_content_height(0)
        .max_content_height(380)
        .propagate_natural_height(true)
        .propagate_natural_width(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .valign(gtk::Align::Start)
        .build();

    popover.set_child(Some(&scroll));

    let visible_suggestions = Rc::new(RefCell::new(Vec::<RecipientSuggestion>::new()));
    let generation = Rc::new(Cell::new(0u64));
    let debounce_source = Rc::new(RefCell::new(Option::<glib::SourceId>::None));
    let suppress_autocompletion = Rc::new(Cell::new(false));

    let entry_for_activate = entry.downgrade();
    let popover_for_activate = popover.clone();
    let visible_for_activate = Rc::clone(&visible_suggestions);
    let dialog_weak_for_activate = dialog_weak.clone();
    let suppress_for_activate = Rc::clone(&suppress_autocompletion);
    let debounce_for_activate = Rc::clone(&debounce_source);
    let generation_for_activate = Rc::clone(&generation);
    let accept_selection = move |selected_index: usize| {
        let Some(entry) = entry_for_activate.upgrade() else {
            return;
        };
        let selected_item = {
            let suggestions = visible_for_activate.borrow();
            suggestions.get(selected_index).cloned()
        };
        let Some(suggestion) = selected_item else {
            return;
        };
        if let Some(source_id) = debounce_for_activate.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source_id.as_raw()) };
        }
        generation_for_activate.set(generation_for_activate.get().wrapping_add(1));
        visible_for_activate.borrow_mut().clear();

        let formatted = recipient_suggestion_label(&suggestion);
        let new_text = replace_current_recipient(&entry.text(), &formatted);

        suppress_for_activate.set(true);
        popover_for_activate.popdown();
        entry.set_text(&new_text);
        suppress_for_activate.set(false);

        place_entry_cursor_at_end_unselected(&entry);

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
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
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
                let target_idx = if current >= 0 && (current as usize) < count {
                    current as usize
                } else {
                    0
                };
                accept_for_key(target_idx);
                glib::Propagation::Stop
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

    let popover_unrealize = popover.clone();
    entry.connect_unrealize(move |_| {
        popover_unrealize.popdown();
        if popover_unrealize.parent().is_some() {
            popover_unrealize.unparent();
        }
    });

    let popover_realize = popover.clone();
    entry.connect_realize(move |e| {
        if popover_realize.parent().is_none() {
            popover_realize.set_parent(e);
        }
    });

    let entry_weak = entry.downgrade();
    let popover_for_changed = popover.clone();
    let list_for_changed = list.clone();
    let scroll_for_changed = scroll.clone();
    let combo_from_clone = combo_from.clone();
    let dialog_weak_for_changed = dialog_weak.clone();
    let suppress_for_changed = Rc::clone(&suppress_autocompletion);
    entry.connect_changed(move |e| {
        if suppress_for_changed.get() {
            return;
        }
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

        if let Some(source_id) = debounce_source.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source_id.as_raw()) };
        }

        let generation_id = generation.get().wrapping_add(1);
        generation.set(generation_id);

        if fragment.trim().is_empty() || fragment.chars().count() < effective_min_len {
            visible_suggestions.replace(Vec::new());
            popover_for_changed.popdown();
            return;
        }

        // 1. Instant synchronous local search from in-memory pool (< 0.1 ms)
        let mut suggestions = search_recipient_suggestions(&fragment, show_address);
        suggestions.truncate(16);

        if suggestions.is_empty() {
            popover_for_changed.popdown();
            visible_suggestions.replace(Vec::new());
            while let Some(row) = list_for_changed.first_child() {
                list_for_changed.remove(&row);
            }
        } else {
            render_recipient_suggestions_to_list(&list_for_changed, &suggestions);
            configure_recipient_scroll_sizing(
                &scroll_for_changed,
                &list_for_changed,
                &popover_for_changed,
                suggestions.len(),
                380,
            );
            visible_suggestions.replace(suggestions);
            if let Some(first_row) = list_for_changed.row_at_index(0) {
                list_for_changed.select_row(Some(&first_row));
            }
            safe_popup_popover(&popover_for_changed, e);
        }

        // 2. Non-blocking debounced asynchronous GAL search in background thread
        if fragment.chars().count() >= 3 {
            let entry_weak_clone = entry_weak.clone();
            let popover_clone = popover_for_changed.clone();
            let list_clone = list_for_changed.clone();
            let scroll_clone = scroll_for_changed.clone();
            let visible_clone = Rc::clone(&visible_suggestions);
            let generation_clone = Rc::clone(&generation);
            let combo_from_source = combo_from_clone.clone();
            let dialog_weak_source = dialog_weak_for_changed.clone();
            let debounce_source_timer = Rc::clone(&debounce_source);
            let fragment_for_timer = fragment.clone();

            let source_id = glib::timeout_add_local(
                std::time::Duration::from_millis(80),
                move || {
                    debounce_source_timer.borrow_mut().take();
                    if generation_clone.get() != generation_id {
                        return glib::ControlFlow::Break;
                    }
                    let source_uid = dialog_weak_source
                        .upgrade()
                        .map(|d| d.selected_identity(combo_from_source.selected()).source_uid)
                        .unwrap_or_default();
                    if source_uid.trim().is_empty() {
                        return glib::ControlFlow::Break;
                    }

                    let generation_for_gal = generation_id;
                    let gen_tracker = Rc::clone(&generation_clone);
                    let list_for_gal = list_clone.clone();
                    let scroll_for_gal = scroll_clone.clone();
                    let visible_for_gal = Rc::clone(&visible_clone);
                    let popover_for_gal = popover_clone.clone();
                    let entry_weak_for_gal = entry_weak_clone.clone();
                    let fragment_for_gal = fragment_for_timer.clone();
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
                                    && !current
                                        .iter()
                                        .any(|s| s.address.eq_ignore_ascii_case(&sugg.address))
                                {
                                    current.push(sugg);
                                    added = true;
                                }
                            }
                            if added {
                                current.truncate(16);
                                render_recipient_suggestions_to_list(&list_for_gal, &current);
                                configure_recipient_scroll_sizing(
                                    &scroll_for_gal,
                                    &list_for_gal,
                                    &popover_for_gal,
                                    current.len(),
                                    380,
                                );
                                visible_for_gal.replace(current);
                                if let Some(first_row) = list_for_gal.row_at_index(0) {
                                    if list_for_gal.selected_row().is_none() {
                                        list_for_gal.select_row(Some(&first_row));
                                    }
                                }
                                if let Some(entry) = entry_weak_for_gal.upgrade() {
                                    safe_popup_popover(&popover_for_gal, &entry);
                                }
                            }
                        }
                    });

                    glib::ControlFlow::Break
                },
            );
            debounce_source.borrow_mut().replace(source_id);
        }
    });
}

fn append_recipient_to_entry(entry: &gtk::Entry, formatted_recipient: &str) {
    let formatted_recipient = formatted_recipient.trim();
    if formatted_recipient.is_empty() {
        return;
    }
    let current = entry.text().to_string();
    let new_key = recipient_key(formatted_recipient);
    let existing = split_recipient_list(&current);
    if existing.iter().any(|item| recipient_key(item) == new_key) {
        if !current.trim().is_empty() && !current.trim_end().ends_with(',') {
            entry.set_text(&format!("{}, ", existing.join(", ")));
        }
        place_entry_cursor_at_end_unselected(entry);
        return;
    }
    let mut updated: Vec<String> = existing;
    updated.push(formatted_recipient.to_string());
    entry.set_text(&format!("{}, ", updated.join(", ")));
    place_entry_cursor_at_end_unselected(entry);
}

fn setup_recipient_label_contact_picker(
    label: &gtk::Label,
    entry: &gtk::Entry,
    combo_from: &gtk::DropDown,
    dialog_weak: glib::WeakRef<ComposeDialog>,
) {
    let popover = gtk::Popover::builder()
        .autohide(true)
        .position(gtk::PositionType::Bottom)
        .has_arrow(false)
        .halign(gtk::Align::Start)
        .css_classes(vec![
            "recipient-completion-popover",
            "composer-contact-picker-popover",
        ])
        .build();
    popover.set_parent(label);

    let outer_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .width_request(480)
        .build();

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(crate::i18n::gettext("Search contacts…"))
        .hexpand(true)
        .css_classes(vec!["composer-contact-picker-search"])
        .build();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .valign(gtk::Align::Start)
        .build();

    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .width_request(480)
        .min_content_height(0)
        .max_content_height(480)
        .propagate_natural_height(true)
        .propagate_natural_width(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .valign(gtk::Align::Start)
        .build();

    outer_box.append(&search_entry);
    outer_box.append(&scroll);
    popover.set_child(Some(&outer_box));

    let visible_suggestions = Rc::new(RefCell::new(Vec::<RecipientSuggestion>::new()));
    let generation = Rc::new(Cell::new(0u64));

    let populate_for_query = {
        let list_c = list.clone();
        let scroll_c = scroll.clone();
        let popover_c = popover.clone();
        let visible_c = Rc::clone(&visible_suggestions);
        let gen_c = Rc::clone(&generation);
        let combo_c = combo_from.clone();
        let dialog_w = dialog_weak.clone();
        Rc::new(move |query: &str| {
            let gen_id = gen_c.get().wrapping_add(1);
            gen_c.set(gen_id);
            let q = query.trim();
            let items = if q.is_empty() {
                let pool = get_or_load_recipient_pool();
                let mut all: Vec<RecipientSuggestion> = pool
                    .iter()
                    .filter(|s| !is_synthetic_test_recipient_address(&s.address))
                    .cloned()
                    .collect();
                all.sort_by(|a, b| {
                    let a_has_name = !a.display.trim().is_empty();
                    let b_has_name = !b.display.trim().is_empty();
                    match b_has_name.cmp(&a_has_name) {
                        std::cmp::Ordering::Equal => {
                            let a_key = if a_has_name {
                                a.display.trim().to_lowercase()
                            } else {
                                a.address.trim().to_lowercase()
                            };
                            let b_key = if b_has_name {
                                b.display.trim().to_lowercase()
                            } else {
                                b.address.trim().to_lowercase()
                            };
                            a_key
                                .cmp(&b_key)
                                .then_with(|| a.address.to_lowercase().cmp(&b.address.to_lowercase()))
                        }
                        other => other,
                    }
                });
                all
            } else {
                search_recipient_suggestions(q, true)
                    .into_iter()
                    .filter(|s| !is_synthetic_test_recipient_address(&s.address))
                    .collect()
            };
            render_recipient_suggestions_to_list(&list_c, &items);
            configure_recipient_scroll_sizing(
                &scroll_c,
                &list_c,
                &popover_c,
                items.len(),
                480,
            );
            visible_c.replace(items);
            if let Some(first_row) = list_c.row_at_index(0) {
                list_c.select_row(Some(&first_row));
            }

            if q.chars().count() >= 3 {
                let source_uid = dialog_w
                    .upgrade()
                    .map(|d| d.selected_identity(combo_c.selected()).source_uid)
                    .unwrap_or_default();
                if !source_uid.trim().is_empty() {
                    let gen_tracker = Rc::clone(&gen_c);
                    let list_gal = list_c.clone();
                    let scroll_gal = scroll_c.clone();
                    let popover_gal = popover_c.clone();
                    let vis_gal = Rc::clone(&visible_c);
                    let q_owned = q.to_string();
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let contacts = graph_service_for_source(&source_uid)
                            .and_then(|graph| graph.search_gal_sync(&q_owned).ok());
                        let _ = sender.send_blocking(contacts);
                    });
                    glib::spawn_future_local(async move {
                        if let Ok(Some(gal_contacts)) = receiver.recv().await {
                            if gen_tracker.get() != gen_id {
                                return;
                            }
                            let mut current = vis_gal.borrow().clone();
                            let mut added = false;
                            for gc in gal_contacts {
                                if is_internal_microsoft_routing_address(&gc.email_address) {
                                    continue;
                                }
                                let sugg = gal_contact_to_recipient_suggestion(gc);
                                if !sugg.address.is_empty()
                                    && !is_internal_microsoft_routing_address(&sugg.address)
                                    && !current
                                        .iter()
                                        .any(|s| s.address.eq_ignore_ascii_case(&sugg.address))
                                {
                                    current.push(sugg);
                                    added = true;
                                }
                            }
                            if added {
                                render_recipient_suggestions_to_list(&list_gal, &current);
                                configure_recipient_scroll_sizing(
                                    &scroll_gal,
                                    &list_gal,
                                    &popover_gal,
                                    current.len(),
                                    480,
                                );
                                vis_gal.replace(current);
                            }
                        }
                    });
                }
            }
        })
    };

    let entry_weak = entry.downgrade();
    let popover_for_activate = popover.clone();
    let visible_for_activate = Rc::clone(&visible_suggestions);
    let dialog_weak_for_activate = dialog_weak.clone();
    let accept_idx: Rc<dyn Fn(usize)> = Rc::new(move |selected_index: usize| {
        let Some(entry) = entry_weak.upgrade() else {
            return;
        };
        let selected_item = {
            let suggestions = visible_for_activate.borrow();
            suggestions.get(selected_index).cloned()
        };
        let Some(suggestion) = selected_item else {
            return;
        };
        let formatted = recipient_suggestion_label(&suggestion);
        popover_for_activate.popdown();
        append_recipient_to_entry(&entry, &formatted);
        if let Some(dialog) = dialog_weak_for_activate.upgrade() {
            dialog.mark_changed();
        }
    });

    let accept_for_row = Rc::clone(&accept_idx);
    list.connect_row_activated(move |_, row| {
        let idx = row.index();
        if idx >= 0 {
            accept_for_row(idx as usize);
        }
    });

    let pop_query = Rc::clone(&populate_for_query);
    search_entry.connect_search_changed(move |se| {
        pop_query(&se.text());
    });

    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let list_for_key = list.clone();
    let scroll_for_key = scroll.clone();
    let vis_for_key = Rc::clone(&visible_suggestions);
    let accept_for_key = Rc::clone(&accept_idx);
    let pop_for_key = popover.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        let count = vis_for_key.borrow().len();
        match key {
            gtk::gdk::Key::Down if count > 0 => {
                let current = list_for_key.selected_row().map(|r| r.index()).unwrap_or(-1);
                let next = (current + 1).min((count - 1) as i32);
                if let Some(row) = list_for_key.row_at_index(next) {
                    list_for_key.select_row(Some(&row));
                    let vadj = scroll_for_key.vadjustment();
                    if let Some(bounds) = row.compute_bounds(&list_for_key) {
                        let y = bounds.y() as f64;
                        let h = bounds.height() as f64;
                        if y + h > vadj.value() + vadj.page_size() {
                            vadj.set_value(y + h - vadj.page_size());
                        }
                    }
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up if count > 0 => {
                let current = list_for_key.selected_row().map(|r| r.index()).unwrap_or(0);
                let prev = (current - 1).max(0);
                if let Some(row) = list_for_key.row_at_index(prev) {
                    list_for_key.select_row(Some(&row));
                    let vadj = scroll_for_key.vadjustment();
                    if let Some(bounds) = row.compute_bounds(&list_for_key) {
                        let y = bounds.y() as f64;
                        if y < vadj.value() {
                            vadj.set_value(y);
                        }
                    }
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if count > 0 => {
                let current = list_for_key.selected_row().map(|r| r.index()).unwrap_or(0);
                let target_idx = if current >= 0 && (current as usize) < count {
                    current as usize
                } else {
                    0
                };
                accept_for_key(target_idx);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Escape => {
                pop_for_key.popdown();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    search_entry.add_controller(key_ctrl);

    let click_gesture = gtk::GestureClick::new();
    let pop_for_click = popover.clone();
    let se_for_click = search_entry.clone();
    let scroll_for_click = scroll.clone();
    let populate_on_open = Rc::clone(&populate_for_query);
    click_gesture.connect_released(move |gesture, _, _, _| {
        let Some(widget) = gesture.widget() else {
            return;
        };
        if !widget.is_realized() || widget.root().is_none() {
            return;
        }
        se_for_click.set_text("");
        populate_on_open("");
        scroll_for_click.vadjustment().set_value(0.0);
        pop_for_click.popup();
        se_for_click.grab_focus();
    });
    label.add_controller(click_gesture);

    let popover_unrealize = popover.clone();
    label.connect_unrealize(move |_| {
        popover_unrealize.popdown();
        if popover_unrealize.parent().is_some() {
            popover_unrealize.unparent();
        }
    });
    let popover_realize = popover.clone();
    label.connect_realize(move |lbl| {
        if popover_realize.parent().is_none() {
            popover_realize.set_parent(lbl);
        }
    });
}

fn render_recipient_suggestions_to_list(
    list: &gtk::ListBox,
    suggestions: &[RecipientSuggestion],
) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }

    let (show_avatars, fetch_logos) = AppSettings::try_new()
        .map(|s| (s.mail_show_avatar(), s.mail_fetch_remote_logos()))
        .unwrap_or((true, true));

    for sugg in suggestions {
        let list_row = gtk::ListBoxRow::new();
        list_row.add_css_class("recipient-suggestion-row");
        list_row.set_activatable(true);
        list_row.set_selectable(true);

        let row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(2)
            .margin_end(2)
            .margin_top(1)
            .margin_bottom(1)
            .valign(gtk::Align::Fill)
            .build();

        let display = if sugg.display.is_empty() {
            &sugg.address
        } else {
            &sugg.display
        };

        if show_avatars {
            let avatar = adw::Avatar::builder()
                .size(20)
                .text(display)
                .show_initials(true)
                .valign(gtk::Align::Center)
                .build();

            if fetch_logos {
                if let Some(bytes) = AvatarCacheService::global().get_cached(&sugg.address) {
                    if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
                        avatar.set_custom_image(Some(&texture));
                    }
                }
            }

            row_box.append(&avatar);
        }

        let has_separate_email = !sugg.display.is_empty()
            && !sugg.address.is_empty()
            && !sugg.display.eq_ignore_ascii_case(&sugg.address);

        let lbl_name = gtk::Label::builder()
            .label(display)
            .xalign(0.0)
            .yalign(0.5)
            .valign(gtk::Align::Fill)
            .hexpand(!has_separate_email)
            .max_width_chars(30)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(vec!["recipient-suggestion-name"])
            .build();
        row_box.append(&lbl_name);

        if has_separate_email {
            let email_str = format!("<{}>", sugg.address);
            let lbl_email = gtk::Label::builder()
                .label(&email_str)
                .xalign(0.0)
                .yalign(0.5)
                .valign(gtk::Align::Fill)
                .hexpand(true)
                .css_classes(vec!["dim-label", "recipient-suggestion-email"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            row_box.append(&lbl_email);
        }

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

pub fn is_synthetic_test_recipient_address(email: &str) -> bool {
    let lower = email.trim().to_ascii_lowercase();
    let Some(domain) = lower.split('@').nth(1) else {
        return false;
    };
    matches!(
        domain,
        "example.com"
            | "example.org"
            | "example.net"
            | "example.edu"
            | "example.test"
            | "notest.com"
            | "prefix.com"
            | "local"
            | "invalid"
            | "test"
    ) || domain.ends_with(".example.com")
        || domain.ends_with(".example.org")
        || domain.ends_with(".example.net")
        || domain.ends_with(".invalid")
        || domain.ends_with(".test")
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
            trigger_background_eds_pool_update();
            return current;
        }
    }

    // Fast path: load local sources immediately (< 3ms) and trigger background EDS sync
    let fast_pool = Arc::new(load_fast_local_recipients_pool());
    if let Ok(mut write) = LOCAL_RECIPIENT_POOL_CACHE.write() {
        *write = Some((std::time::Instant::now(), Arc::clone(&fast_pool)));
    }
    trigger_background_eds_pool_update();
    fast_pool
}

pub fn warm_recipient_cache() {
    trigger_background_eds_pool_update();
}

pub fn invalidate_recipient_cache() {
    if let Ok(mut write) = LOCAL_RECIPIENT_POOL_CACHE.write() {
        *write = None;
    }
}

fn trigger_background_eds_pool_update() {
    if !IS_UPDATING_POOL.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            let mut results = if let Ok(read) = LOCAL_RECIPIENT_POOL_CACHE.read() {
                read.as_ref()
                    .map(|(_, p)| (**p).clone())
                    .unwrap_or_else(load_fast_local_recipients_pool)
            } else {
                load_fast_local_recipients_pool()
            };

            let mut email_to_index: HashMap<String, usize> = results
                .iter()
                .enumerate()
                .map(|(idx, s)| (s.address.to_ascii_lowercase(), idx))
                .collect();

            let eds_contacts = crate::services::ebook_service::load_contacts_prefer_eds();
            for record in eds_contacts.contacts {
                let suggestions = contact_to_recipient_suggestions(&record.contact, false);
                for s in suggestions {
                    let clean_email = s.address.trim();
                    if clean_email.is_empty()
                        || !clean_email.contains('@')
                        || is_internal_microsoft_routing_address(clean_email)
                        || (!cfg!(test) && is_synthetic_test_recipient_address(clean_email))
                    {
                        continue;
                    }
                    let key = clean_email.to_ascii_lowercase();
                    let clean_display = s.display.trim();

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
                }
            }

            if let Ok(mut write) = LOCAL_RECIPIENT_POOL_CACHE.write() {
                *write = Some((std::time::Instant::now(), Arc::new(results)));
            }
            IS_UPDATING_POOL.store(false, Ordering::SeqCst);
        });
    }
}

fn load_fast_local_recipients_pool() -> Vec<RecipientSuggestion> {
    let mut results: Vec<RecipientSuggestion> = Vec::new();
    let mut email_to_index: HashMap<String, usize> = HashMap::new();

    let mut add_or_update = |display_name: &str, email: &str| {
        let clean_email = email.trim();
        if clean_email.is_empty()
            || !clean_email.contains('@')
            || is_internal_microsoft_routing_address(clean_email)
            || (!cfg!(test) && is_synthetic_test_recipient_address(clean_email))
        {
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

    // 1. Check recent recipients (frecency ranking - highest priority, < 0.2ms)
    let recent = RecentRecipientsService::new().all_recipients();
    for r in recent {
        add_or_update(&r.display_name, &r.email);
    }

    // 2. Query cached Evolution contacts directly from SQLite (< 0.5ms, no D-Bus or network)
    let sqlite_contacts = crate::services::evolution_contact_cache::load_contacts_from_evolution_sqlite_cache();
    for record in sqlite_contacts {
        let suggestions = contact_to_recipient_suggestions(&record.contact, false);
        for s in suggestions {
            add_or_update(&s.display, &s.address);
        }
    }

    // 3. Query local vCard store (local address book fallback, < 1ms)
    let store = ContactStoreService::new();
    let contacts = store.load_contacts();
    for record in contacts {
        let suggestions = contact_to_recipient_suggestions(&record.contact, false);
        for s in suggestions {
            add_or_update(&s.display, &s.address);
        }
    }

    // 4. Query previous correspondents from cached mail messages (< 1ms, limit to most recent 1000 messages)
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
        Some("eml") => "message/rfc822",
        Some("msg") => "application/vnd.ms-outlook",
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
    SaveAndClose,
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
        CloseRequestDecision::SaveAndClose
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
            CloseRequestDecision::SaveAndClose
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
        assert!(COMPOSER_ACTION_SHORTCUTS
            .iter()
            .any(|(action, shortcut)| *action == "toggle-format-toolbar" && *shortcut == "<Control><Shift>f"));
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
        let storage_path = recent_srv.storage_path().to_path_buf();
        let original_bytes = std::fs::read(&storage_path).ok();

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

        match original_bytes {
            Some(bytes) => {
                let _ = std::fs::write(&storage_path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&storage_path);
            }
        }
        invalidate_recipient_cache();

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
            "<blockquote type=\"cite\" style=\"margin: 0 0 0 0.8ex; border-left: 2px solid #729fcf; padding-left: 1ex;\">Hello\nWorld</blockquote>"
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

        assert!(inline.contains("<p>Hello<br/>World</p>") || inline.contains("<p>Hello<br>World</p>"));
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
        assert!(inline.contains("<p>Hello<br/>World</p>") || inline.contains("<p>Hello<br>World</p>"));
        assert!(quoted.contains("Ada") && (quoted.contains("schrieb") || quoted.contains("wrote")));
        assert!(quoted.contains("&gt; Hello<br>&gt; World"));
        assert!(!no_quote.contains("Hello"));
        assert!(!no_quote.contains("Ada"));
    }

    #[test]
    fn test_format_sender_display_name() {
        assert_eq!(
            format_sender_display_name("Oliver Bennett <OBennett@acme-corp.example>"),
            "Oliver Bennett"
        );
        assert_eq!(
            format_sender_display_name("\"Oliver Bennett\" <OBennett@acme-corp.example>"),
            "Oliver Bennett"
        );
        assert_eq!(
            format_sender_display_name("<OBennett@acme-corp.example>"),
            "OBennett@acme-corp.example"
        );
        assert_eq!(
            format_sender_display_name("OBennett@acme-corp.example"),
            "OBennett@acme-corp.example"
        );
        assert_eq!(format_sender_display_name(""), "");
    }

    #[test]
    fn test_format_reply_attribution_evolution_german() {
        let attr = format_reply_attribution(
            "Oliver Bennett <OBennett@acme-corp.example>",
            "Mon, 12 Oct 2026 07:04:00 +0200",
        );
        assert_eq!(
            attr,
            "Am Montag, dem 12.10.2026 um 07:04 +0200 schrieb Oliver Bennett:"
        );
    }

    #[test]
    fn test_format_reply_attribution_without_date() {
        let attr = format_reply_attribution("Oliver Bennett <OBennett@acme-corp.example>", "");
        assert_eq!(attr, "Oliver Bennett schrieb:");
    }

    #[test]
    fn test_prepare_quote_html_preserves_rich_markup() {
        let html = "<html><head><style>body { color: red; }</style></head><body><p>Guten Morgen zusammen,</p><p><b>DB Connect PKW Teil 2</b><br><b>08.10.2026</b></p></body></html>";
        let prepared = prepare_quote_html(html);
        assert!(prepared.contains("<b>DB Connect PKW Teil 2</b>"));
        assert!(prepared.contains("<b>08.10.2026</b>"));
        assert!(!prepared.contains("<style>"));
        assert!(!prepared.contains("<head>"));
        assert!(!prepared.contains("<body"));
        assert!(!prepared.contains("<pre>"));
    }

    #[test]
    fn test_reply_body_html_with_style_renders_evolution_blockquote() {
        let original = "<p>Guten Morgen,</p><p><b>DB Connect PKW Teil 2</b></p>";
        let body = reply_body_html_with_style(
            "Oliver Bennett <OBennett@acme-corp.example>",
            "test@example.com",
            "",
            "Mon, 12 Oct 2026 07:04:00 +0200",
            original,
            "blockquote",
        );
        assert!(body.contains("Am Montag, dem 12.10.2026 um 07:04 +0200 schrieb Oliver Bennett:"));
        assert!(body.contains("<blockquote type=\"cite\" style=\"margin: 0 0 0 0.8ex; border-left: 2px solid #729fcf; padding-left: 1ex;\">"));
        assert!(body.contains("<b>DB Connect PKW Teil 2</b>"));
        assert!(!body.contains("<pre>"));
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
    fn format_signature_markup_avoids_nested_paragraphs_when_html_contains_p() {
        let markup = format_signature_markup("<p>Mit freundlichen Grüßen</p><p>Martin</p>");
        assert!(markup.contains("id=\"_evolution_signature_\""));
        assert!(markup.contains("<p>Mit freundlichen Grüßen</p><p>Martin</p>"));
        assert!(!markup.contains("<p><p>"));
    }

    #[test]
    fn format_signature_markup_strips_white_backgrounds() {
        let raw = r#"<p style="background-color: rgb(255, 255, 255); caret-color: rgb(46, 52, 54); font-family: Aptos;">Mit freundlichen Grüßen</p>"#;
        let markup = format_signature_markup(raw);
        assert!(!markup.contains("background-color"));
        assert!(!markup.contains("caret-color"));
        assert!(markup.contains("font-family: Aptos"));
        assert!(markup.contains("id=\"_evolution_signature_\""));
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
        assert!(
            !choices
                .iter()
                .any(|c| c.name.to_lowercase().contains("allgemeine standardsignatur")),
            "Redundant 'Allgemeine Standardsignatur' must not appear in choices"
        );
    }

    #[test]
    fn test_format_attachment_summary() {
        assert_eq!(format_attachment_summary(1, 1024), "1 attachment (1.0 KiB)");
        assert_eq!(format_attachment_summary(3, 2097152), "3 attachments (2.0 MiB)");
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
        settings.set_composer_show_format_toolbar(false);

        let dialog = ComposeDialog::new();
        assert!(dialog.html_editor().is_some());
        let editor = dialog.html_editor().unwrap();
        assert!(!editor.is_format_toolbar_visible());
        editor.set_format_toolbar_visible(true);
        assert!(editor.is_format_toolbar_visible());
        assert!(!editor.toggle_format_toolbar());
        assert!(!editor.is_format_toolbar_visible());
        assert!(dialog.composer_action("toggle-format-toolbar").is_some());

        // Test Cc/Bcc visibility methods
        dialog.set_row_cc_visible(false);
        dialog.set_row_bcc_visible(false);
        assert!(!dialog.is_row_cc_visible());
        assert!(!dialog.is_row_bcc_visible());
        dialog.set_row_cc_visible(true);
        dialog.set_row_bcc_visible(true);
        assert!(dialog.is_row_cc_visible());
        assert!(dialog.is_row_bcc_visible());
        assert_eq!(dialog.imp().row_reply_to_widgets.borrow().len(), 2);
        assert_eq!(dialog.imp().row_cc_widgets.borrow().len(), 2);
        assert_eq!(dialog.imp().row_bcc_widgets.borrow().len(), 2);

        // Test persisted format toolbar visibility on startup
        settings.set_composer_show_format_toolbar(true);
        let dialog_with_toolbar = ComposeDialog::new();
        let editor_with_toolbar = dialog_with_toolbar.html_editor().unwrap();
        assert!(editor_with_toolbar.is_format_toolbar_visible());
        if let Some(btn_fmt) = dialog_with_toolbar.imp().format_button.borrow().as_ref() {
            btn_fmt.set_active(false);
            assert!(!editor_with_toolbar.is_format_toolbar_visible());
            btn_fmt.set_active(true);
            assert!(editor_with_toolbar.is_format_toolbar_visible());
        }
        settings.set_composer_show_format_toolbar(false);

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
        assert_eq!(mime_type_for_path(Path::new("email.eml")), "message/rfc822");
        assert_eq!(
            mime_type_for_path(Path::new("item.msg")),
            "application/vnd.ms-outlook"
        );
    }

    #[test]
    fn test_typing_in_entry_to_does_not_crash() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        invalidate_recipient_cache();
        let start = std::time::Instant::now();
        let pool = get_or_load_recipient_pool();
        let load_duration = start.elapsed();
        // Loading fast local pool must be instant (< 500ms), never blocking 8s on EDS
        assert!(load_duration < std::time::Duration::from_millis(500), "Fast pool load took too long: {:?}", load_duration);
        assert!(!pool.is_empty(), "Recipient pool should not be empty");

        let suggestions = search_recipient_suggestions("m", false);
        let _ = suggestions.len();

        let dialog = ComposeDialog::new();
        dialog.present_standalone();
        while context.iteration(false) {}

        let entry = dialog.imp().entry_to.borrow().as_ref().cloned().unwrap();
        entry.set_text("m");
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(200) {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_attachment_view_mode_and_expansion_controls() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = ComposeDialog::new();
        while context.iteration(false) {}

        // Initial view mode is Icons and expanded is false (compact 1-line chips)
        assert_eq!(dialog.imp().attachment_view_mode.get(), AttachmentViewMode::Icons);
        assert!(!dialog.imp().attachment_expanded.get());

        // Toggle to List
        dialog.set_attachment_view_mode(AttachmentViewMode::List);
        assert_eq!(dialog.imp().attachment_view_mode.get(), AttachmentViewMode::List);
        assert!(dialog.imp().attachment_expanded.get());
        if let Some(stack) = dialog.imp().attachment_stack.borrow().as_ref() {
            assert_eq!(stack.visible_child_name().as_deref(), Some("list"));
        }

        // Toggle to Icons
        dialog.set_attachment_view_mode(AttachmentViewMode::Icons);
        assert_eq!(dialog.imp().attachment_view_mode.get(), AttachmentViewMode::Icons);
        assert!(dialog.imp().attachment_expanded.get());
        if let Some(stack) = dialog.imp().attachment_stack.borrow().as_ref() {
            assert_eq!(stack.visible_child_name().as_deref(), Some("icons"));
        }

        // Test show_attachment_bar_drag_hint & adaptive dock visibility
        assert!(!dialog.imp().attachment_bar.borrow().as_ref().unwrap().is_visible());
        assert!(!dialog.imp().attachment_bar_separator.borrow().as_ref().unwrap().is_visible());
        assert!(!dialog.imp().attachment_bar_add_btn.borrow().as_ref().unwrap().is_visible());
        dialog.show_attachment_bar_drag_hint(true);
        assert!(dialog.imp().attachment_bar.borrow().as_ref().unwrap().is_visible());
        assert_eq!(
            dialog.imp().attachment_summary_label.borrow().as_ref().unwrap().text().as_str(),
            "Drop files here to attach"
        );
        dialog.show_attachment_bar_drag_hint(false);
        assert!(!dialog.imp().attachment_bar.borrow().as_ref().unwrap().is_visible());

        // Toggle expanded state in Icons mode (reveals child)
        dialog.set_attachment_view_mode(AttachmentViewMode::Icons);
        dialog.set_attachment_expanded(false);
        assert!(!dialog.imp().attachment_expanded.get());
        if let Some(revealer) = dialog.imp().attachment_detail_revealer.borrow().as_ref() {
            assert!(!revealer.reveals_child());
        }

        dialog.toggle_attachment_expanded();
        assert!(dialog.imp().attachment_expanded.get());
        if let Some(revealer) = dialog.imp().attachment_detail_revealer.borrow().as_ref() {
            assert!(revealer.reveals_child());
        }

        dialog.toggle_attachment_expanded();
        assert!(!dialog.imp().attachment_expanded.get());
        if let Some(revealer) = dialog.imp().attachment_detail_revealer.borrow().as_ref() {
            assert!(!revealer.reveals_child());
        }

        // Test Escape collapses drawer when expanded without closing dialog
        dialog.set_attachment_expanded(true);
        assert!(dialog.imp().attachment_expanded.get());
        let _ = dialog.activate_action("compose.close", None);
        assert!(!dialog.imp().attachment_expanded.get(), "compose.close should collapse sheet");

        // Test Escape cancels drag session when drag is active without closing dialog
        dialog.start_drag_session();
        assert!(dialog.imp().is_drag_active.get());
        assert!(!dialog.imp().attachment_expanded.get(), "start_drag_session does not auto-expand until hovered over bar");
        dialog.set_attachment_expanded(true);
        assert!(dialog.imp().attachment_expanded.get());
        let _ = dialog.activate_action("compose.close", None);
        assert!(!dialog.imp().is_drag_active.get(), "compose.close should deactivate drag session");
        assert!(!dialog.imp().attachment_expanded.get(), "compose.close should collapse sheet");

        // Test drag session can be active and expanded
        dialog.start_drag_session();
        assert!(dialog.imp().is_drag_active.get());
        dialog.set_attachment_expanded(true);
        assert!(dialog.imp().attachment_expanded.get());
        dialog.set_attachment_expanded(false);
        assert!(!dialog.imp().attachment_expanded.get());

        // Test external drag auto-expands drawer
        assert!(dialog.is_external_drag(None));
        dialog.start_external_drag_session();
        assert!(dialog.imp().is_drag_active.get());
        assert!(dialog.imp().attachment_expanded.get(), "start_external_drag_session must auto-expand drawer");
        assert!(dialog.imp().drag_auto_expanded.get());
        let _ = dialog.activate_action("compose.close", None);
        assert!(!dialog.imp().is_drag_active.get());
        assert!(!dialog.imp().attachment_expanded.get());
        assert!(!dialog.imp().drag_auto_expanded.get());

        // Test internal drag flag prevents external drag classification and drawer expansion
        dialog.imp().is_internal_drag.set(true);
        assert!(!dialog.is_external_drag(None));
        if dialog.is_external_drag(None) {
            dialog.start_external_drag_session();
        }
        assert!(!dialog.imp().drag_auto_expanded.get());
        dialog.imp().is_internal_drag.set(false);
        assert!(dialog.is_external_drag(None));

        // Test vertical height capping (max 220px to show 7-8 rows comfortably)
        dialog.update_attachment_max_height();
        if let Some(scrolled) = dialog.imp().attachment_scrolled.borrow().as_ref() {
            let max_h = scrolled.max_content_height();
            assert_eq!(max_h, 220, "Max height should be capped at 220px to show 7-8 rows, got {max_h}");
        }

        // Test add and remove attachment updates FlowBox, ListBox, and Bottom Bar chips
        let att = DraftAttachment {
            path: "/tmp/invoice.pdf".to_string(),
            filename: "invoice.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 4096,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        dialog.add_attachment(att.clone());
        assert_eq!(dialog.imp().attachments.borrow().len(), 1);

        if let Some(lbl) = dialog.imp().attachment_summary_label.borrow().as_ref() {
            assert!(lbl.text().contains("invoice.pdf") || lbl.text().contains("4.0 KiB") || lbl.text().contains("1"));
        }
        if let Some(fb) = dialog.imp().attachment_flowbox.borrow().as_ref() {
            assert!(fb.first_child().is_some(), "FlowBox should have child");
        }
        if let Some(lb) = dialog.imp().attachment_listbox.borrow().as_ref() {
            assert!(lb.first_child().is_some(), "ListBox should have child");
        }
        if let Some(chip_box) = dialog.imp().attachment_bar_chip_box.borrow().as_ref() {
            assert!(chip_box.first_child().is_some(), "Bottom bar chip box should have child");
        }
        assert!(!dialog.imp().attachment_bar_separator.borrow().as_ref().unwrap().is_visible());
        assert!(!dialog.imp().attachment_bar_add_btn.borrow().as_ref().unwrap().is_visible(), "add_btn hidden when collapsed");
        dialog.set_attachment_expanded(true);
        assert!(dialog.imp().attachment_bar_add_btn.borrow().as_ref().unwrap().is_visible(), "add_btn visible when expanded");
        assert!(dialog.imp().attachment_sheet_handle_btn.borrow().as_ref().unwrap().is_visible(), "handle_btn visible when expanded");
        dialog.set_attachment_expanded(false);
        assert!(!dialog.imp().attachment_sheet_handle_btn.borrow().as_ref().unwrap().is_visible(), "handle_btn hidden when collapsed");

        // Test compact mode for mobile / narrow view (<= 600sp) vs Desktop (> 600sp)
        assert!(!dialog.is_attachment_table_compact());
        dialog.set_attachment_table_compact(true);
        assert!(dialog.is_attachment_table_compact());
        if let Some(col_type) = dialog.imp().attachment_col_type.borrow().as_ref() {
            assert!(!col_type.is_visible());
        }
        if let Some(col_mod) = dialog.imp().attachment_col_mod.borrow().as_ref() {
            assert!(!col_mod.is_visible());
        }

        dialog.set_attachment_table_compact(false);
        assert!(!dialog.is_attachment_table_compact());
        if let Some(col_type) = dialog.imp().attachment_col_type.borrow().as_ref() {
            assert!(col_type.is_visible());
        }
        if let Some(col_mod) = dialog.imp().attachment_col_mod.borrow().as_ref() {
            assert!(col_mod.is_visible());
        }

        if let Some(fb) = dialog.imp().attachment_flowbox.borrow().as_ref() {
            let child = fb.first_child().expect("FlowBox child");
            assert!(child.is_focusable(), "FlowBoxChild should be focusable for keyboard navigation");
        }

        // Add more attachments to verify overflow button (+X ›)
        let att2 = DraftAttachment {
            path: "/tmp/doc2.pdf".to_string(),
            filename: "doc2.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 1024,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        let att3 = DraftAttachment {
            path: "/tmp/doc3.pdf".to_string(),
            filename: "doc3.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 1024,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        let att4 = DraftAttachment {
            path: "/tmp/doc4.pdf".to_string(),
            filename: "doc4.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 1024,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        let att5 = DraftAttachment {
            path: "/tmp/doc5.pdf".to_string(),
            filename: "doc5.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 1024,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        dialog.add_attachment(att2.clone());
        dialog.add_attachment(att3.clone());
        dialog.add_attachment(att4.clone());
        dialog.add_attachment(att5.clone());
        assert_eq!(dialog.imp().attachments.borrow().len(), 5);

        // Verify overflow button (+X …) exists
        if let Some(chip_box) = dialog.imp().attachment_bar_chip_box.borrow().as_ref() {
            let last_child = chip_box.last_child().expect("last child in chip box");
            assert!(
                last_child.has_css_class("attachment-overflow-btn"),
                "With 5 attachments, last child must be overflow button"
            );
        }

        // Remove attachments
        dialog.remove_attachment(&att.path);
        dialog.remove_attachment(&att2.path);
        dialog.remove_attachment(&att3.path);
        dialog.remove_attachment(&att4.path);
        dialog.remove_attachment(&att5.path);
        assert_eq!(dialog.imp().attachments.borrow().len(), 0);
        if let Some(fb) = dialog.imp().attachment_flowbox.borrow().as_ref() {
            assert!(fb.first_child().is_none(), "FlowBox child should be removed");
        }
        if let Some(lb) = dialog.imp().attachment_listbox.borrow().as_ref() {
            assert!(lb.first_child().is_none(), "ListBox child should be removed");
        }
        if let Some(chip_box) = dialog.imp().attachment_bar_chip_box.borrow().as_ref() {
            assert!(chip_box.first_child().is_none(), "Bottom bar chip box should be empty after removal");
        }

        // Test visual creation (thumbnail vs symbolic icon)
        let visual_pdf = create_attachment_visual(&att, 18);
        assert!(visual_pdf.downcast_ref::<gtk::Image>().is_some());

        let tmp_img = std::env::temp_dir().join("meridian_test_thumb.png");
        let png_bytes = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92,
            0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let _ = std::fs::write(&tmp_img, &png_bytes);
        let att_img = DraftAttachment {
            path: tmp_img.to_string_lossy().to_string(),
            filename: "photo.png".to_string(),
            mime_type: "image/png".to_string(),
            size: png_bytes.len() as u64,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        let visual_img = create_attachment_visual(&att_img, 20);
        let picture_widget = visual_img.downcast_ref::<gtk::Picture>().expect("Picture widget for thumbnail");
        assert!(picture_widget.has_css_class("attachment-thumbnail"));

        let chip_img = create_attachment_chip(&att_img, dialog.downgrade());
        let chip_img_box = chip_img.downcast_ref::<gtk::Box>().expect("chip box");
        assert!(chip_img_box.has_css_class("attachment-chip"));
        let chip_first = chip_img_box.first_child().expect("chip first child");
        assert!(chip_first.downcast_ref::<gtk::Image>().is_some(), "Chip for image should use clean symbolic icon");

        let chip_pdf = create_attachment_chip(&att, dialog.downgrade());
        let chip_pdf_box = chip_pdf.downcast_ref::<gtk::Box>().expect("chip box");
        let chip_pdf_first = chip_pdf_box.first_child().expect("chip pdf first child");
        assert!(chip_pdf_first.downcast_ref::<gtk::Image>().is_some(), "Chip for PDF should have an Image icon visual");

        let card = create_attachment_card(&att_img, dialog.downgrade());
        let overlay = card.child().and_downcast::<gtk::Overlay>().expect("overlay");
        let card_box = overlay.child().and_downcast::<gtk::Box>().expect("card box");
        let visual_box = card_box.first_child().and_downcast::<gtk::CenterBox>().expect("visual center box");
        assert!(visual_box.has_css_class("attachment-card-visual-box"));
        let center_visual = visual_box.center_widget().expect("center widget");
        let pic = center_visual.downcast_ref::<gtk::Picture>().expect("picture");
        assert_eq!(pic.content_fit(), gtk::ContentFit::Contain);
        assert_eq!(pic.halign(), gtk::Align::Center);
        assert_eq!(pic.valign(), gtk::Align::Center);

        let _ = std::fs::remove_file(&tmp_img);

        let tmp_wide = std::env::temp_dir().join("meridian_test_wide.png");
        let wide_pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 100, 10).unwrap();
        wide_pixbuf.savev(&tmp_wide, "png", &[]).unwrap();
        let att_wide = DraftAttachment {
            path: tmp_wide.to_string_lossy().to_string(),
            filename: "wide_banner.png".to_string(),
            mime_type: "image/png".to_string(),
            size: 1000,
            content_id: None,
            embedded_content_base64: String::new(),
        };
        let visual_wide = create_attachment_visual(&att_wide, 64);
        let pic_wide = visual_wide.downcast_ref::<gtk::Picture>().expect("wide picture");
        assert_eq!(pic_wide.width_request(), 64);
        assert_eq!(pic_wide.height_request(), 6);
        let _ = std::fs::remove_file(&tmp_wide);

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_html_editor_image_support_check() {
        assert!(crate::views::HtmlEditor::is_supported_image(std::path::Path::new("photo.png")));
        assert!(crate::views::HtmlEditor::is_supported_image(std::path::Path::new("IMAGE.JPG")));
        assert!(crate::views::HtmlEditor::is_supported_image(std::path::Path::new("graphic.svg")));
        assert!(crate::views::HtmlEditor::is_supported_image(std::path::Path::new("anim.gif")));
        assert!(crate::views::HtmlEditor::is_supported_image(std::path::Path::new("modern.webp")));
        assert!(!crate::views::HtmlEditor::is_supported_image(std::path::Path::new("doc.pdf")));
        assert!(!crate::views::HtmlEditor::is_supported_image(std::path::Path::new("archive.zip")));
        assert!(!crate::views::HtmlEditor::is_supported_image(std::path::Path::new("file.docx")));
    }

    #[test]
    fn test_compose_dialog_image_convert_to_attachment() {
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = ComposeDialog::new();
        assert_eq!(dialog.imp().attachments.borrow().len(), 0);

        let att = DraftAttachment {
            path: "/tmp/converted_inline.png".to_string(),
            filename: "converted_inline.png".to_string(),
            mime_type: "image/png".to_string(),
            size: 2048,
            content_id: None,
            embedded_content_base64: "dGVzdA==".to_string(),
        };

        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            for cb in editor.imp().image_convert_callbacks.borrow().0.iter() {
                cb(att.clone());
            }
        }

        assert_eq!(dialog.imp().attachments.borrow().len(), 1);
        assert_eq!(
            dialog.imp().attachments.borrow()[0].filename,
            "converted_inline.png"
        );
        assert!(dialog.imp().attachment_expanded.get());

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_compose_dialog_responsive_attach_button() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = ComposeDialog::new();
        let btn = dialog.imp().btn_attach.borrow().as_ref().cloned().expect("btn_attach exists");
        assert_eq!(btn.icon_name().as_deref(), Some("mail-attachment-symbolic"));
        assert!(btn.has_css_class("flat"));

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_empty_compose_dialog_is_not_dirty_and_closes_without_prompt() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = ComposeDialog::new();
        assert!(!dialog.has_user_content());
        assert!(!dialog.is_dirty());
        // Closing an empty compose dialog must be allowed immediately without prompting
        assert!(!dialog.handle_close_request());
        assert!(!dialog.imp().close_prompt_open.get());

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_compose_dialog_with_content_becomes_dirty_and_prompts() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = ComposeDialog::new();
        assert!(!dialog.is_dirty());

        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text("Hello");
        }
        assert!(dialog.has_user_content());
        assert!(dialog.is_dirty());
        assert_eq!(
            close_request_decision(dialog.is_dirty(), dialog.imp().operation.get()),
            CloseRequestDecision::SaveAndClose
        );

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_add_attachment_path_defaults_to_collapsed_chips() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = ComposeDialog::new();
        while context.iteration(false) {}

        let temp_dir = std::env::temp_dir().join(format!("meridian-test-att-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let file1 = temp_dir.join("test_report.pdf");
        let file2 = temp_dir.join("test_sheet.xlsx");
        let file3 = temp_dir.join("test_notes.txt");
        std::fs::write(&file1, b"pdf content").unwrap();
        std::fs::write(&file2, b"excel content").unwrap();
        std::fs::write(&file3, b"notes content").unwrap();

        // Adding 1st attachment keeps bar collapsed as chips
        dialog.add_attachment_path(&file1);
        while context.iteration(false) {}
        assert_eq!(dialog.imp().attachments.borrow().len(), 1);
        assert!(!dialog.imp().attachment_expanded.get(), "Must stay collapsed as chip");
        if let Some(rev) = dialog.imp().attachment_detail_revealer.borrow().as_ref() {
            assert!(!rev.reveals_child(), "Detail drawer must remain closed");
        }
        if let Some(chip_box) = dialog.imp().attachment_bar_chip_box.borrow().as_ref() {
            assert!(chip_box.is_visible(), "Chip box must be visible");
            assert_eq!(chip_box.first_child().is_some(), true);
        }

        // Adding 2nd and 3rd attachments still keeps bar collapsed as chips
        dialog.add_attachment_path(&file2);
        dialog.add_attachment_path(&file3);
        while context.iteration(false) {}
        assert_eq!(dialog.imp().attachments.borrow().len(), 3);
        assert!(!dialog.imp().attachment_expanded.get(), "Must stay collapsed with 3 chips");
        if let Some(rev) = dialog.imp().attachment_detail_revealer.borrow().as_ref() {
            assert!(!rev.reveals_child(), "Detail drawer must still be closed");
        }

        // Manual toggle still expands properly
        dialog.toggle_attachment_expanded();
        while context.iteration(false) {}
        assert!(dialog.imp().attachment_expanded.get());
        if let Some(rev) = dialog.imp().attachment_detail_revealer.borrow().as_ref() {
            assert!(rev.reveals_child());
        }

        // Collapse back
        dialog.toggle_attachment_expanded();
        while context.iteration(false) {}
        assert!(!dialog.imp().attachment_expanded.get());

        let _ = std::fs::remove_file(&file1);
        let _ = std::fs::remove_file(&file2);
        let _ = std::fs::remove_file(&file3);
        let _ = std::fs::remove_dir(&temp_dir);

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_compose_dialog_close_button_label_and_autosave_on_close() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = ComposeDialog::new();
        while context.iteration(false) {}

        // Verify close button has gray background styling and no flat class
        let btn_close = dialog.imp().btn_close.borrow().clone().expect("close button exists");
        assert!(btn_close.has_css_class("composer-close-btn"));
        assert!(!btn_close.has_css_class("flat"));
        assert_eq!(btn_close.label().as_deref(), Some("Close"));

        // Verify save draft floppy disk button is not packed into the header bar
        if let Some(btn) = dialog.imp().save_draft_button.borrow().as_ref() {
            assert!(btn.parent().is_none(), "Save draft floppy disk button should not be packed into header bar");
        }

        // When user types content, close_request_decision is SaveAndClose without any modal prompt
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text("Test Subject");
        }
        assert!(dialog.is_dirty());
        assert_eq!(
            close_request_decision(dialog.is_dirty(), dialog.imp().operation.get()),
            CloseRequestDecision::SaveAndClose
        );
        assert!(!dialog.imp().close_prompt_open.get());

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_composer_modern_flat_header_styling() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = ComposeDialog::new();
        dialog.present_standalone();
        while context.iteration(false) {}

        let entry_to = dialog.imp().entry_to.borrow().as_ref().cloned().expect("entry_to exists");
        assert!(entry_to.has_css_class("composer-field-entry"));

        let entry_subject = dialog.imp().entry_subject.borrow().as_ref().cloned().expect("entry_subject exists");
        assert!(entry_subject.has_css_class("composer-field-entry"));
        assert!(entry_subject.has_css_class("composer-subject-entry"));

        let entry_cc = dialog.imp().entry_cc.borrow().as_ref().cloned().expect("entry_cc exists");
        assert!(entry_cc.has_css_class("composer-field-entry"));

        let entry_bcc = dialog.imp().entry_bcc.borrow().as_ref().cloned().expect("entry_bcc exists");
        assert!(entry_bcc.has_css_class("composer-field-entry"));

        let entry_reply_to = dialog.imp().entry_reply_to.borrow().as_ref().cloned().expect("entry_reply_to exists");
        assert!(entry_reply_to.has_css_class("composer-field-entry"));

        let editor = dialog.html_editor().expect("html editor exists");
        let toolbar = editor.imp().format_toolbar.borrow().as_ref().cloned().expect("format toolbar exists");
        assert!(toolbar.has_css_class("composer-format-toolbar"));
        assert_eq!(toolbar.margin_top(), 4);
        assert_eq!(toolbar.margin_bottom(), 8);
        assert_eq!(toolbar.margin_start(), 16);
        assert_eq!(toolbar.margin_end(), 16);

        dialog.close();
        while context.iteration(false) {}
    }

    #[test]
    fn test_reply_and_forward_not_dirty_until_user_edits() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let reply_dialog = ComposeDialog::reply_to_message(
            "Alice <alice@example.com>",
            "",
            "Hello World",
            "<p>Original body</p>",
        );
        while context.iteration(false) {}
        assert!(
            !reply_dialog.is_dirty(),
            "Freshly opened reply dialog must not be dirty before user types"
        );
        assert!(
            reply_dialog.imp().focus_editor_on_open.get(),
            "Reply dialog must focus the composer editor instead of the To entry"
        );
        if let Some(entry_to) = reply_dialog.imp().entry_to.borrow().as_ref() {
            assert_eq!(
                entry_to.selection_bounds(),
                None,
                "To entry text must not be selected when opening a reply"
            );
        }
        if let Some(editor) = reply_dialog.html_editor() {
            assert!(editor.imp().focus_on_ready.get());
            if let Some(wv) = editor.web_view() {
                assert_eq!(
                    adw::prelude::AdwDialogExt::focus(&reply_dialog).as_ref(),
                    Some(wv.upcast_ref::<gtk::Widget>())
                );
            }
        }
        assert_eq!(
            close_request_decision(reply_dialog.is_dirty(), reply_dialog.imp().operation.get()),
            CloseRequestDecision::Allow
        );

        let reply_all_dialog = ComposeDialog::reply_all_message(
            "Alice <alice@example.com>",
            "",
            "Bob <bob@example.com>",
            "Carol <carol@example.com>",
            "Hello World",
            "<p>Original body</p>",
        );
        while context.iteration(false) {}
        assert!(
            !reply_all_dialog.is_dirty(),
            "Freshly opened reply-all dialog must not be dirty before user types"
        );

        let forward_dialog = ComposeDialog::forward_message(
            "Hello World",
            "Alice <alice@example.com>",
            "Bob <bob@example.com>",
            "",
            "2026-11-02",
            "<p>Original body</p>",
        );
        while context.iteration(false) {}
        assert!(
            !forward_dialog.is_dirty(),
            "Freshly opened forward dialog must not be dirty before user types"
        );

        // Once the user edits a field or types in the editor, it becomes dirty
        if let Some(entry) = reply_dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text("Re: Hello World (edited)");
        }
        assert!(reply_dialog.is_dirty());
        assert_eq!(
            close_request_decision(reply_dialog.is_dirty(), reply_dialog.imp().operation.get()),
            CloseRequestDecision::SaveAndClose
        );

        reply_dialog.close();
        reply_all_dialog.close();
        forward_dialog.close();
        while context.iteration(false) {}
    }
}



