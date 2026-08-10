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

use crate::services::camel_mail_service;
use crate::services::draft_store_service::{DraftAttachment, DraftMessage, DraftStoreService};
use crate::services::eds_service;
use crate::services::mail_composer_service::{self, OutgoingMessage};

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
        pub requested_source_uid: RefCell<Option<String>>,
        pub html_editor: RefCell<Option<crate::views::HtmlEditor>>,
        pub attachments: RefCell<Vec<DraftAttachment>>,
        pub attachment_list: RefCell<Option<gtk::ListBox>>,
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
        glib::Object::builder()
            .property("title", "Neue E-Mail")
            .property("modal", false)
            .property("default-width", 780)
            .property("default-height", 620)
            .build()
    }

    pub fn reply_to(recipient: &str, subject: &str) -> Self {
        let dialog = Self::new();
        dialog.set_title(Some(&format!("Antworten: {subject}")));
        if let Some(entry) = dialog.imp().entry_to.borrow().as_ref() {
            entry.set_text(recipient);
        }
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&reply_subject(subject));
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html(&format!(
                "<p><br></p><p>Am ursprünglichen Nachrichtenverlauf von {}:</p>",
                html_escape(recipient)
            ));
        }
        dialog
    }

    pub fn forward(subject: &str) -> Self {
        let dialog = Self::new();
        dialog.set_title(Some(&format!("Weiterleiten: {subject}")));
        if let Some(entry) = dialog.imp().entry_subject.borrow().as_ref() {
            entry.set_text(&forward_subject(subject));
        }
        if let Some(editor) = dialog.imp().html_editor.borrow().as_ref() {
            editor.set_html("<p><br></p><p>Weitergeleitete Nachricht:</p>");
        }
        dialog
    }

    pub fn from_draft(draft: &DraftMessage) -> Self {
        let dialog = Self::new();
        *dialog.imp().draft_id.borrow_mut() = draft.id.clone();
        *dialog.imp().requested_source_uid.borrow_mut() =
            (!draft.source_uid.is_empty()).then(|| draft.source_uid.clone());
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
            editor.set_html(&draft.html_body);
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

        let btn_sign = gtk::ToggleButton::builder()
            .icon_name("security-high-symbolic")
            .tooltip_text("PGP Signieren")
            .active(true)
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_sign);

        let btn_encrypt = gtk::ToggleButton::builder()
            .icon_name("changes-prevent-symbolic")
            .tooltip_text("PGP Verschlüsseln")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&btn_encrypt);

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
        html_editor.set_vexpand(true);

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
            let outgoing = OutgoingMessage {
                source_uid: dialog.selected_source_uid(combo_from_for_send.selected()),
                to: entry_to_for_send.text().to_string(),
                cc: entry_cc_for_send.text().to_string(),
                bcc: entry_bcc_for_send.text().to_string(),
                subject: entry_subject_for_send.text().to_string(),
                html_body: String::new(),
                attachments: dialog.imp().attachments.borrow().clone(),
            };
            let dialog_weak = dialog.downgrade();
            let send_button_weak_for_result = send_button_weak.clone();
            if let Some(button) = send_button_weak.upgrade() {
                button.set_sensitive(false);
            }

            html_editor_for_send.read_html(move |result| {
                let html_body = match result {
                    Ok(html_body) => html_body,
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
                outgoing.html_body = html_body;
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
            match eds_service::list_mail_sources().await {
                Ok(sources) => {
                    let active_sources: Vec<_> = sources
                        .into_iter()
                        .filter(|source| source.enabled)
                        .collect();
                    let names: Vec<String> = active_sources
                        .iter()
                        .map(|source| {
                            if source.backend_name.is_empty() {
                                source.display_name.clone()
                            } else {
                                format!("{} · {}", source.display_name, source.backend_name)
                            }
                        })
                        .collect();
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
                    if let Some(dialog) = dialog_for_source_load.upgrade() {
                        let mut source_uids = dialog.imp().source_uids.borrow_mut();
                        source_uids.clear();
                        source_uids.extend(active_sources.iter().map(|source| source.uid.clone()));
                        if let Some(requested_uid) =
                            dialog.imp().requested_source_uid.borrow().as_deref()
                        {
                            if let Some(index) =
                                source_uids.iter().position(|uid| uid == requested_uid)
                            {
                                combo_from_for_load.set_selected(index as u32);
                            }
                        }
                    }
                    if active_sources.is_empty() {
                        source_model_for_load.splice(
                            0,
                            source_model_for_load.n_items(),
                            &["Kein aktives EDS-Mailkonto verfügbar"],
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load EDS mail sources for composer");
                    source_model_for_load.splice(
                        0,
                        source_model_for_load.n_items(),
                        &["EDS-Mailkonten sind nicht erreichbar"],
                    );
                }
            }
        });

        let draft_store = DraftStoreService::default();
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
                let draft_store = draft_store.clone();
                let to = entry_to.text().to_string();
                let cc = entry_cc.text().to_string();
                let bcc = entry_bcc.text().to_string();
                let subject = entry_subject.text().to_string();
                let from = dropdown_text(&combo_from);
                let source_uid = dialog.selected_source_uid(combo_from.selected());
                let attachments = dialog.imp().attachments.borrow().clone();
                let draft_id = dialog.imp().draft_id.borrow().clone();
                let updated_at = Utc::now().to_rfc3339();

                html_editor.read_html(move |result| match result {
                    Ok(html_body) => {
                        let draft = DraftMessage {
                            id: draft_id,
                            from,
                            source_uid,
                            to,
                            cc,
                            bcc,
                            subject,
                            html_body,
                            updated_at,
                            attachments,
                        };
                        match draft_store.save(draft) {
                            Ok(()) => {
                                tracing::info!("Draft saved to application-owned draft store");
                                if let Some(parent) = dialog.transient_for() {
                                    if let Ok(window) =
                                        parent.downcast::<crate::window::EvolutionWindow>()
                                    {
                                        window.add_toast(&adw::Toast::new(
                                            "Entwurf lokal gespeichert",
                                        ));
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "Could not save draft");
                                if let Some(parent) = dialog.transient_for() {
                                    if let Ok(window) =
                                        parent.downcast::<crate::window::EvolutionWindow>()
                                    {
                                        window.add_toast(&adw::Toast::new(&format!(
                                            "Entwurf konnte nicht gespeichert werden: {error}"
                                        )));
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not read HTML editor contents");
                    }
                });
            }
        ));

        toolbar_view.set_content(Some(&content_box));
        self.set_content(Some(&toolbar_view));
    }

    fn selected_source_uid(&self, selected: u32) -> String {
        self.imp()
            .source_uids
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
}
