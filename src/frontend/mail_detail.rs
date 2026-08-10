// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 mail detail component.  It receives immutable message snapshots from
// the mail component and routes local read/flag mutations through the same
// service boundary used by the imperative frontend.  Once Camel is connected
// the component will keep the protocol unchanged and only the service
// implementation will change.

use super::mail::MailSummary;
use crate::services::mail_operations_service::MailOperationsService;
use crate::views::MailViewer;
use gtk::gio;
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;
use std::fmt;
use std::fs;

#[derive(Debug)]
pub(crate) enum MailDetailInput {
    Show(Option<MailSummary>),
    ToggleRead,
    ToggleFlag,
}

#[derive(Debug)]
pub(crate) enum MailDetailOutput {
    StateChanged {
        id: String,
        is_unread: bool,
        is_flagged: bool,
    },
}

pub(crate) struct MailDetailModel {
    selected: Option<MailSummary>,
    status: String,
    viewer: MailViewer,
    attachment_box: gtk::Box,
}

impl fmt::Debug for MailDetailModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MailDetailModel")
            .field("selected", &self.selected)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[relm4::component(pub)]
impl Component for MailDetailModel {
    type Init = ();
    type Input = MailDetailInput;
    type Output = MailDetailOutput;
    type CommandOutput = ();

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_hexpand: true,
            set_vexpand: true,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 4,
                set_margin_start: 16,
                set_margin_end: 16,
                set_margin_top: 16,

                gtk::Label {
                    #[watch]
                    set_label: &model.subject_label(),
                    set_halign: gtk::Align::Start,
                    set_wrap: true,
                    add_css_class: "title-2",
                },
                gtk::Label {
                    #[watch]
                    set_label: &model.from_label(),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },
                gtk::Label {
                    #[watch]
                    set_label: &model.to_label(),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },
                gtk::Label {
                    #[watch]
                    set_label: &model.date_label(),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },
                gtk::Label {
                    #[watch]
                    set_label: &model.folder_label(),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },
            },

            gtk::Separator {
                set_margin_top: 12,
                set_margin_bottom: 8,
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 6,
                set_margin_start: 16,
                set_margin_end: 16,

                gtk::Button {
                    #[watch]
                    set_label: &model.read_action_label(),
                    set_sensitive: model.selected.is_some(),
                    connect_clicked => MailDetailInput::ToggleRead,
                },
                gtk::Button {
                    #[watch]
                    set_icon_name: if model.is_flagged() {
                        "starred-symbolic"
                    } else {
                        "non-starred-symbolic"
                    },
                    set_tooltip_text: Some("Markierung umschalten"),
                    set_sensitive: model.selected.is_some(),
                    connect_clicked => MailDetailInput::ToggleFlag,
                },
                gtk::Label {
                    #[watch]
                    set_label: &model.status,
                    set_hexpand: true,
                    set_halign: gtk::Align::End,
                    add_css_class: "dim-label",
                },
            },

            #[name(attachment_box)]
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 8,
                set_margin_start: 16,
                set_margin_end: 16,
                set_margin_top: 8,
            },

            #[name(content_box)]
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_vexpand: true,
                set_hexpand: true,
                set_margin_top: 8,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let viewer = MailViewer::new();
        viewer.set_hexpand(true);
        viewer.set_vexpand(true);

        let mut model = MailDetailModel {
            selected: None,
            status: "Keine Nachricht ausgewählt".to_string(),
            viewer,
            attachment_box: gtk::Box::default(),
        };
        let widgets = view_output!();
        widgets.content_box.append(&model.viewer);
        model.attachment_box = widgets.attachment_box.clone();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            MailDetailInput::Show(selected) => {
                self.selected = selected;
                if let Some(message) = self.selected.as_ref() {
                    self.viewer.load_html(&message.html_body, None);
                    self.status = if message.is_unread {
                        "Nachricht geöffnet · als ungelesen markiert".to_string()
                    } else {
                        "Nachricht geöffnet".to_string()
                    };
                } else {
                    self.viewer.load_html("", None);
                    self.status = "Keine Nachricht ausgewählt".to_string();
                }
                self.render_attachments();
            }
            MailDetailInput::ToggleRead => {
                let Some(message) = self.selected.as_mut() else {
                    return;
                };
                let next_unread = !message.is_unread;
                let operations = MailOperationsService::new();
                let result = if next_unread {
                    operations.mark_unread(&message.id, message.is_flagged)
                } else {
                    operations.mark_read(&message.id, message.is_flagged)
                };
                match result {
                    Ok(()) => {
                        message.is_unread = next_unread;
                        self.status = if next_unread {
                            "Als ungelesen markiert".to_string()
                        } else {
                            "Als gelesen markiert".to_string()
                        };
                        let _ = sender.output(MailDetailOutput::StateChanged {
                            id: message.id.clone(),
                            is_unread: message.is_unread,
                            is_flagged: message.is_flagged,
                        });
                    }
                    Err(error) => {
                        self.status =
                            format!("Lesestatus konnte nicht gespeichert werden: {error}");
                    }
                }
            }
            MailDetailInput::ToggleFlag => {
                let Some(message) = self.selected.as_mut() else {
                    return;
                };
                let next_flagged = !message.is_flagged;
                let operations = MailOperationsService::new();
                match operations.set_flagged(&message.id, message.is_unread, next_flagged) {
                    Ok(()) => {
                        message.is_flagged = next_flagged;
                        self.status = if next_flagged {
                            "Markiert".to_string()
                        } else {
                            "Markierung entfernt".to_string()
                        };
                        let _ = sender.output(MailDetailOutput::StateChanged {
                            id: message.id.clone(),
                            is_unread: message.is_unread,
                            is_flagged: message.is_flagged,
                        });
                    }
                    Err(error) => {
                        self.status =
                            format!("Markierung konnte nicht gespeichert werden: {error}");
                    }
                }
            }
        }
    }
}

impl MailDetailModel {
    fn render_attachments(&self) {
        while let Some(child) = self.attachment_box.first_child() {
            self.attachment_box.remove(&child);
        }

        let Some(message) = self.selected.as_ref() else {
            self.attachment_box.set_visible(false);
            return;
        };

        if message.attachments.is_empty() {
            self.attachment_box.set_visible(false);
            return;
        }

        self.attachment_box.set_visible(true);
        for attachment in &message.attachments {
            let chip = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .css_classes(vec!["card", "attachment-chip"])
                .margin_top(4)
                .margin_bottom(4)
                .build();
            chip.append(&gtk::Image::from_icon_name("mail-attachment-symbolic"));
            chip.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{} ({})",
                        attachment.filename, attachment.size_formatted
                    ))
                    .css_classes(vec!["caption"])
                    .build(),
            );

            let save_button = gtk::Button::builder()
                .icon_name("document-save-symbolic")
                .tooltip_text("Anhang speichern unter…")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();
            let filename = attachment.filename.clone();
            let content = attachment.content.clone();
            let parent = self.attachment_box.root().and_downcast::<gtk::Window>();
            save_button.connect_clicked(move |_| {
                let Some(parent) = parent.as_ref() else {
                    tracing::warn!("Cannot save attachment without a parent window");
                    return;
                };
                let dialog = gtk::FileDialog::builder()
                    .title("Anhang speichern")
                    .modal(true)
                    .initial_name(&filename)
                    .build();
                let filename = filename.clone();
                let content = content.clone();
                dialog.save(
                    Some(parent),
                    None::<&gio::Cancellable>,
                    move |result| match result {
                        Ok(file) => {
                            let Some(path) = file.path() else {
                                tracing::warn!("Cannot save attachment to a non-local URI");
                                return;
                            };
                            if let Err(error) = fs::write(&path, &content) {
                                tracing::error!(path = %path.display(), %error, "Could not save attachment");
                            } else {
                                tracing::info!(path = %path.display(), filename = %filename, "Attachment saved");
                            }
                        }
                        Err(error) => tracing::debug!(%error, "Attachment save cancelled"),
                    },
                );
            });
            chip.append(&save_button);
            self.attachment_box.append(&chip);
        }
    }

    fn subject_label(&self) -> String {
        self.selected
            .as_ref()
            .map(|message| message.subject.clone())
            .unwrap_or_else(|| "Keine Nachricht ausgewählt".to_string())
    }

    fn from_label(&self) -> String {
        self.selected
            .as_ref()
            .map(|message| format!("Von: {} <{}>", message.sender, message.sender_email))
            .unwrap_or_default()
    }

    fn to_label(&self) -> String {
        self.selected
            .as_ref()
            .map(|message| format!("An: {}", message.recipient_to))
            .unwrap_or_default()
    }

    fn date_label(&self) -> String {
        self.selected
            .as_ref()
            .map(|message| message.date.clone())
            .unwrap_or_default()
    }

    fn folder_label(&self) -> String {
        self.selected
            .as_ref()
            .map(|message| format!("Ordner: {}", message.folder_id))
            .unwrap_or_default()
    }

    fn read_action_label(&self) -> &'static str {
        match self.selected.as_ref() {
            Some(message) if message.is_unread => "Als gelesen markieren",
            Some(_) => "Als ungelesen markieren",
            None => "Lesestatus",
        }
    }

    fn is_flagged(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|message| message.is_flagged)
    }
}
