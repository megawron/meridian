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

use crate::services::draft_store_service::{DraftMessage, DraftStoreService};

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct ComposeDialog {
        pub draft_id: RefCell<String>,
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
        let title = format!("Antworten: {}", subject);
        let dialog = glib::Object::builder::<Self>()
            .property("title", &title)
            .property("modal", false)
            .property("default-width", 780)
            .property("default-height", 620)
            .build();

        tracing::info!("Replying to: {} with subject: {}", recipient, subject);
        dialog
    }

    pub fn forward(subject: &str) -> Self {
        let title = format!("Weiterleiten: {}", subject);
        let dialog = glib::Object::builder::<Self>()
            .property("title", &title)
            .property("modal", false)
            .property("default-width", 780)
            .property("default-height", 620)
            .build();

        tracing::info!("Forwarding email with subject: {}", subject);
        dialog
    }

    fn build_ui(&self) {
        let draft_id = {
            let mut stored_id = self.imp().draft_id.borrow_mut();
            if stored_id.is_empty() {
                *stored_id = uuid::Uuid::new_v4().to_string();
            }
            stored_id.clone()
        };
        let toolbar_view = adw::ToolbarView::new();

        // ── Header bar ──────────────────────────────────────────────────────
        let header_bar = adw::HeaderBar::new();

        let btn_send = gtk::Button::builder()
            .label("Senden")
            .icon_name("mail-send-symbolic")
            .css_classes(vec!["suggested-action"])
            .tooltip_text("E-Mail jetzt senden (Ctrl+Enter)")
            .build();
        btn_send.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                tracing::warn!(
                    "Send requested, but the EDS/Camel transport backend is not connected yet"
                );
                if let Some(transient) = dialog.transient_for() {
                    if let Ok(win) = transient.downcast::<crate::window::EvolutionWindow>() {
                        let toast = adw::Toast::new(
                            "Versand ist noch nicht verbunden — der Entwurf bleibt geöffnet",
                        );
                        win.add_toast(&toast);
                    }
                }
            }
        ));
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

                file_dialog.open(Some(&dialog), None::<&gio::Cancellable>, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            tracing::info!("Attached file: {}", path.display());
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
        let combo_from = gtk::DropDown::from_strings(&["Kein EDS-Absender konfiguriert"]);
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
        form_grid.attach(&lbl_subject, 0, 2, 1, 1);
        form_grid.attach(&entry_subject, 1, 2, 1, 1);

        content_box.append(&form_grid);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Rich WebKitGTK 6.0 WYSIWYG HTML Editor
        let html_editor = crate::views::HtmlEditor::new();
        html_editor.set_vexpand(true);

        content_box.append(&html_editor);

        let draft_store = DraftStoreService::default();
        btn_save_draft.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            entry_to,
            #[strong]
            entry_subject,
            #[strong]
            html_editor,
            #[strong]
            draft_id,
            move |_| {
                let draft_store = draft_store.clone();
                let to = entry_to.text().to_string();
                let subject = entry_subject.text().to_string();
                let updated_at = Utc::now().to_rfc3339();
                let draft_id = draft_id.clone();

                html_editor.read_html(move |result| match result {
                    Ok(html_body) => {
                        let draft = DraftMessage {
                            id: draft_id,
                            to,
                            cc: String::new(),
                            bcc: String::new(),
                            subject,
                            html_body,
                            updated_at,
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
}

impl Default for ComposeDialog {
    fn default() -> Self {
        Self::new()
    }
}
