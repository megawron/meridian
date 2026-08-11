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
use gtk::glib;
use std::cell::RefCell;
use std::path::Path;

use crate::services::AppSettings;
use crate::services::account_config_service::{AccountConfigStore, AccountProfile};
use crate::services::camel_mail_service;
use crate::services::contact_store_service::ContactStoreService;
use crate::services::draft_store_service::{DraftAttachment, DraftMessage, DraftStoreService};
use crate::services::eds_service::{self, EdsSourceKind, EdsSourceOrigin};
use crate::services::html_sanitizer::sanitize_html;
use crate::services::mail_composer_service::{self, OutgoingMessage};
use crate::services::signature_store_service::SignatureStore;
use crate::services::snippet_store_service::{MailSnippet, SnippetStore};
use crate::services::template_store_service::{MailTemplate, TemplateStore};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecipientSuggestion {
    display: String,
    address: String,
}

#[derive(Debug, Clone, Default)]
struct ComposeIdentity {
    source_uid: String,
    label: String,
    from: String,
    reply_to: String,
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ComposeDialog {
        pub draft_id: RefCell<String>,
        pub entry_to: RefCell<Option<gtk::Entry>>,
        pub entry_cc: RefCell<Option<gtk::Entry>>,
        pub entry_bcc: RefCell<Option<gtk::Entry>>,
        pub entry_subject: RefCell<Option<gtk::Entry>>,
        pub combo_from: RefCell<Option<gtk::DropDown>>,
        pub source_uids: RefCell<Vec<String>>,
        pub(super) source_identities: RefCell<Vec<ComposeIdentity>>,
        pub requested_source_uid: RefCell<Option<String>>,
        pub requested_from: RefCell<Option<String>>,
        pub calendar_method: RefCell<Option<String>>,
        pub calendar_component: RefCell<Option<String>>,
        pub html_editor: RefCell<Option<crate::views::HtmlEditor>>,
        pub attachments: RefCell<Vec<DraftAttachment>>,
        pub attachment_list: RefCell<Option<gtk::ListBox>>,
        pub autosave_source: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ComposeDialog {
        const NAME: &'static str = "EvolutionComposeDialog";
        type Type = super::ComposeDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for ComposeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(source) = self.autosave_source.borrow_mut().take() {
                source.remove();
            }
        }
    }

    impl WidgetImpl for ComposeDialog {}
    impl WindowImpl for ComposeDialog {}
    impl AdwWindowImpl for ComposeDialog {}
}

glib::wrapper! {
    pub struct ComposeDialog(ObjectSubclass<imp::ComposeDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl ComposeDialog {
    pub fn new() -> Self {
        let dialog: Self = glib::Object::builder()
            .property("title", "Neue E-Mail")
            .property("modal", false)
            .property("default-width", 780)
            .property("default-height", 620)
            .build();
        dialog.apply_default_signature();
        dialog
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
        let dialog = Self::new();
        dialog.set_title(Some(&format!("Antworten: {subject}")));
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
            editor.set_html(&reply_body_html(sender, original_html));
        }
        dialog
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
        let dialog = Self::new();
        dialog.set_title(Some(&format!("Weiterleiten: {subject}")));
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&forward_subject(subject));
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html(&forward_body_html(
                sender,
                recipient_to,
                recipient_cc,
                date,
                original_html,
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
        let dialog = Self::reply_to_message(sender, reply_to, subject, original_html);
        let primary = if reply_to.trim().is_empty() {
            sender.trim()
        } else {
            reply_to.trim()
        };
        let cc = merge_recipients(&[original_to, original_cc], &[primary]);
        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
            entry.set_text(&cc);
        }
        dialog.set_title(Some(&format!("Allen antworten: {subject}")));
        dialog
    }

    pub fn from_draft(draft: &DraftMessage) -> Self {
        let dialog = Self::new();
        *dialog.imp().draft_id.borrow_mut() = draft.id.clone();
        *dialog.imp().requested_source_uid.borrow_mut() =
            (!draft.source_uid.is_empty()).then(|| draft.source_uid.clone());
        *dialog.imp().requested_from.borrow_mut() =
            (!draft.from.trim().is_empty()).then(|| draft.from.clone());
        *dialog.imp().calendar_method.borrow_mut() = draft.calendar_method.clone();
        *dialog.imp().calendar_component.borrow_mut() = draft.calendar_component.clone();
        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
            entry.set_text(&draft.to);
        }
        if let Some(entry) = dialog.imp().entry_cc.borrow().as_ref() {
            entry.set_text(&draft.cc);
        }
        if let Some(entry) = dialog.imp().entry_bcc.borrow().as_ref() {
            entry.set_text(&draft.bcc);
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
        dialog.set_title(Some(&title));
        dialog
    }

    fn build_ui(&self) {
        {
            let mut stored_id = self.imp().draft_id.borrow_mut();
            if stored_id.is_empty() {
                *stored_id = uuid::Uuid::new_v4().to_string();
            }
        }
        let toolbar_view = adw::ToolbarView::new();

        // ── Header bar ──────────────────────────────────────────────────────
        let header_bar = adw::HeaderBar::new();

        let btn_send = gtk::Button::builder()
            .label("Senden")
            .icon_name("mail-send-symbolic")
            .css_classes(vec!["suggested-action"])
            .tooltip_text("E-Mail jetzt senden (Ctrl+Enter)")
            .build();
        header_bar.pack_end(&btn_send);

        let btn_save_draft = gtk::Button::builder()
            .label("Entwurf speichern")
            .icon_name("document-save-symbolic")
            .tooltip_text("Entwurf lokal speichern")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_end(&btn_save_draft);

        let btn_attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Anhang hinzufügen…")
            .css_classes(vec!["flat"])
            .build();

        btn_attach.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let file_dialog = gtk::FileDialog::builder()
                    .title("Anhang auswählen")
                    .modal(true)
                    .build();

                let dialog_for_file = dialog.clone();
                file_dialog.open(Some(&dialog), None::<&gio::Cancellable>, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            dialog_for_file.add_attachment_path(&path);
                        }
                    }
                });
            }
        ));
        header_bar.pack_start(&btn_attach);

        let btn_insert_template = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Vorlage oder Textbaustein einfügen")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_insert_template);

        let btn_save_template = gtk::Button::builder()
            .icon_name("document-save-as-symbolic")
            .tooltip_text("Aktuellen Inhalt als Vorlage speichern")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_save_template);

        let btn_save_snippet = gtk::Button::builder()
            .icon_name("insert-text-symbolic")
            .tooltip_text("Aktuellen Inhalt als Textbaustein speichern")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_save_snippet);

        let sign_by_default = AppSettings::try_new()
            .map(|settings| settings.sign_new_messages())
            .unwrap_or(false);
        let btn_sign = gtk::ToggleButton::builder()
            .icon_name("security-high-symbolic")
            .tooltip_text("PGP signieren (Linux-EDS/GnuPG erforderlich)")
            .active(sign_by_default)
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_sign);

        let btn_encrypt = gtk::ToggleButton::builder()
            .icon_name("changes-prevent-symbolic")
            .tooltip_text("PGP verschlüsseln (Linux-EDS/GnuPG erforderlich)")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_encrypt);

        let composer_html = AppSettings::try_new()
            .map(|settings| settings.composer_html())
            .unwrap_or(true);
        let btn_plain_text = gtk::ToggleButton::builder()
            .icon_name("text-x-generic-symbolic")
            .tooltip_text("Nur-Text-Modus beim Verfassen")
            .active(!composer_html)
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_plain_text);

        toolbar_view.add_top_bar(&header_bar);

        // ── Content Form ────────────────────────────────────────────────────
        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let form_grid = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(8)
            .build();

        // From
        let lbl_from = gtk::Label::builder()
            .label("Von:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let source_model = gtk::StringList::new(&["EDS-Absender werden geladen…"]);
        let combo_from = gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        combo_from.set_hexpand(true);

        // To
        let lbl_to = gtk::Label::builder()
            .label("An:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let entry_to = gtk::Entry::builder()
            .placeholder_text("Empfänger eingeben…")
            .hexpand(true)
            .build();

        // Cc/Bcc are visible from the beginning so recipients are never
        // silently omitted from a locally restored draft.
        let lbl_cc = gtk::Label::builder()
            .label("Cc:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let entry_cc = gtk::Entry::builder()
            .placeholder_text("Weitere Empfänger (Cc)…")
            .hexpand(true)
            .build();

        let lbl_bcc = gtk::Label::builder()
            .label("Bcc:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let entry_bcc = gtk::Entry::builder()
            .placeholder_text("Verdeckte Empfänger (Bcc)…")
            .hexpand(true)
            .build();

        // Subject
        let lbl_subject = gtk::Label::builder()
            .label("Betreff:")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::End)
            .build();
        let entry_subject = gtk::Entry::builder()
            .placeholder_text("Betreff…")
            .hexpand(true)
            .build();

        form_grid.attach(&lbl_from, 0, 0, 1, 1);
        form_grid.attach(&combo_from, 1, 0, 1, 1);
        form_grid.attach(&lbl_to, 0, 1, 1, 1);
        form_grid.attach(&entry_to, 1, 1, 1, 1);
        form_grid.attach(&lbl_cc, 0, 2, 1, 1);
        form_grid.attach(&entry_cc, 1, 2, 1, 1);
        form_grid.attach(&lbl_bcc, 0, 3, 1, 1);
        form_grid.attach(&entry_bcc, 1, 3, 1, 1);
        form_grid.attach(&lbl_subject, 0, 4, 1, 1);
        form_grid.attach(&entry_subject, 1, 4, 1, 1);

        content_box.append(&form_grid);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Rich WebKitGTK 6.0 WYSIWYG HTML Editor
        let html_editor = crate::views::HtmlEditor::new();
        html_editor.set_plain_text_mode(btn_plain_text.is_active());
        html_editor.set_vexpand(true);

        let settings_for_composer_mode = AppSettings::try_new();
        let editor_for_composer_mode = html_editor.clone();
        btn_plain_text.connect_toggled(move |button| {
            editor_for_composer_mode.set_plain_text_mode(button.is_active());
            if let Some(settings) = settings_for_composer_mode.as_ref() {
                settings.set_composer_html(!button.is_active());
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
        self.imp().entry_cc.replace(Some(entry_cc.clone()));
        self.imp().entry_bcc.replace(Some(entry_bcc.clone()));
        self.imp()
            .entry_subject
            .replace(Some(entry_subject.clone()));
        self.imp().combo_from.replace(Some(combo_from.clone()));
        self.imp().html_editor.replace(Some(html_editor.clone()));
        self.imp().attachment_list.replace(Some(attachment_list));

        let dialog_for_default_account = self.downgrade();
        let settings_for_default_account = AppSettings::try_new();
        combo_from.connect_selected_notify(move |combo| {
            let Some(dialog) = dialog_for_default_account.upgrade() else {
                return;
            };
            let source_uid = dialog
                .imp()
                .source_uids
                .borrow()
                .get(combo.selected() as usize)
                .cloned()
                .unwrap_or_default();
            if !source_uid.trim().is_empty() {
                if let Some(settings) = settings_for_default_account.as_ref() {
                    settings.set_default_account_id(&source_uid);
                }
            }
        });

        let dialog_weak_for_templates = self.downgrade();
        let entry_subject_for_templates = entry_subject.clone();
        let html_editor_for_templates = html_editor.clone();
        btn_insert_template.connect_clicked(move |button| {
            let popover = gtk::Popover::new();
            popover.set_parent(button);
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
        btn_save_template.connect_clicked(move |button| {
            let popover = gtk::Popover::new();
            popover.set_parent(button);
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
        btn_save_snippet.connect_clicked(move |button| {
            let popover = gtk::Popover::new();
            popover.set_parent(button);
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

        let local_recipient_suggestions =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::<RecipientSuggestion>::new()));
        let (recipient_sender, recipient_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let mut suggestions = ContactStoreService::new()
                .load_contacts()
                .into_iter()
                .flat_map(|record| {
                    let display = record.contact.fn_name.trim().to_string();
                    let addresses = if record.contact.emails.is_empty() {
                        vec![record.contact.email.clone()]
                    } else {
                        record.contact.emails.clone()
                    };
                    addresses
                        .into_iter()
                        .map(move |address| RecipientSuggestion {
                            display: display.clone(),
                            address: address.trim().to_string(),
                        })
                        .filter(|suggestion| !suggestion.address.is_empty())
                })
                .collect::<Vec<_>>();
            suggestions.sort_by_key(|suggestion| suggestion.address.to_ascii_lowercase());
            suggestions.dedup_by(|left, right| left.address.eq_ignore_ascii_case(&right.address));
            if let Err(error) = recipient_sender.send_blocking(suggestions) {
                tracing::debug!(%error, "Recipient suggestion result was dropped");
            }
        });
        let suggestions_for_load = std::rc::Rc::clone(&local_recipient_suggestions);
        glib::spawn_future_local(async move {
            if let Ok(suggestions) = recipient_receiver.recv().await {
                suggestions_for_load.replace(suggestions);
            }
        });
        for entry in [&entry_to, &entry_cc, &entry_bcc] {
            setup_local_recipient_completion(
                entry,
                std::rc::Rc::clone(&local_recipient_suggestions),
            );
        }

        let dialog_weak_for_send = self.downgrade();
        let send_button_weak = btn_send.downgrade();
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
            let identity = dialog.selected_identity(combo_from_for_send.selected());
            let outgoing = OutgoingMessage {
                source_uid: identity.source_uid,
                from: identity.from,
                reply_to: identity.reply_to,
                request_read_receipt: AppSettings::try_new()
                    .map(|settings| settings.request_read_receipt())
                    .unwrap_or(false),
                to: entry_to_for_send.text().to_string(),
                cc: entry_cc_for_send.text().to_string(),
                bcc: entry_bcc_for_send.text().to_string(),
                subject: entry_subject_for_send.text().to_string(),
                html_body: String::new(),
                plain_text: false,
                attachments: dialog.imp().attachments.borrow().clone(),
                calendar_method: dialog.imp().calendar_method.borrow().clone(),
                calendar_component: dialog.imp().calendar_component.borrow().clone(),
            };
            let dialog_weak = dialog.downgrade();
            let send_button_weak_for_result = send_button_weak.clone();
            if let Some(button) = send_button_weak.upgrade() {
                button.set_sensitive(false);
            }

            html_editor_for_send.read_content(move |result| {
                let content = match result {
                    Ok(content) => content,
                    Err(error) => {
                        if let Some(button) = send_button_weak_for_result.upgrade() {
                            button.set_sensitive(true);
                        }
                        if let Some(dialog) = dialog_weak.upgrade() {
                            dialog.show_toast(&format!(
                                "Nachricht konnte nicht gelesen werden: {error}"
                            ));
                        }
                        return;
                    }
                };

                let mut outgoing = outgoing;
                outgoing.html_body = content.body;
                outgoing.plain_text = content.plain_text;
                let raw_message = match mail_composer_service::build_rfc822(&outgoing) {
                    Ok(raw_message) => raw_message,
                    Err(error) => {
                        if let Some(button) = send_button_weak_for_result.upgrade() {
                            button.set_sensitive(true);
                        }
                        if let Some(dialog) = dialog_weak.upgrade() {
                            dialog.show_toast(&error);
                        }
                        return;
                    }
                };

                let source_uid = outgoing.source_uid;
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let result = camel_mail_service::send_message(&source_uid, &raw_message);
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Composer send result was dropped");
                    }
                });

                glib::spawn_future_local(async move {
                    let result = receiver.recv().await;
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    if let Some(button) = send_button_weak_for_result.upgrade() {
                        button.set_sensitive(true);
                    }
                    match result {
                        Ok(Ok(sent_message_saved)) => {
                            let message = if sent_message_saved {
                                "E-Mail gesendet; Kopie in Gesendet gespeichert"
                            } else {
                                "E-Mail gesendet"
                            };
                            dialog.delete_local_draft();
                            dialog.show_toast(message);
                            dialog.close();
                        }
                        Ok(Err(error)) => {
                            dialog.show_toast(&format!(
                                "E-Mail konnte nicht gesendet werden: {error}"
                            ));
                        }
                        Err(error) => {
                            dialog.show_toast(&format!("Versand abgebrochen: {error}"));
                        }
                    }
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
                    let label = if source.backend_name.is_empty() {
                        format!("{}{}", source.display_name, cache_suffix)
                    } else {
                        format!(
                            "{} · {}{}",
                            source.display_name, source.backend_name, cache_suffix
                        )
                    };
                    ComposeIdentity {
                        source_uid: source.uid.clone(),
                        label,
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
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            if let Some(dialog) = dialog_for_source_load.upgrade() {
                let requested_uid = dialog.imp().requested_source_uid.borrow().clone();
                let requested_from = dialog.imp().requested_from.borrow().clone();
                let preferred_uid = AppSettings::try_new()
                    .map(|settings| settings.default_account_id())
                    .filter(|uid| !uid.trim().is_empty());
                let mut source_uids = dialog.imp().source_uids.borrow_mut();
                source_uids.clear();
                source_uids.extend(
                    identities
                        .iter()
                        .map(|identity| identity.source_uid.clone()),
                );
                dialog.imp().source_identities.replace(identities.clone());
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
            }
            if names.is_empty() {
                let message = if discovery.origin == EdsSourceOrigin::Empty {
                    "EDS-Mailkonten sind nicht erreichbar"
                } else {
                    "Kein aktives EDS-Mailkonto verfügbar"
                };
                source_model_for_load.splice(0, source_model_for_load.n_items(), &[message]);
            }
        });

        let native_adapter_available = camel_mail_service::native_adapter_available();
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
            move |_| {
                let to = entry_to.text().to_string();
                let cc = entry_cc.text().to_string();
                let bcc = entry_bcc.text().to_string();
                let subject = entry_subject.text().to_string();
                let identity = dialog.selected_identity(combo_from.selected());
                let from = if identity.from.trim().is_empty() {
                    dropdown_text(&combo_from)
                } else {
                    identity.from.clone()
                };
                let reply_to = identity.reply_to;
                let source_uid = identity.source_uid;
                let attachments = dialog.imp().attachments.borrow().clone();
                let calendar_method = dialog.imp().calendar_method.borrow().clone();
                let calendar_component = dialog.imp().calendar_component.borrow().clone();
                let draft_id = dialog.imp().draft_id.borrow().clone();
                let updated_at = Utc::now().to_rfc3339();
                let dialog_weak = dialog.downgrade();

                html_editor.read_content(move |result| match result {
                    Ok(content) => {
                        let crate::views::EditorContent { body, plain_text } = content;
                        if native_adapter_available {
                            let outgoing = OutgoingMessage {
                                source_uid: source_uid.clone(),
                                from: from.clone(),
                                reply_to: reply_to.clone(),
                                request_read_receipt: false,
                                to,
                                cc,
                                bcc,
                                subject,
                                html_body: body,
                                plain_text,
                                attachments,
                                calendar_method,
                                calendar_component,
                            };
                            let raw_message = match mail_composer_service::build_rfc822(&outgoing) {
                                Ok(raw_message) => raw_message,
                                Err(error) => {
                                    if let Some(dialog) = dialog_weak.upgrade() {
                                        dialog.show_toast(&error);
                                    }
                                    return;
                                }
                            };
                            let source_uid = outgoing.source_uid;
                            let (sender, receiver) = async_channel::bounded(1);
                            std::thread::spawn(move || {
                                let result = camel_mail_service::save_message(
                                    &source_uid,
                                    &raw_message,
                                    camel_mail_service::CamelSaveFolder::Drafts,
                                );
                                if let Err(error) = sender.send_blocking(result) {
                                    tracing::debug!(%error, "Draft save result was dropped");
                                }
                            });
                            glib::spawn_future_local(async move {
                                let result = receiver.recv().await;
                                let Some(dialog) = dialog_weak.upgrade() else {
                                    return;
                                };
                                match result {
                                    Ok(Ok(())) => {
                                        tracing::info!("Draft saved to EDS Drafts folder");
                                        dialog.show_toast("Entwurf in EDS gespeichert");
                                    }
                                    Ok(Err(error)) => dialog.show_toast(&format!(
                                        "Entwurf konnte nicht in EDS gespeichert werden: {error}"
                                    )),
                                    Err(error) => dialog.show_toast(&format!(
                                        "Entwurfsspeicherung abgebrochen: {error}"
                                    )),
                                }
                            });
                        } else {
                            let draft = DraftMessage {
                                id: draft_id,
                                from,
                                source_uid,
                                to,
                                cc,
                                bcc,
                                subject,
                                html_body: body,
                                plain_text,
                                updated_at,
                                attachments,
                                calendar_method,
                                calendar_component,
                            };
                            match DraftStoreService::default().save(draft) {
                                Ok(()) => {
                                    tracing::info!("Draft saved to application-owned draft store");
                                    if let Some(dialog) = dialog_weak.upgrade() {
                                        dialog.show_toast("Entwurf lokal gespeichert");
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%error, "Could not save draft");
                                    if let Some(dialog) = dialog_weak.upgrade() {
                                        dialog.show_toast(&format!(
                                            "Entwurf konnte nicht gespeichert werden: {error}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not read HTML editor contents");
                        if let Some(dialog) = dialog_weak.upgrade() {
                            dialog.show_toast(&format!(
                                "Entwurf konnte nicht gelesen werden: {error}"
                            ));
                        }
                    }
                });
            }
        ));

        toolbar_view.set_content(Some(&content_box));
        self.set_content(Some(&toolbar_view));

        self.start_local_autosave();
    }

    fn apply_default_signature(&self) {
        let signature = signature_body_html();
        if signature.is_empty() {
            return;
        }
        if let Some(editor) = self.imp().html_editor.borrow().as_ref() {
            editor.set_html(&signature);
        }
    }

    fn start_local_autosave(&self) {
        // Native EDS/Camel draft persistence is deliberately a separate
        // Linux acceptance item.  The portable fallback owns local autosave
        // until that backend is verified on the target machine.
        if camel_mail_service::native_adapter_available() {
            return;
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
        if camel_mail_service::native_adapter_available() {
            return;
        }
        let Some(entry_to) = self.imp().entry_to.borrow().as_ref().cloned() else {
            return;
        };
        let Some(entry_cc) = self.imp().entry_cc.borrow().as_ref().cloned() else {
            return;
        };
        let Some(entry_bcc) = self.imp().entry_bcc.borrow().as_ref().cloned() else {
            return;
        };
        let Some(entry_subject) = self.imp().entry_subject.borrow().as_ref().cloned() else {
            return;
        };
        let Some(combo_from) = self.imp().combo_from.borrow().as_ref().cloned() else {
            return;
        };
        let Some(html_editor) = self.imp().html_editor.borrow().as_ref().cloned() else {
            return;
        };

        let to = entry_to.text().to_string();
        let cc = entry_cc.text().to_string();
        let bcc = entry_bcc.text().to_string();
        let subject = entry_subject.text().to_string();
        let identity = self.selected_identity(combo_from.selected());
        let from = if identity.from.trim().is_empty() {
            dropdown_text(&combo_from)
        } else {
            identity.from.clone()
        };
        let source_uid = identity.source_uid;
        let draft_id = self.imp().draft_id.borrow().clone();
        let attachments = self.imp().attachments.borrow().clone();
        let calendar_method = self.imp().calendar_method.borrow().clone();
        let calendar_component = self.imp().calendar_component.borrow().clone();
        if to.trim().is_empty()
            && cc.trim().is_empty()
            && bcc.trim().is_empty()
            && subject.trim().is_empty()
            && attachments.is_empty()
        {
            return;
        }

        let dialog = self.downgrade();
        html_editor.read_content(move |result| {
            let Ok(content) = result else {
                return;
            };
            let Some(_dialog) = dialog.upgrade() else {
                return;
            };
            let draft = DraftMessage {
                id: draft_id,
                from,
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
            };
            if let Err(error) = DraftStoreService::default().save(draft) {
                tracing::warn!(%error, "Could not autosave local draft");
            } else {
                tracing::debug!("Local draft autosaved");
            }
        });
    }

    fn delete_local_draft(&self) {
        if camel_mail_service::native_adapter_available() {
            return;
        }
        let draft_id = self.imp().draft_id.borrow().clone();
        if draft_id.is_empty() {
            return;
        }
        match DraftStoreService::default().delete(&draft_id) {
            Ok(true) => tracing::debug!(%draft_id, "Local draft removed after send"),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, %draft_id, "Could not remove local draft after send")
            }
        }
    }

    fn selected_identity(&self, selected: u32) -> ComposeIdentity {
        self.imp()
            .source_identities
            .borrow()
            .get(selected as usize)
            .cloned()
            .unwrap_or_default()
    }

    fn show_toast(&self, message: &str) {
        let Some(parent) = self.transient_for() else {
            tracing::warn!(%message, "Composer result");
            return;
        };
        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
        } else {
            tracing::warn!(%message, "Composer result");
        }
    }

    fn add_attachment_path(&self, path: &Path) {
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let attachment = DraftAttachment {
            path: path.to_string_lossy().into_owned(),
            filename: filename.to_string(),
            mime_type: mime_type_for_path(path),
            size: std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        };
        self.add_attachment(attachment);
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
        let Some(list) = self.imp().attachment_list.borrow().as_ref().cloned() else {
            return;
        };

        let row = adw::ActionRow::builder()
            .title(&attachment.filename)
            .subtitle(&format!(
                "{} · {}",
                attachment.mime_type,
                format_size(attachment.size)
            ))
            .build();
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
            }
            list_for_remove.remove(&row_for_remove);
            list_for_remove.set_visible(list_for_remove.first_child().is_some());
        });
    }
}

impl Default for ComposeDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn dropdown_text(dropdown: &gtk::DropDown) -> String {
    dropdown
        .selected_item()
        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
        .map(|item| item.string().to_string())
        .unwrap_or_default()
}

fn local_account_choices() -> Vec<ComposeIdentity> {
    AccountConfigStore::default()
        .load()
        .into_iter()
        .filter(|profile| profile.enabled)
        .flat_map(compose_identities_for_profile)
        .collect()
}

fn compose_identities_for_profile(profile: AccountProfile) -> Vec<ComposeIdentity> {
    let source_uid = profile.id;
    let display_name = profile.display_name.trim().to_string();
    let reply_to = profile.reply_to.trim().to_string();
    let primary_address = profile.email.trim().to_string();
    let primary_label = format!("{} <{}> · lokales Profil", display_name, primary_address);
    let primary_from = format!("{} <{}>", display_name, primary_address);
    let mut identities = vec![ComposeIdentity {
        source_uid: source_uid.clone(),
        label: primary_label,
        from: primary_from,
        reply_to: reply_to.clone(),
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

fn reply_body_html(sender: &str, original_html: &str) -> String {
    let attribution = if sender.trim().is_empty() {
        "der ursprünglichen Nachricht".to_string()
    } else {
        format!("{} schrieb", html_escape(sender.trim()))
    };
    let quoted = quote_as_safe_text(original_html);
    let quoted = format_quoted_body(&quoted, default_quote_style());
    let prefix = signature_prefix_html();
    if quoted.is_empty() {
        prefix
    } else {
        format!("{prefix}<p>{attribution}:</p>{quoted}")
    }
}

fn forward_body_html(
    sender: &str,
    recipient_to: &str,
    recipient_cc: &str,
    date: &str,
    original_html: &str,
) -> String {
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
    let quoted = format_quoted_body(&quoted, default_quote_style());
    let prefix = signature_prefix_html();
    if quoted.is_empty() {
        prefix
    } else {
        format!("{prefix}<p>{header_block}</p>{quoted}")
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

fn signature_body_html() -> String {
    let signature = default_signature_html();
    if signature.is_empty() {
        String::new()
    } else {
        format!("<p><br></p><p>--<br>{signature}</p>")
    }
}

fn signature_prefix_html() -> String {
    let signature = default_signature_html();
    if signature.is_empty() {
        "<p><br></p>".to_string()
    } else {
        format!("<p>--<br>{signature}</p><p><br></p>")
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
        if signature.format.to_ascii_lowercase().contains("html") {
            return sanitize_html(signature.text.trim_end());
        }
        return html_escape(signature.text.trim_end()).replace('\n', "<br>");
    }
    let text = settings.signature_text();
    if text.trim().is_empty() {
        return String::new();
    }
    html_escape(text.trim_end()).replace('\n', "<br>")
}

fn default_quote_style() -> &'static str {
    let Some(settings) = AppSettings::try_new() else {
        return "blockquote";
    };
    if !settings.quote_original() {
        return "none";
    }
    if settings.quote_style() == "prefix" {
        "prefix"
    } else {
        "blockquote"
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

fn setup_local_recipient_completion(
    entry: &gtk::Entry,
    suggestions: std::rc::Rc<std::cell::RefCell<Vec<RecipientSuggestion>>>,
) {
    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_parent(entry);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .width_request(360)
        .build();
    popover.set_child(Some(&list));

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

    let visible_addresses_for_change = std::rc::Rc::clone(&visible_addresses);
    let popover_for_change = popover.clone();
    entry.connect_changed(move |entry| {
        let entry_text = entry.text();
        let query = current_recipient_fragment(&entry_text);
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        let matches = if query.len() < 2 {
            Vec::new()
        } else {
            let query = query.to_ascii_lowercase();
            suggestions
                .borrow()
                .iter()
                .filter(|suggestion| {
                    suggestion.address.to_ascii_lowercase().contains(&query)
                        || suggestion.display.to_ascii_lowercase().contains(&query)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        visible_addresses_for_change.replace(
            matches
                .iter()
                .map(|suggestion| suggestion.address.clone())
                .collect(),
        );
        for suggestion in &matches {
            let label = if suggestion.display.is_empty() {
                suggestion.address.clone()
            } else {
                format!("{} <{}>", suggestion.display, suggestion.address)
            };
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::builder()
                .label(&label)
                .xalign(0.0)
                .margin_start(12)
                .margin_end(12)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            row.set_child(Some(&label));
            list.append(&row);
        }
        if matches.is_empty() {
            popover_for_change.popdown();
        } else {
            popover_for_change.popup();
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let identities = compose_identities_for_profile(profile);
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
    }
}
