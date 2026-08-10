// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 compose window.  Draft persistence is deliberately delegated to the
// framework-neutral DraftStoreService; Camel/EDS transport will later consume
// the same message state through an explicit output/command boundary.

use crate::services::draft_store_service::{DraftMessage, DraftStoreService};
use crate::views::HtmlEditor;
use chrono::Utc;
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;
use std::fmt;

#[derive(Debug)]
pub(crate) enum ComposeInput {
    ToChanged(String),
    CcChanged(String),
    BccChanged(String),
    SubjectChanged(String),
    SaveDraft,
    Send,
    AddAttachment,
    AttachmentSelected(String),
    EditorRead(Result<String, String>),
    ToggleSign(bool),
    ToggleEncrypt(bool),
}

pub(crate) struct ComposeModel {
    draft_id: String,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    status: String,
    attachment_names: Vec<String>,
    sign_message: bool,
    encrypt_message: bool,
    editor: HtmlEditor,
}

impl fmt::Debug for ComposeModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComposeModel")
            .field("draft_id", &self.draft_id)
            .field("to", &self.to)
            .field("cc", &self.cc)
            .field("bcc", &self.bcc)
            .field("subject", &self.subject)
            .field("status", &self.status)
            .field("attachment_names", &self.attachment_names)
            .field("sign_message", &self.sign_message)
            .field("encrypt_message", &self.encrypt_message)
            .finish_non_exhaustive()
    }
}

#[relm4::component(pub)]
impl Component for ComposeModel {
    type Init = ();
    type Input = ComposeInput;
    type Output = ();
    type CommandOutput = ();

    view! {
        #[root]
        relm4::adw::Window {
            set_title: Some("Neue E-Mail"),
            set_default_width: 820,
            set_default_height: 680,
            set_modal: false,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,
                    set_margin_start: 10,
                    set_margin_end: 10,
                    set_margin_top: 8,
                    set_margin_bottom: 8,

                    gtk::Button {
                        set_label: "Senden",
                        set_icon_name: "mail-send-symbolic",
                        add_css_class: "suggested-action",
                        connect_clicked => ComposeInput::Send,
                    },
                    gtk::Button {
                        set_label: "Entwurf speichern",
                        set_icon_name: "document-save-symbolic",
                        connect_clicked => ComposeInput::SaveDraft,
                    },
                    gtk::Button {
                        set_icon_name: "mail-attachment-symbolic",
                        set_tooltip_text: Some("Anhang hinzufügen…"),
                        connect_clicked => ComposeInput::AddAttachment,
                    },
                    gtk::ToggleButton {
                        set_icon_name: "security-high-symbolic",
                        set_active: model.sign_message,
                        set_tooltip_text: Some("PGP-Signatur anfordern"),
                        connect_toggled[sender] => move |button| {
                            sender.input(ComposeInput::ToggleSign(button.is_active()));
                        },
                    },
                    gtk::ToggleButton {
                        set_icon_name: "changes-prevent-symbolic",
                        set_active: model.encrypt_message,
                        set_tooltip_text: Some("PGP-Verschlüsselung anfordern"),
                        connect_toggled[sender] => move |button| {
                            sender.input(ComposeInput::ToggleEncrypt(button.is_active()));
                        },
                    },
                    gtk::Label {
                        #[watch]
                        set_label: &model.status,
                        set_hexpand: true,
                        set_halign: gtk::Align::End,
                        add_css_class: "dim-label",
                    },
                },

                gtk::Separator {},

                gtk::Grid {
                    set_row_spacing: 8,
                    set_column_spacing: 12,
                    set_margin_start: 16,
                    set_margin_end: 16,
                    set_margin_top: 12,
                    set_margin_bottom: 8,

                    attach[0, 0, 1, 1] = &gtk::Label {
                        set_label: "Von:",
                        set_halign: gtk::Align::End,
                        add_css_class: "dim-label",
                    },
                    attach[1, 0, 1, 1] = &gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Kein EDS-Absender konfiguriert",
                        ])),
                        set_hexpand: true,
                        set_sensitive: false,
                    },
                    attach[0, 1, 1, 1] = &gtk::Label {
                        set_label: "An:",
                        set_halign: gtk::Align::End,
                        add_css_class: "dim-label",
                    },
                    attach[1, 1, 1, 1] = &gtk::Entry {
                        set_placeholder_text: Some("Empfänger eingeben…"),
                        set_hexpand: true,
                        connect_changed[sender] => move |entry| {
                            sender.input(ComposeInput::ToChanged(entry.text().to_string()));
                        },
                    },
                    attach[0, 2, 1, 1] = &gtk::Label {
                        set_label: "Cc:",
                        set_halign: gtk::Align::End,
                        add_css_class: "dim-label",
                    },
                    attach[1, 2, 1, 1] = &gtk::Entry {
                        set_placeholder_text: Some("Cc (optional)…"),
                        set_hexpand: true,
                        connect_changed[sender] => move |entry| {
                            sender.input(ComposeInput::CcChanged(entry.text().to_string()));
                        },
                    },
                    attach[0, 3, 1, 1] = &gtk::Label {
                        set_label: "Bcc:",
                        set_halign: gtk::Align::End,
                        add_css_class: "dim-label",
                    },
                    attach[1, 3, 1, 1] = &gtk::Entry {
                        set_placeholder_text: Some("Bcc (optional)…"),
                        set_hexpand: true,
                        connect_changed[sender] => move |entry| {
                            sender.input(ComposeInput::BccChanged(entry.text().to_string()));
                        },
                    },
                    attach[0, 4, 1, 1] = &gtk::Label {
                        set_label: "Betreff:",
                        set_halign: gtk::Align::End,
                        add_css_class: "dim-label",
                    },
                    attach[1, 4, 1, 1] = &gtk::Entry {
                        set_placeholder_text: Some("Betreff…"),
                        set_hexpand: true,
                        connect_changed[sender] => move |entry| {
                            sender.input(ComposeInput::SubjectChanged(entry.text().to_string()));
                        },
                    },
                },

                #[name(attachment_label)]
                gtk::Label {
                    #[watch]
                    set_label: &model.attachment_label(),
                    set_halign: gtk::Align::Start,
                    set_wrap: true,
                    set_margin_start: 16,
                    set_margin_end: 16,
                    add_css_class: "dim-label",
                },

                #[name(editor_box)]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_vexpand: true,
                    set_hexpand: true,
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_bottom: 12,
                },
            }
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let editor = HtmlEditor::new();
        editor.set_vexpand(true);
        editor.set_hexpand(true);
        let model = ComposeModel {
            draft_id: uuid::Uuid::new_v4().to_string(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            status: "Neuer Entwurf".to_string(),
            attachment_names: Vec::new(),
            sign_message: false,
            encrypt_message: false,
            editor,
        };
        let widgets = view_output!();
        widgets.editor_box.append(&model.editor);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            ComposeInput::ToChanged(value) => self.to = value,
            ComposeInput::CcChanged(value) => self.cc = value,
            ComposeInput::BccChanged(value) => self.bcc = value,
            ComposeInput::SubjectChanged(value) => self.subject = value,
            ComposeInput::SaveDraft => {
                let sender = sender.clone();
                self.status = "Entwurf wird gespeichert…".to_string();
                self.editor.read_html(move |result| {
                    sender.input(ComposeInput::EditorRead(
                        result.map_err(|error| error.to_string()),
                    ));
                });
            }
            ComposeInput::EditorRead(result) => match result {
                Ok(html_body) => {
                    let draft = DraftMessage {
                        id: self.draft_id.clone(),
                        to: self.to.clone(),
                        cc: self.cc.clone(),
                        bcc: self.bcc.clone(),
                        subject: self.subject.clone(),
                        html_body,
                        updated_at: Utc::now().to_rfc3339(),
                    };
                    match DraftStoreService::default().save(draft) {
                        Ok(()) => self.status = "Entwurf lokal gespeichert".to_string(),
                        Err(error) => {
                            self.status =
                                format!("Entwurf konnte nicht gespeichert werden: {error}")
                        }
                    }
                }
                Err(error) => self.status = format!("Editor konnte nicht gelesen werden: {error}"),
            },
            ComposeInput::Send => {
                self.status =
                    "Versand ist noch nicht verbunden — der Entwurf bleibt geöffnet".to_string();
                tracing::warn!(
                    "Send requested, but the EDS/Camel transport backend is not connected yet"
                );
            }
            ComposeInput::AddAttachment => {
                let dialog = gtk::FileDialog::builder()
                    .title("Anhang auswählen")
                    .modal(true)
                    .build();
                let sender = sender.clone();
                dialog.open(Some(root), None::<&gtk::gio::Cancellable>, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            sender.input(ComposeInput::AttachmentSelected(
                                path.to_string_lossy().into_owned(),
                            ));
                        }
                    }
                });
            }
            ComposeInput::AttachmentSelected(path) => {
                self.attachment_names.push(path);
                self.status = "Anhang für den Entwurf vorgemerkt".to_string();
            }
            ComposeInput::ToggleSign(value) => {
                self.sign_message = value;
                self.status = if value {
                    "PGP-Signatur angefordert; Camel/GnuPG-Anbindung folgt".to_string()
                } else {
                    "PGP-Signatur nicht angefordert".to_string()
                };
            }
            ComposeInput::ToggleEncrypt(value) => {
                self.encrypt_message = value;
                self.status = if value {
                    "PGP-Verschlüsselung angefordert; Camel/GnuPG-Anbindung folgt".to_string()
                } else {
                    "PGP-Verschlüsselung nicht angefordert".to_string()
                };
            }
        }
    }
}

impl ComposeModel {
    fn attachment_label(&self) -> String {
        if self.attachment_names.is_empty() {
            return String::new();
        }
        format!("Anhänge: {}", self.attachment_names.join(", "))
    }
}
